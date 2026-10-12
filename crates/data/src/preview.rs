//! Preview (CONTEXT.md, spec Q-2): a Query's result at a selected Applied Step, on the first
//! [`PREVIEW_ROWS`] Source rows, with its Cell errors per cell — an error mask plus messages per
//! column (ADR 0005). Derived and cheap: nothing is stored.
//!
//! A Cell error is a value a Changed Type step couldn't produce (null in the data). Later steps
//! carry it: renames and Promoted Headers follow the column, and any other step (remove,
//! reorder, filter, merge…) is run once more with a hidden row-number column to see where each
//! row went. A step that drops that column drops the errors it can't place.
//!
//! **Through a merge**, the other Query's rows come from its loaded Table when that is loaded
//! and not Stale, else from its own Preview (its first 1,000 Source rows, at its last step); a
//! Preview never runs another Query in full. Power Query's editor works the same way in spirit:
//! it caches up to 1,000 preview rows per query and shows steps from that cache, flagging
//! preview data that is out of date (Microsoft Learn, "Disable Power Query background
//! refresh"). *Parity uncertainty* (to observe in the VM): whether Power Query's preview of a
//! merge joins the other query's cached preview, its full result, or something else; until
//! then, rows past the other side's first 1,000 may be missing from a merge preview of a Stale
//! or unloaded Query, though they load.

use std::sync::Arc;

use arrow_array::{Array, ArrayRef, RecordBatch, StringArray, UInt64Array};
use arrow_cast::cast;
use arrow_schema::{DataType, Field, Schema};

use crate::query::Ctx;
use crate::source::PREVIEW_ROWS;
use crate::step::{ColumnType, Step};
use crate::{Error, Query, Result, Table};

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
    /// A Merged Queries step fails here; use [`Query::preview_with`].
    pub fn preview(&self, step: Option<usize>, limit: usize) -> Result<Preview> {
        self.preview_with(&[], &[], step, limit)
    }

    /// [`Query::preview`], with `queries` (the Document's) supplying the other side of Merged
    /// Queries steps: the other Query's Table in `loaded` (the loaded Tables that aren't Stale),
    /// else that Query's own Preview at its last step. Never a full run of the other Query (see
    /// the module docs).
    pub fn preview_with<'a>(
        &'a self,
        queries: &'a [Query],
        loaded: &'a [&'a Table],
        step: Option<usize>,
        limit: usize,
    ) -> Result<Preview> {
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
        self.preview_in(&mut Ctx::preview(queries, loaded, limit), upto)
    }

    /// The Preview after the first `upto` steps, merges resolved by `ctx`.
    pub(crate) fn preview_in<'a>(&'a self, ctx: &mut Ctx<'a>, upto: usize) -> Result<Preview> {
        let (mut batch, _) =
            self.source
                .read_batch(&self.name, PREVIEW_ROWS, self.culture, ctx.limit())?;
        let mut errors = Errors::new();
        for step in self.steps.get(..upto).unwrap_or_default() {
            let (next, _) = ctx.apply(self, step, &batch)?;
            errors = carry(step, &batch, &next, errors, &mut |b| {
                ctx.apply(self, step, b).map(|r| r.0)
            });
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
    apply: &mut dyn FnMut(&RecordBatch) -> Result<RecordBatch>,
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
        _ => moved(before, errors, apply),
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
fn moved(
    before: &RecordBatch,
    errors: Errors,
    apply: &mut dyn FnMut(&RecordBatch) -> Result<RecordBatch>,
) -> Errors {
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
    let Ok(out) = apply(&tagged) else {
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
    use crate::culture::Culture;
    use crate::source::{Bytes, File, Source, csv};
    use crate::step::{JoinKind, Merge};

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

    #[test]
    fn previews_after_a_merge_resolve_the_other_query_and_keep_errors() {
        let mut targets = query("Region,Target\nNorth,10\nr1,5\n".into());
        targets.name = "Targets".into();
        let mut sales = sales();
        sales.steps.push(Step::MergedQueries(Merge {
            keys: vec!["Region".into()],
            query: "Targets".into(),
            other_keys: vec!["Region".into()],
            kind: JoinKind::LeftOuter,
            column: "Targets".into(),
            expand: vec![("Target".into(), "Targets.Target".into())],
        }));
        assert!(
            sales.preview(Some(2), usize::MAX).is_err(),
            "needs the queries"
        );
        let queries = [sales.clone(), targets];
        let p = sales
            .preview_with(&queries, &[], Some(2), usize::MAX)
            .unwrap();
        assert_eq!(p.batch.num_rows(), PREVIEW_ROWS - 1);
        assert_eq!(p.batch.schema().field(3).name(), "Targets.Target");
        assert!(p.error(250, 1).is_some_and(|m| m.ends_with("oops")));
        assert_eq!(p.error_count(), 1);
    }

    /// The rows asked of each Source read since the last call, by file.
    fn reads() -> Vec<(String, usize)> {
        crate::source::READS.with(|r| std::mem::take(&mut *r.borrow_mut()))
    }

    /// `Orders` (Region, Qty) merging `Targets` (5,000 rows: region `t<i>`, Target `i`), each
    /// from its own file.
    fn merged() -> [Query; 2] {
        let file = |name: &str, text: String| {
            let file = File::Bytes {
                name: name.into(),
                bytes: Bytes::from(text),
            };
            Source::csv(name, file, csv::Options::default()).unwrap()
        };
        let culture = Culture::get("en-US").unwrap();
        let mut csv = String::from("Region,Target\n");
        for i in 0..5000 {
            csv.push_str(&format!("t{i},{i}\n"));
        }
        let targets =
            Query::connect("Targets", file("targets.csv", csv), culture, usize::MAX).unwrap();
        let orders = "Region,Qty\nt5,1\nt4000,2\n".to_string();
        let mut orders =
            Query::connect("Orders", file("orders.csv", orders), culture, usize::MAX).unwrap();
        orders.steps.push(Step::MergedQueries(Merge {
            keys: vec!["Region".into()],
            query: "Targets".into(),
            other_keys: vec!["Region".into()],
            kind: JoinKind::LeftOuter,
            column: "Targets".into(),
            expand: vec![("Target".into(), "Targets.Target".into())],
        }));
        [orders, targets]
    }

    /// Targets.Target as text, per Orders row.
    fn targets(p: &Preview) -> Vec<Option<String>> {
        let c = cast(p.batch.column(2), &DataType::Utf8).unwrap();
        let c = c.as_any().downcast_ref::<StringArray>().unwrap();
        c.iter().map(|v| v.map(str::to_string)).collect()
    }

    #[test]
    fn a_merge_previews_against_the_other_querys_preview_never_its_full_run() {
        let queries = merged();
        reads();
        let p = queries[0]
            .preview_with(&queries, &[], Some(2), usize::MAX)
            .unwrap();
        // t4000 is past Targets' first 1,000 rows: not in its Preview (parity uncertainty).
        assert_eq!(targets(&p), [Some("5".into()), None]);
        let targets_reads: Vec<usize> = reads()
            .into_iter()
            .filter(|(f, _)| f == "targets.csv")
            .map(|(_, rows)| rows)
            .collect();
        assert_eq!(
            targets_reads,
            [PREVIEW_ROWS],
            "one Preview read, not a full run"
        );
    }

    #[test]
    fn a_merge_previews_against_the_other_querys_loaded_table() {
        let queries = merged();
        let (loaded, _) = queries[1].run(usize::MAX).unwrap();
        reads();
        let p = queries[0]
            .preview_with(&queries, &[&loaded], Some(2), usize::MAX)
            .unwrap();
        assert_eq!(targets(&p), [Some("5".into()), Some("4000".into())]);
        assert!(
            reads().iter().all(|(f, _)| f != "targets.csv"),
            "Targets isn't read"
        );
    }
}
