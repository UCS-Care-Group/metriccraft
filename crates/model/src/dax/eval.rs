//! The formula engine (ADR 0006): interprets the `opendash_dax` AST under a filter context and
//! a row context stack. Aggregations go to storage scans ([`super::storage`]); functions are in
//! [`super::functions`].
//!
//! Semantics chosen here, to settle with parity cases:
//! - `VAR`s are evaluated where they are defined (eagerly), so an unused `VAR` that fails fails
//!   the expression; DAX may evaluate them lazily.
//! - `&&` and `||` evaluate both sides.
//! - A Measure reference inside a row context needs context transition (M3.4): an error for now.
//! - A table of one column and at most one row is a scalar where one is needed (empty: BLANK).
//! - `{ … }` names its columns `Value` (one column) or `Value1`, `Value2`, ….

use std::cmp::Ordering;
use std::rc::Rc;

use opendash_core::Value;
use opendash_dax::{BinOp, Define, Expr, ExprKind, Query, Span, UnaryOp};

use super::functions::{self, Call};
use super::storage::{self, Bitset, Cancel, and};
use super::{
    Database, DaxError, ErrorKind, FilterContext, MAX_EVAL_DEPTH, MAX_MEASURE_CHAIN,
    MAX_RELATIONSHIP_DEPTH, Options, Result, ResultTable,
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
    // Recursion goes eval → (one small arm function) → scalar/eval: every arm is its own
    // non-inlined function so the frames on the recursive path stay small (MAX_EVAL_DEPTH
    // levels must fit a 1 MiB stack in debug builds; see the depth test).
    pub fn eval(&mut self, e: &Expr, fc: &FilterContext) -> Result<Val> {
        if self.depth >= MAX_EVAL_DEPTH {
            return Err(too_deep(e.span));
        }
        self.depth += 1;
        let span = e.span;
        let r = match &e.kind {
            ExprKind::Integer(i) => Ok(Val::Scalar(Value::Integer(*i))),
            ExprKind::Decimal(d) => Ok(Val::Scalar(Value::Decimal(*d))),
            ExprKind::Text(s) => Ok(Val::Scalar(Value::text(s))),
            ExprKind::Name(n) => self.name(n, span, fc),
            ExprKind::Column { table, column } => self.column(table.as_deref(), column, span, fc),
            ExprKind::Call { name, args } => self.call(name, args, span, fc),
            ExprKind::Unary(op, x) => self.unary(*op, x, span, fc),
            ExprKind::Binary(op, l, r) => self.binary(*op, l, r, span, fc),
            ExprKind::Var { vars, body } => self.var(vars, body, fc),
            ExprKind::Table(rows) => self.constructor(rows, span, fc),
        };
        self.depth -= 1;
        r
    }

    #[inline(never)]
    fn call(&mut self, name: &str, args: &[Expr], span: Span, fc: &FilterContext) -> Result<Val> {
        let Some(f) = functions::lookup(&name.to_ascii_lowercase()) else {
            return Err(DaxError::new(
                span,
                format!("OpenDash BI doesn't know the function {name} (yet)"),
            ));
        };
        f(
            self,
            &Call {
                name,
                args,
                span,
                fc,
            },
        )
    }

    #[inline(never)]
    fn unary(&mut self, op: UnaryOp, x: &Expr, span: Span, fc: &FilterContext) -> Result<Val> {
        let v = self.scalar(x, fc)?;
        let r = match op {
            UnaryOp::Negate => v.negate(),
            UnaryOp::Plus => Ok(v),
            UnaryOp::Not => v.not(),
        };
        r.map(Val::Scalar).map_err(|e| value_error(span, &e))
    }

    #[inline(never)]
    fn var(&mut self, vars: &[(String, Expr)], body: &Expr, fc: &FilterContext) -> Result<Val> {
        let base = self.vars.len();
        let mut r = Ok(());
        for (name, x) in vars {
            match self.eval(x, fc) {
                Ok(v) => self.vars.push((name.clone(), v)),
                Err(e) => {
                    r = Err(e);
                    break;
                }
            }
        }
        let r = r.and_then(|()| self.eval(body, fc));
        self.vars.truncate(base);
        r
    }

    #[inline(never)]
    fn binary(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        span: Span,
        fc: &FilterContext,
    ) -> Result<Val> {
        if op == BinOp::In {
            // TODO(M3.5): `IN` (CONTAINSROW semantics).
            return Err(DaxError::new(span, "OpenDash BI doesn't support IN (yet)"));
        }
        let a = self.scalar(l, fc)?;
        let b = self.scalar(r, fc)?;
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
            BinOp::In => Ok(Value::Blank),
        };
        v.map(Val::Scalar).map_err(|e| value_error(span, &e))
    }

    /// A bare name: a variable, else a Table (its visible rows).
    #[inline(never)]
    fn name(&mut self, n: &str, span: Span, fc: &FilterContext) -> Result<Val> {
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
    fn column(
        &mut self,
        table: Option<&str>,
        column: &str,
        span: Span,
        fc: &FilterContext,
    ) -> Result<Val> {
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
        // TODO(M3.4): context transition turns the row context into filters here.
        if !self.rows.is_empty()
            || self.measure_stack.contains(&m)
            || self.measure_stack.len() >= MAX_MEASURE_CHAIN
        {
            return Err(self.measure_error(m, span));
        }
        self.measure_stack.push(m);
        // A Measure sees the query's VARs, not the caller's.
        let local = self.vars.split_off(self.query_vars.min(self.vars.len()));
        let r = self.scalar(expr, fc);
        self.vars.truncate(self.query_vars);
        self.vars.extend(local);
        self.measure_stack.pop();
        r.map(Val::Scalar)
    }

    /// Why Measure `m` can't be evaluated here (see [`Self::measure`]).
    #[cold]
    #[inline(never)]
    fn measure_error(&self, m: usize, span: Span) -> DaxError {
        let name = self.measures.get(m).map_or("", |x| x.name.as_str());
        if !self.rows.is_empty() {
            DaxError::new(
                span,
                format!(
                    "[{name}] is used inside a row context: context transition isn't supported \
                     (yet)"
                ),
            )
        } else if self.measure_stack.contains(&m) {
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
    fn constructor(&mut self, rows: &[Vec<Expr>], span: Span, fc: &FilterContext) -> Result<Val> {
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
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut values = Vec::with_capacity(width);
            for x in r {
                let v = self.scalar(x, fc)?;
                self.charge(text_bytes(&v), x.span)?;
                values.push(v);
            }
            out.push(values);
        }
        let columns = if width == 1 {
            vec![RowsColumn::new("Value")]
        } else {
            (1..=width)
                .map(|i| RowsColumn::new(&format!("Value{i}")))
                .collect()
        };
        Ok(Val::Table(Rc::new(TableVal::Rows(Rows {
            columns,
            rows: out,
        }))))
    }

    /// Evaluates `e` to a scalar: a one-column table of at most one row converts.
    pub fn scalar(&mut self, e: &Expr, fc: &FilterContext) -> Result<Value> {
        match self.eval(e, fc)? {
            Val::Scalar(v) => Ok(v),
            Val::Table(t) => self.table_scalar(&t, e.span),
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

    /// The rows of model Table `table` visible under `fc` (`None`: all): its own filters, and
    /// through active Relationships (many → one) the filters on the Tables it reaches.
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
        for f in fc.filters.iter().filter(|f| f.table() == table) {
            match f.rows(t, self.cancel) {
                Ok(rows) => mask = and(mask, rows),
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
fn value_error(span: Span, e: &opendash_core::ValueError) -> DaxError {
    DaxError::new(span, e.to_string())
}

/// Bytes a value's text costs when materialised.
fn text_bytes(v: &Value) -> usize {
    match v {
        Value::Text(s) => s.len(),
        _ => 0,
    }
}
