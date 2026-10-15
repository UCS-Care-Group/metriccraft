use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;

/// A fresh directory under the system temp dir, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("metriccraft-rel-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn write(&self, name: &str, text: &str) -> String {
        let path = self.0.join(name);
        std::fs::write(&path, text).unwrap();
        path.display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SALES: &str = "Region,Code,Qty\nNorth,1,3\nNorth,1,4\nSouth,2,5\n";
const REGIONS: &str = "Region,Code,Price\nNorth,1,1.1\nSouth,2,1.2\n";

/// Sales then Regions loaded, with the auto-detect switch `autodetect`.
fn app(dir: &Dir, autodetect: bool) -> App {
    let mut app = App::new();
    if !autodetect {
        app.run(
            "options.autodetectRelationships",
            &json!({"enabled": false}),
        )
        .unwrap();
    }
    for (name, text) in [("Sales.csv", SALES), ("Regions.csv", REGIONS)] {
        let path = dir.write(name, text);
        app.run("home.getData", &json!({"kind": "csv", "path": path}))
            .unwrap();
    }
    app
}

fn shown(app: &App) -> Vec<String> {
    app.document()
        .model
        .relationships
        .iter()
        .map(|r| format!("{r}{}", if r.active { "" } else { " inactive" }))
        .collect()
}

fn by_region() -> Value {
    json!({"fromTable": "Sales", "fromColumn": "Region", "toTable": "Regions",
        "toColumn": "Region"})
}

fn by_code() -> Value {
    json!({"fromTable": "Sales", "fromColumn": "Code", "toTable": "Regions",
        "toColumn": "Code"})
}

#[test]
fn create_edit_delete_and_undo() {
    let dir = Dir::new();
    let mut app = app(&dir, false);
    assert!(shown(&app).is_empty());
    let start = app.document().clone();
    app.run("modeling.newRelationship", &by_region()).unwrap();
    assert_eq!(app.notice(), None);
    // A second active one on the pair is created inactive, with a notice.
    app.run("modeling.newRelationship", &by_code()).unwrap();
    assert!(app.notice().is_some_and(|n| n.contains("inactive")));
    assert_eq!(
        shown(&app),
        [
            "Sales[Region] -> Regions[Region]",
            "Sales[Code] -> Regions[Code] inactive"
        ]
    );
    // Activating it fails until the other is deactivated.
    let before = app.document().clone();
    let e = app
        .run(
            "modeling.setRelationshipActive",
            &json!({"relationship": 1}),
        )
        .unwrap_err();
    assert!(
        e.to_string().contains("You can't create a direct active"),
        "{e}"
    );
    assert_eq!(app.document(), &before);
    app.run(
        "modeling.setRelationshipActive",
        &json!({"relationship": by_region(), "active": false}),
    )
    .unwrap();
    app.run(
        "modeling.setRelationshipActive",
        &json!({"relationship": 1}),
    )
    .unwrap();
    assert_eq!(
        shown(&app),
        [
            "Sales[Region] -> Regions[Region] inactive",
            "Sales[Code] -> Regions[Code]"
        ]
    );
    // Edit the ends; undo puts them back.
    let edit = json!({"relationship": 0, "fromColumn": "Qty", "toColumn": "Price"});
    app.run("modeling.editRelationship", &edit).unwrap();
    assert_eq!(shown(&app)[0], "Sales[Qty] -> Regions[Price] inactive");
    app.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(shown(&app)[0], "Sales[Region] -> Regions[Region] inactive");
    // Editing it active while the pair has an active one keeps it inactive, with a notice.
    app.run(
        "modeling.editRelationship",
        &json!({"relationship": 0, "active": true}),
    )
    .unwrap();
    assert!(app.notice().is_some_and(|n| n.contains("inactive")));
    assert!(!app.document().model.relationships[0].active);
    // Delete, by columns.
    app.run(
        "modeling.deleteRelationship",
        &json!({"relationship": by_code()}),
    )
    .unwrap();
    assert_eq!(shown(&app), ["Sales[Region] -> Regions[Region] inactive"]);
    app.run("edit.undo", &Value::Null).unwrap();
    assert_eq!(shown(&app).len(), 2);
    for _ in 0..5 {
        app.run("edit.undo", &Value::Null).unwrap();
    }
    assert_eq!(app.document(), &start);
}

#[test]
fn bad_params_are_invalid_and_change_nothing() {
    let dir = Dir::new();
    let mut app = app(&dir, false);
    app.run("modeling.newRelationship", &by_region()).unwrap();
    let before = app.document().clone();
    let reversed = json!({"fromTable": "Regions", "fromColumn": "Region",
        "toTable": "Sales", "toColumn": "Region"});
    for (id, params) in [
        ("modeling.newRelationship", json!({"fromTable": "Sales"})),
        ("modeling.newRelationship", by_region()),
        ("modeling.newRelationship", reversed.clone()),
        (
            "modeling.newRelationship",
            json!({"fromTable": "Sales", "fromColumn": "Qty", "toTable": "Sales",
                "toColumn": "Code"}),
        ),
        (
            "modeling.newRelationship",
            json!({"fromTable": "Sales", "fromColumn": "Nope", "toTable": "Regions",
                "toColumn": "Code"}),
        ),
        (
            "modeling.newRelationship",
            json!({"fromTable": "Regions", "fromColumn": "Code", "toTable": "Sales",
                "toColumn": "Code"}),
        ),
        (
            "modeling.newRelationship",
            json!({"fromTable": "Sales", "fromColumn": "Code", "toTable": "Regions",
                "toColumn": "Code", "active": "yes"}),
        ),
        ("modeling.editRelationship", json!({"relationship": 1})),
        ("modeling.editRelationship", json!({"relationship": -1})),
        (
            "modeling.editRelationship",
            json!({"relationship": 0, "toColumn": 3}),
        ),
        (
            "modeling.deleteRelationship",
            json!({"relationship": reversed}),
        ),
        ("modeling.deleteRelationship", json!({"relationship": "0"})),
        (
            "modeling.setRelationshipActive",
            json!({"relationship": 0, "active": 1}),
        ),
        ("options.autodetectRelationships", json!({"enabled": "on"})),
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
            "modeling.newRelationship",
            &json!({"fromTable": "Regions", "fromColumn": "Code", "toTable": "Sales",
                "toColumn": "Code"}),
        )
        .unwrap_err();
    assert!(
        e.to_string()
            .contains("Column 'Code' in Table 'Sales' contains a duplicate value '1'"),
        "{e}"
    );
}

#[test]
fn autodetect_after_load_and_on_demand() {
    let dir = Dir::new();
    let mut app = app(&dir, true);
    // Region and Code are both keys of Regions, not of Sales: the first is active.
    assert_eq!(
        shown(&app),
        [
            "Sales[Region] -> Regions[Region]",
            "Sales[Code] -> Regions[Code] inactive"
        ]
    );
    assert_eq!(app.last_load().unwrap().relationships_added.len(), 2);
    // A refresh loads no new Table: a deleted one isn't brought back.
    app.run("modeling.deleteRelationship", &json!({"relationship": 1}))
        .unwrap();
    app.run("home.refresh", &Value::Null).unwrap();
    assert!(app.last_load().unwrap().relationships_added.is_empty());
    assert_eq!(shown(&app).len(), 1);
    // Switched off: a new Table gets none; on demand still finds them.
    let mut off = self::app(&dir, false);
    assert!(shown(&off).is_empty());
    off.run("modeling.autodetectRelationships", &Value::Null)
        .unwrap();
    assert_eq!(off.notice(), Some("Found 2 new relationships."));
    assert_eq!(shown(&off).len(), 2);
    // The pair has relationships now: nothing found, nothing recorded.
    let doc = off.document().clone();
    off.run("modeling.autodetectRelationships", &json!({}))
        .unwrap();
    assert_eq!(off.notice(), Some("Found no new relationships."));
    assert_eq!(off.document(), &doc);
    off.run("edit.undo", &Value::Null).unwrap();
    assert!(
        shown(&off).is_empty(),
        "the last undo step is the first autodetect"
    );
}

#[test]
fn a_duplicate_key_refresh_fails_and_keeps_the_old_data() {
    let dir = Dir::new();
    let mut app = app(&dir, true);
    // Case and trailing spaces don't make a value different.
    dir.write("Regions.csv", "Region,Code,Price\nNorth,1,1\nnorth ,3,1\n");
    let before = app.document().clone();
    let e = app.run("home.refresh", &Value::Null).unwrap_err();
    assert!(
        matches!(&e, Error::Model(m) if m.table == "Regions" && m.column == "Region"),
        "{e:?}"
    );
    assert!(
        e.to_string()
            .contains("contains a duplicate value 'north '"),
        "{e}"
    );
    assert_eq!(app.document(), &before);
    // An empty CSV field is empty text, not blank: one is a key value like any other.
    dir.write("Regions.csv", "Region,Code,Price\nNorth,1,1\n,3,1\n");
    app.run("home.refresh", &Value::Null).unwrap();
}

#[test]
fn a_load_without_a_related_column_drops_the_relationship() {
    let dir = Dir::new();
    let mut app = app(&dir, true);
    app.run(
        "home.removeColumns",
        &json!({"query": "Regions", "columns": ["Code"]}),
    )
    .unwrap();
    app.run("home.closeApply", &Value::Null).unwrap();
    assert_eq!(shown(&app), ["Sales[Region] -> Regions[Region]"]);
    let removed = &app.last_load().unwrap().relationships_removed;
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].to_string(), "Sales[Code] -> Regions[Code]");
    // A Table no longer loaded loses its relationships.
    app.run(
        "query.enableLoad",
        &json!({"query": "Regions", "enabled": false}),
    )
    .unwrap();
    app.run("home.closeApply", &Value::Null).unwrap();
    assert!(shown(&app).is_empty());
}

#[test]
fn a_type_change_that_breaks_a_key_fails() {
    let dir = Dir::new();
    let mut app = app(&dir, false);
    let price = json!({"fromTable": "Sales", "fromColumn": "Qty", "toTable": "Regions",
        "toColumn": "Price"});
    app.run("modeling.newRelationship", &price).unwrap();
    let before = app.document().clone();
    // 1.1 and 1.2 both become 1.
    let whole = json!({"table": "Regions", "column": "Price", "type": "whole"});
    let e = app.run("columnTools.dataType", &whole).unwrap_err();
    assert!(e.to_string().contains("duplicate value '1'"), "{e}");
    assert_eq!(app.document(), &before);
    // A change that keeps it unique is fine, whatever the other side's type.
    let text = json!({"table": "Regions", "column": "Price", "type": "text"});
    app.run("columnTools.dataType", &text).unwrap();
}

#[test]
fn the_autodetect_switch_toggles_and_undoes() {
    let mut app = App::new();
    assert!(app.document().model.autodetect_relationships);
    app.run("options.autodetectRelationships", &Value::Null)
        .unwrap();
    assert!(!app.document().model.autodetect_relationships);
    app.run("edit.undo", &Value::Null).unwrap();
    assert!(app.document().model.autodetect_relationships);
}

#[test]
fn orient_swaps_ends_dragged_the_other_way() {
    let dir = Dir::new();
    let mut app = app(&dir, false);
    let backwards = json!({"fromTable": "Regions", "fromColumn": "Region", "toTable": "Sales",
        "toColumn": "Region"});
    assert!(app.run("modeling.newRelationship", &backwards).is_err());
    let mut oriented = backwards;
    oriented["orient"] = json!(true);
    app.run("modeling.newRelationship", &oriented).unwrap();
    assert_eq!(shown(&app), ["Sales[Region] -> Regions[Region]"]);
}
