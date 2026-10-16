//! The icon set: monochrome line icons drawn in code on a 16×16 grid, tinted by the
//! App theme. Original work; no vendor artwork (AGENTS.md).

use egui::{
    Color32, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetInfo, WidgetType,
};

/// One stroke of an icon, in 16×16 grid units.
enum Mark {
    Path(&'static [(f32, f32)]),
    Closed(&'static [(f32, f32)]),
    Rect(f32, f32, f32, f32),
    Circle(f32, f32, f32),
    Dot(f32, f32),
}

use Mark::{Circle, Closed, Dot, Path, Rect as Box};

/// An icon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Undo,
    Redo,
    File,
    GetData,
    Refresh,
    RemoveColumns,
    Merge,
    NewPage,
    NewVisual,
    NewMeasure,
    NewColumn,
    Relationship,
    Report,
    Table,
    Model,
    Filter,
    Format,
    Sort,
    Properties,
    Close,
    Command,
}

impl Icon {
    /// The icon for a command, if it has one.
    pub fn for_command(id: &str) -> Option<Icon> {
        Some(match id {
            "edit.undo" => Icon::Undo,
            "edit.redo" => Icon::Redo,
            "home.getData" => Icon::GetData,
            "home.refresh" => Icon::Refresh,
            "home.removeColumns" => Icon::RemoveColumns,
            "home.mergeQueries" => Icon::Merge,
            "home.closeApply" => Icon::Close,
            "insert.newPage" => Icon::NewPage,
            "insert.newVisual" => Icon::NewVisual,
            "modeling.newMeasure" => Icon::NewMeasure,
            "modeling.newColumn" => Icon::NewColumn,
            "modeling.newRelationship" | "modeling.autodetectRelationships" => Icon::Relationship,
            "columnTools.format" | "columnTools.dataType" => Icon::Format,
            "columnTools.sortByColumn" => Icon::Sort,
            "columnTools.summarization" => Icon::Properties,
            _ if id.starts_with("file.") => Icon::File,
            _ => return None,
        })
    }

    fn marks(self) -> &'static [Mark] {
        match self {
            Icon::Undo => &[
                Path(&[(5.0, 3.0), (2.0, 6.0), (5.0, 9.0)]),
                Path(&[
                    (2.0, 6.0),
                    (10.0, 6.0),
                    (13.0, 8.5),
                    (13.0, 10.5),
                    (10.0, 13.0),
                    (6.0, 13.0),
                ]),
            ],
            Icon::Redo => &[
                Path(&[(11.0, 3.0), (14.0, 6.0), (11.0, 9.0)]),
                Path(&[
                    (14.0, 6.0),
                    (6.0, 6.0),
                    (3.0, 8.5),
                    (3.0, 10.5),
                    (6.0, 13.0),
                    (10.0, 13.0),
                ]),
            ],
            Icon::File => &[
                Closed(&[
                    (3.5, 1.5),
                    (10.0, 1.5),
                    (13.0, 4.5),
                    (13.0, 14.5),
                    (3.5, 14.5),
                ]),
                Path(&[(10.0, 1.5), (10.0, 4.5), (13.0, 4.5)]),
            ],
            Icon::GetData => &[
                Box(3.0, 3.5, 10.0, 9.5),
                Path(&[(3.0, 6.5), (13.0, 6.5)]),
                Path(&[(3.0, 9.5), (13.0, 9.5)]),
                Path(&[(8.0, 6.5), (8.0, 13.0)]),
            ],
            Icon::Refresh => &[
                Path(&[
                    (13.0, 8.0),
                    (12.0, 11.0),
                    (9.0, 13.0),
                    (6.0, 13.0),
                    (3.5, 11.0),
                    (2.5, 8.0),
                    (3.5, 5.0),
                    (6.0, 3.0),
                    (9.0, 3.0),
                    (11.5, 4.5),
                ]),
                Path(&[(12.0, 1.5), (12.0, 5.0), (8.5, 5.0)]),
            ],
            Icon::RemoveColumns => &[
                Box(2.0, 2.5, 4.0, 11.0),
                Box(10.0, 2.5, 4.0, 11.0),
                Path(&[(6.5, 6.0), (9.5, 10.0)]),
                Path(&[(9.5, 6.0), (6.5, 10.0)]),
            ],
            Icon::Merge => &[
                Path(&[(2.0, 3.0), (6.0, 3.0), (9.0, 8.0), (14.0, 8.0)]),
                Path(&[(2.0, 13.0), (6.0, 13.0), (9.0, 8.0)]),
                Path(&[(11.5, 5.5), (14.0, 8.0), (11.5, 10.5)]),
            ],
            Icon::NewPage => &[
                Box(2.5, 3.5, 11.0, 9.0),
                Path(&[(8.0, 5.5), (8.0, 10.5)]),
                Path(&[(5.5, 8.0), (10.5, 8.0)]),
            ],
            Icon::NewVisual | Icon::Report => &[
                Path(&[(2.0, 14.0), (14.0, 14.0)]),
                Box(3.0, 8.0, 2.5, 6.0),
                Box(7.0, 4.0, 2.5, 10.0),
                Box(11.0, 10.0, 2.5, 4.0),
            ],
            Icon::NewMeasure => &[
                Path(&[
                    (11.0, 2.5),
                    (8.5, 2.5),
                    (7.0, 4.0),
                    (6.0, 13.0),
                    (4.5, 14.0),
                ]),
                Path(&[(4.5, 7.0), (10.0, 7.0)]),
                Path(&[(9.5, 9.5), (13.5, 13.5)]),
                Path(&[(13.5, 9.5), (9.5, 13.5)]),
            ],
            Icon::NewColumn => &[
                Box(2.0, 2.5, 5.0, 11.0),
                Path(&[(11.5, 5.0), (11.5, 11.0)]),
                Path(&[(8.5, 8.0), (14.5, 8.0)]),
            ],
            Icon::Relationship | Icon::Model => &[
                Box(1.5, 2.0, 5.5, 4.5),
                Box(9.0, 9.5, 5.5, 4.5),
                Path(&[(4.25, 6.5), (4.25, 11.75), (9.0, 11.75)]),
                Dot(9.0, 11.75),
            ],
            Icon::Table => &[
                Box(2.0, 2.5, 12.0, 11.0),
                Path(&[(2.0, 6.0), (14.0, 6.0)]),
                Path(&[(2.0, 9.75), (14.0, 9.75)]),
                Path(&[(6.0, 2.5), (6.0, 13.5)]),
                Path(&[(10.0, 2.5), (10.0, 13.5)]),
            ],
            Icon::Filter => &[Closed(&[
                (2.0, 3.0),
                (14.0, 3.0),
                (9.5, 8.5),
                (9.5, 13.5),
                (6.5, 12.0),
                (6.5, 8.5),
            ])],
            Icon::Format => &[
                Path(&[(3.0, 13.0), (8.0, 3.0), (13.0, 13.0)]),
                Path(&[(5.0, 9.0), (11.0, 9.0)]),
            ],
            Icon::Sort => &[
                Path(&[(4.5, 2.5), (4.5, 13.5)]),
                Path(&[(2.0, 11.0), (4.5, 13.5), (7.0, 11.0)]),
                Path(&[(9.0, 4.0), (14.0, 4.0)]),
                Path(&[(9.0, 8.0), (13.0, 8.0)]),
                Path(&[(9.0, 12.0), (11.5, 12.0)]),
            ],
            Icon::Properties => &[
                Circle(8.0, 8.0, 2.5),
                Path(&[(8.0, 1.5), (8.0, 4.0)]),
                Path(&[(8.0, 12.0), (8.0, 14.5)]),
                Path(&[(1.5, 8.0), (4.0, 8.0)]),
                Path(&[(12.0, 8.0), (14.5, 8.0)]),
            ],
            Icon::Close => &[Path(&[(3.0, 8.5), (6.5, 12.0), (13.0, 4.0)])],
            Icon::Command => &[Circle(7.0, 7.0, 4.5), Path(&[(10.5, 10.5), (14.0, 14.0)])],
        }
    }

    /// Paints the icon into `rect` (scaled from the 16-unit grid) in `color`.
    pub fn paint(self, painter: &Painter, rect: Rect, color: Color32) {
        let k = rect.width().min(rect.height()) / 16.0;
        let at = |(x, y): (f32, f32)| rect.min + Vec2::new(x * k, y * k);
        let stroke = Stroke::new((1.4 * k).max(1.0), color);
        for mark in self.marks() {
            match *mark {
                Path(points) => {
                    let pts: Vec<Pos2> = points.iter().copied().map(at).collect();
                    painter.add(egui::Shape::line(pts, stroke));
                }
                Closed(points) => {
                    let pts: Vec<Pos2> = points.iter().copied().map(at).collect();
                    painter.add(egui::Shape::closed_line(pts, stroke));
                }
                Box(x, y, w, h) => {
                    let r = Rect::from_min_max(at((x, y)), at((x + w, y + h)));
                    painter.rect_stroke(r, 1.0 * k, stroke, egui::StrokeKind::Middle);
                }
                Circle(x, y, r) => {
                    painter.circle_stroke(at((x, y)), r * k, stroke);
                }
                Dot(x, y) => {
                    painter.circle_filled(at((x, y)), 1.4 * k, color);
                }
            }
        }
    }
}

/// A Ribbon button: the icon above the label. Its AccessKit label is `label`.
pub fn big_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    enabled: bool,
    t: &crate::theme::Tokens,
) -> Response {
    let font = egui::TextStyle::Small.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.into(), font, t.text);
    let size = Vec2::new(galley.size().x.max(32.0) + 10.0, 48.0);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let color = if enabled {
            t.text
        } else {
            t.muted.gamma_multiply(0.6)
        };
        if enabled && (response.hovered() || response.has_focus()) {
            ui.painter().rect_filled(rect, 4.0, t.hover);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                4.0,
                Stroke::new(1.5, t.accent),
                egui::StrokeKind::Inside,
            );
        }
        let icon_rect = Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 15.0),
            Vec2::splat(20.0),
        );
        icon.paint(ui.painter(), icon_rect, color);
        let pos = egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.bottom() - galley.size().y - 4.0,
        );
        ui.painter().galley(pos, galley, color);
    }
    response
}

/// Gives `r` (a control showing a symbol or nothing) the label `label` for screen readers,
/// and as its tooltip.
pub fn name(r: egui::Response, typ: egui::WidgetType, label: &str) -> egui::Response {
    r.widget_info(|| egui::WidgetInfo::labeled(typ, r.enabled(), label));
    r.on_hover_text(label)
}

/// A small square button with just an icon; `label` is its AccessKit label and tooltip.
pub fn icon_button(
    ui: &mut Ui,
    icon: Icon,
    label: &str,
    enabled: bool,
    t: &crate::theme::Tokens,
) -> Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(26.0), sense);
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, enabled, label));
    if ui.is_rect_visible(rect) {
        let color = if enabled {
            t.text
        } else {
            t.muted.gamma_multiply(0.6)
        };
        if enabled && (response.hovered() || response.has_focus()) {
            ui.painter().rect_filled(rect, 4.0, t.hover);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                4.0,
                Stroke::new(1.5, t.accent),
                egui::StrokeKind::Inside,
            );
        }
        icon.paint(ui.painter(), rect.shrink(4.0), color);
    }
    response.on_hover_text(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 21] = [
        Icon::Undo,
        Icon::Redo,
        Icon::File,
        Icon::GetData,
        Icon::Refresh,
        Icon::RemoveColumns,
        Icon::Merge,
        Icon::NewPage,
        Icon::NewVisual,
        Icon::NewMeasure,
        Icon::NewColumn,
        Icon::Relationship,
        Icon::Report,
        Icon::Table,
        Icon::Model,
        Icon::Filter,
        Icon::Format,
        Icon::Sort,
        Icon::Properties,
        Icon::Close,
        Icon::Command,
    ];

    #[test]
    fn every_icon_stays_on_its_grid() {
        for icon in ALL {
            for mark in icon.marks() {
                let pts: Vec<(f32, f32)> = match *mark {
                    Path(p) | Closed(p) => p.to_vec(),
                    Box(x, y, w, h) => vec![(x, y), (x + w, y + h)],
                    Circle(x, y, r) => vec![(x - r, y - r), (x + r, y + r)],
                    Dot(x, y) => vec![(x, y)],
                };
                for (x, y) in pts {
                    assert!(
                        (0.0..=16.0).contains(&x) && (0.0..=16.0).contains(&y),
                        "{icon:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn ribbon_commands_have_icons() {
        for c in metriccraft_engine::COMMANDS
            .iter()
            .filter(|c| c.ribbon.contains(" > "))
        {
            if c.ribbon.starts_with("Transform") || c.ribbon.starts_with("Power Query") {
                continue;
            }
            assert!(Icon::for_command(c.id).is_some(), "{} has no icon", c.id);
        }
    }
}
