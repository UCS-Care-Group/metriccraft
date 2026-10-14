use std::path::{Path, PathBuf};

use opendash_data::source::ExcelKind;
use opendash_data::step::Step;

use super::*;
use crate::manifest::Migration;

fn culture(name: &str) -> &'static Culture {
    Culture::get(name).unwrap()
}

fn root() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\work\Sales")
    } else {
        PathBuf::from("/work/Sales")
    }
}

fn elsewhere() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"D:\shared\regions.xlsx")
    } else {
        PathBuf::from("/shared/regions.xlsx")
    }
}

fn parquet(path: PathBuf) -> Source {
    Source::Parquet {
        file: File::Path(path),
    }
}

fn query(name: &str, source: Source, steps: Vec<Step>, c: &str) -> Query {
    Query {
        name: name.into(),
        source,
        steps,
        culture: culture(c),
    }
}

/// Queries `Sales` (en-GB, inside the Project), `Unit Regions` (outside), `sales` (case
/// collision with `Sales`) and `CON` (reserved name, Enable load off).
fn sample() -> Project {
    let mut model = Model::default();
    let mut qty = Column::new("Qty", ColumnType::Whole);
    qty.data_type = ColumnType::Decimal;
    let mut price = Column::new("Unit Price", ColumnType::FixedDecimal);
    price.format_string = "#,0.00".into();
    price.hidden = true;
    price.description = "Price \"each\"\nper unit".into();
    let mut region = Column::new("Region", ColumnType::Text);
    region.sort_by = Some("Region Order".into());
    let order = Column::new("Region Order", ColumnType::Whole);
    model.tables.push(ModelTable {
        name: "Sales".into(),
        hidden: false,
        description: "Every sale".into(),
        columns: vec![region.clone(), qty, price, order],
        measures: Vec::new(),
    });
    model.tables.push(ModelTable {
        name: "Unit Regions".into(),
        hidden: true,
        description: String::new(),
        columns: vec![Column::new("Region", ColumnType::Text)],
        measures: Vec::new(),
    });
    model.tables.push(ModelTable {
        name: "sales".into(),
        hidden: false,
        description: String::new(),
        columns: vec![Column::new("x]y", ColumnType::Boolean)],
        measures: Vec::new(),
    });
    model.relationships.push(Relationship::new(
        "Sales",
        "Region",
        "Unit Regions",
        "Region",
    ));
    let mut inactive = Relationship::new("sales", "x]y", "Unit Regions", "Region");
    inactive.active = false;
    model.relationships.push(inactive);
    model.autodetect_relationships = false;
    Project {
        culture: "en-US".into(),
        queries: vec![
            query(
                "Unit Regions",
                Source::Excel {
                    file: File::Path(elsewhere()),
                    item: "Sheet1".into(),
                    kind: ExcelKind::Sheet,
                },
                vec![Step::PromotedHeaders],
                "en-US",
            ),
            query(
                "Sales",
                parquet(root().join("data").join("sales.parquet")),
                vec![Step::ChangedType(vec![("Qty".into(), ColumnType::Whole)])],
                "en-GB",
            ),
            query("sales", parquet(root().join("s.parquet")), vec![], "en-US"),
            query("CON", parquet(root().join("c.parquet")), vec![], "en-US"),
        ],
        unloaded: ["CON".to_string()].into(),
        model,
        report: BTreeMap::new(),
    }
}

fn text(files: &FileSet, path: &str) -> String {
    String::from_utf8(files.get(path).unwrap_or_else(|| panic!("{path}")).clone()).unwrap()
}

/// `files` with the Manifest's hashes recomputed for every part it lists plus `add`.
fn rehash(files: &mut FileSet, add: &[&str]) {
    let mut m = Manifest::parse(&files[MANIFEST]).unwrap();
    for p in add {
        m.parts.insert(p.to_string(), String::new());
    }
    for (p, h) in m.parts.iter_mut() {
        *h = hash(&files[p]);
    }
    files.insert(MANIFEST.into(), m.print());
}

fn round_trip(files: &FileSet) -> (Project, Preserved) {
    decode(files, Some(&root())).unwrap()
}

#[test]
fn save_twice_is_byte_identical_and_reopens_the_same() {
    let p = sample();
    let a = encode(&p, Some(&root()), &Preserved::default()).unwrap();
    let b = encode(&p, Some(&root()), &Preserved::default()).unwrap();
    assert_eq!(a, b);
    let (back, pre) = round_trip(&a);
    assert_eq!(back, p, "decode(encode(p)) == p");
    assert_eq!(encode(&back, Some(&root()), &pre).unwrap(), a);
    for (path, bytes) in &a {
        let t = std::str::from_utf8(bytes).unwrap();
        assert!(t.ends_with('\n') && !t.contains('\r'), "{path}");
    }
}

#[test]
fn layout_follows_the_adr() {
    let files = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    let paths: Vec<&str> = files.keys().map(String::as_str).collect();
    let sales_stem = crate::escape::file_stems(&["Sales", "sales"])[1].clone();
    let mut want = vec![
        ".gitignore".to_string(),
        "model/model.odm".into(),
        "model/relationships.odm".into(),
        "model/tables/Sales.odm".into(),
        "model/tables/Unit Regions.odm".into(),
        format!("model/tables/{sales_stem}.odm"),
        "project.odash.json".into(),
        "queries/%43ON.odq".into(),
        "queries/Sales.odq".into(),
        "queries/Unit Regions.odq".into(),
        format!("queries/{sales_stem}.odq"),
    ];
    want.sort();
    assert_eq!(paths, want);
    assert_eq!(text(&files, ".gitignore"), ".cache/\n.local/\n.staging/\n");

    assert_eq!(
        text(&files, "model/tables/Sales.odm"),
        "table Sales\n  \
           description: \"Every sale\"\n  \
           column Region\n    \
             dataType: text\n    \
             summarizeBy: none\n    \
             sortByColumn: 'Region Order'\n  \
           column Qty\n    \
             dataType: decimal\n    \
             queryType: whole\n    \
             summarizeBy: sum\n  \
           column 'Unit Price'\n    \
             dataType: fixedDecimal\n    \
             formatString: \"#,0.00\"\n    \
             summarizeBy: sum\n    \
             description: \"Price \\\"each\\\"\\nper unit\"\n    \
             hidden\n  \
           column 'Region Order'\n    \
             dataType: whole\n    \
             summarizeBy: sum\n"
    );
    assert_eq!(
        text(&files, "model/relationships.odm"),
        "relationship Sales[Region] -> 'Unit Regions'[Region]\n\
         relationship sales[x]]y] -> 'Unit Regions'[Region]\n  inactive\n"
    );
    assert_eq!(
        text(&files, "model/model.odm"),
        "model Model\n  \
           culture: en-US\n  \
           autodetectRelationships: false\n  \
           ref query 'Unit Regions'\n  \
           ref query Sales\n  \
           ref query sales\n  \
           ref query CON\n  \
           ref table Sales\n  \
           ref table 'Unit Regions'\n  \
           ref table sales\n"
    );
    let sales = text(&files, "queries/Sales.odq");
    assert!(sales.starts_with("query Sales\n  load: true\n  culture: en-GB\n  m:\n    let\n"));
    assert!(
        sales.contains(r#"Parquet.Document(File.Contents("data/sales.parquet"))"#),
        "inside the Project: relative, with /\n{sales}"
    );
    let con = text(&files, "queries/%43ON.odq");
    assert!(con.starts_with("query CON\n  load: false\n  m:\n"), "{con}");
    let regions = text(&files, "queries/Unit Regions.odq");
    assert!(
        regions.contains(&elsewhere().display().to_string().replace('\\', "\\\\")),
        "outside the Project: absolute\n{regions}"
    );
    let manifest = text(&files, MANIFEST);
    assert!(manifest.starts_with("{\n  \"formatVersion\": 1,\n  \"parts\": {\n"));
    assert!(!manifest.contains(".gitignore"));
}

#[test]
fn unknown_content_survives_open_and_save() {
    let mut files = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    let sales = text(&files, "model/tables/Sales.odm").replace(
        "  column Qty\n",
        "  lineageTag: abc\n  column Qty\n    annotation Foo\n      value: 1\n    isKey\n",
    ) + "perspective Main\n  showAll\n";
    files.insert("model/tables/Sales.odm".into(), sales.into_bytes());
    let q = text(&files, "queries/Sales.odq").replace(
        "  load: true\n",
        "  load: true\n  note:\n    free text\n      kept as is\n",
    );
    files.insert("queries/Sales.odq".into(), q.into_bytes());
    files.insert("report/report.json".into(), b"{\"pages\": []}\n".to_vec());
    files.insert("model/extra.odm".into(), b"thing X\n  y: 1\n".to_vec());
    rehash(&mut files, &["report/report.json", "model/extra.odm"]);
    let m = text(&files, MANIFEST).replacen('{', "{\n  \"generator\": {\"b\": 1, \"a\": [2]},", 1);
    files.insert(MANIFEST.into(), m.into_bytes());

    let (p, pre) = round_trip(&files);
    assert!(!pre.is_empty());
    assert_eq!(
        p.report.get("report/report.json").map(Vec::as_slice),
        Some(&b"{\"pages\": []}\n"[..]),
        "report parts are carried, opaque"
    );
    let model = Project {
        report: BTreeMap::new(),
        ..p.clone()
    };
    assert_eq!(model, sample(), "unknown content doesn't change the model");
    let saved = encode(&p, Some(&root()), &pre).unwrap();
    let sales = text(&saved, "model/tables/Sales.odm");
    for kept in [
        "  lineageTag: abc\n",
        "    annotation Foo\n      value: 1\n    isKey\n",
        "perspective Main\n  showAll\n",
    ] {
        assert!(sales.contains(kept), "{kept:?} in\n{sales}");
    }
    assert!(
        text(&saved, "queries/Sales.odq").contains("  note:\n    free text\n      kept as is\n")
    );
    assert_eq!(text(&saved, "report/report.json"), "{\"pages\": []}\n");
    assert_eq!(text(&saved, "model/extra.odm"), "thing X\n  y: 1\n");
    let manifest = text(&saved, MANIFEST);
    assert!(
        manifest.contains("\"generator\": {\n    \"a\": [\n      2\n    ],\n    \"b\": 1\n  }")
    );
    assert!(manifest.contains("report/report.json"));

    // And it is stable: open → save again gives the same bytes.
    let (p2, pre2) = round_trip(&saved);
    assert_eq!(encode(&p2, Some(&root()), &pre2).unwrap(), saved);
}

#[test]
fn report_parts_are_carried_opaque_and_listed() {
    let mut project = sample();
    for (path, text) in [
        ("report/report.json", "{\n  \"pages\": [\"a\"]\n}\n"),
        ("report/pages/a/page.json", "not even JSON"),
    ] {
        project.report.insert(path.into(), text.as_bytes().to_vec());
    }
    let files = encode(&project, Some(&root()), &Preserved::default()).unwrap();
    assert!(text(&files, MANIFEST).contains("report/pages/a/page.json"));
    let (back, pre) = round_trip(&files);
    assert_eq!(back, project);
    assert!(
        !pre.parts.keys().any(|p| p.starts_with(REPORT)),
        "report parts are the Project's, not unknown"
    );
    let packed = crate::unpack(&crate::pack(&files).unwrap()).unwrap();
    assert_eq!(packed, files, "a Package carries them");

    let mut bad = sample();
    bad.report.insert("model/x.json".into(), Vec::new());
    assert!(matches!(
        encode(&bad, None, &Preserved::default()),
        Err(Error::Save(_))
    ));
    bad.report.clear();
    bad.report.insert("report/../x".into(), Vec::new());
    assert!(matches!(
        encode(&bad, None, &Preserved::default()),
        Err(Error::Save(_))
    ));
}

#[test]
fn known_items_the_model_dropped_are_not_resurrected() {
    let files = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    let (mut p, pre) = round_trip(&files);
    let t = p.model.table_mut("Sales").unwrap();
    t.description.clear();
    t.columns.retain(|c| c.name != "Qty");
    let saved = encode(&p, Some(&root()), &pre).unwrap();
    let sales = text(&saved, "model/tables/Sales.odm");
    assert!(
        !sales.contains("Every sale") && !sales.contains("Qty"),
        "{sales}"
    );
}

#[test]
fn interrupted_or_damaged_parts_are_detected() {
    let mut files = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    files.insert("queries/Sales.odq".into(), b"query Sales\n".to_vec());
    files.remove("model/model.odm");
    assert_eq!(
        decode(&files, None),
        Err(Error::Damaged {
            parts: vec!["model/model.odm".into(), "queries/Sales.odq".into()]
        })
    );
}

#[test]
fn newer_format_is_refused() {
    let mut files = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    let m = text(&files, MANIFEST).replace("\"formatVersion\": 1", "\"formatVersion\": 2");
    files.insert(MANIFEST.into(), m.into_bytes());
    let e = decode(&files, None).unwrap_err();
    assert_eq!(
        e,
        Error::NewerFormat {
            found: 2,
            supported: 1
        }
    );
    assert!(e.to_string().contains("newer OpenDash BI"));
}

/// Format version 0 (test-only) called `dataType` `type`.
fn v0_to_v1(tree: &mut Tree) -> std::result::Result<(), String> {
    fn rename(items: &mut [Item]) {
        for item in items {
            match item {
                Item::Prop(k, _) if k == "type" => *k = "dataType".into(),
                Item::Node(n) => rename(&mut n.items),
                _ => {}
            }
        }
    }
    for (path, items) in tree.syntax.iter_mut() {
        if path.starts_with(TABLES) {
            rename(items);
        }
    }
    Ok(())
}

#[test]
fn older_formats_migrate_one_step_at_a_time() {
    let p = sample();
    let mut files = encode(&p, Some(&root()), &Preserved::default()).unwrap();
    for (path, bytes) in files.iter_mut() {
        if path.starts_with(TABLES) {
            *bytes = String::from_utf8(bytes.clone())
                .unwrap()
                .replace("dataType:", "type:")
                .into_bytes();
        }
    }
    rehash(&mut files, &[]);
    let m = text(&files, MANIFEST).replace("\"formatVersion\": 1", "\"formatVersion\": 0");
    files.insert(MANIFEST.into(), m.into_bytes());

    assert_eq!(
        decode(&files, Some(&root())),
        Err(Error::NoMigration { from: 0 }),
        "no real migration from 0"
    );
    let migrations = [Migration {
        from: 0,
        run: v0_to_v1,
    }];
    let (back, pre) = decode_with(&files, Some(&root()), &migrations).unwrap();
    assert_eq!(back, p);
    let saved = encode(&back, Some(&root()), &pre).unwrap();
    assert!(text(&saved, MANIFEST).contains("\"formatVersion\": 1"));
    assert_eq!(
        saved,
        encode(&p, Some(&root()), &Preserved::default()).unwrap()
    );
}

#[test]
fn inconsistent_parts_are_errors() {
    let base = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    let cases: &[(&str, &str, &str)] = &[
        (
            "model/relationships.odm",
            "relationship Sales[Nope] -> sales[x]]y]\n",
            "Nope",
        ),
        (
            "model/relationships.odm",
            "relationship Sales[Region]\n",
            "expected Table[Column]",
        ),
        (
            "model/relationships.odm",
            "relationship Sales[Region] -> 'Unit Regions'[Region]\n\
             relationship 'Unit Regions'[Region] -> Sales[Region]\n",
            "two active",
        ),
        (
            "model/tables/Sales.odm",
            "table Sales\n  column A\n",
            "no `dataType`",
        ),
        (
            "model/tables/Sales.odm",
            "table Sales\n  column A\n    dataType: x\n",
            "dataType: x",
        ),
        (
            "model/tables/Sales.odm",
            "table Sales\n  column A\n    dataType: text\n    sortByColumn: B\n",
            "sorts by `B`",
        ),
        (
            "model/tables/Sales.odm",
            "table Sales\n  column A\n    dataType: text\n  column A\n    dataType: text\n",
            "twice",
        ),
        ("model/tables/Sales.odm", "table Orphan\n", "no query"),
        ("model/tables/Sales.odm", "table 'Unit Regions'\n", "twice"),
        ("model/tables/Sales.odm", "column A\n", "expected a `table`"),
        (
            "queries/Sales.odq",
            "query Sales\n  load: maybe\n  m:\n    1\n",
            "true or false",
        ),
        (
            "queries/Sales.odq",
            "query Sales\n  load: true\n",
            "no `m:`",
        ),
        (
            "queries/Sales.odq",
            "query Sales\n  m:\n    1 +\n",
            "m: line",
        ),
        (
            "queries/Sales.odq",
            "query Sales\n  culture: xx-XX\n  m:\n    1\n",
            "xx-XX",
        ),
        ("queries/Sales.odq", "query a b\n", "expected one name"),
        ("queries/Sales.odq", "// hi\n", "comments"),
        ("model/model.odm", "model Model\n  culture: zz\n", "zz"),
    ];
    for (path, content, want) in cases {
        let mut files = base.clone();
        files.insert(path.to_string(), content.as_bytes().to_vec());
        rehash(&mut files, &[]);
        let e = decode(&files, Some(&root())).expect_err(content);
        assert!(e.to_string().contains(want), "{content:?}: {e}");
    }
}

#[test]
fn names_with_line_breaks_are_refused_on_save() {
    let mut p = sample();
    p.model.tables[0].columns[0].name = "a\nb".into();
    assert!(matches!(
        encode(&p, None, &Preserved::default()),
        Err(Error::Save(_))
    ));
}

#[test]
fn relationship_headers_round_trip() {
    for (ft, fc, tt, tc) in [
        ("A", "B", "C", "D"),
        ("it's", "x]]y", "Q R", "[z]"),
        ("é", "", "_", " "),
    ] {
        let h = relationship_header(ft, fc, tt, tc);
        assert_eq!(
            parse_relationship(&h),
            Some((ft.into(), fc.into(), tt.into(), tc.into())),
            "{h}"
        );
    }
}

#[test]
fn empty_tables_have_the_model_columns() {
    let p = sample();
    let t = empty_table(&p.model.tables[0]).unwrap();
    assert_eq!(t.rows(), 0);
    assert_eq!(t.name(), "Sales");
    let names: Vec<String> = t
        .batch()
        .schema()
        .fields()
        .iter()
        .map(|f| f.name().clone())
        .collect();
    assert_eq!(names, ["Region", "Qty", "Unit Price", "Region Order"]);
    let types: Vec<ColumnType> = t
        .batch()
        .schema()
        .fields()
        .iter()
        .map(|f| ColumnType::of_data_type(f.data_type()).unwrap())
        .collect();
    assert_eq!(
        types,
        [
            ColumnType::Text,
            ColumnType::Decimal,
            ColumnType::FixedDecimal,
            ColumnType::Whole
        ]
    );
}

#[test]
fn relative_paths_resolve_against_the_root() {
    let files = encode(&sample(), Some(&root()), &Preserved::default()).unwrap();
    let elsewhere_root = Path::new("/moved/Sales");
    let (p, _) = decode(&files, Some(elsewhere_root)).unwrap();
    let sales = p.queries.iter().find(|q| q.name == "Sales").unwrap();
    assert_eq!(
        sales.source.file(),
        &File::Path(elsewhere_root.join("data").join("sales.parquet"))
    );
    let (p, _) = decode(&files, None).unwrap();
    let sales = p.queries.iter().find(|q| q.name == "Sales").unwrap();
    assert_eq!(
        sales.source.file(),
        &File::Path(PathBuf::from("data/sales.parquet"))
    );
}

/// A tiny deterministic generator for the fuzz-style tests.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, bytes: &mut Vec<u8>) {
    const PIECES: &[&[u8]] = &[
        b"\n",
        b"  ",
        b":",
        b"'",
        b"\"",
        b"[",
        b"]",
        b" -> ",
        b"\r",
        b"\t",
        b"\xff",
        b"table ",
        b"column ",
        b"query ",
        b"m:\n",
        b"ref ",
        b"//",
        b"relationship ",
        b"{",
        b"}",
        b"formatVersion",
        b"..",
        b"/",
    ];
    for _ in 0..1 + rng.below(4) {
        let at = rng.below(bytes.len() + 1);
        match rng.below(4) {
            0 if !bytes.is_empty() => {
                let end = (at + rng.below(8)).min(bytes.len());
                bytes.drain(at.min(end)..end);
            }
            1 if at < bytes.len() => bytes[at] = rng.next() as u8,
            _ => {
                let piece = PIECES[rng.below(PIECES.len())];
                bytes.splice(at..at, piece.iter().copied());
            }
        }
    }
}

/// [`sample`] with Measures and Calculated columns.
fn sample_with_expressions() -> Project {
    let mut p = sample();
    let sales = p.model.table_mut("Sales").unwrap();
    let mut margin = Column::new("Margin", ColumnType::Decimal);
    margin.expression = Some("[Qty] *\n    2 -- twice\n\n  + 1".into());
    margin.query_type = ColumnType::Whole;
    sales.columns.push(margin);
    let mut total = Measure::new("Total Qty", "SUM ( Sales[Qty] )");
    total.format_string = "#,0".into();
    total.description = "All of it".into();
    total.hidden = true;
    sales.measures = vec![Measure::new("broken", "SUM(("), total];
    p.model.table_mut("Unit Regions").unwrap().measures = vec![Measure::new("Count", "")];
    p
}

#[test]
fn measures_and_calculated_columns_round_trip() {
    let project = sample_with_expressions();
    let files = encode(&project, Some(&root()), &Preserved::default()).unwrap();
    let sales = text(&files, "model/tables/Sales.odm");
    assert!(
        sales.contains(
            "  column Margin =\n      [Qty] *\n          2 -- twice\n\n        + 1\n    \
             dataType: decimal\n    queryType: whole\n"
        ),
        "{sales}"
    );
    assert!(
        sales.ends_with(
            "  measure broken =\n      SUM((\n  measure 'Total Qty' =\n      SUM ( Sales[Qty] \
             )\n    formatString: \"#,0\"\n    description: \"All of it\"\n    hidden\n"
        ),
        "measures last, sorted by name: {sales}"
    );
    let (back, pre) = round_trip(&files);
    assert_eq!(back, project);
    assert_eq!(encode(&back, Some(&root()), &pre).unwrap(), files);
    // The one-line form reads too.
    let mut files = files;
    let one_line = sales.replace(
        "  measure broken =\n      SUM((\n",
        "  measure broken = 1 + 2\n",
    );
    files.insert("model/tables/Sales.odm".into(), one_line.into_bytes());
    rehash(&mut files, &[]);
    let (back, _) = round_trip(&files);
    let m = back
        .model
        .table("Sales")
        .unwrap()
        .measure("broken")
        .unwrap();
    assert_eq!(m.expression, "1 + 2");
    // A Measure named like a Column, or twice, is refused.
    for bad in [
        "  measure Qty =\n      1\n",
        "  measure 'total qty' =\n      1\n",
    ] {
        let mut files = encode(&project, Some(&root()), &Preserved::default()).unwrap();
        let t = text(&files, "model/tables/Sales.odm") + bad;
        files.insert("model/tables/Sales.odm".into(), t.into_bytes());
        rehash(&mut files, &[]);
        assert!(decode(&files, Some(&root())).is_err(), "{bad}");
    }
}

// Regression (M3 review): every Measure was compared with every other, allocating lowercase
// copies each time, so a file with tens of thousands of Measures hung on open.
#[test]
fn a_project_with_50k_measures_opens_quickly() {
    let mut project = sample();
    for (t, name) in ["Sales", "Unit Regions"].into_iter().enumerate() {
        project.model.table_mut(name).unwrap().measures = (0..25_000)
            .map(|i| Measure::new(&format!("M{t}_{i:05}"), "1"))
            .collect();
    }
    let files = encode(&project, Some(&root()), &Preserved::default()).unwrap();
    let started = std::time::Instant::now();
    let (back, _) = round_trip(&files);
    assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
    assert_eq!(back, project);
    // A duplicate across Tables is still found.
    let mut files = files;
    let t = text(&files, "model/tables/Sales.odm") + "  measure m1_00007 =\n      1\n";
    files.insert("model/tables/Sales.odm".into(), t.into_bytes());
    rehash(&mut files, &[]);
    let e = decode(&files, Some(&root())).unwrap_err().to_string();
    assert!(e.contains("measure names are unique"), "{e}");
}

#[test]
fn opening_mutated_projects_never_panics() {
    let base = encode(
        &sample_with_expressions(),
        Some(&root()),
        &Preserved::default(),
    )
    .unwrap();
    let paths: Vec<String> = base.keys().cloned().collect();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut opened = 0;
    for i in 0..3000 {
        let mut files = base.clone();
        let path = paths[rng.below(paths.len())].clone();
        if let Some(bytes) = files.get_mut(&path) {
            mutate(&mut rng, bytes);
        }
        // Mostly with fixed-up hashes, so the mutation reaches the parsers.
        if i % 4 != 0 && path != MANIFEST {
            rehash(&mut files, &[]);
        }
        if let Ok((p, pre)) = decode(&files, Some(&root())) {
            opened += 1;
            // Whatever opens saves, and reopens the same.
            if let Ok(saved) = encode(&p, Some(&root()), &pre) {
                let (p2, _) = decode(&saved, Some(&root())).unwrap();
                assert_eq!(p2, p);
            }
        }
    }
    assert!(opened > 0, "some mutations are harmless");
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(42);
    for _ in 0..500 {
        let mut files = FileSet::new();
        for _ in 0..rng.below(4) {
            let len = rng.below(200);
            let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let name = ["project.odash.json", "model/model.odm", "queries/a.odq"][rng.below(3)];
            files.insert(name.into(), bytes);
        }
        let _ = decode(&files, None);
    }
}
