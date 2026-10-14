//! The table Visual (M4.3): a header row in well order, this window's rows, the total row.
//! Numbers right-aligned, text left; columns sized to content, shrunk to fit the Visual and cut
//! with "…"; rows that don't fit aren't drawn (the window scrolls in M5).
//!
//! **Tags** (hit-testing, M4.5): every body and total cell has an invisible Rect over the cell
//! with `Tag { series: display column index, point: row index in result.rows }`; the total row
//! is `point: rows.len()`.
//!
//! The grid helpers here (`Grid`, `fit`, `cell`, `notice`) are shared with the matrix.

use opendash_core::{Locale, Value};
use opendash_data::step::ColumnType;

use super::{Context, PAD, frame, value_text};
use crate::draw::{Align, Cmd, Color, Scene, Tag, Weight};
use crate::query::{DisplayColumn, Header, QueryResult, ResultRow};
use crate::text;
use crate::visual::Visual;

/// Horizontal padding inside a cell.
pub(super) const CELL_PAD: f32 = 6.0;
/// Vertical padding above and below a cell's text.
const CELL_VPAD: f32 = 4.0;
/// A column shrinks no narrower than this (or its natural width, if less); columns pushed past
/// the right edge are clipped.
pub(super) const MIN_COL: f32 = 40.0;

/// Type metrics of a grid's cells.
#[derive(Clone, Copy, Debug)]
pub(super) struct Grid {
    pub size: f32,
    pub ascent: f32,
    pub row_h: f32,
}

impl Grid {
    pub fn new(cx: &Context<'_>) -> Grid {
        let size = text::clamp_size(cx.theme.value_size);
        let (ascent, descent) = text::ascent_descent(size, Weight::Bold);
        Grid {
            size,
            ascent,
            row_h: ascent + descent + 2.0 * CELL_VPAD,
        }
    }

    /// How many rows of `row_h` fit between `top` and `bottom` (0 for nonsense).
    pub fn rows_between(&self, top: f32, bottom: f32) -> usize {
        let n = ((bottom - top) / self.row_h).floor();
        if n.is_finite() && n > 0.0 {
            n as usize
        } else {
            0
        }
    }
}

/// A Value that lines up right (a number).
pub(super) fn is_number(v: &Value) -> bool {
    matches!(
        v,
        Value::Integer(_) | Value::Decimal(_) | Value::Currency(_)
    )
}

/// Whether a column of `header` lines up right: by its type, else (a Measure) by its first
/// non-blank value, numbers by default.
pub(super) fn right_aligned<'v>(
    header: &Header,
    mut values: impl Iterator<Item = &'v Value>,
) -> bool {
    match header.data_type {
        Some(t) => matches!(
            t,
            ColumnType::Whole | ColumnType::Decimal | ColumnType::FixedDecimal
        ),
        None => values.find(|v| !v.is_blank()).is_none_or(is_number),
    }
}

/// Column widths for `natural` widths in `avail` px: natural when they fit, else shrunk in
/// proportion (but not below [`MIN_COL`]).
pub(super) fn fit(natural: &[f32], avail: f32) -> Vec<f32> {
    let clean = |w: f32| if w.is_finite() { w.max(0.0) } else { 0.0 };
    let sum: f32 = natural.iter().map(|w| clean(*w)).sum();
    let avail = clean(avail);
    if sum <= avail || sum <= 0.0 {
        return natural.iter().map(|w| clean(*w)).collect();
    }
    let scale = avail / sum;
    natural
        .iter()
        .map(|w| {
            let w = clean(*w);
            (w * scale).max(w.min(MIN_COL))
        })
        .collect()
}

/// One cell's text, cut to the cell, plus (when `tag`) an invisible tagged Rect over the cell.
#[allow(clippy::too_many_arguments)]
pub(super) fn cell(
    s: &mut Scene,
    g: &Grid,
    (x, y, w): (f32, f32, f32),
    t: &str,
    weight: Weight,
    color: Color,
    align: Align,
    tag: Option<Tag>,
) {
    if tag.is_some() {
        s.push(Cmd::Rect {
            x,
            y,
            w,
            h: g.row_h,
            fill: None,
            stroke: None,
            tag,
        });
    }
    if t.is_empty() {
        return;
    }
    let t = text::truncate(t, g.size, weight, w - 2.0 * CELL_PAD);
    if t.is_empty() {
        return;
    }
    let tx = match align {
        Align::End => x + w - CELL_PAD,
        Align::Middle => x + w / 2.0,
        Align::Start => x + CELL_PAD,
    };
    s.push(Cmd::Text {
        x: tx,
        y: y + CELL_VPAD + g.ascent,
        text: t,
        size: g.size,
        weight,
        color,
        align,
        tag: None,
    });
}

/// A horizontal rule at `y` from `x` for `w` px.
pub(super) fn rule(s: &mut Scene, x: f32, y: f32, w: f32, color: Color) {
    if w > 0.0 {
        s.push(Cmd::Line {
            x1: x,
            y1: y,
            x2: x + w,
            y2: y,
            color,
            width: 1.0,
        });
    }
}

/// The muted line a capped or windowed result shows.
pub(super) fn notice(result: &QueryResult, locale: &Locale) -> Option<String> {
    if !result.truncated {
        return None;
    }
    let n = |i: usize| {
        let v = Value::Integer(i64::try_from(i).unwrap_or(i64::MAX));
        opendash_core::format(&v, "#,0", locale).unwrap_or_else(|_| i.to_string())
    };
    let shown = result.rows.len();
    let first = result.offset.saturating_add(1);
    let last = result.offset.saturating_add(shown);
    Some(if shown == 0 {
        "No rows in this part of the data".into()
    } else if result.offset == 0 && shown < result.total_rows {
        format!(
            "Showing the first {} of {} rows",
            n(shown),
            n(result.total_rows)
        )
    } else if shown < result.total_rows {
        format!(
            "Showing rows {}–{} of {}",
            n(first),
            n(last),
            n(result.total_rows)
        )
    } else {
        "Some columns aren't shown".into()
    })
}

/// Draws `msg` (from [`notice`]) at the bottom of the Visual; returns its height (0 when not
/// drawn).
pub(super) fn draw_notice(s: &mut Scene, msg: Option<&str>, top: f32, cx: &Context<'_>) -> f32 {
    let Some(msg) = msg else {
        return 0.0;
    };
    let size = text::clamp_size(cx.theme.label_size);
    let (ascent, descent) = text::ascent_descent(size, Weight::Regular);
    let h = ascent + descent + 4.0;
    let baseline = s.height - PAD - descent;
    if baseline - ascent < top {
        return 0.0;
    }
    let t = text::truncate(msg, size, Weight::Regular, s.width - 2.0 * PAD);
    if !t.is_empty() {
        s.push(Cmd::Text {
            x: PAD,
            y: baseline,
            text: t,
            size,
            weight: Weight::Regular,
            color: cx.theme.text_muted,
            align: Align::Start,
            tag: None,
        });
    }
    h
}

/// Lays a table out.
pub fn layout(visual: &Visual, result: &QueryResult, cx: &Context<'_>) -> Scene {
    let (mut s, top) = frame(visual, Some(result), cx);
    let avail_w = s.width - 2.0 * PAD;
    if avail_w <= 0.0 || s.height - PAD - top <= 0.0 || result.display.is_empty() {
        return s;
    }
    let g = Grid::new(cx);
    let msg = notice(result, cx.locale);
    let notice_h = draw_notice(&mut s, msg.as_deref(), top, cx);
    let grid_bottom = s.height - PAD - notice_h;
    let total_h = if result.total.is_some() { g.row_h } else { 0.0 };
    // Header row, then as many body rows as fit above the total row.
    let body_top = top + g.row_h;
    if body_top > grid_bottom {
        return s;
    }
    let shown = g
        .rows_between(body_top, grid_bottom - total_h)
        .min(result.rows.len());
    let body = result.rows.get(..shown).unwrap_or(&[]);

    struct Col<'r> {
        header: &'r Header,
        group: Option<usize>,
        value: Option<usize>,
        right: bool,
    }
    let cols: Vec<Col<'_>> = result
        .display
        .iter()
        .filter_map(|d| match *d {
            DisplayColumn::Group(i) => {
                let header = result.row_groups.get(i)?;
                let keys = body.iter().filter_map(|r| r.keys.get(i)?.as_ref());
                Some(Col {
                    header,
                    group: Some(i),
                    value: None,
                    right: right_aligned(header, keys),
                })
            }
            DisplayColumn::Value(k) => {
                let header = result.values.get(k)?;
                let vals = body.iter().filter_map(|r| result.cell(r, 0, k));
                Some(Col {
                    header,
                    group: None,
                    value: Some(k),
                    right: right_aligned(header, vals),
                })
            }
        })
        .collect();

    // Cell texts: body rows, then the total row (bold).
    let texts = |row: &ResultRow, first_label: bool| -> Vec<String> {
        cols.iter()
            .enumerate()
            .map(|(ci, c)| match (c.group, c.value) {
                (Some(i), _) => match row.keys.get(i).and_then(Option::as_ref) {
                    Some(v) => value_text(v, c.header, cx.locale),
                    None if ci == 0 && first_label => "Total".into(),
                    None => String::new(),
                },
                (_, Some(k)) => result
                    .cell(row, 0, k)
                    .map(|v| value_text(v, c.header, cx.locale))
                    .unwrap_or_default(),
                _ => String::new(),
            })
            .collect()
    };
    let body_texts: Vec<Vec<String>> = body.iter().map(|r| texts(r, false)).collect();
    let total_texts = result.total.as_ref().map(|r| texts(r, true));

    let natural: Vec<f32> = cols
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            let mut w = text::width(&c.header.label, g.size, Weight::Bold);
            for row in &body_texts {
                if let Some(t) = row.get(ci) {
                    w = w.max(text::width(t, g.size, Weight::Regular));
                }
            }
            if let Some(t) = total_texts.as_ref().and_then(|r| r.get(ci)) {
                w = w.max(text::width(t, g.size, Weight::Bold));
            }
            w + 2.0 * CELL_PAD
        })
        .collect();
    let widths = fit(&natural, avail_w);
    let grid_w: f32 = widths.iter().sum::<f32>().min(avail_w);
    let xs: Vec<f32> = widths
        .iter()
        .scan(PAD, |x, w| {
            let at = *x;
            *x += w;
            Some(at)
        })
        .collect();
    let right_edge = PAD + avail_w;
    let align = |c: &Col<'_>| if c.right { Align::End } else { Align::Start };

    s.push(Cmd::PushClip {
        x: PAD,
        y: top,
        w: avail_w,
        h: grid_bottom - top,
    });
    // Header.
    for ((c, x), w) in cols.iter().zip(&xs).zip(&widths) {
        if *x >= right_edge {
            break;
        }
        let label = c.header.label.as_str();
        cell(
            &mut s,
            &g,
            (*x, top, *w),
            label,
            Weight::Bold,
            cx.theme.text,
            align(c),
            None,
        );
    }
    rule(&mut s, PAD, body_top, grid_w, cx.theme.axis);
    // Body.
    let mut y = body_top;
    let draw_row = |s: &mut Scene, y: f32, texts: &[String], point: usize, weight: Weight| {
        let total = point == result.rows.len();
        for (ci, ((c, x), w)) in cols.iter().zip(&xs).zip(&widths).enumerate() {
            if *x >= right_edge {
                break;
            }
            let tag = Tag {
                series: u32::try_from(ci).unwrap_or(u32::MAX),
                point: u32::try_from(point).unwrap_or(u32::MAX),
            };
            let t = texts.get(ci).map_or("", String::as_str);
            cell(
                s,
                &g,
                (*x, y, *w),
                t,
                weight,
                cx.theme.text,
                // The total row's "Total" label reads from the left.
                if total && ci == 0 && c.group.is_some() {
                    Align::Start
                } else {
                    align(c)
                },
                Some(tag),
            );
        }
    };
    for (i, texts) in body_texts.iter().enumerate() {
        draw_row(&mut s, y, texts, i, Weight::Regular);
        y += g.row_h;
        if i + 1 < body_texts.len() {
            rule(&mut s, PAD, y, grid_w, cx.theme.gridline);
        }
    }
    // Total.
    if let Some(texts) = &total_texts
        && y + g.row_h <= grid_bottom
    {
        rule(&mut s, PAD, y, grid_w, cx.theme.axis);
        draw_row(&mut s, y, texts, result.rows.len(), Weight::Bold);
    }
    s.push(Cmd::PopClip);
    s
}

#[cfg(test)]
pub(super) mod test_support {
    //! A small Sales Database for the M4.3 snapshot tests (like `query_tests.rs`).

    use std::path::Path;
    use std::sync::Arc;

    use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
    use opendash_data::step::ColumnType;
    use opendash_data::{Table, TableBuilder, fold_case};
    use opendash_model::dax::{Database, FilterContext, Options};
    use opendash_model::{Column, Measure, Model, ModelTable, SummarizeBy};

    use crate::draw::Scene;
    use crate::query::{QueryInput, QueryResult, run_query};
    use crate::theme::Theme;
    use crate::visual::{FieldRef, Visual, VisualType, Well};

    pub struct Fixture {
        pub db: Database,
        pub model: Model,
    }

    fn table(name: &str, columns: Vec<(&str, ArrayRef)>) -> Table {
        let batch = RecordBatch::try_from_iter(columns).unwrap();
        let mut b = TableBuilder::new(name, batch.schema(), usize::MAX);
        b.push(&batch).unwrap();
        fold_case(b.finish().unwrap(), usize::MAX).unwrap()
    }

    fn text(v: &[Option<&str>]) -> ArrayRef {
        Arc::new(StringArray::from(v.to_vec()))
    }

    /// Sales (Region, Product with a BLANK, Year, Quarter, Amount); Big (2,000
    /// Ids) for the window.
    pub fn fixture() -> Fixture {
        let s = Some;
        let sales = table(
            "Sales",
            vec![
                (
                    "Region",
                    text(&[
                        s("North"),
                        s("North"),
                        s("North"),
                        s("South"),
                        s("South"),
                        s("South"),
                        s("South"),
                        s("West"),
                    ]),
                ),
                (
                    "Product",
                    text(&[
                        s("Apple"),
                        s("Apple"),
                        s("Pear"),
                        s("Apple"),
                        s("Pear"),
                        s("Plum"),
                        s("Plum"),
                        None,
                    ]),
                ),
                (
                    "Year",
                    Arc::new(Int64Array::from(vec![
                        2023, 2024, 2023, 2024, 2023, 2024, 2024, 2024,
                    ])),
                ),
                (
                    "Quarter",
                    text(&[
                        s("Q1"),
                        s("Q2"),
                        s("Q2"),
                        s("Q1"),
                        s("Q1"),
                        s("Q1"),
                        s("Q2"),
                        s("Q1"),
                    ]),
                ),
                (
                    "Amount",
                    Arc::new(Float64Array::from(vec![
                        Some(1250.5),
                        Some(20.0),
                        Some(5.25),
                        Some(7.0),
                        Some(3.0),
                        Some(30_000.0),
                        Some(2.0),
                        Some(4.0),
                    ])),
                ),
            ],
        );
        let ids: Vec<i64> = (0..2_000).collect();
        let vals: Vec<f64> = (0..2_000).map(|i| f64::from(i % 7) * 1.5).collect();
        let big = table(
            "Big",
            vec![
                ("Id", Arc::new(Int64Array::from(ids)) as ArrayRef),
                ("V", Arc::new(Float64Array::from(vals)) as ArrayRef),
            ],
        );
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
                year,
                col("Quarter", ColumnType::Text),
                amount,
            ],
            measures: vec![
                Measure::new("Total", "SUM(Sales[Amount])"),
                Measure::new("Rows", "COUNTROWS(Sales)"),
                Measure::new("Big Total", "SUM(Sales[Amount]) * 1000000"),
                Measure::new("Nothing", "BLANK()"),
                Measure::new("Greeting", "\"Hello, world\""),
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

    pub fn c(column: &str) -> FieldRef {
        let (table, column) = column.split_once('.').unwrap_or(("Sales", column));
        FieldRef::Column {
            table: table.into(),
            column: column.into(),
        }
    }

    pub fn m(name: &str) -> FieldRef {
        FieldRef::Measure { name: name.into() }
    }

    pub fn visual(kind: VisualType, w: i32, h: i32, wells: &[(Well, Vec<FieldRef>)]) -> Visual {
        let mut v = Visual::new("v1", kind);
        v.w = w;
        v.h = h;
        for (well, fs) in wells {
            v.wells.insert(*well, fs.clone());
        }
        v
    }

    pub fn query(f: &Fixture, v: &Visual) -> QueryResult {
        let input = QueryInput {
            db: &f.db,
            model: &f.model,
            filters: &FilterContext::default(),
            options: Options {
                cancel: None,
                memory_cap: usize::MAX,
            },
        };
        run_query(v, &input, 0).unwrap_or_else(|e| panic!("{e}"))
    }

    pub fn theme() -> Theme {
        Theme::light()
    }

    pub fn snap(name: &str, scene: &Scene) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots");
        crate::snapshot::check(&dir, name, scene).unwrap();
    }

    /// Every text and rect stays inside the scene; nothing is NaN.
    pub fn assert_sane(s: &Scene) {
        use crate::draw::Cmd;
        for c in &s.cmds {
            match c {
                Cmd::Text { x, y, .. } => {
                    assert!(x.is_finite() && y.is_finite(), "{c:?}");
                }
                Cmd::Rect { x, y, w, h, .. } => {
                    assert!(x.is_finite() && y.is_finite() && w.is_finite() && h.is_finite());
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use crate::visual::{VisualType, Well};

    fn lay(v: &Visual, r: &QueryResult) -> Scene {
        let th = theme();
        let cx = Context {
            theme: &th,
            locale: &opendash_core::EN_GB,
        };
        let s = layout(v, r, &cx);
        assert_sane(&s);
        s
    }

    fn sales_table(w: i32, h: i32) -> Visual {
        visual(
            VisualType::Table,
            w,
            h,
            &[(
                Well::Values,
                vec![c("Region"), c("Product"), c("Amount"), m("Rows")],
            )],
        )
    }

    #[test]
    fn table_snapshot_normal_with_blanks_and_total() {
        let f = fixture();
        let v = sales_table(400, 300);
        let r = query(&f, &v);
        assert!(r.total.is_some());
        let s = lay(&v, &r);
        let text = s.to_text();
        assert!(text.contains("\"Total\""), "{text}");
        assert!(text.contains("\"1,270.50\""), "{text}");
        snap("table_normal", &s);
    }

    #[test]
    fn table_snapshot_no_total() {
        let f = fixture();
        let mut v = sales_table(400, 300);
        v.format.row_subtotals = false;
        let r = query(&f, &v);
        snap("table_no_total", &lay(&v, &r));
    }

    #[test]
    fn table_snapshot_truncated_notice() {
        let f = fixture();
        let v = visual(
            VisualType::Table,
            300,
            260,
            &[(Well::Values, vec![c("Big.Id"), c("Big.V")])],
        );
        let r = query(&f, &v);
        assert!(r.truncated);
        let s = lay(&v, &r);
        assert!(s.to_text().contains("Showing the first 500 of 2,000 rows"));
        // Only the rows that fit are drawn.
        let tagged = s
            .cmds
            .iter()
            .filter(|c| matches!(c, Cmd::Rect { tag: Some(_), .. }))
            .count();
        assert!(tagged < 40, "{tagged}");
        snap("table_truncated", &s);
    }

    #[test]
    fn table_snapshot_too_narrow() {
        let f = fixture();
        let v = sales_table(150, 200);
        let r = query(&f, &v);
        let s = lay(&v, &r);
        assert!(s.to_text().contains('…'));
        snap("table_narrow", &s);
    }

    #[test]
    fn table_snapshot_tiny_and_hostile_sizes() {
        let f = fixture();
        let r = query(&f, &sales_table(400, 300));
        snap("table_tiny", &lay(&sales_table(30, 20), &r));
        for (w, h) in [(0, 0), (-50, -10), (1, 1), (i32::MAX, 40), (20, i32::MAX)] {
            let _ = lay(&sales_table(w, h), &r);
        }
    }

    #[test]
    fn table_handles_empty_and_odd_results() {
        let f = fixture();
        let empty = visual(VisualType::Table, 300, 200, &[]);
        let r = query(&f, &empty);
        let s = lay(&empty, &r);
        assert!(s.cmds.iter().all(|c| !matches!(c, Cmd::PushClip { .. })));
        // Hand-made, inconsistent result: display points past the headers, NaN cells.
        let v = sales_table(300, 200);
        let mut r = query(&f, &v);
        r.display.push(DisplayColumn::Value(99));
        r.display.push(DisplayColumn::Group(99));
        if let Some(row) = r.rows.first_mut() {
            row.cells = vec![Value::Decimal(f64::NAN)];
            row.keys.clear();
        }
        r.truncated = true;
        r.offset = usize::MAX;
        let _ = lay(&v, &r);
        assert_eq!(fit(&[f32::NAN, 10.0, f32::INFINITY], 5.0).len(), 3);
    }
}
