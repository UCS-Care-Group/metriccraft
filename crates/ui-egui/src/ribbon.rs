//! The Ribbon (CONTEXT.md): tabs of command buttons, built from each command's `ribbon` path
//! (`"Home > Data"` is the Data group of the Home tab). Every button runs a command.

use egui::{RichText, Ui};
use metriccraft_engine::Command;

use crate::palette::{Action, invoke};
use crate::strings::Str;
use crate::theme::Tokens;
use crate::worker::Snapshot;

/// The main window's tabs, in order. File is a menu, not a tab; the Transform tabs belong to the
/// Power Query editor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tab {
    #[default]
    Home,
    Insert,
    Modeling,
    View,
    Help,
    /// Contextual: shown in Table view.
    ColumnTools,
}

impl Tab {
    pub const MAIN: [Tab; 5] = [Tab::Home, Tab::Insert, Tab::Modeling, Tab::View, Tab::Help];

    pub fn label(self) -> Str {
        match self {
            Tab::Home => Str::TabHome,
            Tab::Insert => Str::TabInsert,
            Tab::Modeling => Str::TabModeling,
            Tab::View => Str::TabView,
            Tab::Help => Str::TabHelp,
            Tab::ColumnTools => Str::TabColumnTools,
        }
    }

    /// The tab name as commands' `ribbon` paths spell it.
    fn path_name(self) -> &'static str {
        match self {
            Tab::Home => "Home",
            Tab::Insert => "Insert",
            Tab::Modeling => "Modeling",
            Tab::View => "View",
            Tab::Help => "Help",
            Tab::ColumnTools => "Column tools",
        }
    }
}

/// The tab's groups, in command-table order, each with its commands.
pub fn groups(
    commands: &'static [Command],
    tab: Tab,
) -> Vec<(&'static str, Vec<&'static Command>)> {
    groups_at(commands, tab.path_name())
}

/// The groups of the tab its commands' `ribbon` paths call `tab` (the Power Query editor's
/// tabs are `"Power Query Editor: Home"` and `"Transform"`).
pub fn groups_at(
    commands: &'static [Command],
    tab: &str,
) -> Vec<(&'static str, Vec<&'static Command>)> {
    let mut out: Vec<(&'static str, Vec<&'static Command>)> = Vec::new();
    for c in commands {
        let Some((t, group)) = c.ribbon.split_once(" > ") else {
            continue;
        };
        if t != tab {
            continue;
        }
        match out.iter_mut().find(|(g, _)| *g == group) {
            Some((_, list)) => list.push(c),
            None => out.push((group, vec![c])),
        }
    }
    out
}

/// The commands of the File menu.
pub fn file_menu(commands: &'static [Command]) -> impl Iterator<Item = &'static Command> {
    commands.iter().filter(|c| c.ribbon == "File")
}

/// A Ribbon button: icon above label (label only if the command has no icon).
pub fn big(
    ui: &mut Ui,
    t: &Tokens,
    snap: &Snapshot,
    commands: &[Command],
    c: &'static Command,
) -> Option<Action> {
    let Some(icon) = crate::icons::Icon::for_command(c.id) else {
        return button(ui, snap, commands, c);
    };
    let enabled = snap.enabled(commands, c.id);
    let mut r = crate::icons::big_button(ui, icon, c.label, enabled, t);
    if let Some(key) = c.shortcut {
        r = r.on_hover_text(Str::Shortcut.fill(&[key]));
    }
    r.clicked().then(|| invoke(c))
}

/// An icon-only button (the quick-access bar), labelled with the command and its shortcut.
pub fn small(
    ui: &mut Ui,
    t: &Tokens,
    snap: &Snapshot,
    commands: &[Command],
    c: &'static Command,
) -> Option<Action> {
    let Some(icon) = crate::icons::Icon::for_command(c.id) else {
        return button(ui, snap, commands, c);
    };
    let enabled = snap.enabled(commands, c.id);
    let r = crate::icons::icon_button(ui, icon, c.label, enabled, t);
    let r = match c.shortcut {
        Some(key) => r.on_hover_text(Str::Shortcut.fill(&[key])),
        None => r,
    };
    r.clicked().then(|| invoke(c))
}

/// One button: the command's label, greyed out when it can't run now.
pub fn button(
    ui: &mut Ui,
    snap: &Snapshot,
    commands: &[Command],
    c: &'static Command,
) -> Option<Action> {
    let enabled = snap.enabled(commands, c.id);
    let mut r = ui.add_enabled(
        enabled,
        egui::Button::new(c.label).min_size(egui::vec2(0.0, 28.0)),
    );
    if let Some(key) = c.shortcut {
        r = r.on_hover_text(Str::Shortcut.fill(&[key]));
    }
    r.clicked().then(|| invoke(c))
}

/// The groups of `tab` as rows of buttons with the group name beneath; with key tips from
/// letter number `tips` on (see [`crate::keys`]).
pub fn show(
    ui: &mut Ui,
    t: &Tokens,
    snap: &Snapshot,
    commands: &'static [Command],
    tab: Tab,
    tips: Option<usize>,
) -> Option<Action> {
    show_groups(ui, t, snap, commands, groups(commands, tab), tips)
}

/// The buttons of `groups`, each group labelled under its buttons; key tips as for [`show`].
pub fn show_groups(
    ui: &mut Ui,
    t: &Tokens,
    snap: &Snapshot,
    commands: &'static [Command],
    groups: Vec<(&'static str, Vec<&'static Command>)>,
    tips: Option<usize>,
) -> Option<Action> {
    let mut action = None;
    let mut n = tips.unwrap_or_default();
    ui.horizontal_top(|ui| {
        for (group, list) in groups {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    for c in list {
                        let r = ui.scope(|ui| big(ui, t, snap, commands, c));
                        if let Some(a) = r.inner {
                            action = Some(a);
                        }
                        if tips.is_some()
                            && let Some(letter) = crate::keys::button_letter(n)
                        {
                            crate::keys::badge(ui, r.response.rect, letter, t);
                        }
                        n += 1;
                    }
                });
                ui.label(RichText::new(group).small().color(t.muted));
            });
            ui.separator();
        }
    });
    action
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriccraft_engine::COMMANDS;

    #[test]
    fn groups_follow_ribbon_paths() {
        let home = groups(COMMANDS, Tab::Home);
        let names: Vec<&str> = home.iter().map(|(g, _)| *g).collect();
        assert!(
            names.contains(&"Data") && names.contains(&"Queries"),
            "{names:?}"
        );
        let insert = groups(COMMANDS, Tab::Insert);
        assert!(
            insert
                .iter()
                .any(|(_, l)| l.iter().any(|c| c.id == "insert.newPage"))
        );
        // Power Query editor commands stay out of the main window's tabs.
        for tab in Tab::MAIN {
            for (_, list) in groups(COMMANDS, tab) {
                assert!(list.iter().all(|c| !c.ribbon.starts_with("Power Query")));
            }
        }
        assert!(file_menu(COMMANDS).any(|c| c.id == "file.save"));
    }
}
