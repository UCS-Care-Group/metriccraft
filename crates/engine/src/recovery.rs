//! Autosave recovery (M2; Power BI's AutoRecovery): while the
//! Document has unsaved changes, a recovery copy is written to the user cache directory, never
//! into the Project; the next launch offers to restore it.
//!
//! **No timers in the engine.** The app layer asks [`App::autosave_due`] on an interval (every
//! few seconds is fine) and calls [`App::write_recovery`] when it says so;
//! `options.autosaveMinutes` sets the interval (default 5, 0 = off, as Power BI's *Store
//! AutoRecovery information every N minutes*). At launch it lists [`App::recoverable`] copies
//! and runs `file.recover` or `file.discardRecovery` on each the user picks.
//!
//! **Layout** (`<dir>` is [`recovery_dir`], e.g. `~/.cache/metriccraft/recovery`):
//!
//! ```text
//! <dir>/<id>/                    one Document's copy: a Project folder
//!   project.mcraft.json, model/…, queries/…   its definitions, as a save writes them
//!   .cache/<stem>.parquet|.meta  its Data cache (provenance kept, so Stale comes back exactly)
//!   recovery.json                written last: where it came from, when, its tables and Stale
//! ```
//!
//! `<id>` is a hash of the Project location (so a Project has one copy), or `session-…` for a
//! document never saved. Source paths are written as a save to the original location would
//! write them, and resolved against it on recovery.
//!
//! **Dirty tracking.** The app counts Document changes (`App::version`); a copy is written only
//! when the count moved since the last save (or open) and since the last copy. Like a save, only
//! tables whose data changed since the last copy are written (the same buffer-identity check,
//! `file::write_cache`); the others are reused.
//!
//! **Crash safety.** `recovery.json` is deleted first and written last (beside it, then renamed);
//! the definitions are written staged, Manifest last; each table's cache in the cache's own
//! order. A copy without `recovery.json` was interrupted while being written: it is listed with
//! a problem, offered for discard only, never opened half-written.
//!
//! **Deleted** on a successful save, on [`App::close`] (a clean exit), and when the Document is
//! replaced (New, Open, another recovery). Dropping the [`App`] without `close` (a crash) leaves
//! it for the next launch.
//!
//! ponytail: two app instances on the same Project share one copy (last writer wins); a lock
//! file arrives if that turns out to matter.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use metriccraft_file::{FileSet, Recover};
use serde_json::{Value, json};

use crate::file::{CacheFrom, Location, Saved};
use crate::model::{invalid, text};
use crate::{App, Error, Result};

/// The default autosave interval, in minutes (Power BI's default).
pub const DEFAULT_AUTOSAVE_MINUTES: u32 = 5;
/// The longest autosave interval accepted, in minutes (a day).
pub const MAX_AUTOSAVE_MINUTES: u32 = 24 * 60;
/// The file that marks a complete recovery copy (see the module docs).
pub const INFO: &str = "recovery.json";
/// The `recovery.json` format this build writes and reads.
const INFO_VERSION: u64 = 1;
/// Largest `recovery.json` read.
const MAX_INFO: u64 = 16 << 20;
/// Longest recovery id.
const MAX_ID: usize = 64;

/// The autosave option and this app's recovery copy (not part of the Document).
#[derive(Clone, Debug)]
pub(crate) struct Autosave {
    /// Minutes between recovery copies; 0 = off.
    minutes: u32,
    copy: Option<Copy>,
}

impl Default for Autosave {
    fn default() -> Self {
        Self {
            minutes: DEFAULT_AUTOSAVE_MINUTES,
            copy: None,
        }
    }
}

/// This app's recovery copy: where, and what it holds.
#[derive(Clone, Debug)]
struct Copy {
    /// The recovery folder it is in (`dir` of [`App::write_recovery`]).
    dir: PathBuf,
    id: String,
    /// The Document version it holds; `None` if a write failed part-way.
    version: Option<u64>,
    /// When it was last written, in milliseconds since 1970.
    at: u64,
    /// Its Data cache, by table file stem (as `FileState::cache` for a Project folder).
    cache: std::collections::BTreeMap<String, Saved>,
}

impl Copy {
    fn path(&self) -> PathBuf {
        self.dir.join(&self.id)
    }
}

/// A recovery copy that can be offered at launch ([`App::recoverable`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryInfo {
    /// Its id: `file.recover` / `file.discardRecovery` take it.
    pub id: String,
    /// The Project or Package it was a copy of; `None` for a document never saved.
    pub location: Option<Location>,
    /// When it was written, in milliseconds since 1970.
    pub saved_at: Option<u64>,
    /// Its Tables, in model order.
    pub tables: Vec<String>,
    /// Why it can't be recovered (interrupted while being written, or damaged): offer Discard
    /// only. `None` for a usable copy.
    pub problem: Option<String>,
}

/// What `recovery.json` holds.
struct Info {
    location: Option<Location>,
    saved_at: u64,
    tables: Vec<String>,
    stale: BTreeSet<String>,
}

/// The user's recovery folder: `$XDG_CACHE_HOME/metriccraft/recovery` (or `~/.cache/…`) on Linux,
/// `%LOCALAPPDATA%\metriccraft\recovery` on Windows, `~/Library/Caches/metriccraft/recovery` on macOS.
/// `None` where there is none (the web build, or no home directory).
pub fn recovery_dir() -> Option<PathBuf> {
    let var = |k: &str| {
        std::env::var_os(k)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    #[cfg(windows)]
    let base = var("LOCALAPPDATA");
    #[cfg(target_os = "macos")]
    let base = var("HOME").map(|h| h.join("Library").join("Caches"));
    #[cfg(not(any(windows, target_os = "macos")))]
    let base = var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")));
    Some(base?.join("metriccraft").join("recovery"))
}

impl App {
    /// The autosave interval in minutes; 0 = off.
    pub fn autosave_minutes(&self) -> u32 {
        self.autosave.minutes
    }

    /// Whether a recovery copy should be written now (`now`: milliseconds since 1970): autosave
    /// is on, the Document has changed since it was saved and since the last copy, and the
    /// interval has passed since the last copy (or none was written yet).
    pub fn autosave_due(&self, now: u64) -> bool {
        let minutes = self.autosave.minutes;
        if minutes == 0 || !self.needs_recovery() {
            return false;
        }
        let interval = u64::from(minutes).saturating_mul(60_000);
        self.autosave
            .copy
            .as_ref()
            .is_none_or(|c| now.saturating_sub(c.at) >= interval || now < c.at)
    }

    /// Whether the Document has changes no recovery copy holds yet.
    fn needs_recovery(&self) -> bool {
        self.is_dirty()
            && self
                .autosave
                .copy
                .as_ref()
                .is_none_or(|c| c.version != Some(self.version))
    }

    /// Writes the recovery copy into the recovery folder `dir` (normally [`recovery_dir`]), at
    /// time `now` (milliseconds since 1970). Writes nothing and returns `false` when there are no
    /// changes it doesn't hold already (see the module docs).
    pub fn write_recovery(&mut self, dir: &Path, now: u64) -> Result<bool> {
        let mine = self.autosave.copy.take().filter(|c| c.dir == dir);
        if !self.is_dirty()
            || mine
                .as_ref()
                .is_some_and(|c| c.version == Some(self.version))
        {
            self.autosave.copy = mine;
            return Ok(false);
        }
        let (id, prev) = match mine {
            Some(c) => (c.id, c.cache),
            None => (self.recovery_id(now), Default::default()),
        };
        let path = dir.join(&id);
        if let Some(Location::Folder(project)) = self.file.location.as_ref()
            && let (Ok(project), Ok(p)) = (std::path::absolute(project), std::path::absolute(&path))
            && p.starts_with(project)
        {
            return Err(Error::File(metriccraft_file::Error::Save(
                "the recovery folder can't be inside the Project".into(),
            )));
        }
        // Until it completes, the copy is this app's (so close still deletes it), holding nothing.
        self.autosave.copy = Some(Copy {
            dir: dir.to_path_buf(),
            id: id.clone(),
            version: None,
            at: now,
            cache: Default::default(),
        });
        let cache = self.write_copy(&path, now, prev)?;
        self.autosave.copy = Some(Copy {
            dir: dir.to_path_buf(),
            id,
            version: Some(self.version),
            at: now,
            cache,
        });
        Ok(true)
    }

    fn write_copy(
        &self,
        path: &Path,
        now: u64,
        prev: std::collections::BTreeMap<String, Saved>,
    ) -> Result<std::collections::BTreeMap<String, Saved>> {
        let location = self.file.location.clone();
        let root = location.as_ref().and_then(Location::root);
        let files = self.files(root.as_deref())?;
        let entries = self.cache_entries(root.as_deref())?;
        let info_path = path.join(INFO);
        match fs::remove_file(&info_path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&info_path, e)),
            _ => {}
        }
        metriccraft_file::write_folder(path, &files).map_err(Error::File)?;
        let cache = crate::file::write_cache(path, entries, prev, &self.stems())?;
        let doc = self.document();
        let info = Info {
            location,
            saved_at: now,
            tables: doc.model.tables.iter().map(|t| t.name.clone()).collect(),
            stale: doc.stale.clone(),
        };
        let tmp = path.join(format!("{INFO}.tmp"));
        fs::write(&tmp, info.print())
            .and_then(|()| fs::rename(&tmp, &info_path))
            .map_err(|e| {
                let _ = fs::remove_file(&tmp);
                io(&info_path, e)
            })?;
        Ok(cache)
    }

    /// The id for a new copy: the Project location's hash, or a session id.
    fn recovery_id(&self, now: u64) -> String {
        match self.file.location.as_ref() {
            Some(location) => {
                let (kind, p) = match location {
                    Location::Folder(p) => ("folder", p),
                    Location::Package(p) => ("package", p),
                };
                let p = std::path::absolute(p).unwrap_or_else(|_| p.clone());
                let h =
                    metriccraft_file::manifest::hash(format!("{kind}:{}", p.display()).as_bytes());
                let hex = h.strip_prefix("sha256:").unwrap_or(&h);
                format!("project-{}", hex.get(..16).unwrap_or(hex))
            }
            None => {
                use std::sync::atomic::{AtomicU64, Ordering};
                static N: AtomicU64 = AtomicU64::new(0);
                let n = N.fetch_add(1, Ordering::Relaxed);
                #[cfg(not(target_arch = "wasm32"))]
                let pid = std::process::id();
                #[cfg(target_arch = "wasm32")]
                let pid = 0u32;
                format!("session-{now:x}-{pid:x}-{n:x}")
            }
        }
    }

    /// The recovery copies in the recovery folder `dir`, newest first, each with a problem if it
    /// can't be recovered. Never fails: an unreadable folder lists nothing.
    pub fn recoverable(dir: &Path) -> Vec<RecoveryInfo> {
        let Ok(entries) = fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for entry in entries.flatten() {
            let Some(id) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if check_id(&id).is_err() || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path();
            out.push(match read_info(&path) {
                Ok(info) => RecoveryInfo {
                    id,
                    problem: (!path.join(metriccraft_file::MANIFEST).is_file())
                        .then(|| "its definitions are missing".to_string()),
                    location: info.location,
                    saved_at: Some(info.saved_at),
                    tables: info.tables,
                },
                Err(problem) => RecoveryInfo {
                    id,
                    location: None,
                    saved_at: None,
                    tables: Vec::new(),
                    problem: Some(problem),
                },
            });
        }
        out.sort_by(|a, b| b.saved_at.cmp(&a.saved_at).then_with(|| a.id.cmp(&b.id)));
        out
    }

    /// Opens recovery copy `id` from `dir` as the Document (`file.recover`): its location is
    /// the original's, and it is dirty until saved. Replaces the Document, not undoable.
    pub fn recover(&mut self, dir: &Path, id: &str) -> Result {
        let cmd = "file.recover";
        check_id(id).map_err(|m| invalid(cmd, m))?;
        let path = dir.join(id);
        let info = read_info(&path)
            .map_err(|m| invalid(cmd, format!("recovery copy `{id}` can't be used: {m}")))?;
        let files: FileSet =
            metriccraft_file::read_folder(&path, Recover::No).map_err(Error::File)?;
        let root = info.location.as_ref().and_then(Location::root);
        self.open_files(
            files,
            info.location,
            root.as_deref(),
            &CacheFrom::Folder(&path),
            Some(&info.stale),
        )?;
        // The cache read came from the copy, not the Project: the Project's is unknown.
        let cache = std::mem::take(&mut self.file.cache);
        let previous = self.autosave.copy.take();
        if let Some(p) = previous.filter(|p| p.path() != path) {
            let _ = fs::remove_dir_all(p.path());
        }
        self.clean = None;
        self.autosave.copy = Some(Copy {
            dir: dir.to_path_buf(),
            id: id.to_string(),
            version: Some(self.version),
            at: info.saved_at,
            cache,
        });
        Ok(())
    }

    /// Deletes recovery copy `id` from `dir` (`file.discardRecovery`).
    pub fn discard_recovery(&mut self, dir: &Path, id: &str) -> Result {
        check_id(id).map_err(|m| invalid("file.discardRecovery", m))?;
        let path = dir.join(id);
        if !path.is_dir() {
            return Err(invalid(
                "file.discardRecovery",
                format!("there is no recovery copy `{id}`"),
            ));
        }
        fs::remove_dir_all(&path).map_err(|e| io(&path, e))?;
        if self
            .autosave
            .copy
            .as_ref()
            .is_some_and(|c| c.path() == path)
        {
            self.autosave.copy = None;
        }
        Ok(())
    }

    /// Deletes this app's recovery copy, if any: the Document was saved or replaced. Best effort.
    pub(crate) fn discard_own_recovery(&mut self) {
        if let Some(c) = self.autosave.copy.take() {
            let _ = fs::remove_dir_all(c.path());
        }
    }

    /// A clean exit: deletes this app's recovery copy. (Dropping the app without closing it, as
    /// a crash does, leaves the copy for the next launch.)
    pub fn close(mut self) -> Result {
        if let Some(c) = self.autosave.copy.take() {
            let path = c.path();
            match fs::remove_dir_all(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(&path, e)),
                _ => {}
            }
        }
        Ok(())
    }
}

fn io(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::File(metriccraft_file::Error::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })
}

/// A recovery id: ASCII letters, digits and `-`, at most [`MAX_ID`] (so it can't leave `dir`).
fn check_id(id: &str) -> std::result::Result<(), String> {
    if id.is_empty()
        || id.len() > MAX_ID
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(format!("`{}` is not a recovery id", id.escape_debug()));
    }
    Ok(())
}

impl Info {
    fn print(&self) -> Vec<u8> {
        let location = match &self.location {
            Some(Location::Folder(p)) => json!({"folder": p.display().to_string()}),
            Some(Location::Package(p)) => json!({"package": p.display().to_string()}),
            None => Value::Null,
        };
        let v = json!({
            "formatVersion": INFO_VERSION,
            "location": location,
            "savedAt": self.saved_at,
            "tables": self.tables,
            "stale": self.stale,
        });
        // Serialising a `Value` can't fail; an empty object (unreadable) is the fallback.
        serde_json::to_vec_pretty(&v).unwrap_or_else(|_| b"{}".to_vec())
    }

    fn parse(bytes: &[u8]) -> std::result::Result<Info, String> {
        let v: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("{INFO} is not valid JSON: {e}"))?;
        let version = v.get("formatVersion").and_then(Value::as_u64);
        if version != Some(INFO_VERSION) {
            return Err(format!(
                "{INFO} format version {version:?} is not {INFO_VERSION}"
            ));
        }
        let path = |v: &Value| v.as_str().map(PathBuf::from).filter(|p| p.is_absolute());
        let location = match v.get("location") {
            None | Some(Value::Null) => None,
            Some(l) => Some(
                match (
                    l.get("folder").and_then(path),
                    l.get("package").and_then(path),
                ) {
                    (Some(p), None) => Location::Folder(p),
                    (None, Some(p)) => Location::Package(p),
                    _ => return Err(format!("{INFO}: `location` is malformed")),
                },
            ),
        };
        let names = |k: &str| -> std::result::Result<Vec<String>, String> {
            v.get(k)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("{INFO}: `{k}` must be a list"))?
                .iter()
                .map(|n| {
                    n.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("{INFO}: `{k}` must hold text"))
                })
                .collect()
        };
        Ok(Info {
            location,
            saved_at: v
                .get("savedAt")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("{INFO}: `savedAt` must be a whole number"))?,
            tables: names("tables")?,
            stale: names("stale")?.into_iter().collect(),
        })
    }
}

/// The copy's `recovery.json`, or why it can't be used.
fn read_info(path: &Path) -> std::result::Result<Info, String> {
    let info = path.join(INFO);
    let meta = match fs::symlink_metadata(&info) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err("it was interrupted while being written".into());
        }
        Err(e) => return Err(e.to_string()),
    };
    if !meta.is_file() || meta.len() > MAX_INFO {
        return Err(format!("{INFO} is not a regular file of at most 16 MB"));
    }
    let bytes = fs::read(&info).map_err(|e| e.to_string())?;
    Info::parse(&bytes)
}

// ---- commands ----

/// The `dir` param, or the user's recovery folder.
fn dir_param(params: &Value, cmd: &str) -> Result<PathBuf> {
    match params.get("dir") {
        None | Some(Value::Null) => recovery_dir()
            .ok_or_else(|| invalid(cmd, "there is no user cache folder: give `dir`".into())),
        Some(_) => Ok(PathBuf::from(text(params, "dir", cmd)?)),
    }
}

/// `file.recover`: opens a recovery copy as the Document.
pub(crate) fn recover(app: &mut App, params: &Value) -> Result {
    const ID: &str = "file.recover";
    let id = text(params, "id", ID)?;
    let dir = dir_param(params, ID)?;
    app.recover(&dir, &id)
}

/// `file.discardRecovery`: deletes a recovery copy.
pub(crate) fn discard(app: &mut App, params: &Value) -> Result {
    const ID: &str = "file.discardRecovery";
    let id = text(params, "id", ID)?;
    let dir = dir_param(params, ID)?;
    app.discard_recovery(&dir, &id)
}

/// `options.autosaveMinutes`: the autosave interval (an app option, not undoable).
pub(crate) fn set_minutes(app: &mut App, params: &Value) -> Result {
    const ID: &str = "options.autosaveMinutes";
    let minutes = params
        .get("minutes")
        .and_then(Value::as_u64)
        .and_then(|m| u32::try_from(m).ok())
        .filter(|m| *m <= MAX_AUTOSAVE_MINUTES)
        .ok_or_else(|| {
            invalid(
                ID,
                format!("`minutes` must be a whole number from 0 (off) to {MAX_AUTOSAVE_MINUTES}"),
            )
        })?;
    app.autosave.minutes = minutes;
    Ok(())
}

#[cfg(test)]
mod tests;
