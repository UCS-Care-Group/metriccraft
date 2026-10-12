//! Queries (CONTEXT.md): an ordered list of Applied Steps that turns a Source into a table.

use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{Field, Schema, SchemaRef};

use crate::culture::Culture;
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
    /// produce, and Excel error cells; all loaded as blank).
    ///
    /// Streams (ADR 0005): the leading steps that work batch by batch run as each batch is read;
    /// from the first step that needs the whole table on, steps run on the loaded result.
    pub fn run(&self, limit: usize) -> Result<(Table, usize)> {
        let culture = self.culture;
        let streamed = self.steps.iter().take_while(|s| streams(s)).count();
        let (head, tail) = self.steps.split_at(streamed);
        // Promoted Headers takes the names from the first row of the first batch only; later
        // batches get those names.
        let mut headers: Vec<Option<SchemaRef>> = vec![None; head.len()];
        let mut errors = 0usize;
        let mut builder: Option<TableBuilder> = None;
        let source_errors =
            self.source
                .read(&self.name, usize::MAX, culture, limit, &mut |mut batch| {
                    for (step, names) in head.iter().zip(headers.iter_mut()) {
                        batch = match (step, &*names) {
                            (Step::PromotedHeaders, Some(names)) => rename(&batch, names)?,
                            _ => {
                                let promotes =
                                    *step == Step::PromotedHeaders && batch.num_rows() > 0;
                                let (next, n) = step.apply(&batch, culture)?;
                                errors = errors.saturating_add(n);
                                if promotes {
                                    *names = Some(next.schema());
                                }
                                next
                            }
                        };
                    }
                    builder
                        .get_or_insert_with(|| TableBuilder::new(&self.name, batch.schema(), limit))
                        .push(&batch)
                })?;
        let table = builder
            .ok_or_else(|| Error::Source {
                source: self.name.clone(),
                message: "the source gave no data".into(),
            })?
            .finish()?;
        if tail.is_empty() {
            return Ok((table, errors.saturating_add(source_errors)));
        }
        let mut batch = table.batch().clone();
        drop(table);
        for step in tail {
            let (next, n) = step.apply(&batch, culture)?;
            batch = next;
            errors = errors.saturating_add(n);
        }
        let mut b = TableBuilder::new(&self.name, batch.schema(), limit);
        b.push(&batch)?;
        Ok((b.finish()?, errors.saturating_add(source_errors)))
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
}
