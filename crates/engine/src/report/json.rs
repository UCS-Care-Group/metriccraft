//! The report's files (ADR 0003): JSON, read and written here because `opendash-file` (layer
//! 2) can't see the Visual definitions (layer 3); it carries them as opaque parts
//! (`Project::report`).
//!
//! ```text
//! report/report.json                          {"current": <pageId>, "pages": [<pageId>, …]}
//!                                             (the Page open when saved; Page order)
//! report/pages/<pageId>/page.json             {"height", "name", "selections": {<visualId>:
//!                                              [values…]}, "width"}
//! report/pages/<pageId>/visuals/<visualId>.json
//!     {"format": {"columnSubtotals", "dataLabels", "legend", "legendPosition",
//!                 "rowSubtotals", "showTitle", "slicerStyle", "title"},
//!      "h", "sort": null | {"direction", "field"}, "type", "w",
//!      "wells": {<well>: [{"column", "table"} | {"measure"}, …]}, "x", "y", "z"}
//! ```
//!
//! **Determinism.** Keys sorted (serde_json's map is ordered), 2-space indent, LF, a trailing
//! newline; every known member written, even at its default, so a file means the same if a
//! default changes; empty wells left out. A report with no Pages writes no files.
//!
//! **Unknown content.** Members we don't read — in `report.json`, `page.json`, a Visual file,
//! its `format` or its `wells` — are kept with their Page or Visual and written back after the
//! known ones (a known member always wins). Files under `report/` we don't read, and a Visual
//! of a type we don't know, are kept verbatim ([`Kept`], in `App`'s file state) and written
//! back, except inside the folder of a Page that has since been deleted.
//!
//! **Reading** is strict about what it knows: a malformed file (not JSON, a wrong type, a Field
//! that is neither shape, a well its type doesn't have, more Fields than a well takes, a slicer
//! selection of more than `MAX_SELECTED` items) is an error naming the file. A Page folder that `report.json` doesn't list is added after the
//! listed ones; a listed id with no folder is skipped.

use std::collections::BTreeMap;

use opendash_file::Error as FileError;
use opendash_visuals::visual::{LegendPosition, SlicerStyle};
use opendash_visuals::{FieldRef, Format, Sort, SortDirection, Visual, VisualType, Well};
use serde_json::{Map, Value};

use super::{
    MAX_COORD, MAX_PAGES, MAX_VISUALS, PAGE_HEIGHT, PAGE_WIDTH, Page, Report, is_chart,
    legend_position_id, slicer_style_id,
};
use crate::selection::MAX_SELECTED;
use crate::{Error, Result};

pub(crate) const REPORT_JSON: &str = "report/report.json";
pub(crate) const PAGES: &str = "report/pages/";

/// Report parts: path → bytes.
pub(crate) type Parts = BTreeMap<String, Vec<u8>>;

/// The report files kept verbatim: those in a Page's folder go when the Page does.
#[derive(Clone, Debug, Default)]
pub(crate) struct Kept {
    /// In the folder of a Page that was read, by path.
    in_pages: Parts,
    /// Anywhere else (including a folder with no `page.json`).
    loose: Parts,
}

// ---- writing ----

/// The report's files, plus `unknown` (see the module docs).
pub(crate) fn encode(report: &Report, kept: &Kept) -> Result<Parts> {
    let mut out = Parts::new();
    if !report.pages.is_empty() || !report.extra.is_empty() {
        let mut m = Map::new();
        let ids = report.pages.iter().map(|p| Value::String(p.id.clone()));
        m.insert("pages".into(), Value::Array(ids.collect()));
        if let Some(id) = &report.current
            && report.pages.iter().any(|p| &p.id == id)
        {
            m.insert("current".into(), Value::String(id.clone()));
        }
        fill(&mut m, &report.extra);
        out.insert(REPORT_JSON.into(), print(m)?);
    }
    for page in &report.pages {
        let dir = format!("{PAGES}{}/", page.id);
        let mut m = Map::new();
        m.insert("name".into(), Value::String(page.name.clone()));
        m.insert("width".into(), page.width.into());
        m.insert("height".into(), page.height.into());
        let selections = page
            .selections
            .iter()
            .map(|(k, v)| (k.clone(), Value::Array(v.clone())));
        m.insert("selections".into(), Value::Object(selections.collect()));
        fill(&mut m, &page.extra);
        out.insert(format!("{dir}page.json"), print(m)?);
        for v in &page.visuals {
            let mut m = visual_json(v);
            if let Some(extra) = page.visual_extra.get(&v.id) {
                fill(&mut m, extra);
            }
            out.insert(format!("{dir}visuals/{}.json", v.id), print(m)?);
        }
    }
    let in_pages = kept.in_pages.iter().filter(|(path, _)| {
        page_of(path).is_some_and(|id| report.pages.iter().any(|p| p.id == id))
    });
    for (path, bytes) in in_pages.chain(&kept.loose) {
        if !out.contains_key(path) {
            out.insert(path.clone(), bytes.clone());
        }
    }
    Ok(out)
}

/// The Page id of a path inside `report/pages/<id>/…`.
fn page_of(path: &str) -> Option<&str> {
    path.strip_prefix(PAGES)?.split_once('/').map(|(id, _)| id)
}

/// Adds `extra`'s members missing from `m`, recursing into objects both have.
fn fill(m: &mut Map<String, Value>, extra: &Map<String, Value>) {
    for (k, x) in extra {
        match (m.get_mut(k), x) {
            (Some(Value::Object(known)), Value::Object(x)) => fill(known, x),
            (Some(_), _) => {}
            (None, _) => {
                m.insert(k.clone(), x.clone());
            }
        }
    }
}

fn print(m: Map<String, Value>) -> Result<Vec<u8>> {
    let mut s = serde_json::to_string_pretty(&Value::Object(m))
        .map_err(|e| Error::File(FileError::Save(e.to_string())))?;
    s.push('\n');
    Ok(s.into_bytes())
}

pub(crate) fn field_json(f: &FieldRef) -> Value {
    let mut m = Map::new();
    match f {
        FieldRef::Column { table, column } => {
            m.insert("table".into(), Value::String(table.clone()));
            m.insert("column".into(), Value::String(column.clone()));
        }
        FieldRef::Measure { name } => {
            m.insert("measure".into(), Value::String(name.clone()));
        }
    }
    Value::Object(m)
}

fn direction_id(d: SortDirection) -> &'static str {
    match d {
        SortDirection::Ascending => "ascending",
        SortDirection::Descending => "descending",
    }
}

fn visual_json(v: &Visual) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("type".into(), Value::String(v.kind.id().into()));
    for (k, n) in [("x", v.x), ("y", v.y), ("w", v.w), ("h", v.h), ("z", v.z)] {
        m.insert(k.into(), n.into());
    }
    let wells = v
        .wells
        .iter()
        .filter(|(_, f)| !f.is_empty())
        .map(|(w, f)| (w.id().to_string(), f.iter().map(field_json).collect()));
    m.insert("wells".into(), Value::Object(wells.collect()));
    let sort = v.sort.as_ref().map_or(Value::Null, |s| {
        let mut o = Map::new();
        o.insert("field".into(), field_json(&s.field));
        o.insert(
            "direction".into(),
            Value::String(direction_id(s.direction).into()),
        );
        Value::Object(o)
    });
    m.insert("sort".into(), sort);
    let f = &v.format;
    let mut o = Map::new();
    o.insert(
        "title".into(),
        f.title.clone().map_or(Value::Null, Value::String),
    );
    for (k, b) in [
        ("showTitle", f.show_title),
        ("dataLabels", f.data_labels),
        ("legend", f.legend),
        ("rowSubtotals", f.row_subtotals),
        ("columnSubtotals", f.column_subtotals),
    ] {
        o.insert(k.into(), Value::Bool(b));
    }
    o.insert(
        "legendPosition".into(),
        Value::String(legend_position_id(f.legend_position).into()),
    );
    o.insert(
        "slicerStyle".into(),
        Value::String(slicer_style_id(f.slicer_style).into()),
    );
    m.insert("format".into(), Value::Object(o));
    m
}

// ---- reading ----

pub(crate) fn field_from(v: &Value) -> Option<FieldRef> {
    let o = v.as_object()?;
    let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    match o.len() {
        1 => Some(FieldRef::Measure {
            name: s("measure")?,
        }),
        2 => Some(FieldRef::Column {
            table: s("table")?,
            column: s("column")?,
        }),
        _ => None,
    }
}

pub(crate) fn direction_from(s: &str) -> Option<SortDirection> {
    [SortDirection::Ascending, SortDirection::Descending]
        .into_iter()
        .find(|d| direction_id(*d) == s)
}

pub(crate) fn legend_position_from(s: &str) -> Option<LegendPosition> {
    [
        LegendPosition::Top,
        LegendPosition::Bottom,
        LegendPosition::Left,
        LegendPosition::Right,
    ]
    .into_iter()
    .find(|p| legend_position_id(*p) == s)
}

pub(crate) fn slicer_style_from(s: &str) -> Option<SlicerStyle> {
    [SlicerStyle::List, SlicerStyle::Dropdown]
        .into_iter()
        .find(|x| slicer_style_id(*x) == s)
}

fn part_err(path: &str, message: impl Into<String>) -> Error {
    Error::File(FileError::Part {
        path: path.into(),
        message: message.into(),
    })
}

type Obj = Map<String, Value>;

fn object(path: &str, bytes: &[u8]) -> Result<Obj> {
    match serde_json::from_slice(bytes) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(part_err(path, "expected a JSON object")),
        Err(e) => Err(part_err(path, format!("not valid JSON: {e}"))),
    }
}

/// Removes `key` from `m`: an integer in `lo..=hi`, `default` if absent.
fn take_int(m: &mut Obj, path: &str, key: &str, default: i32, lo: i32, hi: i32) -> Result<i32> {
    match m.remove(key) {
        None => Ok(default),
        Some(v) => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (lo..=hi).contains(n))
            .ok_or_else(|| {
                part_err(
                    path,
                    format!("`{key}` must be an integer from {lo} to {hi}"),
                )
            }),
    }
}

fn take_bool(m: &mut Obj, path: &str, key: &str, default: bool) -> Result<bool> {
    match m.remove(key) {
        None => Ok(default),
        Some(Value::Bool(b)) => Ok(b),
        Some(_) => Err(part_err(path, format!("`{key}` must be true or false"))),
    }
}

fn take_object(m: &mut Obj, path: &str, key: &str) -> Result<Obj> {
    match m.remove(key) {
        None | Some(Value::Null) => Ok(Obj::new()),
        Some(Value::Object(o)) => Ok(o),
        Some(_) => Err(part_err(path, format!("`{key}` must be an object"))),
    }
}

/// The report from its parts, and the parts kept verbatim (see the module docs).
pub(crate) fn decode(parts: &Parts) -> Result<(Report, Kept)> {
    let mut report = Report::default();
    let mut unknown = Parts::new();
    let mut listed: Vec<String> = Vec::new();
    let mut pages: BTreeMap<String, Page> = BTreeMap::new();
    let mut visuals: Vec<(&str, &str, &String, &Vec<u8>)> = Vec::new();
    for (path, bytes) in parts {
        if path == REPORT_JSON {
            let mut m = object(path, bytes)?;
            match m.remove("pages") {
                None => {}
                Some(Value::Array(ids)) => {
                    for id in ids {
                        let Value::String(id) = id else {
                            return Err(part_err(path, "`pages` must be a list of page ids"));
                        };
                        listed.push(id);
                    }
                }
                Some(_) => return Err(part_err(path, "`pages` must be a list of page ids")),
            }
            match m.remove("current") {
                None => {}
                Some(Value::String(id)) => report.current = Some(id),
                Some(_) => return Err(part_err(path, "`current` must be a page id")),
            }
            report.extra = m;
            continue;
        }
        let rest = path.strip_prefix(PAGES).unwrap_or_default();
        let segments: Vec<&str> = rest.split('/').collect();
        match segments.as_slice() {
            [id, "page.json"] => {
                if pages.len() >= MAX_PAGES {
                    return Err(part_err(path, format!("more than {MAX_PAGES} pages")));
                }
                pages.insert((*id).to_string(), page(path, id, bytes)?);
            }
            [page, "visuals", file] if file.len() > 5 && file.ends_with(".json") => {
                let id = file.get(..file.len().saturating_sub(5)).unwrap_or_default();
                visuals.push((page, id, path, bytes));
            }
            _ => {
                unknown.insert(path.clone(), bytes.clone());
            }
        }
    }
    for (page_id, id, path, bytes) in visuals {
        let Some(page) = pages.get_mut(page_id) else {
            unknown.insert(path.clone(), bytes.clone());
            continue;
        };
        match visual(path, id, bytes)? {
            Some((v, extra)) => {
                if page.visuals.len() >= MAX_VISUALS {
                    return Err(part_err(path, format!("more than {MAX_VISUALS} visuals")));
                }
                if !extra.is_empty() {
                    page.visual_extra.insert(v.id.clone(), extra);
                }
                page.visuals.push(v);
            }
            None => {
                unknown.insert(path.clone(), bytes.clone());
            }
        }
    }
    for id in listed {
        if let Some(p) = pages.remove(&id) {
            report.pages.push(p);
        }
    }
    report.pages.extend(pages.into_values());
    for p in &mut report.pages {
        p.visuals.sort_by(|a, b| a.id.cmp(&b.id));
    }
    let (in_pages, loose) = unknown.into_iter().partition(|(path, _)| {
        page_of(path).is_some_and(|id| report.pages.iter().any(|p| p.id == id))
    });
    Ok((report, Kept { in_pages, loose }))
}

fn page(path: &str, id: &str, bytes: &[u8]) -> Result<Page> {
    let mut m = object(path, bytes)?;
    let name = match m.remove("name") {
        Some(Value::String(s)) => s,
        _ => return Err(part_err(path, "`name` must be text")),
    };
    let mut p = Page::new(id, name);
    p.width = take_int(&mut m, path, "width", PAGE_WIDTH, 1, MAX_COORD)?;
    p.height = take_int(&mut m, path, "height", PAGE_HEIGHT, 1, MAX_COORD)?;
    let selections = take_object(&mut m, path, "selections")?;
    if selections.len() > MAX_VISUALS {
        return Err(part_err(
            path,
            format!("more than {MAX_VISUALS} selections"),
        ));
    }
    for (visual, values) in selections {
        let Value::Array(values) = values else {
            return Err(part_err(
                path,
                format!("selection `{visual}` must be a list of values"),
            ));
        };
        // As `slicer.select` takes: every draw filters, keys and lists each item.
        if values.len() > MAX_SELECTED {
            return Err(part_err(
                path,
                format!("selection `{visual}` has more than {MAX_SELECTED} items"),
            ));
        }
        p.selections.insert(visual, values);
    }
    p.extra = m;
    Ok(p)
}

/// A Visual and its unknown members; `None` for a type we don't know (kept verbatim).
fn visual(path: &str, id: &str, bytes: &[u8]) -> Result<Option<(Visual, Obj)>> {
    let mut m = object(path, bytes)?;
    let kind = match m.get("type") {
        Some(Value::String(t)) => match VisualType::ALL.into_iter().find(|k| k.id() == t) {
            Some(k) => k,
            None => return Ok(None),
        },
        _ => return Err(part_err(path, "`type` must be text")),
    };
    m.remove("type");
    let mut v = Visual::new(id, kind);
    v.x = take_int(&mut m, path, "x", 0, 0, MAX_COORD)?;
    v.y = take_int(&mut m, path, "y", 0, 0, MAX_COORD)?;
    v.w = take_int(&mut m, path, "w", v.w, 1, MAX_COORD)?;
    v.h = take_int(&mut m, path, "h", v.h, 1, MAX_COORD)?;
    v.z = take_int(&mut m, path, "z", 0, i32::MIN, i32::MAX)?;
    let mut extra = Obj::new();

    let mut unknown_wells = Obj::new();
    for (k, fields) in take_object(&mut m, path, "wells")? {
        let Some(well) = Well::ALL.into_iter().find(|w| w.id() == k) else {
            unknown_wells.insert(k, fields);
            continue;
        };
        if !kind.wells().contains(&well) {
            return Err(part_err(
                path,
                format!("a {} visual has no {k} well", kind.id()),
            ));
        }
        let Value::Array(fields) = fields else {
            return Err(part_err(
                path,
                format!("well `{k}` must be a list of fields"),
            ));
        };
        if fields.len() > kind.max_fields(well) {
            return Err(part_err(path, format!("well `{k}` has too many fields")));
        }
        let fields = fields
            .iter()
            .map(|f| {
                field_from(f).ok_or_else(|| {
                    part_err(
                        path,
                        format!(
                            "a field in `{k}` must be {{\"table\", \"column\"}} or {{\"measure\"}}"
                        ),
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?;
        if !fields.is_empty() {
            v.wells.insert(well, fields);
        }
    }
    if is_chart(kind)
        && v.wells.contains_key(&Well::Legend)
        && v.wells.get(&Well::Values).map_or(0, Vec::len) > 1
    {
        return Err(part_err(
            path,
            "a chart has a legend or several values, not both",
        ));
    }
    if !unknown_wells.is_empty() {
        extra.insert("wells".into(), Value::Object(unknown_wells));
    }

    v.sort = match m.remove("sort") {
        None | Some(Value::Null) => None,
        Some(Value::Object(mut s)) => {
            let field = s.remove("field").as_ref().and_then(field_from);
            let direction = s
                .remove("direction")
                .as_ref()
                .and_then(Value::as_str)
                .and_then(direction_from);
            let (Some(field), Some(direction)) = (field, direction) else {
                return Err(part_err(
                    path,
                    "`sort` must have a `field` and a `direction` (ascending or descending)",
                ));
            };
            if !v.wells.values().flatten().any(|f| *f == field) {
                return Err(part_err(path, "`sort` names a field not in the visual"));
            }
            if !s.is_empty() {
                extra.insert("sort".into(), Value::Object(s));
            }
            Some(Sort { field, direction })
        }
        Some(_) => return Err(part_err(path, "`sort` must be an object or null")),
    };

    let mut f = take_object(&mut m, path, "format")?;
    let d = Format::default();
    v.format = Format {
        title: match f.remove("title") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s),
            Some(_) => return Err(part_err(path, "`format.title` must be text or null")),
        },
        show_title: take_bool(&mut f, path, "showTitle", d.show_title)?,
        data_labels: take_bool(&mut f, path, "dataLabels", d.data_labels)?,
        legend: take_bool(&mut f, path, "legend", d.legend)?,
        legend_position: match f.remove("legendPosition") {
            None => d.legend_position,
            Some(p) => p.as_str().and_then(legend_position_from).ok_or_else(|| {
                part_err(
                    path,
                    "`format.legendPosition` is top, bottom, left or right",
                )
            })?,
        },
        row_subtotals: take_bool(&mut f, path, "rowSubtotals", d.row_subtotals)?,
        column_subtotals: take_bool(&mut f, path, "columnSubtotals", d.column_subtotals)?,
        slicer_style: match f.remove("slicerStyle") {
            None => d.slicer_style,
            Some(s) => s
                .as_str()
                .and_then(slicer_style_from)
                .ok_or_else(|| part_err(path, "`format.slicerStyle` is list or dropdown"))?,
        },
    };
    if !f.is_empty() {
        extra.insert("format".into(), Value::Object(f));
    }
    for (k, x) in m {
        extra.insert(k, x);
    }
    Ok(Some((v, extra)))
}
