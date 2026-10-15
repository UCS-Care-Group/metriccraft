//! Clustered bar, clustered column and line charts (M4.4): a chart's [`QueryResult`] (rows are
//! categories, series are Legend tuples × Values) into a [`Scene`].
//!
//! **Tags** (for hit-testing and dimming, M4.5). `Tag.series` is the series index, which is the
//! cell index in a row (`j * values.len() + k` for Legend tuple `j`, Value `k`); `Tag.point` is
//! the category's index in `result.rows`.
//! - bar, column: every bar `Rect` and its data label carry the bar's Tag.
//! - line: each unbroken run of a series is a `Path` tagged with the series and the run's first
//!   point; a lone point (BLANKs on both sides) is a small filled `Rect` with its Tag; and every
//!   point has an invisible hit `Rect` (no fill, no stroke, [`HIT`] px square, centred on the
//!   point) with its Tag, emitted after all the lines. Renderers draw nothing for it.
//! - Axes, gridlines, legend and notices are untagged.

use metriccraft_core::Value;
use metriccraft_data::step::ColumnType;

use crate::draw::{Align, Cmd, Color, Scene, Tag, Weight};
use crate::layout::axis::{date_ticks, nice_ticks, tick_labels};
use crate::layout::{Context, PAD, auto_units, frame, key_text};
use crate::query::{Header, QueryResult};
use crate::text;
use crate::visual::{LegendPosition, Visual};

/// Most series a chart draws, whatever the result holds (the query caps Legend tuples; this
/// caps a hostile number of Values).
const MAX_SERIES: usize = 256;
/// The side of a line point's invisible hit rectangle.
pub const HIT: f32 = 8.0;
/// The share of a category's band its bars fill.
// ponytail: fixed inner padding, not Power BI's adjustable one.
const GROUP: f32 = 0.8;
/// Gap between axis labels and the plot.
const GAP: f32 = 6.0;
/// Category labels never take more than this, even when there is room.
const MAX_CATEGORY_LABEL: f32 = 160.0;
/// Width a thinned category label may take before it is cut.
const THINNED_LABEL: f32 = 100.0;
const LINE_WIDTH: f32 = 2.0;
const LONE_POINT: f32 = 5.0;
const SWATCH: f32 = 8.0;

/// A clustered column chart: categories along x, values up from 0.
pub fn column(visual: &Visual, result: &QueryResult, cx: &Context<'_>) -> Scene {
    let data = Data::new(result, cx);
    let (mut s, area) = chrome(visual, result, &data, cx);
    let t = Type::new(cx);
    let plot_y0 = area.y + t.line / 2.0;
    let plot_y1 = area.bottom() - t.line - GAP / 2.0;
    let axis = ValueAxis::new(&data, ticks_for(plot_y1 - plot_y0), result, cx);
    let label_w = axis.labels.iter().map(|l| t.width(l)).fold(0.0, f32::max);
    let plot = Rect::new(area.x + label_w + GAP, plot_y0, area.right(), plot_y1);
    if !plot.drawable() {
        return s;
    }
    let y = |v: f64| plot.bottom() - axis.frac(v) * plot.h;
    value_gridlines_h(&mut s, &axis, &plot, &t, cx, y);
    let n = data.categories.len();
    if n > 0 {
        let band = plot.w / n as f32;
        category_labels_x(&mut s, &data, &plot, &area, &t, cx, |i| {
            plot.x + (i as f32 + 0.5) * band
        });
        let bar_w = band * GROUP / data.series.len().max(1) as f32;
        let zero = y(0.0);
        for (si, series) in data.series.iter().enumerate() {
            for (i, v) in series.values.iter().enumerate() {
                let Some(v) = *v else { continue };
                let x = plot.x + i as f32 * band + band * (1.0 - GROUP) / 2.0 + si as f32 * bar_w;
                let yv = y(v);
                let tag = tag(si, i);
                s.push(Cmd::Rect {
                    x,
                    y: yv.min(zero),
                    w: bar_w,
                    h: (yv - zero).abs(),
                    fill: Some(series.color),
                    stroke: None,
                    tag: Some(tag),
                });
                if visual.format.data_labels {
                    let label = data.label(result, si, i, cx);
                    let tw = t.width(&label);
                    let room = if data.series.len() == 1 { band } else { bar_w };
                    if label.is_empty() || tw > room {
                        continue;
                    }
                    let (outside, inside) = if v >= 0.0 {
                        (yv - 3.0 - t.descent, yv + 3.0 + t.ascent)
                    } else {
                        (yv + 3.0 + t.ascent, yv - 3.0 - t.descent)
                    };
                    let fits_out = outside - t.ascent >= area.y && outside + t.descent <= plot_y1;
                    let fits_in = (yv - zero).abs() >= t.line + 6.0;
                    let (baseline, color) = match (fits_out, fits_in) {
                        (true, _) => (outside, cx.theme.text),
                        (false, true) => (inside, cx.theme.background),
                        (false, false) => continue,
                    };
                    t.text(
                        &mut s,
                        x + bar_w / 2.0,
                        baseline,
                        label,
                        Align::Middle,
                        color,
                        tag,
                    );
                }
            }
        }
    }
    baseline_h(&mut s, &plot, y(0.0), cx);
    s
}

/// A clustered bar chart: categories down y (first on top), values right from 0.
pub fn bar(visual: &Visual, result: &QueryResult, cx: &Context<'_>) -> Scene {
    let data = Data::new(result, cx);
    let (mut s, area) = chrome(visual, result, &data, cx);
    let t = Type::new(cx);
    let cat_w = data
        .categories
        .iter()
        .map(|c| t.width(c))
        .fold(0.0, f32::max)
        .min(MAX_CATEGORY_LABEL)
        .min(area.w * 0.35);
    let plot_x0 = area.x + cat_w + GAP;
    // Room for half of the last value label past the plot's right edge.
    let plot_x1 = area.right() - t.width("0.0K") / 2.0;
    let plot_y1 = area.bottom() - t.line - GAP / 2.0;
    let plot_w = plot_x1 - plot_x0;
    // As many ticks as fit side by side.
    let mut max_ticks = ((plot_w / 60.0).max(0.0) as usize + 1).clamp(2, 8);
    let mut axis = ValueAxis::new(&data, max_ticks, result, cx);
    while max_ticks > 2 {
        let widest = axis.labels.iter().map(|l| t.width(l)).fold(0.0, f32::max);
        let gaps = axis.ticks.len().saturating_sub(1).max(1) as f32;
        if (widest + 8.0) * gaps <= plot_w {
            break;
        }
        max_ticks -= 1;
        axis = ValueAxis::new(&data, max_ticks, result, cx);
    }
    let plot = Rect::new(plot_x0, area.y, plot_x1, plot_y1);
    if !plot.drawable() {
        return s;
    }
    let x = |v: f64| plot.x + axis.frac(v) * plot.w;
    for (tick, label) in axis.ticks.iter().zip(&axis.labels) {
        let tx = x(*tick);
        if *tick != 0.0 {
            s.push(vline(tx, plot.y, plot.bottom(), cx.theme.gridline));
        }
        let (lx, align) = place(tx, t.width(label), area.x, area.right());
        let y = plot.bottom() + GAP / 2.0 + t.ascent;
        t.text(
            &mut s,
            lx,
            y,
            label.clone(),
            align,
            cx.theme.text_muted,
            None,
        );
    }
    let n = data.categories.len();
    if n > 0 {
        let band = plot.h / n as f32;
        let every = thin(n, band, t.line + 2.0);
        for (i, c) in data.categories.iter().enumerate().step_by(every) {
            let cy = plot.y + (i as f32 + 0.5) * band;
            let label = text::truncate(c, t.size, Weight::Regular, cat_w);
            t.text(
                &mut s,
                plot.x - GAP,
                cy + t.middle(),
                label,
                Align::End,
                cx.theme.text_muted,
                None,
            );
        }
        let bar_h = band * GROUP / data.series.len().max(1) as f32;
        let zero = x(0.0);
        for (si, series) in data.series.iter().enumerate() {
            for (i, v) in series.values.iter().enumerate() {
                let Some(v) = *v else { continue };
                let y = plot.y + i as f32 * band + band * (1.0 - GROUP) / 2.0 + si as f32 * bar_h;
                let xv = x(v);
                let tag = tag(si, i);
                s.push(Cmd::Rect {
                    x: xv.min(zero),
                    y,
                    w: (xv - zero).abs(),
                    h: bar_h,
                    fill: Some(series.color),
                    stroke: None,
                    tag: Some(tag),
                });
                if visual.format.data_labels {
                    let label = data.label(result, si, i, cx);
                    let tw = t.width(&label);
                    if label.is_empty() || bar_h < t.ascent {
                        continue;
                    }
                    let length = (xv - zero).abs();
                    let (out_x, out_align, in_x, in_align, fits_out) = if v >= 0.0 {
                        let ox = xv + 3.0;
                        (
                            ox,
                            Align::Start,
                            xv - 3.0,
                            Align::End,
                            ox + tw <= area.right(),
                        )
                    } else {
                        let ox = xv - 3.0;
                        (ox, Align::End, xv + 3.0, Align::Start, ox - tw >= plot.x)
                    };
                    let (lx, align, color) = if fits_out {
                        (out_x, out_align, cx.theme.text)
                    } else if length >= tw + 6.0 {
                        (in_x, in_align, cx.theme.background)
                    } else {
                        continue;
                    };
                    let baseline = y + bar_h / 2.0 + t.middle();
                    t.text(&mut s, lx, baseline, label, align, color, tag);
                }
            }
        }
    }
    s.push(vline(x(0.0), plot.y, plot.bottom(), cx.theme.axis));
    s
}

/// A line chart: one line per series, broken at BLANKs. A date, date/time or number Axis is
/// continuous (points placed by value, ticks at nice dates or numbers); others categorical.
pub fn line(visual: &Visual, result: &QueryResult, cx: &Context<'_>) -> Scene {
    let data = Data::new(result, cx);
    let (mut s, area) = chrome(visual, result, &data, cx);
    let t = Type::new(cx);
    let plot_y0 = area.y + t.line / 2.0;
    let plot_y1 = area.bottom() - t.line - GAP / 2.0;
    let axis = ValueAxis::new(&data, ticks_for(plot_y1 - plot_y0), result, cx);
    let label_w = axis.labels.iter().map(|l| t.width(l)).fold(0.0, f32::max);
    let plot = Rect::new(area.x + label_w + GAP, plot_y0, area.right(), plot_y1);
    if !plot.drawable() {
        return s;
    }
    let y = |v: f64| plot.bottom() - axis.frac(v) * plot.h;
    value_gridlines_h(&mut s, &axis, &plot, &t, cx, y);

    // Where each category sits along x, in drawing order.
    let n = data.categories.len();
    let mut order: Vec<(usize, f32)> = Vec::with_capacity(n);
    match Continuous::new(result) {
        Some(c) => {
            let (lo, hi) = c.domain();
            let span = hi - lo;
            let px = |v: f64| {
                let f = (v - lo) / span;
                plot.x
                    + if f.is_finite() {
                        f.clamp(0.0, 1.0) as f32
                    } else {
                        0.0
                    } * plot.w
            };
            for (i, key) in c.keys.iter().enumerate() {
                if let Some(k) = key {
                    order.push((i, px(*k)));
                }
            }
            order.sort_by(|a, b| a.1.total_cmp(&b.1));
            let max_ticks = ((plot.w / 80.0).max(0.0) as usize + 1).clamp(2, 10);
            let ticks: Vec<(f64, String)> = if c.date {
                date_ticks(lo, hi, max_ticks, cx.locale)
            } else {
                let mut ts: Vec<f64> = nice_ticks(lo, hi, max_ticks)
                    .into_iter()
                    .filter(|v| *v >= lo - span * 1e-9 && *v <= hi + span * 1e-9)
                    .collect();
                if c.whole {
                    ts.retain(|v| v.fract() == 0.0);
                }
                let max = ts.iter().fold(0f64, |m, v| m.max(v.abs()));
                let labels = tick_labels(&ts, c.header, max >= 1e5, cx.locale);
                ts.into_iter().zip(labels).collect()
            };
            let mut right_of_last = f32::NEG_INFINITY;
            for (v, label) in ticks {
                let tx = px(v);
                let w = t.width(&label);
                let (lx, align) = place(tx, w, area.x, area.right());
                let left = match align {
                    Align::Start => lx,
                    Align::Middle => lx - w / 2.0,
                    Align::End => lx - w,
                };
                if left < right_of_last + 8.0 {
                    continue;
                }
                right_of_last = left + w;
                s.push(Cmd::Line {
                    x1: tx,
                    y1: plot.bottom(),
                    x2: tx,
                    y2: plot.bottom() + 3.0,
                    color: cx.theme.axis,
                    width: 1.0,
                });
                let ly = plot.bottom() + GAP / 2.0 + t.ascent;
                t.text(&mut s, lx, ly, label, align, cx.theme.text_muted, None);
            }
        }
        None if n > 0 => {
            let band = plot.w / n as f32;
            let at = |i: usize| plot.x + (i as f32 + 0.5) * band;
            category_labels_x(&mut s, &data, &plot, &area, &t, cx, at);
            order.extend((0..n).map(|i| (i, at(i))));
        }
        None => {}
    }

    // Lines, lone points, then hit rectangles and data labels on top.
    let mut hits = Vec::new();
    let mut labels = Vec::new();
    for (si, series) in data.series.iter().enumerate() {
        let mut run: Vec<(f32, f32)> = Vec::new();
        let mut first = 0;
        let mut last_label_right = f32::NEG_INFINITY;
        for &(i, px) in &order {
            match series.values.get(i).copied().flatten() {
                Some(v) => {
                    let py = y(v);
                    if run.is_empty() {
                        first = i;
                    }
                    run.push((px, py));
                    hits.push(Cmd::Rect {
                        x: px - HIT / 2.0,
                        y: py - HIT / 2.0,
                        w: HIT,
                        h: HIT,
                        fill: None,
                        stroke: None,
                        tag: Some(tag(si, i)),
                    });
                    if visual.format.data_labels {
                        let label = data.label(result, si, i, cx);
                        let w = t.width(&label);
                        let top = py - 4.0 - t.line;
                        if !label.is_empty()
                            && px - w / 2.0 > last_label_right + 2.0
                            && top >= area.y
                        {
                            last_label_right = px + w / 2.0;
                            labels.push((px, py - 4.0 - t.descent, label, tag(si, i)));
                        }
                    }
                }
                None => flush(&mut s, &mut run, series.color, tag(si, first)),
            }
        }
        flush(&mut s, &mut run, series.color, tag(si, first));
    }
    s.cmds.extend(hits);
    for (x, y, label, tag) in labels {
        t.text(&mut s, x, y, label, Align::Middle, cx.theme.text, tag);
    }
    baseline_h(&mut s, &plot, y(0.0), cx);
    s
}

/// Draws a finished run of a line: a Path, or a lone point's mark.
fn flush(s: &mut Scene, run: &mut Vec<(f32, f32)>, color: Color, tag: Tag) {
    match run.as_slice() {
        [] => {}
        [(x, y)] => s.push(Cmd::Rect {
            x: x - LONE_POINT / 2.0,
            y: y - LONE_POINT / 2.0,
            w: LONE_POINT,
            h: LONE_POINT,
            fill: Some(color),
            stroke: None,
            tag: Some(tag),
        }),
        _ => s.push(Cmd::Path {
            points: std::mem::take(run),
            color,
            width: LINE_WIDTH,
            tag: Some(tag),
        }),
    }
    run.clear();
}

// ---------------------------------------------------------------------------------------------
// The data a chart draws.

struct Series {
    name: String,
    color: Color,
    /// Legend tuple and Value of the series (its cells: `j * values.len() + k`).
    j: usize,
    k: usize,
    /// One per category: the number, or `None` for BLANK, non-numbers and non-finite values.
    values: Vec<Option<f64>>,
}

struct Data {
    series: Vec<Series>,
    /// One label per category (row), "(Blank)" for a BLANK key.
    categories: Vec<String>,
    /// The smallest and largest value of any series, if any.
    range: Option<(f64, f64)>,
}

impl Data {
    fn new(result: &QueryResult, cx: &Context<'_>) -> Data {
        let categories = result
            .rows
            .iter()
            .map(|row| {
                let keys: Vec<String> = row
                    .keys
                    .iter()
                    .zip(&result.row_groups)
                    .map(|(k, h)| {
                        k.as_ref()
                            .map(|v| key_text(v, h, cx.locale))
                            .unwrap_or_default()
                    })
                    .collect();
                keys.join(" ")
            })
            .collect();
        let legend = !result.column_groups.is_empty();
        let mut series = Vec::new();
        'outer: for (j, ck) in result.column_keys.iter().enumerate() {
            for (k, vh) in result.values.iter().enumerate() {
                if series.len() >= MAX_SERIES {
                    break 'outer;
                }
                let name = if legend {
                    let key: Vec<String> = ck
                        .iter()
                        .zip(&result.column_groups)
                        .map(|(v, h)| {
                            v.as_ref()
                                .map(|v| key_text(v, h, cx.locale))
                                .unwrap_or_default()
                        })
                        .collect();
                    let key = key.join(" ");
                    if result.values.len() > 1 {
                        format!("{key} {}", vh.label)
                    } else {
                        key
                    }
                } else {
                    vh.label.clone()
                };
                let values = result
                    .rows
                    .iter()
                    .map(|row| result.cell(row, j, k).and_then(number))
                    .collect();
                series.push(Series {
                    name,
                    color: cx.theme.series(series.len()),
                    j,
                    k,
                    values,
                });
            }
        }
        let range = series.iter().flat_map(|s| s.values.iter().flatten()).fold(
            None,
            |r: Option<(f64, f64)>, v| match r {
                None => Some((*v, *v)),
                Some((lo, hi)) => Some((lo.min(*v), hi.max(*v))),
            },
        );
        Data {
            series,
            categories,
            range,
        }
    }

    /// Series `si`'s data label at category `i`, in auto display units.
    fn label(&self, result: &QueryResult, si: usize, i: usize, cx: &Context<'_>) -> String {
        let Some(series) = self.series.get(si) else {
            return String::new();
        };
        let (Some(row), Some(header)) = (result.rows.get(i), result.values.get(series.k)) else {
            return String::new();
        };
        match result.cell(row, series.j, series.k) {
            Some(v) => auto_units(v, header, cx.locale),
            None => String::new(),
        }
    }
}

/// A value as a number to plot: numbers only, finite only.
fn number(v: &Value) -> Option<f64> {
    let d = match v {
        Value::Integer(i) => *i as f64,
        Value::Decimal(d) => *d,
        Value::Currency(c) => *c as f64 / metriccraft_core::CURRENCY_SCALE as f64,
        _ => return None,
    };
    d.is_finite().then_some(d)
}

fn tag(series: usize, point: usize) -> Tag {
    Tag {
        series: u32::try_from(series).unwrap_or(u32::MAX),
        point: u32::try_from(point).unwrap_or(u32::MAX),
    }
}

/// A line chart's continuous Axis: the one Axis Column is a date, date/time or number.
struct Continuous<'a> {
    keys: Vec<Option<f64>>,
    date: bool,
    whole: bool,
    header: Option<&'a Header>,
}

impl<'a> Continuous<'a> {
    fn new(result: &'a QueryResult) -> Option<Continuous<'a>> {
        let [header] = result.row_groups.as_slice() else {
            return None;
        };
        let (date, whole) = match header.data_type? {
            ColumnType::Date | ColumnType::DateTime => (true, false),
            ColumnType::Whole => (false, true),
            ColumnType::Decimal | ColumnType::FixedDecimal => (false, false),
            ColumnType::Text | ColumnType::Boolean => return None,
        };
        let keys: Vec<Option<f64>> = result
            .rows
            .iter()
            .map(|r| match r.keys.first() {
                Some(Some(Value::DateTime(d))) if d.is_finite() => Some(*d),
                Some(Some(v)) => number(v),
                _ => None,
            })
            .collect();
        Some(Continuous {
            keys,
            date,
            whole,
            header: Some(header),
        })
    }

    /// The keys' range; a single key (or none) widened by one either side.
    fn domain(&self) -> (f64, f64) {
        let (lo, hi) = self
            .keys
            .iter()
            .flatten()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(*v), hi.max(*v))
            });
        if !lo.is_finite() || !hi.is_finite() {
            return (0.0, 1.0);
        }
        if hi - lo > 0.0 && (hi - lo).is_finite() {
            (lo, hi)
        } else if (hi - lo).is_finite() {
            (lo - 1.0, hi + 1.0)
        } else {
            (lo / 2.0, hi / 2.0)
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Axes, legend, frame.

/// The value axis: ticks from [`nice_ticks`] over the data and 0 (0..1 without data).
struct ValueAxis {
    ticks: Vec<f64>,
    labels: Vec<String>,
}

impl ValueAxis {
    fn new(data: &Data, max_ticks: usize, result: &QueryResult, cx: &Context<'_>) -> ValueAxis {
        let (lo, hi) = data.range.unwrap_or((0.0, 0.0));
        let (lo, hi) = (lo.min(0.0), hi.max(0.0));
        let hi = if lo == hi { 1.0 } else { hi };
        let ticks = nice_ticks(lo, hi, max_ticks);
        let labels = tick_labels(&ticks, result.values.first(), true, cx.locale);
        ValueAxis { ticks, labels }
    }

    /// Where `v` sits between the first and last tick, 0..=1 (0 when the span is unusable).
    fn frac(&self, v: f64) -> f32 {
        let (Some(t0), Some(t1)) = (self.ticks.first(), self.ticks.last()) else {
            return 0.0;
        };
        let f = (v - t0) / (t1 - t0);
        if f.is_finite() {
            f.clamp(0.0, 1.0) as f32
        } else {
            0.0
        }
    }
}

/// Most ticks for a vertical value axis `len` px tall: one per ~40 px, 2–8.
fn ticks_for(len: f32) -> usize {
    ((len / 40.0).max(0.0) as usize + 1).clamp(2, 8)
}

/// Horizontal gridlines and value labels left of the plot (column, line).
fn value_gridlines_h(
    s: &mut Scene,
    axis: &ValueAxis,
    plot: &Rect,
    t: &Type,
    cx: &Context<'_>,
    y: impl Fn(f64) -> f32,
) {
    for (tick, label) in axis.ticks.iter().zip(&axis.labels) {
        let ty = y(*tick);
        if *tick != 0.0 {
            s.push(Cmd::Line {
                x1: plot.x,
                y1: ty,
                x2: plot.right(),
                y2: ty,
                color: cx.theme.gridline,
                width: 1.0,
            });
        }
        let ly = ty + t.middle();
        t.text(
            s,
            plot.x - GAP,
            ly,
            label.clone(),
            Align::End,
            cx.theme.text_muted,
            None,
        );
    }
}

/// The zero line across the plot, over the marks.
fn baseline_h(s: &mut Scene, plot: &Rect, y: f32, cx: &Context<'_>) {
    s.push(Cmd::Line {
        x1: plot.x,
        y1: y,
        x2: plot.right(),
        y2: y,
        color: cx.theme.axis,
        width: 1.0,
    });
}

fn vline(x: f32, y1: f32, y2: f32, color: Color) -> Cmd {
    Cmd::Line {
        x1: x,
        y1,
        x2: x,
        y2,
        color,
        width: 1.0,
    }
}

/// Category labels under the plot, centred at `at(i)`: every n-th when crowded, cut with "…".
fn category_labels_x(
    s: &mut Scene,
    data: &Data,
    plot: &Rect,
    area: &Rect,
    t: &Type,
    cx: &Context<'_>,
    at: impl Fn(usize) -> f32,
) {
    let n = data.categories.len();
    if n == 0 {
        return;
    }
    let band = plot.w / n as f32;
    let widest = data
        .categories
        .iter()
        .map(|c| t.width(c))
        .fold(0.0, f32::max)
        .min(MAX_CATEGORY_LABEL);
    // ponytail: no rotated labels; thin to every n-th instead, each up to THINNED_LABEL wide.
    let every = thin(n, band, widest.min(THINNED_LABEL) + GAP);
    let room = band * every as f32 - GAP;
    let y = plot.bottom() + GAP / 2.0 + t.ascent;
    for (i, c) in data.categories.iter().enumerate().step_by(every) {
        let x = at(i);
        let max = room.min(2.0 * (x - area.x)).min(2.0 * (area.right() - x));
        let label = text::truncate(c, t.size, Weight::Regular, max);
        if !label.is_empty() {
            t.text(s, x, y, label, Align::Middle, cx.theme.text_muted, None);
        }
    }
}

/// Label every `n`-th of `count` items `band` px apart, so each has `need` px.
fn thin(count: usize, band: f32, need: f32) -> usize {
    if band.is_nan() || band <= 0.0 || !need.is_finite() {
        return count.max(1);
    }
    let every = (need / band).ceil();
    if every.is_finite() && every >= 1.0 {
        (every as usize).clamp(1, count.max(1))
    } else {
        1
    }
}

/// An x and alignment for a label `w` wide centred at `x`, kept inside `lo..hi`.
fn place(x: f32, w: f32, lo: f32, hi: f32) -> (f32, Align) {
    if x - w / 2.0 < lo {
        (lo, Align::Start)
    } else if x + w / 2.0 > hi {
        (hi, Align::End)
    } else {
        (x, Align::Middle)
    }
}

/// The frame, title, truncation notice and legend; returns the area left for the plot.
fn chrome(visual: &Visual, result: &QueryResult, data: &Data, cx: &Context<'_>) -> (Scene, Rect) {
    let (mut s, top) = frame(visual, Some(result), cx);
    let t = Type::new(cx);
    let mut area = Rect::new(PAD, top, s.width - PAD, s.height - PAD);
    if result.truncated {
        let notice = notice(result, cx);
        let y = area.bottom() - t.descent;
        let label = text::truncate(&notice, t.size, Weight::Regular, area.w);
        t.text(
            &mut s,
            area.x,
            y,
            label,
            Align::Start,
            cx.theme.text_muted,
            None,
        );
        area = Rect::new(area.x, area.y, area.right(), area.bottom() - t.line - 4.0);
    }
    let show_legend = visual.format.legend
        && !data.series.is_empty()
        && (!result.column_groups.is_empty() || data.series.len() > 1);
    if show_legend {
        let title = result.column_groups.first().map(|h| h.label.as_str());
        area = legend(
            &mut s,
            data,
            title,
            visual.format.legend_position,
            area,
            &t,
            cx,
        );
    }
    (s, area)
}

/// Our wording for a capped result: categories when rows were left out, else series.
fn notice(result: &QueryResult, cx: &Context<'_>) -> String {
    let count = |n: usize| {
        metriccraft_core::format(&Value::Integer(n as i64), "#,0", cx.locale)
            .unwrap_or_else(|_| n.to_string())
    };
    if result.total_rows > result.rows.len() {
        format!("Showing the first {} categories", count(result.rows.len()))
    } else {
        format!(
            "Showing the first {} series",
            count(result.column_keys.len())
        )
    }
}

/// Draws the legend at `pos` inside `area` and returns what is left of `area`.
// ponytail: items that don't fit are left out (no legend paging or wrapping).
fn legend(
    s: &mut Scene,
    data: &Data,
    title: Option<&str>,
    pos: LegendPosition,
    area: Rect,
    t: &Type,
    cx: &Context<'_>,
) -> Rect {
    let item_w = |name: &str| SWATCH + 4.0 + t.width(name);
    let swatch = |s: &mut Scene, x: f32, baseline: f32, color: Color| {
        s.push(Cmd::Rect {
            x,
            y: baseline - t.ascent / 2.0 - SWATCH / 2.0 + 1.0,
            w: SWATCH,
            h: SWATCH,
            fill: Some(color),
            stroke: None,
            tag: None,
        });
    };
    match pos {
        LegendPosition::Top | LegendPosition::Bottom => {
            let baseline = match pos {
                LegendPosition::Top => area.y + t.ascent,
                _ => area.bottom() - t.descent,
            };
            let mut x = area.x;
            if let Some(title) = title {
                let label = text::truncate(title, t.size, Weight::Bold, area.w / 3.0);
                let w = text::width(&label, t.size, Weight::Bold);
                if !label.is_empty() {
                    s.push(Cmd::Text {
                        x,
                        y: baseline,
                        text: label,
                        size: t.size,
                        weight: Weight::Bold,
                        color: cx.theme.text_muted,
                        align: Align::Start,
                        tag: None,
                    });
                    x += w + 8.0;
                }
            }
            // When the row is too short, each item gets an equal share (at least 60 px).
            let natural: f32 = data.series.iter().map(|se| item_w(&se.name) + 12.0).sum();
            let share = if x + natural <= area.right() {
                f32::INFINITY
            } else {
                ((area.right() - x) / data.series.len().max(1) as f32 - 12.0).max(60.0)
            };
            for series in &data.series {
                let w = item_w(&series.name).min(share);
                if x + SWATCH + 4.0 > area.right() || w <= SWATCH + 4.0 {
                    break;
                }
                // Slack: `w` was summed from the label's own width.
                let max = (w - SWATCH - 4.0 + 0.5).min(area.right() - x - SWATCH - 4.0);
                let label = text::truncate(&series.name, t.size, Weight::Regular, max);
                if label.is_empty() {
                    break;
                }
                swatch(s, x, baseline, series.color);
                let lw = t.width(&label);
                t.text(
                    s,
                    x + SWATCH + 4.0,
                    baseline,
                    label,
                    Align::Start,
                    cx.theme.text,
                    None,
                );
                x += SWATCH + 4.0 + lw + 12.0;
            }
            let h = t.line + 6.0;
            match pos {
                LegendPosition::Top => Rect::new(area.x, area.y + h, area.right(), area.bottom()),
                _ => Rect::new(area.x, area.y, area.right(), area.bottom() - h),
            }
        }
        LegendPosition::Left | LegendPosition::Right => {
            let natural = data
                .series
                .iter()
                .map(|se| item_w(&se.name))
                .chain(title.map(|ti| text::width(ti, t.size, Weight::Bold)))
                .fold(0.0, f32::max);
            let w = natural.min(area.w / 3.0);
            let x = match pos {
                LegendPosition::Left => area.x,
                _ => area.right() - w,
            };
            let mut baseline = area.y + t.ascent;
            if let Some(title) = title {
                let label = text::truncate(title, t.size, Weight::Bold, w);
                if !label.is_empty() {
                    s.push(Cmd::Text {
                        x,
                        y: baseline,
                        text: label,
                        size: t.size,
                        weight: Weight::Bold,
                        color: cx.theme.text_muted,
                        align: Align::Start,
                        tag: None,
                    });
                }
                baseline += t.line + 4.0;
            }
            for series in &data.series {
                if baseline + t.descent > area.bottom() {
                    break;
                }
                let label = text::truncate(&series.name, t.size, Weight::Regular, w - SWATCH - 4.0);
                if label.is_empty() {
                    break;
                }
                swatch(s, x, baseline, series.color);
                t.text(
                    s,
                    x + SWATCH + 4.0,
                    baseline,
                    label,
                    Align::Start,
                    cx.theme.text,
                    None,
                );
                baseline += t.line + 4.0;
            }
            match pos {
                LegendPosition::Left => {
                    Rect::new(area.x + w + 12.0, area.y, area.right(), area.bottom())
                }
                _ => Rect::new(area.x, area.y, area.right() - w - 12.0, area.bottom()),
            }
        }
    }
}

/// A rectangle by its left/top and right/bottom; a reversed one has zero size.
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect {
            x: x0,
            y: y0,
            w: (x1 - x0).max(0.0),
            h: (y1 - y0).max(0.0),
        }
    }

    fn right(&self) -> f32 {
        self.x + self.w
    }

    fn bottom(&self) -> f32 {
        self.y + self.h
    }

    /// Big enough to plot in.
    fn drawable(&self) -> bool {
        self.w >= 4.0 && self.h >= 4.0
    }
}

/// The label type every chart text uses: theme label size, regular weight.
struct Type {
    size: f32,
    ascent: f32,
    descent: f32,
    line: f32,
}

impl Type {
    fn new(cx: &Context<'_>) -> Type {
        let size = cx.theme.label_size;
        let (ascent, descent) = text::ascent_descent(size, Weight::Regular);
        Type {
            size,
            ascent,
            descent,
            line: ascent + descent,
        }
    }

    fn width(&self, s: &str) -> f32 {
        text::width(s, self.size, Weight::Regular)
    }

    /// Baseline offset that centres text vertically on a y.
    fn middle(&self) -> f32 {
        (self.ascent - self.descent) / 2.0
    }

    #[allow(clippy::too_many_arguments)]
    fn text(
        &self,
        s: &mut Scene,
        x: f32,
        y: f32,
        text: String,
        align: Align,
        color: Color,
        tag: impl Into<Option<Tag>>,
    ) {
        s.push(Cmd::Text {
            x,
            y,
            text,
            size: self.size,
            weight: Weight::Regular,
            color,
            align,
            tag: tag.into(),
        });
    }
}

#[cfg(test)]
#[path = "chart_tests.rs"]
mod tests;
