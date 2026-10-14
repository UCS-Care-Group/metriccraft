//! The Table view's grid (spec U-5): any Table's rows, virtual. Only the visible rows are read
//! and laid out, and the position is a row index rather than a pixel offset, so 10M rows scroll
//! as smoothly as 10 (egui's f32 scroll offsets lose whole rows past a few million).

use egui::{Align2, FontId, Key, Rect, RichText, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType};
use opendash_core::Value;
use opendash_engine::Document;
use opendash_model::dax::storage::Col;

use crate::strings::Str;
use crate::theme::Tokens;

const ROW: f32 = 22.0;
const WIDTH: f32 = 150.0;
const PAD: f32 = 6.0;

/// The grid's place in the picked Table.
#[derive(Default)]
pub struct Grid {
    table: String,
    /// The first visible row.
    pub top: usize,
    /// The first visible column.
    pub left: usize,
    /// Wheel movement not yet a whole row.
    rest: f32,
    /// The picked column (Column tools act on it).
    pub column: Option<String>,
}

/// `v` as the Table view shows it: the column's format string, else its own text.
fn text(v: &Value, format: &str) -> String {
    if v.is_blank() {
        return String::new();
    }
    opendash_core::format(v, format, &opendash_core::EN_US).unwrap_or_else(|_| v.to_text())
}

impl Grid {
    /// Shows Table `name` of `doc`; `None` or an unknown name shows the first Table.
    pub fn show(&mut self, ui: &mut Ui, t: &Tokens, doc: &Document, name: Option<&str>) {
        let table = name
            .and_then(|n| doc.tables.iter().find(|tb| tb.name() == n))
            .or_else(|| doc.tables.first());
        let Some(table) = table else {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(Str::NoTables.text()).color(t.muted));
            });
            return;
        };
        if self.table != table.name() {
            *self = Grid {
                table: table.name().to_string(),
                ..Grid::default()
            };
        }
        let model = doc.model.tables.iter().find(|m| m.name == table.name());
        let batch = table.batch();
        let schema = batch.schema();
        let rows = table.rows();
        let count = text(
            &Value::Integer(i64::try_from(rows).unwrap_or(i64::MAX)),
            "#,0",
        );
        ui.label(RichText::new(Str::RowCount.fill(&[&count])).color(t.muted));

        let area = ui.available_rect_before_wrap();
        let body = Rect::from_min_max(area.min, area.max - Vec2::splat(14.0));
        let visible = ((body.height() - ROW) / ROW).floor().max(1.0) as usize;
        let fits = ((body.width() / WIDTH).floor().max(1.0)) as usize;
        let ncols = schema.fields().len();
        let max_top = rows.saturating_sub(visible);
        let max_left = ncols.saturating_sub(fits);

        let id = ui.id().with("table-grid");
        let r = ui.interact(body, id, Sense::click());
        let label = Str::TableOf.fill(&[table.name()]);
        r.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &label));
        if r.clicked() {
            r.request_focus();
        }
        if r.hovered() {
            let (dy, dx) = ui.input(|i| (i.smooth_scroll_delta.y, i.smooth_scroll_delta.x));
            self.rest -= dy / ROW;
            let whole = self.rest.trunc();
            self.rest -= whole;
            self.top = step(self.top, whole as i64, max_top);
            if dx.abs() > WIDTH / 4.0 {
                self.left = step(self.left, if dx < 0.0 { 1 } else { -1 }, max_left);
            }
        }
        if r.has_focus() {
            let page = visible as i64;
            let moves = ui.input(|i| {
                [
                    (Key::ArrowDown, 1),
                    (Key::ArrowUp, -1),
                    (Key::PageDown, page),
                    (Key::PageUp, -page),
                    (Key::End, i64::MAX / 2),
                    (Key::Home, i64::MIN / 2),
                ]
                .into_iter()
                .filter(|(k, _)| i.key_pressed(*k))
                .map(|(_, d)| d)
                .sum::<i64>()
            });
            self.top = step(self.top, moves, max_top);
        }
        self.top = self.top.min(max_top);
        self.left = self.left.min(max_left);

        let painter = ui.painter_at(area);
        painter.rect_filled(body, 0.0, t.base);
        let font = FontId::proportional(13.0);
        let shown: Vec<_> = (self.left..ncols.min(self.left + fits + 1)).collect();
        for (n, &c) in shown.iter().enumerate() {
            let (Some(field), Some(array)) = (schema.fields().get(c), batch.columns().get(c))
            else {
                continue;
            };
            let col = Col::new(array);
            let props = model.and_then(|m| m.columns.iter().find(|mc| &mc.name == field.name()));
            let format = props.map_or("", |p| p.format_string.as_str());
            let x = body.left() + n as f32 * WIDTH;
            let head = Rect::from_min_size(egui::pos2(x, body.top()), Vec2::new(WIDTH, ROW));
            let picked = self.column.as_deref() == Some(field.name().as_str());
            let hr = ui.interact(head, id.with(c), Sense::click());
            hr.widget_info(|| WidgetInfo::selected(WidgetType::Button, true, picked, field.name()));
            if hr.clicked() {
                self.column = Some(field.name().clone());
            }
            painter.rect_filled(head, 0.0, if picked { t.hover } else { t.panel });
            let cell = painter.with_clip_rect(head.shrink(1.0).intersect(body));
            let hidden = props.is_some_and(|p| p.hidden);
            let color = if hidden { t.muted } else { t.text };
            cell.text(
                head.left_center() + Vec2::new(PAD, 0.0),
                Align2::LEFT_CENTER,
                field.name(),
                FontId::proportional(13.0),
                color,
            );
            let shown_rows = visible.min(rows.saturating_sub(self.top)) as f32;
            let bottom = (body.top() + ROW * (shown_rows + 1.0)).min(body.bottom());
            painter.line_segment(
                [head.right_top(), egui::pos2(head.right(), bottom)],
                Stroke::new(1.0, t.border),
            );
            for i in 0..visible.min(rows.saturating_sub(self.top)) {
                let row = self.top + i;
                let v = col.value(row);
                let s = text(&v, format);
                let y = body.top() + ROW * (i as f32 + 1.0);
                let rect = Rect::from_min_size(egui::pos2(x, y), Vec2::new(WIDTH, ROW));
                if picked {
                    painter.rect_filled(rect, 0.0, t.hover);
                }
                let cell = painter.with_clip_rect(rect.shrink(1.0).intersect(body));
                if matches!(v, Value::Text(_) | Value::Boolean(_)) {
                    cell.text(
                        rect.left_center() + Vec2::new(PAD, 0.0),
                        Align2::LEFT_CENTER,
                        s,
                        font.clone(),
                        t.text,
                    );
                } else {
                    cell.text(
                        rect.right_center() - Vec2::new(PAD, 0.0),
                        Align2::RIGHT_CENTER,
                        s,
                        font.clone(),
                        t.text,
                    );
                }
            }
        }
        let right = (body.left() + shown.len() as f32 * WIDTH).min(body.right());
        for i in 0..=visible.min(rows.saturating_sub(self.top)) {
            let y = body.top() + ROW * (i as f32 + 1.0);
            painter.line_segment(
                [egui::pos2(body.left(), y), egui::pos2(right, y)],
                Stroke::new(1.0, t.border),
            );
        }

        // Scrollbars: sliders over row and column indexes (f64 holds any row count exactly
        // enough), keyboard-reachable like any slider.
        if max_top > 0 {
            let rect = Rect::from_min_max(
                egui::pos2(body.right(), body.top()),
                egui::pos2(area.right(), body.bottom()),
            );
            let mut v = self.top as f64;
            let changed = ui
                .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.spacing_mut().slider_width = rect.height();
                    let s = egui::Slider::new(&mut v, max_top as f64..=0.0)
                        .vertical()
                        .show_value(false);
                    ui.add(s).on_hover_text(Str::RowScroll.text()).changed()
                })
                .inner;
            if changed {
                self.top = (v.max(0.0).round() as usize).min(max_top);
            }
        }
        if max_left > 0 {
            let rect = Rect::from_min_max(
                egui::pos2(body.left(), body.bottom()),
                egui::pos2(body.right(), area.bottom()),
            );
            let mut v = self.left as f64;
            let changed = ui
                .scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    ui.spacing_mut().slider_width = rect.width();
                    let s = egui::Slider::new(&mut v, 0.0..=max_left as f64).show_value(false);
                    ui.add(s).on_hover_text(Str::ColumnScroll.text()).changed()
                })
                .inner;
            if changed {
                self.left = (v.max(0.0).round() as usize).min(max_left);
            }
        }
        ui.advance_cursor_after_rect(area);
    }
}

/// `at` moved by `by`, kept in `0..=max`.
fn step(at: usize, by: i64, max: usize) -> usize {
    let at = i64::try_from(at).unwrap_or(i64::MAX).saturating_add(by);
    usize::try_from(at.max(0)).unwrap_or(usize::MAX).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stay_in_range() {
        assert_eq!(step(5, -10, 100), 0);
        assert_eq!(step(5, i64::MAX / 2, 100), 100);
        assert_eq!(step(9_999_990, 3, 9_999_995), 9_999_993);
    }
}
