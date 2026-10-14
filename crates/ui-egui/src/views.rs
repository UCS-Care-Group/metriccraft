//! The Views (Report, Table, Model) and the Data pane. The Views themselves are `canvas.rs`,
//! `grid.rs` and `diagram.rs`.

use egui::{Color32, RichText, Ui};
use opendash_engine::Document;
use opendash_visuals::FieldRef;

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
/// Table view; clicking a Field returns it (the Report view adds it to the selected Visual).
pub fn data_pane(
    ui: &mut Ui,
    t: &Tokens,
    doc: &Document,
    picked: &mut Option<String>,
) -> Option<FieldRef> {
    let mut clicked = None;
    if doc.model.tables.is_empty() {
        ui.label(RichText::new(Str::NoTables.text()).color(t.muted));
        return None;
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
                        if ui.selectable_label(false, c.name.as_str()).clicked() {
                            clicked = Some(FieldRef::Column {
                                table: table.name.clone(),
                                column: c.name.clone(),
                            });
                        }
                    }
                    for m in table.measures.iter().filter(|m| !m.hidden) {
                        let label = RichText::new(m.name.as_str()).italics();
                        if ui.selectable_label(false, label).clicked() {
                            clicked = Some(FieldRef::Measure {
                                name: m.name.clone(),
                            });
                        }
                    }
                });
        }
    });
    clicked
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
