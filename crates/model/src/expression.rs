//! Measures and Calculated columns (CONTEXT.md; ADR 0003's M3.6 amendment; ADR 0006): the DAX
//! the Model holds, the rules for their names, the dependencies between them, and the rewrite of
//! their DAX when a Table, Column or Measure is renamed (spec D-4).
//!
//! - **Text is kept verbatim.** A Measure's or Calculated column's DAX is stored as written and
//!   parsed when needed; one that doesn't parse is kept, with its error ([`Measure::parse`]).
//! - **Names.** A Measure belongs to a home Table; its name is unique among the Model's Measures
//!   and never equals a Column name of its home Table (ignoring case, as all names). A Calculated
//!   column is a Column: unique in its Table, and never a Measure name of that Table.
//! - **Dependencies** ([`Model::dependencies`]): a Measure or Calculated column depends on the
//!   Measures and Calculated columns its DAX names. `[X]` is a Measure when one is named `X`,
//!   else a Calculated column `X` of the expression's own Table, else of any Table; `T[X]` is
//!   `T`'s Calculated column or Measure `X`. Defining or editing one that closes a cycle through
//!   it, or that makes a chain of Measures referencing Measures longer than
//!   [`MAX_MEASURE_CHAIN`], fails and changes nothing.
//! - **Renames rewrite the DAX through the AST**: the parser gives each reference's byte span,
//!   and only those bytes are replaced, so the author's formatting, comments and parentheses
//!   (which the AST doesn't keep) stay as written. DAX that doesn't parse is left as it is.
//!
//! Parity uncertainties, to settle in the VM:
//! - whether a Measure name may equal a Column name of another Table (we allow it) and Power
//!   BI's wording for each refusal;
//! - which `[X]` a rename rewrites when it isn't qualified: we rewrite an unqualified `[old]`
//!   in expressions of the renamed Column's own Table, and elsewhere only when no other Table
//!   has a Column of that name;
//! - a renamed Table referenced unquoted is written unquoted when it can be (`Sales` →
//!   `Orders`), quoted when the reference was.

use std::collections::{BTreeSet, HashMap, VecDeque};

use opendash_dax::{Expr, ExprKind, Span, is_keyword, parse_expr};

use crate::dax::MAX_MEASURE_CHAIN;
use crate::table::{Error, Result};
use crate::{Model, ModelTable, name_key, same_name};

/// A named DAX expression evaluated on demand under the current filter context.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Measure {
    pub name: String,
    /// The DAX as written (lines joined with `\n`).
    pub expression: String,
    /// A Power BI format string; empty for the default. Rendered in M3.8.
    pub format_string: String,
    pub description: String,
    pub hidden: bool,
}

impl Measure {
    pub fn new(name: &str, expression: &str) -> Self {
        Self {
            name: name.into(),
            expression: expression.into(),
            ..Self::default()
        }
    }

    /// The parsed expression, or why it doesn't parse.
    pub fn parse(&self) -> std::result::Result<Expr, opendash_dax::Error> {
        parse_expr(&self.expression)
    }
}

/// DAX text as the Model keeps it: line breaks as `\n`, no trailing blank lines or spaces (what
/// the `.odm` block holds, so a save and open give the same text).
pub fn normalise_expression(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_end()
        .to_string()
}

/// One name a DAX expression refers to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Ref<'e> {
    /// A bare name no `VAR` shadows: a Table.
    Table { name: &'e str, span: Span },
    /// `T[C]` or `[C]`.
    Column {
        table: Option<&'e str>,
        column: &'e str,
        span: Span,
    },
}

/// Every Table, Column and Measure reference in `e`, in source order. Recursion follows the
/// AST, whose depth the parser bounds.
pub(crate) fn references(e: &Expr) -> Vec<Ref<'_>> {
    fn walk<'e>(e: &'e Expr, vars: &mut Vec<&'e str>, out: &mut Vec<Ref<'e>>) {
        match &e.kind {
            ExprKind::Name(n) => {
                if !vars.iter().any(|v| same_name(v, n)) {
                    out.push(Ref::Table {
                        name: n,
                        span: e.span,
                    });
                }
            }
            ExprKind::Column { table, column } => out.push(Ref::Column {
                table: table.as_deref(),
                column,
                span: e.span,
            }),
            ExprKind::Call { args, .. } => args.iter().for_each(|a| walk(a, vars, out)),
            ExprKind::Unary(_, x) => walk(x, vars, out),
            ExprKind::Binary(_, l, r) => {
                walk(l, vars, out);
                walk(r, vars, out);
            }
            ExprKind::Var { vars: defs, body } => {
                let base = vars.len();
                for (name, x) in defs {
                    walk(x, vars, out);
                    vars.push(name);
                }
                walk(body, vars, out);
                vars.truncate(base);
            }
            ExprKind::Table(rows) => rows.iter().flatten().for_each(|x| walk(x, vars, out)),
            ExprKind::Integer(_) | ExprKind::Decimal(_) | ExprKind::Text(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(e, &mut Vec::new(), &mut out);
    out.sort_by_key(|r| match r {
        Ref::Table { span, .. } | Ref::Column { span, .. } => span.start,
    });
    out
}

/// `at` moved past the blanks and comments that start `text[at..]`.
fn skip_trivia(text: &str, mut at: usize) -> Option<usize> {
    loop {
        let rest = text.get(at..)?;
        let trimmed = rest.trim_start();
        at = at.saturating_add(rest.len() - trimmed.len());
        if trimmed.starts_with("//") || trimmed.starts_with("--") {
            let line = trimmed.find('\n').unwrap_or(trimmed.len());
            at = at.saturating_add(line);
        } else if let Some(body) = trimmed.strip_prefix("/*") {
            let close = body.find("*/")?;
            at = at.saturating_add(close.saturating_add(4));
        } else {
            return Some(at);
        }
    }
}

/// The byte length of the `open`…`close` token `text` starts with, a doubled `close` being
/// an escaped one (`'…'` Table names, `[…]` Columns).
fn delimited_len(text: &str, open: u8, close: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&open) {
        return None;
    }
    let mut i = 1usize;
    loop {
        match bytes.get(i) {
            Some(&b) if b == close && bytes.get(i.saturating_add(1)) == Some(&close) => {
                i = i.saturating_add(2);
            }
            Some(&b) if b == close => return Some(i.saturating_add(1)),
            Some(_) => i = i.saturating_add(1),
            None => return None,
        }
    }
}

/// The byte length of the Table name `text` starts with: `'…'` or a plain name (dotted
/// segments, as the lexer reads one).
fn table_name_len(text: &str) -> Option<usize> {
    if text.starts_with('\'') {
        return delimited_len(text, b'\'', b'\'');
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    let mut end = 0usize;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let dotted = c == '.' && end > 0 && chars.peek().is_some_and(|&(_, d)| word(d));
        if word(c) || dotted {
            end = i.saturating_add(c.len_utf8());
        } else {
            break;
        }
    }
    (end > 0).then_some(end)
}

/// Where the reference an expression spanning `span` holds starts. A parenthesised
/// reference's span covers its parentheses (`( [X] )`), so step past every `(`, blank and
/// comment it opens with.
fn reference_start(src: &str, span: Span) -> Option<usize> {
    let text = src.get(span.start..span.end)?;
    let mut at = 0usize;
    loop {
        at = skip_trivia(text, at)?;
        if text.get(at..)?.starts_with('(') {
            at = at.saturating_add(1);
        } else {
            return Some(span.start.saturating_add(at));
        }
    }
}

/// The span of the Table name a [`Ref::Table`] spanning `span` is written as (inside any
/// parentheses).
fn table_part(src: &str, span: Span) -> Option<Span> {
    let start = reference_start(src, span)?;
    let len = table_name_len(src.get(start..span.end)?)?;
    Some(Span::new(start, start.saturating_add(len)))
}

/// The spans of a column reference's Table part (if any) and its `[Column]` part in `src`
/// (inside any parentheses).
fn column_parts(src: &str, span: Span) -> Option<(Option<Span>, Span)> {
    let start = reference_start(src, span)?;
    let text = src.get(start..span.end)?;
    if let Some(len) = delimited_len(text, b'[', b']') {
        return Some((None, Span::new(start, start.saturating_add(len))));
    }
    let table_len = table_name_len(text)?;
    // Blanks and comments may sit between the Table and its `[Column]`.
    let at = skip_trivia(text, table_len)?;
    let column_len = delimited_len(text.get(at..)?, b'[', b']')?;
    let table = Span::new(start, start.saturating_add(table_len));
    let column_start = start.saturating_add(at);
    let column = Span::new(column_start, column_start.saturating_add(column_len));
    Some((Some(table), column))
}

/// A Table name as DAX text: plain when it reads back as one name (and the reference it
/// replaces wasn't quoted), else `'…'`.
fn table_text(name: &str, was_quoted: bool) -> String {
    let plain = !was_quoted
        && !is_keyword(name)
        && !name.eq_ignore_ascii_case("TRUE")
        && !name.eq_ignore_ascii_case("FALSE")
        && name.split('.').enumerate().all(|(i, seg)| {
            let mut chars = seg.chars();
            chars.next().is_some_and(|c| {
                c == '_'
                    || if i == 0 {
                        c.is_alphabetic()
                    } else {
                        c.is_alphanumeric()
                    }
            }) && chars.all(|c| c.is_alphanumeric() || c == '_')
        });
    if plain {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

fn bracket_text(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

/// `src` with each `(span, text)` edit applied (spans don't overlap). `None` if nothing changes.
fn apply(src: &str, mut edits: Vec<(Span, String)>) -> Option<String> {
    if edits.is_empty() {
        return None;
    }
    edits.sort_by_key(|(s, _)| std::cmp::Reverse(s.start));
    let mut out = src.to_string();
    for (span, text) in edits {
        if out.get(span.start..span.end).is_some() {
            out.replace_range(span.start..span.end, &text);
        }
    }
    (out != src).then_some(out)
}

/// What a rename renames.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Renamed<'a> {
    Table {
        old: &'a str,
        new: &'a str,
    },
    /// A Column of `table`. `unqualified`: whether `[old]` without a Table means it in the
    /// expression being rewritten.
    Column {
        table: &'a str,
        old: &'a str,
        new: &'a str,
        unqualified: bool,
    },
    Measure {
        home: &'a str,
        old: &'a str,
        new: &'a str,
    },
}

/// `src` with every reference to what `r` renames rewritten; `None` if `src` doesn't parse or
/// has none.
pub(crate) fn rewrite(src: &str, r: Renamed<'_>) -> Option<String> {
    let e = parse_expr(src).ok()?;
    let mut edits = Vec::new();
    for reference in references(&e) {
        match (reference, r) {
            (Ref::Table { name, span }, Renamed::Table { old, new }) if same_name(name, old) => {
                if let Some(ts) = table_part(src, span) {
                    let quoted = src.get(ts.start..).is_some_and(|s| s.starts_with('\''));
                    edits.push((ts, table_text(new, quoted)));
                }
            }
            (
                Ref::Column {
                    table: Some(t),
                    span,
                    ..
                },
                Renamed::Table { old, new },
            ) if same_name(t, old) => {
                if let Some((Some(ts), _)) = column_parts(src, span) {
                    let quoted = src.get(ts.start..).is_some_and(|s| s.starts_with('\''));
                    edits.push((ts, table_text(new, quoted)));
                }
            }
            (
                Ref::Column {
                    table,
                    column,
                    span,
                },
                Renamed::Column {
                    table: owner,
                    old,
                    new,
                    unqualified,
                },
            ) if same_name(column, old) && table.map_or(unqualified, |t| same_name(t, owner)) => {
                if let Some((_, cs)) = column_parts(src, span) {
                    edits.push((cs, bracket_text(new)));
                }
            }
            (
                Ref::Column {
                    table,
                    column,
                    span,
                },
                Renamed::Measure { home, old, new },
            ) if same_name(column, old) && table.is_none_or(|t| same_name(t, home)) => {
                if let Some((_, cs)) = column_parts(src, span) {
                    edits.push((cs, bracket_text(new)));
                }
            }
            _ => {}
        }
    }
    apply(src, edits)
}

/// A Measure or Calculated column, by position in the Model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Object {
    /// `tables[table].measures[index]`.
    Measure { table: usize, index: usize },
    /// `tables[table].columns[index]` (a Calculated column).
    Column { table: usize, index: usize },
}

/// The dependency graph of the Model's Measures and Calculated columns.
pub struct Dependencies {
    pub objects: Vec<Object>,
    /// `edges[i]`: the objects `objects[i]`'s DAX uses (indices into `objects`).
    pub edges: Vec<Vec<usize>>,
}

impl Dependencies {
    fn index(&self, o: Object) -> Option<usize> {
        self.objects.iter().position(|x| *x == o)
    }

    /// A cycle through `start`, as the objects on it (from `start`), if there is one.
    fn cycle_through(&self, start: usize) -> Option<Vec<usize>> {
        let mut parent: HashMap<usize, usize> = HashMap::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        for &n in self.edges.get(start)? {
            if n == start {
                return Some(vec![start]);
            }
            if parent.insert(n, start).is_none() {
                queue.push_back(n);
            }
        }
        while let Some(at) = queue.pop_front() {
            for &n in self.edges.get(at).map_or(&[][..], Vec::as_slice) {
                if n == start {
                    let mut path = vec![at];
                    let mut x = at;
                    while let Some(&p) = parent.get(&x) {
                        if p == start {
                            break;
                        }
                        path.push(p);
                        x = p;
                    }
                    path.push(start);
                    path.reverse();
                    return Some(path);
                }
                if let std::collections::hash_map::Entry::Vacant(e) = parent.entry(n) {
                    e.insert(at);
                    queue.push_back(n);
                }
            }
        }
        None
    }

    /// The objects in an order where each comes after those it uses; objects on or after a
    /// cycle are left out (Kahn's algorithm, iterative).
    pub fn order(&self) -> Vec<usize> {
        let n = self.objects.len();
        let mut users: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut pending: Vec<usize> = vec![0; n];
        for (i, deps) in self.edges.iter().enumerate() {
            let deps: BTreeSet<usize> = deps.iter().copied().collect();
            for d in deps {
                if let (Some(u), Some(p)) = (users.get_mut(d), pending.get_mut(i)) {
                    u.push(i);
                    *p = p.saturating_add(1);
                }
            }
        }
        let mut ready: VecDeque<usize> = (0..n).filter(|&i| pending.get(i) == Some(&0)).collect();
        let mut out = Vec::with_capacity(n);
        while let Some(i) = ready.pop_front() {
            out.push(i);
            for &u in users.get(i).map_or(&[][..], Vec::as_slice) {
                if let Some(p) = pending.get_mut(u) {
                    *p = p.saturating_sub(1);
                    if *p == 0 {
                        ready.push_back(u);
                    }
                }
            }
        }
        out
    }

    /// The longest chain of Measures referencing Measures that goes through `start` (counting
    /// both ends), ignoring cycles.
    fn measure_chain_through(&self, start: usize) -> usize {
        let is_measure = |i: usize| matches!(self.objects.get(i), Some(Object::Measure { .. }));
        let measure_edges: Vec<Vec<usize>> = self
            .edges
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if is_measure(i) {
                    e.iter().copied().filter(|&d| is_measure(d)).collect()
                } else {
                    Vec::new()
                }
            })
            .collect();
        let order = Dependencies {
            objects: self.objects.clone(),
            edges: measure_edges.clone(),
        }
        .order();
        // `down[i]`: the longest chain from `i` through what it uses (dependencies first in
        // `order`); `up[i]`: through what uses it.
        let n = self.objects.len();
        let mut down = vec![1usize; n];
        for &i in &order {
            let best = measure_edges.get(i).map_or(0, |e| {
                e.iter()
                    .filter_map(|&d| down.get(d))
                    .copied()
                    .max()
                    .unwrap_or(0)
            });
            if let Some(x) = down.get_mut(i) {
                *x = best.saturating_add(1);
            }
        }
        let mut up = vec![1usize; n];
        for &i in order.iter().rev() {
            let here = up.get(i).copied().unwrap_or(1);
            for &d in measure_edges.get(i).map_or(&[][..], Vec::as_slice) {
                if let Some(x) = up.get_mut(d) {
                    *x = (*x).max(here.saturating_add(1));
                }
            }
        }
        if !is_measure(start) {
            return 0;
        }
        down.get(start)
            .copied()
            .unwrap_or(1)
            .saturating_add(up.get(start).copied().unwrap_or(1))
            .saturating_sub(1)
    }
}

impl Model {
    /// The Measure named `name` (ignoring case), anywhere in the Model, with its home Table.
    pub fn measure(&self, name: &str) -> Option<(&ModelTable, &Measure)> {
        self.tables
            .iter()
            .find_map(|t| t.measure(name).map(|m| (t, m)))
    }

    fn expression_of(&self, o: Object) -> Option<(&str, usize)> {
        match o {
            Object::Measure { table, index } => self
                .tables
                .get(table)?
                .measures
                .get(index)
                .map(|m| (m.expression.as_str(), table)),
            Object::Column { table, index } => self
                .tables
                .get(table)?
                .columns
                .get(index)?
                .expression
                .as_deref()
                .map(|e| (e, table)),
        }
    }

    /// The name of a Measure or Calculated column, as `'Table'[Name]`.
    pub fn object_name(&self, o: Object) -> String {
        let (table, name) = match o {
            Object::Measure { table, index } => {
                let t = self.tables.get(table);
                (t, t.and_then(|t| t.measures.get(index)).map(|m| &m.name))
            }
            Object::Column { table, index } => {
                let t = self.tables.get(table);
                (t, t.and_then(|t| t.columns.get(index)).map(|c| &c.name))
            }
        };
        format!(
            "'{}'[{}]",
            table.map_or("", |t| t.name.as_str()),
            name.map_or("", String::as_str)
        )
    }

    /// Every Measure and Calculated column, and what each one's DAX uses (see the module doc).
    pub fn dependencies(&self) -> Dependencies {
        let mut objects = Vec::new();
        for (ti, t) in self.tables.iter().enumerate() {
            for (ci, c) in t.columns.iter().enumerate() {
                if c.expression.is_some() {
                    objects.push(Object::Column {
                        table: ti,
                        index: ci,
                    });
                }
            }
            for mi in 0..t.measures.len() {
                objects.push(Object::Measure {
                    table: ti,
                    index: mi,
                });
            }
        }
        // Folded names → objects, hashed once, so resolving every reference is linear in the
        // Model rather than a scan of all objects per reference (a file may hold 50k Measures).
        // `None` keys any Table; the first object in `objects` order wins, as a scan would.
        let mut measures: HashMap<(Option<usize>, String), usize> = HashMap::new();
        let mut calc: HashMap<(Option<usize>, String), Vec<usize>> = HashMap::new();
        for (i, &o) in objects.iter().enumerate() {
            let (Object::Measure { table, index } | Object::Column { table, index }) = o;
            let t = self.tables.get(table);
            match o {
                Object::Measure { .. } => {
                    if let Some(m) = t.and_then(|t| t.measures.get(index)) {
                        let key = name_key(&m.name);
                        measures.entry((None, key.clone())).or_insert(i);
                        measures.entry((Some(table), key)).or_insert(i);
                    }
                }
                Object::Column { .. } => {
                    if let Some(c) = t.and_then(|t| t.columns.get(index)) {
                        let key = name_key(&c.name);
                        calc.entry((None, key.clone())).or_default().push(i);
                        calc.entry((Some(table), key)).or_default().push(i);
                    }
                }
            }
        }
        let find_measure =
            |table: Option<usize>, name: &str| measures.get(&(table, name_key(name))).copied();
        let calc_columns = |table: Option<usize>, name: &str| -> Vec<usize> {
            calc.get(&(table, name_key(name)))
                .cloned()
                .unwrap_or_default()
        };
        let mut tables: HashMap<String, usize> = HashMap::new();
        for (i, t) in self.tables.iter().enumerate() {
            tables.entry(name_key(&t.name)).or_insert(i);
        }
        let table_index = |name: &str| tables.get(&name_key(name)).copied();
        let edges = objects
            .iter()
            .map(|&o| {
                let Some((text, home)) = self.expression_of(o) else {
                    return Vec::new();
                };
                let Ok(e) = parse_expr(text) else {
                    return Vec::new();
                };
                let mut deps = Vec::new();
                for r in references(&e) {
                    let Ref::Column { table, column, .. } = r else {
                        continue;
                    };
                    match table {
                        None => {
                            if let Some(m) = find_measure(None, column) {
                                deps.push(m);
                            } else {
                                let own = calc_columns(Some(home), column);
                                if own.is_empty() {
                                    deps.extend(calc_columns(None, column));
                                } else {
                                    deps.extend(own);
                                }
                            }
                        }
                        Some(t) => {
                            let Some(t) = table_index(t) else { continue };
                            deps.extend(calc_columns(Some(t), column));
                            deps.extend(find_measure(Some(t), column));
                        }
                    }
                }
                deps.sort_unstable();
                deps.dedup();
                deps
            })
            .collect();
        Dependencies { objects, edges }
    }

    /// Checks the Measure or Calculated column `o` closes no cycle and no Measure chain longer
    /// than [`MAX_MEASURE_CHAIN`] goes through it.
    pub(crate) fn check_dependencies(&self, o: Object) -> Result {
        let deps = self.dependencies();
        let Some(i) = deps.index(o) else {
            return Ok(());
        };
        let (table, name) = self.object_key(o);
        if let Some(cycle) = deps.cycle_through(i) {
            let names: Vec<String> = cycle
                .iter()
                .chain(cycle.first())
                .filter_map(|&k| deps.objects.get(k))
                .map(|&x| self.object_name(x))
                .collect();
            return Err(Error {
                table,
                column: name,
                message: format!("A circular dependency was detected: {}.", names.join(" → ")),
            });
        }
        let chain = deps.measure_chain_through(i);
        if chain > MAX_MEASURE_CHAIN {
            return Err(Error {
                table,
                column: name,
                message: format!(
                    "Measures would refer to Measures {chain} deep; at most \
                     {MAX_MEASURE_CHAIN} is allowed"
                ),
            });
        }
        Ok(())
    }

    fn object_key(&self, o: Object) -> (String, String) {
        let (Object::Measure { table, index } | Object::Column { table, index }) = o;
        let t = self.tables.get(table);
        let name = match o {
            Object::Measure { .. } => t.and_then(|t| t.measures.get(index)).map(|m| &m.name),
            Object::Column { .. } => t.and_then(|t| t.columns.get(index)).map(|c| &c.name),
        };
        (
            t.map(|t| t.name.clone()).unwrap_or_default(),
            name.cloned().unwrap_or_default(),
        )
    }

    fn table_index(&self, table: &str) -> Result<usize> {
        self.tables
            .iter()
            .position(|t| t.name == table)
            .ok_or_else(|| Error {
                table: table.into(),
                column: String::new(),
                message: "there is no such table".into(),
            })
    }

    /// Checks `name` is free for a new Measure or Calculated column of Table `table` (see the
    /// module doc); `except` is the object being renamed.
    fn check_free(&self, table: usize, name: &str, except: Option<Object>) -> Result {
        let t = self.tables.get(table);
        let err = |message: String| Error {
            table: t.map(|t| t.name.clone()).unwrap_or_default(),
            column: name.into(),
            message,
        };
        if name.trim().is_empty() {
            return Err(err("the name must not be empty".into()));
        }
        for (ti, other) in self.tables.iter().enumerate() {
            for (mi, m) in other.measures.iter().enumerate() {
                let this = Object::Measure {
                    table: ti,
                    index: mi,
                };
                if except != Some(this) && same_name(&m.name, name) {
                    return Err(err(format!(
                        "The name '{name}' is already used by the Measure '{}'[{}]",
                        other.name, m.name
                    )));
                }
            }
        }
        if let Some(t) = t {
            for (ci, c) in t.columns.iter().enumerate() {
                let this = Object::Column { table, index: ci };
                if except != Some(this) && same_name(&c.name, name) {
                    return Err(err(format!(
                        "The name '{name}' is already used by the Column '{}'[{}]",
                        t.name, c.name
                    )));
                }
            }
        }
        Ok(())
    }

    /// Adds Measure `measure` to Table `table` (Measures are kept sorted by name). Fails,
    /// changing nothing, if its name is taken or its DAX closes a cycle or too long a chain.
    pub fn add_measure(&mut self, table: &str, measure: Measure) -> Result {
        let ti = self.table_index(table)?;
        self.check_free(ti, &measure.name, None)?;
        let t = self.tables.get_mut(ti).ok_or_else(|| no_table(table))?;
        let index = t
            .measures
            .iter()
            .position(|m| measure_order(m, &measure).is_gt())
            .unwrap_or(t.measures.len());
        t.measures.insert(index, measure);
        let r = self.check_dependencies(Object::Measure { table: ti, index });
        if r.is_err()
            && let Some(t) = self.tables.get_mut(ti)
        {
            t.measures.remove(index);
        }
        r
    }

    /// Adds a Calculated column `name` with DAX `expression` to Table `table`, last. Its data
    /// type is unknown (text) until it is evaluated ([`Model::compute_calculated`]). Fails,
    /// changing nothing, like [`Model::add_measure`].
    pub fn add_calculated_column(&mut self, table: &str, name: &str, expression: &str) -> Result {
        let ti = self.table_index(table)?;
        self.check_free(ti, name, None)?;
        let t = self.tables.get_mut(ti).ok_or_else(|| no_table(table))?;
        let mut c = crate::Column::new(name, opendash_data::step::ColumnType::Text);
        c.expression = Some(expression.into());
        t.columns.push(c);
        let index = t.columns.len().saturating_sub(1);
        let r = self.check_dependencies(Object::Column { table: ti, index });
        if r.is_err()
            && let Some(t) = self.tables.get_mut(ti)
        {
            t.columns.pop();
        }
        r
    }

    /// The Measure or Calculated column `name` of Table `table`.
    pub fn object(&self, table: &str, name: &str) -> Result<Object> {
        let ti = self.table_index(table)?;
        let t = self.tables.get(ti).ok_or_else(|| no_table(table))?;
        if let Some(index) = t.measures.iter().position(|m| m.name == name) {
            return Ok(Object::Measure { table: ti, index });
        }
        match t.columns.iter().position(|c| c.name == name) {
            Some(index) if t.columns.get(index).is_some_and(|c| c.expression.is_some()) => {
                Ok(Object::Column { table: ti, index })
            }
            Some(_) => Err(Error {
                table: table.into(),
                column: name.into(),
                message: "this Column comes from the Query: it has no DAX expression".into(),
            }),
            None => Err(Error {
                table: table.into(),
                column: name.into(),
                message: "there is no Measure or Calculated column of this name".into(),
            }),
        }
    }

    /// Replaces the DAX of Measure or Calculated column `name` of Table `table`. Fails,
    /// changing nothing, if it would close a cycle or too long a Measure chain.
    pub fn set_expression(&mut self, table: &str, name: &str, expression: &str) -> Result {
        let o = self.object(table, name)?;
        let old = self.replace_expression(o, expression.into());
        let r = self.check_dependencies(o);
        if r.is_err() {
            self.replace_expression(o, old);
        }
        r
    }

    fn replace_expression(&mut self, o: Object, text: String) -> String {
        match o {
            Object::Measure { table, index } => self
                .tables
                .get_mut(table)
                .and_then(|t| t.measures.get_mut(index))
                .map(|m| std::mem::replace(&mut m.expression, text))
                .unwrap_or_default(),
            Object::Column { table, index } => self
                .tables
                .get_mut(table)
                .and_then(|t| t.columns.get_mut(index))
                .and_then(|c| c.expression.replace(text))
                .unwrap_or_default(),
        }
    }

    /// Deletes Measure or Calculated column `name` of Table `table`. A Calculated column takes
    /// its Relationships and the sort by of Columns sorted by it with it. DAX that used it is
    /// kept (and fails until fixed), as in Power BI.
    pub fn delete_expression(&mut self, table: &str, name: &str) -> Result<Object> {
        let o = self.object(table, name)?;
        match o {
            Object::Measure { table: ti, index } => {
                if let Some(t) = self.tables.get_mut(ti) {
                    t.measures.remove(index);
                }
            }
            Object::Column { table: ti, index } => {
                if let Some(t) = self.tables.get_mut(ti) {
                    t.columns.remove(index);
                    for c in &mut t.columns {
                        if c.sort_by.as_deref() == Some(name) {
                            c.sort_by = None;
                        }
                    }
                }
                self.relationships.retain(|r| {
                    !((r.from_table == table && r.from_column == name)
                        || (r.to_table == table && r.to_column == name))
                });
            }
        }
        Ok(o)
    }

    /// Renames Measure `old` of Table `table` to `new`, rewriting every reference to it. Fails,
    /// changing nothing, if `new` is taken.
    pub fn rename_measure(&mut self, table: &str, old: &str, new: &str) -> Result {
        let o = self.object(table, old)?;
        let Object::Measure { table: ti, index } = o else {
            return Err(Error {
                table: table.into(),
                column: old.into(),
                message: "this is a Column, not a Measure".into(),
            });
        };
        self.check_free(ti, new, Some(o))?;
        self.rewrite_all(|_| Renamed::Measure {
            home: table,
            old,
            new,
        });
        if let Some(t) = self.tables.get_mut(ti) {
            let mut m = t.measures.remove(index);
            m.name = new.into();
            let at = t
                .measures
                .iter()
                .position(|x| measure_order(x, &m).is_gt())
                .unwrap_or(t.measures.len());
            t.measures.insert(at, m);
        }
        Ok(())
    }

    /// Rewrites every Measure's and Calculated column's DAX for a rename; `r` gets the name of
    /// the Table whose expression it is.
    pub(crate) fn rewrite_all<'a>(&mut self, r: impl Fn(&str) -> Renamed<'a>) {
        for t in &mut self.tables {
            let rename = r(&t.name);
            for m in &mut t.measures {
                if let Some(text) = rewrite(&m.expression, rename) {
                    m.expression = text;
                }
            }
            for c in &mut t.columns {
                if let Some(e) = &mut c.expression
                    && let Some(text) = rewrite(e, rename)
                {
                    *e = text;
                }
            }
        }
    }
}

fn no_table(table: &str) -> Error {
    Error {
        table: table.into(),
        column: String::new(),
        message: "there is no such table".into(),
    }
}

/// How Measures sort in a Table (and in its `.odm` block): by name ignoring case, then exactly.
pub fn measure_order(a: &Measure, b: &Measure) -> std::cmp::Ordering {
    a.name
        .to_lowercase()
        .cmp(&b.name.to_lowercase())
        .then_with(|| a.name.cmp(&b.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rename(src: &str, r: Renamed<'_>) -> String {
        rewrite(src, r).unwrap_or_else(|| src.to_string())
    }

    #[test]
    fn references_skip_variables_and_find_every_column() {
        let e =
            parse_expr("VAR Sales = 1 RETURN SUMX(Sales, [Qty]) + 'Other T'[X] + Sales").unwrap();
        let refs = references(&e);
        assert_eq!(refs.len(), 2, "{refs:?}");
        assert!(matches!(
            refs[0],
            Ref::Column {
                table: None,
                column: "Qty",
                ..
            }
        ));
        let e = parse_expr("SUMX(Sales, Sales[Qty])").unwrap();
        assert!(matches!(
            references(&e)[0],
            Ref::Table { name: "Sales", .. }
        ));
    }

    #[test]
    fn a_table_rename_keeps_formatting_comments_and_parentheses() {
        let src = "( SUM ( Sales[Qty] ) ) /* Sales */\n  + COUNTROWS ( 'Sales' ) -- Sales\n  + Sales  [Qty]";
        let out = rename(
            src,
            Renamed::Table {
                old: "sales",
                new: "Order Lines",
            },
        );
        assert_eq!(
            out,
            "( SUM ( 'Order Lines'[Qty] ) ) /* Sales */\n  + COUNTROWS ( 'Order Lines' ) -- Sales\n  + 'Order Lines'  [Qty]"
        );
        let out = rename(
            "SUM(Sales[Qty])",
            Renamed::Table {
                old: "Sales",
                new: "Orders",
            },
        );
        assert_eq!(out, "SUM(Orders[Qty])");
        let out = rename(
            "SUM('Sales'[Qty])",
            Renamed::Table {
                old: "Sales",
                new: "Orders",
            },
        );
        assert_eq!(out, "SUM('Orders'[Qty])", "quoted stays quoted");
        let out = rename(
            "VAR Sales = 2 RETURN Sales",
            Renamed::Table {
                old: "Sales",
                new: "X",
            },
        );
        assert_eq!(
            out, "VAR Sales = 2 RETURN Sales",
            "a variable isn't the Table"
        );
    }

    #[test]
    fn a_column_rename_rewrites_qualified_and_unqualified_references() {
        let r = Renamed::Column {
            table: "Sales",
            old: "Qty",
            new: "Units ]",
            unqualified: true,
        };
        assert_eq!(
            rename("SUMX(Sales, [qty] * Sales[Qty]) + Other[Qty]", r),
            "SUMX(Sales, [Units ]]] * Sales[Units ]]]) + Other[Qty]"
        );
        let r = Renamed::Column {
            table: "Sales",
            old: "Qty",
            new: "Units",
            unqualified: false,
        };
        assert_eq!(rename("[Qty] + Sales[Qty]", r), "[Qty] + Sales[Units]");
    }

    #[test]
    fn a_measure_rename_and_unparsable_text() {
        let r = Renamed::Measure {
            home: "Sales",
            old: "Total",
            new: "Sum",
        };
        assert_eq!(
            rename("[Total] * 2 + Sales[Total] + Other[Total]", r),
            "[Sum] * 2 + Sales[Sum] + Other[Total]"
        );
        assert_eq!(
            rewrite("SUM(Sales[Total]", r),
            None,
            "doesn't parse: left as is"
        );
    }

    // Regression (M3 review): a parenthesised reference's span covers its parentheses, and the
    // rewrite used to find no reference in `([Total])` and drop the edit, or replace `((Sales))`
    // parentheses and all.
    #[test]
    fn renames_reach_parenthesised_references() {
        let measure = Renamed::Measure {
            home: "Sales",
            old: "Total",
            new: "Sum",
        };
        assert_eq!(rename("([Total]) * 2", measure), "([Sum]) * 2");
        assert_eq!(
            rename("( /* c */ ( Sales[Total] ) ) + (-- x\n[Total])", measure),
            "( /* c */ ( Sales[Sum] ) ) + (-- x\n[Sum])"
        );
        let table = Renamed::Table {
            old: "Sales",
            new: "Orders",
        };
        assert_eq!(
            rename("SUMX(Sales, (Sales[Qty]))", table),
            "SUMX(Orders, (Orders[Qty]))"
        );
        assert_eq!(
            rename("COUNTROWS(((Sales)))", table),
            "COUNTROWS(((Orders)))"
        );
        assert_eq!(
            rename("COUNTROWS(( 'Sales' ))", table),
            "COUNTROWS(( 'Orders' ))"
        );
        let column = Renamed::Column {
            table: "Sales",
            old: "Qty",
            new: "Units",
            unqualified: true,
        };
        assert_eq!(
            rename("SUMX(Sales, (Sales[Qty])) + ([Qty])", column),
            "SUMX(Sales, (Sales[Units])) + ([Units])"
        );
    }

    /// How many references `src` makes to Table `old` (`.0`) and to Columns or Measures named
    /// `old` (`.1`).
    fn old_names(src: &str) -> (usize, usize) {
        let e = parse_expr(src).unwrap_or_else(|err| panic!("{src}: {err}"));
        let (mut tables, mut columns) = (0, 0);
        for r in references(&e) {
            let (table, column) = match r {
                Ref::Table { name, .. } => (Some(name), None),
                Ref::Column { table, column, .. } => (table, Some(column)),
            };
            tables += usize::from(table.is_some_and(|t| same_name(t, "old")));
            columns += usize::from(column.is_some_and(|c| same_name(c, "old")));
        }
        (tables, columns)
    }

    #[test]
    fn renames_reach_references_however_they_are_parenthesised() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        let refs = [
            "[Old]",
            "Old[Qty]",
            "'Old' [Old]",
            "COUNTROWS(Old)",
            "SUMX('Old', Old /* x */ [Old])",
        ];
        let trivia = [" ", "", "/* ( */", "-- )\n", "\n  "];
        for _ in 0..500 {
            let mut terms = Vec::new();
            for _ in 0..=next(4) {
                let mut term = refs[next(refs.len())].to_string();
                for _ in 0..next(4) {
                    let (a, b) = (trivia[next(trivia.len())], trivia[next(trivia.len())]);
                    term = format!("({a}{term}{b})");
                }
                terms.push(term);
            }
            let src = terms.join(" + ");
            let parens = |s: &str| (s.matches('(').count(), s.matches(')').count());
            let out = rename(
                &src,
                Renamed::Table {
                    old: "Old",
                    new: "New",
                },
            );
            assert_eq!(parens(&out), parens(&src), "{src} → {out}");
            assert_eq!(old_names(&out).0, 0, "{src} → {out}");
            for r in [
                Renamed::Column {
                    table: "Old",
                    old: "Old",
                    new: "New",
                    unqualified: true,
                },
                Renamed::Measure {
                    home: "Old",
                    old: "Old",
                    new: "New",
                },
            ] {
                let out = rename(&src, r);
                assert_eq!(parens(&out), parens(&src), "{src} → {out}");
                assert_eq!(old_names(&out).1, 0, "{r:?}: {src} → {out}");
            }
        }
    }

    // Regression (M3 review): resolving references scanned every object per reference, so a
    // Model of 50k Measures took minutes.
    #[test]
    fn dependencies_of_many_measures_are_linear() {
        let n = 50_000;
        let mut model = Model::default();
        for t in 0..2 {
            model.tables.push(ModelTable {
                name: format!("T{t}"),
                measures: (0..n / 2)
                    .map(|i| Measure::new(&format!("M{t}_{i}"), "[M0_0] + T1[m1_0]"))
                    .collect(),
                ..ModelTable::default()
            });
        }
        let started = std::time::Instant::now();
        let deps = model.dependencies();
        assert!(started.elapsed().as_secs() < 5, "{:?}", started.elapsed());
        assert_eq!(deps.objects.len(), n);
        assert_eq!(deps.edges.get(5).map(Vec::len), Some(2));
    }

    #[test]
    fn normalised_text_has_lf_and_no_trailing_blanks() {
        assert_eq!(normalise_expression("a\r\nb\rc  \n\n "), "a\nb\nc");
    }
}
