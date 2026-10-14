//! Regression tests for the M3 evaluator review: text growth, scoped memory charges, exact
//! mixed-number order and keys, linear Relationship traversal.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch};
use opendash_core::Value;
use opendash_data::{Table, TableBuilder};
use opendash_dax::Span;

use super::model_eval::evaluate_rows;
use super::*;
use crate::{Model, Relationship};

fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    b.finish().unwrap()
}

fn ints(v: impl IntoIterator<Item = i64>) -> ArrayRef {
    Arc::new(Int64Array::from_iter_values(v))
}

fn unbounded() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

/// With `extra` bytes for intermediate data beyond the loaded data.
fn capped(db: &Database, extra: usize) -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: db.loaded_bytes + extra,
    }
}

fn scalar(db: &Database, expr: &str, options: Options<'_>) -> Result<Value> {
    let r = run_query_text(db, &format!("EVALUATE ROW(\"x\", {expr})"), options)?;
    Ok(r[0].rows[0][0].clone())
}

/// `VAR a0 = "…16 x…" VAR a1 = a0 & a0 … VAR a40 = a39 & a39 RETURN a40`: 16 TiB of text.
fn doubling() -> String {
    let mut s = String::from("VAR a0 = \"xxxxxxxxxxxxxxxx\" ");
    for i in 1..=40 {
        s += &format!("VAR a{i} = a{p} & a{p} ", p = i - 1);
    }
    s + "RETURN a40"
}

/// Sales: `n` rows, `OrderId` 0…n-1 (unique), `Amount` 1…n.
fn sales(n: i64) -> (Model, Database) {
    let t = table(
        "Sales",
        vec![("OrderId", ints(0..n)), ("Amount", ints(1..=n))],
    );
    let model = Model::default();
    let db = Database::new(&model, &[t]);
    (model, db)
}

#[test]
fn text_doubled_through_vars_fails_fast() {
    let (model, db) = sales(3);
    let start = Instant::now();
    let e = scalar(&db, &doubling(), unbounded()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit, "{e}");
    // Under a modest cap the memory cap is hit first.
    let e = scalar(&db, &doubling(), capped(&db, 4 << 20)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    // As a Calculated column (each row): the column's error, not an abort.
    let expr = opendash_dax::parse_expr(&doubling()).unwrap();
    let e = evaluate_rows(&db, &model, 0, &expr, unbounded()).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Limit, "{e}");
    assert!(start.elapsed() < Duration::from_secs(20));
    // Text under the cap still concatenates.
    assert_eq!(
        scalar(&db, "\"ab\" & 1 & BLANK() & TRUE()", unbounded()).unwrap(),
        Value::text("ab1True")
    );
}

#[test]
fn temporaries_are_refunded_per_row() {
    // 1500 unique keys: each row's CALCULATE builds 1500 distinct values, each row's FILTER a
    // 1500-bit mask. Charged cumulatively (as before the fix) they exceed the cap many times
    // over; refunded when each call finishes, they fit easily beside the column.
    let n = 1500;
    let (model, db) = sales(n);
    let cap = capped(&db, 192 << 10);
    for (text, want) in [
        // The row's own key: one row each.
        ("CALCULATE(COUNTROWS(Sales), Sales[OrderId] = [OrderId])", 1),
        ("COUNTROWS(FILTER(Sales, Sales[Amount] > 0))", n),
        (
            "VAR t = FILTER(Sales, Sales[Amount] > 0) RETURN COUNTROWS(t)",
            n,
        ),
    ] {
        let expr = opendash_dax::parse_expr(text).unwrap();
        let v = evaluate_rows(&db, &model, 0, &expr, cap).unwrap_or_else(|e| panic!("{text}: {e}"));
        assert_eq!(v.len(), n as usize);
        assert!(
            v.iter().all(|v| *v == Value::Integer(want)),
            "{text}: {:?}",
            v.get(..3)
        );
    }
    // A genuinely huge intermediate still fails: the whole table materialised, or its distinct
    // values, beyond a small cap.
    let small = capped(&db, 64 << 10);
    let e = run_query_text(&db, "EVALUATE Sales", small).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    let e = run_query_text(&db, "EVALUATE ALL(Sales[OrderId])", small).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    let e = run_query_text(&db, "EVALUATE ALL(Sales[OrderId], Sales[Amount])", small).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    // Results kept in a VAR stay charged while in scope: two copies of a materialised table
    // need twice the memory of one.
    let one = "EVALUATE VAR t = ALL(Sales[OrderId], Sales[Amount]) RETURN t";
    let two = "EVALUATE VAR t = ALL(Sales[OrderId], Sales[Amount]) \
               VAR u = ALL(Sales[Amount], Sales[OrderId]) RETURN FILTER(t, COUNTROWS(u) > 0)";
    let fits_one = (1usize << 10..)
        .step_by(16 << 10)
        .find(|&extra| run_query_text(&db, one, capped(&db, extra)).is_ok());
    let fits_one = fits_one.unwrap();
    let e = run_query_text(&db, two, capped(&db, fits_one)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
}

#[test]
fn measures_resolve_ignoring_case_by_map() {
    let (_, db) = sales(3);
    let q = "DEFINE MEASURE Sales[Ünit Price] = 2 \
             MEASURE Sales[A_very_long_measure_name_beyond_sixty_four_bytes_of_ascii_text_ok] = 3 \
             EVALUATE ROW(\"x\", [ünit price] + [a_VERY_long_measure_name_beyond_sixty_four_bytes_of_ascii_text_OK] \
                 + Sales[ÜNIT PRICE])";
    let r = run_query_text(&db, q, unbounded()).unwrap();
    assert_eq!(r[0].rows[0][0], Value::Integer(7));
    let e = run_query_text(
        &db,
        "DEFINE MEASURE Sales[M] = 1 MEASURE Sales[m] = 2 EVALUATE ROW(\"x\", [M])",
        unbounded(),
    )
    .unwrap_err();
    assert!(e.message.contains("defined twice"), "{e}");
    // A Measure named with another Table isn't found there.
    let (mut model, data) = (Model::default(), vec![table("T", vec![("C", ints(0..2))])]);
    model.relationships.clear();
    let db2 = Database::new(&model, &data);
    let e = run_query_text(
        &db2,
        "DEFINE MEASURE T[M] = 1 EVALUATE ROW(\"x\", Other[M])",
        unbounded(),
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Eval, "{e}");
}

#[test]
fn order_by_over_mixed_numbers_beyond_2_pow_53_is_consistent() {
    let (_, db) = sales(1);
    let q = "EVALUATE { 9007199254740993, \"a\", 9007199254740992.0, 1.5, BLANK(), \
             9007199254740992, -9007199254740993, -9007199254740992.0 } ORDER BY [Value]";
    let r = run_query_text(&db, q, unbounded()).unwrap();
    let got: Vec<Value> = r[0].rows.iter().map(|row| row[0].clone()).collect();
    let big = 9_007_199_254_740_992i64;
    assert_eq!(
        got,
        [
            Value::Blank,
            Value::Integer(-big - 1),
            Value::Decimal(-big as f64),
            Value::Decimal(1.5),
            // Equal: kept in their written order.
            Value::Decimal(big as f64),
            Value::Integer(big),
            Value::Integer(big + 1),
            Value::text("a"),
        ]
    );
    let r = run_query_text(&db, &q.replace("[Value]", "[Value] DESC"), unbounded()).unwrap();
    assert_eq!(r[0].rows[0][0].copy_text(), "a");
    assert_eq!(r[0].rows[1][0], Value::Integer(9_007_199_254_740_993));
}

#[test]
fn whole_decimals_key_as_the_integers_they_equal() {
    for i in [
        0i64,
        7,
        -3,
        1 << 53,
        (1 << 53) + 2,
        10_000_000_000_000_000,
        i64::MIN,
    ] {
        assert_eq!(
            Key::of(&Value::Integer(i), false),
            Key::of(&Value::Decimal(i as f64), false),
            "{i}"
        );
    }
    assert_eq!(Key::of(&Value::Decimal(-0.0), false), Some(Key::Int(0)));
    // 2^63 is beyond i64: a float key, not a saturated Integer.
    assert_ne!(
        Key::of(&Value::Decimal(2f64.powi(63)), false),
        Key::of(&Value::Integer(i64::MAX), false)
    );
    // A Relationship from whole numbers to decimals matches 10^16 with 1e16.
    let fact = table(
        "Fact",
        vec![(
            "K",
            ints([10_000_000_000_000_000, 5, 10_000_000_000_000_000]),
        )],
    );
    let dim = table(
        "Dim",
        vec![
            (
                "K",
                Arc::new(Float64Array::from(vec![1e16, 5.0])) as ArrayRef,
            ),
            ("Pick", ints([1, 0])),
        ],
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Fact", "K", "Dim", "K"));
    let db = Database::new(&model, &[fact, dim]);
    assert_eq!(
        scalar(
            &db,
            "CALCULATE(COUNTROWS(Fact), Dim[Pick] = 1)",
            unbounded()
        )
        .unwrap(),
        Value::Integer(2)
    );
    assert_eq!(
        scalar(
            &db,
            "COUNTROWS(FILTER(Fact, RELATED(Dim[Pick]) = 1))",
            unbounded()
        )
        .unwrap(),
        Value::Integer(2)
    );
}

/// `k` stacked diamonds: `A0 → B0, A0 → C0, B0 → A1, C0 → A1, …, → Ak` (many → one), two rows
/// per Table, keys 0 and 1 matching straight down; `A*[V]` = the row number.
fn diamonds(k: usize) -> Database {
    let mut data = Vec::new();
    let mut model = Model::default();
    let two = || ints([0, 1]);
    for i in 0..=k {
        let a = format!("A{i}");
        data.push(table(
            &a,
            vec![("K", two()), ("V", two()), ("KB", two()), ("KC", two())],
        ));
        if i < k {
            for side in ["B", "C"] {
                let name = format!("{side}{i}");
                data.push(table(&name, vec![("K", two()), ("KA", two())]));
                model
                    .relationships
                    .push(Relationship::new(&a, &format!("K{side}"), &name, "K"));
                model.relationships.push(Relationship::new(
                    &name,
                    "KA",
                    &format!("A{}", i + 1),
                    "K",
                ));
            }
        }
    }
    Database::new(&model, &data)
}

#[test]
fn stacked_diamonds_propagate_in_linear_time() {
    let db = diamonds(30);
    let start = Instant::now();
    // Down 60 Relationships, along 2^30 paths: a filter on the last Table reaches the first.
    assert_eq!(
        scalar(&db, "CALCULATE(COUNTROWS(A0), A30[V] = 1)", unbounded()).unwrap(),
        Value::Integer(1)
    );
    // A table filter on the first Table projects onto the last.
    assert_eq!(
        scalar(
            &db,
            "CALCULATE(COUNTROWS(A30), FILTER(A0, A0[V] = 0))",
            unbounded()
        )
        .unwrap(),
        Value::Integer(1)
    );
    assert_eq!(
        scalar(&db, "CALCULATE(COUNTROWS(A0), ALL(A0))", unbounded()).unwrap(),
        Value::Integer(2)
    );
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn relationship_depth_counts_the_chain_not_the_tables() {
    // A star: one fact Table, 100 dimensions.
    let dims = 100;
    let mut model = Model::default();
    let mut columns: Vec<(String, ArrayRef)> = Vec::new();
    let mut data = Vec::new();
    for d in 0..dims {
        columns.push((format!("K{d}"), ints([0, 1, 1])));
        data.push(table(
            &format!("D{d}"),
            vec![("K", ints([0, 1])), ("V", ints([5, 6]))],
        ));
        model.relationships.push(Relationship::new(
            "Fact",
            &format!("K{d}"),
            &format!("D{d}"),
            "K",
        ));
    }
    let fact = table(
        "Fact",
        columns
            .iter()
            .map(|(n, a)| (n.as_str(), Arc::clone(a)))
            .collect(),
    );
    data.push(fact);
    let db = Database::new(&model, &data);
    for (expr, want) in [
        ("CALCULATE(COUNTROWS(Fact), D99[V] = 6)", 2),
        // The filter arguments apply after ALL removes the outer filters.
        ("CALCULATE(COUNTROWS(Fact), D99[V] = 6, ALL(Fact))", 2),
        (
            "CALCULATE(CALCULATE(COUNTROWS(Fact), ALL(Fact)), D99[V] = 6)",
            3,
        ),
        (
            "CALCULATE(CALCULATE(COUNTROWS(Fact), ALLEXCEPT(Fact, D0[V])), D0[V] = 5, D1[V] = 6)",
            1,
        ),
        (
            "CALCULATE(CALCULATE(COUNTROWS(Fact), REMOVEFILTERS(Fact)), D99[V] = 6)",
            3,
        ),
    ] {
        assert_eq!(
            scalar(&db, expr, unbounded()).unwrap_or_else(|e| panic!("{expr}: {e}")),
            Value::Integer(want),
            "{expr}"
        );
    }
    // A chain deeper than the cap is a Limit error, for filters and for ALL alike.
    let n = MAX_RELATIONSHIP_DEPTH + 6;
    let mut model = Model::default();
    let data: Vec<Table> = (0..n)
        .map(|i| table(&format!("T{i}"), vec![("K", ints([0])), ("Up", ints([0]))]))
        .collect();
    for i in 0..n - 1 {
        model.relationships.push(Relationship::new(
            &format!("T{i}"),
            "Up",
            &format!("T{}", i + 1),
            "K",
        ));
    }
    let db = Database::new(&model, &data);
    let last = format!("T{}", n - 1);
    for expr in [
        format!("CALCULATE(COUNTROWS(T0), {last}[K] = 0)"),
        "CALCULATE(COUNTROWS(T0), ALL(T0))".to_string(),
    ] {
        let e = scalar(&db, &expr, unbounded()).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Limit, "{expr}: {e}");
    }
}

#[test]
fn propagation_checks_the_cancel_flag() {
    let db = diamonds(2);
    let flag = AtomicBool::new(true);
    let ev = Evaluator::new(
        &db,
        Options {
            cancel: Some(&flag),
            memory_cap: usize::MAX,
        },
    );
    let a0 = db.table("A0").unwrap();
    let e = ev
        .mask(a0, &FilterContext::default(), Span::default())
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
}
