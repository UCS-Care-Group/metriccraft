//! Applied Step commands (spec Q-1). Each appends a step to a Query (or deletes one), checks it
//! on the Preview rows and marks the Query's table Stale; the table loads on Close & Apply or
//! Refresh (see `load.rs`). All are [`Run::Edit`](crate::Run::Edit), so undo restores the steps
//! and the Stale flags together, and a step that can't run changes nothing.

use arrow_schema::DataType;
use opendash_data::step::{
    ColumnType, Combine, Condition, JoinKind, Merge, Op, Step, Value as FilterValue,
};
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

pub(crate) fn filter_rows(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "transform.filterRows";
    let q = query(doc, params, ID)?;
    let combine = match params.get("combine").map(Value::as_str) {
        None | Some(Some("and")) => Combine::And,
        Some(Some("or")) => Combine::Or,
        _ => return Err(invalid(ID, "`combine` must be `and` or `or`".into())),
    };
    let list = params
        .get("conditions")
        .and_then(Value::as_array)
        .filter(|l| !l.is_empty())
        .ok_or_else(|| invalid(ID, "`conditions` must be a non-empty list".into()))?;
    let conditions = list
        .iter()
        .map(|c| condition(doc, q, c))
        .collect::<Result<Vec<_>>>()?;
    add_step(doc, ID, params, |_| {
        Ok(Step::FilteredRows {
            combine,
            conditions,
        })
    })
}

/// One `{column, op, value}` of `transform.filterRows`. Text values for a date or datetime
/// column (as loaded for Query `q`) are read as dates in the Query's Culture.
fn condition(doc: &Document, q: usize, c: &Value) -> Result<Condition> {
    const ID: &str = "transform.filterRows";
    let column = text(c, "column", ID)?;
    let op = match text(c, "op", ID)?.as_str() {
        "equals" => Op::Equals,
        "notEquals" => Op::NotEquals,
        "greater" => Op::Greater,
        "greaterOrEqual" => Op::GreaterOrEqual,
        "less" => Op::Less,
        "lessOrEqual" => Op::LessOrEqual,
        "contains" => Op::Contains,
        "beginsWith" => Op::BeginsWith,
        "endsWith" => Op::EndsWith,
        other => return Err(invalid(ID, format!("unknown op `{other}`"))),
    };
    let query = doc.queries.get(q);
    // The column's type after the Query's steps so far (its table may be Stale).
    let ty = query.and_then(|query| {
        let step = query.steps.len().checked_sub(1);
        let preview = query
            .preview_with(
                &doc.queries,
                &crate::load::fresh(doc),
                step,
                default_memory_cap(),
            )
            .ok()?;
        let schema = preview.batch.schema();
        let f = schema.field_with_name(&column).ok()?;
        Some(f.data_type().clone())
    });
    let bad = |what: &str| invalid(ID, format!("`{column}`: {what}"));
    let value = match c.get("value") {
        None => return Err(bad("`value` is required (null for is null)")),
        Some(Value::Null) => FilterValue::Null,
        Some(Value::Bool(b)) => FilterValue::Logical(*b),
        Some(Value::Number(n)) => FilterValue::Number(n.as_f64().ok_or_else(|| bad("bad number"))?),
        Some(Value::String(s)) => match (ty, query) {
            (Some(DataType::Date32 | DataType::Date64), Some(query)) => FilterValue::Date(
                query
                    .culture
                    .parse_date(s)
                    .ok_or_else(|| bad("not a date"))?,
            ),
            (Some(DataType::Timestamp(..)), Some(query)) => FilterValue::DateTime(
                query
                    .culture
                    .parse_datetime(s)
                    .ok_or_else(|| bad("not a date and time"))?,
            ),
            _ => FilterValue::Text(s.clone()),
        },
        Some(_) => return Err(bad("`value` must be a number, text, logical or null")),
    };
    Ok(Condition { column, op, value })
}

pub(crate) fn merge_queries(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "home.mergeQueries";
    query(doc, params, ID)?;
    let other = text(params, "with", ID)?;
    if !doc.queries.iter().any(|q| q.name == other) {
        return Err(invalid(ID, format!("there is no query `{other}`")));
    }
    let kind = match params.get("kind").map(Value::as_str) {
        None | Some(Some("leftOuter")) => JoinKind::LeftOuter,
        Some(Some("inner")) => JoinKind::Inner,
        Some(Some("fullOuter")) => JoinKind::FullOuter,
        _ => {
            return Err(invalid(
                ID,
                "`kind` must be leftOuter, inner or fullOuter".into(),
            ));
        }
    };
    let column = match params.get("column") {
        None => other.clone(),
        Some(_) => text(params, "column", ID)?,
    };
    let expand = match params.get("expand") {
        Some(_) => list(params, "expand", ID)?,
        // Power Query's expand dialog ticks every column of the other table.
        None => doc
            .tables
            .iter()
            .find(|t| t.name() == other)
            .map(|t| {
                let schema = t.batch().schema();
                schema.fields().iter().map(|f| f.name().clone()).collect()
            })
            .ok_or_else(|| invalid(ID, format!("`{other}` isn't loaded; list `expand`")))?,
    };
    let prefix = match params.get("prefix") {
        None => true,
        Some(p) => p
            .as_bool()
            .ok_or_else(|| invalid(ID, "`prefix` must be true or false".into()))?,
    };
    let merge = Merge {
        keys: list(params, "keys", ID)?,
        query: other,
        other_keys: list(params, "withKeys", ID)?,
        kind,
        expand: expand
            .into_iter()
            .map(|c| {
                let name = if prefix {
                    format!("{column}.{c}")
                } else {
                    c.clone()
                };
                (c, name)
            })
            .collect(),
        column,
    };
    if merge.keys.len() != merge.other_keys.len() {
        return Err(invalid(ID, "`keys` and `withKeys` must pair up".into()));
    }
    add_step(doc, ID, params, |_| Ok(Step::MergedQueries(merge)))
}

/// After an edit to Query `q`: checks its steps run on the Preview rows (a step that can't run
/// is an [`Error::InvalidParams`]: the edit asked for something the data doesn't allow), then
/// marks its table Stale. The table itself is not reloaded until Close & Apply or Refresh.
/// A Source that can't be read doesn't stop the edit (as in Power Query, editing works offline).
fn reload(doc: &mut Document, q: usize, id: &str) -> Result {
    let query = doc
        .queries
        .get(q)
        .ok_or_else(|| invalid(id, "no such query".into()))?;
    if let Some(last) = query.steps.len().checked_sub(1) {
        let limit = default_memory_cap().saturating_sub(doc.tables.iter().map(Table::memory).sum());
        if let Err(e @ opendash_data::Error::Step { .. }) =
            query.preview_with(&doc.queries, &crate::load::fresh(doc), Some(last), limit)
        {
            return Err(invalid(id, e.to_string()));
        }
    }
    let name = query.name.clone();
    doc.stale.insert(name);
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
    list(params, "columns", id)
}

/// `params[key]`: a non-empty list of column names.
fn list(params: &Value, key: &str, id: &str) -> Result<Vec<String>> {
    let bad = || {
        invalid(
            id,
            format!("`{key}` must be a non-empty list of column names"),
        )
    };
    let list = params
        .get(key)
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
    use opendash_data::Query;
    use opendash_data::culture::Culture;
    use opendash_data::source::{Bytes, File, Source, csv};
    use serde_json::json;

    use super::*;
    use crate::COMMANDS;

    /// A Query `Sales` over a raw CSV source (in memory) with a header row, loaded with no steps
    /// yet.
    fn doc() -> Document {
        let file = File::Bytes {
            name: "sales.csv".into(),
            bytes: Bytes::from_static(b"Region,Qty,Price\nNorth,3,1.5\n,x,2\n"),
        };
        let query = Query {
            name: "Sales".into(),
            source: Source::csv("Sales", file, csv::Options::default()).unwrap(),
            steps: vec![],
            culture: Culture::get("en-GB").unwrap(),
        };
        Document {
            tables: vec![query.run(usize::MAX).unwrap().0],
            queries: vec![query],
            ..Document::default()
        }
    }

    /// `doc()` with headers promoted and applied.
    fn headed() -> App {
        let mut app = App::with_document(doc());
        app.run("transform.useFirstRowAsHeaders", &json!({"query": "Sales"}))
            .unwrap();
        apply(&mut app);
        app
    }

    fn apply(app: &mut App) {
        app.run("home.closeApply", &Value::Null).unwrap();
    }

    fn columns(app: &App) -> Vec<String> {
        let t = &app.document().tables[0];
        let schema = t.batch().schema();
        schema.fields().iter().map(|f| f.name().clone()).collect()
    }

    /// Runs `id`: the table stays as it was, marked Stale; Close & Apply loads the Query's result
    /// and clears Stale; undoing both restores everything.
    fn runs_and_undoes(app: &mut App, id: &str, params: Value) {
        let before = app.document().clone();
        app.run(id, &params).unwrap();
        let doc = app.document();
        assert_eq!(doc.tables, before.tables, "{id}: the table is unchanged");
        assert!(doc.stale.contains("Sales"), "{id}: Stale");
        apply(app);
        let doc = app.document();
        let (want, _) = doc.queries[0].run(usize::MAX).unwrap();
        assert_eq!(doc.tables, [want], "{id}: the table is the Query's result");
        assert!(doc.stale.is_empty());
        app.run("edit.undo", &Value::Null).unwrap();
        assert!(
            app.document().stale.contains("Sales"),
            "{id}: undo restores Stale"
        );
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
        assert_eq!(
            columns(&app),
            ["Region", "Qty", "Price"],
            "Stale until applied"
        );
        apply(&mut app);
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

    #[test]
    fn filter_rows_runs_and_undoes() {
        let mut app = headed();
        runs_and_undoes(
            &mut app,
            "transform.filterRows",
            json!({"query": "Sales", "conditions": [
                {"column": "Region", "op": "equals", "value": null},
                {"column": "Qty", "op": "beginsWith", "value": "3"},
            ], "combine": "or"}),
        );
        // A CSV blank is text "" (as in Power Query) until Changed Type: "x" becomes null here.
        app.run(
            "transform.changeType",
            &json!({"query": "Sales", "columns": ["Qty"], "type": "whole"}),
        )
        .unwrap();
        app.run(
            "transform.filterRows",
            &json!({"query": "Sales", "conditions": [
                {"column": "Qty", "op": "notEquals", "value": null}
            ]}),
        )
        .unwrap();
        apply(&mut app);
        assert_eq!(app.document().tables[0].rows(), 1);
    }

    #[test]
    fn filter_reads_text_as_dates_for_date_columns() {
        let mut app = headed();
        app.run(
            "transform.changeType",
            &json!({"query": "Sales", "columns": ["Price"], "type": "date"}),
        )
        .unwrap();
        let p = json!({"query": "Sales", "conditions": [
            {"column": "Price", "op": "greater", "value": "2026-01-31"}
        ]});
        app.run("transform.filterRows", &p).unwrap();
        let Some(Step::FilteredRows { conditions, .. }) = app.document().queries[0].steps.last()
        else {
            panic!("no filter step");
        };
        assert_eq!(conditions[0].value, FilterValue::Date(20_484));
        let bad = json!({"query": "Sales", "conditions": [
            {"column": "Price", "op": "greater", "value": "soon"}
        ]});
        assert!(matches!(
            app.run("transform.filterRows", &bad),
            Err(Error::InvalidParams { .. })
        ));
    }

    /// `headed()` plus a Query `Targets` (Region, Target), headed and loaded.
    fn two() -> App {
        let file = File::Bytes {
            name: "targets.csv".into(),
            bytes: Bytes::from_static(b"Region,Target\nNorth,10\nNorth,20\nEast,5\n"),
        };
        let query = Query {
            name: "Targets".into(),
            source: Source::csv("Targets", file, csv::Options::default()).unwrap(),
            steps: vec![],
            culture: Culture::get("en-GB").unwrap(),
        };
        let mut doc = headed().document().clone();
        doc.tables.push(query.run(usize::MAX).unwrap().0);
        doc.queries.push(query);
        let mut app = App::with_document(doc);
        app.run(
            "transform.useFirstRowAsHeaders",
            &json!({"query": "Targets"}),
        )
        .unwrap();
        apply(&mut app);
        app
    }

    fn merge_params() -> Value {
        json!({"query": "Sales", "keys": ["Region"], "with": "Targets", "withKeys": ["Region"]})
    }

    #[test]
    fn merge_queries_joins_reloads_dependents_and_undoes() {
        let mut app = two();
        let before = app.document().clone();
        app.run("home.mergeQueries", &merge_params()).unwrap();
        apply(&mut app);
        assert_eq!(
            columns(&app),
            ["Region", "Qty", "Price", "Targets.Region", "Targets.Target"]
        );
        assert_eq!(
            app.document().tables[0].rows(),
            3,
            "North × 2 + unmatched blank"
        );
        // Editing Targets makes it Stale; Close & Apply also reloads Sales, which merges it.
        app.run(
            "transform.filterRows",
            &json!({"query": "Targets", "conditions": [
                {"column": "Target", "op": "equals", "value": "10"}
            ]}),
        )
        .unwrap();
        assert_eq!(app.document().tables[0].rows(), 3);
        assert_eq!(app.document().stale.iter().collect::<Vec<_>>(), ["Targets"]);
        apply(&mut app);
        assert_eq!(app.document().tables[0].rows(), 2);
        assert_eq!(app.document().tables[1].rows(), 1);
        for _ in 0..4 {
            app.run("edit.undo", &Value::Null).unwrap();
        }
        assert_eq!(app.document(), &before);

        let mut p = merge_params();
        p.as_object_mut().unwrap().extend(
            json!({"kind": "inner", "expand": ["Target"], "prefix": false, "column": "T"})
                .as_object()
                .unwrap()
                .clone(),
        );
        app.run("home.mergeQueries", &p).unwrap();
        apply(&mut app);
        assert_eq!(columns(&app), ["Region", "Qty", "Price", "Target"]);
        assert_eq!(app.document().tables[0].rows(), 2);
        let Some(Step::MergedQueries(m)) = app.document().queries[0].steps.last() else {
            panic!("no merge step");
        };
        assert_eq!(
            (m.kind, m.column.as_str(), &m.expand),
            (
                JoinKind::Inner,
                "T",
                &vec![("Target".into(), "Target".into())]
            )
        );
    }

    #[test]
    fn bad_merges_are_invalid_and_change_nothing() {
        let with = |more: Value| {
            let mut p = merge_params();
            p.as_object_mut()
                .unwrap()
                .extend(more.as_object().unwrap().clone());
            p
        };
        let mut cyclic = two();
        cyclic.run("home.mergeQueries", &merge_params()).unwrap();
        for (mut app, params) in [
            (two(), with(json!({"with": "Sales"}))),
            (two(), with(json!({"with": "Nope"}))),
            (two(), with(json!({"kind": "rightAnti"}))),
            (two(), with(json!({"withKeys": ["Region", "Target"]}))),
            (two(), with(json!({"keys": ["Nope"]}))),
            (two(), with(json!({"expand": ["Nope"]}))),
            (two(), with(json!({"expand": ["Region"], "prefix": false}))),
            (two(), with(json!({"prefix": "yes"}))),
            (two(), with(json!({"keys": []}))),
            (
                cyclic,
                json!({"query": "Targets", "keys": ["Region"], "with": "Sales", "withKeys": ["Region"]}),
            ),
        ] {
            let before = app.document().clone();
            let r = app.run("home.mergeQueries", &params);
            assert!(
                matches!(r, Err(Error::InvalidParams { .. })),
                "{params}: {r:?}"
            );
            assert_eq!(app.document(), &before, "{params}");
        }
    }

    #[test]
    fn bad_filters_are_invalid_and_change_nothing() {
        for conditions in [
            json!([]),
            json!([{"column": "Qty", "op": "like", "value": 1}]),
            json!([{"column": "Qty", "op": "equals"}]),
            json!([{"column": "Qty", "op": "equals", "value": [1]}]),
            json!([{"column": "Nope", "op": "equals", "value": 1}]),
            json!([{"column": "Qty", "op": "greater", "value": 1}]),
        ] {
            let mut app = headed();
            let before = app.document().clone();
            let params = json!({"query": "Sales", "conditions": conditions});
            let r = app.run("transform.filterRows", &params);
            assert!(
                matches!(r, Err(Error::InvalidParams { .. })),
                "{params}: {r:?}"
            );
            assert_eq!(app.document(), &before);
        }
        let mut app = headed();
        let r = app.run(
            "transform.filterRows",
            &json!({"query": "Sales", "combine": "xor", "conditions": [
                {"column": "Qty", "op": "equals", "value": "3"}
            ]}),
        );
        assert!(matches!(r, Err(Error::InvalidParams { .. })));
    }
}
