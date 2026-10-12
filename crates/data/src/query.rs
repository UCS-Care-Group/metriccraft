//! Queries (CONTEXT.md): an ordered list of Applied Steps that turns a Source into a table.

use std::collections::HashMap;

use arrow_array::RecordBatch;

use crate::culture::Culture;
use crate::merge;
use crate::step::Step;
use crate::{Error, Result, Table, TableBuilder};

/// A Query. Its result is loaded as the table with the Query's name.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub name: String,
    /// The data the first step runs on: what the Source yields, before any step.
    // ponytail: held in memory so steps can be added, deleted and re-run without touching the
    // file. M1.8 replaces it with a Source description (path + options, or the file bytes on the
    // web) that Refresh re-reads in full and Preview reads the first 1,000 rows of; the CSV and
    // XLSX imports then also need a raw mode (all text, `Column1..N`) for the automatic steps to
    // run here rather than inside the import.
    pub source: Table,
    pub steps: Vec<Step>,
    /// Parses text in Changed Type steps (the Project culture until Projects exist).
    pub culture: &'static Culture,
}

/// How deep merges may nest (A merges B merges C …).
const MAX_DEPTH: usize = 32;

impl Query {
    /// Runs every step over the source: the table to load (named after the Query, encoded narrow,
    /// at most `limit` bytes) and its Cell error count. A Merged Queries step fails here; use
    /// [`Query::run_with`].
    pub fn run(&self, limit: usize) -> Result<(Table, usize)> {
        self.run_with(&[], limit)
    }

    /// [`Query::run`], with `queries` (the Document's; this one may be among them) supplying the
    /// other side of Merged Queries steps. Each other Query runs at most once per call, from its
    /// source; a Query that merges itself, directly or through others, is an error.
    pub fn run_with(&self, queries: &[Query], limit: usize) -> Result<(Table, usize)> {
        let mut ctx = Ctx {
            queries,
            limit,
            stack: Vec::new(),
            done: HashMap::new(),
        };
        let (batch, errors) = ctx.result(self)?;
        let mut b = TableBuilder::new(&self.name, batch.schema(), limit);
        b.push(&batch)?;
        Ok((b.finish()?, errors))
    }

    /// Whether this Query has a Merged Queries step taking in the Query `name`.
    pub fn merges(&self, name: &str) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s, Step::MergedQueries(m) if m.query == name))
    }
}

/// One [`Query::run_with`] call.
struct Ctx<'a> {
    queries: &'a [Query],
    limit: usize,
    /// The Queries being run, outermost first.
    stack: Vec<&'a str>,
    /// Results of other Queries already run in this call.
    done: HashMap<&'a str, (RecordBatch, usize)>,
}

impl<'a> Ctx<'a> {
    fn result(&mut self, q: &'a Query) -> Result<(RecordBatch, usize)> {
        let mut batch = q.source.batch().clone();
        let mut errors = 0usize;
        for step in &q.steps {
            let (next, n) = match step {
                Step::MergedQueries(m) => {
                    let (other, n) = self.other(&q.name, &m.query)?;
                    (merge::join(&batch, &other, m, &q.name, self.limit)?, n)
                }
                step => step.apply(&batch, q.culture)?,
            };
            batch = next;
            errors = errors.saturating_add(n);
        }
        Ok((batch, errors))
    }

    /// The result of the Query `name`, which `into` merges.
    fn other(&mut self, into: &'a str, name: &str) -> Result<(RecordBatch, usize)> {
        let err = |message: String| Error::Step {
            step: "Merged Queries",
            message,
        };
        if name == into || self.stack.contains(&name) {
            return Err(err(format!(
                "`{into}` can't merge `{name}`: `{name}` depends on `{into}`"
            )));
        }
        if let Some(done) = self.done.get(name) {
            return Ok(done.clone());
        }
        let q = self
            .queries
            .iter()
            .find(|q| q.name == name)
            .ok_or_else(|| err(format!("there is no query `{name}`")))?;
        if self.stack.len() >= MAX_DEPTH {
            return Err(err(format!("merges nest more than {MAX_DEPTH} deep")));
        }
        self.stack.push(into);
        let r = self.result(q);
        self.stack.pop();
        let r = r?;
        self.done.insert(&q.name, r.clone());
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{Array, ArrayRef, RecordBatch, StringArray};
    use arrow_schema::DataType;

    use super::*;
    use crate::Error;
    use crate::step::ColumnType;

    fn query(steps: Vec<Step>) -> Query {
        let col = |v: Vec<&str>| Arc::new(StringArray::from(v)) as ArrayRef;
        let batch = RecordBatch::try_from_iter([
            ("Column1", col(vec!["Region", "North", "South"])),
            ("Column2", col(vec!["Qty", "3", "x"])),
        ])
        .unwrap();
        let mut b = TableBuilder::new("raw", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        Query {
            name: "Sales".into(),
            source: b.finish().unwrap(),
            steps,
            culture: Culture::get("en-GB").unwrap(),
        }
    }

    #[test]
    fn runs_every_step_and_loads_narrow() {
        let q = query(vec![
            Step::PromotedHeaders,
            Step::ChangedType(vec![("Qty".into(), ColumnType::Whole)]),
            Step::RenamedColumns(vec![("Qty".into(), "Quantity".into())]),
            Step::ReorderedColumns(vec!["Quantity".into(), "Region".into()]),
        ]);
        let (t, errors) = q.run(usize::MAX).unwrap();
        assert_eq!((t.name(), t.rows(), errors), ("Sales", 2, 1));
        let schema = t.batch().schema();
        assert_eq!(schema.field(0).name(), "Quantity");
        assert_eq!(
            schema.field(0).data_type(),
            &DataType::Int8,
            "narrowed on load"
        );
    }

    #[test]
    fn untouched_columns_share_the_source_buffers() {
        let q = query(vec![Step::RenamedColumns(vec![(
            "Column1".into(),
            "A".into(),
        )])]);
        let (t, _) = q.run(usize::MAX).unwrap();
        let values = |b: &RecordBatch| b.column(0).to_data().child_data()[0].buffers()[1].as_ptr();
        assert_eq!(values(t.batch()), values(q.source.batch()));
    }

    #[test]
    fn a_failing_step_names_itself() {
        let q = query(vec![Step::RemovedColumns(vec!["Nope".into()])]);
        assert!(matches!(
            q.run(usize::MAX),
            Err(Error::Step {
                step: "Removed Columns",
                ..
            })
        ));
    }

    #[test]
    fn no_steps_loads_the_source_and_respects_the_cap() {
        let q = query(vec![]);
        assert_eq!(q.run(usize::MAX).unwrap().0.rows(), 3);
        assert!(matches!(q.run(1), Err(Error::MemoryCap { .. })));
    }

    use crate::step::{JoinKind, Merge};

    /// `Sales` (headed) merging the Query `other` on Region.
    fn merging(name: &str, other: &str) -> Query {
        let mut q = query(vec![
            Step::PromotedHeaders,
            Step::MergedQueries(Merge {
                keys: vec!["Region".into()],
                query: other.into(),
                other_keys: vec!["Region".into()],
                kind: JoinKind::LeftOuter,
                column: other.into(),
                expand: vec![("Qty".into(), format!("{other}.Qty"))],
            }),
        ]);
        q.name = name.into();
        q
    }

    #[test]
    fn merges_run_the_other_query() {
        let mut other = query(vec![Step::PromotedHeaders]);
        other.name = "Targets".into();
        let sales = merging("Sales", "Targets");
        let (t, _) = sales.run_with(&[sales.clone(), other], usize::MAX).unwrap();
        let names: Vec<_> = t
            .batch()
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, ["Region", "Qty", "Targets.Qty"]);
        assert_eq!(t.rows(), 2);
        assert!(sales.merges("Targets") && !sales.merges("Sales"));
        let e = sales.run(usize::MAX).unwrap_err();
        assert!(e.to_string().contains("no query `Targets`"), "{e}");
    }

    #[test]
    fn self_and_circular_merges_are_errors() {
        let me = merging("Sales", "Sales");
        let e = me
            .run_with(std::slice::from_ref(&me), usize::MAX)
            .unwrap_err();
        assert!(e.to_string().contains("`Sales` depends on `Sales`"), "{e}");
        let (a, b) = (merging("A", "B"), merging("B", "A"));
        let e = a.run_with(&[a.clone(), b], usize::MAX).unwrap_err();
        assert!(e.to_string().contains("`A` depends on `B`"), "{e}");
    }

    #[test]
    fn merge_chains_are_bounded() {
        // Q0 merges Q1 … merges Q40: deeper than MAX_DEPTH, an error, not a stack overflow.
        let mut qs: Vec<Query> = (0..40)
            .map(|i| merging(&format!("Q{i}"), &format!("Q{}", i + 1)))
            .collect();
        qs.push(query(vec![Step::PromotedHeaders]));
        if let Some(last) = qs.last_mut() {
            last.name = "Q40".into();
        }
        let e = qs[0].run_with(&qs, usize::MAX).unwrap_err();
        assert!(e.to_string().contains("nest more than"), "{e}");
    }
}
