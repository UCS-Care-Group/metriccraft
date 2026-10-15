use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use metriccraft_core::Value;
use metriccraft_data::{Table, TableBuilder};

use super::eval::{RowFrame, TableVal};
use super::*;
use crate::{Model, Relationship};

#[path = "../../../m/src/fuzz.rs"]
mod fuzz;

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

/// Sales (many) → Product (one) on ProductKey → Key. Sales row 4 has a key with no Product.
fn data() -> (Model, Vec<Table>) {
    let sales = table(
        "Sales",
        vec![
            (
                "Region",
                text(&[
                    Some("North"),
                    Some("north"),
                    Some("South"),
                    None,
                    Some("West"),
                ]),
            ),
            (
                "Amount",
                Arc::new(Float64Array::from(vec![
                    Some(10.5),
                    None,
                    Some(4.0),
                    Some(1.0),
                    Some(2.0),
                ])),
            ),
            (
                "ProductKey",
                ints(&[Some(1), Some(2), Some(1), Some(3), Some(9)]),
            ),
        ],
    );
    let product = table(
        "Product",
        vec![
            ("Key", ints(&[Some(1), Some(2), Some(3)])),
            ("Colour", text(&[Some("Red"), Some("Blue"), Some("RED")])),
        ],
    );
    let mut model = Model::default();
    model
        .relationships
        .push(Relationship::new("Sales", "ProductKey", "Product", "Key"));
    (model, vec![sales, product])
}

fn db() -> Database {
    let (model, data) = data();
    Database::new(&model, &data)
}

fn options() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

fn run(db: &Database, q: &str) -> Result<Vec<ResultTable>> {
    run_query_text(db, q, options())
}

/// The first result's cells as copy text, row by row.
fn cells(db: &Database, q: &str) -> Vec<Vec<String>> {
    let r = run(db, q).unwrap_or_else(|e| panic!("{q}: {e}"));
    r[0].rows
        .iter()
        .map(|row| row.iter().map(Value::copy_text).collect())
        .collect()
}

fn one(db: &Database, expr: &str) -> String {
    cells(db, &format!("EVALUATE ROW(\"x\", {expr})"))[0][0].clone()
}

fn error(db: &Database, q: &str) -> DaxError {
    run(db, q).expect_err(q)
}

fn rows_of(mask: Option<Bitset>, n: usize) -> Vec<usize> {
    match mask {
        None => (0..n).collect(),
        Some(m) => m.ones_in(0, n).collect(),
    }
}

#[test]
fn bitset_basics() {
    let mut b = Bitset::new(130, false);
    for i in [0, 63, 64, 129, 500] {
        b.set(i);
    }
    assert_eq!(b.ones_in(0, 130).collect::<Vec<_>>(), [0, 63, 64, 129]);
    assert_eq!(b.ones_in(1, 129).collect::<Vec<_>>(), [63, 64]);
    assert_eq!(b.count(), 4);
    let mut all = Bitset::new(130, true);
    assert_eq!(all.count(), 130);
    all.and_with(&b);
    assert_eq!(all, b);
    assert!(!b.get(1000));
}

#[test]
fn filter_mask_on_one_table() {
    let db = db();
    let ev = Evaluator::new(&db, options());
    let sales = db.table("sales").unwrap();
    let region = db.tables[sales].column("REGION").unwrap();
    let span = Span::default();
    // No filters: every row, without building a mask.
    assert_eq!(
        ev.mask(sales, &FilterContext::default(), span).unwrap(),
        None
    );
    // Text filters match ignoring case, on dictionary codes.
    let f = Filter::values(&db.tables[sales], sales, region, &[Value::text("NORTH")]);
    assert!(matches!(
        &f,
        Filter::Column {
            allowed: storage::Allowed::Codes { .. },
            ..
        }
    ));
    let fc = FilterContext::default().with(f);
    assert_eq!(rows_of(ev.mask(sales, &fc, span).unwrap(), 5), [0, 1]);
    // BLANK allowed: the null row.
    let fc2 = FilterContext::default().with(Filter::values(
        &db.tables[sales],
        sales,
        region,
        &[Value::Blank, Value::text("west")],
    ));
    assert_eq!(rows_of(ev.mask(sales, &fc2, span).unwrap(), 5), [3, 4]);
    // Number filters: 2 matches 2.0 whatever the type. Two filters AND.
    let key = db.tables[sales].column("ProductKey").unwrap();
    let fc3 = fc.with(Filter::values(
        &db.tables[sales],
        sales,
        key,
        &[Value::Decimal(2.0)],
    ));
    assert_eq!(rows_of(ev.mask(sales, &fc3, span).unwrap(), 5), [1]);
    // A row-set filter.
    let mut rows = Bitset::new(5, false);
    rows.set(2);
    rows.set(4);
    let fc4 = FilterContext::default().with(Filter::Rows { table: sales, rows });
    assert_eq!(rows_of(ev.mask(sales, &fc4, span).unwrap(), 5), [2, 4]);
}

#[test]
fn filters_propagate_one_to_many_over_active_relationships() {
    let db = db();
    let ev = Evaluator::new(&db, options());
    let (sales, product) = (db.table("Sales").unwrap(), db.table("Product").unwrap());
    let colour = db.tables[product].column("Colour").unwrap();
    let span = Span::default();
    let red = FilterContext::default().with(Filter::values(
        &db.tables[product],
        product,
        colour,
        &[Value::text("red")],
    ));
    // Products 1 and 3 are red (either casing): Sales rows with those keys. Row 4 (key 9, no
    // Product) drops out once Product is filtered.
    assert_eq!(rows_of(ev.mask(product, &red, span).unwrap(), 3), [0, 2]);
    assert_eq!(rows_of(ev.mask(sales, &red, span).unwrap(), 5), [0, 2, 3]);
    // Filters never flow many → one.
    let key = db.tables[sales].column("ProductKey").unwrap();
    let one_sale = FilterContext::default().with(Filter::values(
        &db.tables[sales],
        sales,
        key,
        &[Value::Integer(2)],
    ));
    assert_eq!(ev.mask(product, &one_sale, span).unwrap(), None);
    // COUNTROWS sees the propagated filter.
    let q = metriccraft_dax::parse_expr("COUNTROWS(Sales)").unwrap();
    let mut ev = Evaluator::new(&db, options());
    assert!(matches!(ev.scalar(&q, &red), Ok(Value::Integer(3))));
    // An inactive Relationship doesn't propagate.
    let (mut model, data) = data();
    model.relationships[0].active = false;
    let inactive = Database::new(&model, &data);
    let ev = Evaluator::new(&inactive, options());
    assert_eq!(ev.mask(sales, &red, span).unwrap(), None);
}

#[test]
fn relationships_chain_and_cycles_terminate() {
    // Sales → Product → Category, plus a (bogus) Category → Sales back edge.
    let (mut model, mut data) = data();
    data.push(table(
        "Category",
        vec![
            ("Colour", text(&[Some("red"), Some("blue")])),
            ("Warm", ints(&[Some(1), Some(0)])),
        ],
    ));
    model
        .relationships
        .push(Relationship::new("Product", "Colour", "Category", "Colour"));
    model
        .relationships
        .push(Relationship::new("Category", "Warm", "Sales", "ProductKey"));
    let db = Database::new(&model, &data);
    let ev = Evaluator::new(&db, options());
    let (sales, category) = (db.table("Sales").unwrap(), db.table("Category").unwrap());
    let warm = db.tables[category].column("Warm").unwrap();
    let fc = FilterContext::default().with(Filter::values(
        &db.tables[category],
        category,
        warm,
        &[Value::Integer(1)],
    ));
    // Warm → red → products 1, 3 → Sales rows 0, 2, 3.
    assert_eq!(
        rows_of(ev.mask(sales, &fc, Span::default()).unwrap(), 5),
        [0, 2, 3]
    );
}

#[test]
fn row_context_reads_the_current_row() {
    let db = db();
    let mut ev = Evaluator::new(&db, options());
    let sales = db.table("Sales").unwrap();
    let e = metriccraft_dax::parse_expr("Sales[Amount] * 2 & [Region]").unwrap();
    let fc = FilterContext::default();
    let v = ev
        .with_row(
            RowFrame::Base {
                table: sales,
                row: 0,
            },
            |ev| ev.scalar(&e, &fc),
        )
        .unwrap();
    assert_eq!(v, Value::text("21North"));
    // A Column outside any row context has no single value.
    let err = ev.scalar(&e, &fc).unwrap_err();
    assert!(err.message.contains("row context"), "{err}");
    assert_eq!(err.span, metriccraft_dax::Span::new(0, 13));
}

#[test]
fn evaluate_row_table_and_constructor() {
    let db = db();
    let r = run(&db, "EVALUATE ROW(\"A\", 1 + 2, \"B\", \"x\" & BLANK())").unwrap();
    assert_eq!(r[0].columns, ["A", "B"]);
    assert_eq!(r[0].rows, [[Value::Integer(3), Value::text("x")]]);
    let t = run(&db, "EVALUATE 'sales'").unwrap();
    assert_eq!(t[0].columns, ["Region", "Amount", "ProductKey"]);
    assert_eq!(t[0].rows.len(), 5);
    assert_eq!(t[0].rows[3][0], Value::Blank);
    assert_eq!(t[0].rows[0][2], Value::Integer(1));
    assert_eq!(
        cells(&db, "EVALUATE {(1, \"a\"), (2, BLANK())}"),
        [["1", "a"], ["2", ""]]
    );
    assert_eq!(run(&db, "EVALUATE {1}").unwrap()[0].columns, ["Value"]);
    // Function names ignore case; COUNTROWS of nothing is BLANK.
    assert_eq!(one(&db, "countROWS(Product)"), "3");
    assert_eq!(one(&db, "True() && fAlSe"), "false");
    // A one-cell table is a scalar.
    assert_eq!(one(&db, "{5} + 1"), "6");
    let e = error(&db, "EVALUATE ROW(\"x\", {1, 2} + 1)");
    assert!(e.message.contains("several values"), "{e}");
    // EVALUATE needs a table.
    let e = error(&db, "EVALUATE 1");
    assert!(e.message.contains("table expression"), "{e}");
}

#[test]
fn order_by_sorts_rows() {
    let db = db();
    assert_eq!(
        cells(&db, "EVALUATE Sales ORDER BY Sales[Region] DESC, [Amount]")
            .iter()
            .map(|r| r[0].as_str())
            .collect::<Vec<_>>(),
        ["West", "South", "north", "North", ""]
    );
    assert_eq!(
        cells(&db, "EVALUATE {3, 1, 2} ORDER BY [Value]"),
        [["1"], ["2"], ["3"]]
    );
}

#[test]
fn measures_and_vars() {
    let db = db();
    let q = "DEFINE
        MEASURE Sales[Rows] = COUNTROWS(Sales)
        MEASURE Sales[Twice] = [Rows] * 2
        VAR k = 10
    EVALUATE ROW(\"a\", [Twice] + k, \"b\", VAR x = 2 VAR y = x * 3 RETURN y - x, \"c\", Sales[Rows])";
    assert_eq!(cells(&db, q), [["20", "4", "5"]]);
    // A Measure doesn't see the caller's VARs.
    let e = error(
        &db,
        "DEFINE MEASURE Sales[M] = v EVALUATE ROW(\"a\", VAR v = 1 RETURN [M])",
    );
    assert!(e.message.contains("variable"), "{e}");
    // Cycles and duplicate names are errors.
    let e = error(
        &db,
        "DEFINE MEASURE Sales[A] = [B] MEASURE Sales[B] = [A] + 1 EVALUATE ROW(\"x\", [A])",
    );
    assert!(e.message.contains("refers to itself"), "{e}");
    let e = error(
        &db,
        "DEFINE MEASURE Sales[A] = 1 MEASURE Sales[a] = 2 EVALUATE ROW(\"x\", [A])",
    );
    assert!(e.message.contains("twice"), "{e}");
    let e = error(
        &db,
        "DEFINE MEASURE Sales[Amount] = 1 EVALUATE ROW(\"x\", 1)",
    );
    assert!(e.message.contains("Column"), "{e}");
    // A Measure in a row context applies context transition (M3.4; calculate_tests.rs).
    assert_eq!(
        cells(&db, "DEFINE MEASURE Sales[M] = 1 EVALUATE {1} ORDER BY [M]"),
        [["1"]]
    );
}

#[test]
fn measure_chains_are_bounded() {
    let db = db();
    let mut q = String::from("DEFINE MEASURE Sales[m0] = 1\n");
    for i in 1..=MAX_MEASURE_CHAIN + 5 {
        q.push_str(&format!("MEASURE Sales[m{i}] = [m{}] + 1\n", i - 1));
    }
    let ok = format!("{q}EVALUATE ROW(\"x\", [m{}])", MAX_MEASURE_CHAIN - 1);
    assert_eq!(cells(&db, &ok), [[MAX_MEASURE_CHAIN.to_string()]]);
    let e = error(
        &db,
        &format!("{q}EVALUATE ROW(\"x\", [m{}])", MAX_MEASURE_CHAIN + 5),
    );
    assert_eq!(e.kind, ErrorKind::Limit, "{e}");
}

#[test]
fn deep_expressions_hit_the_depth_cap_not_the_stack() {
    let db = db();
    // Each Measure nests ~190 levels; chained, they pass MAX_EVAL_DEPTH long before the
    // Measure-chain cap. Runs on a test thread's default stack.
    let nest = |inner: &str| format!("{}{inner}", "- ".repeat(190));
    let mut q = String::from("DEFINE MEASURE Sales[m0] = 1\n");
    for i in 1..10 {
        q.push_str(&format!(
            "MEASURE Sales[m{i}] = {}\n",
            nest(&format!("[m{}]", i - 1))
        ));
    }
    let e = error(&db, &format!("{q}EVALUATE ROW(\"x\", [m9])"));
    assert_eq!(e.kind, ErrorKind::Limit, "{e}");
    assert!(e.message.contains("levels deep"), "{e}");
    // One Measure that deep is fine.
    assert_eq!(
        cells(&db, &format!("{q}EVALUATE ROW(\"x\", [m1])")),
        [["1"]]
    );
    // The same through binary operators and VARs.
    let mut q = String::from("DEFINE MEASURE Sales[m0] = 1\n");
    for i in 1..10 {
        let ops = "0 + VAR v = 1 RETURN ".repeat(60);
        q.push_str(&format!("MEASURE Sales[m{i}] = {ops}[m{}]\n", i - 1));
    }
    let e = error(&db, &format!("{q}EVALUATE ROW(\"x\", [m9])"));
    assert_eq!(e.kind, ErrorKind::Limit, "{e}");
}

#[test]
fn errors_carry_the_failing_span() {
    let db = db();
    let q = "EVALUATE ROW(\"x\", 1 + (\"abc\" * 2))";
    let e = error(&db, q);
    assert_eq!(&q[e.span.start..e.span.end], "(\"abc\" * 2)", "{e}");
    assert!(e.message.contains("'abc'"), "{e}");
    // Whole numbers wrap silently, as in Power BI (parity case 0037).
    assert_eq!(one(&db, "9223372036854775807 + 1"), "-9223372036854776000");
    let q = "EVALUATE ROW(\"x\", 1 = \"1\")";
    let e = error(&db, q);
    assert!(e.message.contains("compared"), "{e}");
    let q = "EVALUATE ROW(\"x\", NOSUCH(1))";
    let e = error(&db, q);
    assert_eq!(&q[e.span.start..e.span.end], "NOSUCH(1)");
    let e = error(&db, "EVALUATE Nope");
    assert!(e.message.contains("'Nope'"), "{e}");
    let e = error(&db, "EVALUATE ROW(\"x\" 1)");
    assert_eq!(e.kind, ErrorKind::Syntax);
    let e = error(&db, "EVALUATE ROW(\"x\", COUNTROWS(1))");
    assert!(e.message.contains("table is needed"), "{e}");
}

#[test]
fn operators_follow_dax_blank_rules() {
    let db = db();
    for (expr, want) in [
        ("BLANK() + 1", "1"),
        ("BLANK() * 2", ""),
        ("BLANK() = 0", "true"),
        ("BLANK() = \"\"", "true"),
        ("BLANK() == 0", "false"),
        ("BLANK() == BLANK()", "true"),
        ("BLANK() < 1", "true"),
        ("1 / 0", "Infinity"),
        ("-1 / 0", "-Infinity"),
        ("\"\" & (0 / 0)", "-nan(ind)"),
        ("\"North\" = \"NORTH\"", "true"),
        ("\"a\" <> \"A\"", "false"),
        ("2 ^ 10", "1024"),
        ("NOT BLANK()", "true"),
        ("5 - BLANK()", "5"),
        ("BLANK() - 5", "-5"),
    ] {
        assert_eq!(one(&db, expr), want, "{expr}");
    }
}

#[test]
fn cancel_stops_the_query() {
    let db = db();
    let flag = AtomicBool::new(true);
    let options = Options {
        cancel: Some(&flag),
        memory_cap: usize::MAX,
    };
    let e = run_query_text(&db, "EVALUATE Sales", options).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
    // A scan checks between chunks too.
    let flag = AtomicBool::new(false);
    let mut seen = 0;
    let r = storage::scan(
        3 * storage::CHUNK,
        None,
        storage::Cancel(Some(&flag)),
        |row| {
            seen += 1;
            if row == 10 {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(())
        },
    );
    assert_eq!(r.unwrap_err().kind, ErrorKind::Cancelled);
    assert_eq!(seen, storage::CHUNK, "stopped at the next chunk");
    flag.store(false, std::sync::atomic::Ordering::Relaxed);
    assert!(
        run_query_text(
            &db,
            "EVALUATE Sales",
            Options {
                cancel: Some(&flag),
                ..options
            }
        )
        .is_ok()
    );
}

#[test]
fn intermediate_tables_count_against_the_memory_cap() {
    let db = db();
    assert!(db.loaded_bytes > 0);
    let tight = |extra: usize| Options {
        cancel: None,
        memory_cap: db.loaded_bytes + extra,
    };
    let e = run_query_text(&db, "EVALUATE Sales", tight(64)).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory, "{e}");
    assert!(run_query_text(&db, "EVALUATE Sales", tight(1 << 20)).is_ok());
    let big: Vec<String> = (0..200).map(|i| i.to_string()).collect();
    let q = format!("EVALUATE {{{}}}", big.join(", "));
    assert_eq!(
        run_query_text(&db, &q, tight(1024)).unwrap_err().kind,
        ErrorKind::Memory
    );
    // A cap below the loaded data leaves nothing.
    let e = run_query_text(
        &db,
        "EVALUATE {1}",
        Options {
            cancel: None,
            memory_cap: 0,
        },
    )
    .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Memory);
}

#[test]
fn a_table_scalar_takes_its_one_value() {
    let db = db();
    let one_col = Database::new(
        &Model::default(),
        &[table("T", vec![("A", ints(&[Some(7)]))])],
    );
    assert_eq!(one(&one_col, "T + 1"), "8");
    let e = error(&db, "EVALUATE ROW(\"x\", Product + 1)");
    assert!(e.message.contains("2 columns"), "{e}");
    let t = TableVal::Rows(Default::default());
    let ev = Evaluator::new(&db, options());
    assert_eq!(ev.row_count(&t).unwrap(), 0);
}

/// 5,000 cases a run (`MCRAFT_FUZZ_SCALE` multiplies): mutated queries that parse are evaluated
/// against the tiny model; none may panic or hang.
#[test]
fn fuzzed_queries_never_panic() {
    let db = db();
    let seeds = [
        include_str!("../../../../parity/observed/0001-blank-aggregates.md"),
        include_str!("../../../../parity/observed/0002-division-by-zero.md"),
    ]
    .map(|case| {
        let start = case.find("```dax\n").map_or(0, |i| i + 7);
        let end = case[start..].find("```").map_or(case.len(), |i| start + i);
        case[start..end].to_string()
    });
    let mut seeds: Vec<String> = seeds.to_vec();
    seeds.extend(
        [
            "DEFINE MEASURE Sales[R] = COUNTROWS(Sales) * 2 VAR v = {(1, \"a\"), (2, BLANK())}\nEVALUATE v ORDER BY [Value1] DESC",
            "EVALUATE ROW(\"a\", VAR x = 9223372036854775807 RETURN x + -x * 2, \"b\", \"N\" & 1 / 0 = \"n\")",
            "EVALUATE Sales ORDER BY Sales[Region], [Amount] DESC",
            "DEFINE MEASURE Product[M] = [M] + 1 EVALUATE ROW(\"m\", [M], \"p\", COUNTROWS(Product) ^ 0.5)",
            "DEFINE MEASURE Sales[N] = COUNTROWS(Sales) EVALUATE ROW(\"a\", CALCULATE(COUNTROWS(Product), FILTER(Sales, [N] > 0 && RELATED(Product[Colour]) = \"red\")), \"b\", CALCULATE([N], ALLEXCEPT(Sales, Product[Colour]), Sales[Region] = \"north\" || Sales[Amount] > 2))",
            "EVALUATE FILTER(ALL(Sales[Region], Sales[ProductKey]), CALCULATE(COUNTROWS(Sales), ALL(Sales), FILTER(ALL(Product), Product[Key] = 1)) > 0)",
        ]
        .map(String::from),
    );
    let alphabet: Vec<char> = "'\"()[]{},=<>+-*/&|^.e09_ \nVAR RETURN NOT IN EVALUATE DEFINE MEASURE Sales[Amount] COUNTROWS BLANK ROW CALCULATE FILTER ALL ALLEXCEPT RELATED Product[Colour]"
        .chars()
        .collect();
    let mut rng = fuzz::Fuzz::new("dax-eval", 0xD1CE_5EED_0123_4567);
    let options = Options {
        cancel: None,
        memory_cap: db.loaded_bytes + (16 << 20),
    };
    for _ in 0..rng.scaled(5_000) {
        let mut chars: Vec<char> = seeds[rng.below(seeds.len())].chars().collect();
        for _ in 0..=rng.below(4) {
            let at = rng.below(chars.len() + 1);
            match rng.below(3) {
                0 if at < chars.len() => {
                    chars.remove(at);
                }
                1 => chars.insert(at, alphabet[rng.below(alphabet.len())]),
                _ => {
                    let end = (at + rng.below(12)).min(chars.len());
                    let piece: Vec<char> = chars[at.min(end)..end].to_vec();
                    let to = rng.below(chars.len() + 1);
                    for (k, c) in piece.into_iter().enumerate() {
                        chars.insert(to + k, c);
                    }
                }
            }
        }
        let src: String = chars.into_iter().collect();
        rng.case(src.as_bytes(), || {
            if let Ok(q) = metriccraft_dax::parse_query(&src) {
                let _ = run_query(&db, &q, options);
            }
        });
    }
    rng.finish();
}

/// Expression forms that nest: `@` is where the inner expression goes.
const NESTING_FORMS: &[&str] = &[
    "(@ + 1)",
    "(@ - 1)",
    "(@ * 1)",
    "(@ / 1)",
    "(@ ^ 1)",
    "(@ & 1)",
    "(@ && 1)",
    "(@ || 1)",
    "(@ = 1)",
    "(@ == 1)",
    "(@ <> 1)",
    "(@ < 1)",
    "(@ <= 1)",
    "(@ > 1)",
    "(@ >= 1)",
    "(1 + @)",
    "(@ IN {1})",
    "(1 IN {@})",
    "NOT (@ IN {1})",
    "- @",
    "+@",
    "NOT @",
    "VAR v = 1 RETURN @",
    "VAR v = @ RETURN v",
    "{@}",
    "{(1, @)}",
    "IF(TRUE(), @)",
    "IF(@, 1)",
    "SWITCH(@, 1, 2)",
    "SWITCH(1, @, 2)",
    "DIVIDE(@, 1)",
    "ISBLANK(@)",
    "ISERROR(@)",
    "IFERROR(@, 1)",
    "CONTAINSROW({1}, @)",
    "CALCULATE(@)",
    "CALCULATE(@, Sales[Region] = \"North\")",
    "CALCULATE(1, Sales[Region] = @)",
    "CALCULATE(1, Sales[Region] IN {@})",
    "COUNTROWS(CALCULATETABLE(ROW(\"a\", @)))",
    "COUNTROWS(ROW(\"a\", @))",
    "COUNTROWS(FILTER({1}, @))",
    "COUNTROWS(FILTER(Sales, @))",
];

/// Chained Measures, each nesting a form 30 times around the previous one (well past
/// `MAX_EVAL_DEPTH` in all), evaluated on a 1 MiB thread: the depth cap must come before a stack
/// overflow for every form, in debug and release builds.
#[test]
fn every_form_hits_the_depth_cap_on_a_1_mib_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(|| {
            let db = db();
            for form in NESTING_FORMS {
                let mut q = String::from("DEFINE MEASURE Sales[m0] = 1\n");
                for i in 1..21 {
                    let mut body = format!("[m{}]", i - 1);
                    for _ in 0..30 {
                        body = form.replace('@', &body);
                    }
                    q.push_str(&format!("MEASURE Sales[m{i}] = {body}\n"));
                }
                let e = error(&db, &format!("{q}EVALUATE ROW(\"x\", [m20])"));
                assert_eq!(e.kind, ErrorKind::Limit, "{form}: {e}");
                assert!(e.message.contains("nests"), "{form}: {e}");
                // Operators, VARs, constructors and the M3.5 functions stay small enough per
                // level to reach the depth cap itself; CALCULATE and the iterators (a deeper
                // path per level) may stop at the stack budget first in debug builds, as may every
                // form where debug frames are larger (macOS, Windows).
                if cfg!(target_os = "linux")
                    && !["CALCULATE", "FILTER", "ROW("]
                        .iter()
                        .any(|f| form.contains(f))
                {
                    assert!(e.message.contains("levels deep"), "{form}: {e}");
                }
            }
        })
        .unwrap();
    handle.join().unwrap();
}

#[test]
fn in_is_a_calculate_filter() {
    let db = db();
    // Case 0035 `InFilter`: text matches ignoring case ("North" finds "north" too).
    let sum = |filter: &str| one(&db, &format!("CALCULATE(SUM(Sales[Amount]), {filter})"));
    assert_eq!(sum("Sales[Region] IN {\"North\", \"West\"}"), "12.5");
    assert_eq!(sum("Sales[Region] IN {\"nowhere\"}"), "");
    // NOT keeps the other values, BLANK among them.
    assert_eq!(sum("NOT (Sales[Region] IN {\"North\", \"West\"})"), "5");
    // It replaces an outer filter on the column, and ANDs with another argument.
    assert_eq!(
        one(
            &db,
            "CALCULATE(CALCULATE(SUM(Sales[Amount]), Sales[Region] IN {\"North\", \"South\"}), \
             Sales[Region] = \"West\")"
        ),
        "14.5"
    );
    assert_eq!(
        sum("Sales[Region] IN {\"North\", \"South\"}, Sales[ProductKey] IN {1}"),
        "14.5"
    );
    // The table is evaluated once, in the caller's filter context: MAX sees only South.
    assert_eq!(
        one(
            &db,
            "CALCULATE(CALCULATE(COUNTROWS(Sales), Sales[ProductKey] IN {MAX(Sales[ProductKey])}), \
             Sales[Region] = \"South\")"
        ),
        "1"
    );
    // A table of model values, and strict matching: BLANK is only BLANK.
    assert_eq!(
        one(
            &db,
            "CALCULATE(COUNTROWS(Sales), Sales[ProductKey] IN FILTER(ALL(Product[Key]), \
             Product[Key] <> 2))"
        ),
        "3"
    );
    assert_eq!(sum("Sales[Region] IN {BLANK()}"), "1");
    // A table reading the column itself takes the general path; so does a tuple.
    assert_eq!(sum("Sales[Region] IN {Sales[Region]}"), "17.5");
    assert_eq!(
        one(
            &db,
            "CALCULATE(COUNTROWS(Sales), (Sales[Region], Sales[ProductKey]) IN {(\"north\", 1)})"
        ),
        "1"
    );
    // Inside FILTER, IN is evaluated per row.
    assert_eq!(
        one(
            &db,
            "COUNTROWS(FILTER(Sales, Sales[Region] IN {\"north\", \"West\"}))"
        ),
        "3"
    );
    let e = error(
        &db,
        "EVALUATE ROW(\"x\", CALCULATE(1, Sales[Region] IN {(1, 2)}))",
    );
    assert!(e.message.contains("IN compares"), "{e}");
}
