//! The formula engine (ADR 0006): interprets the `opendash_dax` AST under a filter context and
//! a row context stack. Aggregations go to storage scans ([`super::storage`]); functions are in
//! [`super::functions`].
//!
//! Semantics chosen here, to settle with parity cases:
//! - `VAR`s are evaluated where they are defined (eagerly), so an unused `VAR` that fails fails
//!   the expression; DAX may evaluate them lazily.
//! - `&&` and `||` evaluate both sides.
//! - A Measure reference is an implicit `CALCULATE`: inside a row context it applies context
//!   transition ([`super::calculate::transition`]), and its expression sees no row context.
//! - A table of one column and at most one row is a scalar where one is needed (empty: BLANK).
//! - `{ … }` names its columns `Value` (one column) or `Value1`, `Value2`, ….

use std::cmp::Ordering;
use std::collections::HashMap;
use std::rc::Rc;

use opendash_core::Value;
use opendash_dax::{BinOp, Define, Expr, ExprKind, Query, Span, UnaryOp};

use super::functions::{self, Call};
use super::storage::{self, Bitset, Cancel, Filter, Key, and};
use super::{
    Database, DaxError, ErrorKind, FilterContext, MAX_EVAL_DEPTH, MAX_EVAL_STACK,
    MAX_MEASURE_CHAIN, MAX_RELATIONSHIP_DEPTH, Options, Result, ResultTable,
};
use crate::same_name;

/// What an expression evaluates to.
#[derive(Clone, Debug)]
pub enum Val {
    Scalar(Value),
    Table(Rc<TableVal>),
}

/// A table value.
#[derive(Clone, Debug)]
pub enum TableVal {
    /// The visible rows of model Table `table` (`None`: every row), not materialised.
    Base { table: usize, rows: Option<Bitset> },
    /// A materialised table (`ROW`, `{ … }`, a function's result).
    Rows(Rows),
}

/// Rows of values, column by column named.
#[derive(Clone, Debug, Default)]
pub struct Rows {
    pub columns: Vec<RowsColumn>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RowsColumn {
    pub name: String,
    /// The model (Table, Column) the values came from, if any: `T[C]` finds it by that.
    pub lineage: Option<(usize, usize)>,
}

impl RowsColumn {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.into(),
            lineage: None,
        }
    }
}

/// One row of a row context.
#[derive(Clone, Debug)]
pub enum RowFrame {
    /// Row `row` of model Table `table`.
    Base { table: usize, row: usize },
    /// Row `row` of a materialised table.
    Rows { table: Rc<TableVal>, row: usize },
}

/// Bytes one materialised value costs (its slot; text adds its length).
const VALUE_BYTES: usize = std::mem::size_of::<Value>();
/// Bytes one materialised row costs beyond its values.
const ROW_BYTES: usize = std::mem::size_of::<Vec<Value>>();

/// A Measure defined in the query (`DEFINE MEASURE T[M] = …`).
struct Measure<'q> {
    table: String,
    name: String,
    expr: &'q Expr,
}

/// Evaluates DAX against a [`Database`]. One per query: it tracks the memory intermediate
/// tables use, the Measures and query `VAR`s in scope, the row context and the depth.
pub struct Evaluator<'a, 'q> {
    pub db: &'a Database,
    pub cancel: Cancel<'a>,
    memory_left: usize,
    measures: Vec<Measure<'q>>,
    measure_stack: Vec<usize>,
    /// Variables in scope, innermost last; the first `query_vars` are the query's `DEFINE VAR`s.
    vars: Vec<(String, Val)>,
    query_vars: usize,
    rows: Vec<RowFrame>,
    depth: usize,
    /// [`stack_position`] when evaluation started (at depth 0).
    stack_base: usize,
    /// `RELATED`'s one-side lookups, by Relationship, built on first use.
    pub(super) related_index: HashMap<usize, Rc<HashMap<Key, usize>>>,
}

impl<'a, 'q> Evaluator<'a, 'q> {
    pub fn new(db: &'a Database, options: Options<'a>) -> Self {
        Self {
            db,
            cancel: Cancel(options.cancel),
            memory_left: options.memory_cap.saturating_sub(db.loaded_bytes),
            measures: Vec::new(),
            measure_stack: Vec::new(),
            vars: Vec::new(),
            query_vars: 0,
            rows: Vec::new(),
            depth: 0,
            stack_base: 0,
            related_index: HashMap::new(),
        }
    }

    /// Runs `query`: its definitions, then each `EVALUATE`.
    pub fn run(&mut self, query: &'q Query) -> Result<Vec<ResultTable>> {
        let root = FilterContext::default();
        for d in &query.defines {
            if let Define::Measure {
                table,
                name,
                expr,
                span,
            } = d
            {
                self.define_measure(table, name, expr, *span)?;
            }
        }
        for d in &query.defines {
            if let Define::Var { name, expr, .. } = d {
                let v = self.eval(expr, &root)?;
                self.vars.push((name.clone(), v));
                self.query_vars = self.vars.len();
            }
        }
        let mut out = Vec::with_capacity(query.evaluates.len());
        for ev in &query.evaluates {
            let Val::Table(t) = self.eval(&ev.table, &root)? else {
                return Err(DaxError::new(
                    ev.table.span,
                    "EVALUATE needs a table expression: wrap a single value in ROW(\"Name\", …) \
                     or { … }",
                ));
            };
            let mut rows = self.materialise(&t, ev.table.span)?;
            if !ev.order_by.is_empty() {
                rows = self.order_by(rows, &ev.order_by)?;
            }
            out.push(ResultTable {
                columns: rows.columns.into_iter().map(|c| c.name).collect(),
                rows: rows.rows,
            });
        }
        Ok(out)
    }

    fn define_measure(
        &mut self,
        table: &str,
        name: &str,
        expr: &'q Expr,
        span: Span,
    ) -> Result<()> {
        if self.measures.iter().any(|m| same_name(&m.name, name)) {
            return Err(DaxError::new(
                span,
                format!("the Measure [{name}] is defined twice"),
            ));
        }
        let home = self.db.table(table).ok_or_else(|| {
            DaxError::new(span, format!("the Measure's Table '{table}' doesn't exist"))
        })?;
        if self
            .db
            .tables
            .get(home)
            .and_then(|t| t.column(name))
            .is_some()
        {
            return Err(DaxError::new(
                span,
                format!("'{table}' already has a Column [{name}]: a Measure needs another name"),
            ));
        }
        self.measures.push(Measure {
            table: table.into(),
            name: name.into(),
            expr,
        });
        Ok(())
    }

    /// Sorts a result by `ORDER BY` keys, each evaluated in the row context of a result row.
    fn order_by(&mut self, rows: Rows, keys: &[opendash_dax::OrderKey]) -> Result<Rows> {
        let n = rows.rows.len();
        let span = keys.first().map(|k| k.expr.span).unwrap_or_default();
        self.charge(
            n.saturating_mul(
                keys.len()
                    .saturating_mul(VALUE_BYTES)
                    .saturating_add(ROW_BYTES),
            ),
            span,
        )?;
        let shared = Rc::new(TableVal::Rows(rows));
        let root = FilterContext::default();
        let mut sort_keys = Vec::with_capacity(n);
        for row in 0..n {
            if row % storage::CHUNK == 0 {
                self.cancel.check()?;
            }
            let frame = RowFrame::Rows {
                table: Rc::clone(&shared),
                row,
            };
            let k = self.with_row(frame, |ev| {
                keys.iter()
                    .map(|k| ev.scalar(&k.expr, &root))
                    .collect::<Result<Vec<_>>>()
            })?;
            sort_keys.push(k);
        }
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| {
            let (ka, kb) = (sort_keys.get(a), sort_keys.get(b));
            keys.iter()
                .enumerate()
                .map(|(i, k)| {
                    let o = match (ka.and_then(|r| r.get(i)), kb.and_then(|r| r.get(i))) {
                        (Some(x), Some(y)) => x.order(y),
                        _ => Ordering::Equal,
                    };
                    if k.descending { o.reverse() } else { o }
                })
                .find(|o| o.is_ne())
                .unwrap_or(Ordering::Equal)
        });
        let Ok(TableVal::Rows(mut rows)) = Rc::try_unwrap(shared) else {
            return Err(DaxError::new(span, "ORDER BY: the result is still in use"));
        };
        let mut taken: Vec<Option<Vec<Value>>> = rows.rows.drain(..).map(Some).collect();
        rows.rows = order
            .iter()
            .filter_map(|&i| taken.get_mut(i).and_then(Option::take))
            .collect();
        Ok(rows)
    }

    /// Counts `bytes` of intermediate data against the memory cap.
    pub fn charge(&mut self, bytes: usize, span: Span) -> Result<()> {
        if bytes > self.memory_left {
            return Err(DaxError {
                kind: ErrorKind::Memory,
                span,
                message: format!(
                    "the query needs more memory than the memory cap leaves ({} MB)",
                    self.memory_left >> 20
                ),
            });
        }
        self.memory_left -= bytes;
        Ok(())
    }

    /// The row context, outermost first.
    pub fn row_frames(&self) -> &[RowFrame] {
        &self.rows
    }

    /// Runs `f` with `frame` as the innermost row context.
    pub fn with_row<T>(
        &mut self,
        frame: RowFrame,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        self.rows.push(frame);
        let r = f(self);
        self.rows.pop();
        r
    }

    /// Evaluates `e` under `fc`.
    ///
    /// **Stack.** Recursion goes eval → (one arm function) → eval, and [`MAX_EVAL_DEPTH`] levels
    /// of it must fit a 1 MiB stack in debug builds, where every temporary has its own stack
    /// slot. So `eval` only picks the arm (one call site, one result slot), each arm is its own
    /// non-inlined function, and the work between evaluations (operators, conversions, errors)
    /// runs in helpers off the recursive path. As a backstop for forms whose path is inherently
    /// deep (a `CALCULATE` condition iterates a row context, then evaluates), evaluation also
    /// stops with a nesting [`ErrorKind::Limit`] error once it has used [`MAX_EVAL_STACK`] bytes
    /// of stack (measured from a local's address: no `unsafe`, and it works in wasm too).
    pub fn eval(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let here = stack_position();
        if self.depth == 0 {
            self.stack_base = here;
        }
        if self.depth >= MAX_EVAL_DEPTH {
            return Err(too_deep(e.span));
        }
        if here.abs_diff(self.stack_base) > MAX_EVAL_STACK {
            return Err(too_deep_for_stack(e.span));
        }
        self.depth += 1;
        let arm: fn(&mut Self, &Expr, &FilterContext) -> Result<Val> = match &e.kind {
            ExprKind::Integer(_) | ExprKind::Decimal(_) | ExprKind::Text(_) => Self::literal,
            ExprKind::Name(_) => Self::name,
            ExprKind::Column { .. } => Self::column,
            ExprKind::Call { .. } => Self::call,
            ExprKind::Unary(..) => Self::unary,
            ExprKind::Binary(BinOp::In, ..) => |ev, e, fc| functions::logic::in_op(ev, e, fc),
            ExprKind::Binary(..) => Self::binary,
            ExprKind::Var { .. } => Self::var,
            ExprKind::Table(_) => Self::constructor,
        };
        let r = arm(self, e, fc);
        self.depth -= 1;
        r
    }

    #[inline(never)]
    fn literal(&mut self, e: &Expr, _: &FilterContext) -> Result<Val> {
        Ok(Val::Scalar(match &e.kind {
            ExprKind::Integer(i) => Value::Integer(*i),
            ExprKind::Decimal(d) => Value::Decimal(*d),
            ExprKind::Text(s) => Value::text(s),
            _ => return Err(wrong_arm(e.span)),
        }))
    }

    #[inline(never)]
    fn call(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let ExprKind::Call { name, args } = &e.kind else {
            return Err(wrong_arm(e.span));
        };
        let Some(f) = functions::lookup(&name.to_ascii_lowercase()) else {
            return Err(unknown_function(name, e.span));
        };
        f(
            self,
            &Call {
                name,
                args,
                span: e.span,
                fc,
            },
        )
    }

    #[inline(never)]
    fn unary(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let ExprKind::Unary(op, x) = &e.kind else {
            return Err(wrong_arm(e.span));
        };
        let v = self.eval(x, fc);
        unary_op(self, *op, v, x.span, e.span)
    }

    #[inline(never)]
    fn var(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let ExprKind::Var { vars, body } = &e.kind else {
            return Err(wrong_arm(e.span));
        };
        let base = self.vars.len();
        let mut r = Ok(());
        for (name, x) in vars {
            let v = self.eval(x, fc);
            r = self.bind(name, v);
            if r.is_err() {
                break;
            }
        }
        let r = match r {
            Ok(()) => self.eval(body, fc),
            Err(e) => Err(e),
        };
        self.vars.truncate(base);
        r
    }

    /// Puts an evaluated `VAR` in scope.
    #[inline(never)]
    fn bind(&mut self, name: &str, v: Result<Val>) -> Result<()> {
        self.vars.push((name.into(), v?));
        Ok(())
    }

    /// A binary operator (but `IN`, [`functions::logic::in_op`]): both sides are evaluated
    /// here, the operator applied in [`binary_op`].
    #[inline(never)]
    fn binary(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let ExprKind::Binary(op, l, r) = &e.kind else {
            return Err(wrong_arm(e.span));
        };
        let a = self.eval(l, fc);
        let a = self.to_scalar(a, l.span)?;
        let b = self.eval(r, fc);
        binary_op(self, *op, a, b, r.span, e.span)
    }

    /// A bare name: a variable, else a Table (its visible rows).
    #[inline(never)]
    fn name(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let (ExprKind::Name(n), span) = (&e.kind, e.span) else {
            return Err(wrong_arm(e.span));
        };
        if let Some((_, v)) = self.vars.iter().rev().find(|(name, _)| same_name(name, n)) {
            return Ok(v.clone());
        }
        let table = self
            .db
            .table(n)
            .ok_or_else(|| DaxError::new(span, format!("there is no variable or Table '{n}'")))?;
        let rows = self.mask(table, fc, span)?;
        Ok(Val::Table(Rc::new(TableVal::Base { table, rows })))
    }

    /// `T[C]` or `[C]`: a Measure, or a Column's value in the row context.
    #[inline(never)]
    fn column(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let (ExprKind::Column { table, column }, span) = (&e.kind, e.span) else {
            return Err(wrong_arm(e.span));
        };
        let table = table.as_deref();
        if table.is_none()
            && let Some(m) = self.find_measure(None, column)
        {
            return self.measure(m, span, fc);
        }
        if let Some(v) = self.row_value(table, column) {
            return Ok(Val::Scalar(v));
        }
        if let Some(m) = self.find_measure(table, column) {
            return self.measure(m, span, fc);
        }
        Err(self.no_column(table, column, span))
    }

    #[cold]
    #[inline(never)]
    fn no_column(&self, table: Option<&str>, column: &str, span: Span) -> DaxError {
        let name = match table {
            Some(t) => format!("'{t}'[{column}]"),
            None => format!("[{column}]"),
        };
        let exists = match table {
            Some(t) => self
                .db
                .table(t)
                .and_then(|i| self.db.tables.get(i))
                .is_some_and(|t| t.column(column).is_some()),
            None => self.db.tables.iter().any(|t| t.column(column).is_some()),
        };
        DaxError::new(
            span,
            if exists {
                format!(
                    "{name} has no single value here: there is no row context (use it inside an \
                     iterator, or aggregate it)"
                )
            } else {
                format!("there is no Column or Measure {name}")
            },
        )
    }

    /// The value of `T[C]` (or `[C]`) in the innermost row context frame that has it.
    fn row_value(&self, table: Option<&str>, column: &str) -> Option<Value> {
        for frame in self.rows.iter().rev() {
            match frame {
                RowFrame::Base { table: t, row } => {
                    let dt = self.db.tables.get(*t)?;
                    if table.is_none_or(|n| same_name(&dt.name, n))
                        && let Some(c) = dt.column(column).and_then(|c| dt.columns.get(c))
                    {
                        return Some(c.data.value(*row));
                    }
                }
                RowFrame::Rows { table: t, row } => {
                    let TableVal::Rows(rows) = t.as_ref() else {
                        continue;
                    };
                    let found = rows.columns.iter().position(|c| {
                        same_name(&c.name, column)
                            && match (table, c.lineage) {
                                (None, _) => true,
                                (Some(n), Some((lt, _))) => self
                                    .db
                                    .tables
                                    .get(lt)
                                    .is_some_and(|dt| same_name(&dt.name, n)),
                                (Some(_), None) => false,
                            }
                    });
                    if let Some(i) = found {
                        return rows.rows.get(*row).and_then(|r| r.get(i)).cloned();
                    }
                }
            }
        }
        None
    }

    fn find_measure(&self, table: Option<&str>, name: &str) -> Option<usize> {
        self.measures
            .iter()
            .position(|m| same_name(&m.name, name) && table.is_none_or(|t| same_name(&m.table, t)))
    }

    /// Evaluates Measure `m` under `fc`.
    #[inline(never)]
    fn measure(&mut self, m: usize, span: Span, fc: &FilterContext) -> Result<Val> {
        let Some(expr) = self.measures.get(m).map(|x| x.expr) else {
            return Err(DaxError::new(span, "unknown Measure"));
        };
        if self.measure_stack.contains(&m) || self.measure_stack.len() >= MAX_MEASURE_CHAIN {
            return Err(self.measure_error(m, span));
        }
        // A Measure reference is an implicit CALCULATE: context transition, then its expression
        // without the row context.
        let transitioned;
        let fc = if self.rows.is_empty() {
            fc
        } else {
            transitioned = super::calculate::transition(self, fc, span)?;
            &transitioned
        };
        let rows = std::mem::take(&mut self.rows);
        self.measure_stack.push(m);
        // A Measure sees the query's VARs, not the caller's.
        let local = self.vars.split_off(self.query_vars.min(self.vars.len()));
        let r = self.scalar(expr, fc);
        self.vars.truncate(self.query_vars);
        self.vars.extend(local);
        self.measure_stack.pop();
        self.rows = rows;
        r.map(Val::Scalar)
    }

    /// Why Measure `m` can't be evaluated here (see [`Self::measure`]).
    #[cold]
    #[inline(never)]
    fn measure_error(&self, m: usize, span: Span) -> DaxError {
        let name = self.measures.get(m).map_or("", |x| x.name.as_str());
        if self.measure_stack.contains(&m) {
            DaxError::new(span, format!("the Measure [{name}] refers to itself"))
        } else {
            DaxError::limit(
                span,
                format!("Measures refer to Measures more than {MAX_MEASURE_CHAIN} deep"),
            )
        }
    }

    /// `{ a, b }` or `{ (a, b), (c, d) }`.
    #[inline(never)]
    fn constructor(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        let (ExprKind::Table(rows), span) = (&e.kind, e.span) else {
            return Err(wrong_arm(e.span));
        };
        // Not `?` here: its temporaries cost this frame (on the recursive path) a few hundred
        // bytes in debug builds.
        #[allow(clippy::question_mark)]
        let mut out = match self.constructor_start(rows, span) {
            Ok(out) => out,
            Err(e) => return Err(e),
        };
        for r in rows {
            let mut values = Vec::with_capacity(r.len());
            for x in r {
                let v = self.eval(x, fc);
                #[allow(clippy::question_mark)]
                if let Err(e) = self.push_cell(&mut values, v, x.span) {
                    return Err(e);
                }
            }
            out.push(values);
        }
        Ok(constructor_table(out))
    }

    /// Checks a constructor's rows are one width and charges them: the rows to fill.
    #[inline(never)]
    fn constructor_start(&mut self, rows: &[Vec<Expr>], span: Span) -> Result<Vec<Vec<Value>>> {
        let width = rows.first().map_or(1, Vec::len);
        if let Some(bad) = rows.iter().find(|r| r.len() != width) {
            let at = bad.first().map_or(span, |e| e.span);
            return Err(DaxError::new(
                at,
                format!("every row of a table constructor needs {width} values"),
            ));
        }
        self.charge(
            rows.len()
                .saturating_mul(width.saturating_mul(VALUE_BYTES).saturating_add(ROW_BYTES)),
            span,
        )?;
        Ok(Vec::with_capacity(rows.len()))
    }

    /// Adds an evaluated constructor cell to `values`, charged.
    #[inline(never)]
    fn push_cell(&mut self, values: &mut Vec<Value>, v: Result<Val>, span: Span) -> Result<()> {
        let v = self.to_scalar(v, span)?;
        self.charge(text_bytes(&v), span)?;
        values.push(v);
        Ok(())
    }

    /// Evaluates `e` to a scalar: a one-column table of at most one row converts.
    pub fn scalar(&mut self, e: &Expr, fc: &FilterContext) -> Result<Value> {
        let r = self.eval(e, fc);
        self.to_scalar(r, e.span)
    }

    /// An evaluation's result as a scalar, as [`Self::scalar`] converts it.
    #[inline(never)]
    pub fn to_scalar(&self, r: Result<Val>, span: Span) -> Result<Value> {
        match r? {
            Val::Scalar(v) => Ok(v),
            Val::Table(t) => self.table_scalar(&t, span),
        }
    }

    #[inline(never)]
    fn table_scalar(&self, t: &TableVal, span: Span) -> Result<Value> {
        let width = match t {
            TableVal::Base { table, .. } => {
                self.db.tables.get(*table).map_or(0, |t| t.columns.len())
            }
            TableVal::Rows(r) => r.columns.len(),
        };
        if width != 1 {
            return Err(DaxError::new(
                span,
                format!("a table of {width} columns can't be used as a single value"),
            ));
        }
        match self.row_count(t)? {
            0 => Ok(Value::Blank),
            1 => Ok(match t {
                TableVal::Base { table, rows } => {
                    let row = rows
                        .as_ref()
                        .map_or(Some(0), |m| m.ones_in(0, m.len()).next());
                    let col = self.db.tables.get(*table).and_then(|t| t.columns.first());
                    match (row, col) {
                        (Some(r), Some(c)) => c.data.value(r),
                        _ => Value::Blank,
                    }
                }
                TableVal::Rows(r) => r
                    .rows
                    .first()
                    .and_then(|r| r.first())
                    .cloned()
                    .unwrap_or(Value::Blank),
            }),
            _ => Err(DaxError::new(
                span,
                "a table of several values was given where a single value is needed",
            )),
        }
    }

    /// Evaluates `e` to a table.
    pub fn table(&mut self, e: &Expr, fc: &FilterContext) -> Result<Rc<TableVal>> {
        match self.eval(e, fc)? {
            Val::Table(t) => Ok(t),
            Val::Scalar(_) => Err(DaxError::new(
                e.span,
                "a table is needed here, not a single value",
            )),
        }
    }

    /// The number of rows of `t` (a storage scan for a model Table).
    pub fn row_count(&self, t: &TableVal) -> Result<usize> {
        match t {
            TableVal::Base { table, rows } => {
                let n = self.db.tables.get(*table).map_or(0, |t| t.rows);
                storage::count(n, rows.as_ref(), self.cancel)
            }
            TableVal::Rows(r) => Ok(r.rows.len()),
        }
    }

    /// `t` as rows of values (a storage scan for a model Table), charged to the memory cap.
    pub fn materialise(&mut self, t: &TableVal, span: Span) -> Result<Rows> {
        let (table, mask) = match t {
            TableVal::Rows(r) => return Ok(r.clone()),
            TableVal::Base { table, rows } => (*table, rows.as_ref()),
        };
        let dt = self
            .db
            .tables
            .get(table)
            .ok_or_else(|| DaxError::new(span, "unknown Table"))?;
        let n = storage::count(dt.rows, mask, self.cancel)?;
        let width = dt.columns.len();
        self.charge(
            n.saturating_mul(width.saturating_mul(VALUE_BYTES).saturating_add(ROW_BYTES)),
            span,
        )?;
        let mut out = Vec::with_capacity(n);
        let (mut text, left) = (0usize, self.memory_left);
        storage::scan(dt.rows, mask, self.cancel, |r| {
            let row: Vec<Value> = dt.columns.iter().map(|c| c.data.value(r)).collect();
            text = dt
                .columns
                .iter()
                .fold(text, |a, c| a.saturating_add(c.data.text_bytes(r)));
            if text > left {
                return Err(DaxError {
                    kind: ErrorKind::Memory,
                    span,
                    message: "the query needs more memory than the memory cap leaves".into(),
                });
            }
            out.push(row);
            Ok(())
        })?;
        self.charge(text, span)?;
        Ok(Rows {
            columns: dt
                .columns
                .iter()
                .enumerate()
                .map(|(c, col)| RowsColumn {
                    name: col.name.clone(),
                    lineage: Some((table, c)),
                })
                .collect(),
            rows: out,
        })
    }

    /// The rows of model Table `table` visible under `fc` (`None`: all): its own filters,
    /// through active Relationships (many → one) the filters on the Tables it reaches, and the
    /// row-set filters of Tables that reach it (a table filter filters its *expanded* Table:
    /// the one side keeps the rows the filter's rows reach).
    pub fn mask(&self, table: usize, fc: &FilterContext, span: Span) -> Result<Option<Bitset>> {
        let mut path = Vec::new();
        self.mask_on_path(table, fc, span, &mut path)
    }

    fn mask_on_path(
        &self,
        table: usize,
        fc: &FilterContext,
        span: Span,
        path: &mut Vec<usize>,
    ) -> Result<Option<Bitset>> {
        if path.len() >= MAX_RELATIONSHIP_DEPTH {
            return Err(DaxError::limit(
                span,
                format!("filters cross more than {MAX_RELATIONSHIP_DEPTH} Relationships"),
            ));
        }
        let Some(t) = self.db.tables.get(table) else {
            return Ok(None);
        };
        path.push(table);
        let mut mask = None;
        let mut result = Ok(());
        for f in &fc.filters {
            let step = match f.as_ref() {
                _ if f.table() == table => f.rows(t, self.cancel).map(Some),
                // The Tables on the path apply their own row sets.
                Filter::Rows { table: from, rows } if !path.contains(from) => {
                    let mut seen = vec![*from];
                    self.project_up(*from, rows, table, span, &mut seen)
                }
                Filter::Rows { .. } | Filter::Column { .. } => Ok(None),
            };
            match step {
                Ok(Some(rows)) => mask = and(mask, rows),
                Ok(None) => {}
                Err(e) => {
                    result = Err(e);
                    break;
                }
            }
        }
        if result.is_ok() {
            for r in &self.db.relationships {
                if !r.active || r.from_table != table || path.contains(&r.to_table) {
                    continue;
                }
                let step = (|| {
                    let Some(one) = self.mask_on_path(r.to_table, fc, span, path)? else {
                        return Ok(None);
                    };
                    let (Some(to), Some(from)) =
                        (self.db.tables.get(r.to_table), t.columns.get(r.from_column))
                    else {
                        return Ok(None);
                    };
                    let Some(key) = to.columns.get(r.to_column) else {
                        return Ok(None);
                    };
                    let keys = storage::key_set(to, &key.data, &one, self.cancel)?;
                    storage::semijoin(t, &from.data, &keys, self.cancel).map(Some)
                })();
                match step {
                    Ok(Some(rows)) => mask = and(mask, rows),
                    Ok(None) => {}
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
            }
        }
        path.pop();
        result.map(|()| mask)
    }

    /// The rows of `target` that rows `rows` of Table `from` reach through active Relationships
    /// (many → one); `None` when `target` isn't reached. `seen`: the Tables on the way.
    fn project_up(
        &self,
        from: usize,
        rows: &Bitset,
        target: usize,
        span: Span,
        seen: &mut Vec<usize>,
    ) -> Result<Option<Bitset>> {
        if seen.len() > MAX_RELATIONSHIP_DEPTH {
            return Err(DaxError::limit(
                span,
                format!("filters cross more than {MAX_RELATIONSHIP_DEPTH} Relationships"),
            ));
        }
        let Some(t) = self.db.tables.get(from) else {
            return Ok(None);
        };
        let mut out = None;
        for r in &self.db.relationships {
            if !r.active || r.from_table != from || seen.contains(&r.to_table) {
                continue;
            }
            if r.to_table != target && !self.reaches(r.to_table, target, seen) {
                continue;
            }
            let (Some(to), Some(key)) =
                (self.db.tables.get(r.to_table), t.columns.get(r.from_column))
            else {
                continue;
            };
            let Some(one_key) = to.columns.get(r.to_column) else {
                continue;
            };
            let keys = storage::key_set(t, &key.data, rows, self.cancel)?;
            let reached = storage::semijoin(to, &one_key.data, &keys, self.cancel)?;
            if r.to_table == target {
                out = and(out, reached);
                continue;
            }
            seen.push(r.to_table);
            let further = self.project_up(r.to_table, &reached, target, span, seen);
            seen.pop();
            if let Some(m) = further? {
                out = and(out, m);
            }
        }
        Ok(out)
    }

    /// Whether `target` is reachable from `from` over active Relationships (many → one)
    /// without passing the Tables in `avoid`.
    fn reaches(&self, from: usize, target: usize, avoid: &[usize]) -> bool {
        let mut seen = vec![from];
        let mut next = 0;
        while let Some(&t) = seen.get(next) {
            next += 1;
            for r in &self.db.relationships {
                if !r.active || r.from_table != t {
                    continue;
                }
                if r.to_table == target {
                    return true;
                }
                if !seen.contains(&r.to_table) && !avoid.contains(&r.to_table) {
                    seen.push(r.to_table);
                }
            }
        }
        false
    }
}

/// Where the stack is now (the address of a local): [`Evaluator::eval`] measures how much
/// stack evaluation has used from it.
#[inline(always)]
fn stack_position() -> usize {
    let marker = 0u8;
    std::ptr::addr_of!(marker) as usize
}

/// A unary operator applied to its evaluated operand.
#[inline(never)]
fn unary_op(
    ev: &Evaluator<'_, '_>,
    op: UnaryOp,
    x: Result<Val>,
    x_span: Span,
    span: Span,
) -> Result<Val> {
    let v = ev.to_scalar(x, x_span)?;
    let r = match op {
        UnaryOp::Negate => v.negate(),
        UnaryOp::Plus => Ok(v),
        UnaryOp::Not => v.not(),
    };
    r.map(Val::Scalar).map_err(|e| value_error(span, &e))
}

/// A binary operator (not `IN`) applied to its evaluated operands.
#[inline(never)]
fn binary_op(
    ev: &Evaluator<'_, '_>,
    op: BinOp,
    a: Value,
    b: Result<Val>,
    b_span: Span,
    span: Span,
) -> Result<Val> {
    let b = ev.to_scalar(b, b_span)?;
    let ord = |a: &Value, b: &Value, f: fn(Ordering) -> bool| {
        a.compare(b).map(|o| Value::Boolean(o.is_some_and(f)))
    };
    let v = match op {
        BinOp::Add => a.add(&b),
        BinOp::Subtract => a.subtract(&b),
        BinOp::Multiply => a.multiply(&b),
        BinOp::Divide => a.divide(&b),
        BinOp::Power => a.power(&b),
        BinOp::Concat => Ok(a.concat(&b)),
        BinOp::And => a.and(&b),
        BinOp::Or => a.or(&b),
        BinOp::Equal => a.equals(&b).map(Value::Boolean),
        BinOp::StrictEqual => a.strict_equals(&b).map(Value::Boolean),
        BinOp::NotEqual => a
            .compare(&b)
            .map(|o| Value::Boolean(o != Some(Ordering::Equal))),
        BinOp::Less => ord(&a, &b, Ordering::is_lt),
        BinOp::LessEqual => ord(&a, &b, Ordering::is_le),
        BinOp::Greater => ord(&a, &b, Ordering::is_gt),
        BinOp::GreaterEqual => ord(&a, &b, Ordering::is_ge),
        BinOp::In => return Err(wrong_arm(span)),
    };
    v.map(Val::Scalar).map_err(|e| value_error(span, &e))
}

/// The table a constructor's rows make.
#[inline(never)]
fn constructor_table(rows: Vec<Vec<Value>>) -> Val {
    let width = rows.first().map_or(1, Vec::len);
    let columns = if width == 1 {
        vec![RowsColumn::new("Value")]
    } else {
        (1..=width)
            .map(|i| RowsColumn::new(&format!("Value{i}")))
            .collect()
    };
    Val::Table(Rc::new(TableVal::Rows(Rows { columns, rows })))
}

#[cold]
#[inline(never)]
fn unknown_function(name: &str, span: Span) -> DaxError {
    DaxError::new(
        span,
        format!("OpenDash BI doesn't know the function {name} (yet)"),
    )
}

/// An arm of [`Evaluator::eval`] given an expression of another kind (never happens).
#[cold]
#[inline(never)]
fn wrong_arm(span: Span) -> DaxError {
    DaxError::new(
        span,
        "internal error: the evaluator mismatched an expression",
    )
}

#[cold]
#[inline(never)]
fn too_deep(span: Span) -> DaxError {
    DaxError::limit(
        span,
        format!(
            "the expression (with the Measures it uses) nests more than {MAX_EVAL_DEPTH} levels \
             deep"
        ),
    )
}

#[cold]
#[inline(never)]
fn too_deep_for_stack(span: Span) -> DaxError {
    DaxError::limit(
        span,
        format!(
            "the expression (with the Measures it uses) nests too deep: its levels need more \
             than {} KiB of stack",
            MAX_EVAL_STACK >> 10
        ),
    )
}

#[cold]
#[inline(never)]
pub(super) fn value_error(span: Span, e: &opendash_core::ValueError) -> DaxError {
    DaxError::new(span, e.to_string())
}

/// Bytes a value's text costs when materialised.
fn text_bytes(v: &Value) -> usize {
    match v {
        Value::Text(s) => s.len(),
        _ => 0,
    }
}
