//! Parity-case runner (ADR 0004): parses `parity/observed/*.md`, evaluates each query, and
//! compares every **Check** (result column) with the values copied out of Power BI Desktop.
//!
//! A Check passes when our result, rendered the way Power BI copies it, equals the pasted text
//! exactly. `parity/passing.txt` lists the Checks that have passed (one `case/Check` per line);
//! everything else is **Pending**. A listed Check that fails, or an unlisted one that passes,
//! is a problem until `cargo xtask parity --update` rewrites the list.
//!
//! **Shaping cases** (datasets given as `Source: csv` files) run now, without a DAX evaluator:
//! see [`shaping`]. Every other case waits for the M3 evaluator.

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
    fn column(&self, name: &str) -> Option<Vec<&str>> {
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
    /// `## M` blocks: a Query's whole M as OpenDash BI prints it, its file named without a
    /// folder (`File.Contents("Sales.csv")`).
    pub m: Vec<String>,
    pub query: String,
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
            let c = opendash_data::culture::Culture::get(name.trim())
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
    let query = query.ok_or("missing ```dax block under ## Query")?;
    // A case awaiting observation has no Observed block yet, so no Checks.
    let observed = observed.unwrap_or_default();
    if (!observed.columns.is_empty() || !extra.is_empty()) && !versioned {
        return Err("missing `Power BI Desktop version:` line".into());
    }
    if !observed.columns.is_empty() {
        if observed.rows.is_empty() {
            return Err("observed result has no rows".into());
        }
        if observed.rows.len() > 1 && !query.to_uppercase().contains("ORDER BY") {
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
        observed,
        observed_extra: extra,
    })
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

/// Runs a case's query against its dataset and renders the result as Power BI copies it.
fn evaluate(_case: &Case) -> Result<Table> {
    // ponytail: no evaluator until M3; every Check stays Pending.
    Err(NO_EVALUATOR.into())
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
    let dir = root.join("observed");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
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
        let (result, extras) = match &shaping {
            Some(s) => s.outcome(&case),
            None => (evaluate(&case), BTreeMap::new()),
        };
        let mut found = Vec::new();
        for name in &case.observed.columns {
            let expected = case.observed.column(name);
            let (pass, detail) = match &result {
                Ok(t) => match t.column(name) {
                    got if got == expected => (true, String::new()),
                    got => (false, format!("expected {expected:?}, got {got:?}")),
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
