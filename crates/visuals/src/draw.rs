//! Draw commands (ADR 0007): what a Visual's layout produces and every renderer consumes.

use std::fmt::Write;

/// An sRGB colour with straight (not premultiplied) alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    /// An opaque colour from `0xRRGGBB`.
    pub const fn hex(rgb: u32) -> Color {
        Color {
            r: (rgb >> 16) as u8,
            g: (rgb >> 8) as u8,
            b: rgb as u8,
            a: 255,
        }
    }

    /// The same colour with alpha `a`.
    pub const fn with_alpha(self, a: u8) -> Color {
        Color { a, ..self }
    }
}

/// The bundled font's two weights.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    Regular,
    Bold,
}

/// Horizontal anchoring of a text run at its `x`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Middle,
    End,
}

/// The data point a mark shows (for hit-testing and dimming, M4.5): a series and a point index
/// into the Visual's query result.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Tag {
    pub series: u32,
    pub point: u32,
}

/// One draw command, in logical pixels, y down.
#[derive(Clone, Debug, PartialEq)]
pub enum Cmd {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        fill: Option<Color>,
        /// Colour and width.
        stroke: Option<(Color, f32)>,
        tag: Option<Tag>,
    },
    Line {
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        color: Color,
        width: f32,
    },
    /// A polyline (a line series): stroked, never filled.
    Path {
        points: Vec<(f32, f32)>,
        color: Color,
        width: f32,
        tag: Option<Tag>,
    },
    /// Text on one line; `y` is the baseline.
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        weight: Weight,
        color: Color,
        align: Align,
        tag: Option<Tag>,
    },
    /// Clips later commands to a rectangle (intersected with any clip already pushed).
    PushClip {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
    PopClip,
}

/// A laid-out Visual (or Page): its size and draw commands, painted in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub width: f32,
    pub height: f32,
    pub cmds: Vec<Cmd>,
}

impl Scene {
    pub fn new(width: f32, height: f32) -> Scene {
        Scene {
            width,
            height,
            cmds: Vec::new(),
        }
    }

    pub fn push(&mut self, cmd: Cmd) {
        self.cmds.push(cmd);
    }

    /// The commands as stable text, one per line, numbers to 0.01: what snapshot tests commit.
    pub fn to_text(&self) -> String {
        let mut out = format!("scene {} {}\n", n(self.width), n(self.height));
        for c in &self.cmds {
            match c {
                Cmd::Rect {
                    x,
                    y,
                    w,
                    h,
                    fill,
                    stroke,
                    tag,
                } => {
                    let _ = write!(out, "rect {} {} {} {}", n(*x), n(*y), n(*w), n(*h));
                    if let Some(f) = fill {
                        let _ = write!(out, " fill {}", col(*f));
                    }
                    if let Some((c, w)) = stroke {
                        let _ = write!(out, " stroke {} {}", col(*c), n(*w));
                    }
                    tag_text(&mut out, *tag);
                }
                Cmd::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    color,
                    width,
                } => {
                    let _ = write!(
                        out,
                        "line {} {} {} {} {} {}",
                        n(*x1),
                        n(*y1),
                        n(*x2),
                        n(*y2),
                        col(*color),
                        n(*width)
                    );
                }
                Cmd::Path {
                    points,
                    color,
                    width,
                    tag,
                } => {
                    let _ = write!(out, "path {} {}", col(*color), n(*width));
                    for (x, y) in points {
                        let _ = write!(out, " {},{}", n(*x), n(*y));
                    }
                    tag_text(&mut out, *tag);
                }
                Cmd::Text {
                    x,
                    y,
                    text,
                    size,
                    weight,
                    color,
                    align,
                    tag,
                } => {
                    let _ = write!(
                        out,
                        "text {} {} {} {:?} {} {:?} {:?}",
                        n(*x),
                        n(*y),
                        n(*size),
                        weight,
                        col(*color),
                        align,
                        text
                    );
                    tag_text(&mut out, *tag);
                }
                Cmd::PushClip { x, y, w, h } => {
                    let _ = write!(out, "clip {} {} {} {}", n(*x), n(*y), n(*w), n(*h));
                }
                Cmd::PopClip => out.push_str("unclip"),
            }
            out.push('\n');
        }
        out
    }
}

impl Scene {
    /// A Page: each Visual's Scene placed at its (x, y) and clipped to its own size, in order
    /// (later on top). Tags keep their values; hit-test a Visual's own Scene, not the Page.
    pub fn compose(width: f32, height: f32, parts: &[(f32, f32, &Scene)]) -> Scene {
        let mut page = Scene::new(width, height);
        for (dx, dy, s) in parts {
            let (dx, dy) = (*dx, *dy);
            page.push(Cmd::PushClip {
                x: dx,
                y: dy,
                w: s.width,
                h: s.height,
            });
            for c in &s.cmds {
                page.push(match c.clone() {
                    Cmd::Rect {
                        x,
                        y,
                        w,
                        h,
                        fill,
                        stroke,
                        tag,
                    } => Cmd::Rect {
                        x: x + dx,
                        y: y + dy,
                        w,
                        h,
                        fill,
                        stroke,
                        tag,
                    },
                    Cmd::Line {
                        x1,
                        y1,
                        x2,
                        y2,
                        color,
                        width,
                    } => Cmd::Line {
                        x1: x1 + dx,
                        y1: y1 + dy,
                        x2: x2 + dx,
                        y2: y2 + dy,
                        color,
                        width,
                    },
                    Cmd::Path {
                        points,
                        color,
                        width,
                        tag,
                    } => Cmd::Path {
                        points: points.iter().map(|(x, y)| (x + dx, y + dy)).collect(),
                        color,
                        width,
                        tag,
                    },
                    Cmd::Text {
                        x,
                        y,
                        text,
                        size,
                        weight,
                        color,
                        align,
                        tag,
                    } => Cmd::Text {
                        x: x + dx,
                        y: y + dy,
                        text,
                        size,
                        weight,
                        color,
                        align,
                        tag,
                    },
                    Cmd::PushClip { x, y, w, h } => Cmd::PushClip {
                        x: x + dx,
                        y: y + dy,
                        w,
                        h,
                    },
                    Cmd::PopClip => Cmd::PopClip,
                });
            }
            // Close any clips the part left open, then its own.
            let opened = s.cmds.iter().fold(0i64, |n, c| match c {
                Cmd::PushClip { .. } => n + 1,
                Cmd::PopClip => (n - 1).max(0),
                _ => n,
            });
            for _ in 0..=opened {
                page.push(Cmd::PopClip);
            }
        }
        page
    }
}

fn tag_text(out: &mut String, tag: Option<Tag>) {
    if let Some(t) = tag {
        let _ = write!(out, " #{}:{}", t.series, t.point);
    }
}

/// A number to 0.01, without trailing zeros (`-0` as `0`).
fn n(v: f32) -> String {
    if !v.is_finite() {
        return format!("{v}");
    }
    let r = (f64::from(v) * 100.0).round() / 100.0;
    let r = if r == 0.0 { 0.0 } else { r };
    format!("{r}")
}

fn col(c: Color) -> String {
    if c.a == 255 {
        format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
    } else {
        format!("#{:02x}{:02x}{:02x}{:02x}", c.r, c.g, c.b, c.a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_offsets_and_clips_each_part() {
        let mut a = Scene::new(10.0, 10.0);
        a.push(Cmd::PushClip {
            x: 0.0,
            y: 0.0,
            w: 5.0,
            h: 5.0,
        });
        a.push(Cmd::Line {
            x1: 0.0,
            y1: 0.0,
            x2: 1.0,
            y2: 1.0,
            color: Color::hex(0),
            width: 1.0,
        });
        let page = Scene::compose(100.0, 50.0, &[(20.0, 30.0, &a)]);
        assert_eq!(
            page.to_text(),
            "scene 100 50\nclip 20 30 10 10\nclip 20 30 5 5\nline 20 30 21 31 #000000 1\nunclip\nunclip\n"
        );
    }

    #[test]
    fn text_form_is_stable() {
        let mut s = Scene::new(100.0, 50.0);
        s.push(Cmd::Rect {
            x: 0.004,
            y: -0.001,
            w: 10.125,
            h: 5.0,
            fill: Some(Color::hex(0x2a6fdb)),
            stroke: None,
            tag: Some(Tag {
                series: 0,
                point: 3,
            }),
        });
        s.push(Cmd::Text {
            x: 1.0,
            y: 2.0,
            text: "a \"b\"".into(),
            size: 12.0,
            weight: Weight::Bold,
            color: Color::hex(0).with_alpha(128),
            align: Align::End,
            tag: None,
        });
        s.push(Cmd::Line {
            x1: f32::NAN,
            y1: 0.0,
            x2: f32::INFINITY,
            y2: 1.0,
            color: Color::hex(0xffffff),
            width: 1.0,
        });
        assert_eq!(
            s.to_text(),
            "scene 100 50\n\
             rect 0 0 10.13 5 fill #2a6fdb #0:3\n\
             text 1 2 12 Bold #00000080 End \"a \\\"b\\\"\"\n\
             line NaN 0 inf 1 #ffffff 1\n"
        );
    }
}
