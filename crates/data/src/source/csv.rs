//! CSV Source (spec S-1): reads like Power Query's `Csv.Document` (columns `Column1..N`, all
//! text), then applies the automatic *Promoted Headers* and *Changed Type* steps (ADR 0005),
//! streaming in [`BATCH_ROWS`] batches so the whole file is never held.
//!
//! Parity uncertainties, to settle with M1.10 cases in the VM:
//! - Header detection: promoted when every cell of the first row is non-empty text and at least
//!   one column below it detects as non-text. An all-text file keeps `Column1..N`. Duplicate
//!   headers get `_1`, `_2` suffixes.
//! - The column count is the widest row in the first 201 rows (Power Query writes it into
//!   `Csv.Document` as `Columns=N`); later wider rows lose the extra fields, shorter rows get
//!   nulls.
//! - Blank lines are skipped. Quoted fields may contain line breaks (`QuoteStyle.Csv`).
//! - Without a BOM, valid UTF-8 in the first 64 KB means UTF-8, otherwise Windows-1252.

use std::collections::{HashSet, VecDeque};
use std::io::{self, Read};
use std::sync::Arc;

use ::csv::{ReaderBuilder, StringRecord};
use arrow_array::builder::StringBuilder;
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{Field, Schema};
use encoding_rs::{CoderResult, Decoder, Encoding, UTF_8, WINDOWS_1252};

use super::{BATCH_ROWS, load};
use crate::culture::Culture;
use crate::step::{ColumnType, Step};
use crate::{Error, Result, Table};

/// Rows used for type detection.
const SAMPLE_ROWS: usize = 200;
/// Power Query's column limit.
const MAX_COLUMNS: usize = 16_384;
/// Bytes read at a time; the first chunk also decides encoding and delimiter.
const CHUNK: usize = 64 * 1024;
/// Candidate delimiters, in order of preference.
const DELIMITERS: [u8; 4] = *b",;\t|";

/// How to read the file; `None` means detect.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options {
    pub delimiter: Option<u8>,
    pub encoding: Option<&'static Encoding>,
}

/// What an import found and did.
#[derive(Clone, Debug, PartialEq)]
pub struct Import {
    pub delimiter: u8,
    pub encoding: &'static Encoding,
    /// The automatic steps: *Promoted Headers* (when detected), then *Changed Type*.
    pub steps: Vec<Step>,
    /// Cell errors per column: values Changed Type couldn't convert, loaded as blank.
    pub cell_errors: Vec<usize>,
}

impl Import {
    pub fn cell_error_count(&self) -> usize {
        self.cell_errors.iter().sum()
    }
}

/// Imports a CSV file as table `name`, using at most `limit` bytes of memory.
pub fn import(
    name: &str,
    mut reader: impl Read,
    options: Options,
    culture: &Culture,
    limit: usize,
) -> Result<(Table, Import)> {
    let err = |message: String| Error::Source {
        source: name.into(),
        message,
    };
    let mut prefix = Vec::with_capacity(CHUNK);
    reader
        .by_ref()
        .take(CHUNK as u64)
        .read_to_end(&mut prefix)
        .map_err(|e| err(e.to_string()))?;
    let encoding = match Encoding::for_bom(&prefix) {
        Some((bom, _)) => bom,
        None => options.encoding.unwrap_or_else(|| guess(&prefix)),
    };
    let delimiter = options
        .delimiter
        .unwrap_or_else(|| sniff(&encoding.decode(&prefix).0));
    let mut csv = ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .delimiter(delimiter)
        .from_reader(Decode::new(reader, prefix, encoding.new_decoder()));

    // Header row + SAMPLE_ROWS.
    let mut sample = VecDeque::with_capacity(SAMPLE_ROWS + 1);
    while sample.len() <= SAMPLE_ROWS {
        let mut r = StringRecord::new();
        if !csv.read_record(&mut r).map_err(|e| err(e.to_string()))? {
            break;
        }
        sample.push_back(r);
    }
    let width = sample.iter().map(StringRecord::len).max().unwrap_or(0);
    if width == 0 {
        return Err(err("the file has no data".into()));
    }
    if width > MAX_COLUMNS {
        return Err(err(format!("{width} columns; the limit is {MAX_COLUMNS}")));
    }
    let detect = |skip: usize, c: usize| {
        let values = sample.iter().skip(skip).take(SAMPLE_ROWS);
        ColumnType::detect(values.map(|r| r.get(c).unwrap_or("")), culture)
    };
    let below: Vec<ColumnType> = (0..width).map(|c| detect(1, c)).collect();
    let header = sample.front().filter(|first| {
        sample.len() > 1
            && (0..width).all(|c| {
                first
                    .get(c)
                    .is_some_and(|v| ColumnType::of(v, culture) == Some(ColumnType::Text))
            })
            && below.iter().any(|t| *t != ColumnType::Text)
    });
    let mut steps = Vec::with_capacity(2);
    let (names, types) = match header {
        Some(first) => {
            let names = unique_names(first);
            steps.push(Step::PromotedHeaders);
            sample.pop_front();
            (names, below)
        }
        None => (
            (1..=width).map(|i| format!("Column{i}")).collect(),
            (0..width).map(|c| detect(0, c)).collect(),
        ),
    };
    steps.push(Step::ChangedType(
        names.iter().cloned().zip(types.iter().copied()).collect(),
    ));
    let schema = Arc::new(Schema::new(
        names
            .iter()
            .zip(&types)
            .map(|(n, t)| Field::new(n, t.data_type(), true))
            .collect::<Vec<_>>(),
    ));

    let mut cell_errors = vec![0; width];
    let mut record = StringRecord::new();
    let mut done = false;
    let batches = std::iter::from_fn(|| {
        if done {
            return None;
        }
        let mut text: Vec<StringBuilder> = (0..width).map(|_| StringBuilder::new()).collect();
        let mut rows = 0;
        while rows < BATCH_ROWS {
            if let Some(r) = sample.pop_front() {
                record = r;
            } else {
                match csv.read_record(&mut record) {
                    Ok(true) => {}
                    Ok(false) => {
                        done = true;
                        break;
                    }
                    Err(e) => {
                        done = true;
                        return Some(Err(err(e.to_string())));
                    }
                }
            }
            for (c, col) in text.iter_mut().enumerate() {
                col.append_option(record.get(c));
            }
            rows += 1;
        }
        if rows == 0 {
            return None;
        }
        let columns: Vec<ArrayRef> = text
            .iter_mut()
            .zip(&types)
            .zip(cell_errors.iter_mut())
            .map(|((col, ty), errors)| {
                let (array, n) = ty.convert(&col.finish(), culture);
                *errors += n;
                array
            })
            .collect();
        Some(RecordBatch::try_new(Arc::clone(&schema), columns).map_err(|e| err(e.to_string())))
    });
    let table = load(name, Arc::clone(&schema), batches, limit)?;
    Ok((
        table,
        Import {
            delimiter,
            encoding,
            steps,
            cell_errors,
        },
    ))
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
fn sniff(text: &str) -> u8 {
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

/// Header cells as column names, made unique with `_1`, `_2`… suffixes.
fn unique_names(row: &StringRecord) -> Vec<String> {
    let mut seen = HashSet::with_capacity(row.len());
    row.iter()
        .map(|base| {
            let mut name = base.to_string();
            let mut n = 0;
            while seen.contains(&name) {
                n += 1;
                name = format!("{base}_{n}");
            }
            seen.insert(name.clone());
            name
        })
        .collect()
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
    use arrow_schema::DataType;

    use super::*;

    fn culture(name: &str) -> &'static Culture {
        Culture::get(name).unwrap()
    }

    fn run(bytes: &[u8], name: &str) -> Result<(Table, Import)> {
        import("T", bytes, Options::default(), culture(name), usize::MAX)
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
        let (t, _) = import("T", &b"a,b|c\n"[..], opts, culture("en-US"), usize::MAX).unwrap();
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
        assert_eq!(i.cell_error_count(), 0);
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
        assert_eq!(i.cell_errors, [1]);
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
        assert_eq!(i.cell_errors, [1, 2, 0]);
        assert_eq!(i.cell_error_count(), 3);
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
        assert_eq!(i.cell_error_count(), 0);
        let ids = t.batch().column(0).as_primitive::<Int16Type>();
        assert_eq!(ids.value(n - 1), i16::try_from(n - 1).unwrap());
        // The decoder hands over small reads too.
        let mut d = Decode::new(&b""[..], b"ab".to_vec(), UTF_8.new_decoder());
        let mut one = [0u8; 1];
        assert_eq!((d.read(&mut one).unwrap(), one[0]), (1, b'a'));
        assert_eq!((d.read(&mut one).unwrap(), one[0]), (1, b'b'));
        assert_eq!(d.read(&mut one).unwrap(), 0);
    }

    #[test]
    fn memory_cap_names_the_table() {
        let csv = "Name\n".to_string() + &"some text\n".repeat(BATCH_ROWS * 2);
        let r = import(
            "Sales",
            csv.as_bytes(),
            Options::default(),
            culture("en-US"),
            4_096,
        );
        let e = r.unwrap_err();
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
        // Invalid UTF-8 after the sniffed prefix is replaced, not fatal.
        let mut bytes = "a\n".repeat(CHUNK).into_bytes();
        bytes.extend_from_slice(b"\xFF\xFE\n");
        let (t, _) = run(&bytes, "en-US").unwrap();
        assert_eq!(t.rows(), CHUNK + 1);
    }
}
