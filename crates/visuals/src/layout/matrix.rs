//! The matrix Visual (M4.3): stepped layout, fully expanded. One row-header column with each
//! Rows level indented; a header row per Columns level (keys spanning their group, subtotal
//! columns "Total"), plus a row of value labels when there are several Values (or no Columns);
//! subtotal and grand total rows and columns bold. BLANK keys show "(Blank)".
//!
//! With row subtotals off (or a window starting inside a group) the parent keys are derived
//! from key changes and shown as label-only rows.
//!
//! **Tags** (hit-testing, M4.5): every value cell has an invisible Rect over it with
//! `Tag { series: j * values.len() + k (column tuple j, value k), point: row index in
//! result.rows }`; the grand total row is `point: rows.len()`. Derived parent rows have none.

use metriccraft_core::Value;

use super::table::{CELL_PAD, Grid, cell, draw_notice, fit, is_number, notice, rule};
use super::{Context, PAD, frame, key_text, value_text};
use crate::draw::{Align, Cmd, Scene, Tag, Weight};
use crate::query::{QueryResult, ResultRow};
use crate::text;
use crate::visual::Visual;

/// Indent per Rows level in the stepped layout.
const INDENT: f32 = 14.0;

/// One drawn row: a result row, a derived parent, or the grand total.
struct Line<'r> {
    label: String,
    /// Rows level of the label (0 = outermost).
    depth: usize,
    bold: bool,
    /// The result row and its index (`rows.len()` for the grand total).
    row: Option<(&'r ResultRow, usize)>,
}

/// `row`'s first `n` keys.
fn path(row: &ResultRow, n: usize) -> Vec<Option<&Value>> {
    row.keys.iter().take(n).map(Option::as_ref).collect()
}

/// The lines for `rows`, at most `max` of them, deriving parent rows the result doesn't have.
fn lines<'r>(result: &'r QueryResult, max: usize, cx: &Context<'_>) -> Vec<Line<'r>> {
    let mut out: Vec<Line<'r>> = Vec::new();
    let mut context: Vec<Option<&'r Value>> = Vec::new();
    let label = |level: usize, v: &Value| match result.row_groups.get(level) {
        Some(h) => key_text(v, h, cx.locale),
        None => String::new(),
    };
    for (i, row) in result.rows.iter().enumerate() {
        if out.len() >= max {
            break;
        }
        let level = row.level();
        // Ancestors not shown yet (no subtotal row, or the window starts inside a group).
        for a in 0..level.saturating_sub(1) {
            let same = context.len() > a
                && (0..=a).all(|i| {
                    context.get(i).copied().flatten() == row.keys.get(i).and_then(Option::as_ref)
                });
            if same {
                continue;
            }
            context = path(row, a + 1);
            if let Some(key) = row.keys.get(a).and_then(Option::as_ref)
                && out.len() < max
            {
                out.push(Line {
                    label: label(a, key),
                    depth: a,
                    bold: true,
                    row: None,
                });
            }
        }
        if out.len() >= max {
            break;
        }
        let depth = level.saturating_sub(1);
        let key = level
            .checked_sub(1)
            .and_then(|l| row.keys.get(l))
            .and_then(Option::as_ref);
        context = path(row, level);
        out.push(Line {
            label: key.map(|k| label(depth, k)).unwrap_or_default(),
            depth,
            bold: row.is_total(),
            row: Some((row, i)),
        });
    }
    out
}

/// A column tuple that is a subtotal or the grand total.
fn is_total_column(keys: &[Option<Value>]) -> bool {
    keys.iter().any(Option::is_none)
}

/// The header text of column tuple `keys` at level `l`.
fn column_header(
    result: &QueryResult,
    keys: &[Option<Value>],
    l: usize,
    cx: &Context<'_>,
) -> String {
    match keys.get(l) {
        Some(Some(v)) => match result.column_groups.get(l) {
            Some(h) => key_text(v, h, cx.locale),
            None => String::new(),
        },
        // The first missing level names the (sub)total; deeper levels are empty.
        Some(None) if l == 0 || matches!(keys.get(l - 1), Some(Some(_))) => "Total".into(),
        _ => String::new(),
    }
}

/// Lays a matrix out.
pub fn layout(visual: &Visual, result: &QueryResult, cx: &Context<'_>) -> Scene {
    let (mut s, top) = frame(visual, Some(result), cx);
    let avail_w = s.width - 2.0 * PAD;
    if avail_w <= 0.0 || s.height - PAD - top <= 0.0 {
        return s;
    }
    let g = Grid::new(cx);
    let nv = result.values.len();
    let m = result.column_groups.len();
    if nv == 0 && result.row_groups.is_empty() {
        return s;
    }
    let msg = notice(result, cx.locale);
    let notice_h = draw_notice(&mut s, msg.as_deref(), top, cx);
    let grid_bottom = s.height - PAD - notice_h;

    // Header rows: one per Columns level, then value labels.
    let value_row = nv > 0 && (nv > 1 || m == 0);
    let header_rows = (m + usize::from(value_row)).max(1);
    let body_top = top + header_rows as f32 * g.row_h;
    if body_top > grid_bottom {
        return s;
    }
    let total_h = if result.total.is_some() { g.row_h } else { 0.0 };
    let max_lines = g.rows_between(body_top, grid_bottom - total_h);
    let mut body = lines(result, max_lines, cx);
    if let Some(t) = &result.total {
        body.push(Line {
            label: "Total".into(),
            depth: 0,
            bold: true,
            row: Some((t, result.rows.len())),
        });
    }

    // Leaf columns: (tuple j, value k).
    let leaves: Vec<(usize, usize)> = (0..result.column_keys.len())
        .flat_map(|j| (0..nv).map(move |k| (j, k)))
        .collect();
    let leaf_total = |j: usize| {
        result
            .column_keys
            .get(j)
            .is_some_and(|k| is_total_column(k))
    };
    // Cell texts per line (empty for derived parents).
    let texts: Vec<Vec<String>> = body
        .iter()
        .map(|line| match line.row {
            Some((row, _)) => leaves
                .iter()
                .map(
                    |&(j, k)| match (result.cell(row, j, k), result.values.get(k)) {
                        (Some(v), Some(h)) => value_text(v, h, cx.locale),
                        _ => String::new(),
                    },
                )
                .collect(),
            None => Vec::new(),
        })
        .collect();

    // Natural widths: the row-header column, then each leaf.
    let corner = result
        .row_groups
        .first()
        .map(|h| h.label.clone())
        .unwrap_or_default();
    let mut natural = Vec::with_capacity(leaves.len().saturating_add(1));
    let mut rh = text::width(&corner, g.size, Weight::Bold);
    for line in &body {
        let weight = if line.bold {
            Weight::Bold
        } else {
            Weight::Regular
        };
        rh = rh.max(line.depth as f32 * INDENT + text::width(&line.label, g.size, weight));
    }
    natural.push(rh + 2.0 * CELL_PAD);
    for (li, &(j, k)) in leaves.iter().enumerate() {
        let mut w = 0.0f32;
        if value_row && let Some(h) = result.values.get(k) {
            w = w.max(text::width(&h.label, g.size, Weight::Bold));
        }
        if !value_row && let Some(keys) = result.column_keys.get(j) {
            // The innermost header sits over one leaf (upper levels span and truncate).
            if let Some(l) = m.checked_sub(1) {
                let t = column_header(result, keys, l, cx);
                w = w.max(text::width(&t, g.size, Weight::Bold));
            }
        }
        for (line, row) in body.iter().zip(&texts) {
            if let Some(t) = row.get(li) {
                let bold = line.bold || leaf_total(j);
                let weight = if bold { Weight::Bold } else { Weight::Regular };
                w = w.max(text::width(t, g.size, weight));
            }
        }
        natural.push(w + 2.0 * CELL_PAD);
    }
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
    // Leaf `li`'s x and width.
    let leaf_at = |li: usize| -> Option<(f32, f32)> {
        let i = li.checked_add(1)?;
        Some((*xs.get(i)?, *widths.get(i)?))
    };

    s.push(Cmd::PushClip {
        x: PAD,
        y: top,
        w: avail_w,
        h: grid_bottom - top,
    });
    let (hx, hw) = (PAD, widths.first().copied().unwrap_or(0.0));
    let last_header_y = top + (header_rows - 1) as f32 * g.row_h;
    cell(
        &mut s,
        &g,
        (hx, last_header_y, hw),
        &corner,
        Weight::Bold,
        cx.theme.text,
        Align::Start,
        None,
    );

    // Column headers: per level, spans of tuples with the same keys so far.
    for l in 0..m {
        let y = top + l as f32 * g.row_h;
        let innermost = l + 1 == m && !value_row;
        let mut j = 0;
        while let Some(keys) = result.column_keys.get(j) {
            let prefix = keys.get(..=l).unwrap_or(keys);
            let mut end = j + 1;
            while result
                .column_keys
                .get(end)
                .is_some_and(|k| k.get(..=l).unwrap_or(k) == prefix)
            {
                end += 1;
            }
            let first = j.saturating_mul(nv);
            let last = end.saturating_mul(nv);
            if let Some((x, _)) = leaf_at(first)
                && x < right_edge
            {
                let w: f32 = (first..last)
                    .filter_map(|li| leaf_at(li).map(|p| p.1))
                    .sum();
                let t = column_header(result, keys, l, cx);
                let align = if innermost { Align::End } else { Align::Start };
                cell(
                    &mut s,
                    &g,
                    (x, y, w),
                    &t,
                    Weight::Bold,
                    cx.theme.text,
                    align,
                    None,
                );
            }
            j = end;
        }
    }
    if value_row {
        for (li, &(_, k)) in leaves.iter().enumerate() {
            if let (Some((x, w)), Some(h)) = (leaf_at(li), result.values.get(k))
                && x < right_edge
            {
                cell(
                    &mut s,
                    &g,
                    (x, last_header_y, w),
                    &h.label,
                    Weight::Bold,
                    cx.theme.text,
                    Align::End,
                    None,
                );
            }
        }
    }
    rule(&mut s, PAD, body_top, grid_w, cx.theme.axis);

    // Body, then the grand total.
    let mut y = body_top;
    let count = body.len();
    for (n, (line, row_texts)) in body.iter().zip(&texts).enumerate() {
        let grand = result.total.is_some() && n + 1 == count;
        if grand {
            if y + g.row_h > grid_bottom {
                break;
            }
            rule(&mut s, PAD, y, grid_w, cx.theme.axis);
        }
        let weight = if line.bold {
            Weight::Bold
        } else {
            Weight::Regular
        };
        let x = hx + line.depth as f32 * INDENT;
        let w = (hw - line.depth as f32 * INDENT).max(0.0);
        cell(
            &mut s,
            &g,
            (x, y, w),
            &line.label,
            weight,
            cx.theme.text,
            Align::Start,
            None,
        );
        if let Some((row, point)) = line.row {
            for (li, &(j, k)) in leaves.iter().enumerate() {
                let Some((x, w)) = leaf_at(li) else { break };
                if x >= right_edge {
                    break;
                }
                let bold = line.bold || leaf_total(j);
                let weight = if bold { Weight::Bold } else { Weight::Regular };
                let right = result
                    .cell(row, j, k)
                    .is_none_or(|v| v.is_blank() || is_number(v));
                let align = if right { Align::End } else { Align::Start };
                let tag = Tag {
                    series: u32::try_from(li).unwrap_or(u32::MAX),
                    point: u32::try_from(point).unwrap_or(u32::MAX),
                };
                let t = row_texts.get(li).map_or("", String::as_str);
                cell(
                    &mut s,
                    &g,
                    (x, y, w),
                    t,
                    weight,
                    cx.theme.text,
                    align,
                    Some(tag),
                );
            }
        }
        y += g.row_h;
        if n + 1 < count && !(result.total.is_some() && n + 2 == count) {
            rule(&mut s, PAD, y, grid_w, cx.theme.gridline);
        }
    }
    s.push(Cmd::PopClip);
    s
}

#[cfg(test)]
mod tests {
    use super::super::table::test_support::*;
    use super::*;
    use crate::visual::{VisualType, Well};

    fn lay(v: &Visual, r: &QueryResult) -> Scene {
        let th = theme();
        let cx = Context {
            theme: &th,
            locale: &metriccraft_core::EN_GB,
        };
        let s = layout(v, r, &cx);
        assert_sane(&s);
        s
    }

    fn matrix(
        w: i32,
        h: i32,
        columns: Vec<crate::FieldRef>,
        values: Vec<crate::FieldRef>,
    ) -> Visual {
        visual(
            VisualType::Matrix,
            w,
            h,
            &[
                (Well::Rows, vec![c("Region"), c("Product")]),
                (Well::Columns, columns),
                (Well::Values, values),
            ],
        )
    }

    fn texts(s: &Scene) -> Vec<String> {
        s.cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn matrix_snapshot_one_column_level() {
        let f = fixture();
        let v = matrix(520, 400, vec![c("Year")], vec![c("Amount")]);
        let r = query(&f, &v);
        let s = lay(&v, &r);
        let t = texts(&s);
        assert!(t.iter().any(|t| t == "(Blank)"), "{t:?}");
        assert!(t.iter().filter(|t| *t == "Total").count() >= 2, "{t:?}");
        snap("matrix_one_level", &s);
    }

    #[test]
    fn matrix_snapshot_two_column_levels() {
        let f = fixture();
        let v = matrix(640, 400, vec![c("Year"), c("Quarter")], vec![c("Amount")]);
        let r = query(&f, &v);
        snap("matrix_two_levels", &lay(&v, &r));
    }

    #[test]
    fn matrix_snapshot_several_values() {
        let f = fixture();
        let v = matrix(700, 400, vec![c("Year")], vec![c("Amount"), m("Rows")]);
        let r = query(&f, &v);
        let s = lay(&v, &r);
        assert!(texts(&s).iter().any(|t| t == "Rows"));
        snap("matrix_values", &s);
    }

    #[test]
    fn matrix_snapshot_subtotals_off() {
        let f = fixture();
        let mut v = matrix(520, 400, vec![c("Year")], vec![c("Amount")]);
        v.format.row_subtotals = false;
        v.format.column_subtotals = false;
        let r = query(&f, &v);
        assert!(r.rows.iter().all(|row| !row.is_total()));
        let s = lay(&v, &r);
        let t = texts(&s);
        // Parents derived from key changes, stepped.
        for parent in ["North", "South", "West"] {
            assert_eq!(t.iter().filter(|x| *x == parent).count(), 1, "{t:?}");
        }
        assert!(!t.iter().any(|x| x == "Total"), "{t:?}");
        snap("matrix_subtotals_off", &s);
    }

    #[test]
    fn matrix_snapshot_rows_only_and_tiny() {
        let f = fixture();
        let v = visual(
            VisualType::Matrix,
            300,
            200,
            &[
                (Well::Rows, vec![c("Region")]),
                (Well::Values, vec![c("Amount")]),
            ],
        );
        let r = query(&f, &v);
        snap("matrix_rows_only", &lay(&v, &r));
        let big = matrix(640, 400, vec![c("Year"), c("Quarter")], vec![c("Amount")]);
        let r = query(&f, &big);
        snap("matrix_tiny", &lay(&matrix(60, 50, vec![], vec![]), &r));
        for (w, h) in [(0, 0), (-1, -1), (1, 1), (i32::MAX, i32::MAX)] {
            let _ = lay(&matrix(w, h, vec![], vec![]), &r);
        }
    }

    #[test]
    fn matrix_handles_empty_and_huge() {
        let f = fixture();
        let empty = visual(VisualType::Matrix, 300, 200, &[]);
        let _ = lay(&empty, &query(&f, &empty));
        let v = visual(
            VisualType::Matrix,
            400,
            300,
            &[
                (Well::Rows, vec![c("Big.Id")]),
                (Well::Columns, vec![c("Big.V")]),
                (Well::Values, vec![c("Big.V")]),
            ],
        );
        let r = query(&f, &v);
        assert!(r.truncated);
        let s = lay(&v, &r);
        assert!(s.cmds.len() < 2_000, "{}", s.cmds.len());
        // Inconsistent hand-made result.
        let mut r = r;
        r.column_keys.push(vec![None, None, None]);
        r.values.clear();
        if let Some(row) = r.rows.first_mut() {
            row.keys = vec![None, Some(Value::Decimal(f64::NAN))];
        }
        let _ = lay(&v, &r);
    }
}
