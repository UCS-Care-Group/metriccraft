//! M3.3: the column aggregations, on the datasets of parity cases 0001, 0029 and 0030.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Decimal64Array, Float64Array, Int64Array, RecordBatch,
    StringArray,
};
use metriccraft_core::Value;
use metriccraft_data::{Table, TableBuilder, fold_case};

use crate::Model;

// The DAX tests load the same helper; it is test-only and stateless.
#[allow(clippy::duplicate_mod)]
#[path = "../../../m/src/fuzz.rs"]
mod fuzz;
use crate::dax::storage::{self, Bitset, Cancel, Kind};
use crate::dax::*;

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

fn db_of(tables: Vec<Table>) -> Database {
    Database::new(&Model::default(), &tables)
}

/// The dataset of parity cases 0029 and 0030.
fn sales() -> Database {
    db_of(vec![table(
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
        ],
    )])
}

fn options() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

fn eval(db: &Database, expr: &str) -> Result<Value> {
    let q = format!("EVALUATE ROW(\"x\", {expr})");
    let mut r = run_query_text(db, &q, options())?;
    Ok(r.remove(0).rows.remove(0).remove(0))
}

fn one(db: &Database, expr: &str) -> Value {
    eval(db, expr).unwrap_or_else(|e| panic!("{expr}: {e}"))
}

fn copy(db: &Database, expr: &str) -> String {
    one(db, expr).copy_text()
}

fn error(db: &Database, expr: &str) -> DaxError {
    eval(db, expr).expect_err(expr)
}

#[test]
fn case_0001_aggregates_without_calculate() {
    let db = db_of(vec![table(
        "Sales",
        vec![
            (
                "Region",
                text(&[Some("North"), Some("North"), Some("South"), Some("West")]),
            ),
            ("Amount", floats(&[Some(10.5), None, Some(4.0), None])),
            ("Qty", ints(&[Some(2), Some(3), Some(1), Some(0)])),
        ],
    )]);
    assert_eq!(copy(&db, "SUM(Sales[Amount])"), "14.5");
    assert_eq!(copy(&db, "COUNT(Sales[Amount])"), "2");
    assert_eq!(copy(&db, "COUNTA(Sales[Amount])"), "2");
    assert_eq!(copy(&db, "COUNTROWS(Sales)"), "4");
    assert_eq!(copy(&db, "AVERAGE(Sales[Amount])"), "7.25");
    assert_eq!(copy(&db, "MIN(Sales[Amount])"), "4");
    assert_eq!(copy(&db, "MAX(Sales[Amount])"), "10.5");
    assert_eq!(
        copy(&db, "SUM(Sales[Amount]) / SUM(Sales[Qty])"),
        "2.4166666666666665"
    );
}

#[test]
fn case_0029_blanks_negatives_and_whole_numbers() {
    let db = sales();
    assert_eq!(one(&db, "SUM(Sales[Qty])"), Value::Integer(11));
    assert_eq!(one(&db, "AVERAGE(Sales[Qty])"), Value::Decimal(2.2));
    assert_eq!(one(&db, "MIN(Sales[Amount])"), Value::Decimal(-3.0));
    assert_eq!(one(&db, "MAX(Sales[Qty])"), Value::Integer(5));
    assert_eq!(one(&db, "COUNT(Sales[Qty])"), Value::Integer(5));
    assert_eq!(one(&db, "COUNTA(Sales[Product])"), Value::Integer(5));
    // Text counts for COUNT (documented).
    assert_eq!(one(&db, "COUNT(Sales[Product])"), Value::Integer(5));
    // Names are case-insensitive, and an unambiguous column needs no Table.
    assert_eq!(one(&db, "sum([qty])"), Value::Integer(11));
}

#[test]
fn case_0030_distinctcount_blanks_and_case() {
    let db = sales();
    // Apple/apple and Pear/pear are one value each, BLANK is one more.
    assert_eq!(one(&db, "DISTINCTCOUNT(Sales[Product])"), Value::Integer(3));
    assert_eq!(one(&db, "DISTINCTCOUNT(Sales[Region])"), Value::Integer(4));
    assert_eq!(one(&db, "DISTINCTCOUNT(Sales[Amount])"), Value::Integer(5));
    assert_eq!(one(&db, "DISTINCTCOUNT(Sales[Qty])"), Value::Integer(6));
    // Text compares ignoring case; the first spelling loaded is kept.
    assert_eq!(one(&db, "MIN(Sales[Product])"), Value::text("Apple"));
    assert_eq!(one(&db, "MAX(Sales[Product])"), Value::text("pear"));
}

#[test]
fn awaiting_cases_evaluate_their_non_calculate_checks() {
    let db = sales();
    for case in [
        include_str!("../../../../parity/observed/0029-aggregates-blank-edge.md"),
        include_str!("../../../../parity/observed/0030-distinctcount-case.md"),
    ] {
        let start = case.find("```dax\n").map_or(0, |i| i + 7);
        let end = case[start..].find("```").map_or(case.len(), |i| start + i);
        for line in case[start..end].lines() {
            let line = line.trim().trim_end_matches(',');
            let Some((_, expr)) = line.split_once(',') else {
                continue;
            };
            if line.starts_with('"') && !expr.contains("CALCULATE") {
                one(&db, expr.trim());
            }
        }
    }
}

#[test]
fn empty_and_all_blank_give_blank() {
    let db = db_of(vec![
        table("E", vec![("A", floats(&[])), ("T", text(&[]))]),
        table(
            "B",
            vec![("A", floats(&[None, None])), ("T", text(&[None, None]))],
        ),
    ]);
    for f in [
        "SUM",
        "AVERAGE",
        "COUNT",
        "COUNTA",
        "MIN",
        "MAX",
        "DISTINCTCOUNT",
    ] {
        assert_eq!(one(&db, &format!("{f}(E[A])")), Value::Blank, "{f}");
    }
    assert_eq!(one(&db, "COUNTROWS(E)"), Value::Blank);
    for f in ["SUM", "AVERAGE", "COUNT", "COUNTA", "MIN", "MAX"] {
        assert_eq!(one(&db, &format!("{f}(B[A])")), Value::Blank, "{f}");
    }
    // BLANK is a value to DISTINCTCOUNT.
    assert_eq!(one(&db, "DISTINCTCOUNT(B[A])"), Value::Integer(1));
    assert_eq!(one(&db, "DISTINCTCOUNT(B[T])"), Value::Integer(1));
    assert_eq!(one(&db, "MIN(B[T])"), Value::Blank);
    // A sum of zeros is zero, not BLANK.
    let z = db_of(vec![table("Z", vec![("Q", ints(&[Some(0), None]))])]);
    assert_eq!(one(&z, "SUM(Z[Q])"), Value::Integer(0));
}

#[test]
fn types_follow_the_column() {
    let fixed: ArrayRef = Arc::new(
        Decimal64Array::from(vec![Some(12_500), None, Some(-2_500), Some(1)])
            .with_precision_and_scale(18, 4)
            .unwrap(),
    );
    let dates: ArrayRef = Arc::new(Date32Array::from(vec![Some(0), Some(2), None, Some(1)]));
    let db = db_of(vec![table("T", vec![("F", fixed), ("D", dates)])]);
    // Fixed decimal stays fixed decimal (exact).
    assert_eq!(one(&db, "SUM(T[F])"), Value::Currency(10_001));
    assert_eq!(one(&db, "MIN(T[F])"), Value::Currency(-2_500));
    assert_eq!(one(&db, "MAX(T[F])"), Value::Currency(12_500));
    assert_eq!(one(&db, "AVERAGE(T[F])"), Value::Decimal(1.0001 / 3.0));
    // Dates: 1970-01-01 is day 25569.
    assert_eq!(one(&db, "MIN(T[D])"), Value::DateTime(25_569.0));
    assert_eq!(one(&db, "MAX(T[D])"), Value::DateTime(25_571.0));
    assert_eq!(one(&db, "AVERAGE(T[D])"), Value::DateTime(25_570.0));
    assert_eq!(one(&db, "COUNT(T[D])"), Value::Integer(3));
}

#[test]
fn whole_number_sums_overflow_as_an_error() {
    let db = db_of(vec![table(
        "T",
        vec![
            ("N", ints(&[Some(i64::MAX), Some(1)])),
            ("M", ints(&[Some(i64::MAX), Some(i64::MAX)])),
        ],
    )]);
    let e = error(&db, "SUM(T[N])");
    assert!(e.message.contains("too large"), "{e}");
    assert_eq!(e.kind, ErrorKind::Eval);
    // AVERAGE sums exactly, so it doesn't overflow.
    assert_eq!(one(&db, "AVERAGE(T[M])"), Value::Decimal(i64::MAX as f64));
}

#[test]
fn wrong_arguments_are_errors_with_their_span() {
    let bools: ArrayRef = Arc::new(BooleanArray::from(vec![Some(true), None, Some(false)]));
    let db = db_of(vec![
        table(
            "Sales",
            vec![
                ("Region", text(&[Some("a")])),
                ("Amount", floats(&[Some(1.0)])),
            ],
        ),
        table(
            "B",
            vec![
                ("Flag", bools),
                ("Amount", floats(&[Some(2.0), None, None])),
            ],
        ),
    ]);
    for f in ["SUM", "AVERAGE"] {
        let q = format!("EVALUATE ROW(\"x\", {f}(Sales[Region]))");
        let e = run_query_text(&db, &q, options()).expect_err(&q);
        assert_eq!(&q[e.span.start..e.span.end], "Sales[Region]", "{e}");
        assert!(e.message.contains("Text"), "{e}");
    }
    for f in ["SUM", "COUNT", "MIN", "MAX"] {
        let e = error(&db, &format!("{f}(B[Flag])"));
        assert!(e.message.contains("True/False"), "{f}: {e}");
    }
    assert_eq!(one(&db, "COUNTA(B[Flag])"), Value::Integer(2));
    assert_eq!(one(&db, "DISTINCTCOUNT(B[Flag])"), Value::Integer(3));
    assert!(error(&db, "SUM(1)").message.contains("needs a column"));
    assert!(error(&db, "SUM(Sales[Nope])").message.contains("no column"));
    assert!(
        error(&db, "SUM(Nope[Amount])")
            .message
            .contains("no column")
    );
    assert!(
        error(&db, "SUM([Amount])")
            .message
            .contains("several Tables")
    );
    assert!(
        error(&db, "SUM(Sales[Amount], 1)")
            .message
            .contains("arguments")
    );
    assert!(error(&db, "MIN()").message.contains("arguments"));
}

#[test]
fn min_max_of_two_values() {
    let db = sales();
    assert_eq!(one(&db, "MIN(1, 2)"), Value::Integer(1));
    assert_eq!(one(&db, "MAX(1, 2.5)"), Value::Decimal(2.5));
    // BLANK compares as zero and, when it wins, stays BLANK (case 0044).
    assert_eq!(one(&db, "MIN(1, BLANK())"), Value::Blank);
    assert_eq!(one(&db, "MIN(-1, BLANK())"), Value::Integer(-1));
    assert_eq!(one(&db, "MAX(-1, BLANK())"), Value::Blank);
    assert_eq!(one(&db, "MAX(BLANK(), BLANK())"), Value::Blank);
    assert_eq!(one(&db, "MIN(\"b\", \"A\")"), Value::text("A"));
    assert_eq!(one(&db, "MAX(SUM(Sales[Qty]), 20)"), Value::Integer(20));
    assert!(error(&db, "MIN(1, \"a\")").message.contains("compared"));
    assert!(error(&db, "MIN(TRUE(), 1)").message.contains("True/False"));
}

#[test]
fn a_row_context_does_not_filter_an_aggregation() {
    let db = sales();
    let r = run_query_text(
        &db,
        "EVALUATE Sales ORDER BY SUM(Sales[Qty]) + Sales[Qty]",
        options(),
    )
    .unwrap();
    assert_eq!(r[0].rows.len(), 6);
    // Inside a measure the whole table is still summed.
    assert_eq!(
        run_query_text(
            &db,
            "DEFINE MEASURE Sales[Q] = SUM(Sales[Qty]) EVALUATE ROW(\"q\", [Q] * 2)",
            options()
        )
        .unwrap()[0]
            .rows[0][0],
        Value::Integer(22)
    );
}

#[test]
fn scans_honour_the_row_mask() {
    let db = sales();
    let t = &db.tables[0];
    let col = |name: &str| &t.columns[t.column(name).unwrap()].data;
    let mut mask = Bitset::new(t.rows, false);
    for r in [1, 3, 5] {
        mask.set(r);
    }
    let none = Cancel::default();
    let m = Some(&mask);
    assert_eq!(
        storage::count_values(col("Amount"), t.rows, m, none).unwrap(),
        2
    );
    assert_eq!(
        storage::count_values(col("Qty"), t.rows, m, none).unwrap(),
        2
    );
    assert_eq!(
        storage::count_values(col("Region"), t.rows, m, none).unwrap(),
        3
    );
    assert_eq!(
        storage::distinct_count(col("Product"), t.rows, m, none).unwrap(),
        2
    );
    assert_eq!(
        storage::text_extreme(col("Product"), t.rows, m, none, false).unwrap(),
        Some(Value::text("pear"))
    );
    let mut sum = 0.0;
    storage::each_number(col("Amount"), t.rows, m, none, |n| {
        if let storage::Number::Float(f) = n {
            sum += f;
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(sum, 4.25);
    assert_eq!(col("Qty").kind(), Kind::Integer);
    assert_eq!(col("Region").kind(), Kind::Text);
    let empty = Bitset::new(t.rows, false);
    assert_eq!(
        storage::distinct_count(col("Product"), t.rows, Some(&empty), none).unwrap(),
        0
    );
    // Row counts beyond the column are ignored, not read.
    assert_eq!(
        storage::count_values(col("Qty"), t.rows + 10, None, none).unwrap(),
        5
    );
}

#[test]
fn cancel_stops_an_aggregation() {
    let db = sales();
    let flag = AtomicBool::new(true);
    let opts = Options {
        cancel: Some(&flag),
        memory_cap: usize::MAX,
    };
    for f in [
        "SUM(Sales[Qty])",
        "COUNT(Sales[Amount])",
        "DISTINCTCOUNT(Sales[Product])",
        "MAX(Sales[Product])",
    ] {
        let q = format!("EVALUATE ROW(\"x\", {f})");
        let e = run_query_text(&db, &q, opts).expect_err(f);
        assert_eq!(e.kind, ErrorKind::Cancelled, "{f}");
    }
}

#[test]
fn large_scans_cross_chunks() {
    let n = storage::CHUNK * 2 + 7;
    let v: Vec<Option<i64>> = (0..n)
        .map(|i| (i % 3 != 0).then_some(i as i64 % 10))
        .collect();
    let db = db_of(vec![table("T", vec![("N", ints(&v))])]);
    let expect_sum: i64 = v.iter().flatten().sum();
    let expect_count = v.iter().flatten().count() as i64;
    assert_eq!(one(&db, "SUM(T[N])"), Value::Integer(expect_sum));
    assert_eq!(one(&db, "COUNT(T[N])"), Value::Integer(expect_count));
    assert_eq!(one(&db, "DISTINCTCOUNT(T[N])"), Value::Integer(11));
    assert_eq!(one(&db, "MAX(T[N])"), Value::Integer(9));
}

/// 3,000 mutated aggregation queries (`MCRAFT_FUZZ_SCALE` multiplies) on the 0029/0030 data:
/// none may panic or hang.
#[test]
fn fuzzed_aggregations_never_panic() {
    let db = sales();
    let seeds = [
        "EVALUATE ROW(\"a\", SUM(Sales[Qty]), \"b\", AVERAGE(Sales[Amount]), \"c\", COUNT(Sales[Product]))",
        "EVALUATE ROW(\"a\", COUNTA(Sales[Qty]), \"b\", DISTINCTCOUNT(Sales[Product]), \"c\", MIN(Sales[Region]))",
        "EVALUATE ROW(\"a\", MAX(Sales[Amount], 1), \"b\", MIN(BLANK(), -1), \"c\", MAX([Qty]))",
    ];
    let alphabet: Vec<char> = "'\"()[],+-*/ 019SUM AVERAGE COUNT COUNTA DISTINCTCOUNT MIN MAX Sales[Qty] [Amount] BLANK()"
        .chars()
        .collect();
    let mut rng = fuzz::Fuzz::new("dax-aggregate", 0xA66_5EED_0033_0003);
    for _ in 0..rng.scaled(3_000) {
        let mut chars: Vec<char> = seeds[rng.below(seeds.len())].chars().collect();
        for _ in 0..=rng.below(4) {
            let at = rng.below(chars.len() + 1);
            if rng.below(2) == 0 && at < chars.len() {
                chars.remove(at);
            } else {
                chars.insert(at, alphabet[rng.below(alphabet.len())]);
            }
        }
        let src: String = chars.into_iter().collect();
        rng.case(src.as_bytes(), || {
            let _ = run_query_text(&db, &src, options());
        });
    }
    rng.finish();
}
