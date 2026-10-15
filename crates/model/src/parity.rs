//! Parity-case runner (ADR 0004): parses `parity/observed/*.md`, evaluates each query, and
//! compares every **Check** (result column) with the values copied out of Power BI Desktop.
//!
//! A Check passes when our result, rendered the way Power BI copies it, equals the pasted text
//! exactly. `parity/passing.txt` lists the Checks that have passed (one `case/Check` per line);
//! everything else is **Pending**. A listed Check that fails, or an unlisted one that passes,
//! is a problem until `cargo xtask parity --update` rewrites the list.
//!
//! **Shaping cases** (datasets given as `Source: csv` files) run through Get Data and a small
//! `EVALUATE 'Table'` reader: see [`shaping`]. Every other case runs on the DAX evaluator
//! ([`crate::dax`]) over its *Enter data* tables (read in en-GB, the VM's regional settings).
//! An `EVALUATE ROW(…)` that fails is retried a column at a time, so a column using a function
//! we don't have yet stays Pending without failing its neighbours.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub mod shaping;

pub use shaping::{VM_DIR, copy_text, export};

type Result<T> = std::result::Result<T, String>;

/// Column types a dataset may declare (Power Query's names, shortened).
const TYPES: &[&str] = &["text", "decimal", "whole", "date", "datetime", "boolean"];

/// A table of copied cell text: a blank is an empty string.
#[derive(Debug, Default, PartialEq)]
pub struct Table {
    pub name: String,
    pub types: Vec<String>,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<Vec<&str>> {
        let i = self.columns.iter().position(|c| c == name)?;
        self.rows
            .iter()
            .map(|r| r.get(i).map(String::as_str))
            .collect()
    }
}

/// A dataset given as a CSV file (`Source: csv Sales.csv` + a ```` ```csv ```` block), loaded
/// through Get Data as the Query named after the file (`Sales`).
#[derive(Debug, PartialEq)]
pub struct CsvSource {
    /// The file name, e.g. `Sales.csv`.
    pub file: String,
    /// The Query (and table) name: the file name without `.csv`.
    pub query: String,
    /// The file's bytes: the block's lines, each ended by `\n`.
    pub text: String,
}

#[derive(Debug)]
pub struct Case {
    pub id: String,
    pub covers: Vec<String>,
    /// *Enter data* tables.
    pub tables: Vec<Table>,
    pub sources: Vec<CsvSource>,
    /// The Culture (`Culture: en-GB`) Get Data reads the CSV files in.
    pub culture: Option<String>,
    /// `## M` blocks: a Query's whole M as MetricCraft prints it, its file named without a
    /// folder (`File.Contents("Sales.csv")`).
    pub m: Vec<String>,
    /// The DAX query; empty for a visual case.
    pub query: String,
    /// A visual case's Visual (`## Visual`, a ```` ```json ```` block in the report file's
    /// visual shape, ADR 0003), run by `metriccraft_visuals::parity` instead of a query.
    pub visual: Option<String>,
    /// `## Measures`: (Table, Measure, DAX) from `Table[Name] = DAX` lines.
    pub measures: Vec<(String, String, String)>,
    /// `Filter: Table[Column] = value, …` lines: a visual case's slicer selections.
    pub filters: Vec<String>,
    /// `Map: <observed header> = <our column>` lines: which observed columns are Checks.
    pub maps: Vec<(String, String)>,
    pub observed: Table,
    /// Observed facts beyond the result columns, as (Check name, observed text):
    /// `Get Data M <Query>`, `M accepted`, `Load errors <Query>`, `Column order`.
    pub observed_extra: Vec<(String, String)>,
}

/// Parses one case file. Malformed cases are errors, never skipped.
pub fn parse(id: &str, text: &str) -> Result<Case> {
    let mut covers = Vec::new();
    let mut tables = Vec::new();
    let mut sources: Vec<CsvSource> = Vec::new();
    let mut culture = None;
    let mut m = Vec::new();
    let mut query = None;
    let mut visual = None;
    let mut measures = Vec::new();
    let mut filters = Vec::new();
    let mut maps = Vec::new();
    let mut observed = None;
    let mut extra = Vec::new();
    let mut versioned = false;
    let mut section = "";
    let mut next_table: Option<(String, Vec<(String, String)>)> = None;
    let mut next_source: Option<String> = None;
    let mut next_get_data: Option<String> = None;
    let mut fence: Option<(&str, Vec<&str>)> = None;

    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((info, body)) = &mut fence {
            if line.trim_end() != "```" {
                body.push(line);
                continue;
            }
            match *info {
                "dax" if section == "Query" => query = Some(body.join("\n")),
                "json" if section == "Visual" => visual = Some(body.join("\n")),
                "dax" if section == "Measures" => {
                    for l in body.iter().map(|l| l.trim()).filter(|l| !l.is_empty()) {
                        measures.push(measure_line(l)?);
                    }
                }
                "tsv" if section == "Observed" => observed = Some(tsv(body, true)?),
                "csv" if section == "Dataset" => {
                    let file = next_source
                        .take()
                        .ok_or("csv block needs a `Source: csv <File>.csv` line above it")?;
                    let query = file.strip_suffix(".csv").unwrap_or(&file).to_string();
                    let text = body.iter().map(|l| format!("{l}\n")).collect();
                    sources.push(CsvSource { file, query, text });
                }
                "m" if section == "M" => m.push(body.join("\n")),
                "m" if section == "Observed" => {
                    let q = next_get_data
                        .take()
                        .ok_or("observed m block needs a `Get Data M: <Query>` line above it")?;
                    extra.push((format!("Get Data M {q}"), body.join("\n")));
                }
                "tsv" if section == "Dataset" => {
                    let (name, typed) = next_table
                        .take()
                        .ok_or("dataset block needs a `Table:` line above it")?;
                    let mut t = tsv(body, false)?;
                    let names: Vec<_> = typed.iter().map(|(n, _)| n.clone()).collect();
                    if names != t.columns {
                        return Err(format!(
                            "table {name}: `Table:` line lists {names:?}, header is {:?}",
                            t.columns
                        ));
                    }
                    t.name = name;
                    t.types = typed.into_iter().map(|(_, ty)| ty).collect();
                    tables.push(t);
                }
                other => {
                    return Err(format!(
                        "unexpected ```{other} block in section `{section}`"
                    ));
                }
            }
            fence = None;
        } else if let Some(info) = line.strip_prefix("```") {
            fence = Some((info.trim(), Vec::new()));
        } else if let Some(h) = line.strip_prefix("## ") {
            section = h.trim();
        } else if let Some(list) = line.strip_prefix("Covers:") {
            covers = list
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        } else if let Some(spec) = line.strip_prefix("Filter:") {
            filters.push(spec.trim().to_string());
        } else if let Some(spec) = line.strip_prefix("Map:") {
            let (o, ours) = spec
                .split_once(" = ")
                .ok_or("`Map:` needs `<observed header> = <our column>`")?;
            maps.push((o.trim().to_string(), ours.trim().to_string()));
        } else if let Some(spec) = line.strip_prefix("Table:") {
            next_table = Some(table_line(spec)?);
        } else if let Some(spec) = line.strip_prefix("Source:") {
            let file = spec
                .trim()
                .strip_prefix("csv ")
                .map(str::trim)
                .filter(|f| f.len() > 4 && f.ends_with(".csv") && !f.contains(['/', '\\']))
                .ok_or("`Source:` line needs `csv <File>.csv` (a file name, no folder)")?;
            if sources.iter().any(|s| s.file == file) {
                return Err(format!("`{file}` is given twice"));
            }
            next_source = Some(file.to_string());
        } else if let Some(name) = line.strip_prefix("Culture:") {
            let c = metriccraft_data::culture::Culture::get(name.trim())
                .ok_or(format!("unknown Culture `{}`", name.trim()))?;
            culture = Some(c.name.to_string());
        } else if let Some(answer) = line.strip_prefix("M accepted:") {
            let word = answer
                .split_whitespace()
                .next()
                .map(str::to_lowercase)
                .unwrap_or_default();
            if word != "yes" && word != "no" {
                return Err("`M accepted:` must start with `yes` or `no`".into());
            }
            extra.push(("M accepted".to_string(), word));
        } else if let Some(q) = line.strip_prefix("Get Data M:") {
            next_get_data = Some(q.trim().to_string());
        } else if let Some(spec) = line.strip_prefix("Load errors:") {
            let (q, n) = spec
                .trim()
                .rsplit_once(' ')
                .filter(|(_, n)| n.parse::<usize>().is_ok())
                .ok_or("`Load errors:` needs `<Query> <count>`")?;
            extra.push((format!("Load errors {}", q.trim()), n.to_string()));
        } else if line.starts_with("Power BI Desktop version:") {
            versioned = true;
        }
    }

    if fence.is_some() {
        return Err("unclosed ``` block".into());
    }
    if covers.is_empty() {
        return Err("missing `Covers:` line".into());
    }
    if next_source.is_some() {
        return Err("`Source:` line without a ```csv block below it".into());
    }
    if !sources.is_empty() && culture.is_none() {
        return Err("a case with `Source: csv` datasets needs a `Culture:` line".into());
    }
    if !m.is_empty() && sources.is_empty() {
        return Err("## M blocks need `Source: csv` datasets".into());
    }
    let mut seen = BTreeSet::new();
    for (name, _) in &extra {
        let known = if name == "M accepted" {
            !m.is_empty()
        } else {
            sources.iter().any(|s| {
                *name == format!("Get Data M {}", s.query)
                    || *name == format!("Load errors {}", s.query)
            })
        };
        if !known {
            return Err(format!(
                "observed `{name}` doesn't match this case's Queries or ## M"
            ));
        }
        if !seen.insert(name) {
            return Err(format!("observed `{name}` is given twice"));
        }
    }
    let query = match (query, &visual) {
        (Some(q), None) => q,
        (None, Some(_)) => String::new(),
        (None, None) => return Err("missing ```dax block under ## Query".into()),
        (Some(_), Some(_)) => return Err("a case has a ## Query or a ## Visual, not both".into()),
    };
    if visual.is_none() && !(measures.is_empty() && filters.is_empty() && maps.is_empty()) {
        return Err("## Measures, `Filter:` and `Map:` are for visual cases".into());
    }
    // A case awaiting observation has no Observed block yet, so no Checks.
    let observed = observed.unwrap_or_default();
    if (!observed.columns.is_empty() || !extra.is_empty()) && !versioned {
        return Err("missing `Power BI Desktop version:` line".into());
    }
    if !observed.columns.is_empty() {
        if observed.rows.is_empty() {
            return Err("observed result has no rows".into());
        }
        if visual.is_none() && observed.rows.len() > 1 && !query.to_uppercase().contains("ORDER BY")
        {
            return Err("multi-row result needs ORDER BY in the query".into());
        }
        if !sources.is_empty() {
            extra.push(("Column order".into(), observed.columns.join(", ")));
        }
    }
    Ok(Case {
        id: id.into(),
        covers,
        tables,
        sources,
        culture,
        m,
        query,
        visual,
        measures,
        filters,
        maps,
        observed,
        observed_extra: extra,
    })
}

/// `Sales[Total] = SUM(Sales[Amount])` → (Table, Measure, DAX).
fn measure_line(l: &str) -> Result<(String, String, String)> {
    let bad = || format!("measure line `{l}` isn't `Table[Name] = DAX`");
    let (lhs, dax) = l.split_once('=').ok_or_else(bad)?;
    let (table, name) = lhs.trim().split_once('[').ok_or_else(bad)?;
    let name = name.strip_suffix(']').ok_or_else(bad)?;
    let table = table.trim().trim_matches('\'');
    if table.is_empty() || name.is_empty() || dax.trim().is_empty() {
        return Err(bad());
    }
    Ok((table.into(), name.into(), dax.trim().into()))
}

/// `Table: Sales — Region: text, Amount: decimal, Qty: whole`
fn table_line(spec: &str) -> Result<(String, Vec<(String, String)>)> {
    let (name, cols) = spec
        .split_once('—')
        .ok_or("`Table:` line needs `Name — Col: type, …`")?;
    let cols = cols
        .split(',')
        .map(|c| {
            let (n, ty) = c
                .split_once(':')
                .ok_or(format!("column `{}` needs `: type`", c.trim()))?;
            let ty = ty.trim().to_lowercase();
            if !TYPES.contains(&ty.as_str()) {
                return Err(format!("unknown type `{ty}` (known: {})", TYPES.join(", ")));
            }
            Ok((n.trim().to_string(), ty))
        })
        .collect::<Result<_>>()?;
    Ok((name.trim().to_string(), cols))
}

/// The column name in a copied header: `[Name]`, or `Table[Name]` for a table's column.
fn header_name(c: &str) -> Option<&str> {
    let (table, name) = c.strip_suffix(']')?.split_once('[')?;
    (!table.contains(']')).then_some(name)
}

/// Tab-separated header + rows. Power BI's copy wraps headers in `[ ]` (with the table name in
/// front for a table's column); keep only the column name.
fn tsv(lines: &[&str], bracketed: bool) -> Result<Table> {
    let mut lines = lines.iter();
    let header = lines.next().ok_or("empty ```tsv block")?;
    let columns: Vec<String> = header
        .split('\t')
        .map(|c| match (bracketed, header_name(c)) {
            (true, Some(inner)) => Ok(inner.to_string()),
            (true, None) => Err(format!("observed header `{c}` is not `[Name]`")),
            (false, _) => Ok(c.to_string()),
        })
        .collect::<Result<_>>()?;
    let rows = lines
        .map(|l| {
            let row: Vec<String> = l.split('\t').map(str::to_string).collect();
            if row.len() == columns.len() {
                Ok(row)
            } else {
                Err(format!(
                    "row has {} fields, header has {}: `{l}`",
                    row.len(),
                    columns.len()
                ))
            }
        })
        .collect::<Result<_>>()?;
    Ok(Table {
        columns,
        rows,
        ..Table::default()
    })
}

/// Why a query can't be evaluated yet.
const NO_EVALUATOR: &str = "no DAX evaluator yet (M3)";

/// Memory cap for a case's data and query: tiny datasets, generous cap.
const LIMIT: usize = 256 << 20;

/// The Culture *Enter data* values are read in: the VM's regional settings.
const DATASET_CULTURE: &str = "en-GB";

/// Errors of single result columns, by column name.
type ColumnErrors = BTreeMap<String, String>;

/// Runs a case's query against its dataset and renders the result as Power BI copies it. A
/// failing `EVALUATE ROW(…)` is retried a column at a time, so one unsupported column leaves
/// the others' Checks to pass or fail on their own; their errors are returned by column.
fn evaluate(case: &Case) -> (Result<Table>, ColumnErrors) {
    let mut errors = ColumnErrors::new();
    let result = (|| {
        let db = dataset(case)?;
        let query = metriccraft_dax::parse_query(&case.query).map_err(|e| e.to_string())?;
        let options = crate::dax::Options {
            cancel: None,
            memory_cap: LIMIT,
        };
        match crate::dax::run_query(&db, &query, options) {
            Ok(results) => {
                let [result] = results.as_slice() else {
                    return Err(format!(
                        "the query has {} EVALUATEs; a case needs one",
                        results.len()
                    ));
                };
                Ok(copied(result))
            }
            Err(e) => {
                let Some(columns) = row_columns(&query) else {
                    return Err(e.to_string());
                };
                let mut table = Table::default();
                let mut row = Vec::new();
                for (name, one) in columns {
                    match crate::dax::run_query(&db, &one, options) {
                        Ok(r) => {
                            let cell = r.first().and_then(|t| t.rows.first()?.first().cloned());
                            table.columns.push(name);
                            row.push(cell.map(|v| v.copy_text()).unwrap_or_default());
                        }
                        Err(e) => {
                            errors.insert(name, e.to_string());
                        }
                    }
                }
                table.rows.push(row);
                Ok(table)
            }
        }
    })();
    (result, errors)
}

/// The case's *Enter data* tables, loaded as Get Data would (types converted, text
/// dictionary-encoded, Relationships auto-detected), as the evaluator's database. Empty cells
/// are blanks.
fn dataset(case: &Case) -> Result<crate::dax::Database> {
    let (model, loaded) = loaded(case)?;
    Ok(crate::dax::Database::new(&model, &loaded))
}

/// The case's Model (its *Enter data* tables and `## Measures`) and loaded tables.
pub fn loaded(case: &Case) -> Result<(crate::Model, Vec<metriccraft_data::Table>)> {
    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use metriccraft_data::TableBuilder;
    use metriccraft_data::step::ColumnType;

    let culture = metriccraft_data::culture::Culture::get(DATASET_CULTURE)
        .ok_or("the dataset Culture is missing")?;
    let mut model = crate::Model::default();
    let mut loaded = Vec::new();
    for t in &case.tables {
        let mut columns: Vec<(String, ArrayRef)> = Vec::new();
        for (i, (name, ty)) in t.columns.iter().zip(&t.types).enumerate() {
            let ty = match ty.as_str() {
                "text" => ColumnType::Text,
                "decimal" => ColumnType::Decimal,
                "whole" => ColumnType::Whole,
                "date" => ColumnType::Date,
                "datetime" => ColumnType::DateTime,
                "boolean" => ColumnType::Boolean,
                other => return Err(format!("unknown type `{other}`")),
            };
            let text: StringArray = t
                .rows
                .iter()
                .map(|r| r.get(i).map(String::as_str).filter(|s| !s.is_empty()))
                .collect();
            let (array, bad) = ty.convert(&text, culture);
            if bad > 0 {
                return Err(format!(
                    "table {}: {bad} value(s) of column {name} aren't {}",
                    t.name,
                    ty.name()
                ));
            }
            columns.push((name.clone(), array));
        }
        let batch = RecordBatch::try_from_iter(columns).map_err(|e| e.to_string())?;
        let mut b = TableBuilder::new(&t.name, batch.schema(), LIMIT);
        b.push(&batch).map_err(|e| e.to_string())?;
        let table = b.finish().map_err(|e| e.to_string())?;
        let (m, table) =
            crate::ModelTable::load(None, table, culture, LIMIT).map_err(|e| e.to_string())?;
        model.put(m);
        loaded.push(table);
    }
    // Power BI auto-detects Relationships when the tables load (same-named key columns).
    model.autodetect(&loaded, |_| true);
    for (table, name, dax) in &case.measures {
        model
            .add_measure(table, crate::Measure::new(name, dax))
            .map_err(|e| format!("measure {table}[{name}]: {e}"))?;
    }
    Ok((model, loaded))
}

/// A result as Power BI copies it.
fn copied(r: &crate::dax::ResultTable) -> Table {
    Table {
        columns: r.columns.clone(),
        rows: r
            .rows
            .iter()
            .map(|row| row.iter().map(|v| v.copy_text()).collect())
            .collect(),
        ..Table::default()
    }
}

/// For a query that is one `EVALUATE ROW("A", a, "B", b, …)`: each column as its own query
/// (same `DEFINE`s), with its name.
fn row_columns(q: &metriccraft_dax::Query) -> Option<Vec<(String, metriccraft_dax::Query)>> {
    use metriccraft_dax::{Evaluate, Expr, ExprKind};
    let [ev] = q.evaluates.as_slice() else {
        return None;
    };
    let ExprKind::Call { name, args } = &ev.table.kind else {
        return None;
    };
    if !name.eq_ignore_ascii_case("ROW") || args.len() % 2 != 0 {
        return None;
    }
    args.chunks(2)
        .map(|pair| {
            let [n, _] = pair else { return None };
            let ExprKind::Text(text) = &n.kind else {
                return None;
            };
            let one = metriccraft_dax::Query {
                defines: q.defines.clone(),
                evaluates: vec![Evaluate {
                    table: Expr {
                        kind: ExprKind::Call {
                            name: name.clone(),
                            args: pair.to_vec(),
                        },
                        span: ev.table.span,
                    },
                    order_by: Vec::new(),
                    span: ev.span,
                }],
            };
            Some((text.clone(), one))
        })
        .collect()
}

#[derive(Debug)]
pub struct Check {
    pub case: String,
    pub name: String,
    pub covers: Vec<String>,
    pub pass: bool,
    pub listed: bool,
    pub detail: String,
}

impl Check {
    pub fn key(&self) -> String {
        format!("{}/{}", self.case, self.name)
    }
}

#[derive(Debug)]
pub struct Report {
    pub checks: Vec<Check>,
    /// Listed in `passing.txt` but no longer a Check in any case.
    pub stale: Vec<String>,
}

/// Parses every case under `root/observed/`, in file order.
pub fn cases(root: &Path) -> Result<Vec<Case>> {
    cases_in(&root.join("observed"))
}

/// Parses every case file in `dir`, in file order.
pub fn cases_in(dir: &Path) -> Result<Vec<Case>> {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|path| {
            let id = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default();
            let text =
                std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            parse(id, &text).map_err(|e| format!("{}: {e}", path.display()))
        })
        .collect()
}

/// Runs every case under `root/observed/` against `root/passing.txt`.
pub fn run(root: &Path) -> Result<Report> {
    let passing = std::fs::read_to_string(root.join("passing.txt")).unwrap_or_default();
    let mut listed: BTreeSet<&str> = passing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();

    let mut checks = Vec::new();
    for case in cases(root)? {
        let id = case.id.clone();
        // A shaping case that doesn't load (or whose M isn't ours) is malformed, observed or not.
        let shaping = shaping::prepare(&case).map_err(|e| format!("{id}: {e}"))?;
        let mut column_errors = ColumnErrors::new();
        let (result, extras) = match &shaping {
            Some(s) => s.outcome(&case),
            None => {
                let (result, errors) = evaluate(&case);
                column_errors = errors;
                (result, BTreeMap::new())
            }
        };
        let mut found = Vec::new();
        for name in &case.observed.columns {
            let expected = case.observed.column(name);
            let (pass, detail) = match &result {
                Ok(t) => match (t.column(name), column_errors.get(name)) {
                    (got, _) if got == expected => (true, String::new()),
                    (None, Some(e)) => (false, e.clone()),
                    (got, _) => (false, format!("expected {expected:?}, got {got:?}")),
                },
                Err(e) => (false, e.clone()),
            };
            found.push((name.clone(), pass, detail));
        }
        for (name, expected) in &case.observed_extra {
            let (pass, detail) = match extras.get(name) {
                Some(Ok(got)) if got.trim_end() == expected.trim_end() => (true, String::new()),
                Some(Ok(got)) => (false, format!("expected\n{expected}\ngot\n{got}")),
                Some(Err(e)) => (false, e.clone()),
                None => (false, "not checked by the runner".into()),
            };
            found.push((name.clone(), pass, detail));
        }
        for (name, pass, detail) in found {
            let key = format!("{id}/{name}");
            checks.push(Check {
                listed: listed.remove(key.as_str()),
                case: id.clone(),
                name,
                covers: case.covers.clone(),
                pass,
                detail,
            });
        }
    }
    let stale = listed.into_iter().map(str::to_string).collect();
    Ok(Report { checks, stale })
}

impl Report {
    /// Everything that must fail the gate: regressions, unrecorded passes, stale entries.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        for c in &self.checks {
            match (c.listed, c.pass) {
                (true, false) => out.push(format!("REGRESSED {}: {}", c.key(), c.detail)),
                (false, true) => out.push(format!(
                    "NEWLY PASSING {} (run `cargo xtask parity --update`)",
                    c.key()
                )),
                _ => {}
            }
        }
        out.extend(
            self.stale
                .iter()
                .map(|s| format!("STALE {s} in passing.txt (run `cargo xtask parity --update`)")),
        );
        out
    }

    /// The new `passing.txt`.
    pub fn passing_list(&self) -> String {
        self.checks
            .iter()
            .filter(|c| c.pass)
            .map(|c| c.key() + "\n")
            .collect()
    }

    /// Covered tag → (passing Checks, total Checks).
    pub fn by_tag(&self) -> BTreeMap<&str, (usize, usize)> {
        let mut out: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
        for c in &self.checks {
            for tag in &c.covers {
                let e = out.entry(tag.as_str()).or_default();
                e.0 += usize::from(c.pass);
                e.1 += 1;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASE: &str = "# Parity case\n\nCovers: SUM, BLANK\n\n## Dataset\n\n\
        Table: T — A: text, B: decimal\n\n```tsv\nA\tB\nx\t1.5\ny\t\n```\n\n\
        ## Query\n\n```dax\nEVALUATE ROW(\"S\", SUM(T[B]))\n```\n\n## Observed\n\n\
        Power BI Desktop version: 2.158\n\n```tsv\n[S]\t[E]\r\n1.5\t\n```\n";

    #[test]
    fn parses_a_visual_case() {
        let v = "Covers: card\n\n## Dataset\n\nTable: T — A: text, B: decimal\n\n\
            ```tsv\nA\tB\nx\t1.5\n```\n\n## Measures\n\n```dax\nT[S] = SUM(T[B])\n```\n\n\
            ## Visual\n\nFilter: T[A] = x\n\n```json\n{\"type\": \"card\"}\n```\n\n\
            ## Observed\n\nPower BI Desktop version: 2.158\n\n```tsv\n[S]\n1.5\n```\n\
            Map: S = S\n";
        let c = parse("v", v).unwrap();
        assert_eq!(c.measures, [("T".into(), "S".into(), "SUM(T[B])".into())]);
        assert_eq!(c.filters, ["T[A] = x"]);
        assert_eq!(c.maps, [("S".into(), "S".into())]);
        assert!(c.query.is_empty() && c.visual.is_some());
        let (model, _) = loaded(&c).unwrap();
        assert!(model.measure("S").is_some());
        let both = v.replace(
            "## Visual",
            "## Query\n\n```dax\nEVALUATE T\n```\n\n## Visual",
        );
        assert!(parse("v", &both).unwrap_err().contains("not both"));
        assert!(parse("v", &v.replace("T[S] =", "T S =")).is_err());
        assert!(parse("x", &CASE.replace("Covers", "Map: a = b\nCovers")).is_err());
    }

    #[test]
    fn parses_a_case() {
        let c = parse("x", CASE).unwrap();
        assert_eq!(c.covers, ["SUM", "BLANK"]);
        assert_eq!(c.tables[0].name, "T");
        assert_eq!(c.tables[0].types, ["text", "decimal"]);
        assert_eq!(c.tables[0].rows[1], ["y", ""]);
        assert_eq!(c.observed.columns, ["S", "E"]);
        assert_eq!(c.observed.rows, [["1.5", ""]]);
    }

    #[test]
    fn case_awaiting_observation_has_no_checks() {
        let i = CASE.find("## Observed").unwrap();
        assert!(parse("x", &CASE[..i]).unwrap().observed.columns.is_empty());
    }

    #[test]
    fn rejects_malformed_cases() {
        for (bad, why) in [
            (CASE.replace("1.5\t\n```\n", "1.5\n```\n"), "row width"),
            (
                CASE.replace("1.5\t\n", "1.5\t\n2\t\n"),
                "multi-row without ORDER BY",
            ),
            (CASE.replace("Covers: SUM, BLANK", ""), "no Covers"),
            (CASE.replace("B: decimal", "B: money"), "unknown type"),
            (CASE.replace("[S]", "S"), "unbracketed header"),
            (
                CASE.replace("Power BI Desktop version", "Version"),
                "no version",
            ),
        ] {
            assert!(parse("x", &bad).is_err(), "{why} should be rejected");
        }
    }

    #[test]
    fn enter_data_tables_get_auto_detected_relationships() {
        let case = parse(
            "0043",
            include_str!("../../../parity/observed/0043-summarizecolumns.md"),
        )
        .unwrap();
        let db = dataset(&case).unwrap();
        assert_eq!(db.relationships.len(), 1);
        let (result, errors) = evaluate(&case);
        assert!(errors.is_empty());
        let t = result.unwrap();
        assert_eq!(t.columns, ["Region", "Product", "Total", "Big Orders"]);
        assert_eq!(t.rows.len(), 3);
    }

    #[test]
    fn ratchet() {
        let check = |listed, pass| Check {
            case: "c".into(),
            name: "n".into(),
            covers: vec![],
            pass,
            listed,
            detail: String::new(),
        };
        let problems = |c| {
            Report {
                checks: vec![c],
                stale: vec![],
            }
            .problems()
            .len()
        };
        assert_eq!(problems(check(false, false)), 0, "pending");
        assert_eq!(problems(check(true, true)), 0, "passing");
        assert_eq!(problems(check(true, false)), 1, "regression");
        assert_eq!(problems(check(false, true)), 1, "unrecorded pass");
    }
}
