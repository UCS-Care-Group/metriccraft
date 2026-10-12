//! Get Data, Close & Apply, Refresh and Preview (spec S-1, S-2, Q-2): Power Query's edit model.
//!
//! - **Get Data** connects to a file: a Query with its automatic steps, loaded as a table at
//!   once (Power BI's "Load").
//! - **Editing** a Query (every Applied Step command, delete, filter, merge) changes only the
//!   Query: its steps are checked on the Preview rows, the Source is not re-read, and its table
//!   stays as it was, marked **Stale** (`Document::stale`).
//! - **Preview** is how edits are seen: derived (first 1,000 Source rows at a step), not part of
//!   the Document.
//! - **Close & Apply** loads every Stale Query, and every Query that merges one (transitively).
//! - **Refresh** re-runs every Query from its Source in full and clears every Stale flag.
//! - **Enable load** off (`Document::unloaded`): Close & Apply and Refresh don't load the
//!   Query and remove its Table; a Query that merges it still runs it, within the load.
//!   Toggling it marks the Query Stale, as editing does: the Table goes (or comes) at the next
//!   Close & Apply, as in Power BI.
//! - Every loaded Table's model is updated after the load (`opendash_model::ModelTable::load`):
//!   column properties carried over by name, type overrides re-applied, sort by checked. A
//!   failure there fails the whole load.
//! - Then the Relationships (`opendash_model`, `relationship.rs`): those whose Table or Column is
//!   gone are dropped; a one side that is no longer unique fails the whole load, naming the
//!   Table and Column; with the Project's switch on, auto-detect adds Relationships for Tables
//!   that weren't loaded before. Both are listed in the [`LoadReport`].
//!
//! Loading is all-or-nothing: the new tables are built beside the old ones (≈ 2× memory
//! briefly, ADR 0005) and replace them only if every Query succeeds; otherwise nothing changes
//! and the error names the Query. Queries a Query merges load first, and a load reads each
//! Source once, sharing a merged-from Query's result (`Pass`). Every command here is an
//! undoable edit (undoing a refresh is something Power BI can't do): undo puts back the previous
//! tables, steps and Stale flags together.
//!
//! **Memory cap** (spec N-4, [`App::memory_cap`]): a load may use the cap less every distinct
//! column buffer held by the document, undo and redo ([`App::memory`]; snapshots share
//! buffers, so each counts once), the tables being replaced included. When the new data won't
//! fit, the oldest undo steps are dropped while they hold data the document and redo don't
//! (counted in [`LoadReport::undo_dropped`]); if it still doesn't fit with none left, the load
//! fails with the memory-cap error naming the table.

use std::path::Path;

use opendash_data::culture::Culture;
use opendash_data::source::{Encoding, ExcelKind, File, Source, csv};
use opendash_data::{Pass, Preview, Query, Table};
use opendash_model::{ModelTable, Relationship};
use serde_json::Value;

use crate::{App, Document, Error, Result};

/// What the last Get Data or Refresh loaded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadReport {
    pub tables: Vec<Loaded>,
    /// Undo steps dropped (oldest first) to fit the new data under the memory cap.
    pub undo_dropped: usize,
    /// Relationships auto-detect added for the new Tables.
    pub relationships_added: Vec<Relationship>,
    /// Relationships dropped because a Table or Column they use is gone.
    pub relationships_removed: Vec<Relationship>,
}

/// One loaded table: its rows and its Cell errors (loaded as blank).
#[derive(Clone, Debug, PartialEq)]
pub struct Loaded {
    pub table: String,
    pub rows: usize,
    pub cell_errors: usize,
}

const GET_DATA: &str = "home.getData";

pub(crate) fn get_data(app: &mut App, params: &Value) -> Result {
    let text = |key: &str| params.get(key).and_then(Value::as_str);
    let required = |key: &str| {
        text(key)
            .filter(|v| !v.is_empty())
            .ok_or_else(|| invalid(GET_DATA, format!("`{key}` must be text")))
    };
    let kind = required("kind")?;
    let path = required("path")?;
    let culture = match text("culture") {
        // ponytail: the Project culture (from the OS) once Projects exist (M2).
        None => Culture::get("en-US"),
        Some(c) => Culture::get(c),
    }
    .ok_or_else(|| invalid(GET_DATA, "unknown `culture`".into()))?;
    let file = File::Path(path.into());
    let stem = Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned());
    let (source, default_name) = match kind {
        "csv" => {
            let options = csv::Options {
                delimiter: text("delimiter").map(delimiter).transpose()?,
                encoding: text("encoding").map(encoding).transpose()?,
            };
            let name = text("name").or(stem.as_deref()).unwrap_or("Query1");
            (Source::csv(name, file, options).map_err(Error::Data)?, stem)
        }
        "parquet" => (Source::Parquet { file }, stem),
        "xlsx" => {
            let item = required("item")?;
            let kind = match text("itemKind") {
                None | Some("sheet") => ExcelKind::Sheet,
                Some("table") => ExcelKind::Table,
                Some(other) => {
                    return Err(invalid(GET_DATA, format!("unknown `itemKind` `{other}`")));
                }
            };
            let source = Source::Excel {
                file,
                item: item.into(),
                kind,
            };
            (source, Some(item.to_string()))
        }
        other => return Err(invalid(GET_DATA, format!("unknown `kind` `{other}`"))),
    };
    let name = match text("name") {
        Some(n) => n.to_string(),
        None => default_name.unwrap_or_else(|| "Query1".into()),
    };
    if name.is_empty() {
        return Err(invalid(GET_DATA, "`name` must not be empty".into()));
    }
    app.edit_loading(0, |doc, limit| {
        if doc.queries.iter().any(|q| q.name == name) || doc.tables.iter().any(|t| t.name() == name)
        {
            return Err(invalid(GET_DATA, format!("`{name}` already exists")));
        }
        let query = Query::connect(&name, source.clone(), culture, limit).map_err(Error::Data)?;
        let (table, cell_errors) = query.run(limit).map_err(Error::Data)?;
        let limit = limit.saturating_sub(table.memory());
        let (model, table) =
            ModelTable::load(None, table, query.culture, limit).map_err(Error::Model)?;
        let mut report = LoadReport {
            tables: vec![Loaded {
                table: name.clone(),
                rows: table.rows(),
                cell_errors,
            }],
            ..LoadReport::default()
        };
        doc.queries.push(query);
        doc.tables.push(table);
        doc.model.put(model);
        let one = std::slice::from_ref(&name);
        relate(doc, one, one, &mut report)?;
        Ok(report)
    })
}

pub(crate) fn refresh(app: &mut App, _params: &Value) -> Result {
    let estimate = loaded(&app.document().tables);
    app.edit_loading(estimate, |doc, limit| load(doc, |_| true, limit))
}

/// Whether Close & Apply has anything to load.
pub(crate) fn has_stale(app: &App) -> bool {
    !app.document().stale.is_empty()
}

/// Close & Apply: loads every Stale Query, and every Query that merges one (transitively).
pub(crate) fn close_apply(app: &mut App, _params: &Value) -> Result {
    let doc = app.document();
    let mut names: Vec<String> = doc.stale.iter().cloned().collect();
    // Each pass adds at least one Query or stops, so this ends within `queries.len()` passes.
    loop {
        let more: Vec<String> = doc
            .queries
            .iter()
            .filter(|q| !names.contains(&q.name) && names.iter().any(|n| q.merges(n)))
            .map(|q| q.name.clone())
            .collect();
        if more.is_empty() {
            break;
        }
        names.extend(more);
    }
    let estimate = doc
        .tables
        .iter()
        .filter(|t| names.iter().any(|n| n == t.name()))
        .map(Table::memory)
        .sum();
    app.edit_loading(estimate, |doc, limit| {
        load(doc, |q| names.contains(&q.name), limit)
    })
}

/// Re-runs the Queries `pick` selects, from their Sources, and replaces their tables and
/// models; all-or-nothing (the caller runs on a copy of the Document). Clears their Stale
/// flags. A picked Query with Enable load off loses its Table instead.
fn load(doc: &mut Document, pick: impl Fn(&Query) -> bool, limit: usize) -> Result<LoadReport> {
    // The new tables (and the merged-from Queries run for them) must fit `limit` together: the
    // cap less everything held, the old tables included. One pass, so each Source is read once.
    let mut pass = Pass::new(&doc.queries, limit);
    let mut fresh: Vec<Table> = Vec::with_capacity(doc.queries.len());
    let mut report = LoadReport::default();
    let mut models = Vec::with_capacity(doc.queries.len());
    let mut used = 0usize;
    let mut dropped = Vec::new();
    let before: Vec<String> = doc.tables.iter().map(|t| t.name().to_string()).collect();
    for query in merged_first(&doc.queries).into_iter().filter(|q| pick(q)) {
        if doc.unloaded.contains(&query.name) {
            dropped.push(query.name.clone());
            continue;
        }
        let (table, cell_errors) = pass.run(query).map_err(|error| Error::Refresh {
            query: query.name.clone(),
            error,
        })?;
        // Type overrides convert columns beside the pass's results.
        used = used.saturating_add(table.memory());
        let previous = doc.model.table(&query.name);
        let (model, table) =
            ModelTable::load(previous, table, query.culture, limit.saturating_sub(used))
                .map_err(Error::Model)?;
        models.push(model);
        report.tables.push(Loaded {
            table: table.name().into(),
            rows: table.rows(),
            cell_errors,
        });
        fresh.push(table);
    }
    for name in dropped {
        doc.stale.remove(&name);
        doc.tables.retain(|t| t.name() != name);
        doc.model.remove(&name);
    }
    for model in models {
        doc.model.put(model);
    }
    let loaded: Vec<String> = fresh.iter().map(|t| t.name().to_string()).collect();
    for table in fresh {
        doc.stale.remove(table.name());
        match doc.tables.iter_mut().find(|t| t.name() == table.name()) {
            Some(t) => *t = table,
            None => doc.tables.push(table),
        }
    }
    let new: Vec<String> = loaded
        .iter()
        .filter(|n| !before.contains(n))
        .cloned()
        .collect();
    relate(doc, &loaded, &new, &mut report)?;
    Ok(report)
}

/// The Relationships after a load of Tables `loaded` (`new`: those that weren't loaded
/// before): drops the ones whose Table or Column is gone, fails if a one side is no longer
/// unique, then auto-detects for the new Tables when the Project's switch is on.
fn relate(
    doc: &mut Document,
    loaded: &[String],
    new: &[String],
    report: &mut LoadReport,
) -> Result {
    report.relationships_removed = doc
        .model
        .relationships_after_load(&doc.tables, loaded)
        .map_err(Error::Model)?;
    if doc.model.autodetect_relationships && !new.is_empty() {
        report.relationships_added = doc
            .model
            .autodetect(&doc.tables, |n| new.iter().any(|x| x == n));
    }
    Ok(())
}

/// The Preview of Query `query` at Applied Step `step` (0 = first), or of its Source (`None`).
pub(crate) fn preview(
    doc: &Document,
    query: &str,
    step: Option<usize>,
    cap: usize,
) -> Result<Preview> {
    let q = doc
        .queries
        .iter()
        .find(|q| q.name == query)
        .ok_or_else(|| Error::UnknownQuery(query.into()))?;
    let limit = cap.saturating_sub(loaded(&doc.tables));
    q.preview_with(&doc.queries, &fresh(doc), step, limit)
        .map_err(Error::Data)
}

/// The loaded Tables that aren't Stale: what a Preview through a merge may use.
pub(crate) fn fresh(doc: &Document) -> Vec<&Table> {
    doc.tables
        .iter()
        .filter(|t| !doc.stale.contains(t.name()))
        .collect()
}

/// `queries` with each one after the Queries it merges, so a failure names the Query whose own
/// steps or Source failed. Queries in a cycle (or merging a missing one) come last, in order;
/// running them reports the cycle.
fn merged_first(queries: &[Query]) -> Vec<&Query> {
    let mut order: Vec<&Query> = Vec::with_capacity(queries.len());
    loop {
        let ready: Vec<&Query> = queries
            .iter()
            .filter(|q| !order.iter().any(|o| o.name == q.name))
            .filter(|q| {
                queries.iter().all(|other| {
                    !q.merges(&other.name) || order.iter().any(|o| o.name == other.name)
                })
            })
            .collect();
        if ready.is_empty() {
            break;
        }
        order.extend(ready);
    }
    let rest: Vec<&Query> = queries
        .iter()
        .filter(|q| !order.iter().any(|o| o.name == q.name))
        .collect();
    order.extend(rest);
    order
}

fn loaded(tables: &[Table]) -> usize {
    tables.iter().map(Table::memory).sum()
}

/// One ASCII character (`\t` for tab).
fn delimiter(s: &str) -> Result<u8> {
    match s.as_bytes() {
        [b] if b.is_ascii() => Ok(*b),
        b"\\t" => Ok(b'\t'),
        _ => Err(invalid(
            GET_DATA,
            "`delimiter` must be one character".into(),
        )),
    }
}

/// An encoding label M can name as a codepage: `utf-8`, `windows-1252`, `utf-16le`, `utf-16be`.
fn encoding(s: &str) -> Result<&'static Encoding> {
    Encoding::for_label(s.as_bytes())
        .filter(|e| opendash_data::m::codepage(e).is_some())
        .ok_or_else(|| invalid(GET_DATA, format!("unsupported `encoding` `{s}`")))
}

fn invalid(command: &str, message: String) -> Error {
    Error::InvalidParams {
        command: command.into(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use opendash_data::step::{ColumnType, Step};
    use serde_json::json;

    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Dir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("opendash-load-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Dir(dir)
        }

        fn write(&self, name: &str, text: &str) -> String {
            let path = self.0.join(name);
            std::fs::write(&path, text).unwrap();
            path.display().to_string()
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `Region,Qty` with 200 good rows (the detection sample), then a Qty of `x`.
    fn with_error() -> String {
        let mut csv = String::from("Region,Qty\n");
        for i in 0..200 {
            csv.push_str(&format!("North,{i}\n"));
        }
        csv + "South,x\n"
    }

    /// An app with Queries `Sales` and `Stock` loaded from CSV files in `dir`.
    fn two_queries(dir: &Dir) -> App {
        let sales = dir.write("Sales.csv", "Region,Qty\nNorth,3\nSouth,4\n");
        let stock = dir.write("Stock.csv", "Item,Units\nPen,10\n");
        let mut app = App::new();
        app.run(GET_DATA, &json!({"kind": "csv", "path": sales}))
            .unwrap();
        app.run(GET_DATA, &json!({"kind": "csv", "path": stock}))
            .unwrap();
        app
    }

    #[test]
    fn get_data_creates_a_query_with_the_automatic_steps_and_loads_it() {
        let dir = Dir::new();
        let path = dir.write("Sales.csv", &with_error());
        let mut app = App::new();
        app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
            .unwrap();
        let doc = app.document();
        assert_eq!(doc.queries[0].name, "Sales");
        assert_eq!(
            doc.queries[0].steps,
            [
                Step::PromotedHeaders,
                Step::ChangedType(vec![
                    ("Region".into(), ColumnType::Text),
                    ("Qty".into(), ColumnType::Whole)
                ])
            ]
        );
        assert_eq!((doc.tables[0].name(), doc.tables[0].rows()), ("Sales", 201));
        assert_eq!(
            app.last_load().unwrap().tables,
            [Loaded {
                table: "Sales".into(),
                rows: 201,
                cell_errors: 1
            }]
        );
        // Undo removes it; the same name again is refused.
        app.run("edit.undo", &Value::Null).unwrap();
        assert!(app.document().queries.is_empty());
        app.run("edit.redo", &Value::Null).unwrap();
        let again = app.run(GET_DATA, &json!({"kind": "csv", "path": path}));
        assert!(
            matches!(again, Err(Error::InvalidParams { .. })),
            "{again:?}"
        );
    }

    #[test]
    fn get_data_without_params_is_an_error_not_a_dialog() {
        for params in [
            Value::Null,
            json!({}),
            json!({"kind": "csv"}),
            json!({"kind": "dbf", "path": "x"}),
            json!({"kind": "xlsx", "path": "x.xlsx"}),
            json!({"kind": "csv", "path": "x", "delimiter": "ab"}),
            json!({"kind": "csv", "path": "x", "encoding": "shift_jis"}),
        ] {
            let mut app = App::new();
            let r = app.run(GET_DATA, &params);
            assert!(
                matches!(r, Err(Error::InvalidParams { .. })),
                "{params}: {r:?}"
            );
            assert!(!app.can_undo());
        }
        let mut app = App::new();
        let r = app.run(
            GET_DATA,
            &json!({"kind": "parquet", "path": "/nonexistent/x.parquet"}),
        );
        assert!(matches!(r, Err(Error::Data(_))), "{r:?}");
    }

    #[test]
    fn refresh_replaces_the_data_and_reports_cell_errors() {
        let dir = Dir::new();
        let mut app = two_queries(&dir);
        let before = app.document().clone();
        dir.write(
            "Sales.csv",
            "Region,Qty\nNorth,3\nSouth,4\nEast,oops\nWest,5\n",
        );
        app.run("home.refresh", &Value::Null).unwrap();
        let doc = app.document();
        assert_eq!(doc.tables[0].rows(), 4);
        assert_eq!(
            doc.tables[1], before.tables[1],
            "unchanged data, same table"
        );
        assert_eq!(
            app.last_load().unwrap().tables,
            [
                Loaded {
                    table: "Sales".into(),
                    rows: 4,
                    cell_errors: 1
                },
                Loaded {
                    table: "Stock".into(),
                    rows: 1,
                    cell_errors: 0
                }
            ]
        );
        // Undo puts the previous data back; redo the refreshed data.
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.document(), &before);
        app.run("edit.redo", &Value::Null).unwrap();
        assert_eq!(app.document().tables[0].rows(), 4);
    }

    #[test]
    fn a_failing_query_leaves_all_previous_data_intact_and_names_the_query() {
        let dir = Dir::new();
        // A missing file, data the steps can't run on (a column gone), an unreadable file.
        for (file, text) in [
            ("Stock.csv", None),
            ("Stock.csv", Some("Name,Units\nPen,10\n")),
            ("Stock.csv", Some("")),
        ] {
            let mut app = two_queries(&dir);
            dir.write("Sales.csv", "Region,Qty\nNorth,30\n");
            let path = dir.0.join(file);
            match text {
                None => std::fs::remove_file(&path).unwrap(),
                Some(t) => std::fs::write(&path, t).unwrap(),
            }
            let before = app.document().clone();
            let report = app.last_load().cloned();
            let e = app.run("home.refresh", &Value::Null).unwrap_err();
            assert!(
                matches!(&e, Error::Refresh { query, .. } if query == "Stock"),
                "{e:?}"
            );
            assert!(e.to_string().contains("`Stock`"), "{e}");
            assert_eq!(app.document(), &before, "{text:?}: every table as before");
            assert_eq!(app.last_load().cloned(), report);
            dir.write("Sales.csv", "Region,Qty\nNorth,3\nSouth,4\n");
            dir.write("Stock.csv", "Item,Units\nPen,10\n");
        }
    }

    #[test]
    fn refresh_and_preview_resolve_merges() {
        let dir = Dir::new();
        let sales = dir.write("Sales.csv", "Region,Qty\nNorth,3\nSouth,4\n");
        let targets = dir.write("Targets.csv", "Region,Target\nNorth,10\n");
        let mut app = App::new();
        for path in [&sales, &targets] {
            app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
                .unwrap();
        }
        let merge = json!({"query": "Sales", "keys": ["Region"], "with": "Targets",
            "withKeys": ["Region"], "expand": ["Target"]});
        app.run("home.mergeQueries", &merge).unwrap();
        let p = app.preview("Sales", Some(2)).unwrap();
        assert_eq!(p.batch.schema().field(2).name(), "Targets.Target");
        dir.write("Targets.csv", "Region,Target\nNorth,10\nSouth,20\n");
        app.run("home.refresh", &Value::Null).unwrap();
        let t = &app.document().tables[0];
        assert_eq!((t.rows(), t.batch().column(2).null_count()), (2, 0));
        // Targets is refreshed before Sales, so its failure names it, not Sales.
        std::fs::remove_file(&targets).unwrap();
        let before = app.document().clone();
        let e = app.run("home.refresh", &Value::Null).unwrap_err();
        assert!(
            matches!(&e, Error::Refresh { query, .. } if query == "Targets"),
            "{e:?}"
        );
        assert_eq!(app.document(), &before);
    }

    #[test]
    fn a_merge_previews_against_the_loaded_table_unless_stale() {
        let dir = Dir::new();
        let sales = dir.write("Sales.csv", "Region,Qty\nNorth,3\n");
        let targets = dir.write("Targets.csv", "Region,Target\nNorth,10\n");
        let mut app = App::new();
        for path in [&sales, &targets] {
            app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
                .unwrap();
        }
        let merge = json!({"query": "Sales", "keys": ["Region"], "with": "Targets",
            "withKeys": ["Region"], "expand": ["Target"]});
        app.run("home.mergeQueries", &merge).unwrap();
        // Sales' Targets.Target column, printed.
        let target = |app: &App| {
            let p = app.preview("Sales", Some(2)).unwrap();
            format!("{:?}", p.batch.column(2))
        };
        // The file changes, Targets isn't reloaded: its loaded table is used.
        dir.write("Targets.csv", "Region,Target\nNorth,20\n");
        assert!(target(&app).contains("10"), "{}", target(&app));
        // Targets edited (Stale): its own Preview, which reads the file.
        app.run(
            "transform.filterRows",
            &json!({"query": "Targets", "conditions": [
                {"column": "Target", "op": "greater", "value": 0}]}),
        )
        .unwrap();
        assert!(app.document().stale.contains("Targets"));
        assert!(target(&app).contains("20"), "{}", target(&app));
    }

    #[test]
    fn edits_mark_stale_and_close_apply_loads_only_stale_queries() {
        let dir = Dir::new();
        let mut app = two_queries(&dir);
        assert_eq!(
            app.run("home.closeApply", &Value::Null),
            Err(Error::Disabled("home.closeApply".into())),
            "nothing Stale"
        );
        // Both files change; only Sales is edited.
        dir.write("Sales.csv", "Region,Qty\nNorth,3\n");
        dir.write("Stock.csv", "Item,Units\nPen,10\nInk,2\n");
        let loaded = app.document().clone();
        app.run(
            "transform.renameColumn",
            &json!({"query": "Sales", "column": "Qty", "name": "Quantity"}),
        )
        .unwrap();
        let doc = app.document();
        assert_eq!(doc.tables, loaded.tables, "the edit doesn't load");
        assert_eq!(doc.stale.iter().collect::<Vec<_>>(), ["Sales"]);
        let p = app.preview("Sales", Some(2)).unwrap();
        assert_eq!(
            p.batch.schema().field(1).name(),
            "Quantity",
            "seen in Preview"
        );
        let edited = app.document().clone();
        app.run("home.closeApply", &Value::Null).unwrap();
        let doc = app.document();
        assert_eq!(doc.tables[0].rows(), 1, "Sales re-read");
        assert_eq!(doc.tables[1], loaded.tables[1], "Stock not loaded");
        assert!(doc.stale.is_empty());
        assert_eq!(app.last_load().unwrap().tables.len(), 1);
        // Undo restores the old data and the Stale flag.
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.document(), &edited);
        // Refresh loads everything and clears Stale; undo restores both.
        app.run("home.refresh", &Value::Null).unwrap();
        let doc = app.document();
        assert_eq!((doc.tables[0].rows(), doc.tables[1].rows()), (1, 2));
        assert!(doc.stale.is_empty());
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.document(), &edited);
    }

    #[test]
    fn a_failing_close_apply_changes_nothing_and_names_the_query() {
        let dir = Dir::new();
        let mut app = two_queries(&dir);
        app.run(
            "transform.renameColumn",
            &json!({"query": "Stock", "column": "Units", "name": "Count"}),
        )
        .unwrap();
        // The file loses the column after the edit was checked.
        dir.write("Stock.csv", "Item,Qty\nPen,10\n");
        let before = app.document().clone();
        let e = app.run("home.closeApply", &Value::Null).unwrap_err();
        assert!(
            matches!(&e, Error::Refresh { query, .. } if query == "Stock"),
            "{e:?}"
        );
        assert_eq!(app.document(), &before);
    }

    #[test]
    fn a_date_filter_added_offline_still_matches_dates() {
        let dir = Dir::new();
        let text = "Day,Qty\n2024-01-31,1\n2024-02-01,2\n";
        let path = dir.write("Sales.csv", text);
        for op in ["equals", "greater"] {
            let mut app = App::new();
            app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
                .unwrap();
            app.run(
                "transform.changeType",
                &json!({"query": "Sales", "columns": ["Day"], "type": "date"}),
            )
            .unwrap();
            app.run(
                "transform.renameColumn",
                &json!({"query": "Sales", "column": "Day", "name": "Date"}),
            )
            .unwrap();
            // The Source is offline while the filter is added (editing works offline).
            std::fs::remove_file(&path).unwrap();
            app.run(
                "transform.filterRows",
                &json!({"query": "Sales", "conditions": [
                    {"column": "Date", "op": op, "value": "2024-01-31"}]}),
            )
            .unwrap();
            let bad = json!({"query": "Sales", "conditions": [
                {"column": "Date", "op": op, "value": "soon"}]});
            assert!(
                matches!(
                    app.run("transform.filterRows", &bad),
                    Err(Error::InvalidParams { .. })
                ),
                "{op}"
            );
            dir.write("Sales.csv", text);
            app.run("home.closeApply", &Value::Null).unwrap();
            assert_eq!(app.document().tables[0].rows(), 1, "{op}");
        }
    }

    #[test]
    fn undo_and_redo_clear_the_load_report() {
        let dir = Dir::new();
        let mut app = two_queries(&dir);
        assert!(app.last_load().is_some());
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(app.last_load(), None, "the undone load isn't reported");
        app.run("edit.redo", &Value::Null).unwrap();
        assert_eq!(app.last_load(), None);
    }

    /// The first column's value buffer: which load's data the document holds.
    fn data(app: &App) -> usize {
        app.document().tables[0].buffers()[0].0
    }

    #[test]
    fn undo_keeps_every_load_while_the_cap_allows() {
        let dir = Dir::new();
        let path = dir.write("Sales.csv", "Region,Qty\nNorth,3\nSouth,4\n");
        let mut app = App::new();
        app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
            .unwrap();
        let p1 = data(&app);
        app.run("home.refresh", &Value::Null).unwrap();
        let p2 = data(&app);
        app.run("home.refresh", &Value::Null).unwrap();
        assert_eq!(app.last_load().unwrap().undo_dropped, 0);
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(data(&app), p2);
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(data(&app), p1);
    }

    #[test]
    fn undo_held_data_counts_against_the_memory_cap() {
        let dir = Dir::new();
        let mut csv = String::from("Amount\n");
        for i in 0..50_000 {
            csv.push_str(&format!("{i}.5\n"));
        }
        let path = dir.write("Sales.csv", &csv);
        let mut app = App::new();
        app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
            .unwrap();
        let one = app.memory();
        assert!(one >= 50_000 * 8, "{one}");
        // Room for three and a half copies of the table.
        let cap = one * 7 / 2;
        app.set_memory_cap(cap);
        let mut dropped = Vec::new();
        for _ in 0..5 {
            app.run("home.refresh", &Value::Null).unwrap();
            assert!(app.memory() <= cap, "{} held, cap {cap}", app.memory());
            dropped.push(app.last_load().unwrap().undo_dropped);
        }
        // Two refreshes fit beside the first load (3 copies); then each refresh drops the
        // oldest data (the first also drops the empty document before Get Data).
        assert_eq!(dropped, [0, 0, 2, 1, 1]);
        let mut undos = 0;
        while app.can_undo() {
            app.run("edit.undo", &Value::Null).unwrap();
            undos += 1;
        }
        assert_eq!((undos, app.document().tables.len()), (2, 1));

        // No undo data to drop and no room beside the old table: the load fails, naming it.
        let mut app = App::new();
        app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
            .unwrap();
        app.set_memory_cap(one * 3 / 2);
        let before = app.document().clone();
        let e = app.run("home.refresh", &Value::Null).unwrap_err();
        assert!(
            matches!(
                &e,
                Error::Refresh { query, error: opendash_data::Error::MemoryCap { table, .. } }
                    if query == "Sales" && table == "Sales"
            ),
            "{e:?}"
        );
        assert_eq!(app.document(), &before);
    }

    #[test]
    fn preview_is_derived_and_shows_cell_errors() {
        let dir = Dir::new();
        let path = dir.write("Sales.csv", &with_error());
        let mut app = App::new();
        app.run(GET_DATA, &json!({"kind": "csv", "path": path}))
            .unwrap();
        let before = app.document().clone();
        let p = app.preview("Sales", Some(1)).unwrap();
        assert_eq!(p.batch.num_rows(), 201);
        assert!(p.error(200, 1).is_some_and(|m| m.contains("Details: x")));
        assert_eq!(app.preview("Sales", None).unwrap().batch.num_rows(), 202);
        assert_eq!(app.document(), &before);
        assert!(!app.can_redo());
        assert_eq!(
            app.preview("Nope", None),
            Err(Error::UnknownQuery("Nope".into()))
        );
        assert!(matches!(app.preview("Sales", Some(9)), Err(Error::Data(_))));
    }
}
