//! Shaping cases (ADR 0004, "Shaping cases"): datasets given as CSV files, loaded through our
//! real Get Data path, so type detection, Culture parsing, Cell errors, Applied Steps and the M
//! printer are checked against Power BI before there is a DAX evaluator.
//!
//! - Each `Source: csv` file connects as Power BI's *Get Data → Text/CSV* does
//!   ([`Query::connect`]: automatic *Promoted Headers* / *Changed Type*, in the case's Culture).
//! - A `## M` block replaces that Query's steps. It must be exactly what our printer emits for
//!   the Query (path = the bare file name) and start with the automatic steps, so the M Martin
//!   pastes into the Advanced Editor and the steps the runner applies are one and the same.
//! - The query must be `EVALUATE 'Table'`, optionally with `ORDER BY` columns; anything else
//!   waits for the M3 evaluator.
//!
//! Beyond the result columns, a shaping case has these Checks (named as in `observed_extra`):
//! `Get Data M <Query>` (the M Get Data writes, against ours for the same file), `M accepted`
//! (Power BI took the case's M unchanged), `Load errors <Query>` (the Cell error count Close &
//! Apply reports) and `Column order`.
//!
//! **Guesses the Observed pastes decide** (ADR 0004 open threads): dates and datetimes copy as
//! `dd/MM/yyyy HH:mm:ss` (UK regional settings; a date is a datetime at midnight in DAX);
//! `ORDER BY` puts blanks first and compares text case-insensitively; NaN copies as `NaN`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use arrow_array::cast::AsArray;
use arrow_array::types::{Date32Type, Float64Type, Int64Type, TimestampMicrosecondType};
use arrow_array::{Array, ArrayRef};
use arrow_cast::cast;
use arrow_schema::{DataType, TimeUnit};
use opendash_data::Query;
use opendash_data::culture::Culture;
use opendash_data::m::{self, MQuery};
use opendash_data::source::{Bytes, File, Source, csv};

use super::{Case, NO_EVALUATOR, Result, Table};

/// The VM folder `cargo xtask parity --export` output is copied to: case `0003-x`'s file
/// `Sales.csv` is `C:\opendash-parity\0003-x\Sales.csv` there, and that is the path in the M.
pub const VM_DIR: &str = r"C:\opendash-parity";

/// Memory cap for a case's data: tiny files, generous cap.
const LIMIT: usize = 256 << 20;

/// Where a case's file is in the VM.
fn vm_path(case: &str, file: &str) -> String {
    format!(r"{VM_DIR}\{case}\{file}")
}

/// A shaping case's Queries, connected and checked.
pub(crate) struct Shaping {
    /// Each Query as Get Data connects it: Source + automatic steps.
    connected: Vec<Query>,
    /// The same Queries with the case's M applied.
    queries: Vec<Query>,
}

/// Connects a case's CSV files and applies its M. `None` for a case without `Source: csv`
/// datasets; an error means the case is malformed.
pub(crate) fn prepare(case: &Case) -> Result<Option<Shaping>> {
    if case.sources.is_empty() {
        return Ok(None);
    }
    let name = case.culture.as_deref().unwrap_or_default();
    let culture = Culture::get(name).ok_or(format!("unknown Culture `{name}`"))?;
    let mut connected = Vec::new();
    for s in &case.sources {
        let file = File::Bytes {
            name: s.file.clone(),
            bytes: Bytes::from(s.text.clone()),
        };
        let source =
            Source::csv(&s.query, file, csv::Options::default()).map_err(|e| e.to_string())?;
        let q = Query::connect(&s.query, source, culture, LIMIT).map_err(|e| e.to_string())?;
        connected.push(q);
    }
    let mut queries = connected.clone();
    let mut done = Vec::new();
    for text in &case.m {
        let parsed = m::parse(text).map_err(|e| format!("## M: {e}"))?;
        let path = parsed.source.file().path();
        let q = queries
            .iter_mut()
            .find(|q| q.source.file().path() == path)
            .ok_or(format!(
                "## M reads `{path}`, which is not a `Source: csv` file of this case"
            ))?;
        if done.contains(&q.name) {
            return Err(format!("two ## M blocks for `{}`", q.name));
        }
        if !parsed.steps.starts_with(&q.steps) {
            return Err(format!(
                "## M for `{}` must start with the automatic steps Get Data adds: {:?}",
                q.name, q.steps
            ));
        }
        let ours = m::print(&MQuery {
            source: q.source.clone(),
            steps: parsed.steps.clone(),
            culture: parsed.culture,
        });
        if ours != text.trim_end() {
            return Err(format!(
                "## M for `{}` is not what OpenDash BI prints for it; expected:\n{ours}",
                q.name
            ));
        }
        q.steps = parsed.steps;
        if let Some(c) = parsed.culture {
            q.culture = c;
        }
        done.push(q.name.clone());
    }
    Ok(Some(Shaping { connected, queries }))
}

/// `source` reading the file at `path` instead.
fn moved(source: &Source, path: String) -> Source {
    match source.clone() {
        Source::Csv {
            delimiter,
            columns,
            encoding,
            ..
        } => Source::Csv {
            file: File::Path(path.into()),
            delimiter,
            columns,
            encoding,
        },
        Source::Parquet { .. } => Source::Parquet {
            file: File::Path(path.into()),
        },
        Source::Excel { item, kind, .. } => Source::Excel {
            file: File::Path(path.into()),
            item,
            kind,
        },
    }
}

impl Shaping {
    /// Our result (rendered as Power BI copies it) and our value for every extra Check.
    pub(crate) fn outcome(&self, case: &Case) -> (Result<Table>, BTreeMap<String, Result<String>>) {
        let mut extras = BTreeMap::new();
        if !case.m.is_empty() {
            // Our side of "Power BI accepts the M unchanged" is the M itself.
            extras.insert("M accepted".to_string(), Ok("yes".to_string()));
        }
        for q in &self.connected {
            let source = moved(&q.source, vm_path(&case.id, &q.source.file().path()));
            let m = m::print(&MQuery {
                source,
                steps: q.steps.clone(),
                culture: None,
            });
            extras.insert(format!("Get Data M {}", q.name), Ok(m));
        }
        let mut loaded = BTreeMap::new();
        for q in &self.queries {
            let run = q.run_with(&self.queries, LIMIT).map_err(|e| e.to_string());
            let errors = run
                .as_ref()
                .map(|(_, n)| n.to_string())
                .map_err(Clone::clone);
            extras.insert(format!("Load errors {}", q.name), errors);
            loaded.insert(q.name.as_str(), run.map(|(t, _)| t));
        }
        let result = match evaluate_table(&case.query) {
            None => Err(NO_EVALUATOR.to_string()),
            Some(dq) => match loaded.get(dq.table.as_str()) {
                None => Err(format!("there is no table `{}`", dq.table)),
                Some(Err(e)) => Err(e.clone()),
                Some(Ok(t)) => render(t.batch(), &dq),
            },
        };
        let order = result.as_ref().map(|t| t.columns.join(", "));
        extras.insert("Column order".into(), order.map_err(Clone::clone));
        (result, extras)
    }
}

/// `EVALUATE 'Table' ORDER BY 'Table'[A], [B] DESC`.
#[derive(Debug, PartialEq)]
struct TableQuery {
    table: String,
    /// Column, descending.
    order: Vec<(String, bool)>,
}

/// The query, if it is `EVALUATE` of a whole table with an optional `ORDER BY`.
fn evaluate_table(query: &str) -> Option<TableQuery> {
    let mut p = Parser(query);
    p.keyword("EVALUATE")?;
    let table = p.table()?;
    let mut order = Vec::new();
    if p.keyword("ORDER").is_some() {
        p.keyword("BY")?;
        loop {
            p.space();
            if !p.0.starts_with('[') && p.table()? != table {
                return None;
            }
            let column = p.column()?;
            let desc = p.keyword("DESC").is_some();
            if !desc {
                p.keyword("ASC");
            }
            order.push((column, desc));
            p.space();
            match p.0.strip_prefix(',') {
                Some(rest) => p.0 = rest,
                None => break,
            }
        }
    }
    p.space();
    p.0.is_empty().then_some(TableQuery { table, order })
}

struct Parser<'a>(&'a str);

impl Parser<'_> {
    /// Skips whitespace and `//` / `--` line comments.
    fn space(&mut self) {
        loop {
            self.0 = self.0.trim_start();
            if self.0.starts_with("//") || self.0.starts_with("--") {
                self.0 = self.0.split_once('\n').map_or("", |(_, rest)| rest);
            } else {
                return;
            }
        }
    }

    /// A keyword, any case, not followed by more of a name.
    fn keyword(&mut self, word: &str) -> Option<()> {
        self.space();
        let head = self.0.get(..word.len())?;
        let rest = self.0.get(word.len()..)?;
        if !head.eq_ignore_ascii_case(word)
            || rest.starts_with(|c: char| c.is_alphanumeric() || c == '_')
        {
            return None;
        }
        self.0 = rest;
        Some(())
    }

    /// `'Name'` (with `''` for a quote) or a plain name.
    fn table(&mut self) -> Option<String> {
        self.space();
        if let Some(rest) = self.0.strip_prefix('\'') {
            let (name, rest) = quoted(rest, '\'')?;
            self.0 = rest;
            return Some(name);
        }
        let len = self
            .0
            .find(|c: char| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(self.0.len());
        let (name, rest) = self.0.split_at_checked(len)?;
        if name.is_empty() {
            return None;
        }
        self.0 = rest;
        Some(name.to_string())
    }

    /// `[Name]` (with `]]` for a bracket).
    fn column(&mut self) -> Option<String> {
        let rest = self.0.strip_prefix('[')?;
        let (name, rest) = quoted(rest, ']')?;
        self.0 = rest;
        Some(name)
    }
}

/// Text up to a closing `end` (doubled = one literal `end`) and what follows it.
fn quoted(s: &str, end: char) -> Option<(String, &str)> {
    let mut out = String::new();
    let mut chars = s.char_indices();
    while let Some((i, c)) = chars.next() {
        if c != end {
            out.push(c);
            continue;
        }
        let after = s.get(i + c.len_utf8()..)?;
        match after.strip_prefix(end) {
            Some(_) => {
                out.push(end);
                chars.next();
            }
            None => return Some((out, after)),
        }
    }
    None
}

/// One loaded value.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Blank,
    Whole(i64),
    Number(f64),
    Text(String),
    Logical(bool),
    /// Microseconds since 1970-01-01 00:00 (a date is midnight).
    DateTime(i64),
}

const DAY_MICROS: i64 = 86_400_000_000;

/// A column that isn't the type its data type says (never expected).
const BAD: &str = "a loaded column didn't read as its type";

/// A loaded (narrow-encoded) column as values.
fn values(column: &ArrayRef) -> Result<Vec<Value>> {
    let as_type = |t: DataType| cast(column, &t).map_err(|e| e.to_string());
    let get = |i: usize, v: Value| if column.is_null(i) { Value::Blank } else { v };
    let n = column.len();
    Ok(match column.data_type() {
        t if t.is_integer() => {
            let a = as_type(DataType::Int64)?;
            let a = a.as_primitive_opt::<Int64Type>().ok_or(BAD)?;
            (0..n).map(|i| get(i, Value::Whole(a.value(i)))).collect()
        }
        t if t.is_floating() => {
            let a = as_type(DataType::Float64)?;
            let a = a.as_primitive_opt::<Float64Type>().ok_or(BAD)?;
            (0..n).map(|i| get(i, Value::Number(a.value(i)))).collect()
        }
        DataType::Boolean => {
            let a = column.as_boolean_opt().ok_or(BAD)?;
            (0..n).map(|i| get(i, Value::Logical(a.value(i)))).collect()
        }
        DataType::Date32 => {
            let a = column.as_primitive_opt::<Date32Type>().ok_or(BAD)?;
            (0..n)
                .map(|i| {
                    let micros = i64::from(a.value(i)).saturating_mul(DAY_MICROS);
                    get(i, Value::DateTime(micros))
                })
                .collect()
        }
        DataType::Timestamp(TimeUnit::Microsecond, None) => {
            let a = column
                .as_primitive_opt::<TimestampMicrosecondType>()
                .ok_or(BAD)?;
            (0..n)
                .map(|i| get(i, Value::DateTime(a.value(i))))
                .collect()
        }
        _ => {
            let a = as_type(DataType::Utf8)?;
            let a = a.as_string_opt::<i32>().ok_or(BAD)?;
            (0..n)
                .map(|i| get(i, Value::Text(a.value(i).to_string())))
                .collect()
        }
    })
}

impl Value {
    /// DAX `ORDER BY` order (guessed, see the module docs): blanks first, text case-insensitive.
    fn order(&self, other: &Value) -> Ordering {
        #[allow(clippy::cast_precision_loss)]
        let number = |v: &Value| match v {
            Value::Whole(i) => Some(*i as f64),
            Value::Number(f) => Some(*f),
            _ => None,
        };
        match (self, other) {
            (Value::Blank, Value::Blank) => Ordering::Equal,
            (Value::Blank, _) => Ordering::Less,
            (_, Value::Blank) => Ordering::Greater,
            (Value::Text(a), Value::Text(b)) => a.to_lowercase().cmp(&b.to_lowercase()),
            (Value::Logical(a), Value::Logical(b)) => a.cmp(b),
            (Value::DateTime(a), Value::DateTime(b)) => a.cmp(b),
            (a, b) => match (number(a), number(b)) {
                (Some(a), Some(b)) => a.total_cmp(&b),
                _ => Ordering::Equal,
            },
        }
    }

    /// The value as Power BI's copy writes it.
    fn copy(&self) -> String {
        match self {
            Value::Blank => String::new(),
            Value::Whole(i) => i.to_string(),
            Value::Number(f) if f.is_nan() => "NaN".into(),
            Value::Number(f) if f.is_infinite() && *f > 0.0 => "Infinity".into(),
            Value::Number(f) if f.is_infinite() => "-Infinity".into(),
            // Rust prints the shortest text that reads back as the same f64.
            Value::Number(f) => f.to_string(),
            Value::Text(s) => s.clone(),
            Value::Logical(b) => b.to_string(),
            Value::DateTime(t) => {
                let (y, m, d) = civil(t.div_euclid(DAY_MICROS));
                let secs = t.rem_euclid(DAY_MICROS) / 1_000_000;
                format!(
                    "{d:02}/{m:02}/{y:04} {:02}:{:02}:{:02}",
                    secs / 3600,
                    secs / 60 % 60,
                    secs % 60
                )
            }
        }
    }
}

/// (year, month, day) for days since 1970-01-01 (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

/// A loaded table, sorted by the query's `ORDER BY`, as copy text cells.
fn render(batch: &arrow_array::RecordBatch, q: &TableQuery) -> Result<Table> {
    let schema = batch.schema();
    let columns: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();
    let data = batch
        .columns()
        .iter()
        .map(values)
        .collect::<Result<Vec<_>>>()?;
    let keys = q
        .order
        .iter()
        .map(|(name, desc)| {
            let i = columns
                .iter()
                .position(|c| c == name)
                .ok_or(format!("ORDER BY: there is no column `{name}`"))?;
            Ok((data.get(i).ok_or("column out of range")?, *desc))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut rows: Vec<usize> = (0..batch.num_rows()).collect();
    rows.sort_by(|&a, &b| {
        keys.iter()
            .map(|(col, desc)| {
                let o = match (col.get(a), col.get(b)) {
                    (Some(x), Some(y)) => x.order(y),
                    _ => Ordering::Equal,
                };
                if *desc { o.reverse() } else { o }
            })
            .find(|o| o.is_ne())
            .unwrap_or(Ordering::Equal)
    });
    let rows = rows
        .iter()
        .map(|&r| {
            data.iter()
                .map(|col| col.get(r).map(Value::copy).unwrap_or_default())
                .collect()
        })
        .collect();
    Ok(Table {
        name: q.table.clone(),
        types: Vec::new(),
        columns,
        rows,
    })
}

/// A result table as Power BI's DAX query view copies it: tab-separated, CRLF, headers
/// `Table[Column]` (`[Column]` when the table has no name, e.g. `ROW`). Whether a table's column
/// copies with its table name in front is a guess; the runner compares by column name only.
pub fn copy_text(t: &Table) -> String {
    let header: Vec<String> = t
        .columns
        .iter()
        .map(|c| format!("{}[{c}]", t.name))
        .collect();
    let mut out = header.join("\t") + "\r\n";
    for row in &t.rows {
        out.push_str(&row.join("\t"));
        out.push_str("\r\n");
    }
    out
}

/// The files `cargo xtask parity --export` writes for a case, as (path under
/// `observations/cases/`, bytes): `<id>/query.dax`, each *Enter data* table as
/// `<id>/<Table>.tsv`, each CSV byte-exact as `<id>/<File>.csv`, each `## M` block as
/// `<id>/<Query>.m` with the VM path ([`VM_DIR`]) filled in, and `<id>/README.txt`. Text meant
/// for Notepad is CRLF; CSV files are exactly the case's bytes.
pub fn export(case: &Case) -> Result<Vec<(String, Vec<u8>)>> {
    let crlf = |text: &str| (text.trim_end().to_string() + "\n").replace('\n', "\r\n");
    let id = &case.id;
    let mut files = Vec::new();
    match &case.visual {
        Some(v) => files.push((format!("{id}/visual.json"), crlf(v).into_bytes())),
        None => files.push((format!("{id}/query.dax"), crlf(&case.query).into_bytes())),
    }
    if !case.measures.is_empty() {
        let lines: Vec<String> = case
            .measures
            .iter()
            .map(|(t, n, dax)| format!("{t}[{n}] = {dax}"))
            .collect();
        files.push((
            format!("{id}/measures.dax"),
            crlf(&lines.join("\n")).into_bytes(),
        ));
    }
    for t in &case.tables {
        let mut lines = vec![t.columns.join("\t")];
        lines.extend(t.rows.iter().map(|r| r.join("\t")));
        files.push((
            format!("{id}/{}.tsv", t.name),
            crlf(&lines.join("\n")).into_bytes(),
        ));
    }
    for s in &case.sources {
        files.push((format!("{id}/{}", s.file), s.text.clone().into_bytes()));
    }
    let mut pasted = Vec::new();
    for text in &case.m {
        let mut q = m::parse(text).map_err(|e| format!("{id}: ## M: {e}"))?;
        let file = q.source.file().path();
        let query = file.strip_suffix(".csv").unwrap_or(&file).to_string();
        q.source = moved(&q.source, vm_path(id, &file));
        files.push((format!("{id}/{query}.m"), crlf(&m::print(&q)).into_bytes()));
        pasted.push(query);
    }
    files.push((
        format!("{id}/README.txt"),
        crlf(&readme(case, &pasted)).into_bytes(),
    ));
    Ok(files)
}

/// The steps for one case, for the VM (docs/parity.md has the full workflow).
fn readme(case: &Case, pasted: &[String]) -> String {
    let id = &case.id;
    let mut steps = vec!["File > New.".to_string()];
    if case.sources.is_empty() {
        steps.push("Home > Enter data: paste each .tsv file, name the table after it, set the column types the case lists. Close & Apply.".into());
    } else {
        let culture = case.culture.as_deref().unwrap_or_default();
        steps.push(format!(
            "File > Options and settings > Options > Current File > Regional Settings > Locale for import: {culture}."
        ));
        for s in &case.sources {
            steps.push(format!(
                "Get Data > Text/CSV > {VM_DIR}\\{id}\\{} > Transform Data (in Power Query: New Source > Text/CSV). Advanced Editor: copy all into the case as `Get Data M: {}`.",
                s.file, s.query
            ));
        }
        for q in pasted {
            steps.push(format!(
                "Query {q}: Advanced Editor > select all > paste {q}.m > Done. Applied Steps must show the step names of {q}.m, with no error. Record `M accepted: yes` (or `no` and why)."
            ));
        }
        steps.push(
            "Close & Apply. Record `Load errors: <Query> <n>` for every Query (0 if none).".into(),
        );
    }
    if case.visual.is_some() {
        if !case.measures.is_empty() {
            steps.push(
                "Add each measure in measures.dax (Home > New measure, on the table before `[`)."
                    .into(),
            );
        }
        steps.push("Report view: build the visual visual.json describes (type, fields per well in order, format options); Fields that are Columns in Values use their default summarization.".into());
        for f in &case.filters {
            steps.push(format!("Add a slicer and select: {f}."));
        }
        steps.push("View > Performance analyzer > Start recording > Refresh visuals. Expand the visual's entry > Copy query. DAX query view: paste > Run > copy the whole result > paste it into the case's Observed block, under the `Power BI Desktop version:` line. Add a `Map: <observed header> = <our column>` line per column that holds a group or value (see docs/parity.md).".into());
    } else {
        steps.push("DAX query view: paste query.dax > Run > copy the whole result > paste it into the case's Observed block, under the `Power BI Desktop version:` line.".into());
    }
    let dir = if case.visual.is_some() {
        "visuals"
    } else {
        "observed"
    };
    let mut out = format!("Parity case {id} (parity/{dir}/{id}.md)\n\n");
    for (i, s) in steps.iter().enumerate() {
        out.push_str(&format!("{}. {s}\n", i + 1));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parity;

    /// A shaping case with a Changed Type step and a merge.
    const CASE: &str = "# x\n\nCovers: Changed Type\nCulture: en-GB\n\n## Dataset\n\n\
        Source: csv Sales.csv\n\n```csv\nId,Qty,RegionId\n1,5,1\n2,abc,2\n3,2.5,\n```\n\n\
        Source: csv Regions.csv\n\n```csv\nRegionId,Manager\n1,Ann\n,Nobody\n```\n\n\
        ## M\n\n```m\nM\n```\n\n\
        ## Query\n\n```dax\nEVALUATE 'Sales'\n-- by id\nORDER BY 'Sales'[Id] DESC\n```\n";

    const M: &str = r#"let
    Source = Csv.Document(File.Contents("Sales.csv"),[Delimiter=",", Columns=3, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Id", Int64.Type}, {"Qty", type text}, {"RegionId", Int64.Type}}),
    #"Changed Type1" = Table.TransformColumnTypes(#"Changed Type",{{"Qty", Int64.Type}}),
    #"Merged Queries" = Table.NestedJoin(#"Changed Type1", {"RegionId"}, Regions, {"RegionId"}, "Regions", JoinKind.LeftOuter),
    #"Expanded Regions" = Table.ExpandTableColumn(#"Merged Queries", "Regions", {"Manager"}, {"Regions.Manager"})
in
    #"Expanded Regions""#;

    const OBSERVED: &str = "\n## Observed\n\nPower BI Desktop version: 2.158 · Date: x\n\n\
        M accepted: yes\nLoad errors: Sales 1\nLoad errors: Regions 0\n\nGet Data M: Regions\n\n```m\n\
        let\n    Source = Csv.Document(File.Contents(\"C:\\opendash-parity\\x\\Regions.csv\"),[Delimiter=\",\", Columns=2, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),\n    \
        #\"Promoted Headers\" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),\n    \
        #\"Changed Type\" = Table.TransformColumnTypes(#\"Promoted Headers\",{{\"RegionId\", Int64.Type}, {\"Manager\", type text}})\n\
        in\n    #\"Changed Type\"\n```\n\n\
        ```tsv\nSales[Id]\tSales[Qty]\tSales[RegionId]\tSales[Regions.Manager]\n\
        3\t2\t\tNobody\n2\t\t2\t\n1\t5\t1\tAnn\n```\n";

    fn case(observed: &str) -> Case {
        parity::parse(
            "x",
            &(CASE.replace("\nM\n", &format!("\n{M}\n")) + observed),
        )
        .unwrap()
    }

    fn checks(c: &Case) -> Vec<(String, bool, String)> {
        let s = prepare(c).unwrap().unwrap();
        let (result, extras) = s.outcome(c);
        let mut out = Vec::new();
        for name in &c.observed.columns {
            let got = result.as_ref().ok().and_then(|t| t.column(name));
            out.push((name.clone(), got == c.observed.column(name), String::new()));
        }
        for (name, want) in &c.observed_extra {
            let got = extras.get(name).cloned().unwrap_or(Err("missing".into()));
            let pass = got.as_deref().ok() == Some(want.as_str());
            out.push((name.clone(), pass, format!("{got:?}")));
        }
        out
    }

    #[test]
    fn parses_the_shaping_parts() {
        let c = case(OBSERVED);
        assert_eq!(c.culture.as_deref(), Some("en-GB"));
        assert_eq!(c.sources.len(), 2);
        assert_eq!(c.sources[0].query, "Sales");
        assert_eq!(
            c.sources[0].text,
            "Id,Qty,RegionId\n1,5,1\n2,abc,2\n3,2.5,\n"
        );
        assert_eq!(c.m, [M]);
        assert_eq!(c.observed.columns[3], "Regions.Manager");
        let names: Vec<_> = c.observed_extra.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "M accepted",
                "Load errors Sales",
                "Load errors Regions",
                "Get Data M Regions",
                "Column order"
            ]
        );
    }

    #[test]
    fn a_shaping_case_runs_and_compares() {
        let c = case(OBSERVED);
        let got = checks(&c);
        for (name, pass, detail) in &got {
            assert!(pass, "{name}: {detail}");
        }
        assert_eq!(got.len(), 9);
        // A wrong value, count or M fails only its own Check.
        let bad = case(
            &OBSERVED
                .replace("1\t5\t1\tAnn", "1\t6\t1\tAnn")
                .replace("Sales 1", "Sales 2")
                .replace("Columns=2", "Columns=3"),
        );
        let failed: Vec<_> = checks(&bad)
            .into_iter()
            .filter(|(_, pass, _)| !pass)
            .map(|(n, ..)| n)
            .collect();
        assert_eq!(failed, ["Qty", "Load errors Sales", "Get Data M Regions"]);
    }

    #[test]
    fn the_gate_reports_shaping_checks() {
        let dir = std::env::temp_dir().join(format!("opendash-parity-{}", std::process::id()));
        let observed = dir.join("observed");
        std::fs::create_dir_all(&observed).unwrap();
        let text = CASE.replace("\nM\n", &format!("\n{M}\n")) + OBSERVED;
        std::fs::write(observed.join("x.md"), text).unwrap();
        let report = parity::run(&dir).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(report.checks.len(), 9);
        assert!(report.checks.iter().all(|c| c.pass), "{:?}", report.checks);
        assert_eq!(report.problems().len(), 9, "all newly passing");
    }

    #[test]
    fn malformed_shaping_cases_are_errors() {
        let good = CASE.replace("\nM\n", &format!("\n{M}\n"));
        for (bad, why) in [
            (good.replace("Culture: en-GB\n", ""), "no Culture"),
            (
                good.replace("Culture: en-GB", "Culture: xx-XX"),
                "unknown Culture",
            ),
            (
                good.replace("Source: csv Sales.csv", "Source: csv a/Sales.csv"),
                "folder",
            ),
            (
                good.replace("```csv\nId", "```\nId"),
                "Source without csv block",
            ),
        ] {
            assert!(
                parity::parse("x", &bad).is_err(),
                "{why} should be rejected"
            );
        }
        for (bad, why) in [
            (good.replace(",{{\"Qty\"", ", {{\"Qty\""), "not printed"),
            (
                good.replace(", Columns=3,", ", Columns=4,"),
                "not our source",
            ),
            (good.replace("Sales.csv\"", "Other.csv\""), "unknown file"),
            (
                good.replace("{\"Qty\", type text}", "{\"Qty\", Int64.Type}"),
                "automatic steps differ",
            ),
        ] {
            let c = parity::parse("x", &bad).unwrap();
            assert!(prepare(&c).is_err(), "{why} should be rejected");
        }
        for (observed, why) in [
            (OBSERVED.replace("Sales 1", "Nope 1"), "unknown query"),
            (
                OBSERVED.replace("M accepted: yes", "M accepted: maybe"),
                "yes/no",
            ),
            (
                OBSERVED.replace("Power BI Desktop version", "Version"),
                "no version",
            ),
            (
                OBSERVED.replace("Get Data M: Regions\n", ""),
                "m block without label",
            ),
        ] {
            let text = good.clone() + &observed;
            assert!(
                parity::parse("x", &text).is_err(),
                "{why} should be rejected"
            );
        }
        let no_m = CASE.replace("## M\n\n```m\nM\n```\n\n", "") + OBSERVED;
        assert!(parity::parse("x", &no_m).is_err(), "M accepted without M");
    }

    #[test]
    fn only_evaluate_table_queries_run() {
        let q = evaluate_table("evaluate Sales order by Sales[A], [B C] desc, 'Sales'[D]]x] asc")
            .unwrap();
        assert_eq!(q.table, "Sales");
        assert_eq!(
            q.order,
            [
                ("A".into(), false),
                ("B C".into(), true),
                ("D]x".into(), false)
            ]
        );
        assert_eq!(evaluate_table("EVALUATE 'It''s'").unwrap().table, "It's");
        for other in [
            "EVALUATE ROW(\"x\", 1)",
            "EVALUATE 'Sales' ORDER BY 'Other'[A]",
            "EVALUATE FILTER('Sales', [A] > 1)",
            "EVALUATEx 'Sales'",
            "EVALUATE 'Sales' ORDER BY",
        ] {
            assert_eq!(evaluate_table(other), None, "{other}");
        }
    }

    #[test]
    fn values_copy_as_power_bi_does() {
        assert_eq!(
            Value::Number(2.4166666666666665).copy(),
            "2.4166666666666665"
        );
        assert_eq!(Value::Number(4.0).copy(), "4");
        assert_eq!(Value::Number(f64::INFINITY).copy(), "Infinity");
        assert_eq!(Value::Logical(true).copy(), "true");
        assert_eq!(Value::Blank.copy(), "");
        // 2026-04-03 10:30:05, and a date before 1970.
        let t = 20_546 * DAY_MICROS + (10 * 3600 + 30 * 60 + 5) * 1_000_000;
        assert_eq!(Value::DateTime(t).copy(), "03/04/2026 10:30:05");
        assert_eq!(Value::DateTime(-DAY_MICROS).copy(), "31/12/1969 00:00:00");
        assert_eq!(
            Value::Text("B".into()).order(&Value::Text("a".into())),
            Ordering::Greater
        );
        assert_eq!(Value::Blank.order(&Value::Whole(-5)), Ordering::Less);
    }

    #[test]
    fn copy_text_reads_back() {
        let c = case(OBSERVED);
        let s = prepare(&c).unwrap().unwrap();
        let t = s.outcome(&c).0.unwrap();
        let text = copy_text(&t);
        assert!(text.starts_with("Sales[Id]\tSales[Qty]"), "{text}");
        let lines: Vec<&str> = text.split("\r\n").filter(|l| !l.is_empty()).collect();
        let back = parity::tsv(&lines, true).unwrap();
        assert_eq!((back.columns, back.rows), (t.columns, t.rows));
    }

    #[test]
    fn export_writes_what_the_vm_needs() {
        let c = case("");
        let files: BTreeMap<String, Vec<u8>> = export(&c).unwrap().into_iter().collect();
        let names: Vec<&str> = files.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            [
                "x/README.txt",
                "x/Regions.csv",
                "x/Sales.csv",
                "x/Sales.m",
                "x/query.dax"
            ]
        );
        assert_eq!(
            files["x/Sales.csv"],
            c.sources[0].text.as_bytes(),
            "byte-exact"
        );
        let m = String::from_utf8(files["x/Sales.m"].clone()).unwrap();
        assert!(
            m.contains(r#"File.Contents("C:\opendash-parity\x\Sales.csv")"#),
            "{m}"
        );
        assert!(m.contains("\r\n    #\"Changed Type1\""), "CRLF: {m}");
        let dax = String::from_utf8(files["x/query.dax"].clone()).unwrap();
        assert!(dax.starts_with("EVALUATE 'Sales'\r\n"), "{dax}");
        let readme = String::from_utf8(files["x/README.txt"].clone()).unwrap();
        assert!(readme.contains("Locale for import: en-GB"), "{readme}");
        assert!(readme.contains("paste Sales.m"), "{readme}");
    }
}
