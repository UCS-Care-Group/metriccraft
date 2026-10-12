//! Get Data, Refresh and Preview (spec S-1, S-2, Q-2).
//!
//! - **Get Data** connects to a file: a Query with its automatic steps, loaded as a table.
//! - **Refresh** re-runs every Query from its Source in full. All-or-nothing: the new tables are
//!   built beside the old ones (≈ 2× memory briefly, ADR 0005) and replace them only if every
//!   Query succeeds; otherwise nothing changes and the error names the Query.
//! - **Preview** is derived (first 1,000 Source rows at a step) and not part of the Document.
//!
//! Both commands are undoable edits: undo puts back the previous tables with the previous steps,
//! so the Document never mixes a Query's steps with data from other steps. Editing a step (the
//! Applied Step commands) also re-reads the Query's Source, so a loaded table is always its
//! Query's result; there is no Stale table until Data caches exist (M2.4).

use std::path::Path;

use opendash_data::culture::Culture;
use opendash_data::source::{Encoding, ExcelKind, File, Source, csv};
use opendash_data::{Preview, Query, Table, default_memory_cap};
use serde_json::Value;

use crate::{App, Document, Error, Result};

/// What the last Get Data or Refresh loaded.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LoadReport {
    pub tables: Vec<Loaded>,
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
    app.edit_loading(|doc| {
        if doc.queries.iter().any(|q| q.name == name) || doc.tables.iter().any(|t| t.name() == name)
        {
            return Err(invalid(GET_DATA, format!("`{name}` already exists")));
        }
        let limit = default_memory_cap().saturating_sub(loaded(&doc.tables));
        let query = Query::connect(&name, source, culture, limit).map_err(Error::Data)?;
        let (table, cell_errors) = query.run(limit).map_err(Error::Data)?;
        let report = LoadReport {
            tables: vec![Loaded {
                table: name.clone(),
                rows: table.rows(),
                cell_errors,
            }],
        };
        doc.queries.push(query);
        doc.tables.push(table);
        Ok(report)
    })
}

pub(crate) fn refresh(app: &mut App, _params: &Value) -> Result {
    app.edit_loading(|doc| {
        let cap = default_memory_cap();
        let mut fresh: Vec<Table> = Vec::with_capacity(doc.queries.len());
        let mut report = LoadReport::default();
        for query in merged_first(&doc.queries) {
            // The new tables together must fit the cap; the old ones are held beside them.
            let limit = cap.saturating_sub(loaded(&fresh));
            let (table, cell_errors) =
                query
                    .run_with(&doc.queries, limit)
                    .map_err(|error| Error::Refresh {
                        query: query.name.clone(),
                        error,
                    })?;
            report.tables.push(Loaded {
                table: table.name().into(),
                rows: table.rows(),
                cell_errors,
            });
            fresh.push(table);
        }
        for table in fresh {
            match doc.tables.iter_mut().find(|t| t.name() == table.name()) {
                Some(t) => *t = table,
                None => doc.tables.push(table),
            }
        }
        Ok(report)
    })
}

/// The Preview of Query `query` at Applied Step `step` (0 = first), or of its Source (`None`).
pub(crate) fn preview(doc: &Document, query: &str, step: Option<usize>) -> Result<Preview> {
    let q = doc
        .queries
        .iter()
        .find(|q| q.name == query)
        .ok_or_else(|| Error::UnknownQuery(query.into()))?;
    let limit = default_memory_cap().saturating_sub(loaded(&doc.tables));
    q.preview_with(&doc.queries, step, limit)
        .map_err(Error::Data)
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
    fn step_edits_re_read_the_source() {
        let dir = Dir::new();
        let mut app = two_queries(&dir);
        dir.write("Sales.csv", "Region,Qty\nNorth,3\n");
        app.run(
            "transform.renameColumn",
            &json!({"query": "Sales", "column": "Qty", "name": "Quantity"}),
        )
        .unwrap();
        assert_eq!(
            app.document().tables[0].rows(),
            1,
            "the edit re-read the file"
        );
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(
            app.document().tables[0].rows(),
            2,
            "undo restores the data too"
        );
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
