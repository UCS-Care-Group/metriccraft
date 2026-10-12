//! Tables, relationships, measures, calculated columns and the DAX evaluator.
//!
//! Layer L2 — see `docs/plan/0001-mvp.md` §1. Depends on `opendash-data` (loaded tables,
//! column types), never the other way.

pub mod parity;
mod table;

pub use table::{Column, Error, Model, ModelTable, SummarizeBy};
