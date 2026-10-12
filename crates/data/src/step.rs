//! Applied Steps (CONTEXT.md). M1.2 needs only the automatic ones a Source adds; M1.5 adds the
//! rest.

use std::sync::Arc;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringArray,
    TimestampMicrosecondArray,
};
use arrow_schema::{DataType, TimeUnit};

use crate::culture::Culture;

/// One recorded shaping operation.
#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// `Table.PromoteHeaders`: the first row becomes the column names.
    PromotedHeaders,
    /// `Table.TransformColumnTypes`: column name → type.
    ChangedType(Vec<(String, ColumnType)>),
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
}
