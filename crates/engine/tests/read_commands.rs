//! Read commands, Batch and Origin (spec C-1, C-4; ADR 0009).

use metriccraft_engine::{App, Error, Origin};
use serde_json::{Value, json};

/// An app with `Sales` loaded from a CSV in a fresh temp dir (removed on drop).
struct Fixture {
    app: App,
    dir: std::path::PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
fn fixture(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("mcraft-read-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Sales.csv");
    std::fs::write(
        &path,
        "Region,Qty,Date\nNorth,3,2024-03-01\nSouth,4,2024-03-02\nNorth,5,\n",
    )
    .unwrap();
    let mut app = App::new();
    app.run(
        "home.getData",
        &json!({"kind": "csv", "path": path.display().to_string()}),
    )
    .unwrap();
    Fixture { app, dir }
}

fn eval(app: &mut App, query: &str, max: Option<i64>) -> Result<Value, Error> {
    let mut p = json!({ "query": query });
    if let Some(m) = max {
        p["maxRows"] = json!(m);
    }
    app.run("dax.evaluate", &p)
}

#[test]
fn dax_evaluate_returns_typed_columns_and_null_for_blank() {
    let mut f = fixture("eval");
    let out = eval(
        &mut f.app,
        "EVALUATE SUMMARIZECOLUMNS(Sales[Region], \"Total\", SUM(Sales[Qty])) ORDER BY Sales[Region]",
        None,
    )
    .unwrap();
    assert_eq!(
        out,
        json!({
            "columns": [{"name": "Region", "type": "text"}, {"name": "Total", "type": "whole"}],
            "rows": [["North", 8], ["South", 4]],
            "truncated": false,
        })
    );
    let dates = eval(
        &mut f.app,
        "EVALUATE SUMMARIZECOLUMNS(Sales[Date]) ORDER BY Sales[Date]",
        None,
    )
    .unwrap();
    assert_eq!(
        dates["rows"],
        json!([[null], ["2024-03-01"], ["2024-03-02"]])
    );
    assert_eq!(dates["columns"][0]["type"], "datetime");
}

#[test]
fn dax_evaluate_truncates_and_caps_max_rows() {
    let mut f = fixture("truncate");
    let out = eval(&mut f.app, "EVALUATE Sales", Some(2)).unwrap();
    assert_eq!(out["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(out["truncated"], true);
    assert!(matches!(
        eval(&mut f.app, "EVALUATE Sales", Some(1_000_001)),
        Err(Error::InvalidParams { .. })
    ));
    assert!(matches!(
        eval(&mut f.app, "EVALUATE Sales", Some(-1)),
        Err(Error::InvalidParams { .. })
    ));
}

#[test]
fn dax_evaluate_errors_never_change_anything() {
    let mut f = fixture("errors");
    let before = f.app.document().clone();
    for q in ["EVALUATE", "EVALUATE Nope", "EVALUATE Sales EVALUATE Sales"] {
        assert!(
            matches!(eval(&mut f.app, q, None), Err(Error::Read { .. })),
            "{q}"
        );
    }
    assert_eq!(f.app.document(), &before);
}

#[test]
fn read_commands_are_not_undoable() {
    let mut f = fixture("undo");
    let undo = f.app.undo_label().map(str::to_string);
    let seq = f.app.run("document.describe", &Value::Null).unwrap()["seq"].clone();
    eval(&mut f.app, "EVALUATE Sales", None).unwrap();
    let again = f.app.run("document.describe", &Value::Null).unwrap();
    assert_eq!(again["seq"], seq, "a Read command changes nothing");
    assert_eq!(f.app.undo_label().map(str::to_string), undo);
    assert_eq!(again["tables"][0]["name"], "Sales");
    assert_eq!(again["tables"][0]["rows"], 3);
    assert!(f.app.command("dax.evaluate").is_some_and(|c| c.read_only()));
    assert!(!f.app.command("batch").is_some_and(|c| c.read_only()));
}

#[test]
fn page_render_returns_a_png() {
    let mut f = fixture("render");
    f.app
        .run(
            "modeling.newMeasure",
            &json!({"table": "Sales", "name": "Total", "expression": "SUM(Sales[Qty])"}),
        )
        .unwrap();
    f.app
        .run(
            "insert.newVisual",
            &json!({"type": "card", "x": 10, "y": 10}),
        )
        .unwrap();
    let doc = f.app.run("document.describe", &Value::Null).unwrap();
    let visual = doc["pages"][0]["visuals"][0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let page = doc["pages"][0]["id"].as_str().unwrap().to_string();
    f.app
        .run(
            "visual.addField",
            &json!({"page": page, "visual": visual, "well": "values", "field": {"measure": "Total"}}),
        )
        .unwrap();
    let out = f.app.run("page.render", &json!({"width": 640})).unwrap();
    assert_eq!(
        (out["width"].as_f64(), out["height"].as_f64()),
        (Some(640.0), Some(360.0))
    );
    // The PNG signature, base64.
    assert!(out["png"].as_str().unwrap().starts_with("iVBORw0KGgo"));
    assert!(matches!(
        f.app.run("page.render", &json!({"scale": 1000})),
        Err(Error::Read { .. })
    ));
}

#[test]
fn a_batch_is_one_undo_step_labelled_with_its_origin() {
    let mut f = fixture("batch");
    let measures = |app: &App| app.document().model.table("Sales").unwrap().measures.len();
    let out = f
        .app
        .run_as(
            Origin::Mcp,
            "batch",
            &json!({"commands": [
                {"run": "modeling.newMeasure", "params": {"table": "Sales", "name": "A", "expression": "1"}},
                {"run": "dax.evaluate", "params": {"query": "EVALUATE ROW(\"x\", [A])"}},
                {"run": "modeling.newMeasure", "params": {"table": "Sales", "name": "B", "expression": "2"}},
            ]}),
        )
        .unwrap();
    assert_eq!(
        out[1]["rows"],
        json!([[1]]),
        "a step sees the steps before it"
    );
    assert_eq!(measures(&f.app), 2);
    assert_eq!(f.app.undo_label(), Some("MCP — 2 changes"));
    f.app.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(measures(&f.app), 0, "one Undo undoes the whole Batch");
    assert_eq!(f.app.redo_label(), Some("MCP — 2 changes"));
    f.app.run("edit.redo", &Value::Null).unwrap();
    assert_eq!(measures(&f.app), 2);

    f.app
        .run_as(
            Origin::Cli,
            "modeling.newMeasure",
            &json!({"table": "Sales", "name": "C", "expression": "3"}),
        )
        .unwrap();
    assert_eq!(f.app.undo_label(), Some("CLI — New measure"));
}

#[test]
fn a_failing_batch_changes_nothing() {
    let mut f = fixture("batch-fail");
    let before = f.app.document().clone();
    let undo = f.app.undo_label().map(str::to_string);
    let dirty = f.app.is_dirty();
    let err = f
        .app
        .run_as(
            Origin::Control,
            "batch",
            &json!({"commands": [
                {"run": "modeling.newMeasure", "params": {"table": "Sales", "name": "A", "expression": "1"}},
                {"run": "modeling.newMeasure", "params": {"table": "Nope", "name": "B", "expression": "2"}},
            ]}),
        )
        .unwrap_err();
    assert!(matches!(err, Error::Batch { index: 1, .. }), "{err}");
    assert_eq!(f.app.document(), &before);
    assert_eq!(f.app.undo_label().map(str::to_string), undo);
    assert_eq!(f.app.is_dirty(), dirty);
}

#[test]
fn a_batch_rejects_what_it_cannot_hold_before_running() {
    let mut f = fixture("batch-reject");
    let before = f.app.document().clone();
    let first = json!({"run": "modeling.newMeasure", "params": {"table": "Sales", "name": "A", "expression": "1"}});
    for bad in [
        json!({"run": "edit.undo"}),
        json!({"run": "file.new"}),
        json!({"run": "batch", "params": {"commands": []}}),
        json!({"run": "nope"}),
        json!({"params": {}}),
        json!(7),
    ] {
        let r = f.app.run("batch", &json!({ "commands": [first, bad] }));
        assert!(
            matches!(r, Err(Error::InvalidParams { .. })),
            "{bad}: {r:?}"
        );
        assert_eq!(f.app.document(), &before);
    }
    let many: Vec<Value> = (0..=metriccraft_engine::MAX_BATCH)
        .map(|_| json!({"run": "document.describe"}))
        .collect();
    assert!(f.app.run("batch", &json!({ "commands": many })).is_err());
    assert_eq!(f.app.run("batch", &json!({"commands": []})), Ok(json!([])));
    assert_eq!(f.app.document(), &before, "an empty Batch records nothing");
}
