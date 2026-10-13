//! Error type, values and data types, BLANK, locale and format-string rendering.
//!
//! Layer L0 — see `docs/plan/0001-mvp.md` §1.

mod value;

pub use value::{CURRENCY_SCALE, DataType, Value, ValueError, fold, number_text};
