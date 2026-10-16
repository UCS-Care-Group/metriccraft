//! Measures and Calculated columns (`metriccraft_model`'s `expression.rs` and
//! `calculated.rs` hold the rules): New measure, New column, editing a formula, deleting one,
//! renaming a Measure. All undoable; a change that fails changes nothing.
//!
//! - DAX is kept as written (line breaks normalised to `\n`, trailing blanks dropped, so a
//!   save and open give it back). A Measure or Calculated column whose DAX doesn't parse is
//!   kept, as in Power BI, and shows its error; one whose DAX would close a cycle, or make a
//!   chain of Measures referencing Measures deeper than 64, is refused.
//! - **Calculated columns are recomputed** ([`recompute`]) after every load, on open, and after
//!   every edit that changes the Model (`App::edit_noting`: a formula, a Relationship, a rename,
//!   a data type, a deleted Table): their values depend on the data, the Relationships and each
//!   other. They are never in the Data cache.
//! - Programmatic calls never open a dialog: a missing `name` or `expression` is an error.
//!
//! Parity uncertainties: Power BI names a new Measure `Measure` (`Measure 2`, …) and a new
//! column `Column` until the formula bar names it; we need `name`.

use metriccraft_data::culture::{CULTURES, Culture};
use metriccraft_model::{Measure, Object, normalise_expression};
use serde_json::Value;

use crate::model::{invalid, text};
use crate::{App, Document, Error, Result};

pub(crate) fn has_model_tables(app: &App) -> bool {
    !app.document().model.tables.is_empty()
}

/// Recomputes every Calculated column of `doc` (see the module doc); `limit`: bytes the new
/// columns may use.
pub(crate) fn recompute(doc: &mut Document, limit: usize) -> Result {
    if !doc.model.tables.iter().any(|t| t.has_calculated()) {
        doc.formula_errors.clear();
        return Ok(());
    }
    let fallback = Culture::get("en-US")
        .or(CULTURES.first())
        .ok_or_else(|| invalid("model", "no culture".into()))?;
    let queries = &doc.queries;
    let cultures = |table: &str| {
        queries
            .iter()
            .find(|q| q.name == table)
            .map_or(fallback, |q| q.culture)
    };
    doc.formula_errors = doc
        .model
        .compute_calculated(&mut doc.tables, &cultures, limit, None)
        .map_err(Error::Model)?;
    Ok(())
}

fn expression(params: &Value, id: &str) -> Result<String> {
    let e = normalise_expression(&text(params, "expression", id)?);
    if e.is_empty() {
        return Err(invalid(id, "`expression` must not be empty".into()));
    }
    Ok(e)
}

fn optional(params: &Value, key: &str, id: &str) -> Result<String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(String::new()),
        Some(_) => text(params, key, id),
    }
}

/// A Measure or Column name Power BI accepts.
fn new_name(params: &Value, key: &str, id: &str) -> Result<String> {
    let name = text(params, key, id)?;
    if name.trim().is_empty() {
        return Err(invalid(id, format!("`{key}` must not be empty")));
    }
    if name.trim() != name {
        return Err(invalid(
            id,
            format!("`{key}` can't start or end with a space"),
        ));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid(
            id,
            format!("`{key}` can't contain control characters"),
        ));
    }
    Ok(name)
}

fn table_name(doc: &Document, params: &Value, id: &str) -> Result<String> {
    let table = text(params, "table", id)?;
    if doc.model.table(&table).is_none() {
        return Err(invalid(id, format!("there is no table `{table}`")));
    }
    Ok(table)
}

/// `modeling.newMeasure`.
pub(crate) fn new_measure(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.newMeasure";
    let table = table_name(doc, params, ID)?;
    let name = new_name(params, "name", ID)?;
    let mut m = Measure::new(&name, &expression(params, ID)?);
    m.format_string = optional(params, "formatString", ID)?;
    m.description = optional(params, "description", ID)?;
    doc.model.add_measure(&table, m).map_err(Error::Model)
}

/// `modeling.newColumn`.
pub(crate) fn new_column(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.newColumn";
    let table = table_name(doc, params, ID)?;
    let name = new_name(params, "name", ID)?;
    let e = expression(params, ID)?;
    if !doc.tables.iter().any(|t| t.name() == table) {
        return Err(invalid(ID, format!("table `{table}` has no data")));
    }
    doc.model
        .add_calculated_column(&table, &name, &e)
        .map_err(Error::Model)
}

/// `modeling.editExpression`.
pub(crate) fn edit_expression(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.editExpression";
    let table = table_name(doc, params, ID)?;
    let name = text(params, "name", ID)?;
    let e = expression(params, ID)?;
    doc.model
        .set_expression(&table, &name, &e)
        .map_err(Error::Model)
}

/// `modeling.delete`.
pub(crate) fn delete(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.delete";
    let table = table_name(doc, params, ID)?;
    let name = text(params, "name", ID)?;
    let gone = doc
        .model
        .delete_expression(&table, &name)
        .map_err(Error::Model)?;
    if matches!(gone, Object::Column { .. }) {
        for t in &mut doc.tables {
            if t.name() == table {
                *t = t.without_columns(|c| c == name).map_err(Error::Data)?;
            }
        }
    }
    Ok(())
}

/// `modeling.renameMeasure`.
pub(crate) fn rename_measure(app: &mut App, params: &Value) -> Result {
    const ID: &str = "modeling.renameMeasure";
    let table = table_name(app.document(), params, ID)?;
    let old = text(params, "measure", ID)?;
    let new = new_name(params, "name", ID)?;
    if new == old {
        app.document()
            .model
            .object(&table, &old)
            .map_err(Error::Model)?;
        return Ok(());
    }
    app.edit_noting(|doc| {
        doc.model
            .rename_measure(&table, &old, &new)
            .map_err(Error::Model)?;
        doc.report.rename_measure(&old, &new);
        Ok(None)
    })?;
    app.file.preserved.rename_measure(&table, &old, &new);
    Ok(())
}

impl Document {
    /// Runs a DAX query (`EVALUATE …`, optional `DEFINE`) against the loaded data, with the
    /// Model's Measures in scope.
    pub fn dax_query(
        &self,
        query: &str,
    ) -> metriccraft_model::dax::Result<Vec<metriccraft_model::dax::ResultTable>> {
        let db = metriccraft_model::dax::Database::new(&self.model, &self.tables);
        metriccraft_model::dax::model_eval::run_model_query_text(
            &db,
            &self.model,
            query,
            metriccraft_model::dax::Options::default(),
        )
    }
}

#[cfg(test)]
mod tests;
