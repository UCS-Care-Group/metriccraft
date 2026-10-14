//! Page refresh bench (M4.7; spec N-3; `docs/bench.md`). For each `import/parquet_<n>` file in
//! `OPENDASH_BENCH_IMPORTS` (see `dax.rs`), loads it once and builds the reference page: card,
//! clustered bar (Country), line (OrderDate), table (Product, 500-row window), matrix
//! (Region → Country × Channel, with subtotals) and a Category slicer. Then, median of [`RUNS`]:
//!
//! - `page/refresh_<n>`: a slicer change (`slicer.select`) and every Visual re-queried and laid
//!   out (`App::page_scene`, the slicer itself a cache hit);
//! - `page/layout_<n>`: `page_scene` again with nothing changed (every query a cache hit): the
//!   layout share of a refresh.

use std::time::Instant;

use opendash_engine::App;
use opendash_visuals::Theme;
use opendash_visuals::layout::Context;
use serde_json::{Value, json};

const RUNS: usize = 5;

fn main() {
    let imports = std::env::var("OPENDASH_BENCH_IMPORTS").unwrap_or_default();
    for (name, path) in imports.lines().filter_map(|l| l.split_once('\t')) {
        let Some(label) = name.strip_prefix("import/parquet_") else {
            continue;
        };
        if let Err(e) = measure(label, path) {
            eprintln!("bench page/*_{label}: {e}");
            std::process::exit(1);
        }
    }
}

fn run(app: &mut App, id: &str, params: &Value) -> Result<(), String> {
    app.run(id, params)
        .map(|_| ())
        .map_err(|e| format!("{id}: {e}"))
}

/// Adds a Visual of `kind` with `fields` (well, Column) and returns its id.
fn visual(
    app: &mut App,
    table: &str,
    kind: &str,
    fields: &[(&str, &str)],
) -> Result<String, String> {
    let ids = |app: &App| -> Vec<String> {
        app.document()
            .report
            .pages
            .first()
            .map(|p| p.visuals.iter().map(|v| v.id.clone()).collect())
            .unwrap_or_default()
    };
    let before = ids(app);
    run(app, "insert.newVisual", &json!({"type": kind}))?;
    let id = ids(app)
        .into_iter()
        .find(|v| !before.contains(v))
        .ok_or("no new visual")?;
    let page = app
        .document()
        .report
        .pages
        .first()
        .ok_or("no page")?
        .id
        .clone();
    for (well, column) in fields {
        let field = json!({"table": table, "column": column});
        run(
            app,
            "visual.addField",
            &json!({"page": page, "visual": id, "well": well, "field": field}),
        )?;
    }
    Ok(id)
}

fn measure(label: &str, path: &str) -> Result<(), String> {
    let mut app = App::new();
    run(
        &mut app,
        "home.getData",
        &json!({"kind": "parquet", "path": path, "item": "Sales"}),
    )?;
    let t = app
        .document()
        .tables
        .first()
        .ok_or("nothing loaded")?
        .name()
        .to_string();
    let t = t.as_str();
    visual(&mut app, t, "card", &[("values", "SalesAmount")])?;
    visual(
        &mut app,
        t,
        "clusteredBar",
        &[("axis", "Country"), ("values", "SalesAmount")],
    )?;
    visual(
        &mut app,
        t,
        "line",
        &[("axis", "OrderDate"), ("values", "SalesAmount")],
    )?;
    visual(
        &mut app,
        t,
        "table",
        &[("values", "Product"), ("values", "SalesAmount")],
    )?;
    visual(
        &mut app,
        t,
        "matrix",
        &[
            ("rows", "Region"),
            ("rows", "Country"),
            ("columns", "Channel"),
            ("values", "SalesAmount"),
        ],
    )?;
    let slicer = visual(&mut app, t, "slicer", &[("field", "Category")])?;
    let page = app
        .document()
        .report
        .pages
        .first()
        .ok_or("no page")?
        .id
        .clone();
    let th = Theme::light();
    let cx = Context {
        theme: &th,
        locale: &opendash_core::EN_GB,
    };
    app.page_scene(&page, &cx).map_err(|e| e.to_string())?;
    let categories = ["Bikes", "Helmets", "Clothing", "Shoes", "Tents", "Camping"];
    let (mut refresh, mut layout) = (Vec::new(), Vec::new());
    for i in 0..RUNS {
        let item = categories
            .get(i % categories.len())
            .copied()
            .unwrap_or("Bikes");
        let start = Instant::now();
        run(
            &mut app,
            "slicer.select",
            &json!({"page": page, "visual": slicer, "items": [item]}),
        )?;
        let scenes = app.page_scene(&page, &cx).map_err(|e| e.to_string())?;
        refresh.push(start.elapsed().as_nanos());
        std::hint::black_box(scenes);
        let start = Instant::now();
        let scenes = app.page_scene(&page, &cx).map_err(|e| e.to_string())?;
        layout.push(start.elapsed().as_nanos());
        std::hint::black_box(scenes);
    }
    for (name, mut times) in [("refresh", refresh), ("layout", layout)] {
        times.sort_unstable();
        if let Some(ns) = times.get(RUNS / 2) {
            println!("BENCH page/{name}_{label} {ns}");
        }
    }
    Ok(())
}
