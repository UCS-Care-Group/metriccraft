use arrow_array::cast::AsArray;
use opendash_core::Value as DaxValue;
use opendash_data::Query;
use opendash_data::culture::Culture;
use opendash_data::source::{Bytes, File, Source, csv};
use opendash_data::step::ColumnType;
use opendash_model::ModelTable;
use serde_json::json;

use super::*;

/// Connects `csv` as Query and Table `name` (as Get Data does), with its model.
fn add(doc: &mut Document, name: &str, csv: &'static str) {
    let file = File::Bytes {
        name: format!("{name}.csv"),
        bytes: Bytes::from_static(csv.as_bytes()),
    };
    let culture = Culture::get("en-US").unwrap();
    let source = Source::csv(name, file, csv::Options::default()).unwrap();
    let query = Query::connect(name, source, culture, usize::MAX).unwrap();
    let (table, _) = query.run(usize::MAX).unwrap();
    let (model, table) = ModelTable::load(None, table, culture, usize::MAX).unwrap();
    doc.queries.push(query);
    doc.tables.push(table);
    doc.model.put(model);
}

fn app() -> App {
    let mut doc = Document::default();
    add(&mut doc, "Sales", "Region,Qty\nNorth,3\nSouth,4\nNorth,5\n");
    add(&mut doc, "Regions", "Region,Rank\nNorth,1\nSouth,2\n");
    App::with_document(doc)
}

fn run(app: &mut App, id: &str, params: serde_json::Value) {
    app.run(id, &params)
        .unwrap_or_else(|e| panic!("{id} {params}: {e}"));
}

fn sales(app: &App) -> &ModelTable {
    app.document().model.table("Sales").unwrap()
}

/// Whole-number column `name` of loaded Table `Sales`.
fn ints(app: &App, name: &str) -> Vec<Option<i64>> {
    let t = app
        .document()
        .tables
        .iter()
        .find(|t| t.name() == "Sales")
        .unwrap();
    let c = t.batch().column_by_name(name).unwrap();
    let c = arrow_cast::cast(c, &arrow_schema::DataType::Int64).unwrap();
    c.as_primitive::<arrow_array::types::Int64Type>()
        .iter()
        .collect()
}

fn one(app: &App, query: &str) -> DaxValue {
    let r = app.document().dax_query(query).unwrap();
    r[0].rows[0][0].clone()
}

#[test]
fn measures_and_calculated_columns_are_undoable_commands() {
    let mut app = app();
    let start = app.document().clone();
    run(
        &mut app,
        "modeling.newColumn",
        json!({"table": "Sales", "name": "Double", "expression": "[Qty] * 2\r\n"}),
    );
    assert_eq!(ints(&app, "Double"), [Some(6), Some(8), Some(10)]);
    let c = sales(&app).column("Double").unwrap();
    assert_eq!(
        (c.expression.as_deref(), c.data_type),
        (Some("[Qty] * 2"), ColumnType::Whole)
    );
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "Rows", "expression": "COUNTROWS(Sales) + 1",
            "formatString": "0"}),
    );
    assert_eq!(
        one(&app, "EVALUATE ROW(\"n\", [Rows])"),
        DaxValue::Integer(4)
    );
    // A Column that uses another: recomputed when that one's formula changes.
    run(
        &mut app,
        "modeling.newColumn",
        json!({"table": "Sales", "name": "Quad", "expression": "[Double] * 2"}),
    );
    run(
        &mut app,
        "modeling.editExpression",
        json!({"table": "Sales", "name": "Double", "expression": "[Qty] + 1"}),
    );
    assert_eq!(ints(&app, "Quad"), [Some(8), Some(10), Some(12)]);
    // Properties of a Measure through the usual commands.
    run(
        &mut app,
        "model.setHidden",
        json!({"table": "Sales", "column": "Rows"}),
    );
    run(
        &mut app,
        "model.setDescription",
        json!({"table": "Sales", "column": "Rows", "description": "How many"}),
    );
    let m = sales(&app).measure("Rows").unwrap();
    assert!(m.hidden && m.description == "How many" && m.format_string == "0");
    // Delete: the Measure, then the Column (its data goes; Quad now fails, keeps BLANKs).
    run(
        &mut app,
        "modeling.delete",
        json!({"table": "Sales", "name": "Rows"}),
    );
    run(
        &mut app,
        "modeling.delete",
        json!({"table": "Sales", "name": "Double"}),
    );
    let names: Vec<_> = sales(&app)
        .columns
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["Region", "Qty", "Quad"]);
    assert!(sales(&app).measures.is_empty());
    let errors = &app.document().formula_errors;
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].column, "Quad");
    assert_eq!(ints(&app, "Quad"), [None, None, None]);
    let end = app.document().clone();
    while app.can_undo() {
        app.run("edit.undo", &Value::Null).unwrap();
    }
    assert_eq!(app.document(), &start);
    while app.can_redo() {
        app.run("edit.redo", &Value::Null).unwrap();
    }
    assert_eq!(app.document(), &end);
}

#[test]
fn refused_changes_change_nothing() {
    let mut app = app();
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "A", "expression": "1"}),
    );
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "B", "expression": "[A] + 1"}),
    );
    run(
        &mut app,
        "modeling.newColumn",
        json!({"table": "Sales", "name": "C", "expression": "[Qty]"}),
    );
    let before = app.document().clone();
    for (id, params) in [
        ("modeling.newMeasure", json!({})),
        (
            "modeling.newMeasure",
            json!({"table": "Sales", "name": "x"}),
        ),
        (
            "modeling.newMeasure",
            json!({"table": "Sales", "name": "x", "expression": "  \n"}),
        ),
        (
            "modeling.newMeasure",
            json!({"table": "Nope", "name": "x", "expression": "1"}),
        ),
        (
            "modeling.newMeasure",
            json!({"table": "Regions", "name": "b", "expression": "1"}),
        ),
        (
            "modeling.newMeasure",
            json!({"table": "Sales", "name": "QTY", "expression": "1"}),
        ),
        (
            "modeling.newMeasure",
            json!({"table": "Sales", "name": " x", "expression": "1"}),
        ),
        (
            "modeling.newColumn",
            json!({"table": "Sales", "name": "a", "expression": "1"}),
        ),
        (
            "modeling.newColumn",
            json!({"table": "Sales", "name": "c", "expression": "1"}),
        ),
        (
            "modeling.editExpression",
            json!({"table": "Sales", "name": "A", "expression": "[B]"}),
        ),
        (
            "modeling.editExpression",
            json!({"table": "Sales", "name": "C", "expression": "[C] + 1"}),
        ),
        (
            "modeling.editExpression",
            json!({"table": "Sales", "name": "Qty", "expression": "1"}),
        ),
        ("modeling.delete", json!({"table": "Sales", "name": "Qty"})),
        ("modeling.delete", json!({"table": "Sales", "name": "Nope"})),
        (
            "modeling.renameMeasure",
            json!({"table": "Sales", "measure": "A", "name": "b"}),
        ),
        (
            "modeling.renameMeasure",
            json!({"table": "Sales", "measure": "A", "name": "Region"}),
        ),
        (
            "modeling.renameMeasure",
            json!({"table": "Sales", "measure": "C", "name": "D"}),
        ),
        (
            "modeling.renameColumn",
            json!({"table": "Sales", "column": "Qty", "name": "a"}),
        ),
    ] {
        let r = app.run(id, &params);
        assert!(
            matches!(r, Err(Error::InvalidParams { .. } | Error::Model(_))),
            "{id} {params}: {r:?}"
        );
        assert_eq!(app.document(), &before, "{id} {params}");
    }
    let e = app
        .run(
            "modeling.editExpression",
            &json!({"table": "Sales", "name": "A", "expression": "[B]"}),
        )
        .unwrap_err();
    assert!(e.to_string().contains("circular dependency"), "{e}");
    // A formula that doesn't parse is kept, with its error.
    run(
        &mut app,
        "modeling.editExpression",
        json!({"table": "Sales", "name": "C", "expression": "SUM(("}),
    );
    let c = sales(&app).column("C").unwrap();
    assert_eq!(c.expression.as_deref(), Some("SUM(("));
    let e = &app.document().formula_errors[0];
    assert!(e.column == "C" && e.message.contains("syntax"), "{e:?}");
}

#[test]
fn renames_rewrite_dax_and_carry_over() {
    let mut app = app();
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "Rows", "expression": "COUNTROWS ( 'Sales' ) // all"}),
    );
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Regions", "name": "Twice", "expression": "[Rows] * 2"}),
    );
    run(
        &mut app,
        "modeling.newColumn",
        json!({"table": "Sales", "name": "Double", "expression": "Sales[Qty]*2"}),
    );
    run(
        &mut app,
        "modeling.renameTable",
        json!({"table": "Sales", "name": "Orders"}),
    );
    run(
        &mut app,
        "modeling.renameColumn",
        json!({"table": "Orders", "column": "Qty", "name": "Units"}),
    );
    run(
        &mut app,
        "modeling.renameColumn",
        json!({"table": "Orders", "column": "Double", "name": "Twice Units"}),
    );
    run(
        &mut app,
        "modeling.renameMeasure",
        json!({"table": "Orders", "measure": "Rows", "name": "Row Count"}),
    );
    let doc = app.document();
    let orders = doc.model.table("Orders").unwrap();
    assert_eq!(
        orders.measure("Row Count").unwrap().expression,
        "COUNTROWS ( 'Orders' ) // all"
    );
    assert_eq!(
        orders.column("Twice Units").unwrap().expression.as_deref(),
        Some("Orders[Units]*2")
    );
    let regions = doc.model.table("Regions").unwrap();
    assert_eq!(
        regions.measure("Twice").unwrap().expression,
        "[Row Count] * 2"
    );
    let q = doc.queries.iter().find(|q| q.name == "Orders").unwrap();
    assert_eq!(
        q.steps.len(),
        3,
        "one rename step, for the Query's column only"
    );
    assert_eq!(
        one(&app, "EVALUATE ROW(\"n\", [Twice])"),
        DaxValue::Integer(6)
    );
    // Refresh: still computed, under the new names.
    run(&mut app, "home.refresh", Value::Null);
    let t = &app.document().tables[0];
    let names: Vec<_> = t
        .batch()
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    assert_eq!(names, ["Region", "Units", "Twice Units"]);
}

#[test]
fn loads_recompute_and_a_query_may_not_take_a_calculated_name() {
    let mut app = app();
    run(
        &mut app,
        "modeling.newColumn",
        json!({"table": "Sales", "name": "Big", "expression": "[Qty] > 3"}),
    );
    run(
        &mut app,
        "transform.filterRows",
        json!({"query": "Sales", "conditions": [{"column": "Qty", "op": "greater", "value": 3}]}),
    );
    run(&mut app, "home.closeApply", Value::Null);
    let t = &app.document().tables[0];
    let big = t
        .batch()
        .column_by_name("Big")
        .unwrap()
        .as_boolean()
        .clone();
    assert_eq!(big.iter().collect::<Vec<_>>(), [Some(true), Some(true)]);
    run(
        &mut app,
        "transform.renameColumn",
        json!({"query": "Sales", "column": "Region", "name": "big"}),
    );
    let before = app.document().clone();
    let e = app.run("home.closeApply", &Value::Null).unwrap_err();
    assert!(e.to_string().contains("Calculated column"), "{e}");
    assert_eq!(app.document(), &before);
}

#[test]
fn calculated_columns_are_recomputed_on_open_not_cached() {
    let mut app = app();
    run(
        &mut app,
        "modeling.newColumn",
        json!({"table": "Sales", "name": "Double", "expression": "[Qty] * 2"}),
    );
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "Rows", "expression": "COUNTROWS(Sales)"}),
    );
    let bytes = app.package_bytes().unwrap();
    let files = opendash_file::unpack(&bytes).unwrap();
    let meta = String::from_utf8(files[&opendash_file::cache::meta_path("Sales")].clone()).unwrap();
    assert!(!meta.contains("Double"), "{meta}");
    let odm = String::from_utf8(files["model/tables/Sales.odm"].clone()).unwrap();
    assert!(
        odm.contains("  column Double =\n      [Qty] * 2\n    dataType: whole\n"),
        "{odm}"
    );
    assert!(
        odm.contains("  measure Rows =\n      COUNTROWS(Sales)\n"),
        "{odm}"
    );
    let mut other = App::new();
    other.open_package_bytes(&bytes).unwrap();
    assert_eq!(other.document().tables, app.document().tables);
    assert_eq!(other.document().model, app.document().model);
    assert_eq!(ints(&other, "Double"), [Some(6), Some(8), Some(10)]);
}
