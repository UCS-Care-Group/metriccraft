//! The OpenDash BI window (layer 6, ADR 0008): egui panels over the engine. Thin: everything
//! the user does goes to the engine as a command ([`worker::Engine::run`]); panels only read the
//! published [`worker::Snapshot`]. Swappable: nothing below depends on it.

pub mod icons;
pub mod palette;
pub mod ribbon;
pub mod strings;
pub mod theme;
pub mod views;
pub mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Key, KeyboardShortcut, Modifiers, RichText, Ui};
use opendash_engine::{Command, RecoveryInfo};
use serde_json::{Value, json};

use crate::palette::{Action, Form, Palette};
use crate::ribbon::Tab;
use crate::strings::Str;
use crate::theme::{Mode, Tokens};
use crate::views::{Pane, View};
use crate::worker::{Engine, Event, Snapshot};

/// A message in the bottom-right corner; stays until dismissed (spec U-7).
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
}

/// Preferences kept per user, outside the Project (ADR 0008).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prefs {
    pub theme: Mode,
    pub view: View,
}

impl Prefs {
    pub fn to_json(self) -> Value {
        let theme = match self.theme {
            Mode::Light => "light",
            Mode::Dark => "dark",
        };
        json!({"theme": theme, "view": self.view.key()})
    }

    /// Unknown or missing members keep their defaults.
    pub fn from_json(v: &Value) -> Prefs {
        let theme = match v.get("theme").and_then(Value::as_str) {
            Some("dark") => Mode::Dark,
            _ => Mode::Light,
        };
        let view = v
            .get("view")
            .and_then(Value::as_str)
            .and_then(View::from_key)
            .unwrap_or_default();
        Prefs { theme, view }
    }
}

/// A native file dialog the shell asks the app for (the app owns the window it parents to).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileAsk {
    /// Pick a `.odash` Package or a Project's `project.odash.json`.
    Open,
    /// Pick where to save; a name ending `.odash` saves a Package, any other a Project folder.
    SaveAs { name: String },
}

/// Shows a dialog and answers through the receiver: the path, or `None` if cancelled.
pub type FileHook = Box<dyn Fn(FileAsk) -> std::sync::mpsc::Receiver<Option<PathBuf>>>;

/// Asking to save before the window closes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Closing {
    No,
    Asking,
    /// Saving; close when it succeeds.
    Saving,
}

/// The window.
pub struct Shell {
    engine: Engine,
    commands: &'static [Command],
    snap: Arc<Snapshot>,
    prefs: Prefs,
    tokens: Tokens,
    tab: Tab,
    palette: Option<Palette>,
    form: Option<Form>,
    toasts: Vec<Toast>,
    /// The last message, for the status bar.
    status: Option<String>,
    recovery: Option<(PathBuf, Vec<RecoveryInfo>)>,
    closing: Closing,
    /// Set when the window may close now; the app closes it.
    pub quit_requested: bool,
    dock: egui_dock::DockState<Pane>,
    /// The Table picked for the Table view.
    table: Option<String>,
    last_autosave: f64,
    file_hook: Option<FileHook>,
    /// A dialog shown for this command; its path becomes the command's `path`.
    picking: Option<(&'static Command, std::sync::mpsc::Receiver<Option<PathBuf>>)>,
}

impl Shell {
    /// A window over `engine`, offering the recovery copies `recovery` (from
    /// [`Engine::recoverable`]).
    pub fn new(
        engine: Engine,
        prefs: Prefs,
        recovery: (Option<PathBuf>, Vec<RecoveryInfo>),
    ) -> Self {
        let commands = engine.commands();
        let snap = engine.snapshot();
        let recovery = match recovery {
            (Some(dir), list) if !list.is_empty() => Some((dir, list)),
            _ => None,
        };
        let mut dock = egui_dock::DockState::new(vec![Pane::Visualizations, Pane::Data]);
        dock.main_surface_mut().split_above(
            egui_dock::NodeIndex::root(),
            0.35,
            vec![Pane::Filters],
        );
        Shell {
            engine,
            commands,
            snap,
            prefs,
            tokens: Tokens::get(prefs.theme),
            tab: Tab::Home,
            palette: None,
            form: None,
            toasts: Vec::new(),
            status: None,
            recovery,
            closing: Closing::No,
            quit_requested: false,
            dock,
            table: None,
            last_autosave: 0.0,
            file_hook: None,
            picking: None,
        }
    }

    /// Native file dialogs for Open and Save as; without them those commands use their forms.
    pub fn with_file_dialogs(mut self, hook: FileHook) -> Self {
        self.file_hook = Some(hook);
        self
    }

    /// Asks for a path with a native dialog if there is one for this command.
    fn ask_path(&mut self, c: &'static Command) -> bool {
        let ask = match c.id {
            "file.open" => FileAsk::Open,
            "file.saveAs" => {
                let name = match &self.snap.location {
                    Some(
                        opendash_engine::Location::Folder(p)
                        | opendash_engine::Location::Package(p),
                    ) => p.file_name().map(|n| n.to_string_lossy().into_owned()),
                    None => None,
                };
                FileAsk::SaveAs {
                    name: name.unwrap_or_else(|| Str::Untitled.text().to_string()),
                }
            }
            _ => return false,
        };
        let Some(hook) = &self.file_hook else {
            return false;
        };
        self.picking = Some((c, hook(ask)));
        true
    }

    pub fn prefs(&self) -> Prefs {
        self.prefs
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snap
    }

    pub fn toasts(&self) -> &[Toast] {
        &self.toasts
    }

    /// Sets up fonts and colours; call once with the window's context.
    pub fn setup(&self, ctx: &egui::Context) {
        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in [
            ("Inter", opendash_visuals::text::REGULAR),
            ("Inter Bold", opendash_visuals::text::BOLD),
        ] {
            fonts
                .font_data
                .insert(name.into(), Arc::new(egui::FontData::from_static(bytes)));
        }
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, "Inter".into());
        }
        ctx.set_fonts(fonts);
        ctx.set_visuals(self.tokens.visuals());
    }

    /// Switches to the dark App theme (scripts, tests).
    pub fn set_dark(&mut self, ctx: &egui::Context) {
        self.set_theme(ctx, Mode::Dark);
    }

    /// Picks a View by key (`report`, `table`, `model`); unknown keys are ignored.
    pub fn set_view(&mut self, key: &str) {
        if let Some(v) = View::from_key(key) {
            self.prefs.view = v;
        }
    }

    fn set_theme(&mut self, ctx: &egui::Context, mode: Mode) {
        self.prefs.theme = mode;
        self.tokens = Tokens::get(mode);
        ctx.set_visuals(self.tokens.visuals());
    }

    /// Runs a command, as a Ribbon button or the palette does.
    pub fn run(&mut self, id: &str, params: Value) {
        self.engine.run(id, params);
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::Run(c, params) => {
                self.form = None;
                self.palette = None;
                self.engine.run(c.id, params);
            }
            Action::Form(c) => {
                self.palette = None;
                if !self.ask_path(c) {
                    self.form = Some(Form::new(c));
                }
            }
            Action::Close => {
                self.palette = None;
                self.form = None;
            }
        }
    }

    /// The window's close button: `true` if it may close now; otherwise asks to save first.
    pub fn close_requested(&mut self) -> bool {
        if self.quit_requested || !self.snap.dirty {
            return true;
        }
        if self.closing == Closing::No {
            self.closing = Closing::Asking;
        }
        false
    }

    /// Everything that isn't drawing: engine events, shortcuts, autosave.
    pub fn logic(&mut self, ctx: &egui::Context) {
        self.snap = self.engine.snapshot();
        for event in self.engine.take_events() {
            match event {
                Event::Done { id, notice } => {
                    self.status = Some(self.label_of(&id).to_string());
                    if let Some(n) = notice {
                        self.toasts.push(Toast {
                            text: n,
                            error: false,
                        });
                    }
                    if self.closing == Closing::Saving && id.starts_with("file.save") {
                        self.quit_requested = true;
                    }
                }
                Event::Failed { id, error } => {
                    self.status = Some(error.clone());
                    self.toasts.push(Toast {
                        text: error,
                        error: true,
                    });
                    if self.closing == Closing::Saving && id.starts_with("file.save") {
                        self.closing = Closing::No;
                    }
                }
            }
        }
        if let Some((c, rx)) = &self.picking {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    let (id, path) = (c.id, path.display().to_string());
                    self.picking = None;
                    self.engine.run(id, json!({"path": path}));
                }
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.picking = None;
                    if self.closing == Closing::Saving {
                        self.closing = Closing::No;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.palette.is_none() && self.form.is_none() && self.picking.is_none() {
            self.shortcuts(ctx);
        }
        let now = ctx.input(|i| i.time);
        if self.snap.dirty {
            if now - self.last_autosave >= 1.0 {
                self.last_autosave = now;
                self.engine.autosave(now_ms());
            }
            ctx.request_repaint_after(std::time::Duration::from_secs(1));
        }
    }

    fn label_of(&self, id: &str) -> &'static str {
        self.commands
            .iter()
            .find(|c| c.id == id)
            .map_or("", |c| c.label)
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let palette = KeyboardShortcut::new(Modifiers::COMMAND, Key::K);
        if ctx.input_mut(|i| i.consume_shortcut(&palette)) {
            self.palette = Some(Palette::default());
            return;
        }
        for c in self.commands {
            let Some(shortcut) = c.shortcut.and_then(parse_shortcut) else {
                continue;
            };
            // Keys that act on a selection (F2, Delete) belong to the Views that have one.
            if c.params.iter().any(|p| p.required) {
                continue;
            }
            if self.snap.enabled(self.commands, c.id)
                && ctx.input_mut(|i| i.consume_shortcut(&shortcut))
            {
                self.engine.run(c.id, json!({}));
            }
        }
    }

    /// Draws the window.
    pub fn ui(&mut self, ui: &mut Ui) {
        let t = self.tokens;
        let snap = Arc::clone(&self.snap);
        let mut action: Option<Action> = None;

        egui::Panel::top("ribbon")
            .frame(
                egui::Frame::NONE
                    .fill(t.panel)
                    .inner_margin(egui::Margin::symmetric(8, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.menu_button(Str::TabFile.text(), |ui| {
                        for c in ribbon::file_menu(self.commands) {
                            if let Some(a) = ribbon::button(ui, &snap, self.commands, c) {
                                action = Some(a);
                                ui.close();
                            }
                        }
                    });
                    for id in ["edit.undo", "edit.redo"] {
                        if let Some(c) = self.commands.iter().find(|c| c.id == id)
                            && let Some(a) = ribbon::small(ui, &t, &snap, self.commands, c)
                        {
                            action = Some(a);
                        }
                    }
                    ui.separator();
                    let mut tabs: Vec<Tab> = Tab::MAIN.to_vec();
                    if self.prefs.view == View::Table {
                        tabs.push(Tab::ColumnTools);
                    }
                    for tab in tabs {
                        ui.selectable_value(&mut self.tab, tab, tab.label().text());
                    }
                });
                ui.add_space(4.0);
                if self.tab == Tab::View {
                    ui.horizontal(|ui| {
                        ui.label(Str::AppTheme.text());
                        let mut mode = self.prefs.theme;
                        ui.selectable_value(&mut mode, Mode::Light, Str::ThemeLight.text());
                        ui.selectable_value(&mut mode, Mode::Dark, Str::ThemeDark.text());
                        if mode != self.prefs.theme {
                            self.set_theme(ui.ctx(), mode);
                        }
                    });
                } else if let Some(a) = ribbon::show(ui, &t, &snap, self.commands, self.tab) {
                    action = Some(a);
                }
            });

        if let Some(a) = self.recovery_banner(ui, &t) {
            action = Some(a);
        }

        egui::Panel::bottom("status")
            .frame(
                egui::Frame::NONE
                    .fill(t.panel)
                    .inner_margin(egui::Margin::symmetric(8, 3)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let text = match (snap.busy, &self.status) {
                        (Some(id), _) => Str::Working.fill(&[self.label_of(id)]),
                        (None, Some(s)) => s.clone(),
                        (None, None) => Str::Ready.text().to_string(),
                    };
                    ui.label(RichText::new(text).color(t.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if snap.dirty {
                            ui.label(RichText::new(Str::Unsaved.text()).color(t.muted));
                        }
                        let pages = &snap.doc.report.pages;
                        if let Some(page) = views::current_page(&snap.doc) {
                            let n = pages.iter().position(|p| p.id == page.id).unwrap_or(0) + 1;
                            let label =
                                Str::PageNumber.fill(&[&n.to_string(), &pages.len().to_string()]);
                            ui.label(RichText::new(label).color(t.muted));
                        }
                    });
                });
            });

        egui::Panel::left("views")
            .resizable(false)
            .exact_size(84.0)
            .frame(egui::Frame::NONE.fill(t.panel).inner_margin(6))
            .show(ui, |ui| {
                for view in View::ALL {
                    let on = self.prefs.view == view;
                    let icon = match view {
                        View::Report => icons::Icon::Report,
                        View::Table => icons::Icon::Table,
                        View::Model => icons::Icon::Model,
                    };
                    let r = icons::big_button(ui, icon, view.label().text(), true, &t);
                    if on {
                        let bar = egui::Rect::from_min_size(
                            r.rect.left_top() - egui::vec2(5.0, 0.0),
                            egui::vec2(3.0, r.rect.height()),
                        );
                        ui.painter().rect_filled(bar, 1.0, t.accent);
                    }
                    if r.clicked() {
                        self.prefs.view = view;
                        if view != View::Table && self.tab == Tab::ColumnTools {
                            self.tab = Tab::Home;
                        }
                    }
                }
            });

        egui::Panel::right("panes")
            .default_size(280.0)
            .resizable(true)
            .frame(egui::Frame::NONE.fill(t.panel))
            .show(ui, |ui| {
                let mut viewer = PaneViewer {
                    t,
                    snap: &snap,
                    table: &mut self.table,
                };
                egui_dock::DockArea::new(&mut self.dock)
                    .id(egui::Id::new("panes-dock"))
                    .style(egui_dock::Style::from_egui(ui.style()))
                    .show_close_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_inside(ui, &mut viewer);
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(t.well))
            .show(ui, |ui| match self.prefs.view {
                View::Report => {
                    if let Some(a) = self.page_tabs(ui, &t, &snap) {
                        action = Some(a);
                    }
                    views::report_view(ui, &t, &snap.doc);
                }
                View::Table => views::table_view(ui, &t, &snap.doc, self.table.as_deref()),
                View::Model => views::model_view(ui, &t, &snap.doc),
            });

        self.show_toasts(ui, &t);

        if let Some(p) = &mut self.palette
            && let Some(a) = p.show(ui, &t, self.commands, &snap.enabled)
        {
            action = Some(a);
        }
        if let Some(f) = &mut self.form
            && let Some(a) = f.show(ui, &t, &snap.doc)
        {
            action = Some(a);
        }
        if self.closing == Closing::Asking {
            self.close_prompt(ui, &snap);
        }
        if let Some(a) = action {
            self.act(a);
        }
    }

    /// Page tabs above the canvas; clicking one makes it current, `+` inserts a Page.
    fn page_tabs(&mut self, ui: &mut Ui, t: &Tokens, snap: &Snapshot) -> Option<Action> {
        let mut action = None;
        egui::Panel::bottom("page-tabs")
            .frame(
                egui::Frame::NONE
                    .fill(t.panel)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let current = views::current_page(&snap.doc).map(|p| p.id.clone());
                    for page in &snap.doc.report.pages {
                        let on = current.as_deref() == Some(page.id.as_str());
                        if ui.selectable_label(on, page.name.as_str()).clicked() && !on {
                            self.engine.run("page.setCurrent", json!({"page": page.id}));
                        }
                    }
                    if let Some(c) = self.commands.iter().find(|c| c.id == "insert.newPage") {
                        let r = ui
                            .add_enabled(snap.enabled(self.commands, c.id), egui::Button::new("+"));
                        if r.on_hover_text(c.label).clicked() {
                            action = Some(palette::invoke(c));
                        }
                    }
                });
            });
        action
    }

    fn recovery_banner(&mut self, ui: &mut Ui, t: &Tokens) -> Option<Action> {
        let (dir, list) = self.recovery.as_ref()?;
        let info = list.first()?;
        let what = match &info.location {
            Some(opendash_engine::Location::Folder(p) | opendash_engine::Location::Package(p)) => {
                p.display().to_string()
            }
            None => Str::RecoveryUnsaved.text().to_string(),
        };
        let params = json!({"id": info.id, "dir": dir.display().to_string()});
        let usable = info.problem.is_none();
        let mut chosen: Option<&str> = None;
        egui::Panel::top("recovery")
            .frame(
                egui::Frame::NONE
                    .fill(t.hover)
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(Str::RecoveryFound.fill(&[&what]));
                    if usable && ui.button(Str::Recover.text()).clicked() {
                        chosen = Some("file.recover");
                    }
                    if ui.button(Str::Discard.text()).clicked() {
                        chosen = Some("file.discardRecovery");
                    }
                    if ui.button(Str::Dismiss.text()).clicked() {
                        chosen = Some("");
                    }
                });
            });
        let chosen = chosen?;
        if let Some((_, list)) = &mut self.recovery {
            list.remove(0);
            if list.is_empty() {
                self.recovery = None;
            }
        }
        let c = self.commands.iter().find(|c| c.id == chosen)?;
        Some(Action::Run(c, params))
    }

    fn show_toasts(&mut self, ui: &mut Ui, t: &Tokens) {
        let mut dismissed = None;
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -36.0])
            .show(ui.ctx(), |ui| {
                for (i, toast) in self.toasts.iter().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(360.0);
                        let color = if toast.error { t.danger } else { t.warning };
                        ui.label(RichText::new(toast.text.as_str()).color(color));
                        if ui.small_button(Str::Dismiss.text()).clicked() {
                            dismissed = Some(i);
                        }
                    });
                }
            });
        if let Some(i) = dismissed
            && i < self.toasts.len()
        {
            self.toasts.remove(i);
        }
    }

    fn close_prompt(&mut self, ui: &mut Ui, snap: &Snapshot) {
        let name = match &snap.location {
            Some(opendash_engine::Location::Folder(p) | opendash_engine::Location::Package(p)) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            None => Str::Untitled.text().to_string(),
        };
        let has_location = snap.location.is_some();
        let modal = egui::Modal::new(egui::Id::new("close")).show(ui.ctx(), |ui| {
            ui.label(Str::SaveFirst.fill(&[&name]));
            ui.horizontal(|ui| {
                if ui.button(Str::Save.text()).clicked() {
                    if has_location {
                        self.closing = Closing::Saving;
                        self.engine.run("file.save", json!({}));
                    } else if let Some(c) = self.commands.iter().find(|c| c.id == "file.saveAs") {
                        self.closing = Closing::Saving;
                        if !self.ask_path(c) {
                            self.form = Some(Form::new(c));
                        }
                    }
                }
                if ui.button(Str::DontSave.text()).clicked() {
                    self.quit_requested = true;
                }
                if ui.button(Str::Cancel.text()).clicked() {
                    self.closing = Closing::No;
                }
            });
        });
        if modal.should_close() && self.closing == Closing::Asking {
            self.closing = Closing::No;
        }
    }
}

/// Draws each Pane's content.
struct PaneViewer<'a> {
    t: Tokens,
    snap: &'a Snapshot,
    table: &'a mut Option<String>,
}

impl egui_dock::TabViewer for PaneViewer<'_> {
    type Tab = Pane;

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("pane", *tab as u8))
    }

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        tab.label().text().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Pane) {
        match tab {
            Pane::Data => views::data_pane(ui, &self.t, &self.snap.doc, self.table),
            Pane::Filters | Pane::Visualizations => {
                ui.label(RichText::new(Str::SelectVisual.text()).color(self.t.muted));
            }
        }
    }
}

/// `"Ctrl+Z"` as an egui shortcut (Ctrl is Cmd on macOS).
fn parse_shortcut(s: &str) -> Option<KeyboardShortcut> {
    let mut mods = Modifiers::NONE;
    let mut key = None;
    for part in s.split('+') {
        match part {
            "Ctrl" => mods |= Modifiers::COMMAND,
            "Shift" => mods |= Modifiers::SHIFT,
            "Alt" => mods |= Modifiers::ALT,
            k => key = Key::from_name(k),
        }
    }
    Some(KeyboardShortcut::new(mods, key?))
}

/// Milliseconds since 1970 (0 on the web, where autosave is off).
fn now_ms() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefs_round_trip_and_tolerate_junk() {
        let p = Prefs {
            theme: Mode::Dark,
            view: View::Model,
        };
        assert_eq!(Prefs::from_json(&p.to_json()), p);
        assert_eq!(
            Prefs::from_json(&json!({"theme": 3, "view": "nope"})),
            Prefs::default()
        );
        assert_eq!(Prefs::from_json(&Value::Null), Prefs::default());
    }

    #[test]
    fn shortcuts_parse() {
        let s = parse_shortcut("Ctrl+Z");
        assert_eq!(s, Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)));
        assert_eq!(parse_shortcut("F2").map(|s| s.logical_key), Some(Key::F2));
        assert_eq!(parse_shortcut("Ctrl+Nope"), None);
    }
}
