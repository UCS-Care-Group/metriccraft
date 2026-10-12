//! Model commands (spec D-1): Enable load, and the Table and Column properties of Power BI's
//! Column tools ribbon and Properties pane. All are [`Run::Edit`](crate::Run::Edit): undoable,
//! and a change that fails changes nothing. Renames are M2.5's.
//!
//! Data-type override: see `opendash_model` (`table.rs`) for the rules and parity
//! uncertainties.

use opendash_data::culture::Culture;
use opendash_data::step::ColumnType;
use opendash_data::{Table, default_memory_cap};
use opendash_model::{ModelTable, SummarizeBy};
use serde_json::Value;

use crate::{App, Document, Error, Result};

pub(crate) fn has_tables(app: &App) -> bool {
    !app.document().tables.is_empty()
}

/// `query.enableLoad`: turns a Query's Enable load on or off (default: the opposite of now)
/// and marks it Stale; Close & Apply then loads it or removes its Table.
pub(crate) fn enable_load(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "query.enableLoad";
    let name = text(params, "query", ID)?;
    if !doc.queries.iter().any(|q| q.name == name) {
        return Err(invalid(ID, format!("there is no query `{name}`")));
    }
    let on = match params.get("enabled") {
        None => doc.unloaded.contains(&name),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(ID, "`enabled` must be true or false".into()))?,
    };
    let changed = if on {
        doc.unloaded.remove(&name)
    } else {
        doc.unloaded.insert(name.clone())
    };
    if changed {
        doc.stale.insert(name);
    }
    Ok(())
}

/// `model.setHidden`: hides or shows a Table, or one of its Columns.
pub(crate) fn set_hidden(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "model.setHidden";
    let hidden = match params.get("hidden") {
        None => true,
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(ID, "`hidden` must be true or false".into()))?,
    };
    let (table, column) = target(doc, params, ID)?;
    match column {
        None => table.hidden = hidden,
        Some(c) => table.column_mut(&c).map_err(Error::Model)?.hidden = hidden,
    }
    Ok(())
}

/// `model.setDescription`: a Table's or Column's description (empty clears it).
pub(crate) fn set_description(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "model.setDescription";
    let description = text(params, "description", ID)?;
    let (table, column) = target(doc, params, ID)?;
    match column {
        None => table.description = description,
        Some(c) => table.column_mut(&c).map_err(Error::Model)?.description = description,
    }
    Ok(())
}

/// `columnTools.format`: a Column's format string (empty: the default).
pub(crate) fn set_format(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "columnTools.format";
    let format = text(params, "formatString", ID)?;
    // ponytail: format strings are checked when M3.8 renders them; any text is kept until then.
    let (table, column) = column(doc, params, ID)?;
    table
        .column_mut(&column)
        .map_err(Error::Model)?
        .format_string = format;
    Ok(())
}

/// `columnTools.summarization`: how visuals aggregate a Column by default.
pub(crate) fn set_summarize_by(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "columnTools.summarization";
    let name = text(params, "summarizeBy", ID)?;
    let by = SummarizeBy::from_name(&name).ok_or_else(|| {
        invalid(
            ID,
            format!(
                "unknown `summarizeBy` `{name}`: none, sum, average, min, max, count or \
                 distinctCount"
            ),
        )
    })?;
    let (table, column) = column(doc, params, ID)?;
    let c = table.column_mut(&column).map_err(Error::Model)?;
    if !by.allowed_for(c.data_type) {
        return Err(invalid(
            ID,
            format!("`{column}` isn't a number: it can't be summarized by {name}"),
        ));
    }
    c.summarize_by = by;
    Ok(())
}

/// `columnTools.sortByColumn`: sorts a Column by another of its Table (null clears it).
pub(crate) fn set_sort_by(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "columnTools.sortByColumn";
    let by = match params.get("by") {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        _ => return Err(invalid(ID, "`by` must be a column name or null".into())),
    };
    let data = data(doc, params, ID)?;
    let (table, column) = column(doc, params, ID)?;
    table
        .set_sort_by(&data, &column, by.as_deref())
        .map_err(Error::Model)
}

/// `columnTools.dataType`: changes a Column's type in the Model, converting its loaded data.
/// The Query is untouched; the override is re-applied after every load.
pub(crate) fn set_data_type(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "columnTools.dataType";
    let name = text(params, "type", ID)?;
    let ty = ColumnType::from_name(&name).ok_or_else(|| {
        invalid(
            ID,
            format!(
                "unknown `type` `{name}`: decimal, fixedDecimal, whole, datetime, date, text or \
                 boolean"
            ),
        )
    })?;
    let data = data(doc, params, ID)?;
    let culture = doc
        .queries
        .iter()
        .find(|q| q.name == data.name())
        .map(|q| q.culture)
        .or_else(|| Culture::get("en-US"))
        .ok_or_else(|| invalid(ID, "no culture".into()))?;
    let loaded: usize = doc.tables.iter().map(Table::memory).sum();
    let limit = default_memory_cap().saturating_sub(loaded);
    let (table, column) = column(doc, params, ID)?;
    let converted = table
        .set_data_type(&data, &column, ty, culture, limit)
        .map_err(Error::Model)?;
    if let Some(t) = doc.tables.iter_mut().find(|t| t.name() == converted.name()) {
        *t = converted;
    }
    Ok(())
}

/// The model Table `params.table` and, if given, `params.column`'s name.
fn target<'a>(
    doc: &'a mut Document,
    params: &Value,
    id: &str,
) -> Result<(&'a mut ModelTable, Option<String>)> {
    let column = match params.get("column") {
        None => None,
        Some(_) => Some(text(params, "column", id)?),
    };
    let name = text(params, "table", id)?;
    let table = doc
        .model
        .table_mut(&name)
        .ok_or_else(|| invalid(id, format!("there is no table `{name}`")))?;
    Ok((table, column))
}

/// The model Table `params.table` and the Column `params.column` (required).
fn column<'a>(
    doc: &'a mut Document,
    params: &Value,
    id: &str,
) -> Result<(&'a mut ModelTable, String)> {
    let column = text(params, "column", id)?;
    let (table, _) = target(doc, params, id)?;
    table.column_mut(&column).map_err(Error::Model)?;
    Ok((table, column))
}

/// The loaded data of Table `params.table` (a clone: columns are shared).
fn data(doc: &Document, params: &Value, id: &str) -> Result<Table> {
    let name = text(params, "table", id)?;
    doc.tables
        .iter()
        .find(|t| t.name() == name)
        .cloned()
        .ok_or_else(|| invalid(id, format!("there is no table `{name}`")))
}

fn text(params: &Value, key: &str, id: &str) -> Result<String> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| invalid(id, format!("`{key}` must be text")))
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

    use arrow_array::{Array, Decimal32Array};
    use opendash_model::Column;
    use serde_json::json;

    use super::*;

    /// A fresh directory under the system temp dir, removed on drop.
    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Dir {
            static N: AtomicUsize = AtomicUsize::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("opendash-model-{}-{n}", std::process::id()));
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

    const SALES: &str = "Month,MonthNo,Price\nJan,1,0.1\nFeb,2,0.2\nJan,1,1.5\n";

    fn app(dir: &Dir) -> App {
        let mut app = App::new();
        for (name, text) in [
            ("Sales.csv", SALES),
            ("Targets.csv", "Month,Target\nJan,10\nFeb,20\n"),
        ] {
            let path = dir.write(name, text);
            app.run("home.getData", &json!({"kind": "csv", "path": path}))
                .unwrap();
        }
        app
    }

    fn column<'a>(app: &'a App, table: &str, column: &str) -> &'a Column {
        app.document()
            .model
            .table(table)
            .and_then(|t| t.column(column))
            .unwrap()
    }

    fn names(app: &App) -> Vec<&str> {
        app.document().tables.iter().map(Table::name).collect()
    }

    #[test]
    fn get_data_builds_the_model() {
        let dir = Dir::new();
        let app = app(&dir);
        let m = app.document().model.table("Sales").unwrap();
        let types: Vec<_> = m.columns.iter().map(|c| c.data_type.name()).collect();
        assert_eq!(types, ["text", "whole", "decimal"]);
        assert_eq!(
            column(&app, "Sales", "Price").summarize_by,
            SummarizeBy::Sum
        );
        assert_eq!(
            column(&app, "Sales", "Month").summarize_by,
            SummarizeBy::None
        );
    }

    #[test]
    fn an_unloaded_query_has_no_table_but_still_merges() {
        let dir = Dir::new();
        let mut app = app(&dir);
        app.run(
            "query.enableLoad",
            &json!({"query": "Targets", "enabled": false}),
        )
        .unwrap();
        assert_eq!(names(&app), ["Sales", "Targets"], "not until Close & Apply");
        assert!(app.document().stale.contains("Targets"));
        app.run("home.closeApply", &Value::Null).unwrap();
        assert_eq!(names(&app), ["Sales"]);
        assert!(app.document().model.table("Targets").is_none());
        assert!(app.document().stale.is_empty());
        // Sales merges the unloaded Targets: computed within the load.
        let merge = json!({"query": "Sales", "keys": ["Month"], "with": "Targets",
            "withKeys": ["Month"], "expand": ["Target"]});
        app.run("home.mergeQueries", &merge).unwrap();
        app.run("home.closeApply", &Value::Null).unwrap();
        let t = &app.document().tables[0];
        assert_eq!(t.batch().schema().field(3).name(), "Targets.Target");
        assert_eq!(t.batch().column(3).null_count(), 0);
        assert_eq!(names(&app), ["Sales"]);
        // Refresh skips it too.
        app.run("home.refresh", &Value::Null).unwrap();
        assert_eq!(names(&app), ["Sales"]);
        assert_eq!(app.last_load().unwrap().tables.len(), 1);
        // Toggled back on (no `enabled`: the opposite of now), it loads again; undo removes it.
        app.run("query.enableLoad", &json!({"query": "Targets"}))
            .unwrap();
        app.run("home.closeApply", &Value::Null).unwrap();
        let mut loaded = names(&app);
        loaded.sort_unstable();
        assert_eq!(loaded, ["Sales", "Targets"]);
        assert!(app.document().model.table("Targets").is_some());
        app.run("edit.undo", &Value::Null).unwrap();
        app.run("edit.undo", &Value::Null).unwrap();
        assert_eq!(names(&app), ["Sales"]);
        assert!(app.document().unloaded.contains("Targets"));
    }

    #[test]
    fn column_properties_are_set_undone_and_survive_a_refresh() {
        let dir = Dir::new();
        let mut app = app(&dir);
        let runs = [
            ("model.setHidden", json!({"table": "Sales"})),
            (
                "model.setHidden",
                json!({"table": "Sales", "column": "Price"}),
            ),
            (
                "model.setDescription",
                json!({"table": "Sales", "description": "Orders"}),
            ),
            (
                "model.setDescription",
                json!({"table": "Sales", "column": "Price",
                "description": "Unit price"}),
            ),
            (
                "columnTools.format",
                json!({"table": "Sales", "column": "Price",
                "formatString": "#,0.00"}),
            ),
            (
                "columnTools.summarization",
                json!({"table": "Sales", "column": "Price",
                "summarizeBy": "average"}),
            ),
            (
                "columnTools.sortByColumn",
                json!({"table": "Sales", "column": "Month",
                "by": "MonthNo"}),
            ),
        ];
        let start = app.document().clone();
        for (id, params) in &runs {
            let before = app.document().clone();
            app.run(id, params).unwrap();
            assert_ne!(app.document(), &before, "{id}");
        }
        let set = app.document().model.clone();
        let m = set.table("Sales").unwrap();
        assert!(m.hidden);
        assert_eq!(m.description, "Orders");
        let price = m.column("Price").unwrap();
        assert!(price.hidden);
        assert_eq!(
            (price.description.as_str(), price.format_string.as_str()),
            ("Unit price", "#,0.00")
        );
        assert_eq!(price.summarize_by, SummarizeBy::Average);
        assert_eq!(
            m.column("Month").unwrap().sort_by.as_deref(),
            Some("MonthNo")
        );
        app.run("home.refresh", &Value::Null).unwrap();
        assert_eq!(app.document().model, set, "kept across a load");
        for _ in 0..=runs.len() {
            app.run("edit.undo", &Value::Null).unwrap();
        }
        assert_eq!(app.document(), &start);
        // A column the Query drops loses its properties; back again, it has the defaults.
        for _ in 0..runs.len() {
            app.run("edit.redo", &Value::Null).unwrap();
        }
        app.run(
            "home.removeColumns",
            &json!({"query": "Sales", "columns": ["Price"]}),
        )
        .unwrap();
        app.run("home.closeApply", &Value::Null).unwrap();
        assert!(
            app.document()
                .model
                .table("Sales")
                .unwrap()
                .column("Price")
                .is_none()
        );
        app.run("query.deleteStep", &json!({"query": "Sales", "step": 2}))
            .unwrap();
        app.run("home.closeApply", &Value::Null).unwrap();
        assert_eq!(
            column(&app, "Sales", "Price"),
            &Column::new("Price", ColumnType::Decimal)
        );
        assert_eq!(
            column(&app, "Sales", "Month").sort_by.as_deref(),
            Some("MonthNo")
        );
    }

    #[test]
    fn bad_params_are_invalid_and_change_nothing() {
        let dir = Dir::new();
        let mut app = app(&dir);
        let before = app.document().clone();
        for (id, params) in [
            ("query.enableLoad", json!({"query": "Nope"})),
            (
                "query.enableLoad",
                json!({"query": "Sales", "enabled": "no"}),
            ),
            ("model.setHidden", json!({"table": "Nope"})),
            (
                "model.setHidden",
                json!({"table": "Sales", "column": "Nope"}),
            ),
            ("model.setHidden", json!({"table": "Sales", "hidden": 1})),
            ("model.setDescription", json!({"table": "Sales"})),
            (
                "columnTools.format",
                json!({"table": "Sales", "column": "Price"}),
            ),
            (
                "columnTools.summarization",
                json!({"table": "Sales", "column": "Price",
                "summarizeBy": "median"}),
            ),
            (
                "columnTools.summarization",
                json!({"table": "Sales", "column": "Month",
                "summarizeBy": "sum"}),
            ),
            (
                "columnTools.sortByColumn",
                json!({"table": "Sales", "column": "Month"}),
            ),
            (
                "columnTools.sortByColumn",
                json!({"table": "Sales", "column": "Month",
                "by": "Price"}),
            ),
            (
                "columnTools.sortByColumn",
                json!({"table": "Targets", "column": "Month",
                "by": "Price"}),
            ),
            (
                "columnTools.dataType",
                json!({"table": "Sales", "column": "Month",
                "type": "currency"}),
            ),
            (
                "columnTools.dataType",
                json!({"table": "Sales", "column": "Month",
                "type": "whole"}),
            ),
        ] {
            let r = app.run(id, &params);
            assert!(
                matches!(r, Err(Error::InvalidParams { .. } | Error::Model(_))),
                "{id} {params}: {r:?}"
            );
            assert_eq!(app.document(), &before, "{id} {params}");
        }
        let e = app
            .run(
                "columnTools.dataType",
                &json!({"table": "Sales", "column": "Month",
                "type": "whole"}),
            )
            .unwrap_err();
        assert_eq!(
            e.to_string(),
            "`Sales`[`Month`]: Cannot convert value 'Jan' of type Text to type Integer."
        );
        assert_eq!(
            column(&app, "Sales", "Month").data_type,
            ColumnType::Text,
            "kept"
        );
    }

    #[test]
    fn a_type_override_converts_the_data_and_outlives_loads() {
        let dir = Dir::new();
        let mut app = app(&dir);
        let steps = app.document().queries[0].steps.clone();
        let fixed = json!({"table": "Sales", "column": "Price", "type": "fixedDecimal"});
        app.run("columnTools.dataType", &fixed).unwrap();
        let price = |app: &App| {
            let c = app.document().tables[0].batch().column(2).clone();
            let d = c.as_any().downcast_ref::<Decimal32Array>().unwrap().clone();
            d.values().to_vec()
        };
        assert_eq!(price(&app), [1_000, 2_000, 15_000]);
        assert_eq!(
            app.document().queries[0].steps,
            steps,
            "the Query is untouched"
        );
        assert!(app.document().stale.is_empty(), "nothing to Close & Apply");
        let c = column(&app, "Sales", "Price");
        assert_eq!(
            (c.data_type, c.query_type),
            (ColumnType::FixedDecimal, ColumnType::Decimal)
        );
        // Refresh with new data: the override is applied again.
        dir.write("Sales.csv", "Month,MonthNo,Price\nJan,1,2.00005\n");
        app.run("home.refresh", &Value::Null).unwrap();
        assert_eq!(price(&app), [20_000]);
        // Data the override can't take fails the load, changing nothing.
        dir.write("Sales.csv", "Month,MonthNo,Price\nJan,1,1e300\n");
        let before = app.document().clone();
        let e = app.run("home.refresh", &Value::Null).unwrap_err();
        assert!(
            matches!(&e, Error::Model(m) if m.column == "Price"),
            "{e:?}"
        );
        assert_eq!(app.document(), &before);
        // Undo the refresh and the override: decimal data again.
        app.run("edit.undo", &Value::Null).unwrap();
        app.run("edit.undo", &Value::Null).unwrap();
        let c = app.document().tables[0].batch().column(2).clone();
        assert_eq!(c.data_type(), &arrow_schema::DataType::Float64);
    }

    #[test]
    fn a_load_that_breaks_a_sort_by_column_fails() {
        let dir = Dir::new();
        let mut app = app(&dir);
        app.run(
            "columnTools.sortByColumn",
            &json!({"table": "Sales", "column": "Month",
            "by": "MonthNo"}),
        )
        .unwrap();
        dir.write("Sales.csv", "Month,MonthNo,Price\nJan,1,1\nJan,2,1\n");
        let before = app.document().clone();
        let e = app.run("home.refresh", &Value::Null).unwrap_err();
        assert!(
            e.to_string().contains("Cannot sort 'Month' by 'MonthNo'"),
            "{e}"
        );
        assert_eq!(app.document(), &before);
        // Cleared with null, the refresh goes through.
        app.run(
            "columnTools.sortByColumn",
            &json!({"table": "Sales", "column": "Month",
            "by": null}),
        )
        .unwrap();
        app.run("home.refresh", &Value::Null).unwrap();
    }
}
