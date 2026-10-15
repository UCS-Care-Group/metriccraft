use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use serde_json::json;

use super::*;
use crate::Document;

/// A fresh directory under the system temp dir, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("metriccraft-recovery-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    fn text(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const SALES: &str = "Region,Qty,Price\nNorth,3,1.5\nNorth,4,2.5\nSouth,5,3.5\n";
const REGIONS: &str = "Region,Code\nNorth,1\nSouth,2\n";
const MIN: u64 = 60_000;

/// Sales (inside the Project folder `Proj`) and Regions (outside it), loaded.
fn app(dir: &Dir) -> App {
    fs::create_dir_all(dir.path("Proj/data")).unwrap();
    fs::write(dir.path("Proj/data/Sales.csv"), SALES).unwrap();
    fs::write(dir.path("Regions.csv"), REGIONS).unwrap();
    let mut app = App::new();
    for path in ["Proj/data/Sales.csv", "Regions.csv"] {
        app.run(
            "home.getData",
            &json!({"kind": "csv", "path": dir.text(path)}),
        )
        .unwrap();
    }
    app
}

fn saved(dir: &Dir) -> App {
    let mut app = app(dir);
    app.run("file.saveAs", &json!({"path": dir.text("Proj")}))
        .unwrap();
    app
}

/// Edits that leave Sales Stale (an unapplied step) and change the model.
fn edit(app: &mut App) {
    app.run(
        "transform.renameColumn",
        &json!({"query": "Sales", "column": "Qty", "name": "Units"}),
    )
    .unwrap();
    app.run(
        "columnTools.format",
        &json!({"table": "Regions", "column": "Code", "formatString": "0"}),
    )
    .unwrap();
}

fn entries(dir: &Path) -> Vec<String> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<String> = rd
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn mtime(path: &Path) -> SystemTime {
    fs::metadata(path).unwrap().modified().unwrap()
}

fn recover(dir: &Dir, id: &str) -> App {
    let mut app = App::new();
    app.run("file.recover", &json!({"id": id, "dir": dir.text("rec")}))
        .unwrap();
    app
}

#[test]
fn a_killed_session_recovers_its_definitions_data_and_stale_flags() {
    let dir = Dir::new();
    let rec = dir.path("rec");
    let mut app = saved(&dir);
    edit(&mut app);
    assert!(app.is_dirty());
    assert!(app.write_recovery(&rec, 1_000).unwrap());
    let original: Document = app.document().clone();
    let tracked = fs::read(dir.path("Proj/queries/Sales.mcq")).unwrap();
    drop(app); // killed: no close

    assert_eq!(
        fs::read(dir.path("Proj/queries/Sales.mcq")).unwrap(),
        tracked,
        "the Project is untouched"
    );
    let list = App::recoverable(&rec);
    assert_eq!(list.len(), 1);
    let info = &list[0];
    assert_eq!(info.problem, None);
    assert_eq!(info.saved_at, Some(1_000));
    assert_eq!(info.tables, ["Sales", "Regions"]);
    assert_eq!(info.location, Some(Location::Folder(dir.path("Proj"))));
    assert!(info.id.starts_with("project-"), "{}", info.id);

    let mut back = recover(&dir, &info.id);
    assert_eq!(back.document(), &original, "data, Stale flags, provenance");
    assert_eq!(back.document().stale.iter().collect::<Vec<_>>(), ["Sales"]);
    assert_eq!(back.location(), Some(&Location::Folder(dir.path("Proj"))));
    assert!(back.is_dirty(), "must be saved");
    assert!(!back.can_undo());
    // Nothing new to write: the copy holds it.
    assert!(!back.write_recovery(&rec, 2_000).unwrap());

    // Saving goes to the original Project and deletes the copy.
    back.run("file.save", &Value::Null).unwrap();
    assert!(!back.is_dirty());
    assert!(App::recoverable(&rec).is_empty());
    let mut reopened = App::new();
    reopened
        .run("file.open", &json!({"path": dir.text("Proj")}))
        .unwrap();
    assert_eq!(reopened.document(), &original);
}

#[test]
fn a_never_saved_document_recovers_without_a_location() {
    let dir = Dir::new();
    let rec = dir.path("rec");
    let mut app = app(&dir);
    edit(&mut app);
    assert!(app.write_recovery(&rec, 5).unwrap());
    let original = app.document().clone();
    drop(app);
    let list = App::recoverable(&rec);
    assert_eq!(list.len(), 1);
    assert!(list[0].id.starts_with("session-"), "{}", list[0].id);
    assert_eq!(list[0].location, None);
    let back = recover(&dir, &list[0].id);
    assert_eq!(back.document(), &original);
    assert_eq!(back.location(), None);
    assert!(back.is_dirty());
}

#[test]
fn nothing_is_written_when_clean() {
    let dir = Dir::new();
    let rec = dir.path("rec");
    let mut fresh = App::new();
    assert!(!fresh.is_dirty());
    assert!(!fresh.autosave_due(u64::MAX));
    assert!(!fresh.write_recovery(&rec, 0).unwrap());

    let mut app = saved(&dir);
    assert!(!app.is_dirty(), "just saved");
    assert!(!app.autosave_due(10 * MIN));
    assert!(!app.write_recovery(&rec, 0).unwrap());
    assert!(entries(&rec).is_empty());

    let mut opened = App::new();
    opened
        .run("file.open", &json!({"path": dir.text("Proj")}))
        .unwrap();
    assert!(!opened.write_recovery(&rec, 0).unwrap());
    assert!(entries(&rec).is_empty());

    // Changed: due at once, then not again until it changes and the interval passes.
    edit(&mut app);
    assert!(app.autosave_due(0));
    assert!(app.write_recovery(&rec, 0).unwrap());
    assert!(!app.autosave_due(10 * MIN), "nothing new");
    assert!(!app.write_recovery(&rec, 1).unwrap());
    app.run("edit.undo", &Value::Null).unwrap();
    assert!(!app.autosave_due(4 * MIN), "within 5 minutes");
    assert!(app.autosave_due(5 * MIN));
    app.run("options.autosaveMinutes", &json!({"minutes": 0}))
        .unwrap();
    assert!(!app.autosave_due(u64::MAX), "off");
    app.run("options.autosaveMinutes", &json!({"minutes": 1}))
        .unwrap();
    assert_eq!(app.autosave_minutes(), 1);
    assert!(app.autosave_due(MIN));
    for bad in [json!({}), json!({"minutes": -1}), json!({"minutes": 1441})] {
        assert!(app.run("options.autosaveMinutes", &bad).is_err(), "{bad}");
    }
    assert_eq!(app.autosave_minutes(), 1);
}

#[test]
fn unchanged_tables_are_not_rewritten() {
    let dir = Dir::new();
    let rec = dir.path("rec");
    let mut app = saved(&dir);
    edit(&mut app);
    app.write_recovery(&rec, 0).unwrap();
    let id = App::recoverable(&rec)[0].id.clone();
    let copy = rec.join(&id);
    let sales = copy.join(".cache/Sales.parquet");
    let regions = copy.join(".cache/Regions.parquet");
    let before = (mtime(&sales), mtime(&regions));

    // A model edit only: definitions rewritten, no table data.
    app.run(
        "model.setHidden",
        &json!({"table": "Sales", "column": "Price"}),
    )
    .unwrap();
    assert!(app.write_recovery(&rec, MIN).unwrap());
    assert_eq!((mtime(&sales), mtime(&regions)), before);
    assert!(
        fs::read_to_string(copy.join("model/tables/Sales.mcm"))
            .unwrap()
            .contains("hidden")
    );

    // New data for Regions only.
    app.run(
        "columnTools.dataType",
        &json!({"table": "Regions", "column": "Code", "type": "decimal"}),
    )
    .unwrap();
    assert!(app.write_recovery(&rec, 2 * MIN).unwrap());
    assert_eq!(mtime(&sales), before.0, "Sales reused");
    assert_ne!(mtime(&regions), before.1, "Regions rewritten");
    let expected = app.document().clone();
    drop(app);

    // A recovered copy keeps reusing what it holds.
    let mut back = recover(&dir, &id);
    assert_eq!(back.document(), &expected);
    let now = (mtime(&sales), mtime(&regions));
    back.run(
        "model.setHidden",
        &json!({"table": "Regions", "column": "Code"}),
    )
    .unwrap();
    assert!(back.write_recovery(&rec, 3 * MIN).unwrap());
    assert_eq!((mtime(&sales), mtime(&regions)), now);
    assert_eq!(App::recoverable(&rec).len(), 1, "same copy");
}

#[test]
fn discard_close_save_and_open_delete_the_copy() {
    let dir = Dir::new();
    let rec = dir.path("rec");
    let mut app = saved(&dir);
    edit(&mut app);
    app.write_recovery(&rec, 0).unwrap();
    let id = App::recoverable(&rec)[0].id.clone();
    let mut other = App::new();
    assert!(
        other
            .run(
                "file.discardRecovery",
                &json!({"id": "nope", "dir": dir.text("rec")})
            )
            .is_err()
    );
    other
        .run(
            "file.discardRecovery",
            &json!({"id": id, "dir": dir.text("rec")}),
        )
        .unwrap();
    assert!(App::recoverable(&rec).is_empty());

    // close deletes; dropping doesn't.
    app.run("edit.undo", &Value::Null).unwrap();
    app.write_recovery(&rec, 0).unwrap();
    assert_eq!(App::recoverable(&rec).len(), 1);
    app.close().unwrap();
    assert!(App::recoverable(&rec).is_empty());

    let mut app = saved(&dir);
    edit(&mut app);
    app.write_recovery(&rec, 0).unwrap();
    app.run("file.open", &json!({"path": dir.text("Proj")}))
        .unwrap();
    assert!(App::recoverable(&rec).is_empty(), "open replaced it");
    edit(&mut app);
    app.write_recovery(&rec, 0).unwrap();
    app.run("file.new", &Value::Null).unwrap();
    assert!(App::recoverable(&rec).is_empty(), "new replaced it");
}

#[test]
fn a_corrupt_copy_is_listed_with_a_problem_never_a_crash() {
    let dir = Dir::new();
    let rec = dir.path("rec");
    let write = |dir: &Dir| {
        let mut app = app(dir);
        edit(&mut app);
        app.write_recovery(&dir.path("rec"), 0).unwrap();
        let list = App::recoverable(&dir.path("rec"));
        assert_eq!(list.len(), 1);
        list[0].id.clone()
    };
    // recovery.json: garbage, missing (interrupted), wrong shape.
    for (bytes, problem) in [
        (Some(&b"{ nope"[..]), "not valid JSON"),
        (None, "interrupted"),
        (
            Some(&br#"{"formatVersion": 1, "savedAt": "x"}"#[..]),
            "savedAt",
        ),
        (Some(&br#"{"formatVersion": 9}"#[..]), "format version"),
    ] {
        let id = write(&dir);
        let info = rec.join(&id).join(INFO);
        match bytes {
            Some(b) => fs::write(&info, b).unwrap(),
            None => fs::remove_file(&info).unwrap(),
        }
        let list = App::recoverable(&rec);
        let entry = list.iter().find(|i| i.id == id).unwrap();
        let why = entry.problem.as_deref().unwrap();
        assert!(why.contains(problem), "{why}");
        let mut app = App::new();
        let before = app.document().clone();
        let err = app
            .run("file.recover", &json!({"id": id, "dir": dir.text("rec")}))
            .unwrap_err();
        assert!(err.to_string().contains(problem), "{err}");
        assert_eq!(app.document(), &before);
        app.discard_recovery(&rec, &id).unwrap();
    }

    // Damaged definitions: an error. Damaged data: opens empty and Stale, with a notice.
    let id = write(&dir);
    fs::write(rec.join(&id).join("queries/Sales.mcq"), "garbage").unwrap();
    let mut app = App::new();
    assert!(app.recover(&rec, &id).is_err());
    app.discard_recovery(&rec, &id).unwrap();
    let id = write(&dir);
    fs::write(rec.join(&id).join(".cache/Sales.parquet"), "garbage").unwrap();
    let mut app = App::new();
    app.recover(&rec, &id).unwrap();
    assert!(app.notice().unwrap().contains("`Sales`"));
    assert!(app.document().stale.contains("Sales"));
    app.discard_recovery(&rec, &id).unwrap();

    // Stray files and hostile ids.
    fs::write(rec.join("loose.txt"), "x").unwrap();
    fs::create_dir_all(rec.join("bad name")).unwrap();
    assert!(App::recoverable(&rec).is_empty());
    assert!(App::recoverable(&dir.path("missing")).is_empty());
    let mut app = App::new();
    for id in ["", "..", "../rec", "a/b", "x".repeat(65).as_str()] {
        assert!(app.recover(&rec, id).is_err(), "{id}");
        assert!(app.discard_recovery(&rec, id).is_err(), "{id}");
    }
    assert!(rec.is_dir());
}

#[test]
fn the_copy_never_goes_inside_the_project() {
    let dir = Dir::new();
    let mut app = saved(&dir);
    edit(&mut app);
    assert!(app.write_recovery(&dir.path("Proj/rec"), 0).is_err());
    assert!(!dir.path("Proj/rec").exists());
}

#[test]
fn the_recovery_dir_is_the_user_cache() {
    // Only shape: the environment is the test runner's.
    if let Some(d) = recovery_dir() {
        assert!(d.is_absolute());
        assert!(d.ends_with("metriccraft/recovery"));
    }
}
