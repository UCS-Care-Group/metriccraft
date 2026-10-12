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
//! - steps renamed in Power Query lose their names here (a [`Step`] has no name of its own).

use std::collections::HashSet;

use opendash_m::{Binding, Error, Expr, Let};

use crate::culture::Culture;
use crate::step::{ColumnType, Step};

/// Where a Query's data comes from, as its M `Source` step describes it.
// ponytail: M1.8 makes this the Query's real source (today `Query::source` is an in-memory table).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// `Csv.Document(File.Contents(path),[Delimiter=",", Columns=N, Encoding=65001, QuoteStyle=…])`.
    Csv {
        path: String,
        delimiter: u8,
        columns: usize,
        /// Windows codepage: 65001 UTF-8, 1252 Western, 1200 UTF-16LE, 1201 UTF-16BE.
        encoding: u32,
        /// `QuoteStyle.Csv` (quoted cells may hold line breaks) vs `QuoteStyle.None`.
        quotes: bool,
    },
    /// `Parquet.Document(File.Contents(path))`.
    Parquet { path: String },
    /// `Excel.Workbook(File.Contents(path), null, true)`, then the `{[Item=…,Kind=…]}[Data]`
    /// navigation step Power Query names `Item_Kind`.
    Excel {
        path: String,
        item: String,
        kind: String,
    },
}

/// A Query as M describes it: Source, Applied Steps and the culture Changed Type steps use when
/// it isn't the Project's.
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

/// A step's M: the call that turns `prev` (the previous step) into its result.
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
    }
}

/// Prints a Query as Power Query's M (no trailing newline).
pub fn print(q: &MQuery) -> String {
    let mut bindings = Vec::new();
    let source = match &q.source {
        Source::Csv {
            path,
            delimiter,
            columns,
            encoding,
            quotes,
        } => {
            let options = Expr::record(
                vec![
                    ("Delimiter", Expr::text(char::from(*delimiter))),
                    ("Columns", Expr::Number(*columns as f64)),
                    ("Encoding", Expr::Number(f64::from(*encoding))),
                    (
                        "QuoteStyle",
                        Expr::ident(if *quotes {
                            "QuoteStyle.Csv"
                        } else {
                            "QuoteStyle.None"
                        }),
                    ),
                ],
                false,
            );
            Expr::call("Csv.Document", vec![file_contents(path), options], true)
        }
        Source::Parquet { path } => {
            Expr::call("Parquet.Document", vec![file_contents(path)], false)
        }
        Source::Excel { path, .. } => Expr::call(
            "Excel.Workbook",
            vec![file_contents(path), Expr::Null, Expr::Logical(true)],
            false,
        ),
    };
    bindings.push(Binding::new("Source", source));
    if let Source::Excel { item, kind, .. } = &q.source {
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
        // Power Query numbers repeats: `Changed Type`, `Changed Type1`, `Changed Type2`…
        let mut name = step.name().to_string();
        let mut n = 0u32;
        while used.contains(&name) {
            n = n.saturating_add(1);
            name = format!("{}{n}", step.name());
        }
        used.insert(name.clone());
        bindings.push(Binding::new(
            name,
            step_m(step, Expr::Ident(prev), q.culture),
        ));
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

fn csv_source(path: String, options: &Expr) -> Option<Source> {
    let f = fields(options, &["Delimiter", "Columns", "Encoding", "QuoteStyle"])?;
    let delimiter = text(field(&f, "Delimiter")?)?;
    let mut chars = delimiter.chars();
    let delimiter = match (chars.next(), chars.next()) {
        (Some(c), None) => u8::try_from(u32::from(c)).ok()?,
        _ => return None,
    };
    let quotes = match field(&f, "QuoteStyle")? {
        Expr::Ident(q) if q == "QuoteStyle.Csv" => true,
        Expr::Ident(q) if q == "QuoteStyle.None" => false,
        _ => return None,
    };
    Some(Source::Csv {
        path,
        delimiter,
        columns: usize::try_from(whole(field(&f, "Columns")?)?).ok()?,
        encoding: whole(field(&f, "Encoding")?)?,
        quotes,
    })
}

fn source_from(e: &Expr) -> Option<Source> {
    match call(e)? {
        ("Csv.Document", [file, options]) => csv_source(file_path(file)?, options),
        ("Parquet.Document", [file]) => Some(Source::Parquet {
            path: file_path(file)?,
        }),
        ("Excel.Workbook", [file, Expr::Null, Expr::Logical(true)]) => Some(Source::Excel {
            path: file_path(file)?,
            item: String::new(),
            kind: String::new(),
        }),
        _ => None,
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
        // A known function with arguments outside the forms above.
        (
            "Table.PromoteHeaders"
            | "Table.TransformColumnTypes"
            | "Table.RenameColumns"
            | "Table.RemoveColumns"
            | "Table.ReorderColumns",
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
    let mut bindings = l.bindings.iter();
    let Some(first) = bindings.next() else {
        return Err(Error::new(1, 1, "a Query needs a Source step"));
    };
    let mut source = source_from(&first.value).ok_or_else(|| {
        at(
            first,
            "the Source must be `Csv.Document`, `Parquet.Document` or `Excel.Workbook` over \
             `File.Contents(\"…\")`, with the options Power Query writes"
                .into(),
        )
    })?;
    let mut prev = first;
    if let Source::Excel { item, kind, .. } = &mut source {
        let nav = bindings.next().ok_or_else(|| {
            at(
                first,
                "an Excel Source needs a `{[Item=…,Kind=…]}[Data]` step".into(),
            )
        })?;
        (*item, *kind) = navigation(&nav.value, &first.name).ok_or_else(|| {
            at(
                nav,
                format!(
                    "expected `{}{{[Item=\"…\",Kind=\"…\"]}}[Data]`",
                    opendash_m::quote_identifier(&first.name)
                ),
            )
        })?;
        prev = nav;
    }
    let mut steps = Vec::new();
    let mut culture: Option<&'static Culture> = None;
    for b in bindings {
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
        steps.push(step);
        prev = b;
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
            path: "C:\\Data\\sales.csv".into(),
            delimiter: b',',
            columns: 3,
            encoding: 65001,
            quotes: true,
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

    #[test]
    fn each_source_kind_round_trips() {
        round_trip(&query(
            Source::Csv {
                path: "a.tsv".into(),
                delimiter: b'\t',
                columns: 12,
                encoding: 1252,
                quotes: false,
            },
            vec![],
        ));
        let m = round_trip(&query(
            Source::Parquet {
                path: "/data/sales.parquet".into(),
            },
            vec![Step::RemovedColumns(vec!["x".into()])],
        ));
        assert!(m.contains("Source = Parquet.Document(File.Contents(\"/data/sales.parquet\"))"));
        let m = round_trip(&query(
            Source::Excel {
                path: "Book.xlsx".into(),
                item: "Sheet1".into(),
                kind: "Sheet".into(),
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
                path: "Book.xlsx".into(),
                item: "My Sales".into(),
                kind: "Table".into(),
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
