//! Hit-testing and dimming over a [`Scene`] (M4.5): what a click at a point picks,
//! and a Selection's source Visual with its unselected marks faded. Both read the marks' [`Tag`]s,
//! so every renderer agrees on what a click selects.

use crate::draw::{Align, Cmd, Color, Scene, Tag};
use crate::text;

/// How close to a line series a click must be to pick it, logical pixels.
pub const PATH_SLOP: f32 = 4.0;

/// A dimmed mark keeps this fraction of its alpha.
pub const DIM: f32 = 0.35;

/// An axis-aligned box: left, top, right, bottom.
#[derive(Clone, Copy, Debug)]
struct Box2 {
    l: f32,
    t: f32,
    r: f32,
    b: f32,
}

impl Box2 {
    fn of(x: f32, y: f32, w: f32, h: f32) -> Box2 {
        Box2 {
            l: x.min(x + w),
            t: y.min(y + h),
            r: x.max(x + w),
            b: y.max(y + h),
        }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        (self.l..=self.r).contains(&x) && (self.t..=self.b).contains(&y)
    }

    fn intersect(&self, o: &Box2) -> Box2 {
        Box2 {
            l: self.l.max(o.l),
            t: self.t.max(o.t),
            r: self.r.min(o.r),
            b: self.b.min(o.b),
        }
    }
}

/// The topmost tagged mark (later commands are on top) at (`x`, `y`), honouring clips: a Rect
/// by its box, a Text by its measured box, a Path within [`PATH_SLOP`] of a segment.
pub fn hit(scene: &Scene, x: f32, y: f32) -> Option<Tag> {
    if !x.is_finite() || !y.is_finite() {
        return None;
    }
    let mut clips: Vec<Box2> = Vec::new();
    let mut found = None;
    for c in &scene.cmds {
        match c {
            Cmd::PushClip { x, y, w, h } => {
                let b = Box2::of(*x, *y, *w, *h);
                let b = clips.last().map_or(b, |outer| outer.intersect(&b));
                clips.push(b);
            }
            Cmd::PopClip => {
                clips.pop();
            }
            _ => {
                let Some(tag) = tag(c) else { continue };
                if clips.last().is_some_and(|clip| !clip.contains(x, y)) {
                    continue;
                }
                if covers(c, x, y) {
                    found = Some(tag);
                }
            }
        }
    }
    found
}

/// A mark's tag.
fn tag(c: &Cmd) -> Option<Tag> {
    match c {
        Cmd::Rect { tag, .. } | Cmd::Path { tag, .. } | Cmd::Text { tag, .. } => *tag,
        _ => None,
    }
}

/// Whether mark `c` covers (`x`, `y`).
fn covers(c: &Cmd, x: f32, y: f32) -> bool {
    match c {
        Cmd::Rect {
            x: rx, y: ry, w, h, ..
        } => Box2::of(*rx, *ry, *w, *h).contains(x, y),
        Cmd::Text {
            x: tx,
            y: ty,
            text: t,
            size,
            weight,
            align,
            ..
        } => {
            let w = text::width(t, *size, *weight);
            let (ascent, descent) = text::ascent_descent(*size, *weight);
            let left = match align {
                Align::Start => *tx,
                Align::Middle => *tx - w / 2.0,
                Align::End => *tx - w,
            };
            Box2::of(left, *ty - ascent, w, ascent + descent).contains(x, y)
        }
        Cmd::Path { points, width, .. } => {
            let slop = PATH_SLOP.max(width / 2.0);
            match points.as_slice() {
                [only] => dist(*only, *only, (x, y)) <= slop,
                _ => points
                    .windows(2)
                    .any(|s| matches!(s, [a, b] if dist(*a, *b, (x, y)) <= slop)),
            }
        }
        _ => false,
    }
}

/// The distance from `p` to the segment `a`–`b`.
fn dist(a: (f32, f32), b: (f32, f32), p: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

/// Fades every tagged mark that `selected` rejects (its colours keep [`DIM`] of their alpha):
/// how a Selection's source Visual shows what is picked. Untagged commands are left alone.
pub fn dim(scene: &mut Scene, selected: impl Fn(Tag) -> bool) {
    let fade = |c: &mut Color| {
        c.a = (f32::from(c.a) * DIM).round().clamp(0.0, 255.0) as u8;
    };
    for c in &mut scene.cmds {
        match c {
            Cmd::Rect {
                fill,
                stroke,
                tag: Some(t),
                ..
            } if !selected(*t) => {
                if let Some(f) = fill {
                    fade(f);
                }
                if let Some((s, _)) = stroke {
                    fade(s);
                }
            }
            Cmd::Path {
                color,
                tag: Some(t),
                ..
            }
            | Cmd::Text {
                color,
                tag: Some(t),
                ..
            } if !selected(*t) => fade(color),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Weight;

    fn t(series: u32, point: u32) -> Option<Tag> {
        Some(Tag { series, point })
    }

    fn rect(x: f32, y: f32, w: f32, h: f32, tag: Option<Tag>) -> Cmd {
        Cmd::Rect {
            x,
            y,
            w,
            h,
            fill: Some(Color::hex(0x3a6ea5)),
            stroke: None,
            tag,
        }
    }

    #[test]
    fn rects_topmost_wins_and_untagged_marks_are_ignored() {
        let mut s = Scene::new(200.0, 100.0);
        s.push(rect(0.0, 0.0, 100.0, 100.0, t(0, 0)));
        s.push(rect(50.0, 0.0, 100.0, 100.0, t(0, 1)));
        s.push(rect(60.0, 0.0, 10.0, 10.0, None));
        // Negative sizes are boxes too.
        s.push(rect(200.0, 100.0, -20.0, -20.0, t(1, 5)));
        assert_eq!(hit(&s, 10.0, 10.0), t(0, 0));
        assert_eq!(
            hit(&s, 60.0, 5.0),
            t(0, 1),
            "later is on top; untagged doesn't block"
        );
        assert_eq!(hit(&s, 190.0, 90.0), t(1, 5));
        assert_eq!(hit(&s, 160.0, 50.0), None);
        assert_eq!(hit(&s, f32::NAN, 5.0), None);
        assert_eq!(hit(&s, f32::INFINITY, 5.0), None);
    }

    #[test]
    fn clips_hide_what_they_cut() {
        let mut s = Scene::new(200.0, 100.0);
        s.push(Cmd::PushClip {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 50.0,
        });
        s.push(Cmd::PushClip {
            x: 20.0,
            y: 0.0,
            w: 200.0,
            h: 200.0,
        });
        s.push(rect(0.0, 0.0, 200.0, 200.0, t(0, 0)));
        s.push(Cmd::PopClip);
        s.push(Cmd::PopClip);
        // An unbalanced pop is harmless.
        s.push(Cmd::PopClip);
        assert_eq!(hit(&s, 50.0, 25.0), t(0, 0));
        assert_eq!(hit(&s, 10.0, 25.0), None, "outside the inner clip");
        assert_eq!(hit(&s, 50.0, 75.0), None, "outside the outer clip");
        assert_eq!(hit(&s, 150.0, 25.0), None);
    }

    #[test]
    fn text_by_its_measured_box_and_paths_by_distance() {
        let mut s = Scene::new(300.0, 200.0);
        for (align, x, p) in [(Align::Start, 10.0, 0), (Align::End, 290.0, 1)] {
            s.push(Cmd::Text {
                x,
                y: 20.0,
                text: "North".into(),
                size: 12.0,
                weight: Weight::Regular,
                color: Color::hex(0),
                align,
                tag: t(0, p),
            });
        }
        let w = text::width("North", 12.0, Weight::Regular);
        assert_eq!(hit(&s, 12.0, 15.0), t(0, 0));
        assert_eq!(hit(&s, 10.0 + w + 2.0, 15.0), None);
        assert_eq!(hit(&s, 288.0, 15.0), t(0, 1));
        assert_eq!(hit(&s, 288.0, 40.0), None, "below the descent");
        s.push(Cmd::Path {
            points: vec![(0.0, 100.0), (100.0, 100.0), (100.0, 200.0)],
            color: Color::hex(0),
            width: 2.0,
            tag: t(3, 0),
        });
        s.push(Cmd::Path {
            points: vec![(200.0, 150.0)],
            color: Color::hex(0),
            width: 2.0,
            tag: t(4, 0),
        });
        s.push(Cmd::Path {
            points: vec![],
            color: Color::hex(0),
            width: 2.0,
            tag: t(5, 0),
        });
        assert_eq!(hit(&s, 50.0, 103.0), t(3, 0));
        assert_eq!(hit(&s, 103.0, 150.0), t(3, 0));
        assert_eq!(hit(&s, 50.0, 106.0), None);
        assert_eq!(hit(&s, 202.0, 151.0), t(4, 0));
    }

    #[test]
    fn dim_fades_only_rejected_tagged_marks() {
        let mut s = Scene::new(100.0, 100.0);
        s.push(rect(0.0, 0.0, 10.0, 10.0, t(0, 0)));
        s.push(rect(0.0, 0.0, 10.0, 10.0, t(0, 1)));
        s.push(rect(0.0, 0.0, 10.0, 10.0, None));
        s.push(Cmd::Text {
            x: 0.0,
            y: 0.0,
            text: "x".into(),
            size: 12.0,
            weight: Weight::Regular,
            color: Color::hex(0).with_alpha(200),
            align: Align::Start,
            tag: t(0, 1),
        });
        dim(&mut s, |t| t.point == 0);
        let alphas: Vec<u8> = s
            .cmds
            .iter()
            .map(|c| match c {
                Cmd::Rect { fill: Some(f), .. } => f.a,
                Cmd::Text { color, .. } => color.a,
                _ => 0,
            })
            .collect();
        assert_eq!(alphas, [255, 89, 255, 70]);
    }
}
