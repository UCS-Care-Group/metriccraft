//! Error type, values and data types, BLANK, locale and format-string rendering.
//!
//! Layer L0 — see the `LAYERS` table in `xtask/src/main.rs`.

pub mod date;
mod format;
mod value;

pub use format::{
    DE_DE, EN_GB, EN_US, FR_FR, FormatError, FormatKind, LOCALES, Locale, MAX_FORMAT_LEN,
    default_format, format,
};
pub use value::{CURRENCY_SCALE, DataType, Value, ValueError, fold, number_text};
