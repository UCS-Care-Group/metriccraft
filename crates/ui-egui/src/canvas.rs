//! The report canvas: the current Page with its Visuals painted from the
//! engine's Scenes, selected by click, moved and resized by mouse (previewed here, one command on
//! release) and arrow keys (merged into one undo step), clicked through to cross-filter.

use egui::{Key, Pos2, Rect, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType};
use metriccraft_engine::Page;
use metriccraft_visuals::draw::Scene;
use serde_json::{Value, json};

use crate::strings::Str;
use crate::theme::Tokens;

/// How the Page is fitted.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Zoom {
    /// The whole Page in view.
    #[default]
    FitPage,
    /// The Page's width fills the view; scroll for the rest.
    FitWidth,
    /// Screen points per logical pixel.
    Fixed(f32),
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    Move,
    Resize,
}

#[derive(Clone, Debug, PartialEq)]
struct Drag {
    visual: String,
    grab: Grab,
    /// The Visual's x, y, w, h when the drag started.
    from: (i32, i32, i32, i32),
    /// Pointer movement so far, in logical pixels.
    delta: Vec2,
}

/// What the canvas asks the shell to run.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Command(&'static str, Value),
    /// A click inside a Visual at logical `x`, `y` (cross-filter, `App::click`).
    Click {
        visual: String,
        x: f32,
        y: f32,
        add: bool,
    },
}

/// The canvas's state across frames.
#[derive(Default)]
pub struct Canvas {
    pub zoom: Zoom,
    /// The selected Visual's id.
    pub selected: Option<String>,
    /// The scale last drawn at, for the zoom slider while fitting.
    pub last_scale: f32,
    drag: Option<Drag>,
    /// The Visual the last arrow-key nudge moved: the next nudge merges with it.
    nudged: Option<String>,
}

/// The size of the resize handle at a Visual's bottom-right corner, in screen points.
const HANDLE: f32 = 10.0;
/// Largest coordinate the engine accepts.
const MAX: i32 = metriccraft_engine::MAX_COORD;

impl Canvas {
    /// Screen points per logical pixel for `page` in `space`.
    fn scale(&self, page: &Page, space: Vec2) -> f32 {
        let (w, h) = (page.width.max(1) as f32, page.height.max(1) as f32);
        let s = match self.zoom {
            Zoom::FitPage => ((space.x - 32.0) / w).min((space.y - 32.0) / h),
            Zoom::FitWidth => (space.x - 32.0) / w,
            Zoom::Fixed(s) => s,
        };
        s.clamp(0.05, 8.0)
    }

    /// Draws `page` with its Visuals' `scenes`; returns what to run.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        t: &Tokens,
        page: &Page,
        scenes: &[(String, Scene)],
    ) -> Vec<Request> {
        let mut out = Vec::new();
        if self
            .selected
            .as_ref()
            .is_some_and(|id| !page.visuals.iter().any(|v| &v.id == id))
        {
            self.selected = None;
        }
        let space = ui.available_size();
        let scale = self.scale(page, space);
        self.last_scale = scale;
        let size = Vec2::new(page.width.max(1) as f32, page.height.max(1) as f32) * scale;
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            let full = Vec2::new(size.x.max(space.x - 16.0), size.y.max(space.y - 16.0));
            let (area, bg) = ui.allocate_exact_size(full + Vec2::splat(16.0), Sense::click());
            let origin = Pos2::new(
                area.left() + ((area.width() - size.x) / 2.0).max(16.0),
                area.top() + ((area.height() - size.y) / 2.0).max(16.0),
            );
            let sheet = Rect::from_min_size(origin, size);
            let painter = ui.painter_at(area);
            painter.rect_filled(sheet, 0.0, egui::Color32::WHITE);
            painter.rect_stroke(
                sheet,
                0.0,
                Stroke::new(1.0, t.border),
                egui::StrokeKind::Outside,
            );
            if bg.clicked() {
                self.selected = None;
                out.push(Request::Command(
                    "selection.clear",
                    json!({"page": page.id}),
                ));
            }

            let mut visuals: Vec<_> = page.visuals.iter().collect();
            visuals.sort_by(|a, b| a.z.cmp(&b.z).then_with(|| a.id.cmp(&b.id)));
            for v in visuals {
                let (x, y, w, h) = match &self.drag {
                    Some(d) if d.visual == v.id => preview(d),
                    _ => (v.x, v.y, v.w, v.h),
                };
                let rect = Rect::from_min_size(
                    origin + Vec2::new(x as f32, y as f32) * scale,
                    Vec2::new(w as f32, h as f32) * scale,
                );
                if let Some((_, scene)) = scenes.iter().find(|(id, _)| id == &v.id) {
                    let sp = painter.with_clip_rect(rect.intersect(sheet));
                    crate::painter::paint(&sp, scene, rect.min, scale);
                }
                let id = ui.id().with(("visual", &v.id));
                let r = ui.interact(rect, id, Sense::click_and_drag());
                let label = Str::VisualOf.fill(&[visual_name(v.kind).text()]);
                r.widget_info(|| WidgetInfo::labeled(WidgetType::Other, true, &label));
                let selected = self.selected.as_deref() == Some(v.id.as_str());
                if selected || r.hovered() {
                    let c = if selected { t.accent } else { t.border };
                    painter.rect_stroke(
                        rect,
                        0.0,
                        Stroke::new(if selected { 2.0 } else { 1.0 }, c),
                        egui::StrokeKind::Outside,
                    );
                }
                let handle =
                    Rect::from_min_size(rect.max - Vec2::splat(HANDLE), Vec2::splat(HANDLE));
                if selected {
                    painter.rect_filled(handle, 1.0, t.accent);
                }
                if r.drag_started() {
                    let on_handle = r
                        .interact_pointer_pos()
                        .is_some_and(|p| handle.expand(2.0).contains(p));
                    self.selected = Some(v.id.clone());
                    self.nudged = None;
                    self.drag = Some(Drag {
                        visual: v.id.clone(),
                        grab: if on_handle { Grab::Resize } else { Grab::Move },
                        from: (v.x, v.y, v.w, v.h),
                        delta: Vec2::ZERO,
                    });
                }
                if r.dragged()
                    && let Some(d) = &mut self.drag
                    && d.visual == v.id
                {
                    d.delta += r.drag_delta() / scale;
                }
                if r.drag_stopped()
                    && let Some(d) = self.drag.take()
                {
                    let (x, y, w, h) = preview(&d);
                    if (x, y, w, h) != d.from {
                        out.push(match d.grab {
                            Grab::Move => Request::Command(
                                "visual.move",
                                json!({"page": page.id, "visual": d.visual, "x": x, "y": y}),
                            ),
                            Grab::Resize => Request::Command(
                                "visual.resize",
                                json!({"page": page.id, "visual": d.visual, "w": w, "h": h}),
                            ),
                        });
                    }
                }
                if r.clicked() {
                    self.selected = Some(v.id.clone());
                    self.nudged = None;
                    if let Some(p) = r.interact_pointer_pos() {
                        let at = (p - rect.min) / scale;
                        let add = ui.input(|i| i.modifiers.command);
                        out.push(Request::Click {
                            visual: v.id.clone(),
                            x: at.x,
                            y: at.y,
                            add,
                        });
                    }
                }
            }
        });
        out.extend(self.keys(ui, page));
        out
    }

    /// Arrow keys nudge the selected Visual (1 px, Shift 10 px); Delete removes it.
    fn keys(&mut self, ui: &Ui, page: &Page) -> Vec<Request> {
        let Some(id) = self.selected.clone() else {
            return Vec::new();
        };
        if ui.ctx().egui_wants_keyboard_input() {
            return Vec::new();
        }
        let Some(v) = page.visuals.iter().find(|v| v.id == id) else {
            return Vec::new();
        };
        let (dx, dy, delete, shift) = ui.input(|i| {
            let k = |key| i.key_pressed(key);
            (
                i32::from(k(Key::ArrowRight)) - i32::from(k(Key::ArrowLeft)),
                i32::from(k(Key::ArrowDown)) - i32::from(k(Key::ArrowUp)),
                k(Key::Delete),
                i.modifiers.shift,
            )
        });
        if delete {
            self.selected = None;
            self.nudged = None;
            return vec![Request::Command(
                "visual.delete",
                json!({"page": page.id, "visual": id}),
            )];
        }
        if dx == 0 && dy == 0 {
            return Vec::new();
        }
        let step = if shift { 10 } else { 1 };
        let x = v.x.saturating_add(dx * step).clamp(0, MAX);
        let y = v.y.saturating_add(dy * step).clamp(0, MAX);
        let merge = self.nudged.as_deref() == Some(id.as_str());
        self.nudged = Some(id.clone());
        vec![Request::Command(
            "visual.move",
            json!({"page": page.id, "visual": id, "x": x, "y": y, "merge": merge}),
        )]
    }
}

/// A Visual's geometry while dragged.
fn preview(d: &Drag) -> (i32, i32, i32, i32) {
    let (x, y, w, h) = d.from;
    let (dx, dy) = (d.delta.x.round() as i32, d.delta.y.round() as i32);
    match d.grab {
        Grab::Move => (
            x.saturating_add(dx).clamp(0, MAX),
            y.saturating_add(dy).clamp(0, MAX),
            w,
            h,
        ),
        Grab::Resize => (
            x,
            y,
            w.saturating_add(dx).clamp(1, MAX),
            h.saturating_add(dy).clamp(1, MAX),
        ),
    }
}

/// A Visual type's display name.
pub fn visual_name(kind: metriccraft_visuals::visual::VisualType) -> Str {
    use metriccraft_visuals::visual::VisualType as V;
    match kind {
        V::Table => Str::VisTable,
        V::Matrix => Str::VisMatrix,
        V::Card => Str::VisCard,
        V::ClusteredBar => Str::VisBar,
        V::ClusteredColumn => Str::VisColumn,
        V::Line => Str::VisLine,
        V::Slicer => Str::VisSlicer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_clamp_to_the_engines_bounds() {
        let d = Drag {
            visual: "v".into(),
            grab: Grab::Move,
            from: (5, 5, 10, 10),
            delta: Vec2::new(-20.0, 3.4),
        };
        assert_eq!(preview(&d), (0, 8, 10, 10));
        let d = Drag {
            grab: Grab::Resize,
            delta: Vec2::new(-50.0, 1e12),
            ..d
        };
        assert_eq!(preview(&d), (5, 5, 1, MAX));
    }
}
