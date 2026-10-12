//! Applied Steps (CONTEXT.md) and their execution on Arrow data: the automatic steps a Source
//! adds (*Promoted Headers*, *Changed Type*) and the shaping steps rename, remove, reorder.
//!
//! Parity uncertainties, to settle with M1.10 cases in the VM:
//! - Promoted Headers names a column with a null/empty header cell `ColumnN`; on an empty table
//!   it changes nothing.
//! - Changed Type on a non-text column: numbers and booleans convert between each other
//!   (decimal to whole rounds half to even), dates and datetimes between each other, anything to
//!   text in Arrow's invariant format. Other pairs (date to number…) are Cell errors for now;
//!   Power Query gives serial numbers.
//! - Removing every column is an error (Power Query allows a table with no columns).

use std::collections::HashSet;
use std::sync::Arc;

use arrow_array::types::Float64Type;
use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch,
    RecordBatchOptions, StringArray, TimestampMicrosecondArray,
};
use arrow_cast::{CastOptions, cast, cast_with_options};
use arrow_schema::{DataType, Field, Schema, TimeUnit};

use crate::culture::Culture;
use crate::{Error, Result};

/// One recorded shaping operation.
// ponytail: Power Query merges a rename straight after a rename into one step and asks before
// stacking Changed Type steps; M1.7 decides whether we mirror that for M parity.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// `Table.PromoteHeaders`: the first row becomes the column names.
    PromotedHeaders,
    /// `Table.TransformColumnTypes`: column name → type.
    // ponytail: no per-step Culture override yet (CONTEXT.md allows one); the Query's applies.
    ChangedType(Vec<(String, ColumnType)>),
    /// `Table.RenameColumns`: old name → new name.
    RenamedColumns(Vec<(String, String)>),
    /// `Table.RemoveColumns`.
    RemovedColumns(Vec<String>),
    /// `Table.ReorderColumns`: the listed columns, in this order, fill the positions they held;
    /// unlisted columns stay put.
    ReorderedColumns(Vec<String>),
}

impl Step {
    /// The step's name in Power Query's Applied Steps (and in its M: `#"Renamed Columns"`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::PromotedHeaders => "Promoted Headers",
            Self::ChangedType(_) => "Changed Type",
            Self::RenamedColumns(_) => "Renamed Columns",
            Self::RemovedColumns(_) => "Removed Columns",
            Self::ReorderedColumns(_) => "Reordered Columns",
        }
    }

    /// Runs the step on `batch`, returning the result and its Cell error count. Columns the step
    /// doesn't change are shared, not copied.
    pub fn apply(&self, batch: &RecordBatch, culture: &Culture) -> Result<(RecordBatch, usize)> {
        let err = |message: String| Error::Step {
            step: self.name(),
            message,
        };
        let schema = batch.schema();
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        let index = |name: &str| {
            names
                .iter()
                .position(|n| *n == name)
                .ok_or_else(|| err(format!("the column `{name}` wasn't found")))
        };
        let mut fields: Vec<Field> = schema.fields().iter().map(|f| f.as_ref().clone()).collect();
        let mut columns = batch.columns().to_vec();
        let mut rows = batch.num_rows();
        let mut errors = 0usize;
        match self {
            Self::PromotedHeaders if rows > 0 => {
                fields = unique_names(columns.iter().map(first_text))
                    .into_iter()
                    .zip(&columns)
                    .map(|(n, c)| Field::new(n, c.data_type().clone(), true))
                    .collect();
                rows -= 1;
                columns = columns.iter().map(|c| c.slice(1, rows)).collect();
            }
            Self::PromotedHeaders => {}
            Self::ChangedType(types) => {
                for (name, ty) in types {
                    let i = index(name)?;
                    let (Some(field), Some(column)) = (fields.get_mut(i), columns.get_mut(i))
                    else {
                        return Err(err(format!("the column `{name}` wasn't found")));
                    };
                    let (array, n) = change_type(column, *ty, culture).map_err(err)?;
                    *field = Field::new(name, array.data_type().clone(), true);
                    *column = array;
                    errors = errors.saturating_add(n);
                }
            }
            Self::RenamedColumns(pairs) => {
                for (old, new) in pairs {
                    if new.is_empty() {
                        return Err(err(format!("`{old}` can't be renamed to an empty name")));
                    }
                    let i = index(old)?;
                    if let Some(f) = fields.get_mut(i) {
                        *f = f.clone().with_name(new);
                    }
                }
                let mut seen = HashSet::with_capacity(fields.len());
                if let Some(dup) = fields.iter().find(|f| !seen.insert(f.name())) {
                    return Err(err(format!("the column `{}` already exists", dup.name())));
                }
            }
            Self::RemovedColumns(remove) => {
                let mut gone = HashSet::with_capacity(remove.len());
                for name in remove {
                    gone.insert(index(name)?);
                }
                let keep: Vec<usize> = (0..fields.len()).filter(|i| !gone.contains(i)).collect();
                if keep.is_empty() {
                    return Err(err("a table needs at least one column".into()));
                }
                (fields, columns) = pick(&fields, &columns, &keep);
            }
            Self::ReorderedColumns(listed) => {
                let moved = listed
                    .iter()
                    .map(|n| index(n))
                    .collect::<Result<Vec<_>>>()?;
                let mut slots = moved.clone();
                slots.sort_unstable();
                if let Some(&[i, _]) = slots.windows(2).find(|w| w.first() == w.get(1)) {
                    let name = names.get(i).copied().unwrap_or_default();
                    return Err(err(format!("the column `{name}` is listed twice")));
                }
                let mut order: Vec<usize> = (0..fields.len()).collect();
                for (slot, i) in slots.iter().zip(moved) {
                    if let Some(o) = order.get_mut(*slot) {
                        *o = i;
                    }
                }
                (fields, columns) = pick(&fields, &columns, &order);
            }
        }
        let options = RecordBatchOptions::new().with_row_count(Some(rows));
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new_with_options(schema, columns, &options)
            .map_err(|e| err(e.to_string()))?;
        Ok((batch, errors))
    }
}

/// The fields and columns at `indices`, in that order.
fn pick(fields: &[Field], columns: &[ArrayRef], indices: &[usize]) -> (Vec<Field>, Vec<ArrayRef>) {
    indices
        .iter()
        .filter_map(|&i| Some((fields.get(i)?.clone(), Arc::clone(columns.get(i)?))))
        .unzip()
}

/// The first value of a column as text; `None` if null, empty or not a scalar.
fn first_text(column: &ArrayRef) -> Option<String> {
    let text = cast(&column.slice(0, 1), &DataType::Utf8).ok()?;
    let value = text
        .as_any()
        .downcast_ref::<StringArray>()?
        .iter()
        .next()??;
    (!value.is_empty()).then(|| value.to_string())
}

/// Column names from header values: missing ones become `ColumnN`, duplicates get `_1`, `_2`.
fn unique_names(headers: impl Iterator<Item = Option<String>>) -> Vec<String> {
    let mut seen = HashSet::new();
    headers
        .enumerate()
        .map(|(i, h)| {
            let base = h.unwrap_or_else(|| format!("Column{}", i.saturating_add(1)));
            let mut name = base.clone();
            let mut n = 0u32;
            while seen.contains(&name) {
                n = n.saturating_add(1);
                name = format!("{base}_{n}");
            }
            seen.insert(name.clone());
            name
        })
        .collect()
}

fn is_text(t: &DataType) -> bool {
    match t {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => true,
        DataType::Dictionary(_, v) => is_text(v),
        _ => false,
    }
}

/// The Changed Type kernel for any column: text parses with the Culture, other types cast.
/// Values that don't convert become null and count as Cell errors.
fn change_type(
    column: &ArrayRef,
    ty: ColumnType,
    culture: &Culture,
) -> std::result::Result<(ArrayRef, usize), String> {
    let from = column.data_type();
    if is_text(from) {
        let text = cast(column, &DataType::Utf8).map_err(|e| e.to_string())?;
        let text = text
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or("a text column didn't cast to Utf8")?;
        return Ok(ty.convert(text, culture));
    }
    let number = from.is_numeric() || *from == DataType::Boolean;
    let temporal = matches!(
        from,
        DataType::Date32 | DataType::Date64 | DataType::Timestamp(..)
    );
    let supported = match ty {
        ColumnType::Text => true,
        ColumnType::Decimal | ColumnType::Whole | ColumnType::Boolean => number,
        ColumnType::Date | ColumnType::DateTime => temporal,
    };
    if !supported {
        let errors = column.len().saturating_sub(column.null_count());
        let nulls = arrow_array::new_null_array(&ty.data_type(), column.len());
        return Ok((nulls, errors));
    }
    let input = if ty == ColumnType::Whole && from.is_floating() {
        let f = cast(column, &DataType::Float64).map_err(|e| e.to_string())?;
        let f = f
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or("a number column didn't cast to Float64")?;
        Arc::new(f.unary::<_, Float64Type>(f64::round_ties_even)) as ArrayRef
    } else {
        Arc::clone(column)
    };
    let safe = CastOptions {
        safe: true,
        ..CastOptions::default()
    };
    let out = cast_with_options(&input, &ty.data_type(), &safe).map_err(|e| e.to_string())?;
    let errors = out.null_count().saturating_sub(column.null_count());
    Ok((out, errors))
}

/// A column type a Changed Type step can set (Power Query's `type text`, `type number`,
/// `Int64.Type`, `type date`, `type datetime`, `type logical`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Text,
    Decimal,
    Whole,
    Date,
    DateTime,
    Boolean,
}

impl ColumnType {
    /// The plain Arrow type a Source batch carries for this column.
    pub fn data_type(self) -> DataType {
        match self {
            Self::Text => DataType::Utf8,
            Self::Decimal => DataType::Float64,
            Self::Whole => DataType::Int64,
            Self::Date => DataType::Date32,
            Self::DateTime => DataType::Timestamp(TimeUnit::Microsecond, None),
            Self::Boolean => DataType::Boolean,
        }
    }

    /// Converts text to this type. Empty text becomes null; text that doesn't convert is a
    /// Cell error: null here, and counted.
    // ponytail: the load path only needs the count. A Preview (M1.8) needs the error mask and
    // message per column (ADR 0005); return them from here then.
    pub fn convert(self, text: &StringArray, culture: &Culture) -> (ArrayRef, usize) {
        let mut errors = 0;
        let e = &mut errors;
        let array: ArrayRef = match self {
            Self::Text => Arc::new(text.clone()),
            Self::Decimal => Arc::new(Float64Array::from(parse(text, e, |s| {
                culture.parse_decimal(s)
            }))),
            Self::Whole => Arc::new(Int64Array::from(parse(text, e, |s| culture.parse_whole(s)))),
            Self::Date => Arc::new(Date32Array::from(parse(text, e, |s| culture.parse_date(s)))),
            Self::DateTime => Arc::new(TimestampMicrosecondArray::from(parse(text, e, |s| {
                culture.parse_datetime(s)
            }))),
            Self::Boolean => Arc::new(BooleanArray::from(parse(text, e, |s| {
                culture.parse_boolean(s)
            }))),
        };
        (array, errors)
    }

    /// The type of one value; `None` for empty text.
    pub fn of(value: &str, culture: &Culture) -> Option<Self> {
        Some(if value.trim().is_empty() {
            return None;
        } else if culture.parse_boolean(value).is_some() {
            Self::Boolean
        } else if culture.is_whole(value) {
            Self::Whole
        } else if culture.parse_decimal(value).is_some() {
            Self::Decimal
        } else if culture.parse_date(value).is_some() {
            Self::Date
        } else if culture.parse_datetime(value).is_some() {
            Self::DateTime
        } else {
            Self::Text
        })
    }

    /// The narrowest type that holds every value (empty values ignored; all empty is text).
    pub fn detect<'a>(values: impl IntoIterator<Item = &'a str>, culture: &Culture) -> Self {
        let mut found = None;
        for v in values {
            let Some(t) = Self::of(v, culture) else {
                continue;
            };
            found = Some(match (found, t) {
                (None, t) => t,
                (Some(a), b) if a == b => a,
                (Some(Self::Whole | Self::Decimal), Self::Whole | Self::Decimal) => Self::Decimal,
                (Some(Self::Date | Self::DateTime), Self::Date | Self::DateTime) => Self::DateTime,
                _ => return Self::Text,
            });
        }
        found.unwrap_or(Self::Text)
    }
}

/// Parses each non-empty value, counting the ones that don't parse.
fn parse<T>(
    text: &StringArray,
    errors: &mut usize,
    f: impl Fn(&str) -> Option<T>,
) -> Vec<Option<T>> {
    text.iter()
        .map(|v| match v {
            Some(s) if !s.trim().is_empty() => {
                let r = f(s);
                *errors += usize::from(r.is_none());
                r
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use arrow_array::Array;

    use super::*;

    fn gb() -> &'static Culture {
        Culture::get("en-GB").unwrap()
    }

    #[test]
    fn detects_the_narrowest_type() {
        let cases: [(&[&str], ColumnType); 8] = [
            (&["1", "", "-20"], ColumnType::Whole),
            (&["1", "2.5"], ColumnType::Decimal),
            (&["TRUE", "false"], ColumnType::Boolean),
            (&["01/02/2026", "2026-12-31"], ColumnType::Date),
            (&["01/02/2026", "01/02/2026 10:00"], ColumnType::DateTime),
            (&["1", "x"], ColumnType::Text),
            (&["1", "true"], ColumnType::Text),
            (&["", " "], ColumnType::Text),
        ];
        for (values, want) in cases {
            assert_eq!(
                ColumnType::detect(values.iter().copied(), gb()),
                want,
                "{values:?}"
            );
        }
    }

    #[test]
    fn convert_counts_cell_errors() {
        let text = StringArray::from(vec![Some("1.5"), Some("abc"), Some(""), None, Some("x")]);
        let (array, errors) = ColumnType::Decimal.convert(&text, gb());
        assert_eq!(errors, 2);
        let v = array.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(
            v.iter().collect::<Vec<_>>(),
            [Some(1.5), None, None, None, None]
        );
        let (array, errors) = ColumnType::Text.convert(&text, gb());
        assert_eq!((errors, array.len()), (0, 5));
    }

    /// `Column1..N` text columns, like a raw CSV Source.
    fn raw(rows: &[&[Option<&str>]]) -> RecordBatch {
        let width = rows.first().map_or(0, |r| r.len());
        let cols = (0..width).map(|c| {
            let values: Vec<Option<&str>> = rows.iter().map(|r| r[c]).collect();
            (
                format!("Column{}", c + 1),
                Arc::new(StringArray::from(values)) as ArrayRef,
            )
        });
        RecordBatch::try_from_iter(cols).unwrap()
    }

    fn sales() -> RecordBatch {
        raw(&[
            &[Some("Region"), Some("Qty"), Some("Price")],
            &[Some("North"), Some("3"), Some("1.5")],
            &[None, Some("x"), None],
        ])
    }

    fn run(step: Step, batch: &RecordBatch) -> Result<(RecordBatch, usize)> {
        step.apply(batch, gb())
    }

    fn names(batch: &RecordBatch) -> Vec<String> {
        batch
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    fn col(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn promoted() -> RecordBatch {
        run(Step::PromotedHeaders, &sales()).unwrap().0
    }

    #[test]
    fn promote_headers_uses_the_first_row() {
        let (b, errors) = run(Step::PromotedHeaders, &sales()).unwrap();
        assert_eq!(
            (names(&b), b.num_rows(), errors),
            (col(&["Region", "Qty", "Price"]), 2, 0)
        );
        let region = b.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(region.iter().collect::<Vec<_>>(), [Some("North"), None]);
    }

    #[test]
    fn promote_headers_fills_missing_and_duplicate_names() {
        let b = raw(&[&[Some("A"), None, Some("A"), Some("")], &[Some("1"); 4]]);
        let (b, _) = run(Step::PromotedHeaders, &b).unwrap();
        assert_eq!(names(&b), col(&["A", "Column2", "A_1", "Column4"]));
    }

    #[test]
    fn promote_headers_on_an_empty_table_changes_nothing() {
        let empty = sales().slice(0, 0);
        let (b, _) = run(Step::PromotedHeaders, &empty).unwrap();
        assert_eq!((names(&b), b.num_rows()), (names(&empty), 0));
        let none = RecordBatch::new_empty(Arc::new(Schema::empty()));
        assert_eq!(run(Step::PromotedHeaders, &none).unwrap().0, none);
    }

    #[test]
    fn promote_headers_names_from_non_text_values() {
        let b = RecordBatch::try_from_iter([(
            "x",
            Arc::new(Int64Array::from(vec![Some(2026), None])) as ArrayRef,
        )])
        .unwrap();
        assert_eq!(names(&run(Step::PromotedHeaders, &b).unwrap().0), ["2026"]);
    }

    #[test]
    fn change_type_converts_and_counts_cell_errors() {
        let step = Step::ChangedType(vec![
            ("Qty".into(), ColumnType::Whole),
            ("Price".into(), ColumnType::Decimal),
        ]);
        let input = promoted();
        let (b, errors) = run(step, &input).unwrap();
        assert_eq!(errors, 1, "`x` is not a whole number; null stays null");
        let qty = b.column(1).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(qty.iter().collect::<Vec<_>>(), [Some(3), None]);
        assert_eq!(b.schema().field(2).data_type(), &DataType::Float64);
        assert!(
            Arc::ptr_eq(b.column(0), input.column(0)),
            "untouched column shared"
        );
    }

    #[test]
    fn change_type_from_typed_columns() {
        let b = RecordBatch::try_from_iter([
            (
                "n",
                Arc::new(Float64Array::from(vec![
                    Some(2.5),
                    Some(3.5),
                    None,
                    Some(1e300),
                ])) as ArrayRef,
            ),
            (
                "d",
                Arc::new(Date32Array::from(vec![Some(0), None, Some(1), Some(2)])) as ArrayRef,
            ),
        ])
        .unwrap();
        let step = Step::ChangedType(vec![
            ("n".into(), ColumnType::Whole),
            ("d".into(), ColumnType::Decimal),
        ]);
        let (out, errors) = run(step, &b).unwrap();
        let n = out.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(
            n.iter().collect::<Vec<_>>(),
            [Some(2), Some(4), None, None],
            "half to even; out of range is a Cell error"
        );
        assert_eq!(
            out.column(1).null_count(),
            4,
            "date to number isn't supported yet"
        );
        assert_eq!(errors, 1 + 3);
        let step = Step::ChangedType(vec![("n".into(), ColumnType::Text)]);
        let (out, errors) = run(step, &b).unwrap();
        assert_eq!(
            (out.schema().field(0).data_type(), errors),
            (&DataType::Utf8, 0)
        );
    }

    #[test]
    fn change_type_of_dictionary_text() {
        use arrow_array::DictionaryArray;
        use arrow_array::types::Int8Type;
        let dict: DictionaryArray<Int8Type> = vec![Some("1"), None, Some("2"), Some("1")]
            .into_iter()
            .collect();
        let b = RecordBatch::try_from_iter([("q", Arc::new(dict) as ArrayRef)]).unwrap();
        let step = Step::ChangedType(vec![("q".into(), ColumnType::Whole)]);
        let (out, errors) = run(step, &b).unwrap();
        let q = out.column(0).as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(
            (q.iter().collect::<Vec<_>>(), errors),
            (vec![Some(1), None, Some(2), Some(1)], 0)
        );
    }

    #[test]
    fn rename_columns() {
        let step = Step::RenamedColumns(vec![("Qty".into(), "Quantity".into())]);
        let (b, _) = run(step, &promoted()).unwrap();
        assert_eq!(names(&b), col(&["Region", "Quantity", "Price"]));
        assert_eq!(b.column(1), promoted().column(1), "data unchanged");
        let swap = Step::RenamedColumns(vec![
            ("Qty".into(), "Price".into()),
            ("Price".into(), "Qty".into()),
        ]);
        assert_eq!(
            names(&run(swap, &promoted()).unwrap().0),
            col(&["Region", "Price", "Qty"])
        );
    }

    #[test]
    fn rename_errors() {
        for (pairs, want) in [
            (vec![("Qty", "Region")], "`Region` already exists"),
            (vec![("Qty", "A"), ("Price", "A")], "`A` already exists"),
            (vec![("Nope", "A")], "`Nope` wasn't found"),
            (vec![("Qty", "")], "empty name"),
        ] {
            let pairs = pairs
                .into_iter()
                .map(|(a, b)| (a.into(), b.into()))
                .collect();
            let e = run(Step::RenamedColumns(pairs), &promoted()).unwrap_err();
            assert!(e.to_string().contains(want), "{e}");
            assert!(e.to_string().starts_with("Renamed Columns: "), "{e}");
        }
    }

    #[test]
    fn remove_columns() {
        let step = Step::RemovedColumns(col(&["Qty", "Region", "Qty"]));
        let (b, _) = run(step, &promoted()).unwrap();
        assert_eq!((names(&b), b.num_rows()), (col(&["Price"]), 2));
        let e = run(Step::RemovedColumns(col(&["Nope"])), &promoted()).unwrap_err();
        assert!(e.to_string().contains("`Nope` wasn't found"), "{e}");
        let all = Step::RemovedColumns(col(&["Region", "Qty", "Price"]));
        assert!(run(all, &promoted()).is_err());
    }

    #[test]
    fn reorder_columns() {
        let full = Step::ReorderedColumns(col(&["Price", "Region", "Qty"]));
        let (b, _) = run(full, &promoted()).unwrap();
        assert_eq!(names(&b), col(&["Price", "Region", "Qty"]));
        assert_eq!(b.column(0), promoted().column(2));
        let partial = Step::ReorderedColumns(col(&["Price", "Region"]));
        let (b, _) = run(partial, &promoted()).unwrap();
        assert_eq!(names(&b), col(&["Price", "Qty", "Region"]), "Qty stays put");
        for (bad, want) in [
            (col(&["Qty", "Nope"]), "`Nope` wasn't found"),
            (col(&["Qty", "Region", "Qty"]), "`Qty` is listed twice"),
        ] {
            let e = run(Step::ReorderedColumns(bad), &promoted()).unwrap_err();
            assert!(e.to_string().contains(want), "{e}");
        }
    }
}
