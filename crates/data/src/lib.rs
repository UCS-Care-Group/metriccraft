//! Arrow columnar store, Sources and Applied Step execution.
//!
//! Layer L2 — see `docs/plan/0001-mvp.md` §1.

mod cell;
pub mod culture;
#[cfg(test)]
#[path = "../../m/src/fuzz.rs"]
mod fuzz;
pub mod m;
mod merge;
mod preview;
mod query;
pub mod source;
pub mod step;
mod table;

pub use preview::Preview;
pub use query::{Pass, Query};
pub use table::{Table, TableBuilder, default_memory_cap};

/// Errors from loading data.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// Loading this table would take loaded data past the memory cap (spec N-4).
    MemoryCap { table: String, limit: usize },
    /// Arrow rejected the data (mismatched batch, too many distinct values…).
    Arrow { table: String, message: String },
    /// A Source could not be read (bad file, wrong format, unsupported feature…).
    Source { source: String, message: String },
    /// An Applied Step can't run on its input (a column it names is missing, a name collides…).
    Step { step: &'static str, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MemoryCap { table, limit } => write!(
                f,
                "loading `{table}` would exceed the memory limit ({} MB left)",
                limit / (1 << 20)
            ),
            Self::Arrow { table, message } => write!(f, "`{table}`: {message}"),
            Self::Source { source, message } => write!(f, "{source}: {message}"),
            Self::Step { step, message } => write!(f, "{step}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;
