//! The Model view: Tables as boxes at their saved positions, Relationships as lines
//! (`*` on the many side, `1` on the one side, dashed when inactive). Drag a box by its header to
//! move it (`model.moveTable` on release); drag a column onto another Table's column to relate
//! them (`modeling.newRelationship`, either way round).

use egui::{Align2, FontId, Rect, Sense, Shape, Stroke, Ui, Vec2, WidgetInfo, WidgetType};
use metriccraft_engine::Document;
use serde_json::{Value, json};

use crate::strings::Str;
use crate::theme::Tokens;

const WIDTH: f32 = 220.0;
const HEAD: f32 = 28.0;
const ROW: f32 = 20.0;
/// Largest coordinate the engine accepts.
const MAX: i32 = metriccraft_engine::MAX_COORD;

#[derive(Default)]
pub struct Diagram {
    /// The Table whose box is dragged, and how far so far.
    moving: Option<(String, Vec2)>,
    /// The column a relationship is dragged from.
    linking: Option<(String, String)>,
}

/// Where Table number `i` sits until it is moved: four to a row.
fn default_at(i: usize) -> (i32, i32) {
    let (col, row) = ((i % 4) as i32, (i / 4) as i32);
    (40 + col * 280, 40 + row.saturating_mul(320))
}

struct Laid<'a> {
    name: &'a str,
    rect: Rect,
    /// Each shown column's name and row rect.
    rows: Vec<(&'a str, Rect)>,
}

impl Diagram {
    /// Draws `doc`'s Model; returns the commands to run.
    pub fn show(&mut self, ui: &mut Ui, t: &Tokens, doc: &Document) -> Vec<(&'static str, Value)> {
        let mut out = Vec::new();
        if doc.model.tables.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.label(egui::RichText::new(Str::NoTables.text()).color(t.muted));
            });
            return out;
        }
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let origin = ui.min_rect().min;
            let mut boxes = Vec::new();
            let mut extent = Vec2::ZERO;
            for (i, table) in doc.model.tables.iter().enumerate() {
                let (x, y) = doc.diagram.get(&table.name).copied().unwrap_or_else(|| default_at(i));
                let mut at = origin + Vec2::new(x as f32, y as f32);
                if let Some((name, d)) = &self.moving
                    && name == &table.name
                {
                    at += *d;
                }
                let names: Vec<&str> = table
                    .columns
                    .iter()
                    .filter(|c| !c.hidden)
                    .map(|c| c.name.as_str())
                    .collect();
                let rect = Rect::from_min_size(at, Vec2::new(WIDTH, HEAD + ROW * names.len() as f32 + 6.0));
                let rows = names
                    .iter()
                    .enumerate()
                    .map(|(n, c)| {
                        let top = rect.top() + HEAD + ROW * n as f32;
                        (*c, Rect::from_min_size(egui::pos2(rect.left(), top), Vec2::new(WIDTH, ROW)))
                    })
                    .collect();
                extent = extent.max(rect.max - origin + Vec2::splat(40.0));
                boxes.push(Laid { name: &table.name, rect, rows });
            }
            ui.allocate_rect(Rect::from_min_size(origin, extent), Sense::hover());
            let painter = ui.painter().clone();
            let row_of = |table: &str, column: &str| {
                boxes.iter().find(|b| b.name == table).and_then(|b| b.rows.iter().find(|(c, _)| *c == column).map(|(_, r)| (b.rect, *r)))
            };

            // Relationships under the boxes.
            for r in &doc.model.relationships {
                let (Some((fb, fr)), Some((tb, tr))) = (row_of(&r.from_table, &r.from_column), row_of(&r.to_table, &r.to_column)) else {
                    continue;
                };
                let right = fb.center().x < tb.center().x;
                let a = if right { fr.right_center() } else { fr.left_center() };
                let b = if right { tr.left_center() } else { tr.right_center() };
                let stroke = Stroke::new(1.5, if r.active { t.text } else { t.muted });
                if r.active {
                    painter.line_segment([a, b], stroke);
                } else {
                    painter.extend(Shape::dashed_line(&[a, b], stroke, 6.0, 4.0));
                }
                let dir = (b - a).normalized();
                let font = FontId::proportional(13.0);
                painter.text(a + dir * 12.0 - Vec2::new(0.0, 8.0), Align2::CENTER_CENTER, "*", font.clone(), t.text);
                painter.text(b - dir * 12.0 - Vec2::new(0.0, 8.0), Align2::CENTER_CENTER, "1", font, t.text);
            }

            let pointer = ui.ctx().pointer_interact_pos();
            for b in &boxes {
                painter.rect_filled(b.rect, 4.0, t.base);
                painter.rect_stroke(b.rect, 4.0, Stroke::new(1.0, t.border), egui::StrokeKind::Inside);
                let head = Rect::from_min_size(b.rect.min, Vec2::new(WIDTH, HEAD));
                painter.rect_filled(head, egui::CornerRadius { nw: 4, ne: 4, sw: 0, se: 0 }, t.panel);
                painter.text(head.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, b.name, FontId::proportional(14.0), t.text);
                let hr = ui.interact(head, ui.id().with(("table", b.name)), Sense::drag());
                let label = Str::TableOf.fill(&[b.name]);
                hr.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &label));
                if hr.drag_started() {
                    self.moving = Some((b.name.to_string(), Vec2::ZERO));
                }
                if hr.dragged()
                    && let Some((_, d)) = &mut self.moving
                {
                    *d += hr.drag_delta();
                }
                if hr.drag_stopped()
                    && let Some((name, d)) = self.moving.take()
                {
                    let at = b.rect.min - origin;
                    let (x, y) = ((at.x.round() as i32).clamp(0, MAX), (at.y.round() as i32).clamp(0, MAX));
                    if d != Vec2::ZERO {
                        out.push(("model.moveTable", json!({"table": name, "x": x, "y": y})));
                    }
                }
                for (c, rect) in &b.rows {
                    let r = ui.interact(*rect, ui.id().with(("column", b.name, *c)), Sense::drag());
                    let label = Str::ColumnOf.fill(&[c, b.name]);
                    r.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &label));
                    let linking = self.linking.as_ref().is_some_and(|(lt, lc)| lt == b.name && lc == c);
                    let target = self.linking.as_ref().is_some_and(|(lt, _)| lt != b.name) && pointer.is_some_and(|p| rect.contains(p));
                    if linking || target || r.hovered() {
                        painter.rect_filled(rect.shrink(1.0), 2.0, t.hover);
                    }
                    painter.text(rect.left_center() + Vec2::new(12.0, 0.0), Align2::LEFT_CENTER, *c, FontId::proportional(13.0), t.text);
                    if r.drag_started() {
                        self.linking = Some((b.name.to_string(), c.to_string()));
                    }
                    if linking && let Some(p) = pointer {
                        painter.line_segment([rect.right_center(), p], Stroke::new(1.5, t.accent));
                    }
                }
            }

            // A relationship drag ends on whatever column is under the pointer.
            if ui.input(|i| i.pointer.any_released())
                && let Some((ft, fc)) = self.linking.take()
                && let Some(p) = ui.input(|i| i.pointer.interact_pos())
            {
                let hit = boxes.iter().filter(|b| b.name != ft).find_map(|b| {
                    b.rows.iter().find(|(_, r)| r.contains(p)).map(|(c, _)| (b.name, *c))
                });
                if let Some((tt, tc)) = hit {
                    out.push((
                        "modeling.newRelationship",
                        json!({"fromTable": ft, "fromColumn": fc, "toTable": tt, "toColumn": tc, "orient": true}),
                    ));
                }
            }
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_positions_fill_rows_of_four() {
        assert_eq!(default_at(0), (40, 40));
        assert_eq!(default_at(5), (320, 360));
    }
}
