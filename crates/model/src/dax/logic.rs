//! Logical and error functions (M3.5): `IF`, `SWITCH`, `DIVIDE`, `ISBLANK`, `ISERROR`,
//! `IFERROR`, `CONTAINSROW`, and the `IN` operator ([`in_op`], called from the evaluator).
//!
//! - **Lazy branches.** `IF`, `SWITCH` and `IFERROR` evaluate only the branch they take, so an
//!   error in an untaken branch never surfaces (case 0033).
//! - **Conditions** convert as `NOT` does: BLANK is FALSE, a number is TRUE unless 0.
//! - **`SWITCH`** compares strictly (`==`: BLANK doesn't match 0, case 0033), each value
//!   evaluated in order until one matches.
//! - **`DIVIDE(n, d, alt)`** is BLANK when `n` is BLANK (even for `d` 0, case 0034); else `alt`
//!   (default BLANK) when `d` is 0 or BLANK; otherwise `n / d`.
//! - **`ISERROR` / `IFERROR`** catch DAX errors ([`ErrorKind::Eval`]: not limits, the memory cap
//!   or cancelling) and treat an infinite or NaN result as an error (case 0002).
//! - **`IN` / `CONTAINSROW`** compare strictly (`==`: BLANK matches only BLANK, per Microsoft's
//!   CONTAINSROW docs; text still ignores case). The left side is a value or a row tuple
//!   (`(1, "a")`, parsed as a one-row table), and needs as many values as the table has
//!   columns.
//!   As a `CALCULATE` filter argument (`CALCULATE(…, T[C] IN {"a", "b"})`, case 0035
//!   `InFilter`), `IN` is CALCULATE's to turn into a column filter (`calculate::in_filter`):
//!   it evaluates the right side once and keeps the values of `T[C]` that [`contains`] finds.
//!
//! **Stack.** Every argument is a recursive path (eval → call → function → eval), and nesting
//! to [`super::super::MAX_EVAL_DEPTH`] must fit a 1 MiB stack in debug builds, where every
//! temporary has its own stack slot. So each function is a small state machine ([`Step`]) run
//! by [`drive`], whose frame is the only one on the path: it holds one result, calls
//! [`Evaluator::eval`] at one place, and the step (arity, conversions, comparisons, errors)
//! runs between evaluations, off the path.

use std::rc::Rc;

use metriccraft_core::{Value, ValueError};
use metriccraft_dax::{Expr, ExprKind, Span};

use super::{Call, Function};
use crate::dax::eval::{Evaluator, TableVal, Val};
use crate::dax::storage;
use crate::dax::{DaxError, ErrorKind, FilterContext, Result};

pub const IF: Function = drive::<If>;
pub const SWITCH: Function = drive::<Switch>;
pub const DIVIDE: Function = drive::<Divide>;
pub const ISBLANK: Function = drive::<IsBlank>;
pub const ISERROR: Function = drive::<IsError>;
pub const IFERROR: Function = drive::<IfError>;
pub const CONTAINSROW: Function = drive::<ContainsRow>;

/// What a [`Step`] wants next.
pub enum Next<'e> {
    /// Evaluate this expression (in the call's filter context) and step again with the result.
    Eval(&'e Expr),
    /// The function's value.
    Done(Val),
}

/// A function as a state machine: given the last evaluation's result (`None` at the start),
/// what to evaluate next, or the answer.
pub trait Step: Default {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>>;
}

/// Runs a [`Step`] function. No `?` here: each one adds temporaries to this frame.
pub fn drive<S: Step>(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    let mut st = S::default();
    let mut r = None;
    loop {
        match st.step(ev, call, r.take()) {
            Ok(Next::Eval(e)) => r = Some(ev.eval(e, call.fc)),
            Ok(Next::Done(v)) => return Ok(v),
            Err(e) => return Err(e),
        }
    }
}

#[cold]
#[inline(never)]
fn value_error(span: Span, e: &ValueError) -> DaxError {
    DaxError::new(span, e.to_string())
}

/// Argument `i` after checking there are `min..=max` arguments.
fn checked<'e>(call: &Call<'e>, min: usize, max: usize, i: usize) -> Result<&'e Expr> {
    call.arity(min, max)?;
    let args: &'e [Expr] = call.args;
    args.get(i)
        .ok_or_else(|| DaxError::new(call.span, "an argument is missing"))
}

/// The span of argument `i` (the call's if it is missing).
fn arg_span(call: &Call<'_>, i: usize) -> Span {
    call.args.get(i).map_or(call.span, |e| e.span)
}

/// An evaluated argument as a single value, as [`Evaluator::scalar`] converts it.
fn value_of(ev: &mut Evaluator<'_, '_>, r: Result<Val>, span: Span) -> Result<Value> {
    match r? {
        Val::Scalar(v) => Ok(v),
        Val::Table(t) => table_value(ev, &t, span),
    }
}

/// A one-column table of at most one row as its value (empty: BLANK).
#[cold]
#[inline(never)]
fn table_value(ev: &mut Evaluator<'_, '_>, t: &TableVal, span: Span) -> Result<Value> {
    let width = width(ev, t);
    if width != 1 {
        return Err(DaxError::new(
            span,
            format!("a table of {width} columns can't be used as a single value"),
        ));
    }
    let rows = ev.materialise(t, span)?;
    match rows.rows.as_slice() {
        [] => Ok(Value::Blank),
        [row] => Ok(row.first().cloned().unwrap_or(Value::Blank)),
        _ => Err(DaxError::new(
            span,
            "a table of several values was given where a single value is needed",
        )),
    }
}

/// An evaluated argument that must be a table.
fn table_of(r: Result<Val>, span: Span) -> Result<Rc<TableVal>> {
    match r? {
        Val::Table(t) => Ok(t),
        Val::Scalar(_) => Err(DaxError::new(
            span,
            "a table is needed here, not a single value",
        )),
    }
}

fn width(ev: &Evaluator<'_, '_>, t: &TableVal) -> usize {
    match t {
        TableVal::Base { table, .. } => ev.db.tables.get(*table).map_or(0, |t| t.columns.len()),
        TableVal::Rows(r) => r.columns.len(),
    }
}

/// `IF(condition, then, [else])`: the untaken branch isn't evaluated; no else is BLANK.
#[derive(Default)]
pub struct If {
    branch: bool,
}

impl Step for If {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let Some(r) = r else {
            return checked(call, 2, 3, 0).map(Next::Eval);
        };
        if self.branch {
            return r.map(Next::Done);
        }
        let span = arg_span(call, 0);
        let take = value_of(ev, r, span)?
            .to_bool()
            .map_err(|e| value_error(span, &e))?;
        let args: &'e [Expr] = call.args;
        Ok(match args.get(if take { 1 } else { 2 }) {
            Some(e) => {
                self.branch = true;
                Next::Eval(e)
            }
            None => Next::Done(Val::Scalar(Value::Blank)),
        })
    }
}

/// `SWITCH(expr, value1, result1, …, [else])`: the result of the first value `==` to `expr`;
/// values after the match and the results not taken aren't evaluated.
#[derive(Default)]
pub struct Switch {
    subject: Option<Value>,
    /// The argument index of the value being compared.
    i: usize,
    /// The result (or else) is being evaluated.
    taken: bool,
}

impl Step for Switch {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let args: &'e [Expr] = call.args;
        let Some(r) = r else {
            return checked(call, 3, usize::MAX, 0).map(Next::Eval);
        };
        if self.taken {
            return r.map(Next::Done);
        }
        match &self.subject {
            None => {
                self.subject = Some(value_of(ev, r, arg_span(call, 0))?);
                self.i = 1;
            }
            Some(subject) => {
                let span = arg_span(call, self.i);
                let v = value_of(ev, r, span)?;
                if subject
                    .strict_equals(&v)
                    .map_err(|e| value_error(span, &e))?
                {
                    self.taken = true;
                    return args
                        .get(self.i.saturating_add(1))
                        .map(Next::Eval)
                        .ok_or_else(|| DaxError::new(call.span, "SWITCH is missing a result"));
                }
                self.i = self.i.saturating_add(2);
            }
        }
        Ok(
            match (args.get(self.i), args.get(self.i.saturating_add(1))) {
                (Some(value), Some(_)) => Next::Eval(value),
                (Some(otherwise), None) => {
                    self.taken = true;
                    Next::Eval(otherwise)
                }
                _ => Next::Done(Val::Scalar(Value::Blank)),
            },
        )
    }
}

/// `DIVIDE(numerator, denominator, [alternate])`: `alternate` (default BLANK) when the
/// denominator is 0 or BLANK.
#[derive(Default)]
pub struct Divide {
    numerator: Option<Value>,
    /// The alternate is being evaluated.
    alternate: bool,
}

impl Step for Divide {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let Some(r) = r else {
            return checked(call, 2, 3, 0).map(Next::Eval);
        };
        if self.alternate {
            return r.map(Next::Done);
        }
        let Some(n) = &self.numerator else {
            self.numerator = Some(value_of(ev, r, arg_span(call, 0))?);
            return checked(call, 2, 3, 1).map(Next::Eval);
        };
        let span = arg_span(call, 1);
        let d = value_of(ev, r, span)?;
        if n.is_blank() {
            return Ok(Next::Done(Val::Scalar(Value::Blank)));
        }
        if is_zero(&d).map_err(|e| value_error(span, &e))? {
            let args: &'e [Expr] = call.args;
            return Ok(match args.get(2) {
                Some(alt) => {
                    self.alternate = true;
                    Next::Eval(alt)
                }
                None => Next::Done(Val::Scalar(Value::Blank)),
            });
        }
        n.divide(&d)
            .map(|v| Next::Done(Val::Scalar(v)))
            .map_err(|e| value_error(call.span, &e))
    }
}

/// Whether `v` is BLANK or zero as a number (text is read as a number, as `/` reads it).
fn is_zero(v: &Value) -> std::result::Result<bool, ValueError> {
    if v.is_blank() {
        return Ok(true);
    }
    // `+ 0` converts to a number the way arithmetic does (text, TRUE/FALSE).
    Ok(match v.add(&Value::Integer(0))? {
        Value::Integer(i) | Value::Currency(i) => i == 0,
        Value::Decimal(d) | Value::DateTime(d) => d == 0.0,
        _ => false,
    })
}

/// `ISBLANK(value)`.
#[derive(Default)]
pub struct IsBlank;

impl Step for IsBlank {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let Some(r) = r else {
            return checked(call, 1, 1, 0).map(Next::Eval);
        };
        let v = value_of(ev, r, arg_span(call, 0))?;
        Ok(Next::Done(Val::Scalar(Value::Boolean(v.is_blank()))))
    }
}

/// `ISERROR(value)`: TRUE for a DAX error or an infinite/NaN result.
#[derive(Default)]
pub struct IsError;

impl Step for IsError {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let Some(r) = r else {
            return checked(call, 1, 1, 0).map(Next::Eval);
        };
        let ok = caught(ev, r, arg_span(call, 0))?;
        Ok(Next::Done(Val::Scalar(Value::Boolean(ok.is_none()))))
    }
}

/// `IFERROR(value, value_if_error)`: `value_if_error` is evaluated only on an error.
#[derive(Default)]
pub struct IfError {
    /// `value_if_error` is being evaluated.
    fallback: bool,
}

impl Step for IfError {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let Some(r) = r else {
            return checked(call, 2, 2, 0).map(Next::Eval);
        };
        if self.fallback {
            return r.map(Next::Done);
        }
        match caught(ev, r, arg_span(call, 0))? {
            Some(v) => Ok(Next::Done(Val::Scalar(v))),
            None => {
                self.fallback = true;
                checked(call, 2, 2, 1).map(Next::Eval)
            }
        }
    }
}

/// An evaluated argument's value, or `None` if it is a DAX error or not finite. Limits, the
/// memory cap and cancelling are not caught.
fn caught(ev: &mut Evaluator<'_, '_>, r: Result<Val>, span: Span) -> Result<Option<Value>> {
    match value_of(ev, r, span) {
        Ok(Value::Decimal(d)) if !d.is_finite() => Ok(None),
        Ok(v) => Ok(Some(v)),
        Err(err) if err.kind == ErrorKind::Eval => Ok(None),
        Err(err) => Err(err),
    }
}

/// `CONTAINSROW(table, value, …)`: `(value, …) IN table`.
#[derive(Default)]
pub struct ContainsRow {
    table: Option<Rc<TableVal>>,
    needle: Vec<Value>,
}

impl Step for ContainsRow {
    fn step<'e>(
        &mut self,
        ev: &mut Evaluator<'_, '_>,
        call: &Call<'e>,
        r: Option<Result<Val>>,
    ) -> Result<Next<'e>> {
        let args: &'e [Expr] = call.args;
        match (r, &self.table) {
            (None, _) => return checked(call, 2, usize::MAX, 0).map(Next::Eval),
            (Some(r), None) => self.table = Some(table_of(r, arg_span(call, 0))?),
            (Some(r), Some(_)) => {
                let span = arg_span(call, self.needle.len().saturating_add(1));
                let v = value_of(ev, r, span)?;
                self.needle.push(v);
            }
        }
        if let Some(next) = args.get(self.needle.len().saturating_add(1)) {
            return Ok(Next::Eval(next));
        }
        let Some(t) = &self.table else {
            return Err(DaxError::new(call.span, "CONTAINSROW needs a table"));
        };
        contains(ev, t, &self.needle, call.span).map(|b| Next::Done(Val::Scalar(Value::Boolean(b))))
    }
}

/// `l IN r`: whether table `r` has a row strictly equal to `l` (a value or a row tuple). The
/// evaluator's arm for [`metriccraft_dax::BinOp::In`] (`e`); a [`drive`] loop of its own.
#[inline(never)]
pub fn in_op(ev: &mut Evaluator<'_, '_>, e: &Expr, fc: &FilterContext) -> Result<Val> {
    let (ExprKind::Binary(_, l, r), span) = (&e.kind, e.span) else {
        return Err(DaxError::new(e.span, "IN needs two sides"));
    };
    let mut needle = None;
    let mut last = None;
    loop {
        match in_step(ev, [l, r], span, &mut needle, last.take()) {
            Ok(Next::Eval(e)) => last = Some(ev.eval(e, fc)),
            Ok(Next::Done(v)) => return Ok(v),
            Err(e) => return Err(e),
        }
    }
}

/// The side `IN` evaluates next (left, then right), or its answer.
#[inline(never)]
fn in_step<'e>(
    ev: &mut Evaluator<'_, '_>,
    [l, r]: [&'e Expr; 2],
    span: Span,
    needle: &mut Option<Vec<Value>>,
    last: Option<Result<Val>>,
) -> Result<Next<'e>> {
    let Some(last) = last else {
        return Ok(Next::Eval(l));
    };
    let Some(values) = needle else {
        *needle = Some(match last? {
            Val::Scalar(v) => vec![v],
            Val::Table(t) => one_row(ev, &t, l.span)?,
        });
        return Ok(Next::Eval(r));
    };
    let t = table_of(last, r.span)?;
    contains(ev, &t, values, span).map(|b| Next::Done(Val::Scalar(Value::Boolean(b))))
}

/// The one row of a tuple (or a one-column table of at most one row: empty is BLANK).
fn one_row(ev: &mut Evaluator<'_, '_>, t: &TableVal, span: Span) -> Result<Vec<Value>> {
    let rows = ev.materialise(t, span)?;
    match rows.rows.as_slice() {
        [row] => Ok(row.clone()),
        [] if rows.columns.len() == 1 => Ok(vec![Value::Blank]),
        _ => Err(DaxError::new(
            span,
            "the left side of IN must be a single value or one row of values",
        )),
    }
}

/// Whether `t` has a row whose values are each strictly equal (`==`) to `needle`'s.
pub(crate) fn contains(
    ev: &Evaluator<'_, '_>,
    t: &TableVal,
    needle: &[Value],
    span: Span,
) -> Result<bool> {
    let width = width(ev, t);
    if width != needle.len() {
        return Err(DaxError::new(
            span,
            format!(
                "IN compares {} value(s) with a table of {width} column(s): they must match",
                needle.len()
            ),
        ));
    }
    let same = |a: &Value, b: &Value| a.strict_equals(b).map_err(|e| value_error(span, &e));
    match t {
        TableVal::Rows(r) => {
            for (i, row) in r.rows.iter().enumerate() {
                if i % storage::CHUNK == 0 {
                    ev.cancel.check()?;
                }
                if row_matches(row.iter(), needle, same)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        TableVal::Base { table, rows } => {
            let Some(dt) = ev.db.tables.get(*table) else {
                return Ok(false);
            };
            let mut found = false;
            storage::scan(dt.rows, rows.as_ref(), ev.cancel, |r| {
                if !found {
                    let values = dt.columns.iter().map(|c| c.data.value(r));
                    found = row_matches(values, needle, same)?;
                }
                Ok(())
            })?;
            Ok(found)
        }
    }
}

fn row_matches<V: std::borrow::Borrow<Value>>(
    row: impl Iterator<Item = V>,
    needle: &[Value],
    same: impl Fn(&Value, &Value) -> Result<bool>,
) -> Result<bool> {
    for (a, b) in row.zip(needle) {
        if !same(a.borrow(), b)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
    use metriccraft_data::{Table, TableBuilder};

    use crate::Model;
    use crate::dax::{Database, MAX_EVAL_DEPTH, Options, ResultTable, run_query_text};

    use super::*;

    fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
        let batch = RecordBatch::try_from_iter(columns).unwrap();
        let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        b.finish().unwrap()
    }

    /// Sales(Region, Qty) and a one-column Regions(Name).
    fn db() -> Database {
        let sales = table(
            "Sales",
            vec![
                (
                    "Region",
                    Arc::new(StringArray::from(vec![Some("North"), None, Some("West")])),
                ),
                (
                    "Qty",
                    Arc::new(Int64Array::from(vec![Some(2), Some(3), None])),
                ),
            ],
        );
        let regions = table(
            "Regions",
            vec![(
                "Name",
                Arc::new(StringArray::from(vec![Some("North"), Some("South")])),
            )],
        );
        Database::new(&Model::default(), &[sales, regions])
    }

    fn run(db: &Database, q: &str) -> Result<Vec<ResultTable>> {
        run_query_text(
            db,
            q,
            Options {
                cancel: None,
                memory_cap: usize::MAX,
            },
        )
    }

    fn one(db: &Database, expr: &str) -> String {
        let q = format!("EVALUATE ROW(\"x\", {expr})");
        let r = run(db, &q).unwrap_or_else(|e| panic!("{q}: {e}"));
        r[0].rows[0][0].copy_text()
    }

    fn error(db: &Database, expr: &str) -> DaxError {
        run(db, &format!("EVALUATE ROW(\"x\", {expr})")).expect_err(expr)
    }

    fn check(cases: &[(&str, &str)]) {
        let db = db();
        for (expr, want) in cases {
            assert_eq!(one(&db, expr), *want, "{expr}");
        }
    }

    #[test]
    fn if_and_switch_are_lazy_and_blank_aware() {
        // Case 0033.
        check(&[
            ("IF(1 < 2, \"yes\", \"no\")", "yes"),
            ("ISBLANK(IF(1 > 2, \"yes\"))", "true"),
            ("IF(BLANK(), \"yes\", \"no\")", "no"),
            ("IF(0, \"yes\", \"no\")", "no"),
            ("IF(TRUE(), 1, \"a\" + 1)", "1"),
            ("IF(FALSE(), \"a\" + 1, 2)", "2"),
            ("SWITCH(2, 1, \"one\", 2, \"two\", \"other\")", "two"),
            ("SWITCH(9, 1, \"one\", 2, \"two\", \"other\")", "other"),
            ("ISBLANK(SWITCH(9, 1, \"one\"))", "true"),
            ("SWITCH(TRUE(), 1 > 2, \"a\", 2 > 1, \"b\", \"c\")", "b"),
            ("SWITCH(BLANK(), 0, \"zero\", \"other\")", "other"),
            ("SWITCH(BLANK(), BLANK(), \"blank\", \"other\")", "blank"),
            ("SWITCH(1, 1.0, \"one\", \"other\")", "one"),
            ("SWITCH(1, 1, 10, \"a\" + 1)", "10"),
            ("SWITCH(1, 1, 10, 2, \"a\" + 1)", "10"),
            ("SWITCH(\"A\", \"a\", 1, 2)", "1"),
        ]);
        let db = db();
        assert!(error(&db, "IF(\"maybe\", 1, 2)").message.contains("maybe"));
        assert!(error(&db, "IF(1)").message.contains("2 to 3"));
        assert!(error(&db, "SWITCH(1, 2)").message.contains("at least 3"));
        // The error is the taken branch's.
        let q = "EVALUATE ROW(\"x\", IF(TRUE(), \"a\" * 2, 1))";
        let e = run(&db, q).expect_err(q);
        assert_eq!(&q[e.span.start..e.span.end], "\"a\" * 2");
    }

    #[test]
    fn divide_gives_the_alternate_for_zero_or_blank() {
        // Cases 0001 and 0034.
        check(&[
            ("DIVIDE(7, 2)", "3.5"),
            ("DIVIDE(6, 3)", "2"),
            ("ISBLANK(DIVIDE(1, 0))", "true"),
            ("ISBLANK(DIVIDE(0, 0))", "true"),
            ("ISBLANK(DIVIDE(1, BLANK()))", "true"),
            ("ISBLANK(DIVIDE(BLANK(), 2))", "true"),
            ("ISBLANK(DIVIDE(BLANK(), 0, -1))", "true"),
            ("DIVIDE(1, 0, \"none\")", "none"),
            ("DIVIDE(-1, 4)", "-0.25"),
            ("DIVIDE(1, 0, -1)", "-1"),
            ("DIVIDE(1, 0.0, -1)", "-1"),
            ("DIVIDE(1, 2, \"a\" + 1)", "0.5"),
        ]);
        assert!(error(&db(), "DIVIDE(1, \"x\")").message.contains("'x'"));
    }

    #[test]
    fn iserror_and_iferror_catch_errors_and_infinity() {
        // Cases 0002 and 0036.
        check(&[
            ("ISBLANK(1 / 0)", "false"),
            ("ISBLANK(0 / 0)", "false"),
            ("ISERROR(1 / 0)", "true"),
            ("ISERROR(-1 / 0)", "true"),
            ("ISERROR(0 / 0)", "true"),
            ("ISERROR(\"a\" + 1)", "true"),
            ("ISERROR(1 + 1)", "false"),
            ("ISERROR(BLANK())", "false"),
            ("ISERROR(NOSUCH(1))", "true"),
            ("IFERROR(\"a\" + 1, -1)", "-1"),
            ("IFERROR(1 / 0, -1)", "-1"),
            ("IFERROR(2 * 3, -1)", "6"),
            ("IFERROR(2 * 3, \"a\" + 1)", "6"),
            ("ISBLANK(BLANK())", "true"),
            ("ISBLANK(\"\")", "false"),
            ("ISBLANK(0)", "false"),
        ]);
    }

    /// Measures `m1..m9`, each `wrap` around the previous one: chained, far past the depth cap.
    fn chain(wrap: impl Fn(&str) -> String) -> String {
        let mut q = String::from("DEFINE MEASURE Sales[m0] = 1\n");
        for i in 1..10 {
            q.push_str(&format!(
                "MEASURE Sales[m{i}] = {}\n",
                wrap(&format!("[m{}]", i - 1))
            ));
        }
        q
    }

    #[test]
    fn the_parity_case_queries_evaluate() {
        // 0035 needs its dataset and CALCULATE/SUM: its IN Checks are mirrored above.
        let db = db();
        for md in [
            include_str!("../../../../parity/observed/0033-if-switch.md"),
            include_str!("../../../../parity/observed/0034-divide.md"),
            include_str!("../../../../parity/observed/0036-iserror-iferror.md"),
        ] {
            let q = md
                .split("```dax\n")
                .nth(1)
                .and_then(|s| s.split("\n```").next())
                .unwrap();
            let r = run(&db, q).unwrap_or_else(|e| panic!("{q}: {e}"));
            assert_eq!(r[0].rows.len(), 1, "{q}");
        }
    }

    #[test]
    fn iserror_does_not_catch_limits() {
        let q = chain(|inner| format!("{}{inner}", "- ".repeat(190)));
        let q = format!("{q}EVALUATE ROW(\"x\", ISERROR([m9]))");
        let e = run(&db(), &q).expect_err("too deep");
        assert_eq!(e.kind, ErrorKind::Limit, "{e}");
    }

    #[test]
    fn in_matches_values_and_tuples_strictly() {
        // Case 0035 (BLANK IN {0} follows the CONTAINSROW docs: strict).
        check(&[
            ("2 IN {1, 2, 3}", "true"),
            ("NOT (5 IN {1, 2, 3})", "true"),
            ("\"north\" IN {\"North\", \"South\"}", "true"),
            ("BLANK() IN {0}", "false"),
            ("BLANK() IN {0, BLANK()}", "true"),
            ("0 IN {BLANK()}", "false"),
            ("(1, \"a\") IN {(1, \"a\"), (2, \"b\")}", "true"),
            ("(1, \"b\") IN {(1, \"a\"), (2, \"b\")}", "false"),
            ("2 IN {}", "false"),
            ("2.0 IN {1, 2}", "true"),
            ("\"south\" IN Regions", "true"),
            ("\"West\" IN Regions", "false"),
            ("(\"West\", BLANK()) IN Sales", "true"),
            ("(BLANK(), 3) IN Sales", "true"),
            ("(\"north\", 2) IN Sales", "true"),
            ("(\"north\", 3) IN Sales", "false"),
            ("CONTAINSROW({(1, \"a\")}, 1, \"A\")", "true"),
            ("CONTAINSROW(Regions, \"Nope\")", "false"),
        ]);
        let db = db();
        assert!(
            error(&db, "(1, 2) IN {1, 2}")
                .message
                .contains("must match")
        );
        assert!(error(&db, "1 IN Sales").message.contains("must match"));
        assert!(error(&db, "1 IN {\"a\"}").message.contains("compared"));
        assert!(error(&db, "1 IN 1").message.contains("table is needed"));
    }

    #[test]
    fn nested_branches_hit_the_depth_cap_not_the_stack() {
        // Every argument these functions evaluate is a recursive path: nested through each,
        // chained past MAX_EVAL_DEPTH on a 1 MiB stack, the cap is hit first.
        let paths = [
            ("IF(", ", 1, 0)"),
            ("IF(TRUE(), ", ", 0)"),
            ("IF(FALSE(), 0, ", ")"),
            ("SWITCH(", ", 1, 1, 0)"),
            ("SWITCH(1, ", ", 1, 0)"),
            ("SWITCH(1, 1, ", ", 0)"),
            ("SWITCH(1, 2, 0, ", ")"),
            ("IFERROR(", ", 0)"),
            ("IFERROR(1 / 0, ", ")"),
            ("ISERROR(", ")"),
            ("ISBLANK(", ")"),
            ("DIVIDE(", ", 1)"),
            ("DIVIDE(1, ", ")"),
            ("DIVIDE(1, 0, ", ")"),
            ("CONTAINSROW({1}, ", ")"),
            ("(", " IN {1})"),
        ];
        for (open, close) in paths {
            let worker = std::thread::Builder::new()
                .stack_size(1 << 20)
                .spawn(move || {
                    let db = db();
                    let q =
                        chain(|inner| format!("{}{inner}{}", open.repeat(60), close.repeat(60)));
                    let e = run(&db, &format!("{q}EVALUATE ROW(\"x\", [m9])")).expect_err(open);
                    assert_eq!(e.kind, ErrorKind::Limit, "{open}: {e}");
                    assert!(e.message.contains(&MAX_EVAL_DEPTH.to_string()), "{e}");
                    // One Measure that deep is within the cap (it may still be a DAX error).
                    let r = run(&db, &format!("{q}EVALUATE ROW(\"x\", [m1])"));
                    assert!(
                        r.as_ref()
                            .map_or_else(|e| e.kind == ErrorKind::Eval, |_| true),
                        "{open}: {r:?}"
                    );
                })
                .unwrap();
            worker.join().unwrap();
        }
    }
}
