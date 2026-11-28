//! The MetricCraft window (layer 6): egui panels over the engine. Thin: everything
//! the user does goes to the engine as a command ([`worker::Engine::run`]); panels only read the
//! published [`worker::Snapshot`]. Swappable: nothing below depends on it.

pub mod canvas;
pub mod diagram;
pub mod formula;
pub mod grid;
pub mod i18n;
pub mod icons;
pub mod keys;
pub mod painter;
pub mod palette;
pub mod panes;
pub mod pq;
pub mod ribbon;
pub mod strings;
pub mod theme;
pub mod views;
pub mod worker;

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Key, KeyboardShortcut, Modifiers, RichText, Ui};
use metriccraft_engine::{Command, RecoveryInfo};
use serde_json::{Value, json};

use crate::i18n::Lang;
use crate::palette::{Action, Form, Palette};
use crate::ribbon::Tab;
use crate::strings::Str;
use crate::theme::{Mode, Tokens};
use crate::views::{Pane, View};
use crate::worker::{Engine, Event, Snapshot};

/// A message in the bottom-right corner; stays until dismissed.
#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    pub error: bool,
}

/// Preferences kept per user, outside the Project.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Prefs {
    pub theme: Mode,
    pub view: View,
    pub language: Lang,
}

impl Prefs {
    pub fn to_json(self) -> Value {
        let theme = match self.theme {
            Mode::Light => "light",
            Mode::Dark => "dark",
        };
        json!({"theme": theme, "view": self.view.key(), "language": self.language.code()})
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
        let language = v
            .get("language")
            .and_then(Value::as_str)
            .and_then(Lang::from_tag)
            .filter(|l| *l != Lang::Hebrew || Lang::hebrew_font_available())
            .unwrap_or_default();
        Prefs {
            theme,
            view,
            language,
        }
    }
}

/// A native file dialog the shell asks the app for (the app owns the window it parents to).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileAsk {
    /// Pick a `.mcraft` Package or a Project's `project.mcraft.json`.
    Open,
    /// Pick where to save; a name ending `.mcraft` saves a Package, any other a Project folder.
    SaveAs { name: String },
    /// Pick a CSV, Excel or Parquet file to get data from.
    Data,
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

/// Last finished-command state shown in the status bar (resolved per frame so language switches update it).
#[derive(Clone, Debug, PartialEq, Eq)]
enum Status {
    Done(String),
    Failed(String),
}

/// The commands that load data, with progress and Cancel.
const LOADS: [&str; 3] = ["home.getData", "home.refresh", "home.closeApply"];

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
    /// The last status-bar message source, resolved in the active language each frame.
    status: Option<Status>,
    recovery: Option<(PathBuf, Vec<RecoveryInfo>)>,
    closing: Closing,
    /// Set when the window may close now; the app closes it.
    pub quit_requested: bool,
    dock: egui_dock::DockState<Pane>,
    /// The Table picked for the Table view.
    table: Option<String>,
    canvas: canvas::Canvas,
    grid: grid::Grid,
    diagram: diagram::Diagram,
    /// The formula bar, while a Measure or Calculated column is edited.
    formula: Option<formula::Formula>,
    /// Focus the formula bar next frame.
    focus_formula: bool,
    /// The Power Query editor.
    pq: pq::QueryEditor,
    /// The open form belongs to the Power Query editor's window.
    form_in_pq: bool,
    keytips: keys::KeyTips,
    /// Fonts and colours set up ([`Shell::setup`]); the first frame does it if the app didn't.
    ready: bool,
    /// The region F6 last moved focus to.
    region: Option<usize>,
    last_autosave: f64,
    file_hook: Option<FileHook>,
    /// A dialog shown for this command; its path becomes the command's `path`.
    picking: Option<(&'static Command, std::sync::mpsc::Receiver<Option<PathBuf>>)>,
    /// A workbook's sheets and tables being listed for Get Data: its path and the answer.
    listing: Option<(
        String,
        std::sync::mpsc::Receiver<Result<ExcelItems, String>>,
    )>,
}

type ExcelItems = Vec<(String, &'static str)>;

impl Shell {
    /// A window over `engine`, offering the recovery copies `recovery` (from
    /// [`Engine::recoverable`]).
    pub fn new(
        engine: Engine,
        prefs: Prefs,
        recovery: (Option<PathBuf>, Vec<RecoveryInfo>),
    ) -> Self {
        i18n::set_language(prefs.language);
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
            canvas: canvas::Canvas::default(),
            grid: grid::Grid::default(),
            diagram: diagram::Diagram::default(),
            formula: None,
            focus_formula: false,
            pq: pq::QueryEditor::default(),
            form_in_pq: false,
            keytips: keys::KeyTips::default(),
            ready: false,
            region: None,
            last_autosave: 0.0,
            file_hook: None,
            picking: None,
            listing: None,
        }
    }

    /// Get Data's form for the file at `path`: its kind from the extension; a workbook's sheets
    /// and tables are listed first (off the window's thread, where there are threads).
    fn get_data_form(&mut self, c: &'static Command, path: String) {
        let ext = std::path::Path::new(&path)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let kind = match ext.as_str() {
            "csv" | "txt" | "tsv" => "csv",
            "xlsx" => "xlsx",
            "parquet" => "parquet",
            _ => "",
        };
        if kind != "xlsx" {
            let form = Form::new(c).with_value("path", &path);
            self.form = Some(if kind.is_empty() {
                form
            } else {
                form.with_value("kind", kind)
            });
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let list = {
            let path = path.clone();
            move || {
                let items = metriccraft_engine::excel_items(&path).map_err(|e| e.to_string());
                let _ = tx.send(items);
            }
        };
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::spawn(list);
        #[cfg(target_arch = "wasm32")]
        list();
        self.listing = Some((path, rx));
    }

    /// Native file dialogs for Open, Save as and Get Data; without them those commands use their forms.
    pub fn with_file_dialogs(mut self, hook: FileHook) -> Self {
        self.file_hook = Some(hook);
        self
    }

    /// Asks for a path with a native dialog if there is one for this command.
    fn ask_path(&mut self, c: &'static Command) -> bool {
        let ask = match c.id {
            "file.open" => FileAsk::Open,
            "home.getData" => FileAsk::Data,
            "file.saveAs" => {
                let name = match &self.snap.location {
                    Some(
                        metriccraft_engine::Location::Folder(p)
                        | metriccraft_engine::Location::Package(p),
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

    /// Sets up fonts and colours; call once with the window's context (the first
    /// [`Shell::logic`] does, if not).
    pub fn setup(&mut self, ctx: &egui::Context) {
        self.ready = true;
        let mut fonts = egui::FontDefinitions::default();
        for (name, bytes) in [
            ("Inter", metriccraft_visuals::text::REGULAR),
            ("Inter Bold", metriccraft_visuals::text::BOLD),
        ] {
            fonts
                .font_data
                .insert(name.into(), Arc::new(egui::FontData::from_static(bytes)));
        }
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.insert(0, "Inter".into());
        }
        // The canvas paints Scenes in the Inter faces their layout measured with.
        for name in [painter::REGULAR, painter::BOLD] {
            fonts
                .families
                .insert(egui::FontFamily::Name(name.into()), vec![name.into()]);
        }
        #[cfg(not(target_arch = "wasm32"))]
        install_hebrew_fallback_fonts(&mut fonts);
        ctx.set_fonts(fonts);
        ctx.set_visuals(self.tokens.visuals());
    }

    /// Switches to the dark App theme (scripts, tests).
    pub fn set_dark(&mut self, ctx: &egui::Context) {
        self.set_theme(ctx, Mode::Dark);
    }

    /// Switches UI language (`en` / `he`) and updates the active translation state.
    pub fn set_language(&mut self, lang: Lang) {
        let resolved = if lang == Lang::Hebrew && !Lang::hebrew_font_available() {
            Lang::English
        } else {
            lang
        };
        self.prefs.language = resolved;
        i18n::set_language(resolved);
    }

    /// Picks a View by key (`report`, `table`, `model`); unknown keys are ignored.
    pub fn set_view(&mut self, key: &str) {
        if let Some(v) = View::from_key(key) {
            self.prefs.view = v;
        }
    }

    pub fn zoom(&self) -> canvas::Zoom {
        self.canvas.zoom
    }

    /// Opens the Power Query editor (scripts, tests), at Query `query` if given.
    pub fn open_query_editor(&mut self, query: Option<&str>) {
        self.pq.open = true;
        if let Some(q) = query {
            self.pq.query = Some(q.to_string());
            self.pq.selected = None;
        }
    }

    pub fn query_editor_open(&self) -> bool {
        self.pq.open
    }

    /// The Table view's grid.
    pub fn grid(&self) -> &grid::Grid {
        &self.grid
    }

    fn open_formula(&mut self, f: formula::Formula) {
        self.formula = Some(f);
        self.focus_formula = true;
    }

    /// The canvas's selected Visual.
    pub fn selected(&self) -> Option<&str> {
        self.canvas.selected.as_deref()
    }

    /// Selects Visual `id` on the canvas (scripts, tests).
    pub fn select(&mut self, id: &str) {
        self.canvas.selected = Some(id.to_string());
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

    /// Invokes a command as a Ribbon button does: its form (or panel) if it needs params.
    pub fn invoke(&mut self, id: &str) {
        if let Some(c) = self.commands.iter().find(|c| c.id == id) {
            self.act(palette::invoke(c));
        }
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
                let kind = match c.id {
                    "modeling.newMeasure" => Some(formula::Kind::Measure),
                    "modeling.newColumn" => Some(formula::Kind::Column),
                    _ => None,
                };
                let table = self
                    .table
                    .clone()
                    .or_else(|| self.snap.doc.model.tables.first().map(|t| t.name.clone()));
                if let (Some(kind), Some(table)) = (kind, table) {
                    self.open_formula(formula::Formula::new(kind, &table));
                } else if !self.ask_path(c) {
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
        if !self.ready {
            // Painting a Scene before its fonts are registered would panic in egui.
            self.setup(ctx);
        }
        self.snap = self.engine.snapshot();
        for event in self.engine.take_events() {
            match event {
                Event::Done { id, notice } => {
                    self.status = Some(Status::Done(id.clone()));
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
                    self.status = Some(Status::Failed(error.clone()));
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
        if let Some((path, rx)) = &self.listing
            && let Ok(answer) = rx.try_recv()
        {
            let path = path.clone();
            self.listing = None;
            match (
                answer,
                self.commands.iter().find(|c| c.id == "home.getData"),
            ) {
                (Ok(items), Some(c)) => {
                    let form = Form::new(c)
                        .with_value("kind", "xlsx")
                        .with_value("path", &path);
                    self.form = Some(form.with_excel_items(items));
                }
                (Err(error), _) => self.toasts.push(Toast {
                    text: error,
                    error: true,
                }),
                (Ok(_), None) => {}
            }
        }
        if let Some((c, rx)) = &self.picking {
            match rx.try_recv() {
                Ok(Some(path)) => {
                    let (c, path) = (*c, path.display().to_string());
                    self.picking = None;
                    if c.id == "home.getData" {
                        self.get_data_form(c, path);
                    } else {
                        self.engine.run(c.id, json!({"path": path}));
                    }
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
            if let Some(forward) = keys::f6(ctx) {
                let n = match (self.region, forward) {
                    (None, _) => 0,
                    (Some(r), true) => (r + 1) % keys::REGIONS,
                    (Some(r), false) => (r + keys::REGIONS - 1) % keys::REGIONS,
                };
                self.region = Some(n);
                ctx.memory_mut(|m| m.request_focus(keys::region_id(n)));
            }
            match self.keytips.input(ctx, &self.tabs()) {
                Some(keys::Tip::Tab(tab)) => self.tab = tab,
                Some(keys::Tip::Button(tab, n)) => self.tip_button(tab, n),
                None => {}
            }
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

    /// The main window's tabs shown now.
    fn tabs(&self) -> Vec<Tab> {
        let mut tabs: Vec<Tab> = Tab::MAIN.to_vec();
        if self.prefs.view == View::Table {
            tabs.push(Tab::ColumnTools);
        }
        tabs
    }

    /// Runs the `n`th button of `tab`, as its key tip does. Home's first is Transform data.
    fn tip_button(&mut self, tab: Tab, n: usize) {
        let n = match (tab, n) {
            (Tab::Home, 0) => {
                self.pq.open = true;
                return;
            }
            (Tab::Home, n) => n - 1,
            (_, n) => n,
        };
        let c = ribbon::groups(self.commands, tab)
            .into_iter()
            .flat_map(|(_, list)| list)
            .nth(n);
        if let Some(c) = c.filter(|c| self.snap.enabled(self.commands, c.id)) {
            self.act(palette::invoke(c));
        }
    }

    fn label_of_for(&self, lang: Lang, id: &str) -> &'static str {
        self.commands
            .iter()
            .find(|c| c.id == id)
            .map_or("", |c| i18n::command_label_for(lang, c))
    }

    fn display_of_for(&self, lang: Lang, id: &str) -> String {
        self.commands
            .iter()
            .find(|c| c.id == id)
            .map_or_else(String::new, |c| i18n::command_display_for(lang, c))
    }

    fn status_text_for(&self, lang: Lang, busy: Option<&str>) -> (String, String) {
        match (busy, &self.status) {
            (Some(id), _) => (
                Str::Working.fill_for(lang, &[self.label_of_for(lang, id)]),
                Str::Working.fill_logical_for(lang, &[self.label_of_for(lang, id)]),
            ),
            (None, Some(Status::Done(id))) => (
                self.display_of_for(lang, id),
                self.label_of_for(lang, id).to_owned(),
            ),
            (None, Some(Status::Failed(error))) => (i18n::visual_rtl(error), error.clone()),
            (None, None) => (
                Str::Ready.show_for(lang),
                Str::Ready.text_for(lang).to_owned(),
            ),
        }
    }

    fn status_text(&self, busy: Option<&str>) -> (String, String) {
        self.status_text_for(i18n::language(), busy)
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
                keys::anchor(ui, 0, Str::RegionRibbon.text());
                ui.horizontal(|ui| {
                    ui.menu_button(Str::TabFile.show(), |ui| {
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
                    for tab in self.tabs() {
                        let r = icons::a11y_label(
                            ui.selectable_value(&mut self.tab, tab, tab.label().show()),
                            egui::WidgetType::SelectableLabel,
                            tab.label().text(),
                        );
                        if self.keytips.level == Some(keys::Level::Tabs) {
                            keys::badge(ui, r.rect, keys::tab_letter(tab), &t);
                        }
                    }
                });
                ui.add_space(4.0);
                if self.tab == Tab::View {
                    ui.horizontal(|ui| {
                        i18n::str_label(ui, Str::AppTheme, |r| r);
                        let mut mode = self.prefs.theme;
                        icons::a11y_label(
                            ui.selectable_value(&mut mode, Mode::Light, Str::ThemeLight.show()),
                            egui::WidgetType::SelectableLabel,
                            Str::ThemeLight.text(),
                        );
                        icons::a11y_label(
                            ui.selectable_value(&mut mode, Mode::Dark, Str::ThemeDark.show()),
                            egui::WidgetType::SelectableLabel,
                            Str::ThemeDark.text(),
                        );
                        if mode != self.prefs.theme {
                            self.set_theme(ui.ctx(), mode);
                        }
                        ui.separator();
                        i18n::str_label(ui, Str::Language, |r| r);
                        let mut lang = self.prefs.language;
                        for &choice in Lang::available() {
                            icons::a11y_label(
                                ui.selectable_value(
                                    &mut lang,
                                    choice,
                                    choice.display_name().show(),
                                ),
                                egui::WidgetType::SelectableLabel,
                                choice.display_name().text(),
                            );
                        }
                        if lang != self.prefs.language {
                            self.set_language(lang);
                        }
                    });
                } else {
                    ui.horizontal_top(|ui| {
                        let tips = self.keytips.level == Some(keys::Level::Buttons(self.tab));
                        let mut first = 0;
                        if self.tab == Tab::Home {
                            let icon = icons::Icon::Table;
                            let label = Str::TransformData.show();
                            ui.vertical(|ui| {
                                let r = icons::big_button_labeled(
                                    ui,
                                    icon,
                                    &label,
                                    Str::TransformData.text(),
                                    true,
                                    &t,
                                );
                                if r.clicked() {
                                    self.pq.open = true;
                                }
                                if tips {
                                    keys::badge(ui, r.rect, 'A', &t);
                                }
                                i18n::str_label(ui, Str::PowerQueryGroup, |r| {
                                    r.small().color(t.muted)
                                });
                            });
                            ui.separator();
                            first = 1;
                        }
                        let tips = tips.then_some(first);
                        if let Some(a) = ribbon::show(ui, &t, &snap, self.commands, self.tab, tips)
                        {
                            action = Some(a);
                        }
                    });
                }
            });

        if let Some(f) = &mut self.formula {
            let outcome = egui::Panel::top("formula-bar")
                .frame(
                    egui::Frame::NONE
                        .fill(t.panel)
                        .inner_margin(egui::Margin::symmetric(8, 4)),
                )
                .show(ui, |ui| f.show(ui, &t))
                .inner;
            if std::mem::take(&mut self.focus_formula) {
                ui.memory_mut(|m| m.request_focus(formula::id()));
            }
            match outcome {
                formula::Outcome::Editing => {}
                formula::Outcome::Cancel => self.formula = None,
                formula::Outcome::Commit(commands) => {
                    self.formula = None;
                    for (id, params) in commands {
                        self.engine.run(id, params);
                    }
                }
            }
        }

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
                    let (shown, logical) = self.status_text(snap.busy);
                    icons::a11y_label(
                        ui.label(RichText::new(shown).color(t.muted)),
                        egui::WidgetType::Label,
                        &logical,
                    );
                    if snap.busy.is_some_and(|id| LOADS.contains(&id)) {
                        // Rows read so far, per Query; polled while the load runs.
                        for (query, rows) in self.engine.load_rows() {
                            let n = metriccraft_core::format(
                                &metriccraft_core::Value::Integer(
                                    i64::try_from(rows).unwrap_or(i64::MAX),
                                ),
                                "#,0",
                                &metriccraft_core::EN_US,
                            )
                            .unwrap_or_default();
                            icons::a11y_label(
                                ui.label(
                                    RichText::new(Str::RowsRead.fill(&[&query, &n])).color(t.muted),
                                ),
                                egui::WidgetType::Label,
                                &Str::RowsRead.fill_logical(&[&query, &n]),
                            );
                        }
                        if icons::a11y_label(
                            ui.button(Str::Cancel.show()),
                            egui::WidgetType::Button,
                            Str::Cancel.text(),
                        )
                        .clicked()
                        {
                            self.engine.cancel_load();
                        }
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(100));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if snap.dirty {
                            i18n::str_label(ui, Str::Unsaved, |r| r.color(t.muted));
                        }
                        // What Undo would undo, with its Origin (`Undo: MCP — 30 changes`).
                        if let Some(undo) = &snap.undo {
                            icons::a11y_label(
                                ui.label(
                                    RichText::new(Str::UndoLabel.fill(&[undo])).color(t.muted),
                                ),
                                egui::WidgetType::Label,
                                &Str::UndoLabel.fill_logical(&[undo]),
                            );
                        }
                        let pages = &snap.doc.report.pages;
                        if let Some(page) = views::current_page(&snap.doc) {
                            let n = pages.iter().position(|p| p.id == page.id).unwrap_or(0) + 1;
                            let n_s = n.to_string();
                            let total_s = pages.len().to_string();
                            let label = Str::PageNumber.fill(&[&n_s, &total_s]);
                            let logical = Str::PageNumber.fill_logical(&[&n_s, &total_s]);
                            icons::a11y_label(
                                ui.label(RichText::new(label).color(t.muted)),
                                egui::WidgetType::Label,
                                &logical,
                            );
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
                    let r = icons::big_button_labeled(
                        ui,
                        icon,
                        &view.label().show(),
                        view.label().text(),
                        true,
                        &t,
                    );
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
                keys::anchor(ui, 2, Str::RegionPanes.text());
                let mut viewer = PaneViewer {
                    t,
                    snap: &snap,
                    table: &mut self.table,
                    selected: self.canvas.selected.as_deref(),
                    out: Vec::new(),
                    open: None,
                };
                egui_dock::DockArea::new(&mut self.dock)
                    .id(egui::Id::new("panes-dock"))
                    .style(egui_dock::Style::from_egui(ui.style()))
                    .show_close_buttons(false)
                    .show_leaf_close_all_buttons(false)
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, &mut viewer);
                let open = viewer.open.take();
                for (id, params) in viewer.out {
                    self.engine.run(id, params);
                }
                if let Some(f) = open {
                    self.open_formula(f);
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(t.well))
            .show(ui, |ui| {
                keys::anchor(ui, 1, self.prefs.view.label().text());
                self.view(ui, &t, &snap, &mut action);
            });

        self.show_toasts(ui, &t);

        if let Some(p) = &mut self.palette
            && let Some(a) = p.show(ui, &t, self.commands, &snap.enabled)
        {
            action = Some(a);
        }
        if self.pq.open {
            self.query_editor(ui, &t, &snap);
        }
        if !self.form_in_pq
            && let Some(f) = &mut self.form
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

    /// The View picked on the rail.
    fn view(&mut self, ui: &mut Ui, t: &Tokens, snap: &Snapshot, action: &mut Option<Action>) {
        match self.prefs.view {
            View::Report => {
                if let Some(a) = self.page_tabs(ui, t, snap) {
                    *action = Some(a);
                }
                self.report(ui, t, snap);
            }
            View::Table => self.grid.show(ui, t, &snap.doc, self.table.as_deref()),
            View::Model => {
                for (id, params) in self.diagram.show(ui, t, &snap.doc) {
                    self.engine.run(id, params);
                }
            }
        }
    }

    /// The Report view: the canvas over the current Page.
    fn report(&mut self, ui: &mut Ui, t: &Tokens, snap: &Snapshot) {
        let Some(page) = views::current_page(&snap.doc) else {
            views::report_view(ui, t, &snap.doc);
            return;
        };
        let (drawn, scenes) = &*snap.scenes;
        let scenes = if drawn == &page.id {
            scenes.as_slice()
        } else {
            &[]
        };
        for r in self.canvas.show(ui, t, page, scenes) {
            match r {
                canvas::Request::Command(id, params) => self.engine.run(id, params),
                canvas::Request::Click { visual, x, y, add } => {
                    self.engine.click(&page.id, &visual, x, y, add)
                }
            }
        }
    }

    /// The Power Query editor in its own window (embedded where there is one window only).
    fn query_editor(&mut self, ui: &mut Ui, t: &Tokens, snap: &Snapshot) {
        let id = egui::ViewportId::from_hash_of("power-query-editor");
        let builder = egui::ViewportBuilder::default()
            .with_title(Str::QueryEditor.text())
            .with_inner_size([1100.0, 700.0]);
        let preview = self.engine.preview();
        let (requests, form_action, close) =
            ui.ctx().show_viewport_immediate(id, builder, |ui, _| {
                let mut close = ui.input(|i| i.viewport().close_requested());
                egui::Panel::top("pq-close").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            close |= icons::a11y_label(
                                ui.button(Str::Close.show()),
                                egui::WidgetType::Button,
                                Str::Close.text(),
                            )
                            .clicked();
                        });
                    });
                });
                let requests = self.pq.show(ui, t, snap, self.commands, preview.as_deref());
                let mut form_action = None;
                if self.form_in_pq
                    && let Some(f) = &mut self.form
                {
                    form_action = f.show(ui, t, &snap.doc);
                }
                (requests, form_action, close)
            });
        for r in requests {
            match r {
                pq::Request::Run(id, params) => {
                    if id == "home.closeApply" {
                        self.pq.open = false;
                    }
                    self.engine.run(id, params);
                }
                pq::Request::Form(f) => {
                    self.form = Some(f);
                    self.form_in_pq = true;
                }
                pq::Request::Preview(q, step) => self.engine.request_preview(&q, step),
            }
        }
        if let Some(a) = form_action {
            self.form_in_pq = false;
            self.act(a);
        }
        if close {
            self.pq.open = false;
            if self.form_in_pq {
                self.form = None;
                self.form_in_pq = false;
            }
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
                        let shown = i18n::visual_rtl(&page.name);
                        if icons::a11y_label(
                            ui.selectable_label(on, shown),
                            egui::WidgetType::SelectableLabel,
                            &page.name,
                        )
                        .clicked()
                            && !on
                        {
                            self.engine.run("page.setCurrent", json!({"page": page.id}));
                        }
                    }
                    if let Some(c) = self.commands.iter().find(|c| c.id == "insert.newPage") {
                        let r = ui
                            .add_enabled(snap.enabled(self.commands, c.id), egui::Button::new("+"));
                        if icons::name_labeled(
                            r,
                            egui::WidgetType::Button,
                            &i18n::command_display(c),
                            i18n::command_label(c),
                        )
                        .clicked()
                        {
                            action = Some(palette::invoke(c));
                        }
                    }
                    if current.is_some() {
                        let layout = egui::Layout::right_to_left(egui::Align::Center);
                        ui.with_layout(layout, |ui| self.zoom_controls(ui));
                    }
                });
            });
        action
    }

    /// Fit page, fit width and a zoom slider (10 % to 400 %).
    fn zoom_controls(&mut self, ui: &mut Ui) {
        use canvas::Zoom;
        let mut pct = (self.canvas.last_scale * 100.0).clamp(10.0, 400.0).round();
        // Rounded and in range, so the slider sees no change of its own making.
        let z = &mut self.canvas.zoom;
        let slider = egui::Slider::new(&mut pct, 10.0..=400.0).show_value(false);
        let r = icons::name(ui.add(slider), egui::WidgetType::Slider, Str::Zoom.text());
        if r.changed() {
            *z = Zoom::Fixed(pct / 100.0);
        }
        let pct = format!("{pct:.0}");
        ui.label(Str::Percent.fill(&[&pct]));
        if icons::a11y_label(
            ui.selectable_label(*z == Zoom::FitWidth, Str::FitWidth.show()),
            egui::WidgetType::SelectableLabel,
            Str::FitWidth.text(),
        )
        .clicked()
        {
            *z = Zoom::FitWidth;
        }
        if icons::a11y_label(
            ui.selectable_label(*z == Zoom::FitPage, Str::FitPage.show()),
            egui::WidgetType::SelectableLabel,
            Str::FitPage.text(),
        )
        .clicked()
        {
            *z = Zoom::FitPage;
        }
    }

    fn recovery_banner(&mut self, ui: &mut Ui, t: &Tokens) -> Option<Action> {
        let (dir, list) = self.recovery.as_ref()?;
        let info = list.first()?;
        let what = match &info.location {
            Some(
                metriccraft_engine::Location::Folder(p) | metriccraft_engine::Location::Package(p),
            ) => p.display().to_string(),
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
                    if usable
                        && icons::a11y_label(
                            ui.button(Str::Recover.show()),
                            egui::WidgetType::Button,
                            Str::Recover.text(),
                        )
                        .clicked()
                    {
                        chosen = Some("file.recover");
                    }
                    if icons::a11y_label(
                        ui.button(Str::Discard.show()),
                        egui::WidgetType::Button,
                        Str::Discard.text(),
                    )
                    .clicked()
                    {
                        chosen = Some("file.discardRecovery");
                    }
                    if icons::a11y_label(
                        ui.button(Str::Dismiss.show()),
                        egui::WidgetType::Button,
                        Str::Dismiss.text(),
                    )
                    .clicked()
                    {
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
                        if icons::a11y_label(
                            ui.small_button(Str::Dismiss.show()),
                            egui::WidgetType::Button,
                            Str::Dismiss.text(),
                        )
                        .clicked()
                        {
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
            Some(
                metriccraft_engine::Location::Folder(p) | metriccraft_engine::Location::Package(p),
            ) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            None => Str::Untitled.text().to_string(),
        };
        let has_location = snap.location.is_some();
        let modal = egui::Modal::new(egui::Id::new("close")).show(ui.ctx(), |ui| {
            i18n::wrapped_label(ui, &Str::SaveFirst.fill_logical(&[&name]), |r| r);
            ui.horizontal(|ui| {
                if icons::a11y_label(
                    ui.button(Str::Save.show()),
                    egui::WidgetType::Button,
                    Str::Save.text(),
                )
                .clicked()
                {
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
                if icons::a11y_label(
                    ui.button(Str::DontSave.show()),
                    egui::WidgetType::Button,
                    Str::DontSave.text(),
                )
                .clicked()
                {
                    self.quit_requested = true;
                }
                if icons::a11y_label(
                    ui.button(Str::Cancel.show()),
                    egui::WidgetType::Button,
                    Str::Cancel.text(),
                )
                .clicked()
                {
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
    /// The canvas's selected Visual.
    selected: Option<&'a str>,
    out: Vec<panes::Request>,
    /// A Measure or Calculated column clicked: edit it in the formula bar.
    open: Option<formula::Formula>,
}

impl egui_dock::TabViewer for PaneViewer<'_> {
    type Tab = Pane;

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("pane", *tab as u8))
    }

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        tab.label().show().into()
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Pane) {
        let page = views::current_page(&self.snap.doc);
        let visual = page.and_then(|p| {
            let id = self.selected?;
            p.visuals.iter().find(|v| v.id == id)
        });
        match tab {
            Pane::Data => {
                let field = views::data_pane(ui, &self.t, &self.snap.doc, self.table);
                self.open = field.as_ref().and_then(|f| formula_for(&self.snap.doc, f));
                if let (Some(f), Some(p), Some(v)) = (field, page, visual)
                    && let Some(well) = panes::well_for(v, &f)
                {
                    let params = json!({"page": p.id, "visual": v.id, "well": well.id(), "field": panes::field_json(&f)});
                    self.out.push(("visual.addField", params));
                }
            }
            Pane::Visualizations => {
                let r = panes::visualizations(ui, &self.t, page, visual);
                self.out.extend(r);
            }
            Pane::Filters => {
                let r = panes::filters(ui, &self.t, page);
                self.out.extend(r);
            }
        }
    }
}

/// The formula bar for Field `f` if it is a Measure or a Calculated column.
fn formula_for(
    doc: &metriccraft_engine::Document,
    f: &metriccraft_visuals::FieldRef,
) -> Option<formula::Formula> {
    use metriccraft_visuals::FieldRef;
    match f {
        FieldRef::Measure { name } => {
            let (t, m) = doc.model.measure(name)?;
            Some(formula::Formula::edit(
                formula::Kind::Measure,
                &t.name,
                &m.name,
                &m.expression,
            ))
        }
        FieldRef::Column { table, column } => {
            let t = doc.model.tables.iter().find(|t| &t.name == table)?;
            let c = t.columns.iter().find(|c| &c.name == column)?;
            let expr = c.expression.as_deref()?;
            Some(formula::Formula::edit(
                formula::Kind::Column,
                table,
                column,
                expr,
            ))
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

/// Registers the first valid desktop system Hebrew fallback font on every UI font family without
/// committing or embedding external binary font assets. Web (`wasm32`) builds exclude Hebrew in
/// `Lang::available()` so the web UI never renders missing-glyph boxes.
#[cfg(not(target_arch = "wasm32"))]
fn read_be_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let slice = bytes.get(offset..offset + 2)?;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

#[cfg(not(target_arch = "wasm32"))]
fn read_be_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset + 4)?;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

#[cfg(not(target_arch = "wasm32"))]
fn has_valid_sfnt_at(bytes: &[u8], offset: usize) -> bool {
    let Some(magic) = bytes.get(offset..offset + 4) else {
        return false;
    };
    if !matches!(magic, [0x00, 0x01, 0x00, 0x00] | b"OTTO" | b"true") {
        return false;
    }
    let Some(num_tables) = read_be_u16(bytes, offset + 4).map(usize::from) else {
        return false;
    };
    if num_tables == 0 {
        return false;
    }
    let Some(dir_end) = offset
        .checked_add(12)
        .and_then(|s| s.checked_add(num_tables * 16))
    else {
        return false;
    };
    if dir_end > bytes.len() {
        return false;
    }
    let mut has_valid_head = false;
    for i in 0..num_tables {
        let rec = offset + 12 + i * 16;
        let Some(tag) = bytes.get(rec..rec + 4) else {
            return false;
        };
        let Some(tbl_off) = read_be_u32(bytes, rec + 8).and_then(|v| usize::try_from(v).ok())
        else {
            return false;
        };
        let Some(tbl_len) = read_be_u32(bytes, rec + 12).and_then(|v| usize::try_from(v).ok())
        else {
            return false;
        };
        let Some(tbl_end) = tbl_off.checked_add(tbl_len) else {
            return false;
        };
        if tbl_end > bytes.len() {
            return false;
        }
        if tag == b"head" {
            if tbl_len < 54 || read_be_u32(bytes, tbl_off + 12) != Some(0x5F0F_3CF5) {
                return false;
            }
            has_valid_head = true;
        }
    }
    has_valid_head
}

#[cfg(not(target_arch = "wasm32"))]
fn has_valid_sfnt_structure(bytes: &[u8]) -> bool {
    if bytes.get(..4) == Some(b"ttcf") {
        let Some(num_fonts) = read_be_u32(bytes, 8).and_then(|v| usize::try_from(v).ok()) else {
            return false;
        };
        if num_fonts == 0 {
            return false;
        }
        let Some(first_offset) = read_be_u32(bytes, 12).and_then(|v| usize::try_from(v).ok())
        else {
            return false;
        };
        return has_valid_sfnt_at(bytes, first_offset);
    }
    has_valid_sfnt_at(bytes, 0)
}

#[cfg(not(target_arch = "wasm32"))]
fn is_valid_font_bytes(bytes: &[u8]) -> bool {
    if !has_valid_sfnt_structure(bytes) {
        return false;
    }
    let mut defs = egui::FontDefinitions::empty();
    defs.font_data.insert(
        "hebrew-probe".to_owned(),
        Arc::new(egui::FontData::from_owned(bytes.to_vec())),
    );
    // Parse the full font through epaint's loader so truncated/corrupt faces are skipped instead
    // of panicking during startup FontDefinitions initialization.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ =
            egui::epaint::text::FontsImpl::new(egui::epaint::text::TextOptions::default(), defs);
    }))
    .is_ok()
}

#[cfg(not(target_arch = "wasm32"))]
fn read_valid_font(path: &std::path::Path) -> Option<Arc<egui::FontData>> {
    let bytes = std::fs::read(path).ok()?;
    if !is_valid_font_bytes(&bytes) {
        eprintln!(
            "note: skipping invalid Hebrew fallback font file: {}",
            path.display()
        );
        return None;
    }
    Some(Arc::new(egui::FontData::from_owned(bytes)))
}

#[cfg(not(target_arch = "wasm32"))]
fn load_hebrew_fallback_font(
    candidates: &[(&'static str, &str)],
) -> Option<(&'static str, Arc<egui::FontData>)> {
    for env_var in ["METRICCRAFT_FONT_DIR", "CRAFT_FONTS_DIR"] {
        if let Ok(dir) = std::env::var(env_var) {
            let trimmed = dir.trim();
            if !trimmed.is_empty() {
                for filename in [
                    "NotoSansHebrew-Regular.ttf",
                    "NotoSansHebrew[wdth,wght].ttf",
                ] {
                    let candidate = std::path::Path::new(trimmed).join(filename);
                    if let Some(data) = read_valid_font(&candidate) {
                        return Some(("Noto Sans Hebrew Env", data));
                    }
                }
            }
        }
    }
    candidates.iter().find_map(|&(name, path)| {
        read_valid_font(std::path::Path::new(path)).map(|data| (name, data))
    })
}

#[cfg(not(target_arch = "wasm32"))]
fn hebrew_fallback_font() -> Option<&'static (&'static str, Arc<egui::FontData>)> {
    static FONT: std::sync::OnceLock<Option<(&'static str, Arc<egui::FontData>)>> =
        std::sync::OnceLock::new();
    FONT.get_or_init(|| {
        const CANDIDATES: [(&str, &str); 13] = [
            (
                "Arial Hebrew",
                "/System/Library/Fonts/Supplemental/Arial Hebrew.ttf",
            ),
            ("SF Hebrew", "/System/Library/Fonts/SFHebrew.ttf"),
            ("Arial Hebrew Local", "/Library/Fonts/Arial Hebrew.ttf"),
            (
                "Arial Unicode",
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
            ),
            (
                "Noto Sans Hebrew",
                "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
            ),
            (
                "Noto Sans Hebrew Arch",
                "/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf",
            ),
            (
                "Noto Sans Hebrew Nix",
                "/run/current-system/sw/share/fonts/truetype/NotoSansHebrew-Regular.ttf",
            ),
            (
                "FreeSans",
                "/usr/share/fonts/truetype/freefont/FreeSans.ttf",
            ),
            ("FreeSans Arch", "/usr/share/fonts/TTF/FreeSans.ttf"),
            ("FreeSans GNU", "/usr/share/fonts/gnu-free/FreeSans.ttf"),
            ("Arial", "C:\\Windows\\Fonts\\arial.ttf"),
            ("Segoe UI", "C:\\Windows\\Fonts\\segoeui.ttf"),
            ("Tahoma", "C:\\Windows\\Fonts\\tahoma.ttf"),
        ];
        let loaded = load_hebrew_fallback_font(&CANDIDATES);
        if loaded.is_none() {
            eprintln!(
                "note: no Hebrew fallback font found (checked METRICCRAFT_FONT_DIR, CRAFT_FONTS_DIR, and system font paths); Hebrew UI disabled"
            );
        }
        loaded
    })
    .as_ref()
}

pub(crate) fn hebrew_font_available() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        hebrew_fallback_font().is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn install_hebrew_fallback_fonts(fonts: &mut egui::FontDefinitions) -> bool {
    let Some((name, data)) = hebrew_fallback_font() else {
        return false;
    };
    if !fonts.font_data.contains_key(*name) {
        fonts.font_data.insert((*name).into(), Arc::clone(data));
        append_font_fallback(fonts, name);
    }
    true
}

#[cfg(not(target_arch = "wasm32"))]
fn append_font_fallback(fonts: &mut egui::FontDefinitions, name: &str) {
    for family in [
        egui::FontFamily::Proportional,
        egui::FontFamily::Monospace,
        egui::FontFamily::Name(painter::REGULAR.into()),
        egui::FontFamily::Name(painter::BOLD.into()),
    ] {
        if let Some(list) = fonts.families.get_mut(&family)
            && !list.iter().any(|existing| existing == name)
        {
            list.push(name.to_owned());
        }
    }
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
            language: Lang::Hebrew,
        };
        let expected_lang = if Lang::hebrew_font_available() {
            Lang::Hebrew
        } else {
            Lang::English
        };
        let parsed = Prefs::from_json(&p.to_json());
        assert_eq!(parsed.theme, p.theme);
        assert_eq!(parsed.view, p.view);
        assert_eq!(parsed.language, expected_lang);
        assert_eq!(
            Prefs::from_json(&json!({"language": "iw"})).language,
            expected_lang
        );
        assert_eq!(
            Prefs::from_json(&json!({"theme": 3, "view": "nope", "language": "x-he"})),
            Prefs::default()
        );
        assert_eq!(Prefs::from_json(&Value::Null), Prefs::default());
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn hebrew_font_validation_skips_missing_and_invalid_files() {
        assert!(is_valid_font_bytes(metriccraft_visuals::text::REGULAR));
        assert!(!is_valid_font_bytes(b""));
        assert!(!is_valid_font_bytes(b"not a font"));
        // Truncated SFNT header / truncated real font must be rejected so epaint cannot panic.
        assert!(!is_valid_font_bytes(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x01]));
        let truncated_real =
            &metriccraft_visuals::text::REGULAR[..metriccraft_visuals::text::REGULAR.len() / 2];
        assert!(!is_valid_font_bytes(truncated_real));
        // Structurally valid SFNT directory pointing at corrupt `head` bytes must also be rejected.
        let mut corrupt_sfnt = vec![
            0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x10, 0x00, 0x00, 0x00, 0x00,
        ];
        corrupt_sfnt.extend_from_slice(b"head");
        corrupt_sfnt.extend_from_slice(&[0u8; 4]);
        corrupt_sfnt.extend_from_slice(&28u32.to_be_bytes());
        corrupt_sfnt.extend_from_slice(&4u32.to_be_bytes());
        corrupt_sfnt.extend_from_slice(&[0xFF; 4]);
        assert!(!is_valid_font_bytes(&corrupt_sfnt));

        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!("metriccraft-bogus-font-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp font dir");
        let bogus = dir.join("bogus.ttf");
        let valid = dir.join("valid.ttf");
        let second = dir.join("second.ttf");
        std::fs::write(&bogus, [0x00, 0x01, 0x00, 0x00, 0x00, 0x01]).expect("write bogus font");
        std::fs::write(&valid, metriccraft_visuals::text::REGULAR).expect("write valid font");
        std::fs::write(&second, metriccraft_visuals::text::REGULAR).expect("write second font");
        let missing = "/nonexistent/path/metriccraft-missing.ttf";
        let bogus_str = bogus.to_string_lossy().into_owned();
        let valid_str = valid.to_string_lossy().into_owned();
        let second_str = second.to_string_lossy().into_owned();
        let loaded = load_hebrew_fallback_font(&[
            ("Missing", missing),
            ("Bogus", &bogus_str),
            ("Valid", &valid_str),
            ("Second", &second_str),
        ]);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            loaded.as_ref().map(|(name, _)| *name),
            Some("Valid"),
            "missing and truncated candidates must be skipped and only the first valid candidate kept"
        );
    }

    #[test]
    fn shortcuts_parse() {
        let s = parse_shortcut("Ctrl+Z");
        assert_eq!(s, Some(KeyboardShortcut::new(Modifiers::COMMAND, Key::Z)));
        assert_eq!(parse_shortcut("F2").map(|s| s.logical_key), Some(Key::F2));
        assert_eq!(parse_shortcut("Ctrl+Nope"), None);
    }

    #[test]
    fn status_bar_updates_when_language_switches() {
        let mut shell = Shell::new(
            Engine::new(metriccraft_engine::App::new(), || {}),
            Prefs::default(),
            (None, Vec::new()),
        );
        let save_id = "file.save";
        let save_cmd = shell
            .commands
            .iter()
            .find(|c| c.id == save_id)
            .expect("file.save registered");
        let save_en = i18n::command_label_for(Lang::English, save_cmd).to_owned();
        let save_he_logical = i18n::command_label_for(Lang::Hebrew, save_cmd);
        let save_he_visual = i18n::command_display_for(Lang::Hebrew, save_cmd);

        shell.status = Some(Status::Done(save_id.to_owned()));
        assert_eq!(
            shell.status_text_for(Lang::English, None),
            (save_en.clone(), save_en)
        );
        assert_eq!(
            shell.status_text_for(Lang::Hebrew, None),
            (save_he_visual, save_he_logical.to_owned())
        );

        let err = "שגיאת בדיקה";
        shell.status = Some(Status::Failed(err.to_owned()));
        assert_eq!(
            shell.status_text_for(Lang::Hebrew, None),
            (i18n::visual_rtl(err), err.to_owned())
        );
        assert_eq!(
            shell.status_text_for(Lang::English, None),
            (i18n::visual_rtl(err), err.to_owned())
        );
    }
}
