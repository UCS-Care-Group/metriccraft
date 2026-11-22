//! The Report view's Panes acting on the canvas: Visualizations (type gallery, field wells),
//! the Data pane's Field clicks, and Filters (the Page's slicer selections).

use egui::{RichText, Ui};
use metriccraft_engine::Page;
use metriccraft_visuals::{FieldRef, Visual, VisualType, Well};
use serde_json::{Value, json};

use crate::canvas::visual_name;
use crate::strings::Str;
use crate::theme::Tokens;

/// A command a Pane asks for.
pub type Request = (&'static str, Value);

pub fn well_name(w: Well) -> Str {
    match w {
        Well::Values => Str::WellValues,
        Well::Rows => Str::WellRows,
        Well::Columns => Str::WellColumns,
        Well::Axis => Str::WellAxis,
        Well::Legend => Str::WellLegend,
        Well::Field => Str::WellField,
    }
}

fn field_name(f: &FieldRef) -> &str {
    match f {
        FieldRef::Column { column, .. } => column,
        FieldRef::Measure { name } => name,
    }
}

pub fn field_json(f: &FieldRef) -> Value {
    match f {
        FieldRef::Column { table, column } => json!({"table": table, "column": column}),
        FieldRef::Measure { name } => json!({"measure": name}),
    }
}

/// The well a Field clicked in the Data pane goes to, as Power BI picks it: Measures to Values;
/// Columns to the grouping well (a chart's axis until it has one, then Values).
pub fn well_for(v: &Visual, f: &FieldRef) -> Option<Well> {
    let measure = matches!(f, FieldRef::Measure { .. });
    let has = |w: Well| v.kind.wells().contains(&w);
    let empty = |w: Well| v.wells.get(&w).is_none_or(Vec::is_empty);
    match v.kind {
        VisualType::Slicer => (!measure).then_some(Well::Field),
        VisualType::Matrix if !measure => Some(Well::Rows),
        _ if measure => has(Well::Values).then_some(Well::Values),
        _ if has(Well::Axis) && empty(Well::Axis) => Some(Well::Axis),
        _ => has(Well::Values).then_some(Well::Values),
    }
}

/// The Visualizations pane: the type gallery (changes the selected Visual's type, or inserts a
/// Visual), then the selected Visual's wells with a remove button per Field.
pub fn visualizations(
    ui: &mut Ui,
    t: &Tokens,
    page: Option<&Page>,
    selected: Option<&Visual>,
) -> Vec<Request> {
    let mut out = Vec::new();
    ui.horizontal_wrapped(|ui| {
        for kind in VisualType::ALL {
            let on = selected.is_some_and(|v| v.kind == kind);
            let r = crate::icons::a11y_label(
                ui.selectable_label(on, visual_name(kind).show()),
                egui::WidgetType::SelectableLabel,
                visual_name(kind).text(),
            );
            if r.clicked() && !on {
                out.push(match (page, selected) {
                    (Some(p), Some(v)) => (
                        "visual.changeType",
                        json!({"page": p.id, "visual": v.id, "type": kind.id()}),
                    ),
                    (Some(p), None) => {
                        ("insert.newVisual", json!({"page": p.id, "type": kind.id()}))
                    }
                    (None, _) => ("insert.newVisual", json!({"type": kind.id()})),
                });
            }
        }
    });
    let (Some(p), Some(v)) = (page, selected) else {
        ui.add_space(8.0);
        crate::icons::a11y_label(
            crate::i18n::wrapped_label(ui, Str::SelectVisual.text(), |r| r.color(t.muted)),
            egui::WidgetType::Label,
            Str::SelectVisual.text(),
        );
        return out;
    };
    ui.separator();
    for &well in v.kind.wells() {
        crate::icons::a11y_label(
            ui.label(RichText::new(well_name(well).show()).strong()),
            egui::WidgetType::Label,
            well_name(well).text(),
        );
        let fields = v.wells.get(&well).map(Vec::as_slice).unwrap_or_default();
        if fields.is_empty() {
            crate::icons::a11y_label(
                crate::i18n::wrapped_label(ui, Str::WellEmpty.text(), |r| r.color(t.muted)),
                egui::WidgetType::Label,
                Str::WellEmpty.text(),
            );
        }
        for (i, f) in fields.iter().enumerate() {
            ui.horizontal(|ui| {
                let name = field_name(f);
                crate::icons::a11y_label(
                    ui.label(crate::i18n::visual_rtl(name)),
                    egui::WidgetType::Label,
                    name,
                );
                let r = ui.small_button("×");
                let tooltip = Str::RemoveField.fill(&[name]);
                let a11y = Str::RemoveField.fill_logical(&[name]);
                if crate::icons::name_labeled(r, egui::WidgetType::Button, &tooltip, &a11y)
                    .clicked()
                {
                    out.push((
                        "visual.removeField",
                        json!({"page": p.id, "visual": v.id, "well": well.id(), "index": i}),
                    ));
                }
            });
        }
        ui.add_space(6.0);
    }
    out
}

/// The Filters pane: each slicer's selection on the Page, with a button to clear it.
// ponytail: filter cards (basic/advanced filters per Visual, Page, report) aren't in the v0.1
// spec; data-point Selections are session-only and show on the canvas.
pub fn filters(ui: &mut Ui, t: &Tokens, page: Option<&Page>) -> Vec<Request> {
    let mut out = Vec::new();
    let Some(p) = page else {
        return out;
    };
    let mut any = false;
    for (visual, items) in p.selections.iter().filter(|(_, i)| !i.is_empty()) {
        let Some(v) = p.visuals.iter().find(|v| &v.id == visual) else {
            continue;
        };
        any = true;
        let field = v
            .wells
            .get(&Well::Field)
            .and_then(|f| f.first())
            .map_or("", field_name);
        let values: Vec<String> = items
            .iter()
            .map(|i| i.as_str().map_or_else(|| i.to_string(), str::to_string))
            .collect();
        crate::icons::a11y_label(
            ui.label(RichText::new(crate::i18n::visual_rtl(field)).strong()),
            egui::WidgetType::Label,
            field,
        );
        ui.horizontal_wrapped(|ui| {
            let joined = values.join(", ");
            crate::icons::a11y_label(
                ui.label(crate::i18n::visual_rtl(&joined)),
                egui::WidgetType::Label,
                &joined,
            );
            let r = ui.small_button("×");
            let tooltip = Str::ClearSelection.fill(&[field]);
            let a11y = Str::ClearSelection.fill_logical(&[field]);
            if crate::icons::name_labeled(r, egui::WidgetType::Button, &tooltip, &a11y).clicked() {
                out.push((
                    "slicer.select",
                    json!({"page": p.id, "visual": v.id, "items": []}),
                ));
            }
        });
        ui.add_space(6.0);
    }
    if !any {
        crate::i18n::wrapped_label(ui, Str::NoFilters.text(), |r| r.color(t.muted));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_pane_clicks_pick_power_bis_well() {
        let col = FieldRef::Column {
            table: "T".into(),
            column: "C".into(),
        };
        let m = FieldRef::Measure { name: "M".into() };
        let mut chart = Visual::new("v".to_string(), VisualType::ClusteredColumn);
        assert_eq!(well_for(&chart, &col), Some(Well::Axis));
        assert_eq!(well_for(&chart, &m), Some(Well::Values));
        chart.wells.insert(Well::Axis, vec![col.clone()]);
        assert_eq!(well_for(&chart, &col), Some(Well::Values));
        let slicer = Visual::new("s".to_string(), VisualType::Slicer);
        assert_eq!(well_for(&slicer, &m), None);
        assert_eq!(well_for(&slicer, &col), Some(Well::Field));
        let matrix = Visual::new("x".to_string(), VisualType::Matrix);
        assert_eq!(well_for(&matrix, &col), Some(Well::Rows));
    }
}
