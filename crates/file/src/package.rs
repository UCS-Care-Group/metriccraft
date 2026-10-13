//! The `.odash` Package (ADR 0003): a zip of a Project's files, MIME type
//! [`MIME`].
//!
//! **Writing** is deterministic: entries in path order, every timestamp 1980-01-01 00:00, Unix
//! permissions 0644, no extra fields or comments; Deflate at level [`LEVEL`] for text, stored for
//! Parquet. `.local/` is never packed (it is per-user). The same files always give the same bytes,
//! so Project → Package → Project is byte-identical.
//!
//! **Reading** happens in memory (the web build has no file system, and a Package's definitions
//! are small): every entry is checked before it is read: at most [`Limits::entries`] entries; no
//! absolute paths, `..`, backslashes or duplicates; only stored or Deflate entries; no symlinks
//! or encryption; and at most [`Limits::bytes`] bytes uncompressed in all, counted as they are
//! inflated (a zip bomb's declared sizes are not trusted). Data cache Parquet (`.cache/*.parquet`) is
//! counted against [`Limits::cache`] instead (the app passes its memory cap, since that data is
//! loaded anyway); one that doesn't fit is left out, so its table opens without data.

use std::io::{Cursor, Read, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, System, ZipArchive, ZipWriter};

use crate::{Error, FileSet, Result, check_path};

/// The Package's MIME type, registered on install.
pub const MIME: &str = "application/vnd.opendash.package+zip";
/// The Package's file extension.
pub const EXTENSION: &str = "odash";
/// The Deflate level for text entries; fixed, so output is reproducible.
pub const LEVEL: i64 = 6;

/// Caps on what reading a Package may take.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub entries: usize,
    /// Total uncompressed bytes of everything but the Data cache Parquet.
    pub bytes: u64,
    /// Total uncompressed bytes of the Data cache Parquet (`.cache/*.parquet`).
    pub cache: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 100_000,
            bytes: 512 << 20,
            // The app passes its memory cap instead.
            cache: 1 << 30,
        }
    }
}

fn zip_err(e: impl std::fmt::Display) -> Error {
    Error::Package(e.to_string())
}

/// The Package bytes for `files` (see the module docs). Files under `.local/` are left out.
pub fn pack(files: &FileSet) -> Result<Vec<u8>> {
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    for (path, bytes) in files {
        if path == ".local" || path.starts_with(".local/") {
            continue;
        }
        check_path(path).map_err(Error::Package)?;
        let method = if path.ends_with(".parquet") {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let options = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level((method == CompressionMethod::Deflated).then_some(LEVEL))
            .last_modified_time(DateTime::DEFAULT)
            .system(System::Unix)
            .unix_permissions(0o644)
            .large_file(bytes.len() >= u32::MAX as usize);
        zip.start_file(path.as_str(), options).map_err(zip_err)?;
        zip.write_all(bytes).map_err(zip_err)?;
    }
    Ok(zip.finish().map_err(zip_err)?.into_inner())
}

/// [`unpack_with`] the default [`Limits`].
pub fn unpack(bytes: &[u8]) -> Result<FileSet> {
    unpack_with(bytes, Limits::default())
}

/// A Package's files, checked against `limits` (see the module docs). Directory entries are
/// skipped.
pub fn unpack_with(bytes: &[u8], limits: Limits) -> Result<FileSet> {
    let mut zip = ZipArchive::new(Cursor::new(bytes)).map_err(zip_err)?;
    if zip.len() > limits.entries {
        return Err(Error::Package(format!(
            "more than {} entries",
            limits.entries
        )));
    }
    let mut files = FileSet::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut defs_left = limits.bytes;
    let mut cache_left = limits.cache;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(zip_err)?;
        let name = std::str::from_utf8(entry.name_raw())
            .map_err(|_| Error::Package("an entry name is not UTF-8".into()))?
            .to_string();
        if entry.is_dir() {
            continue;
        }
        check_path(&name).map_err(Error::Package)?;
        if entry.is_symlink() {
            return Err(Error::Package(format!("`{name}` is a symbolic link")));
        }
        if !matches!(
            entry.compression(),
            CompressionMethod::Stored | CompressionMethod::Deflated
        ) {
            return Err(Error::Package(format!(
                "`{name}` uses a compression other than stored or Deflate"
            )));
        }
        // Case-insensitively: unpacked on Windows or macOS, the two would be one file.
        if !seen.insert(name.to_lowercase()) {
            return Err(Error::Package(format!("`{name}` appears twice")));
        }
        let cache = name.starts_with(".cache/") && name.ends_with(".parquet");
        let left = if cache {
            &mut cache_left
        } else {
            &mut defs_left
        };
        // Read at most one byte past what's left, so an oversized entry is caught whatever its
        // header says.
        let mut data = Vec::new();
        let read = (&mut entry)
            .take(left.saturating_add(1))
            .read_to_end(&mut data)
            .map_err(zip_err)?;
        let read = u64::try_from(read).unwrap_or(u64::MAX);
        if read > *left {
            if cache {
                // Data over the cap is left out: that table opens without it.
                continue;
            }
            return Err(Error::Package(format!(
                "more than {} MB uncompressed",
                limits.bytes >> 20
            )));
        }
        *left = left.saturating_sub(read);
        files.insert(name, data);
    }
    Ok(files)
}

#[cfg(test)]
mod tests;
