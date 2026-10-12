//! Queries ↔ Power Query's own M (ADR 0005): a Source and its Applied Steps print as the
//! `let … in` Power Query writes, which pastes into Power BI's Advanced Editor, and parse back.
//! M that isn't one of these forms is an error naming the line; there are no opaque steps.
//!
//! Adding a step's M form: one arm in [`step_m`] (print), one arm in [`step_from`] (parse, plus its name in the
//! catch-all for wrong arguments) and a
//! case in the `every_step_round_trips` test.
//!
//! Parity uncertainties, to settle with M1.10 paste checks in the VM:
//! - which calls Power Query prints with `,` and which with `, ` between arguments (mirrored here
//!   from memory of its output: `Table.TransformColumnTypes(#"Promoted Headers",{{…}})`, but
//!   `Table.PromoteHeaders(Source, [PromoteAllScalars=true])`);
//! - the culture form `Table.TransformColumnTypes(x, {{…}}, "en-GB")` spacing;
//! - `QuoteStyle.Csv` vs Power Query's `QuoteStyle.None` default for files without multi-line
//!   cells; codepages beyond 65001/1252/1200/1201;
//! - steps renamed in Power Query lose their names here (a [`Step`] has no name of its own);
//! - `Table.SelectRows(prev, each ([A] = 1 and [B] > 2))`: parentheses around comparisons only,
//!   `#date`/`#datetime` argument spacing, and how Power Query prints negative numbers;
//! - a Merge prints as two steps, `#"Merged Queries"` (`Table.NestedJoin`, spaced arguments) and
//!   `#"Expanded <column>"` (`Table.ExpandTableColumn`); parsing folds them back into one.

use std::collections::HashSet;

use opendash_m::{BinOp, Binding, Error, Expr, Let};

use crate::culture::{Culture, days_from_civil};
use crate::source::xlsx::civil;
use crate::source::{Encoding, ExcelKind, File, Source};
use crate::step::{ColumnType, Combine, Condition, JoinKind, Merge, Op, Step, Value};

/// The Windows codepages M's `Encoding=` takes, for the encodings a CSV Source can have.
const CODEPAGES: [(u32, &Encoding); 4] = [
    (65001, encoding_rs::UTF_8),
    (1252, encoding_rs::WINDOWS_1252),
    (1200, encoding_rs::UTF_16LE),
    (1201, encoding_rs::UTF_16BE),
];

/// The codepage M writes for `encoding`; `None` for one M can't name here (Get Data refuses
/// those, and detection only finds these four).
pub fn codepage(encoding: &Encoding) -> Option<u32> {
    CODEPAGES
        .iter()
        .find(|(_, e)| *e == encoding)
        .map(|(cp, _)| *cp)
}

fn excel_kind(kind: ExcelKind) -> &'static str {
    match kind {
        ExcelKind::Sheet => "Sheet",
        ExcelKind::Table => "Table",
    }
}

/// A Query as M describes it: Source, Applied Steps and the culture Changed Type steps use when
/// it isn't the Project's. A Source's file prints as `File.Contents(path)` and parses as a path.
#[derive(Clone, Debug, PartialEq)]
pub struct MQuery {
    pub source: Source,
    pub steps: Vec<Step>,
    pub culture: Option<&'static Culture>,
}

fn file_contents(path: &str) -> Expr {
    Expr::call("File.Contents", vec![Expr::text(path)], false)
}

fn type_m(ty: ColumnType) -> Expr {
    match ty {
        ColumnType::Text => Expr::Type("text".into()),
        ColumnType::Decimal => Expr::Type("number".into()),
        ColumnType::Whole => Expr::ident("Int64.Type"),
        ColumnType::Date => Expr::Type("date".into()),
        ColumnType::DateTime => Expr::Type("datetime".into()),
        ColumnType::Boolean => Expr::Type("logical".into()),
    }
}

fn type_from(e: &Expr) -> Option<ColumnType> {
    Some(match e {
        Expr::Type(t) if t == "text" => ColumnType::Text,
        Expr::Type(t) if t == "number" => ColumnType::Decimal,
        Expr::Ident(t) if t == "Int64.Type" => ColumnType::Whole,
        Expr::Type(t) if t == "date" => ColumnType::Date,
        Expr::Type(t) if t == "datetime" => ColumnType::DateTime,
        Expr::Type(t) if t == "logical" => ColumnType::Boolean,
        _ => return None,
    })
}

fn texts(names: &[String]) -> Expr {
    Expr::list(names.iter().map(Expr::text).collect(), false)
}

/// A number literal; M has no negative literals, so `-5` is a negation.
fn number(n: f64) -> Expr {
    if n < 0.0 {
        Expr::Negate(Box::new(Expr::Number(-n)))
    } else {
        Expr::Number(n)
    }
}

const DAY_MICROS: i64 = 86_400_000_000;

fn date_parts(days: i64) -> Vec<Expr> {
    let (y, m, d) = civil(days);
    [y, m, d].iter().map(|v| number(*v as f64)).collect()
}

fn value_m(v: &Value) -> Expr {
    match v {
        Value::Null => Expr::Null,
        Value::Number(n) => number(*n),
        Value::Text(s) => Expr::text(s),
        Value::Logical(b) => Expr::Logical(*b),
        Value::Date(d) => Expr::call("#date", date_parts(i64::from(*d)), false),
        Value::DateTime(t) => {
            let rem = t.rem_euclid(DAY_MICROS);
            let mut parts = date_parts(t.div_euclid(DAY_MICROS));
            parts.push(number((rem / 3_600_000_000) as f64));
            parts.push(number((rem / 60_000_000 % 60) as f64));
            parts.push(number((rem % 60_000_000) as f64 / 1e6));
            Expr::call("#datetime", parts, false)
        }
    }
}

fn cond_m(c: &Condition) -> Expr {
    let field = Expr::Field(None, c.column.clone());
    let value = value_m(&c.value);
    let op = match c.op {
        Op::Equals => BinOp::Equal,
        Op::NotEquals => BinOp::NotEqual,
        Op::Greater => BinOp::Greater,
        Op::GreaterOrEqual => BinOp::GreaterEqual,
        Op::Less => BinOp::Less,
        Op::LessOrEqual => BinOp::LessEqual,
        Op::Contains => return Expr::call("Text.Contains", vec![field, value], false),
        Op::BeginsWith => return Expr::call("Text.StartsWith", vec![field, value], false),
        Op::EndsWith => return Expr::call("Text.EndsWith", vec![field, value], false),
    };
    Expr::Binary(op, Box::new(field), Box::new(value))
}

fn join_kind(kind: JoinKind) -> &'static str {
    match kind {
        JoinKind::LeftOuter => "JoinKind.LeftOuter",
        JoinKind::Inner => "JoinKind.Inner",
        JoinKind::FullOuter => "JoinKind.FullOuter",
    }
}

/// `Table.ExpandTableColumn`, the second M step of a Merge.
fn expand_m(m: &Merge, merged: &str) -> Expr {
    let (cols, names): (Vec<String>, Vec<String>) = m.expand.iter().cloned().unzip();
    Expr::call(
        "Table.ExpandTableColumn",
        vec![
            Expr::ident(merged),
            Expr::text(&m.column),
            texts(&cols),
            texts(&names),
        ],
        false,
    )
}

/// A step's M: the call that turns `prev` (the previous step) into its result. A Merge's
/// `Table.ExpandTableColumn` step is added by [`print`].
fn step_m(step: &Step, prev: Expr, culture: Option<&Culture>) -> Expr {
    match step {
        Step::PromotedHeaders => Expr::call(
            "Table.PromoteHeaders",
            vec![
                prev,
                Expr::record(vec![("PromoteAllScalars", Expr::Logical(true))], false),
            ],
            false,
        ),
        Step::ChangedType(types) => {
            let pairs = types
                .iter()
                .map(|(c, t)| Expr::list(vec![Expr::text(c), type_m(*t)], false))
                .collect();
            let mut args = vec![prev, Expr::list(pairs, false)];
            if let Some(c) = culture {
                args.push(Expr::text(c.name));
            }
            Expr::call("Table.TransformColumnTypes", args, culture.is_none())
        }
        Step::RenamedColumns(pairs) => {
            let pairs = pairs
                .iter()
                .map(|(a, b)| Expr::list(vec![Expr::text(a), Expr::text(b)], false))
                .collect();
            Expr::call(
                "Table.RenameColumns",
                vec![prev, Expr::list(pairs, false)],
                true,
            )
        }
        Step::RemovedColumns(names) => {
            Expr::call("Table.RemoveColumns", vec![prev, texts(names)], true)
        }
        Step::ReorderedColumns(names) => {
            Expr::call("Table.ReorderColumns", vec![prev, texts(names)], true)
        }
        Step::FilteredRows {
            combine,
            conditions,
        } => {
            let op = match combine {
                Combine::And => BinOp::And,
                Combine::Or => BinOp::Or,
            };
            let body = conditions
                .iter()
                .map(cond_m)
                .reduce(|a, b| Expr::Binary(op, Box::new(a), Box::new(b)));
            let body = match body {
                // An empty And keeps every row, an empty Or none.
                None => Expr::Logical(*combine == Combine::And),
                // Power Query brackets comparisons, not `Text.Contains(…)` calls.
                Some(b) if conditions.iter().all(|c| !is_text_op(c.op)) => Expr::Paren(Box::new(b)),
                Some(b) => b,
            };
            Expr::call(
                "Table.SelectRows",
                vec![prev, Expr::Each(Box::new(body))],
                false,
            )
        }
        Step::MergedQueries(m) => Expr::call(
            "Table.NestedJoin",
            vec![
                prev,
                texts(&m.keys),
                Expr::ident(&m.query),
                texts(&m.other_keys),
                Expr::text(&m.column),
                Expr::ident(join_kind(m.kind)),
            ],
            false,
        ),
    }
}

fn is_text_op(op: Op) -> bool {
    matches!(op, Op::Contains | Op::BeginsWith | Op::EndsWith)
}

/// The step name, numbered the way Power Query numbers repeats: `Changed Type`, `Changed Type1`…
fn unique(used: &mut HashSet<String>, base: &str) -> String {
    let mut name = base.to_string();
    let mut n = 0u32;
    while used.contains(&name) {
        n = n.saturating_add(1);
        name = format!("{base}{n}");
    }
    used.insert(name.clone());
    name
}

/// Prints a Query as Power Query's M (no trailing newline).
pub fn print(q: &MQuery) -> String {
    let mut bindings = Vec::new();
    let source = match &q.source {
        Source::Csv {
            file,
            delimiter,
            columns,
            encoding,
        } => {
            // ponytail: an encoding M can't name prints as UTF-8; none reaches here today.
            let cp = codepage(encoding).unwrap_or(65001);
            let options = Expr::record(
                vec![
                    ("Delimiter", Expr::text(char::from(*delimiter))),
                    ("Columns", Expr::Number(*columns as f64)),
                    ("Encoding", Expr::Number(f64::from(cp))),
                    // The CSV reader always honours quotes.
                    ("QuoteStyle", Expr::ident("QuoteStyle.Csv")),
                ],
                false,
            );
            Expr::call(
                "Csv.Document",
                vec![file_contents(&file.path()), options],
                true,
            )
        }
        Source::Parquet { file } => {
            Expr::call("Parquet.Document", vec![file_contents(&file.path())], false)
        }
        Source::Excel { file, .. } => Expr::call(
            "Excel.Workbook",
            vec![file_contents(&file.path()), Expr::Null, Expr::Logical(true)],
            false,
        ),
    };
    bindings.push(Binding::new("Source", source));
    if let Source::Excel { item, kind, .. } = &q.source {
        let kind = excel_kind(*kind);
        let selector = Expr::record(
            vec![("Item", Expr::text(item)), ("Kind", Expr::text(kind))],
            true,
        );
        let nav = Expr::Field(
            Some(Box::new(Expr::Item(
                Box::new(Expr::ident("Source")),
                Box::new(selector),
            ))),
            "Data".into(),
        );
        bindings.push(Binding::new(format!("{item}_{kind}"), nav));
    }
    let mut used: HashSet<String> = bindings.iter().map(|b| b.name.clone()).collect();
    for step in &q.steps {
        let prev = bindings.last().map_or_else(String::new, |b| b.name.clone());
        let name = unique(&mut used, step.name());
        bindings.push(Binding::new(
            name.clone(),
            step_m(step, Expr::Ident(prev), q.culture),
        ));
        if let Step::MergedQueries(m) = step
            && !m.expand.is_empty()
        {
            let expanded = unique(&mut used, &format!("Expanded {}", m.column));
            bindings.push(Binding::new(expanded, expand_m(m, &name)));
        }
    }
    let last = bindings.last().map_or_else(String::new, |b| b.name.clone());
    opendash_m::print(&Expr::Let(Let {
        bindings,
        body: Box::new(Expr::Ident(last)),
    }))
}

fn call(e: &Expr) -> Option<(&str, &[Expr])> {
    match e {
        Expr::Call { func, args, .. } => match func.as_ref() {
            Expr::Ident(f) => Some((f.as_str(), args.as_slice())),
            _ => None,
        },
        _ => None,
    }
}

fn text(e: &Expr) -> Option<&str> {
    match e {
        Expr::Text(s) => Some(s),
        _ => None,
    }
}

fn whole(e: &Expr) -> Option<u32> {
    match e {
        Expr::Number(n) if n.fract() == 0.0 && *n >= 0.0 && *n <= f64::from(u32::MAX) => {
            Some(*n as u32)
        }
        _ => None,
    }
}

fn text_list(e: &Expr) -> Option<Vec<String>> {
    match e {
        Expr::List { items, .. } => items.iter().map(|i| text(i).map(String::from)).collect(),
        _ => None,
    }
}

fn pairs(e: &Expr) -> Option<Vec<(&Expr, &Expr)>> {
    match e {
        Expr::List { items, .. } => items
            .iter()
            .map(|i| match i {
                Expr::List { items, .. } => match items.as_slice() {
                    [a, b] => Some((a, b)),
                    _ => None,
                },
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

fn file_path(e: &Expr) -> Option<String> {
    match call(e)? {
        ("File.Contents", [p]) => text(p).map(String::from),
        _ => None,
    }
}

/// The record's fields, if every name is in `allowed`.
fn fields<'a>(e: &'a Expr, allowed: &[&str]) -> Option<Vec<(&'a str, &'a Expr)>> {
    match e {
        Expr::Record { fields, .. } => fields
            .iter()
            .map(|(k, v)| allowed.contains(&k.as_str()).then_some((k.as_str(), v)))
            .collect(),
        _ => None,
    }
}

fn field<'a>(fields: &[(&str, &'a Expr)], name: &str) -> Option<&'a Expr> {
    fields.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// Why a Source step didn't parse, when it's more than "not a Source we know".
enum SourceError {
    Unknown,
    Unsupported(String),
}

fn csv_source(path: String, options: &Expr) -> Result<Source, SourceError> {
    let unknown = || SourceError::Unknown;
    let f =
        fields(options, &["Delimiter", "Columns", "Encoding", "QuoteStyle"]).ok_or_else(unknown)?;
    let delimiter = field(&f, "Delimiter").and_then(text).ok_or_else(unknown)?;
    let mut chars = delimiter.chars();
    let delimiter = match (chars.next(), chars.next()) {
        (Some(c), None) => u8::try_from(u32::from(c)).map_err(|_| unknown())?,
        _ => return Err(unknown()),
    };
    match field(&f, "QuoteStyle") {
        Some(Expr::Ident(q)) if q == "QuoteStyle.Csv" => {}
        Some(Expr::Ident(q)) if q == "QuoteStyle.None" => {
            return Err(SourceError::Unsupported(
                "`QuoteStyle.None` is not supported yet: OpenDash BI reads quotes as \
                 `QuoteStyle.Csv`"
                    .into(),
            ));
        }
        _ => return Err(unknown()),
    }
    let cp = field(&f, "Encoding").and_then(whole).ok_or_else(unknown)?;
    let encoding = CODEPAGES
        .iter()
        .find(|(c, _)| *c == cp)
        .map(|(_, e)| *e)
        .ok_or_else(|| SourceError::Unsupported(format!("codepage {cp} is not supported yet")))?;
    let columns = field(&f, "Columns")
        .and_then(whole)
        .and_then(|c| usize::try_from(c).ok())
        .ok_or_else(unknown)?;
    Ok(Source::Csv {
        file: File::Path(path.into()),
        delimiter,
        columns,
        encoding,
    })
}

fn source_from(e: &Expr) -> Result<Source, SourceError> {
    let path = |file: &Expr| file_path(file).ok_or(SourceError::Unknown);
    match call(e).ok_or(SourceError::Unknown)? {
        ("Csv.Document", [file, options]) => csv_source(path(file)?, options),
        ("Parquet.Document", [file]) => Ok(Source::Parquet {
            file: File::Path(path(file)?.into()),
        }),
        ("Excel.Workbook", [file, Expr::Null, Expr::Logical(true)]) => Ok(Source::Excel {
            file: File::Path(path(file)?.into()),
            item: String::new(),
            kind: ExcelKind::Sheet,
        }),
        _ => Err(SourceError::Unknown),
    }
}

/// `Source{[Item="Sheet1",Kind="Sheet"]}[Data]` → ("Sheet1", "Sheet").
fn navigation(e: &Expr, source: &str) -> Option<(String, String)> {
    let Expr::Field(Some(target), data) = e else {
        return None;
    };
    let Expr::Item(table, selector) = target.as_ref() else {
        return None;
    };
    if data != "Data" || !matches!(table.as_ref(), Expr::Ident(s) if s == source) {
        return None;
    }
    let f = fields(selector, &["Item", "Kind"])?;
    Some((
        text(field(&f, "Item")?)?.to_string(),
        text(field(&f, "Kind")?)?.to_string(),
    ))
}

fn whole_i64(e: &Expr) -> Option<i64> {
    match e {
        Expr::Negate(n) => whole(n).map(|v| -i64::from(v)),
        e => whole(e).map(i64::from),
    }
}

fn value_from(e: &Expr) -> Option<Value> {
    Some(match e {
        Expr::Null => Value::Null,
        Expr::Number(n) => Value::Number(*n),
        Expr::Negate(n) => match n.as_ref() {
            Expr::Number(n) => Value::Number(-n),
            _ => return None,
        },
        Expr::Text(s) => Value::Text(s.clone()),
        Expr::Logical(b) => Value::Logical(*b),
        _ => match call(e)? {
            ("#date", [y, m, d]) => Value::Date(days_from_civil(whole(y)?, whole(m)?, whole(d)?)?),
            ("#datetime", [y, m, d, h, mi, s]) => {
                let days = days_from_civil(whole(y)?, whole(m)?, whole(d)?)?;
                let (h, mi) = (whole_i64(h)?, whole_i64(mi)?);
                let s = match s {
                    Expr::Number(s) if (0.0..60.0).contains(s) => (s * 1e6).round() as i64,
                    _ => return None,
                };
                if !(0..24).contains(&h) || !(0..60).contains(&mi) {
                    return None;
                }
                let time = (h * 3600 + mi * 60) * 1_000_000 + s;
                Value::DateTime(i64::from(days).checked_mul(DAY_MICROS)?.checked_add(time)?)
            }
            _ => return None,
        },
    })
}

fn unparen(mut e: &Expr) -> &Expr {
    while let Expr::Paren(inner) = e {
        e = inner;
    }
    e
}

fn cond_from(e: &Expr) -> Option<Condition> {
    let e = unparen(e);
    let (op, column, value) = match e {
        Expr::Binary(op, a, b) => {
            let op = match op {
                BinOp::Equal => Op::Equals,
                BinOp::NotEqual => Op::NotEquals,
                BinOp::Greater => Op::Greater,
                BinOp::GreaterEqual => Op::GreaterOrEqual,
                BinOp::Less => Op::Less,
                BinOp::LessEqual => Op::LessOrEqual,
                _ => return None,
            };
            (op, a.as_ref(), b.as_ref())
        }
        _ => match call(e)? {
            ("Text.Contains", [a, b]) => (Op::Contains, a, b),
            ("Text.StartsWith", [a, b]) => (Op::BeginsWith, a, b),
            ("Text.EndsWith", [a, b]) => (Op::EndsWith, a, b),
            _ => return None,
        },
    };
    let Expr::Field(None, column) = column else {
        return None;
    };
    Some(Condition {
        column: column.clone(),
        op,
        value: value_from(value)?,
    })
}

/// `a and b and c` → its conditions; a different operator inside is unsupported (`None`).
fn flatten(e: &Expr, op: BinOp, out: &mut Vec<Condition>) -> Option<()> {
    match unparen(e) {
        Expr::Binary(o, a, b) if *o == op => {
            flatten(a, op, out)?;
            flatten(b, op, out)
        }
        e => {
            out.push(cond_from(e)?);
            Some(())
        }
    }
}

fn filter_from(body: &Expr) -> Option<Step> {
    let body = unparen(body);
    let (combine, op) = match body {
        Expr::Logical(true) => (Combine::And, BinOp::And),
        Expr::Logical(false) => (Combine::Or, BinOp::Or),
        Expr::Binary(BinOp::Or, ..) => (Combine::Or, BinOp::Or),
        _ => (Combine::And, BinOp::And),
    };
    let mut conditions = Vec::new();
    if !matches!(body, Expr::Logical(_)) {
        flatten(body, op, &mut conditions)?;
    }
    Some(Step::FilteredRows {
        combine,
        conditions,
    })
}

fn merge_from(args: &[Expr]) -> Option<Step> {
    let [
        keys,
        Expr::Ident(query),
        other_keys,
        column,
        Expr::Ident(kind),
    ] = args
    else {
        return None;
    };
    let kind = match kind.as_str() {
        "JoinKind.LeftOuter" => JoinKind::LeftOuter,
        "JoinKind.Inner" => JoinKind::Inner,
        "JoinKind.FullOuter" => JoinKind::FullOuter,
        _ => return None,
    };
    Some(Step::MergedQueries(Merge {
        keys: text_list(keys)?,
        query: query.clone(),
        other_keys: text_list(other_keys)?,
        kind,
        column: text(column)?.to_string(),
        expand: Vec::new(),
    }))
}

/// `Table.ExpandTableColumn(<merged>, "<column>", {cols}, {names})` after a Merge.
fn expand_from(e: &Expr, merged: &str, column: &str) -> Option<Vec<(String, String)>> {
    let ("Table.ExpandTableColumn", [Expr::Ident(input), col, cols, names]) = call(e)? else {
        return None;
    };
    if input != merged || text(col)? != column {
        return None;
    }
    let (cols, names) = (text_list(cols)?, text_list(names)?);
    (cols.len() == names.len()).then(|| cols.into_iter().zip(names).collect())
}

/// A step from its M function and arguments after the input table (and the step's culture).
fn step_from(func: &str, args: &[Expr]) -> Result<(Step, Option<String>), String> {
    let bad = || format!("`{func}` has arguments OpenDash BI doesn't support yet");
    let step = match (func, args) {
        ("Table.PromoteHeaders", []) => Step::PromotedHeaders,
        ("Table.PromoteHeaders", [options]) => {
            let f = fields(options, &["PromoteAllScalars"]).ok_or_else(bad)?;
            match field(&f, "PromoteAllScalars") {
                None | Some(Expr::Logical(true)) => Step::PromotedHeaders,
                _ => return Err(bad()),
            }
        }
        ("Table.TransformColumnTypes", [list, rest @ ..]) if rest.len() <= 1 => {
            let types = pairs(list)
                .and_then(|ps| {
                    ps.into_iter()
                        .map(|(c, t)| Some((text(c)?.to_string(), type_from(t)?)))
                        .collect::<Option<Vec<_>>>()
                })
                .ok_or_else(|| {
                    "column types must be `{\"Column\", type text}` pairs of text, number, \
                     Int64.Type, date, datetime or logical"
                        .to_string()
                })?;
            let culture = match rest {
                [] => None,
                [c] => Some(text(c).ok_or_else(bad)?.to_string()),
                _ => return Err(bad()),
            };
            return Ok((Step::ChangedType(types), culture));
        }
        ("Table.RenameColumns", [list]) => Step::RenamedColumns(
            pairs(list)
                .and_then(|ps| {
                    ps.into_iter()
                        .map(|(a, b)| Some((text(a)?.to_string(), text(b)?.to_string())))
                        .collect()
                })
                .ok_or_else(bad)?,
        ),
        ("Table.RemoveColumns", [list]) => Step::RemovedColumns(text_list(list).ok_or_else(bad)?),
        ("Table.ReorderColumns", [list]) => {
            Step::ReorderedColumns(text_list(list).ok_or_else(bad)?)
        }
        ("Table.SelectRows", [Expr::Each(body)]) => filter_from(body).ok_or_else(|| {
            "the filter must be comparisons of `[Column]` with a literal, or \
             `Text.Contains`/`StartsWith`/`EndsWith`, joined by all `and` or all `or`"
                .to_string()
        })?,
        ("Table.NestedJoin", args) => merge_from(args).ok_or_else(bad)?,
        // A known function with arguments outside the forms above.
        (
            "Table.PromoteHeaders"
            | "Table.TransformColumnTypes"
            | "Table.RenameColumns"
            | "Table.RemoveColumns"
            | "Table.ReorderColumns"
            | "Table.SelectRows",
            _,
        ) => return Err(bad()),
        _ => return Err(format!("`{func}` is not a step OpenDash BI supports yet")),
    };
    Ok((step, None))
}

/// Parses a Query's M. Errors name the line of the step at fault.
pub fn parse(src: &str) -> Result<MQuery, Error> {
    let Expr::Let(l) = opendash_m::parse(src)? else {
        return Err(Error::new(
            1,
            1,
            "a Query's M must be a `let … in` expression",
        ));
    };
    let at = |b: &Binding, m: String| Error::new(b.line, b.column, m);
    let mut bindings = l.bindings.iter().peekable();
    let Some(first) = bindings.next() else {
        return Err(Error::new(1, 1, "a Query needs a Source step"));
    };
    let mut source = source_from(&first.value).map_err(|e| match e {
        SourceError::Unsupported(m) => at(first, m),
        SourceError::Unknown => at(
            first,
            "the Source must be `Csv.Document`, `Parquet.Document` or `Excel.Workbook` over \
             `File.Contents(\"…\")`, with the options Power Query writes"
                .into(),
        ),
    })?;
    let mut prev = first;
    if let Source::Excel { item, kind, .. } = &mut source {
        let nav = bindings.next().ok_or_else(|| {
            at(
                first,
                "an Excel Source needs a `{[Item=…,Kind=…]}[Data]` step".into(),
            )
        })?;
        let (it, k) = navigation(&nav.value, &first.name).ok_or_else(|| {
            at(
                nav,
                format!(
                    "expected `{}{{[Item=\"…\",Kind=\"…\"]}}[Data]`",
                    opendash_m::quote_identifier(&first.name)
                ),
            )
        })?;
        *item = it;
        *kind = match k.as_str() {
            "Sheet" => ExcelKind::Sheet,
            "Table" => ExcelKind::Table,
            other => {
                return Err(at(
                    nav,
                    format!("Kind `{other}` is not supported: use \"Sheet\" or \"Table\""),
                ));
            }
        };
        prev = nav;
    }
    let mut steps = Vec::new();
    let mut culture: Option<&'static Culture> = None;
    while let Some(b) = bindings.next() {
        let (func, args) = call(&b.value)
            .ok_or_else(|| at(b, "expected a step like `Table.RenameColumns(…)`".into()))?;
        let rest = match args {
            [Expr::Ident(input), rest @ ..] if *input == prev.name => rest,
            _ => {
                return Err(at(
                    b,
                    format!(
                        "`{func}` must start from the previous step `{}`",
                        opendash_m::quote_identifier(&prev.name)
                    ),
                ));
            }
        };
        let (step, step_culture) = step_from(func, rest).map_err(|m| at(b, m))?;
        if let Some(name) = step_culture {
            let c = Culture::get(&name)
                .ok_or_else(|| at(b, format!("culture `{name}` is not supported yet")))?;
            if culture.is_some_and(|old| old != c) {
                return Err(at(
                    b,
                    "steps with different cultures are not supported yet".into(),
                ));
            }
            culture = Some(c);
        }
        prev = b;
        let step = match step {
            Step::MergedQueries(mut m) => {
                if let Some(next) = bindings.peek()
                    && let Some(expand) = expand_from(&next.value, &b.name, &m.column)
                {
                    m.expand = expand;
                    prev = next;
                    bindings.next();
                }
                Step::MergedQueries(m)
            }
            step => step,
        };
        steps.push(step);
    }
    if !matches!(l.body.as_ref(), Expr::Ident(n) if *n == prev.name) {
        return Err(at(
            prev,
            format!(
                "the `in` expression must name the last step `{}`",
                opendash_m::quote_identifier(&prev.name)
            ),
        ));
    }
    Ok(MQuery {
        source,
        steps,
        culture,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv() -> Source {
        Source::Csv {
            file: File::Path("C:\\Data\\sales.csv".into()),
            delimiter: b',',
            columns: 3,
            encoding: encoding_rs::UTF_8,
        }
    }

    fn query(source: Source, steps: Vec<Step>) -> MQuery {
        MQuery {
            source,
            steps,
            culture: None,
        }
    }

    fn round_trip(q: &MQuery) -> String {
        let m = print(q);
        let back = parse(&m).unwrap_or_else(|e| panic!("{m}\n{e}"));
        assert_eq!(&back, q, "{m}");
        assert_eq!(print(&back), m);
        m
    }

    #[test]
    fn typical_csv_query_prints_power_querys_m() {
        let q = query(
            csv(),
            vec![
                Step::PromotedHeaders,
                Step::ChangedType(vec![
                    ("Region".into(), ColumnType::Text),
                    ("Qty".into(), ColumnType::Whole),
                    ("Price".into(), ColumnType::Decimal),
                ]),
            ],
        );
        assert_eq!(
            round_trip(&q),
            r#"let
    Source = Csv.Document(File.Contents("C:\Data\sales.csv"),[Delimiter=",", Columns=3, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),
    #"Promoted Headers" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),
    #"Changed Type" = Table.TransformColumnTypes(#"Promoted Headers",{{"Region", type text}, {"Qty", Int64.Type}, {"Price", type number}})
in
    #"Changed Type""#
        );
    }

    #[test]
    fn every_step_round_trips() {
        let all_types = [
            ColumnType::Text,
            ColumnType::Decimal,
            ColumnType::Whole,
            ColumnType::Date,
            ColumnType::DateTime,
            ColumnType::Boolean,
        ];
        let steps = vec![
            Step::PromotedHeaders,
            Step::ChangedType(
                all_types
                    .iter()
                    .enumerate()
                    .map(|(i, t)| (format!("C{i}"), *t))
                    .collect(),
            ),
            Step::RenamedColumns(vec![("C0".into(), "Say \"hi\"\n#(x)".into())]),
            Step::RemovedColumns(vec!["C1".into(), "C2".into()]),
            Step::ReorderedColumns(vec!["C3".into(), "C0".into()]),
            filter(),
            Step::FilteredRows {
                combine: Combine::Or,
                conditions: vec![],
            },
            Step::FilteredRows {
                combine: Combine::And,
                conditions: vec![],
            },
            merge(vec![("Manager".into(), "Regions.Manager".into())]),
            merge(vec![]),
        ];
        for step in &steps {
            round_trip(&query(csv(), vec![step.clone()]));
        }
        let m = round_trip(&query(csv(), steps));
        assert!(
            m.contains(
                r##"Table.RenameColumns(#"Changed Type",{{"C0", "Say ""hi""#(lf)#(#)(x)"}})"##
            ),
            "{m}"
        );
        assert!(
            m.contains(r#"Table.RemoveColumns(#"Renamed Columns",{"C1", "C2"})"#),
            "{m}"
        );
        assert!(
            m.contains(r#"Table.ReorderColumns(#"Removed Columns",{"C3", "C0"})"#),
            "{m}"
        );
        assert!(m.contains(r#"{"C5", type logical}"#), "{m}");
    }

    fn filter() -> Step {
        let c = |column: &str, op, value| Condition {
            column: column.into(),
            op,
            value,
        };
        Step::FilteredRows {
            combine: Combine::Or,
            conditions: vec![
                c("Qty", Op::GreaterOrEqual, Value::Number(-2.5)),
                c("Unit Price", Op::Equals, Value::Null),
                c("Region", Op::BeginsWith, Value::Text("N\"o".into())),
                c("Day", Op::Less, Value::Date(20_000)),
                c("At", Op::NotEquals, Value::DateTime(1_700_000_000_123_456)),
                c("Ok", Op::Equals, Value::Logical(false)),
            ],
        }
    }

    fn merge(expand: Vec<(String, String)>) -> Step {
        Step::MergedQueries(Merge {
            keys: vec!["Region".into(), "Year".into()],
            query: "Region Targets".into(),
            other_keys: vec!["Region".into(), "Year".into()],
            kind: JoinKind::FullOuter,
            column: "Region Targets".into(),
            expand,
        })
    }

    #[test]
    fn filter_and_merge_print_power_querys_m() {
        let parquet = || Source::Parquet {
            file: File::Path("p".into()),
        };
        let m = round_trip(&query(
            parquet(),
            vec![
                merge(vec![("Manager".into(), "Region Targets.Manager".into())]),
                merge(vec![]),
                filter(),
            ],
        ));
        assert_eq!(
            m,
            r#"let
    Source = Parquet.Document(File.Contents("p")),
    #"Merged Queries" = Table.NestedJoin(Source, {"Region", "Year"}, #"Region Targets", {"Region", "Year"}, "Region Targets", JoinKind.FullOuter),
    #"Expanded Region Targets" = Table.ExpandTableColumn(#"Merged Queries", "Region Targets", {"Manager"}, {"Region Targets.Manager"}),
    #"Merged Queries1" = Table.NestedJoin(#"Expanded Region Targets", {"Region", "Year"}, #"Region Targets", {"Region", "Year"}, "Region Targets", JoinKind.FullOuter),
    #"Filtered Rows" = Table.SelectRows(#"Merged Queries1", each [Qty] >= -2.5 or [Unit Price] = null or Text.StartsWith([Region], "N""o") or [Day] < #date(2024, 10, 4) or [At] <> #datetime(2023, 11, 14, 22, 13, 20.123456) or [Ok] = false)
in
    #"Filtered Rows""#
        );
        let simple = Step::FilteredRows {
            combine: Combine::And,
            conditions: vec![Condition {
                column: "Region".into(),
                op: Op::Equals,
                value: Value::Text("North".into()),
            }],
        };
        let m = round_trip(&query(parquet(), vec![simple]));
        assert!(
            m.contains(r#"Table.SelectRows(Source, each ([Region] = "North"))"#),
            "{m}"
        );
        // Mixed and/or, and a filter on something other than a column, are errors.
        for bad in [
            "each [A] = 1 and [B] = 2 or [C] = 3",
            "each [A] = [B]",
            "each #date(2024, 2, 30) = [A]",
        ] {
            let src = m.replace(r#"each ([Region] = "North")"#, bad);
            let e = parse(&src).expect_err(&src);
            assert_eq!(e.line, 3, "{src}\n{e}");
        }
    }

    #[test]
    fn each_source_kind_round_trips() {
        round_trip(&query(
            Source::Csv {
                file: File::Path("a.tsv".into()),
                delimiter: b'\t',
                columns: 12,
                encoding: encoding_rs::WINDOWS_1252,
            },
            vec![],
        ));
        let m = round_trip(&query(
            Source::Parquet {
                file: File::Path("/data/sales.parquet".into()),
            },
            vec![Step::RemovedColumns(vec!["x".into()])],
        ));
        assert!(m.contains("Source = Parquet.Document(File.Contents(\"/data/sales.parquet\"))"));
        let m = round_trip(&query(
            Source::Excel {
                file: File::Path("Book.xlsx".into()),
                item: "Sheet1".into(),
                kind: ExcelKind::Sheet,
            },
            vec![Step::PromotedHeaders],
        ));
        assert_eq!(
            m,
            r#"let
    Source = Excel.Workbook(File.Contents("Book.xlsx"), null, true),
    Sheet1_Sheet = Source{[Item="Sheet1",Kind="Sheet"]}[Data],
    #"Promoted Headers" = Table.PromoteHeaders(Sheet1_Sheet, [PromoteAllScalars=true])
in
    #"Promoted Headers""#
        );
        let m = round_trip(&query(
            Source::Excel {
                file: File::Path("Book.xlsx".into()),
                item: "My Sales".into(),
                kind: ExcelKind::Table,
            },
            vec![],
        ));
        assert!(m.contains("#\"My Sales_Table\" = Source{"), "{m}");
    }

    #[test]
    fn repeated_steps_are_numbered_and_culture_prints() {
        let ct = |c: &str| Step::ChangedType(vec![(c.into(), ColumnType::Date)]);
        let q = MQuery {
            source: csv(),
            steps: vec![ct("A"), ct("B"), ct("C")],
            culture: Culture::get("en-GB"),
        };
        let m = round_trip(&q);
        assert!(m.contains(
            r#"#"Changed Type1" = Table.TransformColumnTypes(#"Changed Type", {{"B", type date}}, "en-GB")"#
        ), "{m}");
        assert!(m.contains(r#"#"Changed Type2" = "#), "{m}");
    }

    #[test]
    fn accepts_power_query_variants() {
        // Without the options record, extra whitespace, and a step renamed in Power Query.
        let q = parse(
            "let Source = Parquet.Document(File.Contents(\"p\")),\n  Headers = Table.PromoteHeaders( Source )\nin Headers",
        )
        .unwrap();
        assert_eq!(q.steps, vec![Step::PromotedHeaders]);
    }

    #[test]
    fn errors_name_the_line() {
        let base = print(&query(csv(), vec![Step::PromotedHeaders]));
        let cases = [
            // An unsupported step.
            (
                base.replace(
                    "Table.PromoteHeaders(Source, [PromoteAllScalars=true])",
                    "Table.Distinct(Source)",
                ),
                3,
                "Table.Distinct",
            ),
            // A step that doesn't start from the previous one.
            (
                base.replace("PromoteHeaders(Source", "PromoteHeaders(Other"),
                3,
                "previous step",
            ),
            (base.replace("Columns=3", "Columns=-3"), 2, "Source must be"),
            (
                base.replace("QuoteStyle.Csv", "QuoteStyle.None"),
                2,
                "QuoteStyle.None",
            ),
            (
                base.replace("Encoding=65001", "Encoding=932"),
                2,
                "codepage 932",
            ),
            (
                base.replace("Csv.Document", "Web.Contents"),
                2,
                "Source must be",
            ),
            (
                base.replace("in\n    #\"Promoted Headers\"", "in\n    Source"),
                3,
                "last step",
            ),
            (base.replace("true]", "true]x"), 3, "expected"),
            ("1 + 2".into(), 1, "let"),
            (
                base.replace("[PromoteAllScalars=true]", "{\"x\"}, 1"),
                3,
                "arguments",
            ),
        ];
        for (src, line, needle) in cases {
            let e = parse(&src).expect_err(&src);
            assert_eq!(e.line, line, "{src}\n{e}");
            assert!(e.message.contains(needle), "{src}\n{e}");
        }
        let typed = print(&query(
            csv(),
            vec![Step::ChangedType(vec![("A".into(), ColumnType::Text)])],
        ));
        let e = parse(&typed.replace("type text", "Currency.Type")).expect_err("currency");
        assert_eq!(e.line, 3);
        let e = parse(
            &typed
                .replace(",{{", ", {{")
                .replace("}})", "}}, \"xx-XX\")"),
        )
        .expect_err("culture");
        assert!(e.message.contains("xx-XX"), "{e}");
    }
}
