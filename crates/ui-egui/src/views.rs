//! The Views (Report, Table, Model) and the Panes (Filters, Visualizations, Data). M5.1 gives
//! each its frame; the canvas, grid and diagram fill them in M5.3–M5.4.

use egui::{Color32, RichText, Ui};
use opendash_engine::Document;

use crate::strings::Str;
use crate::theme::Tokens;

/// The View picked on the left rail.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    Report,
    Table,
    Model,
}

impl View {
    pub const ALL: [View; 3] = [View::Report, View::Table, View::Model];

    pub fn label(self) -> Str {
        match self {
            View::Report => Str::ViewReport,
            View::Table => Str::ViewTable,
            View::Model => Str::ViewModel,
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            View::Report => "report",
            View::Table => "table",
            View::Model => "model",
        }
    }

    pub fn from_key(key: &str) -> Option<View> {
        View::ALL.into_iter().find(|v| v.key() == key)
    }
}

/// A Pane (CONTEXT.md): a dockable side panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Filters,
    Visualizations,
    Data,
}

impl Pane {
    pub fn label(self) -> Str {
        match self {
            Pane::Filters => Str::PaneFilters,
            Pane::Visualizations => Str::PaneVisualizations,
            Pane::Data => Str::PaneData,
        }
    }
}

/// The Data pane: every Table with its Columns and Measures. Clicking a Table picks it for the
/// Table view.
pub fn data_pane(ui: &mut Ui, t: &Tokens, doc: &Document, picked: &mut Option<String>) {
    if doc.model.tables.is_empty() {
        ui.label(RichText::new(Str::NoTables.text()).color(t.muted));
        return;
    }
    egui::ScrollArea::vertical().show(ui, |ui| {
        for table in &doc.model.tables {
            let id = ui.make_persistent_id(("data-pane", &table.name));
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, false)
                .show_header(ui, |ui| {
                    let on = picked.as_deref() == Some(table.name.as_str());
                    if ui.selectable_label(on, table.name.as_str()).clicked() {
                        *picked = Some(table.name.clone());
                    }
                })
                .body(|ui| {
                    for c in table.columns.iter().filter(|c| !c.hidden) {
                        ui.label(c.name.as_str());
                    }
                    for m in table.measures.iter().filter(|m| !m.hidden) {
                        ui.label(RichText::new(m.name.as_str()).italics());
                    }
                });
        }
    });
}

/// The Report view until the canvas lands (M5.4): the current Page as a blank sheet at its
/// aspect, fitted to the space.
pub fn report_view(ui: &mut Ui, t: &Tokens, doc: &Document) {
    let rect = ui.available_rect_before_wrap();
    ui.painter().rect_filled(rect, 0.0, t.well);
    let Some(page) = current_page(doc) else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new(Str::NoPages.text()).color(t.muted));
        });
        return;
    };
    let (w, h) = (page.width.max(1) as f32, page.height.max(1) as f32);
    let scale = ((rect.width() - 48.0) / w)
        .min((rect.height() - 48.0) / h)
        .max(0.01);
    let sheet = egui::Rect::from_center_size(rect.center(), egui::vec2(w * scale, h * scale));
    ui.painter().rect_filled(sheet, 2.0, Color32::WHITE);
    ui.painter().rect_stroke(
        sheet,
        2.0,
        egui::Stroke::new(1.0, t.border),
        egui::StrokeKind::Outside,
    );
}

/// The Page the report shows: the saved current one, else the first.
pub fn current_page(doc: &Document) -> Option<&opendash_engine::Page> {
    let pages = &doc.report.pages;
    doc.report
        .current
        .as_deref()
        .and_then(|id| pages.iter().find(|p| p.id == id))
        .or_else(|| pages.first())
}

/// The Table view until the grid lands (M5.3).
pub fn table_view(ui: &mut Ui, t: &Tokens, doc: &Document, picked: Option<&str>) {
    let table = picked.and_then(|n| doc.tables.iter().find(|tb| tb.name() == n));
    ui.centered_and_justified(|ui| match table {
        Some(tb) => {
            ui.label(format!("{} — {}", tb.name(), tb.rows()));
        }
        None => {
            ui.label(RichText::new(Str::TableViewHint.text()).color(t.muted));
        }
    });
}

/// The Model view until the diagram lands (M5.3): the Tables in a grid.
pub fn model_view(ui: &mut Ui, t: &Tokens, doc: &Document) {
    if doc.model.tables.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new(Str::NoTables.text()).color(t.muted));
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for table in &doc.model.tables {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.vertical(|ui| {
                    ui.set_min_width(160.0);
                    ui.strong(table.name.as_str());
                    for c in table.columns.iter().take(12) {
                        ui.label(c.name.as_str());
                    }
                });
            });
        }
    });
}
