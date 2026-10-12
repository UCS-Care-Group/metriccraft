//! Applied Step commands (spec Q-1). Each appends a step to a Query (or deletes one) and re-runs
//! the Query, replacing its loaded table. All are [`Run::Edit`](crate::Run::Edit), so undo
//! restores the steps and the table together, and a step that can't run changes nothing.
//!
//! Open points: the result here is the full run over the Query's source; M1.8 splits it into
//! Preview (first 1,000 rows, at a selected step, Cell errors shown per cell) and Refresh (what
//! loads, reporting the Cell error count, which is dropped here). M1.7 prints each step as M.

use opendash_data::step::{ColumnType, Step};
use opendash_data::{Table, default_memory_cap};
use serde_json::Value;

use crate::{App, Document, Error, Result};

pub(crate) fn has_queries(app: &App) -> bool {
    !app.document().queries.is_empty()
}

pub(crate) fn use_first_row_as_headers(doc: &mut Document, params: &Value) -> Result {
    add_step(doc, "transform.useFirstRowAsHeaders", params, |_| {
        Ok(Step::PromotedHeaders)
    })
}

pub(crate) fn change_type(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "transform.changeType";
    add_step(doc, ID, params, |p| {
        let ty = match text(p, "type", ID)?.as_str() {
            "text" => ColumnType::Text,
            "decimal" => ColumnType::Decimal,
            "whole" => ColumnType::Whole,
            "date" => ColumnType::Date,
            "datetime" => ColumnType::DateTime,
            "boolean" => ColumnType::Boolean,
            other => return Err(invalid(ID, format!("unknown type `{other}`"))),
        };
        let columns = names(p, ID)?;
        Ok(Step::ChangedType(
            columns.into_iter().map(|c| (c, ty)).collect(),
        ))
    })
}

pub(crate) fn rename_column(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "transform.renameColumn";
    add_step(doc, ID, params, |p| {
        Ok(Step::RenamedColumns(vec![(
            text(p, "column", ID)?,
            text(p, "name", ID)?,
        )]))
    })
}

pub(crate) fn remove_columns(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "home.removeColumns";
    add_step(doc, ID, params, |p| Ok(Step::RemovedColumns(names(p, ID)?)))
}

pub(crate) fn reorder_columns(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "transform.reorderColumns";
    add_step(doc, ID, params, |p| {
        Ok(Step::ReorderedColumns(names(p, ID)?))
    })
}

pub(crate) fn delete_step(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "query.deleteStep";
    let q = query(doc, params, ID)?;
    let step = params
        .get("step")
        .and_then(Value::as_u64)
        .and_then(|s| usize::try_from(s).ok())
        .ok_or_else(|| invalid(ID, "`step` must be a step position (0 = first)".into()))?;
    let steps = doc.queries.get_mut(q).map(|q| &mut q.steps);
    match steps {
        Some(steps) if step < steps.len() => {
            steps.remove(step);
        }
        _ => return Err(invalid(ID, format!("there is no step {step}"))),
    }
    reload(doc, q, ID)
}

/// Appends the step `make` builds from `params` to the Query `params.query`, then reloads it.
fn add_step(
    doc: &mut Document,
    id: &str,
    params: &Value,
    make: impl FnOnce(&Value) -> Result<Step>,
) -> Result {
    let q = query(doc, params, id)?;
    let step = make(params)?;
    if let Some(query) = doc.queries.get_mut(q) {
        query.steps.push(step);
    }
    reload(doc, q, id)
}

/// Re-runs Query `q` and replaces (or adds) its table. A step that fails is an
/// [`Error::InvalidParams`]: the edit asked for something the data doesn't allow.
fn reload(doc: &mut Document, q: usize, id: &str) -> Result {
    let query = doc
        .queries
        .get(q)
        .ok_or_else(|| invalid(id, "no such query".into()))?;
    let others: usize = doc
        .tables
        .iter()
        .filter(|t| t.name() != query.name)
        .map(Table::memory)
        .sum();
    let (table, _cell_errors) = query
        .run(default_memory_cap().saturating_sub(others))
        .map_err(|e| match e {
            opendash_data::Error::Step { .. } => invalid(id, e.to_string()),
            e => Error::Data(e),
        })?;
    match doc.tables.iter_mut().find(|t| t.name() == table.name()) {
        Some(t) => *t = table,
        None => doc.tables.push(table),
    }
    Ok(())
}

/// The index of the Query named by `params.query`.
fn query(doc: &Document, params: &Value, id: &str) -> Result<usize> {
    let name = text(params, "query", id)?;
    doc.queries
        .iter()
        .position(|q| q.name == name)
        .ok_or_else(|| invalid(id, format!("there is no query `{name}`")))
}

fn text(params: &Value, key: &str, id: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(id, format!("`{key}` must be text")))
}

/// `params.columns`: a non-empty list of column names.
fn names(params: &Value, id: &str) -> Result<Vec<String>> {
    let bad = || {
        invalid(
            id,
            "`columns` must be a non-empty list of column names".into(),
        )
    };
    let list = params
        .get("columns")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
        .ok_or_else(bad)?;
    list.iter()
        .map(|v| v.as_str().map(str::to_string).ok_or_else(bad))
        .collect()
}

fn invalid(command: &str, message: String) -> Error {
    Error::InvalidParams {
        command: command.into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use arrow_array::{ArrayRef, RecordBatch, StringArray};
    use opendash_data::culture::Culture;
    use opendash_data::{Query, TableBuilder};
    use serde_json::json;

    use super::*;
    use crate::COMMANDS;

    /// A Query `Sales` over a raw text source with a header row, loaded with no steps yet.
    fn doc() -> Document {
        let col = |v: Vec<Option<&str>>| Arc::new(StringArray::from(v)) as ArrayRef;
        let batch = RecordBatch::try_from_iter([
            ("Column1", col(vec![Some("Region"), Some("North"), None])),
            ("Column2", col(vec![Some("Qty"), Some("3"), Some("x")])),
            ("Column3", col(vec![Some("Price"), Some("1.5"), Some("2")])),
        ])
        .unwrap();
        let mut b = TableBuilder::new("Sales", batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        let source = b.finish().unwrap();
        let query = Query {
            name: "Sales".into(),
            source: source.clone(),
            steps: vec![],
            culture: Culture::get("en-GB").unwrap(),
        };
        Document {
            tables: vec![source],
            queries: vec![query],
            ..Document::default()
        }
    }

    fn headed() -> App {
        let mut app = App::with_document(doc());
        app.run("transform.useFirstRowAsHeaders", &json!({"query": "Sales"}))
            .unwrap();
        app
    }

    fn columns(app: &App) -> Vec<String> {
        let t = &app.document().tables[0];
        let schema = t.batch().schema();
        schema.fields().iter().map(|f| f.name().clone()).collect()
    }

    /// Runs `id`, checks the table is the Query's result, then that undo restores everything.
    fn runs_and_undoes(app: &mut App, id: &str, params: Value) {
        let before = app.document().clone();
        app.run(id, &params).unwrap();
        let doc = app.document();
        let (want, _) = doc.queries[0].run(usize::MAX).unwrap();
        assert_eq!(doc.tables, [want], "{id}: the table is the Query's result");
        assert_eq!(doc.tables.len(), 1);
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.document(), &before, "{id}: undo restores");
    }

    #[test]
    fn every_step_command_runs_and_undoes() {
        let mut app = App::with_document(doc());
        runs_and_undoes(
            &mut app,
            "transform.useFirstRowAsHeaders",
            json!({"query": "Sales"}),
        );
        let mut app = headed();
        let sales = |more: Value| {
            let mut p = json!({"query": "Sales"});
            p.as_object_mut()
                .unwrap()
                .extend(more.as_object().unwrap().clone());
            p
        };
        for (id, params) in [
            (
                "transform.changeType",
                json!({"columns": ["Qty", "Price"], "type": "decimal"}),
            ),
            (
                "transform.renameColumn",
                json!({"column": "Qty", "name": "Quantity"}),
            ),
            ("home.removeColumns", json!({"columns": ["Region"]})),
            (
                "transform.reorderColumns",
                json!({"columns": ["Price", "Qty"]}),
            ),
            ("query.deleteStep", json!({"step": 0})),
        ] {
            runs_and_undoes(&mut app, id, sales(params));
        }
    }

    #[test]
    fn steps_shape_the_loaded_table() {
        let mut app = headed();
        for (id, params) in [
            (
                "transform.changeType",
                json!({"query": "Sales", "columns": ["Qty"], "type": "whole"}),
            ),
            (
                "transform.renameColumn",
                json!({"query": "Sales", "column": "Qty", "name": "Quantity"}),
            ),
            (
                "transform.reorderColumns",
                json!({"query": "Sales", "columns": ["Price", "Region"]}),
            ),
            (
                "home.removeColumns",
                json!({"query": "Sales", "columns": ["Region"]}),
            ),
        ] {
            app.run(id, &params).unwrap();
        }
        assert_eq!(columns(&app), ["Price", "Quantity"]);
        let t = &app.document().tables[0];
        assert_eq!(t.rows(), 2);
        assert_eq!(t.batch().schema().field(1).data_type().to_string(), "Int8");
        assert_eq!(app.document().queries[0].steps.len(), 5);
    }

    #[test]
    fn bad_params_are_invalid_and_change_nothing() {
        for (id, params) in [
            ("transform.useFirstRowAsHeaders", json!({"query": "Nope"})),
            (
                "transform.changeType",
                json!({"query": "Sales", "columns": ["Qty"], "type": "money"}),
            ),
            (
                "transform.changeType",
                json!({"query": "Sales", "columns": ["Nope"], "type": "text"}),
            ),
            (
                "transform.changeType",
                json!({"query": "Sales", "columns": [], "type": "text"}),
            ),
            (
                "transform.renameColumn",
                json!({"query": "Sales", "column": "Qty", "name": "Region"}),
            ),
            (
                "transform.renameColumn",
                json!({"query": "Sales", "column": "Qty"}),
            ),
            (
                "home.removeColumns",
                json!({"query": "Sales", "columns": ["Nope"]}),
            ),
            (
                "home.removeColumns",
                json!({"query": "Sales", "columns": [1]}),
            ),
            (
                "transform.reorderColumns",
                json!({"query": "Sales", "columns": ["Qty", "Qty"]}),
            ),
            ("query.deleteStep", json!({"query": "Sales", "step": 2})),
            ("query.deleteStep", json!({"query": "Sales", "step": -1})),
            // Later steps need the headers.
            ("query.deleteStep", json!({"query": "Sales", "step": 0})),
        ] {
            let mut app = headed();
            app.run(
                "transform.renameColumn",
                &json!({"query": "Sales", "column": "Price", "name": "Cost"}),
            )
            .unwrap();
            let before = app.document().clone();
            let r = app.run(id, &params);
            assert!(
                matches!(r, Err(Error::InvalidParams { .. })),
                "{id} {params}: {r:?}"
            );
            assert_eq!(app.document(), &before, "{id} {params}");
        }
    }

    #[test]
    fn every_command_survives_empty_params_on_a_query() {
        for cmd in COMMANDS {
            for params in [Value::Null, json!({}), json!({"query": "Sales"})] {
                let mut app = headed();
                let before = app.document().clone();
                if app.run(cmd.id, &params).is_err() {
                    assert_eq!(
                        app.document(),
                        &before,
                        "{} changed the doc on error",
                        cmd.id
                    );
                }
            }
        }
    }

    #[test]
    fn step_commands_need_a_query() {
        let mut app = App::new();
        let r = app.run(
            "home.removeColumns",
            &json!({"query": "Sales", "columns": ["A"]}),
        );
        assert_eq!(r, Err(Error::Disabled("home.removeColumns".into())));
    }
}
