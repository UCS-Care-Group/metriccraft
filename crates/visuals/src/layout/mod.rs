//! Layout: a Visual's [`QueryResult`] into a [`Scene`] (M4.3–M4.5, ADR 0007). This module holds
//! what every Visual shares: the frame and title, value text, auto display units and the error
//! state; each Visual type has its own submodule.

use opendash_core::{Locale, Value};

use crate::draw::{Align, Cmd, Scene, Weight};
use crate::query::{Header, QueryResult};
use crate::text;
use crate::theme::Theme;
use crate::visual::Visual;

pub mod card;
pub mod matrix;
pub mod table;

/// Padding inside a Visual's frame, logical pixels.
pub const PAD: f32 = 8.0;

/// What layout needs besides the Visual and its result.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    pub theme: &'a Theme,
    /// The Project's Culture, for formatting values (spec R-4).
    pub locale: &'a Locale,
}

/// `v` as a cell shows it: BLANK empty, else `FORMAT` with the Field's format string (the
/// value's own text when the string is empty or doesn't apply).
pub fn value_text(v: &Value, header: &Header, locale: &Locale) -> String {
    if v.is_blank() {
        return String::new();
    }
    opendash_core::format(v, &header.format_string, locale).unwrap_or_else(|_| v.to_text())
}

/// A group key as a row or axis label: BLANK is "(Blank)", as in Power BI.
pub fn key_text(v: &Value, header: &Header, locale: &Locale) -> String {
    if v.is_blank() {
        return "(Blank)".into();
    }
    value_text(v, header, locale)
}

/// `v` in Power BI's "Auto" display units: K, M, bn, T with up to three significant digits
/// (`1.23K`, `45.6M`); below 1,000 as `value_text` would show it. Non-numbers as their text.
pub fn auto_units(v: &Value, header: &Header, locale: &Locale) -> String {
    let d = match v {
        Value::Integer(i) => *i as f64,
        Value::Decimal(d) => *d,
        Value::Currency(c) => *c as f64 / opendash_core::CURRENCY_SCALE as f64,
        _ => return value_text(v, header, locale),
    };
    let a = d.abs();
    let (div, suffix) = match a {
        a if !a.is_finite() || a < 1e3 => return value_text(v, header, locale),
        a if a < 1e6 => (1e3, "K"),
        a if a < 1e9 => (1e6, "M"),
        a if a < 1e12 => (1e9, "bn"),
        _ => (1e12, "T"),
    };
    let scaled = d / div;
    let decimals = match scaled.abs() {
        s if s >= 100.0 => 0,
        s if s >= 10.0 => 1,
        _ => 2,
    };
    let s = format!("{scaled:.decimals$}");
    let s = if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    };
    format!("{}{suffix}", s.replace('.', &locale.decimal.to_string()))
}

/// The title a Visual shows: its own, else Power BI's generated one ("Sum of Amount by
/// Region"; "Sum of Amount and Count of Id by Region and Year").
pub fn title(visual: &Visual, result: &QueryResult) -> String {
    if let Some(t) = &visual.format.title {
        return t.clone();
    }
    let names = |hs: &[Header]| -> String {
        let labels: Vec<&str> = hs.iter().map(|h| h.label.as_str()).collect();
        match labels.as_slice() {
            [] => String::new(),
            [one] => (*one).to_string(),
            [init @ .., last] => format!("{} and {last}", init.join(", ")),
        }
    };
    let values = names(&result.values);
    let mut groups = result.row_groups.clone();
    groups.extend(result.column_groups.iter().cloned());
    let by = names(&groups);
    match (values.is_empty(), by.is_empty()) {
        (false, false) => format!("{values} by {by}"),
        (false, true) => values,
        (true, _) => by,
    }
}

/// Starts a Visual's Scene: background, and the title when shown. Returns the Scene and the
/// top of the area below the title.
pub fn frame(visual: &Visual, result: Option<&QueryResult>, cx: &Context<'_>) -> (Scene, f32) {
    let (w, h) = (visual.w.max(0) as f32, visual.h.max(0) as f32);
    let mut s = Scene::new(w, h);
    s.push(Cmd::Rect {
        x: 0.0,
        y: 0.0,
        w,
        h,
        fill: Some(cx.theme.background),
        stroke: None,
        tag: None,
    });
    let mut top = PAD;
    let t = match result {
        Some(r) => title(visual, r),
        None => visual.format.title.clone().unwrap_or_default(),
    };
    if visual.format.show_title && !t.is_empty() {
        let size = cx.theme.title_size;
        let (ascent, descent) = text::ascent_descent(size, Weight::Bold);
        s.push(Cmd::Text {
            x: PAD,
            y: top + ascent,
            text: text::truncate(&t, size, Weight::Bold, w - 2.0 * PAD),
            size,
            weight: Weight::Bold,
            color: cx.theme.text,
            align: Align::Start,
            tag: None,
        });
        top += ascent + descent + PAD / 2.0;
    }
    (s, top)
}

/// The whole Visual as an error (X-3, grill Q16): the frame and the message, wrapped to fit.
pub fn error_scene(visual: &Visual, message: &str, cx: &Context<'_>) -> Scene {
    let (mut s, top) = frame(visual, None, cx);
    let size = cx.theme.label_size;
    let (ascent, descent) = text::ascent_descent(size, Weight::Regular);
    let line = ascent + descent + 2.0;
    let width = s.width - 2.0 * PAD;
    let mut y = top + ascent;
    let heading = "Couldn't show this visual";
    s.push(Cmd::Text {
        x: PAD,
        y,
        text: text::truncate(heading, size, Weight::Bold, width),
        size,
        weight: Weight::Bold,
        color: cx.theme.error,
        align: Align::Start,
        tag: None,
    });
    for l in wrap(message, size, width) {
        y += line;
        if y + descent > s.height {
            break;
        }
        s.push(Cmd::Text {
            x: PAD,
            y,
            text: l,
            size,
            weight: Weight::Regular,
            color: cx.theme.text_muted,
            align: Align::Start,
            tag: None,
        });
    }
    s
}

/// `msg` broken into lines of at most `width` px at `size` (words longer than a line are cut).
pub fn wrap(msg: &str, size: f32, width: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in msg.split_whitespace().take(text::MAX_CHARS) {
        let candidate = if cur.is_empty() {
            word.to_string()
        } else {
            format!("{cur} {word}")
        };
        if text::width(&candidate, size, Weight::Regular) <= width || cur.is_empty() {
            cur = text::truncate(&candidate, size, Weight::Regular, width);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = text::truncate(word, size, Weight::Regular, width);
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::visual::{FieldRef, VisualType};

    fn header(fmt: &str) -> Header {
        Header {
            field: FieldRef::Measure { name: "M".into() },
            label: "M".into(),
            format_string: fmt.into(),
            data_type: None,
            aggregate: None,
        }
    }

    #[test]
    fn auto_units_like_power_bi() {
        let h = header("");
        let en = &opendash_core::EN_GB;
        let u = |v: Value| auto_units(&v, &h, en);
        assert_eq!(u(Value::Integer(999)), "999");
        assert_eq!(u(Value::Integer(1234)), "1.23K");
        assert_eq!(u(Value::Decimal(45_600_000.0)), "45.6M");
        assert_eq!(u(Value::Decimal(-2e9)), "-2bn");
        assert_eq!(u(Value::Decimal(123.4e12)), "123T");
        assert_eq!(u(Value::Blank), "");
        assert_eq!(
            auto_units(&Value::Integer(1234), &h, &opendash_core::DE_DE),
            "1,23K"
        );
        assert_eq!(key_text(&Value::Blank, &h, en), "(Blank)");
        assert_eq!(value_text(&Value::Decimal(0.5), &header("0%"), en), "50%");
    }

    #[test]
    fn error_scene_wraps_inside_the_visual() {
        let mut v = Visual::new("v1", VisualType::Card);
        v.w = 160;
        v.h = 80;
        let th = Theme::light();
        let cx = Context {
            theme: &th,
            locale: &opendash_core::EN_GB,
        };
        let s = error_scene(&v, &"word ".repeat(200), &cx);
        assert!(s.cmds.len() > 3);
        for c in &s.cmds {
            if let Cmd::Text { text: t, size, .. } = c {
                assert!(text::width(t, *size, Weight::Bold) <= 160.0, "{t}");
            }
        }
        let tiny = Visual {
            w: -5,
            h: 0,
            ..v.clone()
        };
        let _ = error_scene(&tiny, "x", &cx);
    }
}
