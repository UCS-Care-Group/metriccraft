//! The storage tier (ADR 0006): the model's Tables as encoded column readers, row masks
//! ([`Bitset`]), the filter context's filters, and the **storage scan** — one pass over one
//! Table under a row mask, checking the cancel flag between scans and every [`CHUNK`] rows.
//!
//! Columns are read in their loaded encoding (ADR 0005): dictionary text by code (filters on
//! text are code bitsets, a Relationship key match is decided once per dictionary entry), narrow
//! integers and fixed decimals widened per value.
//!
//! Fast paths (M3.11), each tested against the per-row path: a text filter's row mask is built
//! a 64-row word at a time from the integer codes ([`code_mask`]); a column's distinct values
//! are its distinct codes, read once each ([`distinct_values`]); `DISTINCTCOUNT` of whole
//! numbers is a range bitmap (or an integer hash set when the range is sparse); scans under a
//! mask walk its set bits word by word.

use std::collections::HashSet;
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};

use arrow_array::cast::AsArray;
use arrow_array::types::{
    Date32Type, Decimal32Type, Decimal64Type, Float32Type, Float64Type, Int8Type, Int16Type,
    Int32Type, Int64Type,
};
use arrow_array::{Array, ArrayRef, ArrowPrimitiveType, BooleanArray, PrimitiveArray, StringArray};
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

    /// `len` bits from `words` (bit `i` is bit `i % 64` of word `i / 64`): missing words are
    /// off, extra words and bits past `len` dropped.
    pub fn from_words(mut words: Vec<u64>, len: usize) -> Self {
        let n = len.div_ceil(64);
        words.resize(n, 0);
        if !len.is_multiple_of(64)
            && let Some(last) = words.last_mut()
        {
            *last &= (1u64 << (len % 64)) - 1;
        }
        Self { words, len }
    }

    /// The bits as 64-bit words (bit `i` is bit `i % 64` of word `i / 64`).
    pub fn words(&self) -> &[u64] {
        &self.words
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
                // `start` is a multiple of 64 (CHUNK is), so chunks are whole words.
                let first = start / 64;
                let words = m.words().get(first..end.div_ceil(64)).unwrap_or(&[]);
                for (k, &word) in words.iter().enumerate() {
                    let base = (first + k) * 64;
                    let mut w = word;
                    while w != 0 {
                        let r = base + w.trailing_zeros() as usize;
                        if r >= end {
                            break;
                        }
                        w &= w - 1;
                        f(r)?;
                    }
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
                    Allowed::Codes { codes, blank } => {
                        return code_mask(col, t.rows, codes, *blank, cancel);
                    }
                    Allowed::Values { keys, blank } if col.kind() == Kind::Integer => {
                        return int_mask(col, t.rows, keys, *blank, cancel);
                    }
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
            each_code(col, t.rows, Some(mask), cancel, |_, c| {
                if let Some(c) = c {
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
            return code_mask(col, t.rows, &hit, false, cancel);
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

/// What kind of values a column holds, as the aggregations treat it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Integer,
    Decimal,
    Currency,
    DateTime,
    Text,
    Boolean,
    /// Every value BLANK ([`Col::Null`]): no type to speak of.
    Blank,
}

impl Kind {
    /// Holds numbers or dates (what `SUM`, `AVERAGE` take).
    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            Kind::Integer | Kind::Decimal | Kind::Currency | Kind::DateTime
        )
    }
}

/// A non-blank value of a numeric column, read without building a [`Value`]: a whole number,
/// a float (dates as days since 1899-12-30, decimals of a scale other than 4), or Currency
/// (×10⁴).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Number {
    Int(i64),
    Float(f64),
    Currency(i64),
}

impl Col {
    /// The kind of values this column holds.
    pub fn kind(&self) -> Kind {
        match self {
            Col::Dict { .. } | Col::Text(_) => Kind::Text,
            Col::I8(_) | Col::I16(_) | Col::I32(_) | Col::I64(_) => Kind::Integer,
            Col::F32(_) | Col::F64(_) => Kind::Decimal,
            Col::D32(_, 4) | Col::D64(_, 4) => Kind::Currency,
            Col::D32(..) | Col::D64(..) => Kind::Decimal,
            Col::Bool(_) => Kind::Boolean,
            Col::Date(_) | Col::Ts(..) => Kind::DateTime,
            Col::Null(_) => Kind::Blank,
        }
    }

    /// The column's Arrow array (`None` for [`Col::Null`]); for a dictionary column, its codes.
    fn array(&self) -> Option<&dyn Array> {
        Some(match self {
            Col::Dict { codes, .. } => match codes {
                Codes::I8(a) => a,
                Codes::I16(a) => a,
                Codes::I32(a) => a,
            },
            Col::Text(a) => a,
            Col::I8(a) => a,
            Col::I16(a) => a,
            Col::I32(a) => a,
            Col::I64(a) => a,
            Col::F32(a) => a,
            Col::F64(a) => a,
            Col::D32(a, _) => a,
            Col::D64(a, _) => a,
            Col::Bool(a) => a,
            Col::Date(a) => a,
            Col::Ts(a, _) => a,
            Col::Null(_) => return None,
        })
    }
}

/// Scans the non-null values of `a` over the rows of `0..rows` in `mask`, calling `f` with each
/// mapped through `map`.
fn each_native<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    map: impl Fn(T::Native) -> Number,
    f: &mut impl FnMut(Number) -> Result<()>,
) -> Result<()> {
    let values = a.values();
    let nulls = a.nulls().filter(|n| n.null_count() > 0);
    let n = rows.min(values.len());
    if mask.is_none() && nulls.is_none() {
        // Every row: the values a chunk at a time.
        cancel.check()?;
        let mut start = 0;
        while start < n {
            cancel.check()?;
            let end = start.saturating_add(CHUNK).min(n);
            for &v in values.get(start..end).unwrap_or(&[]) {
                f(map(v))?;
            }
            start = end;
        }
        return Ok(());
    }
    scan(n, mask, cancel, |r| {
        if nulls.is_some_and(|n| n.is_null(r)) {
            return Ok(());
        }
        match values.get(r) {
            Some(v) => f(map(*v)),
            None => Ok(()),
        }
    })
}

/// The storage scan for numeric aggregations: calls `f` with every non-blank value of a
/// numeric column (see [`Kind::is_numeric`]) over the rows of `0..rows` in `mask`, in order.
/// Other kinds of column visit nothing.
pub fn each_number(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    mut f: impl FnMut(Number) -> Result<()>,
) -> Result<()> {
    let f = &mut f;
    let divisor = |scale: i8| 10f64.powi(i32::from(scale));
    match col {
        Col::I8(a) => each_native(a, rows, mask, cancel, |v| Number::Int(v.into()), f),
        Col::I16(a) => each_native(a, rows, mask, cancel, |v| Number::Int(v.into()), f),
        Col::I32(a) => each_native(a, rows, mask, cancel, |v| Number::Int(v.into()), f),
        Col::I64(a) => each_native(a, rows, mask, cancel, Number::Int, f),
        Col::F32(a) => each_native(a, rows, mask, cancel, |v| Number::Float(v.into()), f),
        Col::F64(a) => each_native(a, rows, mask, cancel, Number::Float, f),
        Col::D32(a, 4) => each_native(a, rows, mask, cancel, |v| Number::Currency(v.into()), f),
        Col::D64(a, 4) => each_native(a, rows, mask, cancel, Number::Currency, f),
        Col::D32(a, s) => {
            let d = divisor(*s);
            each_native(
                a,
                rows,
                mask,
                cancel,
                |v| Number::Float(f64::from(v) / d),
                f,
            )
        }
        Col::D64(a, s) => {
            let d = divisor(*s);
            each_native(a, rows, mask, cancel, |v| Number::Float(v as f64 / d), f)
        }
        Col::Date(a) => each_native(
            a,
            rows,
            mask,
            cancel,
            |d| Number::Float(f64::from(d) + UNIX_EPOCH_DAYS),
            f,
        ),
        Col::Ts(a, per_day) => {
            let per_day = *per_day;
            each_native(
                a,
                rows,
                mask,
                cancel,
                |t| Number::Float(t as f64 / per_day + UNIX_EPOCH_DAYS),
                f,
            )
        }
        Col::Dict { .. } | Col::Text(_) | Col::Bool(_) | Col::Null(_) => cancel.check(),
    }
}

/// The non-blank values of `col` over the rows of `0..rows` in `mask`: a scan (none when the
/// column has no nulls).
pub fn count_values(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
) -> Result<usize> {
    let Some(array) = col.array() else {
        return cancel.check().map(|()| 0);
    };
    let rows = rows.min(array.len());
    let mut n = 0usize;
    match col {
        Col::Dict { .. } => each_code(col, rows, mask, cancel, |_, c| {
            if c.is_some() {
                n += 1;
            }
            Ok(())
        })?,
        _ if array.null_count() == 0 => {
            return match mask {
                None => cancel.check().map(|()| rows),
                Some(m) => cancel.check().map(|()| m.ones_in(0, rows).count()),
            };
        }
        _ => scan(rows, mask, cancel, |r| {
            if array.is_valid(r) {
                n += 1;
            }
            Ok(())
        })?,
    }
    Ok(n)
}

/// The distinct values of `col` over the rows of `0..rows` in `mask`, BLANK counting as one
/// value: a scan. A dictionary column collects codes, then counts their values ignoring case
/// (dictionaries are folded at load, so normally one code is one value).
pub fn distinct_count(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
) -> Result<usize> {
    let mut blank = false;
    let n = match col {
        Col::Dict { values, .. } => {
            let mut seen = Bitset::new(values.len(), false);
            each_code(col, rows, mask, cancel, |_, c| {
                match c {
                    Some(c) => seen.set(c),
                    None => blank = true,
                }
                Ok(())
            })?;
            let distinct: HashSet<String> = seen
                .ones_in(0, values.len())
                .map(|c| fold(values.value(c)))
                .collect();
            distinct.len()
        }
        Col::I8(a) => int_distinct(a, rows, mask, cancel, &mut blank)?,
        Col::I16(a) => int_distinct(a, rows, mask, cancel, &mut blank)?,
        Col::I32(a) => int_distinct(a, rows, mask, cancel, &mut blank)?,
        Col::I64(a) => int_distinct(a, rows, mask, cancel, &mut blank)?,
        // Dates are whole days: distinct days are distinct values.
        Col::Date(a) => int_distinct(a, rows, mask, cancel, &mut blank)?,
        _ => {
            let mut keys = HashSet::new();
            scan(rows, mask, cancel, |r| {
                match Key::of(&col.value(r), false) {
                    Some(k) => {
                        keys.insert(k);
                    }
                    None => blank = true,
                }
                Ok(())
            })?;
            keys.len()
        }
    };
    Ok(n.saturating_add(usize::from(blank)))
}

/// The smallest (or, with `max`, largest) non-blank text of `col` over the rows of `0..rows`
/// in `mask`, comparing ignoring case; of spellings equal but for case, the first in the
/// dictionary (or, for plain text, the first row) wins. `None` when there
/// is none. A dictionary column compares each code seen once.
pub fn text_extreme(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    max: bool,
) -> Result<Option<Value>> {
    let better = |candidate: &str, best: &str| {
        let o = fold(candidate).cmp(&fold(best));
        if max { o.is_gt() } else { o.is_lt() }
    };
    match col {
        Col::Dict { values, .. } => {
            let mut seen = Bitset::new(values.len(), false);
            each_code(col, rows, mask, cancel, |_, c| {
                if let Some(c) = c {
                    seen.set(c);
                }
                Ok(())
            })?;
            let mut best: Option<&str> = None;
            for c in seen.ones_in(0, values.len()) {
                let s = values.value(c);
                if best.is_none_or(|b| better(s, b)) {
                    best = Some(s);
                }
            }
            Ok(best.map(Value::text))
        }
        Col::Text(a) => {
            let mut best: Option<&str> = None;
            scan(rows.min(a.len()), mask, cancel, |r| {
                if a.is_valid(r) {
                    let s = a.value(r);
                    if best.is_none_or(|b| better(s, b)) {
                        best = Some(s);
                    }
                }
                Ok(())
            })?;
            Ok(best.map(Value::text))
        }
        _ => cancel.check().map(|()| None),
    }
}

/// Calls `f(row, code)` for every row of `0..rows` in `mask`, in order, with the row's
/// dictionary code: `None` for BLANK (a null, a code out of range or naming a null entry, a row
/// past the column). A column without a dictionary visits nothing.
pub fn each_code(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    mut f: impl FnMut(usize, Option<usize>) -> Result<()>,
) -> Result<()> {
    let Col::Dict { codes, values } = col else {
        return cancel.check();
    };
    let f = &mut f;
    match codes {
        Codes::I8(a) => each_code_native(a, values, rows, mask, cancel, f),
        Codes::I16(a) => each_code_native(a, values, rows, mask, cancel, f),
        Codes::I32(a) => each_code_native(a, values, rows, mask, cancel, f),
    }
}

fn each_code_native<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    dict: &StringArray,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    f: &mut impl FnMut(usize, Option<usize>) -> Result<()>,
) -> Result<()>
where
    T::Native: Into<i64>,
{
    let keys = a.values();
    let nulls = a.nulls().filter(|n| n.null_count() > 0);
    let dict_nulls = dict.nulls().filter(|n| n.null_count() > 0);
    let len = dict.len();
    scan(rows, mask, cancel, |r| {
        if nulls.is_some_and(|n| r >= n.len() || n.is_null(r)) {
            return f(r, None);
        }
        let code = keys
            .get(r)
            .and_then(|&k| usize::try_from(k.into()).ok())
            .filter(|&c| c < len && dict_nulls.is_none_or(|n| n.is_valid(c)));
        f(r, code)
    })
}

/// The distinct dictionary codes of `col` over the rows of `0..rows` in `mask`, in first-seen
/// order, `None` standing for BLANK (see [`each_code`]). Without a mask, the scan stops once
/// every code has been seen and BLANK has been seen or can't occur.
fn first_codes(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
) -> Result<Vec<Option<usize>>> {
    let mut order = Vec::new();
    let Col::Dict { codes, values } = col else {
        return cancel.check().map(|()| order);
    };
    if mask.is_none() {
        return match codes {
            Codes::I8(a) => first_codes_native(a, values, rows, cancel),
            Codes::I16(a) => first_codes_native(a, values, rows, cancel),
            Codes::I32(a) => first_codes_native(a, values, rows, cancel),
        };
    }
    let mut seen = Bitset::new(values.len(), false);
    let mut blank = false;
    each_code(col, rows, mask, cancel, |_, code| {
        match code {
            Some(c) if !seen.get(c) => {
                seen.set(c);
                order.push(Some(c));
            }
            None if !blank => {
                blank = true;
                order.push(None);
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(order)
}

fn first_codes_native<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    dict: &StringArray,
    rows: usize,
    cancel: Cancel<'_>,
) -> Result<Vec<Option<usize>>>
where
    T::Native: Into<i64>,
{
    let keys = a.values();
    let n = rows.min(keys.len());
    let len = dict.len();
    let nulls = a.nulls().filter(|b| b.null_count() > 0);
    // Per code: 0 unseen, 1 seen, 2 BLANK (a null entry).
    let mut state: Vec<u8> = (0..len).map(|c| u8::from(dict.is_null(c)) * 2).collect();
    let mut left = state.iter().filter(|&&s| s == 0).count();
    let mut order = Vec::new();
    let mut blank = false;
    // BLANK can occur only through null keys, null entries, out-of-range codes or a short
    // column; the last is checked when the scan stops early.
    let blank_possible = nulls.is_some() || left < len || n < rows;
    let mut stopped = None;
    cancel.check()?;
    let mut start = 0;
    'scan: while start < n {
        cancel.check()?;
        let end = start.saturating_add(CHUNK).min(n);
        for (i, &k) in keys.get(start..end).unwrap_or(&[]).iter().enumerate() {
            let r = start + i;
            let null = nulls.is_some_and(|b| b.is_null(r));
            let slot = if null {
                None
            } else {
                usize::try_from(k.into())
                    .ok()
                    .and_then(|c| Some((c, state.get_mut(c)?)))
            };
            match slot {
                Some((_, 1)) => {}
                Some((c, s @ 0)) => {
                    *s = 1;
                    order.push(Some(c));
                    left -= 1;
                }
                _ => {
                    if !blank {
                        blank = true;
                        order.push(None);
                    }
                }
            }
            if left == 0 && (blank || !blank_possible) {
                stopped = Some(r + 1);
                break 'scan;
            }
        }
        start = end;
    }
    if let Some(from) = stopped
        && !blank
    {
        // Every code is seen; BLANK only if a later code is out of range.
        let rest = keys.get(from..n).unwrap_or(&[]);
        let out_of_range = rest
            .iter()
            .any(|&k| usize::try_from(k.into()).map_or(true, |c| c >= len));
        if out_of_range {
            order.push(None);
        }
    } else if n < rows && !blank {
        order.push(None);
    }
    Ok(order)
}

/// The rows of `0..rows` whose dictionary code is in `allowed` (BLANK rows, see [`each_code`]:
/// when `blank`): a scan building the mask a word at a time from the integer codes. A column
/// without a dictionary is all BLANK.
pub fn code_mask(
    col: &Col,
    rows: usize,
    allowed: &Bitset,
    blank: bool,
    cancel: Cancel<'_>,
) -> Result<Bitset> {
    let Col::Dict { codes, values } = col else {
        cancel.check()?;
        return Ok(Bitset::new(rows, blank));
    };
    // Per code: allowed, or (a null entry) BLANK.
    let mut keep = Bitset::new(values.len(), false);
    for c in 0..values.len() {
        let on = if values.is_valid(c) {
            allowed.get(c)
        } else {
            blank
        };
        if on {
            keep.set(c);
        }
    }
    match codes {
        Codes::I8(a) => code_mask_native(a, rows, &keep, blank, cancel),
        Codes::I16(a) => code_mask_native(a, rows, &keep, blank, cancel),
        Codes::I32(a) => code_mask_native(a, rows, &keep, blank, cancel),
    }
}

fn code_mask_native<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    rows: usize,
    keep: &Bitset,
    blank: bool,
    cancel: Cancel<'_>,
) -> Result<Bitset>
where
    T::Native: Into<i64>,
{
    let (len, kw) = (keep.len(), keep.words());
    let lookup = |c: i64| match usize::try_from(c) {
        Ok(c) if c < len => kw.get(c / 64).is_some_and(|w| (w >> (c % 64)) & 1 == 1),
        _ => blank,
    };
    // Narrow codes (Int8, Int16) index a table of every possible code, offset to be positive.
    let width = std::mem::size_of::<T::Native>();
    let (lut, offset): (Vec<u64>, i64) = if width <= 2 {
        let half = 1i64 << (8 * width - 1);
        ((-half..half).map(|c| u64::from(lookup(c))).collect(), half)
    } else {
        (Vec::new(), 0)
    };
    let test = |k: T::Native| -> u64 {
        let c: i64 = k.into();
        if lut.is_empty() {
            u64::from(lookup(c))
        } else {
            usize::try_from(c.wrapping_add(offset))
                .ok()
                .and_then(|i| lut.get(i))
                .copied()
                .unwrap_or(u64::from(blank))
        }
    };
    mask_words(a, rows, blank, cancel, test)
}

/// The rows of `0..rows` for which `test` of the value is 1 (null rows and rows past the
/// column: `blank`): a scan building the mask a 64-row word at a time.
fn mask_words<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    rows: usize,
    blank: bool,
    cancel: Cancel<'_>,
    test: impl Fn(T::Native) -> u64,
) -> Result<Bitset> {
    let keys = a.values();
    let n = rows.min(keys.len());
    // Validity as words aligned with the rows (a null row is BLANK).
    let valid: Option<Vec<u64>> = a.nulls().filter(|b| b.null_count() > 0).map(|b| {
        let chunks = b.inner().bit_chunks();
        chunks
            .iter()
            .chain(std::iter::once(chunks.remainder_bits()))
            .collect()
    });
    let blank_word = if blank { u64::MAX } else { 0 };
    let mut words = Vec::with_capacity(rows.div_ceil(64));
    cancel.check()?;
    let mut start = 0;
    while start < n {
        cancel.check()?;
        let end = start.saturating_add(CHUNK).min(n);
        for block in keys.get(start..end).unwrap_or(&[]).chunks(64) {
            let mut w = 0u64;
            for (i, &k) in block.iter().enumerate() {
                w |= test(k) << i;
            }
            if let Some(v) = &valid {
                let v = v.get(words.len()).copied().unwrap_or(0);
                w = (w & v) | (!v & blank_word);
            }
            words.push(w);
        }
        start = end;
    }
    let mut out = Bitset::from_words(words, rows);
    if blank {
        // Rows past the column are BLANK.
        for r in n..rows {
            out.set(r);
        }
    }
    Ok(out)
}

/// The rows of `0..rows` of a whole-number column whose value is in `keys` (BLANK rows: when
/// `blank`): the keys resolved to integers once, then a word-at-a-time scan of the values.
fn int_mask(
    col: &Col,
    rows: usize,
    keys: &HashSet<Key>,
    blank: bool,
    cancel: Cancel<'_>,
) -> Result<Bitset> {
    // Only `Key::Int`s can equal a whole number.
    let mut ints: Vec<i64> = keys
        .iter()
        .filter_map(|k| match k {
            Key::Int(i) => Some(*i),
            _ => None,
        })
        .collect();
    ints.sort_unstable();
    let set: HashSet<i64, BuildHasherDefault<IntHasher>> = ints.iter().copied().collect();
    let small = ints.len() <= 16;
    let hit = |v: i64| -> u64 {
        u64::from(if small {
            ints.binary_search(&v).is_ok()
        } else {
            set.contains(&v)
        })
    };
    match col {
        Col::I8(a) => mask_words(a, rows, blank, cancel, |v| hit(v.into())),
        Col::I16(a) => mask_words(a, rows, blank, cancel, |v| hit(v.into())),
        Col::I32(a) => mask_words(a, rows, blank, cancel, |v| hit(v.into())),
        Col::I64(a) => mask_words(a, rows, blank, cancel, hit),
        _ => {
            cancel.check()?;
            Ok(Bitset::new(rows, blank))
        }
    }
}

/// A hasher for whole numbers (a SplitMix64 finaliser): `DISTINCTCOUNT` over a wide range.
#[derive(Default)]
struct IntHasher(u64);

impl Hasher for IntHasher {
    fn finish(&self) -> u64 {
        let mut z = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(b);
        }
    }

    fn write_i64(&mut self, i: i64) {
        self.0 = self.0.rotate_left(32) ^ i as u64;
    }
}

/// Most bits a `DISTINCTCOUNT` range bitmap may take per visible value (beyond a floor of
/// 2^16): wider ranges hash instead.
const RANGE_BITS_PER_VALUE: u64 = 16;

/// The distinct non-null whole numbers of `a` over the rows of `0..rows` in `mask` (setting
/// `blank` when a visible row is BLANK): a range bitmap when the values are dense enough
/// (a min/max pass, then a set-bits pass), else a hash set.
fn int_distinct<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
    blank: &mut bool,
) -> Result<usize>
where
    T::Native: Into<i64>,
{
    let values = a.values();
    let n = rows.min(values.len());
    if n < rows && mask.is_none_or(|m| m.ones_in(n, rows).next().is_some()) {
        *blank = true;
    }
    let nulls = a.nulls().filter(|b| b.null_count() > 0);
    let null = |r: usize| nulls.is_some_and(|b| r >= b.len() || b.is_null(r));
    let (mut lo, mut hi, mut visible) = (i64::MAX, i64::MIN, 0u64);
    scan(n, mask, cancel, |r| {
        if null(r) {
            *blank = true;
        } else if let Some(&v) = values.get(r) {
            let v: i64 = v.into();
            lo = lo.min(v);
            hi = hi.max(v);
            visible += 1;
        }
        Ok(())
    })?;
    if visible == 0 {
        return Ok(0);
    }
    let span = u64::try_from(i128::from(hi) - i128::from(lo) + 1).unwrap_or(u64::MAX);
    let cap = visible
        .saturating_mul(RANGE_BITS_PER_VALUE)
        .clamp(1 << 16, 1 << 32);
    if let Ok(bits) = usize::try_from(span)
        && span <= cap
    {
        let mut seen = Bitset::new(bits, false);
        scan(n, mask, cancel, |r| {
            if !null(r)
                && let Some(&v) = values.get(r)
                && let Ok(i) = usize::try_from(v.into().wrapping_sub(lo) as u64)
            {
                seen.set(i);
            }
            Ok(())
        })?;
        return Ok(seen.count());
    }
    let mut seen: HashSet<i64, BuildHasherDefault<IntHasher>> = HashSet::default();
    scan(n, mask, cancel, |r| {
        if !null(r)
            && let Some(&v) = values.get(r)
        {
            seen.insert(v.into());
        }
        Ok(())
    })?;
    Ok(seen.len())
}

/// The distinct values of `col` over the rows of `0..rows` in `mask`, in first-seen order
/// (text ignoring case: the first spelling seen wins), BLANK among them: a scan. A dictionary
/// column collects codes ([`first_codes`]), then reads each code's value once.
pub fn distinct_values(
    col: &Col,
    rows: usize,
    mask: Option<&Bitset>,
    cancel: Cancel<'_>,
) -> Result<Vec<Value>> {
    let mut keys: HashSet<Option<Key>> = HashSet::new();
    let mut out = Vec::new();
    if let Some(dict) = col.dictionary() {
        for code in first_codes(col, rows, mask, cancel)? {
            let v = code.map_or(Value::Blank, |c| Value::text(dict.value(c)));
            if keys.insert(Key::of(&v, false)) {
                out.push(v);
            }
        }
        return Ok(out);
    }
    scan(rows, mask, cancel, |r| {
        let v = col.value(r);
        if keys.insert(Key::of(&v, false)) {
            out.push(v);
        }
        Ok(())
    })?;
    Ok(out)
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
