use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::json;

use super::*;

/// A fresh directory under the system temp dir, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new() -> Dir {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("opendash-file-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Dir(dir)
    }

    fn path(&self, rel: &str) -> String {
        self.0.join(rel).display().to_string()
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const SALES: &str = "Region,Qty,Price\nNorth,3,1.5\nNorth,4,2.5\nSouth,5,3.5\n";
const REGIONS: &str = "Region,Code\nNorth,1\nSouth,2\n";

/// Sales (inside the Project folder `Proj`) and Regions (outside it), loaded and edited.
fn app(dir: &Dir) -> App {
    std::fs::create_dir_all(dir.0.join("Proj/data")).unwrap();
    std::fs::write(dir.0.join("Proj/data/Sales.csv"), SALES).unwrap();
    std::fs::write(dir.0.join("Regions.csv"), REGIONS).unwrap();
    let mut app = App::new();
    for (path, culture) in [("Proj/data/Sales.csv", "en-GB"), ("Regions.csv", "en-US")] {
        app.run(
            "home.getData",
            &json!({"kind": "csv", "path": dir.path(path), "culture": culture}),
        )
        .unwrap();
    }
    app.run(
        "columnTools.format",
        &json!({"table": "Sales", "column": "Price", "formatString": "#,0.00"}),
    )
    .unwrap();
    app.run(
        "model.setHidden",
        &json!({"table": "Regions", "column": "Code"}),
    )
    .unwrap();
    // A Model type override, cached as applied.
    app.run(
        "columnTools.dataType",
        &json!({"table": "Sales", "column": "Price", "type": "fixedDecimal"}),
    )
    .unwrap();
    app
}

/// Every file under `root`, by relative path.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                let rel = path.strip_prefix(root).unwrap().display().to_string();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    out
}

#[test]
fn save_and_open_a_project_folder() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    assert_eq!(app.location(), Some(&Location::Folder(dir.0.join("Proj"))));
    let sales = std::fs::read_to_string(dir.0.join("Proj/queries/Sales.odq")).unwrap();
    assert!(
        sales.contains(r#"File.Contents("data/Sales.csv")"#),
        "{sales}"
    );
    assert!(sales.contains("culture: en-GB"), "{sales}");

    for f in [
        ".cache/Sales.parquet",
        ".cache/Sales.meta",
        ".cache/Regions.parquet",
    ] {
        assert!(dir.0.join("Proj").join(f).is_file(), "{f}");
    }
    let meta = std::fs::read_to_string(dir.0.join("Proj/.cache/Sales.meta")).unwrap();
    assert!(meta.contains(r#""path": "data/Sales.csv""#), "{meta}");
    assert!(meta.contains(r#""queryType": "decimal""#), "{meta}");

    let mut other = App::new();
    other
        .run("file.open", &json!({"path": dir.path("Proj")}))
        .unwrap();
    assert_eq!(other.document(), &original, "data, Stale flags, provenance");
    assert_eq!(other.notice(), None);
    assert!(!other.can_undo(), "open is not undoable");
}

/// Sets `path`'s modified time.
fn set_mtime(path: &Path, t: std::time::SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

fn mtime(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

fn open(dir: &Dir, path: &str) -> App {
    let mut app = App::new();
    app.run("file.open", &json!({"path": dir.path(path)}))
        .unwrap();
    app
}

fn stale(app: &App) -> Vec<&str> {
    app.document().stale.iter().map(String::as_str).collect()
}

#[test]
fn reopening_loads_from_the_cache_without_reading_sources() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    // Same size and modified time, different bytes: only reading the Source would notice.
    let sales = dir.0.join("Proj/data/Sales.csv");
    let t = mtime(&sales);
    std::fs::write(&sales, "x".repeat(SALES.len())).unwrap();
    set_mtime(&sales, t);
    let other = open(&dir, "Proj");
    assert_eq!(other.document().tables, original.tables);
    assert!(other.document().stale.is_empty());
}

#[test]
fn a_touched_source_reopens_stale_with_its_cached_data() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let regions = dir.0.join("Regions.csv");
    set_mtime(
        &regions,
        mtime(&regions) + std::time::Duration::from_secs(60),
    );
    std::fs::write(
        dir.0.join("Proj/data/Sales.csv"),
        format!("{SALES}West,1,1\n"),
    )
    .unwrap();
    let other = open(&dir, "Proj");
    assert_eq!(stale(&other), ["Regions", "Sales"]);
    assert_eq!(
        other.document().tables,
        original.tables,
        "cached data shown"
    );
    assert_eq!(
        other.notice(),
        Some("2 tables are out of date since they loaded (Stale): Refresh to update")
    );
}

#[test]
fn unapplied_edits_reopen_stale_with_the_data_as_loaded() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let parquet = dir.0.join("Proj/.cache/Sales.parquet");
    let meta = dir.0.join("Proj/.cache/Sales.meta");
    let before = (mtime(&parquet), std::fs::read(&meta).unwrap());
    app.run(
        "transform.renameColumn",
        &json!({"query": "Sales", "column": "Qty", "name": "Units"}),
    )
    .unwrap();
    app.run("file.save", &Value::Null).unwrap();
    // The cache describes the data as loaded, which hasn't changed: nothing is rewritten.
    assert_eq!(mtime(&parquet), before.0);
    assert_eq!(std::fs::read(&meta).unwrap(), before.1);

    let mut other = open(&dir, "Proj");
    assert_eq!(stale(&other), ["Sales"]);
    assert_eq!(other.document().tables, original.tables);
    assert_eq!(
        other.notice(),
        Some("1 table is out of date since it loaded (Stale): Refresh to update")
    );
    // Close & Apply loads it; the next save writes the new data.
    other.run("home.closeApply", &Value::Null).unwrap();
    assert!(other.document().stale.is_empty());
    other.run("file.save", &Value::Null).unwrap();
    assert!(open(&dir, "Proj").document().stale.is_empty());
}

#[test]
fn a_deleted_cache_reopens_empty_and_stale() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    std::fs::remove_dir_all(dir.0.join("Proj/.cache")).unwrap();
    let mut other = open(&dir, "Proj");
    let doc = other.document();
    assert_eq!(doc.queries, original.queries);
    assert_eq!(doc.model, original.model);
    assert_eq!(stale(&other), ["Regions", "Sales"]);
    assert!(other.document().tables.iter().all(|t| t.rows() == 0));
    assert_eq!(
        other.notice(),
        Some("2 queries have no data yet: Refresh to load")
    );
    // Nothing loaded, nothing cached.
    other.run("file.save", &Value::Null).unwrap();
    assert!(!dir.0.join("Proj/.cache/Sales.meta").exists());

    other.run("home.closeApply", &Value::Null).unwrap();
    assert_eq!(other.document().tables, original.tables);
    assert_eq!(other.document().model, original.model);
    assert!(other.document().stale.is_empty());
}

#[test]
fn a_corrupt_cache_gives_an_empty_stale_table_and_a_notice() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let parquet = dir.0.join("Proj/.cache/Sales.parquet");
    let mut bytes = std::fs::read(&parquet).unwrap();
    for i in (8..bytes.len()).step_by(5) {
        bytes[i] ^= 0xa5;
    }
    std::fs::write(&parquet, bytes).unwrap();
    std::fs::write(dir.0.join("Proj/.cache/Regions.meta"), "{ not json").unwrap();
    let other = open(&dir, "Proj");
    assert_eq!(stale(&other), ["Regions", "Sales"]);
    assert!(other.document().tables.iter().all(|t| t.rows() == 0));
    let notice = other.notice().unwrap();
    assert!(
        notice.starts_with("the Data cache can't be used for `Sales` (Sales: corrupt file"),
        "{notice}"
    );
    assert!(
        notice.contains("`Regions` (metadata is not valid JSON"),
        "{notice}"
    );
    assert!(
        notice.ends_with("2 queries have no data yet: Refresh to load"),
        "{notice}"
    );
    assert_eq!(other.document().model, original.model);
}

#[test]
fn opening_holds_the_cache_to_the_memory_cap() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    app.run("file.saveAs", &json!({"path": dir.path("P.odash")}))
        .unwrap();
    for path in ["Proj", "P.odash"] {
        let mut other = App::new();
        other.set_memory_cap(64);
        other
            .run("file.open", &json!({"path": dir.path(path)}))
            .unwrap();
        assert_eq!(stale(&other), ["Regions", "Sales"], "{path}");
        assert!(other.document().tables.iter().all(|t| t.rows() == 0));
        assert!(other.notice().unwrap().contains("memory limit"), "{path}");
    }
    // Data over the cap isn't even unpacked.
    let mut tiny = App::new();
    tiny.set_memory_cap(1);
    tiny.open_package_bytes(&app.package_bytes().unwrap())
        .unwrap();
    assert!(tiny.document().tables.iter().all(|t| t.rows() == 0));
}

#[test]
fn saving_twice_writes_identical_files() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let first = snapshot(&dir.0.join("Proj"));
    assert!(first.contains_key("project.odash.json") && first.contains_key(".gitignore"));
    let cache = [
        ".cache/Sales.parquet",
        ".cache/Sales.meta",
        ".cache/Regions.parquet",
    ]
    .map(|f| dir.0.join("Proj").join(f));
    let times = cache.clone().map(|f| mtime(&f));
    app.run("file.save", &json!({})).unwrap();
    assert_eq!(snapshot(&dir.0.join("Proj")), first);
    assert_eq!(
        cache.clone().map(|f| mtime(&f)),
        times,
        "cache not rewritten"
    );

    // Open and save again: still identical.
    let mut other = App::new();
    other
        .run(
            "file.open",
            &json!({"path": dir.path("Proj/project.odash.json")}),
        )
        .unwrap();
    other.run("file.save", &Value::Null).unwrap();
    assert_eq!(snapshot(&dir.0.join("Proj")), first);
    assert_eq!(cache.map(|f| mtime(&f)), times, "cache not rewritten");
}

#[test]
fn save_and_open_a_package() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let original = app.document().clone();
    app.run("file.saveAs", &json!({"path": dir.path("Sales.odash")}))
        .unwrap();
    assert_eq!(
        app.location(),
        Some(&Location::Package(dir.0.join("Sales.odash")))
    );
    let bytes = std::fs::read(dir.0.join("Sales.odash")).unwrap();
    assert_eq!(&bytes[..2], b"PK");

    let mut other = App::new();
    other
        .run("file.open", &json!({"path": dir.path("Sales.odash")}))
        .unwrap();
    assert_eq!(other.document(), &original, "with its data");
    other.run("file.save", &Value::Null).unwrap();
    assert_eq!(std::fs::read(dir.0.join("Sales.odash")).unwrap(), bytes);
    let files = opendash_file::read_package(&dir.0.join("Sales.odash")).unwrap();
    assert!(files.contains_key(".cache/Sales.parquet"));

    // Without data: definitions only; opens empty and Stale.
    app.run(
        "file.saveAs",
        &json!({"path": dir.path("Bare.odash"), "data": false}),
    )
    .unwrap();
    let files = opendash_file::read_package(&dir.0.join("Bare.odash")).unwrap();
    assert!(!files.keys().any(|k| k.starts_with(".cache")), "{files:?}");
    let bare = open(&dir, "Bare.odash");
    assert_eq!(bare.document().model, original.model);
    assert_eq!(stale(&bare), ["Regions", "Sales"]);
    assert!(bare.document().tables.iter().all(|t| t.rows() == 0));

    // Package → Project folder → Package.
    other
        .run(
            "file.saveAs",
            &json!({"path": dir.path("Unpacked"), "package": false}),
        )
        .unwrap();
    assert!(dir.0.join("Unpacked/project.odash.json").exists());
    assert_eq!(open(&dir, "Unpacked").document().tables, original.tables);
}

#[test]
fn the_web_build_opens_and_saves_package_bytes() {
    let dir = Dir::new();
    let app = app(&dir);
    let bytes = app.package_bytes().unwrap();
    let mut web = App::new();
    web.open_package_bytes(&bytes).unwrap();
    assert_eq!(web.document().model, app.document().model);
    assert_eq!(web.location(), None);
    assert_eq!(web.package_bytes().unwrap(), bytes);
    assert!(matches!(
        web.open_package_bytes(b"junk"),
        Err(Error::File(opendash_file::Error::Package(_)))
    ));
    assert_eq!(
        web.document().model,
        app.document().model,
        "a failed open changes nothing"
    );
}

#[test]
fn an_interrupted_save_is_reported_and_recoverable() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    // The state a save leaves after moving one part in, before the Manifest.
    let part = dir.0.join("Proj/queries/Regions.odq");
    let old = dir.0.join("Proj/.staging/old/queries/Regions.odq");
    std::fs::create_dir_all(old.parent().unwrap()).unwrap();
    std::fs::rename(&part, &old).unwrap();
    std::fs::write(&part, "query Regions\n  load: false\n").unwrap();

    let mut other = App::new();
    let e = other
        .run("file.open", &json!({"path": dir.path("Proj")}))
        .unwrap_err();
    assert_eq!(
        e,
        Error::File(opendash_file::Error::Interrupted {
            parts: vec!["queries/Regions.odq".into()],
            staging: false
        })
    );
    assert!(e.to_string().contains("interrupted"), "{e}");
    other
        .run(
            "file.open",
            &json!({"path": dir.path("Proj"), "recover": "lastGood"}),
        )
        .unwrap();
    assert_eq!(other.document().model, app.document().model);
    assert!(matches!(
        other.run(
            "file.open",
            &json!({"path": dir.path("Proj"), "recover": "later"})
        ),
        Err(Error::InvalidParams { .. })
    ));
}

#[test]
fn save_without_a_location_and_bad_params_are_errors() {
    let dir = Dir::new();
    let mut app = app(&dir);
    for (id, params) in [
        ("file.save", json!({})),
        ("file.saveAs", json!({})),
        (
            "file.saveAs",
            json!({"path": dir.path("x"), "package": "yes"}),
        ),
        ("file.open", json!({})),
        ("file.open", json!({"path": dir.path("missing.odash")})),
        ("file.open", json!({"path": dir.path("Regions.csv")})),
    ] {
        let before = app.document().clone();
        assert!(app.run(id, &params).is_err(), "{id} {params}");
        assert_eq!(app.document(), &before);
    }
}

#[test]
fn new_starts_an_empty_document() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    app.run("file.new", &Value::Null).unwrap();
    assert_eq!(app.document(), &Document::default());
    assert!(!app.can_undo());
    assert_eq!(app.location(), None);
}

// ---- Local overrides: source-path remapping (M2.9) ----

/// Every tracked file of a Project (not `.cache/`, `.local/`).
fn tracked(root: &Path) -> BTreeMap<String, Vec<u8>> {
    snapshot(root)
        .into_iter()
        .filter(|(k, _)| !k.starts_with(".cache") && !k.starts_with(".local"))
        .collect()
}

/// Copies the tracked files of Project `from` to a new folder `to` (a clone: no Data cache).
fn clone_project(from: &Path, to: &Path) {
    for (rel, bytes) in tracked(from) {
        let path = to.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}

#[test]
fn a_remapped_project_loads_its_sources_without_touching_tracked_files() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let original = app.document().clone();
    // A second machine: a clone, with Sales somewhere else.
    clone_project(&dir.0.join("Proj"), &dir.0.join("B"));
    std::fs::create_dir_all(dir.0.join("share")).unwrap();
    std::fs::write(dir.0.join("share/Sales.csv"), SALES).unwrap();
    let before = tracked(&dir.0.join("B"));

    let mut b = open(&dir, "B");
    assert_eq!(stale(&b), ["Regions", "Sales"]);
    b.run(
        "file.remapSource",
        &json!({"from": "data/Sales.csv", "to": dir.path("share/Sales.csv")}),
    )
    .unwrap();
    assert!(b.notice().unwrap().contains("Refresh"), "{:?}", b.notice());
    assert!(!b.can_undo(), "not undoable");
    let paths = std::fs::read_to_string(dir.0.join("B/.local/paths.odl")).unwrap();
    assert_eq!(
        paths,
        format!(
            "remap 'data/Sales.csv'\n  to: '{}'\n",
            dir.path("share/Sales.csv")
        )
    );
    b.run("home.refresh", &Value::Null).unwrap();
    assert!(b.document().stale.is_empty());
    assert_eq!(b.document().tables, original.tables);
    b.run("file.save", &Value::Null).unwrap();
    assert_eq!(tracked(&dir.0.join("B")), before, "tracked files untouched");
    let meta = std::fs::read_to_string(dir.0.join("B/.cache/Sales.meta")).unwrap();
    assert!(meta.contains(r#""path": "data/Sales.csv""#), "{meta}");

    // Reopened: the remap applies again; nothing Stale.
    let again = open(&dir, "B");
    assert!(again.document().stale.is_empty(), "{:?}", stale(&again));
    assert_eq!(again.document().tables, original.tables);
    assert_eq!(again.notice(), None);
    assert_eq!(
        again.document().queries[0].source.file(),
        &opendash_data::source::File::Path(dir.0.join("share/Sales.csv"))
    );

    // A Package never carries `.local/` and keeps the Project's paths.
    let files = opendash_file::unpack(&again.package_bytes().unwrap()).unwrap();
    assert!(files.keys().all(|k| !k.starts_with(".local")));
    let odq = String::from_utf8(files["queries/Sales.odq"].clone()).unwrap();
    let path = dir.0.join("B/data/Sales.csv").display().to_string();
    assert!(odq.contains(&path), "{odq}");
}

#[test]
fn a_remap_alone_never_makes_a_table_stale() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    // The file moves (a rename keeps its size and modified time).
    std::fs::rename(dir.0.join("Proj/data/Sales.csv"), dir.0.join("moved.csv")).unwrap();
    let mut other = open(&dir, "Proj");
    assert_eq!(stale(&other), ["Sales"], "its file is gone");
    other
        .run(
            "file.remapSource",
            &json!({"from": "data/Sales.csv", "to": dir.path("moved.csv")}),
        )
        .unwrap();
    assert_eq!(stale(&other), ["Sales"], "the command changes no flag");
    let reopened = open(&dir, "Proj");
    assert!(
        reopened.document().stale.is_empty(),
        "{:?}",
        stale(&reopened)
    );

    // Removing the remap: the Project's own path again, Stale (the file isn't there).
    let mut other = open(&dir, "Proj");
    other
        .run(
            "file.remapSource",
            &json!({"from": "data/Sales.csv", "to": null}),
        )
        .unwrap();
    assert!(!dir.0.join("Proj/.local/paths.odl").exists());
    assert_eq!(
        other.document().queries[0].source.file(),
        &opendash_data::source::File::Path(dir.0.join("Proj/data/Sales.csv"))
    );
    assert_eq!(stale(&open(&dir, "Proj")), ["Sales"]);
}

#[test]
fn remap_needs_a_project_folder_and_good_params() {
    let dir = Dir::new();
    let mut app = app(&dir);
    let p = json!({"from": "a.csv", "to": "b.csv"});
    assert!(app.run("file.remapSource", &p).is_err(), "unsaved");
    app.run("file.saveAs", &json!({"path": dir.path("P.odash")}))
        .unwrap();
    assert!(app.run("file.remapSource", &p).is_err(), "a Package");
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    for bad in [
        json!({}),
        json!({"from": ""}),
        json!({"from": "a\nb", "to": "c"}),
        json!({"from": "a", "to": 3}),
        json!({"from": "a", "to": " "}),
    ] {
        assert!(app.run("file.remapSource", &bad).is_err(), "{bad}");
    }
    assert!(!dir.0.join("Proj/.local").exists());
    // A broken paths.odl is ignored, with a notice.
    std::fs::create_dir_all(dir.0.join("Proj/.local")).unwrap();
    std::fs::write(dir.0.join("Proj/.local/paths.odl"), "nonsense here\n").unwrap();
    let other = open(&dir, "Proj");
    let notice = other.notice().unwrap();
    assert!(notice.contains(".local/paths.odl is ignored"), "{notice}");
    assert!(other.document().stale.is_empty());
}

#[test]
fn rename_then_save_and_open_keeps_tables_fresh() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run(
        "home.mergeQueries",
        &json!({"query": "Sales", "keys": ["Region"], "with": "Regions", "withKeys": ["Region"]}),
    )
    .unwrap();
    app.run("home.closeApply", &Value::Null).unwrap();
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    app.run(
        "modeling.renameTable",
        &json!({"table": "Regions", "name": "Areas"}),
    )
    .unwrap();
    assert!(app.document().provenance.contains_key("Areas"));
    assert!(!app.document().provenance.contains_key("Regions"));
    app.run("file.save", &Value::Null).unwrap();
    let other = open(&dir, "Proj");
    assert!(other.document().stale.is_empty(), "{:?}", stale(&other));
    assert_eq!(other.notice(), None);
    assert_eq!(other.document(), app.document());
}
