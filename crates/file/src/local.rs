//! Local overrides (`.local/`, ADR 0003): per-user settings beside a Project folder, never part
//! of its content. Gitignored (`.gitignore` lists `.local/`), never in the Manifest, never in a
//! Package.
//!
//! **Source-path remapping** (`.local/paths.odl`): a Source file the Project's Queries name at
//! one path is read from another on this machine. In the `.odm` syntax ([`crate::syntax`]):
//!
//! ```text
//! remap 'data/Sales.csv'
//!   to: '/mnt/share/Sales.csv'
//! ```
//!
//! The header is the path as the Queries name it, the `to` the path to read instead; each is
//! relative to the Project folder when inside it (with `/`), otherwise absolute, as in a Query.
//! Paths are resolved exactly as the Queries' are ([`resolve`]), so a Windows path in a Project
//! opened on Linux is still matched as written.
//!
//! The app applies a remap on open (to the Queries and to the Data cache's Source fingerprints)
//! and reverses it on save, so the definitions and the cache metadata always hold the original
//! paths: `.odq` files never change per user, and a remap alone never makes a table Stale.

use std::fs;
use std::path::{Path, PathBuf};

use opendash_data::Query;
use opendash_data::source::File;

use crate::cache::{Fingerprint, Provenance};
use crate::folder::{exists, io, join, read_capped, rename, write_file};
use crate::project::{relative, source_file_mut};
use crate::syntax::{self, Item, Node, Value};
use crate::{Error, Result};

/// The Local overrides folder inside a Project.
pub const DIR: &str = ".local";
/// The source-path remaps file.
pub const PATHS: &str = ".local/paths.odl";
/// Largest `paths.odl` read.
pub const MAX_PATHS: u64 = 1 << 20;
/// Most remaps one `paths.odl` may hold.
const MAX_REMAPS: usize = 10_000;

/// Source-path remaps: `(from, to)`, both resolved against the Project folder. At most one per
/// `from`.
pub type Remaps = Vec<(PathBuf, PathBuf)>;

/// A Source path as written in a Query (or a remap) resolved against the Project folder `root`:
/// relative paths are joined to it (split on `/`), absolute ones kept. The same rule as reading
/// a Query, so the two always match.
pub fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let mut full = root.to_path_buf();
    full.extend(path.split('/').filter(|s| !s.is_empty()));
    full
}

/// `path` as written beside `root`: relative with `/` when inside it, otherwise absolute.
pub fn written(path: &Path, root: &Path) -> String {
    relative(path, root).unwrap_or_else(|| path.display().to_string())
}

/// Where `path` is read from under `remaps` (itself if no remap names it).
pub fn map<'a>(remaps: &'a [(PathBuf, PathBuf)], path: &'a Path) -> &'a Path {
    remaps
        .iter()
        .find(|(from, _)| from == path)
        .map_or(path, |(_, to)| to.as_path())
}

/// The path the Project names for `path` read under `remaps` (itself if no remap reads it).
pub fn unmap<'a>(remaps: &'a [(PathBuf, PathBuf)], path: &'a Path) -> &'a Path {
    remaps
        .iter()
        .find(|(_, to)| to == path)
        .map_or(path, |(from, _)| from.as_path())
}

/// Rewrites the Source file paths of `queries` under `remaps`: from → to, or to → from if
/// `reverse`.
pub fn remap_queries(remaps: &[(PathBuf, PathBuf)], queries: &mut [Query], reverse: bool) {
    if remaps.is_empty() {
        return;
    }
    for q in queries {
        if let File::Path(p) = source_file_mut(&mut q.source) {
            *p = swap(remaps, p, reverse);
        }
    }
}

/// Rewrites the Source file paths of `provenance` under `remaps`, as [`remap_queries`].
pub fn remap_provenance(remaps: &[(PathBuf, PathBuf)], provenance: &mut Provenance, reverse: bool) {
    for s in &mut provenance.sources {
        if let Fingerprint::File { path, .. } = s {
            *path = swap(remaps, path, reverse);
        }
    }
}

fn swap(remaps: &[(PathBuf, PathBuf)], path: &Path, reverse: bool) -> PathBuf {
    if reverse {
        unmap(remaps, path).to_path_buf()
    } else {
        map(remaps, path).to_path_buf()
    }
}

/// Parses `paths.odl` bytes, resolving paths against `root`. Never panics.
pub fn parse_paths(bytes: &[u8], root: &Path) -> Result<Remaps> {
    let err = |message: String| Error::Part {
        path: PATHS.into(),
        message,
    };
    let text = std::str::from_utf8(bytes).map_err(|_| err("not UTF-8 text".into()))?;
    let items = syntax::parse(text).map_err(|e| err(e.to_string()))?;
    let mut out: Remaps = Vec::new();
    for item in items {
        let Item::Node(node) = item else {
            return Err(err("expected `remap '<path>'` blocks only".into()));
        };
        let from = match (node.kind.as_str(), node.name()) {
            ("remap", Some(from)) if !from.is_empty() => from,
            _ => {
                return Err(err(format!(
                    "expected `remap '<path>'`, found `{} {}`",
                    node.kind, node.header
                )));
            }
        };
        let to = node
            .prop("to")
            .map(Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| err(format!("remap '{from}' has no `to:`")))?;
        if out.len() >= MAX_REMAPS {
            return Err(err(format!("more than {MAX_REMAPS} remaps")));
        }
        let from = resolve(root, &from);
        let to = resolve(root, to);
        out.retain(|(f, _)| *f != from);
        out.push((from, to));
    }
    Ok(out)
}

/// `paths.odl` bytes for `remaps`, with paths written relative to `root` when inside it.
pub fn print_paths(remaps: &[(PathBuf, PathBuf)], root: &Path) -> Vec<u8> {
    let items: Vec<Item> = remaps
        .iter()
        .map(|(from, to)| {
            let mut node = Node {
                kind: "remap".into(),
                header: format!("'{}'", written(from, root).replace('\'', "''")),
                items: Vec::new(),
            };
            node.items
                .push(Item::Prop("to".into(), Value::Name(written(to, root))));
            Item::Node(node)
        })
        .collect();
    syntax::print(&items).into_bytes()
}

/// The remaps in the Project folder `root` (none if there is no `paths.odl`).
pub fn read_paths(root: &Path) -> Result<Remaps> {
    match read_capped(&join(root, PATHS), MAX_PATHS)? {
        Some(bytes) => parse_paths(&bytes, root),
        None => Ok(Vec::new()),
    }
}

/// Writes `remaps` to `root`'s `paths.odl` (written beside it, then renamed over it); removes it
/// when there are none. Unchanged bytes aren't rewritten.
pub fn write_paths(root: &Path, remaps: &[(PathBuf, PathBuf)]) -> Result {
    let live = join(root, PATHS);
    if remaps.is_empty() {
        if exists(&live) {
            fs::remove_file(&live).map_err(|e| io(&live, e))?;
        }
        return Ok(());
    }
    let bytes = print_paths(remaps, root);
    if read_capped(&live, MAX_PATHS).ok().flatten().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let tmp = join(root, &format!("{PATHS}.tmp"));
    let written = write_file(&tmp, &bytes).and_then(|()| rename(&tmp, &live));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_round_trip_relative_and_absolute() {
        let root = Path::new("/p/Proj");
        let remaps = vec![
            (
                root.join("data/Sales.csv"),
                PathBuf::from("/mnt/share/Sales.csv"),
            ),
            (resolve(root, r"C:\Data\it's.csv"), root.join("local/x.csv")),
        ];
        let bytes = print_paths(&remaps, root);
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(
            text,
            "remap 'data/Sales.csv'\n  to: '/mnt/share/Sales.csv'\nremap 'C:\\Data\\it''s.csv'\n  \
             to: 'local/x.csv'\n"
        );
        assert_eq!(parse_paths(&bytes, root).unwrap(), remaps);
    }

    #[test]
    fn map_and_unmap_are_inverse() {
        let remaps = vec![(PathBuf::from("/a"), PathBuf::from("/b"))];
        assert_eq!(map(&remaps, Path::new("/a")), Path::new("/b"));
        assert_eq!(unmap(&remaps, Path::new("/b")), Path::new("/a"));
        assert_eq!(map(&remaps, Path::new("/c")), Path::new("/c"));
    }

    #[test]
    fn bad_paths_files_are_errors() {
        let root = Path::new("/p");
        for bad in [
            "remap 'x'\n",
            "table 'x'\n  to: 'y'\n",
            "to: 'y'\n",
            "remap 'x'\n  to: ''\n",
            "remap\n",
            "\u{0}\u{1}",
        ] {
            assert!(parse_paths(bad.as_bytes(), root).is_err(), "{bad:?}");
        }
        assert!(parse_paths(&[0xff, 0xfe], root).is_err());
        assert_eq!(parse_paths(b"", root).unwrap(), Vec::new());
    }
}
