//! M3.9 tests: SUMMARIZECOLUMNS, the `summarize` API, multi-row results with ORDER BY.

use std::sync::Arc;

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use opendash_core::Value;
use opendash_data::{Table, TableBuilder, fold_case};

use super::{ColumnRef, summarize};
use crate::dax::*;
use crate::{Model, Relationship};

fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    fold_case(b.finish().unwrap(), usize::MAX).unwrap()
}

fn text(v: &[Option<&str>]) -> ArrayRef {
    Arc::new(StringArray::from(v.to_vec()))
}

fn ints(v: &[Option<i64>]) -> ArrayRef {
    Arc::new(Int64Array::from(v.to_vec()))
}

fn floats(v: &[Option<f64>]) -> ArrayRef {
    Arc::new(Float64Array::from(v.to_vec()))
}

/// Parity case 0043's Regions and Sales (Sales[Region] → Regions[Region]), plus Sales rows with
/// a blank Product and a Region (`East`) Regions doesn't have, and an unrelated Colours table.
fn db_with(extra: bool) -> Database {
    let mut region = vec![
        Some("North"),
        Some("North"),
        Some("North"),
        Some("South"),
        Some("South"),
        Some("South"),
    ];
    let mut product = vec![
        Some("Apple"),
        Some("Pear"),
        Some("Apple"),
        Some("Apple"),
        Some("Pear"),
        Some("Plum"),
    ];
    let mut amount = vec![Some(10.5), Some(-3.0), Some(4.0), None, Some(7.25), None];
    let mut qty = vec![Some(2), Some(3), Some(1), Some(1), Some(5), Some(2)];
    if extra {
        region.extend([Some("North"), Some("East")]);
        product.extend([None, Some("Apple")]);
        amount.extend([Some(1.0), Some(2.0)]);
        qty.extend([Some(4), Some(1)]);
    }
    let sales = table(
        "Sales",
        vec![
            ("Region", text(&region)),
            ("Product", text(&product)),
            ("Amount", floats(&amount)),
            ("Qty", ints(&qty)),
        ],
    );
    let regions = table(
        "Regions",
        vec![
            (
                "Region",
                text(&[Some("North"), Some("South"), Some("West")]),
            ),
            ("Manager", text(&[Some("Ann"), Some("Bob"), Some("Cy")])),
        ],
    );
    let colours = table(
        "Colours",
        vec![("Colour", text(&[Some("Red"), Some("Blue")]))],
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Sales", "Region", "Regions", "Region"));
    Database::new(&model, &[sales, regions, colours])
}

fn db() -> Database {
    db_with(false)
}

fn options() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

fn run(db: &Database, q: &str) -> ResultTable {
    run_query_text(db, q, options())
        .unwrap_or_else(|e| panic!("{q}: {e}"))
        .remove(0)
}

fn cells(db: &Database, q: &str) -> Vec<Vec<String>> {
    run(db, q)
        .rows
        .iter()
        .map(|row| row.iter().map(Value::copy_text).collect())
        .collect()
}

fn error(db: &Database, q: &str) -> DaxError {
    run_query_text(db, q, options()).expect_err(q)
}

fn rows(expected: &[&[&str]]) -> Vec<Vec<String>> {
    expected
        .iter()
        .map(|r| r.iter().map(|s| s.to_string()).collect())
        .collect()
}

fn case_query(case: &str) -> String {
    let start = case.find("```dax\n").map_or(0, |i| i + 7);
    let end = case[start..].find("```").map_or(case.len(), |i| start + i);
    case[start..end].to_string()
}

#[test]
fn parity_0043_summarizecolumns() {
    let q = case_query(include_str!(
        "../../../../parity/observed/0043-summarizecolumns.md"
    ));
    let r = run(&db(), &q);
    assert_eq!(r.columns, ["Region", "Product", "Total", "Big Orders"]);
    // South/Apple and South/Plum have only BLANK expressions: dropped.
    assert_eq!(
        cells(&db(), &q),
        rows(&[
            &["South", "Pear", "7.25", "1"],
            &["North", "Apple", "14.5", ""],
            &["North", "Pear", "-3", "1"],
        ])
    );
}

#[test]
fn no_expressions_gives_existing_combinations() {
    let q = "EVALUATE SUMMARIZECOLUMNS(Regions[Region], Sales[Product]) \
             ORDER BY Regions[Region], Sales[Product]";
    assert_eq!(
        cells(&db(), q),
        rows(&[
            &["North", "Apple"],
            &["North", "Pear"],
            &["South", "Apple"],
            &["South", "Pear"],
            &["South", "Plum"],
        ])
    );
    // One Table: its distinct values (West has no Sales, but is a Region).
    let q = "EVALUATE SUMMARIZECOLUMNS(Regions[Region]) ORDER BY Regions[Region]";
    assert_eq!(cells(&db(), q), rows(&[&["North"], &["South"], &["West"]]));
    // Through the Relationship, an unmatched key reads BLANK; BLANK sorts first.
    let q = "EVALUATE SUMMARIZECOLUMNS(Sales[Region], Regions[Manager]) \
             ORDER BY Regions[Manager], Sales[Region]";
    assert_eq!(
        cells(&db_with(true), q),
        rows(&[&["East", ""], &["North", "Ann"], &["South", "Bob"]])
    );
}

#[test]
fn unrelated_tables_cross_join() {
    let q = "EVALUATE SUMMARIZECOLUMNS(Regions[Region], Colours[Colour]) \
             ORDER BY Regions[Region], Colours[Colour] DESC";
    assert_eq!(
        cells(&db(), q),
        rows(&[
            &["North", "Red"],
            &["North", "Blue"],
            &["South", "Red"],
            &["South", "Blue"],
            &["West", "Red"],
            &["West", "Blue"],
        ])
    );
    // A measure on Sales doesn't filter by Colours: every Colour shows each Region's total.
    let q = "EVALUATE SUMMARIZECOLUMNS(Regions[Region], Colours[Colour], \"T\", SUM(Sales[Qty])) \
             ORDER BY [T] DESC, Colours[Colour]";
    assert_eq!(
        cells(&db(), q),
        rows(&[
            &["South", "Blue", "8"],
            &["South", "Red", "8"],
            &["North", "Blue", "6"],
            &["North", "Red", "6"],
        ])
    );
}

#[test]
fn expressions_see_the_group_and_the_outer_filters() {
    let q = "EVALUATE CALCULATETABLE(\
                 SUMMARIZECOLUMNS(Sales[Product], \"Q\", SUM(Sales[Qty]), \"N\", COUNTROWS(Sales)), \
                 Regions[Region] = \"North\") \
             ORDER BY Sales[Product]";
    assert_eq!(
        cells(&db(), q),
        rows(&[&["Apple", "3", "2"], &["Pear", "3", "1"]])
    );
    // Only expressions: one row (dropped when every expression is BLANK).
    let q = "EVALUATE SUMMARIZECOLUMNS(\"Q\", SUM(Sales[Qty]))";
    assert_eq!(cells(&db(), q), rows(&[&["14"]]));
    let q = "EVALUATE CALCULATETABLE(SUMMARIZECOLUMNS(\"Q\", SUM(Sales[Qty])), \
             Regions[Region] = \"West\")";
    assert!(cells(&db(), q).is_empty());
}

#[test]
fn order_by_keys_desc_and_blanks() {
    let db = db_with(true);
    // Blank Product sorts first ascending, last descending; ties go to the next keys.
    let q = "EVALUATE SUMMARIZECOLUMNS(Sales[Product], Sales[Region], \"Q\", SUM(Sales[Qty])) \
             ORDER BY Sales[Product], [Q] DESC, Sales[Region]";
    assert_eq!(
        cells(&db, q),
        rows(&[
            &["", "North", "4"],
            &["Apple", "North", "3"],
            &["Apple", "East", "1"],
            &["Apple", "South", "1"],
            &["Pear", "South", "5"],
            &["Pear", "North", "3"],
            &["Plum", "South", "2"],
        ])
    );
    let q = "EVALUATE SUMMARIZECOLUMNS(Sales[Product], \"Q\", SUM(Sales[Qty])) \
             ORDER BY Sales[Product] DESC";
    assert_eq!(
        cells(&db, q),
        rows(&[&["Plum", "2"], &["Pear", "8"], &["Apple", "5"], &["", "4"]])
    );
}

#[test]
fn measures_and_context_transition_over_the_result() {
    let q = "DEFINE MEASURE Sales[Qty Total] = SUM(Sales[Qty]) \
             EVALUATE SUMMARIZECOLUMNS(Regions[Region], \"Q\", [Qty Total]) \
             ORDER BY Regions[Region]";
    assert_eq!(cells(&db(), q), rows(&[&["North", "6"], &["South", "8"]]));
    // The result's group-by columns keep their lineage: a filter iterating it transitions.
    let q = "DEFINE MEASURE Sales[Qty Total] = SUM(Sales[Qty]) \
             EVALUATE ROW(\"n\", COUNTROWS(FILTER(SUMMARIZECOLUMNS(Regions[Region]), \
             [Qty Total] > 6)))";
    assert_eq!(cells(&db(), q), rows(&[&["1"]]));
}

#[test]
fn malformed_calls_are_errors() {
    let db = db();
    for (q, why) in [
        (
            "EVALUATE SUMMARIZECOLUMNS(Sales[Product], FILTER(Sales, Sales[Qty] > 1), \"Q\", 1)",
            "filter table",
        ),
        ("EVALUATE SUMMARIZECOLUMNS([Product])", "no Table"),
        ("EVALUATE SUMMARIZECOLUMNS(Sales[Nope])", "no column"),
        (
            "EVALUATE SUMMARIZECOLUMNS(Sales[Product], \"Q\")",
            "unpaired name",
        ),
        (
            "EVALUATE SUMMARIZECOLUMNS(Sales[Product], \"Q\", 1, Sales[Region])",
            "column after names",
        ),
        (
            "EVALUATE SUMMARIZECOLUMNS(Sales[Product], \"Q\", 1, \"q\", 2)",
            "duplicate names",
        ),
        (
            "EVALUATE SUMMARIZECOLUMNS(Sales[Product], 'Sales'[Product])",
            "duplicate column",
        ),
        ("EVALUATE SUMMARIZECOLUMNS()", "no arguments"),
    ] {
        let e = error(&db, q);
        assert_eq!(e.kind, ErrorKind::Eval, "{why}: {e}");
    }
}

#[test]
fn cross_products_count_against_the_memory_cap() {
    let n = 3000;
    let a: Vec<Option<i64>> = (0..n).map(Some).collect();
    let tables = [
        table("A", vec![("X", ints(&a))]),
        table("B", vec![("Y", ints(&a))]),
        table("C", vec![("Z", ints(&a))]),
    ];
    let db = Database::new(&Model::default(), &tables);
    let opts = Options {
        cancel: None,
        memory_cap: db.loaded_bytes + (64 << 20),
    };
    // 3000³ groups: refused before anything is built.
    let e = run_query_text(&db, "EVALUATE SUMMARIZECOLUMNS(A[X], B[Y], C[Z])", opts)
        .expect_err("too big");
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    let e = run_query_text(&db, "EVALUATE SUMMARIZECOLUMNS(A[X], B[Y])", opts).expect_err("big");
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
}

#[test]
fn cancel_stops_it() {
    let flag = std::sync::atomic::AtomicBool::new(true);
    let opts = Options {
        cancel: Some(&flag),
        memory_cap: usize::MAX,
    };
    let e = run_query_text(&db(), "EVALUATE SUMMARIZECOLUMNS(Sales[Product])", opts)
        .expect_err("cancelled");
    assert_eq!(e.kind, ErrorKind::Cancelled);
}

#[test]
fn the_rust_api_matches_the_dax_function() {
    let db = db();
    let sum = opendash_dax::parse_expr("SUM(Sales[Amount])").unwrap();
    let count = opendash_dax::parse_expr("COUNTROWS(FILTER(Sales, Sales[Qty] >= 3))").unwrap();
    let r = summarize(
        &db,
        &[
            ColumnRef {
                table: "Regions",
                column: "Region",
            },
            ColumnRef {
                table: "Sales",
                column: "Product",
            },
        ],
        &[("Total", &sum), ("Big Orders", &count)],
        &FilterContext::default(),
        options(),
    )
    .unwrap();
    let q = "EVALUATE SUMMARIZECOLUMNS(Regions[Region], Sales[Product], \
             \"Total\", SUM(Sales[Amount]), \"Big Orders\", COUNTROWS(FILTER(Sales, Sales[Qty] >= 3)))";
    assert_eq!(r, run(&db, q));
    assert_eq!(r.rows.len(), 3);
    // Filters (a slicer): Product = Pear.
    let sales = db.table("Sales").unwrap();
    let product = db.tables[sales].column("Product").unwrap();
    let pear = FilterContext::default().with(Filter::values(
        &db.tables[sales],
        sales,
        product,
        &[Value::text("pear")],
    ));
    let r = summarize(
        &db,
        &[ColumnRef {
            table: "Regions",
            column: "Region",
        }],
        &[("Total", &sum)],
        &pear,
        options(),
    )
    .unwrap();
    assert_eq!(r.columns, ["Region", "Total"]);
    let shown: Vec<Vec<String>> = r
        .rows
        .iter()
        .map(|row| row.iter().map(Value::copy_text).collect())
        .collect();
    assert_eq!(shown, rows(&[&["North", "-3"], &["South", "7.25"]]));
    let bad = summarize(
        &db,
        &[ColumnRef {
            table: "Nope",
            column: "X",
        }],
        &[],
        &FilterContext::default(),
        options(),
    );
    assert!(bad.is_err());
}
