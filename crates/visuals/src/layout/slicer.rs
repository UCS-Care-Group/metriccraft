//! The slicer (M4.5, spec R-2): its Field's values, ascending, as a list of checkboxes or a
//! closed dropdown. Which items are selected is the Page's saved Selection, passed in; clicking
//! is the engine's (hit-testing the item marks, each tagged `Tag { series: 0, point: row }`).
//!
//! Parity uncertainties, to observe in the VM: row height and checkbox size; the dropdown's
//! wording for several items ("Multiple selections").

use opendash_core::Value;
use opendash_model::dax::Key;

use super::{Context, PAD, frame, key_text};
use crate::draw::{Align, Cmd, Scene, Tag, Weight};
use crate::query::{Header, QueryResult};
use crate::text;
use crate::visual::{SlicerStyle, Visual};

/// The checkbox's side, logical pixels.
const BOX: f32 = 12.0;
/// Space between an item's checkbox and its text.
const GAP: f32 = 6.0;

/// `visual` (a slicer) over `result`, with `selected` items checked.
pub fn layout(
    visual: &Visual,
    result: &QueryResult,
    selected: &[Value],
    cx: &Context<'_>,
) -> Scene {
    let (mut s, top) = frame(visual, Some(result), cx);
    let Some(header) = result.row_groups.first() else {
        return s;
    };
    let selected: Vec<Option<Key>> = selected.iter().map(|v| Key::of(v, false)).collect();
    let is_selected = |v: &Value| selected.contains(&Key::of(v, false));
    let size = cx.theme.label_size;
    let (ascent, descent) = text::ascent_descent(size, Weight::Regular);
    let row_h = (ascent + descent + 8.0).max(BOX + 6.0);
    let (w, h) = (s.width, s.height);
    match visual.format.slicer_style {
        SlicerStyle::List => {
            s.push(Cmd::PushClip {
                x: 0.0,
                y: top,
                w,
                h: (h - top).max(0.0),
            });
            let text_x = PAD + BOX + GAP;
            for (i, row) in result.rows.iter().enumerate() {
                let y = top + i as f32 * row_h;
                if y >= h {
                    break;
                }
                let tag = Some(Tag {
                    series: 0,
                    point: u32::try_from(i).unwrap_or(u32::MAX),
                });
                let item = item(row.keys.first());
                let on = is_selected(&item);
                // The whole row is the click target.
                s.push(Cmd::Rect {
                    x: 0.0,
                    y,
                    w,
                    h: row_h,
                    fill: None,
                    stroke: None,
                    tag,
                });
                let by = y + (row_h - BOX) / 2.0;
                s.push(Cmd::Rect {
                    x: PAD,
                    y: by,
                    w: BOX,
                    h: BOX,
                    fill: Some(if on {
                        cx.theme.text
                    } else {
                        cx.theme.background
                    }),
                    stroke: Some((cx.theme.text_muted, 1.0)),
                    tag,
                });
                if on {
                    // A check mark in the background colour.
                    s.push(Cmd::Path {
                        points: vec![
                            (PAD + 2.5, by + 6.0),
                            (PAD + 5.0, by + 8.5),
                            (PAD + 9.5, by + 3.5),
                        ],
                        color: cx.theme.background,
                        width: 1.5,
                        tag,
                    });
                }
                s.push(Cmd::Text {
                    x: text_x,
                    y: y + (row_h + ascent - descent) / 2.0,
                    text: text::truncate(
                        &key_text(&item, header, cx.locale),
                        size,
                        Weight::Regular,
                        w - text_x - PAD,
                    ),
                    size,
                    weight: Weight::Regular,
                    color: cx.theme.text,
                    align: Align::Start,
                    tag,
                });
            }
            s.push(Cmd::PopClip);
        }
        SlicerStyle::Dropdown => {
            let label = match selected.len() {
                0 => "All".to_string(),
                1 => dropdown_item(result, header, &selected, cx),
                _ => "Multiple selections".to_string(),
            };
            let (bx, bw, bh) = (PAD, (w - 2.0 * PAD).max(0.0), row_h + 4.0);
            s.push(Cmd::Rect {
                x: bx,
                y: top,
                w: bw,
                h: bh,
                fill: Some(cx.theme.background),
                stroke: Some((cx.theme.axis, 1.0)),
                tag: None,
            });
            let chevron = 16.0;
            s.push(Cmd::Text {
                x: bx + PAD,
                y: top + (bh + ascent - descent) / 2.0,
                text: text::truncate(&label, size, Weight::Regular, bw - 2.0 * PAD - chevron),
                size,
                weight: Weight::Regular,
                color: cx.theme.text,
                align: Align::Start,
                tag: None,
            });
            let (cx_, cy) = (bx + bw - PAD - 4.0, top + bh / 2.0);
            s.push(Cmd::Path {
                points: vec![
                    (cx_ - 4.0, cy - 2.0),
                    (cx_, cy + 2.0),
                    (cx_ + 4.0, cy - 2.0),
                ],
                color: cx.theme.text_muted,
                width: 1.5,
                tag: None,
            });
        }
    }
    s
}

/// An item's value (a slicer has no total rows; a missing key reads as BLANK).
fn item(key: Option<&Option<Value>>) -> Value {
    key.and_then(Option::as_ref)
        .cloned()
        .unwrap_or(Value::Blank)
}

/// The one selected item's text, from the item in `result` it matches (formatted as listed).
fn dropdown_item(
    result: &QueryResult,
    header: &Header,
    selected: &[Option<Key>],
    cx: &Context<'_>,
) -> String {
    let found = result
        .rows
        .iter()
        .map(|r| item(r.keys.first()))
        .find(|v| selected.first() == Some(&Key::of(v, false)));
    match found {
        Some(v) => key_text(&v, header, cx.locale),
        // Selected but not listed (filtered out by another slicer): still one item.
        None => "1 selected".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use opendash_data::step::ColumnType;

    use super::*;
    use crate::query::ResultRow;
    use crate::snapshot::check;
    use crate::theme::Theme;
    use crate::visual::{FieldRef, VisualType};

    fn result(items: &[Value]) -> QueryResult {
        QueryResult {
            kind: VisualType::Slicer,
            row_groups: vec![Header {
                field: FieldRef::Column {
                    table: "Sales".into(),
                    column: "Region".into(),
                },
                label: "Region".into(),
                format_string: String::new(),
                data_type: Some(ColumnType::Text),
                aggregate: None,
            }],
            column_groups: Vec::new(),
            values: Vec::new(),
            column_keys: vec![Vec::new()],
            rows: items
                .iter()
                .map(|v| ResultRow {
                    keys: vec![Some(v.clone())],
                    cells: Vec::new(),
                })
                .collect(),
            total: None,
            display: Vec::new(),
            offset: 0,
            total_rows: items.len(),
            truncated: false,
        }
    }

    fn slicer(style: SlicerStyle, w: i32, h: i32) -> Visual {
        let mut v = Visual::new("s1", VisualType::Slicer);
        v.w = w;
        v.h = h;
        v.format.slicer_style = style;
        v
    }

    fn items() -> Vec<Value> {
        vec![
            Value::Blank,
            Value::text("East"),
            Value::text("North"),
            Value::text("South with a very long region name that is cut"),
            Value::text("West"),
        ]
    }

    fn snap(name: &str, scene: &Scene) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("snapshots");
        check(&dir, name, scene).unwrap();
    }

    fn lay(v: &Visual, r: &QueryResult, selected: &[Value]) -> Scene {
        let th = Theme::light();
        let cx = Context {
            theme: &th,
            locale: &opendash_core::EN_GB,
        };
        layout(v, r, selected, &cx)
    }

    fn tags(s: &Scene) -> Vec<u32> {
        let mut t: Vec<u32> = s
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Rect { tag: Some(t), .. } => Some(t.point),
                _ => None,
            })
            .collect();
        t.dedup();
        t
    }

    #[test]
    fn slicer_list_snapshot() {
        let v = slicer(SlicerStyle::List, 200, 150);
        let s = lay(&v, &result(&items()), &[Value::text("north"), Value::Blank]);
        snap("slicer_list", &s);
        // A row per item that fits, each tagged with its row.
        assert_eq!(tags(&s), [0, 1, 2, 3, 4]);
        let text = s.to_text();
        assert!(text.contains("\"(Blank)\""), "{text}");
        assert!(text.contains('…'), "the long item is cut: {text}");
    }

    #[test]
    fn slicer_list_clipped_snapshot() {
        let v = slicer(SlicerStyle::List, 120, 70);
        let s = lay(&v, &result(&items()), &[]);
        snap("slicer_list_clipped", &s);
        let shown = tags(&s);
        assert!(!shown.is_empty() && shown.len() < 5, "{shown:?}");
    }

    #[test]
    fn slicer_dropdown_snapshot() {
        let r = result(&items());
        let v = slicer(SlicerStyle::Dropdown, 200, 60);
        snap("slicer_dropdown_all", &lay(&v, &r, &[]));
        snap("slicer_dropdown_one", &lay(&v, &r, &[Value::text("West")]));
        let many = lay(&v, &r, &[Value::text("West"), Value::text("East")]);
        snap("slicer_dropdown_many", &many);
        assert!(many.to_text().contains("Multiple selections"));
        let gone = lay(&v, &r, &[Value::text("Nowhere")]);
        assert!(gone.to_text().contains("1 selected"));
    }

    #[test]
    fn hostile_sizes_and_empty_results() {
        for (w, h) in [(0, 0), (1, 1), (-50, 30), (100_000, 100_000)] {
            for style in [SlicerStyle::List, SlicerStyle::Dropdown] {
                let v = slicer(style, w, h);
                let _ = lay(&v, &result(&items()), &[Value::Integer(3)]);
                let _ = lay(&v, &result(&[]), &[]);
            }
        }
        let mut no_header = result(&items());
        no_header.row_groups.clear();
        let s = lay(&slicer(SlicerStyle::List, 200, 150), &no_header, &[]);
        assert!(tags(&s).is_empty());
    }
}
