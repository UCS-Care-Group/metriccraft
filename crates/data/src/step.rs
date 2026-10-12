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
//! - Filtered Rows: a condition that is null in M (`null > 3`, `Text.Contains(null, "a")`)
//!   drops the row; `<>` keeps nulls (`null <> 3` is true); `=` between different types is
//!   false; ordering different types (`"a" > 3`) or a text operator on a non-text column fails
//!   the step (M raises an error). Text compares ordinally by code point, case-sensitive
//!   (M's default `Comparer.Ordinal` orders by UTF-16 unit, which differs only beyond U+FFFF).
//!   Whole numbers order against decimals as f64.
//! - Merged Queries: see `merge.rs`.
//! - Fixed decimal (`Currency.Type`): text parses exactly and rounds to 4 places half to even
//!   (see `culture.rs`); a decimal converts via its shortest round-trip digits, then the same
//!   rounding; a whole number exactly. Out of range (±922,337,203,685,477.5807) is a Cell
//!   error. Fixed decimal to text, whole or logical goes through the decimal (`1.5`, not
//!   `1.5000`).
//! - Parquet DECIMAL columns load as decimal (`type number`), not fixed decimal: we believe
//!   Power Query's `Parquet.Document` types them `type number`; unchecked in the VM.
//!
//! **Fixed decimal storage.** Arrow `Decimal64(18, 4)`: the exact value × 10⁴ in an `i64`
//! (Power BI's Currency range is the whole `i64`, so 19-digit values exceed the nominal
//! precision 18; we never validate precision). Self-describing, unlike a bare `Int64` × 10⁴, so
//! Previews, filters, merges and casts read it right; a loaded table narrows it to
//! `Decimal32(9, 4)` when every value fits an `i32` (`TableBuilder`). M3 DAX reads either as
//! scaled integers.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use arrow_array::types::Float64Type;
use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch,
    RecordBatchOptions, StringArray, TimestampMicrosecondArray,
};
use arrow_cast::{CastOptions, cast, cast_with_options};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
use arrow_select::filter::filter_record_batch;

use crate::cell::{Cell, Cells, is_text};
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
    /// `Table.SelectRows(prev, each c1 and c2 …)` (or `or`): keeps the rows where the
    /// conditions hold. An empty list keeps every row.
    FilteredRows {
        combine: Combine,
        conditions: Vec<Condition>,
    },
    /// `Table.NestedJoin` followed by `Table.ExpandTableColumn`: two M steps, one Applied Step
    /// here (there are no nested-table columns). Runs through [`crate::Query::run_with`], which
    /// supplies the other Query's result.
    MergedQueries(Merge),
}

/// Most conditions in one Filtered Rows step. Its M is one `and`/`or` chain, and the M parser
/// counts each operator as a nesting level (`opendash_m::MAX_DEPTH`), so a longer chain would
/// print M that doesn't parse back. The command, the M reader and the step itself enforce it.
pub const MAX_CONDITIONS: usize = 100;

/// The error for a Filtered Rows step with `n` > [`MAX_CONDITIONS`] conditions.
pub fn too_many_conditions(n: usize) -> String {
    format!("{n} conditions: a filter may have at most {MAX_CONDITIONS} conditions")
}

/// How a Filtered Rows step joins its conditions (M `and` / `or`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Combine {
    And,
    Or,
}

/// One filter condition on one column: `[column] <op> value`.
#[derive(Clone, Debug, PartialEq)]
pub struct Condition {
    pub column: String,
    pub op: Op,
    pub value: Value,
}

/// A filter operator, as Power Query's filter menus emit them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    /// `=` (with [`Value::Null`]: "is null").
    Equals,
    /// `<>` (with [`Value::Null`]: "is not null").
    NotEquals,
    /// `>`
    Greater,
    /// `>=`
    GreaterOrEqual,
    /// `<`
    Less,
    /// `<=`
    LessOrEqual,
    /// `Text.Contains([column], value)`
    Contains,
    /// `Text.StartsWith([column], value)`
    BeginsWith,
    /// `Text.EndsWith([column], value)`
    EndsWith,
}

/// An M literal in a step.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`
    Null,
    /// A number literal (`3`, `1.5`).
    Number(f64),
    /// `"text"`
    Text(String),
    /// `true` / `false`
    Logical(bool),
    /// `#date(y, m, d)`: days since 1970-01-01.
    Date(i32),
    /// `#datetime(y, m, d, h, min, s)`: microseconds since 1970-01-01 00:00.
    DateTime(i64),
}

impl Value {
    fn cell(&self) -> Cell<'_> {
        match self {
            Self::Null => Cell::Null,
            Self::Number(n) => Cell::Number(*n),
            Self::Text(s) => Cell::Text(s),
            Self::Logical(b) => Cell::Logical(*b),
            Self::Date(d) => Cell::Date(*d),
            Self::DateTime(t) => Cell::DateTime(*t),
        }
    }
}

/// A merge of another Query into this one.
#[derive(Clone, Debug, PartialEq)]
pub struct Merge {
    /// This table's key columns.
    pub keys: Vec<String>,
    /// The other Query (M: its name as an identifier, e.g. `Customers`).
    pub query: String,
    /// The other table's key columns, matched to `keys` in order.
    pub other_keys: Vec<String>,
    pub kind: JoinKind,
    /// `newColumnName` of `Table.NestedJoin`, the column `Table.ExpandTableColumn` expands
    /// (Power Query's default: the other Query's name).
    pub column: String,
    /// The other table's columns to expand, each with its new name (Power Query's default
    /// with "Use original column name as prefix": `<column>.<name>`).
    pub expand: Vec<(String, String)>,
}

/// `JoinKind.LeftOuter`, `JoinKind.Inner`, `JoinKind.FullOuter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinKind {
    LeftOuter,
    Inner,
    FullOuter,
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
            Self::FilteredRows { .. } => "Filtered Rows",
            Self::MergedQueries(_) => "Merged Queries",
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
            Self::FilteredRows {
                combine,
                conditions,
            } => {
                if conditions.len() > MAX_CONDITIONS {
                    return Err(err(too_many_conditions(conditions.len())));
                }
                let mut tests = Vec::with_capacity(conditions.len());
                for c in conditions {
                    let column = columns
                        .get(index(&c.column)?)
                        .ok_or_else(|| err(format!("the column `{}` wasn't found", c.column)))?;
                    let cells = Cells::new(column).ok_or_else(|| {
                        err(format!("the column `{}` can't be filtered yet", c.column))
                    })?;
                    tests.push((cells, c));
                }
                let mut keep = Vec::with_capacity(rows);
                for row in 0..rows {
                    let mut hits = tests.iter().map(|(cells, c)| holds(cells.get(row), c));
                    let hit = match combine {
                        Combine::And => hits.try_fold(true, |a, h| Ok::<_, String>(a && h?)),
                        Combine::Or => hits.try_fold(false, |a, h| Ok::<_, String>(a || h?)),
                    };
                    keep.push(hit.map_err(err)?);
                }
                let mask = BooleanArray::from(keep);
                let kept = filter_record_batch(batch, &mask).map_err(|e| err(e.to_string()))?;
                return Ok((kept, 0));
            }
            Self::MergedQueries(m) => {
                return Err(err(format!(
                    "merging `{}` needs the other queries (run the Query, not the step)",
                    m.query
                )));
            }
        }
        let options = RecordBatchOptions::new().with_row_count(Some(rows));
        let schema = Arc::new(Schema::new(fields));
        let batch = RecordBatch::try_new_with_options(schema, columns, &options)
            .map_err(|e| err(e.to_string()))?;
        Ok((batch, errors))
    }
}

/// Whether one filter condition holds for `cell`; an error where M raises one. M's three-valued
/// logic reduces to two values here: there is no `not`, and a null condition drops the row.
fn holds(cell: Cell<'_>, c: &Condition) -> std::result::Result<bool, String> {
    let value = c.value.cell();
    let order = |want: fn(Ordering) -> bool| {
        if cell == Cell::Null || value == Cell::Null {
            return Ok(false);
        }
        cell.compare(value).map(want).ok_or_else(|| {
            format!(
                "`{}` holds {cell:?}, which can't be compared with {:?}",
                c.column, c.value
            )
        })
    };
    let text = |f: fn(&str, &str) -> bool| match (cell, value) {
        (Cell::Text(a), Cell::Text(b)) => Ok(f(a, b)),
        (Cell::Null, Cell::Text(_)) => Ok(false),
        _ => Err(format!(
            "{:?} needs a text column and text value (`{}`)",
            c.op, c.column
        )),
    };
    match c.op {
        Op::Equals => Ok(cell.equals(value)),
        Op::NotEquals => Ok(!cell.equals(value)),
        Op::Greater => order(Ordering::is_gt),
        Op::GreaterOrEqual => order(Ordering::is_ge),
        Op::Less => order(Ordering::is_lt),
        Op::LessOrEqual => order(Ordering::is_le),
        Op::Contains => text(|a, b| a.contains(b)),
        Op::BeginsWith => text(|a, b| a.starts_with(b)),
        Op::EndsWith => text(|a, b| a.ends_with(b)),
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

/// The Changed Type kernel for any column (also the Model's data-type change): text parses with
/// the Culture, other types cast. Values that don't convert become null and count as Cell
/// errors. An error only if Arrow can't cast the column at all.
pub fn change_type(
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
    // Fixed decimal to anything but itself goes through the decimal it holds.
    let decimal = matches!(from, DataType::Decimal32(..) | DataType::Decimal64(..));
    if decimal && ty != ColumnType::FixedDecimal {
        let f = cast(column, &DataType::Float64).map_err(|e| e.to_string())?;
        return change_type(&f, ty, culture);
    }
    let number = from.is_numeric() || *from == DataType::Boolean;
    let temporal = matches!(
        from,
        DataType::Date32 | DataType::Date64 | DataType::Timestamp(..)
    );
    let supported = match ty {
        ColumnType::Text => true,
        ColumnType::Decimal | ColumnType::Whole | ColumnType::Boolean => number,
        ColumnType::FixedDecimal if number => return Ok(to_fixed(column)),
        ColumnType::FixedDecimal => false,
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

/// A number column as fixed decimal (exact × 10⁴), counting values out of range as Cell errors.
fn to_fixed(column: &ArrayRef) -> (ArrayRef, usize) {
    let values: Vec<Option<i64>> = match column.data_type() {
        DataType::Decimal32(_, 4) | DataType::Decimal64(_, 4) => {
            match cast(column, &FIXED).ok().and_then(|c| {
                c.as_any()
                    .downcast_ref::<arrow_array::Decimal64Array>()
                    .map(|a| a.iter().collect())
            }) {
                Some(v) => v,
                None => vec![None; column.len()],
            }
        }
        t if t.is_integer() || *t == DataType::Boolean => {
            let ints = cast(column, &DataType::Int64).ok();
            let ints = ints
                .as_ref()
                .and_then(|c| c.as_any().downcast_ref::<Int64Array>());
            match ints {
                Some(a) => a.iter().map(|v| v?.checked_mul(10_000)).collect(),
                None => vec![None; column.len()],
            }
        }
        _ => {
            let floats = cast(column, &DataType::Float64).ok();
            let floats = floats
                .as_ref()
                .and_then(|c| c.as_any().downcast_ref::<Float64Array>());
            match floats {
                Some(a) => a.iter().map(|v| fixed_from_f64(v?)).collect(),
                None => vec![None; column.len()],
            }
        }
    };
    let errors = values
        .iter()
        .zip(0..)
        .filter(|(v, i)| v.is_none() && column.is_valid(*i))
        .count();
    (fixed_array(values), errors)
}

/// A decimal as fixed decimal: its shortest round-trip digits, rounded to 4 places half to even.
pub fn fixed_from_f64(f: f64) -> Option<i64> {
    if !f.is_finite() {
        return None;
    }
    crate::culture::fixed(&format!("{f:e}"))
}

/// The Arrow type of a fixed decimal column in Applied Steps (see the module doc).
pub const FIXED: DataType = DataType::Decimal64(18, 4);

/// Fixed decimal values (× 10⁴) as a [`FIXED`] array.
pub fn fixed_array(values: Vec<Option<i64>>) -> ArrayRef {
    // `Decimal64Array` accepts any `Decimal64` type: this can't fail.
    Arc::new(arrow_array::Decimal64Array::from(values).with_data_type(FIXED))
}

/// A column type a Changed Type step can set (Power Query's `type text`, `type number`,
/// `Currency.Type`, `Int64.Type`, `type date`, `type datetime`, `type logical`), and a Model
/// Column's data type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnType {
    Text,
    Decimal,
    /// Fixed decimal number: exactly 4 decimal places (see the module doc for storage).
    FixedDecimal,
    Whole,
    Date,
    DateTime,
    Boolean,
}

impl ColumnType {
    /// Every type, in Power BI's Data type menu order.
    pub const ALL: [Self; 7] = [
        Self::Decimal,
        Self::FixedDecimal,
        Self::Whole,
        Self::DateTime,
        Self::Date,
        Self::Text,
        Self::Boolean,
    ];

    /// The name commands and `.odm` files use: `text`, `decimal`, `fixedDecimal`, `whole`,
    /// `date`, `datetime`, `boolean`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Decimal => "decimal",
            Self::FixedDecimal => "fixedDecimal",
            Self::Whole => "whole",
            Self::Date => "date",
            Self::DateTime => "datetime",
            Self::Boolean => "boolean",
        }
    }

    /// The type with this [`ColumnType::name`].
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The type of a column of Arrow type `t`, as stored in a loaded table (narrow integers,
    /// dictionary text); an all-null column is text.
    pub fn of_data_type(t: &DataType) -> Option<Self> {
        Some(match t {
            DataType::Decimal32(..) | DataType::Decimal64(..) => Self::FixedDecimal,
            t if t.is_integer() => Self::Whole,
            t if t.is_numeric() => Self::Decimal,
            DataType::Boolean => Self::Boolean,
            DataType::Date32 | DataType::Date64 => Self::Date,
            DataType::Timestamp(..) => Self::DateTime,
            DataType::Null => Self::Text,
            t if is_text(t) => Self::Text,
            _ => return None,
        })
    }

    /// Whether values of this type are numbers (Power BI sums them by default).
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::Decimal | Self::FixedDecimal | Self::Whole)
    }

    /// The plain Arrow type a Source batch carries for this column.
    pub fn data_type(self) -> DataType {
        match self {
            Self::Text => DataType::Utf8,
            Self::Decimal => DataType::Float64,
            Self::FixedDecimal => FIXED,
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
            Self::FixedDecimal => fixed_array(parse(text, e, |s| culture.parse_fixed(s))),
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
    fn a_filter_past_the_condition_cap_is_an_error() {
        let batch = RecordBatch::try_from_iter([(
            "A",
            Arc::new(arrow_array::Int64Array::from(vec![1])) as ArrayRef,
        )])
        .unwrap();
        let step = |n: usize| Step::FilteredRows {
            combine: Combine::Or,
            conditions: (0..n)
                .map(|i| Condition {
                    column: "A".into(),
                    op: Op::Equals,
                    value: Value::Number(i as f64),
                })
                .collect(),
        };
        assert_eq!(
            step(MAX_CONDITIONS)
                .apply(&batch, gb())
                .unwrap()
                .0
                .num_rows(),
            1
        );
        let e = step(MAX_CONDITIONS + 1).apply(&batch, gb()).unwrap_err();
        assert!(e.to_string().contains("conditions"), "{e}");
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

    fn fixed_values(a: &ArrayRef) -> Vec<Option<i64>> {
        assert_eq!(a.data_type(), &FIXED);
        let d = a
            .as_any()
            .downcast_ref::<arrow_array::Decimal64Array>()
            .unwrap();
        d.iter().collect()
    }

    #[test]
    fn fixed_decimal_is_exact() {
        let text = StringArray::from(vec![Some("0.1"), Some("0.2"), Some("1.23455"), Some("x")]);
        let (array, errors) = ColumnType::FixedDecimal.convert(&text, gb());
        assert_eq!(errors, 1);
        let v = fixed_values(&array);
        assert_eq!(v, [Some(1_000), Some(2_000), Some(12_346), None]);
        // 0.1 + 0.2 is exactly 0.3 in fixed decimal (not in f64).
        let sum = v[0].unwrap() + v[1].unwrap();
        assert_eq!(sum, 3_000);
        assert_ne!(0.1 + 0.2, 0.3);
        // Shown as a number: 0.3, through the decimal it holds.
        let back = ColumnType::Text.data_type();
        let (t, _) = change_type(&fixed_array(vec![Some(sum)]), ColumnType::Text, gb()).unwrap();
        assert_eq!(t.data_type(), &back);
        let t = t.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(t.value(0), "0.3");
    }

    #[test]
    fn fixed_decimal_converts_from_numbers_and_overflows_to_cell_errors() {
        let floats: ArrayRef = Arc::new(Float64Array::from(vec![
            Some(0.1),
            Some(2.00005),
            None,
            Some(1e300),
            Some(f64::NAN),
        ]));
        let (a, errors) = change_type(&floats, ColumnType::FixedDecimal, gb()).unwrap();
        assert_eq!(
            fixed_values(&a),
            [Some(1_000), Some(20_000), None, None, None]
        );
        assert_eq!(errors, 2);
        let ints: ArrayRef = Arc::new(Int64Array::from(vec![Some(3), Some(i64::MAX)]));
        let (a, errors) = change_type(&ints, ColumnType::FixedDecimal, gb()).unwrap();
        assert_eq!((fixed_values(&a), errors), (vec![Some(30_000), None], 1));
        // Back to whole: half to even, through the decimal.
        let (w, _) = change_type(
            &fixed_array(vec![Some(25_000), Some(35_000)]),
            ColumnType::Whole,
            gb(),
        )
        .unwrap();
        let w = w.as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(w.values(), &[2, 4]);
        // Never detected (Power Query doesn't detect Currency).
        assert_eq!(
            ColumnType::detect(["1.25", "3.5"], gb()),
            ColumnType::Decimal
        );
    }

    #[test]
    fn column_type_names_round_trip() {
        for t in ColumnType::ALL {
            assert_eq!(ColumnType::from_name(t.name()), Some(t));
            assert_eq!(ColumnType::of_data_type(&t.data_type()), Some(t));
        }
        assert_eq!(ColumnType::from_name("currency"), None);
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

    /// Region (dictionary text), Qty (Int8), Day (Date32), with nulls in each.
    fn filterable() -> RecordBatch {
        use arrow_array::types::Int8Type;
        use arrow_array::{DictionaryArray, Int8Array};
        let region: DictionaryArray<Int8Type> = vec![
            Some("North"),
            Some("South"),
            None,
            Some("Northeast"),
            Some("north"),
        ]
        .into_iter()
        .collect();
        RecordBatch::try_from_iter([
            ("Region", Arc::new(region) as ArrayRef),
            (
                "Qty",
                Arc::new(Int8Array::from(vec![
                    Some(3),
                    None,
                    Some(5),
                    Some(1),
                    Some(3),
                ])) as _,
            ),
            (
                "Day",
                Arc::new(Date32Array::from(vec![
                    Some(10),
                    Some(20),
                    None,
                    Some(30),
                    Some(40),
                ])) as _,
            ),
        ])
        .unwrap()
    }

    fn cond(column: &str, op: Op, value: Value) -> Condition {
        Condition {
            column: column.into(),
            op,
            value,
        }
    }

    /// The kept rows' positions in [`filterable`] (Qty is unique enough with Day).
    fn kept(combine: Combine, conditions: Vec<Condition>) -> Result<Vec<Option<i32>>> {
        let step = Step::FilteredRows {
            combine,
            conditions,
        };
        let (b, _) = run(step, &filterable())?;
        let day = b.column(2).as_any().downcast_ref::<Date32Array>().unwrap();
        Ok(day.iter().collect())
    }

    #[test]
    fn filter_single_conditions() {
        use Value::*;
        let t = |s: &str| Text(s.into());
        for (c, want) in [
            (
                cond("Qty", Op::Equals, Number(3.0)),
                vec![Some(10), Some(40)],
            ),
            (
                cond("Qty", Op::NotEquals, Number(3.0)),
                vec![Some(20), None, Some(30)],
            ),
            (cond("Qty", Op::Equals, Null), vec![Some(20)]),
            (
                cond("Qty", Op::NotEquals, Null),
                vec![Some(10), None, Some(30), Some(40)],
            ),
            (cond("Qty", Op::Greater, Number(3.0)), vec![None]),
            (
                cond("Qty", Op::GreaterOrEqual, Number(3.0)),
                vec![Some(10), None, Some(40)],
            ),
            (cond("Qty", Op::Less, Number(2.5)), vec![Some(30)]),
            (cond("Qty", Op::LessOrEqual, Number(1.0)), vec![Some(30)]),
            (cond("Qty", Op::Greater, Null), vec![]),
            (cond("Qty", Op::Equals, t("3")), vec![]),
            (cond("Region", Op::Equals, t("North")), vec![Some(10)]),
            (
                cond("Region", Op::Contains, t("orth")),
                vec![Some(10), Some(30), Some(40)],
            ),
            (
                cond("Region", Op::BeginsWith, t("North")),
                vec![Some(10), Some(30)],
            ),
            (
                cond("Region", Op::EndsWith, t("th")),
                vec![Some(10), Some(20), Some(40)],
            ),
            (cond("Region", Op::Less, t("O")), vec![Some(10), Some(30)]),
            (cond("Day", Op::Greater, Date(20)), vec![Some(30), Some(40)]),
            (cond("Day", Op::Equals, Null), vec![None]),
        ] {
            assert_eq!(kept(Combine::And, vec![c.clone()]).unwrap(), want, "{c:?}");
        }
    }

    #[test]
    fn filter_and_or() {
        let north = cond("Region", Op::BeginsWith, Value::Text("North".into()));
        let three = cond("Qty", Op::Equals, Value::Number(3.0));
        assert_eq!(
            kept(Combine::And, vec![north.clone(), three.clone()]).unwrap(),
            [Some(10)]
        );
        assert_eq!(
            kept(Combine::Or, vec![north, three]).unwrap(),
            [Some(10), Some(30), Some(40)]
        );
        assert_eq!(kept(Combine::And, vec![]).unwrap().len(), 5);
        assert_eq!(kept(Combine::Or, vec![]).unwrap().len(), 0);
    }

    #[test]
    fn filter_errors_where_m_does() {
        for (c, want) in [
            (
                cond("Qty", Op::Greater, Value::Text("a".into())),
                "can't be compared",
            ),
            (
                cond("Qty", Op::Contains, Value::Text("1".into())),
                "needs a text column",
            ),
            (
                cond("Region", Op::Contains, Value::Null),
                "needs a text column",
            ),
            (cond("Nope", Op::Equals, Value::Null), "`Nope` wasn't found"),
        ] {
            let e = kept(Combine::And, vec![c]).unwrap_err();
            assert!(e.to_string().contains(want), "{e}");
            assert!(e.to_string().starts_with("Filtered Rows: "), "{e}");
        }
    }

    #[test]
    fn filter_keeps_dictionary_encoding() {
        let step = Step::FilteredRows {
            combine: Combine::And,
            conditions: vec![cond("Qty", Op::Equals, Value::Number(3.0))],
        };
        let (b, _) = run(step, &filterable()).unwrap();
        assert_eq!(b.schema(), filterable().schema());
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
