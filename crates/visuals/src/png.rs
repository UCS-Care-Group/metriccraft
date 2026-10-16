//! Headless renderer: a [`Scene`] to PNG bytes with `tiny-skia`. Text is the bundled
//! font's glyph outlines filled as paths, so clipping treats text like any other mark.

use ab_glyph::{Font, OutlineCurve, PxScale, ScaleFont};
use tiny_skia::{FillRule, Mask, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

use crate::draw::{Align, Cmd, Color, Scene, Weight};
use crate::text;

/// Largest side of a rendered image, in pixels.
pub const MAX_SIDE: u32 = 16_384;
/// Most pixels in a rendered image (64 Mpx: 256 MB of RGBA).
pub const MAX_PIXELS: u64 = 64 << 20;

/// `scene` painted at `scale` device pixels per logical pixel on `background`, as PNG bytes.
pub fn render(scene: &Scene, scale: f32, background: Color) -> Result<Vec<u8>, String> {
    let pixmap = paint(scene, scale, background)?;
    pixmap.encode_png().map_err(|e| e.to_string())
}

/// `scene` painted into a pixmap (for tests that read pixels).
pub fn paint(scene: &Scene, scale: f32, background: Color) -> Result<Pixmap, String> {
    let side = |v: f32| -> Result<u32, String> {
        let px = (v * scale).ceil();
        if !px.is_finite() || px < 1.0 || px > MAX_SIDE as f32 {
            return Err(format!("can't render a {v} px side at scale {scale}"));
        }
        Ok(px as u32)
    };
    let (w, h) = (side(scene.width)?, side(scene.height)?);
    if u64::from(w) * u64::from(h) > MAX_PIXELS {
        return Err(format!("{w}×{h} pixels is too large to render"));
    }
    let mut pm = Pixmap::new(w, h).ok_or("can't allocate the image")?;
    pm.fill(skia_color(background));
    let t = Transform::from_scale(scale, scale);
    // Clip rectangles in logical pixels, intersected; the mask for the innermost.
    let mut clips: Vec<Option<Rect>> = Vec::new();
    let mut clip = Clip::new(w, h, scale);
    for cmd in &scene.cmds {
        let mask = clip.mask(!clips.is_empty());
        match cmd {
            Cmd::PushClip { x, y, w: cw, h: ch } => {
                let outer = clips.last().copied().flatten();
                let r = Rect::from_xywh(*x, *y, *cw, *ch);
                let inner = match (clips.is_empty(), outer, r) {
                    (true, _, r) => r,
                    (false, Some(o), Some(r)) => o.intersect(&r),
                    _ => None,
                };
                clips.push(inner);
                clip.set(inner, t);
            }
            Cmd::PopClip => {
                clips.pop();
                if let Some(r) = clips.last() {
                    clip.set(*r, t);
                }
            }
            // An empty clip hides everything until it's popped.
            _ if clips.last().is_some_and(Option::is_none) => {}
            Cmd::Rect {
                x,
                y,
                w: rw,
                h: rh,
                fill,
                stroke,
                ..
            } => {
                let Some(r) = Rect::from_xywh(*x, *y, *rw, *rh) else {
                    continue;
                };
                let path = PathBuilder::from_rect(r);
                if let Some(f) = fill {
                    pm.fill_path(&path, &paint_of(*f), FillRule::Winding, t, mask);
                }
                if let Some((c, sw)) = stroke {
                    stroke_path(&mut pm, &path, *c, *sw, t, mask);
                }
            }
            Cmd::Line {
                x1,
                y1,
                x2,
                y2,
                color,
                width,
            } => {
                let mut pb = PathBuilder::new();
                pb.move_to(*x1, *y1);
                pb.line_to(*x2, *y2);
                if let Some(path) = pb.finish() {
                    stroke_path(&mut pm, &path, *color, *width, t, mask);
                }
            }
            Cmd::Path {
                points,
                color,
                width,
                ..
            } => {
                let mut pb = PathBuilder::new();
                for (i, (x, y)) in points.iter().enumerate() {
                    if i == 0 {
                        pb.move_to(*x, *y);
                    } else {
                        pb.line_to(*x, *y);
                    }
                }
                if let Some(path) = pb.finish() {
                    stroke_path(&mut pm, &path, *color, *width, t, mask);
                }
            }
            Cmd::Text {
                x,
                y,
                text: s,
                size,
                weight,
                color,
                align,
                ..
            } => {
                if let Some(path) = text_path(s, *x, *y, *size, *weight, *align) {
                    pm.fill_path(&path, &paint_of(*color), FillRule::Winding, t, mask);
                }
            }
        }
    }
    Ok(pm)
}

fn skia_color(c: Color) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

fn paint_of(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(c.r, c.g, c.b, c.a);
    p.anti_alias = true;
    p
}

fn stroke_path(
    pm: &mut Pixmap,
    path: &tiny_skia::Path,
    c: Color,
    width: f32,
    t: Transform,
    mask: Option<&Mask>,
) {
    if !(width.is_finite() && width > 0.0) {
        return;
    }
    let stroke = Stroke {
        width: width.min(1000.0),
        ..Stroke::default()
    };
    pm.stroke_path(path, &paint_of(c), &stroke, t, mask);
}

/// The clip mask: one canvas-sized mask, allocated at the first clip and reused. Nested clips
/// intersect into one rectangle, so setting a clip only clears the previous rectangle's pixels
/// and fills the new one: the cost is the rectangles' area, not the canvas's, however many
/// Visuals a Page has.
struct Clip {
    mask: Option<Mask>,
    w: u32,
    h: u32,
    scale: f32,
    /// Device pixels the mask may have set: rows `y0..y1`, columns `x0..x1`.
    dirty: Option<(u32, u32, u32, u32)>,
}

impl Clip {
    fn new(w: u32, h: u32, scale: f32) -> Clip {
        Clip {
            mask: None,
            w,
            h,
            scale,
            dirty: None,
        }
    }

    /// The mask to paint through while `clipped`.
    fn mask(&self, clipped: bool) -> Option<&Mask> {
        self.mask.as_ref().filter(|_| clipped)
    }

    /// Makes the mask clip rectangle `r` (logical pixels; `None`: empty).
    fn set(&mut self, r: Option<Rect>, t: Transform) {
        if self.mask.is_none() {
            self.mask = Mask::new(self.w, self.h);
        }
        let Some(m) = self.mask.as_mut() else {
            return;
        };
        if let Some((x0, y0, x1, y1)) = self.dirty.take() {
            let data = m.data_mut();
            for y in y0..y1 {
                let row = (y as usize).saturating_mul(self.w as usize);
                let span = row.saturating_add(x0 as usize)..row.saturating_add(x1 as usize);
                if let Some(px) = data.get_mut(span) {
                    px.fill(0);
                }
            }
        }
        if let Some(r) = r {
            m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, true, t);
            // The rectangle in device pixels, one more each side for anti-aliasing.
            let px = |v: f32, max: u32| (v * self.scale).clamp(0.0, max as f32) as u32;
            let (x0, y0) = (px(r.left() - 1.0, self.w), px(r.top() - 1.0, self.h));
            let (x1, y1) = (px(r.right() + 1.0, self.w), px(r.bottom() + 1.0, self.h));
            self.dirty = Some((
                x0,
                y0,
                x1.saturating_add(1).min(self.w),
                y1.saturating_add(1).min(self.h),
            ));
        }
    }
}

/// The outlines of `s` as one path, anchored at `x` per `align`, baseline `y`.
fn text_path(
    s: &str,
    x: f32,
    y: f32,
    size: f32,
    weight: Weight,
    align: Align,
) -> Option<tiny_skia::Path> {
    if !(x.is_finite() && y.is_finite()) {
        return None;
    }
    let font = text::font(weight)?;
    let size = text::clamp_size(size);
    let scaled = font.as_scaled(PxScale::from(size));
    let (sx, sy) = (scaled.h_scale_factor(), scaled.v_scale_factor());
    let width = text::width(s, size, weight);
    let mut pen = match align {
        Align::Start => x,
        Align::Middle => x - width / 2.0,
        Align::End => x - width,
    };
    let mut pb = PathBuilder::new();
    let mut prev = None;
    for c in s.chars().take(text::MAX_CHARS) {
        let id = scaled.glyph_id(c);
        if let Some(p) = prev {
            pen += scaled.kern(p, id);
        }
        prev = Some(id);
        if let Some(outline) = font.outline(id) {
            let pt = |p: ab_glyph::Point| (pen + p.x * sx, y - p.y * sy);
            let mut last: Option<ab_glyph::Point> = None;
            for curve in &outline.curves {
                let start = match curve {
                    OutlineCurve::Line(a, _)
                    | OutlineCurve::Quad(a, _, _)
                    | OutlineCurve::Cubic(a, _, _, _) => *a,
                };
                if last != Some(start) {
                    if last.is_some() {
                        pb.close();
                    }
                    let (px, py) = pt(start);
                    pb.move_to(px, py);
                }
                match curve {
                    OutlineCurve::Line(_, b) => {
                        let (bx, by) = pt(*b);
                        pb.line_to(bx, by);
                        last = Some(*b);
                    }
                    OutlineCurve::Quad(_, c1, b) => {
                        let ((cx, cy), (bx, by)) = (pt(*c1), pt(*b));
                        pb.quad_to(cx, cy, bx, by);
                        last = Some(*b);
                    }
                    OutlineCurve::Cubic(_, c1, c2, b) => {
                        let ((c1x, c1y), (c2x, c2y), (bx, by)) = (pt(*c1), pt(*c2), pt(*b));
                        pb.cubic_to(c1x, c1y, c2x, c2y, bx, by);
                        last = Some(*b);
                    }
                }
            }
            if last.is_some() {
                pb.close();
            }
        }
        pen += scaled.h_advance(id);
    }
    pb.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(pm: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let c = pm.pixel(x, y).map(|p| p.demultiply());
        c.map_or([0; 4], |c| [c.red(), c.green(), c.blue(), c.alpha()])
    }

    #[test]
    fn paints_rects_text_and_clips() {
        let mut s = Scene::new(40.0, 20.0);
        s.push(Cmd::PushClip {
            x: 0.0,
            y: 0.0,
            w: 20.0,
            h: 20.0,
        });
        s.push(Cmd::Rect {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 10.0,
            fill: Some(Color::hex(0xff0000)),
            stroke: None,
            tag: None,
        });
        s.push(Cmd::PopClip);
        s.push(Cmd::Text {
            x: 20.0,
            y: 18.0,
            text: "Hi".into(),
            size: 10.0,
            weight: Weight::Bold,
            color: Color::hex(0x000000),
            align: Align::Start,
            tag: None,
        });
        let pm = paint(&s, 1.0, Color::hex(0xffffff)).unwrap();
        assert_eq!(rgba(&pm, 5, 5), [255, 0, 0, 255], "inside the clip");
        assert_eq!(rgba(&pm, 30, 5), [255, 255, 255, 255], "clipped away");
        let dark = (20..40)
            .flat_map(|x| (10..20).map(move |y| (x, y)))
            .filter(|&(x, y)| rgba(&pm, x, y)[0] < 128)
            .count();
        assert!(dark > 5, "text drew {dark} dark pixels");
        assert!(
            render(&s, 2.0, Color::hex(0xffffff))
                .unwrap()
                .starts_with(b"\x89PNG")
        );
    }

    /// A Page of 1,000 Visuals, each clipped with a nested clip inside, on the largest canvas:
    /// the clip mask is reused, not a canvas-sized mask built (and cleared) per push and pop
    /// (which took 2,000 × 64 MB of memset here).
    #[test]
    fn many_clips_render_fast() {
        let mut s = Scene::new(16_384.0, 4_096.0);
        for i in 0..1_000u16 {
            let (x, y) = (f32::from(i % 40) * 32.0, f32::from(i / 40) * 28.0);
            let rect = |fill: u32| Cmd::Rect {
                x,
                y,
                w: 40.0,
                h: 40.0,
                fill: Some(Color::hex(fill)),
                stroke: None,
                tag: None,
            };
            s.push(Cmd::PushClip {
                x,
                y,
                w: 30.0,
                h: 26.0,
            });
            s.push(rect(0x0000ff));
            s.push(Cmd::PushClip {
                x: x + 2.0,
                y: y + 2.0,
                w: 26.0,
                h: 22.0,
            });
            s.push(rect(0xff0000));
            s.push(Cmd::PopClip);
            s.push(Cmd::PopClip);
        }
        let start = std::time::Instant::now();
        let pm = paint(&s, 1.0, Color::hex(0xffffff)).unwrap();
        let took = start.elapsed();
        eprintln!("1,000 clipped visuals painted in {took:?}");
        assert!(took.as_secs_f32() < 5.0, "{took:?}");
        // Inner clip red, the outer ring blue, outside both white.
        assert_eq!(rgba(&pm, 10, 10), [255, 0, 0, 255]);
        assert_eq!(rgba(&pm, 0, 0), [0, 0, 255, 255]);
        assert_eq!(rgba(&pm, 31, 1), [255, 255, 255, 255]);
    }

    #[test]
    fn hostile_scenes_are_errors_or_ignored() {
        for (w, h, scale) in [
            (f32::NAN, 10.0, 1.0),
            (10.0, 10.0, f32::INFINITY),
            (-5.0, 10.0, 1.0),
            (1e9, 1e9, 1.0),
            (16_000.0, 16_000.0, 1.0),
            (10.0, 10.0, 0.0),
        ] {
            assert!(paint(&Scene::new(w, h), scale, Color::hex(0)).is_err());
        }
        let mut s = Scene::new(10.0, 10.0);
        for v in [f32::NAN, f32::INFINITY, -1e30, 1e30] {
            s.push(Cmd::Rect {
                x: v,
                y: 0.0,
                w: v,
                h: 5.0,
                fill: Some(Color::hex(0)),
                stroke: Some((Color::hex(0), v)),
                tag: None,
            });
            s.push(Cmd::Line {
                x1: v,
                y1: v,
                x2: 1.0,
                y2: 1.0,
                color: Color::hex(0),
                width: v,
            });
            s.push(Cmd::Text {
                x: v,
                y: 5.0,
                text: "x".repeat(10_000),
                size: v,
                weight: Weight::Regular,
                color: Color::hex(0),
                align: Align::Middle,
                tag: None,
            });
            s.push(Cmd::PushClip {
                x: v,
                y: 0.0,
                w: v,
                h: v,
            });
        }
        s.push(Cmd::PopClip);
        s.push(Cmd::PopClip);
        assert!(paint(&s, 1.0, Color::hex(0xffffff)).is_ok());
    }
}
