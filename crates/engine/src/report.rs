//! The report (spec R-1, F-1, F-2, D-4; ADR 0003, 0007): Pages of Visuals, their Fields and the
//! Pages' saved slicer selections, and the commands that edit them. All undoable; a command that
//! fails changes nothing; empty params run with defaults or are an error, never a dialog.
//!
//! - **Ids.** A Page or Visual gets a short id at creation (8 hex digits, random, unique in the
//!   report) that never changes: it is its path in the Project (`json.rs`). Random rather than
//!   counted so two branches of a Project that each add a Page don't collide in git.
//! - **Order.** Pages keep user order (`page.move`). A Page's Visuals are kept sorted by id, the
//!   order of their files, so reading a Project gives back the same Document; `z` is the draw
//!   order (a new Visual goes on top).
//! - **Fields** name a Column (`{table, column}`) or a Measure (`{measure}`). Adding one checks
//!   the Model and stores its own spelling of the names. Renaming a Table, Column or Measure
//!   rewrites every Field (and sort) naming it, in the same undoable command (`rename.rs`,
//!   `expression.rs`). Deleting one leaves its Fields in place: [`Document::broken_fields`]
//!   lists them (spec R-1: a missing Field shows as broken), as does renaming a column in Power
//!   Query (its model Column goes at the next Close & Apply, as in Power BI).
//! - **Wells** follow [`VisualType::wells`]: only Values takes a Measure; a one-Field well
//!   (Legend, a card's Values, a slicer's Field) has its Field replaced, as dropping on it does in
//!   Power BI. A chart takes a Legend or several Values, not both: adding a Legend keeps only the
//!   first Value, adding a second Value removes the Legend. `visual.changeType` keeps the Fields
//!   whose wells the new type has (up to each well's limit) and drops the rest.
//! - **Slicer selections** are saved with their Page, by slicer Visual id; `slicer.select`
//!   changes them (`selection.rs`). Deleting the slicer, changing its Field or its type drops
//!   its selection (in the same undoable edit).
//! - **Unknown content** of the report files (members we don't read) rides with its Page or
//!   Visual in the Document, so undoing a delete brings it back, and is written again on save.
//!
//! Parity uncertainties, to observe in the VM: new Pages are named `Page n` (the first free n)
//! and duplicate Page names are refused; the last Page can't be deleted; a new Visual's default
//! size and position (Power BI finds a free spot; we use 0,0 unless given); `visual.sort`'s
//! default direction (descending); whether a Column already in an Axis/Rows well may be added
//! again (we refuse, and allow it in Values).

mod json;

use std::collections::BTreeMap;

use opendash_model::{Model, same_name};
use opendash_visuals::visual::{LegendPosition, SlicerStyle};
use opendash_visuals::{FieldRef, Sort, SortDirection, Visual, VisualType, Well};
use serde_json::{Map, Value};

use crate::model::{invalid, text};
use crate::{App, Document, Result};

pub(crate) use json::{Kept, decode, encode};

/// A new Page's size, in logical pixels (16:9, Power BI's default).
pub const PAGE_WIDTH: i32 = 1280;
pub const PAGE_HEIGHT: i32 = 720;
/// The largest position or size accepted, in logical pixels.
pub const MAX_COORD: i32 = 100_000;
/// Most Pages in a report, and Visuals on a Page (bounds what a file can make us allocate).
pub const MAX_PAGES: usize = 10_000;
pub const MAX_VISUALS: usize = 10_000;

/// The report: its Pages, in order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub pages: Vec<Page>,
    /// Unknown members of `report.json`.
    pub(crate) extra: Map<String, Value>,
}

/// One canvas of Visuals.
#[derive(Clone, Debug, PartialEq)]
pub struct Page {
    /// Short stable id, unique in the report: its folder in the Project.
    pub id: String,
    pub name: String,
    pub width: i32,
    pub height: i32,
    /// Sorted by id; `z` is the draw order.
    pub visuals: Vec<Visual>,
    /// Saved slicer selections (CONTEXT.md: Selection), by slicer Visual id: the selected
    /// values. No entry: nothing selected.
    pub selections: BTreeMap<String, Vec<Value>>,
    /// Unknown members of `page.json`.
    pub(crate) extra: Map<String, Value>,
    /// Unknown members of each Visual's file, by Visual id.
    pub(crate) visual_extra: BTreeMap<String, Map<String, Value>>,
}

impl Page {
    /// An empty Page of the default size.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Page {
        Page {
            id: id.into(),
            name: name.into(),
            width: PAGE_WIDTH,
            height: PAGE_HEIGHT,
            visuals: Vec::new(),
            selections: BTreeMap::new(),
            extra: Map::new(),
            visual_extra: BTreeMap::new(),
        }
    }

    /// The Visual with id `id`.
    pub fn visual(&self, id: &str) -> Option<&Visual> {
        self.visuals.iter().find(|v| v.id == id)
    }

    /// The Visuals in draw order (back to front): by `z`, then id.
    pub fn draw_order(&self) -> Vec<&Visual> {
        let mut v: Vec<&Visual> = self.visuals.iter().collect();
        v.sort_by(|a, b| a.z.cmp(&b.z).then_with(|| a.id.cmp(&b.id)));
        v
    }
}

impl Report {
    /// The Page with id `key`, or else the one named `key` (ignoring case).
    pub fn page(&self, key: &str) -> Option<&Page> {
        self.page_index(key).and_then(|i| self.pages.get(i))
    }

    fn page_index(&self, key: &str) -> Option<usize> {
        self.pages
            .iter()
            .position(|p| p.id == key)
            .or_else(|| self.pages.iter().position(|p| same_name(&p.name, key)))
    }

    fn fields_mut(&mut self) -> impl Iterator<Item = &mut FieldRef> {
        self.pages.iter_mut().flat_map(|p| {
            p.visuals.iter_mut().flat_map(|v| {
                let sort = v.sort.as_mut().map(|s| &mut s.field);
                v.wells.values_mut().flatten().chain(sort)
            })
        })
    }

    /// Table `old` is now `new`: rewrites the Fields naming it.
    pub(crate) fn rename_table(&mut self, old: &str, new: &str) {
        for f in self.fields_mut() {
            if let FieldRef::Column { table, .. } = f
                && same_name(table, old)
            {
                *table = new.into();
            }
        }
    }

    /// Column `old` of Table `table` is now `new`: rewrites the Fields naming it.
    pub(crate) fn rename_column(&mut self, table: &str, old: &str, new: &str) {
        for f in self.fields_mut() {
            if let FieldRef::Column { table: t, column } = f
                && same_name(t, table)
                && same_name(column, old)
            {
                *column = new.into();
            }
        }
    }

    /// Measure `old` is now `new`: rewrites the Fields naming it.
    pub(crate) fn rename_measure(&mut self, old: &str, new: &str) {
        for f in self.fields_mut() {
            if let FieldRef::Measure { name } = f
                && same_name(name, old)
            {
                *name = new.into();
            }
        }
    }

    /// Whether `id` is taken by a Page or a Visual.
    fn taken(&self, id: &str) -> bool {
        self.pages
            .iter()
            .any(|p| p.id == id || p.visuals.iter().any(|v| v.id == id))
    }

    /// A fresh id (see the module docs).
    ///
    /// Hashes, with std's `RandomState` keys: the host's entropy ([`seed_ids`]), a per-process
    /// count, the report's content (Page and Visual ids and names) and a retry count.
    /// `RandomState` is seeded from the OS on native targets, which makes ids random there. On
    /// wasm32-unknown-unknown std has no entropy source (its keys are fixed constants, counted
    /// up per call), so every web session would make the same ids: the web host seeds them
    /// (`crypto.getRandomValues`) through [`seed_ids`], and until it does, ids still differ
    /// between Projects whose content differs.
    // ponytail: no `getrandom` (it needs a JS bridge the web build doesn't have yet); the web
    // app calls `seed_ids` once it has one.
    fn new_id(&self) -> String {
        use std::hash::{BuildHasher, Hash, Hasher};
        use std::sync::atomic::Ordering::Relaxed;
        let state = std::collections::hash_map::RandomState::new();
        let seed = ID_SEED.load(Relaxed);
        let count = ID_COUNT.fetch_add(1, Relaxed);
        let mut content = state.build_hasher();
        for p in &self.pages {
            (&p.id, &p.name).hash(&mut content);
            for v in &p.visuals {
                v.id.hash(&mut content);
            }
        }
        let content = content.finish();
        let mut n = 0u64;
        loop {
            let mut h = state.build_hasher();
            for x in [seed, count, content, n] {
                h.write_u64(x);
            }
            let id = format!("{:08x}", h.finish() & 0xFFFF_FFFF);
            if !self.taken(&id) {
                return id;
            }
            n = n.wrapping_add(1);
        }
    }
}

/// Entropy from the host, mixed into every new Page and Visual id (see `Report::new_id`).
static ID_SEED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Ids made by this process.
static ID_COUNT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mixes `entropy` (random bytes from the host) into the ids of new Pages and Visuals. Native
/// builds don't need it (std seeds from the OS); the web build has no entropy of its own and
/// calls this at start with bytes from `crypto.getRandomValues`, so two web sessions editing
/// the same Project don't make the same ids.
pub fn seed_ids(entropy: &[u8]) {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write(entropy);
    ID_SEED.fetch_xor(h.finish(), std::sync::atomic::Ordering::Relaxed);
}

/// A Field of a Visual that names no Column or Measure of the Model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokenField {
    pub page: String,
    pub visual: String,
    pub well: Well,
    /// Its position in the well.
    pub index: usize,
    pub field: FieldRef,
}

impl Document {
    /// Every Field whose Column or Measure is gone (deleted, or renamed outside the model), in
    /// report order: kept, shown as broken (spec R-1).
    pub fn broken_fields(&self) -> Vec<BrokenField> {
        let mut out = Vec::new();
        for p in &self.report.pages {
            for v in &p.visuals {
                for (well, fields) in &v.wells {
                    for (index, f) in fields.iter().enumerate() {
                        if resolve(&self.model, f).is_none() {
                            out.push(BrokenField {
                                page: p.id.clone(),
                                visual: v.id.clone(),
                                well: *well,
                                index,
                                field: f.clone(),
                            });
                        }
                    }
                }
            }
        }
        out
    }
}

/// `f` as the Model spells it, if it names a Column or Measure (ignoring case).
pub fn resolve(model: &Model, f: &FieldRef) -> Option<FieldRef> {
    match f {
        FieldRef::Column { table, column } => {
            let t = model.tables.iter().find(|t| same_name(&t.name, table))?;
            let c = t.columns.iter().find(|c| same_name(&c.name, column))?;
            Some(FieldRef::Column {
                table: t.name.clone(),
                column: c.name.clone(),
            })
        }
        FieldRef::Measure { name } => model.measure(name).map(|(_, m)| FieldRef::Measure {
            name: m.name.clone(),
        }),
    }
}

// ---- params ----

pub(crate) fn has_pages(app: &App) -> bool {
    !app.document().report.pages.is_empty()
}

pub(crate) fn has_visuals(app: &App) -> bool {
    app.document()
        .report
        .pages
        .iter()
        .any(|p| !p.visuals.is_empty())
}

pub(crate) fn page_mut<'a>(
    doc: &'a mut Document,
    params: &Value,
    id: &str,
) -> Result<&'a mut Page> {
    let key = text(params, "page", id)?;
    let i = doc
        .report
        .page_index(&key)
        .ok_or_else(|| invalid(id, format!("there is no page `{key}`")))?;
    doc.report
        .pages
        .get_mut(i)
        .ok_or_else(|| invalid(id, format!("there is no page `{key}`")))
}

fn visual_mut<'a>(page: &'a mut Page, params: &Value, id: &str) -> Result<&'a mut Visual> {
    let key = text(params, "visual", id)?;
    page.visuals
        .iter_mut()
        .find(|v| v.id == key)
        .ok_or_else(|| invalid(id, format!("there is no visual `{key}` on this page")))
}

/// Integer `params[key]` in `lo..=hi`; `default` if absent (`None`: required).
fn int(params: &Value, key: &str, id: &str, default: Option<i32>, lo: i32, hi: i32) -> Result<i32> {
    match params.get(key) {
        None | Some(Value::Null) => {
            default.ok_or_else(|| invalid(id, format!("`{key}` must be an integer")))
        }
        Some(v) => v
            .as_i64()
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (lo..=hi).contains(n))
            .ok_or_else(|| invalid(id, format!("`{key}` must be an integer from {lo} to {hi}"))),
    }
}

/// Index `params[key]`, at most `max`; `default` if absent (`None`: required).
fn index(params: &Value, key: &str, id: &str, default: Option<usize>, max: usize) -> Result<usize> {
    match params.get(key) {
        None | Some(Value::Null) => {
            default.ok_or_else(|| invalid(id, format!("`{key}` must be an integer")))
        }
        Some(v) => v
            .as_u64()
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= max)
            .ok_or_else(|| invalid(id, format!("`{key}` must be an integer from 0 to {max}"))),
    }
}

fn visual_type(params: &Value, id: &str) -> Result<VisualType> {
    let t = text(params, "type", id)?;
    VisualType::from_id(&t).ok_or_else(|| {
        let all: Vec<&str> = VisualType::ALL.iter().map(|t| t.id()).collect();
        invalid(
            id,
            format!("there is no visual type `{t}` (one of {})", all.join(", ")),
        )
    })
}

fn well(params: &Value, key: &str, id: &str) -> Result<Well> {
    let w = text(params, key, id)?;
    Well::from_id(&w).ok_or_else(|| invalid(id, format!("there is no well `{w}`")))
}

/// `params[key]` as a Field: `{table, column}` or `{measure}`.
fn field(params: &Value, key: &str, id: &str) -> Result<FieldRef> {
    params.get(key).and_then(json::field_from).ok_or_else(|| {
        invalid(
            id,
            format!("`{key}` must be {{\"table\", \"column\"}} or {{\"measure\"}}"),
        )
    })
}

/// A Page name Power BI accepts.
fn page_name(params: &Value, id: &str) -> Result<String> {
    let name = text(params, "name", id)?;
    if name.trim().is_empty() {
        return Err(invalid(id, "`name` must not be empty".into()));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid(
            id,
            "`name` can't contain control characters".into(),
        ));
    }
    Ok(name)
}

fn check_free_name(report: &Report, name: &str, except: Option<&str>, id: &str) -> Result {
    if let Some(p) = report
        .pages
        .iter()
        .find(|p| Some(p.id.as_str()) != except && same_name(&p.name, name))
    {
        return Err(invalid(id, format!("there is already a page `{}`", p.name)));
    }
    Ok(())
}

/// `Page n` for the first free n.
fn next_page_name(report: &Report) -> String {
    // Each name tried is taken by a distinct Page, so one is free within this many.
    (1..=report.pages.len().saturating_add(1))
        .map(|n| format!("Page {n}"))
        .find(|n| !report.pages.iter().any(|p| same_name(&p.name, n)))
        .unwrap_or_else(|| "Page".into())
}

// ---- Pages ----

/// `insert.newPage`: a new empty Page at the end.
pub(crate) fn new_page(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "insert.newPage";
    if doc.report.pages.len() >= MAX_PAGES {
        return Err(invalid(
            ID,
            format!("a report has at most {MAX_PAGES} pages"),
        ));
    }
    let name = match params.get("name") {
        None | Some(Value::Null) => next_page_name(&doc.report),
        Some(_) => page_name(params, ID)?,
    };
    check_free_name(&doc.report, &name, None, ID)?;
    let page = Page::new(doc.report.new_id(), name);
    doc.report.pages.push(page);
    Ok(())
}

/// `page.delete`.
pub(crate) fn delete_page(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "page.delete";
    let key = page_mut(doc, params, ID)?.id.clone();
    if doc.report.pages.len() <= 1 {
        return Err(invalid(ID, "a report keeps at least one page".into()));
    }
    doc.report.pages.retain(|p| p.id != key);
    Ok(())
}

/// `page.rename`.
pub(crate) fn rename_page(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "page.rename";
    let name = page_name(params, ID)?;
    let key = page_mut(doc, params, ID)?.id.clone();
    check_free_name(&doc.report, &name, Some(&key), ID)?;
    page_mut(doc, params, ID)?.name = name;
    Ok(())
}

/// `page.move`.
pub(crate) fn move_page(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "page.move";
    let key = page_mut(doc, params, ID)?.id.clone();
    let last = doc.report.pages.len().saturating_sub(1);
    let to = index(params, "index", ID, None, last)?;
    let from = doc
        .report
        .pages
        .iter()
        .position(|p| p.id == key)
        .ok_or_else(|| invalid(ID, format!("there is no page `{key}`")))?;
    let page = doc.report.pages.remove(from);
    doc.report
        .pages
        .insert(to.min(doc.report.pages.len()), page);
    Ok(())
}

// ---- Visuals ----

/// `insert.newVisual`: an empty Visual on top of the others.
pub(crate) fn new_visual(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "insert.newVisual";
    let kind = visual_type(params, ID)?;
    let x = int(params, "x", ID, Some(0), 0, MAX_COORD)?;
    let y = int(params, "y", ID, Some(0), 0, MAX_COORD)?;
    let w = int(params, "w", ID, Some(400), 1, MAX_COORD)?;
    let h = int(params, "h", ID, Some(300), 1, MAX_COORD)?;
    let id = doc.report.new_id();
    let page = match params.get("page") {
        None | Some(Value::Null) => {
            if doc.report.pages.is_empty() {
                let first = Page::new(doc.report.new_id(), next_page_name(&doc.report));
                doc.report.pages.push(first);
            }
            doc.report
                .pages
                .first_mut()
                .ok_or_else(|| invalid(ID, "the report has no page".into()))?
        }
        Some(_) => page_mut(doc, params, ID)?,
    };
    if page.visuals.len() >= MAX_VISUALS {
        return Err(invalid(
            ID,
            format!("a page has at most {MAX_VISUALS} visuals"),
        ));
    }
    let mut v = Visual::new(id, kind);
    (v.x, v.y, v.w, v.h) = (x, y, w, h);
    v.z = page
        .visuals
        .iter()
        .map(|v| v.z.saturating_add(1))
        .max()
        .unwrap_or(0);
    let at = page.visuals.partition_point(|o| o.id < v.id);
    page.visuals.insert(at, v);
    Ok(())
}

/// `visual.delete`: the Visual and its slicer selection.
pub(crate) fn delete_visual(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.delete";
    let page = page_mut(doc, params, ID)?;
    let key = visual_mut(page, params, ID)?.id.clone();
    page.visuals.retain(|v| v.id != key);
    page.selections.remove(&key);
    page.visual_extra.remove(&key);
    Ok(())
}

/// `visual.move`.
pub(crate) fn move_visual(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.move";
    let x = int(params, "x", ID, None, 0, MAX_COORD)?;
    let y = int(params, "y", ID, None, 0, MAX_COORD)?;
    let v = visual_mut(page_mut(doc, params, ID)?, params, ID)?;
    (v.x, v.y) = (x, y);
    Ok(())
}

/// `visual.resize`.
pub(crate) fn resize_visual(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.resize";
    let w = int(params, "w", ID, None, 1, MAX_COORD)?;
    let h = int(params, "h", ID, None, 1, MAX_COORD)?;
    let v = visual_mut(page_mut(doc, params, ID)?, params, ID)?;
    (v.w, v.h) = (w, h);
    Ok(())
}

/// `visual.changeType`.
pub(crate) fn change_type(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.changeType";
    let kind = visual_type(params, ID)?;
    edit_visual(doc, params, ID, |v| {
        v.kind = kind;
        tidy(v);
        Ok(())
    })
}

/// Runs `edit` on the Visual `params` names. A slicer whose Field changes, or that stops being a
/// slicer, loses its saved selection in the same edit: its items were values of the old Column.
fn edit_visual(
    doc: &mut Document,
    params: &Value,
    id: &str,
    edit: impl FnOnce(&mut Visual) -> Result,
) -> Result {
    let slicer_field = |v: &Visual| {
        (v.kind == VisualType::Slicer)
            .then(|| v.fields(Well::Field).first().cloned())
            .flatten()
    };
    let page = page_mut(doc, params, id)?;
    let v = visual_mut(page, params, id)?;
    let before = slicer_field(v);
    edit(v)?;
    if before.is_some() && slicer_field(v) != before {
        let key = v.id.clone();
        page.selections.remove(&key);
    }
    Ok(())
}

fn is_chart(kind: VisualType) -> bool {
    matches!(
        kind,
        VisualType::ClusteredBar | VisualType::ClusteredColumn | VisualType::Line
    )
}

/// Makes `v` fit its type: drops wells it doesn't have and Fields beyond a well's limit, a
/// chart's Values beyond the first while it has a Legend, empty wells, and a sort by a Field no
/// longer in it.
fn tidy(v: &mut Visual) {
    let kind = v.kind;
    v.wells.retain(|w, fields| {
        fields.truncate(kind.max_fields(*w));
        kind.wells().contains(w) && !fields.is_empty()
    });
    if is_chart(kind)
        && v.wells.contains_key(&Well::Legend)
        && let Some(values) = v.wells.get_mut(&Well::Values)
    {
        values.truncate(1);
    }
    if let Some(s) = &v.sort
        && !v.wells.values().flatten().any(|f| *f == s.field)
    {
        v.sort = None;
    }
}

/// Adds `f` to `well` of `v` at `at` (default last), by the rules in the module docs.
fn add(v: &mut Visual, well: Well, f: FieldRef, at: Option<usize>, id: &str) -> Result {
    let kind = v.kind;
    if !kind.wells().contains(&well) {
        return Err(invalid(
            id,
            format!("a {} visual has no {} well", kind.id(), well.id()),
        ));
    }
    if well != Well::Values && matches!(f, FieldRef::Measure { .. }) {
        return Err(invalid(
            id,
            format!("the {} well takes columns, not measures", well.id()),
        ));
    }
    let max = kind.max_fields(well);
    let fields = v.wells.entry(well).or_default();
    if well != Well::Values && max > 1 && fields.contains(&f) {
        return Err(invalid(
            id,
            format!("that field is already in the {} well", well.id()),
        ));
    }
    if max > 1 && fields.len() >= max {
        return Err(invalid(
            id,
            format!("the {} well takes at most {max} fields", well.id()),
        ));
    }
    if max <= 1 {
        // Dropping on a one-field well replaces its Field.
        fields.clear();
        fields.push(f);
    } else {
        let at = at.unwrap_or(fields.len());
        if at > fields.len() {
            return Err(invalid(
                id,
                format!("`index` must be from 0 to {}", fields.len()),
            ));
        }
        fields.insert(at, f);
    }
    if is_chart(kind) {
        let values = v.wells.get(&Well::Values).map_or(0, Vec::len);
        if well == Well::Legend {
            if let Some(values) = v.wells.get_mut(&Well::Values) {
                values.truncate(1);
            }
        } else if well == Well::Values && values > 1 {
            v.wells.remove(&Well::Legend);
        }
    }
    tidy(v);
    Ok(())
}

/// `visual.addField`.
pub(crate) fn add_field(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.addField";
    let w = well(params, "well", ID)?;
    let given = field(params, "field", ID)?;
    let f = resolve(&doc.model, &given)
        .ok_or_else(|| invalid(ID, format!("the model has no {}", describe(&given))))?;
    let at = match params.get("index") {
        None | Some(Value::Null) => None,
        Some(_) => Some(index(params, "index", ID, None, usize::MAX)?),
    };
    edit_visual(doc, params, ID, |v| add(v, w, f, at, ID))
}

fn describe(f: &FieldRef) -> String {
    match f {
        FieldRef::Column { table, column } => format!("column `{table}`[`{column}`]"),
        FieldRef::Measure { name } => format!("measure `{name}`"),
    }
}

/// `visual.removeField`.
pub(crate) fn remove_field(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.removeField";
    let w = well(params, "well", ID)?;
    let by_field = match params.get("field") {
        None | Some(Value::Null) => None,
        Some(_) => Some(field(params, "field", ID)?),
    };
    edit_visual(doc, params, ID, |v| {
        let fields = v
            .wells
            .get_mut(&w)
            .ok_or_else(|| invalid(ID, format!("the {} well of this visual is empty", w.id())))?;
        let at = match by_field {
            Some(f) => fields
                .iter()
                .position(|x| *x == f)
                .ok_or_else(|| invalid(ID, format!("{} is not in this well", describe(&f))))?,
            None => index(params, "index", ID, None, fields.len().saturating_sub(1))?,
        };
        if at >= fields.len() {
            return Err(invalid(ID, "`index` is past the well's fields".into()));
        }
        fields.remove(at);
        tidy(v);
        Ok(())
    })
}

/// `visual.moveField`: within a well or to another, by `visual.addField`'s rules.
pub(crate) fn move_field(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.moveField";
    let from = well(params, "well", ID)?;
    let to = match params.get("toWell") {
        None | Some(Value::Null) => from,
        Some(_) => well(params, "toWell", ID)?,
    };
    edit_visual(doc, params, ID, |v| {
        let fields = v.wells.get_mut(&from).ok_or_else(|| {
            invalid(
                ID,
                format!("the {} well of this visual is empty", from.id()),
            )
        })?;
        let at = index(params, "index", ID, None, fields.len().saturating_sub(1))?;
        if at >= fields.len() {
            return Err(invalid(ID, "`index` is past the well's fields".into()));
        }
        let f = fields.remove(at);
        // A sort by the moved Field survives the move.
        let sort = v.sort.take();
        if fields.is_empty() {
            v.wells.remove(&from);
        }
        let len = v.wells.get(&to).map_or(0, Vec::len);
        let to_index = index(params, "toIndex", ID, Some(len), len)?;
        add(v, to, f, Some(to_index), ID)?;
        v.sort = sort;
        tidy(v);
        Ok(())
    })
}

/// `visual.sort`.
pub(crate) fn sort(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.sort";
    let by = match params.get("field") {
        None => return Err(invalid(ID, "`field` must be a field or null".into())),
        Some(Value::Null) => None,
        Some(_) => Some(field(params, "field", ID)?),
    };
    let direction = match params.get("direction") {
        None | Some(Value::Null) => SortDirection::Descending,
        Some(_) => json::direction_from(&text(params, "direction", ID)?)
            .ok_or_else(|| invalid(ID, "`direction` must be ascending or descending".into()))?,
    };
    let v = visual_mut(page_mut(doc, params, ID)?, params, ID)?;
    v.sort = match by {
        None => None,
        Some(f) => {
            if !v.wells.values().flatten().any(|x| *x == f) {
                return Err(invalid(
                    ID,
                    format!("{} is not a field of this visual", describe(&f)),
                ));
            }
            Some(Sort {
                field: f,
                direction,
            })
        }
    };
    Ok(())
}

/// `visual.format`.
pub(crate) fn format(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "visual.format";
    let option = text(params, "option", ID)?;
    let value = params.get("value").unwrap_or(&Value::Null);
    let v = visual_mut(page_mut(doc, params, ID)?, params, ID)?;
    let kind = v.kind;
    let applies = match option.as_str() {
        "title" | "showTitle" => true,
        "dataLabels" | "legend" | "legendPosition" => is_chart(kind),
        "rowSubtotals" | "columnSubtotals" => kind == VisualType::Matrix,
        "slicerStyle" => kind == VisualType::Slicer,
        _ => return Err(invalid(ID, format!("there is no format option `{option}`"))),
    };
    if !applies {
        return Err(invalid(
            ID,
            format!("a {} visual has no `{option}` option", kind.id()),
        ));
    }
    let flag = || {
        value
            .as_bool()
            .ok_or_else(|| invalid(ID, format!("`{option}` takes true or false")))
    };
    let f = &mut v.format;
    match option.as_str() {
        "title" => {
            f.title = match value {
                Value::Null => None,
                Value::String(s) if !s.chars().any(char::is_control) => Some(s.clone()),
                _ => {
                    return Err(invalid(
                        ID,
                        "`title` takes text without control characters, or null".into(),
                    ));
                }
            }
        }
        "showTitle" => f.show_title = flag()?,
        "dataLabels" => f.data_labels = flag()?,
        "legend" => f.legend = flag()?,
        "rowSubtotals" => f.row_subtotals = flag()?,
        "columnSubtotals" => f.column_subtotals = flag()?,
        "legendPosition" => {
            f.legend_position = value
                .as_str()
                .and_then(json::legend_position_from)
                .ok_or_else(|| {
                    invalid(ID, "`legendPosition` is top, bottom, left or right".into())
                })?;
        }
        _ => {
            f.slicer_style = value
                .as_str()
                .and_then(json::slicer_style_from)
                .ok_or_else(|| invalid(ID, "`slicerStyle` is list or dropdown".into()))?;
        }
    }
    Ok(())
}

/// Ids of the format enums, as files and commands spell them.
pub(crate) fn legend_position_id(p: LegendPosition) -> &'static str {
    match p {
        LegendPosition::Top => "top",
        LegendPosition::Bottom => "bottom",
        LegendPosition::Left => "left",
        LegendPosition::Right => "right",
    }
}

pub(crate) fn slicer_style_id(s: SlicerStyle) -> &'static str {
    match s {
        SlicerStyle::List => "list",
        SlicerStyle::Dropdown => "dropdown",
    }
}

#[cfg(test)]
mod tests;
