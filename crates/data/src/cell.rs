//! Reading single values out of Arrow columns of any supported type, and comparing them with
//! M's rules: numbers compare across widths (`1 = 1.0`), different types are never equal,
//! `null = null`.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};

use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringArray,
    TimestampMicrosecondArray,
};
use arrow_cast::cast;
use arrow_schema::{DataType, TimeUnit};

/// One value, borrowed from a [`Cells`] column or a step's literal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Cell<'a> {
    Null,
    Int(i64),
    Number(f64),
    Text(&'a str),
    Logical(bool),
    /// Days since 1970-01-01.
    Date(i32),
    /// Microseconds since 1970-01-01 00:00.
    DateTime(i64),
}

/// `f` as a whole `i64`, if it is one.
fn whole(f: f64) -> Option<i64> {
    // 2^63 is exact as f64; `as` is exact for every whole value in range.
    (f.fract() == 0.0 && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&f))
        .then_some(f as i64)
}

impl Cell<'_> {
    /// M's `=`: `null = null` is true, null and a value are not equal, values of different
    /// types are not equal.
    pub(crate) fn equals(self, other: Cell<'_>) -> bool {
        match (self, other) {
            (Cell::Null, Cell::Null) => true,
            (Cell::Int(a), Cell::Number(b)) | (Cell::Number(b), Cell::Int(a)) => {
                whole(b) == Some(a)
            }
            (a, b) => a.compare(b) == Some(Ordering::Equal),
        }
    }

    /// Order of two non-null values of the same type; `None` for null or mismatched types.
    pub(crate) fn compare(self, other: Cell<'_>) -> Option<Ordering> {
        match (self, other) {
            (Cell::Int(a), Cell::Int(b)) => Some(a.cmp(&b)),
            (Cell::Number(a), Cell::Number(b)) => a.partial_cmp(&b),
            #[allow(clippy::cast_precision_loss)]
            (Cell::Int(a), Cell::Number(b)) => (a as f64).partial_cmp(&b),
            #[allow(clippy::cast_precision_loss)]
            (Cell::Number(a), Cell::Int(b)) => a.partial_cmp(&(b as f64)),
            (Cell::Text(a), Cell::Text(b)) => Some(a.cmp(b)),
            (Cell::Logical(a), Cell::Logical(b)) => Some(a.cmp(&b)),
            (Cell::Date(a), Cell::Date(b)) => Some(a.cmp(&b)),
            (Cell::DateTime(a), Cell::DateTime(b)) => Some(a.cmp(&b)),
            _ => None,
        }
    }

    /// Hashes so that [`Cell::equals`] values hash the same.
    pub(crate) fn hash_into(self, h: &mut impl Hasher) {
        match self {
            Cell::Null => 0u8.hash(h),
            Cell::Int(i) => (1u8, i).hash(h),
            Cell::Number(f) => match whole(f) {
                Some(i) => (1u8, i).hash(h),
                None => (2u8, f.to_bits()).hash(h),
            },
            Cell::Text(s) => (3u8, s).hash(h),
            Cell::Logical(b) => (4u8, b).hash(h),
            Cell::Date(d) => (5u8, d).hash(h),
            Cell::DateTime(t) => (6u8, t).hash(h),
        }
    }
}

/// A column cast to one plain array per value type, read one [`Cell`] at a time.
// ponytail: dictionary text is cast to plain text (a transient copy of the column's strings).
// Evaluate on the dictionary values and map the keys if that copy shows up in memory benches.
pub(crate) enum Cells {
    Null,
    Int(Int64Array),
    Number(Float64Array),
    Text(StringArray),
    Logical(BooleanArray),
    Date(Date32Array),
    DateTime(TimestampMicrosecondArray),
}

impl Cells {
    /// `None` if the column's type isn't one Applied Steps compare yet.
    pub(crate) fn new(column: &ArrayRef) -> Option<Self> {
        let t = column.data_type();
        let to = |ty: DataType| cast(column, &ty).ok();
        Some(match t {
            DataType::Null => Self::Null,
            DataType::Boolean => Self::Logical(downcast(&to(DataType::Boolean)?)?),
            DataType::Date32 | DataType::Date64 => Self::Date(downcast(&to(DataType::Date32)?)?),
            DataType::Timestamp(..) => Self::DateTime(downcast(&to(DataType::Timestamp(
                TimeUnit::Microsecond,
                None,
            ))?)?),
            t if t.is_integer() => Self::Int(downcast(&to(DataType::Int64)?)?),
            t if t.is_numeric() => Self::Number(downcast(&to(DataType::Float64)?)?),
            t if is_text(t) => Self::Text(downcast(&to(DataType::Utf8)?)?),
            _ => return None,
        })
    }

    /// The value at row `i`; null past the end.
    pub(crate) fn get(&self, i: usize) -> Cell<'_> {
        fn at<A: Array>(a: &A, i: usize) -> bool {
            i < a.len() && a.is_valid(i)
        }
        match self {
            Self::Int(a) if at(a, i) => Cell::Int(a.value(i)),
            Self::Number(a) if at(a, i) => Cell::Number(a.value(i)),
            Self::Text(a) if at(a, i) => Cell::Text(a.value(i)),
            Self::Logical(a) if at(a, i) => Cell::Logical(a.value(i)),
            Self::Date(a) if at(a, i) => Cell::Date(a.value(i)),
            Self::DateTime(a) if at(a, i) => Cell::DateTime(a.value(i)),
            _ => Cell::Null,
        }
    }
}

fn downcast<A: Array + Clone + 'static>(a: &ArrayRef) -> Option<A> {
    a.as_any().downcast_ref::<A>().cloned()
}

pub(crate) fn is_text(t: &DataType) -> bool {
    match t {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => true,
        DataType::Dictionary(_, v) => is_text(v),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;

    use super::*;

    fn h(c: Cell<'_>) -> u64 {
        let mut s = DefaultHasher::new();
        c.hash_into(&mut s);
        s.finish()
    }

    #[test]
    fn m_equality() {
        assert!(Cell::Null.equals(Cell::Null));
        assert!(!Cell::Null.equals(Cell::Int(0)));
        assert!(Cell::Int(1).equals(Cell::Number(1.0)));
        assert_eq!(h(Cell::Int(1)), h(Cell::Number(1.0)));
        assert!(!Cell::Int(1).equals(Cell::Number(1.5)));
        assert!(!Cell::Int(1).equals(Cell::Text("1")));
        assert!(!Cell::Date(1).equals(Cell::Int(1)));
        assert_eq!(Cell::Text("a").compare(Cell::Int(1)), None);
        assert_eq!(Cell::Null.compare(Cell::Null), None);
        assert_eq!(whole(f64::MAX), None);
        assert_eq!(whole(-0.0), Some(0));
    }
}
