//! Queries (CONTEXT.md): an ordered list of Applied Steps that turns a Source into a table.

use crate::culture::Culture;
use crate::step::Step;
use crate::{Result, Table, TableBuilder};

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

impl Query {
    /// Runs every step over the source: the table to load (named after the Query, encoded narrow,
    /// at most `limit` bytes) and its Cell error count.
    pub fn run(&self, limit: usize) -> Result<(Table, usize)> {
        let mut batch = self.source.batch().clone();
        let mut errors = 0usize;
        for step in &self.steps {
            let (next, n) = step.apply(&batch, self.culture)?;
            batch = next;
            errors = errors.saturating_add(n);
        }
        let mut b = TableBuilder::new(&self.name, batch.schema(), limit);
        b.push(&batch)?;
        Ok((b.finish()?, errors))
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
}
