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
use super::storage::{self, Bitset, Cancel, DbTable, Filter, Key, and};
use super::{
    Database, DaxError, ErrorKind, FilterContext, MAX_EVAL_DEPTH, MAX_EVAL_STACK,
    MAX_MEASURE_CHAIN, MAX_RELATIONSHIP_DEPTH, MAX_TEXT_BYTES, Options, Result, ResultTable,
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
    /// Bytes intermediate data may use: the memory cap less the loaded data.
    budget: usize,
    /// Bytes charged by the evaluations in progress ([`Self::charge`]); a finished function
    /// call, `VAR` block, Measure or row of an iteration refunds what it charged but for its
    /// result's own bytes ([`Self::end_scope`]).
    used: usize,
    /// Bytes charged for the whole query: caches kept across calls ([`Self::pin`]).
    pinned: usize,
    measures: Vec<Measure<'q>>,
    /// [`Self::measures`] by [`crate::name_key`] (names are unique ignoring case).
    measure_names: HashMap<String, usize>,
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
            budget: options.memory_cap.saturating_sub(db.loaded_bytes),
            used: 0,
            pinned: 0,
            measures: Vec::new(),
            measure_names: HashMap::new(),
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
        if folded_get(&self.measure_names, name).is_some() {
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
        self.measure_names
            .insert(crate::name_key(name), self.measures.len());
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
            self.charge(
                k.iter().map(text_bytes).fold(0, usize::saturating_add),
                span,
            )?;
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

    /// Counts `bytes` of intermediate data against the memory cap, until the function call,
    /// `VAR` block, Measure or iteration row charging it finishes (see [`Self::end_scope`]).
    pub fn charge(&mut self, bytes: usize, span: Span) -> Result<()> {
        let left = self.memory_left();
        if bytes > left {
            return Err(out_of_memory(span, left));
        }
        self.used = self.used.saturating_add(bytes);
        Ok(())
    }

    /// Counts `bytes` against the memory cap for the rest of the query: data kept across calls
    /// (a cache such as `RELATED`'s lookups).
    pub fn pin(&mut self, bytes: usize, span: Span) -> Result<()> {
        let left = self.memory_left();
        if bytes > left {
            return Err(out_of_memory(span, left));
        }
        self.pinned = self.pinned.saturating_add(bytes);
        Ok(())
    }

    /// Bytes intermediate data may still use.
    pub fn memory_left(&self) -> usize {
        self.budget
            .saturating_sub(self.used.saturating_add(self.pinned))
    }

    /// Ends a memory scope begun when [`Self::used`] was `before`: what the scope charged is
    /// refunded (its temporaries are dropped), but for the bytes its result still holds.
    #[inline(never)]
    fn end_scope(&mut self, before: usize, result: &Result<Val>) {
        let kept = match result {
            Ok(v) => val_bytes(v),
            Err(_) => 0,
        };
        self.used = self.used.min(before.saturating_add(kept));
    }

    /// The row context, outermost first.
    pub fn row_frames(&self) -> &[RowFrame] {
        &self.rows
    }

    /// Runs `f` with `frame` as the innermost row context. The memory `f` charges is refunded
    /// when it returns: a caller keeping its result charges what it keeps.
    pub fn with_row<T>(
        &mut self,
        frame: RowFrame,
        f: impl FnOnce(&mut Self) -> Result<T>,
    ) -> Result<T> {
        let before = self.used;
        self.rows.push(frame);
        let r = f(self);
        self.rows.pop();
        self.used = self.used.min(before);
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
        let before = self.used;
        let r = f(
            self,
            &Call {
                name,
                args,
                span: e.span,
                fc,
            },
        );
        self.end_scope(before, &r);
        r
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
        let before = self.used;
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
        self.end_scope(before, &r);
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
        let &m = folded_get(&self.measure_names, name)?;
        let home = &self.measures.get(m)?.table;
        table.is_none_or(|t| same_name(home, t)).then_some(m)
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
        let before = self.used;
        self.measure_stack.push(m);
        // A Measure sees the query's VARs, not the caller's.
        let local = self.vars.split_off(self.query_vars.min(self.vars.len()));
        let r = self.scalar(expr, fc);
        self.vars.truncate(self.query_vars);
        self.vars.extend(local);
        self.measure_stack.pop();
        self.rows = rows;
        let r = r.map(Val::Scalar);
        self.end_scope(before, &r);
        r
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
            TableVal::Rows(r) => {
                self.charge(rows_bytes(r), span)?;
                return Ok(r.clone());
            }
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
        let (mut text, left) = (0usize, self.memory_left());
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
    ///
    /// The Tables on the way from `table` apply their own row sets: a Table reached from them
    /// leaves their row-set filters out (as `excluded` bits), so a row of theirs whose key has no
    /// match isn't dropped by its own filter's projection.
    ///
    /// Each Table's rows (per set of row-set filters left out), and each row-set filter's
    /// projection onto a Table, are worked out once per call ([`Walk`]), so stacked diamonds of
    /// Relationships cost linear time, not one pass per path. Depth along Relationships is
    /// capped at [`MAX_RELATIONSHIP_DEPTH`].
    pub fn mask(&self, table: usize, fc: &FilterContext, span: Span) -> Result<Option<Bitset>> {
        let n = self.db.tables.len();
        let mut walk = Walk {
            masks: HashMap::new(),
            busy: vec![false; n],
            sources: vec![0; n],
            bits: Vec::with_capacity(fc.filters.len()),
            projections: HashMap::new(),
        };
        let mut next = 0u32;
        for f in &fc.filters {
            walk.bits.push(match f.as_ref() {
                Filter::Rows { table: from, .. } => {
                    let bit = 1u64.checked_shl(next).ok_or_else(|| {
                        DaxError::limit(span, "more than 64 table filters apply at once")
                    })?;
                    next += 1;
                    if let Some(s) = walk.sources.get_mut(*from) {
                        *s |= bit;
                    }
                    bit
                }
                Filter::Column { .. } => 0,
            });
        }
        self.mask_of(table, 0, fc, span, 0, &mut walk)
    }

    fn mask_of(
        &self,
        table: usize,
        excluded: u64,
        fc: &FilterContext,
        span: Span,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Option<Bitset>> {
        if let Some(done) = walk.masks.get(&(table, excluded)) {
            return Ok(done.clone());
        }
        if depth >= MAX_RELATIONSHIP_DEPTH {
            return Err(too_many_relationships(span));
        }
        let Some(t) = self.db.tables.get(table) else {
            return Ok(None);
        };
        self.cancel.check()?;
        if let Some(b) = walk.busy.get_mut(table) {
            *b = true;
        }
        let r = self.mask_parts(table, t, excluded, fc, span, depth, walk);
        if let Some(b) = walk.busy.get_mut(table) {
            *b = false;
        }
        let mask = r?;
        // The Table asked for is never reached again: only the ones below it are memoised.
        if depth > 0 {
            walk.masks.insert((table, excluded), mask.clone());
        }
        Ok(mask)
    }

    /// [`Self::mask_of`]'s work: `table`'s own filters, the row-set filters reaching it (but
    /// the `excluded` ones), and the Tables it reaches (those on the way to it skipped: a
    /// cycle).
    #[allow(clippy::too_many_arguments)]
    fn mask_parts(
        &self,
        table: usize,
        t: &DbTable,
        excluded: u64,
        fc: &FilterContext,
        span: Span,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Option<Bitset>> {
        let mut mask = None;
        for (i, f) in fc.filters.iter().enumerate() {
            let bit = walk.bits.get(i).copied().unwrap_or(0);
            let rows = match f.as_ref() {
                _ if f.table() == table => Some(f.rows(t, self.cancel)?),
                Filter::Rows { .. } if excluded & bit != 0 => None,
                Filter::Rows { table: from, rows } => {
                    self.projection(i, *from, rows, table, span, depth, walk)?
                }
                Filter::Column { .. } => None,
            };
            if let Some(rows) = rows {
                mask = and(mask, rows);
            }
        }
        let below = excluded | walk.sources.get(table).copied().unwrap_or(0);
        for r in &self.db.relationships {
            if !r.active || r.from_table != table || walk.busy.get(r.to_table) != Some(&false) {
                continue;
            }
            let Some(one) = self.mask_of(r.to_table, below, fc, span, depth + 1, walk)? else {
                continue;
            };
            let (Some(to), Some(from)) =
                (self.db.tables.get(r.to_table), t.columns.get(r.from_column))
            else {
                continue;
            };
            let Some(key) = to.columns.get(r.to_column) else {
                continue;
            };
            let keys = storage::key_set(to, &key.data, &one, self.cancel)?;
            mask = and(mask, storage::semijoin(t, &from.data, &keys, self.cancel)?);
        }
        Ok(mask)
    }

    /// The rows of `target` that rows `rows` of Table `from` (filter `filter` of the context)
    /// reach through active Relationships (many → one), ANDed over the Relationships into
    /// `target`; `None` when `target` isn't reached. Memoised per filter and Table in `walk`.
    #[allow(clippy::too_many_arguments)]
    fn projection(
        &self,
        filter: usize,
        from: usize,
        rows: &Bitset,
        target: usize,
        span: Span,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Option<Bitset>> {
        if depth >= MAX_RELATIONSHIP_DEPTH {
            return Err(too_many_relationships(span));
        }
        let n = self.db.tables.len();
        let p = walk
            .projections
            .entry(filter)
            .or_insert_with(|| Projection {
                reach: self.reachable(from),
                rows: vec![None; n],
                busy: vec![false; n],
            });
        if p.reach.get(target) != Some(&true) {
            return Ok(None);
        }
        if let Some(Some(done)) = p.rows.get(target) {
            return Ok(done.clone());
        }
        self.cancel.check()?;
        if let Some(b) = p.busy.get_mut(target) {
            *b = true;
        }
        let r = self.projection_parts(filter, from, rows, target, span, depth, walk);
        if let Some(p) = walk.projections.get_mut(&filter) {
            if let Some(b) = p.busy.get_mut(target) {
                *b = false;
            }
            if let (Ok(out), Some(slot)) = (&r, p.rows.get_mut(target)) {
                *slot = Some(out.clone());
            }
        }
        r
    }

    /// [`Self::projection`]'s work: over each active Relationship into `target` from `from` or
    /// a Table `from` reaches, the source rows' keys matched on `target`.
    #[allow(clippy::too_many_arguments)]
    fn projection_parts(
        &self,
        filter: usize,
        from: usize,
        rows: &Bitset,
        target: usize,
        span: Span,
        depth: usize,
        walk: &mut Walk,
    ) -> Result<Option<Bitset>> {
        let Some(to) = self.db.tables.get(target) else {
            return Ok(None);
        };
        let mut out = None;
        for r in &self.db.relationships {
            if !r.active || r.to_table != target || r.from_table == target {
                continue;
            }
            let source = if r.from_table == from {
                Some(rows.clone())
            } else {
                let usable = walk.projections.get(&filter).is_some_and(|p| {
                    p.reach.get(r.from_table) == Some(&true)
                        && p.busy.get(r.from_table) == Some(&false)
                });
                if !usable {
                    continue;
                }
                self.projection(filter, from, rows, r.from_table, span, depth + 1, walk)?
            };
            let Some(source) = source else {
                continue;
            };
            let (Some(t), Some(one_key)) = (
                self.db.tables.get(r.from_table),
                to.columns.get(r.to_column),
            ) else {
                continue;
            };
            let Some(key) = t.columns.get(r.from_column) else {
                continue;
            };
            let keys = storage::key_set(t, &key.data, &source, self.cancel)?;
            out = and(
                out,
                storage::semijoin(to, &one_key.data, &keys, self.cancel)?,
            );
        }
        Ok(out)
    }

    /// The Tables reachable from `from` over active Relationships (many → one), by index.
    fn reachable(&self, from: usize) -> Vec<bool> {
        let mut reach = vec![false; self.db.tables.len()];
        let mut queue = vec![from];
        while let Some(t) = queue.pop() {
            for r in &self.db.relationships {
                if r.active
                    && r.from_table == t
                    && let Some(slot) = reach.get_mut(r.to_table)
                    && !*slot
                {
                    *slot = true;
                    queue.push(r.to_table);
                }
            }
        }
        reach
    }
}

/// [`Evaluator::mask`]'s memo: each Table's visible rows once worked out, the Tables being
/// worked out (a Relationship back to one is a cycle, skipped), and the row-set filters'
/// projections.
struct Walk {
    /// By (Table, row-set filters left out).
    masks: HashMap<(usize, u64), Option<Bitset>>,
    busy: Vec<bool>,
    /// Per Table, the bits of the row-set filters it is the Table of.
    sources: Vec<u64>,
    /// Per filter of the context, its bit (0: not a row-set filter).
    bits: Vec<u64>,
    projections: HashMap<usize, Projection>,
}

/// One row-set filter's projection onto the Tables its Table reaches.
struct Projection {
    reach: Vec<bool>,
    rows: Vec<Option<Option<Bitset>>>,
    busy: Vec<bool>,
}

#[cold]
#[inline(never)]
fn too_many_relationships(span: Span) -> DaxError {
    DaxError::limit(
        span,
        format!("filters cross more than {MAX_RELATIONSHIP_DEPTH} Relationships"),
    )
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
    ev: &mut Evaluator<'_, '_>,
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
        BinOp::Concat => return concat(ev, &a, &b, span),
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

/// `a & b`: its text is at most [`MAX_TEXT_BYTES`] long and charged to the memory cap, so
/// text doubled through `VAR`s fails fast rather than exhausting memory (review M3 #1).
#[inline(never)]
fn concat(ev: &mut Evaluator<'_, '_>, a: &Value, b: &Value, span: Span) -> Result<Val> {
    if text_bytes(a).saturating_add(text_bytes(b)) > MAX_TEXT_BYTES {
        return Err(text_too_long(span));
    }
    let v = a.concat(b);
    let n = text_bytes(&v);
    if n > MAX_TEXT_BYTES {
        return Err(text_too_long(span));
    }
    ev.charge(n, span)?;
    Ok(Val::Scalar(v))
}

#[cold]
#[inline(never)]
fn text_too_long(span: Span) -> DaxError {
    DaxError::limit(
        span,
        format!(
            "the text would be longer than {} MiB, the longest a text value may be",
            MAX_TEXT_BYTES >> 20
        ),
    )
}

#[cold]
#[inline(never)]
fn out_of_memory(span: Span, left: usize) -> DaxError {
    DaxError {
        kind: ErrorKind::Memory,
        span,
        message: format!(
            "the query needs more memory than the memory cap leaves ({} MB)",
            left >> 20
        ),
    }
}

/// `map[name]` ignoring case, for a map keyed by [`crate::name_key`]: an ASCII name of up to 64
/// bytes is folded on the stack (no allocation per lookup).
fn folded_get<'m, V>(map: &'m HashMap<String, V>, name: &str) -> Option<&'m V> {
    let mut buf = [0u8; 64];
    if name.is_ascii()
        && let Some(b) = buf.get_mut(..name.len())
    {
        b.copy_from_slice(name.as_bytes());
        b.make_ascii_lowercase();
        return map.get(std::str::from_utf8(b).ok()?);
    }
    map.get(&crate::name_key(name))
}

/// Bytes a result holds, as charged while it was built: a materialised table's values and
/// rows, a row set's bits, a text's bytes.
fn val_bytes(v: &Val) -> usize {
    match v {
        Val::Scalar(v) => text_bytes(v),
        Val::Table(t) => match t.as_ref() {
            TableVal::Base { rows, .. } => rows
                .as_ref()
                .map_or(0, |b| b.words().len().saturating_mul(8)),
            TableVal::Rows(r) => rows_bytes(r),
        },
    }
}

/// Bytes materialised rows cost, as [`Evaluator::materialise`] charges them.
pub(super) fn rows_bytes(r: &Rows) -> usize {
    let width = r.columns.len();
    let text = r
        .rows
        .iter()
        .flatten()
        .map(text_bytes)
        .fold(0usize, usize::saturating_add);
    r.rows
        .len()
        .saturating_mul(width.saturating_mul(VALUE_BYTES).saturating_add(ROW_BYTES))
        .saturating_add(text)
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
