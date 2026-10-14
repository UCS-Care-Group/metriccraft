//! Selection and cross-filter (CONTEXT.md: Selection; spec R-2, R-3; ADR 0007).
//!
//! - **Slicer selections** are the Page's (`Page::selections`): saved, undoable, changed by
//!   `slicer.select`. Items are stored as JSON (text, number, logical, null for BLANK), checked
//!   against the slicer Column's type; dates are their serial number (days since 1899-12-30).
//! - **Data-point Selection** is the session's ([`Session`]): picked by `selection.select` or a
//!   click, cleared by `selection.clear`; not saved, not undone, forgotten by File > Open / New.
//!   One Visual per Page holds it. A data point is the Columns it is grouped by and their values
//!   ([`Point`]): a row's keys, plus a Legend's or matrix column's keys; a total has fewer.
//!   It is dropped, whole, when it goes stale: its Visual or Page is deleted (undo doesn't bring
//!   it back), the Visual's type or group-by Fields change (a rename included), or one of its
//!   Columns isn't loaded.
//! - **Filters.** A Visual sees every slicer on its Page except itself, and the data-point
//!   Selection unless it is its source; values from one source OR together, sources AND. The
//!   source Visual isn't filtered by its own Selection: its unselected marks are dimmed.
//! - **Cache.** Each Visual's query is cached under a canonical key of exactly the filters it
//!   sees ([`QueryCache`]), so a slicer change re-queries only the Visuals it filters. The
//!   cache's generation changes when the Model or the loaded data does (undo included).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use opendash_core::{CURRENCY_SCALE, Value as Datum};
use opendash_data::step::ColumnType;
use opendash_model::Model;
use opendash_model::dax::{Bitset, Database, DaxError, Filter, FilterContext, Key, Options};
use opendash_visuals::hit::{dim, hit};
use opendash_visuals::layout::{Context, card, chart, error_scene, matrix, slicer, table};
use opendash_visuals::{
    FieldRef, QueryCache, QueryInput, QueryResult, Scene, Tag, Visual, VisualType, Well,
};
use serde_json::{Value, json};

use crate::model::{invalid, text};
use crate::report::{Page, page_mut, resolve};
use crate::{App, Document, Result};

/// Most items one slicer selection, or points one data-point Selection, holds.
pub const MAX_SELECTED: usize = 10_000;
/// Most key pairs one data point has.
const MAX_KEYS: usize = 64;

/// One data point: the Columns it is grouped by and their values.
#[derive(Clone, Debug, PartialEq)]
pub struct Point(pub Vec<(FieldRef, Datum)>);

impl Point {
    /// Its canonical form: equal for points that filter alike (`2` and `2.0`, text ignoring case).
    fn key(&self) -> Vec<(FieldRef, Option<Key>)> {
        self.0
            .iter()
            .map(|(f, v)| (f.clone(), Key::of(v, false)))
            .collect()
    }
}

/// The data points picked in one Visual of a Page.
#[derive(Clone, Debug, PartialEq)]
pub struct DataSelection {
    pub visual: String,
    pub points: Vec<Point>,
    /// The Visual's [`Signature`] when picked: the Selection holds while it does.
    signature: Signature,
}

/// What a Visual's data points are made of: its type and the Columns it groups by.
type Signature = (VisualType, Vec<FieldRef>);

/// `v`'s [`Signature`]. A table's Values Columns all count (whether one groups depends on the
/// Model), Measures don't.
fn signature(v: &Visual) -> Signature {
    let wells: &[Well] = match v.kind {
        VisualType::Table => &[Well::Values],
        VisualType::Matrix => &[Well::Rows, Well::Columns],
        VisualType::ClusteredBar | VisualType::ClusteredColumn | VisualType::Line => {
            &[Well::Axis, Well::Legend]
        }
        VisualType::Card | VisualType::Slicer => &[],
    };
    let fields = wells
        .iter()
        .flat_map(|w| v.fields(*w))
        .filter(|f| matches!(f, FieldRef::Column { .. }))
        .cloned()
        .collect();
    (v.kind, fields)
}

/// What a Visual's query is cached under: the canonical text of each filter source it sees,
/// sorted.
type SelectionKey = Vec<String>;

/// A loaded Table as the generation sees it: name, rows and buffers (address, bytes).
type Loaded = (String, usize, Vec<(usize, usize)>);

/// The last drawing of a Visual: what clicks are hit-tested against.
struct Drawn {
    scene: Scene,
    result: Option<Arc<QueryResult>>,
}

/// The session-only report state, held by the [`App`].
#[derive(Default)]
pub(crate) struct Session {
    /// The data-point Selection of each Page, by Page id.
    points: HashMap<String, DataSelection>,
    cache: QueryCache<SelectionKey>,
    generation: u64,
    /// What the generation was computed from: the Model and the loaded buffers.
    fingerprint: Option<(Model, Vec<Loaded>)>,
    /// By (Page id, Visual id).
    drawn: HashMap<(String, String), Drawn>,
}

impl Session {
    /// Drops every data-point Selection and drawing (a new Document).
    pub(crate) fn forget_selections(&mut self) {
        self.points.clear();
        self.drawn.clear();
    }

    /// After any change to the Document (an edit, undo, redo, load): drops the Selections that
    /// went stale (see the module docs) and the drawings of Pages and Visuals that are gone.
    pub(crate) fn prune(&mut self, doc: &Document) {
        let pages: HashMap<&str, &Page> = doc
            .report
            .pages
            .iter()
            .map(|p| (p.id.as_str(), p))
            .collect();
        let visual = |page: &str, id: &str| pages.get(page).and_then(|p| p.visual(id));
        self.points.retain(|page, sel| {
            visual(page, &sel.visual).is_some_and(|v| signature(v) == sel.signature)
        });
        self.drawn
            .retain(|(page, id), _| visual(page, id).is_some());
    }

    /// Moves to a new generation if the Model or the loaded data changed.
    fn sync(&mut self, doc: &Document) {
        let same = self.fingerprint.as_ref().is_some_and(|(model, tables)| {
            *model == doc.model
                && tables.len() == doc.tables.len()
                && tables
                    .iter()
                    .zip(&doc.tables)
                    .all(|((name, rows, buffers), t)| {
                        name == t.name() && *rows == t.rows() && *buffers == t.buffers()
                    })
        });
        if !same {
            self.generation = self.generation.wrapping_add(1);
            let tables = doc
                .tables
                .iter()
                .map(|t| (t.name().to_string(), t.rows(), t.buffers()))
                .collect();
            self.fingerprint = Some((doc.model.clone(), tables));
        }
    }
}

// ---- values ----

/// `v` (a slicer item or key value as JSON) as a value of a Column of type `ty`; null is BLANK.
pub(crate) fn datum(v: &Value, ty: ColumnType) -> Option<Datum> {
    if v.is_null() {
        return Some(Datum::Blank);
    }
    let finite = || v.as_f64().filter(|f| f.is_finite());
    Some(match ty {
        ColumnType::Text => Datum::text(v.as_str()?),
        ColumnType::Boolean => Datum::Boolean(v.as_bool()?),
        ColumnType::Whole => match v.as_i64() {
            Some(i) => Datum::Integer(i),
            None => {
                let f = finite().filter(|f| f.fract() == 0.0 && f.abs() < 9.0e15)?;
                Datum::Integer(f as i64)
            }
        },
        ColumnType::Decimal => Datum::Decimal(finite()?),
        ColumnType::FixedDecimal => {
            let scaled = (finite()? * CURRENCY_SCALE as f64).round();
            if scaled.abs() >= 9.0e18 {
                return None;
            }
            Datum::Currency(scaled as i64)
        }
        ColumnType::Date | ColumnType::DateTime => Datum::DateTime(finite()?),
    })
}

/// `v` as JSON (see [`datum`]); `None` for a non-finite number.
pub(crate) fn json_of(v: &Datum) -> Option<Value> {
    let num = |f: f64| serde_json::Number::from_f64(f).map(Value::Number);
    match v {
        Datum::Blank => Some(Value::Null),
        Datum::Boolean(b) => Some(Value::Bool(*b)),
        Datum::Integer(i) => Some(json!(i)),
        Datum::Decimal(d) | Datum::DateTime(d) => num(*d),
        Datum::Currency(c) => num(*c as f64 / CURRENCY_SCALE as f64),
        Datum::Text(s) => Some(Value::String(s.to_string())),
    }
}

/// The type of Column `f` in the Model (`None`: not a Column of it).
fn column_type(model: &Model, f: &FieldRef) -> Option<(FieldRef, ColumnType)> {
    let FieldRef::Column { table, column } = resolve(model, f)? else {
        return None;
    };
    let t = model.tables.iter().find(|t| t.name == table)?;
    let c = t.columns.iter().find(|c| c.name == column)?;
    Some((FieldRef::Column { table, column }, c.data_type))
}

/// A slicer's Column and its type, if it has one in the Model.
fn slicer_column(model: &Model, v: &Visual) -> Option<(FieldRef, ColumnType)> {
    column_type(model, v.fields(Well::Field).first()?)
}

/// A slicer's saved items as values of its Column (items that don't convert are left out).
fn slicer_items(model: &Model, page: &Page, v: &Visual) -> Vec<Datum> {
    let (Some(items), Some((_, ty))) = (page.selections.get(&v.id), slicer_column(model, v)) else {
        return Vec::new();
    };
    // At most what `slicer.select` and a file allow, whatever built the Page.
    items
        .iter()
        .take(MAX_SELECTED)
        .filter_map(|i| datum(i, ty))
        .collect()
}

// ---- data points ----

/// The data point a mark tagged `tag` shows in `result`; `None` for a total or a Visual
/// without groups (a card).
pub(crate) fn point_of(result: &QueryResult, tag: Tag) -> Option<Point> {
    let row = result.rows.get(usize::try_from(tag.point).ok()?)?;
    let mut pairs: Vec<(FieldRef, Datum)> = result
        .row_groups
        .iter()
        .zip(&row.keys)
        .filter_map(|(h, k)| Some((h.field.clone(), k.clone()?)))
        .collect();
    if result.kind != VisualType::Table && !result.column_groups.is_empty() {
        let j = usize::try_from(tag.series).ok()? / result.values.len().max(1);
        if let Some(keys) = result.column_keys.get(j) {
            pairs.extend(
                result
                    .column_groups
                    .iter()
                    .zip(keys)
                    .filter_map(|(h, k)| Some((h.field.clone(), k.clone()?))),
            );
        }
    }
    (!pairs.is_empty()).then_some(Point(pairs))
}

// ---- filters ----

/// One filter source: points ORed together.
type Source = Vec<Point>;

/// The canonical text of a source: its distinct point keys, sorted.
fn source_key(source: &Source) -> String {
    let points: BTreeSet<String> = source.iter().map(|p| format!("{:?}", p.key())).collect();
    points.into_iter().collect::<Vec<_>>().join("|")
}

/// Table and column of Column `f` in `db`.
fn locate(db: &Database, f: &FieldRef) -> Option<(usize, usize)> {
    let FieldRef::Column { table, column } = f else {
        return None;
    };
    let t = db.table(table)?;
    Some((t, db.tables.get(t)?.column(column)?))
}

/// A point's keys as (table, column, value) in the Database.
type Located<'a> = Vec<(usize, usize, &'a Datum)>;

/// `fc` with the filter of `source` added (see the module docs).
fn add_source(db: &Database, fc: FilterContext, source: &Source) -> FilterContext {
    // Each point as (table, column, value) triples. A point naming a Column not loaded makes
    // the source not filter: leaving just it out would narrow the OR to the others.
    let points: Option<Vec<Located<'_>>> = source
        .iter()
        .map(|p| {
            p.0.iter()
                .map(|(f, v)| locate(db, f).map(|(t, c)| (t, c, v)))
                .collect()
        })
        .collect();
    let Some(points) = points else {
        return fc;
    };
    let Some(first) = points.first() else {
        return fc;
    };
    let cols = |p: &Vec<(usize, usize, &Datum)>| -> Vec<(usize, usize)> {
        p.iter().map(|(t, c, _)| (*t, *c)).collect()
    };
    let first_cols = cols(first);
    let uniform = points.iter().all(|p| cols(p) == first_cols);
    let one_table = points
        .iter()
        .flatten()
        .all(|(t, _, _)| Some(t) == first_cols.first().map(|(t, _)| t));
    if !(uniform && (first_cols.len() == 1 || points.len() == 1)) && one_table {
        return rows_filter(db, fc, &points);
    }
    // Exact for one Column or one point. Points over several Tables ORed together can't be one
    // row set, so each Column gets the values any point has for it.
    // ponytail: over-selects multi-key, multi-point Selections across Tables (a Legend from a
    // different Table than the Axis); exact needs a multi-table tuple filter (TREATAS-like).
    let shared: Vec<(usize, usize)> = first_cols
        .into_iter()
        .filter(|tc| points.iter().all(|p| cols(p).contains(tc)))
        .collect();
    let mut fc = fc;
    for (t, c) in shared {
        let values: Vec<Datum> = points
            .iter()
            .flatten()
            .filter(|(pt, pc, _)| (*pt, *pc) == (t, c))
            .map(|(_, _, v)| (*v).clone())
            .collect();
        if let Some(table) = db.tables.get(t) {
            fc = fc.with(Filter::values(table, t, c, &values));
        }
    }
    fc
}

/// Points in one Table as a row set: the rows matching every key of some point.
fn rows_filter(
    db: &Database,
    fc: FilterContext,
    points: &[Vec<(usize, usize, &Datum)>],
) -> FilterContext {
    let Some(t) = points.iter().flatten().map(|(t, _, _)| *t).next() else {
        return fc;
    };
    let Some(table) = db.tables.get(t) else {
        return fc;
    };
    // Points grouped by their column list, as sets of key tuples.
    let mut groups: HashMap<Vec<usize>, HashSet<Vec<Option<Key>>>> = HashMap::new();
    for p in points {
        let cols = p.iter().map(|(_, c, _)| *c).collect();
        let keys = p.iter().map(|(_, _, v)| Key::of(v, false)).collect();
        groups.entry(cols).or_default().insert(keys);
    }
    // ponytail: a Key per row and column (no dictionary fast path); fine until a page bench
    // (M4.7) shows multi-key Selections on big tables.
    let mut rows = Bitset::new(table.rows, false);
    for r in 0..table.rows {
        let hit = groups.iter().any(|(cols, set)| {
            let tuple: Vec<Option<Key>> = cols
                .iter()
                .map(|c| {
                    table
                        .columns
                        .get(*c)
                        .and_then(|col| Key::of(&col.data.value(r), false))
                })
                .collect();
            set.contains(&tuple)
        });
        if hit {
            rows.set(r);
        }
    }
    fc.with(Filter::Rows { table: t, rows })
}

// ---- drawing ----

/// A Page's filter sources, ready to query its Visuals.
struct Prepared {
    page: String,
    db: Database,
    /// Each slicer's id and its items as one-key points.
    slicers: Vec<(String, Source)>,
    /// The data-point Selection, if its Visual is on the Page.
    points: Option<DataSelection>,
    /// Filter contexts built so far, by key.
    built: HashMap<SelectionKey, FilterContext>,
}

impl Prepared {
    /// Drops the Page's data-point Selection from `session` if it went stale.
    fn new(doc: &Document, session: &mut Session, page: &Page) -> Prepared {
        let slicers = page
            .visuals
            .iter()
            .filter(|v| v.kind == VisualType::Slicer)
            .filter_map(|v| {
                let (field, _) = slicer_column(&doc.model, v)?;
                let items = slicer_items(&doc.model, page, v);
                let source: Source = items
                    .into_iter()
                    .map(|d| Point(vec![(field.clone(), d)]))
                    .collect();
                (!source.is_empty()).then(|| (v.id.clone(), source))
            })
            .collect();
        let db = Database::new(&doc.model, &doc.tables);
        let fresh = session.points.get(&page.id).map(|s| {
            page.visual(&s.visual)
                .is_some_and(|v| signature(v) == s.signature)
                && s.points
                    .iter()
                    .all(|p| p.0.iter().all(|(f, _)| locate(&db, f).is_some()))
        });
        if fresh == Some(false) {
            session.points.remove(&page.id);
        }
        let points = session.points.get(&page.id).cloned();
        Prepared {
            page: page.id.clone(),
            db,
            slicers,
            points,
            built: HashMap::new(),
        }
    }

    /// The sources Visual `id` sees.
    fn sources(&self, id: &str) -> Vec<&Source> {
        let slicers = self
            .slicers
            .iter()
            .filter(|(s, _)| s != id)
            .map(|(_, src)| src);
        let points = self
            .points
            .as_ref()
            .filter(|p| p.visual != id)
            .map(|p| &p.points);
        slicers.chain(points).collect()
    }

    /// `v`'s query result under the filters it sees.
    fn query(
        &mut self,
        session: &mut Session,
        model: &Model,
        memory_cap: usize,
        cancel: Option<&AtomicBool>,
        v: &Visual,
    ) -> std::result::Result<Arc<QueryResult>, DaxError> {
        let sources = self.sources(&v.id);
        let mut key: SelectionKey = sources.iter().map(|s| source_key(s)).collect();
        key.sort();
        let fc = match self.built.get(&key) {
            Some(fc) => fc.clone(),
            None => {
                let fc = sources.iter().fold(FilterContext::default(), |fc, s| {
                    add_source(&self.db, fc, s)
                });
                self.built.insert(key.clone(), fc.clone());
                fc
            }
        };
        let input = QueryInput {
            db: &self.db,
            model,
            filters: &fc,
            options: Options { cancel, memory_cap },
        };
        session
            .cache
            .get_or_run(session.generation, v, key, &input, 0)
    }
}

/// `visual` laid out over `result`.
fn lay_out(visual: &Visual, result: &QueryResult, items: &[Datum], cx: &Context<'_>) -> Scene {
    match visual.kind {
        VisualType::Slicer => slicer::layout(visual, result, items, cx),
        VisualType::Table => table::layout(visual, result, cx),
        VisualType::Matrix => matrix::layout(visual, result, cx),
        VisualType::Card => card::layout(visual, result, cx),
        VisualType::ClusteredBar => chart::bar(visual, result, cx),
        VisualType::ClusteredColumn => chart::column(visual, result, cx),
        VisualType::Line => chart::line(visual, result, cx),
    }
}

impl App {
    /// Page `page` (id or name) drawn: each Visual's Scene in draw order (back to front), in its
    /// own logical pixels, under the Page's Selection. Queries come from the cache when the
    /// filters a Visual sees haven't changed; a failing query draws as the Visual's error.
    /// Keeps the Scenes for [`App::click`].
    ///
    /// Setting [`App::cancel_flag`] (from another thread) stops it: the queries still running
    /// or to come fail as cancelled and draw as errors. It clears the flag when done, so a
    /// cancel stops the drawing in progress, or else the next one.
    pub fn page_scene(&mut self, page: &str, cx: &Context<'_>) -> Result<Vec<(String, Scene)>> {
        let memory_cap = self.memory_cap();
        let cancel = Arc::clone(&self.cancel);
        let out = self.draw_page(page, cx, memory_cap, &cancel);
        cancel.store(false, Ordering::Relaxed);
        out
    }

    fn draw_page(
        &mut self,
        page: &str,
        cx: &Context<'_>,
        memory_cap: usize,
        cancel: &AtomicBool,
    ) -> Result<Vec<(String, Scene)>> {
        let App { doc, session, .. } = self;
        let page = doc
            .report
            .page(page)
            .ok_or_else(|| invalid("page", format!("there is no page `{page}`")))?;
        session.sync(doc);
        let mut prep = Prepared::new(doc, session, page);
        let mut out = Vec::new();
        for v in page.draw_order() {
            let (scene, result) = match prep.query(session, &doc.model, memory_cap, Some(cancel), v)
            {
                Ok(r) => {
                    let items = if v.kind == VisualType::Slicer {
                        slicer_items(&doc.model, page, v)
                    } else {
                        Vec::new()
                    };
                    let mut scene = lay_out(v, &r, &items, cx);
                    if let Some(sel) = prep.points.as_ref().filter(|s| s.visual == v.id) {
                        let keys: Vec<_> = sel.points.iter().map(Point::key).collect();
                        dim(&mut scene, |t| {
                            point_of(&r, t).is_some_and(|p| keys.contains(&p.key()))
                        });
                    }
                    (scene, Some(r))
                }
                Err(e) => (error_scene(v, &e.message, cx), None),
            };
            session.drawn.insert(
                (prep.page.clone(), v.id.clone()),
                Drawn {
                    scene: scene.clone(),
                    result,
                },
            );
            out.push((v.id.clone(), scene));
        }
        Ok(out)
    }

    /// Visual `visual`'s result as last drawn by [`App::page_scene`].
    pub fn visual_result(&self, page: &str, visual: &str) -> Option<Arc<QueryResult>> {
        let page = self.doc.report.page(page)?;
        let d = self
            .session
            .drawn
            .get(&(page.id.clone(), visual.to_string()))?;
        d.result.clone()
    }

    /// The data-point Selection on Page `page` (id or name).
    pub fn selection(&self, page: &str) -> Option<&DataSelection> {
        let page = self.doc.report.page(page)?;
        self.session.points.get(&page.id)
    }

    /// Visual queries run so far (cache misses), for tests and diagnostics.
    pub fn query_runs(&self) -> u64 {
        self.session.cache.runs()
    }

    /// A click at (`x`, `y`) in Visual `visual`'s own logical pixels, as last drawn by
    /// [`App::page_scene`]; `add` is Ctrl. On a slicer item it selects that item (`add`
    /// toggles it; clicking the only selected item clears), as `slicer.select` — undoable. On a
    /// data point it selects it (`add` toggles it in the Selection; clicking the only selected
    /// point clears), as `selection.select`; on anything else the Page's Selection is cleared,
    /// as `selection.clear`. Each runs as that command ([`App::run`]).
    pub fn click(&mut self, page: &str, visual: &str, x: f32, y: f32, add: bool) -> Result {
        const ID: &str = "selection.select";
        let p = self
            .doc
            .report
            .page(page)
            .ok_or_else(|| invalid(ID, format!("there is no page `{page}`")))?;
        let v = p
            .visual(visual)
            .ok_or_else(|| invalid(ID, format!("there is no visual `{visual}` on this page")))?;
        let page_id = p.id.clone();
        let drawn = self
            .session
            .drawn
            .get(&(page_id.clone(), visual.to_string()))
            .ok_or_else(|| invalid(ID, format!("visual `{visual}` hasn't been drawn")))?;
        let tag = hit(&drawn.scene, x, y);
        let result = drawn.result.clone();
        if v.kind == VisualType::Slicer {
            let item = tag
                .zip(result)
                .and_then(|(t, r)| {
                    r.rows
                        .get(usize::try_from(t.point).ok()?)?
                        .keys
                        .first()
                        .cloned()
                })
                .flatten()
                .and_then(|d| json_of(&d).map(|j| (d, j)));
            let Some((d, item)) = item else {
                return Ok(());
            };
            let only = slicer_items(&self.doc.model, p, v);
            let again = only.len() == 1
                && only.first().map(|o| Key::of(o, false)) == Some(Key::of(&d, false));
            let items = if again && !add {
                json!([])
            } else {
                json!([item])
            };
            let params = json!({"page": page_id, "visual": visual, "items": items, "add": add});
            return self.run("slicer.select", &params);
        }
        // The point by its place in the result: `selection.select` queries again (a cache hit
        // under the same filters) and takes the same point.
        match tag.filter(|t| result.as_ref().is_some_and(|r| point_of(r, *t).is_some())) {
            Some(t) => {
                let point = json!({"row": t.point, "series": t.series});
                let params = json!({"page": page_id, "visual": visual, "point": point, "add": add});
                self.run("selection.select", &params)
            }
            None => self.run("selection.clear", &json!({"page": page_id})),
        }
    }

    /// The flag that stops a running [`App::page_scene`]: set it from another thread (the UI's
    /// Cancel, the CLI's Ctrl-C) to stop a long Page refresh.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }
}

/// Picks `point` of Visual `visual` on Page `page` (see [`App::click`]).
fn pick(session: &mut Session, page: &str, visual: &Visual, point: Point, add: bool) {
    let key = point.key();
    let sig = signature(visual);
    match session.points.get_mut(page) {
        Some(sel) if sel.visual == visual.id && sel.signature == sig => {
            let at = sel.points.iter().position(|p| p.key() == key);
            match (add, at) {
                (true, Some(i)) => {
                    sel.points.remove(i);
                }
                (true, None) if sel.points.len() < MAX_SELECTED => sel.points.push(point),
                (true, None) => {}
                (false, Some(0)) if sel.points.len() == 1 => sel.points.clear(),
                (false, _) => sel.points = vec![point],
            }
            if sel.points.is_empty() {
                session.points.remove(page);
            }
        }
        _ => {
            session.points.insert(
                page.to_string(),
                DataSelection {
                    visual: visual.id.clone(),
                    points: vec![point],
                    signature: sig,
                },
            );
        }
    }
}

// ---- commands ----

/// Logical `params[key]`, false if absent.
fn flag(params: &Value, key: &str, id: &str) -> Result<bool> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(false),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(id, format!("`{key}` must be true or false"))),
    }
}

/// `slicer.select`: sets (or with `add`, toggles) the slicer's selected items. Undoable.
pub(crate) fn slicer_select(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "slicer.select";
    let add = flag(params, "add", ID)?;
    let page_key = text(params, "page", ID)?;
    let visual = text(params, "visual", ID)?;
    let Some(Value::Array(given)) = params.get("items") else {
        return Err(invalid(ID, "`items` must be a list of values".into()));
    };
    if given.len() > MAX_SELECTED {
        return Err(invalid(
            ID,
            format!("a slicer selects at most {MAX_SELECTED} items"),
        ));
    }
    let page = doc
        .report
        .page(&page_key)
        .ok_or_else(|| invalid(ID, format!("there is no page `{page_key}`")))?;
    let v = page
        .visual(&visual)
        .ok_or_else(|| invalid(ID, format!("there is no visual `{visual}` on this page")))?;
    if v.kind != VisualType::Slicer {
        return Err(invalid(ID, format!("visual `{visual}` is not a slicer")));
    }
    let (_, ty) = slicer_column(&doc.model, v)
        .ok_or_else(|| invalid(ID, "the slicer has no column of the model".into()))?;
    let mut items: Vec<(Option<Key>, Value)> = Vec::new();
    for g in given {
        let d = datum(g, ty).ok_or_else(|| {
            invalid(
                ID,
                format!("{g} is not a value of the slicer's column ({ty:?})"),
            )
        })?;
        let j = json_of(&d).ok_or_else(|| invalid(ID, format!("{g} is not a usable value")))?;
        let k = Key::of(&d, false);
        if !items.iter().any(|(o, _)| *o == k) {
            items.push((k, j));
        }
    }
    let page = page_mut(doc, params, ID)?;
    let mut next: Vec<Value> = if add {
        let mut cur: Vec<(Option<Key>, Value)> = page
            .selections
            .get(&visual)
            .into_iter()
            .flatten()
            .filter_map(|j| Some((Key::of(&datum(j, ty)?, false), j.clone())))
            .collect();
        for (k, j) in items {
            match cur.iter().position(|(o, _)| *o == k) {
                Some(i) => {
                    cur.remove(i);
                }
                None => cur.push((k, j)),
            }
        }
        cur.into_iter().map(|(_, j)| j).collect()
    } else {
        items.into_iter().map(|(_, j)| j).collect()
    };
    next.truncate(MAX_SELECTED);
    if next.is_empty() {
        page.selections.remove(&visual);
    } else {
        page.selections.insert(visual, next);
    }
    Ok(())
}

/// `selection.select`: picks a data point (session-only, not undoable).
pub(crate) fn select(app: &mut App, params: &Value) -> Result {
    const ID: &str = "selection.select";
    let add = flag(params, "add", ID)?;
    let page_key = text(params, "page", ID)?;
    let visual = text(params, "visual", ID)?;
    let page = app
        .doc
        .report
        .page(&page_key)
        .ok_or_else(|| invalid(ID, format!("there is no page `{page_key}`")))?;
    let v = page
        .visual(&visual)
        .ok_or_else(|| invalid(ID, format!("there is no visual `{visual}` on this page")))?;
    if v.kind == VisualType::Slicer {
        return Err(invalid(
            ID,
            "a slicer's items are picked with slicer.select".into(),
        ));
    }
    let Some(spec) = params.get("point").filter(|p| p.is_object()) else {
        return Err(invalid(
            ID,
            "`point` must be {row, series} or {keys: [{table, column, value}, …]}".into(),
        ));
    };
    let point = match spec.get("keys") {
        Some(keys) => point_from_keys(&app.doc.model, keys)?,
        None => {
            let row = index(spec, "row", ID, None)?;
            let series = index(spec, "series", ID, Some(0))?;
            let memory_cap = app.memory_cap();
            let (doc, session) = (&app.doc, &mut app.session);
            session.sync(doc);
            let mut prep = Prepared::new(doc, session, page);
            let result = prep
                .query(session, &doc.model, memory_cap, None, v)
                .map_err(|e| invalid(ID, format!("the visual's query failed: {}", e.message)))?;
            point_of(&result, Tag { series, point: row }).ok_or_else(|| {
                invalid(ID, format!("row {row} of the visual is not a data point"))
            })?
        }
    };
    pick(&mut app.session, &page.id, v, point, add);
    Ok(())
}

/// A `u32` `params[key]`; `default` if absent (`None`: required).
fn index(params: &Value, key: &str, id: &str, default: Option<u32>) -> Result<u32> {
    match params.get(key) {
        None | Some(Value::Null) => {
            default.ok_or_else(|| invalid(id, format!("`{key}` must be an integer")))
        }
        Some(v) => v
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| invalid(id, format!("`{key}` must be an integer from 0"))),
    }
}

/// `[{table, column, value}, …]` as a point of Columns of `model`.
fn point_from_keys(model: &Model, keys: &Value) -> Result<Point> {
    const ID: &str = "selection.select";
    let bad = || {
        invalid(
            ID,
            format!("`keys` must be 1 to {MAX_KEYS} of {{table, column, value}}"),
        )
    };
    let keys = keys
        .as_array()
        .filter(|k| (1..=MAX_KEYS).contains(&k.len()));
    let mut pairs = Vec::new();
    for k in keys.ok_or_else(bad)? {
        let (Some(table), Some(column)) = (
            k.get("table").and_then(Value::as_str),
            k.get("column").and_then(Value::as_str),
        ) else {
            return Err(bad());
        };
        let f = FieldRef::Column {
            table: table.into(),
            column: column.into(),
        };
        let (f, ty) = column_type(model, &f)
            .ok_or_else(|| invalid(ID, format!("the model has no column `{table}`[`{column}`]")))?;
        let value = k.get("value").unwrap_or(&Value::Null);
        let d = datum(value, ty).ok_or_else(|| {
            invalid(
                ID,
                format!("{value} is not a value of `{table}`[`{column}`] ({ty:?})"),
            )
        })?;
        pairs.push((f, d));
    }
    Ok(Point(pairs))
}

/// `selection.clear`: the data-point Selection of a Page, or of every Page.
pub(crate) fn clear(app: &mut App, params: &Value) -> Result {
    const ID: &str = "selection.clear";
    match params.get("page") {
        None | Some(Value::Null) => app.session.points.clear(),
        Some(_) => {
            let key = text(params, "page", ID)?;
            let id = app
                .doc
                .report
                .page(&key)
                .map(|p| p.id.clone())
                .ok_or_else(|| invalid(ID, format!("there is no page `{key}`")))?;
            app.session.points.remove(&id);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
