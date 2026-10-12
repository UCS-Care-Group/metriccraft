//! A Project folder and a Package file on disk: the thin native layer over the [`FileSet`]
//! functions. (It compiles everywhere; on the web, where there is no file system, every call
//! returns [`Error::Io`], and the app uses [`crate::pack`]/[`crate::unpack`] on bytes instead.)
//!
//! **Save safety (ADR 0003).** [`write_folder`]:
//! 1. writes the new Manifest to `.staging/project.odash.json` and every part whose bytes differ
//!    from the file on disk to `.staging/new/<part>`;
//! 2. moves each changed part into place, first moving the file it replaces to
//!    `.staging/old/<part>` (parts no longer listed move there too);
//! 3. moves the new Manifest into place — last, in one rename;
//! 4. deletes `.staging/`.
//!
//! A crash before 3 leaves the old Manifest with some parts already new: [`read_folder`] finds
//! parts that don't match its hashes and returns [`Error::Interrupted`]. Reading again with
//! [`Recover::LastGood`] takes the old copies from `.staging/old/` (the state before the save);
//! [`Recover::Staging`] takes the staged Manifest and `.staging/new/` (the save being made).
//! Either way every part is checked against the chosen Manifest's hash, so the two are never
//! mixed; the next save makes the folder consistent again. A crash during 1 or after 3 leaves a
//! consistent folder (and a stale `.staging/`, cleared by the next save).
//!
//! A Package is written to `<name>.tmp` beside it, then renamed over it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::manifest::{FORMAT_VERSION, MANIFEST, MAX_MANIFEST, Manifest, hash};
use crate::project::{GITIGNORE, GITIGNORE_TEXT};
use crate::{Error, FileSet, Result, pack, unpack};

/// The staging folder inside a Project, while a save is in progress.
pub const STAGING: &str = ".staging";
/// Largest part read from disk.
pub const MAX_PART: u64 = 256 << 20;
/// Largest Package file read.
pub const MAX_PACKAGE: u64 = 1 << 30;

/// Which state to read after an interrupted save (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Recover {
    /// Fail with [`Error::Interrupted`] if the save was interrupted.
    #[default]
    No,
    /// The save that was being made.
    Staging,
    /// The state before it.
    LastGood,
}

fn io(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    }
}

/// `root` + a `/`-separated relative path (already checked by [`crate::check_path`]).
fn join(root: &Path, rel: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    p.extend(rel.split('/'));
    p
}

/// The file's bytes, or `None` if there is nothing there. A non-file or one over `cap` bytes is
/// an error.
fn read_capped(path: &Path, cap: u64) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(path, e)),
    };
    if !meta.is_file() {
        return Err(io(path, "not a regular file"));
    }
    if meta.len() > cap {
        return Err(io(path, format!("larger than {} MB", cap >> 20)));
    }
    fs::read(path).map(Some).map_err(|e| io(path, e))
}

fn write_file(path: &Path, bytes: &[u8]) -> Result {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    let mut f = fs::File::create(path).map_err(|e| io(path, e))?;
    f.write_all(bytes).map_err(|e| io(path, e))?;
    f.sync_all().map_err(|e| io(path, e))
}

fn rename(from: &Path, to: &Path) -> Result {
    if let Some(dir) = to.parent() {
        fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
    }
    fs::rename(from, to).map_err(|e| io(from, e))
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// A Project folder's files: the Manifest, its parts (each checked against its hash) and
/// `.gitignore`. `root` is the folder. See the module docs for `recover`.
pub fn read_folder(root: &Path, recover: Recover) -> Result<FileSet> {
    let staging = root.join(STAGING);
    let staged_manifest = staging.join(MANIFEST);
    let (manifest_path, alt) = match recover {
        Recover::No => (root.join(MANIFEST), None),
        Recover::Staging => (staged_manifest.clone(), Some(staging.join("new"))),
        Recover::LastGood => (root.join(MANIFEST), Some(staging.join("old"))),
    };
    let manifest_bytes =
        read_capped(&manifest_path, MAX_MANIFEST as u64)?.ok_or_else(|| match recover {
            Recover::Staging => Error::Damaged {
                parts: vec![format!("{STAGING}/{MANIFEST}")],
            },
            _ => Error::NotAProject(root.display().to_string()),
        })?;
    let manifest = Manifest::parse(&manifest_bytes)?;
    if manifest.version > FORMAT_VERSION {
        return Err(Error::NewerFormat {
            found: manifest.version,
            supported: FORMAT_VERSION,
        });
    }
    let mut files = FileSet::new();
    let mut bad = Vec::new();
    for (part, h) in &manifest.parts {
        let mut bytes = match &alt {
            Some(alt) => read_capped(&join(alt, part), MAX_PART)?,
            None => None,
        };
        if bytes.is_none() {
            bytes = read_capped(&join(root, part), MAX_PART)?;
        }
        match bytes {
            Some(b) if hash(&b) == *h => {
                files.insert(part.clone(), b);
            }
            _ => bad.push(part.clone()),
        }
    }
    if !bad.is_empty() {
        return Err(match recover {
            Recover::No => Error::Interrupted {
                parts: bad,
                staging: exists(&staged_manifest),
            },
            _ => Error::Damaged { parts: bad },
        });
    }
    files.insert(MANIFEST.into(), manifest_bytes);
    if let Some(g) = read_capped(&root.join(GITIGNORE), 1 << 20)? {
        files.insert(GITIGNORE.into(), g);
    }
    Ok(files)
}

/// Saves `files` (from [`crate::encode`]) into the folder `root`, staged (see the module docs).
/// Creates the folder, and `.gitignore` if there is none.
pub fn write_folder(root: &Path, files: &FileSet) -> Result {
    let manifest_bytes = files
        .get(MANIFEST)
        .ok_or_else(|| Error::Save(format!("no {MANIFEST}")))?;
    let manifest = Manifest::parse(manifest_bytes)?;
    fs::create_dir_all(root).map_err(|e| io(root, e))?;
    let staging = root.join(STAGING);
    if exists(&staging) {
        fs::remove_dir_all(&staging).map_err(|e| io(&staging, e))?;
    }
    // A damaged or missing old Manifest just means nothing to clean up.
    let old_parts: Vec<String> = read_capped(&root.join(MANIFEST), MAX_MANIFEST as u64)
        .ok()
        .flatten()
        .and_then(|b| Manifest::parse(&b).ok())
        .map(|m| m.parts.into_keys().collect())
        .unwrap_or_default();

    let mut changed = Vec::new();
    for part in manifest.parts.keys() {
        let bytes = files
            .get(part)
            .ok_or_else(|| Error::Save(format!("{part} is listed but missing")))?;
        let live = read_capped(&join(root, part), MAX_PART).ok().flatten();
        if live.as_deref() != Some(bytes.as_slice()) {
            changed.push((part, bytes));
        }
    }
    let removed = old_parts
        .iter()
        .filter(|p| !manifest.parts.contains_key(*p) && crate::manifest::check_part(p).is_ok());

    let new = staging.join("new");
    let old = staging.join("old");
    write_file(&staging.join(MANIFEST), manifest_bytes)?;
    for (part, bytes) in &changed {
        write_file(&join(&new, part), bytes)?;
    }
    for (part, _) in &changed {
        let live = join(root, part);
        if exists(&live) {
            rename(&live, &join(&old, part))?;
        }
        rename(&join(&new, part), &live)?;
    }
    for part in removed {
        let live = join(root, part);
        if exists(&live) {
            rename(&live, &join(&old, part))?;
        }
    }
    rename(&staging.join(MANIFEST), &root.join(MANIFEST))?;

    let gitignore = root.join(GITIGNORE);
    if !exists(&gitignore) {
        let text = files
            .get(GITIGNORE)
            .map_or(GITIGNORE_TEXT.as_bytes(), Vec::as_slice);
        write_file(&gitignore, text)?;
    }
    // The save is complete; a staging folder left behind is cleared by the next save.
    let _ = fs::remove_dir_all(&staging);
    Ok(())
}

/// A Package file's files (checked as [`crate::unpack`] does).
pub fn read_package(path: &Path) -> Result<FileSet> {
    let bytes = read_capped(path, MAX_PACKAGE)?.ok_or_else(|| io(path, "there is no such file"))?;
    unpack(&bytes)
}

/// Saves `files` as a Package at `path`: written beside it as `<name>.tmp`, then renamed over it.
pub fn write_package(path: &Path, files: &FileSet) -> Result {
    let bytes = pack(files)?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let written = write_file(&tmp, &bytes).and_then(|()| rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests;
