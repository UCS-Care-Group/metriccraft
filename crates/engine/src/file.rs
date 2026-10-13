//! The File menu (ADR 0003): New, Open, Save and Save As, over `opendash_file`.
//!
//! The Document maps to an `opendash_file::Project` (Queries, Enable load, Model, Project
//! culture); what the files held beyond it (unknown content) and where they live stay in
//! [`FileState`], beside the Document, so undo never touches them.
//!
//! Opening (and New) replaces the Document and clears undo and redo: not undoable, as in Power
//! BI.
//!
//! **Data cache** (`opendash_file::cache`, M2.6). Saving writes each Table's data (as loaded,
//! after Model type overrides) as Parquet, with its provenance (`Document::provenance`) as
//! metadata. In a Project folder only what changed since the last save there is written: data
//! by buffer identity (columns are immutable and shared, ADR 0005, so changed data is different
//! buffers; [`FileState`] keeps the buffer set last written), metadata by its bytes. A Package
//! is written whole, with the cache unless `file.saveAs` is given `data: false`.
//!
//! Opening loads each Table from the cache, within the memory cap, without reading any Source:
//! Stale when its Query or Source files changed since it loaded (the cached data is shown);
//! empty and Stale, with a notice offering Refresh, when there is no usable cache. A Table with
//! no provenance (e.g. renamed since it loaded) is saved with its Query's current hash and
//! files, or as Stale if its Query has unapplied edits; one that never loaded isn't saved.
//!
//! ponytail: `.local/` source-path remapping (Local overrides) is not done yet: it needs the
//! remap applied on open and undone on save, so the definitions don't change per user.
//!
//! The web build has no file system: it uses [`App::open_package_bytes`] and
//! [`App::package_bytes`] instead of the commands' paths.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use opendash_data::Table;
use opendash_data::source::Bytes;
use opendash_file::cache::{self, Meta, Provenance};
use opendash_file::escape::file_stems;
use opendash_file::package::Limits;
use opendash_file::{FileSet, MANIFEST, Preserved, Project, Recover};
use opendash_model::ModelTable;
use serde_json::Value;

use crate::model::{invalid, text};
use crate::{App, Document, Error, Result};

/// Where a document was opened from or saved to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// A Project folder.
    Folder(PathBuf),
    /// A `.odash` Package file.
    Package(PathBuf),
}

impl Location {
    /// The location `path` names: a folder (or its Manifest) is a Project, anything else a
    /// Package.
    fn of(path: &Path) -> Self {
        if path.file_name().is_some_and(|n| n == MANIFEST) {
            let parent = path.parent().unwrap_or(Path::new("."));
            Self::Folder(parent.to_path_buf())
        } else if path.is_dir() {
            Self::Folder(path.to_path_buf())
        } else {
            Self::Package(path.to_path_buf())
        }
    }

    /// The folder source paths are made relative to: the Project folder, or the folder holding
    /// the Package.
    pub(crate) fn root(&self) -> Option<PathBuf> {
        let absolute = |p: &Path| std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
        match self {
            Self::Folder(p) => Some(absolute(p)),
            Self::Package(p) => absolute(p).parent().map(Path::to_path_buf),
        }
    }
}

/// The file side of the open document.
#[derive(Clone, Debug, Default)]
pub(crate) struct FileState {
    pub(crate) location: Option<Location>,
    /// The Project culture read from the files; `None` for a new document (en-US).
    culture: Option<String>,
    /// Kept up to date by renames (`rename.rs`).
    pub(crate) preserved: Preserved,
    /// The Data cache as last written to (or read from) the Project folder at `location`, by
    /// table file stem. Empty for a Package, which is always written whole.
    pub(crate) cache: BTreeMap<String, Saved>,
}

/// One table's cache in the Project folder: which data, and its metadata bytes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Saved {
    data: DataKey,
    meta: Vec<u8>,
}

/// Which data a table holds: its columns and their buffers (address, size). Columns are
/// immutable and shared (ADR 0005), so different data means different buffers.
#[derive(Clone, Debug, PartialEq)]
struct DataKey {
    columns: Vec<String>,
    buffers: Vec<(usize, usize)>,
    rows: usize,
}

impl DataKey {
    fn of(table: &Table) -> DataKey {
        DataKey {
            columns: table
                .batch()
                .schema()
                .fields()
                .iter()
                .map(|f| format!("{}: {}", f.name(), f.data_type()))
                .collect(),
            buffers: table.buffers(),
            rows: table.rows(),
        }
    }
}

/// One table's cache as it would be saved now.
pub(crate) struct Entry {
    stem: String,
    /// A clone: shares the columns.
    table: Table,
    meta: Vec<u8>,
}

/// The `m` written for data with no provenance whose Query has unapplied edits: never a hash,
/// so it reopens Stale.
const UNAPPLIED: &str = "unapplied";

/// Where opening reads the Data cache from.
pub(crate) enum CacheFrom<'a> {
    /// The Project folder on disk.
    Folder(&'a Path),
    /// The `.cache/` entries of the files (a Package).
    Files,
}

const OPEN: &str = "file.open";
const SAVE: &str = "file.save";
const SAVE_AS: &str = "file.saveAs";

pub(crate) const OPEN_PARAMS: &str = "path: text — a Project folder, its project.odash.json, or a .odash Package\n\
     recover: text — after an interrupted save only: staging (the save that was being made) or \
     lastGood (the state before it)";
pub(crate) const SAVE_PARAMS: &str =
    "path: text — save here instead, as Save As does (default: where it was opened or last saved)";
pub(crate) const SAVE_AS_PARAMS: &str = "path: text — the Project folder or .odash Package to write\n\
     package: logical — write a .odash Package (default: whether path ends in .odash)\n\
     data: logical — include the Data cache in a Package (default true; a Project folder always \
     keeps it)";

impl App {
    /// Where the document was opened from or last saved to.
    pub fn location(&self) -> Option<&Location> {
        self.file.location.as_ref()
    }

    /// Opens a `.odash` Package from its bytes (the web build's File > Open). Like `file.open`:
    /// replaces the Document, not undoable. Relative source paths stay relative.
    pub fn open_package_bytes(&mut self, bytes: &[u8]) -> Result {
        let files = opendash_file::package::unpack_with(bytes, self.package_limits())
            .map_err(Error::File)?;
        self.open_files(files, None, None, &CacheFrom::Files, None)?;
        self.discard_own_recovery();
        Ok(())
    }

    /// The document as `.odash` Package bytes, with its Data cache (the web build's Save: a
    /// download).
    pub fn package_bytes(&self) -> Result<Vec<u8>> {
        let files = self.package_files(None, true)?;
        opendash_file::pack(&files).map_err(Error::File)
    }

    /// Package reading limits: the Data cache counts against the memory cap.
    fn package_limits(&self) -> Limits {
        Limits {
            cache: u64::try_from(self.memory_cap()).unwrap_or(u64::MAX),
            ..Limits::default()
        }
    }

    fn project(&self) -> Project {
        let doc = self.document();
        Project {
            culture: self
                .file
                .culture
                .clone()
                .unwrap_or_else(|| Project::default().culture),
            queries: doc.queries.clone(),
            unloaded: doc.unloaded.clone(),
            model: doc.model.clone(),
        }
    }

    pub(crate) fn files(&self, root: Option<&Path>) -> Result<FileSet> {
        opendash_file::encode(&self.project(), root, &self.file.preserved).map_err(Error::File)
    }

    /// The definitions, plus the Data cache if `data`.
    fn package_files(&self, root: Option<&Path>, data: bool) -> Result<FileSet> {
        let mut files = self.files(root)?;
        if data {
            for e in self.cache_entries(root)? {
                files.insert(
                    cache::parquet_path(&e.stem),
                    cache::encode_table(&e.table).map_err(Error::File)?,
                );
                files.insert(cache::meta_path(&e.stem), e.meta);
            }
        }
        Ok(files)
    }

    /// Every table stem, and the cache of each table that has data to keep (see the module
    /// docs), with paths in its metadata relative to `root`.
    pub(crate) fn cache_entries(&self, root: Option<&Path>) -> Result<Vec<Entry>> {
        let doc = self.document();
        let names: Vec<&str> = doc.model.tables.iter().map(|t| t.name.as_str()).collect();
        let mut out = Vec::new();
        for (model, stem) in doc.model.tables.iter().zip(file_stems(&names)) {
            let Some(table) = doc.tables.iter().find(|t| t.name() == model.name) else {
                continue;
            };
            let provenance = match doc.provenance.get(&model.name) {
                Some(p) => p.clone(),
                None => {
                    let stale = cache::closure(&doc.queries, &model.name)
                        .iter()
                        .any(|q| doc.stale.contains(&q.name));
                    if stale && table.rows() == 0 {
                        continue;
                    }
                    let mut p = Provenance::of(&doc.queries, &model.name, None);
                    if stale {
                        p.m = UNAPPLIED.into();
                    }
                    p
                }
            };
            let meta = Meta::new(table, model, provenance).map_err(Error::File)?;
            out.push(Entry {
                stem,
                table: table.clone(),
                meta: meta.print(root),
            });
        }
        Ok(out)
    }

    /// Replaces the Document with `files`. `recovered`: the Stale Queries recorded with a
    /// recovery copy, Stale whatever their cache says (e.g. a Query edited and edited back).
    pub(crate) fn open_files(
        &mut self,
        mut files: FileSet,
        location: Option<Location>,
        root: Option<&Path>,
        from: &CacheFrom<'_>,
        recovered: Option<&BTreeSet<String>>,
    ) -> Result {
        let recorded = |name: &String| recovered.is_some_and(|r| r.contains(name));
        let (project, preserved) = opendash_file::decode(&files, root).map_err(Error::File)?;
        let mut doc = Document::default();
        let mut saved = BTreeMap::new();
        let mut used = 0usize;
        let mut fresh = BTreeSet::new();
        let mut stale_data = 0usize;
        let mut problems = Vec::new();
        let names: Vec<&str> = project
            .model
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        for (model, stem) in project.model.tables.iter().zip(file_stems(&names)) {
            let limit = self.memory_cap().saturating_sub(used);
            match read_cached(&mut files, from, root, model, &stem, limit) {
                Ok(Some((table, meta, meta_bytes))) => {
                    used = used.saturating_add(table.memory());
                    let name = model.name.clone();
                    if meta.provenance.is_stale(&project.queries, &name)
                        || project.unloaded.contains(&name)
                        || recorded(&name)
                    {
                        stale_data = stale_data.saturating_add(1);
                    } else {
                        fresh.insert(name.clone());
                    }
                    if matches!(from, CacheFrom::Folder(_)) {
                        let data = DataKey::of(&table);
                        saved.insert(
                            stem,
                            Saved {
                                data,
                                meta: meta_bytes,
                            },
                        );
                    }
                    doc.provenance.insert(name, meta.provenance);
                    doc.tables.push(table);
                }
                Ok(None) => {
                    doc.tables
                        .push(opendash_file::empty_table(model).map_err(Error::Data)?);
                }
                Err(why) => {
                    problems.push(format!("`{}` ({why})", model.name));
                    doc.tables
                        .push(opendash_file::empty_table(model).map_err(Error::Data)?);
                }
            }
        }
        doc.stale = project
            .queries
            .iter()
            .filter(|q| {
                !project.unloaded.contains(&q.name) || project.model.table(&q.name).is_some()
            })
            .filter(|q| !fresh.contains(&q.name))
            .map(|q| q.name.clone())
            .collect();
        doc.stale.extend(
            project
                .queries
                .iter()
                .filter(|q| recorded(&q.name))
                .map(|q| q.name.clone()),
        );
        let no_data = doc.stale.len().saturating_sub(stale_data);
        doc.queries = project.queries;
        doc.unloaded = project.unloaded;
        doc.model = project.model;
        self.replace(doc);
        self.file = FileState {
            location,
            culture: Some(project.culture),
            preserved,
            cache: saved,
        };
        let mut notes = Vec::new();
        if !problems.is_empty() {
            notes.push(format!(
                "the Data cache can't be used for {}",
                problems.join(", ")
            ));
        }
        if no_data > 0 {
            notes.push(format!(
                "{no_data} {} no data yet: Refresh to load",
                if no_data == 1 {
                    "query has"
                } else {
                    "queries have"
                }
            ));
        }
        if stale_data > 0 {
            notes.push(format!(
                "{stale_data} {} out of date since {} loaded (Stale): Refresh to update",
                if stale_data == 1 {
                    "table is"
                } else {
                    "tables are"
                },
                if stale_data == 1 { "it" } else { "they" }
            ));
        }
        if !notes.is_empty() {
            self.note(&notes.join("; "));
        }
        Ok(())
    }

    fn save_to(&mut self, location: Location, data: bool) -> Result {
        let root = location.root();
        match &location {
            Location::Folder(p) => {
                let files = self.files(root.as_deref())?;
                let entries = self.cache_entries(root.as_deref())?;
                opendash_file::write_folder(p, &files).map_err(Error::File)?;
                let mut saved = std::mem::take(&mut self.file.cache);
                if self.file.location.as_ref() != Some(&location) {
                    saved.clear();
                }
                self.file.cache = write_cache(p, entries, saved, &self.stems())?;
            }
            Location::Package(p) => {
                let files = self.package_files(root.as_deref(), data)?;
                opendash_file::write_package(p, &files).map_err(Error::File)?;
                self.file.cache.clear();
            }
        }
        self.file.location = Some(location);
        self.clean = Some(self.version);
        // Saved: the recovery copy is no longer needed (best effort: the save itself succeeded).
        self.discard_own_recovery();
        Ok(())
    }

    /// The file stem of every model Table.
    pub(crate) fn stems(&self) -> BTreeSet<String> {
        let names: Vec<&str> = self
            .document()
            .model
            .tables
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        file_stems(&names).into_iter().collect()
    }
}

/// Writes the Data cache `entries` into the folder `dir` and deletes the cache of tables not in
/// `keep`. `prev` is what `dir` holds (as last written there): a table whose data is the same
/// buffers isn't rewritten, and its metadata only if its bytes changed. Each table is written in
/// [`cache::write`]'s crash-safe order. Returns what `dir` now holds.
pub(crate) fn write_cache(
    dir: &Path,
    entries: Vec<Entry>,
    mut prev: BTreeMap<String, Saved>,
    keep: &BTreeSet<String>,
) -> Result<BTreeMap<String, Saved>> {
    let mut next = BTreeMap::new();
    for e in entries {
        let data = DataKey::of(&e.table);
        let before = prev.remove(&e.stem);
        let same =
            cache::exists_in(dir, &e.stem) && before.as_ref().is_some_and(|s| s.data == data);
        if !same {
            let bytes = cache::encode_table(&e.table).map_err(Error::File)?;
            cache::write(dir, &e.stem, Some(&bytes), &e.meta).map_err(Error::File)?;
        } else if before.as_ref().is_some_and(|s| s.meta != e.meta) {
            cache::write(dir, &e.stem, None, &e.meta).map_err(Error::File)?;
        }
        next.insert(e.stem, Saved { data, meta: e.meta });
    }
    cache::remove_others(dir, keep).map_err(Error::File)?;
    Ok(next)
}

/// Table `model`'s cached data, its metadata and the metadata's bytes; `None` if there is no
/// cache for it; an error message if there is one that can't be used.
fn read_cached(
    files: &mut FileSet,
    from: &CacheFrom<'_>,
    root: Option<&Path>,
    model: &ModelTable,
    stem: &str,
    limit: usize,
) -> std::result::Result<Option<(Table, Meta, Vec<u8>)>, String> {
    let meta_bytes = match from {
        CacheFrom::Folder(dir) => cache::read_meta(dir, stem).map_err(|e| e.to_string())?,
        CacheFrom::Files => files.remove(&cache::meta_path(stem)),
    };
    let Some(meta_bytes) = meta_bytes else {
        return Ok(None);
    };
    let meta = Meta::parse(&meta_bytes, root)?;
    if !meta.fits(model) {
        return Err("its columns don't match the model".into());
    }
    let missing = || "its data file is missing, or over the memory limit".to_string();
    let table = match from {
        CacheFrom::Folder(dir) => {
            let file = cache::open_parquet(dir, stem)
                .map_err(|e| e.to_string())?
                .ok_or_else(missing)?;
            cache::decode_table(file, &meta, limit)?
        }
        CacheFrom::Files => {
            let bytes = files
                .remove(&cache::parquet_path(stem))
                .ok_or_else(missing)?;
            cache::decode_table(Bytes::from(bytes), &meta, limit)?
        }
    };
    Ok(Some((table, meta, meta_bytes)))
}

/// `file.new`: an empty document.
pub(crate) fn new(app: &mut App, _params: &Value) -> Result {
    app.replace(Document::default());
    app.file = FileState::default();
    app.discard_own_recovery();
    Ok(())
}

/// `file.open`: replaces the document with a Project or Package.
pub(crate) fn open(app: &mut App, params: &Value) -> Result {
    let path = PathBuf::from(text(params, "path", OPEN)?);
    let recover = match params.get("recover") {
        None | Some(Value::Null) => Recover::No,
        Some(v) => match v.as_str() {
            Some("staging") => Recover::Staging,
            Some("lastGood") => Recover::LastGood,
            _ => {
                return Err(invalid(
                    OPEN,
                    "`recover` must be staging or lastGood".into(),
                ));
            }
        },
    };
    let location = Location::of(&path);
    let root = location.root();
    match &location {
        Location::Folder(p) => {
            let files = opendash_file::read_folder(p, recover).map_err(Error::File)?;
            let dir = p.clone();
            app.open_files(
                files,
                Some(location),
                root.as_deref(),
                &CacheFrom::Folder(&dir),
                None,
            )?;
        }
        Location::Package(p) => {
            let files =
                opendash_file::read_package_with(p, app.package_limits()).map_err(Error::File)?;
            app.open_files(
                files,
                Some(location),
                root.as_deref(),
                &CacheFrom::Files,
                None,
            )?;
        }
    }
    // The document it replaced is gone, unsaved changes and all: so is its recovery copy.
    app.discard_own_recovery();
    Ok(())
}

fn is_package_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(opendash_file::package::EXTENSION))
}

/// `file.save`: saves where the document was opened from or last saved; with `path`, as Save As.
pub(crate) fn save(app: &mut App, params: &Value) -> Result {
    if params.get("path").is_some_and(|p| !p.is_null()) {
        let path = PathBuf::from(text(params, "path", SAVE)?);
        let location = if is_package_path(&path) {
            Location::Package(path)
        } else {
            Location::Folder(path)
        };
        return app.save_to(location, true);
    }
    let location = app.file.location.clone().ok_or_else(|| {
        invalid(
            SAVE,
            "the document has not been saved yet: give `path`, or use file.saveAs".into(),
        )
    })?;
    app.save_to(location, true)
}

/// `file.saveAs`: saves to a new Project folder or Package, which becomes the document's.
pub(crate) fn save_as(app: &mut App, params: &Value) -> Result {
    let path = PathBuf::from(text(params, "path", SAVE_AS)?);
    let flag = |key: &str, default: bool| match params.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(SAVE_AS, format!("`{key}` must be true or false"))),
    };
    let package = flag("package", is_package_path(&path))?;
    let data = flag("data", true)?;
    app.save_to(
        if package {
            Location::Package(path)
        } else {
            Location::Folder(path)
        },
        data,
    )
}

#[cfg(test)]
mod tests;
