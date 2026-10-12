//! XLSX Source: list a workbook's sheets and Excel tables (the navigator), import either one.
//!
//! Column types come from the cells: whole numbers → `Int64`, other numbers → `Float64`,
//! booleans → `Boolean`, dates → `Date32` (all date-only) else `Timestamp(µs)`, text → `Utf8`.
//! A column mixing kinds (number and text, date and number…) becomes `Utf8`, other kinds
//! rendered as text (numbers as Rust prints them, dates ISO 8601, booleans `true`/`false`).
//! Excel error cells (`#DIV/0!`, `#N/A`…) become blank and are counted (Cell errors).

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek};
use std::sync::Arc;

use arrow_array::builder::{
    BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use calamine::{Data, Reader, SheetType, Xlsx};

use super::{BATCH_ROWS, load};
use crate::{Error, Result, Table};

/// Excel's sheet limits; anything past them is a malformed workbook.
const MAX_ROWS: usize = 1 << 20;
const MAX_COLS: usize = 1 << 14;

const DAY_US: i64 = 86_400_000_000;

/// A loaded sheet or table, with the number of Excel error cells that became blank.
#[derive(Debug)]
pub struct Import {
    pub table: Table,
    pub cell_errors: usize,
}

/// An open workbook.
pub struct Workbook<R: Read + Seek> {
    xlsx: Xlsx<R>,
}

type Cells<'a> = Box<dyn Iterator<Item = Result<(u32, u32, Data)>> + 'a>;

impl<R: Read + Seek> Workbook<R> {
    pub fn open(reader: R) -> Result<Self> {
        Ok(Self {
            xlsx: Xlsx::new(reader).map_err(error)?,
        })
    }

    /// Worksheet names in workbook order (chart sheets left out).
    pub fn sheets(&self) -> Vec<String> {
        self.xlsx
            .sheets_metadata()
            .iter()
            .filter(|s| s.typ == SheetType::WorkSheet)
            .map(|s| s.name.clone())
            .collect()
    }

    /// Excel table names.
    pub fn tables(&mut self) -> Result<Vec<String>> {
        self.xlsx.load_tables().map_err(error)?;
        Ok(self.xlsx.table_names().into_iter().cloned().collect())
    }

    /// Imports a worksheet: its first used row is the headers, the used columns are the columns.
    /// Streams the sheet XML twice (types, then values), so peak memory is the Table plus one
    /// batch — the sheet is never held whole.
    pub fn load_sheet(&mut self, name: &str, limit: usize) -> Result<Import> {
        let is_1904 = self.xlsx.has_1904_epoch();
        let scan = scan(self.sheet_cells(name)?, true, is_1904)?;
        let (Some((top, bottom)), Some((left, right))) = (scan.rows, scan.cols) else {
            // An empty sheet is one blank column with no rows (Arrow needs a column).
            let (headers, kinds) = (vec![String::new()], vec![Kind::Empty]);
            let cells = std::iter::empty();
            return import(name, headers, kinds, 0, 0, 0, cells, 0, is_1904, limit);
        };
        let width = to_usize(right.saturating_sub(left)).saturating_add(1);
        let rows = to_usize(bottom.saturating_sub(top));
        if width > MAX_COLS || rows >= MAX_ROWS {
            return Err(source(format!(
                "sheet `{name}` is larger than Excel allows"
            )));
        }
        let headers = (left..=right)
            .map(|c| scan.headers.get(&c).cloned().unwrap_or_default())
            .collect();
        let kinds = (left..=right)
            .map(|c| scan.kinds.get(&c).copied().unwrap_or(Kind::Empty))
            .collect();
        let cells = self.sheet_cells(name)?;
        import(
            name,
            headers,
            kinds,
            top.saturating_add(1),
            left,
            rows,
            cells,
            scan.errors,
            is_1904,
            limit,
        )
    }

    /// Imports an Excel table: its header row names the columns.
    // ponytail: calamine materialises the table's whole sheet as a `Range` (the peak here, on
    // top of the Table), and trusts the table's `ref` (a hostile one can make calamine panic or
    // over-allocate inside `Range::new`). Read the table XML ourselves and stream
    // the sheet with `worksheet_cells_reader`, as `load_sheet` does, if that peak matters.
    pub fn load_table(&mut self, name: &str, limit: usize) -> Result<Import> {
        let is_1904 = self.xlsx.has_1904_epoch();
        self.xlsx.load_tables().map_err(error)?;
        let table = self.xlsx.table_by_name(name).map_err(error)?;
        let data = table.data();
        let width = data.width().max(table.columns().len());
        let rows = data.height();
        let scan = scan(range_cells(data), false, is_1904)?;
        let headers = (0..width)
            .map(|i| table.columns().get(i).cloned().unwrap_or_default())
            .collect();
        let kinds = (0..width)
            .map(|c| {
                u32::try_from(c)
                    .ok()
                    .and_then(|c| scan.kinds.get(&c).copied())
                    .unwrap_or(Kind::Empty)
            })
            .collect();
        import(
            name,
            headers,
            kinds,
            0,
            0,
            rows,
            range_cells(data),
            scan.errors,
            is_1904,
            limit,
        )
    }

    fn sheet_cells(&mut self, name: &str) -> Result<Cells<'_>> {
        let mut reader = self.xlsx.worksheet_cells_reader(name).map_err(error)?;
        Ok(Box::new(std::iter::from_fn(move || {
            match reader.next_cell() {
                Ok(Some(cell)) => {
                    let (r, c) = cell.get_position();
                    Some(Ok((r, c, Data::from(cell.get_value().clone()))))
                }
                Ok(None) => None,
                Err(e) => Some(Err(error(e))),
            }
        })))
    }
}

#[allow(clippy::too_many_arguments)]
fn import<'a>(
    name: &str,
    headers: Vec<String>,
    kinds: Vec<Kind>,
    top: u32,
    left: u32,
    rows: usize,
    cells: impl Iterator<Item = Result<(u32, u32, Data)>> + 'a,
    cell_errors: usize,
    is_1904: bool,
    limit: usize,
) -> Result<Import> {
    let fields: Vec<Field> = unique_names(headers)
        .into_iter()
        .zip(&kinds)
        .map(|(n, k)| Field::new(n, k.data_type(), true))
        .collect();
    let schema = Arc::new(Schema::new(fields));
    let batches = Batches {
        cells,
        pending: None,
        schema: Arc::clone(&schema),
        kinds,
        top,
        left,
        rows,
        done: 0,
        is_1904,
    };
    Ok(Import {
        table: load(name, schema, batches, limit)?,
        cell_errors,
    })
}

/// What a column holds, widened as cells are seen.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Kind {
    Empty,
    Int,
    Float,
    Bool,
    Date,
    DateTime,
    Text,
}

impl Kind {
    /// A cell's kind; `None` for an error cell (or a date outside Excel's range).
    fn of(d: &Data, is_1904: bool) -> Option<Kind> {
        Some(match d {
            Data::Empty => Kind::Empty,
            Data::Error(_) => return None,
            Data::Int(_) => Kind::Int,
            Data::Float(f) if whole(*f).is_some() => Kind::Int,
            Data::Float(_) => Kind::Float,
            Data::Bool(_) => Kind::Bool,
            Data::DateTime(t) if t.is_duration() => Kind::Float,
            Data::DateTime(_) => match micros(d, is_1904) {
                None => return None,
                Some(us) if us % DAY_US == 0 => Kind::Date,
                Some(_) => Kind::DateTime,
            },
            Data::String(_) | Data::DateTimeIso(_) | Data::DurationIso(_) => Kind::Text,
        })
    }

    fn merge(self, other: Kind) -> Kind {
        use Kind::*;
        match (self, other) {
            (a, b) if a == b => a,
            (Empty, k) | (k, Empty) => k,
            (Int, Float) | (Float, Int) => Float,
            (Date, DateTime) | (DateTime, Date) => DateTime,
            _ => Text,
        }
    }

    fn data_type(self) -> DataType {
        match self {
            Kind::Empty | Kind::Text => DataType::Utf8,
            Kind::Int => DataType::Int64,
            Kind::Float => DataType::Float64,
            Kind::Bool => DataType::Boolean,
            Kind::Date => DataType::Date32,
            Kind::DateTime => DataType::Timestamp(TimeUnit::Microsecond, None),
        }
    }
}

#[derive(Default)]
struct Scan {
    rows: Option<(u32, u32)>,
    cols: Option<(u32, u32)>,
    headers: BTreeMap<u32, String>,
    kinds: BTreeMap<u32, Kind>,
    errors: usize,
}

/// First pass: bounds, headers (the first used row, if `header`), column kinds, error count.
fn scan(cells: Cells<'_>, header: bool, is_1904: bool) -> Result<Scan> {
    let mut s = Scan::default();
    for cell in cells {
        let (r, c, d) = cell?;
        if d == Data::Empty {
            continue;
        }
        let header_row = match s.rows {
            Some((_, bottom)) if r < bottom => {
                return Err(source(format!(
                    "rows out of order ({} after {})",
                    r + 1,
                    bottom + 1
                )));
            }
            Some((top, _)) => {
                s.rows = Some((top, r));
                header && r == top
            }
            None => {
                s.rows = Some((r, r));
                header
            }
        };
        s.cols = Some(s.cols.map_or((c, c), |(l, h)| (l.min(c), h.max(c))));
        if header_row {
            s.headers.insert(c, text(&d, is_1904).unwrap_or_default());
            continue;
        }
        match Kind::of(&d, is_1904) {
            Some(k) => {
                let e = s.kinds.entry(c).or_insert(Kind::Empty);
                *e = e.merge(k);
            }
            None => s.errors += 1,
        }
    }
    Ok(s)
}

/// Second pass: converts cells to Arrow, [`BATCH_ROWS`] rows at a time.
struct Batches<I> {
    cells: I,
    pending: Option<(u32, u32, Data)>,
    schema: SchemaRef,
    kinds: Vec<Kind>,
    /// Sheet row and column of the first data cell.
    top: u32,
    left: u32,
    rows: usize,
    done: usize,
    is_1904: bool,
}

impl<I: Iterator<Item = Result<(u32, u32, Data)>>> Iterator for Batches<I> {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Result<RecordBatch>> {
        if self.done >= self.rows {
            return None;
        }
        let n = (self.rows - self.done).min(BATCH_ROWS);
        let mut cols: Vec<Col> = self.kinds.iter().map(|k| Col::new(*k, n)).collect();
        loop {
            let (r, c, d) = match self.pending.take() {
                Some(cell) => cell,
                None => match self.cells.next() {
                    Some(Ok(cell)) => cell,
                    Some(Err(e)) => {
                        self.done = self.rows;
                        return Some(Err(e));
                    }
                    None => break,
                },
            };
            let Some(row) = r.checked_sub(self.top).map(to_usize) else {
                continue; // the header row
            };
            if row >= self.done + n {
                self.pending = Some((r, c, d));
                break;
            }
            let col = c
                .checked_sub(self.left)
                .and_then(|c| cols.get_mut(to_usize(c)));
            if let (Some(col), Some(row)) = (col, row.checked_sub(self.done)) {
                col.put(row, &d, self.is_1904);
            }
        }
        self.done += n;
        let arrays = cols.into_iter().map(|c| c.finish(n)).collect();
        Some(
            RecordBatch::try_new(Arc::clone(&self.schema), arrays)
                .map_err(|e| source(e.to_string())),
        )
    }
}

/// One column of the batch being built.
enum Col {
    Text(StringBuilder),
    Int(Int64Builder),
    Float(Float64Builder),
    Bool(BooleanBuilder),
    Date(Date32Builder),
    DateTime(TimestampMicrosecondBuilder),
}

impl Col {
    fn new(kind: Kind, n: usize) -> Col {
        match kind {
            Kind::Empty | Kind::Text => Col::Text(StringBuilder::with_capacity(n, n * 8)),
            Kind::Int => Col::Int(Int64Builder::with_capacity(n)),
            Kind::Float => Col::Float(Float64Builder::with_capacity(n)),
            Kind::Bool => Col::Bool(BooleanBuilder::with_capacity(n)),
            Kind::Date => Col::Date(Date32Builder::with_capacity(n)),
            Kind::DateTime => Col::DateTime(TimestampMicrosecondBuilder::with_capacity(n)),
        }
    }

    fn len(&self) -> usize {
        use arrow_array::builder::ArrayBuilder;
        match self {
            Col::Text(b) => b.len(),
            Col::Int(b) => b.len(),
            Col::Float(b) => b.len(),
            Col::Bool(b) => b.len(),
            Col::Date(b) => b.len(),
            Col::DateTime(b) => b.len(),
        }
    }

    fn pad(&mut self, to: usize) {
        let n = to.saturating_sub(self.len());
        match self {
            Col::Text(b) => (0..n).for_each(|_| b.append_null()),
            Col::Int(b) => b.append_nulls(n),
            Col::Float(b) => b.append_nulls(n),
            Col::Bool(b) => b.append_nulls(n),
            Col::Date(b) => b.append_nulls(n),
            Col::DateTime(b) => b.append_nulls(n),
        }
    }

    /// Writes `d` at `row` (blank-filling the gap); a second cell for the same row is ignored.
    fn put(&mut self, row: usize, d: &Data, is_1904: bool) {
        self.pad(row);
        if self.len() != row {
            return;
        }
        match self {
            Col::Text(b) => b.append_option(text(d, is_1904)),
            Col::Int(b) => b.append_option(match d {
                Data::Int(i) => Some(*i),
                Data::Float(f) => whole(*f),
                _ => None,
            }),
            Col::Float(b) => b.append_option(match d {
                Data::Int(i) => Some(*i as f64),
                Data::Float(f) => Some(*f),
                Data::DateTime(t) if t.is_duration() => Some(t.as_f64()),
                _ => None,
            }),
            Col::Bool(b) => b.append_option(match d {
                Data::Bool(v) => Some(*v),
                _ => None,
            }),
            Col::Date(b) => {
                b.append_option(micros(d, is_1904).and_then(|us| i32::try_from(us / DAY_US).ok()))
            }
            Col::DateTime(b) => b.append_option(micros(d, is_1904)),
        }
    }

    fn finish(mut self, n: usize) -> ArrayRef {
        self.pad(n);
        match self {
            Col::Text(mut b) => Arc::new(b.finish()),
            Col::Int(mut b) => Arc::new(b.finish()),
            Col::Float(mut b) => Arc::new(b.finish()),
            Col::Bool(mut b) => Arc::new(b.finish()),
            Col::Date(mut b) => Arc::new(b.finish()),
            Col::DateTime(mut b) => Arc::new(b.finish()),
        }
    }
}

/// `f` as an `i64` when it is a whole number in range.
fn whole(f: f64) -> Option<i64> {
    // 2^63 is exact in f64; `as` saturates, the bounds keep it exact.
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    (f.is_finite() && f.fract() == 0.0 && (-LIMIT..LIMIT).contains(&f)).then_some(f as i64)
}

/// A date cell as microseconds since 1970-01-01, `None` if not a date or outside
/// Excel's 0..=9999-12-31 range.
///
/// 1900 system: serial 1 is 1900-01-01, and Excel counts a 1900-02-29 that never existed
/// (serial 60), so serials below 60 are shifted a day to land where Excel shows them; serial
/// 60 itself comes out as 1900-02-28. 1904 system: serial 0 is 1904-01-01.
fn micros(d: &Data, is_1904: bool) -> Option<i64> {
    let Data::DateTime(t) = d else { return None };
    if !t.is_datetime() {
        return None;
    }
    let serial = t.as_f64();
    if !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let ms = (serial * 86_400_000.0).round() as i64;
    let mut days = ms.div_euclid(86_400_000);
    let ms = ms.rem_euclid(86_400_000);
    let epoch = if is_1904 {
        -24_107
    } else {
        if days < 60 {
            days += 1;
        }
        -25_569
    };
    Some((days + epoch) * DAY_US + ms * 1000)
}

/// A cell as text for a `Utf8` column; `None` for blanks and errors.
fn text(d: &Data, is_1904: bool) -> Option<String> {
    match d {
        Data::Empty | Data::Error(_) => None,
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => Some(s.clone()),
        Data::Int(i) => Some(i.to_string()),
        Data::Float(f) => Some(f.to_string()),
        Data::Bool(b) => Some(b.to_string()),
        Data::DateTime(t) if t.is_duration() => Some(t.as_f64().to_string()),
        Data::DateTime(_) => micros(d, is_1904).map(|us| {
            let (y, m, day) = civil(us.div_euclid(DAY_US));
            let s = us.rem_euclid(DAY_US) / 1_000_000;
            if s == 0 {
                format!("{y:04}-{m:02}-{day:02}")
            } else {
                format!(
                    "{y:04}-{m:02}-{day:02} {:02}:{:02}:{:02}",
                    s / 3600,
                    s / 60 % 60,
                    s % 60
                )
            }
        }),
    }
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// Power Query's header promotion: a blank header becomes `Column<n>` (1-based position), a
/// repeat gets `_1`, `_2`… appended.
// Uncertain until observed in the parity VM: Power Query may trim or number differently.
fn unique_names(raw: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    raw.into_iter()
        .enumerate()
        .map(|(i, name)| {
            let base = if name.is_empty() {
                format!("Column{}", i + 1)
            } else {
                name
            };
            let mut name = base.clone();
            let mut n = 1;
            while !seen.insert(name.clone()) {
                name = format!("{base}_{n}");
                n += 1;
            }
            name
        })
        .collect()
}

fn range_cells(data: &calamine::Range<Data>) -> Cells<'_> {
    Box::new(
        data.cells()
            .map(|(r, c, d)| match (u32::try_from(r), u32::try_from(c)) {
                (Ok(r), Ok(c)) => Ok((r, c, d.clone())),
                _ => Err(source("table is larger than Excel allows".into())),
            }),
    )
}

fn to_usize(n: u32) -> usize {
    usize::try_from(n).unwrap_or(usize::MAX)
}

fn source(message: String) -> Error {
    Error::Source {
        source: "XLSX".into(),
        message,
    }
}

fn error(e: calamine::XlsxError) -> Error {
    source(e.to_string())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use arrow_array::cast::AsArray;
    use arrow_array::types::{Int8Type, Int16Type, Int32Type, Int64Type};
    use arrow_array::{Array, BooleanArray, Date32Array, Float64Array, TimestampMicrosecondArray};
    use calamine::{ExcelDateTime, ExcelDateTimeType};
    use rust_xlsxwriter::{ExcelDateTime as XDate, Format, Formula, Workbook as Writer, Worksheet};

    use super::*;

    fn open(book: &mut Writer) -> Workbook<Cursor<Vec<u8>>> {
        Workbook::open(Cursor::new(book.save_to_buffer().unwrap())).unwrap()
    }

    /// A workbook with one sheet, "Data", holding `rows` as text (empty strings left blank).
    fn sheet(rows: &[&[&str]]) -> Writer {
        let mut book = Writer::new();
        let ws = book.add_worksheet().set_name("Data").unwrap();
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                if !v.is_empty() {
                    ws.write_string(r as u32, c as u16, *v).unwrap();
                }
            }
        }
        book
    }

    fn names(t: &Table) -> Vec<String> {
        t.batch()
            .schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    fn strings(t: &Table, i: usize) -> Vec<Option<String>> {
        let col = t.batch().column(i);
        let dict = col.as_any_dictionary();
        let values = dict.values().as_string::<i32>();
        let keys = dict.normalized_keys();
        (0..col.len())
            .map(|r| col.is_valid(r).then(|| values.value(keys[r]).to_string()))
            .collect()
    }

    fn ints(t: &Table, i: usize) -> Vec<Option<i64>> {
        let col = t.batch().column(i);
        match col.data_type() {
            DataType::Int8 => col
                .as_primitive::<Int8Type>()
                .iter()
                .map(|v| v.map(i64::from))
                .collect(),
            DataType::Int16 => col
                .as_primitive::<Int16Type>()
                .iter()
                .map(|v| v.map(i64::from))
                .collect(),
            DataType::Int32 => col
                .as_primitive::<Int32Type>()
                .iter()
                .map(|v| v.map(i64::from))
                .collect(),
            _ => col.as_primitive::<Int64Type>().iter().collect(),
        }
    }

    fn date(ws: &mut Worksheet, r: u32, c: u16, d: &XDate, fmt: &str) {
        ws.write_datetime_with_format(r, c, d, &Format::new().set_num_format(fmt))
            .unwrap();
    }

    #[test]
    fn sheet_import_types_columns() {
        let mut book = Writer::new();
        let ws = book.add_worksheet().set_name("Sales").unwrap();
        for (c, h) in ["Region", "Qty", "Amount", "Paid", "Day", "At"]
            .iter()
            .enumerate()
        {
            ws.write_string(0, c as u16, *h).unwrap();
        }
        ws.write_string(1, 0, "North").unwrap();
        ws.write_number(1, 1, 3).unwrap();
        ws.write_number(1, 2, 1.5).unwrap();
        ws.write_boolean(1, 3, true).unwrap();
        date(
            ws,
            1,
            4,
            &XDate::from_ymd(2024, 1, 31).unwrap(),
            "yyyy-mm-dd",
        );
        let at = XDate::from_ymd(2024, 2, 29)
            .unwrap()
            .and_hms(13, 45, 30)
            .unwrap();
        date(ws, 1, 5, &at, "yyyy-mm-dd hh:mm:ss");
        ws.write_string(2, 0, "South").unwrap();
        ws.write_number(2, 1, -2).unwrap();
        ws.write_number(2, 2, 4).unwrap();
        ws.write_boolean(2, 3, false).unwrap();
        // Row 3's Day and At left blank.
        book.add_worksheet().set_name("Other").unwrap();
        let mut wb = open(&mut book);
        assert_eq!(wb.sheets(), ["Sales", "Other"]);

        let Import {
            table: t,
            cell_errors,
        } = wb.load_sheet("Sales", usize::MAX).unwrap();
        assert_eq!(cell_errors, 0);
        assert_eq!(t.name(), "Sales");
        assert_eq!(t.rows(), 2);
        assert_eq!(names(&t), ["Region", "Qty", "Amount", "Paid", "Day", "At"]);
        assert_eq!(strings(&t, 0), [Some("North".into()), Some("South".into())]);
        assert_eq!(ints(&t, 1), [Some(3), Some(-2)]);
        let b = t.batch();
        let amount = b.column(2).as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(amount.values().to_vec(), [1.5, 4.0]);
        let paid = b.column(3).as_any().downcast_ref::<BooleanArray>().unwrap();
        assert_eq!(paid.iter().collect::<Vec<_>>(), [Some(true), Some(false)]);
        let day = b.column(4).as_any().downcast_ref::<Date32Array>().unwrap();
        assert_eq!(day.iter().collect::<Vec<_>>(), [Some(19_753), None]); // 2024-01-31
        let at = b
            .column(5)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        let want = (19_782 * 86_400 + 13 * 3600 + 45 * 60 + 30) * 1_000_000; // 2024-02-29 13:45:30
        assert_eq!(at.value(0), want);
        assert!(at.is_null(1));
    }

    #[test]
    fn table_import_uses_its_header_row() {
        let mut book = Writer::new();
        let ws = book.add_worksheet().set_name("Report").unwrap();
        ws.write_string(0, 0, "Title above the table").unwrap();
        ws.write_string(3, 2, "Pen").unwrap();
        ws.write_number(3, 3, 10).unwrap();
        ws.write_string(4, 2, "Ink").unwrap();
        ws.write_number(4, 3, 2.5).unwrap();
        let cols = [
            rust_xlsxwriter::TableColumn::new().set_header("Item"),
            rust_xlsxwriter::TableColumn::new().set_header("Units"),
        ];
        let table = rust_xlsxwriter::Table::new()
            .set_name("Stock")
            .set_columns(&cols);
        ws.add_table(2, 2, 4, 3, &table).unwrap();
        let mut wb = open(&mut book);
        assert_eq!(wb.tables().unwrap(), ["Stock"]);

        let t = wb.load_table("Stock", usize::MAX).unwrap().table;
        assert_eq!(t.name(), "Stock");
        assert_eq!(names(&t), ["Item", "Units"]);
        assert_eq!(strings(&t, 0), [Some("Pen".into()), Some("Ink".into())]);
        let units = t
            .batch()
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(units.values().to_vec(), [10.0, 2.5]);
    }

    #[test]
    fn error_cells_become_blank_and_are_counted() {
        let mut book = sheet(&[&["Qty"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        ws.write_number(1, 0, 7).unwrap();
        ws.write_formula(2, 0, Formula::new("=1/0").set_result("#DIV/0!"))
            .unwrap();
        ws.write_formula(3, 0, Formula::new("=NA()").set_result("#N/A"))
            .unwrap();
        let Import { table, cell_errors } = open(&mut book).load_sheet("Data", usize::MAX).unwrap();
        assert_eq!(cell_errors, 2);
        assert_eq!(ints(&table, 0), [Some(7), None, None]);
    }

    #[test]
    fn mixed_column_is_text() {
        let mut book = sheet(&[&["Mixed"], &["a"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        ws.write_number(2, 0, 1).unwrap();
        ws.write_number(3, 0, 2.5).unwrap();
        ws.write_boolean(4, 0, true).unwrap();
        date(
            ws,
            5,
            0,
            &XDate::from_ymd(2024, 1, 31).unwrap(),
            "yyyy-mm-dd",
        );
        let t = open(&mut book)
            .load_sheet("Data", usize::MAX)
            .unwrap()
            .table;
        assert_eq!(
            strings(&t, 0),
            ["a", "1", "2.5", "true", "2024-01-31"].map(|s| Some(s.to_string()))
        );
    }

    #[test]
    fn blank_and_repeated_headers_are_made_unique() {
        let mut book = sheet(&[&["Name", "", "Name", "Name_1"], &["a", "b", "c", "d"]]);
        let t = open(&mut book)
            .load_sheet("Data", usize::MAX)
            .unwrap()
            .table;
        assert_eq!(names(&t), ["Name", "Column2", "Name_1", "Name_1_1"]);
        assert_eq!(
            unique_names(vec!["A".into(), "A".into(), "A".into(), String::new()]),
            ["A", "A_1", "A_2", "Column4"]
        );
    }

    #[test]
    fn streams_more_than_one_batch() {
        let n = BATCH_ROWS * 2 + 5;
        let mut book = sheet(&[&["N"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        for i in 0..n as u32 {
            if i != 9000 {
                ws.write_number(i + 1, 0, i).unwrap();
            }
        }
        let t = open(&mut book)
            .load_sheet("Data", usize::MAX)
            .unwrap()
            .table;
        assert_eq!(t.rows(), n);
        let v = ints(&t, 0);
        assert_eq!(v[8191], Some(8191));
        assert_eq!(v[9000], None, "the empty row stays a blank row");
        assert_eq!(v[n - 1], Some(n as i64 - 1));
    }

    #[test]
    fn header_only_and_empty_sheets_load() {
        let t = open(&mut sheet(&[&["A", "B"]]))
            .load_sheet("Data", usize::MAX)
            .unwrap()
            .table;
        assert_eq!((t.rows(), names(&t)), (0, vec!["A".into(), "B".into()]));
        let t = open(&mut sheet(&[]))
            .load_sheet("Data", usize::MAX)
            .unwrap()
            .table;
        assert_eq!((t.rows(), names(&t)), (0, vec!["Column1".into()]));
    }

    #[test]
    fn malformed_workbook_is_an_error() {
        for bytes in [
            Vec::new(),
            b"not a workbook".to_vec(),
            vec![0x50, 0x4b, 3, 4, 0xff],
        ] {
            let err = Workbook::open(Cursor::new(bytes)).err().unwrap();
            assert!(matches!(err, Error::Source { .. }), "{err:?}");
        }
    }

    #[test]
    fn missing_sheet_or_table_is_an_error() {
        let mut wb = open(&mut sheet(&[&["A"]]));
        for err in [
            wb.load_sheet("Nope", usize::MAX).unwrap_err(),
            wb.load_table("Nope", usize::MAX).unwrap_err(),
        ] {
            assert!(matches!(err, Error::Source { .. }), "{err:?}");
            assert!(err.to_string().contains("Nope"), "{err}");
        }
    }

    #[test]
    fn memory_cap_stops_the_import() {
        let mut book = sheet(&[&["N"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        for i in 1..=BATCH_ROWS as u32 * 3 {
            ws.write_number(i, 0, i).unwrap();
        }
        let err = open(&mut book).load_sheet("Data", 1_000).unwrap_err();
        assert!(matches!(err, Error::MemoryCap { .. }), "{err:?}");
    }

    #[test]
    fn excel_serials_to_dates() {
        let ymd = |serial: f64, is_1904| {
            let d = Data::DateTime(ExcelDateTime::new(
                serial,
                ExcelDateTimeType::DateTime,
                is_1904,
            ));
            micros(&d, is_1904).map(|us| {
                let (y, m, d) = civil(us.div_euclid(DAY_US));
                (y, m, d, us.rem_euclid(DAY_US) / 1_000_000)
            })
        };
        assert_eq!(ymd(1.0, false), Some((1900, 1, 1, 0)));
        assert_eq!(ymd(59.0, false), Some((1900, 2, 28, 0)));
        assert_eq!(
            ymd(60.0, false),
            Some((1900, 2, 28, 0)),
            "Excel's 1900-02-29"
        );
        assert_eq!(ymd(61.0, false), Some((1900, 3, 1, 0)));
        assert_eq!(ymd(45_943.5, false), Some((2025, 10, 13, 12 * 3600)));
        assert_eq!(ymd(0.0, true), Some((1904, 1, 1, 0)));
        assert_eq!(ymd(44_481.0, true), Some((2025, 10, 13, 0)));
        assert_eq!(ymd(-1.0, false), None);
        assert_eq!(ymd(3e6, false), None);
        assert_eq!(ymd(f64::NAN, false), None);
        let duration = Data::DateTime(ExcelDateTime::new(1.5, ExcelDateTimeType::TimeDelta, false));
        assert_eq!(Kind::of(&duration, false), Some(Kind::Float));
    }
}
