//! Rename, delete and duplicate (spec D-4, C-1): Tables and Queries are 1:1 and share a name,
//! so renaming or deleting either does both, as in Power BI.
//!
//! **Rename a Table / Query** (`modeling.renameTable`, `query.rename`): one undoable edit that
//! renames the Query, its loaded Table and model Table, every Relationship end on it, every
//! Merged Queries step in other Queries that takes it in (`Merge::query`; the merged column's
//! own name, `Merge::column`, is the other Query's and is kept), and its Stale / Enable load
//! flags, and its loaded data's provenance (so a save and open keep it fresh; a Query that merges
//! it keeps its fresh provenance too). Nothing is reloaded and nothing goes Stale: the data is
//! the same.
//!
//! **Rename a model Column** (`modeling.renameColumn`): Power BI renames a column of an import
//! Table in Data / Report / Model view by adding a *Renamed Columns* step to its Query (Microsoft
//! Learn, "Work with TMDL view": the UI adds a rename step while the model column's name and
//! its `sourceColumn` are identical, which in OpenDash BI they always are). So one undoable
//! edit: appends `Table.RenameColumns` (old → new) to the Query, renames the loaded column and
//! the model Column (keeping its properties), every sort by and Relationship end on it, and the
//! key and expanded columns that Merged Queries steps in other Queries take from it (the
//! columns those merges add keep their names). The loaded data already matches the new step,
//! so nothing goes Stale.
//!
//! Both renames carry the object's unknown file content (what the opened files held beyond
//! the model, kept per object) to the new name, so the next save still writes it. It is copied,
//! not moved: it lives outside undo, and an undone rename must find it under the old name.
//!
//! Names are unique ignoring case (Tables with Queries; Columns within a Table), as Analysis
//! Services compares them. Renaming to the very same name changes nothing and records no undo
//! step; a change of case alone is a rename.
//!
//! **DAX** (M3.6): both renames, and renaming a Measure (`modeling.renameMeasure`,
//! `expression.rs`), rewrite every reference in the Model's Measures and Calculated columns
//! through the AST, replacing only the referencing bytes (`opendash_model`, `expression.rs`).
//! Renaming a Calculated column adds no Query step: it isn't the Query's.
//!
//! Parity uncertainties, to observe in the VM:
//! - whether a model column rename extends a trailing *Renamed Columns* step instead of adding
//!   one (we always add one);
//! - which names Power BI refuses (we refuse empty names, leading or trailing spaces and
//!   control characters) and its collision message wording;
//! - deleting a Query that another merges: Power Query warns and lists the dependants; we
//!   refuse until the merge is gone;
//! - a duplicated Query's name (`Sales (2)`, the next free number) and whether it copies
//!   *Enable load* (we copy it).

use opendash_data::step::Step;
use opendash_file::cache::m_hash;
use opendash_model::same_name;
use serde_json::Value;

use crate::model::{invalid, text};
use crate::query::check_steps;
use crate::{App, Document, Error, Result};

/// `modeling.renameTable`: renames a loaded Table and its Query.
pub(crate) fn rename_table(app: &mut App, params: &Value) -> Result {
    const ID: &str = "modeling.renameTable";
    let old = text(params, "table", ID)?;
    if !app.document().tables.iter().any(|t| t.name() == old) {
        return Err(invalid(ID, format!("there is no table `{old}`")));
    }
    rename(app, ID, old, params)
}

/// `query.rename`: renames a Query and its Table, if loaded.
pub(crate) fn rename_query(app: &mut App, params: &Value) -> Result {
    const ID: &str = "query.rename";
    let old = text(params, "query", ID)?;
    rename(app, ID, old, params)
}

fn rename(app: &mut App, id: &str, old: String, params: &Value) -> Result {
    let new = name(params, id)?;
    let doc = app.document();
    if !doc.queries.iter().any(|q| q.name == old) {
        return Err(invalid(id, format!("there is no query `{old}`")));
    }
    if new == old {
        return Ok(());
    }
    let taken = doc.queries.iter().map(|q| q.name.as_str());
    let taken = taken.chain(doc.tables.iter().map(|t| t.name()));
    if let Some(other) = taken.filter(|n| *n != old).find(|n| same_name(n, &new)) {
        return Err(invalid(id, format!("there is already a query `{other}`")));
    }
    app.edit_noting(|doc| {
        rename_in(doc, &old, &new);
        Ok(None)
    })?;
    app.file.preserved.rename_table(&old, &new);
    Ok(())
}

/// Renames Query and Table `old` to `new` everywhere (see the module doc).
fn rename_in(doc: &mut Document, old: &str, new: &str) {
    // Provenance current before the rename stays current: the renamed Table's moves with it,
    // and a hash that changes only because a merge now names `new` is recomputed.
    let current: Vec<String> = doc
        .provenance
        .iter()
        .filter(|(t, p)| p.m == m_hash(&doc.queries, t))
        .map(|(t, _)| t.clone())
        .collect();
    for q in &mut doc.queries {
        if q.name == old {
            q.name = new.into();
        } else {
            q.rename_merged_query(old, new);
        }
    }
    for t in &mut doc.tables {
        if t.name() == old {
            *t = t.renamed(new);
        }
    }
    doc.model.rename_table(old, new);
    for set in [&mut doc.stale, &mut doc.unloaded] {
        if set.remove(old) {
            set.insert(new.into());
        }
    }
    if let Some(p) = doc.provenance.remove(old) {
        doc.provenance.insert(new.into(), p);
    }
    for t in current {
        let t = if t == old { new.to_string() } else { t };
        let m = m_hash(&doc.queries, &t);
        if let Some(p) = doc.provenance.get_mut(&t) {
            p.m = m;
        }
    }
}

/// `modeling.renameColumn`: renames a model Column (see the module doc).
pub(crate) fn rename_column(app: &mut App, params: &Value) -> Result {
    const ID: &str = "modeling.renameColumn";
    let table = text(params, "table", ID)?;
    let old = text(params, "column", ID)?;
    let new = name(params, ID)?;
    let doc = app.document();
    let column = doc.model.table(&table).and_then(|t| t.column(&old));
    if column.is_none() {
        return Err(Error::Model(opendash_model::Error {
            table,
            column: old,
            message: "there is no such column".into(),
        }));
    }
    if new == old {
        return Ok(());
    }
    app.edit_noting(|doc| {
        let calculated = doc
            .model
            .table(&table)
            .and_then(|t| t.column(&old))
            .is_some_and(opendash_model::Column::is_calculated);
        doc.model
            .rename_column(&table, &old, &new)
            .map_err(Error::Model)?;
        if calculated {
            // Not the Query's: no step, nothing merges it.
            for t in &mut doc.tables {
                if t.name() == table {
                    *t = t.with_column_renamed(&old, &new).map_err(Error::Data)?;
                }
            }
            return Ok(None);
        }
        let q = doc
            .queries
            .iter()
            .position(|q| q.name == table)
            .ok_or_else(|| invalid(ID, format!("there is no query `{table}`")))?;
        if let Some(query) = doc.queries.get_mut(q) {
            query
                .steps
                .push(Step::RenamedColumns(vec![(old.clone(), new.clone())]));
        }
        check_steps(doc, q, ID)?;
        for (i, query) in doc.queries.iter_mut().enumerate() {
            if i != q {
                query.rename_merged_column(&table, &old, &new);
            }
        }
        for t in &mut doc.tables {
            if t.name() == table {
                *t = t.with_column_renamed(&old, &new).map_err(Error::Data)?;
            }
        }
        Ok(None)
    })?;
    app.file.preserved.rename_column(&table, &old, &new);
    Ok(())
}

/// `query.delete`: deletes a Query and its Table.
pub(crate) fn delete_query(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "query.delete";
    let name = text(params, "query", ID)?;
    delete(doc, ID, &name)
}

/// `modeling.deleteTable`: deletes a Table and its Query.
pub(crate) fn delete_table(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.deleteTable";
    let name = text(params, "table", ID)?;
    if !doc.tables.iter().any(|t| t.name() == name) {
        return Err(invalid(ID, format!("there is no table `{name}`")));
    }
    delete(doc, ID, &name)
}

/// Deletes Query `name`, its Table, model Table and Relationships, and its flags. Refused while
/// another Query merges it.
fn delete(doc: &mut Document, id: &str, name: &str) -> Result {
    if !doc.queries.iter().any(|q| q.name == name) {
        return Err(invalid(id, format!("there is no query `{name}`")));
    }
    if let Some(user) = doc
        .queries
        .iter()
        .find(|q| q.name != name && q.merges(name))
    {
        return Err(invalid(
            id,
            format!(
                "`{}` merges `{name}`: delete or change that step first",
                user.name
            ),
        ));
    }
    doc.queries.retain(|q| q.name != name);
    doc.tables.retain(|t| t.name() != name);
    doc.model.delete_table(name);
    doc.stale.remove(name);
    doc.unloaded.remove(name);
    doc.provenance.remove(name);
    Ok(())
}

/// `query.duplicate`: a copy of a Query's steps under the next free name `<name> (n)`, not
/// loaded until Close & Apply (it is Stale unless its Enable load, copied, is off).
pub(crate) fn duplicate_query(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "query.duplicate";
    let name = text(params, "query", ID)?;
    let mut copy = doc
        .queries
        .iter()
        .find(|q| q.name == name)
        .cloned()
        .ok_or_else(|| invalid(ID, format!("there is no query `{name}`")))?;
    let taken = |n: &str| {
        doc.queries.iter().any(|q| same_name(&q.name, n))
            || doc.tables.iter().any(|t| same_name(t.name(), n))
    };
    // Each name tried is taken by a distinct Query or Table, so one is free within this many.
    let tries = doc
        .queries
        .len()
        .saturating_add(doc.tables.len())
        .saturating_add(2);
    copy.name = (2..=tries)
        .map(|n| format!("{name} ({n})"))
        .find(|n| !taken(n))
        .ok_or_else(|| invalid(ID, "no free name".into()))?;
    if doc.unloaded.contains(&name) {
        doc.unloaded.insert(copy.name.clone());
    } else {
        doc.stale.insert(copy.name.clone());
    }
    doc.queries.push(copy);
    Ok(())
}

/// `params.name`: a Table, Query or Column name Power BI accepts.
fn name(params: &Value, id: &str) -> Result<String> {
    let name = text(params, "name", id)?;
    if name.trim().is_empty() {
        return Err(invalid(id, "`name` must not be empty".into()));
    }
    if name.trim() != name {
        return Err(invalid(id, "`name` can't start or end with a space".into()));
    }
    if name.chars().any(char::is_control) {
        return Err(invalid(
            id,
            "`name` can't contain control characters".into(),
        ));
    }
    Ok(name)
}

#[cfg(test)]
mod tests;
