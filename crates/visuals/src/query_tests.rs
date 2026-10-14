//! M4.1 tests: the visual query over a small Sales Database built in code.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use opendash_core::Value;
use opendash_data::step::ColumnType;
use opendash_data::{Table, TableBuilder, fold_case};
use opendash_model::dax::{Database, ErrorKind, Filter, FilterContext, Options};
use opendash_model::{Column, Measure, Model, ModelTable, SummarizeBy};

use super::*;
use crate::visual::{Sort, SortDirection, Visual, VisualType, Well};

#[path = "../../m/src/fuzz.rs"]
mod fuzz;

fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
    let batch = RecordBatch::try_from_iter(columns).unwrap();
    let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    fold_case(b.finish().unwrap(), usize::MAX).unwrap()
}

fn text(v: &[&str]) -> ArrayRef {
    Arc::new(StringArray::from(v.to_vec()))
}

fn ints(v: &[i64]) -> ArrayRef {
    Arc::new(Int64Array::from(v.to_vec()))
}

fn floats(v: &[f64]) -> ArrayRef {
    Arc::new(Float64Array::from(v.to_vec()))
}

/// Sales: totals by Region North 35 / South 42, by Product Apple 37 / Pear 8 / Plum 32;
/// Customers North {A, B}, South {A, B, C}. Big: 4,000 Ids for the caps.
struct Fixture {
    db: Database,
    model: Model,
}

fn fixture() -> Fixture {
    let sales = table(
        "Sales",
        vec![
            (
                "Region",
                text(&[
                    "North", "North", "North", "South", "South", "South", "South",
                ]),
            ),
            (
                "Product",
                text(&["Apple", "Apple", "Pear", "Apple", "Pear", "Plum", "Plum"]),
            ),
            ("Customer", text(&["A", "B", "A", "A", "C", "B", "C"])),
            ("Year", ints(&[2023, 2024, 2023, 2024, 2023, 2024, 2024])),
            ("Quarter", text(&["Q1", "Q2", "Q2", "Q1", "Q1", "Q1", "Q2"])),
            ("Amount", floats(&[10.0, 20.0, 5.0, 7.0, 3.0, 30.0, 2.0])),
        ],
    );
    let ids: Vec<i64> = (0..4_000).collect();
    let vals: Vec<f64> = (0..4_000).map(|i| f64::from(i % 7)).collect();
    let big = table("Big", vec![("Id", ints(&ids)), ("V", floats(&vals))]);

    let col = |name: &str, ty: ColumnType| Column::new(name, ty);
    let mut year = col("Year", ColumnType::Whole);
    year.summarize_by = SummarizeBy::None;
    let mut amount = col("Amount", ColumnType::Decimal);
    amount.format_string = "#,0.00".into();
    let mut id = col("Id", ColumnType::Whole);
    id.summarize_by = SummarizeBy::None;
    let mut model = Model::default();
    model.put(ModelTable {
        name: "Sales".into(),
        columns: vec![
            col("Region", ColumnType::Text),
            col("Product", ColumnType::Text),
            col("Customer", ColumnType::Text),
            year,
            col("Quarter", ColumnType::Text),
            amount,
        ],
        measures: vec![
            Measure::new("Bad", "1 + SUM(Sales[Nope])"),
            Measure::new("Customers", "DISTINCTCOUNT(Sales[Customer])"),
            Measure::new("Double", "[Total] * 2"),
            Measure::new("Total", "SUM(Sales[Amount])"),
        ],
        ..ModelTable::default()
    });
    model.put(ModelTable {
        name: "Big".into(),
        columns: vec![id, col("V", ColumnType::Decimal)],
        ..ModelTable::default()
    });
    let db = Database::new(&model, &[sales, big]);
    Fixture { db, model }
}

fn options() -> Options<'static> {
    Options {
        cancel: None,
        memory_cap: usize::MAX,
    }
}

fn c(column: &str) -> FieldRef {
    let (table, column) = column.split_once('.').unwrap_or(("Sales", column));
    FieldRef::Column {
        table: table.into(),
        column: column.into(),
    }
}

fn m(name: &str) -> FieldRef {
    FieldRef::Measure { name: name.into() }
}

fn visual(kind: VisualType, wells: &[(Well, Vec<FieldRef>)]) -> Visual {
    let mut v = Visual::new("v1", kind);
    for (w, fs) in wells {
        v.wells.insert(*w, fs.clone());
    }
    v
}

fn query_at(f: &Fixture, v: &Visual, fc: &FilterContext, offset: usize) -> Result<QueryResult> {
    let input = QueryInput {
        db: &f.db,
        model: &f.model,
        filters: fc,
        options: options(),
    };
    run_query(v, &input, offset)
}

fn query(f: &Fixture, v: &Visual) -> QueryResult {
    query_at(f, v, &FilterContext::default(), 0).unwrap_or_else(|e| panic!("{e}"))
}

fn show(v: &Value) -> String {
    v.copy_text()
}

fn show_key(k: &Option<Value>) -> String {
    k.as_ref().map_or_else(|| "*".into(), show)
}

/// Each row as "key|key: cell, cell".
fn grid(r: &QueryResult) -> Vec<String> {
    r.rows.iter().chain(&r.total).map(line).collect()
}

fn line(row: &ResultRow) -> String {
    let keys: Vec<String> = row.keys.iter().map(show_key).collect();
    let cells: Vec<String> = row.cells.iter().map(show).collect();
    format!("{}: {}", keys.join("|"), cells.join(", "))
}

#[test]
fn implicit_measures() {
    let f = fixture();
    let card = query(
        &f,
        &visual(VisualType::Card, &[(Well::Values, vec![c("Amount")])]),
    );
    assert_eq!(grid(&card), [": 77"]);
    let h = &card.values[0];
    assert_eq!(h.label, "Sum of Amount");
    assert_eq!(h.aggregate, Some(SummarizeBy::Sum));
    assert_eq!(h.data_type, Some(ColumnType::Decimal));
    assert_eq!(h.format_string, "#,0.00");

    // A text Column summarized "none" counts outside a table.
    let card = query(
        &f,
        &visual(VisualType::Card, &[(Well::Values, vec![c("Product")])]),
    );
    assert_eq!(grid(&card), [": 7"]);
    assert_eq!(card.values[0].label, "Count of Product");
    assert_eq!(card.values[0].data_type, Some(ColumnType::Whole));

    // In a table the same Column groups; Amount sums; a Measure on a Measure works.
    let t = query(
        &f,
        &visual(
            VisualType::Table,
            &[(Well::Values, vec![c("Region"), c("Amount"), m("Double")])],
        ),
    );
    assert_eq!(t.row_groups.len(), 1);
    assert_eq!(
        t.display,
        [
            DisplayColumn::Group(0),
            DisplayColumn::Value(0),
            DisplayColumn::Value(1)
        ]
    );
    assert_eq!(grid(&t), ["North: 35, 70", "South: 42, 84", "*: 77, 154"]);
}

#[test]
fn table_total_of_a_non_additive_measure() {
    let f = fixture();
    let t = query(
        &f,
        &visual(
            VisualType::Table,
            &[(Well::Values, vec![c("Region"), m("Customers")])],
        ),
    );
    // 2 + 3 rows, but three customers in all.
    assert_eq!(grid(&t), ["North: 2", "South: 3", "*: 3"]);
    assert!(t.total.as_ref().is_some_and(ResultRow::is_total));
    assert!(!t.truncated);

    let mut v = visual(
        VisualType::Table,
        &[(Well::Values, vec![c("Region"), m("Customers")])],
    );
    v.format.row_subtotals = false;
    assert_eq!(query(&f, &v).total, None);
}

#[test]
fn matrix_subtotals_on_rows_and_columns() {
    let f = fixture();
    let mut v = visual(
        VisualType::Matrix,
        &[
            (Well::Rows, vec![c("Region"), c("Product")]),
            (Well::Columns, vec![c("Year"), c("Quarter")]),
            (Well::Values, vec![m("Customers")]),
        ],
    );
    let r = query(&f, &v);
    let cols: Vec<String> = r
        .column_keys
        .iter()
        .map(|k| k.iter().map(show_key).collect::<Vec<_>>().join("|"))
        .collect();
    assert_eq!(
        cols,
        [
            "2023|Q1", "2023|Q2", "2023|*", "2024|Q1", "2024|Q2", "2024|*", "*|*"
        ]
    );
    assert_eq!(
        grid(&r),
        [
            "North|*: 1, 1, 1, , 1, 1, 2",
            "North|Apple: 1, , 1, , 1, 1, 2",
            "North|Pear: , 1, 1, , , , 1",
            "South|*: 1, , 1, 2, 1, 3, 3",
            "South|Apple: , , , 1, , 1, 1",
            "South|Pear: 1, , 1, , , , 1",
            "South|Plum: , , , 1, 1, 2, 2",
            "*|*: 2, 1, 2, 2, 2, 3, 3",
        ]
    );
    assert_eq!(r.rows[0].level(), 1);
    assert_eq!(r.rows[1].level(), 2);
    assert_eq!(r.total_rows, 7);

    v.format.row_subtotals = false;
    v.format.column_subtotals = false;
    let r = query(&f, &v);
    assert_eq!(r.column_keys.len(), 4);
    assert_eq!(r.total, None);
    assert!(r.rows.iter().all(|row| !row.is_total()));
    assert_eq!(r.rows.len(), 5);
}

#[test]
fn caps_windows_and_truncation() {
    let f = fixture();
    let table = visual(
        VisualType::Table,
        &[(Well::Values, vec![c("Big.Id"), c("Big.V")])],
    );
    let r = query_at(&f, &table, &FilterContext::default(), 0).unwrap();
    assert_eq!(r.rows.len(), TABLE_WINDOW);
    assert_eq!(r.total_rows, 4_000);
    assert!(r.truncated);
    assert_eq!(show_key(&r.rows[0].keys[0]), "0");
    let r = query_at(&f, &table, &FilterContext::default(), 3_800).unwrap();
    assert_eq!(r.rows.len(), 200);
    assert_eq!(r.offset, 3_800);
    assert_eq!(show_key(&r.rows[0].keys[0]), "3800");
    assert!(r.total.is_some());
    let r = query_at(&f, &table, &FilterContext::default(), usize::MAX).unwrap();
    assert!(r.rows.is_empty());

    let bar = visual(
        VisualType::ClusteredBar,
        &[
            (Well::Axis, vec![c("Big.Id")]),
            (Well::Values, vec![c("Big.V")]),
        ],
    );
    let r = query(&f, &bar);
    assert_eq!(r.rows.len(), CHART_CATEGORY_CAP);
    assert!(r.truncated);

    let line = visual(
        VisualType::Line,
        &[
            (Well::Axis, vec![c("Big.Id")]),
            (Well::Values, vec![c("Big.V")]),
        ],
    );
    let r = query(&f, &line);
    assert_eq!(r.rows.len(), LINE_POINT_CAP);
    assert!(r.truncated);

    let slicer = visual(VisualType::Slicer, &[(Well::Field, vec![c("Big.Id")])]);
    let r = query(&f, &slicer);
    assert_eq!(r.rows.len(), SLICER_CAP);
    assert!(r.truncated);
    assert!(r.rows[0].cells.is_empty());

    // Under the caps nothing is truncated.
    let small = visual(
        VisualType::Line,
        &[
            (Well::Axis, vec![c("Product")]),
            (Well::Values, vec![m("Total")]),
        ],
    );
    assert!(!query(&f, &small).truncated);
}

fn first_keys(r: &QueryResult) -> Vec<String> {
    r.rows.iter().map(|row| show_key(&row.keys[0])).collect()
}

#[test]
fn default_and_explicit_sorts() {
    let f = fixture();
    let by_product = |kind| {
        visual(
            kind,
            &[
                (Well::Axis, vec![c("Product")]),
                (Well::Values, vec![m("Total")]),
            ],
        )
    };
    // Bar/column: first measure descending.
    for kind in [VisualType::ClusteredBar, VisualType::ClusteredColumn] {
        assert_eq!(
            first_keys(&query(&f, &by_product(kind))),
            ["Apple", "Plum", "Pear"]
        );
    }
    // Line: axis ascending.
    assert_eq!(
        first_keys(&query(&f, &by_product(VisualType::Line))),
        ["Apple", "Pear", "Plum"]
    );
    // Table: first column ascending, whether a Column or a Measure.
    let t = visual(
        VisualType::Table,
        &[(Well::Values, vec![c("Product"), m("Total")])],
    );
    assert_eq!(first_keys(&query(&f, &t)), ["Apple", "Pear", "Plum"]);
    let t2 = visual(
        VisualType::Table,
        &[(Well::Values, vec![m("Total"), c("Product")])],
    );
    assert_eq!(first_keys(&query(&f, &t2)), ["Pear", "Plum", "Apple"]);
    // Slicer: ascending; explicit descending.
    let mut s = visual(VisualType::Slicer, &[(Well::Field, vec![c("Customer")])]);
    assert_eq!(first_keys(&query(&f, &s)), ["A", "B", "C"]);
    s.sort = Some(Sort {
        field: c("customer"),
        direction: SortDirection::Descending,
    });
    assert_eq!(first_keys(&query(&f, &s)), ["C", "B", "A"]);

    // Explicit: a table by a Measure descending, a bar chart by its axis.
    let mut t = t;
    t.sort = Some(Sort {
        field: m("Total"),
        direction: SortDirection::Descending,
    });
    assert_eq!(first_keys(&query(&f, &t)), ["Apple", "Plum", "Pear"]);
    let mut bar = by_product(VisualType::ClusteredBar);
    bar.sort = Some(Sort {
        field: c("Product"),
        direction: SortDirection::Ascending,
    });
    assert_eq!(first_keys(&query(&f, &bar)), ["Apple", "Pear", "Plum"]);

    // A matrix sorted by a Measure orders each level by its own subtotals.
    let mut mx = visual(
        VisualType::Matrix,
        &[
            (Well::Rows, vec![c("Region"), c("Product")]),
            (Well::Values, vec![m("Total")]),
        ],
    );
    let r = query(&f, &mx);
    assert_eq!(
        grid(&r),
        [
            "North|*: 35",
            "North|Apple: 30",
            "North|Pear: 5",
            "South|*: 42",
            "South|Apple: 7",
            "South|Pear: 3",
            "South|Plum: 32",
            "*|*: 77"
        ]
    );
    mx.sort = Some(Sort {
        field: m("Total"),
        direction: SortDirection::Descending,
    });
    assert_eq!(
        grid(&query(&f, &mx)),
        [
            "South|*: 42",
            "South|Plum: 32",
            "South|Apple: 7",
            "South|Pear: 3",
            "North|*: 35",
            "North|Apple: 30",
            "North|Pear: 5",
            "*|*: 77"
        ]
    );
    // By the inner Rows Field descending: the outer level stays ascending.
    mx.sort = Some(Sort {
        field: c("Product"),
        direction: SortDirection::Descending,
    });
    assert_eq!(
        first_keys(&query(&f, &mx))
            .into_iter()
            .zip(query(&f, &mx).rows.iter().map(|r| show_key(&r.keys[1])))
            .map(|(a, b)| format!("{a}|{b}"))
            .collect::<Vec<_>>(),
        [
            "North|*",
            "North|Pear",
            "North|Apple",
            "South|*",
            "South|Plum",
            "South|Pear",
            "South|Apple"
        ]
    );
}

#[test]
fn legend_or_several_values() {
    let f = fixture();
    // Legend and two Values: only the first Value, one series per Legend item.
    let v = visual(
        VisualType::ClusteredColumn,
        &[
            (Well::Axis, vec![c("Region")]),
            (Well::Legend, vec![c("Product")]),
            (Well::Values, vec![m("Total"), m("Customers")]),
        ],
    );
    let r = query(&f, &v);
    assert_eq!(r.values.len(), 1);
    assert_eq!(r.values[0].label, "Total");
    let series: Vec<String> = r.column_keys.iter().map(|k| show_key(&k[0])).collect();
    assert_eq!(series, ["Apple", "Pear", "Plum"]);
    // Sorted by the category's total across the Legend: South 42 before North 35.
    assert_eq!(grid(&r), ["South: 7, 3, 32", "North: 30, 5, "]);
    assert_eq!(r.cell(&r.rows[0], 2, 0), Some(&Value::Decimal(32.0)));
    assert_eq!(r.total, None);

    // Two Values, no Legend: two series.
    let v = visual(
        VisualType::ClusteredColumn,
        &[
            (Well::Axis, vec![c("Region")]),
            (Well::Values, vec![m("Total"), m("Customers")]),
        ],
    );
    let r = query(&f, &v);
    assert_eq!(r.column_keys, [Vec::<Option<Value>>::new()]);
    assert_eq!(r.values.len(), 2);
    assert_eq!(grid(&r), ["South: 42, 3", "North: 35, 2"]);
}

#[test]
fn a_failing_measure_fails_the_visual() {
    let f = fixture();
    let v = visual(
        VisualType::Table,
        &[(Well::Values, vec![c("Region"), m("Total"), m("Bad")])],
    );
    let e = query_at(&f, &v, &FilterContext::default(), 0).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Eval);
    // The span points into the Measure's own text.
    assert!(e.span.start >= 4, "{e:?}");
    assert!(e.message.contains("Nope"), "{e}");

    let unknown = visual(VisualType::Card, &[(Well::Values, vec![m("Missing")])]);
    assert!(query_at(&f, &unknown, &FilterContext::default(), 0).is_err());
    let measure_on_axis = visual(VisualType::Line, &[(Well::Axis, vec![m("Total")])]);
    assert!(query_at(&f, &measure_on_axis, &FilterContext::default(), 0).is_err());

    let cancel = AtomicBool::new(true);
    let input = QueryInput {
        db: &f.db,
        model: &f.model,
        filters: &FilterContext::default(),
        options: Options {
            cancel: Some(&cancel),
            memory_cap: usize::MAX,
        },
    };
    let e = run_query(&v, &input, 0).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Cancelled);
}

#[test]
fn selection_filters_the_query() {
    let f = fixture();
    let t = f.db.table("Sales").unwrap();
    let region = f.db.tables[t].column("Region").unwrap();
    let fc = FilterContext::default().with(Filter::values(
        &f.db.tables[t],
        t,
        region,
        &[Value::text("South")],
    ));
    let v = visual(VisualType::Card, &[(Well::Values, vec![m("Total")])]);
    let r = query_at(&f, &v, &fc, 0).unwrap();
    assert_eq!(grid(&r), [": 42"]);
}

#[test]
fn cache_hits_and_generation_invalidates() {
    let f = fixture();
    let fc = FilterContext::default();
    let input = QueryInput {
        db: &f.db,
        model: &f.model,
        filters: &fc,
        options: options(),
    };
    let mut v = visual(
        VisualType::Table,
        &[(Well::Values, vec![c("Region"), m("Total")])],
    );
    let mut cache: QueryCache<&str> = QueryCache::new();
    let a = cache.get_or_run(1, &v, "none", &input, 0).unwrap();
    let b = cache.get_or_run(1, &v, "none", &input, 0).unwrap();
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(cache.runs(), 1);

    // Moving or retitling the Visual is no new query; another window or Selection is.
    v.x = 99;
    v.format.title = Some("Sales".into());
    cache.get_or_run(1, &v, "none", &input, 0).unwrap();
    assert_eq!(cache.runs(), 1);
    cache.get_or_run(1, &v, "none", &input, 500).unwrap();
    cache.get_or_run(1, &v, "South", &input, 0).unwrap();
    assert_eq!(cache.runs(), 3);
    assert_eq!(cache.len(), 3);

    // A new generation (data load, Model edit) empties it.
    let c = cache.get_or_run(2, &v, "none", &input, 0).unwrap();
    assert!(!Arc::ptr_eq(&a, &c));
    assert_eq!(cache.runs(), 4);
    assert_eq!(cache.len(), 1);

    // Errors aren't cached.
    let bad = visual(VisualType::Card, &[(Well::Values, vec![m("Bad")])]);
    assert!(cache.get_or_run(2, &bad, "none", &input, 0).is_err());
    assert!(cache.get_or_run(2, &bad, "none", &input, 0).is_err());
    assert_eq!(cache.runs(), 6);
}

#[test]
fn fuzzed_wells_never_panic() {
    let f = fixture();
    let pool = [
        c("Region"),
        c("Product"),
        c("Customer"),
        c("Year"),
        c("Quarter"),
        c("Amount"),
        // Not Big.Id: its 4,000 groups cross-joined with Sales make slow (debug) but valid cases.
        c("Big.V"),
        c("Nope"),
        c("Nope.Nope"),
        m("Total"),
        m("Customers"),
        m("Double"),
        m("Bad"),
        m("Missing"),
    ];
    let mut rng = fuzz::Fuzz::new("visual-query", 0x0D45_4A11_2026_1014);
    for _ in 0..rng.scaled(300) {
        let kind = VisualType::ALL[rng.below(VisualType::ALL.len())];
        let mut v = Visual::new("f", kind);
        for w in Well::ALL {
            let n = rng.below(4);
            let fields: Vec<FieldRef> = (0..n)
                .map(|_| pool[rng.below(pool.len())].clone())
                .collect();
            v.wells.insert(w, fields);
        }
        if rng.below(2) == 0 {
            v.sort = Some(Sort {
                field: pool[rng.below(pool.len())].clone(),
                direction: if rng.below(2) == 0 {
                    SortDirection::Ascending
                } else {
                    SortDirection::Descending
                },
            });
        }
        v.format.row_subtotals = rng.below(2) == 0;
        v.format.column_subtotals = rng.below(2) == 0;
        let offset = [0, 1, 499, 3_999, usize::MAX][rng.below(5)];
        let input = format!("{v:?} offset={offset}");
        rng.case(input.as_bytes(), || {
            if let Ok(r) = query_at(&f, &v, &FilterContext::default(), offset) {
                assert!(r.rows.len() <= TABLE_WINDOW.max(LINE_POINT_CAP));
                for row in r.rows.iter().chain(&r.total) {
                    assert_eq!(row.keys.len(), r.row_groups.len());
                    assert_eq!(row.cells.len(), r.column_keys.len() * r.values.len());
                }
            }
        });
    }
    rng.finish();
}
