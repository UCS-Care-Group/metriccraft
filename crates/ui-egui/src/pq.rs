//! The Power Query editor: its own window on desktop (a window
//! inside the app where there is only one, on the web). Queries on the left, the Preview of the
//! selected Applied Step in the middle, Applied Steps on the right, the Query's M below. Its
//! Ribbon runs the transform commands on the selected Query (and column).

use egui::text::{LayoutJob, TextFormat};
use egui::{FontId, RichText, Ui};
use metriccraft_engine::Command;
use serde_json::{Value, json};

use crate::grid::Grid;
use crate::palette::{Action, Form};
use crate::strings::Str;
use crate::theme::Tokens;
use crate::worker::{PreviewOut, Snapshot};

/// The editor's Ribbon tabs, as commands' `ribbon` paths name them.
const TABS: [(Str, &str); 2] = [
    (Str::TabHome, "Power Query Editor: Home"),
    (Str::TabTransform, "Transform"),
];

/// What the editor asks the window to do.
pub enum Request {
    Run(&'static str, Value),
    /// Show this form (its params filled in from the selection).
    Form(Form),
    /// Make this Preview.
    Preview(String, Option<usize>),
}

#[derive(Default)]
pub struct QueryEditor {
    pub open: bool,
    /// The selected Query.
    pub query: Option<String>,
    /// The selected entry of Applied Steps: 0 is Source, `k` the `k`th step; `None` follows
    /// the last step.
    pub selected: Option<usize>,
    tab: usize,
    grid: Grid,
    /// The Preview last asked for, and after which snapshot.
    asked: Option<(String, Option<usize>, u64)>,
}

/// Applied Steps' names as Power Query shows them: a repeated name gets a number (`Changed
/// Type1`).
pub fn step_names(steps: &[metriccraft_engine::Step]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(steps.len());
    for s in steps {
        let name = s.name();
        let seen = out
            .iter()
            .filter(|n| {
                n.as_str() == name
                    || n.strip_prefix(name)
                        .is_some_and(|r| r.parse::<u32>().is_ok())
            })
            .count();
        out.push(if seen == 0 {
            name.to_string()
        } else {
            format!("{name}{seen}")
        });
    }
    out
}

impl QueryEditor {
    /// The editor's contents; `preview` is the last Preview made.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        t: &Tokens,
        snap: &Snapshot,
        commands: &'static [Command],
        preview: Option<&PreviewOut>,
    ) -> Vec<Request> {
        let mut out = Vec::new();
        let doc = &snap.doc;
        if self
            .query
            .as_ref()
            .is_none_or(|q| !doc.queries.iter().any(|x| &x.name == q))
        {
            self.query = doc.queries.first().map(|q| q.name.clone());
            self.selected = None;
        }
        let query = self
            .query
            .as_deref()
            .and_then(|n| doc.queries.iter().find(|q| q.name == n));
        let steps = query.map(|q| q.steps.as_slice()).unwrap_or_default();
        let selected = self
            .selected
            .filter(|s| *s <= steps.len())
            .unwrap_or(steps.len());
        // The Preview's step: `None` is the Source.
        let at = selected.checked_sub(1);

        egui::Panel::top("pq-ribbon").show(ui, |ui| {
            ui.horizontal(|ui| {
                for (i, (label, _)) in TABS.iter().enumerate() {
                    if ui.selectable_label(self.tab == i, label.text()).clicked() {
                        self.tab = i;
                    }
                }
            });
            let path = TABS.get(self.tab).map_or("", |(_, p)| p);
            let groups = crate::ribbon::groups_at(commands, path);
            if let Some(a) = crate::ribbon::show_groups(ui, t, snap, commands, groups, None) {
                out.push(match a {
                    Action::Run(c, params) => Request::Run(c.id, params),
                    Action::Form(c) => Request::Form(self.form(c)),
                    Action::Close => return,
                });
            }
        });

        egui::Panel::left("pq-queries")
            .default_size(180.0)
            .show(ui, |ui| {
                ui.label(RichText::new(Str::Queries.text()).strong());
                for q in &doc.queries {
                    let on = self.query.as_deref() == Some(q.name.as_str());
                    if ui.selectable_label(on, q.name.as_str()).clicked() && !on {
                        self.query = Some(q.name.clone());
                        self.selected = None;
                    }
                }
            });

        egui::Panel::right("pq-steps")
            .default_size(220.0)
            .show(ui, |ui| {
                ui.label(RichText::new(Str::AppliedSteps.text()).strong());
                let Some(q) = query else {
                    return;
                };
                let names =
                    std::iter::once(Str::SourceStep.text().to_string()).chain(step_names(steps));
                for (i, name) in names.enumerate() {
                    ui.horizontal(|ui| {
                        if ui.selectable_label(i == selected, name.as_str()).clicked() {
                            self.selected = if i == steps.len() { None } else { Some(i) };
                        }
                        if i > 0
                            && crate::icons::name(
                                ui.small_button("×"),
                                egui::WidgetType::Button,
                                &Str::DeleteStepNamed.fill(&[&name]),
                            )
                            .clicked()
                        {
                            out.push(Request::Run(
                                "query.deleteStep",
                                json!({"query": q.name, "step": i - 1}),
                            ));
                            self.selected = None;
                        }
                    });
                }
            });

        egui::Panel::bottom("pq-m")
            .resizable(true)
            .default_size(150.0)
            .show(ui, |ui| {
                ui.label(RichText::new(Str::QueryM.text()).strong());
                let m = self
                    .query
                    .as_deref()
                    .and_then(|n| metriccraft_engine::query_m(doc, n))
                    .unwrap_or_default();
                egui::ScrollArea::both().show(ui, |ui| {
                    let job = m_colour(&m, t, &FontId::monospace(13.0));
                    ui.label(job);
                });
            });

        egui::CentralPanel::default().show(ui, |ui| {
            let Some(q) = query else {
                ui.label(RichText::new(Str::NoQueries.text()).color(t.muted));
                return;
            };
            let want = (q.name.clone(), at, snap.seq);
            if self.asked.as_ref() != Some(&want) {
                out.push(Request::Preview(q.name.clone(), at));
                self.asked = Some(want);
            }
            match preview.filter(|p| p.query == q.name && p.step == at) {
                Some(PreviewOut { result: Ok(p), .. }) => {
                    let key = format!("{}#{at:?}", q.name);
                    let label = Str::PreviewOf.fill(&[&q.name]);
                    let error = |row: usize, col: usize| p.error(row, col).map(str::to_string);
                    self.grid.show_batch(
                        ui,
                        t,
                        &key,
                        &label,
                        &p.batch,
                        &|_| (String::new(), false),
                        &error,
                    );
                }
                Some(PreviewOut { result: Err(e), .. }) => {
                    ui.label(RichText::new(e.as_str()).color(t.danger));
                }
                None => {
                    ui.label(RichText::new(Str::LoadingPreview.text()).color(t.muted));
                }
            }
        });
        out
    }

    /// `c`'s form with the selected Query, and the picked column where it takes columns.
    fn form(&self, c: &'static Command) -> Form {
        let mut f = Form::new(c);
        if let Some(q) = &self.query {
            f = f.with_value("query", q);
        }
        if let Some(col) = &self.grid.column {
            for name in ["column", "columns"] {
                if c.params.iter().any(|p| p.name == name) {
                    f = f.with_value(name, col);
                }
            }
        }
        f
    }
}

/// M coloured for reading: text, `#"names"`, numbers, keywords and function names.
pub fn m_colour(src: &str, t: &Tokens, font: &FontId) -> LayoutJob {
    const KEYWORDS: [&str; 12] = [
        "let", "in", "each", "and", "or", "not", "if", "then", "else", "true", "false", "null",
    ];
    let mut job = LayoutJob::default();
    let mut add = |piece: &str, color| {
        job.append(
            piece,
            0.0,
            TextFormat {
                font_id: font.clone(),
                color,
                ..Default::default()
            },
        );
    };
    let mut rest = src;
    while let Some(c) = rest.chars().next() {
        let (len, color) = if c == '"' || rest.starts_with("#\"") {
            // A text or a quoted name: to the closing quote (`""` is an escaped quote).
            let start = if c == '#' { 2 } else { 1 };
            let mut end = rest.len();
            let mut i = start;
            while let Some(q) = rest.get(i..).and_then(|r| r.find('"')) {
                let at = i + q;
                if rest.get(at + 1..at + 2) == Some("\"") {
                    i = at + 2;
                } else {
                    end = at + 1;
                    break;
                }
            }
            (end, if c == '#' { t.code_field } else { t.code_text })
        } else if c.is_ascii_digit() {
            let n = rest
                .find(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
                .unwrap_or(rest.len());
            (n, t.code_number)
        } else if c.is_alphabetic() || c == '_' || c == '#' {
            let n = rest
                .char_indices()
                .skip(1)
                .find(|(_, ch)| !(ch.is_alphanumeric() || *ch == '_' || *ch == '.'))
                .map_or(rest.len(), |(i, _)| i);
            let word = rest.get(..n).unwrap_or_default();
            let call = rest.get(n..).is_some_and(|r| r.starts_with('('));
            let color = if KEYWORDS.contains(&word) || call {
                t.code_function
            } else {
                t.text
            };
            (n, color)
        } else {
            (c.len_utf8(), t.text)
        };
        let len = len.max(c.len_utf8()).min(rest.len());
        add(rest.get(..len).unwrap_or_default(), color);
        rest = rest.get(len..).unwrap_or_default();
    }
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use metriccraft_engine::Step;

    #[test]
    fn repeated_step_names_are_numbered_as_power_query_does() {
        let steps = [
            Step::PromotedHeaders,
            Step::RemovedColumns(vec![]),
            Step::RemovedColumns(vec![]),
        ];
        assert_eq!(
            step_names(&steps),
            ["Promoted Headers", "Removed Columns", "Removed Columns1"]
        );
    }

    #[test]
    fn m_colouring_keeps_every_byte() {
        let t = Tokens::get(crate::theme::Mode::Light);
        let font = FontId::monospace(13.0);
        for src in [
            "let\n    Source = Csv.Document(File.Contents(\"C:\\a \"\"b\"\".csv\"),[Delimiter=\",\"]),\n    #\"Changed Type\" = Table.TransformColumnTypes(Source,{{\"A\", Int64.Type}})\nin\n    #\"Changed Type\"",
            "\"unterminated",
            "#\"",
            "é 1.5 ü",
            "",
        ] {
            assert_eq!(m_colour(src, &t, &font).text, src);
        }
    }
}
