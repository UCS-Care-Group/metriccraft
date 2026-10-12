//! Document, commands, undo/redo and cross-filter state.
//!
//! Layer L5 — see `docs/plan/0001-mvp.md` §1. Everything a user can do is a [`Command`], run
//! through [`App::run`]; the UI, control channel, CLI and MCP all go through it.

mod app;
mod cmd;

pub use app::{App, Document, UNDO_LIMIT};
pub use cmd::{COMMANDS, Command, Run};

/// Errors from running commands.
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// No command has this id.
    UnknownCommand(String),
    /// The command exists but `enabled` is false right now (e.g. undo with nothing to undo).
    Disabled(String),
    /// The params are missing, of the wrong type or out of range.
    InvalidParams { command: String, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCommand(id) => write!(f, "unknown command `{id}`"),
            Self::Disabled(id) => write!(f, "command `{id}` is not available now"),
            Self::InvalidParams { command, message } => {
                write!(f, "command `{command}`: {message}")
            }
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;
