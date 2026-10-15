//! The Model view layout (ADR 0003, M5 amendment): where each Table's box sits, saved in
//! `model/diagram.json` as `{"tables": {"<Table>": {"x": 0, "y": 0}}}` (integer logical pixels,
//! keys sorted). Tables without a position are placed by the UI; positions of Tables that no
//! longer exist are dropped on save.

use std::collections::BTreeMap;

use metriccraft_file::Error as FileError;
use metriccraft_file::project::DIAGRAM;
use serde_json::{Map, Value, json};

use crate::model::{invalid, text};
use crate::report::MAX_COORD;
use crate::{Document, Error, Result};

/// Table name → its box's top-left corner.
pub type Diagram = BTreeMap<String, (i32, i32)>;

/// `diagram.json`'s bytes: only Tables of the model, `None` when none has a position.
pub(crate) fn encode(doc: &Document) -> Result<Option<Vec<u8>>> {
    let mut tables = Map::new();
    for t in &doc.model.tables {
        if let Some((x, y)) = doc.diagram.get(&t.name) {
            tables.insert(t.name.clone(), json!({"x": x, "y": y}));
        }
    }
    if tables.is_empty() {
        return Ok(None);
    }
    let mut s = serde_json::to_string_pretty(&json!({ "tables": tables }))
        .map_err(|e| Error::File(FileError::Save(e.to_string())))?;
    s.push('\n');
    Ok(Some(s.into_bytes()))
}

/// The layout from `diagram.json`; unknown members are ignored, a malformed file is an error
/// naming it.
pub(crate) fn decode(bytes: Option<&[u8]>) -> Result<Diagram> {
    let err = |m: &str| {
        Error::File(FileError::Part {
            path: DIAGRAM.into(),
            message: m.into(),
        })
    };
    let Some(bytes) = bytes else {
        return Ok(Diagram::new());
    };
    let v: Value =
        serde_json::from_slice(bytes).map_err(|e| err(&format!("not valid JSON: {e}")))?;
    let mut out = Diagram::new();
    let Some(tables) = v.get("tables") else {
        return Ok(out);
    };
    let tables = tables
        .as_object()
        .ok_or_else(|| err("`tables` must be an object"))?;
    for (name, pos) in tables {
        let coord = |k: &str| {
            pos.get(k)
                .and_then(Value::as_i64)
                .and_then(|n| i32::try_from(n).ok())
                .filter(|n| (0..=MAX_COORD).contains(n))
                .ok_or_else(|| {
                    err(&format!(
                        "`{name}`: `{k}` must be an integer, 0 to {MAX_COORD}"
                    ))
                })
        };
        out.insert(name.clone(), (coord("x")?, coord("y")?));
    }
    Ok(out)
}

/// `model.moveTable`: puts a Table's box at `x`, `y` in Model view.
pub(crate) fn move_table(doc: &mut Document, params: &Value) -> Result {
    const ID: &str = "model.moveTable";
    let name = text(params, "table", ID)?;
    let key = metriccraft_model::name_key(&name);
    let table = doc
        .model
        .tables
        .iter()
        .find(|t| metriccraft_model::name_key(&t.name) == key)
        .ok_or_else(|| invalid(ID, format!("there is no table `{name}`")))?
        .name
        .clone();
    let coord = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_i64)
            .and_then(|n| i32::try_from(n).ok())
            .filter(|n| (0..=MAX_COORD).contains(n))
            .ok_or_else(|| invalid(ID, format!("`{k}` must be an integer, 0 to {MAX_COORD}")))
    };
    let pos = (coord("x")?, coord("y")?);
    doc.diagram.insert(table, pos);
    Ok(())
}

/// A Table renamed: its box keeps its place.
pub(crate) fn rename_table(diagram: &mut Diagram, old: &str, new: &str) {
    if let Some(pos) = diagram.remove(old) {
        diagram.insert(new.into(), pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_files_are_errors_not_panics() {
        for bad in [
            &b"[]"[..],
            b"{\"tables\": 3}",
            b"{\"tables\": {\"T\": {\"x\": -1, \"y\": 0}}}",
            b"{\"tables\": {\"T\": {\"x\": 1e30, \"y\": 0}}}",
            b"{\"tables\": {\"T\": {\"x\": 1}}}",
            b"not json",
        ] {
            let r = decode(Some(bad));
            let fine = matches!(&r, Err(Error::File(FileError::Part { .. })))
                || matches!(&r, Ok(d) if d.is_empty());
            assert!(fine, "{r:?}");
        }
        assert_eq!(decode(None).ok(), Some(Diagram::new()));
        let ok = decode(Some(
            b"{\"tables\": {\"T\": {\"x\": 4, \"y\": 5}}, \"zoom\": 2}",
        ));
        assert_eq!(ok.ok(), Some(Diagram::from([("T".into(), (4, 5))])));
    }
}
