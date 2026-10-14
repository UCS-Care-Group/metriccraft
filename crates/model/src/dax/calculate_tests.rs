//! M3.4 tests: CALCULATE, FILTER, ALL, ALLEXCEPT, context transition, RELATED, expanded
//! tables. `TESTSUM` stands in for `SUM` (M3.3).

use std::sync::Arc;

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use opendash_core::Value;
use opendash_data::{Table, TableBuilder};

use super::super::*;
use crate::{Model, Relationship};

fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    b.finish().unwrap()
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

/// Parity cases 0031/0032's Sales, plus a ProductKey → Product (Key, Colour) Relationship.
/// Sales row 5 has key 9, with no Product.
fn db() -> Database {
    let sales = table(
        "Sales",
        vec![
            (
                "Region",
                text(&[
                    Some("North"),
                    Some("North"),
                    Some("South"),
                    Some("South"),
                    Some("West"),
                    Some("East"),
                ]),
            ),
            (
                "Product",
                text(&[
                    Some("Apple"),
                    Some("pear"),
                    Some("Apple"),
                    Some("Pear"),
                    Some("apple"),
                    None,
                ]),
            ),
            (
                "Amount",
                floats(&[Some(10.5), Some(-3.0), Some(4.0), None, None, Some(7.25)]),
            ),
            (
                "Qty",
                ints(&[Some(2), Some(3), Some(1), Some(5), Some(0), None]),
            ),
            (
                "ProductKey",
                ints(&[Some(1), Some(2), Some(1), Some(2), Some(3), Some(9)]),
            ),
        ],
    );
    let product = table(
        "Product",
        vec![
            ("Key", ints(&[Some(1), Some(2), Some(3), Some(4)])),
            (
                "Colour",
                text(&[Some("Red"), Some("Green"), Some("red"), Some("Blue")]),
            ),
        ],
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Sales", "ProductKey", "Product", "Key"));
    Database::new(&model, &[sales, product])
}

fn options() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

fn cells(db: &Database, q: &str) -> Vec<Vec<String>> {
    let r = run_query_text(db, q, options()).unwrap_or_else(|e| panic!("{q}: {e}"));
    r[0].rows
        .iter()
        .map(|row| row.iter().map(Value::copy_text).collect())
        .collect()
}

fn one(db: &Database, expr: &str) -> String {
    cells(db, &format!("EVALUATE ROW(\"x\", {expr})"))[0][0].clone()
}

fn error(db: &Database, expr: &str) -> DaxError {
    let q = format!("EVALUATE ROW(\"x\", {expr})");
    run_query_text(db, &q, options()).expect_err(&q)
}

/// The query of a parity case, with `SUM` swapped for `TESTSUM`.
fn case_query(case: &str) -> String {
    let start = case.find("```dax\n").map_or(0, |i| i + 7);
    let end = case[start..].find("```").map_or(case.len(), |i| start + i);
    case[start..end].replace("SUM(", "TESTSUM(")
}

#[test]
fn parity_0031_calculate_filters() {
    let db = db();
    let q = case_query(include_str!(
        "../../../../parity/observed/0031-calculate-filters.md"
    ));
    // InnerWins, TwoArgs, OrSameColumn, FilterTable, FilterRows, AllRows, AllColumn,
    // AllExcept, AllOtherColumn, NoMatch.
    assert_eq!(
        cells(&db, &q),
        [["5", "5", "5", "7.5", "3", "2", "11", "6", "6", ""]]
    );
}

#[test]
fn parity_0032_context_transition() {
    let db = db();
    let q = case_query(include_str!(
        "../../../../parity/observed/0032-context-transition.md"
    ));
    // NoTransition, Transition, MeasureRef, MeasureTotal, MeasureFiltered.
    assert_eq!(cells(&db, &q), [["6", "1", "1", "11", "6"]]);
}

#[test]
fn boolean_filters_replace_their_columns() {
    let db = db();
    for (expr, want) in [
        // Text compares ignoring case; an inner filter replaces the outer one on its column.
        (
            r#"CALCULATE(COUNTROWS(Sales), Sales[Product] = "APPLE")"#,
            "3",
        ),
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), Sales[Region] = "West"), Sales[Region] = "North")"#,
            "1",
        ),
        // Filters on other columns stay.
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), Sales[Product] = "apple"), Sales[Region] = "North")"#,
            "1",
        ),
        // Several columns of one Table in one condition.
        (
            r#"CALCULATE(COUNTROWS(Sales), Sales[Region] = "North" && Sales[Qty] > 2)"#,
            "1",
        ),
        (
            r#"CALCULATE(COUNTROWS(Sales), NOT(Sales[Region] = "North"))"#,
            "4",
        ),
        // BLANK is a value a condition can keep.
        ("CALCULATE(COUNTROWS(Sales), Sales[Qty] = BLANK())", "2"),
        // CALCULATETABLE.
        (
            r#"COUNTROWS(CALCULATETABLE(Sales, Sales[Region] = "South"))"#,
            "2",
        ),
        // No filter arguments: the expression as is.
        ("CALCULATE(COUNTROWS(Sales))", "6"),
    ] {
        assert_eq!(one(&db, expr), want, "{expr}");
    }
}

#[test]
fn table_filters_and_modifiers() {
    let db = db();
    for (expr, want) in [
        // A FILTER over ALL(column) replaces the outer filter on that column.
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), FILTER(ALL(Sales[Region]), Sales[Region] = "West")), Sales[Region] = "North")"#,
            "1",
        ),
        // FILTER over a model Table sees the outer filter.
        (
            r#"CALCULATE(COUNTROWS(FILTER(Sales, Sales[Qty] > 0)), Sales[Region] = "North")"#,
            "2",
        ),
        // FILTER(ALL(Sales), …) replaces the outer filters on Sales.
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), FILTER(ALL(Sales), Sales[Qty] >= 2)), Sales[Region] = "North")"#,
            "3",
        ),
        // ALL as a table: every row, or a column's distinct values (case folded).
        (
            r#"CALCULATE(COUNTROWS(ALL(Sales)), Sales[Region] = "North")"#,
            "6",
        ),
        ("COUNTROWS(ALL(Sales[Product]))", "3"),
        ("COUNTROWS(ALL(Sales[Region], Sales[Product]))", "6"),
        (
            "COUNTROWS(ALLEXCEPT(Sales, Sales[Amount], Sales[Qty], Sales[ProductKey]))",
            "6",
        ),
        // ALL() and REMOVEFILTERS remove everything.
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), ALL()), Sales[Region] = "North")"#,
            "6",
        ),
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), REMOVEFILTERS(Sales[Region])), Sales[Region] = "North")"#,
            "6",
        ),
        // Removing some columns of a row-set filter keeps its projection on the others.
        (
            "CALCULATE(CALCULATE(COUNTROWS(Sales), ALL(Sales[Qty])), FILTER(Sales, Sales[Qty] = 2))",
            "1",
        ),
        (
            "CALCULATE(CALCULATE(COUNTROWS(Sales), ALLEXCEPT(Sales, Sales[Region])), FILTER(Sales, Sales[Qty] = 2))",
            "2",
        ),
        (
            "CALCULATE(CALCULATE(COUNTROWS(Sales), ALL(Sales)), FILTER(Sales, Sales[Qty] = 2))",
            "6",
        ),
    ] {
        assert_eq!(one(&db, expr), want, "{expr}");
    }
}

#[test]
fn expanded_tables_follow_active_relationships() {
    let db = db();
    for (expr, want) in [
        // One side filters the many side; key 9 has no Product, so it drops out.
        (
            r#"CALCULATE(COUNTROWS(Sales), Product[Colour] = "red")"#,
            "3",
        ),
        // A column filter on the many side doesn't filter the one side…
        ("CALCULATE(COUNTROWS(Product), Sales[Qty] > 2)", "4"),
        // …a table filter does (expanded table): products 1 and 2.
        (
            "CALCULATE(COUNTROWS(Product), FILTER(Sales, Sales[Qty] >= 1))",
            "2",
        ),
        ("CALCULATE(COUNTROWS(Product), Sales)", "3"),
        // ALL(Sales) removes the filters on Product too (its expanded Table)…
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), ALL(Sales)), Product[Colour] = "red")"#,
            "6",
        ),
        // …ALL on a Sales column doesn't.
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), ALL(Sales[Region])), Product[Colour] = "red")"#,
            "3",
        ),
        // ALLEXCEPT keeps a related Table's column.
        (
            r#"CALCULATE(CALCULATE(COUNTROWS(Sales), ALLEXCEPT(Sales, Product[Colour])), Product[Colour] = "red", Sales[Region] = "North")"#,
            "3",
        ),
        // Context transition filters the row's own Table only.
        (
            "COUNTROWS(FILTER(Sales, CALCULATE(COUNTROWS(Product)) = 4))",
            "6",
        ),
        // RELATED reads the one side's row; no match is BLANK.
        (
            r#"COUNTROWS(FILTER(Sales, RELATED(Product[Colour]) = "RED"))"#,
            "3",
        ),
        (
            "COUNTROWS(FILTER(Sales, ISBLANKTEST(RELATED(Product[Colour]))))",
            "1",
        ),
    ] {
        if expr.contains("ISBLANKTEST") {
            // ISBLANK is M3.5's: compare with BLANK strictly instead.
            let e = expr.replace(
                "ISBLANKTEST(RELATED(Product[Colour]))",
                "RELATED(Product[Colour]) == BLANK()",
            );
            assert_eq!(one(&db, &e), want, "{e}");
            continue;
        }
        assert_eq!(one(&db, expr), want, "{expr}");
    }
}

#[test]
fn inactive_relationships_neither_filter_nor_relate() {
    let sales = table("Sales", vec![("K", ints(&[Some(1), Some(2)]))]);
    let product = table(
        "Product",
        vec![
            ("K", ints(&[Some(1), Some(2)])),
            ("C", text(&[Some("a"), Some("b")])),
        ],
    );
    let mut model = Model::default();
    let mut r = Relationship::new("Sales", "K", "Product", "K");
    r.active = false;
    model.relationships.push(r);
    let db = Database::new(&model, &[sales, product]);
    assert_eq!(
        one(&db, r#"CALCULATE(COUNTROWS(Sales), Product[C] = "a")"#),
        "2"
    );
    let e = error(&db, "COUNTROWS(FILTER(Sales, RELATED(Product[C]) = \"a\"))");
    assert!(e.message.contains("RELATED needs"), "{e}");
}

#[test]
fn related_follows_chains() {
    // Sales → Product → Category.
    let sales = table("Sales", vec![("P", ints(&[Some(1), Some(2), Some(2)]))]);
    let product = table(
        "Product",
        vec![
            ("P", ints(&[Some(1), Some(2)])),
            ("Cat", text(&[Some("x"), Some("y")])),
        ],
    );
    let category = table(
        "Category",
        vec![
            ("Cat", text(&[Some("X"), Some("y")])),
            ("Name", text(&[Some("Ex"), Some("Why")])),
        ],
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Sales", "P", "Product", "P"));
    model
        .relationships
        .push(Relationship::new("Product", "Cat", "Category", "Cat"));
    let db = Database::new(&model, &[sales, product, category]);
    assert_eq!(
        one(
            &db,
            r#"COUNTROWS(FILTER(Sales, RELATED(Category[Name]) = "Why"))"#
        ),
        "2"
    );
    // A filter on Category reaches Sales; a table filter on Sales reaches Category.
    assert_eq!(
        one(&db, r#"CALCULATE(COUNTROWS(Sales), Category[Name] = "ex")"#),
        "1"
    );
    assert_eq!(
        one(
            &db,
            "CALCULATE(COUNTROWS(Category), FILTER(Sales, Sales[P] = 1))"
        ),
        "1"
    );
    // RELATED outside a row context.
    let e = error(&db, "RELATED(Category[Name])");
    assert!(e.message.contains("row context"), "{e}");
}

#[test]
fn context_transition_keeps_duplicate_rows_together() {
    let t = table("T", vec![("A", ints(&[Some(1), Some(1), Some(2)]))]);
    let db = Database::new(&Model::default(), &[t]);
    assert_eq!(
        one(&db, "COUNTROWS(FILTER(T, CALCULATE(COUNTROWS(T)) = 2))"),
        "2"
    );
    // Outside any row context, CALCULATE transitions nothing.
    assert_eq!(one(&db, "CALCULATE(COUNTROWS(T))"), "3");
}

#[test]
fn measures_transition_in_any_row_context() {
    let db = db();
    let q = "DEFINE MEASURE Sales[N] = COUNTROWS(Sales)
        EVALUATE FILTER(ALL(Sales[Region]), [N] = 2) ORDER BY Sales[Region]";
    assert_eq!(cells(&db, q), [["North"], ["South"]]);
    // A row of a table without lineage filters nothing.
    let q = "DEFINE MEASURE Sales[N] = COUNTROWS(Sales) EVALUATE {1, 2} ORDER BY [N], [Value]";
    assert_eq!(cells(&db, q), [["1"], ["2"]]);
    // A Measure's expression sees no row context.
    let q = "DEFINE MEASURE Sales[Q] = Sales[Qty] EVALUATE FILTER(Sales, [Q] = 2)";
    let e = run_query_text(&db, q, options()).unwrap_err();
    assert!(e.message.contains("no row context"), "{e}");
    // Measures in a row context of the result rows (ORDER BY).
    let q = "DEFINE MEASURE Sales[Q] = TESTSUM(Sales[Qty])
        EVALUATE ALL(Sales[Region]) ORDER BY [Q] DESC, Sales[Region]";
    assert_eq!(cells(&db, q), [["South"], ["North"], ["West"], ["East"]]);
}

#[test]
fn calculate_errors() {
    let db = db();
    for (expr, msg) in [
        (
            r#"CALCULATE(1, Sales[Region] = Product[Colour])"#,
            "only one Table",
        ),
        ("CALCULATE(1, 1 = 1)", "condition on columns"),
        ("CALCULATE(1, {1, 2})", "model Tables"),
        (
            "CALCULATE(1, ALL(Sales[Region], Product[Colour]))",
            "one Table",
        ),
        ("CALCULATE(1, ALLEXCEPT(Sales[Region]))", "needs a Table"),
        ("CALCULATE(1, ALL(Nope))", "no Table"),
        ("CALCULATE(1, ALL(Sales[Nope]))", "no Column"),
        ("COUNTROWS(FILTER(Sales, \"maybe\"))", "maybe"),
        ("COUNTROWS(FILTER(Sales))", "takes 2"),
        ("COUNTROWS(REMOVEFILTERS(Sales))", "only be a CALCULATE"),
        ("CALCULATE()", "at least 1"),
        ("RELATED(Sales[Region])", "RELATED needs"),
    ] {
        let e = error(&db, expr);
        assert!(e.message.contains(msg), "{expr}: {e}");
    }
}

#[test]
fn nested_calculates_hit_the_depth_cap_not_the_stack() {
    let db = db();
    // Measures nesting CALCULATE inside FILTER inside CALCULATE, chained past MAX_EVAL_DEPTH.
    let mut q = String::from("DEFINE MEASURE Sales[m0] = 1\n");
    for i in 1..60 {
        let mut body = format!("[m{}]", i - 1);
        for _ in 0..10 {
            body = format!("CALCULATE({body}, Sales[Region] = \"North\")");
        }
        q.push_str(&format!("MEASURE Sales[m{i}] = {body}\n"));
    }
    let e = run_query_text(&db, &format!("{q}EVALUATE ROW(\"x\", [m59])"), options()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit, "{e}");
    assert_eq!(
        cells(&db, &format!("{q}EVALUATE ROW(\"x\", [m20])")),
        [["1"]]
    );
}
