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

    let mut other = App::new();
    other
        .run("file.open", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let doc = other.document();
    assert_eq!(doc.queries, original.queries);
    assert_eq!(doc.model, original.model);
    assert_eq!(doc.unloaded, original.unloaded);
    assert_eq!(
        doc.stale.iter().map(String::as_str).collect::<Vec<_>>(),
        ["Regions", "Sales"],
        "no Data cache yet: everything is Stale"
    );
    assert!(doc.tables.iter().all(|t| t.rows() == 0));
    assert_eq!(
        other.notice(),
        Some("2 queries have no data yet: Refresh to load")
    );
    assert!(!other.can_undo(), "open is not undoable");

    other.run("home.closeApply", &Value::Null).unwrap();
    assert_eq!(other.document().tables, original.tables);
    assert_eq!(other.document().model, original.model);
    assert!(other.document().stale.is_empty());
}

#[test]
fn saving_twice_writes_identical_files() {
    let dir = Dir::new();
    let mut app = app(&dir);
    app.run("file.saveAs", &json!({"path": dir.path("Proj")}))
        .unwrap();
    let first = snapshot(&dir.0.join("Proj"));
    assert!(first.contains_key("project.odash.json") && first.contains_key(".gitignore"));
    app.run("file.save", &json!({})).unwrap();
    assert_eq!(snapshot(&dir.0.join("Proj")), first);

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
    assert_eq!(other.document().queries, original.queries);
    assert_eq!(other.document().model, original.model);
    other.run("file.save", &Value::Null).unwrap();
    assert_eq!(std::fs::read(dir.0.join("Sales.odash")).unwrap(), bytes);

    // Package → Project folder → Package.
    other
        .run(
            "file.saveAs",
            &json!({"path": dir.path("Unpacked"), "package": false}),
        )
        .unwrap();
    assert!(dir.0.join("Unpacked/project.odash.json").exists());
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
