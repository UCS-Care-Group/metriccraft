//! The vector Scene painter (ADR 0008): a Visual's draw commands (ADR 0007) as egui shapes,
//! scaled by the canvas zoom. Layout stays at the Visual's logical size; only painting scales,
//! so text stays sharp at any zoom. Text uses the bundled Inter the layout measured with.

use egui::{Color32, FontFamily, FontId, Painter, Pos2, Rect, Stroke, Vec2};
use opendash_visuals::draw::{Align, Cmd, Color, Scene, Weight};
use opendash_visuals::text;

/// The egui font families holding Inter (registered by [`crate::Shell::setup`]).
pub const REGULAR: &str = "Inter";
pub const BOLD: &str = "Inter Bold";

fn color(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r, c.g, c.b, c.a)
}

fn family(w: Weight) -> FontFamily {
    FontFamily::Name(
        match w {
            Weight::Regular => REGULAR,
            Weight::Bold => BOLD,
        }
        .into(),
    )
}

/// Paints `scene` with its top-left at `origin`, `scale` screen points per logical pixel, clipped
/// to the scene and to `painter`'s clip.
pub fn paint(painter: &Painter, scene: &Scene, origin: Pos2, scale: f32) {
    let at = |x: f32, y: f32| origin + Vec2::new(x, y) * scale;
    let finite = |v: f32| v.is_finite();
    let bounds = Rect::from_min_size(origin, Vec2::new(scene.width, scene.height) * scale);
    let mut clips = vec![painter.clip_rect().intersect(bounds)];
    let mut p = painter.with_clip_rect(clips.last().copied().unwrap_or(bounds));
    for cmd in &scene.cmds {
        match cmd {
            Cmd::Rect {
                x,
                y,
                w,
                h,
                fill,
                stroke,
                ..
            } => {
                if ![*x, *y, *w, *h].into_iter().all(finite) {
                    continue;
                }
                let r = Rect::from_min_size(at(*x, *y), Vec2::new(*w, *h) * scale);
                if let Some(f) = fill {
                    p.rect_filled(r, 0.0, color(*f));
                }
                if let Some((c, width)) = stroke {
                    p.rect_stroke(
                        r,
                        0.0,
                        Stroke::new(width * scale, color(*c)),
                        egui::StrokeKind::Inside,
                    );
                }
            }
            Cmd::Line {
                x1,
                y1,
                x2,
                y2,
                color: c,
                width,
            } => {
                if ![*x1, *y1, *x2, *y2].into_iter().all(finite) {
                    continue;
                }
                p.line_segment(
                    [at(*x1, *y1), at(*x2, *y2)],
                    Stroke::new(width * scale, color(*c)),
                );
            }
            Cmd::Path {
                points,
                color: c,
                width,
                ..
            } => {
                let pts: Vec<Pos2> = points
                    .iter()
                    .filter(|(x, y)| x.is_finite() && y.is_finite())
                    .map(|&(x, y)| at(x, y))
                    .collect();
                if pts.len() >= 2 {
                    p.add(egui::Shape::line(
                        pts,
                        Stroke::new(width * scale, color(*c)),
                    ));
                }
            }
            Cmd::Text {
                x,
                y,
                text: s,
                size,
                weight,
                color: c,
                align,
                ..
            } => {
                if !(x.is_finite() && y.is_finite() && size.is_finite()) || s.is_empty() {
                    continue;
                }
                let em = text::em_size(*size, *weight) * scale;
                if em < 0.5 {
                    continue;
                }
                let galley =
                    p.layout_no_wrap(s.clone(), FontId::new(em, family(*weight)), color(*c));
                // The galley's top-left from the baseline: egui puts glyph baselines at `pos.y`.
                let baseline = galley
                    .rows
                    .first()
                    .and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y))
                    .unwrap_or(em);
                let w = galley.size().x;
                let dx = match align {
                    Align::Start => 0.0,
                    Align::Middle => -w / 2.0,
                    Align::End => -w,
                };
                let pos = at(*x, *y) + Vec2::new(dx, -baseline);
                p.galley(pos, galley, color(*c));
            }
            Cmd::PushClip { x, y, w, h } => {
                let current = clips.last().copied().unwrap_or(bounds);
                let r = if [*x, *y, *w, *h].into_iter().all(finite) {
                    Rect::from_min_size(at(*x, *y), Vec2::new(*w, *h) * scale).intersect(current)
                } else {
                    current
                };
                clips.push(r);
                p = painter.with_clip_rect(r);
            }
            Cmd::PopClip => {
                if clips.len() > 1 {
                    clips.pop();
                }
                p = painter.with_clip_rect(clips.last().copied().unwrap_or(bounds));
            }
        }
    }
}
