//! Read commands (glossary): `dax.evaluate`, `document.describe`, `page.render`.
//! They only look: never change the Document, never undoable. What a program (CLI, Control
//! channel, MCP) asks before and after it edits.

use metriccraft_core::Value as Cell;
use metriccraft_model::dax::model_eval::run_model_query_text;
use metriccraft_model::dax::{Database, Options};
use metriccraft_visuals::draw::{Cmd, Scene};
use metriccraft_visuals::layout::Context;
use serde_json::{Value, json};

use crate::{App, Error, Result};

/// `dax.evaluate`'s default and largest `maxRows`.
pub const DEFAULT_ROWS: i64 = 10_000;
pub const MAX_ROWS: i64 = 1_000_000;

fn failed(command: &str, message: impl std::fmt::Display) -> Error {
    Error::Read {
        command: command.into(),
        message: message.to_string(),
    }
}

fn invalid(command: &str, message: impl Into<String>) -> Error {
    Error::InvalidParams {
        command: command.into(),
        message: message.into(),
    }
}

/// `dax.evaluate`: runs a DAX query (one `EVALUATE`) with the Model's Measures in scope.
/// `{columns: [{name, type}], rows, truncated}`; BLANK is `null`, a column's type is its first
/// non-blank value's.
pub(crate) fn evaluate(app: &mut App, p: &Value) -> Result<Value> {
    const ID: &str = "dax.evaluate";
    let text = p.get("query").and_then(Value::as_str).unwrap_or_default();
    let max = p
        .get("maxRows")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_ROWS);
    if !(0..=MAX_ROWS).contains(&max) {
        return Err(invalid(ID, format!("`maxRows` must be 0 to {MAX_ROWS}")));
    }
    let doc = &app.doc;
    let db = Database::new(&doc.model, &doc.tables);
    let options = Options {
        cancel: Some(&app.cancel),
        memory_cap: app.memory_cap(),
    };
    let out = run_model_query_text(&db, &doc.model, text, options);
    app.cancel
        .store(false, std::sync::atomic::Ordering::Relaxed);
    let mut tables = out.map_err(|e| failed(ID, e))?;
    let table = match tables.len() {
        1 => tables.swap_remove(0),
        n => return Err(failed(ID, format!("needs one EVALUATE, the query has {n}"))),
    };
    let keep = usize::try_from(max).unwrap_or(usize::MAX);
    let columns: Vec<Value> = table
        .columns
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let ty = table
                .rows
                .iter()
                .filter_map(|r| r.get(i))
                .find(|v| !v.is_blank())
                .map_or("blank", type_name);
            json!({"name": name, "type": ty})
        })
        .collect();
    let truncated = table.rows.len() > keep;
    let rows: Vec<Value> = table
        .rows
        .iter()
        .take(keep)
        .map(|r| Value::Array(r.iter().map(cell).collect()))
        .collect();
    Ok(json!({"columns": columns, "rows": rows, "truncated": truncated}))
}

fn type_name(v: &Cell) -> &'static str {
    match v {
        Cell::Blank => "blank",
        Cell::Boolean(_) => "boolean",
        Cell::Integer(_) => "whole",
        Cell::Decimal(_) => "decimal",
        Cell::Currency(_) => "fixedDecimal",
        Cell::Text(_) => "text",
        Cell::DateTime(_) => "datetime",
    }
}

/// A DAX value as JSON: BLANK `null`, numbers as numbers (non-finite as the text Power BI
/// copies, `Infinity`), fixed decimals exact as text-free numbers, dates ISO 8601.
fn cell(v: &Cell) -> Value {
    match v {
        Cell::Blank => Value::Null,
        Cell::Boolean(b) => json!(b),
        Cell::Integer(i) => json!(i),
        Cell::Decimal(d) if d.is_finite() => json!(d),
        Cell::Decimal(_) => json!(v.copy_text()),
        Cell::Currency(c) => json!(*c as f64 / 10_000.0),
        Cell::Text(s) => json!(&**s),
        Cell::DateTime(d) => json!(iso(*d)),
    }
}

/// Days since 1899-12-30 as `2024-03-01`, or `2024-03-01T13:45:00` with a time of day.
fn iso(d: f64) -> String {
    if !d.is_finite() || d.abs() > 1e7 {
        return metriccraft_core::Value::DateTime(d).copy_text();
    }
    let days = d.floor();
    let secs = ((d - days) * 86_400.0).round() as i64;
    let (days, secs) = if secs >= 86_400 {
        (days as i64 + 1, 0)
    } else {
        (days as i64, secs)
    };
    let (y, m, dd) =
        metriccraft_core::date::civil(days.saturating_sub(metriccraft_core::date::UNIX_EPOCH_DAYS));
    if secs == 0 {
        format!("{y:04}-{m:02}-{dd:02}")
    } else {
        let (h, mi, s) = (secs / 3600, secs / 60 % 60, secs % 60);
        format!("{y:04}-{m:02}-{dd:02}T{h:02}:{mi:02}:{s:02}")
    }
}

/// `document.describe`: what the open Project holds, and `seq`, which changes with every change
/// to the Document (clients poll it).
pub(crate) fn describe(app: &mut App, _: &Value) -> Result<Value> {
    let doc = &app.doc;
    let tables: Vec<Value> = doc
        .model
        .tables
        .iter()
        .map(|t| {
            let rows = doc
                .tables
                .iter()
                .find(|d| d.name() == t.name)
                .map(|d| d.rows());
            json!({
                "name": t.name,
                "rows": rows,
                "hidden": t.hidden,
                "columns": t.columns.iter().map(|c| json!({
                    "name": c.name,
                    "type": c.data_type.name(),
                    "hidden": c.hidden,
                    "expression": c.expression,
                })).collect::<Vec<_>>(),
                "measures": t.measures.iter().map(|m| json!({
                    "name": m.name,
                    "expression": m.expression,
                    "formatString": m.format_string,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let relationships: Vec<Value> = doc
        .model
        .relationships
        .iter()
        .map(|r| {
            json!({
                "from": {"table": r.from_table, "column": r.from_column},
                "to": {"table": r.to_table, "column": r.to_column},
                "active": r.active,
            })
        })
        .collect();
    let queries: Vec<Value> = doc
        .queries
        .iter()
        .map(|q| {
            json!({
                "name": q.name,
                "steps": q.steps.len(),
                "load": !doc.unloaded.contains(&q.name),
                "stale": doc.stale.contains(&q.name),
            })
        })
        .collect();
    let pages: Vec<Value> = doc
        .report
        .pages
        .iter()
        .map(|p| {
            json!({
                "id": p.id,
                "name": p.name,
                "width": p.width,
                "height": p.height,
                "visuals": p.visuals.iter().map(|v| json!({
                    "id": v.id,
                    "type": v.kind.id(),
                    "x": v.x, "y": v.y, "width": v.w, "height": v.h,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    Ok(json!({
        "seq": app.version,
        "dirty": app.is_dirty(),
        "path": app.location().map(|l| match l {
            crate::Location::Folder(p) | crate::Location::Package(p) => p.display().to_string(),
        }),
        "tables": tables,
        "relationships": relationships,
        "queries": queries,
        "pages": pages,
        "currentPage": doc.report.current,
        "undo": app.undo_label(),
        "redo": app.redo_label(),
    }))
}

/// `page.render`: a Page as PNG, `{png: base64, width, height}` in pixels. `width` (pixels)
/// wins over `scale` (device pixels per page pixel, default 1).
pub(crate) fn render(app: &mut App, p: &Value) -> Result<Value> {
    const ID: &str = "page.render";
    let page = match p.get("page").and_then(Value::as_str) {
        Some(key) => app.doc.report.page(key),
        None => app
            .doc
            .report
            .current
            .as_deref()
            .and_then(|id| app.doc.report.page(id))
            .or_else(|| app.doc.report.pages.first()),
    }
    .ok_or_else(|| invalid(ID, "there is no such page"))?;
    let (id, w, h) = (page.id.clone(), page.width as f32, page.height as f32);
    let places: Vec<(String, f32, f32)> = page
        .visuals
        .iter()
        .map(|v| (v.id.clone(), v.x as f32, v.y as f32))
        .collect();
    let scale = match (p.get("width").and_then(Value::as_f64), p.get("scale")) {
        (Some(px), _) => px as f32 / w,
        (None, Some(s)) => s.as_f64().unwrap_or(1.0) as f32,
        (None, None) => 1.0,
    };
    let theme = metriccraft_visuals::Theme::light();
    // ponytail: en-US formatting until Projects carry a Culture, as the window does.
    let cx = Context {
        theme: &theme,
        locale: &metriccraft_core::EN_US,
    };
    let scenes = app.page_scene(&id, &cx)?;
    let mut out = Scene::new(w, h);
    for (visual, scene) in &scenes {
        if let Some((_, x, y)) = places.iter().find(|(v, ..)| v == visual) {
            place(&mut out, scene, *x, *y);
        }
    }
    let png = metriccraft_visuals::png::render(&out, scale, theme.background)
        .map_err(|e| failed(ID, e))?;
    Ok(json!({
        "png": base64(&png),
        "width": (w * scale).ceil(),
        "height": (h * scale).ceil(),
    }))
}

/// Paints `scene` into `page` at `x`, `y`, clipped to its own box.
fn place(page: &mut Scene, scene: &Scene, x: f32, y: f32) {
    page.push(Cmd::PushClip {
        x,
        y,
        w: scene.width,
        h: scene.height,
    });
    for cmd in &scene.cmds {
        let mut c = cmd.clone();
        match &mut c {
            Cmd::Rect { x: cx, y: cy, .. }
            | Cmd::Text { x: cx, y: cy, .. }
            | Cmd::PushClip { x: cx, y: cy, .. } => {
                *cx += x;
                *cy += y;
            }
            Cmd::Line { x1, y1, x2, y2, .. } => {
                *x1 += x;
                *y1 += y;
                *x2 += x;
                *y2 += y;
            }
            Cmd::Path { points, .. } => points.iter_mut().for_each(|(px, py)| {
                *px += x;
                *py += y;
            }),
            Cmd::PopClip => {}
        }
        page.push(c);
    }
    page.push(Cmd::PopClip);
}

/// Standard base64 with padding.
// ponytail: a dozen lines instead of a dependency; only `page.render` encodes.
pub fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                let idx = (n >> (18 - 6 * i) & 63) as usize;
                out.push(char::from(ABC.get(idx).copied().unwrap_or(b'=')));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648() {
        for (raw, enc) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(raw.as_bytes()), enc);
        }
    }

    #[test]
    fn dates_are_iso() {
        assert_eq!(iso(45_352.0), "2024-03-01");
        assert_eq!(iso(45_352.5), "2024-03-01T12:00:00");
        assert_eq!(iso(45_352.999_999_99), "2024-03-02");
    }
}
