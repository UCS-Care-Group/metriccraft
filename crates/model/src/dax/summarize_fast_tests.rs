//! M4.7 tests: the one-pass group-by gives exactly the per-group path's results (a differential
//! test over random group-bys, aggregations and filters), each aggregation's BLANK rules, the
//! fallbacks, cancellation and the memory cap.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arrow_array::{
    ArrayRef, BooleanArray, Date32Array, Decimal64Array, Float64Array, Int64Array, NullArray,
    RecordBatch, StringArray,
};
use opendash_core::Value;
use opendash_data::{Table, TableBuilder, fold_case};
use opendash_dax::{Expr, parse_expr};

use super::{ColumnRef, FAST_HITS, FORCE_SLOW, summarize};
use crate::dax::*;
use crate::{Model, Relationship};

/// splitmix64: the same rows every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }

    fn pick<'a, T>(&mut self, from: &'a [T]) -> &'a T {
        &from[self.below(from.len())]
    }
}

/// A Table of `columns`; `fold`: case-fold its dictionaries as a load does (unfolded keeps
/// spellings that differ only in case as separate codes).
fn table(name: &str, columns: Vec<(&str, ArrayRef)>, fold: bool) -> Table {
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    let t = b.finish().unwrap();
    if fold {
        fold_case(t, usize::MAX).unwrap()
    } else {
        t
    }
}

fn text(v: Vec<Option<&str>>) -> ArrayRef {
    Arc::new(StringArray::from(v))
}

fn ints(v: Vec<Option<i64>>) -> ArrayRef {
    Arc::new(Int64Array::from(v))
}

/// Sales (unfolded text, every column kind, blanks, negatives) → Regions (by text key, one
/// with a trailing space) and → Customers (by whole-number key, some keys unmatched).
fn fixture(seed: u64, rows: usize) -> Database {
    let mut rng = Rng(seed);
    let regions = [
        Some("North"),
        Some("north"),
        Some("NORTH"),
        Some("South"),
        Some("East"),
        Some("West "),
        Some("Nowhere"),
        None,
    ];
    let products = [
        Some("Apple"),
        Some("APPLE"),
        Some("pear"),
        Some("Plum"),
        None,
    ];
    let mut region = Vec::new();
    let mut product = Vec::new();
    let mut qty = Vec::new();
    let mut amount = Vec::new();
    let mut price = Vec::new();
    let mut rate = Vec::new();
    let mut day = Vec::new();
    let mut flag = Vec::new();
    let mut customer = Vec::new();
    for _ in 0..rows {
        region.push(*rng.pick(&regions));
        product.push(*rng.pick(&products));
        qty.push((!rng.chance(10)).then(|| rng.below(26) as i64 - 5));
        amount.push((!rng.chance(15)).then(|| (rng.below(20_001) as f64 - 10_000.0) / 100.0));
        price.push((!rng.chance(10)).then(|| rng.below(2_000_000) as i64 - 500_000));
        rate.push((!rng.chance(10)).then(|| rng.below(1_000) as i64 - 100));
        day.push((!rng.chance(5)).then(|| 18_000 + rng.below(60) as i32));
        flag.push((!rng.chance(5)).then(|| rng.chance(30)));
        customer.push((!rng.chance(5)).then(|| 1 + rng.below(33) as i64));
    }
    let sales = table(
        "Sales",
        vec![
            ("Region", text(region)),
            ("Product", text(product)),
            ("Qty", ints(qty)),
            ("Amount", Arc::new(Float64Array::from(amount))),
            (
                "Price",
                Arc::new(
                    Decimal64Array::from(price)
                        .with_precision_and_scale(18, 4)
                        .unwrap(),
                ),
            ),
            (
                "Rate",
                Arc::new(
                    Decimal64Array::from(rate)
                        .with_precision_and_scale(18, 2)
                        .unwrap(),
                ),
            ),
            ("Day", Arc::new(Date32Array::from(day))),
            ("Flag", Arc::new(BooleanArray::from(flag))),
            ("CustomerID", ints(customer)),
            ("Nothing", Arc::new(NullArray::new(rows))),
        ],
        false,
    );
    let dims = table(
        "Regions",
        vec![
            (
                "Region",
                text(vec![
                    Some("North"),
                    Some("South"),
                    Some("East"),
                    Some("West"),
                    Some("Central"),
                ]),
            ),
            (
                "Manager",
                text(vec![
                    Some("Ann"),
                    Some("ann"),
                    Some("Bob"),
                    None,
                    Some("Cy"),
                ]),
            ),
            ("Tier", ints(vec![Some(1), Some(2), Some(1), Some(3), None])),
        ],
        false,
    );
    let segments = [Some("A"), Some("b"), Some("B"), Some("c"), None];
    let customers = table(
        "Customers",
        vec![
            ("CustomerID", ints((1..=30).map(Some).collect())),
            (
                "Segment",
                text((0..30).map(|i| segments[i % segments.len()]).collect()),
            ),
            (
                "Since",
                Arc::new(Date32Array::from(
                    (0..30).map(|i| Some(17_000 + (i % 7))).collect::<Vec<_>>(),
                )),
            ),
        ],
        true,
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Sales", "Region", "Regions", "Region"));
    model.relationships.push(Relationship::new(
        "Sales",
        "CustomerID",
        "Customers",
        "CustomerID",
    ));
    Database::new(&model, &[sales, dims, customers])
}

fn options() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

/// `f` with the per-group path forced (`slow`) or not.
fn with_path<T>(slow: bool, f: impl FnOnce() -> T) -> T {
    FORCE_SLOW.with(|s| s.set(slow));
    let r = f();
    FORCE_SLOW.with(|s| s.set(false));
    r
}

fn hits() -> usize {
    FAST_HITS.with(std::cell::Cell::get)
}

/// Runs `summarize` both ways and checks they agree exactly; whether the one-pass path ran.
fn both(
    db: &Database,
    group_by: &[ColumnRef<'_>],
    measures: &[(&str, &Expr)],
    fc: &FilterContext,
    what: &str,
) -> (Result<ResultTable>, bool) {
    let before = hits();
    let fast = with_path(false, || summarize(db, group_by, measures, fc, options()));
    let used = hits() > before;
    let slow = with_path(true, || summarize(db, group_by, measures, fc, options()));
    match (&fast, &slow) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "{what}"),
        (Err(a), Err(b)) => assert_eq!((a.kind, &a.message), (b.kind, &b.message), "{what}"),
        _ => panic!("{what}: fast {fast:?} vs slow {slow:?}"),
    }
    (fast, used)
}

/// Both ways through DAX query text.
fn both_query(db: &Database, q: &str) -> (Result<Vec<ResultTable>>, bool) {
    let before = hits();
    let fast = with_path(false, || run_query_text(db, q, options()));
    let used = hits() > before;
    let slow = with_path(true, || run_query_text(db, q, options()));
    match (&fast, &slow) {
        (Ok(a), Ok(b)) => assert_eq!(a, b, "{q}"),
        (Err(a), Err(b)) => assert_eq!((a.kind, &a.message), (b.kind, &b.message), "{q}"),
        _ => panic!("{q}: fast {fast:?} vs slow {slow:?}"),
    }
    (fast, used)
}

const GROUP_BY: &[(&str, &str)] = &[
    ("Sales", "Region"),
    ("Sales", "Product"),
    ("Sales", "Qty"),
    ("Sales", "Day"),
    ("Sales", "Flag"),
    ("Sales", "Amount"),
    ("Sales", "Price"),
    ("Sales", "CustomerID"),
    ("Sales", "Nothing"),
    ("Regions", "Manager"),
    ("Regions", "Tier"),
    ("Regions", "Region"),
    ("Customers", "Segment"),
    ("Customers", "Since"),
];

const MEASURES: &[&str] = &[
    "SUM(Sales[Qty])",
    "SUM(Sales[Amount])",
    "SUM(Sales[Price])",
    "SUM(Sales[Rate])",
    "SUM(Sales[Day])",
    "SUM(Sales[Nothing])",
    "AVERAGE(Sales[Qty])",
    "AVERAGE(Sales[Amount])",
    "AVERAGE(Sales[Price])",
    "AVERAGE(Sales[Day])",
    "MIN(Sales[Qty])",
    "MAX(Sales[Amount])",
    "MIN(Sales[Price])",
    "MAX(Sales[Day])",
    "MIN(Sales[Region])",
    "MAX(Sales[Product])",
    "MAX(Sales[Nothing])",
    "COUNT(Sales[Region])",
    "COUNT(Sales[Amount])",
    "COUNT(Sales[Nothing])",
    "COUNTA(Sales[Flag])",
    "COUNTA(Sales[Product])",
    "COUNTROWS(Sales)",
    "count(sales[qty])",
    "DISTINCTCOUNT(Sales[Region])",
    "DISTINCTCOUNT(Sales[Qty])",
    "DISTINCTCOUNT(Sales[Amount])",
    "DISTINCTCOUNT(Sales[Day])",
    "DISTINCTCOUNT(Sales[Flag])",
    "DISTINCTCOUNT(Sales[Nothing])",
    "DISTINCTCOUNT(Sales[Price])",
    // Not plain aggregations of Sales: the per-group path runs.
    "SUM(Sales[Qty]) + 1",
    "COUNTROWS(Regions)",
    "MAX(Regions[Tier])",
    // Errors (a type the function rejects): the per-group path's error.
    "SUM(Sales[Region])",
    "MIN(Sales[Flag])",
];

/// A filter on one of a few columns, allowing 1–3 of its values (BLANK among them sometimes).
fn random_filter(db: &Database, rng: &mut Rng) -> Filter {
    let on = [
        ("Sales", "Product"),
        ("Sales", "Day"),
        ("Sales", "Qty"),
        ("Sales", "Region"),
        ("Regions", "Manager"),
        ("Customers", "Segment"),
    ];
    let (t, c) = *rng.pick(&on);
    let ti = db.table(t).unwrap();
    let dt = &db.tables[ti];
    let ci = dt.column(c).unwrap();
    let col = &dt.columns[ci].data;
    let n = 1 + rng.below(3);
    let values: Vec<Value> = (0..n)
        .map(|_| {
            if rng.chance(10) {
                Value::Blank
            } else {
                col.value(rng.below(dt.rows))
            }
        })
        .collect();
    Filter::values(dt, ti, ci, &values)
}

#[test]
fn one_pass_matches_per_group_on_random_queries() {
    let exprs: Vec<Expr> = MEASURES.iter().map(|m| parse_expr(m).unwrap()).collect();
    let mut fast_runs = 0;
    let mut cases = 0;
    // 128-row batches in tests: 2 batches run on one thread, 3 or more on several.
    for (seed, rows) in [(1u64, 300), (2, 250), (3, 600)] {
        let db = fixture(seed, rows);
        let mut rng = Rng(seed * 7919);
        for case in 0..100 {
            let mut gb: Vec<ColumnRef<'_>> = Vec::new();
            for _ in 0..1 + rng.below(3) {
                let (table, column) = *rng.pick(GROUP_BY);
                let c = ColumnRef { table, column };
                if !gb.contains(&c) {
                    gb.push(c);
                }
            }
            let names = ["a", "b", "c"];
            let mut ms: Vec<(&str, &Expr)> = Vec::new();
            for name in names.iter().take(1 + rng.below(3)) {
                // Mostly plain aggregations, so most cases take the one-pass path.
                let k = if rng.chance(85) {
                    rng.below(MEASURES.len() - 5)
                } else {
                    rng.below(MEASURES.len())
                };
                ms.push((name, &exprs[k]));
            }
            let mut fc = FilterContext::default();
            for _ in 0..rng.below(3) {
                fc = fc.with(random_filter(&db, &mut rng));
            }
            let what = format!(
                "seed {seed} case {case}: {gb:?} {:?} {:?}",
                ms.iter()
                    .map(|(_, e)| opendash_dax::print_expr(e))
                    .collect::<Vec<_>>(),
                fc.filters
            );
            let (_, used) = both(&db, &gb, &ms, &fc, &what);
            fast_runs += usize::from(used);
            cases += 1;
        }
    }
    // Most random cases are plain aggregations of Sales grouped within reach.
    assert!(
        fast_runs * 2 > cases,
        "{fast_runs} of {cases} took the one-pass path"
    );
}

#[test]
fn measure_chains_and_query_filters_take_the_one_pass_path() {
    let db = fixture(5, 300);
    let q = "DEFINE MEASURE Sales[Total] = SUM(Sales[Amount]) \
             MEASURE Sales[Again] = [Total] \
             MEASURE Sales[Rows] = COUNTROWS(Sales) \
             EVALUATE SUMMARIZECOLUMNS(Sales[Region], Regions[Manager], Customers[Segment], \
               \"t\", [Again], \"n\", Sales[Rows], \"d\", DISTINCTCOUNT(Sales[Product]))";
    let (r, used) = both_query(&db, q);
    assert!(used);
    assert!(!r.unwrap()[0].rows.is_empty());
    let q = "EVALUATE CALCULATETABLE(SUMMARIZECOLUMNS(Sales[Product], Sales[Day], \
             \"q\", SUM(Sales[Qty]), \"m\", MIN(Sales[Region])), Sales[Region] = \"north\")";
    let _ = both_query(&db, q);
    let q = "EVALUATE CALCULATETABLE(SUMMARIZECOLUMNS(Regions[Tier], \"q\", AVERAGE(Sales[Price])), \
             Customers[Segment] = \"b\")";
    let _ = both_query(&db, q);
}

#[test]
fn falls_back_where_it_must() {
    let db = fixture(9, 200);
    // A row-set filter (FILTER over the Table) restricts expanded tables: per-group path.
    let q = "EVALUATE CALCULATETABLE(SUMMARIZECOLUMNS(Regions[Manager], \"q\", SUM(Sales[Qty])), \
             FILTER(Sales, Sales[Qty] > 3))";
    let (_, used) = both_query(&db, q);
    assert!(!used);
    // Not plain.
    let e = parse_expr("SUM(Sales[Qty]) * 2").unwrap();
    let gb = [ColumnRef {
        table: "Sales",
        column: "Product",
    }];
    let (_, used) = both(
        &db,
        &gb,
        &[("x", &e)],
        &FilterContext::default(),
        "not plain",
    );
    assert!(!used);
    // A query VAR named like the Table: COUNTROWS counts the variable.
    let q = "DEFINE VAR Sales = {1, 2} \
             EVALUATE SUMMARIZECOLUMNS('Sales'[Product], \"n\", COUNTROWS(Sales))";
    let (_, used) = both_query(&db, q);
    assert!(!used);
}

#[test]
fn a_dimension_with_a_repeated_key_falls_back() {
    let sales = table(
        "Sales",
        vec![
            ("K", text(vec![Some("a"), Some("b"), Some("a")])),
            ("V", ints(vec![Some(1), Some(2), Some(4)])),
        ],
        true,
    );
    // Key "a" twice: a Sales row belongs to both of its groups.
    let dim = table(
        "Dim",
        vec![
            ("K", text(vec![Some("a"), Some("A"), Some("b")])),
            ("Name", text(vec![Some("x"), Some("y"), Some("z")])),
        ],
        false,
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Sales", "K", "Dim", "K"));
    let db = Database::new(&model, &[sales, dim]);
    let e = parse_expr("SUM(Sales[V])").unwrap();
    let gb = [ColumnRef {
        table: "Dim",
        column: "Name",
    }];
    let (r, used) = both(&db, &gb, &[("s", &e)], &FilterContext::default(), "dup");
    assert!(!used);
    assert_eq!(r.unwrap().rows.len(), 3);
}

/// One small Table for the BLANK rules: (Key, Amount, N, Word).
fn small() -> Database {
    let t = table(
        "T",
        vec![
            (
                "Key",
                text(vec![Some("a"), Some("b"), Some("b"), Some("c"), Some("A")]),
            ),
            (
                "Amount",
                Arc::new(Float64Array::from(vec![
                    None,
                    Some(1.5),
                    Some(-0.5),
                    None,
                    None,
                ])),
            ),
            (
                "N",
                ints(vec![None, Some(2), Some(i64::MAX), Some(3), None]),
            ),
            (
                "Word",
                text(vec![Some("b"), Some("Zed"), Some("zed"), None, Some("B")]),
            ),
        ],
        false,
    );
    Database::new(&Model::default(), &[t])
}

fn grouped(db: &Database, measures: &[&str]) -> (Vec<Vec<Value>>, bool) {
    let exprs: Vec<Expr> = measures.iter().map(|m| parse_expr(m).unwrap()).collect();
    let named: Vec<(&str, &Expr)> = exprs
        .iter()
        .enumerate()
        .map(|(i, e)| (["a", "b", "c"][i], e))
        .collect();
    let gb = [ColumnRef {
        table: "T",
        column: "Key",
    }];
    let (r, used) = both(db, &gb, &named, &FilterContext::default(), "small");
    (r.unwrap().rows, used)
}

fn t(s: &str) -> Value {
    Value::text(s)
}

#[test]
fn blank_rules_per_aggregation() {
    let db = small();
    // Groups (first spelling kept, case ignored): a (rows 0, 4), b (1, 2), c (3).
    let (rows, used) = grouped(&db, &["SUM(T[Amount])"]);
    assert!(used);
    assert_eq!(rows, vec![vec![t("b"), Value::Decimal(1.0)]]);
    let (rows, _) = grouped(&db, &["SUM(T[Amount])", "COUNTROWS(T)"]);
    assert_eq!(
        rows,
        vec![
            vec![t("a"), Value::Blank, Value::Integer(2)],
            vec![t("b"), Value::Decimal(1.0), Value::Integer(2)],
            vec![t("c"), Value::Blank, Value::Integer(1)],
        ]
    );
    let (rows, _) = grouped(&db, &["COUNT(T[Amount])", "AVERAGE(T[Amount])"]);
    assert_eq!(
        rows,
        vec![vec![t("b"), Value::Integer(2), Value::Decimal(0.5)]]
    );
    // DISTINCTCOUNT counts BLANK as a value.
    let (rows, _) = grouped(&db, &["DISTINCTCOUNT(T[Amount])"]);
    assert_eq!(
        rows,
        vec![
            vec![t("a"), Value::Integer(1)],
            vec![t("b"), Value::Integer(2)],
            vec![t("c"), Value::Integer(1)],
        ]
    );
    // Text MIN/MAX ignore case; of equal spellings the dictionary's first.
    let (rows, _) = grouped(&db, &["MIN(T[Word])", "MAX(T[Word])", "COUNTA(T[Word])"]);
    assert_eq!(
        rows,
        vec![
            vec![t("a"), t("b"), t("b"), Value::Integer(2)],
            vec![t("b"), t("Zed"), t("Zed"), Value::Integer(2)],
        ]
    );
    let (rows, _) = grouped(&db, &["MIN(T[N])", "MAX(T[N])"]);
    assert_eq!(
        rows,
        vec![
            vec![t("b"), Value::Integer(2), Value::Integer(i64::MAX)],
            vec![t("c"), Value::Integer(3), Value::Integer(3)],
        ]
    );
    let (rows, _) = grouped(&db, &["AVERAGE(T[N])"]);
    assert_eq!(rows[1], vec![t("c"), Value::Decimal(3.0)]);
}

#[test]
fn whole_number_sum_overflow_is_the_per_group_error() {
    let db = small();
    let e = parse_expr("SUM(T[N])").unwrap();
    let gb = [ColumnRef {
        table: "T",
        column: "Key",
    }];
    let (r, used) = both(
        &db,
        &gb,
        &[("s", &e)],
        &FilterContext::default(),
        "overflow",
    );
    assert!(!used);
    assert_eq!(r.unwrap_err().kind, ErrorKind::Eval);
}

#[test]
fn a_cancelled_query_stops() {
    let db = fixture(3, 300);
    let flag = AtomicBool::new(true);
    let e = parse_expr("SUM(Sales[Qty])").unwrap();
    let gb = [ColumnRef {
        table: "Sales",
        column: "Product",
    }];
    let options = Options {
        cancel: Some(&flag),
        memory_cap: usize::MAX,
    };
    let err = summarize(&db, &gb, &[("s", &e)], &FilterContext::default(), options).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Cancelled);
}

#[test]
fn the_memory_cap_bounds_the_groups() {
    let db = fixture(4, 2_000);
    let e = parse_expr("SUM(Sales[Amount])").unwrap();
    let gb = [
        ColumnRef {
            table: "Sales",
            column: "Amount",
        },
        ColumnRef {
            table: "Sales",
            column: "Product",
        },
    ];
    let before = hits();
    let options = Options {
        cancel: None,
        memory_cap: db.loaded_bytes + 4_096,
    };
    let err = summarize(&db, &gb, &[("s", &e)], &FilterContext::default(), options).unwrap_err();
    assert_eq!(err.kind, ErrorKind::Memory);
    assert_eq!(hits(), before);
    // With room, the same query takes the one-pass path.
    let (_, used) = both(&db, &gb, &[("s", &e)], &FilterContext::default(), "room");
    assert!(used);
}

#[test]
fn date_filters_match_per_row() {
    let db = fixture(6, 500);
    let ti = db.table("Sales").unwrap();
    let dt = &db.tables[ti];
    let ci = dt.column("Day").unwrap();
    let col = &dt.columns[ci].data;
    let values = [
        col.value(3),
        col.value(10),
        Value::Decimal(18_010.5),
        Value::Blank,
    ];
    let f = Filter::values(dt, ti, ci, &values);
    let mask = f.rows(dt, storage::Cancel::default()).unwrap();
    let keys: Vec<_> = values.iter().map(|v| Key::of(v, false)).collect();
    for r in 0..dt.rows {
        assert_eq!(
            mask.get(r),
            keys.contains(&Key::of(&col.value(r), false)),
            "row {r}"
        );
    }
}

#[test]
fn whole_numbers_beyond_the_dense_range_hash() {
    let n = 64;
    let wide: Vec<Option<i64>> = (0..n).map(|i| Some((i % 5) * (1 << 40) - 7)).collect();
    let t = table(
        "W",
        vec![
            ("K", ints(wide)),
            ("V", ints((0..n).map(|i| Some(i % 3)).collect())),
        ],
        false,
    );
    let db = Database::new(&Model::default(), &[t]);
    let e = parse_expr("SUM(W[V])").unwrap();
    let d = parse_expr("DISTINCTCOUNT(W[K])").unwrap();
    let gb = [ColumnRef {
        table: "W",
        column: "K",
    }];
    let (r, used) = both(
        &db,
        &gb,
        &[("s", &e), ("d", &d)],
        &FilterContext::default(),
        "wide",
    );
    assert!(used);
    assert_eq!(r.unwrap().rows.len(), 5);
}
