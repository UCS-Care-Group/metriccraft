//! Command parameter schemas (spec C-3, ADR 0008): what each command takes, checked by
//! [`crate::App::run`] before the command runs, and read by the UI's forms, the command palette,
//! MCP and CLI help.

use serde_json::Value;

/// One named parameter of a command.
#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: &'static str,
    pub kind: Kind,
    /// Must be given. Optional params have a default (said in `doc`) or matter only sometimes.
    pub required: bool,
    /// `null` is a meaningful value (it clears or resets something).
    pub nullable: bool,
    pub doc: &'static str,
}

/// What a parameter's value is. Every name kind is text naming an object of the document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    /// One of these words (compared ignoring ASCII case).
    Choice(&'static [&'static str]),
    Integer,
    Number,
    Bool,
    /// A list of text.
    Texts,
    Table,
    /// A Column (or Measure, where the command says so) of the model.
    Column,
    Measure,
    Query,
    /// A Page's id or name.
    Page,
    /// A Visual's id.
    Visual,
    /// A file or folder path.
    Path,
    /// A Field: `{table, column}` or `{measure}`.
    Field,
    /// Anything; the command checks the shape.
    Json,
}

impl Kind {
    /// How the kind reads in docs.
    pub fn word(self) -> String {
        match self {
            Self::Text => "text".into(),
            Self::Choice(words) => words.join(" | "),
            Self::Integer => "integer".into(),
            Self::Number => "number".into(),
            Self::Bool => "logical".into(),
            Self::Texts => "list of text".into(),
            Self::Table => "Table".into(),
            Self::Column => "Column".into(),
            Self::Measure => "Measure".into(),
            Self::Query => "Query".into(),
            Self::Page => "Page".into(),
            Self::Visual => "Visual".into(),
            Self::Path => "path".into(),
            Self::Field => "Field".into(),
            Self::Json => "JSON".into(),
        }
    }

    fn accepts(self, v: &Value) -> bool {
        match self {
            Self::Text
            | Self::Table
            | Self::Column
            | Self::Measure
            | Self::Query
            | Self::Page
            | Self::Visual
            | Self::Path => v.is_string(),
            Self::Choice(words) => v
                .as_str()
                .is_some_and(|s| words.iter().any(|w| w.eq_ignore_ascii_case(s))),
            Self::Integer => v.is_i64() || v.is_u64(),
            Self::Number => v.is_number(),
            Self::Bool => v.is_boolean(),
            Self::Texts => v
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
            Self::Field => v.is_object(),
            Self::Json => true,
        }
    }
}

/// A shorthand for the command table: a required param.
pub(crate) const fn req(name: &'static str, kind: Kind, doc: &'static str) -> Param {
    Param {
        name,
        kind,
        required: true,
        nullable: false,
        doc,
    }
}

/// An optional param.
pub(crate) const fn opt(name: &'static str, kind: Kind, doc: &'static str) -> Param {
    Param {
        name,
        kind,
        required: false,
        nullable: false,
        doc,
    }
}

/// A param that may be `null`.
pub(crate) const fn null(p: Param) -> Param {
    Param {
        nullable: true,
        ..p
    }
}

/// Checks `params` against `schema`: an object (or `null` for none) with only known names, every
/// required one present, each value of its kind. Returns the first problem.
pub fn validate(schema: &[Param], params: &Value) -> Result<(), String> {
    let empty = serde_json::Map::new();
    let map = match params {
        Value::Null => &empty,
        Value::Object(map) => map,
        _ => return Err("params must be an object".into()),
    };
    if let Some(name) = map
        .keys()
        .find(|k| !schema.iter().any(|p| p.name == k.as_str()))
    {
        let known: Vec<&str> = schema.iter().map(|p| p.name).collect();
        return Err(if known.is_empty() {
            format!("unknown param `{name}`: this command takes none")
        } else {
            format!("unknown param `{name}` (takes {})", known.join(", "))
        });
    }
    for p in schema {
        match map.get(p.name) {
            None if p.required => return Err(format!("missing param `{}`", p.name)),
            None => {}
            Some(Value::Null) if p.nullable => {}
            Some(v) if p.kind.accepts(v) => {}
            Some(_) => {
                let null = if p.nullable { " or null" } else { "" };
                return Err(format!("`{}` must be {}{null}", p.name, p.kind.word()));
            }
        }
    }
    Ok(())
}

/// A command's params as text, one per line: `name: kind — doc`, `?` marking optional ones.
pub fn doc(schema: &[Param]) -> String {
    schema
        .iter()
        .map(|p| {
            let opt = if p.required { "" } else { "?" };
            let null = if p.nullable { " or null" } else { "" };
            format!("{}{opt}: {}{null} — {}", p.name, p.kind.word(), p.doc)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SCHEMA: &[Param] = &[
        req("page", Kind::Page, "the Page"),
        opt("index", Kind::Integer, "where"),
        opt("well", Kind::Choice(&["values", "rows"]), "the well"),
        null(opt("by", Kind::Column, "sort by")),
        opt("columns", Kind::Texts, "columns"),
        opt("on", Kind::Bool, "on"),
        opt("field", Kind::Field, "a Field"),
        opt("any", Kind::Json, "anything"),
    ];

    #[test]
    fn accepts_well_formed_params() {
        assert_eq!(validate(SCHEMA, &json!({"page": "P"})), Ok(()));
        let all = json!({"page": "P", "index": 2, "well": "ROWS", "by": null, "columns": ["a"],
                         "on": true, "field": {"measure": "M"}, "any": [1, null]});
        assert_eq!(validate(SCHEMA, &all), Ok(()));
        assert_eq!(validate(&[], &Value::Null), Ok(()));
        assert_eq!(validate(&[], &json!({})), Ok(()));
    }

    #[test]
    fn rejects_each_kind_of_mistake() {
        let err = |v| validate(SCHEMA, &v).unwrap_err();
        assert_eq!(err(json!({})), "missing param `page`");
        assert!(err(json!({"page": "P", "pgae": 1})).starts_with("unknown param `pgae`"));
        assert_eq!(err(json!({"page": 3})), "`page` must be Page");
        assert_eq!(
            err(json!({"page": "P", "index": 1.5})),
            "`index` must be integer"
        );
        assert_eq!(
            err(json!({"page": "P", "well": "axis"})),
            "`well` must be values | rows"
        );
        assert_eq!(
            err(json!({"page": "P", "by": 1})),
            "`by` must be Column or null"
        );
        assert_eq!(
            err(json!({"page": "P", "index": null})),
            "`index` must be integer"
        );
        assert_eq!(
            err(json!({"page": "P", "columns": ["a", 1]})),
            "`columns` must be list of text"
        );
        assert_eq!(
            err(json!({"page": "P", "on": "yes"})),
            "`on` must be logical"
        );
        assert_eq!(
            err(json!({"page": "P", "field": "M"})),
            "`field` must be Field"
        );
        assert_eq!(err(json!(["P"])), "params must be an object");
        assert_eq!(
            validate(&[], &json!({"x": 1})).unwrap_err(),
            "unknown param `x`: this command takes none"
        );
    }

    #[test]
    fn every_command_schema_is_well_formed() {
        for c in crate::COMMANDS {
            for (i, p) in c.params.iter().enumerate() {
                assert!(!p.doc.is_empty(), "{}.{} has a doc", c.id, p.name);
                let dup = c.params.iter().skip(i + 1).any(|q| q.name == p.name);
                assert!(!dup, "{}: `{}` twice", c.id, p.name);
            }
        }
    }

    #[test]
    fn run_rejects_bad_params_before_running() {
        let mut app = crate::App::new();
        let e = app
            .run("insert.newPage", &json!({"nmae": "X"}))
            .unwrap_err();
        assert!(matches!(e, crate::Error::InvalidParams { .. }), "{e:?}");
        assert!(app.document().report.pages.is_empty());
    }
}
