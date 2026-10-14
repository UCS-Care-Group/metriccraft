//! `CALCULATE`, `CALCULATETABLE`, `FILTER`, `ALL`, `ALLEXCEPT`, `REMOVEFILTERS`, `RELATED` and
//! context transition (M3.4, ADR 0006).
//!
//! **`CALCULATE(expr, args…)`** runs in DAX's order:
//! 1. every filter argument is evaluated in the caller's filter context;
//! 2. context transition: each row of the row context becomes equality filters on every column
//!    of its Table (and, for a materialised row, on the columns that have a model lineage);
//! 3. modifiers (`ALL`, `ALLEXCEPT`, `REMOVEFILTERS` written directly as an argument) remove
//!    filters;
//! 4. each filter argument removes the filters on its columns, then all of them are added
//!    (so two arguments on one column AND together, and an inner filter replaces an outer one);
//! 5. `expr` is evaluated in the new filter context.
//!
//! A **boolean argument** (`T[c] = v`, `T[c] = 1 || T[c] = 2`) is `FILTER(ALL(T[c]…), arg)`
//! over the columns it references (one Table only): a single-column value filter, or a row-set
//! filter of the Table when it names several of its columns. A **table argument** is a filter:
//! a model Table's rows (from `FILTER(T, …)` or `T`) filter the *expanded* Table — the one side
//! of its active Relationships is restricted to the rows it reaches ([`Evaluator::mask`]) —
//! and replace the filters on the expanded Table's columns; a materialised table (`ALL(T[c])`,
//! `FILTER(ALL(T[c]), …)`) filters the columns its values came from.
//!
//! **`T[C] IN table`** (and `NOT (T[C] IN table)`) as an argument (case 0035 `InFilter`) is that
//! boolean filter too, but `table` is evaluated once, in the caller's filter context, and `T[C]`
//! keeps the values `IN` finds in it ([`logic::contains`]: strict `==`, text ignoring case). A
//! `table` that reads `T[C]` itself takes the general path.
//!
//! Removing filters from some columns of a row-set filter keeps its projection on the other
//! columns of its Table (as DAX does), as a value filter (one column) or a row set.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use opendash_core::Value;
use opendash_dax::{BinOp, Expr, ExprKind, Span, UnaryOp};

use super::eval::{Evaluator, RowFrame, Rows, RowsColumn, TableVal, Val, value_error};
use super::functions::{Call, logic};
use super::storage::{self, Bitset, Filter, Key};
use super::{DaxError, FilterContext, MAX_EVAL_DEPTH, MAX_RELATIONSHIP_DEPTH, Result};

/// Bytes one materialised value costs, for the memory cap.
const VALUE_BYTES: usize = std::mem::size_of::<Value>();
/// Bytes one hashed key costs (with the set's overhead), for the memory cap.
const KEY_BYTES: usize = 2 * std::mem::size_of::<Key>();

/// Functions that return a table: a `CALCULATE` argument calling one is a table filter.
const TABLE_FUNCTIONS: &[&str] = &[
    "filter",
    "all",
    "allexcept",
    "calculatetable",
    "values",
    "distinct",
    "row",
    "relatedtable",
    "summarize",
    "summarizecolumns",
    "addcolumns",
    "selectcolumns",
    "topn",
    "crossjoin",
    "union",
    "intersect",
    "except",
    "treatas",
    "datatable",
    "generateseries",
];

/// Filter-removing `CALCULATE` arguments.
const MODIFIERS: &[&str] = &["all", "allexcept", "removefilters"];

/// Which columns a modifier or a new filter clears the filters from.
#[derive(Clone, Debug, Default)]
struct Removed {
    /// Every column of these Tables…
    tables: Vec<usize>,
    /// …except these (Table, Column)s;
    except: Vec<(usize, usize)>,
    /// and these (Table, Column)s.
    columns: Vec<(usize, usize)>,
}

impl Removed {
    fn removes(&self, table: usize, column: usize) -> bool {
        (self.tables.contains(&table) && !self.except.contains(&(table, column)))
            || self.columns.contains(&(table, column))
    }
}

/// A filter a `CALCULATE` argument adds, and the filters it replaces.
struct NewFilter {
    replaces: Removed,
    filter: Option<Filter>,
}

/// `CALCULATE(expr, filters…)`: `expr` (a scalar) in the modified filter context.
pub fn calculate(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, usize::MAX)?;
    let fc = calculate_context(ev, call)?;
    let v = ev.scalar(call.arg(0)?, &fc)?;
    Ok(Val::Scalar(v))
}

/// `CALCULATETABLE(table, filters…)`: `table` in the modified filter context.
pub fn calculatetable(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, usize::MAX)?;
    let fc = calculate_context(ev, call)?;
    // A model Table's rows are computed under the new context: nothing is materialised.
    Ok(Val::Table(ev.table(call.arg(0)?, &fc)?))
}

/// The filter context `CALCULATE`'s arguments 2… build from `call.fc` (see the module doc).
#[inline(never)]
fn calculate_context(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<FilterContext> {
    let mut modifiers = Vec::new();
    let mut filters = Vec::new();
    for arg in call.args.iter().skip(1) {
        match classify(arg) {
            Kind::Modifier => modifiers.push(modifier(ev, arg)?),
            Kind::Table => filters.push(table_filter(ev, arg, call.fc)?),
            Kind::Boolean => filters.push(boolean_filter(ev, arg, call.fc)?),
        }
    }
    let mut fc = if ev.row_frames().is_empty() {
        call.fc.clone()
    } else {
        transition(ev, call.fc, call.span)?
    };
    for m in &modifiers {
        fc = remove(ev, &fc, m, call.span)?;
    }
    for f in &filters {
        fc = remove(ev, &fc, &f.replaces, call.span)?;
    }
    for f in filters {
        if let Some(filter) = f.filter {
            fc = fc.with(filter);
        }
    }
    Ok(fc)
}

enum Kind {
    Modifier,
    Table,
    Boolean,
}

fn classify(e: &Expr) -> Kind {
    match &e.kind {
        ExprKind::Call { name, .. } => {
            let lower = name.to_ascii_lowercase();
            if MODIFIERS.contains(&lower.as_str()) {
                Kind::Modifier
            } else if TABLE_FUNCTIONS.contains(&lower.as_str()) {
                Kind::Table
            } else {
                Kind::Boolean
            }
        }
        ExprKind::Name(_) | ExprKind::Table(_) => Kind::Table,
        _ => Kind::Boolean,
    }
}

/// Context transition: `fc` with every row of the row context turned into equality filters on
/// its columns (replacing the filters on them), outermost row first.
#[inline(never)]
pub fn transition(
    ev: &mut Evaluator<'_, '_>,
    fc: &FilterContext,
    span: Span,
) -> Result<FilterContext> {
    let frames: Vec<RowFrame> = ev.row_frames().to_vec();
    let mut fc = fc.clone();
    for frame in frames {
        let mut cells: Vec<(usize, usize, Value)> = Vec::new();
        match &frame {
            RowFrame::Base { table, row } => {
                if let Some(t) = ev.db.tables.get(*table) {
                    for (c, col) in t.columns.iter().enumerate() {
                        cells.push((*table, c, col.data.value(*row)));
                    }
                }
            }
            RowFrame::Rows { table, row } => {
                if let TableVal::Rows(rows) = table.as_ref() {
                    let values = rows.rows.get(*row);
                    for (i, col) in rows.columns.iter().enumerate() {
                        if let (Some((t, c)), Some(v)) =
                            (col.lineage, values.and_then(|r| r.get(i)))
                        {
                            cells.push((t, c, v.clone()));
                        }
                    }
                }
            }
        }
        let removed = Removed {
            columns: cells.iter().map(|(t, c, _)| (*t, *c)).collect(),
            ..Removed::default()
        };
        fc = remove(ev, &fc, &removed, span)?;
        for (t, c, v) in cells {
            if let Some(dt) = ev.db.tables.get(t) {
                fc = fc.with(Filter::values(dt, t, c, std::slice::from_ref(&v)));
            }
        }
    }
    Ok(fc)
}

/// `fc` without the filters `removed` covers; a row-set filter losing only some of its Table's
/// columns keeps its projection on the rest.
fn remove(
    ev: &mut Evaluator<'_, '_>,
    fc: &FilterContext,
    removed: &Removed,
    span: Span,
) -> Result<FilterContext> {
    let mut out = FilterContext::default();
    for f in &fc.filters {
        match f.as_ref() {
            Filter::Column { table, column, .. } => {
                if !removed.removes(*table, *column) {
                    out.filters.push(Arc::clone(f));
                }
            }
            Filter::Rows { table, rows } => {
                let width = ev.db.tables.get(*table).map_or(0, |t| t.columns.len());
                let kept: Vec<usize> = (0..width)
                    .filter(|&c| !removed.removes(*table, c))
                    .collect();
                if kept.len() == width {
                    out.filters.push(Arc::clone(f));
                } else if !kept.is_empty() {
                    out = out.with(project(ev, *table, rows, &kept, span)?);
                }
            }
        }
    }
    Ok(out)
}

/// The filter on `kept` columns of `table` that row set `rows` implies: the rows whose values
/// in those columns appear among `rows`.
fn project(
    ev: &mut Evaluator<'_, '_>,
    table: usize,
    rows: &Bitset,
    kept: &[usize],
    span: Span,
) -> Result<Filter> {
    let Some(t) = ev.db.tables.get(table) else {
        return Ok(Filter::Rows {
            table,
            rows: rows.clone(),
        });
    };
    let tuples = distinct(ev, table, kept, Some(rows), span)?;
    if let [c] = kept {
        let values: Vec<Value> = tuples.into_iter().filter_map(|mut r| r.pop()).collect();
        return Ok(Filter::values(t, table, *c, &values));
    }
    let keys: HashSet<Vec<Option<Key>>> = tuples.iter().map(|r| tuple_key(r)).collect();
    Ok(Filter::Rows {
        table,
        rows: matching_rows(ev, table, kept, &keys)?,
    })
}

fn tuple_key(values: &[Value]) -> Vec<Option<Key>> {
    values.iter().map(|v| Key::of(v, false)).collect()
}

/// The rows of `table` (all of them) whose `columns` tuple is in `keys`.
fn matching_rows(
    ev: &Evaluator<'_, '_>,
    table: usize,
    columns: &[usize],
    keys: &HashSet<Vec<Option<Key>>>,
) -> Result<Bitset> {
    let Some(t) = ev.db.tables.get(table) else {
        return Ok(Bitset::new(0, false));
    };
    let mut out = Bitset::new(t.rows, false);
    storage::scan(t.rows, None, ev.cancel, |r| {
        let key: Vec<Option<Key>> = columns
            .iter()
            .map(|&c| {
                t.columns
                    .get(c)
                    .and_then(|col| Key::of(&col.data.value(r), false))
            })
            .collect();
        if keys.contains(&key) {
            out.set(r);
        }
        Ok(())
    })?;
    Ok(out)
}

/// The distinct value tuples of `columns` of `table` over the rows in `mask` (all: `None`), in
/// first-seen order; charged to the memory cap.
fn distinct(
    ev: &mut Evaluator<'_, '_>,
    table: usize,
    columns: &[usize],
    mask: Option<&Bitset>,
    span: Span,
) -> Result<Vec<Vec<Value>>> {
    let Some(t) = ev.db.tables.get(table) else {
        return Ok(Vec::new());
    };
    let mut seen: HashSet<Vec<Option<Key>>> = HashSet::new();
    let mut out = Vec::new();
    let mut bytes = 0usize;
    storage::scan(t.rows, mask, ev.cancel, |r| {
        let values: Vec<Value> = columns
            .iter()
            .map(|&c| {
                t.columns
                    .get(c)
                    .map_or(Value::Blank, |col| col.data.value(r))
            })
            .collect();
        if seen.insert(tuple_key(&values)) {
            bytes = bytes.saturating_add(
                columns
                    .len()
                    .saturating_mul(VALUE_BYTES.saturating_add(KEY_BYTES)),
            );
            bytes = values.iter().fold(bytes, |b, v| match v {
                Value::Text(s) => b.saturating_add(s.len().saturating_mul(2)),
                _ => b,
            });
            out.push(values);
        }
        Ok(())
    })?;
    ev.charge(bytes, span)?;
    Ok(out)
}

/// `(Table, Column)` of a written `T[C]`.
fn column_ref(ev: &Evaluator<'_, '_>, e: &Expr) -> Result<(usize, usize)> {
    let ExprKind::Column {
        table: Some(table),
        column,
    } = &e.kind
    else {
        return Err(DaxError::new(
            e.span,
            "a column is needed here, written with its Table: 'Table'[Column]",
        ));
    };
    let t = ev
        .db
        .table(table)
        .ok_or_else(|| DaxError::new(e.span, format!("there is no Table '{table}'")))?;
    let c = ev
        .db
        .tables
        .get(t)
        .and_then(|dt| dt.column(column))
        .ok_or_else(|| DaxError::new(e.span, format!("there is no Column '{table}'[{column}]")))?;
    Ok((t, c))
}

/// A model Table written by name (`Sales`, `'Sales'`).
fn table_ref(ev: &Evaluator<'_, '_>, e: &Expr, what: &str) -> Result<usize> {
    match &e.kind {
        ExprKind::Name(n) => ev
            .db
            .table(n)
            .ok_or_else(|| DaxError::new(e.span, format!("there is no Table '{n}'"))),
        _ => Err(DaxError::new(
            e.span,
            format!("{what} needs a Table here, written by name"),
        )),
    }
}

/// The Tables `table` expands to: itself and every Table it reaches through active
/// Relationships, many → one.
fn expanded(ev: &Evaluator<'_, '_>, table: usize, span: Span) -> Result<Vec<usize>> {
    let mut out = vec![table];
    let mut next = 0;
    while let Some(&t) = out.get(next) {
        next += 1;
        if next > MAX_RELATIONSHIP_DEPTH {
            return Err(DaxError::limit(
                span,
                format!("Relationships chain more than {MAX_RELATIONSHIP_DEPTH} Tables"),
            ));
        }
        for r in &ev.db.relationships {
            if r.active && r.from_table == t && !out.contains(&r.to_table) {
                out.push(r.to_table);
            }
        }
    }
    Ok(out)
}

/// What a modifier argument (`ALL(…)`, `ALLEXCEPT(…)`, `REMOVEFILTERS(…)`) removes.
fn modifier(ev: &Evaluator<'_, '_>, e: &Expr) -> Result<Removed> {
    let ExprKind::Call { name, args } = &e.kind else {
        return Ok(Removed::default());
    };
    let upper = name.to_uppercase();
    let Some(first) = args.first() else {
        if upper == "ALLEXCEPT" {
            return Err(DaxError::new(e.span, "ALLEXCEPT needs a Table"));
        }
        // ALL() / REMOVEFILTERS(): every filter.
        return Ok(Removed {
            tables: (0..ev.db.tables.len()).collect(),
            ..Removed::default()
        });
    };
    if upper == "ALLEXCEPT" {
        let table = table_ref(ev, first, "ALLEXCEPT")?;
        let except = args
            .iter()
            .skip(1)
            .map(|a| column_ref(ev, a))
            .collect::<Result<Vec<_>>>()?;
        return Ok(Removed {
            tables: expanded(ev, table, e.span)?,
            except,
            columns: Vec::new(),
        });
    }
    if let ExprKind::Name(_) = first.kind {
        if args.len() > 1 {
            return Err(DaxError::new(
                e.span,
                format!("{upper} takes one Table, or columns of one Table"),
            ));
        }
        let table = table_ref(ev, first, &upper)?;
        return Ok(Removed {
            tables: expanded(ev, table, e.span)?,
            ..Removed::default()
        });
    }
    let columns = same_table_columns(ev, args, &upper)?.1;
    Ok(Removed {
        columns,
        ..Removed::default()
    })
}

/// Resolves column arguments that must all be of one Table.
fn same_table_columns(
    ev: &Evaluator<'_, '_>,
    args: &[Expr],
    what: &str,
) -> Result<(usize, Vec<(usize, usize)>)> {
    let mut cols: Vec<(usize, usize)> = Vec::with_capacity(args.len());
    for a in args {
        let tc = column_ref(ev, a)?;
        if cols.first().is_some_and(|(t, _)| *t != tc.0) {
            return Err(DaxError::new(
                a.span,
                format!("{what}'s columns must all be of one Table"),
            ));
        }
        if !cols.contains(&tc) {
            cols.push(tc);
        }
    }
    let table = cols.first().map_or(0, |(t, _)| *t);
    Ok((table, cols))
}

/// A table `CALCULATE` argument as a filter.
fn table_filter(ev: &mut Evaluator<'_, '_>, e: &Expr, fc: &FilterContext) -> Result<NewFilter> {
    let t = ev.table(e, fc)?;
    match t.as_ref() {
        TableVal::Base { table, rows } => {
            let n = ev.db.tables.get(*table).map_or(0, |t| t.rows);
            Ok(NewFilter {
                replaces: Removed {
                    tables: expanded(ev, *table, e.span)?,
                    ..Removed::default()
                },
                filter: Some(Filter::Rows {
                    table: *table,
                    rows: rows.clone().unwrap_or_else(|| Bitset::new(n, true)),
                }),
            })
        }
        TableVal::Rows(rows) => rows_filter(ev, rows, e.span),
    }
}

/// A materialised table as a filter on the columns its values came from.
fn rows_filter(ev: &mut Evaluator<'_, '_>, rows: &Rows, span: Span) -> Result<NewFilter> {
    let mut cols: Vec<(usize, usize)> = Vec::with_capacity(rows.columns.len());
    for c in &rows.columns {
        let Some(tc) = c.lineage else {
            return Err(DaxError::new(
                span,
                format!(
                    "a filter table's columns must come from model Tables; [{}] doesn't",
                    c.name
                ),
            ));
        };
        if cols.first().is_some_and(|(t, _)| *t != tc.0) {
            return Err(DaxError::new(
                span,
                "a filter table with columns of several Tables isn't supported (yet)",
            ));
        }
        cols.push(tc);
    }
    let Some(&(table, first)) = cols.first() else {
        return Err(DaxError::new(span, "a filter table needs a column"));
    };
    let Some(dt) = ev.db.tables.get(table) else {
        return Err(DaxError::new(span, "unknown Table"));
    };
    let filter = if cols.len() == 1 {
        let values: Vec<Value> = rows
            .rows
            .iter()
            .filter_map(|r| r.first().cloned())
            .collect();
        Filter::values(dt, table, first, &values)
    } else {
        let columns: Vec<usize> = cols.iter().map(|(_, c)| *c).collect();
        let keys: HashSet<Vec<Option<Key>>> = rows.rows.iter().map(|r| tuple_key(r)).collect();
        ev.charge(
            keys.len()
                .saturating_mul(columns.len().saturating_mul(KEY_BYTES)),
            span,
        )?;
        Filter::Rows {
            table,
            rows: matching_rows(ev, table, &columns, &keys)?,
        }
    };
    Ok(NewFilter {
        replaces: Removed {
            columns: cols,
            ..Removed::default()
        },
        filter: Some(filter),
    })
}

/// A boolean `CALCULATE` argument: `FILTER(ALL(columns), e)` over the columns it references.
fn boolean_filter(ev: &mut Evaluator<'_, '_>, e: &Expr, fc: &FilterContext) -> Result<NewFilter> {
    if let Some(f) = in_filter(ev, e, fc)? {
        return Ok(f);
    }
    let mut refs = Vec::new();
    column_refs(ev, e, &mut refs, 0)?;
    let Some(&(table, _)) = refs.first() else {
        return Err(DaxError::new(
            e.span,
            "a CALCULATE filter must be a table, or a condition on columns ('Table'[Column] = …)",
        ));
    };
    if refs.iter().any(|(t, _)| *t != table) {
        return Err(DaxError::new(
            e.span,
            "a CALCULATE condition can use the columns of only one Table: use FILTER for more",
        ));
    }
    let columns: Vec<usize> = refs.iter().map(|(_, c)| *c).collect();
    let tuples = distinct(ev, table, &columns, None, e.span)?;
    let names: Vec<RowsColumn> = columns
        .iter()
        .map(|&c| RowsColumn {
            name: ev
                .db
                .tables
                .get(table)
                .and_then(|t| t.columns.get(c))
                .map_or_else(String::new, |col| col.name.clone()),
            lineage: Some((table, c)),
        })
        .collect();
    let candidates = Rows {
        columns: names,
        rows: tuples,
    };
    let kept = keep_rows(ev, candidates, e, fc)?;
    let filter = match columns.as_slice() {
        [c] => {
            let values: Vec<Value> = kept.into_iter().filter_map(|mut r| r.pop()).collect();
            ev.db
                .tables
                .get(table)
                .map(|dt| Filter::values(dt, table, *c, &values))
        }
        _ => {
            let keys: HashSet<Vec<Option<Key>>> = kept.iter().map(|r| tuple_key(r)).collect();
            Some(Filter::Rows {
                table,
                rows: matching_rows(ev, table, &columns, &keys)?,
            })
        }
    };
    Ok(NewFilter {
        replaces: Removed {
            columns: refs,
            ..Removed::default()
        },
        filter,
    })
}

/// `T[C] IN table` or `NOT (T[C] IN table)` as a filter on `T[C]` (see the module doc); `None`
/// for any other argument.
fn in_filter(
    ev: &mut Evaluator<'_, '_>,
    e: &Expr,
    fc: &FilterContext,
) -> Result<Option<NewFilter>> {
    let (negated, test) = match &e.kind {
        ExprKind::Unary(UnaryOp::Not, x) => (true, x.as_ref()),
        _ => (false, e),
    };
    let ExprKind::Binary(BinOp::In, l, r) = &test.kind else {
        return Ok(None);
    };
    if !matches!(l.kind, ExprKind::Column { table: Some(_), .. }) {
        return Ok(None);
    }
    let Ok((table, column)) = column_ref(ev, l) else {
        return Ok(None);
    };
    let mut inside = Vec::new();
    column_refs(ev, r, &mut inside, 0)?;
    if inside.contains(&(table, column)) {
        return Ok(None);
    }
    let values = ev.table(r, fc)?;
    let candidates = distinct(ev, table, &[column], None, e.span)?;
    let mut kept = Vec::new();
    for (i, mut row) in candidates.into_iter().enumerate() {
        if i % storage::CHUNK == 0 {
            ev.cancel.check()?;
        }
        if let Some(v) = row.pop()
            && logic::contains(ev, &values, std::slice::from_ref(&v), test.span)? != negated
        {
            kept.push(v);
        }
    }
    Ok(Some(NewFilter {
        replaces: Removed {
            columns: vec![(table, column)],
            ..Removed::default()
        },
        filter: ev
            .db
            .tables
            .get(table)
            .map(|dt| Filter::values(dt, table, column, &kept)),
    }))
}

/// The rows of `rows` for which `cond` is TRUE, each evaluated in its row context under `fc`.
fn keep_rows(
    ev: &mut Evaluator<'_, '_>,
    rows: Rows,
    cond: &Expr,
    fc: &FilterContext,
) -> Result<Vec<Vec<Value>>> {
    let n = rows.rows.len();
    let shared = Rc::new(TableVal::Rows(rows));
    let mut keep = Vec::with_capacity(n);
    for row in 0..n {
        if row % storage::CHUNK == 0 {
            ev.cancel.check()?;
        }
        let frame = RowFrame::Rows {
            table: Rc::clone(&shared),
            row,
        };
        let v = ev.with_row(frame, |ev| ev.scalar(cond, fc))?;
        keep.push(v.to_bool().map_err(|err| value_error(cond.span, &err))?);
    }
    let TableVal::Rows(rows) = shared.as_ref() else {
        return Ok(Vec::new());
    };
    Ok(rows
        .rows
        .iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(r, _)| r.clone())
        .collect())
}

/// The model columns `T[C]` that `e` references, outside table-function arguments.
fn column_refs(
    ev: &Evaluator<'_, '_>,
    e: &Expr,
    out: &mut Vec<(usize, usize)>,
    depth: usize,
) -> Result<()> {
    if depth >= MAX_EVAL_DEPTH {
        return Err(DaxError::limit(e.span, "the condition nests too deep"));
    }
    match &e.kind {
        ExprKind::Column {
            table: Some(table),
            column,
        } => {
            if let Some(t) = ev.db.table(table)
                && let Some(c) = ev.db.tables.get(t).and_then(|dt| dt.column(column))
                && !out.contains(&(t, c))
            {
                out.push((t, c));
            }
        }
        ExprKind::Call { name, args } => {
            if !TABLE_FUNCTIONS.contains(&name.to_ascii_lowercase().as_str()) {
                for a in args {
                    column_refs(ev, a, out, depth + 1)?;
                }
            }
        }
        ExprKind::Unary(_, x) => column_refs(ev, x, out, depth + 1)?,
        ExprKind::Binary(_, l, r) => {
            column_refs(ev, l, out, depth + 1)?;
            column_refs(ev, r, out, depth + 1)?;
        }
        ExprKind::Var { vars, body } => {
            for (_, x) in vars {
                column_refs(ev, x, out, depth + 1)?;
            }
            column_refs(ev, body, out, depth + 1)?;
        }
        ExprKind::Table(rows) => {
            for x in rows.iter().flatten() {
                column_refs(ev, x, out, depth + 1)?;
            }
        }
        ExprKind::Column { table: None, .. }
        | ExprKind::Integer(_)
        | ExprKind::Decimal(_)
        | ExprKind::Text(_)
        | ExprKind::Name(_) => {}
    }
    Ok(())
}

/// `FILTER(table, condition)`: the rows of `table` for which `condition`, evaluated in each
/// row's context, is TRUE. A model Table stays a row set of it (so it filters as its expanded
/// Table in `CALCULATE`).
pub fn filter(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(2, 2)?;
    let cond = call.arg(1)?;
    let t = ev.table(call.arg(0)?, call.fc)?;
    match t.as_ref() {
        TableVal::Base { table, rows } => {
            let (table, n) = (*table, ev.db.tables.get(*table).map_or(0, |t| t.rows));
            ev.charge(n / 8, call.span)?;
            let mut out = Bitset::new(n, false);
            let cancel = ev.cancel;
            storage::scan(n, rows.as_ref(), cancel, |row| {
                let v =
                    ev.with_row(RowFrame::Base { table, row }, |ev| ev.scalar(cond, call.fc))?;
                if v.to_bool().map_err(|e| value_error(cond.span, &e))? {
                    out.set(row);
                }
                Ok(())
            })?;
            Ok(Val::Table(Rc::new(TableVal::Base {
                table,
                rows: Some(out),
            })))
        }
        TableVal::Rows(rows) => {
            let columns = rows.columns.clone();
            let kept = keep_rows(ev, rows.clone(), cond, call.fc)?;
            Ok(Val::Table(Rc::new(TableVal::Rows(Rows {
                columns,
                rows: kept,
            }))))
        }
    }
}

/// `ALL(T)`: every row of `T`; `ALL(T[a], T[b]…)`: the distinct values of those columns,
/// ignoring filters; `ALL()` only as a `CALCULATE` modifier.
pub fn all(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, usize::MAX)?;
    let first = call.arg(0)?;
    if let ExprKind::Name(_) = first.kind {
        call.arity(1, 1)?;
        let table = table_ref(ev, first, "ALL")?;
        return Ok(Val::Table(Rc::new(TableVal::Base { table, rows: None })));
    }
    let (table, cols) = same_table_columns(ev, call.args, "ALL")?;
    let columns: Vec<usize> = cols.iter().map(|(_, c)| *c).collect();
    distinct_table(ev, table, &columns, call.span)
}

/// `ALLEXCEPT(T, T[a]…)` as a table: the distinct values of `T`'s other columns, ignoring
/// filters. (As a `CALCULATE` modifier it removes the filters on the expanded `T` but those
/// columns.)
pub fn allexcept(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(2, usize::MAX)?;
    let table = table_ref(ev, call.arg(0)?, "ALLEXCEPT")?;
    let except = call
        .args
        .iter()
        .skip(1)
        .map(|a| column_ref(ev, a))
        .collect::<Result<Vec<_>>>()?;
    let width = ev.db.tables.get(table).map_or(0, |t| t.columns.len());
    let columns: Vec<usize> = (0..width)
        .filter(|c| !except.contains(&(table, *c)))
        .collect();
    distinct_table(ev, table, &columns, call.span)
}

/// `REMOVEFILTERS` is only a `CALCULATE` modifier.
pub fn removefilters(_: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    Err(DaxError::new(
        call.span,
        "REMOVEFILTERS can only be a CALCULATE filter argument",
    ))
}

fn distinct_table(
    ev: &mut Evaluator<'_, '_>,
    table: usize,
    columns: &[usize],
    span: Span,
) -> Result<Val> {
    let rows = distinct(ev, table, columns, None, span)?;
    let names = columns
        .iter()
        .map(|&c| RowsColumn {
            name: ev
                .db
                .tables
                .get(table)
                .and_then(|t| t.columns.get(c))
                .map_or_else(String::new, |col| col.name.clone()),
            lineage: Some((table, c)),
        })
        .collect();
    Ok(Val::Table(Rc::new(TableVal::Rows(Rows {
        columns: names,
        rows,
    }))))
}

/// `RELATED(T[C])`: the value of `T[C]` in the row of `T` that the current row reaches through
/// active many-to-one Relationships (BLANK when its key has no match).
pub fn related(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let arg = call.arg(0)?;
    let (target, column) = column_ref(ev, arg)?;
    let frames: Vec<(usize, usize)> = ev
        .row_frames()
        .iter()
        .rev()
        .filter_map(|f| match f {
            RowFrame::Base { table, row } => Some((*table, *row)),
            RowFrame::Rows { .. } => None,
        })
        .collect();
    for (table, row) in frames {
        let Some(path) = path_to(ev, table, target, call.span)? else {
            continue;
        };
        let mut at = Some(row);
        for r in path {
            let Some(row) = at else { break };
            at = follow(ev, r, row, call.span)?;
        }
        let v = match at {
            Some(row) => ev
                .db
                .tables
                .get(target)
                .and_then(|t| t.columns.get(column))
                .map_or(Value::Blank, |c| c.data.value(row)),
            None => Value::Blank,
        };
        return Ok(Val::Scalar(v));
    }
    let name = ev.db.tables.get(target).map_or("", |t| t.name.as_str());
    Err(DaxError::new(
        call.span,
        format!(
            "RELATED needs a row context on a Table with an active many-to-one Relationship to \
             '{name}'"
        ),
    ))
}

/// The active Relationships (many → one) leading from `from` to `to`, shortest first found.
fn path_to(
    ev: &Evaluator<'_, '_>,
    from: usize,
    to: usize,
    span: Span,
) -> Result<Option<Vec<usize>>> {
    if from == to {
        return Ok(None);
    }
    // Breadth-first: (Table, the Relationships that reach it).
    let mut queue: Vec<(usize, Vec<usize>)> = vec![(from, Vec::new())];
    let mut seen = vec![from];
    let mut next = 0;
    while let Some((t, path)) = queue.get(next).cloned() {
        next += 1;
        if path.len() >= MAX_RELATIONSHIP_DEPTH {
            return Err(DaxError::limit(
                span,
                format!("Relationships chain more than {MAX_RELATIONSHIP_DEPTH} Tables"),
            ));
        }
        for (i, r) in ev.db.relationships.iter().enumerate() {
            if !r.active || r.from_table != t || seen.contains(&r.to_table) {
                continue;
            }
            let mut p = path.clone();
            p.push(i);
            if r.to_table == to {
                return Ok(Some(p));
            }
            seen.push(r.to_table);
            queue.push((r.to_table, p));
        }
    }
    Ok(None)
}

/// The one-side row that row `row` of Relationship `rel`'s many side matches.
fn follow(ev: &mut Evaluator<'_, '_>, rel: usize, row: usize, span: Span) -> Result<Option<usize>> {
    let Some(r) = ev.db.relationships.get(rel) else {
        return Ok(None);
    };
    let (from_table, from_column) = (r.from_table, r.from_column);
    let Some(key) = ev
        .db
        .tables
        .get(from_table)
        .and_then(|t| t.columns.get(from_column))
        .and_then(|c| Key::of(&c.data.value(row), true))
    else {
        return Ok(None);
    };
    let index = related_index(ev, rel, span)?;
    Ok(index.get(&key).copied())
}

/// Relationship `rel`'s one side as key → row, built once per query.
fn related_index(
    ev: &mut Evaluator<'_, '_>,
    rel: usize,
    span: Span,
) -> Result<Rc<HashMap<Key, usize>>> {
    if let Some(i) = ev.related_index.get(&rel) {
        return Ok(Rc::clone(i));
    }
    let mut index = HashMap::new();
    if let Some(r) = ev.db.relationships.get(rel)
        && let Some(t) = ev.db.tables.get(r.to_table)
        && let Some(col) = t.columns.get(r.to_column)
    {
        ev.charge(
            t.rows
                .saturating_mul(KEY_BYTES.saturating_add(std::mem::size_of::<usize>())),
            span,
        )?;
        storage::scan(t.rows, None, ev.cancel, |row| {
            if let Some(k) = Key::of(&col.data.value(row), true) {
                index.entry(k).or_insert(row);
            }
            Ok(())
        })?;
    }
    let index = Rc::new(index);
    ev.related_index.insert(rel, Rc::clone(&index));
    Ok(index)
}

/// Test-only `TESTSUM(T[C])` (the real `SUM` is M3.3's): the sum of a column under the filter
/// context, BLANK for none.
#[cfg(test)]
pub fn test_sum(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    let (table, column) = column_ref(ev, call.arg(0)?)?;
    let mask = ev.mask(table, call.fc, call.span)?;
    let mut sum = Value::Blank;
    if let Some(t) = ev.db.tables.get(table)
        && let Some(col) = t.columns.get(column)
    {
        storage::scan(t.rows, mask.as_ref(), ev.cancel, |r| {
            sum = sum
                .add(&col.data.value(r))
                .map_err(|e| value_error(call.span, &e))?;
            Ok(())
        })?;
    }
    Ok(Val::Scalar(sum))
}

#[cfg(test)]
#[path = "calculate_tests.rs"]
mod tests;
