//! The File menu (ADR 0003): New, Open, Save and Save As, over `opendash_file`.
//!
//! The Document maps to an `opendash_file::Project` (Queries, Enable load, Model, Project
//! culture); what the files held beyond it (unknown content) and where they live stay in
//! [`FileState`], beside the Document, so undo never touches them.
//!
//! Opening (and New) replaces the Document and clears undo and redo: not undoable, as in Power
//! BI. There is no Data cache yet (M2.6), so an opened Document's Tables are empty and every
//! loaded Query is Stale until Refresh or Close & Apply.
//!
//! The web build has no file system: it uses [`App::open_package_bytes`] and
//! [`App::package_bytes`] instead of the commands' paths.

use std::path::{Path, PathBuf};

use opendash_file::{FileSet, MANIFEST, Preserved, Project, Recover};
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
    fn root(&self) -> Option<PathBuf> {
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
    location: Option<Location>,
    /// The Project culture read from the files; `None` for a new document (en-US).
    culture: Option<String>,
    /// Kept up to date by renames (`rename.rs`).
    pub(crate) preserved: Preserved,
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
     package: logical — write a .odash Package (default: whether path ends in .odash)";

impl App {
    /// Where the document was opened from or last saved to.
    pub fn location(&self) -> Option<&Location> {
        self.file.location.as_ref()
    }

    /// Opens a `.odash` Package from its bytes (the web build's File > Open). Like `file.open`:
    /// replaces the Document, not undoable. Relative source paths stay relative.
    pub fn open_package_bytes(&mut self, bytes: &[u8]) -> Result {
        let files = opendash_file::unpack(bytes).map_err(Error::File)?;
        self.open_files(&files, None, None)
    }

    /// The document as `.odash` Package bytes (the web build's Save: a download).
    pub fn package_bytes(&self) -> Result<Vec<u8>> {
        let files = self.files(None)?;
        opendash_file::pack(&files).map_err(Error::File)
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

    fn files(&self, root: Option<&Path>) -> Result<FileSet> {
        opendash_file::encode(&self.project(), root, &self.file.preserved).map_err(Error::File)
    }

    fn open_files(
        &mut self,
        files: &FileSet,
        location: Option<Location>,
        root: Option<&Path>,
    ) -> Result {
        let (project, preserved) = opendash_file::decode(files, root).map_err(Error::File)?;
        let mut doc = Document::default();
        for t in &project.model.tables {
            doc.tables
                .push(opendash_file::empty_table(t).map_err(Error::Data)?);
        }
        doc.stale = project
            .queries
            .iter()
            .filter(|q| {
                !project.unloaded.contains(&q.name) || project.model.table(&q.name).is_some()
            })
            .map(|q| q.name.clone())
            .collect();
        let stale = doc.stale.len();
        doc.queries = project.queries;
        doc.unloaded = project.unloaded;
        doc.model = project.model;
        self.replace(doc);
        self.file = FileState {
            location,
            culture: Some(project.culture),
            preserved,
        };
        if stale > 0 {
            self.note(&format!(
                "{stale} {} no data yet: Refresh to load",
                if stale == 1 {
                    "query has"
                } else {
                    "queries have"
                }
            ));
        }
        Ok(())
    }

    fn save_to(&mut self, location: Location) -> Result {
        let root = location.root();
        let files = self.files(root.as_deref())?;
        match &location {
            Location::Folder(p) => opendash_file::write_folder(p, &files),
            Location::Package(p) => opendash_file::write_package(p, &files),
        }
        .map_err(Error::File)?;
        self.file.location = Some(location);
        Ok(())
    }
}

/// `file.new`: an empty document.
pub(crate) fn new(app: &mut App, _params: &Value) -> Result {
    app.replace(Document::default());
    app.file = FileState::default();
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
    let files = match &location {
        Location::Folder(p) => opendash_file::read_folder(p, recover),
        Location::Package(p) => opendash_file::read_package(p),
    }
    .map_err(Error::File)?;
    let root = location.root();
    app.open_files(&files, Some(location), root.as_deref())
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
        return app.save_to(location);
    }
    let location = app.file.location.clone().ok_or_else(|| {
        invalid(
            SAVE,
            "the document has not been saved yet: give `path`, or use file.saveAs".into(),
        )
    })?;
    app.save_to(location)
}

/// `file.saveAs`: saves to a new Project folder or Package, which becomes the document's.
pub(crate) fn save_as(app: &mut App, params: &Value) -> Result {
    let path = PathBuf::from(text(params, "path", SAVE_AS)?);
    let package = match params.get("package") {
        None | Some(Value::Null) => is_package_path(&path),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(SAVE_AS, "`package` must be true or false".into()))?,
    };
    app.save_to(if package {
        Location::Package(path)
    } else {
        Location::Folder(path)
    })
}

#[cfg(test)]
mod tests;
