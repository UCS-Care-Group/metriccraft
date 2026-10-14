//! The card Visual (M4.3): its one value large and centred in Auto display units (spec R-4),
//! the value's label below in muted text. BLANK shows "(Blank)"; text values as they are.
//!
//! **Tag** (hit-testing, M4.5): an invisible Rect over the value with `Tag { series: 0,
//! point: 0 }`.

use super::{Context, PAD, auto_units, frame};
use crate::draw::{Align, Cmd, Scene, Tag, Weight};
use crate::query::QueryResult;
use crate::text;
use crate::visual::Visual;

/// The value's largest size, px.
const VALUE_SIZE: f32 = 36.0;
/// The value's smallest size before it is cut with "…".
const MIN_VALUE_SIZE: f32 = 8.0;
/// Gap between the value and its label.
const GAP: f32 = 4.0;

/// Lays a card out.
pub fn layout(visual: &Visual, result: &QueryResult, cx: &Context<'_>) -> Scene {
    // ponytail: Power BI's card hides its title by default (the label below names the value);
    // we show only a title the user typed. A per-type title default belongs in M4.2's Format.
    let mut v = visual.clone();
    v.format.show_title &= visual.format.title.is_some();
    let (mut s, top) = frame(&v, Some(result), cx);
    let avail_w = s.width - 2.0 * PAD;
    let avail_h = s.height - PAD - top;
    let Some(header) = result.values.first() else {
        return s;
    };
    if avail_w <= 0.0 || avail_h <= 0.0 {
        return s;
    }
    let value = result.rows.first().and_then(|r| result.cell(r, 0, 0));
    let shown = match value {
        Some(v) if !v.is_blank() => auto_units(v, header, cx.locale),
        _ => "(Blank)".into(),
    };

    let label_size = text::clamp_size(cx.theme.value_size);
    let (la, ld) = text::ascent_descent(label_size, Weight::Regular);
    let label_h = la + ld;
    // The value: as large as fits beside the label, shrunk to the width.
    let (ra, rd) = text::ascent_descent(1.0, Weight::Regular);
    let line = (ra + rd).max(f32::EPSILON);
    let with_label = avail_h >= label_h + GAP + MIN_VALUE_SIZE * line;
    let room = if with_label {
        avail_h - label_h - GAP
    } else {
        avail_h
    };
    let mut size = VALUE_SIZE.min(room / line);
    let w = text::width(&shown, size, Weight::Regular);
    if w > avail_w && w > 0.0 {
        size *= avail_w / w;
    }
    let size = text::clamp_size(size.max(MIN_VALUE_SIZE));
    let (va, vd) = text::ascent_descent(size, Weight::Regular);
    let block = va + vd + if with_label { GAP + label_h } else { 0.0 };
    let y0 = top + ((avail_h - block) / 2.0).max(0.0);
    let cx_mid = PAD + avail_w / 2.0;

    let t = text::truncate(&shown, size, Weight::Regular, avail_w);
    let tw = text::width(&t, size, Weight::Regular);
    s.push(Cmd::Rect {
        x: cx_mid - tw / 2.0,
        y: y0,
        w: tw,
        h: va + vd,
        fill: None,
        stroke: None,
        tag: Some(Tag {
            series: 0,
            point: 0,
        }),
    });
    if !t.is_empty() && y0 + va <= s.height {
        s.push(Cmd::Text {
            x: cx_mid,
            y: y0 + va,
            text: t,
            size,
            weight: Weight::Regular,
            color: cx.theme.text,
            align: Align::Middle,
            tag: None,
        });
    }
    if with_label {
        let l = text::truncate(&header.label, label_size, Weight::Regular, avail_w);
        if !l.is_empty() {
            s.push(Cmd::Text {
                x: cx_mid,
                y: y0 + va + vd + GAP + la,
                text: l,
                size: label_size,
                weight: Weight::Regular,
                color: cx.theme.text_muted,
                align: Align::Middle,
                tag: None,
            });
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::super::table::test_support::*;
    use super::*;
    use crate::visual::{FieldRef, VisualType, Well};

    fn card(w: i32, h: i32, f: FieldRef) -> Visual {
        visual(VisualType::Card, w, h, &[(Well::Values, vec![f])])
    }

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

    fn shown(s: &Scene) -> Vec<String> {
        s.cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Text { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn card_snapshot_number() {
        let f = fixture();
        let v = card(240, 120, m("Rows"));
        let s = lay(&v, &query(&f, &v));
        assert_eq!(shown(&s), ["8", "Rows"]);
        snap("card_number", &s);
    }

    #[test]
    fn card_snapshot_big_number_in_units() {
        let f = fixture();
        let v = card(240, 120, m("Big Total"));
        let s = lay(&v, &query(&f, &v));
        assert_eq!(shown(&s), ["31.3bn", "Big Total"]);
        let v = card(240, 120, c("Amount"));
        let s = lay(&v, &query(&f, &v));
        assert_eq!(shown(&s), ["31.3K", "Sum of Amount"]);
        snap("card_units", &s);
    }

    #[test]
    fn card_snapshot_blank_and_text() {
        let f = fixture();
        let v = card(240, 120, m("Nothing"));
        let s = lay(&v, &query(&f, &v));
        assert_eq!(shown(&s), ["(Blank)", "Nothing"]);
        snap("card_blank", &s);
        let v = card(160, 100, m("Greeting"));
        let s = lay(&v, &query(&f, &v));
        assert_eq!(shown(&s).get(1).map(String::as_str), Some("Greeting"));
        snap("card_text", &s);
    }

    #[test]
    fn card_snapshot_tiny_and_titled() {
        let f = fixture();
        let mut v = card(240, 140, m("Big Total"));
        v.format.title = Some("Revenue".into());
        let r = query(&f, &v);
        let s = lay(&v, &r);
        assert_eq!(shown(&s).first().map(String::as_str), Some("Revenue"));
        snap("card_titled", &s);
        snap("card_tiny", &lay(&card(40, 24, m("Big Total")), &r));
        for (w, h) in [(0, 0), (-3, 9), (1, 1), (i32::MAX, i32::MAX), (5000, 3)] {
            let _ = lay(&card(w, h, m("Big Total")), &r);
        }
        let empty = visual(VisualType::Card, 200, 100, &[]);
        let s = lay(&empty, &query(&f, &empty));
        assert!(shown(&s).is_empty());
    }
}
