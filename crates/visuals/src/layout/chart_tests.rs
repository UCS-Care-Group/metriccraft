//! M4.4 tests: charts laid out from query results built in code (and one from a real query).

use std::path::Path;
use std::sync::Arc;

use arrow_array::{ArrayRef, Float64Array, RecordBatch, StringArray};
use opendash_core::Value;
use opendash_data::step::ColumnType;
use opendash_data::{TableBuilder, fold_case};
use opendash_model::dax::{Database, FilterContext, Options};
use opendash_model::{Column, Model, ModelTable};

use super::*;
use crate::query::{QueryInput, ResultRow, run_query};
use crate::theme::Theme;
use crate::visual::{FieldRef, VisualType, Well};

fn check(name: &str, scene: &Scene) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots");
    crate::snapshot::check(&dir, name, scene).unwrap();
    sane(scene);
}

/// Every coordinate finite, and (in a Visual big enough for its title) every mark inside it.
fn sane(scene: &Scene) {
    let bounded = scene.width >= 100.0 && scene.height >= 100.0;
    let inside = |x: f32, y: f32| {
        x.is_finite() && y.is_finite() && !bounded
            || x >= -0.5 && y >= -0.5 && x <= scene.width + 0.5 && y <= scene.height + 0.5
    };
    for c in &scene.cmds {
        let ok = match c {
            Cmd::Rect { x, y, w, h, .. } => {
                inside(*x, *y) && inside(x + w, y + h) || (*w <= 8.0 && *h <= 8.0)
            }
            Cmd::Line { x1, y1, x2, y2, .. } => inside(*x1, *y1) && inside(*x2, *y2),
            Cmd::Path { points, .. } => points.iter().all(|(x, y)| inside(*x, *y)),
            Cmd::Text { x, y, .. } => inside(*x, *y),
            _ => true,
        };
        assert!(ok, "out of bounds: {c:?}");
    }
}

fn cx_with<R>(f: impl FnOnce(&Context<'_>) -> R) -> R {
    let theme = Theme::light();
    f(&Context {
        theme: &theme,
        locale: &opendash_core::EN_GB,
    })
}

fn header(label: &str, ty: Option<ColumnType>) -> Header {
    Header {
        field: FieldRef::Measure { name: label.into() },
        label: label.into(),
        format_string: String::new(),
        data_type: ty,
        aggregate: None,
    }
}

fn t(s: &str) -> Value {
    Value::text(s)
}

fn d(v: f64) -> Value {
    Value::Decimal(v)
}

/// A chart result: categories `keys` on an Axis of type `ty`, the Legend's keys (if any),
/// Values named `values`, and per category its cells (Legend tuple-major).
fn result(
    kind: VisualType,
    ty: ColumnType,
    keys: Vec<Value>,
    legend: Option<Vec<Value>>,
    values: &[&str],
    cells: Vec<Vec<Value>>,
) -> QueryResult {
    let (column_groups, column_keys) = match legend {
        Some(ks) => (
            vec![header("Product", Some(ColumnType::Text))],
            ks.into_iter().map(|k| vec![Some(k)]).collect(),
        ),
        None => (Vec::new(), vec![Vec::new()]),
    };
    let rows: Vec<ResultRow> = keys
        .into_iter()
        .zip(cells)
        .map(|(k, cells)| ResultRow {
            keys: vec![Some(k)],
            cells,
        })
        .collect();
    QueryResult {
        kind,
        row_groups: vec![header("Region", Some(ty))],
        column_groups,
        values: values.iter().map(|v| header(v, None)).collect(),
        column_keys,
        total_rows: rows.len(),
        rows,
        total: None,
        display: Vec::new(),
        offset: 0,
        truncated: false,
    }
}

fn visual(kind: VisualType, w: i32, h: i32) -> Visual {
    let mut v = Visual::new("v1", kind);
    v.w = w;
    v.h = h;
    v
}

fn regions(n: usize) -> Vec<Value> {
    (0..n).map(|i| t(&format!("Region {i}"))).collect()
}

fn one(vals: &[f64]) -> Vec<Vec<Value>> {
    vals.iter().map(|v| vec![d(*v)]).collect()
}

/// Bars (tagged filled rects) in order: (tag, x, y, w, h).
fn bars(s: &Scene) -> Vec<(Tag, f32, f32, f32, f32)> {
    s.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Rect {
                x,
                y,
                w,
                h,
                fill: Some(_),
                tag: Some(tag),
                ..
            } => Some((*tag, *x, *y, *w, *h)),
            _ => None,
        })
        .collect()
}

fn texts(s: &Scene) -> Vec<&str> {
    s.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Column.

#[test]
fn column_from_a_real_query_snapshot() {
    let batch = RecordBatch::try_from_iter(vec![
        (
            "Region",
            Arc::new(StringArray::from(vec![
                "North", "South", "East", "North", "West",
            ])) as ArrayRef,
        ),
        (
            "Amount",
            Arc::new(Float64Array::from(vec![
                1200.0, 4200.0, 2500.0, 2300.0, 900.0,
            ])) as ArrayRef,
        ),
    ])
    .unwrap();
    let mut b = TableBuilder::new("Sales", batch.schema(), usize::MAX);
    b.push(&batch).unwrap();
    let table = fold_case(b.finish().unwrap(), usize::MAX).unwrap();
    let mut model = Model::default();
    model.put(ModelTable {
        name: "Sales".into(),
        columns: vec![
            Column::new("Region", ColumnType::Text),
            Column::new("Amount", ColumnType::Decimal),
        ],
        ..ModelTable::default()
    });
    let db = Database::new(&model, &[table]);
    let col = |c: &str| FieldRef::Column {
        table: "Sales".into(),
        column: c.into(),
    };
    let mut v = visual(VisualType::ClusteredColumn, 400, 300);
    v.wells.insert(Well::Axis, vec![col("Region")]);
    v.wells.insert(Well::Values, vec![col("Amount")]);
    let input = QueryInput {
        db: &db,
        model: &model,
        filters: &FilterContext::default(),
        options: Options {
            cancel: None,
            memory_cap: usize::MAX,
        },
    };
    let r = run_query(&v, &input, 0).unwrap();
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_query", &s);
    // Sorted by Amount descending: South 4,200, North 3,500, East 2,500, West 900.
    let b = bars(&s);
    assert_eq!(b.len(), 4);
    let heights: Vec<f32> = b.iter().map(|b| b.4).collect();
    assert!(heights.windows(2).all(|w| w[0] > w[1]), "{heights:?}");
    let tags: Vec<u32> = b.iter().map(|b| b.0.point).collect();
    assert_eq!(tags, [0, 1, 2, 3]);
    let tx = texts(&s);
    for l in ["Sum of Amount by Region", "South", "West", "0K", "4K"] {
        assert!(tx.contains(&l), "{l} not in {tx:?}");
    }
}

#[test]
fn column_mixed_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(5),
        None,
        &["Profit"],
        one(&[30.0, -12.5, 18.0, -40.0, 5.0]),
    );
    let v = visual(VisualType::ClusteredColumn, 400, 300);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_mixed", &s);
    // Negative bars hang below the zero line: same top as the positive bars' bottom.
    let b = bars(&s);
    let zero = b.first().map(|b| b.2 + b.4).unwrap();
    let neg = b.get(1).unwrap();
    assert!((neg.2 - zero).abs() < 0.01, "{neg:?} vs {zero}");
}

#[test]
fn column_all_negative_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(3),
        None,
        &["Loss"],
        one(&[-1500.0, -3200.0, -800.0]),
    );
    let v = visual(VisualType::ClusteredColumn, 300, 220);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_negative", &s);
    assert!(texts(&s).contains(&"0K") && texts(&s).contains(&"-3K"));
}

#[test]
fn column_legend_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(3),
        Some(vec![t("Apple"), t("Pear"), Value::Blank]),
        &["Total"],
        vec![
            vec![d(10.0), d(5.0), d(2.0)],
            vec![d(7.0), Value::Blank, d(3.0)],
            vec![d(1.0), d(12.0), d(6.0)],
        ],
    );
    let v = visual(VisualType::ClusteredColumn, 400, 300);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_legend", &s);
    let tx = texts(&s);
    for l in ["Product", "Apple", "Pear", "(Blank)"] {
        assert!(tx.contains(&l), "{l} not in {tx:?}");
    }
    // 8 bars (one BLANK cell), series × category tags.
    let tags: Vec<(u32, u32)> = bars(&s)
        .iter()
        .map(|b| (b.0.series, b.0.point))
        .filter(|t| t.0 < 100)
        .collect();
    assert_eq!(tags.len(), 8, "{tags:?}");
    assert!(tags.contains(&(2, 1)) && !tags.contains(&(1, 1)));
}

#[test]
fn column_several_values_legend_right_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(4),
        None,
        &["Sales", "Cost of sales and other things"],
        vec![
            vec![d(120.0), d(80.0)],
            vec![d(90.0), d(95.0)],
            vec![d(60.0), d(20.0)],
            vec![d(150.0), d(100.0)],
        ],
    );
    let mut v = visual(VisualType::ClusteredColumn, 480, 260);
    v.format.legend_position = LegendPosition::Right;
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_values_right", &s);
    let b = bars(&s);
    assert_eq!(b.len(), 8);
    // Side by side within a category.
    let (a, c) = (b.first().unwrap(), b.get(4).unwrap());
    assert_eq!((a.0.point, c.0.point, c.0.series), (0, 0, 1));
    assert!((a.1 + a.3 - c.1).abs() < 0.01);
}

#[test]
fn column_data_labels_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(4),
        None,
        &["Revenue"],
        one(&[1_234_567.0, 845_000.0, -230_000.0, 0.0]),
    );
    let mut v = visual(VisualType::ClusteredColumn, 400, 260);
    v.format.data_labels = true;
    v.format.title = Some("Revenue by region".into());
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_labels", &s);
    let tx = texts(&s);
    for l in ["1.23M", "845K", "-230K", "1.5M", "0.0M"] {
        assert!(tx.contains(&l), "{l} not in {tx:?}");
    }
}

#[test]
fn column_blank_category_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        vec![t("North"), Value::Blank, t("South")],
        None,
        &["Total"],
        vec![vec![d(4.0)], vec![d(2.0)], vec![Value::Blank]],
    );
    let v = visual(VisualType::ClusteredColumn, 300, 200);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_blank", &s);
    assert!(texts(&s).contains(&"(Blank)"));
    assert_eq!(bars(&s).len(), 2);
}

#[test]
fn column_many_categories_snapshot() {
    let n = 120;
    let vals: Vec<f64> = (0..n).map(|i| f64::from(n - i) * 3.0).collect();
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(n as usize),
        None,
        &["Total"],
        one(&vals),
    );
    let v = visual(VisualType::ClusteredColumn, 500, 260);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_many", &s);
    let labels: Vec<&str> = texts(&s)
        .into_iter()
        .filter(|l| l.starts_with("Reg"))
        .collect();
    assert!(labels.len() > 3 && labels.len() < 20, "{labels:?}");
}

#[test]
fn column_truncated_snapshot() {
    let mut r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(30),
        None,
        &["Total"],
        one(&(0..30).map(|i| 100.0 - f64::from(i)).collect::<Vec<_>>()),
    );
    r.truncated = true;
    r.total_rows = 1_500;
    let v = visual(VisualType::ClusteredColumn, 400, 260);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_column_truncated", &s);
    assert!(texts(&s).contains(&"Showing the first 30 categories"));
    // Series cap instead.
    r.total_rows = 30;
    let s = cx_with(|cx| column(&v, &r, cx));
    assert!(texts(&s).contains(&"Showing the first 1 series"));
}

// ---------------------------------------------------------------------------------------------
// Bar.

#[test]
fn bar_snapshot() {
    let r = result(
        VisualType::ClusteredBar,
        ColumnType::Text,
        vec![
            t("North"),
            t("South"),
            t("A region with a very long name indeed"),
            Value::Blank,
        ],
        None,
        &["Total"],
        one(&[42_000.0, 35_500.0, 12_000.0, 3_000.0]),
    );
    let v = visual(VisualType::ClusteredBar, 400, 260);
    let s = cx_with(|cx| bar(&v, &r, cx));
    check("chart_bar", &s);
    let b = bars(&s);
    assert_eq!(b.len(), 4);
    // First category on top, bars from the same x.
    assert!(
        b.windows(2)
            .all(|w| w[0].2 < w[1].2 && (w[0].1 - w[1].1).abs() < 0.01)
    );
    assert!(texts(&s).iter().any(|l| l.ends_with('…')));
}

#[test]
fn bar_mixed_labels_legend_bottom_snapshot() {
    let r = result(
        VisualType::ClusteredBar,
        ColumnType::Text,
        regions(3),
        Some(vec![t("Apple"), t("Pear")]),
        &["Margin"],
        vec![
            vec![d(25.0), d(-10.0)],
            vec![d(-35.0), d(15.0)],
            vec![d(5.0), d(30.0)],
        ],
    );
    let mut v = visual(VisualType::ClusteredBar, 420, 300);
    v.format.data_labels = true;
    v.format.legend_position = LegendPosition::Bottom;
    let s = cx_with(|cx| bar(&v, &r, cx));
    check("chart_bar_mixed", &s);
    let tx = texts(&s);
    for l in ["-35", "30", "Apple", "Pear"] {
        assert!(tx.contains(&l), "{l} not in {tx:?}");
    }
}

#[test]
fn bar_many_categories_thinned() {
    let n = 400;
    let r = result(
        VisualType::ClusteredBar,
        ColumnType::Text,
        regions(n),
        None,
        &["Total"],
        one(&vec![1.0; n]),
    );
    let v = visual(VisualType::ClusteredBar, 300, 300);
    let s = cx_with(|cx| bar(&v, &r, cx));
    sane(&s);
    let labels = texts(&s).iter().filter(|l| l.starts_with("Reg")).count();
    assert!(labels > 5 && labels < 30, "{labels}");
    assert_eq!(bars(&s).len(), n);
}

// ---------------------------------------------------------------------------------------------
// Line.

/// Hit rectangles (invisible, tagged) in order.
fn hits(s: &Scene) -> Vec<Tag> {
    s.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Rect {
                fill: None,
                stroke: None,
                tag: Some(tag),
                ..
            } => Some(*tag),
            _ => None,
        })
        .collect()
}

fn paths(s: &Scene) -> Vec<(Tag, usize)> {
    s.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Path {
                points,
                tag: Some(tag),
                ..
            } => Some((*tag, points.len())),
            _ => None,
        })
        .collect()
}

#[test]
fn line_gaps_at_blank_snapshot() {
    let r = result(
        VisualType::Line,
        ColumnType::Text,
        ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug"]
            .into_iter()
            .map(t)
            .collect(),
        None,
        &["Visits"],
        vec![
            vec![d(12.0)],
            vec![d(18.0)],
            vec![d(9.0)],
            vec![Value::Blank],
            vec![d(14.0)],
            vec![Value::Blank],
            vec![d(20.0)],
            vec![d(22.0)],
        ],
    );
    let mut v = visual(VisualType::Line, 400, 240);
    v.format.data_labels = true;
    let s = cx_with(|cx| line(&v, &r, cx));
    check("chart_line_gaps", &s);
    // Jan–Mar, May alone, Jul–Aug.
    let p: Vec<(u32, usize)> = paths(&s).iter().map(|(t, n)| (t.point, *n)).collect();
    assert_eq!(p, [(0, 3), (6, 2)]);
    let lone = bars(&s);
    assert_eq!(lone.len(), 1);
    assert_eq!(lone.first().map(|b| b.0.point), Some(4));
    let h: Vec<u32> = hits(&s).iter().map(|t| t.point).collect();
    assert_eq!(h, [0, 1, 2, 4, 6, 7]);
}

#[test]
fn line_continuous_dates_snapshot() {
    // Weekly points through 2024 with a missing stretch: positioned by date, nice month ticks.
    let start = 45_292.0; // 2024-01-01
    let mut keys = Vec::new();
    let mut cells = Vec::new();
    for w in 0..52 {
        if (20..30).contains(&w) {
            continue;
        }
        let day = start + f64::from(w) * 7.0;
        keys.push(Value::DateTime(day));
        let x = f64::from(w);
        cells.push(vec![
            d(1000.0 + 400.0 * (x / 5.0).sin()),
            d(600.0 + 10.0 * x),
        ]);
    }
    let r = result(
        VisualType::Line,
        ColumnType::Date,
        keys,
        Some(vec![t("Online"), t("Store")]),
        &["Orders"],
        cells,
    );
    let v = visual(VisualType::Line, 500, 280);
    let s = cx_with(|cx| line(&v, &r, cx));
    check("chart_line_dates", &s);
    let tx = texts(&s);
    assert!(
        tx.contains(&"Jan 2024") || tx.contains(&"Feb 2024"),
        "{tx:?}"
    );
    // Continuous: the gap in weeks shows as a wider x step, still one Path per series.
    let p = paths(&s);
    assert_eq!(p.len(), 2);
    let pts = s.cmds.iter().find_map(|c| match c {
        Cmd::Path { points, .. } => Some(points.clone()),
        _ => None,
    });
    let pts = pts.unwrap();
    let steps: Vec<f32> = pts.windows(2).map(|w| w[1].0 - w[0].0).collect();
    let (min, max) = steps
        .iter()
        .fold((f32::MAX, 0f32), |(a, b), s| (a.min(*s), b.max(*s)));
    assert!(max > 9.0 * min, "{min} {max}");
}

#[test]
fn line_whole_number_axis_and_unsorted_keys_snapshot() {
    let r = result(
        VisualType::Line,
        ColumnType::Whole,
        vec![
            Value::Integer(2024),
            Value::Integer(2021),
            Value::Integer(2023),
        ],
        None,
        &["Total", "Count"],
        vec![
            vec![d(5.0), d(1.0)],
            vec![d(3.0), d(2.0)],
            vec![d(8.0), d(3.0)],
        ],
    );
    let v = visual(VisualType::Line, 400, 240);
    let s = cx_with(|cx| line(&v, &r, cx));
    check("chart_line_years", &s);
    let tx = texts(&s);
    for l in ["2021", "2022", "2024", "Total", "Count"] {
        assert!(tx.contains(&l), "{l} not in {tx:?}");
    }
    assert!(!tx.iter().any(|l| l.contains("2021.5") || l.ends_with('K')));
    // Drawn in key order: 2021 (row 1), 2023 (row 2), 2024 (row 0).
    let h: Vec<u32> = hits(&s).iter().take(3).map(|t| t.point).collect();
    assert_eq!(h, [1, 2, 0]);
}

// ---------------------------------------------------------------------------------------------
// Sizes, empty results, hostile values.

#[test]
fn tiny_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        regions(3),
        Some(vec![t("Apple"), t("Pear")]),
        &["Total"],
        vec![vec![d(1.0), d(2.0)]; 3],
    );
    let v = visual(VisualType::ClusteredColumn, 120, 80);
    let s = cx_with(|cx| column(&v, &r, cx));
    check("chart_tiny", &s);
}

#[test]
fn empty_snapshot() {
    let r = result(
        VisualType::ClusteredColumn,
        ColumnType::Text,
        Vec::new(),
        None,
        &[],
        Vec::new(),
    );
    for (name, f) in [
        (
            "chart_empty_column",
            column as fn(&Visual, &QueryResult, &Context<'_>) -> Scene,
        ),
        ("chart_empty_bar", bar),
        ("chart_empty_line", line),
    ] {
        let v = visual(VisualType::ClusteredColumn, 300, 200);
        let s = cx_with(|cx| f(&v, &r, cx));
        check(name, &s);
        let tx = texts(&s);
        let has = |p: &str| tx.iter().any(|l| l.starts_with(p));
        assert!(has("0.0") && has("1.0"), "{tx:?}");
    }
}

#[test]
fn hostile_results_never_panic() {
    let kinds: [fn(&Visual, &QueryResult, &Context<'_>) -> Scene; 3] = [column, bar, line];
    let weird = vec![
        d(1e300),
        d(-1e300),
        d(f64::NAN),
        d(f64::INFINITY),
        t("text"),
        Value::Boolean(true),
        Value::Integer(i64::MIN),
        Value::Currency(i64::MAX),
        Value::DateTime(1e300),
        d(1e-300),
        d(-f64::MAX),
        d(f64::MAX),
    ];
    let mut results = vec![
        result(
            VisualType::Line,
            ColumnType::Date,
            weird.clone(),
            None,
            &["A"],
            weird.iter().map(|v| vec![v.clone()]).collect(),
        ),
        result(
            VisualType::Line,
            ColumnType::Decimal,
            vec![d(f64::MAX), d(-f64::MAX)],
            None,
            &["A"],
            one(&[1.0, 2.0]),
        ),
        result(
            VisualType::Line,
            ColumnType::Text,
            regions(10_000),
            None,
            &["A"],
            one(&(0..10_000).map(f64::from).collect::<Vec<_>>()),
        ),
        result(
            VisualType::Line,
            ColumnType::Date,
            regions(1),
            None,
            &["A"],
            one(&[1.0]),
        ),
        result(
            VisualType::Line,
            ColumnType::Text,
            regions(1),
            None,
            &["A", "B"],
            vec![vec![]],
        ),
    ];
    // Cells shorter than the series, Legend keys with no Values, many Values.
    let mut ragged = result(
        VisualType::Line,
        ColumnType::Text,
        regions(3),
        Some(vec![t("x"); 500]),
        &["A"],
        vec![vec![d(1.0)]; 3],
    );
    ragged.values.extend(vec![header("V", None); 600]);
    results.push(ragged);
    for r in &results {
        for f in kinds {
            for (w, h) in [(400, 300), (0, 0), (-50, 10), (20, 20), (60, 40), (5000, 3)] {
                let mut v = visual(VisualType::Line, w, h);
                v.format.data_labels = true;
                let s = cx_with(|cx| f(&v, r, cx));
                sane(&s);
                for pos in [LegendPosition::Left, LegendPosition::Right] {
                    v.format.legend_position = pos;
                    sane(&cx_with(|cx| f(&v, r, cx)));
                }
            }
        }
    }
}
