//! Preview (CONTEXT.md, spec Q-2): a Query's result at a selected Applied Step, on the first
//! [`PREVIEW_ROWS`] Source rows, with its Cell errors per cell — an error mask plus messages per
//! column (ADR 0005). Derived and cheap: nothing is stored.
//!
//! A Cell error is a value a Changed Type step couldn't produce (null in the data). Later steps
//! carry it: renames and Promoted Headers follow the column, and any other step (remove,
//! reorder, filter, merge…) is run once more with a hidden row-number column to see where each
//! row went. A step that drops that column drops the errors it can't place.

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, RecordBatch, StringArray, UInt64Array};
use arrow_cast::cast;
use arrow_schema::{DataType, Field, Schema};

use crate::culture::Culture;
use crate::source::PREVIEW_ROWS;
use crate::step::{ColumnType, Step};
use crate::{Error, Query, Result};

/// A Query's result at one step, on the first [`PREVIEW_ROWS`] Source rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Preview {
    /// The rows (plain Arrow, not the narrow loaded encoding). A Cell error's value is null.
    pub batch: RecordBatch,
    /// Per column of `batch`: each row's Cell error message, null where the cell is fine (the
    /// validity is the error mask); `None` when the column has no errors.
    pub errors: Vec<Option<StringArray>>,
}

impl Preview {
    /// The Cell error message at `row`, `column`, if that cell is an error.
    pub fn error(&self, row: usize, column: usize) -> Option<&str> {
        let e = self.errors.get(column)?.as_ref()?;
        (row < e.len() && e.is_valid(row)).then(|| e.value(row))
    }

    pub fn error_count(&self) -> usize {
        self.errors
            .iter()
            .flatten()
            .map(|e| e.len().saturating_sub(e.null_count()))
            .sum()
    }
}

/// Cell error messages by column name.
type Errors = Vec<(String, StringArray)>;

/// The hidden row-number column used to follow rows through a step.
const ROW: &str = "\u{0}row";

impl Query {
    /// The Preview at Applied Step `step` (0 = first, as Applied Steps lists them), or of the
    /// Source itself for `None`. Reads only the first [`PREVIEW_ROWS`] rows of the Source,
    /// holding at most `limit` bytes. A step that can't run is an error naming it.
    pub fn preview(&self, step: Option<usize>, limit: usize) -> Result<Preview> {
        let upto = match step {
            None => 0,
            Some(i) if i < self.steps.len() => i.saturating_add(1),
            Some(i) => {
                return Err(Error::Step {
                    step: "Preview",
                    message: format!("there is no step {i}"),
                });
            }
        };
        let (mut batch, _) =
            self.source
                .read_batch(&self.name, PREVIEW_ROWS, self.culture, limit)?;
        let mut errors = Errors::new();
        for step in self.steps.get(..upto).unwrap_or_default() {
            let (next, _) = step.apply(&batch, self.culture)?;
            errors = carry(step, &batch, &next, errors, self.culture);
            batch = next;
        }
        let errors = batch
            .schema()
            .fields()
            .iter()
            .map(|f| {
                errors
                    .iter()
                    .find(|(n, _)| n == f.name())
                    .map(|(_, e)| e.clone())
            })
            .collect();
        Ok(Preview { batch, errors })
    }
}

/// The Cell errors after `step` turned `before` into `after`.
fn carry(
    step: &Step,
    before: &RecordBatch,
    after: &RecordBatch,
    mut errors: Errors,
    culture: &Culture,
) -> Errors {
    match step {
        Step::ChangedType(types) => {
            for (name, ty) in types {
                let (Some(old), Some(new)) =
                    (before.column_by_name(name), after.column_by_name(name))
                else {
                    continue;
                };
                let Some(found) = failures(old, new, *ty) else {
                    continue;
                };
                match errors.iter_mut().find(|(n, _)| n == name) {
                    Some((_, e)) => {
                        *e = e
                            .iter()
                            .zip(found.iter())
                            .map(|(a, b)| a.or(b).map(str::to_string))
                            .collect();
                    }
                    None => errors.push((name.clone(), found)),
                }
            }
            errors
        }
        // Columns keep their position; the first row is gone.
        Step::PromotedHeaders if before.num_rows() > 0 => {
            let (old, new) = (before.schema(), after.schema());
            errors
                .into_iter()
                .filter_map(|(name, e)| {
                    let i = old.index_of(&name).ok()?;
                    let name = new.fields().get(i)?.name().clone();
                    let rows = e.len().saturating_sub(1);
                    Some((name, e.slice(1, rows)))
                })
                .collect()
        }
        Step::PromotedHeaders => errors,
        Step::RenamedColumns(pairs) => errors
            .into_iter()
            .map(|(name, e)| {
                let renamed = pairs.iter().find(|(old, _)| *old == name);
                (renamed.map_or(name, |(_, new)| new.clone()), e)
            })
            .collect(),
        _ => moved(step, before, errors, culture),
    }
}

/// The cells `ty` couldn't produce: a value in `old` (not blank text), null in `new`.
fn failures(old: &ArrayRef, new: &ArrayRef, ty: ColumnType) -> Option<StringArray> {
    let text = cast(old, &DataType::Utf8).ok()?;
    let text = text.as_any().downcast_ref::<StringArray>()?;
    let rows = text.len().min(new.len());
    let mut any = false;
    let messages: StringArray = (0..rows)
        .map(|r| {
            let value = text
                .is_valid(r)
                .then(|| text.value(r))
                .filter(|v| !v.trim().is_empty())?;
            if new.is_valid(r) {
                return None;
            }
            any = true;
            Some(format!(
                "DataFormat.Error: We couldn't convert to {}. Details: {value}",
                type_name(ty)
            ))
        })
        .collect();
    any.then_some(messages)
}

/// The type's name in Power Query's conversion errors.
fn type_name(ty: ColumnType) -> &'static str {
    match ty {
        ColumnType::Text => "Text",
        ColumnType::Decimal | ColumnType::Whole => "Number",
        ColumnType::Date => "Date",
        ColumnType::DateTime => "DateTime",
        ColumnType::Boolean => "Logical",
    }
}

/// Errors after a step that moves, drops or repeats rows and columns: the step runs again with
/// a row-number column, which says where each row went; columns are matched by name.
fn moved(step: &Step, before: &RecordBatch, errors: Errors, culture: &Culture) -> Errors {
    if errors.is_empty() {
        return errors;
    }
    let ids: ArrayRef = Arc::new(UInt64Array::from_iter_values(
        (0..before.num_rows()).map(|i| u64::try_from(i).unwrap_or(u64::MAX)),
    ));
    let mut fields: Vec<Field> = before
        .schema()
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();
    fields.push(Field::new(ROW, DataType::UInt64, true));
    let mut columns = before.columns().to_vec();
    columns.push(ids);
    let Ok(tagged) = RecordBatch::try_new(Arc::new(Schema::new(fields)), columns) else {
        return Errors::new();
    };
    let Ok((out, _)) = step.apply(&tagged, culture) else {
        return Errors::new();
    };
    let Some(rows) = out.column_by_name(ROW) else {
        return Errors::new();
    };
    errors
        .into_iter()
        .filter(|(name, _)| out.column_by_name(name).is_some())
        .filter_map(|(name, e)| {
            let taken = arrow_select::take::take(&e, rows.as_ref(), None).ok()?;
            let taken = taken.as_any().downcast_ref::<StringArray>()?.clone();
            Some((name, taken))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{Bytes, File, Source, csv};

    fn query(text: String) -> Query {
        let file = File::Bytes {
            name: "sales.csv".into(),
            bytes: Bytes::from(text),
        };
        let source = Source::csv("Sales", file, csv::Options::default()).unwrap();
        Query::connect("Sales", source, Culture::get("en-US").unwrap(), usize::MAX).unwrap()
    }

    /// Region, Qty (whole; row 250 is "oops"), Price, past the 200 detection rows.
    fn sales() -> Query {
        let mut csv = String::from("Region,Qty,Price\n");
        for i in 0..250 {
            csv.push_str(&format!("r{},{i},{i}.5\n", i % 3));
        }
        csv.push_str("North,oops,1.5\n");
        for i in 0..10_000 {
            csv.push_str(&format!("South,{i},2\n"));
        }
        query(csv)
    }

    #[test]
    fn previews_the_first_1000_rows_at_each_step() {
        let mut q = sales();
        assert_eq!(q.steps.len(), 2, "{:?}", q.steps);
        let source = q.preview(None, usize::MAX).unwrap();
        assert_eq!(source.batch.num_rows(), PREVIEW_ROWS);
        assert_eq!(source.batch.schema().field(0).name(), "Column1");
        let headed = q.preview(Some(0), usize::MAX).unwrap();
        assert_eq!(headed.batch.num_rows(), PREVIEW_ROWS - 1);
        assert_eq!(headed.error_count(), 0);
        assert!(q.preview(Some(2), usize::MAX).is_err());
        q.steps.push(Step::RemovedColumns(vec!["Nope".into()]));
        let e = q.preview(Some(2), usize::MAX).unwrap_err();
        assert!(e.to_string().starts_with("Removed Columns"), "{e}");
    }

    #[test]
    fn shows_cell_errors_with_messages_at_the_change_type_step() {
        let p = sales().preview(Some(1), usize::MAX).unwrap();
        assert_eq!(p.error_count(), 1);
        assert!(p.errors[0].is_none() && p.errors[2].is_none());
        assert_eq!(
            p.error(250, 1),
            Some("DataFormat.Error: We couldn't convert to Number. Details: oops")
        );
        assert!(p.batch.column(1).is_null(250));
        assert_eq!(p.error(249, 1), None);
    }

    #[test]
    fn later_steps_carry_cell_errors() {
        let mut q = sales();
        q.steps.extend([
            Step::RenamedColumns(vec![("Qty".into(), "Quantity".into())]),
            Step::ReorderedColumns(vec!["Quantity".into(), "Region".into()]),
            Step::RemovedColumns(vec!["Price".into()]),
        ]);
        let p = q.preview(Some(4), usize::MAX).unwrap();
        assert_eq!(p.batch.schema().field(0).name(), "Quantity");
        assert_eq!(p.error_count(), 1);
        assert!(p.error(250, 0).is_some_and(|m| m.ends_with("oops")));
        // Removing the column removes its errors.
        q.steps.push(Step::RemovedColumns(vec!["Quantity".into()]));
        assert_eq!(q.preview(Some(5), usize::MAX).unwrap().error_count(), 0);
    }

    #[test]
    fn errors_before_promoted_headers_move_up_a_row() {
        let mut q = query("1,a\nx,b\n2,c\n".into());
        q.steps = vec![
            Step::ChangedType(vec![("Column1".into(), ColumnType::Whole)]),
            Step::PromotedHeaders,
        ];
        let p = q.preview(Some(1), usize::MAX).unwrap();
        assert_eq!(p.batch.schema().field(0).name(), "1");
        assert!(p.error(0, 0).is_some_and(|m| m.ends_with("Details: x")));
    }
}
