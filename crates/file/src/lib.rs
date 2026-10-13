//! Project and Package read/write (ADR 0003).
//!
//! Layer L2 — see `docs/plan/0001-mvp.md` §1. Depends on `opendash-data` (Queries, M) and
//! `opendash-model` (Tables, Relationships), never the other way.
//!
//! The core is pure functions over an in-memory [`FileSet`] (path → bytes), so the web build,
//! which has no file system, opens and saves Packages from bytes:
//!
//! ```text
//! Project ──encode──▶ FileSet ──pack──▶ .odash bytes
//! Project ◀─decode─── FileSet ◀─unpack─ .odash bytes
//!                       │ ▲
//!          write_folder ▼ │ read_folder        (native glue: staged save, interrupted-save
//!                     folder on disk            detection and recovery; `folder` module)
//! ```
//!
//! - [`project`]: the mapping between a [`Project`] (Queries, Model, Project culture) and its
//!   parts, with unknown content carried through ([`Preserved`]).
//! - [`manifest`]: `project.odash.json`, part hashes, Format version and migrations.
//! - [`escape`]: deterministic file names for table and query names.
//! - [`package`]: the `.odash` zip.
//! - [`folder`]: the Project folder on disk.
//! - [`cache`]: the Data cache (`.cache/`): Parquet per table and its metadata.

pub mod cache;
pub mod escape;
pub mod folder;
pub mod manifest;
pub mod package;
pub mod project;
pub mod syntax;

use std::collections::BTreeMap;
use std::fmt;

pub use folder::{
    Recover, read_folder, read_package, read_package_with, write_folder, write_package,
};
pub use manifest::{FORMAT_VERSION, MANIFEST};
pub use package::{pack, unpack};
pub use project::{Preserved, Project, decode, empty_table, encode};

/// A Project's files: path (relative, `/`-separated) → bytes, sorted by path.
pub type FileSet = BTreeMap<String, Vec<u8>>;

/// Why a Project or Package could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The file system refused (or there is none, as on the web).
    Io { path: String, message: String },
    /// There is no `project.odash.json` here.
    NotAProject(String),
    /// The Manifest is malformed.
    Manifest(String),
    /// Written by a newer OpenDash BI: this one only reads Format versions up to `supported`.
    NewerFormat { found: u64, supported: u64 },
    /// An older Format version with no migration to the next one.
    NoMigration { from: u64 },
    /// An interrupted save: these parts don't match the Manifest's hashes. Open again with
    /// [`Recover::Staging`] (the save being made; offered when `staging` is true) or
    /// [`Recover::LastGood`] (the state before it). Never mixed silently.
    Interrupted { parts: Vec<String>, staging: bool },
    /// Parts that don't match their hashes and no recovery applies (a damaged Package, or a
    /// recovery whose copies are incomplete).
    Damaged { parts: Vec<String> },
    /// A part is malformed or inconsistent (names its path and, for syntax, the line).
    Part { path: String, message: String },
    /// The Package (zip) is malformed, unsafe (path traversal, too large) or not ours.
    Package(String),
    /// The Project can't be saved as it is (e.g. a name with a line break).
    Save(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(f, "{path}: {message}"),
            Self::NotAProject(path) => {
                write!(f, "{path} is not an OpenDash BI Project (no {MANIFEST})")
            }
            Self::Manifest(m) => write!(f, "{MANIFEST}: {m}"),
            Self::NewerFormat { found, supported } => write!(
                f,
                "this Project was saved by a newer OpenDash BI (format version {found}); this \
                 version reads up to format version {supported}. Update OpenDash BI to open it"
            ),
            Self::NoMigration { from } => write!(
                f,
                "format version {from} is too old to open: there is no migration from it"
            ),
            Self::Interrupted { parts, staging } => {
                write!(
                    f,
                    "the last save was interrupted ({} don't match the Manifest); open the last \
                     good state",
                    parts.join(", ")
                )?;
                if *staging {
                    write!(f, " or the save that was being made")?;
                }
                Ok(())
            }
            Self::Damaged { parts } => write!(
                f,
                "the Project is damaged: {} don't match the Manifest",
                parts.join(", ")
            ),
            Self::Part { path, message } => write!(f, "{path}: {message}"),
            Self::Package(m) => write!(f, "not a valid .odash Package: {m}"),
            Self::Save(m) => write!(f, "can't save: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T = ()> = std::result::Result<T, Error>;

/// Checks a part path from a Manifest or a Package entry: relative, `/`-separated, no `.`/`..`
/// or empty components, no backslash, colon or control characters (so it can't leave the
/// Project folder or mean something else on Windows), at most 1,024 bytes.
pub fn check_path(path: &str) -> std::result::Result<(), String> {
    if path.is_empty() || path.len() > 1024 {
        return Err(format!("`{path}`: path is empty or too long"));
    }
    if path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        return Err(format!(
            "`{}`: path has a backslash, colon or control character",
            path.escape_debug()
        ));
    }
    for part in path.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(format!(
                "`{path}`: path must be relative, without `.`, `..` or empty parts"
            ));
        }
    }
    Ok(())
}
