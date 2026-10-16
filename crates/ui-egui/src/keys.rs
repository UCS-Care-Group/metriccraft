//! Keyboard navigation beyond Tab: F6 / Shift+F6 cycle focus Ribbon → View →
//! Panes, and key tips: Alt pressed and released alone shows a letter on each Ribbon tab;
//! typing one opens that tab and shows a letter on each of its buttons; typing one of those runs
//! it. Esc steps back.

use egui::{Align2, Event, FontId, Id, Key, Modifiers, Rect, Sense, Ui, WidgetInfo, WidgetType};

use crate::ribbon::Tab;
use crate::theme::Tokens;

/// The regions F6 cycles through, in order.
pub const REGIONS: usize = 3;

/// The focus anchor of region `n`: where F6 puts focus; Tab goes on from there.
pub fn region_id(n: usize) -> Id {
    Id::new(("focus-region", n % REGIONS))
}

/// Places region `n`'s focus anchor (an invisible focusable point labelled `label`) at the
/// cursor.
pub fn anchor(ui: &mut Ui, n: usize, label: &str) {
    let at = ui.cursor().min;
    let r = ui.interact(
        Rect::from_min_size(at, egui::Vec2::ZERO),
        region_id(n),
        Sense::FOCUSABLE,
    );
    r.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, label));
}

/// Whether F6 (`Some(true)`) or Shift+F6 (`Some(false)`) was pressed; consumes it.
pub fn f6(ctx: &egui::Context) -> Option<bool> {
    ctx.input_mut(|i| {
        if i.consume_key(Modifiers::SHIFT, Key::F6) {
            Some(false)
        } else if i.consume_key(Modifiers::NONE, Key::F6) {
            Some(true)
        } else {
            None
        }
    })
}

/// The key tip letter of a main-window tab (Power BI's where it has one).
pub fn tab_letter(tab: Tab) -> char {
    match tab {
        Tab::Home => 'H',
        Tab::Insert => 'N',
        Tab::Modeling => 'M',
        Tab::View => 'W',
        Tab::Help => 'E',
        Tab::ColumnTools => 'J',
    }
}

/// The key tip letter of a tab's `n`th button (A, B, … Z; none past Z).
pub fn button_letter(n: usize) -> Option<char> {
    let n = u8::try_from(n).ok().filter(|n| *n < 26)?;
    Some(char::from(b'A' + n))
}

/// What key tips show now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Tabs,
    Buttons(Tab),
}

/// A key tip typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tip {
    Tab(Tab),
    /// The `n`th button of the tab shown.
    Button(Tab, usize),
}

#[derive(Default)]
pub struct KeyTips {
    pub level: Option<Level>,
    alt_down: bool,
    /// Something else was pressed while Alt was down (Alt+F4, Alt+Tab): no key tips then.
    alt_used: bool,
}

impl KeyTips {
    /// Reads this frame's keys (consuming the letters key tips take); `tabs` are the tabs
    /// shown. Returns the tip typed, if any.
    pub fn input(&mut self, ctx: &egui::Context, tabs: &[Tab]) -> Option<Tip> {
        let alt = ctx.input(|i| i.modifiers.alt);
        let pressed = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(
                    e,
                    Event::Key { pressed: true, .. } | Event::PointerButton { .. }
                )
            })
        });
        match (self.alt_down, alt) {
            (false, true) => {
                self.alt_down = true;
                self.alt_used = pressed;
            }
            (true, true) => self.alt_used |= pressed,
            (true, false) => {
                self.alt_down = false;
                if !self.alt_used && !pressed {
                    self.level = match self.level {
                        None => Some(Level::Tabs),
                        Some(_) => None,
                    };
                }
            }
            (false, false) => {}
        }
        let level = self.level?;
        if ctx.input(|i| i.pointer.any_pressed()) {
            self.level = None;
            return None;
        }
        // The first plain key press: a letter (or Esc) is the key tips'.
        let key = ctx.input_mut(|i| {
            let at = i.events.iter().position(|e| {
                matches!(e, Event::Key { pressed: true, modifiers, .. } if !modifiers.alt && !modifiers.command)
            })?;
            match i.events.get(at) {
                Some(Event::Key { key, .. }) => {
                    let key = *key;
                    i.events.remove(at);
                    // The text the letter would type goes too.
                    i.events.retain(|e| !matches!(e, Event::Text(_)));
                    Some(key)
                }
                _ => None,
            }
        })?;
        if key == Key::Escape {
            self.level = match level {
                Level::Tabs => None,
                Level::Buttons(_) => Some(Level::Tabs),
            };
            return None;
        }
        let letter = key
            .name()
            .chars()
            .next()
            .filter(|_| key.name().len() == 1)?;
        match level {
            Level::Tabs => {
                let tab = tabs.iter().copied().find(|t| tab_letter(*t) == letter)?;
                self.level = Some(Level::Buttons(tab));
                Some(Tip::Tab(tab))
            }
            Level::Buttons(tab) => {
                let n = usize::from(u8::try_from(letter).ok()?.checked_sub(b'A')?);
                self.level = None;
                Some(Tip::Button(tab, n))
            }
        }
    }
}

/// Draws key tip `letter` on the control `rect`: by an icon button's icon, under a text one.
pub fn badge(ui: &Ui, rect: Rect, letter: char, t: &Tokens) {
    let at = if rect.height() > 40.0 {
        rect.center_top() + egui::vec2(14.0, 26.0)
    } else {
        rect.center_bottom()
    };
    let r = Rect::from_center_size(at, egui::vec2(16.0, 16.0));
    let p = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        Id::new("key-tips"),
    ));
    p.rect_filled(r, 2.0, t.text);
    p.text(
        r.center(),
        Align2::CENTER_CENTER,
        letter,
        FontId::proportional(11.0),
        t.base,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_are_distinct_and_bounded() {
        let mut seen: Vec<char> = Tab::MAIN.iter().map(|t| tab_letter(*t)).collect();
        seen.push(tab_letter(Tab::ColumnTools));
        let n = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), n);
        assert_eq!(button_letter(0), Some('A'));
        assert_eq!(button_letter(25), Some('Z'));
        assert_eq!(button_letter(26), None);
    }
}
