//! Arrow columnar store, Sources and Applied Step execution.
//!
//! Layer L2 — see `docs/plan/0001-mvp.md` §1.

mod table;

pub use table::{Table, TableBuilder, default_memory_cap};

/// Errors from loading data.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// Loading this table would take loaded data past the memory cap (spec N-4).
    MemoryCap { table: String, limit: usize },
    /// Arrow rejected the data (mismatched batch, too many distinct values…).
    Arrow { table: String, message: String },
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
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;
