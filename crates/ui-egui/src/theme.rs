//! App theme tokens (glossary **App theme**): the app's own colours, light or dark. Every
//! colour the UI paints comes from here; the report canvas uses the Report theme instead.

use egui::{Color32, Stroke, Visuals};

/// Light or dark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Light,
    Dark,
}

/// The App theme's colours.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub mode: Mode,
    /// Window background behind the panes.
    pub base: Color32,
    /// Ribbon, panes and bars.
    pub panel: Color32,
    /// The canvas area around a Page.
    pub well: Color32,
    pub text: Color32,
    pub muted: Color32,
    pub border: Color32,
    /// The one accent: selection, focus, the active tab.
    pub accent: Color32,
    pub hover: Color32,
    pub danger: Color32,
    pub warning: Color32,
    /// Code colouring (DAX, M): functions, Fields and Tables, text, numbers. Comments are muted.
    pub code_function: Color32,
    pub code_field: Color32,
    pub code_text: Color32,
    pub code_number: Color32,
}

impl Tokens {
    pub fn get(mode: Mode) -> Tokens {
        match mode {
            Mode::Light => Tokens {
                mode,
                base: Color32::from_rgb(0xf3, 0xf4, 0xf6),
                panel: Color32::from_rgb(0xfb, 0xfb, 0xfc),
                well: Color32::from_rgb(0xe6, 0xe8, 0xeb),
                text: Color32::from_rgb(0x1d, 0x22, 0x2b),
                muted: Color32::from_rgb(0x5d, 0x66, 0x73),
                border: Color32::from_rgb(0xd5, 0xd9, 0xdf),
                accent: Color32::from_rgb(0x1f, 0x6f, 0x5c),
                hover: Color32::from_rgb(0xe3, 0xef, 0xec),
                danger: Color32::from_rgb(0xb4, 0x23, 0x18),
                warning: Color32::from_rgb(0x9a, 0x5b, 0x00),
                code_function: Color32::from_rgb(0x1f, 0x4f, 0xb4),
                code_field: Color32::from_rgb(0x7a, 0x3a, 0x9a),
                code_text: Color32::from_rgb(0xa1, 0x3c, 0x14),
                code_number: Color32::from_rgb(0x1a, 0x6a, 0x3a),
            },
            Mode::Dark => Tokens {
                mode,
                base: Color32::from_rgb(0x16, 0x19, 0x1e),
                panel: Color32::from_rgb(0x1f, 0x23, 0x2a),
                well: Color32::from_rgb(0x10, 0x12, 0x16),
                text: Color32::from_rgb(0xe4, 0xe7, 0xec),
                muted: Color32::from_rgb(0x9a, 0xa3, 0xaf),
                border: Color32::from_rgb(0x34, 0x3a, 0x44),
                accent: Color32::from_rgb(0x4f, 0xb8, 0x9c),
                hover: Color32::from_rgb(0x26, 0x34, 0x31),
                danger: Color32::from_rgb(0xf0, 0x6a, 0x5f),
                warning: Color32::from_rgb(0xe0, 0xa4, 0x3c),
                code_function: Color32::from_rgb(0x82, 0xaa, 0xff),
                code_field: Color32::from_rgb(0xd2, 0x9b, 0xf0),
                code_text: Color32::from_rgb(0xf0, 0xa0, 0x70),
                code_number: Color32::from_rgb(0x7c, 0xd3, 0x9b),
            },
        }
    }

    /// egui's own colours from the tokens.
    pub fn visuals(&self) -> Visuals {
        let mut v = match self.mode {
            Mode::Light => Visuals::light(),
            Mode::Dark => Visuals::dark(),
        };
        v.panel_fill = self.panel;
        v.window_fill = self.panel;
        v.extreme_bg_color = self.base;
        v.window_stroke = Stroke::new(1.0, self.border);
        v.override_text_color = Some(self.text);
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = self.accent.gamma_multiply(0.35);
        v.selection.stroke = Stroke::new(1.0, self.accent);
        v.widgets.hovered.weak_bg_fill = self.hover;
        v.widgets.hovered.bg_fill = self.hover;
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.accent);
        v.error_fg_color = self.danger;
        v.warn_fg_color = self.warning;
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an sRGB colour.
    fn luminance(c: Color32) -> f32 {
        let ch = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }

    fn contrast(a: Color32, b: Color32) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn text_meets_wcag_aa_on_every_surface() {
        for mode in [Mode::Light, Mode::Dark] {
            let t = Tokens::get(mode);
            for bg in [t.base, t.panel, t.hover] {
                assert!(contrast(t.text, bg) >= 4.5, "{mode:?} text");
                assert!(contrast(t.muted, bg) >= 4.5, "{mode:?} muted");
            }
            assert!(contrast(t.accent, t.panel) >= 3.0, "{mode:?} accent");
            assert!(contrast(t.danger, t.panel) >= 4.5, "{mode:?} danger");
            for c in [t.code_function, t.code_field, t.code_text, t.code_number] {
                assert!(contrast(c, t.base) >= 4.5, "{mode:?} code {c:?}");
            }
        }
    }
}
