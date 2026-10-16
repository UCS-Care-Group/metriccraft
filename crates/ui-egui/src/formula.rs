//! The formula bar: a Measure's or Calculated column's DAX as `Name = expression`,
//! coloured as you type, with a parse error underlined at its span and named below. Enter
//! commits, Shift+Enter breaks the line, Esc cancels.

use egui::text::{LayoutJob, TextFormat};
use egui::{Event, FontId, Key, RichText, Stroke, Ui};
use metriccraft_dax::{Span, Tok, tokens};
use serde_json::{Value, json};

use crate::strings::Str;
use crate::theme::Tokens;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Measure,
    Column,
}

/// The formula bar's text edit, focused when the bar opens.
pub fn id() -> egui::Id {
    egui::Id::new("formula-bar")
}

/// What the formula bar edits.
#[derive(Clone, Debug, PartialEq)]
pub struct Formula {
    pub kind: Kind,
    pub table: String,
    /// The Measure or Calculated column edited; `None` for a new one.
    pub name: Option<String>,
    pub text: String,
}

/// How a frame of the formula bar ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Editing,
    Commit(Vec<(&'static str, Value)>),
    Cancel,
}

/// `text` split at its first `=` into the trimmed name and the expression, with the
/// expression's byte offset in `text`.
fn split(text: &str) -> Option<(&str, &str, usize)> {
    let eq = text.find('=')?;
    let name = text.get(..eq)?.trim();
    let expr = text.get(eq + 1..)?;
    (!name.is_empty()).then_some((name, expr, eq + 1))
}

/// The parse error of `text`'s expression, its span shifted into `text`.
fn error(text: &str) -> Option<(Span, String)> {
    let Some((_, expr, at)) = split(text) else {
        return Some((
            Span::new(0, text.len()),
            Str::FormulaShape.text().to_string(),
        ));
    };
    if expr.trim().is_empty() {
        return None;
    }
    let e = metriccraft_dax::parse_expr(expr).err()?;
    let span = Span::new(
        e.span.start.saturating_add(at),
        e.span.end.saturating_add(at),
    );
    Some((span, e.message))
}

/// `src` coloured by its DAX tokens; the text between tokens that isn't space is a comment.
/// `bad` is underlined.
pub fn colour(src: &str, t: &Tokens, font: &FontId, bad: Option<Span>) -> LayoutJob {
    let mut job = LayoutJob::default();
    let mut add = |start: usize, end: usize, color| {
        // Split at the error span so its part alone is underlined.
        let mut cuts = vec![start, end];
        if let Some(b) = bad {
            cuts.extend([b.start.clamp(start, end), b.end.clamp(start, end)]);
        }
        cuts.sort_unstable();
        cuts.dedup();
        for w in cuts.windows(2) {
            let (a, z) = (
                w.first().copied().unwrap_or(0),
                w.get(1).copied().unwrap_or(0),
            );
            let Some(piece) = src.get(a..z) else {
                continue;
            };
            let under = bad.is_some_and(|b| a >= b.start && z <= b.end.max(b.start + 1));
            let format = TextFormat {
                font_id: font.clone(),
                color,
                underline: if under {
                    Stroke::new(1.5, t.danger)
                } else {
                    Stroke::NONE
                },
                ..Default::default()
            };
            job.append(piece, 0.0, format);
        }
    };
    let toks = tokens(src).unwrap_or_default();
    let mut pos = 0;
    for (i, tk) in toks.iter().enumerate() {
        let (start, end) = (tk.span.start.min(src.len()), tk.span.end.min(src.len()));
        if start < pos {
            continue;
        }
        if start > pos {
            let gap = src.get(pos..start).unwrap_or_default();
            add(
                pos,
                start,
                if gap.trim().is_empty() {
                    t.text
                } else {
                    t.muted
                },
            );
        }
        let call = matches!(toks.get(i + 1).map(|n| &n.tok), Some(Tok::Punct("(")));
        let color = match &tk.tok {
            Tok::Name(_) if call => t.code_function,
            Tok::Quoted(_) | Tok::Bracket(_) => t.code_field,
            Tok::Text(_) => t.code_text,
            Tok::Integer(_) | Tok::Decimal(_) => t.code_number,
            _ => t.text,
        };
        if end > start {
            add(start, end, color);
        }
        pos = end.max(pos);
    }
    if pos < src.len() {
        add(pos, src.len(), t.text);
    }
    job
}

impl Formula {
    /// A new Measure or Calculated column of `table`, named as Power BI names them.
    pub fn new(kind: Kind, table: &str) -> Formula {
        let name = match kind {
            Kind::Measure => Str::NewMeasureName,
            Kind::Column => Str::NewColumnName,
        };
        Formula {
            kind,
            table: table.to_string(),
            name: None,
            text: format!("{} = ", name.text()),
        }
    }

    /// Editing Measure or Calculated column `name` of `table`, whose DAX is `expression`.
    pub fn edit(kind: Kind, table: &str, name: &str, expression: &str) -> Formula {
        Formula {
            kind,
            table: table.to_string(),
            name: Some(name.to_string()),
            text: format!("{name} = {expression}"),
        }
    }

    /// The commands that save the text: create, or rename and set the expression.
    fn commands(&self) -> Option<Vec<(&'static str, Value)>> {
        let (name, expr, _) = split(&self.text)?;
        let (table, expr) = (&self.table, expr.trim());
        let mut out = Vec::new();
        match (&self.name, self.kind) {
            (None, Kind::Measure) => out.push((
                "modeling.newMeasure",
                json!({"table": table, "name": name, "expression": expr}),
            )),
            (None, Kind::Column) => out.push((
                "modeling.newColumn",
                json!({"table": table, "name": name, "expression": expr}),
            )),
            (Some(old), kind) => {
                if old != name {
                    out.push(match kind {
                        Kind::Measure => (
                            "modeling.renameMeasure",
                            json!({"table": table, "measure": old, "name": name}),
                        ),
                        Kind::Column => (
                            "modeling.renameColumn",
                            json!({"table": table, "column": old, "name": name}),
                        ),
                    });
                }
                out.push((
                    "modeling.editExpression",
                    json!({"table": table, "name": name, "expression": expr}),
                ));
            }
        }
        Some(out)
    }

    pub fn show(&mut self, ui: &mut Ui, t: &Tokens) -> Outcome {
        let id = id();
        let focused = ui.memory(|m| m.has_focus(id));
        // Enter commits (Shift+Enter is a line break, which the text edit handles); taken out
        // of the input before the text edit sees it.
        let enter = focused
            && ui.input_mut(|i| {
                let at = i.events.iter().position(|e| {
                    matches!(e, Event::Key { key: Key::Enter, pressed: true, modifiers, .. } if !modifiers.shift)
                });
                at.map(|a| i.events.remove(a)).is_some()
            });
        let escape = focused && ui.input(|i| i.key_pressed(Key::Escape));
        let err = error(&self.text);
        let mut out = Outcome::Editing;
        ui.horizontal(|ui| {
            let commit = ui.button("✓");
            if crate::icons::name(commit, egui::WidgetType::Button, Str::Commit.text()).clicked()
                || enter
            {
                out = match self.commands() {
                    Some(c) => Outcome::Commit(c),
                    None => Outcome::Editing,
                };
            }
            let cancel = ui.button("✗");
            if crate::icons::name(cancel, egui::WidgetType::Button, Str::Cancel.text()).clicked()
                || escape
            {
                out = Outcome::Cancel;
            }
            let font = FontId::monospace(14.0);
            let bad = err.as_ref().map(|(s, _)| *s);
            let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, wrap: f32| {
                let mut job = colour(text.as_str(), t, &font, bad);
                job.wrap.max_width = wrap;
                ui.fonts_mut(|f| f.layout_job(job))
            };
            let label = match self.kind {
                Kind::Measure => Str::FormulaMeasure,
                Kind::Column => Str::FormulaColumn,
            };
            let edit = egui::TextEdit::multiline(&mut self.text)
                .id(id)
                .desired_rows(1)
                .desired_width(f32::INFINITY)
                .code_editor()
                .layouter(&mut layouter);
            let r = ui.add(edit);
            r.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, label.text())
            });
        });
        if let Some((_, message)) = err {
            ui.label(RichText::new(message).color(t.danger));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_name_and_expression() {
        assert_eq!(split("Total = SUM(T[A])"), Some(("Total", " SUM(T[A])", 7)));
        assert_eq!(split(" = 1"), None);
        assert_eq!(split("no equals"), None);
    }

    #[test]
    fn parse_errors_point_into_the_whole_text() {
        let (span, _) = error("M = SUM(").unwrap_or_default();
        assert!(span.start >= 4, "{span:?}");
        assert!(error("M = SUM(T[A])").is_none());
        assert!(error("M = ").is_none());
        assert!(error("nothing").is_some());
    }

    #[test]
    fn colouring_keeps_every_byte_and_marks_calls() {
        let t = Tokens::get(crate::theme::Mode::Light);
        let font = FontId::monospace(14.0);
        for src in [
            "M = SUM('T'[A]) // note",
            "M = \"é\" & 1.5",
            "M = SUM(",
            "\"open",
            "",
        ] {
            let job = colour(src, &t, &font, error(src).map(|(s, _)| s));
            assert_eq!(job.text, src);
        }
        let job = colour("M = SUM(T[A])", &t, &font, None);
        let sum = job
            .sections
            .iter()
            .find(|s| job.text.get(s.byte_range.start.0..s.byte_range.end.0) == Some("SUM"));
        assert_eq!(sum.map(|s| s.format.color), Some(t.code_function));
    }

    #[test]
    fn commit_creates_or_renames_then_edits() {
        let mut f = Formula::new(Kind::Measure, "Sales");
        f.text = "Total = SUM(Sales[Amount])".into();
        let c = f.commands().unwrap_or_default();
        assert_eq!(c.first().map(|c| c.0), Some("modeling.newMeasure"));
        let mut f = Formula::edit(Kind::Measure, "Sales", "Total", "1");
        f.text = "Grand = 2".into();
        let ids: Vec<_> = f
            .commands()
            .unwrap_or_default()
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(ids, ["modeling.renameMeasure", "modeling.editExpression"]);
    }
}
