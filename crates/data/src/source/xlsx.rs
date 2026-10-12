//! XLSX Source: list a workbook's sheets and Excel tables (the navigator), import either one.
//! A Query reads a sheet raw ([`Workbook::read_sheet`]: first row kept as data, *Promoted
//! Headers* is its step); [`Workbook::load_sheet`] promotes it at import.
//!
//! Column types come from the cells: whole numbers → `Int64`, other numbers → `Float64`,
//! booleans → `Boolean`, dates → `Date32` (all date-only) else `Timestamp(µs)`, text → `Utf8`.
//! A column mixing kinds (number and text, date and number…) becomes `Utf8`, other kinds
//! rendered as text (numbers as Rust prints them, dates ISO 8601, booleans `true`/`false`).
//! Excel error cells (`#DIV/0!`, `#N/A`…) become blank and are counted (Cell errors).
//!
//! **Hostile files** (spec N-1, N-4). [`Workbook::open`] reads every XML part calamine will
//! parse (relationships, workbook, shared strings, styles, each sheet) once itself first, with
//! quick-xml through a metered reader: no single XML event (tag or text run) may exceed
//! [`MAX_EVENT`] bytes, and the parts calamine keeps in memory (shared strings, styles…) may
//! not exceed the memory limit, so a decompression bomb can't fill memory. A shared-strings
//! `uniqueCount` far above the strings present is refused (calamine reserves it up front), and
//! password-protected (OLE) files are refused before calamine's OLE parser sees them. Excel
//! tables are read from their own parts here, not through calamine (whose table loader trusts
//! the `ref` and paths): a `ref` outside Excel's limits, inverted, or with odd header/total
//! row counts is an error for that table only, and the table's cells stream like a sheet's.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::sync::Arc;

use arrow_array::builder::{
    BooleanBuilder, Date32Builder, Float64Builder, Int64Builder, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use calamine::{Data, Reader, SheetType, Xlsx};
use quick_xml::events::{BytesStart, Event};
use zip::ZipArchive;
use zip::result::ZipError;

use super::{BATCH_BYTES, BATCH_ROWS, Sink, collect, emit};
use crate::{Error, Result, Table};

/// Excel's sheet limits; anything past them is a malformed workbook.
const MAX_ROWS: usize = 1 << 20;
const MAX_COLS: usize = 1 << 14;
/// Most cells (rows × columns) one batch holds.
const BATCH_CELLS: usize = 1 << 20;

/// Largest XML event (one tag, or one run of text) accepted. Excel caps a cell at 32,767
/// characters, so real events stay far below this; a bigger one is a decompression bomb.
pub const MAX_EVENT: u64 = 4 << 20;
/// An OLE compound file: how Excel stores a password-protected workbook.
const OLE: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

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
    tables: Vec<TablePart>,
}

/// An Excel table, read from its own part (`xl/tables/tableN.xml`).
struct TablePart {
    name: String,
    sheet: String,
    columns: Vec<String>,
    /// Where its data rows are, or why its `ref` was refused.
    area: std::result::Result<Area, String>,
}

/// A table's data rows: zero-based first row and column, row count and width.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    top: u32,
    left: u32,
    rows: usize,
    width: u32,
}

type Cells<'a> = Box<dyn Iterator<Item = Result<(u32, u32, Data)>> + 'a>;

impl<R: Read + Seek> Workbook<R> {
    /// Opens a workbook, checking all of it first (see the module docs), with `limit` bytes
    /// for the parts held in memory.
    pub fn open(mut reader: R, limit: usize) -> Result<Self> {
        let mut magic = [0; 8];
        let mut n = 0;
        while n < magic.len() {
            match reader.read(magic.get_mut(n..).unwrap_or_default()) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(source(e.to_string())),
            }
        }
        if magic == OLE {
            return Err(source(
                "the workbook is password-protected (or not an XLSX file)".into(),
            ));
        }
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|e| source(e.to_string()))?;
        let mut zip = ZipArchive::new(reader).map_err(|e| source(e.to_string()))?;
        let tables = check_package(&mut zip, limit as u64)?;
        let mut reader = zip.into_inner();
        reader
            .seek(SeekFrom::Start(0))
            .map_err(|e| source(e.to_string()))?;
        Ok(Self {
            xlsx: Xlsx::new(reader).map_err(error)?,
            tables,
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
    pub fn tables(&self) -> Vec<String> {
        self.tables.iter().map(|t| t.name.clone()).collect()
    }

    /// Imports a worksheet: its first used row is the headers, the used columns are the columns.
    /// Streams the sheet XML twice (types, then values), so peak memory is the Table plus one
    /// batch — the sheet is never held whole.
    pub fn load_sheet(&mut self, name: &str, limit: usize) -> Result<Import> {
        let (table, cell_errors) = collect(name, limit, |sink| {
            self.sheet(name, true, usize::MAX, '.', limit, sink)
        })?;
        Ok(Import { table, cell_errors })
    }

    /// Reads a worksheet raw, as Power Query's `Excel.Workbook` gives it: `Column1..N` over the
    /// used columns, the first used row kept as data; at most `rows` rows (both passes stop
    /// there) into `sink`. Numbers in a text column are written with the `decimal` separator so
    /// the Culture reads them back. Returns the error cells, read as blank.
    pub fn read_sheet(
        &mut self,
        name: &str,
        rows: usize,
        decimal: char,
        limit: usize,
        sink: &mut Sink<'_>,
    ) -> Result<usize> {
        self.sheet(name, false, rows, decimal, limit, sink)
    }

    /// A sheet with (`header`) or without its first used row as the column names.
    fn sheet(
        &mut self,
        name: &str,
        header: bool,
        max: usize,
        decimal: char,
        limit: usize,
        sink: &mut Sink<'_>,
    ) -> Result<usize> {
        let is_1904 = self.xlsx.has_1904_epoch();
        let scan = scan(self.sheet_cells(name)?, header, is_1904, max)?;
        let (Some((top, bottom)), Some((left, right))) = (scan.rows, scan.cols) else {
            // An empty sheet is one blank column with no rows (Arrow needs a column).
            let (headers, kinds) = (vec![String::new()], vec![Kind::Empty]);
            let cells = std::iter::empty();
            let at = Place {
                top: 0,
                left: 0,
                rows: 0,
            };
            return import(
                name, headers, kinds, at, cells, 0, is_1904, decimal, limit, sink,
            );
        };
        let width = to_usize(right.saturating_sub(left)).saturating_add(1);
        let rows = to_usize(bottom.saturating_sub(top)).saturating_add(usize::from(!header));
        if width > MAX_COLS || rows > MAX_ROWS {
            return Err(source(format!(
                "sheet `{name}` is larger than Excel allows"
            )));
        }
        // Blank names become `Column1..N`.
        let headers = (left..=right)
            .map(|c| scan.headers.get(&c).cloned().unwrap_or_default())
            .collect();
        let kinds = (left..=right)
            .map(|c| scan.kinds.get(&c).copied().unwrap_or(Kind::Empty))
            .collect();
        let cells = self.sheet_cells(name)?;
        let at = Place {
            top: top.saturating_add(u32::from(header)),
            left,
            rows: rows.min(max),
        };
        import(
            name,
            headers,
            kinds,
            at,
            cells,
            scan.errors,
            is_1904,
            decimal,
            limit,
            sink,
        )
    }

    /// Imports an Excel table: its header row names the columns. Its cells stream from its
    /// sheet like [`Workbook::load_sheet`]'s.
    pub fn load_table(&mut self, name: &str, limit: usize) -> Result<Import> {
        let (table, cell_errors) = collect(name, limit, |sink| {
            self.read_table(name, usize::MAX, '.', limit, sink)
        })?;
        Ok(Import { table, cell_errors })
    }

    /// Reads an Excel table as Power Query gives it (its header row names the columns): at most
    /// `rows` rows into `sink`, `decimal` as in [`Workbook::read_sheet`]. Returns the error
    /// cells, read as blank.
    pub fn read_table(
        &mut self,
        name: &str,
        rows: usize,
        decimal: char,
        limit: usize,
        sink: &mut Sink<'_>,
    ) -> Result<usize> {
        let is_1904 = self.xlsx.has_1904_epoch();
        let table = self
            .tables
            .iter()
            .find(|t| t.name == name)
            .or_else(|| {
                self.tables
                    .iter()
                    .find(|t| t.name.eq_ignore_ascii_case(name))
            })
            .ok_or_else(|| source(format!("no table `{name}`")))?;
        let mut area = table
            .area
            .clone()
            .map_err(|m| source(format!("table `{name}`: {m}")))?;
        area.rows = area.rows.min(rows);
        let sheet = table.sheet.clone();
        let headers = (0..to_usize(area.width))
            .map(|i| table.columns.get(i).cloned().unwrap_or_default())
            .collect();
        let scan = scan(self.area_cells(&sheet, area)?, false, is_1904, usize::MAX)?;
        let kinds = (0..area.width)
            .map(|i| {
                let c = area.left.saturating_add(i);
                scan.kinds.get(&c).copied().unwrap_or(Kind::Empty)
            })
            .collect();
        let cells = self.area_cells(&sheet, area)?;
        let at = Place {
            top: area.top,
            left: area.left,
            rows: area.rows,
        };
        import(
            name,
            headers,
            kinds,
            at,
            cells,
            scan.errors,
            is_1904,
            decimal,
            limit,
            sink,
        )
    }

    fn sheet_cells(&mut self, name: &str) -> Result<Cells<'_>> {
        let mut reader = self.xlsx.worksheet_cells_reader(name).map_err(error)?;
        Ok(Box::new(
            std::iter::from_fn(move || match reader.next_cell() {
                Ok(Some(cell)) => {
                    let (r, c) = cell.get_position();
                    Some(Ok((r, c, Data::from(cell.get_value().clone()))))
                }
                Ok(None) => None,
                Err(e) => Some(Err(error(e))),
            })
            // A table's rows can outlast the cells; calamine errors if asked again after the end.
            .fuse(),
        ))
    }

    /// The cells of `sheet` inside `area`; reading stops after its last row.
    fn area_cells(&mut self, sheet: &str, area: Area) -> Result<Cells<'_>> {
        let rows = u64::from(area.top)..u64::from(area.top) + area.rows as u64;
        let cols = u64::from(area.left)..u64::from(area.left) + u64::from(area.width);
        let end = rows.end;
        let cells = self.sheet_cells(sheet)?;
        Ok(Box::new(
            cells
                .take_while(move |cell| cell.as_ref().map_or(true, |(r, ..)| u64::from(*r) < end))
                .filter(move |cell| match cell {
                    Ok((r, c, _)) => rows.contains(&u64::from(*r)) && cols.contains(&u64::from(*c)),
                    Err(_) => true,
                }),
        ))
    }
}

/// Zip part names by lower-cased path, the way calamine resolves them.
struct Parts(HashMap<String, String>);

impl Parts {
    fn new<R: Read + Seek>(zip: &ZipArchive<R>) -> Self {
        Self(
            zip.file_names()
                .map(|n| (n.replace('\\', "/").to_ascii_lowercase(), n.to_string()))
                .collect(),
        )
    }

    fn name<'a>(&'a self, path: &'a str) -> &'a str {
        self.0
            .get(&path.to_ascii_lowercase())
            .map_or(path, String::as_str)
    }
}

/// A zip part fed to quick-xml, failing once one XML event exceeds [`MAX_EVENT`] bytes or the
/// part exceeds `cap`.
struct Metered<R> {
    inner: BufReader<R>,
    event: u64,
    total: u64,
    cap: u64,
}

impl<R> Metered<R> {
    fn over(&self) -> bool {
        self.event > MAX_EVENT || self.total > self.cap
    }
}

impl<R: Read> Read for Metered<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let data = self.fill_buf()?;
        let n = data.len().min(buf.len());
        if let (Some(dst), Some(src)) = (buf.get_mut(..n), data.get(..n)) {
            dst.copy_from_slice(src);
        }
        self.consume(n);
        Ok(n)
    }
}

impl<R: Read> BufRead for Metered<R> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.over() {
            return Err(io::Error::other("XML part too large"));
        }
        self.inner.fill_buf()
    }

    fn consume(&mut self, n: usize) {
        self.event = self.event.saturating_add(n as u64);
        self.total = self.total.saturating_add(n as u64);
        self.inner.consume(n);
    }
}

type Xml<'a, R> = quick_xml::Reader<Metered<zip::read::ZipFile<'a, R>>>;

/// Opens a part as calamine would (`None` if missing), metered.
fn part<'a, R: Read + Seek>(
    zip: &'a mut ZipArchive<R>,
    parts: &Parts,
    path: &str,
    cap: u64,
) -> Result<Option<Xml<'a, R>>> {
    match zip.by_name(parts.name(path)) {
        Ok(file) => {
            let mut xml = quick_xml::Reader::from_reader(Metered {
                inner: BufReader::new(file),
                event: 0,
                total: 0,
                cap,
            });
            // calamine's settings, so events split exactly as they will for calamine.
            let config = xml.config_mut();
            config.check_end_names = false;
            config.trim_text(false);
            config.check_comments = false;
            config.expand_empty_elements = true;
            Ok(Some(xml))
        }
        Err(ZipError::FileNotFound) => Ok(None),
        Err(e) => Err(source(format!("`{path}`: {e}"))),
    }
}

/// Reads every event of a part, calling `f` with each start tag. A part too large is an
/// error; an XML syntax error just ends the walk (calamine stops at the same place).
fn walk<R: Read + Seek>(
    xml: Option<Xml<'_, R>>,
    path: &str,
    mut f: impl FnMut(&BytesStart<'_>, quick_xml::encoding::Decoder),
) -> Result<()> {
    let Some(mut xml) = xml else {
        return Ok(());
    };
    let mut buf = Vec::new();
    loop {
        buf.clear();
        xml.get_mut().event = 0;
        let event = xml.read_event_into(&mut buf);
        // Checked after each event too: one can end exactly as it crosses the budget.
        if xml.get_ref().over() {
            return Err(source(format!(
                "`{path}` is too large (an XML element over {} MB, or the part over the memory \
                 limit)",
                MAX_EVENT >> 20
            )));
        }
        match event {
            Ok(Event::Start(e) | Event::Empty(e)) => f(&e, xml.decoder()),
            Ok(Event::Eof) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
    }
}

/// An attribute's unescaped value; `local` matches any namespace prefix.
fn attr(
    e: &BytesStart<'_>,
    decoder: quick_xml::encoding::Decoder,
    key: &[u8],
    local: bool,
) -> Option<String> {
    let a = e.attributes().flatten().find(|a| {
        if local {
            a.key.local_name().as_ref() == key
        } else {
            a.key.as_ref() == key
        }
    })?;
    let value = decoder.decode(&a.value).ok()?;
    quick_xml::escape::unescape(&value)
        .ok()
        .map(|v| v.into_owned())
}

/// Checks the package before calamine reads it, and reads its Excel tables.
fn check_package<R: Read + Seek>(zip: &mut ZipArchive<R>, limit: u64) -> Result<Vec<TablePart>> {
    let parts = Parts::new(zip);
    let is = |e: &BytesStart<'_>, name: &[u8]| e.local_name().as_ref() == name;

    // The workbook's folder, as calamine finds it.
    let mut target = None;
    let rels = "_rels/.rels";
    walk(part(zip, &parts, rels, limit)?, rels, |e, d| {
        let office = attr(e, d, b"Type", false)
            .is_some_and(|t| t.ends_with("/relationships/officeDocument"));
        if is(e, b"Relationship") && office {
            target = attr(e, d, b"Target", false).or(target.take());
        }
    })?;
    let folder = match &target {
        Some(t) => t.rfind('/').map_or("", |i| t.get(..=i).unwrap_or_default()),
        None => "xl/",
    };
    let folder = folder.strip_prefix('/').unwrap_or(folder).to_string();

    // The parts calamine holds in memory: each within the limit. calamine reserves the
    // shared strings' `uniqueCount` before reading them.
    let strings = format!("{folder}sharedStrings.xml");
    let (mut unique, mut count) = (0u128, 0u128);
    walk(part(zip, &parts, &strings, limit)?, &strings, |e, _| {
        if is(e, b"sst") {
            let n = e.try_get_attribute(b"uniqueCount").ok().flatten();
            let digits = n.as_ref().map(|a| a.value.as_ref()).unwrap_or_default();
            if !digits.is_empty() && digits.iter().all(u8::is_ascii_digit) {
                let n = digits.iter().fold(0u128, |n, d| {
                    n.saturating_mul(10).saturating_add(u128::from(d - b'0'))
                });
                unique = unique.max(n);
            }
        } else if is(e, b"si") {
            count += 1;
        }
    })?;
    if unique > (count * 2).max(4096) {
        return Err(source(format!(
            "the shared strings claim {unique} entries but hold {count}"
        )));
    }
    let styles = format!("{folder}styles.xml");
    walk(part(zip, &parts, &styles, limit)?, &styles, |_, _| {})?;
    let mut targets = HashMap::new();
    let workbook_rels = format!("{folder}_rels/workbook.xml.rels");
    walk(
        part(zip, &parts, &workbook_rels, limit)?,
        &workbook_rels,
        |e, d| {
            if let (true, Some(id)) = (is(e, b"Relationship"), attr(e, d, b"Id", false)) {
                targets.insert(id, attr(e, d, b"Target", false).unwrap_or_default());
            }
        },
    )?;
    let mut sheets = Vec::new();
    let workbook = format!("{folder}workbook.xml");
    walk(part(zip, &parts, &workbook, limit)?, &workbook, |e, d| {
        if is(e, b"sheet") {
            let name = attr(e, d, b"name", false).unwrap_or_default();
            let target = attr(e, d, b"id", true).and_then(|id| targets.get(&id));
            if let Some(t) = target {
                let path = match t.strip_prefix('/') {
                    Some(absolute) => absolute.to_string(),
                    None => format!("{folder}{t}"),
                };
                sheets.push((name, path));
            }
        }
    })?;

    // Every sheet (streamed, so only its events are bounded), and its tables.
    let mut tables = Vec::new();
    for (sheet, path) in &sheets {
        walk(part(zip, &parts, path, u64::MAX)?, path, |_, _| {})?;
        let (dir, file) = path.rsplit_once('/').unwrap_or(("", path));
        let rels = if dir.is_empty() {
            format!("_rels/{file}.rels")
        } else {
            format!("{dir}/_rels/{file}.rels")
        };
        let mut found = Vec::new();
        walk(part(zip, &parts, &rels, limit)?, &rels, |e, d| {
            let table = attr(e, d, b"Type", false).is_some_and(|t| t.ends_with("/table"));
            if let (true, true, Some(t)) =
                (is(e, b"Relationship"), table, attr(e, d, b"Target", false))
            {
                found.push(resolve(dir, &t));
            }
        })?;
        for table in found {
            let mut t = TablePart {
                name: String::new(),
                sheet: sheet.clone(),
                columns: Vec::new(),
                area: Err("no `ref`".into()),
            };
            walk(part(zip, &parts, &table, limit)?, &table, |e, d| {
                if is(e, b"table") {
                    let get = |k: &[u8]| attr(e, d, k, false);
                    t.name = get(b"displayName")
                        .or_else(|| get(b"name"))
                        .unwrap_or_default();
                    t.area = get(b"ref")
                        .ok_or_else(|| "no `ref`".into())
                        .and_then(|r| area(&r, get(b"headerRowCount"), get(b"totalsRowCount")));
                } else if is(e, b"tableColumn") && t.columns.len() <= MAX_COLS {
                    t.columns
                        .push(attr(e, d, b"name", false).unwrap_or_default());
                }
            })?;
            if !t.name.is_empty() {
                tables.push(t);
            }
        }
    }
    Ok(tables)
}

/// A relationship target relative to the folder `dir` (or absolute, from `/`).
fn resolve(dir: &str, target: &str) -> String {
    let mut path: Vec<&str> = match target.strip_prefix('/') {
        Some(_) => Vec::new(),
        None => dir.split('/').filter(|s| !s.is_empty()).collect(),
    };
    for segment in target.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                path.pop();
            }
            s => path.push(s),
        }
    }
    path.join("/")
}

/// A table's data area from its `ref` (`A1:D20`) and header/total row counts (0 or 1).
fn area(
    range: &str,
    header: Option<String>,
    totals: Option<String>,
) -> std::result::Result<Area, String> {
    let bad = || format!("range `{range}` is not a valid Excel range");
    let (a, b) = range.split_once(':').unwrap_or((range, range));
    let ((r1, c1), (r2, c2)) = (cell(a).ok_or_else(bad)?, cell(b).ok_or_else(bad)?);
    if r1 > r2 || c1 > c2 {
        return Err(format!("range `{range}` is inverted"));
    }
    let count = |v: Option<String>, default: u32| match v.as_deref() {
        None => Some(default),
        Some("0") => Some(0),
        Some("1") => Some(1),
        Some(_) => None,
    };
    let header = count(header, 1).ok_or("header row count is not 0 or 1")?;
    let totals = count(totals, 0).ok_or("totals row count is not 0 or 1")?;
    let top = r1 + header;
    let end = (r2 + 1).saturating_sub(totals);
    Ok(Area {
        top,
        left: c1,
        rows: to_usize(end.saturating_sub(top)),
        width: c2 - c1 + 1,
    })
}

/// A cell reference (`B7`) as zero-based (row, column), within Excel's limits.
fn cell(s: &str) -> Option<(u32, u32)> {
    let split = s.find(|c: char| c.is_ascii_digit())?;
    let (letters, digits) = s.split_at(split);
    if !(1..=3).contains(&letters.len())
        || !(1..=7).contains(&digits.len())
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut col = 0u32;
    for b in letters.bytes() {
        let b = b.to_ascii_uppercase();
        if !b.is_ascii_uppercase() {
            return None;
        }
        col = col * 26 + u32::from(b - b'A') + 1;
    }
    let row: u32 = digits.parse().ok()?;
    let ok = (1..=MAX_COLS as u32).contains(&col) && (1..=MAX_ROWS as u32).contains(&row);
    ok.then(|| (row - 1, col - 1))
}

/// Where the data cells are: the sheet row and column of the first one, and the row count.
struct Place {
    top: u32,
    left: u32,
    rows: usize,
}

/// Streams the cells at `at` as batches into `sink`; returns `cell_errors`.
#[allow(clippy::too_many_arguments)]
fn import<'a>(
    name: &str,
    headers: Vec<String>,
    kinds: Vec<Kind>,
    at: Place,
    cells: impl Iterator<Item = Result<(u32, u32, Data)>> + 'a,
    cell_errors: usize,
    is_1904: bool,
    decimal: char,
    limit: usize,
    sink: &mut Sink<'_>,
) -> Result<usize> {
    let Place { top, left, rows } = at;
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
        decimal,
        name,
        limit,
    };
    emit(schema, batches, usize::MAX, sink)?;
    Ok(cell_errors)
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
/// Stops after `max` rows below the headers.
fn scan(cells: Cells<'_>, header: bool, is_1904: bool, max: usize) -> Result<Scan> {
    let mut s = Scan::default();
    let max = u64::try_from(max).unwrap_or(u64::MAX);
    for cell in cells {
        let (r, c, d) = cell?;
        if let Some((top, _)) = s.rows {
            let end = u64::from(top)
                .saturating_add(u64::from(header))
                .saturating_add(max);
            if u64::from(r) >= end {
                break;
            }
        }
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
            s.headers
                .insert(c, text(&d, is_1904, '.').unwrap_or_default());
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

/// Second pass: converts cells to Arrow, [`BATCH_ROWS`] rows at a time (fewer once a batch
/// holds [`BATCH_BYTES`]).
struct Batches<'n, I> {
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
    /// The decimal separator numbers in text columns are written with.
    decimal: char,
    name: &'n str,
    limit: usize,
}

impl<I: Iterator<Item = Result<(u32, u32, Data)>>> Iterator for Batches<'_, I> {
    type Item = Result<RecordBatch>;

    fn next(&mut self) -> Option<Result<RecordBatch>> {
        if self.done >= self.rows {
            return None;
        }
        // At most BATCH_CELLS cells a batch: a 16,384-column area gets 64-row batches, so
        // the builders allocated up front stay small whatever the area.
        let rows = (BATCH_CELLS / self.kinds.len().max(1)).clamp(1, BATCH_ROWS);
        let mut n = (self.rows - self.done).min(rows);
        let mut cols: Vec<Col> = self.kinds.iter().map(|k| Col::new(*k, n)).collect();
        let (mut bytes, mut last) = (0usize, 0usize);
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
            let Some(row) = row.checked_sub(self.done) else {
                continue;
            };
            // Past the batch, or past its byte budget at a row boundary: next batch.
            if row >= n || (bytes >= BATCH_BYTES && row > last) {
                n = n.min(row);
                self.pending = Some((r, c, d));
                break;
            }
            let col = c
                .checked_sub(self.left)
                .and_then(|c| cols.get_mut(to_usize(c)));
            if let Some(col) = col {
                bytes = bytes.saturating_add(col.put(row, &d, self.is_1904, self.decimal));
                last = row;
            }
            // One row alone can't outgrow the memory limit (a shared string repeated).
            if bytes > self.limit {
                self.done = self.rows;
                return Some(Err(Error::MemoryCap {
                    table: self.name.into(),
                    limit: self.limit,
                }));
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
            Kind::Empty | Kind::Text => Col::Text(StringBuilder::with_capacity(n, 0)),
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

    /// Writes `d` at `row` (blank-filling the gap), returning about how many bytes that took;
    /// a second cell for the same row is ignored.
    fn put(&mut self, row: usize, d: &Data, is_1904: bool, decimal: char) -> usize {
        self.pad(row);
        if self.len() != row {
            return 0;
        }
        match self {
            Col::Text(b) => {
                let t = text(d, is_1904, decimal);
                let n = t.as_ref().map_or(0, String::len);
                b.append_option(t);
                return n + 8;
            }
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
        8
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

/// A cell as text for a `Utf8` column; `None` for blanks and errors. Fractions use `decimal`.
fn text(d: &Data, is_1904: bool, decimal: char) -> Option<String> {
    let number = |f: f64| f.to_string().replace('.', decimal.encode_utf8(&mut [0; 4]));
    match d {
        Data::Empty | Data::Error(_) => None,
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => Some(s.clone()),
        Data::Int(i) => Some(i.to_string()),
        Data::Float(f) => Some(number(*f)),
        Data::Bool(b) => Some(b.to_string()),
        Data::DateTime(t) if t.is_duration() => Some(number(t.as_f64())),
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
pub(crate) fn civil(days: i64) -> (i64, i64, i64) {
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
        Workbook::open(Cursor::new(book.save_to_buffer().unwrap()), usize::MAX).unwrap()
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
        assert_eq!(wb.tables(), ["Stock"]);

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
            let err = Workbook::open(Cursor::new(bytes), usize::MAX)
                .err()
                .unwrap();
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

    // Hostile input (M1.11): everything below must return, never panic.

    /// A workbook with shared strings, numbers, dates, booleans, an error cell and a table.
    fn sample() -> Vec<u8> {
        let mut book = sheet(&[&["Item", "Note"], &["Pen", "blue"], &["Ink", ""]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        ws.write_string(0, 2, "Units").unwrap();
        ws.write_number(1, 2, 10).unwrap();
        ws.write_number(2, 2, 2.5).unwrap();
        ws.write_boolean(1, 3, true).unwrap();
        date(
            ws,
            2,
            3,
            &XDate::from_ymd(2024, 1, 31).unwrap(),
            "yyyy-mm-dd",
        );
        ws.write_formula(3, 0, Formula::new("=1/0").set_result("#DIV/0!"))
            .unwrap();
        let cols = [
            rust_xlsxwriter::TableColumn::new().set_header("Item"),
            rust_xlsxwriter::TableColumn::new().set_header("Note"),
        ];
        let table = rust_xlsxwriter::Table::new()
            .set_name("Stock")
            .set_columns(&cols);
        ws.add_table(0, 0, 2, 1, &table).unwrap();
        book.save_to_buffer().unwrap()
    }

    fn unzip(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut zip = ZipArchive::new(Cursor::new(bytes)).unwrap();
        (0..zip.len())
            .map(|i| {
                let mut f = zip.by_index(i).unwrap();
                let mut data = Vec::new();
                f.read_to_end(&mut data).unwrap();
                (f.name().to_string(), data)
            })
            .collect()
    }

    fn rezip(parts: &[(String, Vec<u8>)]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let stored = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (name, data) in parts {
            zip.start_file(name.as_str(), stored).unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// `bytes` with part `name` edited by `f`.
    fn edit(bytes: &[u8], name: &str, f: impl Fn(&str) -> String) -> Vec<u8> {
        let mut parts = unzip(bytes);
        let part = parts.iter_mut().find(|(n, _)| n == name).unwrap();
        part.1 = f(std::str::from_utf8(&part.1).unwrap()).into_bytes();
        rezip(&parts)
    }

    /// Opens and loads everything; returns whether it all worked. Must never panic.
    fn load_all(bytes: Vec<u8>, limit: usize) -> bool {
        let Ok(mut wb) = Workbook::open(Cursor::new(bytes), limit) else {
            return false;
        };
        let mut ok = true;
        for s in wb.sheets() {
            ok &= wb.load_sheet(&s, limit).is_ok();
        }
        for t in wb.tables() {
            ok &= wb.load_table(&t, limit).is_ok();
        }
        ok
    }

    #[test]
    fn damaged_parts_never_panic() {
        let good = sample();
        assert!(load_all(good.clone(), usize::MAX));
        let parts = unzip(&good);
        assert!(load_all(rezip(&parts), usize::MAX), "rezipped sample loads");
        // Bit flips and cuts in every XML part (sampled to keep the debug run quick), then
        // flips in the zip container itself.
        for (p, (name, data)) in parts.iter().enumerate() {
            let step = (data.len() / 150).max(1);
            for i in (0..data.len()).step_by(step) {
                for flip in [0x01, 0x20, 0x80] {
                    let mut parts = parts.clone();
                    parts[p].1[i] ^= flip;
                    load_all(rezip(&parts), usize::MAX);
                }
            }
            for cut in [
                0,
                data.len() / 3,
                data.len() / 2,
                data.len().saturating_sub(1),
            ] {
                let mut parts = parts.clone();
                parts[p].1.truncate(cut);
                load_all(rezip(&parts), usize::MAX);
            }
            let _ = name;
        }
        for i in (0..good.len()).step_by(7) {
            let mut v = good.clone();
            v[i] ^= 0x10;
            load_all(v, usize::MAX);
        }
    }

    #[test]
    fn hostile_table_refs_are_errors() {
        let good = sample();
        let table = unzip(&good)
            .into_iter()
            .find(|(n, _)| n.starts_with("xl/tables/"))
            .unwrap()
            .0;
        for (from, to) in [
            ("ref=\"A1:B3\"", "ref=\"B3:A1\""),
            ("ref=\"A1:B3\"", "ref=\"A3:B1\""),
            ("ref=\"A1:B3\"", "ref=\"A1:XFE3\""),
            ("ref=\"A1:B3\"", "ref=\"A1:B1048577\""),
            ("ref=\"A1:B3\"", "ref=\"A0:B3\""),
            ("ref=\"A1:B3\"", "ref=\"AAAAAAAAAAAA1:B3\""),
            ("ref=\"A1:B3\"", "ref=\"A1:B99999999999999\""),
            ("ref=\"A1:B3\"", "ref=\"A1:B2:C3\""),
            ("ref=\"A1:B3\"", "ref=\"\""),
            ("ref=\"A1:B3\"", "ref=\"$A$1:$B$3\""),
            ("ref=\"A1:B3\"", ""),
            ("<table ", "<table headerRowCount=\"7\" "),
            ("<table ", "<table totalsRowCount=\"-1\" "),
        ] {
            let bytes = edit(&good, &table, |x| x.replacen(from, to, 1));
            let mut wb = Workbook::open(Cursor::new(bytes), usize::MAX).unwrap();
            assert_eq!(wb.tables(), ["Stock"], "{to}");
            let err = wb.load_table("Stock", usize::MAX).unwrap_err();
            assert!(matches!(err, Error::Source { .. }), "{to}: {err:?}");
            assert!(wb.load_sheet("Data", usize::MAX).is_ok(), "{to}");
        }
        // A header-only table is fine.
        let bytes = edit(&good, &table, |x| {
            x.replacen("ref=\"A1:B3\"", "ref=\"A1:B1\"", 1)
        });
        let mut wb = Workbook::open(Cursor::new(bytes), usize::MAX).unwrap();
        assert_eq!(wb.load_table("Stock", usize::MAX).unwrap().table.rows(), 0);
        // The whole sheet as a table is valid Excel, 16,384 × 1,048,575 mostly blank cells:
        // small batches, stopped by the memory limit rather than allocating it all.
        let bytes = edit(&good, &table, |x| {
            x.replacen("ref=\"A1:B3\"", "ref=\"A1:XFD1048576\"", 1)
        });
        let mut wb = Workbook::open(Cursor::new(bytes), usize::MAX).unwrap();
        let err = wb.load_table("Stock", 64 << 20).unwrap_err();
        assert!(matches!(err, Error::MemoryCap { .. }), "{err:?}");
    }

    #[test]
    fn hostile_cells_never_panic() {
        let good = sample();
        let sheet = "xl/worksheets/sheet1.xml";
        for (from, to) in [
            ("<dimension ref=\"A1:D4\"/>", "<dimension ref=\"D4:A1\"/>"),
            (
                "<dimension ref=\"A1:D4\"/>",
                "<dimension ref=\"A1:ZZZZZZZZ99999999999\"/>",
            ),
            ("r=\"A2\"", "r=\"AAAAAAAAAAAAAAA2\""),
            ("r=\"B2\"", "r=\"B99999999999999\""),
            ("r=\"C3\"", "r=\"C4294967296\""),
            ("<row r=\"2\"", "<row r=\"4294967295\""),
            ("<row r=\"3\"", "<row r=\"0\""),
            ("t=\"s\"", "t=\"s\" s=\"99999999\""),
            ("<v>0</v>", "<v>4294967295</v>"),
            ("<v>10</v>", "<v>1e308</v>"),
        ] {
            let bytes = edit(&good, sheet, |x| x.replacen(from, to, 1));
            load_all(bytes, usize::MAX);
        }
    }

    #[test]
    fn bombs_and_encrypted_files_are_refused() {
        let good = sample();
        let sheet = "xl/worksheets/sheet1.xml";
        // One cell of 5 MB of text (a few KB once deflated, in a real bomb).
        let huge = "x".repeat(MAX_EVENT as usize + 1);
        let bytes = edit(&good, sheet, |x| {
            x.replacen(
                "<sheetData>",
                &format!("<sheetData><row r=\"9\"><c r=\"A9\" t=\"inlineStr\"><is><t>{huge}</t></is></c></row>"),
                1,
            )
        });
        let err = Workbook::open(Cursor::new(bytes), usize::MAX)
            .err()
            .unwrap();
        assert!(err.to_string().contains("too large"), "{err}");
        // A shared-strings count calamine would reserve up front.
        let bytes = edit(&good, "xl/sharedStrings.xml", |x| {
            x.replacen("uniqueCount=\"", "uniqueCount=\"4000000000", 1)
        });
        assert!(Workbook::open(Cursor::new(bytes), usize::MAX).is_err());
        // Shared strings past the memory limit.
        assert!(Workbook::open(Cursor::new(good.clone()), 64).is_err());
        // An encrypted (OLE) file.
        let mut ole = OLE.to_vec();
        ole.resize(4096, 0);
        let err = Workbook::open(Cursor::new(ole), usize::MAX).err().unwrap();
        assert!(err.to_string().contains("password"), "{err}");
    }

    #[test]
    fn table_cells_stream_from_their_area() {
        let mut book = sheet(&[&["x"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        for r in 0..BATCH_ROWS as u32 + 10 {
            ws.write_number(r + 5, 1, r).unwrap();
            ws.write_number(r + 5, 3, 1).unwrap(); // outside the table
        }
        let table = rust_xlsxwriter::Table::new()
            .set_name("Big")
            .set_columns(&[rust_xlsxwriter::TableColumn::new().set_header("N")]);
        ws.add_table(4, 1, BATCH_ROWS as u32 + 14, 1, &table)
            .unwrap();
        let mut wb = open(&mut book);
        let t = wb.load_table("big", usize::MAX).unwrap().table;
        assert_eq!((t.rows(), names(&t)), (BATCH_ROWS + 10, vec!["N".into()]));
        assert_eq!(ints(&t, 0)[BATCH_ROWS + 9], Some(BATCH_ROWS as i64 + 9));
    }

    #[test]
    fn table_refs_parse() {
        let a = |r: &str| area(r, None, None);
        assert_eq!(
            a("C3:D5"),
            Ok(Area {
                top: 3,
                left: 2,
                rows: 2,
                width: 2
            })
        );
        assert_eq!(cell("xfd1048576"), Some((1_048_575, 16_383)));
        assert_eq!(
            resolve("xl/worksheets", "../tables/table1.xml"),
            "xl/tables/table1.xml"
        );
        assert_eq!(
            resolve("xl/worksheets", "/xl/tables/t.xml"),
            "xl/tables/t.xml"
        );
    }

    /// Reads `item` raw (at most `rows` rows) into one table.
    fn raw(
        wb: &mut Workbook<Cursor<Vec<u8>>>,
        item: &str,
        table: bool,
        rows: usize,
    ) -> (Table, usize) {
        collect(item, usize::MAX, |sink| match table {
            false => wb.read_sheet(item, rows, ',', usize::MAX, sink),
            true => wb.read_table(item, rows, ',', usize::MAX, sink),
        })
        .unwrap()
    }

    #[test]
    fn raw_sheets_keep_the_first_row_and_number_the_columns() {
        let mut book = sheet(&[&["Region", "Qty"], &["North"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        ws.write_number(1, 1, 1.5).unwrap();
        ws.write_formula(2, 1, Formula::new("=1/0").set_result("#DIV/0!"))
            .unwrap();
        let (t, errors) = raw(&mut open(&mut book), "Data", false, usize::MAX);
        assert_eq!(names(&t), ["Column1", "Column2"]);
        assert_eq!(errors, 1);
        assert_eq!(
            strings(&t, 1),
            [Some("Qty".into()), Some("1,5".into()), None],
            "numbers in a text column use the given decimal separator"
        );
    }

    #[test]
    fn raw_reads_stop_after_the_rows_asked_for() {
        let mut book = sheet(&[&["N"]]);
        let ws = book.worksheet_from_name("Data").unwrap();
        for i in 1..=BATCH_ROWS as u32 * 2 {
            ws.write_number(i, 0, i).unwrap();
        }
        let table = rust_xlsxwriter::Table::new()
            .set_name("All")
            .set_columns(&[rust_xlsxwriter::TableColumn::new().set_header("N")]);
        ws.add_table(0, 0, BATCH_ROWS as u32 * 2, 0, &table)
            .unwrap();
        let mut wb = open(&mut book);
        let (t, _) = raw(&mut wb, "Data", false, 1000);
        assert_eq!(t.rows(), 1000);
        assert_eq!(strings(&t, 0)[999], Some("999".into()));
        let (t, _) = raw(&mut wb, "All", true, 5);
        assert_eq!((t.rows(), ints(&t, 0)[4]), (5, Some(5)));
    }
}
