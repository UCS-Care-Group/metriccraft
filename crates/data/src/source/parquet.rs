//! Parquet Source: streamed one row group at a time, [`super::BATCH_ROWS`] rows per batch.
//! Codecs: uncompressed, snappy, gzip, lz4, brotli, zstd (all builds, web included).
//!
//! Column types are normalised to the plain Source set:
//! - integers of any width/sign → `Int64`; a `UInt64` value above `i64::MAX` is an error naming
//!   the column (no silent float rounding);
//! - floats → `Float64`; decimals → `Float64` (≈15–17 significant digits, so very wide decimals
//!   lose precision — the DAX Decimal type is handled later in the model);
//! - `Utf8`/`LargeUtf8`/`Utf8View` and string dictionaries → `Utf8`;
//! - `Date32`/`Date64` → `Date32` (Date64 time-of-day dropped);
//! - timestamps of any unit → `Timestamp(Microsecond, None)`; zoned timestamps keep their UTC
//!   instant (shown as UTC wall-clock time), nanoseconds are truncated;
//! - `Boolean` as is. Anything else (nested, binary, time-of-day…) is an error naming the column.
//!
//! The embedded Arrow schema (`ARROW:schema`, read by arrow-ipc's verifying flatbuffer
//! decoder) is used as a type hint, so e.g. Date64 columns keep their type.
//!
//! **Hostile files**. The parquet/arrow 60 decoders panic on some corrupt
//! input, and on wasm a panic aborts the app. So nothing reaches them unchecked:
//! - the footer is walked first ([`thrift`]): every list, binary and nesting depth must fit the
//!   bytes present (no allocation bombs), and schema child counts must fit the schema;
//! - every page header of a column chunk is walked before any of its pages is read: sizes
//!   inside the column chunk, decompressed size at most [`PAGE_CAP`] and the memory limit,
//!   DataPage v2 level lengths inside the page. Lazily, as the decoder reaches each chunk, so a
//!   Preview of the first rows walks only the row groups it reads, not the whole file;
//! - each page is checked as the decoder pulls it ([`Checked`]): a dictionary-encoded page needs
//!   a dictionary page first; a dictionary page claiming 0 values must be empty; definition
//!   levels (RLE/bit-packed) must fit their bytes; a v2 page's levels may not promise more
//!   values than its header. Nested columns (max definition level > 1) are refused up front.
//!
//! Memory: each batch's decoded size is estimated from the pages it pulls (dictionary pages
//! count their widest value per row) and held to the limit, so a small file can't expand into a
//! huge batch. Native builds keep a `catch_unwind` as a last-resort net; the fuzz tests run
//! without it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ::parquet::arrow::arrow_reader::{ParquetRecordBatchReader, RowGroups};
use ::parquet::arrow::{ProjectionMask, parquet_to_arrow_field_levels, parquet_to_arrow_schema};
use ::parquet::basic::{Encoding, Type as Physical};
use ::parquet::column::page::{Page, PageIterator, PageMetadata, PageReader};
use ::parquet::errors::ParquetError;
use ::parquet::file::metadata::{
    ColumnChunkMetaData, ParquetMetaData, ParquetMetaDataReader, RowGroupMetaData,
};
pub use ::parquet::file::reader::ChunkReader;
use ::parquet::file::serialized_reader::SerializedPageReader;
use ::parquet::schema::types::ColumnDescPtr;
use arrow_array::{Array, ArrayRef, RecordBatch, TimestampMicrosecondArray};
use arrow_cast::{CastOptions, cast_with_options};
use arrow_schema::{DataType, Field, Schema, TimeUnit};
pub use bytes::Bytes;

use super::{Sink, batch_rows, collect, emit};
use crate::{Error, Result, Table};

mod thrift;
use thrift::{Thrift, Value};

/// Largest decompressed page accepted (writers default to about 1 MB).
pub const PAGE_CAP: u64 = 256 << 20;
/// Most decoded bytes one batch may pull (arrow's 32-bit string offsets end at 2 GB).
const BATCH_CAP: u64 = 1 << 30;

/// Reads a Parquet file (a `std::fs::File`, or [`Bytes`] already in memory) into a Table named
/// `name`, using at most `limit` bytes.
pub fn read<R: ChunkReader + 'static>(name: &str, reader: R, limit: usize) -> Result<Table> {
    collect(name, limit, |sink| {
        read_rows(name, reader, usize::MAX, limit, sink)
    })
    .map(|(t, ())| t)
}

/// Streams at most `rows` rows into `sink`; decoding stops there.
pub fn read_rows<R: ChunkReader + 'static>(
    name: &str,
    reader: R,
    rows: usize,
    limit: usize,
    sink: &mut Sink<'_>,
) -> Result {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // Last-resort net only: the checks are meant to stop every corrupt input first (wasm
        // has no unwinding, and the fuzz tests call `read_checked` directly).
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            read_checked(name, reader, rows, limit, sink)
        }))
        .unwrap_or_else(|_| Err(corrupt(name, "the Parquet decoder failed")))
    }
    #[cfg(target_arch = "wasm32")]
    read_checked(name, reader, rows, limit, sink)
}

fn corrupt(name: &str, message: &str) -> Error {
    Error::Source {
        source: name.into(),
        message: format!("corrupt file: {message}"),
    }
}

fn read_checked<R: ChunkReader + 'static>(
    name: &str,
    reader: R,
    rows_wanted: usize,
    limit: usize,
    sink: &mut Sink<'_>,
) -> Result {
    let err = |message: String| Error::Source {
        source: name.into(),
        message,
    };
    check_footer(&reader).map_err(|m| corrupt(name, &m))?;
    let meta = ParquetMetaDataReader::new()
        .parse_and_finish(&reader)
        .map_err(|e| err(e.to_string()))?;
    let descr = meta.file_metadata().schema_descr_ptr();
    let hint = meta.file_metadata().key_value_metadata();
    let arrow = parquet_to_arrow_schema(&descr, hint).map_err(|e| err(e.to_string()))?;
    let fields = arrow
        .fields()
        .iter()
        .map(|f| {
            plain(f.data_type())
                .map(|t| Field::new(f.name(), t, true))
                .ok_or_else(|| {
                    err(format!(
                        "column `{}` has unsupported type {}",
                        f.name(),
                        f.data_type()
                    ))
                })
        })
        .collect::<Result<Vec<_>>>()?;
    if let Some(c) = descr
        .columns()
        .iter()
        .find(|c| c.max_rep_level() > 0 || c.max_def_level() > 1)
    {
        return Err(err(format!("column `{}` is nested", c.path())));
    }

    let mut rows = 0usize;
    for group in meta.row_groups() {
        let n = usize::try_from(group.num_rows()).map_err(|_| corrupt(name, "row count"))?;
        rows = rows.saturating_add(n);
        if group.columns().len() != descr.num_columns() {
            return Err(corrupt(name, "row group column count"));
        }
    }

    let levels = parquet_to_arrow_field_levels(&descr, ProjectionMask::all(), Some(arrow.fields()))
        .map_err(|e| err(e.to_string()))?;
    let budget = Arc::new(Budget::new(limit));
    let scan = Arc::new(Scan {
        name: name.into(),
        limit,
        failed: Mutex::new(None),
    });
    let groups = Groups {
        reader: Arc::new(reader),
        meta: Arc::new(meta),
        budget: Arc::clone(&budget),
        scan: Arc::clone(&scan),
        rows,
    };
    // A failed page-header scan says why itself; anything else is the decoder's message.
    let failed = |e: ParquetError| scan.take().unwrap_or_else(|| err(e.to_string()));
    let batch = batch_rows(rows_wanted);
    let mut reader =
        ParquetRecordBatchReader::try_new_with_row_groups(&levels, &groups, batch, None)
            .map_err(failed)?;
    let schema = Arc::new(Schema::new(fields));
    let batches =
        std::iter::from_fn(|| {
            budget.reset();
            let batch = match reader.next()? {
                Ok(batch) => batch,
                Err(e) => {
                    if let Some(e) = scan.take() {
                        return Some(Err(e));
                    }
                    if budget.exceeded() {
                        return Some(Err(Error::MemoryCap {
                            table: name.into(),
                            limit,
                        }));
                    }
                    return Some(Err(err(e.to_string())));
                }
            };
            let columns = batch
                .columns()
                .iter()
                .zip(schema.fields())
                .map(|(col, f)| {
                    normalise(col, f.data_type())
                        .map_err(|e| err(format!("column `{}`: {e}", f.name())))
                })
                .collect::<Result<Vec<_>>>();
            Some(columns.and_then(|c| {
                RecordBatch::try_new(schema.clone(), c).map_err(|e| err(e.to_string()))
            }))
        });
    emit(schema.clone(), batches, rows_wanted, sink)
}

/// Walks the footer's Thrift before the parquet crate parses it.
fn check_footer<R: ChunkReader>(reader: &R) -> std::result::Result<(), String> {
    let len = reader.len();
    if len < 12 {
        return Err("too short for Parquet".into());
    }
    let tail = reader.get_bytes(len - 8, 8).map_err(|e| e.to_string())?;
    let (Some(size), Some(b"PAR1")) = (tail.get(..4), tail.get(4..)) else {
        return Err("not a Parquet file (or an encrypted one)".into());
    };
    let size = u32::from_le_bytes(size.try_into().map_err(|_| "footer")?);
    let size = u64::from(size);
    if size > len - 12 {
        return Err("footer larger than the file".into());
    }
    let footer = reader
        .get_bytes(len - 8 - size, usize::try_from(size).map_err(|_| "footer")?)
        .map_err(|e| e.to_string())?;
    let mut schema_len = 0;
    Thrift::new(footer.as_ref(), size).walk(&mut |path, v| {
        match (path, v) {
            ([2], Value::List(n)) => schema_len = n,
            // A schema element's child count drives an allocation in the parquet crate.
            ([2, 5], Value::Int(n)) if u64::try_from(n).map_or(true, |n| n > schema_len) => {
                return Err("schema child count".into());
            }
            _ => {}
        }
        Ok(())
    })
}

/// Walks each page header of a column chunk (`start..end`) without reading the pages.
fn scan_pages<R: ChunkReader>(
    name: &str,
    reader: &R,
    start: u64,
    end: u64,
    limit: usize,
) -> Result<()> {
    let mut pos = start;
    while pos < end {
        let left = end - pos;
        let input = reader
            .get_read(pos)
            .map_err(|e| corrupt(name, &e.to_string()))?;
        let mut header = [0i64; 4]; // type, uncompressed, compressed, v2 level bytes
        let mut t = Thrift::new(input, left);
        t.walk(&mut |path, v| {
            if let Value::Int(v) = v {
                match path {
                    [1] => header[0] = v,
                    [2] => header[1] = v,
                    [3] => header[2] = v,
                    [8, 5] | [8, 6] if v < 0 => return Err("negative level length".into()),
                    [8, 5] | [8, 6] => header[3] = header[3].saturating_add(v),
                    _ => {}
                }
            }
            Ok(())
        })
        .map_err(|m| corrupt(name, &m))?;
        let [typ, uncompressed, compressed, levels] = header;
        let compressed = u64::try_from(compressed).unwrap_or(u64::MAX);
        if compressed > left - t.read {
            return Err(corrupt(name, "page past the end of its column"));
        }
        let size = u64::try_from(uncompressed).map_err(|_| corrupt(name, "page size"))?;
        if size > limit as u64 {
            return Err(Error::MemoryCap {
                table: name.into(),
                limit,
            });
        }
        if size > PAGE_CAP {
            return Err(Error::Source {
                source: name.into(),
                message: format!("a page of {} MB is too large", size >> 20),
            });
        }
        // DataPage v2: the levels sit uncompressed at the front of the page.
        if typ == 3 && u64::try_from(levels).map_or(true, |l| l > compressed.min(size)) {
            return Err(corrupt(name, "page levels past the page"));
        }
        pos += t.read + compressed;
    }
    Ok(())
}

/// The decoded bytes allowed per batch, shared by the page readers of every column.
struct Budget {
    cap: u64,
    used: AtomicU64,
    exceeded: AtomicBool,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            cap: (limit as u64).min(BATCH_CAP),
            used: AtomicU64::new(0),
            exceeded: AtomicBool::new(false),
        }
    }

    fn reset(&self) {
        self.used.store(0, Ordering::Relaxed);
    }

    fn exceeded(&self) -> bool {
        self.exceeded.load(Ordering::Relaxed)
    }

    fn spend(&self, n: u64) -> std::result::Result<(), String> {
        let used = self.used.load(Ordering::Relaxed).saturating_add(n);
        self.used.store(used, Ordering::Relaxed);
        if used > self.cap {
            self.exceeded.store(true, Ordering::Relaxed);
            return Err("memory limit".into());
        }
        Ok(())
    }
}

/// What a column chunk's page-header walk needs, and the error of the first walk that failed
/// (the decoder only passes on a [`ParquetError`]).
struct Scan {
    name: String,
    limit: usize,
    failed: Mutex<Option<Error>>,
}

impl Scan {
    /// Walks the page headers of `chunk` (see [`scan_pages`]).
    fn chunk<R: ChunkReader>(
        &self,
        reader: &R,
        chunk: &ColumnChunkMetaData,
    ) -> ::parquet::errors::Result<()> {
        let start = chunk
            .dictionary_page_offset()
            .unwrap_or(chunk.data_page_offset());
        let range = u64::try_from(start)
            .ok()
            .zip(u64::try_from(chunk.compressed_size()).ok())
            .and_then(|(s, n)| Some((s, s.checked_add(n)?)))
            .filter(|&(_, end)| end <= reader.len());
        let walked = match range {
            Some((start, end)) => scan_pages(&self.name, reader, start, end, self.limit),
            None => Err(corrupt(&self.name, "column chunk outside the file")),
        };
        walked.map_err(|e| {
            let message = e.to_string();
            if let Ok(mut failed) = self.failed.lock() {
                failed.get_or_insert(e);
            }
            ParquetError::General(message)
        })
    }

    fn take(&self) -> Option<Error> {
        self.failed.lock().ok()?.take()
    }
}

/// The file's row groups, read through [`Checked`] page readers.
struct Groups<R> {
    reader: Arc<R>,
    meta: Arc<ParquetMetaData>,
    budget: Arc<Budget>,
    scan: Arc<Scan>,
    rows: usize,
}

impl<R: ChunkReader + 'static> RowGroups for Groups<R> {
    fn num_rows(&self) -> usize {
        self.rows
    }

    fn column_chunks(&self, column: usize) -> ::parquet::errors::Result<Box<dyn PageIterator>> {
        let descr = self
            .meta
            .file_metadata()
            .schema_descr()
            .columns()
            .get(column)
            .cloned()
            .ok_or_else(|| ParquetError::General("no such column".into()))?;
        Ok(Box::new(Chunks {
            reader: Arc::clone(&self.reader),
            meta: Arc::clone(&self.meta),
            budget: Arc::clone(&self.budget),
            scan: Arc::clone(&self.scan),
            descr,
            column,
            group: 0,
        }))
    }

    fn row_groups(&self) -> Box<dyn Iterator<Item = &RowGroupMetaData> + '_> {
        Box::new(self.meta.row_groups().iter())
    }

    fn metadata(&self) -> &ParquetMetaData {
        &self.meta
    }
}

/// One column's chunks, row group by row group.
struct Chunks<R> {
    reader: Arc<R>,
    meta: Arc<ParquetMetaData>,
    budget: Arc<Budget>,
    scan: Arc<Scan>,
    descr: ColumnDescPtr,
    column: usize,
    group: usize,
}

impl<R: ChunkReader + 'static> Iterator for Chunks<R> {
    type Item = ::parquet::errors::Result<Box<dyn PageReader>>;

    fn next(&mut self) -> Option<Self::Item> {
        let group = self.meta.row_groups().get(self.group)?;
        self.group += 1;
        let page_reader = || {
            let chunk = group
                .columns()
                .get(self.column)
                .ok_or_else(|| ParquetError::General("no such column".into()))?;
            let rows = usize::try_from(group.num_rows())
                .map_err(|_| ParquetError::General("row count".into()))?;
            // The chunk's page headers, before the decoder reads any of its pages.
            self.scan.chunk(self.reader.as_ref(), chunk)?;
            let inner = SerializedPageReader::new(Arc::clone(&self.reader), chunk, rows, None)?;
            Ok(Box::new(Checked {
                inner,
                nullable: self.descr.max_def_level() > 0,
                physical: self.descr.physical_type(),
                type_length: self.descr.type_length(),
                dictionary: None,
                budget: Arc::clone(&self.budget),
            }) as Box<dyn PageReader>)
        };
        Some(page_reader())
    }
}

impl<R: ChunkReader + 'static> PageIterator for Chunks<R> {}

/// Checks each page before the decoders see it (see the module docs).
struct Checked<P> {
    inner: P,
    nullable: bool,
    physical: Physical,
    type_length: i32,
    /// The dictionary page's value count and widest value in bytes, once read.
    dictionary: Option<(u32, u64)>,
    budget: Arc<Budget>,
}

type Check<T = ()> = std::result::Result<T, String>;

impl<P> Checked<P> {
    fn check(&mut self, page: &Page) -> Check {
        match page {
            Page::DictionaryPage {
                buf, num_values, ..
            } => {
                // The byte-array dictionary decoder divides by the count.
                if *num_values == 0 && !buf.is_empty() {
                    return Err("dictionary page with an empty count".into());
                }
                let widest = match self.physical {
                    Physical::BYTE_ARRAY => widest_value(buf, *num_values),
                    Physical::FIXED_LEN_BYTE_ARRAY => u64::try_from(self.type_length).unwrap_or(0),
                    _ => 12,
                };
                self.dictionary = Some((*num_values, widest));
                self.budget.spend(page_cost(buf.len(), *num_values))
            }
            Page::DataPage {
                buf,
                num_values,
                encoding,
                def_level_encoding,
                ..
            } => {
                self.budget.spend(page_cost(buf.len(), *num_values))?;
                let (values, present) = if self.nullable {
                    #[allow(deprecated)]
                    match def_level_encoding {
                        Encoding::RLE => {
                            let size = buf
                                .get(..4)
                                .and_then(|b| b.try_into().ok())
                                .map(u32::from_le_bytes)
                                .ok_or("levels past the page")?;
                            let end = usize::try_from(size)
                                .ok()
                                .and_then(|n| n.checked_add(4))
                                .filter(|&end| end <= buf.len())
                                .ok_or("levels past the page")?;
                            let levels = buf.get(4..end).unwrap_or_default();
                            let present = set_levels(levels, *num_values)?;
                            (buf.get(end..).unwrap_or_default(), Some(present))
                        }
                        Encoding::BIT_PACKED => {
                            let end = usize::try_from(u64::from(*num_values).div_ceil(8))
                                .ok()
                                .filter(|&end| end <= buf.len())
                                .ok_or("levels past the page")?;
                            (buf.get(end..).unwrap_or_default(), None)
                        }
                        _ => return Err("unknown level encoding".into()),
                    }
                } else {
                    (&buf[..], Some(u64::from(*num_values)))
                };
                let decoded = self.check_values(*encoding, values, present, *num_values)?;
                self.budget.spend(decoded.saturating_sub(buf.len() as u64))
            }
            Page::DataPageV2 {
                buf,
                num_values,
                encoding,
                num_nulls,
                def_levels_byte_len,
                rep_levels_byte_len,
                ..
            } => {
                self.budget.spend(page_cost(buf.len(), *num_values))?;
                let promised = num_values
                    .checked_sub(*num_nulls)
                    .ok_or("more nulls than values")?;
                let start = *rep_levels_byte_len as usize;
                let end = start
                    .checked_add(*def_levels_byte_len as usize)
                    .filter(|&end| end <= buf.len())
                    .ok_or("levels past the page")?;
                let present = if self.nullable {
                    set_levels(buf.get(start..end).unwrap_or_default(), *num_values)?
                } else {
                    u64::from(*num_values)
                };
                // The plain byte-array decoder divides by the values left once they run out.
                if present > u64::from(promised) {
                    return Err("levels promise more values than the page holds".into());
                }
                let values = buf.get(end..).unwrap_or_default();
                let decoded = self.check_values(*encoding, values, Some(present), *num_values)?;
                self.budget.spend(decoded.saturating_sub(buf.len() as u64))
            }
        }
    }

    /// Encoding-specific checks of a data page's value bytes (`present` values expected, at
    /// most `rows`), covering what the parquet 60 decoders index or allocate unchecked.
    /// Returns an estimate of the decoded size in bytes.
    fn check_values(
        &self,
        encoding: Encoding,
        values: &[u8],
        present: Option<u64>,
        rows: u32,
    ) -> Check<u64> {
        let rows = u64::from(rows);
        let bytes = values.len() as u64;
        match encoding {
            Encoding::PLAIN => Ok(bytes),
            Encoding::PLAIN_DICTIONARY | Encoding::RLE_DICTIONARY => {
                let (count, widest) = self
                    .dictionary
                    .ok_or("dictionary-encoded page without a dictionary")?;
                let (&width, runs) = values.split_first().ok_or("empty dictionary page")?;
                // Fixed-length dictionaries are indexed unchecked.
                let limit =
                    (self.physical == Physical::FIXED_LEN_BYTE_ARRAY).then_some(u64::from(count));
                hybrid(runs, width, present.unwrap_or(rows), limit)?;
                Ok(widest.saturating_mul(rows))
            }
            // Booleans: a length prefix, then a width-1 hybrid stream.
            Encoding::RLE => {
                let size = values
                    .get(..4)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_le_bytes)
                    .ok_or("RLE values past the page")?;
                let runs = values
                    .get(4..)
                    .and_then(|v| v.get(..usize::try_from(size).ok()?))
                    .ok_or("RLE values past the page")?;
                hybrid(runs, 1, rows, None)?;
                Ok(bytes)
            }
            // The decoder reads stream `j` at `i + j * len / width`, unchecked.
            Encoding::BYTE_STREAM_SPLIT => {
                let width = match self.physical {
                    Physical::INT32 | Physical::FLOAT => 4,
                    Physical::INT64 | Physical::DOUBLE => 8,
                    _ => u64::try_from(self.type_length).unwrap_or(0).max(1),
                };
                match present {
                    Some(n) if n <= bytes / width => Ok(bytes),
                    _ => Err("byte-stream-split values past the page".into()),
                }
            }
            Encoding::DELTA_BINARY_PACKED => {
                let bits = if self.physical == Physical::INT32 {
                    32
                } else {
                    64
                };
                let (count, _) = delta_binary(values, bits, rows, &mut |_| Ok(()))?;
                // With no values the decoder still hands out its header's first value, then
                // underflows its count.
                if count == 0 && present.unwrap_or(rows) > 0 {
                    return Err("delta page without values".into());
                }
                Ok(bytes)
            }
            Encoding::DELTA_LENGTH_BYTE_ARRAY => {
                let mut total = 0u64;
                let (_, start) = delta_binary(values, 32, rows, &mut |len| {
                    let len = u64::try_from(len).map_err(|_| "negative length")?;
                    total = total.checked_add(len).ok_or("lengths overflow")?;
                    Ok(())
                })?;
                if start.checked_add(total).is_none_or(|end| end > bytes) {
                    return Err("delta-length values past the page".into());
                }
                Ok(bytes)
            }
            Encoding::DELTA_BYTE_ARRAY => {
                // The prefixes are held while the suffixes are walked: count them first and pay
                // for them from the budget (up to limit/2 bytes otherwise unaccounted).
                let (count, _) = delta_binary(values, 32, rows, &mut |_| Ok(()))?;
                self.budget.spend(count.saturating_mul(8))?;
                let mut prefixes = Vec::new();
                prefixes
                    .try_reserve_exact(usize::try_from(count).map_err(|_| "prefix count")?)
                    .map_err(|_| "memory limit")?;
                let (_, split) = delta_binary(values, 32, rows, &mut |p| {
                    prefixes.push(u64::try_from(p).map_err(|_| "negative prefix")?);
                    Ok(())
                })?;
                let suffixes = usize::try_from(split)
                    .ok()
                    .and_then(|s| values.get(s..))
                    .ok_or("delta prefixes past the page")?;
                let (mut i, mut last, mut decoded, mut total) = (0, 0u64, 0u64, 0u64);
                let (count, data) = delta_binary(suffixes, 32, rows, &mut |s| {
                    let s = u64::try_from(s).map_err(|_| "negative suffix")?;
                    let prefix = prefixes.get(i).copied().unwrap_or(0);
                    i += 1;
                    // Each value keeps up to `prefix` bytes of the one before.
                    last = prefix.min(last).saturating_add(s);
                    decoded = decoded.saturating_add(last);
                    total = total.checked_add(s).ok_or("lengths overflow")?;
                    Ok(())
                })?;
                if count != prefixes.len() as u64 {
                    return Err("delta prefix and suffix counts differ".into());
                }
                if split
                    .checked_add(data)
                    .and_then(|start| start.checked_add(total))
                    .is_none_or(|end| end > bytes)
                {
                    return Err("delta values past the page".into());
                }
                Ok(decoded)
            }
            _ => Err(format!("{encoding} encoding is not supported")),
        }
    }
}

impl<P: PageReader> Iterator for Checked<P> {
    type Item = ::parquet::errors::Result<Page>;

    fn next(&mut self) -> Option<Self::Item> {
        self.get_next_page().transpose()
    }
}

impl<P: PageReader> PageReader for Checked<P> {
    fn get_next_page(&mut self) -> ::parquet::errors::Result<Option<Page>> {
        let page = self.inner.get_next_page()?;
        if let Some(page) = &page {
            self.check(page)
                .map_err(|m| ParquetError::General(format!("corrupt page: {m}")))?;
        }
        Ok(page)
    }

    fn peek_next_page(&mut self) -> ::parquet::errors::Result<Option<PageMetadata>> {
        self.inner.peek_next_page()
    }

    fn skip_next_page(&mut self) -> ::parquet::errors::Result<()> {
        self.inner.skip_next_page()
    }

    fn at_record_boundary(&mut self) -> ::parquet::errors::Result<bool> {
        self.inner.at_record_boundary()
    }
}

/// A page's decoded size before its values: offsets and validity, 16 bytes a row, plus bytes.
fn page_cost(bytes: usize, rows: u32) -> u64 {
    (bytes as u64).saturating_add(u64::from(rows) * 16)
}

/// The longest of `count` plain-encoded byte-array values (length-prefixed) in `buf`.
fn widest_value(buf: &[u8], count: u32) -> u64 {
    let (mut pos, mut widest) = (0usize, 0u64);
    for _ in 0..count {
        let Some(len) = pos
            .checked_add(4)
            .and_then(|end| buf.get(pos..end))
            .and_then(|b| b.try_into().ok())
            .map(u32::from_le_bytes)
        else {
            break;
        };
        widest = widest.max(u64::from(len));
        pos = pos.saturating_add(4).saturating_add(len as usize);
    }
    widest
}

/// Validates `count` definition levels of width 1 (RLE/bit-packed hybrid) the way the
/// parquet decoder reads them, returning how many are set (non-null). A bit-packed run must
/// have all its bytes: the decoder indexes them unchecked.
fn set_levels(data: &[u8], count: u32) -> Check<u64> {
    let (mut left, mut pos, mut set) = (u64::from(count), 0usize, 0u64);
    while left > 0 && pos < data.len() {
        let mut header = 0u64;
        let mut shift = 0;
        loop {
            let b = *data.get(pos).ok_or("levels end mid-run")?;
            pos += 1;
            header |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 63 {
                return Err("malformed level run".into());
            }
        }
        let run = header >> 1;
        if header & 1 == 1 {
            // Bit-packed: `run` groups of 8 levels, one byte each.
            let bytes = usize::try_from(run)
                .ok()
                .and_then(|n| data.get(pos..pos.checked_add(n)?))
                .ok_or("bit-packed levels past the page")?;
            let take = left.min(run * 8);
            let whole = (take / 8) as usize;
            set += bytes
                .iter()
                .take(whole)
                .map(|b| u64::from(b.count_ones()))
                .sum::<u64>();
            if let Some(b) = bytes.get(whole) {
                set += u64::from((b & ((1u16 << (take % 8)) - 1) as u8).count_ones());
            }
            left -= take;
            pos += bytes.len();
        } else {
            let value = *data.get(pos).ok_or("levels end mid-run")?;
            pos += 1;
            let take = left.min(run);
            if value != 0 {
                set += take;
            }
            left -= take;
        }
    }
    Ok(set)
}

/// A little-endian bit cursor over a page, as parquet's `BitReader` moves.
struct Bits<'a> {
    data: &'a [u8],
    bit: u64,
}

impl Bits<'_> {
    /// The byte offset, counting a partly read byte.
    fn offset(&self) -> u64 {
        self.bit.div_ceil(8)
    }

    /// A byte-aligned ULEB128 integer of at most 10 bytes (parquet panics on an 11th).
    fn vlq(&mut self) -> Check<i64> {
        let start = usize::try_from(self.offset()).map_err(|_| "offset")?;
        let mut v = 0i64;
        for i in 0..10 {
            let b = *self.data.get(start + i).ok_or("integer past the page")?;
            v |= i64::from(b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                self.bit = (start + i + 1) as u64 * 8;
                return Ok(v);
            }
        }
        Err("over-long integer".into())
    }

    /// The next `n` (at most 64) bits as an unsigned value.
    fn bits(&mut self, n: u32) -> Check<u64> {
        let end = self.bit + u64::from(n);
        if end > self.data.len() as u64 * 8 {
            return Err("values past the page".into());
        }
        if n == 0 {
            return Ok(0);
        }
        let first = (self.bit / 8) as usize;
        let last = ((end - 1) / 8) as usize;
        let mut v = 0u128;
        for (k, b) in self
            .data
            .get(first..=last)
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            v |= u128::from(*b) << (8 * k);
        }
        v >>= self.bit % 8;
        self.bit = end;
        Ok((v & ((1u128 << n) - 1)) as u64)
    }
}

fn zigzag(v: i64) -> i64 {
    let u = v as u64;
    (u >> 1) as i64 ^ -((u & 1) as i64)
}

/// Walks a DELTA_BINARY_PACKED stream of `bits`-bit integers the way parquet's decoder reads
/// it, calling `f` with each value; returns the value count and the end offset the decoder
/// reports. At most `max` values are accepted: the byte-array decoders allocate the count.
fn delta_binary(
    data: &[u8],
    bits: u32,
    max: u64,
    f: &mut dyn FnMut(i64) -> Check,
) -> Check<(u64, u64)> {
    // Writers use 128-value blocks; anything far larger is hostile.
    const MAX_BLOCK: u64 = 1 << 20;
    let mut r = Bits { data, bit: 0 };
    let header = |v: i64| u64::try_from(v).map_err(|_| "negative delta header");
    let block = header(r.vlq()?)?;
    let minis = header(r.vlq()?)?;
    let count = header(r.vlq()?)?;
    let first = zigzag(r.vlq()?);
    let fits = |v: i64| bits == 64 || i32::try_from(v).is_ok();
    if minis == 0
        || block == 0
        || block > MAX_BLOCK
        || block % 128 != 0
        || block % minis != 0
        || (block / minis) % 32 != 0
    {
        return Err("bad delta block layout".into());
    }
    if !fits(first) {
        return Err("delta value too large".into());
    }
    if count > max {
        return Err("more delta values than the page holds".into());
    }
    if count == 0 {
        return Ok((0, r.offset()));
    }
    let per_mini = block / minis;
    let wrap = |v: i64| if bits == 64 { v } else { i64::from(v as i32) };
    f(first)?;
    let (mut last, mut left, mut block_end) = (first, count - 1, 0);
    while left > 0 {
        let min_delta = zigzag(r.vlq()?);
        if !fits(min_delta) {
            return Err("delta value too large".into());
        }
        let start = r.offset();
        let widths = usize::try_from(start)
            .ok()
            .zip(usize::try_from(minis).ok())
            .and_then(|(s, n)| data.get(s..s.checked_add(n)?))
            .ok_or("delta bit widths past the page")?;
        r.bit = (start + minis) * 8;
        // Where the decoder thinks the block ends (unused trailing widths count as 0).
        let (mut end, mut rest) = (r.offset(), left);
        for &w in widths {
            if rest > 0 {
                end += u64::from(w) * per_mini / 8;
            }
            rest = rest.saturating_sub(per_mini);
        }
        block_end = end;
        for &w in widths {
            if left == 0 {
                break;
            }
            if u32::from(w) > bits {
                return Err("delta bit width too large".into());
            }
            for _ in 0..per_mini.min(left) {
                let delta = wrap(r.bits(u32::from(w))? as i64);
                last = wrap(delta.wrapping_add(min_delta).wrapping_add(last));
                f(last)?;
            }
            left -= per_mini.min(left);
        }
    }
    Ok((count, r.offset().max(block_end)))
}

/// Walks an RLE/bit-packed hybrid stream of `width`-bit values as parquet's `RleDecoder`
/// reads it: run headers of at most 10 bytes, small enough not to overflow. With `limit`, the
/// first `expected` values must be below it (fixed-length dictionaries are indexed unchecked).
fn hybrid(data: &[u8], width: u8, expected: u64, limit: Option<u64>) -> Check {
    if width > 32 {
        return Err("bit width too large".into());
    }
    let mut r = Bits { data, bit: 0 };
    let mut seen = 0u64;
    while r.offset() < data.len() as u64 {
        let header = r.vlq()?;
        // A zero header ends one read, but the next read carries on after it.
        if header == 0 {
            continue;
        }
        if !(0..1 << 60).contains(&header) {
            return Err("run too long".into());
        }
        // The decoder keeps run lengths in a u32.
        let run = header >> 1;
        if header & 1 == 1 {
            let start = r.bit;
            let n = u64::from((run * 8) as u32);
            if let Some(limit) = limit {
                for _ in 0..n.min(expected.saturating_sub(seen)) {
                    let Ok(v) = r.bits(u32::from(width)) else {
                        break;
                    };
                    if v >= limit {
                        return Err("dictionary index out of range".into());
                    }
                    seen += 1;
                }
            }
            r.bit = start.saturating_add(n.saturating_mul(u64::from(width)));
        } else {
            let bytes = u64::from(width).div_ceil(8);
            if let Some(limit) = limit
                && seen < expected
            {
                let v = r.bits((bytes * 8) as u32)?;
                if v >= limit {
                    return Err("dictionary index out of range".into());
                }
            } else {
                r.bit += bytes * 8;
            }
            seen = seen.saturating_add(u64::from(run as u32));
        }
    }
    Ok(())
}

/// The plain type a Parquet column becomes, or `None` if unsupported.
fn plain(t: &DataType) -> Option<DataType> {
    use DataType::*;
    Some(match t {
        Int8 | Int16 | Int32 | Int64 | UInt8 | UInt16 | UInt32 | UInt64 => Int64,
        Float16 | Float32 | Float64 => Float64,
        Decimal32(..) | Decimal64(..) | Decimal128(..) | Decimal256(..) => Float64,
        Utf8 | LargeUtf8 | Utf8View => Utf8,
        Dictionary(_, v) if plain(v) == Some(Utf8) => Utf8,
        Date32 | Date64 => Date32,
        Timestamp(..) => Timestamp(TimeUnit::Microsecond, None),
        Boolean => Boolean,
        _ => return None,
    })
}

fn normalise(col: &ArrayRef, to: &DataType) -> std::result::Result<ArrayRef, String> {
    let strict = CastOptions {
        safe: false,
        ..Default::default()
    };
    let cast = |t: &DataType| cast_with_options(col, t, &strict).map_err(|e| e.to_string());
    match col.data_type() {
        // Change the unit keeping the zone, then drop the zone: the values are UTC instants.
        DataType::Timestamp(_, tz @ Some(_)) => {
            let micros = cast(&DataType::Timestamp(TimeUnit::Microsecond, tz.clone()))?;
            let micros = micros
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .ok_or("timestamp cast failed")?;
            Ok(Arc::new(micros.clone().with_timezone_opt(None::<String>)))
        }
        _ => cast(to),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::BATCH_ROWS;
    use ::parquet::arrow::ArrowWriter;
    use ::parquet::basic::{BrotliLevel, Compression, GzipLevel, ZstdLevel};
    use ::parquet::file::properties::WriterProperties;
    use arrow_array::{
        BooleanArray, Date32Array, Date64Array, Decimal128Array, Float32Array, Float64Array,
        Int8Array, Int64Array, LargeStringArray, ListArray, StringArray, TimestampNanosecondArray,
        TimestampSecondArray, UInt32Array, UInt64Array, types::Int32Type,
    };

    const BIG: usize = 1 << 30;
    fn codecs() -> [Compression; 7] {
        [
            Compression::UNCOMPRESSED,
            Compression::SNAPPY,
            Compression::GZIP(GzipLevel::default()),
            Compression::LZ4_RAW,
            Compression::LZ4,
            Compression::BROTLI(BrotliLevel::default()),
            Compression::ZSTD(ZstdLevel::default()),
        ]
    }

    fn write(batch: &RecordBatch, compression: Compression, row_group: usize) -> Bytes {
        let props = WriterProperties::builder()
            .set_compression(compression)
            .set_max_row_group_row_count(Some(row_group))
            .build();
        let mut out = Vec::new();
        let mut w = ArrowWriter::try_new(&mut out, batch.schema(), Some(props)).unwrap();
        w.write(batch).unwrap();
        w.close().unwrap();
        Bytes::from(out)
    }

    fn batch(cols: Vec<(&str, ArrayRef)>) -> RecordBatch {
        RecordBatch::try_from_iter(cols).unwrap()
    }

    fn sample() -> RecordBatch {
        batch(vec![
            (
                "region",
                Arc::new(StringArray::from(vec![Some("North"), None, Some("South")])) as ArrayRef,
            ),
            ("units", Arc::new(Int64Array::from(vec![1, 2, 3]))),
            ("price", Arc::new(Float64Array::from(vec![1.5, 2.5, 3.5]))),
        ])
    }

    #[test]
    fn every_codec_round_trips() {
        for c in codecs() {
            let t = read("Sales", write(&sample(), c, 1024), BIG).unwrap();
            assert_eq!(t.rows(), 3, "{c:?}");
            let b = t.batch();
            let units = b
                .column(1)
                .as_any()
                .downcast_ref::<arrow_array::Int8Array>();
            assert_eq!(units.unwrap().values(), &[1, 2, 3], "{c:?}");
            let price = b.column(2).as_any().downcast_ref::<Float64Array>().unwrap();
            assert_eq!(price.values(), &[1.5, 2.5, 3.5], "{c:?}");
            assert!(b.column(0).is_null(1), "{c:?}");
        }
    }

    #[test]
    fn many_row_groups_and_batches_stream() {
        let n = BATCH_ROWS * 3 + 17;
        let ids = Int64Array::from_iter_values(0..n as i64);
        let b = batch(vec![("id", Arc::new(ids) as ArrayRef)]);
        let t = read("Big", write(&b, Compression::SNAPPY, 5000), BIG).unwrap();
        assert_eq!(t.rows(), n);
        let ids = arrow_cast::cast(t.batch().column(0), &DataType::Int64).unwrap();
        let sum: i64 = ids
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap()
            .values()
            .iter()
            .sum();
        assert_eq!(sum, (n as i64 - 1) * n as i64 / 2);
    }

    #[test]
    fn types_normalise_with_nulls() {
        let b = batch(vec![
            (
                "i8",
                Arc::new(Int8Array::from(vec![Some(-1), None])) as ArrayRef,
            ),
            ("u32", Arc::new(UInt32Array::from(vec![Some(7), None]))),
            ("u64", Arc::new(UInt64Array::from(vec![Some(9), None]))),
            ("f32", Arc::new(Float32Array::from(vec![Some(0.5), None]))),
            (
                "dec",
                Arc::new(
                    Decimal128Array::from(vec![Some(12345), None])
                        .with_precision_and_scale(10, 2)
                        .unwrap(),
                ),
            ),
            (
                "large",
                Arc::new(LargeStringArray::from(vec![Some("a"), None])),
            ),
            (
                "dict",
                Arc::new(
                    vec![Some("x"), None]
                        .into_iter()
                        .collect::<arrow_array::DictionaryArray<Int32Type>>(),
                ),
            ),
            (
                "d64",
                Arc::new(Date64Array::from(vec![Some(86_400_000), None])),
            ),
            ("d32", Arc::new(Date32Array::from(vec![Some(3), None]))),
            ("flag", Arc::new(BooleanArray::from(vec![Some(true), None]))),
            (
                "ts_s",
                Arc::new(TimestampSecondArray::from(vec![Some(2), None])),
            ),
            (
                "ts_ns_tz",
                Arc::new(
                    TimestampNanosecondArray::from(vec![Some(3_000_001_000), None])
                        .with_timezone("+05:00"),
                ),
            ),
        ]);
        let t = read("Types", write(&b, Compression::SNAPPY, 1024), BIG).unwrap();
        let schema = t.batch().schema();
        let types: Vec<String> = schema
            .fields()
            .iter()
            .map(|f| format!("{} {}", f.name(), f.data_type()))
            .collect();
        let micros = DataType::Timestamp(TimeUnit::Microsecond, None).to_string();
        let float = DataType::Float64.to_string();
        let utf8 = "Dictionary(Int8, Utf8)";
        for (i, want) in [
            "Int8",
            "Int8",
            "Int8",
            float.as_str(),
            float.as_str(),
            utf8,
            utf8,
            "Date32",
            "Date32",
            "Boolean",
            micros.as_str(),
            micros.as_str(),
        ]
        .iter()
        .enumerate()
        {
            assert!(types[i].ends_with(want), "{} vs {want}", types[i]);
            assert!(t.batch().column(i).is_null(1), "{}", types[i]);
        }
        let col = |i: usize| t.batch().column(i).clone();
        let dec = col(4);
        let dec = dec.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(dec.value(0), 123.45);
        let d64 = col(7);
        assert_eq!(
            d64.as_any().downcast_ref::<Date32Array>().unwrap().value(0),
            1
        );
        let ts = col(10);
        let ts = ts
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 2_000_000);
        let ts = col(11);
        let ts = ts
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 3_000_001, "UTC instant kept, ns truncated");
    }

    #[test]
    fn uint64_overflow_names_the_column() {
        let b = batch(vec![(
            "huge",
            Arc::new(UInt64Array::from(vec![u64::MAX])) as ArrayRef,
        )]);
        let e = read("T", write(&b, Compression::SNAPPY, 1024), BIG).unwrap_err();
        assert!(e.to_string().contains("column `huge`"), "{e}");
    }

    #[test]
    fn unsupported_type_names_the_column() {
        let list = ListArray::from_iter_primitive::<Int32Type, _, _>(vec![Some(vec![Some(1)])]);
        let b = batch(vec![("tags", Arc::new(list) as ArrayRef)]);
        let e = read("T", write(&b, Compression::SNAPPY, 1024), BIG).unwrap_err();
        assert!(matches!(e, Error::Source { .. }));
        assert!(e.to_string().contains("column `tags`"), "{e}");
    }

    #[test]
    fn corrupt_or_truncated_bytes_error() {
        let good = write(&sample(), Compression::SNAPPY, 1024);
        let bad = [
            Bytes::new(),
            Bytes::from_static(b"PAR1 not really parquet PAR1"),
            good.slice(..good.len() / 2),
            good.slice(good.len() / 2..),
        ];
        for b in bad {
            assert!(read("Bad", b, BIG).is_err());
        }
    }

    /// Files covering every page kind and value encoding the decoders have: v1 and v2 pages,
    /// dictionary, plain, byte-stream-split and delta encodings, nulls, fixed-length decimals.
    fn varied_files() -> Vec<Bytes> {
        use ::parquet::basic::Encoding;
        use ::parquet::file::properties::WriterVersion;
        use ::parquet::schema::types::ColumnPath;
        let n = 40;
        let b = batch(vec![
            (
                "s",
                Arc::new(StringArray::from_iter(
                    (0..n).map(|i| (i % 5 != 0).then(|| format!("v{}", i % 7))),
                )) as ArrayRef,
            ),
            (
                "i",
                Arc::new(Int64Array::from_iter(
                    (0..n).map(|i| (i % 4 != 0).then_some(i * 1000)),
                )),
            ),
            (
                "j",
                Arc::new(arrow_array::Int32Array::from_iter_values(0..n as i32)),
            ),
            (
                "f",
                Arc::new(Float64Array::from_iter(
                    (0..n).map(|i| (i % 3 != 0).then_some(i as f64 / 3.0)),
                )),
            ),
            (
                "b",
                Arc::new(BooleanArray::from_iter(
                    (0..n).map(|i| (i % 6 != 0).then_some(i % 2 == 0)),
                )),
            ),
            ("d", Arc::new(Date32Array::from_iter_values(0..n as i32))),
            (
                "m",
                Arc::new(
                    Decimal128Array::from_iter(
                        (0..n).map(|i| (i % 3 != 1).then_some(i as i128 % 5 * 1_000_001)),
                    )
                    .with_precision_and_scale(30, 2)
                    .unwrap(),
                ),
            ),
        ]);
        let col = |c: &str| ColumnPath::from(c);
        let mut files = Vec::new();
        for c in [Compression::UNCOMPRESSED, Compression::SNAPPY] {
            for v in [WriterVersion::PARQUET_1_0, WriterVersion::PARQUET_2_0] {
                for mode in 0..4 {
                    let mut p = WriterProperties::builder()
                        .set_compression(c)
                        .set_writer_version(v)
                        .set_data_page_row_count_limit(16)
                        .set_write_batch_size(8)
                        .set_dictionary_enabled(mode == 0);
                    if mode == 2 {
                        for c in ["f", "d", "m"] {
                            p = p.set_column_encoding(col(c), Encoding::BYTE_STREAM_SPLIT);
                        }
                    }
                    if mode == 3 {
                        p = p
                            .set_column_encoding(col("s"), Encoding::DELTA_LENGTH_BYTE_ARRAY)
                            .set_column_encoding(col("i"), Encoding::DELTA_BINARY_PACKED)
                            .set_column_encoding(col("j"), Encoding::DELTA_BINARY_PACKED)
                            .set_column_encoding(col("m"), Encoding::DELTA_BYTE_ARRAY);
                    }
                    let mut out = Vec::new();
                    let mut w =
                        ArrowWriter::try_new(&mut out, b.schema(), Some(p.build())).unwrap();
                    w.write(&b).unwrap();
                    w.close().unwrap();
                    files.push(Bytes::from(out));
                }
            }
        }
        files
    }

    /// The fuzz test calls `read_checked`, bypassing the native catch_unwind net: every damaged
    /// file must be rejected (or read) before any panic, as on wasm. Sampled to stay quick in
    /// debug builds; a full sweep of 2.7M cases (every byte × 12 flips and 9 values, 20,000
    /// random multi-byte edits a file, every cut) ran clean in release while this was written.
    /// Soak with `MCRAFT_FUZZ_SCALE` / `MCRAFT_FUZZ_SEED` (see `crate::fuzz`): the flip and cut
    /// sweeps get `scale` times denser (a full sweep from about 100) and the random edits
    /// `scale` times more.
    #[test]
    fn damaged_files_never_panic() {
        let mut fuzz = crate::fuzz::Fuzz::new("parquet", 0x9E37_79B9_7F4A_7C15);
        for good in varied_files() {
            let checked = |name: &str, b: Bytes| {
                collect(name, BIG, |sink| {
                    read_checked(name, b, usize::MAX, BIG, sink)
                })
            };
            assert_eq!(checked("ok", good.clone()).unwrap().0.rows(), 40);
            let len = good.len();
            let damaged = |fuzz: &mut crate::fuzz::Fuzz, v: Vec<u8>| {
                let b = Bytes::from(v);
                fuzz.case(&b, || {
                    let _ = checked("Damaged", b.clone());
                });
            };
            for i in (0..len).step_by((len / fuzz.scaled(300)).max(1)) {
                for flip in [0x01, 0x80, 0x07] {
                    let mut v = good.to_vec();
                    v[i] ^= flip;
                    damaged(&mut fuzz, v);
                }
            }
            for _ in 0..fuzz.scaled(300) {
                let mut v = good.to_vec();
                for _ in 0..fuzz.next() % 4 + 1 {
                    let i = fuzz.below(len);
                    v[i] = fuzz.next() as u8;
                }
                damaged(&mut fuzz, v);
            }
            for cut in (0..len).step_by((len / fuzz.scaled(40)).max(1)) {
                damaged(&mut fuzz, good.slice(..cut).to_vec());
            }
        }
        fuzz.finish();
    }

    #[test]
    fn hostile_levels_and_delta_streams_are_refused() {
        // A bit-packed level run longer than its bytes, and an over-long run header.
        assert!(set_levels(&[0x05, 0xFF], 16).is_err());
        assert_eq!(set_levels(&[0x03, 0b1011], 8), Ok(3));
        assert!(hybrid(&[0xFF; 11], 3, 8, None).is_err());
        // A dictionary index past a fixed-length dictionary.
        assert!(hybrid(&[0x02, 0x07], 3, 1, Some(5)).is_err());
        // Delta: an empty block layout, an over-long integer, a count past the page.
        assert!(delta_binary(&[0, 1, 1, 0], 32, 10, &mut |_| Ok(())).is_err());
        assert!(delta_binary(&[0xFF; 12], 32, 10, &mut |_| Ok(())).is_err());
        assert!(delta_binary(&[0x80, 0x01, 4, 99, 0], 32, 10, &mut |_| Ok(())).is_err());
        let mut seen = Vec::new();
        let ok = delta_binary(&[0x80, 0x01, 4, 1, 6], 32, 10, &mut |v| {
            seen.push(v);
            Ok(())
        });
        assert_eq!((ok, seen), (Ok((1, 5)), vec![3]));
    }

    /// Review M1 #11: page headers are walked as each row group is read, so a Preview of the
    /// first rows doesn't walk the whole file, and a damaged later group still fails the load.
    #[test]
    fn page_headers_are_walked_only_for_the_row_groups_read() {
        let ids = Int64Array::from_iter_values(0..5000);
        let good = write(
            &batch(vec![("id", Arc::new(ids) as ArrayRef)]),
            Compression::UNCOMPRESSED,
            1000,
        );
        let meta = ParquetMetaDataReader::new()
            .parse_and_finish(&good)
            .unwrap();
        assert_eq!(meta.num_row_groups(), 5);
        let last = meta.row_group(4).column(0);
        let at = last
            .dictionary_page_offset()
            .unwrap_or(last.data_page_offset()) as usize;
        let mut bad = good.to_vec();
        bad[at..at + 8].fill(0xFF);
        let bad = Bytes::from(bad);
        let rows = |n: usize| {
            collect("Ids", BIG, |sink| {
                read_checked("Ids", bad.clone(), n, BIG, sink)
            })
            .map(|(t, ())| t.rows())
        };
        assert_eq!(rows(1000), Ok(1000));
        let e = rows(usize::MAX).unwrap_err();
        assert!(
            matches!(&e, Error::Source { source, message } if source == "Ids" && message.starts_with("corrupt file")),
            "{e}"
        );
    }

    /// Review M1 #8: DELTA_BYTE_ARRAY's held prefixes (8 bytes each) are paid from the budget.
    #[test]
    fn delta_byte_array_prefixes_count_against_the_budget() {
        // 1,000 zero lengths: 128-value blocks of 4 mini-blocks, each block a zero min delta
        // and four zero bit widths.
        let mut stream = vec![0x80, 0x01, 4, 0xE8, 0x07, 0];
        for _ in 0..999usize.div_ceil(128) {
            stream.extend([0, 0, 0, 0, 0]);
        }
        let values = [stream.clone(), stream].concat();
        let checked = |limit: usize| Checked {
            inner: (),
            nullable: false,
            physical: Physical::BYTE_ARRAY,
            type_length: 0,
            dictionary: None,
            budget: Arc::new(Budget::new(limit)),
        };
        let check =
            |c: &Checked<()>| c.check_values(Encoding::DELTA_BYTE_ARRAY, &values, Some(1000), 1000);
        assert_eq!(check(&checked(1 << 20)), Ok(0));
        let small = checked(4000);
        assert_eq!(check(&small), Err("memory limit".to_string()));
        assert!(small.budget.exceeded());
    }

    #[test]
    fn memory_cap_names_the_table() {
        let e = read("Sales", write(&sample(), Compression::SNAPPY, 1024), 16).unwrap_err();
        assert!(
            matches!(e, Error::MemoryCap { ref table, .. } if table == "Sales"),
            "{e}"
        );
    }
}
