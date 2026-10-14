//! The column aggregations (M3.3): `SUM`, `AVERAGE`, `COUNT`, `COUNTA`, `DISTINCTCOUNT`, `MIN`,
//! `MAX`. Each is one storage scan ([`super::storage`]) of the column's encoded data over the
//! rows its Table shows under the filter context; no column is materialised. A row context
//! doesn't filter them (CONTEXT.md, Row context): only the filter context does.
//!
//! BLANK, per Microsoft's documentation and parity cases 0001, 0029, 0030:
//! - `SUM`, `AVERAGE`, `MIN`, `MAX` skip blanks and give BLANK when no value is left; `0` is a
//!   value (`SUM` of zeros is `0`).
//! - `COUNT` counts non-blank numbers, dates and text; `COUNTA` non-blank values of any type;
//!   both give BLANK rather than `0`.
//! - `DISTINCTCOUNT` counts BLANK as one value; no rows at all gives BLANK.
//! - `MIN(a, b)` / `MAX(a, b)` treat a BLANK as zero, and give BLANK only when both are.
//!
//! Types: `SUM` of a whole-number column stays whole (overflow is an error), of a fixed decimal
//! stays fixed decimal, of a date column stays a date; `AVERAGE` is a decimal number (a date for
//! a date column). `MIN`/`MAX` keep the column's type and compare text ignoring case.
//!
//! Parity guesses (to settle with cases): `SUM`/`AVERAGE`/`COUNT`/`MIN`/`MAX` of a True/False
//! column, and `SUM`/`AVERAGE` of text, are errors here (the docs say `AVERAGE` of text gives
//! BLANK); `AVERAGE` of a fixed decimal column is a decimal number; `SUM` of dates is a date.

use opendash_core::{CURRENCY_SCALE, DataType, Value, ValueError};
use opendash_dax::{ExprKind, Span};

use crate::dax::eval::{Evaluator, Val};
use crate::dax::functions::Call;
use crate::dax::storage::{self, Bitset, Col, Kind, Number};
use crate::dax::{DaxError, Result};

/// A column argument resolved to the model: its Table, the column, and the rows visible.
struct Scan<'a> {
    col: &'a Col,
    rows: usize,
    mask: Option<Bitset>,
    /// `'Table'[Column]`, for messages.
    name: String,
}

/// Resolves argument 0 of `call`, which must be a column reference, and the rows of its Table
/// the filter context shows.
#[inline(never)]
fn column_arg<'a>(ev: &Evaluator<'a, '_>, call: &Call<'_>) -> Result<Scan<'a>> {
    let arg = call.arg(0)?;
    let fname = call.name.to_uppercase();
    let ExprKind::Column { table, column } = &arg.kind else {
        return Err(DaxError::new(
            arg.span,
            format!("{fname} needs a column, like 'Table'[Column]"),
        ));
    };
    let db = ev.db;
    let found = match table {
        Some(t) => db
            .table(t)
            .and_then(|ti| Some((ti, db.tables.get(ti)?.column(column)?))),
        None => {
            let mut hits = db
                .tables
                .iter()
                .enumerate()
                .filter_map(|(ti, t)| Some((ti, t.column(column)?)));
            let first = hits.next();
            if first.is_some() && hits.next().is_some() {
                return Err(DaxError::new(
                    arg.span,
                    format!("several Tables have a column [{column}]: name its Table"),
                ));
            }
            first
        }
    };
    let shown = match table {
        Some(t) => format!("'{t}'[{column}]"),
        None => format!("[{column}]"),
    };
    let Some((ti, ci)) = found else {
        return Err(DaxError::new(
            arg.span,
            format!("{fname} needs a column: there is no column {shown}"),
        ));
    };
    let (Some(t), Some(c)) = (
        db.tables.get(ti),
        db.tables.get(ti).and_then(|t| t.columns.get(ci)),
    ) else {
        return Err(DaxError::new(arg.span, "unknown column"));
    };
    let mask = ev.mask(ti, call.fc, arg.span)?;
    Ok(Scan {
        col: &c.data,
        rows: t.rows,
        mask,
        name: format!("'{}'[{}]", t.name, c.name),
    })
}

#[cold]
#[inline(never)]
fn wrong_kind(call: &Call<'_>, s: &Scan<'_>, wants: &str) -> DaxError {
    let span = call.args.first().map_or(call.span, |a| a.span);
    let kind = match s.col.kind() {
        Kind::Text => DataType::Text,
        Kind::Boolean => DataType::Boolean,
        _ => DataType::Blank,
    };
    DaxError::new(
        span,
        format!(
            "{} takes a column of {wants}; {} holds {kind} values",
            call.name.to_uppercase(),
            s.name
        ),
    )
}

#[cold]
#[inline(never)]
fn overflow(span: Span, t: DataType) -> DaxError {
    DaxError::new(span, ValueError::Overflow(t).to_string())
}

/// A count as a DAX value: BLANK for none.
fn count_value(n: usize, span: Span) -> Result<Val> {
    match i64::try_from(n) {
        Ok(0) => Ok(Val::Scalar(Value::Blank)),
        Ok(n) => Ok(Val::Scalar(Value::Integer(n))),
        Err(_) => Err(DaxError::new(span, "too many rows to count")),
    }
}

/// `SUM(column)`: the sum of the non-blank numbers or dates; BLANK when there are none.
pub fn sum(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let s = column_arg(ev, call)?;
    sum_scan(&s, call, ev.cancel).map(Val::Scalar)
}

#[inline(never)]
fn sum_scan(s: &Scan<'_>, call: &Call<'_>, cancel: storage::Cancel<'_>) -> Result<Value> {
    let kind = s.col.kind();
    if kind == Kind::Blank {
        return Ok(Value::Blank);
    }
    if !kind.is_numeric() {
        return Err(wrong_kind(call, s, "numbers or dates"));
    }
    let (mut int, mut float, mut any) = (0i64, 0f64, false);
    if matches!(kind, Kind::Decimal | Kind::DateTime) {
        // Floats only: a loop without the overflow checks (the same additions, in order).
        storage::each_number(s.col, s.rows, s.mask.as_ref(), cancel, |n| {
            if let Number::Float(f) = n {
                any = true;
                float += f;
            }
            Ok(())
        })?;
        return Ok(match (any, kind) {
            (false, _) => Value::Blank,
            (true, Kind::DateTime) => Value::DateTime(float),
            (true, _) => Value::Decimal(float),
        });
    }
    storage::each_number(s.col, s.rows, s.mask.as_ref(), cancel, |n| {
        any = true;
        match n {
            Number::Int(i) => {
                int = int
                    .checked_add(i)
                    .ok_or_else(|| overflow(call.span, DataType::Integer))?;
            }
            Number::Currency(c) => {
                int = int
                    .checked_add(c)
                    .ok_or_else(|| overflow(call.span, DataType::Currency))?;
            }
            Number::Float(f) => float += f,
        }
        Ok(())
    })?;
    Ok(match (any, kind) {
        (false, _) => Value::Blank,
        (true, Kind::Integer) => Value::Integer(int),
        (true, Kind::Currency) => Value::Currency(int),
        (true, Kind::DateTime) => Value::DateTime(float),
        (true, _) => Value::Decimal(float),
    })
}

/// `AVERAGE(column)`: the mean of the non-blank numbers or dates, a decimal number; BLANK when
/// there are none.
pub fn average(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let s = column_arg(ev, call)?;
    average_scan(&s, call, ev.cancel).map(Val::Scalar)
}

#[inline(never)]
fn average_scan(s: &Scan<'_>, call: &Call<'_>, cancel: storage::Cancel<'_>) -> Result<Value> {
    let kind = s.col.kind();
    if kind == Kind::Blank {
        return Ok(Value::Blank);
    }
    if !kind.is_numeric() {
        return Err(wrong_kind(call, s, "numbers or dates"));
    }
    // Whole numbers and Currency sum exactly (an i128 can't overflow on i64 values within any
    // row count a usize holds); floats sum as f64.
    let (mut exact, mut float, mut n) = (0i128, 0f64, 0u64);
    storage::each_number(s.col, s.rows, s.mask.as_ref(), cancel, |x| {
        n = n.saturating_add(1);
        match x {
            Number::Int(i) | Number::Currency(i) => exact = exact.saturating_add(i128::from(i)),
            Number::Float(f) => float += f,
        }
        Ok(())
    })?;
    if n == 0 {
        return Ok(Value::Blank);
    }
    let count = n as f64;
    let mean = match kind {
        Kind::Integer => exact as f64 / count,
        Kind::Currency => exact as f64 / CURRENCY_SCALE as f64 / count,
        _ => float / count,
    };
    Ok(if kind == Kind::DateTime {
        Value::DateTime(mean)
    } else {
        Value::Decimal(mean)
    })
}

/// `COUNT(column)`: the non-blank numbers, dates and text; BLANK for none. True/False isn't
/// supported (that's `COUNTA`).
pub fn count(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let s = column_arg(ev, call)?;
    if s.col.kind() == Kind::Boolean {
        return Err(wrong_kind(
            call,
            &s,
            "numbers, dates or text (COUNTA counts True/False)",
        ));
    }
    let n = storage::count_values(s.col, s.rows, s.mask.as_ref(), ev.cancel)?;
    count_value(n, call.span)
}

/// `COUNTA(column)`: the non-blank values of any type; BLANK for none.
pub fn counta(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let s = column_arg(ev, call)?;
    let n = storage::count_values(s.col, s.rows, s.mask.as_ref(), ev.cancel)?;
    count_value(n, call.span)
}

/// `DISTINCTCOUNT(column)`: the distinct values, BLANK counting as one; text ignores case.
/// BLANK when no row is visible.
pub fn distinctcount(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(1, 1)?;
    let s = column_arg(ev, call)?;
    let n = storage::distinct_count(s.col, s.rows, s.mask.as_ref(), ev.cancel)?;
    count_value(n, call.span)
}

/// `MIN(column)` or `MIN(a, b)`.
pub fn min(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    extreme(ev, call, false)
}

/// `MAX(column)` or `MAX(a, b)`.
pub fn max(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    extreme(ev, call, true)
}

fn extreme(ev: &mut Evaluator<'_, '_>, call: &Call<'_>, max: bool) -> Result<Val> {
    call.arity(1, 2)?;
    if call.args.len() == 2 {
        let a = ev.scalar(call.arg(0)?, call.fc)?;
        let b = ev.scalar(call.arg(1)?, call.fc)?;
        return pick(a, b, call, max).map(Val::Scalar);
    }
    let s = column_arg(ev, call)?;
    extreme_scan(&s, call, ev.cancel, max).map(Val::Scalar)
}

#[inline(never)]
fn extreme_scan(
    s: &Scan<'_>,
    call: &Call<'_>,
    cancel: storage::Cancel<'_>,
    max: bool,
) -> Result<Value> {
    let kind = s.col.kind();
    match kind {
        Kind::Blank => return Ok(Value::Blank),
        Kind::Boolean => {
            return Err(wrong_kind(
                call,
                s,
                "numbers, dates or text (MINA/MAXA take True/False)",
            ));
        }
        Kind::Text => {
            return storage::text_extreme(s.col, s.rows, s.mask.as_ref(), cancel, max)
                .map(|v| v.unwrap_or(Value::Blank));
        }
        _ => {}
    }
    let mut best: Option<Number> = None;
    storage::each_number(s.col, s.rows, s.mask.as_ref(), cancel, |x| {
        let better = match (best, x) {
            (None, Number::Float(f)) => !f.is_nan(),
            (None, _) => true,
            (Some(Number::Int(b)), Number::Int(v))
            | (Some(Number::Currency(b)), Number::Currency(v)) => {
                if max {
                    v > b
                } else {
                    v < b
                }
            }
            (Some(Number::Float(b)), Number::Float(v)) => {
                if max {
                    v > b
                } else {
                    v < b
                }
            }
            // One column has one kind of number.
            (Some(_), _) => false,
        };
        if better {
            best = Some(x);
        }
        Ok(())
    })?;
    Ok(match best {
        None => Value::Blank,
        Some(Number::Int(i)) => Value::Integer(i),
        Some(Number::Currency(c)) => Value::Currency(c),
        Some(Number::Float(f)) if kind == Kind::DateTime => Value::DateTime(f),
        Some(Number::Float(f)) => Value::Decimal(f),
    })
}

/// The smaller (or larger) of two values; a BLANK is the other side's zero, both BLANK give
/// BLANK, a tie gives the first.
#[inline(never)]
fn pick(a: Value, b: Value, call: &Call<'_>, max: bool) -> Result<Value> {
    if let Some(bad) = [&a, &b]
        .into_iter()
        .position(|v| matches!(v, Value::Boolean(_)))
    {
        let span = call.args.get(bad).map_or(call.span, |e| e.span);
        return Err(DaxError::new(
            span,
            format!(
                "{} doesn't take True/False values (MINA/MAXA do)",
                call.name.to_uppercase()
            ),
        ));
    }
    let o = a
        .compare(&b)
        .map_err(|e| DaxError::new(call.span, e.to_string()))?;
    let take_b = o.is_some_and(|o| if max { o.is_lt() } else { o.is_gt() });
    let (win, other) = if take_b { (b, a) } else { (a, b) };
    Ok(if win.is_blank() {
        zero_like(&other)
    } else {
        win
    })
}

/// What a BLANK compares as against `v`: its type's zero (BLANK against BLANK).
fn zero_like(v: &Value) -> Value {
    match v {
        Value::Blank => Value::Blank,
        Value::Boolean(_) => Value::Boolean(false),
        Value::Integer(_) => Value::Integer(0),
        Value::Decimal(_) => Value::Decimal(0.0),
        Value::Currency(_) => Value::Currency(0),
        Value::Text(_) => Value::text(""),
        Value::DateTime(_) => Value::DateTime(0.0),
    }
}

#[cfg(test)]
#[path = "tests_aggregate.rs"]
mod tests;
