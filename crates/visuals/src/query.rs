//! The query a Visual runs (M4.1; ADR 0006, 0007; grill Q5–Q7, Q25): its Fields resolved
//! against the Model, grouped with `metriccraft_model`'s `summarize`, sorted, capped and pivoted
//! into a [`QueryResult`] that layout (M4.3–M4.5) reads.
//!
//! **Axes.** Every Visual is a grid of *row groups* × *column groups* × *values*:
//!
//! | Visual            | Row groups                       | Column groups | Values                        |
//! |-------------------|----------------------------------|---------------|-------------------------------|
//! | table             | Values Columns summarized "none" | —             | Measures, Implicit measures   |
//! | matrix            | Rows                             | Columns       | Values                        |
//! | card              | —                                | —             | the one Value                 |
//! | bar, column, line | Axis                             | Legend        | Values (the first, w/ Legend) |
//! | slicer            | Field                            | —             | —                             |
//!
//! A Column in Values is an **Implicit measure**: `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNTA` or
//! `DISTINCTCOUNT` over it, by its summarize-by; "none" counts (`COUNTA`) outside a table.
//!
//! **Totals** (CONTEXT.md) are the Values evaluated over the whole group: `summarize` again with
//! the inner group-by Columns left out, never a sum of the rows. A table has a total row; a
//! matrix a subtotal at every level of Rows and of Columns plus grand totals, as its
//! `row_subtotals` / `column_subtotals` say (the table's total row follows `row_subtotals`).
//!
//! **Order.** A matrix's row subtotal comes before its group's rows (stepped layout), a column
//! subtotal after its group's columns; the grand total row is [`QueryResult::total`] and the
//! grand total column is last. Default sorts: table by its first column ascending, matrix by
//! each level ascending, bar/column by the first Value descending (the category's total across
//! the Legend), line by Axis ascending, slicer ascending. `Visual::sort` overrides them; a matrix
//! sorted by a Value orders each level by that level's subtotals.
//!
//! **Caps** ([`TABLE_WINDOW`], [`CHART_CATEGORY_CAP`], …) set [`QueryResult::truncated`].
//!
//! **Errors.** A failing Measure fails the whole query with its [`DaxError`] (span into the
//! Measure's text), as does a Field that isn't in the Model.
//!
//! **Cache** ([`QueryCache`]): keyed by the query-relevant parts of the Visual ([`QueryKey`]),
//! the caller's Selection key and the window; a Model generation change empties it.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::sync::Arc;

use metriccraft_core::Value;
use metriccraft_data::step::ColumnType;
use metriccraft_dax::{Expr, ExprKind, Query, Span};
use metriccraft_model::dax::functions::summarize::summarize_rows;
use metriccraft_model::dax::model_eval::measure_defines;
use metriccraft_model::dax::{
    Database, DaxError, ErrorKind, Evaluator, FilterContext, Key, Options, Result,
};
use metriccraft_model::{Model, SummarizeBy, same_name};

use crate::visual::{FieldRef, Sort, SortDirection, Visual, VisualType, Well};

/// Rows of a table or matrix per window (Power BI fetches 500 more as one scrolls).
pub const TABLE_WINDOW: usize = 500;
/// Column-group tuples a matrix shows (before its column subtotals).
// ponytail: parity guess for Power BI's matrix column data reduction.
pub const MATRIX_COLUMN_CAP: usize = 100;
/// Categories a bar or column chart shows.
pub const CHART_CATEGORY_CAP: usize = 1_000;
/// Categories (points per series) a line chart shows.
// ponytail: Power BI samples 3,500 points across all series; we cap categories.
pub const LINE_POINT_CAP: usize = 3_500;
/// Legend series a chart shows.
// ponytail: parity guess (Power BI's top-60 series reduction).
pub const SERIES_CAP: usize = 60;
/// Values a slicer lists.
pub const SLICER_CAP: usize = 1_000;
/// Fields per axis (a hostile file's 10,000 Rows would mean 10,000 summarize passes).
pub const MAX_AXIS_FIELDS: usize = 16;
/// Fields in Values (a table's columns; a matrix's or chart's values): each is a summarize
/// pass's measure and a cell per row and column.
pub const MAX_VALUE_FIELDS: usize = 64;
/// Results a [`QueryCache`] holds before it starts over.
pub const CACHE_CAP: usize = 256;

/// What a query reads: the data, the Model (Measures, Column metadata) and the Page's
/// Selection as a filter context.
#[derive(Clone, Copy, Debug)]
pub struct QueryInput<'a> {
    pub db: &'a Database,
    pub model: &'a Model,
    pub filters: &'a FilterContext,
    pub options: Options<'a>,
}

/// A Field of the result: what layout titles and formats it with.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub field: FieldRef,
    /// "Region", "Sum of Amount", a Measure's name.
    pub label: String,
    /// The Column's or Measure's format string; empty for the default.
    pub format_string: String,
    /// The Column's (or Implicit measure's) type; `None` for a Measure (its values' types).
    pub data_type: Option<ColumnType>,
    /// The aggregation of an Implicit measure.
    pub aggregate: Option<SummarizeBy>,
}

/// One row of the grid. `keys` has one entry per row group: `None` where the row is a total
/// over that group (a subtotal has its outer keys and `None` inner ones; the grand total all
/// `None`). `cells` holds, for each column-key tuple `j` and value `k`, the value at
/// `j * values.len() + k`.
#[derive(Clone, Debug, PartialEq)]
pub struct ResultRow {
    pub keys: Vec<Option<Value>>,
    pub cells: Vec<Value>,
}

impl ResultRow {
    /// How many row groups this row is grouped by (`keys.len()` for a detail row).
    pub fn level(&self) -> usize {
        self.keys.iter().take_while(|k| k.is_some()).count()
    }

    /// A total or subtotal row.
    pub fn is_total(&self) -> bool {
        self.keys.iter().any(Option::is_none)
    }
}

/// A table column in display (well) order: a row group or a value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayColumn {
    Group(usize),
    Value(usize),
}

/// A Visual's data, pivoted for layout:
/// - **table**: `row_groups` × `values`, `display` gives the well order, `total` the total row;
/// - **matrix**: `rows` (with subtotal rows) × `column_keys` (with subtotal columns) × `values`;
/// - **card**: one row, one cell;
/// - **charts**: `rows` are categories; series are `column_keys` (Legend) × `values`;
/// - **slicer**: `rows` are the items (no cells).
#[derive(Clone, Debug, PartialEq)]
pub struct QueryResult {
    pub kind: VisualType,
    pub row_groups: Vec<Header>,
    pub column_groups: Vec<Header>,
    pub values: Vec<Header>,
    /// Column-group tuples in display order, `None` where a tuple is a (sub)total; `[[]]` when
    /// there are no column groups.
    pub column_keys: Vec<Vec<Option<Value>>>,
    /// The rows in this window, in order.
    pub rows: Vec<ResultRow>,
    /// The grand total row (table, matrix), the same for every window.
    pub total: Option<ResultRow>,
    /// Table only: its columns in well order.
    pub display: Vec<DisplayColumn>,
    /// The window's first row in the full ordered rows.
    pub offset: usize,
    /// Rows before windowing and capping (subtotal rows included).
    pub total_rows: usize,
    /// A cap left rows or columns out (or the window doesn't hold every row).
    pub truncated: bool,
}

impl QueryResult {
    /// The value of column tuple `column`, value `value` in `row`.
    pub fn cell<'r>(&self, row: &'r ResultRow, column: usize, value: usize) -> Option<&'r Value> {
        row.cells
            .get(column.checked_mul(self.values.len())?.checked_add(value)?)
    }
}

/// Runs `visual`'s query: rows from `offset` for a table or matrix (ignored otherwise).
pub fn run_query(visual: &Visual, input: &QueryInput<'_>, offset: usize) -> Result<QueryResult> {
    let plan = plan(visual, input)?;
    let defines = Query {
        defines: measure_defines(input.model, input.db, |_| false),
        evaluates: Vec::new(),
    };
    let mut ev = Evaluator::new(input.db, input.options);
    ev.run(&defines)?;
    execute(&mut ev, &plan, input.filters, offset)
}

// ---------------------------------------------------------------------------------------------
// Planning: Fields → group-by Columns and value expressions.

struct Group {
    header: Header,
    /// (Table, Column) in the Database.
    at: (usize, usize),
}

struct ValueExpr {
    header: Header,
    expr: Expr,
}

/// What to sort by: a row group, a column group (matrix), a value, or each level's keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SortBy {
    Keys,
    Row(usize),
    Column(usize),
    Value(usize),
}

/// How many rows the result keeps.
#[derive(Clone, Copy)]
enum RowCap {
    Window,
    Top(usize),
    None,
}

struct Plan {
    kind: VisualType,
    rows: Vec<Group>,
    columns: Vec<Group>,
    values: Vec<ValueExpr>,
    display: Vec<DisplayColumn>,
    row_subtotals: bool,
    column_subtotals: bool,
    total_row: bool,
    sort: SortBy,
    descending: bool,
    /// Matrix: rows sort level by level.
    hierarchical: bool,
    row_cap: RowCap,
    column_cap: usize,
}

fn plan(visual: &Visual, input: &QueryInput<'_>) -> Result<Plan> {
    let groups = |well: Well| -> Result<Vec<Group>> {
        let fields = visual.fields(well);
        if fields.len() > MAX_AXIS_FIELDS {
            return Err(DaxError::limit(
                Span::default(),
                format!(
                    "a Visual's {} takes at most {MAX_AXIS_FIELDS} Fields",
                    well.id()
                ),
            ));
        }
        fields.iter().map(|f| group(input, f, well)).collect()
    };
    let values = |fields: &[FieldRef]| -> Result<Vec<ValueExpr>> {
        if fields.len() > MAX_VALUE_FIELDS {
            return Err(DaxError::limit(
                Span::default(),
                format!("a Visual's values take at most {MAX_VALUE_FIELDS} Fields"),
            ));
        }
        fields.iter().map(|f| value(input, f)).collect()
    };
    let format = &visual.format;
    let mut p = Plan {
        kind: visual.kind,
        rows: Vec::new(),
        columns: Vec::new(),
        values: Vec::new(),
        display: Vec::new(),
        row_subtotals: false,
        column_subtotals: false,
        total_row: false,
        sort: SortBy::Keys,
        descending: false,
        hierarchical: false,
        row_cap: RowCap::None,
        column_cap: usize::MAX,
    };
    match visual.kind {
        VisualType::Table => {
            let fields = visual.fields(Well::Values);
            let cap = MAX_VALUE_FIELDS;
            if fields.len() > cap {
                return Err(DaxError::limit(
                    Span::default(),
                    format!("a table takes at most {cap} Fields"),
                ));
            }
            for f in fields {
                if is_group_column(input, f) {
                    p.display.push(DisplayColumn::Group(p.rows.len()));
                    p.rows.push(group(input, f, Well::Values)?);
                } else {
                    p.display.push(DisplayColumn::Value(p.values.len()));
                    p.values.push(value(input, f)?);
                }
            }
            p.total_row = format.row_subtotals;
            p.row_cap = RowCap::Window;
            (p.sort, p.descending) = match p.display.first() {
                Some(DisplayColumn::Group(i)) => (SortBy::Row(*i), false),
                Some(DisplayColumn::Value(k)) => (SortBy::Value(*k), false),
                None => (SortBy::Keys, false),
            };
        }
        VisualType::Matrix => {
            p.rows = groups(Well::Rows)?;
            p.columns = groups(Well::Columns)?;
            p.values = values(visual.fields(Well::Values))?;
            p.row_subtotals = format.row_subtotals;
            p.column_subtotals = format.column_subtotals;
            p.total_row = format.row_subtotals;
            p.hierarchical = true;
            p.row_cap = RowCap::Window;
            p.column_cap = MATRIX_COLUMN_CAP;
        }
        VisualType::Card => {
            p.values = values(visual.fields(Well::Values).get(..1).unwrap_or(&[]))?;
        }
        VisualType::ClusteredBar | VisualType::ClusteredColumn | VisualType::Line => {
            p.rows = groups(Well::Axis)?;
            let all = visual.fields(Well::Values);
            let legend = visual.fields(Well::Legend);
            if legend.is_empty() {
                p.values = values(all)?;
            } else {
                // Legend OR several Values: with a Legend only the first Value is drawn.
                p.columns = groups(Well::Legend)?;
                p.values = values(all.get(..1).unwrap_or(&[]))?;
                p.column_cap = SERIES_CAP;
            }
            if visual.kind == VisualType::Line {
                p.row_cap = RowCap::Top(LINE_POINT_CAP);
            } else {
                p.row_cap = RowCap::Top(CHART_CATEGORY_CAP);
                if !p.values.is_empty() {
                    (p.sort, p.descending) = (SortBy::Value(0), true);
                }
            }
        }
        VisualType::Slicer => {
            p.rows = groups(Well::Field)?.into_iter().take(1).collect();
            p.row_cap = RowCap::Top(SLICER_CAP);
        }
    }
    if let Some(sort) = &visual.sort
        && let Some(by) = explicit_sort(&p, sort)
    {
        p.sort = by;
        p.descending = sort.direction == SortDirection::Descending;
    }
    Ok(p)
}

/// The Field `sort` names, if the Visual shows it.
fn explicit_sort(p: &Plan, sort: &Sort) -> Option<SortBy> {
    let find = |gs: &[Group]| {
        gs.iter()
            .position(|g| same_field(&g.header.field, &sort.field))
    };
    if let Some(i) = find(&p.rows) {
        return Some(SortBy::Row(i));
    }
    if p.kind == VisualType::Matrix
        && let Some(i) = find(&p.columns)
    {
        return Some(SortBy::Column(i));
    }
    p.values
        .iter()
        .position(|v| same_field(&v.header.field, &sort.field))
        .map(SortBy::Value)
}

fn same_field(a: &FieldRef, b: &FieldRef) -> bool {
    match (a, b) {
        (
            FieldRef::Column { table, column },
            FieldRef::Column {
                table: t,
                column: c,
            },
        ) => same_name(table, t) && same_name(column, c),
        (FieldRef::Measure { name }, FieldRef::Measure { name: n }) => same_name(name, n),
        _ => false,
    }
}

fn model_column<'m>(
    model: &'m Model,
    table: &str,
    column: &str,
) -> Option<&'m metriccraft_model::Column> {
    model
        .tables
        .iter()
        .find(|t| same_name(&t.name, table))?
        .columns
        .iter()
        .find(|c| same_name(&c.name, column))
}

/// A table's Values Column that groups (summarize by "none") rather than aggregates.
fn is_group_column(input: &QueryInput<'_>, f: &FieldRef) -> bool {
    match f {
        FieldRef::Column { table, column } => model_column(input.model, table, column)
            .is_none_or(|c| c.summarize_by == SummarizeBy::None),
        FieldRef::Measure { .. } => false,
    }
}

/// `f` as a group-by Column of `well`.
fn group(input: &QueryInput<'_>, f: &FieldRef, well: Well) -> Result<Group> {
    let (table, column) = match f {
        FieldRef::Column { table, column } => (table, column),
        FieldRef::Measure { name } => {
            return Err(DaxError::new(
                Span::default(),
                format!(
                    "the Measure [{name}] can't be in {}: it takes Columns",
                    well.id()
                ),
            ));
        }
    };
    let (t, c, name) = db_column(input.db, table, column)?;
    let meta = model_column(input.model, table, column);
    Ok(Group {
        header: Header {
            field: f.clone(),
            label: name,
            format_string: meta.map(|m| m.format_string.clone()).unwrap_or_default(),
            data_type: meta.map(|m| m.data_type),
            aggregate: None,
        },
        at: (t, c),
    })
}

/// The Database's (Table, Column) and the Column's name as loaded.
fn db_column(db: &Database, table: &str, column: &str) -> Result<(usize, usize, String)> {
    let missing = || {
        DaxError::new(
            Span::default(),
            format!("there is no Column '{table}'[{column}]"),
        )
    };
    let t = db.table(table).ok_or_else(missing)?;
    let dt = db.tables.get(t).ok_or_else(missing)?;
    let c = dt.column(column).ok_or_else(missing)?;
    let name = dt
        .columns
        .get(c)
        .map(|c| c.name.clone())
        .ok_or_else(missing)?;
    Ok((t, c, name))
}

fn column_expr(table: &str, column: &str) -> Expr {
    Expr {
        kind: ExprKind::Column {
            table: Some(table.into()),
            column: column.into(),
        },
        span: Span::default(),
    }
}

/// `f` as a value: a Measure reference, or an Implicit measure over a Column.
fn value(input: &QueryInput<'_>, f: &FieldRef) -> Result<ValueExpr> {
    match f {
        FieldRef::Measure { name } => {
            let (home, m) = input.model.measure(name).ok_or_else(|| {
                DaxError::new(Span::default(), format!("there is no Measure [{name}]"))
            })?;
            m.parse().map_err(|e| DaxError {
                kind: ErrorKind::Syntax,
                span: e.span,
                message: e.message,
            })?;
            Ok(ValueExpr {
                header: Header {
                    field: f.clone(),
                    label: m.name.clone(),
                    format_string: m.format_string.clone(),
                    data_type: None,
                    aggregate: None,
                },
                expr: column_expr(&home.name, &m.name),
            })
        }
        FieldRef::Column { table, column } => {
            let (t, _, name) = db_column(input.db, table, column)?;
            let table_name = input
                .db
                .tables
                .get(t)
                .map(|dt| dt.name.clone())
                .unwrap_or_default();
            let meta = model_column(input.model, table, column);
            let agg = match meta.map_or(SummarizeBy::None, |m| m.summarize_by) {
                // ponytail: Power BI shows "First" for a text Column in a card; we count.
                SummarizeBy::None => SummarizeBy::Count,
                s => s,
            };
            let ty = meta.map(|m| m.data_type);
            let (function, label, data_type, format) = match agg {
                SummarizeBy::Sum => ("SUM", format!("Sum of {name}"), ty, true),
                SummarizeBy::Average => (
                    "AVERAGE",
                    format!("Average of {name}"),
                    match ty {
                        Some(ColumnType::FixedDecimal) => ty,
                        Some(_) => Some(ColumnType::Decimal),
                        None => None,
                    },
                    true,
                ),
                SummarizeBy::Min => ("MIN", format!("Min of {name}"), ty, true),
                SummarizeBy::Max => ("MAX", format!("Max of {name}"), ty, true),
                SummarizeBy::DistinctCount => (
                    "DISTINCTCOUNT",
                    // ponytail: parity guess at Power BI's wording.
                    format!("Count of {name} (Distinct)"),
                    Some(ColumnType::Whole),
                    false,
                ),
                SummarizeBy::Count | SummarizeBy::None => (
                    "COUNTA",
                    format!("Count of {name}"),
                    Some(ColumnType::Whole),
                    false,
                ),
            };
            Ok(ValueExpr {
                header: Header {
                    field: f.clone(),
                    label,
                    format_string: if format {
                        meta.map(|m| m.format_string.clone()).unwrap_or_default()
                    } else {
                        String::new()
                    },
                    data_type,
                    aggregate: Some(agg),
                },
                expr: Expr {
                    kind: ExprKind::Call {
                        name: function.into(),
                        args: vec![column_expr(&table_name, &name)],
                    },
                    span: Span::default(),
                },
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Execution: summarize per level, sort, cap, pivot.

type KeyTuple = Vec<Option<Key>>;
/// One level's values by its group keys.
type LevelMap = HashMap<KeyTuple, Vec<Value>>;

fn keys(values: &[Value]) -> KeyTuple {
    values.iter().map(|v| Key::of(v, false)).collect()
}

struct Run<'p, 'e, 'a, 'q> {
    ev: &'e mut Evaluator<'a, 'q>,
    plan: &'p Plan,
    fc: &'p FilterContext,
    levels: HashMap<(usize, usize), LevelMap>,
}

impl Run<'_, '_, '_, '_> {
    /// `summarize` grouped by the first `r` row groups and `c` column groups: rows of the
    /// group keys then the values.
    fn summarize(&mut self, r: usize, c: usize) -> Result<Vec<Vec<Value>>> {
        self.ev.cancel.check()?;
        let p = self.plan;
        let group_by: Vec<(usize, usize)> = p
            .rows
            .iter()
            .take(r)
            .chain(p.columns.iter().take(c))
            .map(|g| g.at)
            .collect();
        let names: Vec<String> = (0..p.values.len()).map(|k| format!("v{k}")).collect();
        let measures: Vec<(&str, &Expr)> = names
            .iter()
            .zip(&p.values)
            .map(|(n, v)| (n.as_str(), &v.expr))
            .collect();
        Ok(summarize_rows(self.ev, &group_by, &measures, self.fc, Span::default())?.rows)
    }

    /// Indexes `rows` (from [`Self::summarize`] at level `(r, c)`) by their keys.
    fn index(&mut self, r: usize, c: usize, rows: Vec<Vec<Value>>) -> Result<()> {
        let width = r.saturating_add(c);
        // Keys twice (the row, its Key tuple) and the values once, per row.
        let per_row = width
            .saturating_mul(2)
            .saturating_add(self.plan.values.len())
            .saturating_add(1);
        let bytes = rows
            .len()
            .saturating_mul(per_row.saturating_mul(std::mem::size_of::<Value>()));
        self.ev.charge(bytes, Span::default())?;
        let mut map = LevelMap::with_capacity(rows.len());
        for mut row in rows {
            let values = row.split_off(width.min(row.len()));
            map.insert(keys(&row), values);
        }
        self.levels.insert((r, c), map);
        Ok(())
    }

    fn ensure(&mut self, r: usize, c: usize) -> Result<()> {
        if !self.levels.contains_key(&(r, c)) {
            let rows = self.summarize(r, c)?;
            self.index(r, c, rows)?;
        }
        Ok(())
    }

    /// Value `k` of the group `key` at level `(r, c)` (BLANK if the group has none).
    fn lookup(&self, r: usize, c: usize, key: &KeyTuple, k: usize) -> Value {
        self.levels
            .get(&(r, c))
            .and_then(|m| m.get(key))
            .and_then(|vs| vs.get(k))
            .cloned()
            .unwrap_or(Value::Blank)
    }
}

/// Distinct tuples of `range` of each row, first-seen order.
fn distinct(rows: &[Vec<Value>], from: usize, to: usize) -> Vec<Vec<Value>> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for row in rows {
        let t = row.get(from..to).unwrap_or(&[]);
        if seen.insert(keys(t)) {
            out.push(t.to_vec());
        }
    }
    out
}

/// Ascending or descending.
fn directed(o: std::cmp::Ordering, descending: bool) -> std::cmp::Ordering {
    if descending { o.reverse() } else { o }
}

fn lexicographic(a: &[Value], b: &[Value]) -> std::cmp::Ordering {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.order(y))
        .find(|o| o.is_ne())
        .unwrap_or_else(|| a.len().cmp(&b.len()))
}

/// The first level at which `a` and `b` differ (their length if none).
fn first_difference(a: &[Value], b: &[Value]) -> usize {
    a.iter()
        .zip(b)
        .position(|(x, y)| Key::of(x, false) != Key::of(y, false))
        .unwrap_or_else(|| a.len().min(b.len()))
}

/// Sorts `tuples` level by level: at each level by `level_key(level, tuple)` (directed when
/// `sorted(level)`), ties by the level's own value ascending, so groups stay together.
fn sort_hierarchical(
    tuples: &mut [Vec<Value>],
    level_keys: &[Vec<Value>],
    sorted: impl Fn(usize) -> bool,
    descending: bool,
) {
    let mut idx: Vec<usize> = (0..tuples.len()).collect();
    let empty: Vec<Value> = Vec::new();
    idx.sort_by(|&a, &b| {
        let (ta, tb) = (
            tuples.get(a).unwrap_or(&empty),
            tuples.get(b).unwrap_or(&empty),
        );
        let (ka, kb) = (
            level_keys.get(a).unwrap_or(&empty),
            level_keys.get(b).unwrap_or(&empty),
        );
        for (i, (x, y)) in ta.iter().zip(tb).enumerate() {
            let by_key = match (ka.get(i), kb.get(i)) {
                (Some(p), Some(q)) => directed(p.order(q), sorted(i) && descending),
                _ => std::cmp::Ordering::Equal,
            };
            let o = by_key.then_with(|| x.order(y));
            if o.is_ne() {
                return o;
            }
        }
        std::cmp::Ordering::Equal
    });
    let ordered: Vec<Vec<Value>> = idx
        .iter()
        .filter_map(|&i| tuples.get_mut(i).map(std::mem::take))
        .collect();
    for (slot, t) in tuples.iter_mut().zip(ordered) {
        *slot = t;
    }
}

fn execute(
    ev: &mut Evaluator<'_, '_>,
    plan: &Plan,
    fc: &FilterContext,
    offset: usize,
) -> Result<QueryResult> {
    let n = plan.rows.len();
    let m = plan.columns.len();
    let mut run = Run {
        ev,
        plan,
        fc,
        levels: HashMap::new(),
    };
    let detail = run.summarize(n, m)?;
    let mut row_tuples = if n == 0 {
        vec![Vec::new()]
    } else {
        distinct(&detail, 0, n)
    };
    let mut col_tuples = if m == 0 {
        vec![Vec::new()]
    } else {
        distinct(&detail, n, n.saturating_add(m))
    };
    run.index(n, m, detail)?;

    // Row order.
    let flat_value = |run: &mut Run<'_, '_, '_, '_>, k: usize, tuples: &[Vec<Value>]| {
        run.ensure(n, 0)?;
        Ok::<_, DaxError>(
            tuples
                .iter()
                .map(|t| run.lookup(n, 0, &keys(t), k))
                .collect::<Vec<_>>(),
        )
    };
    match (plan.sort, plan.hierarchical) {
        (SortBy::Value(k), true) => {
            let mut level_keys = vec![Vec::with_capacity(n); row_tuples.len()];
            for r in 1..=n {
                run.ensure(r, 0)?;
                for (t, lk) in row_tuples.iter().zip(level_keys.iter_mut()) {
                    lk.push(run.lookup(r, 0, &keys(t.get(..r).unwrap_or(&[])), k));
                }
            }
            sort_hierarchical(&mut row_tuples, &level_keys, |_| true, plan.descending);
        }
        (SortBy::Row(d), true) => {
            let level_keys = row_tuples.clone();
            sort_hierarchical(&mut row_tuples, &level_keys, |i| i == d, plan.descending);
        }
        (_, true) => sort_hierarchical(&mut row_tuples, &[], |_| false, false),
        (SortBy::Value(k), false) => {
            let by = flat_value(&mut run, k, &row_tuples)?;
            let mut pairs: Vec<(Value, Vec<Value>)> = by.into_iter().zip(row_tuples).collect();
            pairs.sort_by(|(a, ta), (b, tb)| {
                directed(a.order(b), plan.descending).then_with(|| lexicographic(ta, tb))
            });
            row_tuples = pairs.into_iter().map(|(_, t)| t).collect();
        }
        (SortBy::Row(d), false) => row_tuples.sort_by(|a, b| {
            let (x, y) = (a.get(d), b.get(d));
            let o = match (x, y) {
                (Some(x), Some(y)) => x.order(y),
                _ => std::cmp::Ordering::Equal,
            };
            directed(o, plan.descending).then_with(|| lexicographic(a, b))
        }),
        (_, false) => row_tuples.sort_by(|a, b| directed(lexicographic(a, b), plan.descending)),
    }

    // Column order, then the column cap.
    match plan.sort {
        SortBy::Column(d) => {
            let level_keys = col_tuples.clone();
            sort_hierarchical(&mut col_tuples, &level_keys, |i| i == d, plan.descending);
        }
        _ => sort_hierarchical(&mut col_tuples, &[], |_| false, false),
    }
    let mut truncated = false;
    if col_tuples.len() > plan.column_cap {
        col_tuples.truncate(plan.column_cap);
        truncated = true;
    }

    // Row descriptors (level, tuple): a subtotal row before its group (stepped layout).
    let mut row_desc: Vec<(usize, usize)> = Vec::new();
    for (i, t) in row_tuples.iter().enumerate() {
        let diff = match i.checked_sub(1).and_then(|p| row_tuples.get(p)) {
            Some(prev) => first_difference(prev, t),
            None => 0,
        };
        if plan.row_subtotals {
            for r in diff.saturating_add(1)..n {
                row_desc.push((r, i));
            }
        }
        row_desc.push((n, i));
    }
    let total_rows = row_desc.len();
    let (start, rows_kept) = match plan.row_cap {
        RowCap::Window => {
            let start = offset.min(total_rows);
            (start, start.saturating_add(TABLE_WINDOW).min(total_rows))
        }
        RowCap::Top(cap) => (0, cap.min(total_rows)),
        RowCap::None => (0, total_rows),
    };
    if start > 0 || rows_kept < total_rows {
        truncated = true;
    }
    let window = row_desc.get(start..rows_kept).unwrap_or(&[]);

    // Column descriptors (level, tuple): a subtotal column after its group, grand total last.
    let mut col_desc: Vec<(usize, usize)> = Vec::new();
    for (j, t) in col_tuples.iter().enumerate() {
        col_desc.push((m, j));
        if plan.column_subtotals {
            let diff = match col_tuples.get(j.saturating_add(1)) {
                Some(next) => first_difference(t, next),
                None => 0,
            };
            for c in (diff.saturating_add(1)..m).rev() {
                col_desc.push((c, j));
            }
        }
    }
    if plan.column_subtotals && m > 0 && !col_tuples.is_empty() {
        col_desc.push((0, 0));
    }

    // Every level the window needs.
    let has_total = plan.total_row && n > 0 && !row_tuples.is_empty() && !plan.values.is_empty();
    let mut row_levels: Vec<usize> = window.iter().map(|(r, _)| *r).collect();
    if has_total {
        row_levels.push(0);
    }
    row_levels.sort_unstable();
    row_levels.dedup();
    let mut col_levels: Vec<usize> = col_desc.iter().map(|(c, _)| *c).collect();
    col_levels.sort_unstable();
    col_levels.dedup();
    if !plan.values.is_empty() {
        for &r in &row_levels {
            for &c in &col_levels {
                run.ensure(r, c)?;
            }
        }
    }

    let empty: Vec<Value> = Vec::new();
    let prefix = |t: &[Value], level: usize| -> Vec<Option<Value>> {
        (0..t.len())
            .map(|i| if i < level { t.get(i).cloned() } else { None })
            .collect()
    };
    let column_keys: Vec<Vec<Option<Value>>> = col_desc
        .iter()
        .map(|&(c, j)| prefix(col_tuples.get(j).unwrap_or(&empty), c))
        .collect();
    let v = plan.values.len();
    let row_of = |r: usize, t: &[Value], run: &Run<'_, '_, '_, '_>| -> ResultRow {
        let row_key = keys(t.get(..r).unwrap_or(&[]));
        let mut cells = Vec::with_capacity(col_desc.len().saturating_mul(v));
        for &(c, j) in &col_desc {
            let col = col_tuples.get(j).unwrap_or(&empty);
            let mut key = row_key.clone();
            key.extend(keys(col.get(..c).unwrap_or(&[])));
            for k in 0..v {
                cells.push(run.lookup(r, c, &key, k));
            }
        }
        ResultRow {
            keys: prefix(t, r),
            cells,
        }
    };
    // The cells about to be built: rows (and the total) × column tuples × values.
    let cells = window
        .len()
        .saturating_add(1)
        .saturating_mul(col_desc.len())
        .saturating_mul(v);
    run.ev.charge(
        cells.saturating_mul(std::mem::size_of::<Value>()),
        Span::default(),
    )?;
    let mut rows = Vec::with_capacity(window.len());
    for (i, &(r, ti)) in window.iter().enumerate() {
        if i % 64 == 0 {
            run.ev.cancel.check()?;
        }
        rows.push(row_of(r, row_tuples.get(ti).unwrap_or(&empty), &run));
    }
    let total = if has_total {
        let first = row_tuples.first().unwrap_or(&empty);
        Some(row_of(0, first, &run))
    } else {
        None
    };
    Ok(QueryResult {
        kind: plan.kind,
        row_groups: plan.rows.iter().map(|g| g.header.clone()).collect(),
        column_groups: plan.columns.iter().map(|g| g.header.clone()).collect(),
        values: plan.values.iter().map(|x| x.header.clone()).collect(),
        column_keys,
        rows,
        total,
        display: plan.display.clone(),
        offset: start,
        total_rows,
        truncated,
    })
}

// ---------------------------------------------------------------------------------------------
// Cache.

/// The parts of a Visual its query depends on, plus the window: equal keys, equal results
/// (for one Model generation and Selection).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct QueryKey {
    kind: VisualType,
    wells: BTreeMap<Well, Vec<FieldRef>>,
    sort: Option<Sort>,
    row_subtotals: bool,
    column_subtotals: bool,
    offset: usize,
}

impl QueryKey {
    pub fn new(visual: &Visual, offset: usize) -> Self {
        let windowed = matches!(visual.kind, VisualType::Table | VisualType::Matrix);
        QueryKey {
            kind: visual.kind,
            wells: visual.wells.clone(),
            sort: visual.sort.clone(),
            row_subtotals: visual.format.row_subtotals,
            column_subtotals: visual.format.column_subtotals,
            offset: if windowed { offset } else { 0 },
        }
    }
}

/// Query results by ([`QueryKey`], Selection key `S`), for one Model `generation`. `S` is
/// whatever the caller derives `QueryInput::filters` from (the Page's Selection): equal `S`
/// must mean equal filters. A hit returns the stored result without evaluating anything;
/// errors aren't cached.
// ponytail: when full it starts over rather than evicting least-recently-used entries.
#[derive(Debug)]
pub struct QueryCache<S> {
    generation: u64,
    entries: HashMap<(QueryKey, S), Arc<QueryResult>>,
    runs: u64,
}

impl<S> Default for QueryCache<S> {
    fn default() -> Self {
        QueryCache {
            generation: 0,
            entries: HashMap::new(),
            runs: 0,
        }
    }
}

impl<S: Hash + Eq> QueryCache<S> {
    pub fn new() -> Self {
        Self::default()
    }

    /// `visual`'s result under `selection` (whose filters are `input.filters`) at Model
    /// `generation` (bumped by every data load and Model edit): cached, or run and stored.
    pub fn get_or_run(
        &mut self,
        generation: u64,
        visual: &Visual,
        selection: S,
        input: &QueryInput<'_>,
        offset: usize,
    ) -> Result<Arc<QueryResult>> {
        if generation != self.generation {
            self.entries.clear();
            self.generation = generation;
        }
        let key = (QueryKey::new(visual, offset), selection);
        if let Some(hit) = self.entries.get(&key) {
            return Ok(Arc::clone(hit));
        }
        self.runs = self.runs.saturating_add(1);
        let result = Arc::new(run_query(visual, input, offset)?);
        if self.entries.len() >= CACHE_CAP {
            self.entries.clear();
        }
        self.entries.insert(key, Arc::clone(&result));
        Ok(result)
    }

    /// Queries actually run (cache misses), for tests and diagnostics.
    pub fn runs(&self) -> u64 {
        self.runs
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;
