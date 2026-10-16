//! The one built-in theme: colours and sizes defined in code, original to MetricCraft.

use crate::draw::Color;

/// Colours and type sizes every Visual's layout uses.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub background: Color,
    pub text: Color,
    pub text_muted: Color,
    pub gridline: Color,
    pub axis: Color,
    pub error: Color,
    /// Series colours, in order; series beyond the end repeat it.
    pub palette: [Color; 10],
    pub title_size: f32,
    pub label_size: f32,
    pub value_size: f32,
}

impl Theme {
    /// The light theme.
    pub fn light() -> Theme {
        Theme {
            background: Color::hex(0xffffff),
            text: Color::hex(0x1f2430),
            text_muted: Color::hex(0x5c6370),
            gridline: Color::hex(0xe3e6eb),
            axis: Color::hex(0x9aa1ab),
            error: Color::hex(0xb3261e),
            palette: [
                Color::hex(0x3a6ea5),
                Color::hex(0xe0812f),
                Color::hex(0x4e9f6d),
                Color::hex(0xc4475c),
                Color::hex(0x8a6bbf),
                Color::hex(0x3fa7b5),
                Color::hex(0xb58a2f),
                Color::hex(0x6f7d8c),
                Color::hex(0xd96fa8),
                Color::hex(0x2f4b7c),
            ],
            title_size: 14.0,
            label_size: 11.0,
            value_size: 12.0,
        }
    }

    /// The colour of series `i`.
    pub fn series(&self, i: usize) -> Color {
        let n = self.palette.len();
        self.palette.get(i % n).copied().unwrap_or(self.text)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::light()
    }
}
