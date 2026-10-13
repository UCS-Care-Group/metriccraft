//! The DAX function registry.
//!
//! **Adding a function** (M3.3–M3.5 and later) is a one-place change in this file:
//! 1. write `fn name(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val>` below;
//! 2. add its lowercase name to the `match` in [`lookup`].
//!
//! Arguments arrive **unevaluated** (`call.args`), so a function controls its own evaluation:
//! `IF` evaluates one branch, `CALCULATE` builds a new filter context first, an iterator pushes
//! row frames ([`Evaluator::with_row`]). The tools a function needs are on [`Evaluator`]:
//! [`Evaluator::scalar`] / [`Evaluator::table`] to evaluate an argument under a filter context
//! (`call.fc` is the caller's), [`Evaluator::mask`] for a Table's visible rows,
//! [`super::storage::scan`] for one pass over a Table's encoded columns, and
//! [`Evaluator::charge`] to count an intermediate table against the memory cap. Errors take the
//! span of the argument (or `call.span`) that failed.

use std::rc::Rc;

use opendash_core::Value;
use opendash_dax::{Expr, ExprKind, Span};

use super::eval::{Evaluator, Rows, RowsColumn, TableVal, Val};
use super::{DaxError, FilterContext, Result};
use crate::same_name;

/// A function call being evaluated.
pub struct Call<'e> {
    /// As written (any case).
    pub name: &'e str,
    pub args: &'e [Expr],
    pub span: Span,
    /// The filter context the call is evaluated in.
    pub fc: &'e FilterContext,
}

impl Call<'_> {
    /// Errors unless there are `min..=max` arguments.
    pub fn arity(&self, min: usize, max: usize) -> Result<()> {
        let n = self.args.len();
        if (min..=max).contains(&n) {
            return Ok(());
        }
        let want = match (min, max) {
            (a, b) if a == b => format!("{a}"),
            (a, usize::MAX) => format!("at least {a}"),
            (a, b) => format!("{a} to {b}"),
        };
        Err(DaxError::new(
            self.span,
            format!(
                "{} takes {want} arguments, not {n}",
                self.name.to_uppercase()
            ),
        ))
    }

    /// Argument `i`.
    pub fn arg(&self, i: usize) -> Result<&Expr> {
        self.args.get(i).ok_or_else(|| {
            DaxError::new(
                self.span,
                format!("{} needs argument {}", self.name.to_uppercase(), i + 1),
            )
        })
    }
}

/// A function's implementation.
pub type Function = fn(&mut Evaluator<'_, '_>, &Call<'_>) -> Result<Val>;

/// The function named `lower` (already lowercase: DAX names are case-insensitive).
pub fn lookup(lower: &str) -> Option<Function> {
    Some(match lower {
        "blank" => blank,
        "true" => true_,
        "false" => false_,
        "row" => row,
        "countrows" => countrows,
        _ => return None,
    })
}

fn blank(_: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(0, 0)?;
    Ok(Val::Scalar(Value::Blank))
}

fn true_(_: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(0, 0)?;
    Ok(Val::Scalar(Value::Boolean(true)))
}

fn false_(_: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(0, 0)?;
    Ok(Val::Scalar(Value::Boolean(false)))
}

/// `ROW("Name", expr, …)`: a one-row table.
fn row(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(2, usize::MAX)?;
    if !call.args.len().is_multiple_of(2) {
        return Err(DaxError::new(
            call.span,
            "ROW takes pairs of a column name and an expression",
        ));
    }
    let mut columns: Vec<RowsColumn> = Vec::with_capacity(call.args.len() / 2);
    let mut values = Vec::with_capacity(call.args.len() / 2);
    for pair in call.args.chunks(2) {
        let [name, expr] = pair else {
            continue;
        };
        let ExprKind::Text(n) = &name.kind else {
            return Err(DaxError::new(
                name.span,
                "a ROW column name must be text in quotes (\"Name\")",
            ));
        };
        if columns.iter().any(|c| same_name(&c.name, n)) {
            return Err(DaxError::new(
                name.span,
                format!("ROW has two columns named \"{n}\""),
            ));
        }
        columns.push(RowsColumn::new(n));
        values.push(ev.scalar(expr, call.fc)?);
    }
    Ok(Val::Table(Rc::new(TableVal::Rows(Rows {
        columns,
        rows: vec![values],
    }))))
}

/// `COUNTROWS(table)`: the rows of the table under the filter context; BLANK for none.
fn countrows(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let t = ev.table(call.arg(0)?, call.fc)?;
    let n = ev.row_count(&t)?;
    Ok(Val::Scalar(match i64::try_from(n) {
        Ok(0) => Value::Blank,
        Ok(n) => Value::Integer(n),
        Err(_) => return Err(DaxError::new(call.span, "too many rows to count")),
    }))
}
