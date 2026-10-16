use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use metriccraft_data::Query;
use metriccraft_data::culture::Culture;
use metriccraft_data::source::{Bytes, File, Source, csv};
use metriccraft_model::ModelTable;
use serde_json::json;

use metriccraft_visuals::query::{MAX_AXIS_FIELDS, MAX_VALUE_FIELDS};

use super::*;
use crate::Error;

/// Connects `csv` as Query and Table `name` (as Get Data does), with its model.
fn add_table(doc: &mut Document, name: &str, csv: &'static str) {
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

/// Sales (Region, Product, Qty), Regions (Region, Manager, Rank) and the Measures Total and Count.
fn app() -> App {
    let mut doc = Document::default();
    add_table(
        &mut doc,
        "Sales",
        "Region,Product,Qty\nNorth,Pen,3\nSouth,Ink,4\nNorth,Ink,5\n",
    );
    add_table(
        &mut doc,
        "Regions",
        "Region,Manager,Rank\nNorth,Ann,1\nSouth,Bob,2\n",
    );
    let mut app = App::with_document(doc);
    for (name, dax) in [
        ("Total", "SUM ( Sales[Qty] )"),
        ("Count", "COUNTROWS ( Sales )"),
    ] {
        app.run(
            "modeling.newMeasure",
            &json!({"table": "Sales", "name": name, "expression": dax}),
        )
        .unwrap();
    }
    app
}

fn run(app: &mut App, id: &str, params: Value) {
    if let Err(e) = app.run(id, &params) {
        panic!("{id} {params}: {e}");
    }
}

/// Runs a command that must fail with InvalidParams, changing nothing.
fn refused(app: &mut App, id: &str, params: Value) -> String {
    let before = app.document().clone();
    match app.run(id, &params) {
        Err(Error::InvalidParams { message, .. }) => {
            assert_eq!(
                app.document(),
                &before,
                "{id} failed but changed the document"
            );
            message
        }
        other => panic!("{id} {params}: expected InvalidParams, got {other:?}"),
    }
}

fn pages(app: &App) -> Vec<&str> {
    let pages = &app.document().report.pages;
    pages.iter().map(|p| p.name.as_str()).collect()
}

fn page(app: &App) -> &Page {
    app.document().report.pages.first().unwrap()
}

/// Adds a Visual of `kind` to the first Page (made if none) and returns its id.
fn new_visual(app: &mut App, kind: &str) -> String {
    let before: Vec<String> = app
        .document()
        .report
        .pages
        .first()
        .map(|p| p.visuals.iter().map(|v| v.id.clone()).collect())
        .unwrap_or_default();
    run(app, "insert.newVisual", json!({"type": kind}));
    page(app)
        .visuals
        .iter()
        .find(|v| !before.contains(&v.id))
        .unwrap()
        .id
        .clone()
}

fn visual<'a>(app: &'a App, id: &str) -> &'a Visual {
    page(app).visual(id).unwrap()
}

fn col(table: &str, column: &str) -> FieldRef {
    FieldRef::Column {
        table: table.into(),
        column: column.into(),
    }
}

fn measure(name: &str) -> FieldRef {
    FieldRef::Measure { name: name.into() }
}

fn add_field(app: &mut App, v: &str, well: &str, field: Value) {
    let p = page(app).id.clone();
    run(
        app,
        "visual.addField",
        json!({"page": p, "visual": v, "well": well, "field": field}),
    );
}

fn fields(app: &App, v: &str, well: Well) -> Vec<FieldRef> {
    visual(app, v).fields(well).to_vec()
}

fn undo_redo_round_trips(app: &mut App, steps: usize) {
    let after = app.document().clone();
    for _ in 0..steps {
        run(app, "edit.undo", Value::Null);
    }
    let before = app.document().clone();
    assert_ne!(before, after);
    for _ in 0..steps {
        run(app, "edit.redo", Value::Null);
    }
    assert_eq!(app.document(), &after);
}

#[test]
fn pages_are_added_renamed_moved_and_deleted_undoably() {
    let mut app = app();
    assert!(app.document().report.pages.is_empty());
    run(&mut app, "insert.newPage", json!({}));
    run(&mut app, "insert.newPage", Value::Null);
    run(&mut app, "insert.newPage", json!({"name": "Summary"}));
    assert_eq!(pages(&app), ["Page 1", "Page 2", "Summary"]);
    let p = page(&app);
    assert_eq!((p.width, p.height), (PAGE_WIDTH, PAGE_HEIGHT));
    assert_eq!(p.id.len(), 8);
    let ids: std::collections::BTreeSet<&str> = app
        .document()
        .report
        .pages
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(ids.len(), 3, "ids are unique");

    run(
        &mut app,
        "page.rename",
        json!({"page": "Page 2", "name": "Detail"}),
    );
    run(
        &mut app,
        "page.move",
        json!({"page": "Summary", "index": 0}),
    );
    assert_eq!(pages(&app), ["Summary", "Page 1", "Detail"]);
    let id = app.document().report.page("Detail").unwrap().id.clone();
    run(&mut app, "page.move", json!({"page": id, "index": 2}));
    run(&mut app, "page.delete", json!({"page": "page 1"}));
    assert_eq!(pages(&app), ["Summary", "Detail"]);
    // A new page takes the first free number.
    run(&mut app, "insert.newPage", json!({}));
    assert_eq!(pages(&app), ["Summary", "Detail", "Page 1"]);
    undo_redo_round_trips(&mut app, 5);
    for _ in 0..8 {
        run(&mut app, "edit.undo", Value::Null);
    }
    assert!(app.document().report.pages.is_empty());
}

#[test]
fn bad_page_params_are_errors() {
    let mut app = app();
    run(&mut app, "insert.newPage", json!({"name": "Sales"}));
    assert!(refused(&mut app, "insert.newPage", json!({"name": "SALES"})).contains("already"));
    refused(&mut app, "insert.newPage", json!({"name": " "}));
    refused(&mut app, "insert.newPage", json!({"name": "a\nb"}));
    refused(&mut app, "insert.newPage", json!({"name": 3}));
    assert!(refused(&mut app, "page.delete", json!({"page": "Sales"})).contains("at least one"));
    refused(&mut app, "page.delete", json!({}));
    refused(
        &mut app,
        "page.rename",
        json!({"page": "Nope", "name": "X"}),
    );
    refused(&mut app, "page.rename", json!({"page": "Sales"}));
    refused(&mut app, "page.move", json!({"page": "Sales", "index": 1}));
    refused(&mut app, "page.move", json!({"page": "Sales", "index": -1}));
    refused(&mut app, "page.move", json!({"page": "Sales"}));
    // Renaming to its own name, or a change of case, is fine.
    run(
        &mut app,
        "page.rename",
        json!({"page": "Sales", "name": "SALES"}),
    );
    assert_eq!(pages(&app), ["SALES"]);
}

#[test]
fn visuals_are_added_moved_resized_and_deleted_undoably() {
    let mut app = app();
    // No page yet: the first visual makes `Page 1`, in the same undo step.
    let a = new_visual(&mut app, "table");
    assert_eq!(pages(&app), ["Page 1"]);
    let v = visual(&app, &a);
    assert_eq!(
        (v.kind, v.x, v.y, v.w, v.h, v.z),
        (VisualType::Table, 0, 0, 400, 300, 0)
    );
    let p = page(&app).id.clone();
    run(
        &mut app,
        "insert.newVisual",
        json!({"page": p, "type": "Card", "x": 10, "y": 20, "w": 30, "h": 40}),
    );
    let b = page(&app)
        .visuals
        .iter()
        .find(|v| v.id != a)
        .unwrap()
        .clone();
    assert_eq!(
        (b.kind, b.x, b.y, b.w, b.h, b.z),
        (VisualType::Card, 10, 20, 30, 40, 1)
    );
    let ids: Vec<&str> = page(&app).visuals.iter().map(|v| v.id.as_str()).collect();
    assert!(ids.is_sorted(), "kept sorted by id");
    assert_eq!(
        page(&app).draw_order().last().unwrap().id,
        b.id,
        "new on top"
    );

    run(
        &mut app,
        "visual.move",
        json!({"page": p, "visual": a, "x": 100, "y": 50}),
    );
    run(
        &mut app,
        "visual.resize",
        json!({"page": p, "visual": a, "w": 640, "h": 360}),
    );
    let v = visual(&app, &a);
    assert_eq!((v.x, v.y, v.w, v.h), (100, 50, 640, 360));
    app.edit_noting(|doc| {
        let page = doc.report.pages.first_mut().unwrap();
        page.selections.insert(b.id.clone(), vec![json!("North")]);
        Ok(None)
    })
    .unwrap();
    run(
        &mut app,
        "visual.delete",
        json!({"page": p, "visual": b.id}),
    );
    assert_eq!(page(&app).visuals.len(), 1);
    assert!(
        page(&app).selections.is_empty(),
        "its selection goes with it"
    );
    undo_redo_round_trips(&mut app, 5);
    for _ in 0..6 {
        run(&mut app, "edit.undo", Value::Null);
    }
    assert!(app.document().report.pages.is_empty());
}

#[test]
fn bad_visual_params_are_errors() {
    let mut app = app();
    refused(&mut app, "insert.newVisual", json!({}));
    assert!(refused(&mut app, "insert.newVisual", json!({"type": "pie"})).contains("clusteredBar"));
    refused(
        &mut app,
        "insert.newVisual",
        json!({"type": "card", "page": "Nope"}),
    );
    for (k, n) in [("x", -1), ("y", MAX_COORD + 1), ("w", 0), ("h", -5)] {
        refused(&mut app, "insert.newVisual", json!({"type": "card", k: n}));
    }
    refused(
        &mut app,
        "insert.newVisual",
        json!({"type": "card", "x": 1.5}),
    );
    refused(
        &mut app,
        "insert.newVisual",
        json!({"type": "card", "w": i64::MAX}),
    );
    let v = new_visual(&mut app, "card");
    let p = page(&app).id.clone();
    refused(&mut app, "visual.move", json!({"page": p, "visual": v}));
    refused(
        &mut app,
        "visual.move",
        json!({"page": p, "visual": v, "x": 1}),
    );
    refused(
        &mut app,
        "visual.move",
        json!({"page": p, "visual": "nope", "x": 1, "y": 1}),
    );
    refused(
        &mut app,
        "visual.resize",
        json!({"page": p, "visual": v, "w": 0, "h": 1}),
    );
    refused(&mut app, "visual.delete", json!({"page": p}));
    refused(
        &mut app,
        "visual.changeType",
        json!({"page": p, "visual": v}),
    );
}

#[test]
fn fields_are_checked_against_the_model_and_wells() {
    let mut app = app();
    let t = new_visual(&mut app, "table");
    let p = page(&app).id.clone();
    let add = |app: &mut App, well: &str, field: Value| {
        refused(
            app,
            "visual.addField",
            json!({"page": p, "visual": t, "well": well, "field": field}),
        )
    };
    assert!(
        add(
            &mut app,
            "values",
            json!({"table": "Sales", "column": "Nope"})
        )
        .contains("no column")
    );
    assert!(add(&mut app, "values", json!({"measure": "Nope"})).contains("no measure"));
    assert!(add(&mut app, "axis", json!({"measure": "Total"})).contains("no axis well"));
    add(&mut app, "values", json!({"table": "Sales"}));
    add(&mut app, "values", json!("Sales[Qty]"));
    add(&mut app, "values", json!({"measure": "Total", "x": 1}));
    add(&mut app, "nope", json!({"measure": "Total"}));
    refused(
        &mut app,
        "visual.addField",
        json!({"page": p, "visual": t, "well": "values", "field": {"measure": "Total"}, "index": 5}),
    );

    // Names are matched ignoring case and stored as the model spells them.
    add_field(
        &mut app,
        &t,
        "values",
        json!({"table": "sales", "column": "REGION"}),
    );
    add_field(&mut app, &t, "values", json!({"measure": "total"}));
    // The same Column twice in Values is allowed (e.g. Sum and Count of it).
    add_field(
        &mut app,
        &t,
        "values",
        json!({"table": "Sales", "column": "Region"}),
    );
    run(
        &mut app,
        "visual.addField",
        json!({"page": p, "visual": t, "well": "values", "field": {"measure": "Count"}, "index": 0}),
    );
    assert_eq!(
        fields(&app, &t, Well::Values),
        [
            measure("Count"),
            col("Sales", "Region"),
            measure("Total"),
            col("Sales", "Region")
        ]
    );

    // Only Values takes a Measure; a one-field well has its Field replaced.
    let s = new_visual(&mut app, "slicer");
    let add_s = |app: &mut App, field: Value| {
        refused(
            app,
            "visual.addField",
            json!({"page": p, "visual": s, "well": "field", "field": field}),
        )
    };
    assert!(add_s(&mut app, json!({"measure": "Total"})).contains("not measures"));
    add_field(
        &mut app,
        &s,
        "field",
        json!({"table": "Sales", "column": "Region"}),
    );
    add_field(
        &mut app,
        &s,
        "field",
        json!({"table": "Regions", "column": "Manager"}),
    );
    assert_eq!(fields(&app, &s, Well::Field), [col("Regions", "Manager")]);
    let c = new_visual(&mut app, "card");
    add_field(&mut app, &c, "values", json!({"measure": "Total"}));
    add_field(&mut app, &c, "values", json!({"measure": "Count"}));
    assert_eq!(fields(&app, &c, Well::Values), [measure("Count")]);

    // An Axis takes a Column once.
    let b = new_visual(&mut app, "clusteredBar");
    add_field(
        &mut app,
        &b,
        "axis",
        json!({"table": "Sales", "column": "Region"}),
    );
    let again = refused(
        &mut app,
        "visual.addField",
        json!({"page": p, "visual": b, "well": "axis", "field": {"table": "Sales", "column": "Region"}}),
    );
    assert!(again.contains("already"), "{again}");
}

#[test]
fn a_chart_takes_a_legend_or_several_values() {
    let mut app = app();
    let v = new_visual(&mut app, "clusteredColumn");
    add_field(
        &mut app,
        &v,
        "axis",
        json!({"table": "Sales", "column": "Product"}),
    );
    add_field(&mut app, &v, "values", json!({"measure": "Total"}));
    add_field(&mut app, &v, "values", json!({"measure": "Count"}));
    // A Legend keeps only the first Value.
    add_field(
        &mut app,
        &v,
        "legend",
        json!({"table": "Sales", "column": "Region"}),
    );
    assert_eq!(fields(&app, &v, Well::Values), [measure("Total")]);
    assert_eq!(fields(&app, &v, Well::Legend), [col("Sales", "Region")]);
    // A second Value removes the Legend.
    add_field(&mut app, &v, "values", json!({"measure": "Count"}));
    assert_eq!(
        fields(&app, &v, Well::Values),
        [measure("Total"), measure("Count")]
    );
    assert!(fields(&app, &v, Well::Legend).is_empty());
    assert!(
        !visual(&app, &v).wells.contains_key(&Well::Legend),
        "no empty wells"
    );
    // Undo brings the Legend back.
    run(&mut app, "edit.undo", Value::Null);
    assert_eq!(fields(&app, &v, Well::Legend), [col("Sales", "Region")]);
    // Not a chart: a matrix takes both.
    let m = new_visual(&mut app, "matrix");
    add_field(
        &mut app,
        &m,
        "rows",
        json!({"table": "Sales", "column": "Region"}),
    );
    add_field(&mut app, &m, "values", json!({"measure": "Total"}));
    add_field(&mut app, &m, "values", json!({"measure": "Count"}));
    assert_eq!(fields(&app, &m, Well::Values).len(), 2);
}

#[test]
fn change_type_keeps_the_fields_whose_wells_exist() {
    let mut app = app();
    let v = new_visual(&mut app, "matrix");
    let p = page(&app).id.clone();
    add_field(
        &mut app,
        &v,
        "rows",
        json!({"table": "Sales", "column": "Region"}),
    );
    add_field(
        &mut app,
        &v,
        "columns",
        json!({"table": "Sales", "column": "Product"}),
    );
    add_field(&mut app, &v, "values", json!({"measure": "Total"}));
    add_field(&mut app, &v, "values", json!({"measure": "Count"}));
    run(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": {"table": "Sales", "column": "Region"}}),
    );
    let change = |app: &mut App, t: &str| {
        run(
            app,
            "visual.changeType",
            json!({"page": p, "visual": v, "type": t}),
        );
    };
    change(&mut app, "clusteredColumn");
    let x = visual(&app, &v);
    assert_eq!(x.kind, VisualType::ClusteredColumn);
    assert_eq!(x.wells.keys().copied().collect::<Vec<_>>(), [Well::Values]);
    assert_eq!(
        fields(&app, &v, Well::Values),
        [measure("Total"), measure("Count")]
    );
    assert_eq!(x.sort, None, "the sort's Field is gone");
    change(&mut app, "card");
    assert_eq!(
        fields(&app, &v, Well::Values),
        [measure("Total")],
        "a card takes one"
    );
    change(&mut app, "slicer");
    assert!(visual(&app, &v).wells.is_empty());
    run(&mut app, "edit.undo", Value::Null);
    run(&mut app, "edit.undo", Value::Null);
    run(&mut app, "edit.undo", Value::Null);
    assert_eq!(visual(&app, &v).kind, VisualType::Matrix);
    assert_eq!(fields(&app, &v, Well::Columns), [col("Sales", "Product")]);
    assert!(visual(&app, &v).sort.is_some());
}

#[test]
fn fields_are_removed_and_moved() {
    let mut app = app();
    let v = new_visual(&mut app, "clusteredBar");
    let p = page(&app).id.clone();
    add_field(
        &mut app,
        &v,
        "axis",
        json!({"table": "Sales", "column": "Region"}),
    );
    add_field(
        &mut app,
        &v,
        "axis",
        json!({"table": "Sales", "column": "Product"}),
    );
    add_field(&mut app, &v, "values", json!({"measure": "Total"}));
    run(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": {"measure": "Total"}}),
    );
    // Within a well.
    run(
        &mut app,
        "visual.moveField",
        json!({"page": p, "visual": v, "well": "axis", "index": 1, "toIndex": 0}),
    );
    assert_eq!(
        fields(&app, &v, Well::Axis),
        [col("Sales", "Product"), col("Sales", "Region")]
    );
    // To another well: Axis → Legend.
    run(
        &mut app,
        "visual.moveField",
        json!({"page": p, "visual": v, "well": "axis", "index": 1, "toWell": "legend"}),
    );
    assert_eq!(fields(&app, &v, Well::Axis), [col("Sales", "Product")]);
    assert_eq!(fields(&app, &v, Well::Legend), [col("Sales", "Region")]);
    // A Measure can't move to the Axis; the failure changes nothing.
    refused(
        &mut app,
        "visual.moveField",
        json!({"page": p, "visual": v, "well": "values", "index": 0, "toWell": "axis"}),
    );
    refused(
        &mut app,
        "visual.moveField",
        json!({"page": p, "visual": v, "well": "axis", "index": 3}),
    );
    refused(
        &mut app,
        "visual.moveField",
        json!({"page": p, "visual": v, "well": "rows", "index": 0}),
    );
    // The sorted Field moved within its well keeps the sort.
    assert_eq!(
        visual(&app, &v).sort.as_ref().unwrap().field,
        measure("Total")
    );

    run(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "legend", "index": 0}),
    );
    assert!(!visual(&app, &v).wells.contains_key(&Well::Legend));
    run(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "values", "field": {"measure": "Total"}}),
    );
    assert_eq!(
        visual(&app, &v).sort,
        None,
        "removing the sorted Field drops the sort"
    );
    refused(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "values", "index": 0}),
    );
    refused(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "axis", "index": 1}),
    );
    refused(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "axis"}),
    );
    refused(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "axis", "field": {"measure": "Total"}}),
    );
    undo_redo_round_trips(&mut app, 4);
}

#[test]
fn sort_and_format() {
    let mut app = app();
    let v = new_visual(&mut app, "clusteredColumn");
    let p = page(&app).id.clone();
    add_field(
        &mut app,
        &v,
        "axis",
        json!({"table": "Sales", "column": "Region"}),
    );
    add_field(&mut app, &v, "values", json!({"measure": "Total"}));
    run(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": {"table": "Sales", "column": "Region"}, "direction": "ascending"}),
    );
    assert_eq!(
        visual(&app, &v).sort,
        Some(Sort {
            field: col("Sales", "Region"),
            direction: SortDirection::Ascending
        })
    );
    refused(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": {"measure": "Count"}}),
    );
    refused(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": {"measure": "Total"}, "direction": "up"}),
    );
    refused(&mut app, "visual.sort", json!({"page": p, "visual": v}));
    run(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": null}),
    );
    assert_eq!(visual(&app, &v).sort, None);

    let fmt = |app: &mut App, option: &str, value: Value| {
        run(
            app,
            "visual.format",
            json!({"page": p, "visual": v, "option": option, "value": value}),
        );
    };
    fmt(&mut app, "title", json!("Sales by region"));
    fmt(&mut app, "dataLabels", json!(true));
    fmt(&mut app, "legendPosition", json!("right"));
    fmt(&mut app, "legend", json!(false));
    fmt(&mut app, "showTitle", json!(false));
    let f = &visual(&app, &v).format;
    assert_eq!(f.title.as_deref(), Some("Sales by region"));
    assert!(f.data_labels && !f.legend && !f.show_title);
    assert_eq!(f.legend_position, LegendPosition::Right);
    fmt(&mut app, "title", Value::Null);
    assert_eq!(visual(&app, &v).format.title, None);
    let bad = |app: &mut App, option: &str, value: Value| {
        refused(
            app,
            "visual.format",
            json!({"page": p, "visual": v, "option": option, "value": value}),
        )
    };
    assert!(bad(&mut app, "rowSubtotals", json!(false)).contains("no `rowSubtotals`"));
    bad(&mut app, "slicerStyle", json!("dropdown"));
    bad(&mut app, "legend", json!("yes"));
    bad(&mut app, "legendPosition", json!("middle"));
    bad(&mut app, "title", json!(3));
    bad(&mut app, "nope", json!(true));
    refused(&mut app, "visual.format", json!({"page": p, "visual": v}));

    let m = new_visual(&mut app, "matrix");
    run(
        &mut app,
        "visual.format",
        json!({"page": p, "visual": m, "option": "rowSubtotals", "value": false}),
    );
    assert!(!visual(&app, &m).format.row_subtotals);
    let s = new_visual(&mut app, "slicer");
    run(
        &mut app,
        "visual.format",
        json!({"page": p, "visual": s, "option": "slicerStyle", "value": "dropdown"}),
    );
    assert_eq!(visual(&app, &s).format.slicer_style, SlicerStyle::Dropdown);
}

/// A Visual using Sales[Region], Regions[Manager] and Total, sorted by Total.
fn report_app() -> (App, String) {
    let mut app = app();
    let v = new_visual(&mut app, "matrix");
    let p = page(&app).id.clone();
    add_field(
        &mut app,
        &v,
        "rows",
        json!({"table": "Sales", "column": "Region"}),
    );
    add_field(
        &mut app,
        &v,
        "columns",
        json!({"table": "Regions", "column": "Manager"}),
    );
    add_field(&mut app, &v, "values", json!({"measure": "Total"}));
    add_field(
        &mut app,
        &v,
        "values",
        json!({"table": "Sales", "column": "Qty"}),
    );
    run(
        &mut app,
        "visual.sort",
        json!({"page": p, "visual": v, "field": {"table": "Sales", "column": "Region"}}),
    );
    (app, v)
}

#[test]
fn renaming_a_table_rewrites_its_fields_in_the_same_undo_step() {
    for (id, key) in [("modeling.renameTable", "table"), ("query.rename", "query")] {
        let (mut app, v) = report_app();
        run(&mut app, id, json!({key: "Sales", "name": "Orders"}));
        assert_eq!(fields(&app, &v, Well::Rows), [col("Orders", "Region")]);
        assert_eq!(fields(&app, &v, Well::Columns), [col("Regions", "Manager")]);
        assert_eq!(
            fields(&app, &v, Well::Values),
            [measure("Total"), col("Orders", "Qty")]
        );
        assert_eq!(
            visual(&app, &v).sort.as_ref().unwrap().field,
            col("Orders", "Region")
        );
        assert!(app.document().broken_fields().is_empty());
        run(&mut app, "edit.undo", Value::Null);
        assert_eq!(
            fields(&app, &v, Well::Rows),
            [col("Sales", "Region")],
            "{id}"
        );
    }
}

#[test]
fn renaming_a_column_or_measure_rewrites_its_fields() {
    let (mut app, v) = report_app();
    run(
        &mut app,
        "modeling.renameColumn",
        json!({"table": "Sales", "column": "Region", "name": "Area"}),
    );
    assert_eq!(fields(&app, &v, Well::Rows), [col("Sales", "Area")]);
    assert_eq!(
        visual(&app, &v).sort.as_ref().unwrap().field,
        col("Sales", "Area")
    );
    // Regions[Region] is another column: untouched.
    run(
        &mut app,
        "modeling.renameColumn",
        json!({"table": "Regions", "column": "Manager", "name": "Boss"}),
    );
    assert_eq!(fields(&app, &v, Well::Columns), [col("Regions", "Boss")]);
    run(
        &mut app,
        "modeling.renameMeasure",
        json!({"table": "Sales", "measure": "Total", "name": "Sum Qty"}),
    );
    assert_eq!(
        fields(&app, &v, Well::Values),
        [measure("Sum Qty"), col("Sales", "Qty")]
    );
    assert!(app.document().broken_fields().is_empty());
    run(&mut app, "edit.undo", Value::Null);
    assert_eq!(
        fields(&app, &v, Well::Values),
        [measure("Total"), col("Sales", "Qty")]
    );
    undo_redo_round_trips(&mut app, 2);
}

#[test]
fn a_deleted_field_is_kept_and_flagged_broken() {
    let (mut app, v) = report_app();
    let p = page(&app).id.clone();
    run(
        &mut app,
        "modeling.delete",
        json!({"table": "Sales", "name": "Total"}),
    );
    assert_eq!(
        fields(&app, &v, Well::Values),
        [measure("Total"), col("Sales", "Qty")],
        "kept"
    );
    assert_eq!(
        app.document().broken_fields(),
        [BrokenField {
            page: p.clone(),
            visual: v.clone(),
            well: Well::Values,
            index: 0,
            field: measure("Total")
        }]
    );
    run(
        &mut app,
        "modeling.deleteTable",
        json!({"table": "Regions"}),
    );
    let broken: Vec<FieldRef> = app
        .document()
        .broken_fields()
        .into_iter()
        .map(|b| b.field)
        .collect();
    // In well order: Values before Columns.
    assert_eq!(broken, [measure("Total"), col("Regions", "Manager")]);
    run(&mut app, "edit.undo", Value::Null);
    run(&mut app, "edit.undo", Value::Null);
    assert!(app.document().broken_fields().is_empty());
    // A broken Field can't be added, but can be removed.
    run(
        &mut app,
        "modeling.delete",
        json!({"table": "Sales", "name": "Total"}),
    );
    refused(
        &mut app,
        "visual.addField",
        json!({"page": p, "visual": v, "well": "values", "field": {"measure": "Total"}}),
    );
    run(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": v, "well": "values", "index": 0}),
    );
    assert!(app.document().broken_fields().is_empty());
}

// ---- files ----

/// A fresh directory under the system temp dir, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("metriccraft-report-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Two Pages: the first with `report_app`'s matrix, a slicer with a saved selection and a card;
/// the second empty.
fn full_app() -> App {
    let (mut app, _) = report_app();
    let s = new_visual(&mut app, "slicer");
    add_field(
        &mut app,
        &s,
        "field",
        json!({"table": "Sales", "column": "Region"}),
    );
    let c = new_visual(&mut app, "card");
    add_field(&mut app, &c, "values", json!({"measure": "Count"}));
    let p = page(&app).id.clone();
    run(
        &mut app,
        "visual.format",
        json!({"page": p, "visual": c, "option": "title", "value": "Rows ✓"}),
    );
    run(&mut app, "insert.newPage", json!({"name": "Détail"}));
    app.edit_noting(|doc| {
        let page = doc.report.pages.first_mut().unwrap();
        page.selections.insert(
            s.clone(),
            vec![json!("North"), json!(3), json!(null), json!(2.5)],
        );
        Ok(None)
    })
    .unwrap();
    app
}

fn report_files(app: &App) -> BTreeMap<String, Vec<u8>> {
    let files = app.files(None).unwrap();
    files
        .into_iter()
        .filter(|(k, _)| k.starts_with("report/"))
        .collect()
}

fn text(files: &BTreeMap<String, Vec<u8>>, path: &str) -> String {
    String::from_utf8(files.get(path).unwrap().clone()).unwrap()
}

#[test]
fn the_report_files_follow_the_adr() {
    let app = full_app();
    let files = report_files(&app);
    let pages = &app.document().report.pages;
    let (one, two) = (&pages[0], &pages[1]);
    let mut want = vec![
        "report/report.json".to_string(),
        format!("report/pages/{}/page.json", one.id),
        format!("report/pages/{}/page.json", two.id),
    ];
    want.extend(
        one.visuals
            .iter()
            .map(|v| format!("report/pages/{}/visuals/{}.json", one.id, v.id)),
    );
    want.sort();
    assert_eq!(files.keys().cloned().collect::<Vec<_>>(), want);
    assert_eq!(
        text(&files, "report/report.json"),
        format!(
            "{{\n  \"pages\": [\n    \"{}\",\n    \"{}\"\n  ]\n}}\n",
            one.id, two.id
        )
    );
    let s = one
        .visuals
        .iter()
        .find(|v| v.kind == VisualType::Slicer)
        .unwrap();
    assert_eq!(
        text(&files, &format!("report/pages/{}/page.json", one.id)),
        format!(
            "{{\n  \"height\": 720,\n  \"name\": \"Page 1\",\n  \"selections\": {{\n    \"{}\": [\n      \"North\",\n      3,\n      null,\n      2.5\n    ]\n  }},\n  \"width\": 1280\n}}\n",
            s.id
        )
    );
    let m = one
        .visuals
        .iter()
        .find(|v| v.kind == VisualType::Matrix)
        .unwrap();
    assert_eq!(
        text(
            &files,
            &format!("report/pages/{}/visuals/{}.json", one.id, m.id)
        ),
        r#"{
  "format": {
    "columnSubtotals": true,
    "dataLabels": false,
    "legend": true,
    "legendPosition": "top",
    "rowSubtotals": true,
    "showTitle": true,
    "slicerStyle": "list",
    "title": null
  },
  "h": 300,
  "sort": {
    "direction": "descending",
    "field": {
      "column": "Region",
      "table": "Sales"
    }
  },
  "type": "matrix",
  "w": 400,
  "wells": {
    "columns": [
      {
        "column": "Manager",
        "table": "Regions"
      }
    ],
    "rows": [
      {
        "column": "Region",
        "table": "Sales"
      }
    ],
    "values": [
      {
        "measure": "Total"
      },
      {
        "column": "Qty",
        "table": "Sales"
      }
    ]
  },
  "x": 0,
  "y": 0,
  "z": 0
}
"#
    );
    // No report, no files.
    assert!(report_files(&self::app()).is_empty());
}

#[test]
fn save_twice_is_byte_identical_and_reopens_the_same_report() {
    let dir = Dir::new();
    let mut app = full_app();
    let path = dir.0.join("Proj").display().to_string();
    run(&mut app, "file.saveAs", json!({"path": path}));
    let read = || {
        metriccraft_file::read_folder(&dir.0.join("Proj"), metriccraft_file::Recover::No).unwrap()
    };
    let first = read();
    run(&mut app, "file.save", json!({}));
    assert_eq!(read(), first);

    let mut other = App::new();
    run(&mut other, "file.open", json!({"path": path}));
    assert_eq!(other.document().report, app.document().report);
    run(&mut other, "file.save", Value::Null);
    assert_eq!(read(), first, "open → save gives the same files");
}

#[test]
fn project_and_package_round_trip_the_report() {
    let dir = Dir::new();
    let mut app = full_app();
    let report = app.document().report.clone();
    let pkg = dir.0.join("R.mcraft").display().to_string();
    run(&mut app, "file.saveAs", json!({"path": pkg}));
    let mut opened = App::new();
    run(&mut opened, "file.open", json!({"path": pkg}));
    assert_eq!(opened.document().report, report);
    assert_eq!(page(&opened).selections.values().next().unwrap().len(), 4);
    // Package → Project → Package: the same bytes.
    let proj = dir.0.join("P").display().to_string();
    run(&mut opened, "file.saveAs", json!({"path": proj}));
    let mut again = App::new();
    run(&mut again, "file.open", json!({"path": proj}));
    assert_eq!(again.document().report, report);
    let pkg2 = dir.0.join("R2.mcraft").display().to_string();
    run(&mut again, "file.saveAs", json!({"path": pkg2}));
    assert_eq!(std::fs::read(&pkg2).unwrap(), std::fs::read(&pkg).unwrap());
    // And in memory, as the web build does.
    let mut web = App::new();
    web.open_package_bytes(&app.package_bytes().unwrap())
        .unwrap();
    assert_eq!(web.document().report, report);
}

/// `bytes` (a Package) with its report parts changed by `f`.
fn edit_package(bytes: &[u8], f: impl FnOnce(&mut BTreeMap<String, Vec<u8>>)) -> Vec<u8> {
    let files = metriccraft_file::unpack(bytes).unwrap();
    let (mut project, preserved) = metriccraft_file::decode(&files, None).unwrap();
    f(&mut project.report);
    let mut out = metriccraft_file::encode(&project, None, &preserved).unwrap();
    out.extend(files.into_iter().filter(|(k, _)| k.starts_with(".cache/")));
    metriccraft_file::pack(&out).unwrap()
}

fn json_part(parts: &mut BTreeMap<String, Vec<u8>>, path: &str, f: impl FnOnce(&mut Value)) {
    let mut v: Value = serde_json::from_slice(parts.get(path).unwrap()).unwrap();
    f(&mut v);
    parts.insert(path.into(), serde_json::to_vec(&v).unwrap());
}

#[test]
fn unknown_report_content_is_preserved() {
    let app = full_app();
    let one = page(&app).id.clone();
    let two = app.document().report.pages[1].id.clone();
    let m = page(&app)
        .visuals
        .iter()
        .find(|v| v.kind == VisualType::Matrix)
        .unwrap()
        .id
        .clone();
    let visual_path = format!("report/pages/{one}/visuals/{m}.json");
    let bytes = edit_package(&app.package_bytes().unwrap(), |parts| {
        json_part(parts, "report/report.json", |v| {
            v["theme"] = json!({"name": "Dusk"})
        });
        json_part(parts, &format!("report/pages/{one}/page.json"), |v| {
            v["background"] = json!("#fff");
        });
        json_part(parts, &visual_path, |v| {
            v["tooltips"] = json!(true);
            v["format"]["border"] = json!({"width": 2});
            v["wells"]["tooltips"] = json!([{"measure": "Count"}]);
            v["sort"]["nulls"] = json!("last");
        });
        parts.insert("report/static/logo.svg".into(), b"<svg/>".to_vec());
        parts.insert(format!("report/pages/{two}/notes.txt"), b"hi".to_vec());
        parts.insert(
            format!("report/pages/{one}/visuals/zz.json"),
            br#"{"type": "pie", "x": 1}"#.to_vec(),
        );
    });
    let mut opened = App::new();
    opened.open_package_bytes(&bytes).unwrap();
    assert_eq!(
        page(&opened).visuals.len(),
        page(&app).visuals.len(),
        "a pie (unknown type) isn't a Visual we show"
    );
    // A known edit still writes the unknown members back.
    let p = one.clone();
    run(
        &mut opened,
        "visual.format",
        json!({"page": p, "visual": m, "option": "rowSubtotals", "value": false}),
    );
    let files = report_files(&opened);
    assert!(
        text(&files, "report/report.json").contains("\"theme\": {\n    \"name\": \"Dusk\"\n  }")
    );
    assert!(
        text(&files, &format!("report/pages/{one}/page.json")).contains("\"background\": \"#fff\"")
    );
    let v = text(&files, &visual_path);
    for kept in [
        "\"tooltips\": true",
        "\"border\": {",
        "\"rowSubtotals\": false",
        "\"nulls\": \"last\"",
    ] {
        assert!(v.contains(kept), "{kept} in\n{v}");
    }
    let wells: Value = serde_json::from_str(&v).unwrap();
    assert_eq!(wells["wells"]["tooltips"], json!([{"measure": "Count"}]));
    assert_eq!(wells["wells"]["values"][0], json!({"measure": "Total"}));
    assert_eq!(text(&files, "report/static/logo.svg"), "<svg/>");
    assert_eq!(text(&files, &format!("report/pages/{two}/notes.txt")), "hi");
    assert_eq!(
        text(&files, &format!("report/pages/{one}/visuals/zz.json")),
        r#"{"type": "pie", "x": 1}"#
    );
    // Open → save is stable.
    let saved = opened.package_bytes().unwrap();
    let mut again = App::new();
    again.open_package_bytes(&saved).unwrap();
    assert_eq!(again.package_bytes().unwrap(), saved);

    // A deleted Page's folder goes, unknown files and all; undo brings it back.
    run(&mut opened, "page.delete", json!({"page": two}));
    assert!(!report_files(&opened).keys().any(|k| k.contains(&two)));
    run(&mut opened, "edit.undo", Value::Null);
    assert!(report_files(&opened).contains_key(&format!("report/pages/{two}/notes.txt")));
    // A deleted Visual's unknown members go with it, and come back on undo.
    run(
        &mut opened,
        "visual.delete",
        json!({"page": p, "visual": m}),
    );
    assert!(!report_files(&opened).contains_key(&visual_path));
    run(&mut opened, "edit.undo", Value::Null);
    assert!(text(&report_files(&opened), &visual_path).contains("\"tooltips\": true"));
}

#[test]
fn malformed_report_files_are_errors_naming_the_file() {
    let app = full_app();
    let one = page(&app).id.clone();
    let m = page(&app)
        .visuals
        .iter()
        .find(|v| v.kind == VisualType::Matrix)
        .unwrap()
        .id
        .clone();
    let visual_path = format!("report/pages/{one}/visuals/{m}.json");
    let page_path = format!("report/pages/{one}/page.json");
    let good = app.package_bytes().unwrap();
    type Edit = Box<dyn Fn(&mut Value)>;
    let cases: Vec<(&str, Edit)> = vec![
        ("report/report.json", Box::new(|v| v["pages"] = json!("x"))),
        ("report/report.json", Box::new(|v| v["pages"] = json!([1]))),
        ("report/report.json", Box::new(|v| *v = json!([]))),
        (&page_path, Box::new(|v| v["name"] = json!(null))),
        (&page_path, Box::new(|v| v["width"] = json!(0))),
        (&page_path, Box::new(|v| v["height"] = json!(1e300))),
        (
            &page_path,
            Box::new(|v| v["selections"] = json!({"a": "North"})),
        ),
        // Hostile sizes: a slicer selection beyond `slicer.select`'s cap, a well beyond its
        // limit (each would be re-done on every draw).
        (
            &page_path,
            Box::new(|v| {
                let items = vec![json!("North"); crate::MAX_SELECTED + 1];
                v["selections"] = json!({ "a": items });
            }),
        ),
        (
            &visual_path,
            Box::new(|v| {
                let values = vec![json!({"measure": "Total"}); MAX_VALUE_FIELDS + 1];
                v["wells"]["values"] = json!(values);
                v["sort"] = json!(null);
            }),
        ),
        (
            &visual_path,
            Box::new(|v| {
                let rows = vec![json!({"table": "Sales", "column": "Region"}); MAX_AXIS_FIELDS + 1];
                v["wells"]["rows"] = json!(rows);
                v["sort"] = json!(null);
            }),
        ),
        (&visual_path, Box::new(|v| v["type"] = json!(3))),
        (&visual_path, Box::new(|v| v["x"] = json!(-1))),
        (&visual_path, Box::new(|v| v["z"] = json!("top"))),
        (
            &visual_path,
            Box::new(|v| v["wells"]["values"] = json!([{"table": "Sales"}])),
        ),
        (
            &visual_path,
            Box::new(|v| v["wells"]["values"] = json!({"measure": "Total"})),
        ),
        (
            &visual_path,
            Box::new(|v| v["wells"]["axis"] = json!([{"measure": "Total"}])),
        ),
        (
            &visual_path,
            Box::new(|v| {
                v["sort"] = json!({"field": {"measure": "Nope"}, "direction": "ascending"})
            }),
        ),
        (
            &visual_path,
            Box::new(|v| v["sort"]["direction"] = json!("up")),
        ),
        (
            &visual_path,
            Box::new(|v| v["format"]["legend"] = json!("no")),
        ),
        (
            &visual_path,
            Box::new(|v| v["format"]["legendPosition"] = json!("middle")),
        ),
        (&visual_path, Box::new(|v| v["format"] = json!(7))),
        (
            &visual_path,
            Box::new(|v| {
                v["type"] = json!("clusteredBar");
                v["wells"] = json!({"legend": [{"table": "Sales", "column": "Region"}],
                "values": [{"measure": "Total"}, {"measure": "Count"}]});
                v["sort"] = json!(null);
            }),
        ),
        (
            &visual_path,
            Box::new(|v| {
                v["type"] = json!("card");
                v["wells"] = json!({"values": [{"measure": "Total"}, {"measure": "Count"}]});
                v["sort"] = json!(null);
            }),
        ),
    ];
    for (path, edit) in &cases {
        let bytes = edit_package(&good, |parts| json_part(parts, path, |v| edit(v)));
        let mut opened = App::new();
        match opened.open_package_bytes(&bytes) {
            Err(Error::File(metriccraft_file::Error::Part { path: p, .. })) => assert_eq!(&p, path),
            other => panic!("{path}: expected a Part error, got {other:?}"),
        }
        assert!(
            opened.document().report.pages.is_empty(),
            "a failed open changes nothing"
        );
    }
    // Not JSON at all.
    for path in [
        "report/report.json",
        page_path.as_str(),
        visual_path.as_str(),
    ] {
        let bytes = edit_package(&good, |parts| {
            parts.insert(path.into(), b"{\"pages\": [".to_vec());
        });
        let err = App::new().open_package_bytes(&bytes).unwrap_err();
        assert!(err.to_string().starts_with(path), "{err}");
    }
}

#[test]
fn reading_is_tolerant_of_page_lists() {
    let app = full_app();
    let [one, two] = [0, 1].map(|i| app.document().report.pages[i].id.clone());
    let good = app.package_bytes().unwrap();
    // Unlisted Pages come after the listed ones; listed ids without a folder are skipped.
    let bytes = edit_package(&good, |parts| {
        json_part(parts, "report/report.json", |v| {
            v["pages"] = json!(["gone", two, two])
        });
    });
    let mut opened = App::new();
    opened.open_package_bytes(&bytes).unwrap();
    let ids: Vec<&str> = opened
        .document()
        .report
        .pages
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    assert_eq!(ids, [two.as_str(), one.as_str()]);
    // A Visual file in a folder with no page.json is kept verbatim, and stays.
    let bytes = edit_package(&good, |parts| {
        parts.insert("report/pages/orphan/visuals/a.json".into(), b"{}".to_vec());
    });
    let mut opened = App::new();
    opened.open_package_bytes(&bytes).unwrap();
    assert_eq!(opened.document().report, app.document().report);
    assert!(report_files(&opened).contains_key("report/pages/orphan/visuals/a.json"));
}

#[test]
fn a_full_well_refuses_another_field() {
    let mut app = app();
    let m = new_visual(&mut app, "matrix");
    for _ in 0..MAX_VALUE_FIELDS {
        add_field(&mut app, &m, "values", json!({"measure": "Total"}));
    }
    let p = page(&app).id.clone();
    let msg = refused(
        &mut app,
        "visual.addField",
        json!({"page": p, "visual": m, "well": "values", "field": {"measure": "Count"}}),
    );
    assert!(msg.contains("at most"), "{msg}");
    assert_eq!(fields(&app, &m, Well::Values).len(), MAX_VALUE_FIELDS);
}

/// A slicer's saved items are values of its Column: changing the Column, or the type, drops
/// them in the same undoable edit (else every other Visual is filtered by Product = "North").
#[test]
fn a_slicer_field_or_type_change_drops_its_selection() {
    let mut app = app();
    let s = new_visual(&mut app, "slicer");
    add_field(
        &mut app,
        &s,
        "field",
        json!({"table": "Sales", "column": "Region"}),
    );
    let p = page(&app).id.clone();
    run(
        &mut app,
        "slicer.select",
        json!({"page": p, "visual": s, "items": ["North"]}),
    );
    let selected = |app: &App| page(app).selections.contains_key(&s);
    // Replacing the Field.
    add_field(
        &mut app,
        &s,
        "field",
        json!({"table": "Sales", "column": "Product"}),
    );
    assert!(!selected(&app));
    run(&mut app, "edit.undo", Value::Null);
    assert!(selected(&app), "undo brings it back with the old Field");
    // Moving it within its well keeps it; removing it drops it.
    run(
        &mut app,
        "visual.moveField",
        json!({"page": p, "visual": s, "well": "field", "index": 0}),
    );
    assert!(selected(&app));
    run(
        &mut app,
        "visual.removeField",
        json!({"page": p, "visual": s, "well": "field", "index": 0}),
    );
    assert!(!selected(&app));
    run(&mut app, "edit.undo", Value::Null);
    // Slicer -> table -> slicer doesn't bring it back.
    for kind in ["table", "slicer"] {
        run(
            &mut app,
            "visual.changeType",
            json!({"page": p, "visual": s, "type": kind}),
        );
        assert!(!selected(&app), "{kind}");
    }
    run(&mut app, "edit.undo", Value::Null);
    run(&mut app, "edit.undo", Value::Null);
    assert!(selected(&app));
    // Other edits keep it.
    run(
        &mut app,
        "visual.format",
        json!({"page": p, "visual": s, "option": "showTitle", "value": false}),
    );
    assert!(selected(&app));
}

#[test]
fn fresh_reports_make_different_ids() {
    let (a, b) = (Report::default(), Report::default());
    assert_ne!(a.new_id(), b.new_id());
    let mut seen = std::collections::HashSet::new();
    for _ in 0..1_000 {
        assert!(seen.insert(a.new_id()));
    }
    seed_ids(b"host entropy");
    assert_ne!(a.new_id(), b.new_id());
}

// ---- UI state in the Project (M5.0) ----

#[test]
fn current_page_is_saved_survives_undo_and_does_not_dirty() {
    let dir = Dir::new();
    let mut app = full_app();
    let second = app.document().report.pages[1].id.clone();
    let path = dir.0.join("Proj").display().to_string();
    run(&mut app, "file.saveAs", json!({"path": path}));
    run(&mut app, "page.setCurrent", json!({"page": second}));
    assert!(!app.is_dirty(), "switching Pages alone is not a change");
    assert!(!app.can_undo() || app.document().report.current.is_some());

    // An edit, then undo: the current Page stays.
    run(&mut app, "insert.newPage", json!({"name": "Third"}));
    run(&mut app, "edit.undo", json!({}));
    assert_eq!(
        app.document().report.current.as_deref(),
        Some(second.as_str())
    );

    run(&mut app, "file.save", json!({}));
    let json = std::fs::read_to_string(dir.0.join("Proj/report/report.json")).unwrap();
    assert!(
        json.contains(&format!("\"current\": \"{second}\"")),
        "{json}"
    );
    let mut other = App::new();
    run(&mut other, "file.open", json!({"path": path}));
    assert_eq!(
        other.document().report.current.as_deref(),
        Some(second.as_str())
    );

    // A deleted current Page isn't written; an unknown id reads as no current Page.
    run(&mut other, "page.delete", json!({"page": second}));
    run(&mut other, "file.save", json!({}));
    let json = std::fs::read_to_string(dir.0.join("Proj/report/report.json")).unwrap();
    assert!(!json.contains("current"), "{json}");
    let e = app
        .run("page.setCurrent", &json!({"page": "nope"}))
        .unwrap_err();
    assert!(matches!(e, Error::InvalidParams { .. }), "{e:?}");
}

#[test]
fn table_positions_are_saved_renamed_and_merged_for_nudges() {
    let dir = Dir::new();
    let mut app = self::app();
    run(
        &mut app,
        "model.moveTable",
        json!({"table": "Sales", "x": 10, "y": 20}),
    );
    // The first nudge starts its own step; the ones after it join it.
    run(
        &mut app,
        "model.moveTable",
        json!({"table": "sales", "x": 11, "y": 20, "merge": false}),
    );
    run(
        &mut app,
        "model.moveTable",
        json!({"table": "Sales", "x": 12, "y": 20, "merge": true}),
    );
    assert_eq!(app.document().diagram.get("Sales"), Some(&(12, 20)));
    run(&mut app, "edit.undo", json!({}));
    assert_eq!(
        app.document().diagram.get("Sales"),
        Some(&(10, 20)),
        "nudges are one step"
    );
    run(&mut app, "edit.undo", json!({}));
    assert!(app.document().diagram.is_empty());
    run(&mut app, "edit.redo", json!({}));

    // Without merge (a drag), each move is its own step.
    run(
        &mut app,
        "model.moveTable",
        json!({"table": "Regions", "x": 300, "y": 0}),
    );
    run(
        &mut app,
        "model.moveTable",
        json!({"table": "Regions", "x": 310, "y": 0}),
    );
    run(&mut app, "edit.undo", json!({}));
    assert_eq!(app.document().diagram.get("Regions"), Some(&(300, 0)));

    run(
        &mut app,
        "modeling.renameTable",
        json!({"table": "Sales", "name": "Orders"}),
    );
    assert_eq!(app.document().diagram.get("Orders"), Some(&(10, 20)));

    let path = dir.0.join("Proj").display().to_string();
    run(&mut app, "file.saveAs", json!({"path": path}));
    let bytes = std::fs::read_to_string(dir.0.join("Proj/model/diagram.json")).unwrap();
    assert_eq!(
        bytes,
        "{\n  \"tables\": {\n    \"Orders\": {\n      \"x\": 10,\n      \"y\": 20\n    },\n    \
         \"Regions\": {\n      \"x\": 300,\n      \"y\": 0\n    }\n  }\n}\n"
    );
    let mut other = App::new();
    run(&mut other, "file.open", json!({"path": path}));
    assert_eq!(other.document().diagram, app.document().diagram);

    for bad in [
        json!({"table": "Nope", "x": 0, "y": 0}),
        json!({"table": "Orders", "x": -1, "y": 0}),
    ] {
        assert!(app.run("model.moveTable", &bad).is_err(), "{bad}");
    }
}

#[test]
fn nudges_of_a_visual_merge_into_one_undo_step() {
    let (mut app, v) = report_app();
    let page = app.document().report.pages[0].id.clone();
    let mv =
        |x: i32, merge: bool| json!({"page": page, "visual": v, "x": x, "y": 0, "merge": merge});
    run(&mut app, "visual.move", mv(5, false));
    run(&mut app, "visual.move", mv(6, false));
    run(&mut app, "visual.move", mv(7, true));
    run(&mut app, "edit.undo", json!({}));
    let x = app.document().report.pages[0]
        .visuals
        .iter()
        .find(|x| x.id == v)
        .map(|x| x.x);
    assert_eq!(x, Some(5));
    run(&mut app, "edit.undo", json!({}));
    let x = app.document().report.pages[0]
        .visuals
        .iter()
        .find(|x| x.id == v)
        .map(|x| x.x);
    assert_eq!(x, Some(0));
}
