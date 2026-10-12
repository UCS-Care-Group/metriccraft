//! Sources: CSV, Parquet, XLSX (spec S-1). A [`Source`] describes where a Query's data comes
//! from (the file and how to read it); [`Source::read`] streams it as plain Arrow batches of at
//! most [`BATCH_ROWS`] rows (`Utf8`, `Int64`, `Float64`, `Boolean`, `Date32`,
//! `Timestamp(Microsecond, None)`), stopping early when asked for fewer rows (Preview), so
//! peak memory stays about one batch above what the caller keeps (ADR 0005).
//!
//! A Source reads *raw*, as Power Query's source functions do: a CSV is `Column1..N`, all text,
//! and an Excel sheet keeps its first row as data. The automatic *Promoted Headers* and *Changed
//! Type* steps are found once, when connecting ([`Source::detect`]), and live in the Query's step
//! list.

use std::io::{BufReader, Cursor};
use std::path::PathBuf;

use arrow_array::{Array, ArrayRef, RecordBatch, StringArray};
use arrow_cast::cast;
use arrow_schema::{DataType, SchemaRef};
pub use bytes::Bytes;
pub use encoding_rs::Encoding;

use crate::culture::Culture;
use crate::step::{ColumnType, Step};
use crate::{Error, Result, Table, TableBuilder};

pub mod csv;
pub mod parquet;
pub mod xlsx;

/// Rows per batch read from a Source.
pub const BATCH_ROWS: usize = 8192;
/// Text bytes after which a CSV or XLSX batch ends early, so long text can't make one batch
/// huge.
pub const BATCH_BYTES: usize = 64 << 20;
/// Source rows a Preview runs on (CONTEXT.md).
pub const PREVIEW_ROWS: usize = 1000;
/// Rows the automatic Changed Type looks at to detect column types (ADR 0005).
pub const SAMPLE_ROWS: usize = 200;

/// Where a Query's data comes from. Printed as M by the `m` module; each variant's doc gives the
/// Power Query source it stands for.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// `Csv.Document(File.Contents(file), [Delimiter=delimiter, Columns=columns,
    /// Encoding=encoding, QuoteStyle=QuoteStyle.Csv])`: columns `Column1..columns`, all text.
    /// Found when connecting ([`Source::csv`]) and fixed from then on, as Power Query writes
    /// them into the M: later rows wider than `columns` lose their extra fields, shorter rows
    /// get nulls.
    Csv {
        file: File,
        /// The field separator byte (`,` `;` tab `|` …).
        delimiter: u8,
        /// How many columns to read.
        columns: usize,
        /// The text encoding, used when the file has no byte-order mark (a BOM wins).
        encoding: &'static Encoding,
    },
    /// `Parquet.Document(File.Contents(file))`: the file's own columns and types.
    Parquet { file: File },
    /// `Excel.Workbook(File.Contents(file), null, true){[Item=item, Kind="Sheet"|"Table"]}[Data]`.
    /// A sheet reads raw: `Column1..N` over its used columns, the first used row kept as data
    /// (*Promoted Headers* is a step). A table's header row names its columns.
    Excel {
        file: File,
        /// The sheet or table name.
        item: String,
        kind: ExcelKind,
    },
}

/// Which kind of workbook item an Excel Source reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExcelKind {
    Sheet,
    Table,
}

/// A Source's file: a path on disk, or the file's bytes where there is no file system to re-read
/// (the web build keeps the picked file in memory; cloning shares the bytes).
#[derive(Clone, Debug, PartialEq)]
pub enum File {
    Path(PathBuf),
    /// `name` is the picked file's name: shown, and the path printed in M.
    Bytes {
        name: String,
        bytes: Bytes,
    },
}

impl File {
    /// The path (or picked file name) as text, for M's `File.Contents` and messages.
    pub fn path(&self) -> String {
        match self {
            Self::Path(p) => p.display().to_string(),
            Self::Bytes { name, .. } => name.clone(),
        }
    }

    fn open(&self, name: &str) -> Result<Input> {
        match self {
            Self::Path(p) => std::fs::File::open(p)
                .map(Input::Disk)
                .map_err(|e| Error::Source {
                    source: name.into(),
                    message: format!("{}: {e}", p.display()),
                }),
            Self::Bytes { bytes, .. } => Ok(Input::Memory(bytes.clone())),
        }
    }
}

enum Input {
    Disk(std::fs::File),
    Memory(Bytes),
}

/// Receives a Source's batches in order. There is always at least one (empty if the Source has
/// no rows, so the schema is known).
pub type Sink<'a> = dyn FnMut(RecordBatch) -> Result + 'a;

impl Source {
    /// A CSV Source for `file`, with the delimiter, encoding and column count detected (unless
    /// given in `options`) on its first [`SAMPLE_ROWS`] + 1 records. `name` labels errors.
    pub fn csv(name: &str, file: File, options: csv::Options) -> Result<Source> {
        let format = match file.open(name)? {
            Input::Disk(f) => csv::sniff(name, f, options),
            Input::Memory(b) => csv::sniff(name, Cursor::new(b), options),
        }?;
        Ok(Source::Csv {
            file,
            delimiter: format.delimiter,
            columns: format.columns,
            encoding: format.encoding,
        })
    }

    pub fn file(&self) -> &File {
        match self {
            Self::Csv { file, .. } | Self::Parquet { file } | Self::Excel { file, .. } => file,
        }
    }

    /// Streams at most `rows` rows into `sink` (reading stops there), holding at most `limit`
    /// bytes; `name` (the Query's) labels errors. Returns the Cell errors found in the file
    /// itself (Excel error cells, read as blank).
    pub fn read(
        &self,
        name: &str,
        rows: usize,
        culture: &Culture,
        limit: usize,
        sink: &mut Sink<'_>,
    ) -> Result<usize> {
        match self {
            Self::Csv {
                file,
                delimiter,
                columns,
                encoding,
            } => {
                let format = csv::Format {
                    delimiter: *delimiter,
                    encoding,
                    columns: *columns,
                };
                match file.open(name)? {
                    Input::Disk(f) => csv::read(name, f, &format, rows, sink),
                    Input::Memory(b) => csv::read(name, Cursor::new(b), &format, rows, sink),
                }?;
                Ok(0)
            }
            Self::Parquet { file } => {
                match file.open(name)? {
                    Input::Disk(f) => parquet::read_rows(name, f, rows, limit, sink),
                    Input::Memory(b) => parquet::read_rows(name, b, rows, limit, sink),
                }?;
                Ok(0)
            }
            Self::Excel { file, item, kind } => {
                let input = match file.open(name)? {
                    Input::Disk(f) => Seekable::Disk(BufReader::new(f)),
                    Input::Memory(b) => Seekable::Memory(Cursor::new(b)),
                };
                let mut wb = xlsx::Workbook::open(input, limit)?;
                match kind {
                    ExcelKind::Sheet => wb.read_sheet(item, rows, culture.decimal, limit, sink),
                    ExcelKind::Table => wb.read_table(item, rows, culture.decimal, limit, sink),
                }
            }
        }
    }

    /// The first `rows` rows as one batch (Preview, detection), and the file's own Cell errors.
    pub fn read_batch(
        &self,
        name: &str,
        rows: usize,
        culture: &Culture,
        limit: usize,
    ) -> Result<(RecordBatch, usize)> {
        let mut batches = Vec::new();
        let mut used = 0usize;
        let errors = self.read(name, rows, culture, limit, &mut |b| {
            used = used.saturating_add(b.get_array_memory_size());
            if used > limit {
                return Err(Error::MemoryCap {
                    table: name.into(),
                    limit,
                });
            }
            batches.push(b);
            Ok(())
        })?;
        let schema = batches
            .first()
            .map(RecordBatch::schema)
            .ok_or_else(|| Error::Source {
                source: name.into(),
                message: "the source gave no data".into(),
            })?;
        let batch =
            arrow_select::concat::concat_batches(&schema, &batches).map_err(|e| Error::Arrow {
                table: name.into(),
                message: e.to_string(),
            })?;
        Ok((batch, errors))
    }

    /// The automatic steps for a newly connected Source (ADR 0005), from its first
    /// [`SAMPLE_ROWS`] + 1 rows: *Promoted Headers* when the first row looks like headers (CSV and
    /// Excel sheets, which have no headers of their own), then *Changed Type* with each column's
    /// type detected. Parquet columns are typed already: no steps.
    ///
    /// Parity uncertainties, to settle with M1.10 cases in the VM: the first row is headers when
    /// every cell of it is non-empty text and at least one column below detects as non-text (an
    /// all-text file keeps `Column1..N`); typed columns keep their type; a text column's type is
    /// detected from its text even when the file is Excel (where Power Query may keep text).
    pub fn detect(&self, sample: &RecordBatch, culture: &Culture) -> Result<Vec<Step>> {
        let headerless = match self {
            Self::Parquet { .. } => return Ok(Vec::new()),
            Self::Csv { .. } => true,
            Self::Excel { kind, .. } => *kind == ExcelKind::Sheet,
        };
        let mut steps = Vec::with_capacity(2);
        let below = sample.slice(1, sample.num_rows().saturating_sub(1));
        let mut batch = sample.slice(0, sample.num_rows().min(SAMPLE_ROWS));
        if headerless
            && sample.num_rows() > 1
            && sample.columns().iter().all(|c| first_is_text(c, culture))
            && below
                .columns()
                .iter()
                .any(|c| column_type(c, culture) != ColumnType::Text)
        {
            steps.push(Step::PromotedHeaders);
            batch = Step::PromotedHeaders.apply(sample, culture)?.0;
        }
        let schema = batch.schema();
        let types: Vec<(String, ColumnType)> = schema
            .fields()
            .iter()
            .zip(batch.columns())
            .map(|(f, c)| (f.name().clone(), column_type(c, culture)))
            .collect();
        if !types.is_empty() {
            steps.push(Step::ChangedType(types));
        }
        Ok(steps)
    }
}

/// A seekable reader over a file or bytes (XLSX needs `Seek`).
enum Seekable {
    Disk(BufReader<std::fs::File>),
    Memory(Cursor<Bytes>),
}

impl std::io::Read for Seekable {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Disk(f) => f.read(buf),
            Self::Memory(c) => c.read(buf),
        }
    }
}

impl std::io::Seek for Seekable {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::Disk(f) => f.seek(pos),
            Self::Memory(c) => c.seek(pos),
        }
    }
}

/// `column` as text (any type), or `None` if it doesn't cast.
fn as_text(column: &ArrayRef) -> Option<StringArray> {
    let text = cast(column, &DataType::Utf8).ok()?;
    text.as_any().downcast_ref::<StringArray>().cloned()
}

/// Whether the first value of `column` is non-empty text that isn't a number, date or boolean.
fn first_is_text(column: &ArrayRef, culture: &Culture) -> bool {
    let Some(text) = as_text(&column.slice(0, column.len().min(1))) else {
        return false;
    };
    text.iter()
        .next()
        .flatten()
        .is_some_and(|v| ColumnType::of(v, culture) == Some(ColumnType::Text))
}

/// The type of a column's first [`SAMPLE_ROWS`] values: typed columns keep their type, text is
/// detected with the Culture.
fn column_type(column: &ArrayRef, culture: &Culture) -> ColumnType {
    match column.data_type() {
        t if t.is_integer() => ColumnType::Whole,
        t if t.is_floating() => ColumnType::Decimal,
        DataType::Boolean => ColumnType::Boolean,
        DataType::Date32 | DataType::Date64 => ColumnType::Date,
        DataType::Timestamp(..) => ColumnType::DateTime,
        _ => {
            let sample = column.slice(0, column.len().min(SAMPLE_ROWS));
            match as_text(&sample) {
                Some(text) => ColumnType::detect(text.iter().map(|v| v.unwrap_or("")), culture),
                None => ColumnType::Text,
            }
        }
    }
}

/// Streams batches into a Table, stopping at the first error or once `limit` bytes are used.
pub fn load(
    name: &str,
    schema: SchemaRef,
    batches: impl IntoIterator<Item = Result<RecordBatch>>,
    limit: usize,
) -> Result<Table> {
    let mut b = TableBuilder::new(name, schema, limit);
    for batch in batches {
        b.push(&batch?)?;
    }
    b.finish()
}

/// Loads what `read` sends its sink into a Table named `name`; returns it with `read`'s result.
pub(crate) fn collect<T>(
    name: &str,
    limit: usize,
    read: impl FnOnce(&mut Sink<'_>) -> Result<T>,
) -> Result<(Table, T)> {
    let mut builder: Option<TableBuilder> = None;
    let out = read(&mut |batch| {
        builder
            .get_or_insert_with(|| TableBuilder::new(name, batch.schema(), limit))
            .push(&batch)
    })?;
    let table = builder
        .ok_or_else(|| Error::Source {
            source: name.into(),
            message: "the source gave no data".into(),
        })?
        .finish()?;
    Ok((table, out))
}

/// Sends `batches` to `sink`, at most `rows` rows in all: stops pulling once they are sent, so
/// the reader behind `batches` stops too. Sends one empty batch if there are none.
pub(crate) fn emit(
    schema: SchemaRef,
    batches: impl IntoIterator<Item = Result<RecordBatch>>,
    rows: usize,
    sink: &mut Sink<'_>,
) -> Result {
    let mut left = rows;
    let mut sent = false;
    for batch in batches {
        if left == 0 {
            break;
        }
        let batch = batch?;
        let batch = if batch.num_rows() > left {
            batch.slice(0, left)
        } else {
            batch
        };
        left -= batch.num_rows();
        sink(batch)?;
        sent = true;
    }
    if !sent {
        sink(RecordBatch::new_empty(schema))?;
    }
    Ok(())
}

/// Rows per batch when at most `rows` are wanted: a Preview reads 1,000, not [`BATCH_ROWS`].
pub(crate) fn batch_rows(rows: usize) -> usize {
    BATCH_ROWS.min(rows.max(1))
}

#[cfg(test)]
mod tests {
    use arrow_array::cast::AsArray;

    use super::*;

    fn culture() -> &'static Culture {
        Culture::get("en-US").unwrap()
    }

    fn csv_source(text: &str) -> Source {
        let file = File::Bytes {
            name: "t.csv".into(),
            bytes: Bytes::from(text.to_string()),
        };
        Source::csv("T", file, csv::Options::default()).unwrap()
    }

    #[test]
    fn csv_sources_read_raw_and_detect_steps() {
        let s = csv_source("Region;Qty\nNorth;3\nSouth;4\n");
        let Source::Csv {
            delimiter, columns, ..
        } = &s
        else {
            panic!("{s:?}")
        };
        assert_eq!((*delimiter, *columns), (b';', 2));
        let (raw, errors) = s
            .read_batch("T", usize::MAX, culture(), usize::MAX)
            .unwrap();
        assert_eq!((raw.num_rows(), errors), (3, 0));
        assert_eq!(raw.schema().field(0).name(), "Column1");
        assert_eq!(raw.column(0).as_string::<i32>().value(0), "Region");
        assert_eq!(
            s.detect(&raw, culture()).unwrap(),
            [
                Step::PromotedHeaders,
                Step::ChangedType(vec![
                    ("Region".into(), ColumnType::Text),
                    ("Qty".into(), ColumnType::Whole)
                ])
            ]
        );
        // Read stops at `rows`.
        let (two, _) = s.read_batch("T", 2, culture(), usize::MAX).unwrap();
        assert_eq!(two.num_rows(), 2);
    }

    #[test]
    fn excel_sheets_connect_with_automatic_steps_in_any_culture() {
        let mut book = rust_xlsxwriter::Workbook::new();
        let ws = book.add_worksheet().set_name("Sales").unwrap();
        ws.write_string(0, 0, "Region").unwrap();
        ws.write_string(0, 1, "Qty").unwrap();
        ws.write_string(1, 0, "North").unwrap();
        ws.write_number(1, 1, 1.5).unwrap();
        let file = File::Bytes {
            name: "book.xlsx".into(),
            bytes: Bytes::from(book.save_to_buffer().unwrap()),
        };
        let source = Source::Excel {
            file,
            item: "Sales".into(),
            kind: ExcelKind::Sheet,
        };
        let de = Culture::get("de-DE").unwrap();
        let q = crate::Query::connect("Sales", source, de, usize::MAX).unwrap();
        assert_eq!(
            q.steps,
            [
                Step::PromotedHeaders,
                Step::ChangedType(vec![
                    ("Region".into(), ColumnType::Text),
                    ("Qty".into(), ColumnType::Decimal)
                ])
            ]
        );
        let (t, errors) = q.run(usize::MAX).unwrap();
        let qty = t
            .batch()
            .column(1)
            .as_primitive::<arrow_array::types::Float64Type>();
        assert_eq!((qty.value(0), errors), (1.5, 0));
    }

    #[test]
    fn parquet_sources_have_no_automatic_steps_and_stop_early() {
        let ids = arrow_array::Int64Array::from_iter_values(0..5000);
        let batch =
            RecordBatch::try_from_iter([("id", std::sync::Arc::new(ids) as ArrayRef)]).unwrap();
        let mut out = Vec::new();
        let mut w = ::parquet::arrow::ArrowWriter::try_new(&mut out, batch.schema(), None).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        let source = Source::Parquet {
            file: File::Bytes {
                name: "ids.parquet".into(),
                bytes: Bytes::from(out),
            },
        };
        let q = crate::Query::connect("Ids", source, culture(), usize::MAX).unwrap();
        assert!(q.steps.is_empty());
        let p = q.preview(None, usize::MAX).unwrap();
        assert_eq!(p.batch.num_rows(), PREVIEW_ROWS);
        assert_eq!(q.run(usize::MAX).unwrap().0.rows(), 5000);
    }

    #[test]
    fn a_missing_file_is_an_error_naming_the_query() {
        let s = Source::Parquet {
            file: File::Path("/nonexistent/opendash/x.parquet".into()),
        };
        let e = s
            .read_batch("Sales", 10, culture(), usize::MAX)
            .unwrap_err();
        assert!(e.to_string().starts_with("Sales: "), "{e}");
        assert!(e.to_string().contains("x.parquet"), "{e}");
    }
}
