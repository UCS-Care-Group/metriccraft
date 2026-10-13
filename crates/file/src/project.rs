//! A Project's content ↔ its files (ADR 0003 and its M2 amendment).
//!
//! ```text
//! project.odash.json        Manifest (manifest.rs)
//! model/model.odm           model Model: culture, autodetectRelationships, ref query/table order
//! model/relationships.odm   relationship Sales[Region] -> Regions[Region] (+ inactive); omitted
//!                           when there are none
//! model/tables/<t>.odm      table, its columns and their properties
//! queries/<q>.odq           query: load, culture (when not the Project's), m: block
//! report/…                  reserved for the report (M4); not written yet
//! .gitignore                .cache/, .local/, .staging/ — written on create, then the user's
//! ```
//!
//! **Determinism.** Every part is printed by [`syntax::print`]; Queries, Tables and columns keep
//! user order (Tables and Queries through `ref` lines in `model.odm`, since file names sort),
//! Relationships keep Manage relationships order. Nothing machine- or time-dependent is written.
//!
//! **Column types.** `dataType` is the Model's; `queryType` (the Query's at the last load) is
//! written only when it differs, i.e. when the Model overrides it.
//!
//! **Source paths.** A file inside the Project's folder (`root`) is written relative to it, with
//! `/`; any other path absolute. On read, relative paths are resolved against `root`.
//! ponytail: `.local/` remapping of source paths (Local overrides) is still to come.
//!
//! **Unknown content.** Keys, flags, blocks and objects we don't know, in any part, are kept
//! ([`Preserved`], per object: model, relationships, each table, each query) and written back
//! after the known items of the object they were in. Unknown parts listed in the Manifest are
//! kept as they are; unknown Manifest keys too. A rename carries an object's unknown items to
//! its new name ([`Preserved::rename_table`], [`Preserved::rename_column`]).
//!
//! **No data.** The parts are definitions only; the Data cache (`.cache/`) is [`crate::cache`],
//! written and read beside them by the app.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use arrow_array::RecordBatch;
use arrow_schema::{Field, Schema as ArrowSchema};
use opendash_data::culture::Culture;
use opendash_data::m::{self, MQuery};
use opendash_data::source::{File, Source};
use opendash_data::step::ColumnType;
use opendash_data::{Query, Table, TableBuilder};
use opendash_model::{Column, Model, ModelTable, Relationship, SummarizeBy};
use serde_json::{Map, Value as Json};

use crate::escape::file_stems;
use crate::manifest::{FORMAT_VERSION, MANIFEST, MIGRATIONS, Manifest, Migration, Tree, hash};
use crate::syntax::{self, Item, Node, Value, quote_name, read_name};
use crate::{Error, FileSet, Result};

pub const MODEL: &str = "model/model.odm";
pub const RELATIONSHIPS: &str = "model/relationships.odm";
pub const TABLES: &str = "model/tables/";
pub const QUERIES: &str = "queries/";
/// Reserved for report parts (M4): `report/report.json`, `report/pages/<pageId>/…`.
pub const REPORT: &str = "report/";
pub const GITIGNORE: &str = ".gitignore";
/// The `.gitignore` written when a Project is created.
pub const GITIGNORE_TEXT: &str = ".cache/\n.local/\n.staging/\n";
/// The Project culture when `model.odm` doesn't name one (Get Data's default).
pub const DEFAULT_CULTURE: &str = "en-US";

/// What a Project holds (besides data and the report, still to come).
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    /// The Project culture's name: the culture of Queries that don't name their own.
    pub culture: String,
    /// In Queries pane order.
    pub queries: Vec<Query>,
    /// Queries with *Enable load* off.
    pub unloaded: BTreeSet<String>,
    pub model: Model,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            culture: DEFAULT_CULTURE.into(),
            queries: Vec::new(),
            unloaded: BTreeSet::new(),
            model: Model::default(),
        }
    }
}

/// What a Project's files held beyond what we model, carried to the next save.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preserved {
    trees: BTreeMap<Key, Vec<Item>>,
    manifest: Map<String, Json>,
    parts: BTreeMap<String, Vec<u8>>,
    gitignore: Option<Vec<u8>>,
}

impl Preserved {
    /// Whether nothing unknown was read.
    pub fn is_empty(&self) -> bool {
        self.manifest.is_empty() && self.parts.is_empty() && self.trees.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Model,
    Relationships,
    Table(String),
    Query(String),
}

// ---- what each object may hold (anything else is unknown and preserved) ----

struct Schema {
    nodes: &'static [(&'static str, &'static Schema)],
    props: &'static [&'static str],
    flags: &'static [&'static str],
    blocks: &'static [&'static str],
}

const LEAF: Schema = Schema {
    nodes: &[],
    props: &[],
    flags: &[],
    blocks: &[],
};
static COLUMN: Schema = Schema {
    props: &[
        "dataType",
        "queryType",
        "formatString",
        "summarizeBy",
        "sortByColumn",
        "description",
    ],
    flags: &["hidden"],
    ..LEAF
};
static TABLE: Schema = Schema {
    nodes: &[("column", &COLUMN)],
    props: &["description"],
    flags: &["hidden"],
    ..LEAF
};
static TABLE_FILE: Schema = Schema {
    nodes: &[("table", &TABLE)],
    ..LEAF
};
static QUERY: Schema = Schema {
    props: &["load", "culture"],
    blocks: &["m"],
    ..LEAF
};
static QUERY_FILE: Schema = Schema {
    nodes: &[("query", &QUERY)],
    ..LEAF
};
static REF: Schema = LEAF;
static MODEL_NODE: Schema = Schema {
    nodes: &[("ref", &REF)],
    props: &["culture", "autodetectRelationships"],
    ..LEAF
};
static MODEL_FILE: Schema = Schema {
    nodes: &[("model", &MODEL_NODE)],
    ..LEAF
};
static RELATIONSHIP: Schema = Schema {
    flags: &["inactive"],
    ..LEAF
};
static RELATIONSHIPS_FILE: Schema = Schema {
    nodes: &[("relationship", &RELATIONSHIP)],
    ..LEAF
};

/// Adds `old`'s unknown items to `new` (after its own), recursing into known objects present in
/// both (matched by kind and name).
fn merge(new: &mut Vec<Item>, old: &[Item], schema: &Schema) {
    for item in old {
        let known = match item {
            Item::Node(o) => match schema.nodes.iter().find(|(k, _)| *k == o.kind) {
                Some((_, child)) => {
                    let target = new.iter_mut().find_map(|i| match i {
                        Item::Node(n) if n.kind == o.kind && same_header(&n.header, &o.header) => {
                            Some(n)
                        }
                        _ => None,
                    });
                    if let Some(n) = target {
                        merge(&mut n.items, &o.items, child);
                    }
                    true
                }
                None => false,
            },
            Item::Prop(k, _) => schema.props.contains(&k.as_str()),
            Item::Flag(f) => schema.flags.contains(&f.as_str()),
            Item::Block(k, _) => schema.blocks.contains(&k.as_str()),
        };
        if !known {
            new.push(item.clone());
        }
    }
}

/// A header in the form we print it: a lone name, a relationship, or `word name`.
fn canonical(header: &str) -> Option<String> {
    if let Some((name, "")) = read_name(header) {
        return Some(quote_name(&name));
    }
    if let Some((ft, fc, tt, tc)) = parse_relationship(header) {
        return Some(relationship_header(&ft, &fc, &tt, &tc));
    }
    let (word, rest) = header.split_once(' ')?;
    match read_name(rest) {
        Some((name, "")) => Some(format!("{word} {}", quote_name(&name))),
        _ => None,
    }
}

fn same_header(a: &str, b: &str) -> bool {
    a == b || canonical(a).is_some_and(|ca| canonical(b).is_some_and(|cb| ca == cb))
}

// ---- writing ----

/// A name as a property value: bare when it can be, else `'quoted'`.
fn name_value(name: &str) -> Value {
    if quote_name(name) == name {
        Value::Bare(name.into())
    } else {
        Value::Name(name.into())
    }
}

fn bare(s: &str) -> Value {
    Value::Bare(s.into())
}

fn bracket(column: &str) -> String {
    format!("[{}]", column.replace(']', "]]"))
}

fn relationship_header(ft: &str, fc: &str, tt: &str, tc: &str) -> String {
    format!(
        "{}{} -> {}{}",
        quote_name(ft),
        bracket(fc),
        quote_name(tt),
        bracket(tc)
    )
}

/// `[name]` (with `]]` for `]`) at the start of `s`: the name and the rest.
fn read_bracket(s: &str) -> Option<(String, &str)> {
    let body = s.strip_prefix('[')?;
    let mut name = String::new();
    let mut chars = body.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == ']' {
            if body.get(i + 1..)?.starts_with(']') {
                name.push(']');
                chars.next();
            } else {
                return Some((name, body.get(i + 1..)?));
            }
        } else {
            name.push(c);
        }
    }
    None
}

/// `From[Col] -> To[Col]` → (from table, from column, to table, to column).
fn parse_relationship(h: &str) -> Option<(String, String, String, String)> {
    let (ft, rest) = read_name(h)?;
    let (fc, rest) = read_bracket(rest)?;
    let rest = rest.strip_prefix(" -> ")?;
    let (tt, rest) = read_name(rest)?;
    let (tc, rest) = read_bracket(rest)?;
    rest.is_empty().then_some((ft, fc, tt, tc))
}

pub(crate) fn source_file_mut(source: &mut Source) -> &mut File {
    match source {
        Source::Csv { file, .. } | Source::Parquet { file } | Source::Excel { file, .. } => file,
    }
}

/// `path` relative to `root` with `/` separators, if it is inside it.
pub(crate) fn relative(path: &Path, root: &Path) -> Option<String> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rest
        .components()
        .map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    let parts = parts.filter(|p| !p.is_empty())?;
    Some(parts.join("/"))
}

fn query_node(q: &Query, project: &Project, root: Option<&Path>) -> Node {
    let mut source = q.source.clone();
    if let (File::Path(p), Some(root)) = (source_file_mut(&mut source), root)
        && p.is_absolute()
        && let Some(rel) = relative(p, root)
    {
        *p = PathBuf::from(rel);
    }
    let own_culture = (q.culture.name != project.culture).then_some(q.culture);
    let text = m::print(&MQuery {
        source,
        steps: q.steps.clone(),
        culture: own_culture,
    });
    let mut node = Node::named("query", &q.name);
    let load = if project.unloaded.contains(&q.name) {
        "false"
    } else {
        "true"
    };
    node.items.push(Item::Prop("load".into(), bare(load)));
    if let Some(c) = own_culture {
        node.items.push(Item::Prop("culture".into(), bare(c.name)));
    }
    node.items.push(Item::Block("m".into(), text));
    node
}

fn column_node(c: &Column) -> Node {
    let mut node = Node::named("column", &c.name);
    let mut prop = |k: &str, v: Value| node.items.push(Item::Prop(k.into(), v));
    prop("dataType", bare(c.data_type.name()));
    if c.overridden() {
        prop("queryType", bare(c.query_type.name()));
    }
    if !c.format_string.is_empty() {
        prop("formatString", Value::Text(c.format_string.clone()));
    }
    prop("summarizeBy", bare(c.summarize_by.name()));
    if let Some(by) = &c.sort_by {
        prop("sortByColumn", name_value(by));
    }
    if !c.description.is_empty() {
        prop("description", Value::Text(c.description.clone()));
    }
    if c.hidden {
        node.items.push(Item::Flag("hidden".into()));
    }
    node
}

fn table_node(t: &ModelTable) -> Node {
    let mut node = Node::named("table", &t.name);
    if !t.description.is_empty() {
        node.items.push(Item::Prop(
            "description".into(),
            Value::Text(t.description.clone()),
        ));
    }
    if t.hidden {
        node.items.push(Item::Flag("hidden".into()));
    }
    node.items
        .extend(t.columns.iter().map(|c| Item::Node(column_node(c))));
    node
}

fn model_node(project: &Project) -> Node {
    let mut node = Node::named("model", "Model");
    node.items
        .push(Item::Prop("culture".into(), bare(&project.culture)));
    let auto = if project.model.autodetect_relationships {
        "true"
    } else {
        "false"
    };
    node.items
        .push(Item::Prop("autodetectRelationships".into(), bare(auto)));
    let refs = |kind: &str, name: &str| {
        Item::Node(Node {
            kind: "ref".into(),
            header: format!("{kind} {}", quote_name(name)),
            items: Vec::new(),
        })
    };
    node.items
        .extend(project.queries.iter().map(|q| refs("query", &q.name)));
    node.items
        .extend(project.model.tables.iter().map(|t| refs("table", &t.name)));
    node
}

/// Refuses names the syntax can't hold (line breaks).
fn check_names(project: &Project) -> Result {
    let bad = |what: &str, name: &str| {
        if name.contains(['\n', '\r']) {
            Err(Error::Save(format!(
                "{what} `{}` has a line break in its name; rename it first",
                name.escape_debug()
            )))
        } else {
            Ok(())
        }
    };
    for q in &project.queries {
        bad("query", &q.name)?;
    }
    for t in &project.model.tables {
        bad("table", &t.name)?;
        for c in &t.columns {
            bad("column", &c.name)?;
        }
    }
    Ok(())
}

/// The Project's files: Manifest, parts and `.gitignore` (see the module docs). `root` is the
/// folder source paths are made relative to (`None`: keep them as they are).
pub fn encode(project: &Project, root: Option<&Path>, preserved: &Preserved) -> Result<FileSet> {
    check_names(project)?;
    let mut parts = FileSet::new();
    let print = |mut items: Vec<Item>, key: &Key, schema: &Schema| {
        if let Some(old) = preserved.trees.get(key) {
            merge(&mut items, old, schema);
        }
        (!items.is_empty()).then(|| syntax::print(&items).into_bytes())
    };

    let names: Vec<&str> = project.queries.iter().map(|q| q.name.as_str()).collect();
    for (q, stem) in project.queries.iter().zip(file_stems(&names)) {
        let items = vec![Item::Node(query_node(q, project, root))];
        if let Some(bytes) = print(items, &Key::Query(q.name.clone()), &QUERY_FILE) {
            parts.insert(format!("{QUERIES}{stem}.odq"), bytes);
        }
    }
    let names: Vec<&str> = project
        .model
        .tables
        .iter()
        .map(|t| t.name.as_str())
        .collect();
    for (t, stem) in project.model.tables.iter().zip(file_stems(&names)) {
        let items = vec![Item::Node(table_node(t))];
        if let Some(bytes) = print(items, &Key::Table(t.name.clone()), &TABLE_FILE) {
            parts.insert(format!("{TABLES}{stem}.odm"), bytes);
        }
    }
    let rels = project
        .model
        .relationships
        .iter()
        .map(|r| {
            let mut node = Node {
                kind: "relationship".into(),
                header: relationship_header(
                    &r.from_table,
                    &r.from_column,
                    &r.to_table,
                    &r.to_column,
                ),
                items: Vec::new(),
            };
            if !r.active {
                node.items.push(Item::Flag("inactive".into()));
            }
            Item::Node(node)
        })
        .collect();
    if let Some(bytes) = print(rels, &Key::Relationships, &RELATIONSHIPS_FILE) {
        parts.insert(RELATIONSHIPS.into(), bytes);
    }
    let model = vec![Item::Node(model_node(project))];
    if let Some(bytes) = print(model, &Key::Model, &MODEL_FILE) {
        parts.insert(MODEL.into(), bytes);
    }
    for (path, bytes) in &preserved.parts {
        parts.entry(path.clone()).or_insert_with(|| bytes.clone());
    }

    let manifest = Manifest {
        version: FORMAT_VERSION,
        parts: parts.iter().map(|(p, b)| (p.clone(), hash(b))).collect(),
        extra: preserved.manifest.clone(),
    };
    let mut files = parts;
    files.insert(MANIFEST.into(), manifest.print());
    files.insert(
        GITIGNORE.into(),
        preserved
            .gitignore
            .clone()
            .unwrap_or_else(|| GITIGNORE_TEXT.as_bytes().to_vec()),
    );
    Ok(files)
}

// ---- reading ----

fn part_err(path: &str, message: impl Into<String>) -> Error {
    Error::Part {
        path: path.into(),
        message: message.into(),
    }
}

/// [`decode_with`] the current [`MIGRATIONS`].
pub fn decode(files: &FileSet, root: Option<&Path>) -> Result<(Project, Preserved)> {
    decode_with(files, root, MIGRATIONS)
}

/// Reads a Project from its files: checks the Manifest (Format version, part hashes), parses
/// the parts, migrates older Format versions with `migrations`, then builds the Project and
/// what to preserve. `root` resolves relative source paths. Never panics on hostile input.
pub fn decode_with(
    files: &FileSet,
    root: Option<&Path>,
    migrations: &[Migration],
) -> Result<(Project, Preserved)> {
    let manifest = files
        .get(MANIFEST)
        .ok_or_else(|| Error::NotAProject("this location".into()))?;
    let manifest = Manifest::parse(manifest)?;
    if manifest.version > FORMAT_VERSION {
        return Err(Error::NewerFormat {
            found: manifest.version,
            supported: FORMAT_VERSION,
        });
    }
    let damaged: Vec<String> = manifest
        .parts
        .iter()
        .filter(|(p, h)| files.get(*p).is_none_or(|b| &hash(b) != *h))
        .map(|(p, _)| p.clone())
        .collect();
    if !damaged.is_empty() {
        return Err(Error::Damaged { parts: damaged });
    }
    let mut tree = Tree {
        version: manifest.version,
        extra: manifest.extra,
        ..Tree::default()
    };
    for path in manifest.parts.keys() {
        let bytes = files.get(path).cloned().unwrap_or_default();
        if path.ends_with(".odm") || path.ends_with(".odq") {
            let text = std::str::from_utf8(&bytes).map_err(|_| part_err(path, "not UTF-8"))?;
            let items = syntax::parse(text).map_err(|e| part_err(path, e.to_string()))?;
            tree.syntax.insert(path.clone(), items);
        } else {
            tree.other.insert(path.clone(), bytes);
        }
    }
    crate::manifest::migrate(&mut tree, migrations)?;
    decode_tree(tree, root, files.get(GITIGNORE).cloned())
}

/// Whether `path` is `dir` + one file name ending in `ext`.
fn is_in(path: &str, dir: &str, ext: &str) -> bool {
    path.strip_prefix(dir)
        .is_some_and(|f| f.ends_with(ext) && f.len() > ext.len() && !f.contains('/'))
}

/// The one node of `kind` among a part's top-level items.
fn only_node<'a>(path: &str, items: &'a [Item], kind: &str) -> Result<&'a Node> {
    let mut nodes = items.iter().filter_map(|i| match i {
        Item::Node(n) if n.kind == kind => Some(n),
        _ => None,
    });
    let node = nodes
        .next()
        .ok_or_else(|| part_err(path, format!("expected a `{kind}`")))?;
    if nodes.next().is_some() {
        return Err(part_err(path, format!("more than one `{kind}`")));
    }
    Ok(node)
}

fn node_name(path: &str, node: &Node) -> Result<String> {
    node.name().ok_or_else(|| {
        part_err(
            path,
            format!(
                "`{} {}`: expected one name, quoted '…' if it has spaces or symbols",
                node.kind, node.header
            ),
        )
    })
}

fn bool_prop(path: &str, node: &Node, key: &str, default: bool) -> Result<bool> {
    match node.prop(key).map(Value::as_str) {
        None => Ok(default),
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        Some(other) => Err(part_err(
            path,
            format!("`{key}: {other}`: expected true or false"),
        )),
    }
}

fn culture(path: &str, name: &str) -> Result<&'static Culture> {
    Culture::get(name).ok_or_else(|| part_err(path, format!("culture `{name}` is not supported")))
}

fn column_type(path: &str, ctx: &str, key: &str, v: &str) -> Result<ColumnType> {
    ColumnType::from_name(v).ok_or_else(|| {
        part_err(
            path,
            format!(
                "{ctx}: `{key}: {v}` must be text, decimal, fixedDecimal, whole, date, datetime \
                 or boolean"
            ),
        )
    })
}

fn decode_table(path: &str, node: &Node) -> Result<ModelTable> {
    let name = node_name(path, node)?;
    let mut columns: Vec<Column> = Vec::new();
    for c in node.children().filter(|c| c.kind == "column") {
        let cname = node_name(path, c)?;
        let ctx = format!("column `{cname}`");
        if columns.iter().any(|x| x.name == cname) {
            return Err(part_err(path, format!("{ctx} appears twice")));
        }
        let data_type = c
            .prop("dataType")
            .ok_or_else(|| part_err(path, format!("{ctx} has no `dataType`")))?;
        let data_type = column_type(path, &ctx, "dataType", data_type.as_str())?;
        let query_type = match c.prop("queryType") {
            Some(v) => column_type(path, &ctx, "queryType", v.as_str())?,
            None => data_type,
        };
        let summarize_by = match c.prop("summarizeBy") {
            Some(v) => SummarizeBy::from_name(v.as_str()).ok_or_else(|| {
                part_err(
                    path,
                    format!(
                        "{ctx}: `summarizeBy: {}` must be none, sum, average, min, max, count \
                         or distinctCount",
                        v.as_str()
                    ),
                )
            })?,
            None => SummarizeBy::default_for(data_type),
        };
        let text = |k: &str| {
            c.prop(k)
                .map(|v| v.as_str().to_string())
                .unwrap_or_default()
        };
        columns.push(Column {
            name: cname,
            data_type,
            query_type,
            hidden: c.has_flag("hidden"),
            description: text("description"),
            format_string: text("formatString"),
            summarize_by,
            sort_by: c.prop("sortByColumn").map(|v| v.as_str().to_string()),
        });
    }
    for c in &columns {
        if let Some(by) = &c.sort_by
            && !columns.iter().any(|x| &x.name == by)
        {
            return Err(part_err(
                path,
                format!(
                    "column `{}` sorts by `{by}`, which isn't in the table",
                    c.name
                ),
            ));
        }
    }
    Ok(ModelTable {
        name,
        hidden: node.has_flag("hidden"),
        description: node
            .prop("description")
            .map(|v| v.as_str().to_string())
            .unwrap_or_default(),
        columns,
    })
}

fn decode_query(
    path: &str,
    node: &Node,
    project_culture: &'static Culture,
    root: Option<&Path>,
) -> Result<(Query, bool)> {
    let name = node_name(path, node)?;
    let load = bool_prop(path, node, "load", true)?;
    let own = match node.prop("culture") {
        Some(v) => Some(culture(path, v.as_str())?),
        None => None,
    };
    let text = node
        .block("m")
        .ok_or_else(|| part_err(path, format!("query `{name}` has no `m:` block")))?;
    let mq = m::parse(text).map_err(|e| part_err(path, format!("m: {e}")))?;
    let mut source = mq.source;
    if let (File::Path(p), Some(root)) = (source_file_mut(&mut source), root)
        && p.is_relative()
    {
        let mut full = root.to_path_buf();
        full.extend(p.to_string_lossy().split('/').filter(|s| !s.is_empty()));
        *p = full;
    }
    let query = Query {
        name,
        source,
        steps: mq.steps,
        culture: own.or(mq.culture).unwrap_or(project_culture),
    };
    Ok((query, load))
}

/// Sorts `items` by their position among `refs`, then (unreferenced) by name.
fn order<T>(items: &mut [T], name: impl Fn(&T) -> &str, refs: &[String]) {
    items.sort_by_cached_key(|t| {
        let n = name(t);
        (
            refs.iter().position(|r| r == n).unwrap_or(usize::MAX),
            n.to_string(),
        )
    });
}

fn decode_tree(
    tree: Tree,
    root: Option<&Path>,
    gitignore: Option<Vec<u8>>,
) -> Result<(Project, Preserved)> {
    let mut pre = Preserved {
        manifest: tree.extra,
        gitignore,
        ..Preserved::default()
    };
    let mut project = Project::default();
    let mut refs: (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    if let Some(items) = tree.syntax.get(MODEL) {
        let node = only_node(MODEL, items, "model")?;
        if let Some(v) = node.prop("culture") {
            project.culture = culture(MODEL, v.as_str())?.name.into();
        }
        project.model.autodetect_relationships =
            bool_prop(MODEL, node, "autodetectRelationships", true)?;
        for r in node.children().filter(|n| n.kind == "ref") {
            let target = r
                .header
                .split_once(' ')
                .and_then(|(kind, rest)| Some((kind, read_name(rest)?)));
            match target {
                Some(("query", (name, ""))) => refs.0.push(name),
                Some(("table", (name, ""))) => refs.1.push(name),
                // Another kind of ref: unknown to us, kept.
                _ => {}
            }
        }
    }
    let project_culture = culture(MODEL, &project.culture)?;

    for (path, items) in tree.syntax {
        if path == MODEL {
            pre.trees.insert(Key::Model, items);
        } else if path == RELATIONSHIPS {
            for node in items.iter().filter_map(|i| match i {
                Item::Node(n) if n.kind == "relationship" => Some(n),
                _ => None,
            }) {
                let (ft, fc, tt, tc) = parse_relationship(&node.header).ok_or_else(|| {
                    part_err(
                        &path,
                        format!(
                            "`relationship {}`: expected Table[Column] -> Table[Column]",
                            node.header
                        ),
                    )
                })?;
                let mut r = Relationship::new(&ft, &fc, &tt, &tc);
                r.active = !node.has_flag("inactive");
                project.model.relationships.push(r);
            }
            pre.trees.insert(Key::Relationships, items);
        } else if is_in(&path, TABLES, ".odm") {
            let table = decode_table(&path, only_node(&path, &items, "table")?)?;
            if project.model.table(&table.name).is_some() {
                return Err(part_err(&path, format!("table `{}` twice", table.name)));
            }
            pre.trees.insert(Key::Table(table.name.clone()), items);
            project.model.tables.push(table);
        } else if is_in(&path, QUERIES, ".odq") {
            let node = only_node(&path, &items, "query")?;
            let (query, load) = decode_query(&path, node, project_culture, root)?;
            if project.queries.iter().any(|q| q.name == query.name) {
                return Err(part_err(&path, format!("query `{}` twice", query.name)));
            }
            if !load {
                project.unloaded.insert(query.name.clone());
            }
            pre.trees.insert(Key::Query(query.name.clone()), items);
            project.queries.push(query);
        } else {
            pre.parts.insert(path, syntax::print(&items).into_bytes());
        }
    }
    pre.parts.extend(tree.other);

    order(&mut project.queries, |q| &q.name, &refs.0);
    order(&mut project.model.tables, |t| &t.name, &refs.1);
    check(&project)?;
    Ok((project, pre))
}

/// Cross-part consistency: every Table has its Query; Relationships name real columns and at
/// most one per pair of tables is active.
fn check(project: &Project) -> Result {
    let model = &project.model;
    for t in &model.tables {
        if !project.queries.iter().any(|q| q.name == t.name) {
            return Err(part_err(
                TABLES,
                format!("table `{}` has no query of the same name", t.name),
            ));
        }
    }
    for (i, r) in model.relationships.iter().enumerate() {
        for (table, column) in [(&r.from_table, &r.from_column), (&r.to_table, &r.to_column)] {
            let found = model
                .table(table)
                .is_some_and(|t| t.columns.iter().any(|c| &c.name == column));
            if !found {
                return Err(part_err(
                    RELATIONSHIPS,
                    format!("`{table}`[`{column}`] is not a column of the model"),
                ));
            }
        }
        let twin = model
            .relationships
            .iter()
            .skip(i.saturating_add(1))
            .any(|o| o.active && r.active && o.same_pair(r));
        if twin {
            return Err(part_err(
                RELATIONSHIPS,
                format!(
                    "two active relationships between `{}` and `{}`",
                    r.from_table, r.to_table
                ),
            ));
        }
    }
    Ok(())
}

/// An empty loaded table with `table`'s columns and Model types: what a Table is on open until
/// it is refreshed, when it has no usable Data cache.
pub fn empty_table(table: &ModelTable) -> opendash_data::Result<Table> {
    let fields: Vec<Field> = table
        .columns
        .iter()
        .map(|c| Field::new(&c.name, c.data_type.data_type(), true))
        .collect();
    let schema = Arc::new(ArrowSchema::new(fields));
    let mut b = TableBuilder::new(&table.name, Arc::clone(&schema), usize::MAX);
    b.push(&RecordBatch::new_empty(schema))?;
    b.finish()
}

mod rename;

#[cfg(test)]
mod tests;
