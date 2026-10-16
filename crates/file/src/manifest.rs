//! The Manifest (`project.mcraft.json`, glossary) and Format versions.
//!
//! ```json
//! {
//!   "formatVersion": 1,
//!   "parts": {
//!     "model/model.mcm": "sha256:…",
//!     "queries/Sales.mcq": "sha256:…"
//!   }
//! }
//! ```
//!
//! `parts` lists every definition file with the SHA-256 of its bytes; it is written last on
//! save, so a part that doesn't match means an interrupted save. Not parts: the Manifest itself,
//! `.gitignore` (the user's to edit), and `.cache/`, `.local/` and `.staging/` (never shared or
//! transient). Keys are written sorted, 2-space indented, LF, trailing newline. Unknown keys are
//! kept and written back.
//!
//! **Versioning.** A newer `formatVersion` than [`FORMAT_VERSION`] is refused
//! ([`Error::NewerFormat`]); an older one is migrated on open, one [`Migration`] per step, over
//! the parsed [`Tree`]. Saving always writes [`FORMAT_VERSION`].

use std::collections::BTreeMap;

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::syntax::Item;
use crate::{Error, Result, check_path};

/// The Manifest's file name, at the Project's root.
pub const MANIFEST: &str = "project.mcraft.json";
/// The Format version this build writes, and the newest it reads.
pub const FORMAT_VERSION: u64 = 1;
/// Largest accepted Manifest, in bytes.
pub const MAX_MANIFEST: usize = 16 << 20;
/// Most parts a Manifest may list.
pub const MAX_PARTS: usize = 100_000;

/// Folders and files at the root that are never parts.
pub const NOT_PARTS: &[&str] = &[".gitignore", ".cache", ".local", ".staging", MANIFEST];

/// `sha256:` and the lowercase hex SHA-256 of `bytes`.
pub fn hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(7 + 64);
    out.push_str("sha256:");
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// Whether `path` may be a part: a safe relative path outside the [`NOT_PARTS`].
pub fn check_part(path: &str) -> std::result::Result<(), String> {
    check_path(path)?;
    let first = path.split('/').next().unwrap_or("");
    if NOT_PARTS.contains(&first) {
        return Err(format!("`{path}` can't be a part"));
    }
    Ok(())
}

/// A parsed Manifest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Manifest {
    pub version: u64,
    /// Part path → hash.
    pub parts: BTreeMap<String, String>,
    /// Keys we don't know, kept for the next save.
    pub extra: Map<String, Value>,
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Manifest> {
        let bad = |m: &str| Error::Manifest(m.into());
        if bytes.len() > MAX_MANIFEST {
            return Err(bad("larger than 16 MB"));
        }
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| Error::Manifest(format!("not valid JSON: {e}")))?;
        let Value::Object(mut extra) = value else {
            return Err(bad("must be a JSON object"));
        };
        let version = extra
            .remove("formatVersion")
            .ok_or_else(|| bad("`formatVersion` is missing"))?
            .as_u64()
            .ok_or_else(|| bad("`formatVersion` must be a whole number"))?;
        let parts = match extra.remove("parts") {
            None => Map::new(),
            Some(Value::Object(parts)) => parts,
            Some(_) => return Err(bad("`parts` must be an object of path: hash")),
        };
        if parts.len() > MAX_PARTS {
            return Err(Error::Manifest(format!("more than {MAX_PARTS} parts")));
        }
        let mut out = BTreeMap::new();
        for (path, h) in parts {
            check_part(&path).map_err(Error::Manifest)?;
            let h = h
                .as_str()
                .filter(|h| {
                    h.strip_prefix("sha256:").is_some_and(|x| {
                        x.len() == 64 && x.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    })
                })
                .ok_or_else(|| {
                    Error::Manifest(format!(
                        "`{path}`: hash must be `sha256:` and 64 hex digits"
                    ))
                })?;
            out.insert(path, h.to_string());
        }
        Ok(Manifest {
            version,
            parts: out,
            extra,
        })
    }

    /// The Manifest's bytes (see the module docs).
    pub fn print(&self) -> Vec<u8> {
        let mut map = Map::new();
        for (k, v) in &self.extra {
            map.insert(k.clone(), v.clone());
        }
        map.insert("formatVersion".into(), Value::from(self.version));
        let parts: Map<String, Value> = self
            .parts
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect();
        map.insert("parts".into(), Value::Object(parts));
        let value = sorted(Value::Object(map));
        // Serialising a `Value` can't fail; an empty object is the harmless fallback.
        let mut text = serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".into());
        text.push('\n');
        text.into_bytes()
    }
}

/// `v` with every object's keys sorted, whatever map order serde_json was built with.
pub(crate) fn sorted(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().map(|(k, v)| (k, sorted(v))).collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sorted).collect()),
        v => v,
    }
}

/// A Project as read, before it is decoded: what migrations work on.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Tree {
    pub version: u64,
    /// Unknown Manifest keys.
    pub extra: Map<String, Value>,
    /// `.mcm` / `.mcq` parts, parsed.
    pub syntax: BTreeMap<String, Vec<Item>>,
    /// Every other part, as bytes.
    pub other: BTreeMap<String, Vec<u8>>,
}

/// Upgrades a [`Tree`] from Format version `from` to `from + 1`.
#[derive(Clone, Copy)]
pub struct Migration {
    pub from: u64,
    pub run: fn(&mut Tree) -> std::result::Result<(), String>,
}

/// Every migration, oldest first. Format version 1 is the first; none yet.
pub static MIGRATIONS: &[Migration] = &[];

/// Brings `tree` up to [`FORMAT_VERSION`] with `migrations`.
pub fn migrate(tree: &mut Tree, migrations: &[Migration]) -> Result {
    if tree.version > FORMAT_VERSION {
        return Err(Error::NewerFormat {
            found: tree.version,
            supported: FORMAT_VERSION,
        });
    }
    while tree.version < FORMAT_VERSION {
        let from = tree.version;
        let m = migrations
            .iter()
            .find(|m| m.from == from)
            .ok_or(Error::NoMigration { from })?;
        (m.run)(tree)
            .map_err(|e| Error::Manifest(format!("migrating from format version {from}: {e}")))?;
        tree.version = from.saturating_add(1);
    }
    Ok(())
}
