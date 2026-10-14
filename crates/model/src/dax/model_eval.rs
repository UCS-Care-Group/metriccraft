//! The Model's own DAX through the evaluator (M3.6): its Measures in every query, a Measure by
//! name (what visuals evaluate), and a Calculated column's expression once per row.
//!
//! Built on the evaluator's public API only: the Model's Measures are handed to it as `DEFINE
//! MEASURE`s, so a query names them without defining them. A query's own `DEFINE MEASURE`
//! overrides a Model Measure of the same name, as in Power BI. Measures whose DAX doesn't parse
//! are left out (a query naming one fails as for an unknown name); their parse error is shown
//! where the Measure is ([`crate::Measure::parse`]).

use opendash_core::Value;
use opendash_dax::{Define, Expr, ExprKind, Query, Span};

use super::eval::{Evaluator, RowFrame};
use super::storage::CHUNK;
use super::{Database, DaxError, ErrorKind, FilterContext, Options, Result, ResultTable};
use crate::{Model, same_name};

/// The Model's Measures as `DEFINE MEASURE`s: those that parse, whose home Table is in `db`,
/// whose name isn't taken by an earlier one or a Column of the home Table, and that `skip`
/// doesn't name.
pub fn measure_defines(model: &Model, db: &Database, skip: impl Fn(&str) -> bool) -> Vec<Define> {
    let mut out: Vec<Define> = Vec::new();
    for t in &model.tables {
        let Some(home) = db.table(&t.name).and_then(|i| db.tables.get(i)) else {
            continue;
        };
        for m in &t.measures {
            let taken = out.iter().any(|d| match d {
                Define::Measure { name, .. } => same_name(name, &m.name),
                Define::Var { .. } => false,
            });
            if taken || skip(&m.name) || home.column(&m.name).is_some() {
                continue;
            }
            if let Ok(expr) = m.parse() {
                out.push(Define::Measure {
                    table: t.name.clone(),
                    name: m.name.clone(),
                    expr,
                    span: Span::default(),
                });
            }
        }
    }
    out
}

/// Runs `query` with the Model's Measures in scope (see the module doc).
pub fn run_model_query(
    db: &Database,
    model: &Model,
    query: &Query,
    options: Options<'_>,
) -> Result<Vec<ResultTable>> {
    let own = |name: &str| {
        query.defines.iter().any(|d| match d {
            Define::Measure { name: n, .. } => same_name(n, name),
            Define::Var { .. } => false,
        })
    };
    let mut defines = measure_defines(model, db, own);
    defines.extend(query.defines.iter().cloned());
    let full = Query {
        defines,
        evaluates: query.evaluates.clone(),
    };
    Evaluator::new(db, options).run(&full)
}

/// Parses and runs DAX query text with the Model's Measures in scope.
pub fn run_model_query_text(
    db: &Database,
    model: &Model,
    text: &str,
    options: Options<'_>,
) -> Result<Vec<ResultTable>> {
    let query = opendash_dax::parse_query(text).map_err(syntax)?;
    run_model_query(db, model, &query, options)
}

/// Evaluates Measure `name` of Table `table` under `fc` (empty: the whole Model), as a visual
/// does for one cell.
pub fn evaluate_measure(
    db: &Database,
    model: &Model,
    table: &str,
    name: &str,
    fc: &FilterContext,
    options: Options<'_>,
) -> Result<Value> {
    let Some(m) = model.table(table).and_then(|t| t.measure(name)) else {
        return Err(DaxError::new(
            Span::default(),
            format!("there is no Measure '{table}'[{name}]"),
        ));
    };
    m.parse().map_err(syntax)?;
    let defines = Query {
        defines: measure_defines(model, db, |_| false),
        evaluates: Vec::new(),
    };
    let mut ev = Evaluator::new(db, options);
    ev.run(&defines)?;
    let reference = Expr {
        kind: ExprKind::Column {
            table: Some(table.into()),
            column: m.name.clone(),
        },
        span: Span::default(),
    };
    ev.scalar(&reference, fc)
}

/// `expr` evaluated once per row of `db.tables[table]`, with that row as the row context and no
/// filters (a Calculated column), with the Model's Measures in scope. The values are charged to
/// the memory cap; the cancel flag is checked every [`CHUNK`] rows.
pub fn evaluate_rows(
    db: &Database,
    model: &Model,
    table: usize,
    expr: &Expr,
    options: Options<'_>,
) -> Result<Vec<Value>> {
    let rows = db
        .tables
        .get(table)
        .map(|t| t.rows)
        .ok_or_else(|| DaxError::new(expr.span, "unknown Table"))?;
    let defines = Query {
        defines: measure_defines(model, db, |_| false),
        evaluates: Vec::new(),
    };
    let mut ev = Evaluator::new(db, options);
    ev.run(&defines)?;
    ev.charge(rows.saturating_mul(std::mem::size_of::<Value>()), expr.span)?;
    let root = FilterContext::default();
    let mut out = Vec::with_capacity(rows);
    for row in 0..rows {
        if row % CHUNK == 0 {
            ev.cancel.check()?;
        }
        let v = ev.with_row(RowFrame::Base { table, row }, |ev| ev.scalar(expr, &root))?;
        if let Value::Text(s) = &v {
            ev.charge(s.len(), expr.span)?;
        }
        out.push(v);
    }
    Ok(out)
}

fn syntax(e: opendash_dax::Error) -> DaxError {
    DaxError {
        kind: ErrorKind::Syntax,
        span: e.span,
        message: e.message,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Int64Array, RecordBatch, StringArray};
    use opendash_data::culture::Culture;
    use opendash_data::{Table, TableBuilder};

    use super::*;
    use crate::{Measure, ModelTable};

    fn sales() -> (Model, Vec<Table>) {
        let batch = RecordBatch::try_from_iter([
            (
                "Region",
                Arc::new(StringArray::from(vec!["North", "South", "North"])) as ArrayRef,
            ),
            ("Qty", Arc::new(Int64Array::from(vec![3, 4, 5])) as ArrayRef),
        ])
        .unwrap();
        let mut b = TableBuilder::new("Sales", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        let t = b.finish().unwrap();
        let us = Culture::get("en-US").unwrap();
        let (m, t) = ModelTable::load(None, t, us, usize::MAX).unwrap();
        let mut model = Model::default();
        model.put(m);
        (model, vec![t])
    }

    #[test]
    fn queries_see_model_measures_and_may_override_them() {
        let (mut model, data) = sales();
        model
            .add_measure("Sales", Measure::new("Total", "COUNTROWS(Sales) * 4"))
            .unwrap();
        model
            .add_measure("Sales", Measure::new("Twice", "[Total] * 2"))
            .unwrap();
        model
            .add_measure("Sales", Measure::new("Broken", "SUM(("))
            .unwrap();
        let db = Database::new(&model, &data);
        let r = run_model_query_text(
            &db,
            &model,
            "EVALUATE ROW(\"t\", [Twice])",
            Options::default(),
        )
        .unwrap();
        assert_eq!(r[0].rows, [[Value::Integer(24)]]);
        let r = run_model_query_text(
            &db,
            &model,
            "DEFINE MEASURE Sales[Total] = 1 EVALUATE ROW(\"t\", [Twice])",
            Options::default(),
        )
        .unwrap();
        assert_eq!(r[0].rows, [[Value::Integer(2)]]);
        let v = evaluate_measure(
            &db,
            &model,
            "Sales",
            "twice",
            &FilterContext::default(),
            Options::default(),
        )
        .unwrap();
        assert_eq!(v, Value::Integer(24));
        let e = evaluate_measure(
            &db,
            &model,
            "Sales",
            "Broken",
            &FilterContext::default(),
            Options::default(),
        )
        .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Syntax);
    }

    #[test]
    fn rows_are_evaluated_with_their_row_context() {
        let (model, data) = sales();
        let db = Database::new(&model, &data);
        let e = opendash_dax::parse_expr("[Qty] * 10 & \"\"").unwrap();
        let v = evaluate_rows(&db, &model, 0, &e, Options::default()).unwrap();
        assert_eq!(v, [Value::text("30"), Value::text("40"), Value::text("50")]);
        let tight = Options {
            memory_cap: db.loaded_bytes,
            ..Options::default()
        };
        let e = evaluate_rows(&db, &model, 0, &e, tight).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Memory);
    }
}
