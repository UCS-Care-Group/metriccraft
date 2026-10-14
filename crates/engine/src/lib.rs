//! Document, commands, undo/redo and cross-filter state.
//!
//! Layer L5 — see `docs/plan/0001-mvp.md` §1. Everything a user can do is a [`Command`], run
//! through [`App::run`]; the UI, control channel, CLI and MCP all go through it.

mod app;
mod cmd;
mod expression;
mod file;
mod load;
mod model;
mod query;
mod recovery;
mod relationship;
mod rename;
mod report;
mod selection;

pub use app::{App, Document, UNDO_LIMIT};
pub use cmd::{COMMANDS, Command, Run};
pub use file::Location;
pub use load::{LoadReport, Loaded};
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
    Data(opendash_data::Error),
    /// Refresh (or Close & Apply) stopped at this Query; nothing was changed.
    Refresh {
        query: String,
        error: opendash_data::Error,
    },
    /// No Query has this name.
    UnknownQuery(String),
    /// A model change, or a load's model update (a type override that no longer converts, a
    /// sort by column that no longer holds, a Relationship's one side no longer unique), failed;
    /// nothing was changed.
    Model(opendash_model::Error),
    /// Opening or saving a Project or Package failed; nothing was changed. An interrupted save
    /// ([`opendash_file::Error::Interrupted`]) is retried with `file.open`'s `recover`.
    File(opendash_file::Error),
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
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;
