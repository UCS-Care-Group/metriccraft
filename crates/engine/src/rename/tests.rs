use opendash_data::Query;
use opendash_data::culture::Culture;
use opendash_data::source::{Bytes, File, Source, csv};
use opendash_data::step::Step;
use opendash_model::{ModelTable, Relationship};
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

/// `Sales` (Region, Qty) merging `Regions` (Region, Manager, Rank) on Region, with a
/// Relationship Sales[Region] -> Regions[Region], Manager sorted by Rank, all applied.
fn app() -> App {
    let mut doc = Document::default();
    add(&mut doc, "Sales", "Region,Qty\nNorth,3\nSouth,4\nNorth,5\n");
    add(
        &mut doc,
        "Regions",
        "Region,Manager,Rank\nNorth,Ann,1\nSouth,Bob,2\n",
    );
    let mut app = App::with_document(doc);
    for (id, params) in [
        (
            "home.mergeQueries",
            json!({"query": "Sales", "keys": ["Region"], "with": "Regions",
                "withKeys": ["Region"], "expand": ["Region", "Manager"]}),
        ),
        ("home.closeApply", Value::Null),
        (
            "modeling.newRelationship",
            json!({"fromTable": "Sales", "fromColumn": "Region", "toTable": "Regions",
                "toColumn": "Region"}),
        ),
        (
            "columnTools.sortByColumn",
            json!({"table": "Regions", "column": "Manager", "by": "Rank"}),
        ),
        (
            "model.setDescription",
            json!({"table": "Regions", "column": "Region", "description": "Sales area"}),
        ),
    ] {
        app.run(id, &params).unwrap();
    }
    app
}

fn merge(doc: &Document) -> &opendash_data::step::Merge {
    let sales = doc.queries.iter().find(|q| q.name == "Sales").unwrap();
    let Some(Step::MergedQueries(m)) = sales.steps.last() else {
        panic!("no merge step");
    };
    m
}

fn names(doc: &Document) -> Vec<&str> {
    doc.tables.iter().map(|t| t.name()).collect()
}

#[test]
fn rename_table_rewrites_every_reference_in_one_undoable_edit() {
    for (id, key) in [("modeling.renameTable", "table"), ("query.rename", "query")] {
        let mut app = app();
        app.run(
            "query.enableLoad",
            &json!({"query": "Regions", "enabled": false}),
        )
        .unwrap();
        app.run(
            "query.enableLoad",
            &json!({"query": "Regions", "enabled": true}),
        )
        .unwrap();
        let before = app.document().clone();
        assert!(before.stale.contains("Regions"));
        app.run(id, &json!({key: "Regions", "name": "Areas"}))
            .unwrap();
        let doc = app.document();
        assert_eq!(doc.queries[1].name, "Areas");
        assert_eq!(names(doc), ["Sales", "Areas"]);
        assert!(doc.model.table("Areas").is_some() && doc.model.table("Regions").is_none());
        assert_eq!(
            doc.model.relationships,
            [Relationship::new("Sales", "Region", "Areas", "Region")]
        );
        let m = merge(doc);
        assert_eq!((m.query.as_str(), m.column.as_str()), ("Areas", "Regions"));
        assert!(doc.stale.contains("Areas") && !doc.stale.contains("Regions"));
        // Still loads: the merge finds the renamed Query.
        app.run("home.refresh", &Value::Null).unwrap();
        assert_eq!(names(app.document()), ["Sales", "Areas"]);
        assert_eq!(app.document().model.relationships.len(), 1);
        app.run("edit.undo", &Value::Null).unwrap();
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.document(), &before, "{id}: one undo step");
    }
}

#[test]
fn rename_table_refuses_taken_and_bad_names() {
    for (id, params) in [
        (
            "modeling.renameTable",
            json!({"table": "Regions", "name": "sales"}),
        ),
        ("query.rename", json!({"query": "Regions", "name": "SALES"})),
        ("query.rename", json!({"query": "Regions", "name": ""})),
        (
            "query.rename",
            json!({"query": "Regions", "name": " Areas"}),
        ),
        (
            "query.rename",
            json!({"query": "Regions", "name": "A\u{7}"}),
        ),
        ("query.rename", json!({"query": "Nope", "name": "Areas"})),
        (
            "modeling.renameTable",
            json!({"table": "Nope", "name": "Areas"}),
        ),
        ("modeling.renameTable", json!({"table": "Regions"})),
    ] {
        let mut app = app();
        let before = app.document().clone();
        let r = app.run(id, &params);
        assert!(
            matches!(r, Err(Error::InvalidParams { .. })),
            "{params}: {r:?}"
        );
        assert_eq!(app.document(), &before);
    }
}

#[test]
fn renaming_to_the_same_name_records_nothing_but_a_case_change_renames() {
    let mut app = app();
    let before = app.document().clone();
    let undo = |app: &mut App| {
        let mut n = 0;
        while app.can_undo() {
            app.run("edit.undo", &Value::Null).unwrap();
            n += 1;
        }
        n
    };
    for (id, params) in [
        (
            "query.rename",
            json!({"query": "Regions", "name": "Regions"}),
        ),
        (
            "modeling.renameColumn",
            json!({"table": "Regions", "column": "Rank", "name": "Rank"}),
        ),
    ] {
        app.run(id, &params).unwrap();
        assert_eq!(app.document(), &before);
    }
    app.run(
        "query.rename",
        &json!({"query": "Regions", "name": "regions"}),
    )
    .unwrap();
    assert_eq!(app.document().queries[1].name, "regions");
    app.run(
        "modeling.renameColumn",
        &json!({"table": "regions", "column": "Rank", "name": "RANK"}),
    )
    .unwrap();
    assert_eq!(
        undo(&mut app),
        7,
        "5 from setup + 2 renames, none for the no-ops"
    );
}

#[test]
fn rename_column_adds_a_step_and_rewrites_every_reference() {
    let mut app = app();
    let before = app.document().clone();
    app.run(
        "modeling.renameColumn",
        &json!({"table": "Regions", "column": "Region", "name": "Area"}),
    )
    .unwrap();
    app.run(
        "modeling.renameColumn",
        &json!({"table": "Regions", "column": "Rank", "name": "Order"}),
    )
    .unwrap();
    let doc = app.document();
    assert!(doc.stale.is_empty(), "the loaded data already matches");
    assert_eq!(
        doc.queries[1].steps.last(),
        Some(&Step::RenamedColumns(vec![("Rank".into(), "Order".into())]))
    );
    let regions = doc.tables[1].batch().schema();
    let cols: Vec<&str> = regions.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(cols, ["Area", "Manager", "Order"]);
    let model = doc.model.table("Regions").unwrap();
    let mcols: Vec<&str> = model.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(mcols, cols);
    assert_eq!(model.column("Area").unwrap().description, "Sales area");
    assert_eq!(
        model.column("Manager").unwrap().sort_by.as_deref(),
        Some("Order")
    );
    assert_eq!(
        doc.model.relationships,
        [Relationship::new("Sales", "Region", "Regions", "Area")]
    );
    let m = merge(doc);
    assert_eq!(m.other_keys, ["Area"]);
    assert_eq!(
        m.expand,
        [
            ("Area".to_string(), "Regions.Region".to_string()),
            ("Manager".into(), "Regions.Manager".into())
        ],
        "the merge's own columns keep their names"
    );
    // A Refresh from the Sources gives the same tables and keeps the model.
    let renamed = app.document().clone();
    app.run("home.refresh", &Value::Null).unwrap();
    assert_eq!(app.document().tables, renamed.tables);
    assert_eq!(app.document().model, renamed.model);
    // Sales[Qty] too: its own Query, no merge rewrite.
    app.run(
        "modeling.renameColumn",
        &json!({"table": "Sales", "column": "Qty", "name": "Units"}),
    )
    .unwrap();
    for _ in 0..4 {
        app.run("edit.undo", &Value::Null).unwrap();
    }
    assert_eq!(app.document(), &before);
}

#[test]
fn rename_column_refuses_taken_missing_and_bad_names() {
    for params in [
        json!({"table": "Regions", "column": "Region", "name": "manager"}),
        json!({"table": "Regions", "column": "Nope", "name": "X"}),
        json!({"table": "Nope", "column": "Region", "name": "X"}),
        json!({"table": "Regions", "column": "Region", "name": "X "}),
        json!({"table": "Regions", "column": "Region", "name": "  "}),
        json!({"table": "Regions", "column": "Region"}),
        json!({"table": "Regions", "name": "X"}),
    ] {
        let mut app = app();
        let before = app.document().clone();
        let r = app.run("modeling.renameColumn", &params);
        assert!(
            matches!(r, Err(Error::InvalidParams { .. } | Error::Model(_))),
            "{params}: {r:?}"
        );
        assert_eq!(app.document(), &before);
    }
}

#[test]
fn rename_column_of_a_query_edited_since_loaded_must_still_run() {
    // The Query no longer has the column: its new step can't run, so nothing changes.
    let mut app = app();
    app.run(
        "home.removeColumns",
        &json!({"query": "Regions", "columns": ["Rank"]}),
    )
    .unwrap();
    let before = app.document().clone();
    let r = app.run(
        "modeling.renameColumn",
        &json!({"table": "Regions", "column": "Rank", "name": "Order"}),
    );
    assert!(matches!(r, Err(Error::InvalidParams { .. })), "{r:?}");
    assert_eq!(app.document(), &before);
}

#[test]
fn delete_removes_query_table_model_and_relationships_unless_merged() {
    for (id, key) in [("query.delete", "query"), ("modeling.deleteTable", "table")] {
        let mut app = app();
        let before = app.document().clone();
        let r = app.run(id, &json!({key: "Regions"}));
        assert!(
            matches!(&r, Err(Error::InvalidParams { message, .. }) if message.contains("merges")),
            "{r:?}"
        );
        assert_eq!(app.document(), &before);
        app.run(id, &json!({key: "Sales"})).unwrap();
        let doc = app.document();
        assert_eq!(names(doc), ["Regions"]);
        assert_eq!(doc.queries.len(), 1);
        assert!(doc.model.table("Sales").is_none());
        assert!(doc.model.relationships.is_empty());
        // Now nothing merges Regions.
        app.run(id, &json!({key: "Regions"})).unwrap();
        assert!(app.document().queries.is_empty());
        app.run("edit.undo", &Value::Null).unwrap();
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.document(), &before);
    }
    // An unloaded Query has no Table to delete from the model, but is a Query to delete.
    let mut app = app();
    app.run("query.duplicate", &json!({"query": "Sales"}))
        .unwrap();
    let r = app.run("modeling.deleteTable", &json!({"table": "Sales (2)"}));
    assert!(matches!(r, Err(Error::InvalidParams { .. })), "{r:?}");
    app.run("query.delete", &json!({"query": "Sales (2)"}))
        .unwrap();
    assert!(app.document().stale.is_empty());
}

#[test]
fn duplicate_copies_the_steps_under_the_next_free_name() {
    let mut app = app();
    app.run("query.rename", &json!({"query": "Sales", "name": "s"}))
        .unwrap();
    app.run("query.duplicate", &json!({"query": "Regions"}))
        .unwrap();
    app.run("query.duplicate", &json!({"query": "Regions"}))
        .unwrap();
    let doc = app.document();
    let qs: Vec<&str> = doc.queries.iter().map(|q| q.name.as_str()).collect();
    assert_eq!(qs, ["s", "Regions", "Regions (2)", "Regions (3)"]);
    assert_eq!(doc.queries[2].steps, doc.queries[1].steps);
    assert_eq!(
        doc.stale.iter().map(String::as_str).collect::<Vec<_>>(),
        ["Regions (2)", "Regions (3)"]
    );
    app.run("home.closeApply", &Value::Null).unwrap();
    assert_eq!(
        names(app.document()),
        ["s", "Regions", "Regions (2)", "Regions (3)"]
    );
    // Enable load is copied: an unloaded Query's copy isn't loaded either.
    app.run(
        "query.enableLoad",
        &json!({"query": "Regions (3)", "enabled": false}),
    )
    .unwrap();
    app.run("home.closeApply", &Value::Null).unwrap();
    app.run("query.duplicate", &json!({"query": "Regions (3)"}))
        .unwrap();
    let doc = app.document();
    assert!(doc.unloaded.contains("Regions (3) (2)"));
    assert!(doc.stale.is_empty());
    let r = app.run("query.duplicate", &json!({"query": "Nope"}));
    assert!(matches!(r, Err(Error::InvalidParams { .. })));
}

/// `app`'s Package files, with `edit` applied to some parts and the Manifest rehashed.
fn files_edited(app: &App, edit: &[(&str, &str, &str)]) -> opendash_file::FileSet {
    use opendash_file::manifest::{Manifest, hash};
    let mut files = opendash_file::unpack(&app.package_bytes().unwrap()).unwrap();
    for (path, from, to) in edit {
        let text = String::from_utf8(files[*path].clone()).unwrap();
        assert!(text.contains(from), "{from:?} in {path}:\n{text}");
        files.insert((*path).into(), text.replacen(from, to, 1).into_bytes());
    }
    let mut m = Manifest::parse(&files[opendash_file::MANIFEST]).unwrap();
    for (p, h) in m.parts.iter_mut() {
        *h = hash(&files[p]);
    }
    files.insert(opendash_file::MANIFEST.into(), m.print());
    files
}

fn saved(app: &App, path: &str) -> String {
    let files = opendash_file::unpack(&app.package_bytes().unwrap()).unwrap();
    String::from_utf8(files.get(path).cloned().unwrap_or_default()).unwrap()
}

/// Column `name`'s lines in a saved table part.
fn column<'a>(table: &'a str, name: &str) -> &'a str {
    let rest = table
        .split(&format!("  column {name}\n"))
        .nth(1)
        .unwrap_or_default();
    rest.split("  column ").next().unwrap_or_default()
}

#[test]
fn renames_keep_unknown_file_content_under_the_new_name() {
    let files = files_edited(
        &app(),
        &[
            (
                "model/tables/Regions.odm",
                "table Regions\n",
                "table Regions\n  lineageTag: t1\n",
            ),
            (
                "model/tables/Regions.odm",
                "  column Manager\n",
                "  column Manager\n    isKey\n",
            ),
            (
                "queries/Regions.odq",
                "  load: true\n",
                "  load: true\n  note: kept\n",
            ),
            (
                "model/relationships.odm",
                "relationship Sales[Region] -> Regions[Region]\n",
                "relationship Sales[Region] -> Regions[Region]\n  crossFilter: both\n",
            ),
        ],
    );
    let mut app = App::new();
    app.open_package_bytes(&opendash_file::pack(&files).unwrap())
        .unwrap();
    app.run(
        "modeling.renameTable",
        &json!({"table": "Regions", "name": "Areas"}),
    )
    .unwrap();
    app.run(
        "modeling.renameColumn",
        &json!({"table": "Areas", "column": "Region", "name": "Area"}),
    )
    .unwrap();
    app.run(
        "modeling.renameColumn",
        &json!({"table": "Areas", "column": "Manager", "name": "Boss"}),
    )
    .unwrap();

    let table = saved(&app, "model/tables/Areas.odm");
    assert!(
        table.starts_with("table Areas\n") && table.contains("  lineageTag: t1\n"),
        "{table}"
    );
    assert!(column(&table, "Boss").contains("    isKey\n"), "{table}");
    assert!(saved(&app, "queries/Areas.odq").contains("  note: kept\n"));
    assert_eq!(
        saved(&app, "model/relationships.odm"),
        "relationship Sales[Region] -> Areas[Area]\n  crossFilter: both\n"
    );
    assert_eq!(saved(&app, "model/tables/Regions.odm"), "");

    // Undone, they are under the old names again.
    for _ in 0..3 {
        app.run("edit.undo", &Value::Null).unwrap();
    }
    let table = saved(&app, "model/tables/Regions.odm");
    assert!(
        table.contains("  lineageTag: t1\n") && column(&table, "Manager").contains("    isKey\n"),
        "{table}"
    );
    assert!(saved(&app, "queries/Regions.odq").contains("  note: kept\n"));
    assert!(saved(&app, "model/relationships.odm").contains("crossFilter: both"));
}
