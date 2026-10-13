//! The storage tier (ADR 0006): the model's Tables as encoded column readers, row masks
//! ([`Bitset`]), the filter context's filters, and the **storage scan** — one pass over one
//! Table under a row mask, checking the cancel flag between scans and every [`CHUNK`] rows.
//!
//! Columns are read in their loaded encoding (ADR 0005): dictionary text by code (filters on
//! text are code bitsets, a Relationship key match is decided once per dictionary entry), narrow
//! integers and fixed decimals widened per value.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};

use arrow_array::cast::AsArray;
use arrow_array::types::{
    Date32Type, Decimal32Type, Decimal64Type, Float32Type, Float64Type, Int8Type, Int16Type,
    Int32Type, Int64Type,
};
use arrow_array::{Array, ArrayRef, BooleanArray, PrimitiveArray, StringArray};
use arrow_schema::{DataType, TimeUnit};
use opendash_core::{CURRENCY_SCALE, Value, fold};

use super::{DaxError, Result};

/// Rows per scan chunk: the cancel flag is checked before each.
pub const CHUNK: usize = 64 * 1024;

/// Days from 1899-12-30 (DAX DateTime's epoch) to 1970-01-01 (Arrow's).
const UNIX_EPOCH_DAYS: f64 = 25_569.0;

/// The cancel flag, checked by every scan.
#[derive(Clone, Copy, Default)]
pub struct Cancel<'c>(pub Option<&'c AtomicBool>);

impl Cancel<'_> {
    pub fn check(&self) -> Result<()> {
        match self.0 {
            Some(flag) if flag.load(Ordering::Relaxed) => Err(DaxError::cancelled()),
            _ => Ok(()),
        }
    }
}

/// A set of row (or dictionary code) numbers `0..len`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitset {
    words: Vec<u64>,
    len: usize,
}

impl Bitset {
    /// `len` bits, all `on` or all off.
    pub fn new(len: usize, on: bool) -> Self {
        let n = len.div_ceil(64);
        let mut words = vec![if on { u64::MAX } else { 0 }; n];
        if on
            && !len.is_multiple_of(64)
            && let Some(last) = words.last_mut()
        {
            *last = (1u64 << (len % 64)) - 1;
        }
        Self { words, len }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, i: usize) -> bool {
        self.words
            .get(i / 64)
            .is_some_and(|w| (w >> (i % 64)) & 1 == 1)
    }

    pub fn set(&mut self, i: usize) {
        if i < self.len
            && let Some(w) = self.words.get_mut(i / 64)
        {
            *w |= 1u64 << (i % 64);
        }
    }

    /// Keeps only the bits also in `other`.
    pub fn and_with(&mut self, other: &Bitset) {
        for (i, w) in self.words.iter_mut().enumerate() {
            *w &= other.words.get(i).copied().unwrap_or(0);
        }
    }

    pub fn count(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// The set bits in `start..end`, in order.
    pub fn ones_in(&self, start: usize, end: usize) -> impl Iterator<Item = usize> + '_ {
        let end = end.min(self.len);
        let (first, last) = (start / 64, end.div_ceil(64));
        self.words
            .get(first..last.max(first))
            .unwrap_or(&[])
            .iter()
            .enumerate()
            .flat_map(move |(k, &w)| {
                let base = (first + k) * 64;
                let mut w = w;
                std::iter::from_fn(move || {
                    if w == 0 {
                        return None;
                    }
                    let bit = w.trailing_zeros() as usize;
                    w &= w - 1;
                    Some(base + bit)
                })
            })
            .filter(move |&i| i >= start && i < end)
    }
}

/// `a ∧ b`, where `None` is "every row".
pub fn and(a: Option<Bitset>, b: Bitset) -> Option<Bitset> {
    Some(match a {
        None => b,
        Some(mut a) => {
            a.and_with(&b);
            a
        }
    })
}

/// The storage scan: calls `f` for every row of `0..rows` in `mask` (all rows for `None`), in
/// order, checking `cancel` first and every [`CHUNK`] rows.
pub fn scan(
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    mut f: impl FnMut(usize) -> Result<()>,
) -> Result<()> {
    cancel.check()?;
    let mut start = 0;
    while start < rows {
        cancel.check()?;
        let end = start.saturating_add(CHUNK).min(rows);
        match mask {
            None => {
                for r in start..end {
                    f(r)?;
                }
            }
            Some(m) => {
                for r in m.ones_in(start, end) {
                    f(r)?;
                }
            }
        }
        start = end;
    }
    Ok(())
}

/// The number of rows a scan of `mask` visits.
pub fn count(rows: usize, mask: Option<&Bitset>, cancel: Cancel<'_>) -> Result<usize> {
    cancel.check()?;
    Ok(match mask {
        None => rows,
        Some(m) => m.count(),
    })
}

/// A value as a hashable key: numbers that are equal compare equal whatever their type (2,
/// 2.0, 2.0000 currency); text folded.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Int(i64),
    Float(u64),
    Text(String),
    Bool(bool),
}

impl Key {
    /// `None` for BLANK. `trim`: ignore trailing spaces in text (Relationship keys).
    pub fn of(v: &Value, trim: bool) -> Option<Key> {
        let float = |f: f64| {
            if f.fract() == 0.0 && f.abs() < 9.0e15 {
                Key::Int(f as i64)
            } else if f == 0.0 {
                Key::Int(0)
            } else if f.is_nan() {
                Key::Float(f64::NAN.to_bits())
            } else {
                Key::Float(f.to_bits())
            }
        };
        Some(match v {
            Value::Blank => return None,
            Value::Boolean(b) => Key::Bool(*b),
            Value::Integer(i) => Key::Int(*i),
            Value::Currency(c) if c % CURRENCY_SCALE == 0 => Key::Int(c / CURRENCY_SCALE),
            Value::Currency(c) => float(*c as f64 / CURRENCY_SCALE as f64),
            Value::Decimal(d) | Value::DateTime(d) => float(*d),
            Value::Text(s) if trim => Key::Text(fold(s.trim_end_matches(' '))),
            Value::Text(s) => Key::Text(fold(s)),
        })
    }
}

/// Dictionary keys of an encoded text column.
#[derive(Clone, Debug)]
pub enum Codes {
    I8(PrimitiveArray<Int8Type>),
    I16(PrimitiveArray<Int16Type>),
    I32(PrimitiveArray<Int32Type>),
}

/// One loaded column, as the evaluator reads it.
#[derive(Clone, Debug)]
pub enum Col {
    /// Dictionary text: codes into `values`.
    Dict {
        codes: Codes,
        values: StringArray,
    },
    Text(StringArray),
    I8(PrimitiveArray<Int8Type>),
    I16(PrimitiveArray<Int16Type>),
    I32(PrimitiveArray<Int32Type>),
    I64(PrimitiveArray<Int64Type>),
    F32(PrimitiveArray<Float32Type>),
    F64(PrimitiveArray<Float64Type>),
    /// Decimal with `scale` digits after the point (4: Currency).
    D32(PrimitiveArray<Decimal32Type>, i8),
    D64(PrimitiveArray<Decimal64Type>, i8),
    Bool(BooleanArray),
    Date(PrimitiveArray<Date32Type>),
    /// Timestamps as days: `value / per_day`.
    Ts(PrimitiveArray<Int64Type>, f64),
    /// Every value BLANK (an all-null column, or a type we can't read).
    Null(usize),
}

impl Col {
    /// A reader for `array`; types beyond our loaded encodings are read as text.
    pub fn new(array: &ArrayRef) -> Col {
        let len = array.len();
        let ts = |per_day: f64| -> Col {
            let raw = arrow_cast::cast(array, &DataType::Int64)
                .ok()
                .and_then(|a| a.as_primitive_opt::<Int64Type>().cloned());
            match raw {
                Some(a) => Col::Ts(a, per_day),
                None => Col::Null(len),
            }
        };
        let col = match array.data_type() {
            DataType::Dictionary(key, value) if value.as_ref() == &DataType::Utf8 => {
                let dict = |values: &ArrayRef| values.as_string_opt::<i32>().cloned();
                match key.as_ref() {
                    DataType::Int8 => array.as_dictionary_opt::<Int8Type>().and_then(|d| {
                        Some(Col::Dict {
                            codes: Codes::I8(d.keys().clone()),
                            values: dict(d.values())?,
                        })
                    }),
                    DataType::Int16 => array.as_dictionary_opt::<Int16Type>().and_then(|d| {
                        Some(Col::Dict {
                            codes: Codes::I16(d.keys().clone()),
                            values: dict(d.values())?,
                        })
                    }),
                    DataType::Int32 => array.as_dictionary_opt::<Int32Type>().and_then(|d| {
                        Some(Col::Dict {
                            codes: Codes::I32(d.keys().clone()),
                            values: dict(d.values())?,
                        })
                    }),
                    _ => None,
                }
            }
            DataType::Utf8 => array.as_string_opt::<i32>().cloned().map(Col::Text),
            DataType::Int8 => array.as_primitive_opt().cloned().map(Col::I8),
            DataType::Int16 => array.as_primitive_opt().cloned().map(Col::I16),
            DataType::Int32 => array.as_primitive_opt().cloned().map(Col::I32),
            DataType::Int64 => array.as_primitive_opt().cloned().map(Col::I64),
            DataType::Float32 => array.as_primitive_opt().cloned().map(Col::F32),
            DataType::Float64 => array.as_primitive_opt().cloned().map(Col::F64),
            DataType::Decimal32(_, s) => array.as_primitive_opt().cloned().map(|a| Col::D32(a, *s)),
            DataType::Decimal64(_, s) => array.as_primitive_opt().cloned().map(|a| Col::D64(a, *s)),
            DataType::Boolean => array.as_boolean_opt().cloned().map(Col::Bool),
            DataType::Date32 => array.as_primitive_opt().cloned().map(Col::Date),
            DataType::Timestamp(unit, _) => Some(ts(match unit {
                TimeUnit::Second => 86_400.0,
                TimeUnit::Millisecond => 86_400e3,
                TimeUnit::Microsecond => 86_400e6,
                TimeUnit::Nanosecond => 86_400e9,
            })),
            DataType::Null => Some(Col::Null(len)),
            _ => None,
        };
        col.unwrap_or_else(|| {
            arrow_cast::cast(array, &DataType::Utf8)
                .ok()
                .and_then(|a| a.as_string_opt::<i32>().cloned())
                .map_or(Col::Null(len), Col::Text)
        })
    }

    /// The dictionary of a dictionary column.
    pub fn dictionary(&self) -> Option<&StringArray> {
        match self {
            Col::Dict { values, .. } => Some(values),
            _ => None,
        }
    }

    /// The dictionary code at `row` of a dictionary column; `None` for BLANK, another kind of
    /// column, or a row out of range.
    pub fn code(&self, row: usize) -> Option<usize> {
        fn get<T: arrow_array::ArrowPrimitiveType>(
            a: &PrimitiveArray<T>,
            row: usize,
        ) -> Option<T::Native> {
            (row < a.len() && a.is_valid(row)).then(|| a.value(row))
        }
        let Col::Dict { codes, values } = self else {
            return None;
        };
        let code = match codes {
            Codes::I8(a) => usize::try_from(get(a, row)?).ok(),
            Codes::I16(a) => usize::try_from(get(a, row)?).ok(),
            Codes::I32(a) => usize::try_from(get(a, row)?).ok(),
        }?;
        (code < values.len() && values.is_valid(code)).then_some(code)
    }

    /// The value at `row`; BLANK for a null or a row out of range.
    pub fn value(&self, row: usize) -> Value {
        fn get<T: arrow_array::ArrowPrimitiveType>(
            a: &PrimitiveArray<T>,
            row: usize,
        ) -> Option<T::Native> {
            (row < a.len() && a.is_valid(row)).then(|| a.value(row))
        }
        let decimal = |raw: i64, scale: i8| {
            if scale == 4 {
                Value::Currency(raw)
            } else {
                Value::Decimal(raw as f64 / 10f64.powi(i32::from(scale)))
            }
        };
        let v = match self {
            Col::Dict { values, .. } => self.code(row).map(|c| Value::text(values.value(c))),
            Col::Text(a) => (row < a.len() && a.is_valid(row)).then(|| Value::text(a.value(row))),
            Col::I8(a) => get(a, row).map(|v| Value::Integer(v.into())),
            Col::I16(a) => get(a, row).map(|v| Value::Integer(v.into())),
            Col::I32(a) => get(a, row).map(|v| Value::Integer(v.into())),
            Col::I64(a) => get(a, row).map(Value::Integer),
            Col::F32(a) => get(a, row).map(|v| Value::Decimal(v.into())),
            Col::F64(a) => get(a, row).map(Value::Decimal),
            Col::D32(a, s) => get(a, row).map(|v| decimal(v.into(), *s)),
            Col::D64(a, s) => get(a, row).map(|v| decimal(v, *s)),
            Col::Bool(a) => {
                (row < a.len() && a.is_valid(row)).then(|| Value::Boolean(a.value(row)))
            }
            Col::Date(a) => get(a, row).map(|d| Value::DateTime(f64::from(d) + UNIX_EPOCH_DAYS)),
            Col::Ts(a, per_day) => {
                get(a, row).map(|t| Value::DateTime(t as f64 / per_day + UNIX_EPOCH_DAYS))
            }
            Col::Null(_) => None,
        };
        v.unwrap_or(Value::Blank)
    }

    /// Bytes a materialised value of this column costs beyond its slot (its text).
    pub fn text_bytes(&self, row: usize) -> usize {
        match self {
            Col::Dict { values, .. } => self
                .code(row)
                .map_or(0, |c| values.value_length(c) as usize),
            Col::Text(a) if row < a.len() => a.value_length(row) as usize,
            _ => 0,
        }
    }
}

/// A Table as the evaluator sees it.
#[derive(Clone, Debug)]
pub struct DbTable {
    pub name: String,
    pub columns: Vec<DbColumn>,
    pub rows: usize,
}

#[derive(Clone, Debug)]
pub struct DbColumn {
    pub name: String,
    pub data: Col,
}

impl DbTable {
    /// The column named `name` (ignoring case, as DAX names are).
    pub fn column(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| crate::same_name(&c.name, name))
    }
}

/// Which values of one column a filter allows.
#[derive(Clone, Debug, PartialEq)]
pub enum Allowed {
    /// Dictionary codes of a text column.
    Codes { codes: Bitset, blank: bool },
    /// Values of any other column, as [`Key`]s.
    Values { keys: HashSet<Key>, blank: bool },
}

/// One filter of a filter context (ADR 0006): a table over a set of columns.
#[derive(Clone, Debug, PartialEq)]
pub enum Filter {
    /// Rows of Table `table` whose column `column` holds an allowed value.
    Column {
        table: usize,
        column: usize,
        allowed: Allowed,
    },
    /// A row set of Table `table` (a multi-column filter, e.g. from `FILTER` over the table).
    Rows { table: usize, rows: Bitset },
}

impl Filter {
    /// A single-column filter allowing exactly `values` (BLANK among them allows BLANK).
    pub fn values(db: &DbTable, table: usize, column: usize, values: &[Value]) -> Filter {
        let blank = values.iter().any(Value::is_blank);
        let keys: HashSet<Key> = values.iter().filter_map(|v| Key::of(v, false)).collect();
        let col = db.columns.get(column).map(|c| &c.data);
        let allowed = match col.and_then(Col::dictionary) {
            Some(dict) => {
                let mut codes = Bitset::new(dict.len(), false);
                for (i, s) in dict.iter().enumerate() {
                    if s.is_some_and(|s| keys.contains(&Key::Text(fold(s)))) {
                        codes.set(i);
                    }
                }
                Allowed::Codes { codes, blank }
            }
            None => Allowed::Values { keys, blank },
        };
        Filter::Column {
            table,
            column,
            allowed,
        }
    }

    pub fn table(&self) -> usize {
        match self {
            Filter::Column { table, .. } | Filter::Rows { table, .. } => *table,
        }
    }

    /// The rows of `t` (this filter's Table) it keeps: a scan.
    pub fn rows(&self, t: &DbTable, cancel: Cancel<'_>) -> Result<Bitset> {
        match self {
            Filter::Rows { rows, .. } => {
                let mut r = Bitset::new(t.rows, true);
                r.and_with(rows);
                Ok(r)
            }
            Filter::Column {
                column, allowed, ..
            } => {
                let mut out = Bitset::new(t.rows, false);
                let Some(col) = t.columns.get(*column).map(|c| &c.data) else {
                    return Ok(out);
                };
                match allowed {
                    Allowed::Codes { codes, blank } => scan(t.rows, None, cancel, |r| {
                        let keep = match col.code(r) {
                            Some(c) => codes.get(c),
                            None => *blank,
                        };
                        if keep {
                            out.set(r);
                        }
                        Ok(())
                    })?,
                    Allowed::Values { keys, blank } => scan(t.rows, None, cancel, |r| {
                        let keep = match Key::of(&col.value(r), false) {
                            Some(k) => keys.contains(&k),
                            None => *blank,
                        };
                        if keep {
                            out.set(r);
                        }
                        Ok(())
                    })?,
                }
                Ok(out)
            }
        }
    }
}

/// The key set of `col` over the rows in `mask` (a Relationship's one side): a scan.
pub fn key_set(t: &DbTable, col: &Col, mask: &Bitset, cancel: Cancel<'_>) -> Result<HashSet<Key>> {
    let mut keys = HashSet::new();
    match col.dictionary() {
        Some(dict) => {
            let mut seen = Bitset::new(dict.len(), false);
            scan(t.rows, Some(mask), cancel, |r| {
                if let Some(c) = col.code(r) {
                    seen.set(c);
                }
                Ok(())
            })?;
            for c in seen.ones_in(0, dict.len()) {
                if let Some(k) = Key::of(&Value::text(dict.value(c)), true) {
                    keys.insert(k);
                }
            }
        }
        None => scan(t.rows, Some(mask), cancel, |r| {
            if let Some(k) = Key::of(&col.value(r), true) {
                keys.insert(k);
            }
            Ok(())
        })?,
    }
    Ok(keys)
}

/// The rows of `t` whose `col` value is in `keys` (a Relationship's many side): a scan. A
/// dictionary column decides each code once.
pub fn semijoin(t: &DbTable, col: &Col, keys: &HashSet<Key>, cancel: Cancel<'_>) -> Result<Bitset> {
    let mut out = Bitset::new(t.rows, false);
    match col.dictionary() {
        Some(dict) => {
            let mut hit = Bitset::new(dict.len(), false);
            for (i, s) in dict.iter().enumerate() {
                if s.and_then(|s| Key::of(&Value::text(s), true))
                    .is_some_and(|k| keys.contains(&k))
                {
                    hit.set(i);
                }
            }
            scan(t.rows, None, cancel, |r| {
                if col.code(r).is_some_and(|c| hit.get(c)) {
                    out.set(r);
                }
                Ok(())
            })?;
        }
        None => scan(t.rows, None, cancel, |r| {
            if Key::of(&col.value(r), true).is_some_and(|k| keys.contains(&k)) {
                out.set(r);
            }
            Ok(())
        })?,
    }
    Ok(out)
}
