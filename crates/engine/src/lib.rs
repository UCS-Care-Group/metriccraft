//! Document, commands, undo/redo and cross-filter state.
//!
//! Layer L5 — see `docs/plan/0001-mvp.md` §1. Everything a user can do is a [`Command`], run
//! through [`App::run`]; the UI, control channel, CLI and MCP all go through it.

mod app;
mod cmd;
mod diagram;
mod expression;
mod file;
mod load;
mod model;
pub mod param;
mod query;
mod read;
mod recovery;
mod relationship;
mod rename;
mod report;
mod selection;

pub use app::{App, Document, MAX_BATCH, Origin, UNDO_LIMIT};
pub use cmd::{COMMANDS, Command, Run, reference};
pub use diagram::Diagram;
pub use file::Location;
pub use load::{LoadReport, Loaded, excel_items, query_m};
pub use metriccraft_data::step::Step;
pub use metriccraft_data::{Preview, Progress};
pub use recovery::{DEFAULT_AUTOSAVE_MINUTES, MAX_AUTOSAVE_MINUTES, RecoveryInfo, recovery_dir};
pub use report::{
    BrokenField, MAX_COORD, MAX_PAGES, MAX_VISUALS, PAGE_HEIGHT, PAGE_WIDTH, Page, Report, resolve,
    seed_ids,
};
pub use selection::{DataSelection, MAX_SELECTED, Point};

/// Errors from running commands.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// No command has this id.
    UnknownCommand(String),
    /// The command exists but `enabled` is false right now (e.g. undo with nothing to undo).
    Disabled(String),
    /// The params are missing, of the wrong type or out of range.
    InvalidParams { command: String, message: String },
    /// The data layer failed (e.g. a Query's result would exceed the memory cap).
    Data(metriccraft_data::Error),
    /// Refresh (or Close & Apply) stopped at this Query; nothing was changed.
    Refresh {
        query: String,
        error: metriccraft_data::Error,
    },
    /// No Query has this name.
    UnknownQuery(String),
    /// A model change, or a load's model update (a type override that no longer converts, a
    /// sort by column that no longer holds, a Relationship's one side no longer unique), failed;
    /// nothing was changed.
    Model(metriccraft_model::Error),
    /// Opening or saving a Project or Package failed; nothing was changed. An interrupted save
    /// ([`metriccraft_file::Error::Interrupted`]) is retried with `file.open`'s `recover`.
    File(metriccraft_file::Error),
    /// Step `index` (0 = first) of a Batch failed; nothing was changed.
    Batch {
        index: usize,
        command: String,
        error: Box<Error>,
    },
    /// A Read command failed: a DAX query error, a Page that can't be drawn.
    Read { command: String, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCommand(id) => write!(f, "unknown command `{id}`"),
            Self::Disabled(id) => write!(f, "command `{id}` is not available now"),
            Self::InvalidParams { command, message } => {
                write!(f, "command `{command}`: {message}")
            }
            Self::Data(e) => e.fmt(f),
            Self::Refresh { query, error } => {
                write!(
                    f,
                    "loading query `{query}` failed (no data was changed): {error}"
                )
            }
            Self::UnknownQuery(name) => write!(f, "there is no query `{name}`"),
            Self::Model(e) => e.fmt(f),
            Self::File(e) => e.fmt(f),
            Self::Batch {
                index,
                command,
                error,
            } => write!(
                f,
                "batch command {} (`{command}`) failed, nothing was changed: {error}",
                index.saturating_add(1)
            ),
            Self::Read { command, message } => write!(f, "`{command}`: {message}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;
