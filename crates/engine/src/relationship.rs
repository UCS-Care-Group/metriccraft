//! Relationship commands (spec D-2): Power BI's Modeling > Manage relationships. The rules
//! (keys, one active per pair, auto-detect) are `opendash_model`'s (`relationship.rs`).
//!
//! An existing Relationship is named by `relationship`: its position in Manage relationships
//! order (`Model::relationships`, 0 = first), or `{fromTable, fromColumn, toTable, toColumn}`
//! (unique: two Relationships never link the same two columns).

use opendash_model::Relationship;
use serde_json::Value;

use crate::model::{invalid, text};
use crate::{App, Document, Error, Result};

/// `modeling.newRelationship`.
pub(crate) fn new_relationship(app: &mut App, params: &Value) -> Result {
    const ID: &str = "modeling.newRelationship";
    let mut r = ends(params, ID)?;
    r.active = flag(params, "active", ID)?.unwrap_or(true);
    app.edit_noting(|doc| {
        doc.model
            .add_relationship(r, &doc.tables)
            .map_err(Error::Model)
    })
}

/// `modeling.editRelationship`: changes the given ends (and `active`) of a Relationship.
pub(crate) fn edit_relationship(app: &mut App, params: &Value) -> Result {
    const ID: &str = "modeling.editRelationship";
    let index = find(app.document(), params, ID)?;
    let mut r = app
        .document()
        .model
        .relationships
        .get(index)
        .cloned()
        .ok_or_else(|| invalid(ID, format!("there is no relationship {index}")))?;
    for (key, slot) in [
        ("fromTable", &mut r.from_table),
        ("fromColumn", &mut r.from_column),
        ("toTable", &mut r.to_table),
        ("toColumn", &mut r.to_column),
    ] {
        if params.get(key).is_some() {
            *slot = text(params, key, ID)?;
        }
    }
    if let Some(active) = flag(params, "active", ID)? {
        r.active = active;
    }
    app.edit_noting(|doc| {
        doc.model
            .edit_relationship(index, r, &doc.tables)
            .map_err(Error::Model)
    })
}

/// `modeling.deleteRelationship`.
pub(crate) fn delete_relationship(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.deleteRelationship";
    let index = find(doc, params, ID)?;
    doc.model
        .remove_relationship(index)
        .map(|_| ())
        .ok_or_else(|| invalid(ID, format!("there is no relationship {index}")))
}

/// `modeling.setRelationshipActive`.
pub(crate) fn set_active(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "modeling.setRelationshipActive";
    let index = find(doc, params, ID)?;
    let active = flag(params, "active", ID)?.unwrap_or(true);
    doc.model
        .set_relationship_active(index, active)
        .map_err(Error::Model)
}

/// `modeling.autodetectRelationships`: auto-detect over every pair of Tables, whatever the
/// Project's switch says. Finding nothing changes nothing (no undo step).
pub(crate) fn autodetect(app: &mut App, _params: &Value) -> Result {
    let doc = app.document();
    let mut model = doc.model.clone();
    let found = model.autodetect(&doc.tables, |_| true).len();
    if found == 0 {
        app.note("Found no new relationships.");
        return Ok(());
    }
    let notice = if found == 1 {
        "Found 1 new relationship.".to_string()
    } else {
        format!("Found {found} new relationships.")
    };
    app.edit_noting(|doc| {
        doc.model = model;
        Ok(Some(notice))
    })
}

/// `options.autodetectRelationships`: the Project's *Autodetect new relationships after data
/// is loaded* switch.
pub(crate) fn set_autodetect(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "options.autodetectRelationships";
    let on = flag(params, "enabled", ID)?.unwrap_or(!doc.model.autodetect_relationships);
    doc.model.autodetect_relationships = on;
    Ok(())
}

/// The Relationship `params.relationship` names: its index.
fn find(doc: &Document, params: &Value, id: &str) -> Result<usize> {
    let all = &doc.model.relationships;
    match params.get("relationship") {
        Some(Value::Number(n)) => n
            .as_u64()
            .and_then(|i| usize::try_from(i).ok())
            .filter(|i| *i < all.len())
            .ok_or_else(|| invalid(id, format!("there is no relationship {n}"))),
        Some(v @ Value::Object(_)) => {
            let r = ends(v, id)?;
            all.iter()
                .position(|o| {
                    (&o.from_table, &o.from_column, &o.to_table, &o.to_column)
                        == (&r.from_table, &r.from_column, &r.to_table, &r.to_column)
                })
                .ok_or_else(|| invalid(id, format!("there is no relationship {r}")))
        }
        _ => Err(invalid(
            id,
            "`relationship` must be a position (0 = first) or {fromTable, fromColumn, toTable, \
             toColumn}"
                .into(),
        )),
    }
}

/// An active Relationship from `params`' four ends.
fn ends(params: &Value, id: &str) -> Result<Relationship> {
    Ok(Relationship::new(
        &text(params, "fromTable", id)?,
        &text(params, "fromColumn", id)?,
        &text(params, "toTable", id)?,
        &text(params, "toColumn", id)?,
    ))
}

/// The optional logical `params.key`.
fn flag(params: &Value, key: &str, id: &str) -> Result<Option<bool>> {
    match params.get(key) {
        None => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| invalid(id, format!("`{key}` must be true or false"))),
    }
}

#[cfg(test)]
mod tests;
