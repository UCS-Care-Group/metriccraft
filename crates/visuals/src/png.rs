//! Headless renderer (ADR 0007): a [`Scene`] to PNG bytes with `tiny-skia`. Text is the bundled
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
    let mut mask: Option<Mask> = None;
    for cmd in &scene.cmds {
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
                mask = clip_mask(inner, w, h, t);
            }
            Cmd::PopClip => {
                clips.pop();
                mask = match clips.last() {
                    Some(r) => clip_mask(*r, w, h, t),
                    None => None,
                };
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
                    pm.fill_path(&path, &paint_of(*f), FillRule::Winding, t, mask.as_ref());
                }
                if let Some((c, sw)) = stroke {
                    stroke_path(&mut pm, &path, *c, *sw, t, mask.as_ref());
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
                    stroke_path(&mut pm, &path, *color, *width, t, mask.as_ref());
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
                    stroke_path(&mut pm, &path, *color, *width, t, mask.as_ref());
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
                    pm.fill_path(
                        &path,
                        &paint_of(*color),
                        FillRule::Winding,
                        t,
                        mask.as_ref(),
                    );
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

/// The mask for clip rectangle `r` (logical pixels; `None`: empty).
fn clip_mask(r: Option<Rect>, w: u32, h: u32, t: Transform) -> Option<Mask> {
    let mut m = Mask::new(w, h)?;
    if let Some(r) = r {
        m.fill_path(&PathBuilder::from_rect(r), FillRule::Winding, true, t);
    }
    Some(m)
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
