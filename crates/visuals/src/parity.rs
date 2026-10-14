//! Visual parity cases (M4.6; ADR 0004, amended): `parity/visuals/*.md`, each a dataset, optional
//! `## Measures`, a `## Visual` (the report file's visual JSON) and optional `Filter:` lines (slicer
//! selections). Observed is the result of the visual's own query in Power BI Desktop
//! (Performance Analyzer → Copy query → run → copy), pasted verbatim; `Map:` lines name which of
//! its columns are which of ours. Each mapped column is a **Check**, ratcheted in
//! `parity/visuals/passing.txt` like DAX cases.
//!
//! Our side is the [`QueryResult`] in long form: one record per row (totals included) and
//! column tuple, with the row-group, column-group and value columns named by their labels
//! (`Region`, `Sum of Amount`), values as Power BI copies them, a totalled key empty. Both sides'
//! records are sorted before comparing, so row order (Power BI's total-row placement) doesn't
//! matter; sorting is a separate Check of M4.1's own tests.

use std::collections::BTreeSet;
use std::path::Path;

use opendash_core::Value;
use opendash_model::dax::{Database, Filter, FilterContext, Options};
use opendash_model::parity::{Case, Check, Report, cases_in, loaded};

use crate::query::{QueryInput, QueryResult, run_query};
use crate::visual::{FieldRef, Visual, VisualType, Well};

type Result<T> = std::result::Result<T, String>;

/// Memory cap for a case's dataset and query.
const LIMIT: usize = 256 << 20;

/// Runs every visual case in `dir` against `dir/passing.txt`.
pub fn run(dir: &Path) -> Result<Report> {
    let passing = std::fs::read_to_string(dir.join("passing.txt")).unwrap_or_default();
    let mut listed: BTreeSet<&str> = passing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let mut checks = Vec::new();
    for case in cases_in(dir)? {
        if case.visual.is_none() {
            return Err(format!("{}: a visual case needs a ## Visual", case.id));
        }
        // Malformed visuals or datasets fail the gate even before observation.
        let ours = evaluate(&case).map_err(|e| format!("{}: {e}", case.id));
        let ours = match ours {
            Ok(t) => Ok(t),
            Err(e) if e.contains("malformed") => return Err(e),
            Err(e) => Err(e),
        };
        let observed: Vec<&str> = case.maps.iter().map(|(o, _)| o.as_str()).collect();
        let names: Vec<&str> = case.maps.iter().map(|(_, n)| n.as_str()).collect();
        let expected = records(&case.observed.columns, &case.observed.rows, &observed);
        let got = ours.and_then(|(cols, rows)| records(&cols, &rows, &names));
        for (i, (_, name)) in case.maps.iter().enumerate() {
            let want: Option<Vec<&str>> = expected.as_ref().ok().map(|r| {
                r.iter()
                    .map(|rec| rec.get(i).map_or("", String::as_str))
                    .collect()
            });
            let (pass, detail) = match (&got, &expected) {
                (Err(e), _) => (false, e.clone()),
                (_, Err(e)) => (false, format!("observed: {e}")),
                (Ok(g), Ok(_)) => {
                    let have: Vec<&str> = g
                        .iter()
                        .map(|rec| rec.get(i).map_or("", String::as_str))
                        .collect();
                    if Some(&have) == want.as_ref() {
                        (true, String::new())
                    } else {
                        (false, format!("expected {want:?}, got {have:?}"))
                    }
                }
            };
            let key = format!("{}/{name}", case.id);
            checks.push(Check {
                listed: listed.remove(key.as_str()),
                case: case.id.clone(),
                name: name.clone(),
                covers: case.covers.clone(),
                pass,
                detail,
            });
        }
    }
    let stale = listed.into_iter().map(str::to_string).collect();
    Ok(Report { checks, stale })
}

/// The rows' `wanted` columns, records sorted.
fn records(columns: &[String], rows: &[Vec<String>], wanted: &[&str]) -> Result<Vec<Vec<String>>> {
    let at: Vec<usize> = wanted
        .iter()
        .map(|w| {
            columns
                .iter()
                .position(|c| c == w)
                .ok_or(format!("no column `{w}` (have {columns:?})"))
        })
        .collect::<Result<_>>()?;
    let mut out: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            at.iter()
                .map(|&i| r.get(i).cloned().unwrap_or_default())
                .collect()
        })
        .collect();
    out.sort();
    Ok(out)
}

/// Our side (columns, rows): the case's Visual queried over its dataset, in long form (columns, rows).
pub fn evaluate(case: &Case) -> Result<(Vec<String>, Vec<Vec<String>>)> {
    let json = case.visual.as_deref().unwrap_or_default();
    let visual = parse_visual(json).map_err(|e| format!("malformed ## Visual: {e}"))?;
    let (model, tables) = loaded(case)?;
    let db = Database::new(&model, &tables);
    let mut fc = FilterContext::default();
    for f in &case.filters {
        fc = fc.with(filter(&db, f).map_err(|e| format!("malformed `Filter: {f}`: {e}"))?);
    }
    let input = QueryInput {
        db: &db,
        model: &model,
        filters: &fc,
        options: Options {
            cancel: None,
            memory_cap: LIMIT,
        },
    };
    let result = run_query(&visual, &input, 0).map_err(|e| e.to_string())?;
    Ok(long_form(&result))
}

/// `Sales[Region] = North, South`: the Column limited to those values (text).
// ponytail: text values only; numeric slicer cases parse by Column type when one needs it.
fn filter(db: &Database, spec: &str) -> Result<Filter> {
    let (col, values) = spec
        .split_once('=')
        .ok_or("needs `Table[Column] = values`")?;
    let (table, column) = col
        .trim()
        .strip_suffix(']')
        .and_then(|c| c.split_once('['))
        .ok_or("needs `Table[Column]`")?;
    let t = db
        .table(table.trim().trim_matches('\''))
        .ok_or("no such table")?;
    let dt = db.tables.get(t).ok_or("no such table")?;
    let c = dt.column(column).ok_or("no such column")?;
    let values: Vec<Value> = values.split(',').map(|v| Value::text(v.trim())).collect();
    Ok(Filter::values(dt, t, c, &values))
}

/// The result as records: row keys, column keys, values; totals included, keys of a total empty.
fn long_form(r: &QueryResult) -> (Vec<String>, Vec<Vec<String>>) {
    let mut columns: Vec<String> = r.row_groups.iter().map(|h| h.label.clone()).collect();
    columns.extend(r.column_groups.iter().map(|h| h.label.clone()));
    columns.extend(r.values.iter().map(|h| h.label.clone()));
    let key = |k: &Option<Value>| k.as_ref().map(Value::copy_text).unwrap_or_default();
    let mut rows = Vec::new();
    for row in r.rows.iter().chain(&r.total) {
        for (j, ck) in r.column_keys.iter().enumerate() {
            let mut rec: Vec<String> = row.keys.iter().map(key).collect();
            rec.extend(ck.iter().map(key));
            for k in 0..r.values.len() {
                let i = j.saturating_mul(r.values.len()).saturating_add(k);
                rec.push(row.cells.get(i).map(Value::copy_text).unwrap_or_default());
            }
            rows.push(rec);
        }
    }
    (columns, rows)
}

/// The visual JSON of a report file (ADR 0003), the members a query needs: `type`, `wells`,
/// `format.rowSubtotals` / `columnSubtotals`.
fn parse_visual(json: &str) -> Result<Visual> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let kind = v
        .get("type")
        .and_then(|t| t.as_str())
        .and_then(VisualType::from_id)
        .ok_or("`type` isn't a visual type")?;
    let mut visual = Visual::new("case", kind);
    if let Some(wells) = v.get("wells").and_then(|w| w.as_object()) {
        for (name, fields) in wells {
            let well = Well::from_id(name).ok_or(format!("unknown well `{name}`"))?;
            let list = fields.as_array().ok_or("a well holds a list")?;
            let mut out = Vec::new();
            for f in list {
                let s = |k: &str| f.get(k).and_then(|x| x.as_str()).map(str::to_string);
                out.push(match (s("table"), s("column"), s("measure")) {
                    (Some(table), Some(column), None) => FieldRef::Column { table, column },
                    (None, None, Some(name)) => FieldRef::Measure { name },
                    _ => return Err(format!("bad Field {f}")),
                });
            }
            visual.wells.insert(well, out);
        }
    }
    if let Some(fmt) = v.get("format") {
        let flag = |k: &str| fmt.get(k).and_then(|x| x.as_bool());
        if let Some(b) = flag("rowSubtotals") {
            visual.format.row_subtotals = b;
        }
        if let Some(b) = flag("columnSubtotals") {
            visual.format.column_subtotals = b;
        }
    }
    Ok(visual)
}
