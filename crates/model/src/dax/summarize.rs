//! `SUMMARIZECOLUMNS` (M3.9, ADR 0006) and [`summarize`], the group-by query that M4.1's visual
//! queries reuse.
//!
//! **`SUMMARIZECOLUMNS(T[c]…, "Name", expr…)`** (no filter-table arguments yet: one is an error):
//! 1. **Groups.** The group-by columns' value combinations, over the rows the filter context
//!    shows. Columns of one Table combine as the Table's existing rows do; different Tables
//!    cross-join, related or not (case 0045: `Regions[Region], Sales[Product]` with a constant
//!    expression gives all 3 × 3 pairs), and step 3 drops the pairs with nothing. The cross
//!    product is charged to the memory cap before it is built.
//! 2. **Expressions.** Each is evaluated in its group's filter context: the outer filter context
//!    plus one equality filter per group-by column.
//! 3. **Blank rows.** A group whose expressions are all BLANK is dropped, as Power BI does; with
//!    no expressions, every group is kept.
//!
//! Result columns are the group-by columns (named by column, with their lineage, so
//! `ORDER BY T[c]` and context transition find them), then the expressions by name. Power BI
//! copies the headers as `Table[Column]` and `[Name]`; the parity runner matches the name.
//! Without `ORDER BY` rows come in first-seen order (Power BI's order is unspecified).
//!
//! Parity guesses (to settle with cases):
//! - With no expressions, the full cross join is kept (only case 0045's constant is observed).
//! - A one-side Table has no virtual blank row here, so many-side rows whose key has no match
//!   get no group; Power BI's blank row would show them.
//!
//! Performance: each group's expressions run their own scans (groups × rows); a one-pass
//! group-by scan for plain aggregations is a later optimisation (N-3, M4).

use std::collections::HashSet;
use std::rc::Rc;

use opendash_core::Value;
use opendash_dax::{Expr, ExprKind, Span};

use crate::dax::calculate;
use crate::dax::eval::{Evaluator, Rows, RowsColumn, TableVal, Val};
use crate::dax::functions::Call;
use crate::dax::storage::{self, Filter, Key};
use crate::dax::{Database, DaxError, ErrorKind, FilterContext, Options, Result, ResultTable};
use crate::same_name;

/// Bytes one materialised value costs, for the memory cap.
const VALUE_BYTES: usize = std::mem::size_of::<Value>();
/// Bytes one materialised row costs beyond its values.
const ROW_BYTES: usize = std::mem::size_of::<Vec<Value>>();
/// Bytes one hashed key costs (with the set's overhead), for the memory cap.
const KEY_BYTES: usize = 2 * std::mem::size_of::<Key>();

/// A model column by name, for [`summarize`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColumnRef<'n> {
    pub table: &'n str,
    pub column: &'n str,
}

/// The group-by query as a Rust call (M4.1's visual query): `group_by` columns, each `measures`
/// expression per group, under `filters` (e.g. slicers), as `SUMMARIZECOLUMNS` does (module
/// doc). Measures defined in a query aren't in scope: pass their expressions.
pub fn summarize(
    db: &Database,
    group_by: &[ColumnRef<'_>],
    measures: &[(&str, &Expr)],
    filters: &FilterContext,
    options: Options<'_>,
) -> Result<ResultTable> {
    let span = Span::default();
    let mut columns = Vec::with_capacity(group_by.len());
    for c in group_by {
        let t = db
            .table(c.table)
            .ok_or_else(|| DaxError::new(span, format!("there is no Table '{}'", c.table)))?;
        let col = db
            .tables
            .get(t)
            .and_then(|dt| dt.column(c.column))
            .ok_or_else(|| {
                DaxError::new(
                    span,
                    format!("there is no Column '{}'[{}]", c.table, c.column),
                )
            })?;
        columns.push((t, col));
    }
    check_names(db, &columns, measures.iter().map(|(n, _)| (*n, span)))?;
    let mut ev = Evaluator::new(db, options);
    let rows = summarize_rows(&mut ev, &columns, measures, filters, span)?;
    Ok(ResultTable {
        columns: rows.columns.into_iter().map(|c| c.name).collect(),
        rows: rows.rows,
    })
}

/// `SUMMARIZECOLUMNS(groupBy…, "Name", expr…)`.
pub fn summarizecolumns(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, usize::MAX)?;
    let mut group_by = Vec::new();
    let mut first_name = call.args.len();
    for (i, arg) in call.args.iter().enumerate() {
        match &arg.kind {
            ExprKind::Text(_) => {
                first_name = i;
                break;
            }
            ExprKind::Column { table: Some(_), .. } => {
                group_by.push(calculate::column_ref(ev, arg)?);
            }
            ExprKind::Column {
                table: None,
                column,
            } => {
                return Err(DaxError::new(
                    arg.span,
                    format!(
                        "a SUMMARIZECOLUMNS group-by column needs its Table: 'Table'[{column}]"
                    ),
                ));
            }
            _ => {
                return Err(DaxError::new(
                    arg.span,
                    "OpenDash BI doesn't support SUMMARIZECOLUMNS filter-table arguments (yet): \
                     give group-by columns, then \"Name\", expression pairs",
                ));
            }
        }
    }
    let rest = call.args.get(first_name..).unwrap_or(&[]);
    if !rest.len().is_multiple_of(2) {
        return Err(DaxError::new(
            call.span,
            "SUMMARIZECOLUMNS takes its expressions as pairs of a name and an expression",
        ));
    }
    let mut measures: Vec<(&str, &Expr)> = Vec::with_capacity(rest.len() / 2);
    for pair in rest.chunks(2) {
        let [name, expr] = pair else {
            continue;
        };
        let ExprKind::Text(n) = &name.kind else {
            return Err(DaxError::new(
                name.span,
                "a SUMMARIZECOLUMNS expression name must be text in quotes (\"Name\"), after \
                 every group-by column",
            ));
        };
        measures.push((n.as_str(), expr));
    }
    let spans = rest.chunks(2).filter_map(|p| p.first()).map(|e| e.span);
    check_names(
        ev.db,
        &group_by,
        measures.iter().map(|(n, _)| *n).zip(spans),
    )?;
    let rows = summarize_rows(ev, &group_by, &measures, call.fc, call.span)?;
    Ok(Val::Table(Rc::new(TableVal::Rows(rows))))
}

/// Errors on a group-by column given twice or two expressions of one name.
fn check_names<'n>(
    db: &Database,
    group_by: &[(usize, usize)],
    names: impl Iterator<Item = (&'n str, Span)>,
) -> Result<()> {
    for (i, c) in group_by.iter().enumerate() {
        if group_by.get(..i).is_some_and(|before| before.contains(c)) {
            let name = db
                .tables
                .get(c.0)
                .and_then(|t| Some(format!("'{}'[{}]", t.name, t.columns.get(c.1)?.name)))
                .unwrap_or_default();
            return Err(DaxError::new(
                Span::default(),
                format!("SUMMARIZECOLUMNS groups by {name} twice"),
            ));
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    for (n, span) in names {
        if seen.iter().any(|s| same_name(s, n)) {
            return Err(DaxError::new(
                span,
                format!("SUMMARIZECOLUMNS has two expressions named \"{n}\""),
            ));
        }
        seen.push(n);
    }
    Ok(())
}

/// The group-by query under the evaluator: the groups of `group_by` (model (Table, Column)s)
/// visible under `fc`, each with its `measures` (module doc).
pub fn summarize_rows(
    ev: &mut Evaluator<'_, '_>,
    group_by: &[(usize, usize)],
    measures: &[(&str, &Expr)],
    fc: &FilterContext,
    span: Span,
) -> Result<Rows> {
    let db = ev.db;
    let mut columns: Vec<RowsColumn> = group_by
        .iter()
        .map(|&(t, c)| RowsColumn {
            name: db
                .tables
                .get(t)
                .and_then(|dt| dt.columns.get(c))
                .map_or_else(String::new, |col| col.name.clone()),
            lineage: Some((t, c)),
        })
        .collect();
    columns.extend(measures.iter().map(|(n, _)| RowsColumn::new(n)));
    let width = columns.len();
    let groups = groups(ev, group_by, fc, width, span)?;
    if measures.is_empty() {
        return Ok(Rows {
            columns,
            rows: groups,
        });
    }
    let mut rows = Vec::new();
    for (i, mut group) in groups.into_iter().enumerate() {
        if i % storage::CHUNK == 0 {
            ev.cancel.check()?;
        }
        let mut group_fc = fc.clone();
        for (&(t, c), v) in group_by.iter().zip(&group) {
            if let Some(dt) = db.tables.get(t) {
                group_fc = group_fc.with(Filter::values(dt, t, c, std::slice::from_ref(v)));
            }
        }
        let mut values = Vec::with_capacity(measures.len());
        for (_, e) in measures {
            values.push(ev.scalar(e, &group_fc)?);
        }
        if values.iter().all(Value::is_blank) {
            continue;
        }
        let text = values.iter().fold(0usize, |a, v| match v {
            Value::Text(s) => a.saturating_add(s.len()),
            _ => a,
        });
        ev.charge(text, span)?;
        group.extend(values);
        rows.push(group);
    }
    Ok(Rows { columns, rows })
}

/// The group combinations of `group_by` under `fc`, in `group_by` order: each Table's existing
/// tuples of its group-by columns, cross-joined across Tables; every row is charged for `width`
/// values.
fn groups(
    ev: &mut Evaluator<'_, '_>,
    group_by: &[(usize, usize)],
    fc: &FilterContext,
    width: usize,
    span: Span,
) -> Result<Vec<Vec<Value>>> {
    // Each Table with the positions in `group_by` of its columns.
    let mut sets: Vec<(usize, Vec<usize>)> = Vec::new();
    for (k, &(t, _)) in group_by.iter().enumerate() {
        match sets.iter_mut().find(|(u, _)| *u == t) {
            Some((_, ks)) => ks.push(k),
            None => sets.push((t, vec![k])),
        }
    }

    // Each set's distinct tuples, then their cross product.
    let mut parts: Vec<Vec<Vec<Value>>> = Vec::with_capacity(sets.len());
    for (t, ks) in &sets {
        parts.push(tuples(ev, *t, ks, group_by, fc, span)?);
    }
    let n = parts
        .iter()
        .try_fold(1usize, |a, p| a.checked_mul(p.len()))
        .ok_or_else(|| too_many(span))?;
    let row_bytes = width.saturating_mul(VALUE_BYTES).saturating_add(ROW_BYTES);
    ev.charge(n.saturating_mul(row_bytes), span)?;
    // Where each group-by column is: (set, position in the set's tuple).
    let mut place = vec![(0usize, 0usize); group_by.len()];
    for (s, (_, ks)) in sets.iter().enumerate() {
        for (pos, k) in ks.iter().enumerate() {
            if let Some(p) = place.get_mut(*k) {
                *p = (s, pos);
            }
        }
    }
    let mut out = Vec::with_capacity(n);
    let mut digits = vec![0usize; parts.len()];
    for i in 0..n {
        if i % storage::CHUNK == 0 {
            ev.cancel.check()?;
        }
        let mut row = Vec::with_capacity(width);
        for &(s, pos) in &place {
            let v = digits
                .get(s)
                .and_then(|&d| parts.get(s)?.get(d)?.get(pos))
                .cloned()
                .unwrap_or(Value::Blank);
            row.push(v);
        }
        out.push(row);
        // Odometer: the last set turns fastest.
        for (d, part) in digits.iter_mut().zip(&parts).rev() {
            *d += 1;
            if *d < part.len() {
                break;
            }
            *d = 0;
        }
    }
    Ok(out)
}

/// The distinct value tuples of Table `t`'s group-by columns (`ks`, positions in `group_by`)
/// over its visible rows, in first-seen order; charged to the memory cap.
fn tuples(
    ev: &mut Evaluator<'_, '_>,
    t: usize,
    ks: &[usize],
    group_by: &[(usize, usize)],
    fc: &FilterContext,
    span: Span,
) -> Result<Vec<Vec<Value>>> {
    let db = ev.db;
    let Some(table) = db.tables.get(t) else {
        return Ok(Vec::new());
    };
    let cols: Vec<_> = ks
        .iter()
        .filter_map(|k| table.columns.get(group_by.get(*k)?.1))
        .collect();
    let mask = ev.mask(t, fc, span)?;
    let mut seen: HashSet<Vec<Option<Key>>> = HashSet::new();
    let mut out = Vec::new();
    let tuple_bytes = cols
        .len()
        .saturating_mul(VALUE_BYTES.saturating_add(KEY_BYTES))
        .saturating_add(ROW_BYTES);
    let cancel = ev.cancel;
    storage::scan(table.rows, mask.as_ref(), cancel, |r| {
        let values: Vec<Value> = cols.iter().map(|c| c.data.value(r)).collect();
        let key: Vec<Option<Key>> = values.iter().map(|v| Key::of(v, false)).collect();
        if seen.insert(key) {
            let bytes = values.iter().fold(tuple_bytes, |b, v| match v {
                Value::Text(s) => b.saturating_add(s.len().saturating_mul(2)),
                _ => b,
            });
            ev.charge(bytes, span)?;
            out.push(values);
        }
        Ok(())
    })?;
    Ok(out)
}

#[cold]
fn too_many(span: Span) -> DaxError {
    DaxError {
        kind: ErrorKind::Memory,
        span,
        message: "SUMMARIZECOLUMNS would combine more groups than memory can hold".into(),
    }
}

#[cfg(test)]
#[path = "summarize_tests.rs"]
mod tests;
