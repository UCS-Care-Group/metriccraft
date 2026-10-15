//! The one-pass group-by (M4.7; spec N-3; ADR 0006's storage-scan tier): [`super::summarize_rows`]
//! when every expression is a plain aggregation of one Table's rows. Instead of evaluating each
//! expression once per group under an equality filter (a scan per group), it scans the Table's
//! visible rows once, works out each row's group from dictionary codes and whole numbers (no
//! [`Value`] or text per row), and accumulates every expression per group.
//!
//! **When** (else [`rows`] returns `None` and the per-group path runs, so results never
//! differ):
//! - every expression is `SUM`, `AVERAGE`, `MIN`, `MAX`, `COUNT`, `COUNTA` or `DISTINCTCOUNT` of
//!   a column, or `COUNTROWS` of a Table, directly or through a chain of Measures whose
//!   expression is exactly that; all over one Table, the *fact* Table, and of a type the
//!   function takes (a wrong type is the per-group path's error to give);
//! - each group-by column is on the fact Table, or on a Table it reaches by exactly one active
//!   many-to-one Relationship (and by no other path, and that doesn't reach back), whose key is
//!   unique among its visible rows;
//! - no row context (a Measure would transition) and no row-set filter (expanded tables).
//!
//! **Same results.** A group is a combination of the group-by columns' values as [`Key`]s
//! compare them (text ignoring case); the groups come out in the per-group path's order (each
//! Table's tuples in first-seen order over its visible rows, the Tables cross-joined in
//! group-by order) with the first-seen spellings; each aggregation adds the same values in the
//! same row order with the same types, BLANK rules and overflow (an overflowing whole-number
//! `SUM` falls back, so the error is the per-group path's). Groups whose expressions are all
//! BLANK are dropped. A dimension Table's tuples with no fact rows have only BLANK
//! aggregations, so the per-group path drops them too.
//!
//! **Bounds.** Every group, class table and distinct-value entry is charged to the memory cap;
//! the cancel flag is checked every [`storage::CHUNK`] rows.

use std::collections::{HashMap, HashSet};
use std::hash::BuildHasherDefault;

use arrow_array::{Array, ArrowPrimitiveType, PrimitiveArray, StringArray};
use metriccraft_core::{Value, fold};
use metriccraft_dax::{Expr, ExprKind, Span};

use crate::dax::eval::Evaluator;
use crate::dax::storage::{
    self, Bitset, CHUNK, Cancel, Codes, Col, IntHasher, Key, Kind, Number, UNIX_EPOCH_DAYS,
};
use crate::dax::{Database, Filter, FilterContext, MAX_MEASURE_CHAIN, Result};

type IntMap<K, V> = HashMap<K, V, BuildHasherDefault<IntHasher>>;
type IntSet<K> = HashSet<K, BuildHasherDefault<IntHasher>>;

/// No class / no group (a row left out).
const NONE: u32 = u32::MAX;
/// A row's group key when the row is in no group.
const DROP: u64 = u64::MAX;
/// Most slots a dense group table may have (beyond 4 × the visible rows it hashes).
const DENSE_SLOTS: u64 = 1 << 22;
/// Most classes a whole-number column's range may have before it hashes instead.
const DENSE_RANGE: u64 = 1 << 22;
/// Bytes a hashed entry costs (with the map's overhead), for the memory cap.
const ENTRY_BYTES: usize = 32;
/// Bytes one materialised value costs.
const VALUE_BYTES: usize = std::mem::size_of::<Value>();
/// Bytes one materialised row costs beyond its values.
const ROW_BYTES: usize = std::mem::size_of::<Vec<Value>>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Agg {
    CountRows,
    Count,
    CountA,
    Sum,
    Average,
    Min,
    Max,
    DistinctCount,
}

/// A plain aggregation: its function and (Table, Column) (`None`: `COUNTROWS`).
struct Plain {
    agg: Agg,
    table: usize,
    column: Option<usize>,
}

/// `e` as a plain aggregation, following Measure references.
fn plain(ev: &Evaluator<'_, '_>, e: &Expr, depth: usize) -> Option<Plain> {
    match &e.kind {
        ExprKind::Column { table, column } => {
            if depth >= MAX_MEASURE_CHAIN {
                return None;
            }
            let m = ev.measure_expr(table.as_deref(), column)?;
            plain(ev, m, depth + 1)
        }
        ExprKind::Call { name, args } => {
            let [arg] = args.as_slice() else {
                return None;
            };
            let agg = match name.to_ascii_lowercase().as_str() {
                "countrows" => Agg::CountRows,
                "count" => Agg::Count,
                "counta" => Agg::CountA,
                "sum" => Agg::Sum,
                "average" => Agg::Average,
                "min" => Agg::Min,
                "max" => Agg::Max,
                "distinctcount" => Agg::DistinctCount,
                _ => return None,
            };
            if agg == Agg::CountRows {
                let ExprKind::Name(n) = &arg.kind else {
                    return None;
                };
                if ev.has_var(n) {
                    return None;
                }
                return Some(Plain {
                    agg,
                    table: ev.db.table(n)?,
                    column: None,
                });
            }
            let ExprKind::Column { table, column } = &arg.kind else {
                return None;
            };
            let (table, column) = resolve(ev.db, table.as_deref(), column)?;
            Some(Plain {
                agg,
                table,
                column: Some(column),
            })
        }
        _ => None,
    }
}

/// A column argument as the aggregations resolve it (`aggregate::column_arg`): by Table, or the
/// one Table having it.
fn resolve(db: &Database, table: Option<&str>, column: &str) -> Option<(usize, usize)> {
    match table {
        Some(t) => {
            let ti = db.table(t)?;
            Some((ti, db.tables.get(ti)?.column(column)?))
        }
        None => {
            let mut hits = db
                .tables
                .iter()
                .enumerate()
                .filter_map(|(ti, t)| Some((ti, t.column(column)?)));
            let first = hits.next();
            if hits.next().is_some() {
                return None;
            }
            first
        }
    }
}

/// The groups of `group_by` under `fc`, each with its `measures`, in one scan (module doc):
/// `None` when the per-group path must run. Rows are the group-by values then the expressions';
/// all-BLANK groups are left out.
pub(super) fn rows(
    ev: &mut Evaluator<'_, '_>,
    group_by: &[(usize, usize)],
    measures: &[&Expr],
    fc: &FilterContext,
    span: Span,
) -> Result<Option<Vec<Vec<Value>>>> {
    if measures.is_empty()
        || !ev.row_frames().is_empty()
        || fc
            .filters
            .iter()
            .any(|f| matches!(f.as_ref(), Filter::Rows { .. }))
    {
        return Ok(None);
    }
    let mut plains = Vec::with_capacity(measures.len());
    for e in measures {
        match plain(ev, e, 0) {
            Some(p) => plains.push(p),
            None => return Ok(None),
        }
    }
    let Some(fact) = plains.first().map(|p| p.table) else {
        return Ok(None);
    };
    if plains.iter().any(|p| p.table != fact) {
        return Ok(None);
    }
    let db = ev.db;
    let Some(ft) = db.tables.get(fact) else {
        return Ok(None);
    };

    // Each Table with the positions in `group_by` of its columns, as `groups` orders them.
    let mut sets: Vec<(usize, Vec<usize>)> = Vec::new();
    for (k, &(t, _)) in group_by.iter().enumerate() {
        match sets.iter_mut().find(|(u, _)| *u == t) {
            Some((_, ks)) => ks.push(k),
            None => sets.push((t, vec![k])),
        }
    }
    let mut dims: Vec<Dim> = Vec::new();
    for (t, ks) in &sets {
        if *t == fact {
            continue;
        }
        let cols: Vec<usize> = ks
            .iter()
            .filter_map(|k| Some(group_by.get(*k)?.1))
            .collect();
        match dim(ev, fact, *t, &cols, fc, span)? {
            Some(d) => dims.push(d),
            None => return Ok(None),
        }
    }

    let mask = ev.mask_memo(fact, fc, span)?;
    let visible = storage::count(ft.rows, mask.as_ref(), ev.cancel)?;

    // The parts of a row's group key: the fact Table's group-by columns, then each dimension's
    // tuple; the last part turns fastest.
    let fact_cols: Vec<usize> = sets
        .iter()
        .find(|(t, _)| *t == fact)
        .map(|(_, ks)| {
            ks.iter()
                .filter_map(|k| Some(group_by.get(*k)?.1))
                .collect()
        })
        .unwrap_or_default();
    let mut parts: Vec<Part<'_>> = Vec::with_capacity(fact_cols.len() + dims.len());
    for &c in &fact_cols {
        let Some(col) = ft.columns.get(c).map(|c| &c.data) else {
            return Ok(None);
        };
        let Some((classes, bound)) = Classes::new(col, ft.rows, visible) else {
            return Ok(None);
        };
        ev.charge(classes.bytes(), span)?;
        parts.push(Part {
            col,
            how: How::Classes(classes),
            bound,
            stride: 0,
        });
    }
    let mut fact_bound = 1u64;
    for p in &parts {
        let Some(b) = fact_bound.checked_mul(p.bound) else {
            return Ok(None);
        };
        fact_bound = b;
    }
    for d in &mut dims {
        let Some(col) = ft.columns.get(d.fk).map(|c| &c.data) else {
            return Ok(None);
        };
        parts.push(Part {
            col,
            how: How::Dim(std::mem::replace(
                &mut d.lookup,
                Lookup::Keys(HashMap::new()),
            )),
            bound: u64::try_from(d.first_rows.len()).unwrap_or(u64::MAX).max(1),
            stride: 0,
        });
    }
    let mut stride = 1u64;
    for p in parts.iter_mut().rev() {
        p.stride = stride;
        let Some(s) = stride.checked_mul(p.bound) else {
            return Ok(None);
        };
        stride = s;
    }
    // `stride` is now the number of possible keys; keys are below it, DROP above.
    if stride == u64::MAX {
        return Ok(None);
    }
    let dim_bound = stride / fact_bound.max(1);

    let mut readers = Vec::with_capacity(plains.len());
    let mut accs = Vec::with_capacity(plains.len());
    for p in &plains {
        let col = p.column.and_then(|c| ft.columns.get(c)).map(|c| &c.data);
        match Acc::new(p.agg, col, ft.rows, visible) {
            Some((r, a)) => {
                readers.push(r);
                accs.push(a);
            }
            None => return Ok(None),
        }
    }

    let visible_keys = u64::try_from(visible).unwrap_or(u64::MAX);
    let dense = stride <= DENSE_SLOTS.min(visible_keys.saturating_mul(4).max(1 << 16));
    let slots = if dense {
        let n = usize::try_from(stride).unwrap_or(usize::MAX);
        ev.charge(n.saturating_mul(4), span)?;
        Slots::Dense(vec![NONE; n])
    } else {
        Slots::Hashed(IntMap::default())
    };
    let width = group_by.len().saturating_add(measures.len());
    let n_fact_parts = fact_cols.len();
    let mut state = Absorb {
        slots,
        group_bytes: width
            .saturating_mul(VALUE_BYTES)
            .saturating_add(ROW_BYTES)
            .saturating_add(accs.len().saturating_mul(24))
            .saturating_add(if dense { 16 } else { 16 + ENTRY_BYTES }),
        dim_bound,
        with_dims: !dims.is_empty(),
        fact_seen: IntMap::default(),
        fact_first: Vec::new(),
        group_keys: Vec::new(),
        first_rows: Vec::new(),
        key_maps: parts.iter().map(|_| HashMap::new()).collect(),
        groups: Vec::new(),
        classes: Vec::new(),
        span,
    };
    let scan = Scan {
        rows: ft.rows,
        mask: mask.as_ref(),
        parts: &parts,
        n_fact: n_fact_parts,
        readers: &readers,
        cancel: ev.cancel,
    };
    if !scan.run(ev, &mut state, &mut accs)? {
        return Ok(None);
    }
    let Absorb {
        fact_seen,
        mut fact_first,
        group_keys,
        first_rows,
        ..
    } = state;

    // Order: per set, its tuples' first-seen order (the fact Table's from its scan, each
    // dimension's from its own), sets in group-by order.
    // Per group, its fact tuple; per fact tuple, the first row it was seen on.
    let fact_tuple: Vec<u32> = if dims.is_empty() {
        fact_first.clone_from(&first_rows);
        (0..group_keys.len())
            .map(|g| u32::try_from(g).unwrap_or(NONE))
            .collect()
    } else {
        group_keys
            .iter()
            .map(|&k| {
                fact_seen
                    .get(&(k / dim_bound.max(1)))
                    .copied()
                    .unwrap_or(NONE)
            })
            .collect()
    };
    let dim_tuple = |g: usize, d: usize| -> usize {
        let k = group_keys.get(g).copied().unwrap_or(0);
        parts
            .get(n_fact_parts + d)
            .map_or(0, |p| ((k / p.stride.max(1)) % p.bound.max(1)) as usize)
    };
    let mut order: Vec<usize> = (0..group_keys.len()).collect();
    if !dims.is_empty() {
        let sort_key = |g: usize| -> Vec<usize> {
            let mut d = 0;
            sets.iter()
                .map(|(t, _)| {
                    if *t == fact {
                        fact_tuple.get(g).map_or(0, |&x| x as usize)
                    } else {
                        let v = dim_tuple(g, d);
                        d += 1;
                        v
                    }
                })
                .collect()
        };
        order.sort_by_cached_key(|&g| sort_key(g));
    }

    // Per group-by column, its dimension (`None`: the fact Table's).
    let dim_of: Vec<Option<usize>> = group_by
        .iter()
        .map(|&(t, _)| {
            sets.iter()
                .filter(|(u, _)| *u != fact)
                .position(|(u, _)| *u == t)
        })
        .collect();
    let mut out = Vec::new();
    for (i, g) in order.into_iter().enumerate() {
        cancel_every(ev, i)?;
        let mut row = Vec::with_capacity(width);
        for (&(_, c), dim) in group_by.iter().zip(&dim_of) {
            let v = match *dim {
                None => {
                    let r = fact_tuple
                        .get(g)
                        .and_then(|&ft| fact_first.get(ft as usize))
                        .copied()
                        .unwrap_or(0);
                    ft.columns
                        .get(c)
                        .map_or(Value::Blank, |col| col.data.value(r))
                }
                Some(d) => {
                    let tuple = dim_tuple(g, d);
                    dims.get(d)
                        .and_then(|dm| {
                            let r = *dm.first_rows.get(tuple)?;
                            let col = db.tables.get(dm.table)?.columns.get(c)?;
                            Some(col.data.value(r))
                        })
                        .unwrap_or(Value::Blank)
                }
            };
            row.push(v);
        }
        let start = row.len();
        for a in &accs {
            match a.value(g) {
                Some(v) => row.push(v),
                None => return Ok(None),
            }
        }
        if row.get(start..).unwrap_or(&[]).iter().all(Value::is_blank) {
            continue;
        }
        let text = row.iter().fold(0usize, |a, v| match v {
            Value::Text(s) => a.saturating_add(s.len()),
            _ => a,
        });
        ev.charge(text, span)?;
        out.push(row);
    }
    Ok(Some(out))
}

fn cancel_every(ev: &Evaluator<'_, '_>, i: usize) -> Result<()> {
    if i.is_multiple_of(CHUNK) {
        ev.cancel.check()?;
    }
    Ok(())
}

/// A dimension Table's group-by columns, reached from the fact Table by Relationship.
struct Dim {
    table: usize,
    /// The fact Table's key column.
    fk: usize,
    /// Per tuple (first-seen order over the dimension's visible rows), its first row.
    first_rows: Vec<usize>,
    /// A fact row's key → its tuple.
    lookup: Lookup,
}

/// The dimension `d` of fact Table `f` for group-by columns `cols` (module doc): `None` when it
/// doesn't qualify.
fn dim(
    ev: &mut Evaluator<'_, '_>,
    f: usize,
    d: usize,
    cols: &[usize],
    fc: &FilterContext,
    span: Span,
) -> Result<Option<Dim>> {
    let db = ev.db;
    let rels = &db.relationships;
    let mut direct = rels
        .iter()
        .enumerate()
        .filter(|(_, r)| r.active && r.from_table == f && r.to_table == d);
    let (Some((ri, rel)), None) = (direct.next(), direct.next()) else {
        return Ok(None);
    };
    let from_f = reachable(db, f);
    let other_path = rels.iter().enumerate().any(|(i, r)| {
        i != ri
            && r.active
            && r.to_table == d
            && (r.from_table == f || from_f.get(r.from_table) == Some(&true))
    });
    if other_path || reachable(db, d).get(f) == Some(&true) {
        return Ok(None);
    }
    let (Some(dt), Some(fkcol)) = (
        db.tables.get(d),
        db.tables
            .get(f)
            .and_then(|t| t.columns.get(rel.from_column))
            .map(|c| &c.data),
    ) else {
        return Ok(None);
    };
    let Some(key) = dt.columns.get(rel.to_column).map(|c| &c.data) else {
        return Ok(None);
    };
    let cs: Vec<&Col> = cols
        .iter()
        .filter_map(|c| dt.columns.get(*c).map(|c| &c.data))
        .collect();
    let dmask = ev.mask_memo(d, fc, span)?;
    let mut tuples: HashMap<Vec<Option<Key>>, u32> = HashMap::new();
    let mut first_rows = Vec::new();
    let mut by_key: HashMap<Key, u32> = HashMap::new();
    let mut unique = true;
    let tuple_bytes = cs
        .len()
        .saturating_mul(2 * std::mem::size_of::<Key>())
        .saturating_add(ENTRY_BYTES);
    storage::scan(dt.rows, dmask.as_ref(), ev.cancel, |r| {
        let Some(k) = Key::of(&key.value(r), true) else {
            return Ok(());
        };
        let t: Vec<Option<Key>> = cs.iter().map(|c| Key::of(&c.value(r), false)).collect();
        let next = u32::try_from(first_rows.len()).unwrap_or(NONE);
        let id = match tuples.get(&t) {
            Some(&id) => id,
            None => {
                ev.charge(tuple_bytes, span)?;
                tuples.insert(t, next);
                first_rows.push(r);
                next
            }
        };
        ev.charge(ENTRY_BYTES, span)?;
        if by_key.insert(k, id).is_some() {
            unique = false;
        }
        Ok(())
    })?;
    if !unique || first_rows.len() >= NONE as usize {
        return Ok(None);
    }
    let lookup = match fkcol {
        Col::Dict { values, .. } => {
            let lut: Vec<u32> = (0..values.len())
                .map(|c| {
                    if values.is_null(c) {
                        return NONE;
                    }
                    Key::of(&Value::text(values.value(c)), true)
                        .and_then(|k| by_key.get(&k).copied())
                        .unwrap_or(NONE)
                })
                .collect();
            Lookup::Codes(lut)
        }
        c if c.kind() == Kind::Integer => Lookup::Ints(
            by_key
                .iter()
                .filter_map(|(k, &id)| match k {
                    Key::Int(i) => Some((*i, id)),
                    _ => None,
                })
                .collect(),
        ),
        _ => Lookup::Keys(by_key),
    };
    Ok(Some(Dim {
        table: d,
        fk: rel.from_column,
        first_rows,
        lookup,
    }))
}

/// The Tables reachable from `from` over active Relationships (many → one).
fn reachable(db: &Database, from: usize) -> Vec<bool> {
    let mut reach = vec![false; db.tables.len()];
    let mut queue = vec![from];
    while let Some(t) = queue.pop() {
        for r in &db.relationships {
            if r.active
                && r.from_table == t
                && let Some(slot) = reach.get_mut(r.to_table)
                && !*slot
            {
                *slot = true;
                queue.push(r.to_table);
            }
        }
    }
    reach
}

/// A fact row's key → its dimension tuple ([`NONE`]: no match).
enum Lookup {
    /// Per dictionary code of the key column.
    Codes(Vec<u32>),
    /// By whole-number key.
    Ints(IntMap<i64, u32>),
    /// By key (trailing spaces ignored, as Relationships match).
    Keys(HashMap<Key, u32>),
}

/// Each row's dimension tuple, through `l`.
fn lookup(l: &Lookup, col: &Col, rows: &[usize], out: &mut Vec<u32>) {
    match l {
        Lookup::Codes(lut) => int_classes(col, rows, out, NONE, |c| {
            usize::try_from(c)
                .ok()
                .and_then(|c| lut.get(c).copied())
                .unwrap_or(NONE)
        }),
        Lookup::Ints(m) => {
            int_classes(col, rows, out, NONE, |v| m.get(&v).copied().unwrap_or(NONE))
        }
        Lookup::Keys(m) => out.extend(rows.iter().map(|&r| {
            Key::of(&col.value(r), true)
                .and_then(|k| m.get(&k).copied())
                .unwrap_or(NONE)
        })),
    }
}

/// One part of a row's group key.
struct Part<'a> {
    col: &'a Col,
    how: How,
    /// Classes the part can take: keys are `Σ class × stride`.
    bound: u64,
    stride: u64,
}

enum How {
    Classes(Classes),
    Dim(Lookup),
}

impl Part<'_> {
    /// Adds the part of each row of `rows` to `keys` (a row with no match: [`DROP`]), but for
    /// [`Classes::Keys`] (first-seen classes: [`Absorb`] adds those, in order); `false` when a
    /// class runs past the bound (the per-group path runs instead).
    fn add(&self, rows: &[usize], keys: &mut [u64], classes: &mut Vec<u32>) -> bool {
        classes.clear();
        match &self.how {
            How::Classes(Classes::Keys) => return true,
            How::Classes(c) => c.fill(self.col, rows, classes),
            How::Dim(l) => lookup(l, self.col, rows, classes),
        }
        add_classes(keys, classes, self.bound, self.stride)
    }
}

/// `keys[i] += classes[i] × stride` (a [`NONE`] class drops the row); `false` when a class is
/// past `bound`.
fn add_classes(keys: &mut [u64], classes: &[u32], bound: u64, stride: u64) -> bool {
    for (k, &c) in keys.iter_mut().zip(classes) {
        if *k == DROP {
            continue;
        }
        if c == NONE {
            *k = DROP;
            continue;
        }
        if u64::from(c) >= bound {
            return false;
        }
        *k = k.wrapping_add(u64::from(c).wrapping_mul(stride));
    }
    true
}

/// Rows per batch of the scan (whole 64-row words); the cancel flag is checked per batch.
const BATCH: usize = if cfg!(test) { 128 } else { CHUNK };
/// Batches below which the scan stays on one thread.
#[cfg(not(target_arch = "wasm32"))]
const PARALLEL_BATCHES: usize = if cfg!(test) { 3 } else { 4 };
/// Most threads a scan uses: the scan is bound by memory bandwidth, and on the reference
/// machine (6 cores) 4 threads beat 2 and 8 (`docs/bench.md`).
#[cfg(not(target_arch = "wasm32"))]
const MAX_THREADS: usize = 4;

/// The scan's first phase, a batch of rows at a time: the visible rows, their group keys (but
/// first-seen classes) and each aggregation's values, gathered into flat buffers. It only
/// reads, so batches can run on several threads; [`Absorb`] then takes them in row order, so
/// every group's values are added in the order the per-group path adds them.
struct Scan<'s, 'a> {
    rows: usize,
    mask: Option<&'s Bitset>,
    parts: &'s [Part<'a>],
    /// The fact Table's parts come first.
    n_fact: usize,
    readers: &'s [Reader<'a>],
    cancel: Cancel<'s>,
}

/// One batch's first phase (its buffers reused from batch to batch).
#[derive(Default)]
struct Batch {
    rows: Vec<usize>,
    /// Per row, the fact Table's parts of its key.
    keys: Vec<u64>,
    /// Per row, the dimensions' parts ([`DROP`]: no match); empty without dimensions.
    dkeys: Vec<u64>,
    /// Scratch for the parts' classes.
    classes: Vec<u32>,
    /// Per aggregation, its values.
    gathered: Vec<Gathered>,
}

impl Scan<'_, '_> {
    /// Batch `b`'s first phase into `out` (its buffers reused); `false` when the per-group
    /// path must run.
    fn batch(&self, b: usize, out: &mut Batch) -> Result<bool> {
        self.cancel.check()?;
        let start = b.saturating_mul(BATCH);
        let end = start.saturating_add(BATCH).min(self.rows);
        let Batch {
            rows,
            keys,
            dkeys,
            classes,
            gathered,
        } = out;
        rows.clear();
        match self.mask {
            None => rows.extend(start..end),
            Some(m) => visible_rows(m, start, end, rows),
        }
        let (fact, dim) = self.parts.split_at(self.n_fact.min(self.parts.len()));
        keys.clear();
        keys.resize(rows.len(), 0);
        for p in fact {
            if !p.add(rows, keys, classes) {
                return Ok(false);
            }
        }
        dkeys.clear();
        if !dim.is_empty() {
            dkeys.resize(rows.len(), 0);
            for p in dim {
                if !p.add(rows, dkeys, classes) {
                    return Ok(false);
                }
            }
        }
        gathered.resize_with(self.readers.len(), || Gathered::Nothing);
        for (r, g) in self.readers.iter().zip(gathered.iter_mut()) {
            r.gather(rows, g);
        }
        Ok(true)
    }

    /// Bytes a batch's buffers may take, for the memory cap.
    fn batch_bytes(&self) -> usize {
        let per_row = 16usize
            .saturating_add(if self.parts.len() > self.n_fact { 8 } else { 0 })
            .saturating_add(
                self.readers
                    .iter()
                    .map(Reader::bytes_per_row)
                    .sum::<usize>(),
            );
        per_row.saturating_mul(BATCH.min(self.rows.max(1)))
    }

    /// Both phases over every batch, in order; `false` when the per-group path must run.
    fn run(
        &self,
        ev: &mut Evaluator<'_, '_>,
        state: &mut Absorb,
        accs: &mut [Acc<'_>],
    ) -> Result<bool> {
        let n = self.rows.div_ceil(BATCH);
        let span = state.span;
        #[cfg(not(target_arch = "wasm32"))]
        {
            let threads = if n < PARALLEL_BATCHES {
                1
            } else {
                std::thread::available_parallelism()
                    .map_or(1, std::num::NonZeroUsize::get)
                    .min(MAX_THREADS)
                    .min(n)
            };
            // A worker holds a batch in hand and one queued; this thread one more.
            let in_flight = threads.saturating_mul(2).saturating_add(1);
            if threads > 1
                && ev
                    .charge(self.batch_bytes().saturating_mul(in_flight), span)
                    .is_ok()
            {
                return self.parallel(n, threads, |b| state.absorb(b, self, ev, accs));
            }
        }
        ev.charge(self.batch_bytes(), span)?;
        let mut batch = Batch::default();
        for b in 0..n {
            if !self.batch(b, &mut batch)? || !state.absorb(&mut batch, self, ev, accs)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The first phase on `threads` workers (batch `b` on worker `b % threads`, each with a
    /// queue of one), the second on this thread in batch order. Absorbed batches go back to
    /// their worker to be refilled. A worker the system can't start has its batches done here.
    #[cfg(not(target_arch = "wasm32"))]
    fn parallel(
        &self,
        n: usize,
        threads: usize,
        mut absorb: impl FnMut(&mut Batch) -> Result<bool>,
    ) -> Result<bool> {
        use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
        type Done = Result<Option<Batch>>;
        std::thread::scope(|s| {
            let mut queues: Vec<Option<(Receiver<Done>, SyncSender<Batch>)>> =
                Vec::with_capacity(threads);
            for w in 0..threads {
                let (tx, rx) = sync_channel::<Done>(1);
                let (back, spare) = sync_channel::<Batch>(2);
                let started = std::thread::Builder::new().spawn_scoped(s, move || {
                    let mut b = w;
                    while b < n {
                        let mut batch = spare.try_recv().unwrap_or_default();
                        let r = match self.batch(b, &mut batch) {
                            Ok(true) => Ok(Some(batch)),
                            Ok(false) => Ok(None),
                            Err(e) => Err(e),
                        };
                        let last = !matches!(r, Ok(Some(_)));
                        // A send fails once this thread stops listening: stop too.
                        if tx.send(r).is_err() || last {
                            return;
                        }
                        b = b.saturating_add(threads);
                    }
                });
                queues.push(started.ok().map(|_| (rx, back)));
            }
            // Returning drops the queues, so a worker blocked on a full one stops.
            let mut here = Batch::default();
            for b in 0..n {
                match queues.get(b % threads).and_then(Option::as_ref) {
                    Some((queue, back)) => match queue.recv() {
                        Ok(Ok(Some(mut batch))) => {
                            if !absorb(&mut batch)? {
                                return Ok(false);
                            }
                            // The worker may be done: then the batch is dropped.
                            let _ = back.try_send(batch);
                        }
                        Ok(Err(e)) => return Err(e),
                        Ok(Ok(None)) | Err(_) => return Ok(false),
                    },
                    None => {
                        if !self.batch(b, &mut here)? || !absorb(&mut here)? {
                            return Ok(false);
                        }
                    }
                }
            }
            Ok(true)
        })
    }
}

/// The scan's second phase: groups assigned in row order, then each aggregation's values
/// added.
struct Absorb {
    slots: Slots,
    /// Bytes charged per group.
    group_bytes: usize,
    /// Keys per fact tuple: the dimensions' parts are below it.
    dim_bound: u64,
    with_dims: bool,
    /// Per fact tuple (its key part), its first-seen ordinal (with dimensions only).
    fact_seen: IntMap<u64, u32>,
    /// Per fact tuple ordinal, the first row it was seen on.
    fact_first: Vec<usize>,
    /// Per group, its key and the first row it was seen on.
    group_keys: Vec<u64>,
    first_rows: Vec<usize>,
    /// Per part with first-seen classes ([`Classes::Keys`]), the classes so far.
    key_maps: Vec<HashMap<Option<Key>, u32>>,
    groups: Vec<u32>,
    classes: Vec<u32>,
    span: Span,
}

impl Absorb {
    fn absorb(
        &mut self,
        b: &mut Batch,
        scan: &Scan<'_, '_>,
        ev: &mut Evaluator<'_, '_>,
        accs: &mut [Acc<'_>],
    ) -> Result<bool> {
        let span = self.span;
        for (i, p) in scan.parts.iter().enumerate().take(scan.n_fact) {
            if let How::Classes(Classes::Keys) = &p.how {
                let Some(map) = self.key_maps.get_mut(i) else {
                    return Ok(false);
                };
                self.classes.clear();
                if !keys_fill(map, p.col, &b.rows, &mut self.classes)
                    || !add_classes(&mut b.keys, &self.classes, p.bound, p.stride)
                {
                    return Ok(false);
                }
            }
        }
        if self.with_dims {
            // The fact Table's tuples in first-seen order over every visible row, matched or
            // not (as `groups` sees them).
            for (&k, &r) in b.keys.iter().zip(&b.rows) {
                let fpart = k / self.dim_bound.max(1);
                if let std::collections::hash_map::Entry::Vacant(v) = self.fact_seen.entry(fpart) {
                    ev.charge(ENTRY_BYTES, span)?;
                    v.insert(u32::try_from(self.fact_first.len()).unwrap_or(NONE));
                    self.fact_first.push(r);
                }
            }
            for (k, &d) in b.keys.iter_mut().zip(&b.dkeys) {
                *k = if d == DROP { DROP } else { k.wrapping_add(d) };
            }
        }
        self.groups.clear();
        for (i, &k) in b.keys.iter().enumerate() {
            if k == DROP {
                self.groups.push(NONE);
                continue;
            }
            let next = u32::try_from(self.group_keys.len()).unwrap_or(NONE);
            let slot = match &mut self.slots {
                Slots::Dense(t) => usize::try_from(k).ok().and_then(|k| t.get_mut(k)),
                Slots::Hashed(m) => Some(m.entry(k).or_insert(NONE)),
            };
            let Some(slot) = slot else {
                return Ok(false);
            };
            if *slot == NONE {
                if next == NONE {
                    return Ok(false);
                }
                ev.charge(self.group_bytes, span)?;
                *slot = next;
                self.group_keys.push(k);
                self.first_rows.push(b.rows.get(i).copied().unwrap_or(0));
                for a in accs.iter_mut() {
                    a.grow(self.group_keys.len());
                }
            }
            self.groups.push(*slot);
        }
        for (a, g) in accs.iter_mut().zip(&b.gathered) {
            if !a.add(g, &b.rows, &self.groups, &mut self.classes, ev, span)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

/// The groups seen so far, by key.
enum Slots {
    Dense(Vec<u32>),
    Hashed(IntMap<u64, u32>),
}

/// How one column's values map to whole-number classes: values equal as [`Key`]s compare
/// (text ignoring case) share a class; BLANK has its own.
enum Classes {
    /// Per dictionary code its class (the folded text's).
    Dict { lut: Vec<u32>, blank: u32 },
    /// Whole numbers (or days) `min..`: class `v - min`.
    Range { min: i64, blank: u32 },
    /// True, False, BLANK: 1, 0, 2.
    Bool,
    /// Any other values: classes as first seen ([`keys_fill`], in row order).
    Keys,
}

impl Classes {
    /// The classes of `col` (of a Table of `rows` rows, `visible` of them visible) and how many
    /// there can be; `None` when there could be more than a `u32` holds.
    fn new(col: &Col, rows: usize, visible: usize) -> Option<(Classes, u64)> {
        match col {
            Col::Dict { values, .. } => {
                // ponytail: folds the whole dictionary per query; a load-time "one code per
                // folded value" flag would make the codes the classes.
                let mut ids: HashMap<String, u32> = HashMap::new();
                let mut lut = Vec::with_capacity(values.len());
                for c in 0..values.len() {
                    if values.is_null(c) {
                        lut.push(NONE);
                        continue;
                    }
                    let next = u32::try_from(ids.len()).ok()?;
                    lut.push(*ids.entry(fold(values.value(c))).or_insert(next));
                }
                let blank = u32::try_from(ids.len()).ok()?;
                for c in &mut lut {
                    if *c == NONE {
                        *c = blank;
                    }
                }
                Some((Classes::Dict { lut, blank }, u64::from(blank) + 1))
            }
            Col::I8(_) | Col::I16(_) | Col::I32(_) | Col::I64(_) | Col::Date(_) => {
                let (lo, hi) = int_range(col, rows)?;
                let span = u64::try_from(i128::from(hi) - i128::from(lo) + 1).ok()?;
                if span < DENSE_RANGE {
                    let blank = u32::try_from(span).ok()?;
                    return Some((Classes::Range { min: lo, blank }, span + 1));
                }
                keys_classes(visible)
            }
            Col::Bool(_) => Some((Classes::Bool, 3)),
            _ => keys_classes(visible),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Classes::Dict { lut, .. } => lut.len().saturating_mul(4 + ENTRY_BYTES),
            _ => 0,
        }
    }

    /// Each row's class into `out` (but first-seen classes).
    fn fill(&self, col: &Col, rows: &[usize], out: &mut Vec<u32>) {
        match self {
            Classes::Dict { lut, blank } => {
                let blank = *blank;
                int_classes(col, rows, out, blank, |c| {
                    usize::try_from(c)
                        .ok()
                        .and_then(|c| lut.get(c).copied())
                        .unwrap_or(blank)
                });
            }
            Classes::Range { min, blank } => {
                let (min, blank) = (*min, *blank);
                int_classes(col, rows, out, blank, |v| {
                    u32::try_from(v.wrapping_sub(min)).unwrap_or(blank)
                });
            }
            Classes::Bool => out.extend(rows.iter().map(|&r| match col {
                Col::Bool(a) if r < a.len() && a.is_valid(r) => u32::from(a.value(r)),
                _ => 2,
            })),
            // Not here: [`keys_fill`] assigns them in row order.
            Classes::Keys => out.extend(rows.iter().map(|_| NONE)),
        }
    }
}

fn keys_classes(visible: usize) -> Option<(Classes, u64)> {
    let bound = u64::try_from(visible).ok()?.checked_add(1)?;
    (bound < u64::from(NONE)).then_some((Classes::Keys, bound))
}

/// The smallest and largest raw value of a whole-number or date column (nulls' slots
/// included: they only widen the range); `(0, 0)` when empty.
fn int_range(col: &Col, rows: usize) -> Option<(i64, i64)> {
    fn range<T: ArrowPrimitiveType>(a: &PrimitiveArray<T>, rows: usize) -> (i64, i64)
    where
        T::Native: Into<i64>,
    {
        let values = a.values();
        let n = rows.min(values.len());
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        for &v in values.get(..n).unwrap_or(&[]) {
            let v: i64 = v.into();
            lo = lo.min(v);
            hi = hi.max(v);
        }
        if lo > hi { (0, 0) } else { (lo, hi) }
    }
    Some(match col {
        Col::I8(a) => range(a, rows),
        Col::I16(a) => range(a, rows),
        Col::I32(a) => range(a, rows),
        Col::I64(a) => range(a, rows),
        Col::Date(a) => range(a, rows),
        _ => return None,
    })
}

/// What the scan's first phase reads for one aggregation.
enum Reader<'a> {
    /// Nothing (`COUNTROWS`, or an aggregation that is always BLANK).
    Nothing,
    /// The rows with a value (`COUNT`, `COUNTA`).
    Count(&'a Col),
    /// The numbers, as [`storage::each_number`] reads them.
    Numbers(&'a Col),
    /// The dictionary codes of the rows with a value.
    Codes(&'a Col),
    /// Each row's class (`DISTINCTCOUNT`); first-seen classes are worked out in the second
    /// phase.
    Classes(&'a Col, Classes),
}

/// One batch's values for one aggregation, by position in the batch.
enum Gathered {
    Nothing,
    Marks(Vec<u32>),
    /// The non-blank numbers' payloads ([`Number`] of one variant per column), and their
    /// positions (empty when every row has one).
    Numbers {
        at: Vec<u32>,
        raw: Vec<u64>,
    },
    Codes(Vec<(u32, u32)>),
    Classes(Vec<u32>),
}

impl Reader<'_> {
    /// The values of `rows` into `out` (its buffer reused when it is of the same kind).
    fn gather(&self, rows: &[usize], out: &mut Gathered) {
        match self {
            Reader::Nothing | Reader::Classes(_, Classes::Keys) => *out = Gathered::Nothing,
            Reader::Count(col) => {
                if !matches!(out, Gathered::Marks(_)) {
                    *out = Gathered::Marks(Vec::new());
                }
                if let Gathered::Marks(v) = out {
                    v.clear();
                    count_marks(col, rows, v);
                }
            }
            Reader::Numbers(col) => {
                if !matches!(out, Gathered::Numbers { .. }) {
                    *out = Gathered::Numbers {
                        at: Vec::new(),
                        raw: Vec::new(),
                    };
                }
                if let Gathered::Numbers { at, raw } = out {
                    at.clear();
                    raw.clear();
                    each_number_at(col, rows, |i, x| {
                        at.push(i);
                        raw.push(match x {
                            Number::Int(v) | Number::Currency(v) => v as u64,
                            Number::Float(f) => f.to_bits(),
                        });
                    });
                    if raw.len() == rows.len() {
                        at.clear();
                    }
                }
            }
            Reader::Codes(col) => {
                if !matches!(out, Gathered::Codes(_)) {
                    *out = Gathered::Codes(Vec::new());
                }
                if let Gathered::Codes(v) = out {
                    v.clear();
                    v.extend(
                        rows.iter()
                            .zip(0u32..)
                            .filter_map(|(&r, i)| Some((i, u32::try_from(col.code(r)?).ok()?))),
                    );
                }
            }
            Reader::Classes(col, classes) => {
                if !matches!(out, Gathered::Classes(_)) {
                    *out = Gathered::Classes(Vec::new());
                }
                if let Gathered::Classes(v) = out {
                    v.clear();
                    classes.fill(col, rows, v);
                }
            }
        }
    }

    /// Bytes a row of a batch may cost, for the memory cap.
    fn bytes_per_row(&self) -> usize {
        match self {
            Reader::Nothing => 0,
            Reader::Count(_) | Reader::Classes(..) => 4,
            Reader::Codes(..) => 8,
            Reader::Numbers(_) => 12,
        }
    }
}

/// [`Gathered::Numbers`] read back as (position, number).
struct Numbers<'g> {
    at: &'g [u32],
    raw: &'g [u64],
    form: Form,
}

/// Which [`Number`] variant a column's numbers are.
#[derive(Clone, Copy)]
enum Form {
    Int,
    Float,
    Currency,
}

impl<'g> Numbers<'g> {
    fn of(gathered: &'g Gathered, form: Form) -> Self {
        let (at, raw): (&[u32], &[u64]) = match gathered {
            Gathered::Numbers { at, raw } => (at, raw),
            _ => (&[], &[]),
        };
        Numbers { at, raw, form }
    }

    fn iter(&self) -> impl Iterator<Item = (u32, Number)> + '_ {
        let form = self.form;
        self.raw.iter().zip(0u32..).map(move |(&r, k)| {
            let i = if self.at.is_empty() {
                k
            } else {
                self.at.get(k as usize).copied().unwrap_or(NONE)
            };
            let x = match form {
                Form::Int => Number::Int(r as i64),
                Form::Currency => Number::Currency(r as i64),
                Form::Float => Number::Float(f64::from_bits(r)),
            };
            (i, x)
        })
    }
}

/// One expression's per-group state.
enum Acc<'a> {
    /// `COUNTROWS`, `COUNT`, `COUNTA`.
    Count(Vec<u64>),
    /// `SUM` of whole numbers or Currency: overflow falls back.
    SumInt {
        kind: Kind,
        sum: Vec<i64>,
        any: Vec<bool>,
        overflow: bool,
    },
    /// `SUM` of decimals or dates.
    SumFloat {
        kind: Kind,
        sum: Vec<f64>,
        any: Vec<bool>,
    },
    Average {
        kind: Kind,
        exact: Vec<i128>,
        float: Vec<f64>,
        n: Vec<u64>,
    },
    /// `MIN`/`MAX` of numbers or dates.
    Extreme {
        kind: Kind,
        max: bool,
        best: Vec<Option<Number>>,
    },
    /// `MIN`/`MAX` of dictionary text: per code its rank in folded order; per group the best
    /// (rank, code).
    Text {
        dict: &'a StringArray,
        rank: Vec<u32>,
        max: bool,
        best: Vec<Option<(u32, u32)>>,
    },
    DistinctCount {
        col: &'a Col,
        /// First-seen classes (for a column of [`Classes::Keys`]).
        keys: HashMap<Option<Key>, u32>,
        /// (group, class) pairs seen.
        seen: IntSet<u64>,
        n: Vec<u64>,
    },
    /// Always BLANK (an aggregation of an all-BLANK column).
    Blank,
}

impl<'a> Acc<'a> {
    /// What to read and the state for `agg` of `col` (of a Table of `rows` rows, `visible`
    /// visible); `None` when the per-group path must run (a type the function rejects).
    fn new(
        agg: Agg,
        col: Option<&'a Col>,
        rows: usize,
        visible: usize,
    ) -> Option<(Reader<'a>, Acc<'a>)> {
        let Some(col) = col else {
            return (agg == Agg::CountRows).then(|| (Reader::Nothing, Acc::Count(Vec::new())));
        };
        let kind = col.kind();
        Some(match agg {
            Agg::CountRows => return None,
            Agg::Count if kind == Kind::Boolean => return None,
            Agg::Count | Agg::CountA => (Reader::Count(col), Acc::Count(Vec::new())),
            Agg::Sum | Agg::Average | Agg::Min | Agg::Max if kind == Kind::Blank => {
                (Reader::Nothing, Acc::Blank)
            }
            Agg::Sum if matches!(kind, Kind::Integer | Kind::Currency) => (
                Reader::Numbers(col),
                Acc::SumInt {
                    kind,
                    sum: Vec::new(),
                    any: Vec::new(),
                    overflow: false,
                },
            ),
            Agg::Sum if matches!(kind, Kind::Decimal | Kind::DateTime) => (
                Reader::Numbers(col),
                Acc::SumFloat {
                    kind,
                    sum: Vec::new(),
                    any: Vec::new(),
                },
            ),
            Agg::Average if kind.is_numeric() => (
                Reader::Numbers(col),
                Acc::Average {
                    kind,
                    exact: Vec::new(),
                    float: Vec::new(),
                    n: Vec::new(),
                },
            ),
            Agg::Min | Agg::Max if kind.is_numeric() => (
                Reader::Numbers(col),
                Acc::Extreme {
                    kind,
                    max: agg == Agg::Max,
                    best: Vec::new(),
                },
            ),
            Agg::Min | Agg::Max => {
                // ponytail: plain (undictionaried) text falls back; loads dictionary-encode.
                let dict = col.dictionary()?;
                (
                    Reader::Codes(col),
                    Acc::Text {
                        dict,
                        rank: text_ranks(dict),
                        max: agg == Agg::Max,
                        best: Vec::new(),
                    },
                )
            }
            Agg::DistinctCount => {
                let (classes, _) = Classes::new(col, rows, visible)?;
                (
                    Reader::Classes(col, classes),
                    Acc::DistinctCount {
                        col,
                        keys: HashMap::new(),
                        seen: IntSet::default(),
                        n: Vec::new(),
                    },
                )
            }
            Agg::Sum | Agg::Average => return None,
        })
    }

    /// The variant of the numbers this aggregation reads (by the column's kind).
    fn form(&self) -> Form {
        let kind = match self {
            Acc::SumInt { kind, .. }
            | Acc::SumFloat { kind, .. }
            | Acc::Average { kind, .. }
            | Acc::Extreme { kind, .. } => *kind,
            _ => Kind::Decimal,
        };
        match kind {
            Kind::Integer => Form::Int,
            Kind::Currency => Form::Currency,
            _ => Form::Float,
        }
    }

    /// Room for `n` groups.
    fn grow(&mut self, n: usize) {
        match self {
            Acc::Count(c) | Acc::DistinctCount { n: c, .. } => c.resize(n, 0),
            Acc::SumInt { sum, any, .. } => {
                sum.resize(n, 0);
                any.resize(n, false);
            }
            Acc::SumFloat { sum, any, .. } => {
                sum.resize(n, 0.0);
                any.resize(n, false);
            }
            Acc::Average {
                exact, float, n: c, ..
            } => {
                exact.resize(n, 0);
                float.resize(n, 0.0);
                c.resize(n, 0);
            }
            Acc::Extreme { best, .. } => best.resize(n, None),
            Acc::Text { best, .. } => best.resize(n, None),
            Acc::Blank => {}
        }
    }

    /// Adds one batch: its `gathered` values of rows `rows` in groups `groups` ([`NONE`]: in
    /// none); `false` when the per-group path must run (a whole-number `SUM` overflowed,
    /// classes ran out).
    fn add(
        &mut self,
        gathered: &Gathered,
        rows: &[usize],
        groups: &[u32],
        classes: &mut Vec<u32>,
        ev: &mut Evaluator<'_, '_>,
        span: Span,
    ) -> Result<bool> {
        let group = |i: u32| -> Option<usize> {
            groups
                .get(i as usize)
                .copied()
                .filter(|&g| g != NONE)
                .map(|g| g as usize)
        };
        let numbers = Numbers::of(gathered, self.form());
        match self {
            Acc::Count(n) => {
                let mut add = |g: Option<usize>| {
                    if let Some(c) = g.and_then(|g| n.get_mut(g)) {
                        *c += 1;
                    }
                };
                match gathered {
                    Gathered::Marks(marks) => marks.iter().for_each(|&i| add(group(i))),
                    // COUNTROWS: every row.
                    _ => groups
                        .iter()
                        .for_each(|&g| add((g != NONE).then_some(g as usize))),
                }
            }
            Acc::SumInt {
                sum, any, overflow, ..
            } => {
                for (i, x) in numbers.iter() {
                    let Some(g) = group(i) else {
                        continue;
                    };
                    if let (Some(acc), Some(a)) = (sum.get_mut(g), any.get_mut(g)) {
                        *a = true;
                        if let Number::Int(v) | Number::Currency(v) = x {
                            match acc.checked_add(v) {
                                Some(v) => *acc = v,
                                None => *overflow = true,
                            }
                        }
                    }
                }
                if *overflow {
                    return Ok(false);
                }
            }
            Acc::SumFloat { sum, any, .. } => {
                for (i, x) in numbers.iter() {
                    if let (Some(g), Number::Float(f)) = (group(i), x)
                        && let (Some(acc), Some(a)) = (sum.get_mut(g), any.get_mut(g))
                    {
                        *a = true;
                        *acc += f;
                    }
                }
            }
            Acc::Average {
                exact, float, n, ..
            } => {
                for (i, x) in numbers.iter() {
                    let Some(g) = group(i) else {
                        continue;
                    };
                    if let Some(c) = n.get_mut(g) {
                        *c = c.saturating_add(1);
                    }
                    match x {
                        Number::Int(v) | Number::Currency(v) => {
                            if let Some(e) = exact.get_mut(g) {
                                *e = e.saturating_add(i128::from(v));
                            }
                        }
                        Number::Float(f) => {
                            if let Some(a) = float.get_mut(g) {
                                *a += f;
                            }
                        }
                    }
                }
            }
            Acc::Extreme { max, best, .. } => {
                for (i, x) in numbers.iter() {
                    if let Some(b) = group(i).and_then(|g| best.get_mut(g))
                        && better(*b, x, *max)
                    {
                        *b = Some(x);
                    }
                }
            }
            Acc::Text {
                rank, max, best, ..
            } => {
                let Gathered::Codes(codes) = gathered else {
                    return Ok(true);
                };
                for &(i, c) in codes {
                    let (Some(&r), Some(b)) =
                        (rank.get(c as usize), group(i).and_then(|g| best.get_mut(g)))
                    else {
                        continue;
                    };
                    let take = match *b {
                        None => true,
                        Some((br, bc)) if *max => r > br || (r == br && c < bc),
                        Some(cur) => (r, c) < cur,
                    };
                    if take {
                        *b = Some((r, c));
                    }
                }
            }
            Acc::DistinctCount { col, keys, seen, n } => {
                let row_classes: &[u32] = match gathered {
                    Gathered::Classes(v) => v,
                    _ => {
                        classes.clear();
                        if !keys_fill(keys, col, rows, classes) {
                            return Ok(false);
                        }
                        classes
                    }
                };
                for (&c, &g) in row_classes.iter().zip(groups) {
                    if g == NONE {
                        continue;
                    }
                    if seen.insert((u64::from(g) << 32) | u64::from(c)) {
                        ev.charge(ENTRY_BYTES, span)?;
                        if let Some(k) = n.get_mut(g as usize) {
                            *k += 1;
                        }
                    }
                }
            }
            Acc::Blank => {}
        }
        Ok(true)
    }

    /// Group `g`'s value; `None` when the per-group path must run.
    fn value(&self, g: usize) -> Option<Value> {
        Some(match self {
            Acc::Count(n) | Acc::DistinctCount { n, .. } => {
                match i64::try_from(n.get(g).copied().unwrap_or(0)).ok()? {
                    0 => Value::Blank,
                    n => Value::Integer(n),
                }
            }
            Acc::SumInt { kind, sum, any, .. } => match (any.get(g), sum.get(g)) {
                (Some(true), Some(&s)) if *kind == Kind::Currency => Value::Currency(s),
                (Some(true), Some(&s)) => Value::Integer(s),
                _ => Value::Blank,
            },
            Acc::SumFloat { kind, sum, any } => match (any.get(g), sum.get(g)) {
                (Some(true), Some(&s)) if *kind == Kind::DateTime => Value::DateTime(s),
                (Some(true), Some(&s)) => Value::Decimal(s),
                _ => Value::Blank,
            },
            Acc::Average {
                kind,
                exact,
                float,
                n,
            } => {
                let c = n.get(g).copied().unwrap_or(0);
                if c == 0 {
                    return Some(Value::Blank);
                }
                let count = c as f64;
                let exact = exact.get(g).copied().unwrap_or(0);
                let mean = match kind {
                    Kind::Integer => exact as f64 / count,
                    Kind::Currency => {
                        exact as f64 / metriccraft_core::CURRENCY_SCALE as f64 / count
                    }
                    _ => float.get(g).copied().unwrap_or(0.0) / count,
                };
                if *kind == Kind::DateTime {
                    Value::DateTime(mean)
                } else {
                    Value::Decimal(mean)
                }
            }
            Acc::Extreme { kind, best, .. } => match best.get(g).copied().flatten() {
                None => Value::Blank,
                Some(Number::Int(i)) => Value::Integer(i),
                Some(Number::Currency(c)) => Value::Currency(c),
                Some(Number::Float(f)) if *kind == Kind::DateTime => Value::DateTime(f),
                Some(Number::Float(f)) => Value::Decimal(f),
            },
            Acc::Text { dict, best, .. } => match best.get(g).copied().flatten() {
                None => Value::Blank,
                Some((_, c)) => Value::text(dict.value(usize::try_from(c).ok()?)),
            },
            Acc::Blank => Value::Blank,
        })
    }
}

/// `MIN`/`MAX`'s comparison (`aggregate::extreme_scan`): whether `x` replaces `best`.
fn better(best: Option<Number>, x: Number, max: bool) -> bool {
    match (best, x) {
        (None, Number::Float(f)) => !f.is_nan(),
        (None, _) => true,
        (Some(Number::Int(b)), Number::Int(v))
        | (Some(Number::Currency(b)), Number::Currency(v)) => {
            if max {
                v > b
            } else {
                v < b
            }
        }
        (Some(Number::Float(b)), Number::Float(v)) => {
            if max {
                v > b
            } else {
                v < b
            }
        }
        (Some(_), _) => false,
    }
}

/// Per dictionary code, its rank in folded-text order (codes equal ignoring case share one).
fn text_ranks(dict: &StringArray) -> Vec<u32> {
    let mut codes: Vec<(String, usize)> = (0..dict.len())
        .filter(|&c| dict.is_valid(c))
        .map(|c| (fold(dict.value(c)), c))
        .collect();
    codes.sort();
    let mut rank = vec![0u32; dict.len()];
    let mut r = 0u32;
    let mut prev: Option<&str> = None;
    for (s, c) in &codes {
        if prev.is_some_and(|p| p != s.as_str()) {
            r = r.saturating_add(1);
        }
        prev = Some(s.as_str());
        if let Some(slot) = rank.get_mut(*c) {
            *slot = r;
        }
    }
    rank
}

/// The rows of `start..end` in `mask`, in order (`start` a multiple of 64: whole words).
fn visible_rows(mask: &Bitset, start: usize, end: usize, out: &mut Vec<usize>) {
    let first = start / 64;
    let words = mask.words().get(first..end.div_ceil(64)).unwrap_or(&[]);
    for (k, &word) in words.iter().enumerate() {
        let base = (first + k) * 64;
        let mut w = word;
        while w != 0 {
            let r = base + w.trailing_zeros() as usize;
            if r >= end {
                break;
            }
            w &= w - 1;
            out.push(r);
        }
    }
}

/// Calls `f(i, value)` for each position `i` of `rows` whose row is non-null in `a`.
#[inline]
fn each_at<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    rows: &[usize],
    mut f: impl FnMut(u32, T::Native),
) {
    let values = a.values();
    match a.nulls().filter(|n| n.null_count() > 0) {
        None => {
            for (&r, i) in rows.iter().zip(0u32..) {
                if let Some(&v) = values.get(r) {
                    f(i, v);
                }
            }
        }
        Some(n) => {
            for (&r, i) in rows.iter().zip(0u32..) {
                if r < n.len()
                    && n.is_valid(r)
                    && let Some(&v) = values.get(r)
                {
                    f(i, v);
                }
            }
        }
    }
}

/// Calls `f(i, number)` for each position `i` of `rows` whose row holds a non-blank number,
/// read as [`storage::each_number`] reads it.
#[inline]
fn each_number_at(col: &Col, rows: &[usize], mut f: impl FnMut(u32, Number)) {
    let divisor = |scale: i8| 10f64.powi(i32::from(scale));
    match col {
        Col::I8(a) => each_at(a, rows, |i, v| f(i, Number::Int(v.into()))),
        Col::I16(a) => each_at(a, rows, |i, v| f(i, Number::Int(v.into()))),
        Col::I32(a) => each_at(a, rows, |i, v| f(i, Number::Int(v.into()))),
        Col::I64(a) => each_at(a, rows, |i, v| f(i, Number::Int(v))),
        Col::F32(a) => each_at(a, rows, |i, v| f(i, Number::Float(v.into()))),
        Col::F64(a) => each_at(a, rows, |i, v| f(i, Number::Float(v))),
        Col::D32(a, 4) => each_at(a, rows, |i, v| f(i, Number::Currency(v.into()))),
        Col::D64(a, 4) => each_at(a, rows, |i, v| f(i, Number::Currency(v))),
        Col::D32(a, s) => {
            let d = divisor(*s);
            each_at(a, rows, |i, v| f(i, Number::Float(f64::from(v) / d)));
        }
        Col::D64(a, s) => {
            let d = divisor(*s);
            each_at(a, rows, |i, v| f(i, Number::Float(v as f64 / d)));
        }
        Col::Date(a) => each_at(a, rows, |i, d| {
            f(i, Number::Float(f64::from(d) + UNIX_EPOCH_DAYS));
        }),
        Col::Ts(a, per_day) => {
            let per_day = *per_day;
            each_at(a, rows, |i, t| {
                f(i, Number::Float(t as f64 / per_day + UNIX_EPOCH_DAYS));
            });
        }
        Col::Dict { .. } | Col::Text(_) | Col::Bool(_) | Col::Null(_) => {}
    }
}

/// `out` extended with `class(value)` for each row (`blank` for a null or a row past `a`).
#[inline]
fn classes_at<T: ArrowPrimitiveType>(
    a: &PrimitiveArray<T>,
    rows: &[usize],
    out: &mut Vec<u32>,
    blank: u32,
    class: impl Fn(T::Native) -> u32,
) {
    let values = a.values();
    match a.nulls().filter(|n| n.null_count() > 0) {
        None => out.extend(
            rows.iter()
                .map(|&r| values.get(r).map_or(blank, |&v| class(v))),
        ),
        Some(n) => out.extend(rows.iter().map(|&r| {
            if r < n.len() && n.is_valid(r) {
                values.get(r).map_or(blank, |&v| class(v))
            } else {
                blank
            }
        })),
    }
}

/// `out` extended with each row's class through `f` of its raw whole number (codes, integers,
/// days); `blank` for a null. Other columns: all `blank`.
#[inline]
fn int_classes(col: &Col, rows: &[usize], out: &mut Vec<u32>, blank: u32, f: impl Fn(i64) -> u32) {
    match col {
        Col::Dict { codes, .. } => match codes {
            Codes::I8(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
            Codes::I16(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
            Codes::I32(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
        },
        Col::I8(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
        Col::I16(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
        Col::I32(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
        Col::I64(a) => classes_at(a, rows, out, blank, f),
        Col::Date(a) => classes_at(a, rows, out, blank, |v| f(v.into())),
        _ => out.extend(rows.iter().map(|_| blank)),
    }
}

/// Each row's first-seen class (BLANK one too) into `out`, `map` holding the classes so far;
/// `false` when classes run out.
fn keys_fill(
    map: &mut HashMap<Option<Key>, u32>,
    col: &Col,
    rows: &[usize],
    out: &mut Vec<u32>,
) -> bool {
    for &r in rows {
        let next = u32::try_from(map.len()).unwrap_or(NONE);
        if next == NONE {
            return false;
        }
        out.push(*map.entry(Key::of(&col.value(r), false)).or_insert(next));
    }
    true
}

/// The positions in `rows` of the rows with a value (`storage::count_values`), into `marks`.
fn count_marks(col: &Col, rows: &[usize], marks: &mut Vec<u32>) {
    match col {
        Col::Dict { .. } => marks.extend(
            rows.iter()
                .zip(0u32..)
                .filter(|(r, _)| col.code(**r).is_some())
                .map(|(_, i)| i),
        ),
        _ => {
            if let Some(array) = col.array() {
                let len = array.len();
                let all = array.null_count() == 0;
                marks.extend(
                    rows.iter()
                        .zip(0u32..)
                        .filter(|(r, _)| **r < len && (all || array.is_valid(**r)))
                        .map(|(_, i)| i),
                );
            }
        }
    }
}
