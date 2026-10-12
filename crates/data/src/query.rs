//! Queries (CONTEXT.md): an ordered list of Applied Steps that turns a Source into a table.

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{Field, Schema, SchemaRef};

use crate::culture::Culture;
use crate::merge;
use crate::source::{SAMPLE_ROWS, Source};
use crate::step::Step;
use crate::{Error, Result, Table, TableBuilder};

/// A Query. Its result is loaded as the table with the Query's name.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub name: String,
    /// Where the data comes from. Every run re-reads it: Refresh in full, Preview the first
    /// 1,000 rows. Nothing of it is held between runs.
    pub source: Source,
    pub steps: Vec<Step>,
    /// Parses text in Changed Type steps (the Project culture until Projects exist).
    pub culture: &'static Culture,
}

impl Query {
    /// A new Query over `source` with its automatic steps (*Promoted Headers*, *Changed Type*),
    /// detected on the first [`SAMPLE_ROWS`] rows (ADR 0005). Nothing is loaded yet.
    pub fn connect(
        name: &str,
        source: Source,
        culture: &'static Culture,
        limit: usize,
    ) -> Result<Query> {
        let (sample, _) = source.read_batch(name, SAMPLE_ROWS + 1, culture, limit)?;
        let steps = source.detect(&sample, culture)?;
        Ok(Query {
            name: name.into(),
            source,
            steps,
            culture,
        })
    }

    /// Reads the whole Source and runs every step: the table to load (named after the Query,
    /// encoded narrow, at most `limit` bytes) and its Cell error count (values a step couldn't
    /// produce, and Excel error cells; all loaded as blank). A Merged Queries step fails here;
    /// use [`Query::run_with`].
    pub fn run(&self, limit: usize) -> Result<(Table, usize)> {
        self.run_with(&[], limit)
    }

    /// [`Query::run`], with `queries` (the Document's; this one may be among them) supplying the
    /// other side of Merged Queries steps. Each other Query runs at most once per call, from its
    /// Source; a Query that merges itself, directly or through others, is an error. To load
    /// several Queries, use one [`Pass`], so each Source is read once in all.
    ///
    /// Streams (ADR 0005): the leading steps that work batch by batch run as each batch is read;
    /// from the first step that needs the whole table on (a merge), steps run on the loaded
    /// result.
    pub fn run_with(&self, queries: &[Query], limit: usize) -> Result<(Table, usize)> {
        Pass::new(queries, limit).run(self)
    }

    /// Whether this Query has a Merged Queries step taking in the Query `name`.
    pub fn merges(&self, name: &str) -> bool {
        self.steps
            .iter()
            .any(|s| matches!(s, Step::MergedQueries(m) if m.query == name))
    }

    /// Reads the Source and runs the leading streaming steps batch by batch: the loaded table,
    /// its Cell errors, and the steps left to run on it.
    fn stream(&self, limit: usize) -> Result<(Table, usize, &[Step])> {
        let streamed = self.steps.iter().take_while(|s| streams(s)).count();
        let (head, tail) = self.steps.split_at(streamed);
        let mut head = Head {
            steps: head,
            culture: self.culture,
            headers: vec![None; head.len()],
            errors: 0,
            held: None,
        };
        let mut builder: Option<TableBuilder> = None;
        let mut push = |batch: RecordBatch| {
            builder
                .get_or_insert_with(|| TableBuilder::new(&self.name, batch.schema(), limit))
                .push(&batch)
        };
        let source_errors = self.source.read(
            &self.name,
            usize::MAX,
            self.culture,
            limit,
            &mut |batch| match head.run(batch, 0, true)? {
                Some(batch) => push(batch),
                None => Ok(()),
            },
        )?;
        // No row ever reached a Promoted Headers: run the rest of the steps on the empty batch
        // it held, as the whole table would (nothing to promote).
        if let Some((from, batch)) = head.held.take() {
            if let Some(batch) = head.run(batch, from, false)? {
                push(batch)?;
            }
        }
        let table = builder
            .ok_or_else(|| Error::Source {
                source: self.name.clone(),
                message: "the source gave no data".into(),
            })?
            .finish()?;
        Ok((table, head.errors.saturating_add(source_errors), tail))
    }
}

/// The streaming steps of a run, applied batch by batch.
struct Head<'a> {
    steps: &'a [Step],
    culture: &'static Culture,
    /// Per step: for Promoted Headers, the names it took from the first row that reached it
    /// (whichever batch that was); later batches get those names.
    headers: Vec<Option<SchemaRef>>,
    errors: usize,
    /// The first empty batch stopped at a Promoted Headers no row has reached yet, and that
    /// step's index: kept in case no row ever does.
    held: Option<(usize, RecordBatch)>,
}

impl Head<'_> {
    /// Runs steps `from..` on `batch`. `None` when `hold` and the batch is empty at a Promoted
    /// Headers still waiting for its first row: the step and everything after it wait too,
    /// since they depend on the names that row will give.
    fn run(
        &mut self,
        mut batch: RecordBatch,
        from: usize,
        hold: bool,
    ) -> Result<Option<RecordBatch>> {
        let steps = self.steps.iter().zip(self.headers.iter_mut()).enumerate();
        for (i, (step, names)) in steps.skip(from) {
            batch = match (step, &*names) {
                (Step::PromotedHeaders, Some(names)) => rename(&batch, names)?,
                (Step::PromotedHeaders, None) if hold && batch.num_rows() == 0 => {
                    if self.held.is_none() {
                        self.held = Some((i, batch));
                    }
                    return Ok(None);
                }
                _ => {
                    let promotes = *step == Step::PromotedHeaders && batch.num_rows() > 0;
                    let (next, n) = step.apply(&batch, self.culture)?;
                    self.errors = self.errors.saturating_add(n);
                    if promotes {
                        *names = Some(next.schema());
                    }
                    next
                }
            };
        }
        Ok(Some(batch))
    }
}

/// How deep merges may nest (A merges B merges C …).
const MAX_DEPTH: usize = 32;

/// One load pass (Refresh, Close & Apply): runs Queries in full from their Sources, each at most
/// once. A Query's result is kept for the rest of the pass, so loading it and merging it into
/// other Queries share one read of its Source. Power Query itself re-runs a referenced Query for
/// every Query that uses it (docs: "Referencing Power Query queries"); the results are the same,
/// so this saves reads without changing what loads.
///
/// The results kept count against the pass's memory `limit`: the tables it returns and the
/// merged-from Queries it ran must fit in it together.
pub struct Pass<'a>(Ctx<'a>);

impl<'a> Pass<'a> {
    /// A pass over `queries` (the Document's), holding at most `limit` bytes of results.
    pub fn new(queries: &'a [Query], limit: usize) -> Self {
        Self(Ctx::new(queries, limit))
    }

    /// `q`'s table and Cell errors: run now, unless this pass already ran it (to load it or to
    /// merge it into another). The error is `q`'s own or a merged-from Query's.
    pub fn run(&mut self, q: &'a Query) -> Result<(Table, usize)> {
        self.0.table(q)
    }
}

/// One run (or Preview) that may merge other Queries: resolves them, each once.
pub(crate) struct Ctx<'a> {
    queries: &'a [Query],
    limit: usize,
    /// For a Preview: the loaded Tables that aren't Stale. A merged-from Query uses its Table
    /// here, else its own Preview; never a full run (see `crate::preview`).
    preview: Option<&'a [&'a Table]>,
    /// Previews of merged-from Queries already made.
    previews: HashMap<&'a str, (RecordBatch, usize)>,
    /// The Queries being run, outermost first.
    stack: Vec<&'a str>,
    /// Results of the Queries already run.
    done: HashMap<&'a str, (Table, usize)>,
    /// Bytes held by `done`.
    held: usize,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(queries: &'a [Query], limit: usize) -> Self {
        Self {
            queries,
            limit,
            preview: None,
            previews: HashMap::new(),
            stack: Vec::new(),
            done: HashMap::new(),
            held: 0,
        }
    }

    /// A Preview's: merged-from Queries use `loaded` (the Tables that aren't Stale) or their own
    /// Preview.
    pub(crate) fn preview(queries: &'a [Query], loaded: &'a [&'a Table], limit: usize) -> Self {
        Self {
            preview: Some(loaded),
            ..Self::new(queries, limit)
        }
    }

    pub(crate) fn limit(&self) -> usize {
        self.limit
    }

    /// The limit less the results held.
    fn free(&self) -> usize {
        self.limit.saturating_sub(self.held)
    }

    /// `q`'s loaded table and Cell errors: run once, then reused.
    fn table(&mut self, q: &'a Query) -> Result<(Table, usize)> {
        if let Some(done) = self.done.get(q.name.as_str()) {
            return Ok(done.clone());
        }
        let r = self.compute(q)?;
        self.held = self.held.saturating_add(r.0.memory());
        self.done.insert(&q.name, r.clone());
        Ok(r)
    }

    /// Runs `q` from its Source.
    fn compute(&mut self, q: &'a Query) -> Result<(Table, usize)> {
        let (table, mut errors, tail) = q.stream(self.free())?;
        if tail.is_empty() {
            return Ok((table, errors));
        }
        let mut batch = table.batch().clone();
        drop(table);
        for step in tail {
            let (next, n) = self.apply(q, step, &batch)?;
            batch = next;
            errors = errors.saturating_add(n);
        }
        let mut b = TableBuilder::new(&q.name, batch.schema(), self.free());
        b.push(&batch)?;
        Ok((b.finish()?, errors))
    }

    /// Runs one of `q`'s steps on `batch` (a merge on the whole table, never per batch).
    pub(crate) fn apply(
        &mut self,
        q: &'a Query,
        step: &Step,
        batch: &RecordBatch,
    ) -> Result<(RecordBatch, usize)> {
        match step {
            Step::MergedQueries(m) => {
                let (other, n) = self.other(&q.name, &m.query)?;
                Ok((merge::join(batch, &other, m, &q.name, self.free())?, n))
            }
            step => step.apply(batch, q.culture),
        }
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
        if let Some(loaded) = self.preview {
            if let Some(t) = loaded.iter().find(|t| t.name() == name) {
                return Ok((t.batch().clone(), 0));
            }
            if let Some(p) = self.previews.get(name) {
                return Ok(p.clone());
            }
        } else if let Some((table, n)) = self.done.get(name) {
            return Ok((table.batch().clone(), *n));
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
        let r = match self.preview {
            None => self.table(q).map(|(t, n)| (t.batch().clone(), n)),
            Some(_) => q.preview_in(self, q.steps.len()).map(|p| {
                let n = p.error_count();
                (p.batch, n)
            }),
        };
        self.stack.pop();
        let r = r?;
        if self.preview.is_some() {
            self.previews.insert(&q.name, r.clone());
        }
        Ok(r)
    }
}

/// Steps that run batch by batch as the Source streams (ADR 0005). Any other step (and every
/// step after it) runs once on the whole streamed result.
fn streams(step: &Step) -> bool {
    matches!(
        step,
        Step::PromotedHeaders
            | Step::ChangedType(_)
            | Step::RenamedColumns(_)
            | Step::RemovedColumns(_)
            | Step::ReorderedColumns(_)
            | Step::FilteredRows { .. }
    )
}
/// `batch` with the column names of `names` (Promoted Headers on a batch after the first).
fn rename(batch: &RecordBatch, names: &SchemaRef) -> Result<RecordBatch> {
    let err = |message: String| Error::Step {
        step: "Promoted Headers",
        message,
    };
    if names.fields().len() != batch.num_columns() {
        return Err(err("the column count changed while reading".into()));
    }
    let fields: Vec<Field> = names
        .fields()
        .iter()
        .zip(batch.columns())
        .map(|(f, c)| Field::new(f.name(), c.data_type().clone(), true))
        .collect();
    RecordBatch::try_new(Arc::new(Schema::new(fields)), batch.columns().to_vec())
        .map_err(|e| err(e.to_string()))
}

#[cfg(test)]
mod tests {
    use arrow_schema::DataType;

    use super::*;
    use crate::source::{BATCH_ROWS, Bytes, File, csv};
    use crate::step::ColumnType;

    fn source(text: &str) -> Source {
        let file = File::Bytes {
            name: "sales.csv".into(),
            bytes: Bytes::from(text.to_string()),
        };
        Source::csv("Sales", file, csv::Options::default()).unwrap()
    }

    fn query(steps: Vec<Step>) -> Query {
        Query {
            name: "Sales".into(),
            source: source("Region,Qty\nNorth,3\nSouth,x\n"),
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

    #[test]
    fn connect_adds_the_automatic_steps() {
        let q = Query::connect(
            "Sales",
            source("Region,Qty\nNorth,3\nSouth,4\n"),
            Culture::get("en-GB").unwrap(),
            usize::MAX,
        )
        .unwrap();
        assert_eq!(
            q.steps,
            [
                Step::PromotedHeaders,
                Step::ChangedType(vec![
                    ("Region".into(), ColumnType::Text),
                    ("Qty".into(), ColumnType::Whole)
                ])
            ]
        );
    }

    #[test]
    fn steps_stream_across_batches() {
        let n = BATCH_ROWS * 2 + 5;
        let mut csv = String::from("Id,Name\n");
        for i in 0..n {
            csv.push_str(&format!("{i},n{}\n", i % 7));
        }
        csv.push_str("oops,z\n");
        let q = Query {
            name: "Big".into(),
            source: source(&csv),
            steps: vec![
                Step::PromotedHeaders,
                Step::ChangedType(vec![("Id".into(), ColumnType::Whole)]),
                Step::RenamedColumns(vec![("Name".into(), "Group".into())]),
            ],
            culture: Culture::get("en-US").unwrap(),
        };
        let (t, errors) = q.run(usize::MAX).unwrap();
        assert_eq!((t.rows(), errors), (n + 1, 1), "header row dropped once");
        let names: Vec<_> = t
            .batch()
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();
        assert_eq!(names, ["Id", "Group"]);
    }

    #[test]
    fn promoted_headers_after_a_filter_that_empties_the_first_batch() {
        use crate::step::{Combine, Condition, Op, Value};
        // The first batch is all `drop` rows; the header row is the first one left.
        let mut csv = "drop,1\n".repeat(BATCH_ROWS + 10);
        csv.push_str("Name,Qty\na,2\n");
        let filter = Step::FilteredRows {
            combine: Combine::And,
            conditions: vec![Condition {
                column: "Column1".into(),
                op: Op::NotEquals,
                value: Value::Text("drop".into()),
            }],
        };
        let culture = Culture::get("en-US").unwrap();
        let cases = [
            vec![filter.clone(), Step::PromotedHeaders],
            vec![
                filter.clone(),
                Step::PromotedHeaders,
                Step::ChangedType(vec![("Qty".into(), ColumnType::Whole)]),
            ],
        ];
        for steps in cases {
            let q = Query {
                name: "S".into(),
                source: source(&csv),
                steps,
                culture,
            };
            // The same steps on the whole table at once.
            let (mut whole, _) = q
                .source
                .read_batch("S", usize::MAX, culture, usize::MAX)
                .unwrap();
            for step in &q.steps {
                whole = step.apply(&whole, culture).unwrap().0;
            }
            let (t, _) = q.run(usize::MAX).unwrap();
            let names = |s: SchemaRef| -> Vec<String> {
                s.fields().iter().map(|f| f.name().clone()).collect()
            };
            assert_eq!(names(t.batch().schema()), ["Name", "Qty"], "{:?}", q.steps);
            assert_eq!(names(t.batch().schema()), names(whole.schema()));
            assert_eq!(t.rows(), 1);
        }
        // Every row filtered out: no header to promote, as on the whole table.
        let q = Query {
            name: "S".into(),
            source: source(&"drop,1\n".repeat(BATCH_ROWS + 10)),
            steps: vec![filter, Step::PromotedHeaders],
            culture,
        };
        let (t, _) = q.run(usize::MAX).unwrap();
        assert_eq!(
            (t.rows(), t.batch().schema().field(0).name().as_str()),
            (0, "Column1")
        );
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

    /// The rows asked of each Source read since the last call, by file.
    fn reads() -> Vec<(String, usize)> {
        crate::source::READS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    #[test]
    fn a_pass_reads_each_source_once() {
        // A and B both merge T; loading all three reads T's file once, not three times.
        let file = |name: &str, text: &str| {
            let file = File::Bytes {
                name: name.into(),
                bytes: Bytes::from(text.to_string()),
            };
            Source::csv(name, file, csv::Options::default()).unwrap()
        };
        let mut t = query(vec![Step::PromotedHeaders]);
        t.name = "T".into();
        t.source = file("t.csv", "Region,Qty\nNorth,10\n");
        let (a, mut b) = (merging("A", "T"), merging("B", "T"));
        b.source = file("b.csv", "Region,Qty\nSouth,1\n");
        let queries = [a, b, t];
        reads();
        let mut pass = Pass::new(&queries, usize::MAX);
        // A first: T runs as its merge, then is reused for B and for loading T itself.
        let tables: Vec<Table> = queries.iter().map(|q| pass.run(q).unwrap().0).collect();
        let mut files: Vec<String> = reads().into_iter().map(|(f, _)| f).collect();
        files.sort();
        assert_eq!(files, ["b.csv", "sales.csv", "t.csv"]);
        assert_eq!(tables[2].rows(), 1);
        assert_eq!(tables[0].batch().schema().field(2).name(), "T.Qty");
        // The same results as running each Query on its own.
        for (q, t) in queries.iter().zip(&tables) {
            assert_eq!(&q.run_with(&queries, usize::MAX).unwrap().0, t);
        }
    }

    #[test]
    fn a_pass_counts_what_it_holds_against_its_limit() {
        let mut other = query(vec![Step::PromotedHeaders]);
        other.name = "Targets".into();
        let sales = merging("Sales", "Targets");
        let queries = [sales, other];
        let targets = queries[1].run(usize::MAX).unwrap().0.memory();
        let sales = queries[0]
            .run_with(&queries, usize::MAX)
            .unwrap()
            .0
            .memory();
        // Room for either table, not for both.
        let mut pass = Pass::new(&queries, targets + sales - 1);
        assert!(pass.run(&queries[1]).is_ok());
        assert!(matches!(
            pass.run(&queries[0]),
            Err(Error::MemoryCap { .. })
        ));
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
