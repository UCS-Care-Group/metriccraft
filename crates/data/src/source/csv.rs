//! CSV Source (spec S-1): reads like Power Query's `Csv.Document`: columns `Column1..N`, all
//! text, streaming in batches so the whole file is never held. The automatic *Promoted Headers*
//! and *Changed Type* steps are the Query's ([`super::Source::detect`]).
//!
//! Parity uncertainties, to settle with M1.10 cases in the VM:
//! - The column count is the widest row in the first 201 rows (Power Query writes it into
//!   `Csv.Document` as `Columns=N`); later wider rows lose the extra fields, shorter rows get
//!   nulls.
//! - Blank lines are skipped. Quoted fields may contain line breaks (`QuoteStyle.Csv`).
//! - Without a BOM, valid UTF-8 in the first 64 KB means UTF-8, otherwise Windows-1252.
//!
//! Hostile input (spec N-1, N-4): a record (line, or quoted multi-line field) over
//! [`MAX_RECORD`] bytes is an error naming its line, and a batch ends early once it holds
//! [`BATCH_BYTES`] of text.

use std::cell::Cell;
use std::io::{self, Read};
use std::rc::Rc;
use std::sync::Arc;

use ::csv::{ReaderBuilder, StringRecord};
use arrow_array::builder::StringBuilder;
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use encoding_rs::{CoderResult, Decoder, Encoding, UTF_8, WINDOWS_1252};

use super::{BATCH_BYTES, SAMPLE_ROWS, Sink, batch_rows, emit};
use crate::{Error, Result};

/// Power Query's column limit.
const MAX_COLUMNS: usize = 16_384;
/// Bytes read at a time; the first chunk also decides encoding and delimiter.
const CHUNK: usize = 64 * 1024;
/// Candidate delimiters, in order of preference.
const DELIMITERS: [u8; 4] = *b",;\t|";
/// Longest record accepted, in decoded bytes: one huge line can't exhaust memory.
pub const MAX_RECORD: u64 = 16 << 20;
/// Read-ahead the csv crate may buffer past a record (its buffer is 8 KB).
const FENCE_SLACK: u64 = 64 * 1024;

/// How to read the file; `None` means detect.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub delimiter: Option<u8>,
    pub encoding: Option<&'static Encoding>,
}

/// How a CSV file is read: what connecting found ([`sniff`]), kept in [`super::Source::Csv`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Format {
    pub delimiter: u8,
    pub encoding: &'static Encoding,
    pub columns: usize,
}

/// Finds a file's delimiter and encoding (unless given) and its column count: the widest of the
/// first [`SAMPLE_ROWS`] + 1 records.
pub fn sniff(name: &str, reader: impl Read, options: Options) -> Result<Format> {
    let (delimiter, encoding, mut records) = open(name, reader, options)?;
    let mut r = StringRecord::new();
    let mut columns = 0;
    for _ in 0..=SAMPLE_ROWS {
        if !records.next(&mut r)? {
            break;
        }
        columns = columns.max(r.len());
    }
    let format = Format {
        delimiter,
        encoding,
        columns,
    };
    check(name, &format)?;
    Ok(format)
}

fn check(name: &str, format: &Format) -> Result {
    let err = |message: String| Error::Source {
        source: name.into(),
        message,
    };
    match format.columns {
        0 => Err(err("the file has no data".into())),
        n if n > MAX_COLUMNS => Err(err(format!("{n} columns; the limit is {MAX_COLUMNS}"))),
        _ => Ok(()),
    }
}

/// Reads at most `rows` records as `Column1..N` text batches into `sink`, then stops reading.
pub fn read(
    name: &str,
    reader: impl Read,
    format: &Format,
    rows: usize,
    sink: &mut Sink<'_>,
) -> Result {
    check(name, format)?;
    let options = Options {
        delimiter: Some(format.delimiter),
        encoding: Some(format.encoding),
    };
    let (_, _, mut records) = open(name, reader, options)?;
    let width = format.columns;
    let schema = Arc::new(Schema::new(
        (1..=width)
            .map(|i| Field::new(format!("Column{i}"), DataType::Utf8, true))
            .collect::<Vec<_>>(),
    ));
    let per_batch = batch_rows(rows);
    let mut record = StringRecord::new();
    let mut done = false;
    let batches = std::iter::from_fn(|| {
        if done {
            return None;
        }
        let mut text: Vec<StringBuilder> = (0..width).map(|_| StringBuilder::new()).collect();
        let (mut n, mut bytes) = (0, 0usize);
        while n < per_batch && bytes < BATCH_BYTES {
            match records.next(&mut record) {
                Ok(true) => {}
                Ok(false) => {
                    done = true;
                    break;
                }
                Err(e) => {
                    done = true;
                    return Some(Err(e));
                }
            }
            bytes = bytes.saturating_add(record.as_byte_record().as_slice().len());
            for (c, col) in text.iter_mut().enumerate() {
                col.append_option(record.get(c));
            }
            n += 1;
        }
        if n == 0 {
            return None;
        }
        let columns: Vec<ArrayRef> = text
            .iter_mut()
            .map(|c| Arc::new(c.finish()) as ArrayRef)
            .collect();
        Some(
            RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|e| Error::Source {
                source: name.into(),
                message: e.to_string(),
            }),
        )
    });
    emit(Arc::clone(&schema), batches, rows, sink)
}

/// A file's records, decoded to UTF-8, refusing one longer than [`MAX_RECORD`].
struct Records<'n, R> {
    csv: ::csv::Reader<Fenced<Decode<R>>>,
    fence: Rc<Cell<u64>>,
    name: &'n str,
}

impl<R: Read> Records<'_, R> {
    fn next(&mut self, r: &mut StringRecord) -> Result<bool> {
        // The csv crate buffers a record whole; the fence stops it past MAX_RECORD.
        let start = self.csv.position().clone();
        self.fence
            .set(start.byte().saturating_add(MAX_RECORD + FENCE_SLACK));
        self.csv.read_record(r).map_err(|e| Error::Source {
            source: self.name.into(),
            message: match e.kind() {
                ::csv::ErrorKind::Io(io) if io.kind() == io::ErrorKind::FileTooLarge => format!(
                    "line {}: a record is longer than {} MB",
                    start.line(),
                    MAX_RECORD >> 20
                ),
                _ => e.to_string(),
            },
        })
    }
}

/// Opens a file: the encoding (a BOM wins over `options`) and delimiter, detected unless given.
fn open<R: Read>(
    name: &str,
    mut reader: R,
    options: Options,
) -> Result<(u8, &'static Encoding, Records<'_, R>)> {
    let mut prefix = Vec::with_capacity(CHUNK);
    reader
        .by_ref()
        .take(CHUNK as u64)
        .read_to_end(&mut prefix)
        .map_err(|e| Error::Source {
            source: name.into(),
            message: e.to_string(),
        })?;
    let encoding = match Encoding::for_bom(&prefix) {
        Some((bom, _)) => bom,
        None => options.encoding.unwrap_or_else(|| guess(&prefix)),
    };
    let delimiter = options
        .delimiter
        .unwrap_or_else(|| sniff_delimiter(&encoding.decode(&prefix).0));
    let fence = Rc::new(Cell::new(0));
    let csv = ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(Fenced {
            inner: Decode::new(reader, prefix, encoding.new_decoder()),
            read: 0,
            fence: Rc::clone(&fence),
        });
    Ok((delimiter, encoding, Records { csv, fence, name }))
}

/// Without a BOM: UTF-8 if the bytes are valid UTF-8 (a sequence cut at the end is fine),
/// otherwise Windows-1252.
fn guess(prefix: &[u8]) -> &'static Encoding {
    match std::str::from_utf8(prefix) {
        Ok(_) => UTF_8,
        Err(e) if e.error_len().is_none() => UTF_8,
        Err(_) => WINDOWS_1252,
    }
}

/// The candidate delimiter that splits the first lines most consistently (quotes respected);
/// ties go to the earlier candidate, and `,` when none appears.
fn sniff_delimiter(text: &str) -> u8 {
    let mut lines: Vec<[usize; 4]> = Vec::new();
    let (mut counts, mut quoted) = ([0; 4], false);
    for b in text.bytes() {
        match b {
            b'"' => quoted = !quoted,
            b'\n' if !quoted => {
                lines.push(std::mem::take(&mut counts));
                if lines.len() == 20 {
                    break;
                }
            }
            _ if !quoted => {
                if let Some(i) = DELIMITERS.iter().position(|&d| d == b) {
                    counts[i] += 1;
                }
            }
            _ => {}
        }
    }
    if lines.is_empty() {
        lines.push(counts); // one line with no line break
    }
    let score = |i: usize| {
        let first = lines.first().map_or(0, |l| l[i]);
        let consistent = lines.iter().filter(|l| l[i] == first).count();
        (first > 0, consistent, first)
    };
    (0..DELIMITERS.len())
        .rev() // max_by_key keeps the last maximum
        .max_by_key(|&i| score(i))
        .filter(|&i| score(i).0)
        .and_then(|i| DELIMITERS.get(i).copied())
        .unwrap_or(b',')
}

/// Hands the csv reader bytes up to a fence (set before each record) and fails past it.
struct Fenced<R> {
    inner: R,
    read: u64,
    fence: Rc<Cell<u64>>,
}

impl<R: Read> Read for Fenced<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let room = self.fence.get().saturating_sub(self.read);
        if room == 0 {
            return Err(io::ErrorKind::FileTooLarge.into());
        }
        let n = usize::try_from(room).unwrap_or(usize::MAX).min(buf.len());
        let n = self.inner.read(buf.get_mut(..n).unwrap_or_default())?;
        self.read += n as u64;
        Ok(n)
    }
}

/// Decodes any encoding to UTF-8 as it streams (BOM sniffed and removed by the decoder).
struct Decode<R> {
    inner: R,
    decoder: Decoder,
    input: Vec<u8>,
    start: usize,
    end: usize,
    eof: bool,
    out: Vec<u8>,
    out_len: usize,
    pos: usize,
    finished: bool,
}

impl<R: Read> Decode<R> {
    fn new(inner: R, prefix: Vec<u8>, decoder: Decoder) -> Self {
        Self {
            inner,
            decoder,
            start: 0,
            end: prefix.len(),
            eof: prefix.len() < CHUNK,
            input: prefix,
            out: vec![0; CHUNK],
            out_len: 0,
            pos: 0,
            finished: false,
        }
    }
}

impl<R: Read> Read for Decode<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.pos < self.out_len {
                let src = self.out.get(self.pos..self.out_len).unwrap_or_default();
                let n = src.len().min(buf.len());
                if let (Some(dst), Some(src)) = (buf.get_mut(..n), src.get(..n)) {
                    dst.copy_from_slice(src);
                }
                self.pos += n;
                return Ok(n);
            }
            if self.finished {
                return Ok(0);
            }
            if self.start >= self.end && !self.eof {
                self.input.resize(CHUNK, 0);
                let n = self.inner.read(&mut self.input)?;
                (self.start, self.end, self.eof) = (0, n, n == 0);
            }
            let input = self.input.get(self.start..self.end).unwrap_or_default();
            let (result, read, written, _) =
                self.decoder.decode_to_utf8(input, &mut self.out, self.eof);
            self.start += read;
            (self.out_len, self.pos) = (written, 0);
            self.finished = self.eof && result == CoderResult::InputEmpty;
        }
    }
}

#[cfg(test)]
mod tests {
    use arrow_array::cast::AsArray;
    use arrow_array::types::{Float64Type, Int8Type, Int16Type};
    use arrow_array::{Array, StringArray};

    use super::*;
    use crate::culture::Culture;
    use crate::source::{BATCH_ROWS, Bytes, File, Source};
    use crate::step::{ColumnType, Step};
    use crate::{Query, Table};

    fn culture(name: &str) -> &'static Culture {
        Culture::get(name).unwrap()
    }

    /// What connecting found and loading reported.
    struct Import {
        delimiter: u8,
        encoding: &'static Encoding,
        steps: Vec<Step>,
        cell_errors: usize,
    }

    /// Connects to `bytes` as Query `name` (automatic steps included) and loads it.
    fn connect(
        name: &str,
        bytes: &[u8],
        options: Options,
        culture: &'static Culture,
        limit: usize,
    ) -> crate::Result<(Table, Import)> {
        let file = File::Bytes {
            name: "t.csv".into(),
            bytes: Bytes::copy_from_slice(bytes),
        };
        let source = Source::csv(name, file, options)?;
        let Source::Csv {
            delimiter,
            encoding,
            ..
        } = source
        else {
            panic!("{source:?}")
        };
        let q = Query::connect(name, source, culture, limit)?;
        let (table, cell_errors) = q.run(limit)?;
        Ok((
            table,
            Import {
                delimiter,
                encoding,
                steps: q.steps,
                cell_errors,
            },
        ))
    }

    fn run(bytes: &[u8], name: &str) -> crate::Result<(Table, Import)> {
        connect("T", bytes, Options::default(), culture(name), usize::MAX)
    }

    /// Column `c` as text, whatever its storage (dictionary or plain).
    fn text(t: &Table, c: usize) -> Vec<Option<String>> {
        let col = t.batch().column(c);
        let dict = col.as_any_dictionary();
        let values: &StringArray = dict.values().as_string();
        let keys = dict.normalized_keys();
        (0..col.len())
            .map(|i| (!col.is_null(i)).then(|| values.value(keys[i]).to_string()))
            .collect()
    }

    fn names(t: &Table) -> Vec<String> {
        t.batch()
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    fn some(v: &[&str]) -> Vec<Option<String>> {
        v.iter().map(|s| Some(s.to_string())).collect()
    }

    #[test]
    fn detects_delimiters() {
        for d in [",", ";", "\t", "|"] {
            let csv = format!("a{d}b{d}c\n1{d}2{d}3\n");
            let (t, i) = run(csv.as_bytes(), "en-US").unwrap();
            assert_eq!(i.delimiter, d.as_bytes()[0], "{d:?}");
            assert_eq!(t.batch().num_columns(), 3, "{d:?}");
        }
        // Commas inside quotes don't count; `;` splits every line.
        let (_, i) = run(b"\"a,b,c\";x\n\"1,2\";y\n", "en-US").unwrap();
        assert_eq!(i.delimiter, b';');
        // A given delimiter wins.
        let opts = Options {
            delimiter: Some(b'|'),
            ..Options::default()
        };
        let (t, _) = connect("T", b"a,b|c\n", opts, culture("en-US"), usize::MAX).unwrap();
        assert_eq!(text(&t, 0), some(&["a,b"]));
    }

    #[test]
    fn decodes_encodings_and_boms() {
        let utf8 = "name,n\ncafé,1\n";
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(utf8.as_bytes());
        let utf16 = |be: bool| {
            let mut v = if be {
                vec![0xFE, 0xFF]
            } else {
                vec![0xFF, 0xFE]
            };
            for u in utf8.encode_utf16() {
                v.extend(if be { u.to_be_bytes() } else { u.to_le_bytes() });
            }
            v
        };
        let cp1252 = b"name,n\ncaf\xE9,1\n".to_vec();
        for (bytes, enc) in [
            (utf8.as_bytes().to_vec(), UTF_8),
            (bom, UTF_8),
            (utf16(false), encoding_rs::UTF_16LE),
            (utf16(true), encoding_rs::UTF_16BE),
            (cp1252, WINDOWS_1252),
        ] {
            let (t, i) = run(&bytes, "en-US").unwrap();
            assert_eq!(i.encoding, enc);
            assert_eq!(names(&t), ["name", "n"], "{enc:?}");
            assert_eq!(text(&t, 0), some(&["café"]), "{enc:?}");
        }
    }

    #[test]
    fn quoted_fields() {
        let csv = b"Column,Qty\n\"a, \"\"quoted\"\"\nvalue\",1\nplain,2\n";
        let (t, _) = run(csv, "en-US").unwrap();
        assert_eq!(text(&t, 0), some(&["a, \"quoted\"\nvalue", "plain"]));
    }

    #[test]
    fn ragged_rows_never_panic() {
        let csv = b"a,b\n1\n1,2,3\n,\n";
        let (t, i) = run(csv, "en-US").unwrap();
        assert_eq!(names(&t), ["Column1", "Column2", "Column3"]);
        assert_eq!(t.rows(), 4);
        assert_eq!(text(&t, 1)[1], None, "missing field is null");
        assert_eq!(i.cell_errors, 0);
        // A wider row after the sample loses its extra fields.
        let mut csv = "a,b\n".repeat(300);
        csv.push_str("1,2,3,4\n");
        let (t, _) = run(csv.as_bytes(), "en-US").unwrap();
        assert_eq!((t.rows(), t.batch().num_columns()), (301, 2));
    }

    #[test]
    fn header_detection() {
        let (t, i) = run(b"Name,Qty,Name\nx,1,y\n", "en-US").unwrap();
        assert_eq!(names(&t), ["Name", "Qty", "Name_1"]);
        assert_eq!(i.steps.first(), Some(&Step::PromotedHeaders));
        // All text: nothing says the first row is a header.
        let (t, i) = run(b"Name,City\nx,y\n", "en-US").unwrap();
        assert_eq!(names(&t), ["Column1", "Column2"]);
        assert_eq!(t.rows(), 2);
        assert_eq!(
            i.steps,
            [Step::ChangedType(vec![
                ("Column1".into(), ColumnType::Text),
                ("Column2".into(), ColumnType::Text)
            ])]
        );
        // A number in the first row: not a header.
        let (t, _) = run(b"Name,1\nx,2\n", "en-US").unwrap();
        assert_eq!(names(&t), ["Column1", "Column2"]);
        // An empty header cell: not a header.
        let (t, _) = run(b"Name,\nx,2\n", "en-US").unwrap();
        assert_eq!(t.rows(), 2);
    }

    #[test]
    fn type_detection() {
        let csv = b"Text,Whole,Decimal,Date,DateTime,Flag\n\
            a,1,1.5,2026-03-04,2026-03-04 10:00,true\n\
            b,-2,2,04/03/2026,2026-03-04,FALSE\n";
        let (t, i) = run(csv, "en-GB").unwrap();
        let Some(Step::ChangedType(types)) = i.steps.get(1) else {
            panic!("{:?}", i.steps)
        };
        let types: Vec<_> = types.iter().map(|(_, t)| *t).collect();
        assert_eq!(
            types,
            [
                ColumnType::Text,
                ColumnType::Whole,
                ColumnType::Decimal,
                ColumnType::Date,
                ColumnType::DateTime,
                ColumnType::Boolean
            ]
        );
        let stored: Vec<_> = t
            .batch()
            .schema()
            .fields()
            .iter()
            .map(|f| f.data_type().clone())
            .collect();
        assert_eq!(stored[1], DataType::Int8);
        assert_eq!(stored[3], DataType::Date32);
        let dates = t
            .batch()
            .column(3)
            .as_primitive::<arrow_array::types::Date32Type>();
        assert_eq!(dates.values().to_vec(), [20_516, 20_516]);
        // Detection looks at the first 200 rows only: a later "x" is a Cell error.
        let mut csv = String::from("N\n");
        csv.push_str(&"1\n".repeat(200));
        csv.push_str("x\n");
        let (t, i) = run(csv.as_bytes(), "en-US").unwrap();
        assert_eq!(t.batch().column(0).data_type(), &DataType::Int8);
        assert_eq!(i.cell_errors, 1);
    }

    #[test]
    fn numbers_and_dates_follow_the_culture() {
        let csv = "Amount;When\n\"1,234.5\";03/04/2026\n";
        let (t, _) = run(csv.as_bytes(), "en-US").unwrap();
        let amount = t.batch().column(0).as_primitive::<Float64Type>();
        assert_eq!(amount.value(0), 1234.5);
        let when = t
            .batch()
            .column(1)
            .as_primitive::<arrow_array::types::Date32Type>();
        assert_eq!(when.value(0), 20_516, "en-US: 4 March");
        let (t, _) = run(csv.as_bytes(), "en-GB").unwrap();
        let when = t
            .batch()
            .column(1)
            .as_primitive::<arrow_array::types::Date32Type>();
        assert_eq!(when.value(0), 20_546, "en-GB: 3 April");
        let (t, _) = run(b"Amount;When\n1.234,5;03.04.2026\n", "de-DE").unwrap();
        let amount = t.batch().column(0).as_primitive::<Float64Type>();
        assert_eq!(amount.value(0), 1234.5);
        // The same text under en-US is not a number.
        let (t, _) = run(b"Amount;When\n1.234,5;03.04.2026\n", "en-US").unwrap();
        assert_eq!(text(&t, 0), some(&["1.234,5"]));
    }

    #[test]
    fn bad_cells_import_with_an_error_count() {
        let mut csv = String::from("Qty,Price,Name\n");
        // Past the 200 detection rows, so the types stay Whole / Decimal.
        for i in 0..250 {
            csv.push_str(&format!("{},{i}.5,n{i}\n", i % 100));
        }
        csv.push_str("oops,1.5,a\n,abc,b\n7,??,c\n");
        let (t, i) = run(csv.as_bytes(), "en-US").unwrap();
        assert_eq!(t.rows(), 253);
        assert_eq!(i.cell_errors, 3);
        let qty = t.batch().column(0);
        assert_eq!(qty.null_count(), 2, "one error + one empty");
        let qty = qty.as_primitive::<Int8Type>();
        assert_eq!(qty.value(252), 7);
    }

    #[test]
    fn streams_many_batches() {
        let n = BATCH_ROWS * 2 + 17;
        let mut csv = String::from("Id,Group\n");
        for i in 0..n {
            csv.push_str(&format!("{i},g{}\n", i % 3));
        }
        let (t, i) = run(csv.as_bytes(), "en-US").unwrap();
        assert_eq!(t.rows(), n);
        assert_eq!(i.cell_errors, 0);
        let ids = t.batch().column(0).as_primitive::<Int16Type>();
        assert_eq!(ids.value(n - 1), i16::try_from(n - 1).unwrap());
        // The decoder hands over small reads too.
        let mut d = Decode::new(&b""[..], b"ab".to_vec(), UTF_8.new_decoder());
        let mut one = [0u8; 1];
        assert_eq!((d.read(&mut one).unwrap(), one[0]), (1, b'a'));
        assert_eq!((d.read(&mut one).unwrap(), one[0]), (1, b'b'));
        assert_eq!(d.read(&mut one).unwrap(), 0);
    }

    /// Counts the bytes handed out.
    struct Counting<'a> {
        inner: &'a [u8],
        read: std::rc::Rc<Cell<usize>>,
    }

    impl Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.read.set(self.read.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn reading_fewer_rows_stops_the_reader_early() {
        let row = format!("{},{}\n", "x".repeat(40), 7);
        let csv = row.repeat(10_000);
        let format = Format {
            delimiter: b',',
            encoding: UTF_8,
            columns: 2,
        };
        let read = std::rc::Rc::new(Cell::new(0));
        let counting = Counting {
            inner: csv.as_bytes(),
            read: std::rc::Rc::clone(&read),
        };
        let mut rows = Vec::new();
        super::read("T", counting, &format, 1000, &mut |b| {
            rows.push(b.num_rows());
            Ok(())
        })
        .unwrap();
        assert_eq!(rows, [1000], "one batch of exactly 1,000 rows");
        // 1,000 rows are 43 KB: the 64 KB prefix plus one more chunk at most, of 430 KB.
        assert!(
            read.get() <= 2 * CHUNK,
            "read {} of {} bytes",
            read.get(),
            csv.len()
        );
    }

    #[test]
    fn memory_cap_names_the_table() {
        let csv = "Name\n".to_string() + &"some text\n".repeat(BATCH_ROWS * 2);
        let r = connect(
            "Sales",
            csv.as_bytes(),
            Options::default(),
            culture("en-US"),
            4_096,
        );
        let e = r.err().unwrap();
        assert!(
            matches!(&e, Error::MemoryCap { table, .. } if table == "Sales"),
            "{e}"
        );
    }

    #[test]
    fn empty_and_hostile_input_is_an_error_not_a_panic() {
        assert!(matches!(run(b"", "en-US"), Err(Error::Source { .. })));
        assert!(matches!(run(b"\n\n", "en-US"), Err(Error::Source { .. })));
        let wide = ",".repeat(MAX_COLUMNS + 1);
        assert!(matches!(
            run(wide.as_bytes(), "en-US"),
            Err(Error::Source { .. })
        ));
        let format = Format {
            delimiter: b',',
            encoding: UTF_8,
            columns: 0,
        };
        assert!(super::read("T", &b"a"[..], &format, 1, &mut |_| Ok(())).is_err());
        // Invalid UTF-8 after the sniffed prefix is replaced, not fatal.
        let mut bytes = "a\n".repeat(CHUNK).into_bytes();
        bytes.extend_from_slice(b"\xFF\xFE\n");
        let (t, _) = run(&bytes, "en-US").unwrap();
        assert_eq!(t.rows(), CHUNK + 1);
    }

    #[test]
    fn a_huge_record_is_an_error_naming_its_line() {
        let opts = Options::default();
        // An endless line: refused after MAX_RECORD bytes, not read to the end of memory.
        let endless = b"a,b\n1,2\n".chain(io::repeat(b'x'));
        let e = sniff("T", endless, opts).unwrap_err();
        assert!(e.to_string().contains("line 3: a record is longer"), "{e}");
        // An endless quoted field past the sample.
        let mut head = "n\n".repeat(SAMPLE_ROWS + 5);
        head.push('"');
        let format = sniff("T", head.as_bytes().chain(io::repeat(b'y')), opts).unwrap();
        let endless = head.as_bytes().chain(io::repeat(b'y'));
        let e = super::read("T", endless, &format, usize::MAX, &mut |_| Ok(())).unwrap_err();
        assert!(e.to_string().contains("line 206"), "{e}");
    }

    #[test]
    fn the_sample_counts_against_the_limit() {
        let csv = format!("{}\n", "z".repeat(5000)).repeat(10);
        let e = connect(
            "Wide",
            csv.as_bytes(),
            Options::default(),
            culture("en-US"),
            4096,
        );
        assert!(matches!(e, Err(Error::MemoryCap { .. })), "{:?}", e.err());
    }

    /// Every byte × 4 flips and every cut, then random edits. Soak with `ODASH_FUZZ_SCALE` /
    /// `ODASH_FUZZ_SEED` (see `crate::fuzz`): the random edits get `scale` times more.
    #[test]
    fn damaged_bytes_never_panic() {
        let mut fuzz = crate::fuzz::Fuzz::new("csv-bytes", 0x9E37_79B9_7F4A_7C15);
        let good = "Name;Qty;When;Flag\n\"a;\"\"b\"\"\";1;2026-03-04;true\nc;-2,5;04/03/2026;\n\
                    \u{e9}t\u{e9};;;FALSE\n"
            .repeat(3)
            .into_bytes();
        for i in 0..good.len() {
            for flip in [0x01, 0x22, 0x80, 0xFF] {
                let mut v = good.clone();
                v[i] ^= flip;
                fuzz.case(&v, || drop(run(&v, "en-US")));
            }
            fuzz.case(&good[..i], || drop(run(&good[..i], "de-DE")));
        }
        for _ in 0..fuzz.scaled(500) {
            let mut v = good.clone();
            for _ in 0..fuzz.below(6) + 1 {
                let i = fuzz.below(v.len());
                v[i] = fuzz.next() as u8;
            }
            fuzz.case(&v, || drop(run(&v, "en-US")));
        }
        fuzz.finish();
    }

    /// Generated CSV across delimiters, quoting, line ends, encodings and cultures, then
    /// mutated (bytes deleted, inserted, swapped). 400 cases a run; soak with
    /// `ODASH_FUZZ_SCALE` / `ODASH_FUZZ_SEED` (see `crate::fuzz`).
    #[test]
    fn mutated_csv_never_panics() {
        let mut fuzz = crate::fuzz::Fuzz::new("csv-mutated", 0x2545_F491_4F6C_DD1D);
        let fields = [
            "",
            "x",
            "North",
            "1",
            "-2",
            "2,5",
            "1.234,5",
            "1,234.5",
            "1e308",
            "-0",
            "NaN",
            "9223372036854775808",
            "2026-03-04",
            "04/03/2026",
            "31.12.1999",
            "29/02/2001",
            "3/4/2026 1:00 PM",
            "2026-03-04T10:11:12.123",
            "true",
            "FALSE",
            "caf\u{e9}",
            "\u{1F600}",
            "\"q\"",
            "\"a,b;c\td|e\"",
            "\"multi\nline\"",
            "\"say \"\"hi\"\"\"",
            "\"",
            "\"\"\"",
            "#(lf)",
            "\u{0}",
        ];
        let specials = b",;\t|\"\r\n\x00\xFF\xFE\xEF\xBB\xBF\\ ";
        let cultures = ["en-US", "en-GB", "de-DE", "fr-FR"];
        for _ in 0..fuzz.scaled(400) {
            let delim = [",", ";", "\t", "|"][fuzz.below(4)];
            let eol = ["\n", "\r\n", "\r"][fuzz.below(3)];
            let width = fuzz.below(6) + 1;
            let mut text = String::new();
            for _ in 0..fuzz.below(12) {
                // Mostly rectangular, sometimes ragged.
                let w = if fuzz.below(5) == 0 {
                    fuzz.below(8)
                } else {
                    width
                };
                let row: Vec<&str> = (0..w).map(|_| fields[fuzz.below(fields.len())]).collect();
                text.push_str(&row.join(delim));
                text.push_str(eol);
            }
            let mut bytes = match fuzz.below(5) {
                0 => [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat(),
                1 | 2 => {
                    let be = fuzz.below(2) == 0;
                    let mut v = if be {
                        vec![0xFE, 0xFF]
                    } else {
                        vec![0xFF, 0xFE]
                    };
                    for u in text.encode_utf16() {
                        v.extend(if be { u.to_be_bytes() } else { u.to_le_bytes() });
                    }
                    v
                }
                3 => WINDOWS_1252.encode(&text).0.into_owned(),
                _ => text.into_bytes(),
            };
            for _ in 0..fuzz.below(5) {
                let at = fuzz.below(bytes.len() + 1);
                match fuzz.below(3) {
                    0 if at < bytes.len() => drop(bytes.remove(at)),
                    1 => bytes.insert(at, specials[fuzz.below(specials.len())]),
                    _ => {
                        let other = fuzz.below(bytes.len());
                        if at < bytes.len() && other < bytes.len() {
                            bytes.swap(at, other);
                        }
                    }
                }
            }
            let options = Options {
                delimiter: (fuzz.below(4) == 0).then(|| b",;\t|"[fuzz.below(4)]),
                encoding: (fuzz.below(4) == 0).then(|| {
                    [
                        UTF_8,
                        encoding_rs::UTF_16LE,
                        encoding_rs::UTF_16BE,
                        WINDOWS_1252,
                    ][fuzz.below(4)]
                }),
            };
            let culture = culture(cultures[fuzz.below(cultures.len())]);
            let limit = match fuzz.below(8) {
                0 => fuzz.below(4096),
                _ => usize::MAX,
            };
            fuzz.case(&bytes, || {
                drop(connect("T", &bytes, options, culture, limit))
            });
        }
        fuzz.finish();
    }
}
