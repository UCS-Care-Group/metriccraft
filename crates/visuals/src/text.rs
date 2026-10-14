//! Text metrics over the bundled Inter (ADR 0007), so layout measures what renderers draw.
//! No shaping: one glyph per char, advances plus pair kerning.

use std::sync::OnceLock;

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};

use crate::draw::Weight;

/// Inter Regular (OFL 1.1), the one bundled font (ADR 0007).
pub static REGULAR: &[u8] = include_bytes!("../fonts/Inter-Regular.ttf");
/// Inter Bold.
pub static BOLD: &[u8] = include_bytes!("../fonts/Inter-Bold.ttf");

/// Text longer than this many chars is measured and drawn only up to it (hostile cell text).
pub const MAX_CHARS: usize = 4096;

/// The bundled font of `weight` (`None` only if the bundled file were unreadable).
pub fn font(weight: Weight) -> Option<&'static FontRef<'static>> {
    static FONTS: OnceLock<(Option<FontRef<'static>>, Option<FontRef<'static>>)> = OnceLock::new();
    let (r, b) = FONTS.get_or_init(|| {
        (
            FontRef::try_from_slice(REGULAR).ok(),
            FontRef::try_from_slice(BOLD).ok(),
        )
    });
    match weight {
        Weight::Regular => r.as_ref(),
        Weight::Bold => b.as_ref(),
    }
}

/// A usable pixel size: finite, 1..=512.
pub fn clamp_size(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(1.0, 512.0)
    } else {
        12.0
    }
}

/// The width of `text` at `size` px.
pub fn width(text: &str, size: f32, weight: Weight) -> f32 {
    let Some(font) = font(weight) else {
        return 0.0;
    };
    let f = font.as_scaled(PxScale::from(clamp_size(size)));
    let mut w = 0.0;
    let mut prev = None;
    for c in text.chars().take(MAX_CHARS) {
        let id = f.glyph_id(c);
        if let Some(p) = prev {
            w += f.kern(p, id);
        }
        w += f.h_advance(id);
        prev = Some(id);
    }
    w
}

/// Ascent (above the baseline, positive) and descent (below, positive) at `size` px.
pub fn ascent_descent(size: f32, weight: Weight) -> (f32, f32) {
    match font(weight) {
        Some(font) => {
            let f = font.as_scaled(PxScale::from(clamp_size(size)));
            (f.ascent(), -f.descent())
        }
        None => (size, 0.0),
    }
}

/// The em size a renderer that scales fonts by em (egui) must use so that its glyphs match the
/// ones measured here at `size` (ab_glyph's `PxScale` is the ascent-to-descent height, not the em).
pub fn em_size(size: f32, weight: Weight) -> f32 {
    let size = clamp_size(size);
    match font(weight) {
        Some(f) => {
            let (em, h) = (f.units_per_em().unwrap_or(1.0), f.height_unscaled());
            if h > 0.0 { size * em / h } else { size }
        }
        None => size,
    }
}

/// `text` cut to fit `max` px, ending in "…" when cut; empty when not even "…" fits.
pub fn truncate(text: &str, size: f32, weight: Weight, max: f32) -> String {
    if width(text, size, weight) <= max {
        return text.chars().take(MAX_CHARS).collect();
    }
    let ellipsis = width("…", size, weight);
    if ellipsis.is_nan() || max.is_nan() || ellipsis > max {
        return String::new();
    }
    let mut out = String::new();
    let mut w = ellipsis;
    for c in text.chars().take(MAX_CHARS) {
        let cw = width(&c.to_string(), size, weight);
        if w + cw > max {
            break;
        }
        w += cw;
        out.push(c);
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_and_truncates() {
        assert!(font(Weight::Regular).is_some() && font(Weight::Bold).is_some());
        let w = width("Hello", 12.0, Weight::Regular);
        assert!(w > 20.0 && w < 40.0, "{w}");
        assert!(width("Hello", 12.0, Weight::Bold) > w);
        assert!((width("Hello", 24.0, Weight::Regular) - 2.0 * w).abs() < 0.5);
        let (a, d) = ascent_descent(12.0, Weight::Regular);
        assert!(a > 8.0 && d > 1.0, "{a} {d}");
        assert_eq!(truncate("Hello", 12.0, Weight::Regular, 100.0), "Hello");
        let t = truncate("Hello world", 12.0, Weight::Regular, 30.0);
        assert!(t.ends_with('…') && t.len() < "Hello world".len(), "{t}");
        assert!(width(&t, 12.0, Weight::Regular) <= 30.5);
        assert_eq!(truncate("Hello", 12.0, Weight::Regular, 1.0), "");
        // Hostile input: NaN sizes and widths, huge text.
        assert_eq!(truncate("Hello", f32::NAN, Weight::Regular, f32::NAN), "");
        assert!(width("x", f32::INFINITY, Weight::Regular).is_finite());
        let huge = "x".repeat(1_000_000);
        assert!(width(&huge, 12.0, Weight::Regular) < 1e6);
    }
}
