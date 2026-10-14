//! M4.5 tests: slicers and data-point Selection cross-filtering a small loaded Page.

use opendash_data::Query;
use opendash_data::culture::Culture;
use opendash_data::source::{Bytes, File, Source, csv};
use opendash_model::ModelTable;
use opendash_visuals::{Cmd, Theme};

use super::*;
use crate::Error;

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

/// Totals: Pen 5, Ink 9; North 8, South 4, East 2; all 14.
const SALES: &str = "Region,Product,Qty\nNorth,Pen,3\nSouth,Ink,4\nNorth,Ink,5\nEast,Pen,2\n";

fn run(app: &mut App, id: &str, params: Value) {
    if let Err(e) = app.run(id, &params) {
        panic!("{id} {params}: {e}");
    }
}

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

/// The Page's ids: slicer, card, bar, table.
struct Ids {
    page: String,
    slicer: String,
    card: String,
    bar: String,
    table: String,
}

fn new_visual(app: &mut App, kind: &str, x: i32, wells: &[(&str, Value)]) -> String {
    let before: Vec<String> = app
        .document()
        .report
        .pages
        .first()
        .map(|p| p.visuals.iter().map(|v| v.id.clone()).collect())
        .unwrap_or_default();
    run(app, "insert.newVisual", json!({"type": kind, "x": x}));
    let page = app.document().report.pages.first().unwrap();
    let id = page
        .visuals
        .iter()
        .find(|v| !before.contains(&v.id))
        .unwrap()
        .id
        .clone();
    let p = page.id.clone();
    for (well, field) in wells {
        run(
            app,
            "visual.addField",
            json!({"page": p, "visual": id, "well": well, "field": field}),
        );
    }
    id
}

fn col(c: &str) -> Value {
    json!({"table": "Sales", "column": c})
}

fn setup() -> (App, Ids) {
    let mut doc = Document::default();
    add_table(&mut doc, "Sales", SALES);
    let mut app = App::with_document(doc);
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "Total", "expression": "SUM ( Sales[Qty] )"}),
    );
    let slicer = new_visual(&mut app, "slicer", 0, &[("field", col("Region"))]);
    let card = new_visual(
        &mut app,
        "card",
        400,
        &[("values", json!({"measure": "Total"}))],
    );
    let bar = new_visual(
        &mut app,
        "clusteredBar",
        800,
        &[
            ("axis", col("Product")),
            ("values", json!({"measure": "Total"})),
        ],
    );
    let table = new_visual(
        &mut app,
        "table",
        1200,
        &[("values", col("Region")), ("values", col("Product"))],
    );
    let page = app.document().report.pages.first().unwrap().id.clone();
    (
        app,
        Ids {
            page,
            slicer,
            card,
            bar,
            table,
        },
    )
}

fn draw(app: &mut App, page: &str) -> Vec<(String, Scene)> {
    let th = Theme::light();
    let cx = Context {
        theme: &th,
        locale: &opendash_core::EN_GB,
    };
    app.page_scene(page, &cx).unwrap()
}

fn scene<'a>(scenes: &'a [(String, Scene)], id: &str) -> &'a Scene {
    &scenes.iter().find(|(v, _)| v == id).unwrap().1
}

fn card(app: &App, ids: &Ids) -> String {
    let r = app.visual_result(&ids.page, &ids.card).unwrap();
    r.rows[0].cells[0].to_text()
}

/// The first key of each row of `visual`'s last result.
fn keys(app: &App, ids: &Ids, visual: &str) -> Vec<String> {
    let r = app.visual_result(&ids.page, visual).unwrap();
    r.rows
        .iter()
        .map(|row| {
            row.keys
                .iter()
                .map(|k| k.as_ref().map(Datum::to_text).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect()
}

/// The row of `visual`'s result whose first key is `key`.
fn row_of(app: &App, ids: &Ids, visual: &str, key: &str) -> u32 {
    let r = app.visual_result(&ids.page, visual).unwrap();
    let i = r
        .rows
        .iter()
        .position(|row| {
            row.keys.first().cloned().flatten().map(|k| k.to_text()) == Some(key.into())
        })
        .unwrap();
    u32::try_from(i).unwrap()
}

/// The centre and fill alpha of the filled Rect tagged with row `point` in `s`.
fn mark(s: &Scene, point: u32) -> (f32, f32, u8) {
    s.cmds
        .iter()
        .find_map(|c| match c {
            Cmd::Rect {
                x,
                y,
                w,
                h,
                fill: Some(f),
                tag: Some(t),
                ..
            } if t.point == point => Some((x + w / 2.0, y + h / 2.0, f.a)),
            _ => None,
        })
        .unwrap()
}

/// Clicks the bar of `product`.
fn click_bar(app: &mut App, ids: &Ids, product: &str, add: bool) {
    let scenes = draw(app, &ids.page);
    let (x, y, _) = mark(
        scene(&scenes, &ids.bar),
        row_of(app, ids, &ids.bar, product),
    );
    app.click(&ids.page, &ids.bar, x, y, add).unwrap();
}

/// Clicks the slicer item `item`.
fn click_item(app: &mut App, ids: &Ids, item: &str, add: bool) {
    let scenes = draw(app, &ids.page);
    let (x, y, _) = mark(
        scene(&scenes, &ids.slicer),
        row_of(app, ids, &ids.slicer, item),
    );
    app.click(&ids.page, &ids.slicer, x, y, add).unwrap();
}

#[test]
fn slicer_selection_filters_the_card_and_is_undoable() {
    let (mut app, ids) = setup();
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "14");
    assert_eq!(keys(&app, &ids, &ids.slicer), ["East", "North", "South"]);
    run(
        &mut app,
        "slicer.select",
        json!({"page": ids.page, "visual": ids.slicer, "items": ["north"]}),
    );
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "8");
    assert_eq!(keys(&app, &ids, &ids.table), ["North/Ink", "North/Pen"]);
    // The slicer isn't filtered by itself.
    assert_eq!(keys(&app, &ids, &ids.slicer).len(), 3);
    // Ctrl-click adds an item; clicking the only selected item clears.
    click_item(&mut app, &ids, "South", true);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "12");
    click_item(&mut app, &ids, "South", false);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "4");
    click_item(&mut app, &ids, "South", false);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "14");
    let page = || -> &Page { app.document().report.pages.first().unwrap() };
    assert!(page().selections.is_empty());
    // Undo walks back through the slicer edits; redo forward.
    run(&mut app, "edit.undo", Value::Null);
    assert_eq!(
        app.document().report.pages[0].selections[&ids.slicer],
        [json!("South")]
    );
    run(&mut app, "edit.undo", Value::Null);
    run(&mut app, "edit.undo", Value::Null);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "8");
    run(&mut app, "edit.redo", Value::Null);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "12");
    // Empty items clear; the stored items are the canonical spelling.
    run(
        &mut app,
        "slicer.select",
        json!({"page": ids.page, "visual": ids.slicer, "items": []}),
    );
    assert!(app.document().report.pages[0].selections.is_empty());
}

#[test]
fn bar_click_filters_the_others_and_dims_itself() {
    let (mut app, ids) = setup();
    click_bar(&mut app, &ids, "Pen", false);
    let sel = app.selection(&ids.page).unwrap();
    assert_eq!(sel.visual, ids.bar);
    assert_eq!(sel.points.len(), 1);
    let scenes = draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "5");
    assert_eq!(keys(&app, &ids, &ids.table), ["East/Pen", "North/Pen"]);
    assert_eq!(keys(&app, &ids, &ids.slicer), ["East", "North"]);
    // The bar isn't filtered by its own Selection: both bars, Ink dimmed.
    assert_eq!(keys(&app, &ids, &ids.bar), ["Ink", "Pen"]);
    let bar = scene(&scenes, &ids.bar);
    let (_, _, pen) = mark(bar, row_of(&app, &ids, &ids.bar, "Pen"));
    let (_, _, ink) = mark(bar, row_of(&app, &ids, &ids.bar, "Ink"));
    assert_eq!(pen, 255);
    assert!(ink < 255, "{ink}");
    // Ctrl adds Ink; Ctrl on Ink again takes it out.
    click_bar(&mut app, &ids, "Ink", true);
    assert_eq!(app.selection(&ids.page).unwrap().points.len(), 2);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "14");
    click_bar(&mut app, &ids, "Ink", true);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "5");
    // Clicking the only selected point again clears.
    click_bar(&mut app, &ids, "Pen", false);
    assert!(app.selection(&ids.page).is_none());
    // A click on empty space clears too.
    click_bar(&mut app, &ids, "Ink", false);
    assert!(app.selection(&ids.page).is_some());
    app.click(&ids.page, &ids.bar, 1.0, 1.0, false).unwrap();
    assert!(app.selection(&ids.page).is_none());
    // selection.clear, for one Page or all.
    click_bar(&mut app, &ids, "Ink", false);
    run(&mut app, "selection.clear", json!({"page": ids.page}));
    assert!(app.selection(&ids.page).is_none());
    click_bar(&mut app, &ids, "Ink", false);
    run(&mut app, "selection.clear", json!({}));
    assert!(app.selection(&ids.page).is_none());
}

#[test]
fn points_by_row_or_keys_and_matrix_cells() {
    let (mut app, ids) = setup();
    let legend = new_visual(
        &mut app,
        "clusteredColumn",
        0,
        &[
            ("axis", col("Product")),
            ("legend", col("Region")),
            ("values", json!({"measure": "Total"})),
        ],
    );
    let matrix = new_visual(
        &mut app,
        "matrix",
        0,
        &[
            ("rows", col("Region")),
            ("columns", col("Product")),
            ("values", json!({"measure": "Total"})),
        ],
    );
    let key = |p: &str, r: &str| {
        json!({"keys": [
            {"table": "Sales", "column": "Product", "value": p},
            {"table": "sales", "column": "REGION", "value": r},
        ]})
    };
    // Two points of two keys each: exactly those (Product, Region) pairs (3 + 4), not every
    // combination of their values (3 + 4 + 5).
    run(
        &mut app,
        "selection.select",
        json!({"page": ids.page, "visual": legend, "point": key("Pen", "North")}),
    );
    run(
        &mut app,
        "selection.select",
        json!({"page": ids.page, "visual": legend, "point": key("Ink", "South"), "add": true}),
    );
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "7");
    // By row: the bar's row of Ink.
    draw(&mut app, &ids.page);
    let ink = row_of(&app, &ids, &ids.bar, "Ink");
    run(
        &mut app,
        "selection.select",
        json!({"page": ids.page, "visual": ids.bar, "point": {"row": ink}}),
    );
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "9");
    // A matrix cell selects its row and column values: North × Pen.
    run(&mut app, "selection.clear", Value::Null);
    draw(&mut app, &ids.page);
    let r = app.visual_result(&ids.page, &matrix).unwrap();
    let north = r
        .rows
        .iter()
        .position(|row| row.keys == [Some(Datum::text("North"))])
        .unwrap();
    let pen = r
        .column_keys
        .iter()
        .position(|k| *k == [Some(Datum::text("Pen"))])
        .unwrap();
    run(
        &mut app,
        "selection.select",
        json!({"page": ids.page, "visual": matrix, "point": {"row": north, "series": pen}}),
    );
    let sel = app.selection(&ids.page).unwrap();
    assert_eq!(sel.points[0].0.len(), 2);
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "3");
}

#[test]
fn slicer_state_is_saved_and_data_points_are_not() {
    let (mut app, ids) = setup();
    run(
        &mut app,
        "slicer.select",
        json!({"page": ids.page, "visual": ids.slicer, "items": ["South", "North"]}),
    );
    let undo_before = app.can_undo();
    click_bar(&mut app, &ids, "Pen", false);
    // Not an edit: nothing new to undo, the Document is unchanged, the slicer edit undoes alone.
    assert_eq!(app.can_undo(), undo_before);
    let doc = app.document().clone();
    run(&mut app, "selection.clear", Value::Null);
    click_bar(&mut app, &ids, "Pen", false);
    assert_eq!(app.document(), &doc);
    run(&mut app, "edit.undo", Value::Null);
    assert!(app.document().report.pages[0].selections.is_empty());
    assert!(
        app.selection(&ids.page).is_some(),
        "undo leaves the data point"
    );
    run(&mut app, "edit.redo", Value::Null);
    // Save and open: the slicer selection is back, the data point is gone.
    let bytes = app.package_bytes().unwrap();
    app.open_package_bytes(&bytes).unwrap();
    assert!(app.selection(&ids.page).is_none());
    assert_eq!(
        app.document().report.pages[0].selections[&ids.slicer],
        [json!("South"), json!("North")]
    );
    draw(&mut app, &ids.page);
    assert_eq!(card(&app, &ids), "12");
    let mut other = App::new();
    other.open_package_bytes(&bytes).unwrap();
    draw(&mut other, &ids.page);
    assert_eq!(card(&other, &ids), "12");
}

#[test]
fn only_visuals_whose_filters_changed_requery() {
    let (mut app, ids) = setup();
    draw(&mut app, &ids.page);
    let first = app.query_runs();
    assert_eq!(first, 4);
    draw(&mut app, &ids.page);
    assert_eq!(app.query_runs(), first, "nothing changed: all hits");
    run(
        &mut app,
        "slicer.select",
        json!({"page": ids.page, "visual": ids.slicer, "items": ["East"]}),
    );
    draw(&mut app, &ids.page);
    assert_eq!(
        app.query_runs(),
        first + 3,
        "the slicer doesn't filter itself"
    );
    // A data point on the bar: the bar's own query is unchanged.
    click_bar(&mut app, &ids, "Pen", false);
    let before = app.query_runs();
    draw(&mut app, &ids.page);
    assert_eq!(app.query_runs(), before + 3);
    // Moving a Visual changes no query.
    run(
        &mut app,
        "visual.move",
        json!({"page": ids.page, "visual": ids.card, "x": 10, "y": 10}),
    );
    let before = app.query_runs();
    draw(&mut app, &ids.page);
    assert_eq!(app.query_runs(), before);
    // A data load: every Visual again.
    app.edit_noting(|doc| {
        let mut d = Document::default();
        add_table(&mut d, "Sales", SALES);
        doc.tables[0] = d.tables.remove(0);
        Ok(None)
    })
    .unwrap();
    draw(&mut app, &ids.page);
    assert_eq!(app.query_runs(), before + 4);
    // And a Model edit.
    run(
        &mut app,
        "modeling.newMeasure",
        json!({"table": "Sales", "name": "Rows", "expression": "COUNTROWS ( Sales )"}),
    );
    draw(&mut app, &ids.page);
    assert_eq!(app.query_runs(), before + 8);
}

#[test]
fn bad_params_are_errors() {
    let (mut app, ids) = setup();
    let p = &ids.page;
    for params in [
        json!({"page": p, "visual": ids.slicer}),
        json!({"page": p, "visual": ids.slicer, "items": "North"}),
        json!({"page": p, "visual": ids.slicer, "items": [3]}),
        json!({"page": p, "visual": ids.slicer, "items": [["North"]]}),
        json!({"page": p, "visual": ids.slicer, "items": ["North"], "add": "yes"}),
        json!({"page": p, "visual": ids.card, "items": ["North"]}),
        json!({"page": p, "visual": "nope", "items": []}),
        json!({"page": "nope", "visual": ids.slicer, "items": []}),
    ] {
        refused(&mut app, "slicer.select", params);
    }
    let big: Vec<Value> = (0..=MAX_SELECTED).map(|i| json!(i.to_string())).collect();
    refused(
        &mut app,
        "slicer.select",
        json!({"page": p, "visual": ids.slicer, "items": big}),
    );
    for params in [
        json!({"page": p, "visual": ids.bar}),
        json!({"page": p, "visual": ids.bar, "point": 3}),
        json!({"page": p, "visual": ids.bar, "point": {"row": 99}}),
        json!({"page": p, "visual": ids.bar, "point": {"row": -1}}),
        json!({"page": p, "visual": ids.card, "point": {"row": 0}}),
        json!({"page": p, "visual": ids.slicer, "point": {"row": 0}}),
        json!({"page": p, "visual": ids.bar, "point": {"keys": []}}),
        json!({"page": p, "visual": ids.bar, "point": {"keys": [{"table": "Sales", "column": "Nope", "value": 1}]}}),
        json!({"page": p, "visual": ids.bar, "point": {"keys": [{"table": "Sales", "column": "Qty", "value": "x"}]}}),
        json!({"page": p, "visual": ids.bar, "point": {"row": 0}, "add": 1}),
    ] {
        refused(&mut app, "selection.select", params);
    }
    assert!(app.selection(p).is_none());
    refused(&mut app, "selection.clear", json!({"page": "nope"}));
    refused(&mut app, "selection.clear", json!({"page": 3}));
    // Clicks before drawing, or on what isn't there.
    assert!(app.click(p, &ids.bar, 1.0, 1.0, false).is_err());
    assert!(app.click("nope", &ids.bar, 1.0, 1.0, false).is_err());
    draw(&mut app, p);
    assert!(app.click(p, "nope", 1.0, 1.0, false).is_err());
    let th = Theme::light();
    let cx = Context {
        theme: &th,
        locale: &opendash_core::EN_GB,
    };
    assert!(app.page_scene("nope", &cx).is_err());
}

#[test]
fn values_round_trip_through_json() {
    for (v, ty) in [
        (Datum::text("a"), ColumnType::Text),
        (Datum::Integer(-3), ColumnType::Whole),
        (Datum::Decimal(2.5), ColumnType::Decimal),
        (Datum::Currency(12_345), ColumnType::FixedDecimal),
        (Datum::DateTime(45_352.5), ColumnType::DateTime),
        (Datum::Boolean(true), ColumnType::Boolean),
        (Datum::Blank, ColumnType::Date),
    ] {
        let j = json_of(&v).unwrap();
        assert_eq!(datum(&j, ty), Some(v));
    }
    assert_eq!(json_of(&Datum::Decimal(f64::NAN)), None);
    assert_eq!(
        datum(&json!(2.0), ColumnType::Whole),
        Some(Datum::Integer(2))
    );
    assert_eq!(datum(&json!(2.5), ColumnType::Whole), None);
    assert_eq!(datum(&json!(1e300), ColumnType::FixedDecimal), None);
    assert_eq!(datum(&json!("1"), ColumnType::Decimal), None);
}

#[test]
fn a_broken_query_draws_as_an_error() {
    let (mut app, ids) = setup();
    run(
        &mut app,
        "modeling.delete",
        json!({"table": "Sales", "name": "Total"}),
    );
    let scenes = draw(&mut app, &ids.page);
    assert!(app.visual_result(&ids.page, &ids.card).is_none());
    let text = scene(&scenes, &ids.card).to_text();
    assert!(text.contains("Couldn't show this visual"), "{text}");
    // Clicking it just clears.
    app.click(&ids.page, &ids.card, 5.0, 5.0, false).unwrap();
}

#[test]
fn random_clicks_and_selections_never_panic() {
    let (mut app, ids) = setup();
    let visuals = [&ids.slicer, &ids.card, &ids.bar, &ids.table, "nope"];
    let mut seed: u64 = 0x2545_f491_4f6c_dd1d;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let values = [
        json!("North"),
        json!(null),
        json!(3),
        json!(-1.5),
        json!(true),
        json!([1]),
        json!({}),
        json!(u64::MAX),
    ];
    for _ in 0..400 {
        let v = visuals[(next() % 5) as usize];
        let any = values[(next() % 8) as usize].clone();
        let add = next() % 2 == 0;
        match next() % 7 {
            0 => {
                let th = Theme::light();
                let cx = Context {
                    theme: &th,
                    locale: &opendash_core::EN_GB,
                };
                let _ = app.page_scene(&ids.page, &cx);
            }
            1 => {
                let coord = |n: u64| match n % 6 {
                    0 => f32::NAN,
                    1 => -1e30,
                    2 => f32::INFINITY,
                    _ => (n % 450) as f32,
                };
                let (x, y) = (coord(next()), coord(next()));
                let _ = app.click(&ids.page, v, x, y, add);
            }
            2 => {
                let _ = app.run(
                    "slicer.select",
                    &json!({"page": ids.page, "visual": v, "items": [any], "add": add}),
                );
            }
            3 => {
                let point = if next() % 2 == 0 {
                    json!({"row": next() % 6, "series": next() % 3})
                } else {
                    json!({"keys": [{"table": "Sales", "column": "Region", "value": any}]})
                };
                let _ = app.run(
                    "selection.select",
                    &json!({"page": ids.page, "visual": v, "point": point, "add": add}),
                );
            }
            4 => {
                let _ = app.run("selection.clear", &json!({"page": any}));
            }
            5 => {
                let _ = app.run("edit.undo", &Value::Null);
            }
            _ => {
                let _ = app.run("visual.delete", &json!({"page": ids.page, "visual": v}));
                let _ = app.run("edit.undo", &Value::Null);
            }
        }
    }
}
