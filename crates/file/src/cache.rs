//! The Data cache (ADR 0003 and its M2 amendment): a Project's imported data, beside its
//! definitions, so opening doesn't re-import.
//!
//! ```text
//! .cache/<stem>.parquet     the Table as loaded (after Model type overrides), zstd
//! .cache/<stem>.meta        its metadata (JSON, below)
//! ```
//!
//! `<stem>` is the Table's file stem, as for `model/tables/<stem>.mcm`. The folder is
//! gitignored and never listed in the Manifest; a Package carries it by default (Parquet stored,
//! not deflated) unless saved without data.
//!
//! **Metadata** is a separate `.meta` file rather than Parquet key-value metadata: it is read
//! without touching the Parquet (open checks it first), and rewritten alone when only it changes
//! (a Query edited but not applied). Deterministic: sorted keys, 2-space indent, LF.
//!
//! ```json
//! {
//!   "columns": [{"name": "Region", "type": "text"}, {"name": "Qty", "type": "decimal"}],
//!   "formatVersion": 1,
//!   "m": "sha256:…",
//!   "overrides": [{"column": "Qty", "dataType": "decimal", "queryType": "whole"}],
//!   "refreshed": 1760000000000,
//!   "rows": 3,
//!   "sources": [{"modified": 1760000000123456789, "path": "data/Sales.csv", "size": 52}],
//!   "table": "Sales"
//! }
//! ```
//!
//! - `m`: the [`m_hash`] of the Query (and the Queries it merges) **when the data was loaded**.
//!   It differs from the saved Query's after an edit that wasn't applied: that table reopens
//!   Stale.
//! - `sources`: a [`Fingerprint`] per Source file read for it (the Query's and those of the
//!   Queries it merges), taken when it was loaded: path (relative to the Project when inside
//!   it), size and modified time in nanoseconds (`"missing": true` if it couldn't be read); no
//!   content hash, so open stays fast. A picked file held in memory (web) has `name` and the
//!   SHA-256 of its bytes instead.
//! - `refreshed`: when it was loaded, in milliseconds since 1970 (absent if unknown). Metadata
//!   only: nothing time-dependent is in the definitions.
//! - `columns`: each column's name and type as cached. Fixed decimals are stored in the Parquet
//!   as whole numbers × 10⁴ (exact), since the Parquet reader takes decimals as floats.
//! - `overrides`: the Model type overrides already applied to the cached data; must match the
//!   Model's on open, and are never applied again.
//!
//! **On open** a table is used only if its metadata parses, names the table, and its columns
//! and overrides match the Model; its Parquet is then read through the hostile-input-checked
//! Parquet reader within the memory cap. It is **Stale** if its `m` or `sources` differ from
//! the Project's Queries and files now (the cached data is still shown). Anything else (missing,
//! corrupt, too big) gives an empty table, Stale, and a notice offering Refresh. Opening never
//! reads a Source, only its file metadata.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::cast::AsArray;
use arrow_array::types::{Decimal32Type, Decimal64Type, Int64Type};
use arrow_array::{Array, ArrayRef, Decimal64Array, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use metriccraft_data::m::{self, MQuery};
use metriccraft_data::source::File;
use metriccraft_data::source::parquet::ChunkReader;
use metriccraft_data::step::{ColumnType, FIXED};
use metriccraft_data::{Query, Table};
use metriccraft_model::ModelTable;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::{Map, Value, json};

use crate::folder::{exists, io, join, read_capped, rename, write_file};
use crate::manifest::{hash, sorted};
use crate::project::{relative, source_file_mut};
use crate::{Error, Result};

/// The Data cache folder inside a Project.
pub const DIR: &str = ".cache";
/// The metadata format this build writes and reads.
pub const META_VERSION: u64 = 1;
/// Largest `.meta` file read.
pub const MAX_META: u64 = 16 << 20;
/// Most Sources one table's metadata may list.
const MAX_SOURCES: usize = 10_000;

/// `.cache/<stem>.parquet`.
pub fn parquet_path(stem: &str) -> String {
    format!("{DIR}/{stem}.parquet")
}

/// `.cache/<stem>.meta`.
pub fn meta_path(stem: &str) -> String {
    format!("{DIR}/{stem}.meta")
}

/// What a Source file was like when its data was read (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fingerprint {
    /// A file on disk: `(size, modified ns since 1970)`, or `None` if it couldn't be read.
    File {
        path: PathBuf,
        stat: Option<(u64, u64)>,
    },
    /// A picked file held in memory: its name and the SHA-256 of its bytes.
    Bytes { name: String, sha256: String },
}

impl Fingerprint {
    /// `file`'s fingerprint now (its metadata only; bytes held in memory are hashed).
    pub fn of(file: &File) -> Fingerprint {
        match file {
            File::Path(path) => Fingerprint::File {
                path: path.clone(),
                stat: stat(path),
            },
            File::Bytes { name, bytes } => Fingerprint::Bytes {
                name: name.clone(),
                sha256: hash(bytes),
            },
        }
    }
}

fn stat(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((meta.len(), u64::try_from(modified.as_nanos()).ok()?))
}

/// How a table's data was loaded: the Data cache's metadata, kept with the table in the
/// Document so undo restores it with the data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// [`m_hash`] of the Query when it loaded.
    pub m: String,
    /// The Sources read, as they were then.
    pub sources: Vec<Fingerprint>,
    /// When, in milliseconds since 1970; `None` if unknown.
    pub refreshed: Option<u64>,
}

impl Provenance {
    /// The provenance of Query `name` loaded now from `queries` (stamped `refreshed`).
    pub fn of(queries: &[Query], name: &str, refreshed: Option<u64>) -> Provenance {
        Provenance {
            m: m_hash(queries, name),
            sources: closure(queries, name)
                .into_iter()
                .map(|q| Fingerprint::of(q.source.file()))
                .collect(),
            refreshed,
        }
    }

    /// Whether data loaded with this provenance is out of date for `queries` and their Source
    /// files now (Stale).
    pub fn is_stale(&self, queries: &[Query], name: &str) -> bool {
        let now = Provenance::of(queries, name, None);
        now.m != self.m || now.sources != self.sources
    }
}

/// The time now in milliseconds since 1970 (`None` on the web build, which has no clock here).
pub fn now_ms() -> Option<u64> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?;
        u64::try_from(d.as_millis()).ok()
    }
    #[cfg(target_arch = "wasm32")]
    None
}

/// Query `name` and the Queries it merges, transitively, each once (cycles end).
pub fn closure<'a>(queries: &'a [Query], name: &str) -> Vec<&'a Query> {
    let mut out: Vec<&Query> = Vec::new();
    let mut todo: Vec<&str> = vec![name];
    while let Some(n) = todo.pop() {
        if out.iter().any(|q| q.name == n) {
            continue;
        }
        let Some(q) = queries.iter().find(|q| q.name == n) else {
            continue;
        };
        out.push(q);
        todo.extend(
            queries
                .iter()
                .filter(|o| q.merges(&o.name))
                .map(|o| o.name.as_str()),
        );
    }
    out
}

/// The hash of Query `name`'s M and that of every Query it merges, with their cultures, and
/// with file paths left out (they move with the Project; Source [`Fingerprint`]s cover them).
pub fn m_hash(queries: &[Query], name: &str) -> String {
    let mut text = String::new();
    for q in closure(queries, name) {
        let mut source = q.source.clone();
        *source_file_mut(&mut source) = File::Path(PathBuf::new());
        text.push_str(&m::print(&MQuery {
            source,
            steps: q.steps.clone(),
            culture: Some(q.culture),
        }));
        text.push('\n');
    }
    hash(text.as_bytes())
}

/// A table's `.meta` (see the module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    pub table: String,
    pub rows: u64,
    /// Name and type of each cached column, in order.
    pub columns: Vec<(String, ColumnType)>,
    /// `(column, dataType, queryType)` for each Model type override applied to the data.
    pub overrides: Vec<(String, ColumnType, ColumnType)>,
    pub provenance: Provenance,
}

/// The `(column, dataType, queryType)` overrides of `model`'s loaded columns (Calculated
/// columns are never cached).
pub fn overrides(model: &ModelTable) -> Vec<(String, ColumnType, ColumnType)> {
    model
        .columns
        .iter()
        .filter(|c| c.overridden() && !c.is_calculated())
        .map(|c| (c.name.clone(), c.data_type, c.query_type))
        .collect()
}

impl Meta {
    /// The metadata for `table` as loaded with `provenance`, under `model`'s overrides.
    pub fn new(table: &Table, model: &ModelTable, provenance: Provenance) -> Result<Meta> {
        let schema = table.batch().schema();
        let columns = schema
            .fields()
            .iter()
            .map(|f| {
                ColumnType::of_data_type(f.data_type())
                    .map(|t| (f.name().clone(), t))
                    .ok_or_else(|| {
                        Error::Save(format!(
                            "table `{}`: column `{}` has type {}, which the Data cache can't hold",
                            table.name(),
                            f.name(),
                            f.data_type()
                        ))
                    })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Meta {
            table: table.name().into(),
            rows: u64::try_from(table.rows()).unwrap_or(u64::MAX),
            columns,
            overrides: overrides(model),
            provenance,
        })
    }

    /// Whether this metadata fits `model` (same table, columns, types and overrides; Calculated
    /// columns aren't cached).
    pub fn fits(&self, model: &ModelTable) -> bool {
        let loaded: Vec<_> = model
            .columns
            .iter()
            .filter(|c| !c.is_calculated())
            .collect();
        self.table == model.name
            && self.overrides == overrides(model)
            && self.columns.len() == loaded.len()
            && self
                .columns
                .iter()
                .zip(loaded)
                .all(|((n, t), c)| *n == c.name && *t == c.data_type)
    }

    /// The `.meta` bytes; paths inside `root` are written relative to it.
    pub fn print(&self, root: Option<&Path>) -> Vec<u8> {
        let p = &self.provenance;
        let sources: Vec<Value> = p
            .sources
            .iter()
            .map(|s| match s {
                Fingerprint::File { path, stat } => {
                    let path = root
                        .and_then(|r| relative(path, r))
                        .unwrap_or_else(|| path.display().to_string());
                    match stat {
                        Some((size, modified)) => {
                            json!({"path": path, "size": size, "modified": modified})
                        }
                        None => json!({"path": path, "missing": true}),
                    }
                }
                Fingerprint::Bytes { name, sha256 } => json!({"name": name, "sha256": sha256}),
            })
            .collect();
        let mut map = Map::new();
        map.insert("formatVersion".into(), json!(META_VERSION));
        map.insert("table".into(), json!(self.table));
        map.insert("rows".into(), json!(self.rows));
        map.insert(
            "columns".into(),
            self.columns
                .iter()
                .map(|(n, t)| json!({"name": n, "type": t.name()}))
                .collect(),
        );
        map.insert(
            "overrides".into(),
            self.overrides
                .iter()
                .map(|(c, d, q)| json!({"column": c, "dataType": d.name(), "queryType": q.name()}))
                .collect(),
        );
        map.insert("m".into(), json!(p.m));
        map.insert("sources".into(), Value::Array(sources));
        if let Some(t) = p.refreshed {
            map.insert("refreshed".into(), json!(t));
        }
        // Serialising a `Value` can't fail; an empty object is the harmless fallback.
        let mut text = serde_json::to_string_pretty(&sorted(Value::Object(map)))
            .unwrap_or_else(|_| "{}".into());
        text.push('\n');
        text.into_bytes()
    }

    /// Parses `.meta` bytes; relative paths are resolved against `root`. Never panics.
    pub fn parse(bytes: &[u8], root: Option<&Path>) -> std::result::Result<Meta, String> {
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_META {
            return Err("metadata larger than 16 MB".into());
        }
        let v: Value = serde_json::from_slice(bytes)
            .map_err(|e| format!("metadata is not valid JSON: {e}"))?;
        let version = v.get("formatVersion").and_then(Value::as_u64);
        if version != Some(META_VERSION) {
            return Err(format!(
                "metadata format version {version:?} is not {META_VERSION}"
            ));
        }
        let text = |v: &Value, k: &str| -> std::result::Result<String, String> {
            v.get(k)
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| format!("metadata: `{k}` must be text"))
        };
        let num = |v: &Value, k: &str| {
            v.get(k)
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("metadata: `{k}` must be a whole number"))
        };
        let ty = |v: &Value, k: &str| -> std::result::Result<ColumnType, String> {
            let name = text(v, k)?;
            ColumnType::from_name(&name).ok_or_else(|| format!("metadata: unknown type `{name}`"))
        };
        let list = |k: &str| {
            v.get(k)
                .and_then(Value::as_array)
                .ok_or_else(|| format!("metadata: `{k}` must be a list"))
        };
        let columns = list("columns")?
            .iter()
            .map(|c| Ok((text(c, "name")?, ty(c, "type")?)))
            .collect::<std::result::Result<Vec<_>, String>>()?;
        let overrides = list("overrides")?
            .iter()
            .map(|c| Ok((text(c, "column")?, ty(c, "dataType")?, ty(c, "queryType")?)))
            .collect::<std::result::Result<Vec<_>, String>>()?;
        let sources = list("sources")?;
        if sources.len() > MAX_SOURCES {
            return Err(format!("metadata: more than {MAX_SOURCES} sources"));
        }
        let sources = sources
            .iter()
            .map(|s| {
                if s.get("sha256").is_some() {
                    return Ok(Fingerprint::Bytes {
                        name: text(s, "name")?,
                        sha256: text(s, "sha256")?,
                    });
                }
                let path = text(s, "path")?;
                let path = match root {
                    Some(root) if Path::new(&path).is_relative() => {
                        let mut full = root.to_path_buf();
                        full.extend(path.split('/').filter(|s| !s.is_empty()));
                        full
                    }
                    _ => PathBuf::from(path),
                };
                let stat = if s.get("missing").and_then(Value::as_bool) == Some(true) {
                    None
                } else {
                    Some((num(s, "size")?, num(s, "modified")?))
                };
                Ok(Fingerprint::File { path, stat })
            })
            .collect::<std::result::Result<Vec<_>, String>>()?;
        let refreshed = match v.get("refreshed") {
            None | Some(Value::Null) => None,
            Some(_) => Some(num(&v, "refreshed")?),
        };
        Ok(Meta {
            table: text(&v, "table")?,
            rows: num(&v, "rows")?,
            columns,
            overrides,
            provenance: Provenance {
                m: text(&v, "m")?,
                sources,
                refreshed,
            },
        })
    }
}

// ---- the Parquet ----

/// `table` as cache Parquet bytes (zstd; fixed decimals as whole numbers × 10⁴).
pub fn encode_table(table: &Table) -> Result<Vec<u8>> {
    let err = |e: &dyn std::fmt::Display| {
        Error::Save(format!(
            "table `{}`: writing the Data cache failed: {e}",
            table.name()
        ))
    };
    let batch = table.batch();
    let schema = batch.schema();
    let mut fields = Vec::with_capacity(batch.num_columns());
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(batch.num_columns());
    for (f, c) in schema.fields().iter().zip(batch.columns()) {
        let c: ArrayRef = match c.data_type() {
            DataType::Decimal32(..) => {
                let d = c.as_primitive::<Decimal32Type>();
                let values: Vec<i64> = d.values().iter().map(|&v| i64::from(v)).collect();
                Arc::new(Int64Array::new(values.into(), d.nulls().cloned()))
            }
            DataType::Decimal64(..) => {
                let d = c.as_primitive::<Decimal64Type>();
                Arc::new(Int64Array::new(d.values().clone(), d.nulls().cloned()))
            }
            _ => Arc::clone(c),
        };
        fields.push(Field::new(f.name(), c.data_type().clone(), true));
        columns.push(c);
    }
    let schema = Arc::new(Schema::new(fields));
    let batch = RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|e| err(&e))?;
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_created_by("MetricCraft".into())
        .build();
    let mut out = Vec::new();
    let mut w = ArrowWriter::try_new(&mut out, schema, Some(props)).map_err(|e| err(&e))?;
    w.write(&batch).map_err(|e| err(&e))?;
    w.close().map_err(|e| err(&e))?;
    Ok(out)
}

/// Reads cache Parquet (bytes or a file) for the table `meta` describes, using at most `limit`
/// bytes. Errors (corrupt, too big, not what `meta` says) are messages; never panics.
pub fn decode_table<R: ChunkReader + 'static>(
    reader: R,
    meta: &Meta,
    limit: usize,
) -> std::result::Result<Table, String> {
    let name = meta.table.as_str();
    let mut table =
        metriccraft_data::source::parquet::read(name, reader, limit).map_err(|e| e.to_string())?;
    let schema = table.batch().schema();
    if schema.fields().len() != meta.columns.len()
        || u64::try_from(table.rows()).ok() != Some(meta.rows)
    {
        return Err("the cached data doesn't match its metadata".into());
    }
    for (i, (f, (cname, ty))) in schema.fields().iter().zip(&meta.columns).enumerate() {
        if f.name() != cname {
            return Err(format!("cached column `{}` should be `{cname}`", f.name()));
        }
        if *ty == ColumnType::FixedDecimal {
            let column = table
                .batch()
                .columns()
                .get(i)
                .ok_or("a cached column is missing")?;
            if !column.data_type().is_integer() {
                return Err(format!(
                    "cached column `{cname}` is not stored as fixed decimal"
                ));
            }
            let whole = arrow_cast::cast(column, &DataType::Int64).map_err(|e| e.to_string())?;
            let whole = whole.as_primitive::<Int64Type>();
            let fixed = Decimal64Array::new(whole.values().clone(), whole.nulls().cloned())
                .with_data_type(FIXED);
            let left = limit.saturating_sub(table.memory());
            table = table
                .with_column(i, Arc::new(fixed), left)
                .map_err(|e| e.to_string())?;
        } else if ColumnType::of_data_type(f.data_type()) != Some(*ty) {
            return Err(format!(
                "cached column `{cname}` has type {}, not {}",
                f.data_type(),
                ty.name()
            ));
        }
    }
    Ok(table)
}

// ---- the folder on disk ----

/// The `.meta` of `stem` in the Project folder `root`, if there is one.
pub fn read_meta(root: &Path, stem: &str) -> Result<Option<Vec<u8>>> {
    read_capped(&join(root, &meta_path(stem)), MAX_META)
}

/// The cache Parquet of `stem` in `root`, opened for reading, if there is one.
pub fn open_parquet(root: &Path, stem: &str) -> Result<Option<fs::File>> {
    let path = join(root, &parquet_path(stem));
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io(&path, e)),
        Ok(m) if !m.is_file() => Err(io(&path, "not a regular file")),
        Ok(_) => fs::File::open(&path).map(Some).map_err(|e| io(&path, e)),
    }
}

/// Whether both cache files of `stem` exist in `root`.
pub fn exists_in(root: &Path, stem: &str) -> bool {
    exists(&join(root, &parquet_path(stem))) && exists(&join(root, &meta_path(stem)))
}

fn replace(root: &Path, rel: &str, bytes: &[u8]) -> Result {
    let live = join(root, rel);
    let tmp = join(root, &format!("{rel}.tmp"));
    let written = write_file(&tmp, bytes).and_then(|()| rename(&tmp, &live));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Writes `stem`'s cache in `root`: the `.meta` is removed first and written last, so an
/// interrupted write leaves no metadata (the table then opens empty and Stale, never with
/// mismatched data). `parquet` `None` rewrites the metadata only.
pub fn write(root: &Path, stem: &str, parquet: Option<&[u8]>, meta: &[u8]) -> Result {
    let meta_rel = meta_path(stem);
    if let Some(bytes) = parquet {
        let live = join(root, &meta_rel);
        if exists(&live) {
            fs::remove_file(&live).map_err(|e| io(&live, e))?;
        }
        replace(root, &parquet_path(stem), bytes)?;
    }
    replace(root, &meta_rel, meta)
}

/// Deletes the cache files (`.parquet`, `.meta`, and leftover `.tmp`) in `root` whose stem isn't
/// in `keep`: tables deleted or renamed since. Other files are left alone.
pub fn remove_others(root: &Path, keep: &BTreeSet<String>) -> Result {
    let dir = root.join(DIR);
    let entries = match fs::read_dir(&dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io(&dir, e)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| io(&dir, e))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let base = name.strip_suffix(".tmp").unwrap_or(name);
        let stem = base
            .strip_suffix(".parquet")
            .or_else(|| base.strip_suffix(".meta"));
        if let Some(stem) = stem
            && (base != name || !keep.contains(stem))
            && entry.file_type().is_ok_and(|t| t.is_file())
        {
            let path = entry.path();
            fs::remove_file(&path).map_err(|e| io(&path, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
