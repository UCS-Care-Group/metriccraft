//! The command palette (Ctrl+K, spec U-2) and the forms generated from a command's param schema
//! (spec C-3): how a command that needs params gets them from the user.

use egui::{Key, RichText, Ui};
use opendash_engine::param::{Kind, Param};
use opendash_engine::{Command, Document};
use serde_json::{Map, Value};

use crate::strings::Str;
use crate::theme::Tokens;

/// What the palette or a form asks the shell to do.
pub enum Action {
    /// Run this command with these params.
    Run(&'static Command, Value),
    /// Ask for this command's params.
    Form(&'static Command),
    Close,
}

/// The palette's state while it is open.
#[derive(Default)]
pub struct Palette {
    query: String,
    selected: usize,
}

/// Commands matching `query` (every word in the label, id or ribbon path, ignoring case).
pub fn matches<'a>(commands: &'a [Command], enabled: &[bool], query: &str) -> Vec<&'a Command> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    commands
        .iter()
        .enumerate()
        .filter(|(i, _)| enabled.get(*i).copied().unwrap_or(false))
        .map(|(_, c)| c)
        .filter(|c| {
            let hay = format!("{} {} {}", c.label, c.id, c.ribbon).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}

impl Palette {
    pub fn show(
        &mut self,
        ui: &mut Ui,
        t: &Tokens,
        commands: &'static [Command],
        enabled: &[bool],
    ) -> Option<Action> {
        let found = matches(commands, enabled, &self.query);
        self.selected = self.selected.min(found.len().saturating_sub(1));
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("palette")).show(ui.ctx(), |ui| {
            ui.set_width(520.0);
            let edit = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text(Str::PaletteHint.text())
                    .desired_width(f32::INFINITY),
            );
            edit.request_focus();
            let (down, up, enter) = ui.input(|i| {
                (
                    i.key_pressed(Key::ArrowDown),
                    i.key_pressed(Key::ArrowUp),
                    i.key_pressed(Key::Enter),
                )
            });
            if down {
                self.selected = (self.selected + 1).min(found.len().saturating_sub(1));
            }
            if up {
                self.selected = self.selected.saturating_sub(1);
            }
            ui.separator();
            if found.is_empty() {
                ui.label(RichText::new(Str::PaletteNone.text()).color(t.muted));
            }
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .show(ui, |ui| {
                    for (i, c) in found.iter().enumerate() {
                        let mut text = c.label.to_string();
                        if let Some(key) = c.shortcut {
                            text = format!("{text}    {key}");
                        }
                        let r = ui.selectable_label(i == self.selected, text);
                        let r = if c.ribbon.is_empty() {
                            r
                        } else {
                            r.on_hover_text(c.ribbon)
                        };
                        if i == self.selected {
                            r.scroll_to_me(None);
                        }
                        if r.clicked() {
                            action = Some(invoke(c));
                        }
                    }
                });
            if enter && let Some(c) = found.get(self.selected) {
                action = Some(invoke(c));
            }
        });
        if modal.should_close() && action.is_none() {
            action = Some(Action::Close);
        }
        action
    }
}

/// Runs a command that takes no required params; asks for them otherwise.
pub fn invoke(c: &'static Command) -> Action {
    if c.params.iter().any(|p| p.required) {
        Action::Form(c)
    } else {
        Action::Run(c, Value::Object(Map::new()))
    }
}

/// One param's editor state.
struct Field {
    param: &'static Param,
    /// Optional params are sent only when set.
    set: bool,
    text: String,
    on: bool,
    /// Values to pick from, instead of the document's names or a text box.
    choices: Option<Vec<String>>,
}

/// A form for one command's params.
pub struct Form {
    pub command: &'static Command,
    fields: Vec<Field>,
    /// Get Data from a workbook: its sheets and tables. Picking one as `item` sets `itemKind`.
    excel: Vec<(String, &'static str)>,
}

impl Form {
    pub fn new(command: &'static Command) -> Self {
        let fields = command
            .params
            .iter()
            .map(|p| Field {
                param: p,
                set: p.required,
                text: String::new(),
                on: false,
                choices: None,
            })
            .collect();
        Form {
            command,
            fields,
            excel: Vec::new(),
        }
    }

    /// Sets param `name` to `text` (and marks it given).
    pub fn with_value(mut self, name: &str, text: &str) -> Self {
        if let Some(f) = self.fields.iter_mut().find(|f| f.param.name == name) {
            f.text = text.to_string();
            f.set = true;
        }
        self
    }

    /// Get Data from a workbook: `item` picks among its sheets and tables (`itemKind` follows).
    pub fn with_excel_items(mut self, items: Vec<(String, &'static str)>) -> Self {
        let first = items.first().map(|(n, _)| n.clone()).unwrap_or_default();
        if let Some(f) = self.fields.iter_mut().find(|f| f.param.name == "item") {
            f.choices = Some(items.iter().map(|(n, _)| n.clone()).collect());
        }
        self.excel = items;
        self.with_value("item", &first)
    }

    /// The params as JSON, or the first field's problem.
    pub fn params(&self) -> Result<Value, String> {
        let mut m = Map::new();
        for f in self.fields.iter().filter(|f| f.set) {
            let p = f.param;
            let text = f.text.trim();
            let v = match p.kind {
                Kind::Bool => Value::Bool(f.on),
                _ if p.nullable && text.is_empty() => Value::Null,
                Kind::Integer => text
                    .parse::<i64>()
                    .map(Value::from)
                    .map_err(|_| format!("{}: a whole number", p.name))?,
                Kind::Number => text
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or_else(|| format!("{}: a number", p.name))?,
                Kind::Texts => Value::Array(
                    text.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(|s| Value::String(s.into()))
                        .collect(),
                ),
                Kind::Field | Kind::Json => serde_json::from_str(text)
                    .map_err(|e| format!("{}: not valid JSON ({e})", p.name))?,
                _ => Value::String(text.into()),
            };
            m.insert(p.name.into(), v);
        }
        if let Some(Value::String(item)) = m.get("item")
            && let Some((_, kind)) = self.excel.iter().find(|(n, _)| n == item)
        {
            m.insert("itemKind".into(), Value::String((*kind).into()));
        }
        let v = Value::Object(m);
        opendash_engine::param::validate(self.command.params, &v)?;
        Ok(v)
    }

    /// Draws the form; `Some` when the user runs or cancels it.
    pub fn show(&mut self, ui: &mut Ui, t: &Tokens, doc: &Document) -> Option<Action> {
        let mut action = None;
        let command = self.command;
        let modal = egui::Modal::new(egui::Id::new(("form", command.id))).show(ui.ctx(), |ui| {
            ui.set_width(440.0);
            ui.heading(command.label);
            ui.add_space(6.0);
            let table = self
                .fields
                .iter()
                .find(|f| matches!(f.param.kind, Kind::Table) && f.set)
                .map(|f| f.text.clone());
            let page = self
                .fields
                .iter()
                .find(|f| matches!(f.param.kind, Kind::Page) && f.set)
                .map(|f| f.text.clone());
            egui::Grid::new("form")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    for f in &mut self.fields {
                        let p = f.param;
                        if p.required {
                            ui.label(p.name);
                        } else {
                            ui.checkbox(&mut f.set, p.name)
                                .on_hover_text(Str::Optional.text());
                        }
                        ui.add_enabled_ui(f.set, |ui| {
                            editor(ui, f, doc, table.as_deref(), page.as_deref());
                        })
                        .response
                        .on_hover_text(p.doc);
                        ui.end_row();
                    }
                });
            ui.add_space(8.0);
            let problem = self.params().err();
            if let Some(problem) = &problem {
                ui.label(RichText::new(problem).color(t.muted).small());
            }
            ui.horizontal(|ui| {
                let run = ui.add_enabled(problem.is_none(), egui::Button::new(Str::Run.text()));
                let enter = problem.is_none() && ui.input(|i| i.key_pressed(Key::Enter));
                if (run.clicked() || enter)
                    && let Ok(v) = self.params()
                {
                    action = Some(Action::Run(command, v));
                }
                if ui.button(Str::Cancel.text()).clicked() {
                    action = Some(Action::Close);
                }
            });
        });
        if modal.should_close() && action.is_none() {
            action = Some(Action::Close);
        }
        action
    }
}

/// The editor for one field: a list of the document's names where the kind names something, a
/// checkbox for logical, a text box otherwise.
fn editor(ui: &mut Ui, f: &mut Field, doc: &Document, table: Option<&str>, page: Option<&str>) {
    let p = f.param;
    let names: Option<Vec<String>> = match p.kind {
        _ if f.choices.is_some() => f.choices.clone(),
        Kind::Choice(words) => Some(words.iter().map(|w| (*w).to_string()).collect()),
        Kind::Table => Some(doc.model.tables.iter().map(|t| t.name.clone()).collect()),
        Kind::Query => Some(doc.queries.iter().map(|q| q.name.clone()).collect()),
        Kind::Page => Some(doc.report.pages.iter().map(|pg| pg.name.clone()).collect()),
        Kind::Column | Kind::Measure => table.and_then(|name| {
            let t = doc.model.tables.iter().find(|t| t.name == name)?;
            let measures = t.measures.iter().map(|m| m.name.clone());
            Some(if matches!(p.kind, Kind::Measure) {
                measures.collect()
            } else {
                t.columns
                    .iter()
                    .map(|c| c.name.clone())
                    .chain(measures)
                    .collect()
            })
        }),
        Kind::Visual => page.and_then(|key| {
            let pg = doc.report.page(key)?;
            Some(pg.visuals.iter().map(|v| v.id.clone()).collect())
        }),
        _ => None,
    };
    match (p.kind, names) {
        (Kind::Bool, _) => {
            ui.checkbox(&mut f.on, "");
        }
        (_, Some(names)) if !names.is_empty() => {
            egui::ComboBox::from_id_salt(("param", p.name))
                .selected_text(f.text.as_str())
                .width(260.0)
                .show_ui(ui, |ui| {
                    for n in names {
                        ui.selectable_value(&mut f.text, n.clone(), n);
                    }
                });
        }
        _ => {
            let multiline = matches!(p.kind, Kind::Json | Kind::Field);
            let edit = if multiline {
                egui::TextEdit::multiline(&mut f.text).desired_rows(3)
            } else {
                egui::TextEdit::singleline(&mut f.text)
            };
            ui.add(edit.desired_width(260.0).hint_text(p.kind.word()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opendash_engine::COMMANDS;

    fn command(id: &str) -> &'static Command {
        COMMANDS.iter().find(|c| c.id == id).unwrap_or(&COMMANDS[0])
    }

    #[test]
    fn picking_a_workbook_table_sets_item_kind() {
        let items = vec![
            ("Sheet1".to_string(), "sheet"),
            ("Sales".to_string(), "table"),
        ];
        let form = Form::new(command("home.getData"))
            .with_value("kind", "xlsx")
            .with_value("path", "/b.xlsx")
            .with_excel_items(items);
        let v = form.params().unwrap_or_default();
        assert_eq!(v["item"], "Sheet1");
        assert_eq!(v["itemKind"], "sheet");
        let form = form.with_value("item", "Sales");
        assert_eq!(form.params().unwrap_or_default()["itemKind"], "table");
    }

    #[test]
    fn palette_matches_every_word_and_skips_disabled() {
        let all = vec![true; COMMANDS.len()];
        let found = matches(COMMANDS, &all, "new page");
        assert!(found.iter().any(|c| c.id == "insert.newPage"), "by label");
        assert!(
            matches(COMMANDS, &all, "INSERT.NEWPAGE").len() == 1,
            "by id, any case"
        );
        let none = vec![false; COMMANDS.len()];
        assert!(matches(COMMANDS, &none, "").is_empty());
    }

    #[test]
    fn forms_build_params_of_each_kind_and_validate() {
        let mut form = Form::new(command("insert.newVisual"));
        assert!(form.params().is_err(), "type is required");
        for f in &mut form.fields {
            match f.param.name {
                "type" => f.text = "card".into(),
                "x" => (f.set, f.text) = (true, "12".into()),
                _ => {}
            }
        }
        assert_eq!(
            form.params(),
            Ok(serde_json::json!({"type": "card", "x": 12}))
        );
        for f in &mut form.fields {
            if f.param.name == "x" {
                f.text = "1.5".into();
            }
        }
        assert_eq!(form.params(), Err("x: a whole number".into()));

        let mut form = Form::new(command("transform.changeType"));
        for f in &mut form.fields {
            f.text = match f.param.name {
                "query" => "Sales".into(),
                "columns" => " A, B ,".into(),
                _ => "whole".into(),
            };
        }
        let v = form.params().unwrap_or_default();
        assert_eq!(v["columns"], serde_json::json!(["A", "B"]));

        let mut form = Form::new(command("columnTools.sortByColumn"));
        for f in &mut form.fields {
            f.text = match f.param.name {
                "by" => String::new(),
                _ => "X".into(),
            };
        }
        let v = form.params().unwrap_or_default();
        assert_eq!(v["by"], Value::Null, "an empty nullable param is null");
    }

    #[test]
    fn commands_without_required_params_run_straight_away() {
        assert!(matches!(invoke(command("insert.newPage")), Action::Run(..)));
        assert!(matches!(invoke(command("page.rename")), Action::Form(..)));
    }
}
