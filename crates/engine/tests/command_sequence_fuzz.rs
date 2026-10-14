//! Command-sequence fuzzer (spec N-1, F-2, D-4, R-1; plan M2.5, M4.2): random, valid-ish command sequences
//! over a small Document of in-memory CSV Queries, checking after every command that
//!
//! - (a) nothing panics;
//! - (b) a command that fails leaves the Document unchanged;
//! - (d) the Document is consistent (see [`check`]), Measures and Calculated columns included,
//!   and the report: Pages and Visuals (see [`check_report`]);
//! - (f) a rename never breaks a Field of the report (it rewrites them);
//!
//! and after each sequence that
//!
//! - (c) undo-all gives back the original Document and redo-all the final one;
//! - (e) save → open gives the same definitions (see [`round_trip`]), for the final Document
//!   and the original.
//!
//! Seeded and bounded: `ODASH_FUZZ_SCALE` (default 1) multiplies the number of sequences;
//! `ODASH_FUZZ_SEED` replays one. Each sequence is at most [`STEPS`] commands (well under
//! `UNDO_LIMIT`, so undo-all always reaches the start) over three tables of a few rows.

// In a `cfg(test)` module so clippy treats it as test code (unwrap and panic allowed).
#[cfg(test)]
mod fuzz {

    use std::collections::{BTreeMap, BTreeSet};

    use opendash_data::Query;
    use opendash_data::culture::Culture;
    use opendash_data::source::{Bytes, File, Source, csv};
    use opendash_data::step::Step;
    use opendash_engine::Page;
    use opendash_engine::{App, Document, UNDO_LIMIT};
    use opendash_file::{Preserved, Project};
    use opendash_model::{ModelTable, same_name};
    use opendash_visuals::{FieldRef, Visual, VisualType, Well};
    use serde_json::{Value, json};

    /// Commands per sequence.
    const STEPS: usize = 40;
    /// Sequences at scale 1.
    const SEEDS: u64 = 24;

    const SOURCES: [(&str, &str); 3] = [
        (
            "Sales",
            "Region,Product,Qty,Price\nNorth,Pen,3,1.5\nSouth,Ink,4,2\nNorth,Ink,,x\nEast,Pen,5,0.5\n",
        ),
        (
            "Regions",
            "Region,Manager,Rank\nNorth,Ann,1\nSouth,Bob,2\nEast,Cy,3\n",
        ),
        ("Products", "Product,Colour,Code\nPen,Blue,1\nInk,Black,2\n"),
    ];

    /// SplitMix64: small, seedable, no dependency.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        /// 0..n (0 when n is 0).
        fn below(&mut self, n: usize) -> usize {
            if n == 0 {
                0
            } else {
                (self.next() % n as u64) as usize
            }
        }

        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
            items.get(self.below(items.len()))
        }
    }

    /// The starting Document: each source connected as Get Data does (automatic steps), loaded,
    /// with its model; no Relationships.
    fn document() -> Document {
        let mut doc = Document::default();
        for (name, text) in SOURCES {
            let file = File::Bytes {
                name: format!("{name}.csv"),
                bytes: Bytes::from_static(text.as_bytes()),
            };
            let culture = Culture::get("en-US").unwrap();
            let source = Source::csv(name, file, csv::Options::default()).unwrap();
            let query = Query::connect(name, source, culture, usize::MAX).unwrap();
            let (table, _) = query.run(usize::MAX).unwrap();
            let (model, table) = ModelTable::load(None, table, culture, usize::MAX).unwrap();
            doc.queries.push(query);
            doc.tables.push(table);
            doc.model.put(model);
        }
        doc.report = report();
        doc
    }

    /// The starting report: two Pages with a few Visuals and Fields, so the report commands
    /// have something to act on from the first step.
    fn report() -> opendash_engine::Report {
        let column = |t: &str, c: &str| FieldRef::Column {
            table: t.into(),
            column: c.into(),
        };
        let mut overview = Page::new("p1", "Overview");
        let mut table = Visual::new("v1", VisualType::Table);
        table.wells.insert(
            Well::Values,
            vec![column("Sales", "Region"), column("Sales", "Qty")],
        );
        let mut chart = Visual::new("v2", VisualType::ClusteredColumn);
        chart
            .wells
            .insert(Well::Axis, vec![column("Sales", "Product")]);
        chart
            .wells
            .insert(Well::Values, vec![column("Sales", "Qty")]);
        chart.z = 1;
        let mut slicer = Visual::new("v3", VisualType::Slicer);
        slicer
            .wells
            .insert(Well::Field, vec![column("Regions", "Region")]);
        slicer.z = 2;
        overview.visuals = vec![table, chart, slicer];
        overview
            .selections
            .insert("v3".into(), vec![json!("North")]);
        let mut detail = Page::new("p2", "Detail");
        let mut matrix = Visual::new("v4", VisualType::Matrix);
        matrix
            .wells
            .insert(Well::Rows, vec![column("Products", "Product")]);
        matrix
            .wells
            .insert(Well::Values, vec![column("Products", "Code")]);
        detail.visuals = vec![matrix];
        let mut report = opendash_engine::Report::default();
        report.pages = vec![overview, detail];
        report
    }

    /// Names a command may be given: mostly real, sometimes not.
    struct Names<'a> {
        app: &'a App,
    }

    impl Names<'_> {
        fn query(&self, rng: &mut Rng) -> String {
            let doc = self.app.document();
            let names: Vec<&str> = doc.queries.iter().map(|q| q.name.as_str()).collect();
            self.real_or_not(rng, &names)
        }

        fn table(&self, rng: &mut Rng) -> String {
            let doc = self.app.document();
            let names: Vec<&str> = doc.tables.iter().map(|t| t.name()).collect();
            self.real_or_not(rng, &names)
        }

        /// A column of Query `query` as its steps now give it (its last step's Preview), or of
        /// model Table `query`.
        fn column(&self, rng: &mut Rng, query: &str) -> String {
            let mut names: Vec<String> = Vec::new();
            if let Some(q) = self.app.document().queries.iter().find(|q| q.name == query)
                && let Ok(p) = self.app.preview(query, q.steps.len().checked_sub(1))
            {
                names.extend(p.batch.schema().fields().iter().map(|f| f.name().clone()));
            }
            if let Some(t) = self.app.document().model.table(query) {
                names.extend(t.columns.iter().map(|c| c.name.clone()));
            }
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.real_or_not(rng, &refs)
        }

        /// A Measure or Calculated column of Table `table`, or a name like one.
        fn expression_object(&self, rng: &mut Rng, table: &str) -> String {
            let mut names: Vec<String> = Vec::new();
            if let Some(t) = self.app.document().model.table(table) {
                names.extend(t.measures.iter().map(|m| m.name.clone()));
                names.extend(
                    t.columns
                        .iter()
                        .filter(|c| c.is_calculated())
                        .map(|c| c.name.clone()),
                );
            }
            if names.is_empty() || rng.chance(15) {
                return EXPRESSION_NAMES[rng.below(EXPRESSION_NAMES.len())].into();
            }
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            self.real_or_not(rng, &refs)
        }

        fn real_or_not(&self, rng: &mut Rng, names: &[&str]) -> String {
            match (rng.below(20), rng.pick(names)) {
                (0, _) | (_, None) => "Nope".into(),
                (1, Some(n)) => n.to_uppercase(),
                (2, Some(n)) => format!("{n} "),
                (_, Some(n)) => (*n).to_string(),
            }
        }

        /// A new name: fresh, existing (a collision) or bad.
        fn new_name(&self, rng: &mut Rng, existing: &str) -> String {
            match rng.below(12) {
                0 => existing.into(),
                1 => existing.to_lowercase(),
                2 => String::new(),
                3 => " Padded".into(),
                4 => "Sales".into(),
                5 => "Region".into(),
                _ => format!("N{}", rng.below(6)),
            }
        }
    }

    const TYPES: [&str; 8] = [
        "text",
        "decimal",
        "fixedDecimal",
        "whole",
        "date",
        "datetime",
        "boolean",
        "money",
    ];

    /// Names for new Measures and Calculated columns (some collide with Columns).
    const EXPRESSION_NAMES: [&str; 7] = ["M1", "M2", "C1", "C2", "Total", "Qty", "m1"];

    /// DAX for them: arithmetic over Columns, Measures and each other (so cycles and chains
    /// come up), a Table function, and text that doesn't parse.
    const EXPRESSIONS: [&str; 12] = [
        "[Qty] * 2",
        "COUNTROWS ( Sales ) // rows",
        "[M1] + 1",
        "[M2] * [M1]",
        "Sales[Qty] + [C1]",
        "[C2] - 1",
        "[C1] & \"!\"",
        "'Regions'[Rank] + 0",
        "[Region] & [Manager]",
        "SUM((",
        "1 / 0",
        "[Total]",
    ];

    const VISUAL_TYPES: [&str; 8] = [
        "table",
        "matrix",
        "card",
        "clusteredBar",
        "clusteredColumn",
        "line",
        "slicer",
        "pie",
    ];

    const WELLS: [&str; 7] = ["values", "rows", "columns", "axis", "legend", "field", "x"];

    type FormatValue = fn(&mut Rng) -> Value;

    const FORMAT: [(&str, FormatValue); 8] = [
        ("title", |r| {
            if r.chance(30) {
                Value::Null
            } else {
                json!("T")
            }
        }),
        ("showTitle", |r| json!(r.chance(50))),
        ("dataLabels", |r| json!(r.chance(50))),
        ("legend", |r| json!(r.chance(50))),
        ("legendPosition", |r| {
            json!(["top", "right", "up"][r.below(3)])
        }),
        ("rowSubtotals", |r| json!(r.chance(50))),
        ("slicerStyle", |r| json!(["list", "dropdown"][r.below(2)])),
        ("nope", |_| json!(1)),
    ];

    /// Report params: a Page (id, name or not), a Visual of it, a Field.
    struct Report<'a> {
        app: &'a App,
    }

    impl Report<'_> {
        fn page(&self, rng: &mut Rng) -> String {
            let pages = &self.app.document().report.pages;
            match (rng.below(12), rng.pick(pages)) {
                (0, _) | (_, None) => "Nope".into(),
                (1 | 2, Some(p)) => p.name.clone(),
                (_, Some(p)) => p.id.clone(),
            }
        }

        fn visual(&self, rng: &mut Rng, page: &str) -> String {
            let report = &self.app.document().report;
            let visuals = report.page(page).map_or(&[][..], |p| p.visuals.as_slice());
            match (rng.below(12), rng.pick(visuals)) {
                (0, _) | (_, None) => "nope".into(),
                (_, Some(v)) => v.id.clone(),
            }
        }

        fn field(&self, rng: &mut Rng, n: &Names) -> Value {
            if rng.chance(25) {
                json!({"measure": EXPRESSION_NAMES[rng.below(EXPRESSION_NAMES.len())]})
            } else {
                let t = n.table(rng);
                json!({"table": t, "column": n.column(rng, &t)})
            }
        }

        /// A Field already in the Visual, mostly.
        fn its_field(&self, rng: &mut Rng, n: &Names, page: &str, visual: &str) -> Value {
            let report = &self.app.document().report;
            let v = report.page(page).and_then(|p| p.visual(visual));
            let fields: Vec<&FieldRef> =
                v.map_or(Vec::new(), |v| v.wells.values().flatten().collect());
            match rng.pick(&fields) {
                Some(FieldRef::Column { table, column }) if rng.chance(85) => {
                    json!({"table": table, "column": column})
                }
                Some(FieldRef::Measure { name }) if rng.chance(85) => json!({"measure": name}),
                _ => self.field(rng, n),
            }
        }
    }

    /// One random report command and its params.
    fn report_command(rng: &mut Rng, app: &App, n: &Names) -> (&'static str, Value) {
        let r = Report { app };
        let page = r.page(rng);
        let visual = r.visual(rng, &page);
        let at = |rng: &mut Rng| json!(rng.below(4));
        // Mostly a well the Visual's type has.
        let v = app
            .document()
            .report
            .page(&page)
            .and_then(|p| p.visual(&visual));
        let well = |rng: &mut Rng| match v {
            Some(v) if rng.chance(75) => rng.pick(v.kind.wells()).map_or("values", |w| w.id()),
            _ => WELLS[rng.below(WELLS.len())],
        };
        // Mostly a well that has Fields, and a position in it.
        let filled: Vec<(Well, usize)> = v.map_or(Vec::new(), |v| {
            v.wells.iter().map(|(w, f)| (*w, f.len())).collect()
        });
        let from = |rng: &mut Rng| match rng.pick(&filled) {
            Some((w, len)) if rng.chance(80) => (w.id(), json!(rng.below(*len))),
            _ => (well(rng), at(rng)),
        };
        match rng.below(16) {
            0 => {
                let name = if rng.chance(50) {
                    Value::Null
                } else {
                    json!(format!("P{}", rng.below(4)))
                };
                ("insert.newPage", json!({"name": name}))
            }
            1 => ("page.delete", json!({"page": page})),
            2 => (
                "page.rename",
                json!({"page": page, "name": format!("P{}", rng.below(4))}),
            ),
            3 => ("page.move", json!({"page": page, "index": at(rng)})),
            4 | 5 => {
                let mut p = json!({"type": VISUAL_TYPES[rng.below(VISUAL_TYPES.len())]});
                if rng.chance(70) {
                    p["page"] = json!(page);
                }
                if rng.chance(30) {
                    p["x"] = json!(rng.below(2000));
                    p["w"] = json!(rng.below(800));
                }
                ("insert.newVisual", p)
            }
            6 => ("visual.delete", json!({"page": page, "visual": visual})),
            7 => (
                "visual.move",
                json!({"page": page, "visual": visual, "x": rng.below(1500), "y": rng.below(900)}),
            ),
            8 => (
                "visual.resize",
                json!({"page": page, "visual": visual, "w": rng.below(900), "h": rng.below(900)}),
            ),
            9 => (
                "visual.changeType",
                json!({"page": page, "visual": visual,
                    "type": VISUAL_TYPES[rng.below(VISUAL_TYPES.len())]}),
            ),
            10 | 11 => {
                let mut p = json!({"page": page, "visual": visual, "well": well(rng),
                    "field": r.field(rng, n)});
                if rng.chance(20) {
                    p["index"] = at(rng);
                }
                ("visual.addField", p)
            }
            12 => {
                let (w, i) = from(rng);
                let mut p = json!({"page": page, "visual": visual, "well": w});
                if rng.chance(50) {
                    p["index"] = i;
                } else {
                    p["field"] = r.its_field(rng, n, &page, &visual);
                }
                ("visual.removeField", p)
            }
            13 => {
                let (w, i) = from(rng);
                let mut p = json!({"page": page, "visual": visual, "well": w, "index": i});
                if rng.chance(60) {
                    p["toWell"] = json!(well(rng));
                }
                if rng.chance(40) {
                    p["toIndex"] = at(rng);
                }
                ("visual.moveField", p)
            }
            14 => {
                let field = if rng.chance(20) {
                    Value::Null
                } else {
                    r.its_field(rng, n, &page, &visual)
                };
                let direction = ["ascending", "descending", "sideways"][rng.below(3)];
                (
                    "visual.sort",
                    json!({"page": page, "visual": visual, "field": field,
                        "direction": direction}),
                )
            }
            _ => {
                let (option, value) = FORMAT[rng.below(FORMAT.len())];
                (
                    "visual.format",
                    json!({"page": page, "visual": visual, "option": option,
                        "value": value(rng)}),
                )
            }
        }
    }

    /// One random command and its params.
    fn command(rng: &mut Rng, app: &App) -> (&'static str, Value) {
        let n = Names { app };
        let q = n.query(rng);
        let t = n.table(rng);
        let rel = |rng: &mut Rng| -> Value {
            let count = app.document().model.relationships.len();
            json!(rng.below(count.saturating_add(1)))
        };
        let new_rel = |rng: &mut Rng, n: &Names| {
            let a = n.table(rng);
            let b = n.table(rng);
            json!({"fromTable": a, "fromColumn": n.column(rng, &a),
                   "toTable": b, "toColumn": n.column(rng, &b)})
        };
        let dax = |rng: &mut Rng| EXPRESSIONS[rng.below(EXPRESSIONS.len())];
        if rng.chance(35) {
            return report_command(rng, app, &n);
        }
        match rng.below(37) {
            0 => ("transform.useFirstRowAsHeaders", json!({"query": q})),
            1 => (
                "transform.changeType",
                json!({"query": q, "columns": [n.column(rng, &q)], "type": rng.pick(&TYPES)}),
            ),
            2 => {
                let c = n.column(rng, &q);
                let name = n.new_name(rng, &c);
                (
                    "transform.renameColumn",
                    json!({"query": q, "column": c, "name": name}),
                )
            }
            3 => (
                "home.removeColumns",
                json!({"query": q, "columns": [n.column(rng, &q)]}),
            ),
            4 => (
                "transform.reorderColumns",
                json!({"query": q, "columns": [n.column(rng, &q), n.column(rng, &q)]}),
            ),
            5 => {
                let ops = ["equals", "notEquals", "greater", "contains", "beginsWith"];
                let value = match rng.below(4) {
                    0 => Value::Null,
                    1 => json!(rng.below(5)),
                    2 => json!("North"),
                    _ => json!("2026-01-31"),
                };
                (
                    "transform.filterRows",
                    json!({"query": q, "conditions": [
                        {"column": n.column(rng, &q), "op": rng.pick(&ops), "value": value}
                    ], "combine": if rng.chance(50) { "and" } else { "or" }}),
                )
            }
            6 => {
                let with = n.query(rng);
                let mut p = json!({"query": q, "keys": [n.column(rng, &q)], "with": with,
                    "withKeys": [n.column(rng, &with)],
                    "kind": if rng.chance(70) { "leftOuter" } else { "inner" }});
                if rng.chance(50) {
                    p["expand"] = json!([n.column(rng, &with)]);
                }
                ("home.mergeQueries", p)
            }
            7 => {
                let steps = app
                    .document()
                    .queries
                    .iter()
                    .find(|x| x.name == q)
                    .map_or(0, |x| x.steps.len());
                (
                    "query.deleteStep",
                    json!({"query": q, "step": rng.below(steps.saturating_add(1))}),
                )
            }
            8 => ("query.enableLoad", json!({"query": q})),
            9 => (
                "columnTools.dataType",
                json!({"table": t, "column": n.column(rng, &t), "type": rng.pick(&TYPES)}),
            ),
            10 => (
                "columnTools.format",
                json!({"table": t, "column": n.column(rng, &t), "formatString": "#,0.00"}),
            ),
            11 => {
                let by = ["none", "sum", "average", "count", "distinctCount"];
                (
                    "columnTools.summarization",
                    json!({"table": t, "column": n.column(rng, &t), "summarizeBy": rng.pick(&by)}),
                )
            }
            12 => {
                let by = if rng.chance(20) {
                    Value::Null
                } else {
                    json!(n.column(rng, &t))
                };
                (
                    "columnTools.sortByColumn",
                    json!({"table": t, "column": n.column(rng, &t), "by": by}),
                )
            }
            13 => (
                "model.setHidden",
                json!({"table": t, "column": n.column(rng, &t), "hidden": rng.chance(50)}),
            ),
            14 => (
                "model.setDescription",
                json!({"table": t, "description": "about it"}),
            ),
            15 | 16 => ("modeling.newRelationship", new_rel(rng, &n)),
            17 => {
                let mut p = new_rel(rng, &n);
                p["relationship"] = rel(rng);
                ("modeling.editRelationship", p)
            }
            18 => (
                "modeling.deleteRelationship",
                json!({"relationship": rel(rng)}),
            ),
            19 => (
                "modeling.setRelationshipActive",
                json!({"relationship": rel(rng), "active": rng.chance(50)}),
            ),
            20 => ("modeling.autodetectRelationships", Value::Null),
            21 => ("options.autodetectRelationships", json!({})),
            22 => {
                let name = n.new_name(rng, &q);
                ("query.rename", json!({"query": q, "name": name}))
            }
            23 => {
                let name = n.new_name(rng, &t);
                ("modeling.renameTable", json!({"table": t, "name": name}))
            }
            24 | 25 => {
                let c = n.column(rng, &t);
                let name = n.new_name(rng, &c);
                (
                    "modeling.renameColumn",
                    json!({"table": t, "column": c, "name": name}),
                )
            }
            26 => {
                if rng.chance(50) {
                    ("query.delete", json!({"query": q}))
                } else {
                    ("modeling.deleteTable", json!({"table": t}))
                }
            }
            27 => ("query.duplicate", json!({"query": q})),
            28 => ("home.closeApply", Value::Null),
            29 => ("home.refresh", Value::Null),
            30 => (
                "modeling.newMeasure",
                json!({"table": t, "name": EXPRESSION_NAMES[rng.below(EXPRESSION_NAMES.len())],
                    "expression": dax(rng)}),
            ),
            31 | 32 => (
                "modeling.newColumn",
                json!({"table": t, "name": EXPRESSION_NAMES[rng.below(EXPRESSION_NAMES.len())],
                    "expression": dax(rng)}),
            ),
            33 => (
                "modeling.editExpression",
                json!({"table": t, "name": n.expression_object(rng, &t), "expression": dax(rng)}),
            ),
            34 => (
                "modeling.delete",
                json!({"table": t, "name": n.expression_object(rng, &t)}),
            ),
            35 => {
                let m = n.expression_object(rng, &t);
                let name = n.new_name(rng, &m);
                (
                    "modeling.renameMeasure",
                    json!({"table": t, "measure": m, "name": name}),
                )
            }
            _ => {
                if rng.chance(50) {
                    ("edit.undo", Value::Null)
                } else {
                    ("edit.redo", Value::Null)
                }
            }
        }
    }

    /// (d), the report: unique ids and Page names, Visuals sorted by id, each Visual's Fields in
    /// wells its type has, within their limits, Measures only in Values, a chart with a Legend
    /// or several Values (not both), a sort by one of its Fields, no empty wells.
    fn check_report(doc: &Document) -> Result<(), String> {
        let pages = &doc.report.pages;
        for (i, p) in pages.iter().enumerate() {
            if pages
                .iter()
                .skip(i + 1)
                .any(|q| q.id == p.id || same_name(&q.name, &p.name))
            {
                return Err(format!("page `{}` / `{}` twice", p.id, p.name));
            }
            if !p.visuals.windows(2).all(|w| w[0].id < w[1].id) {
                return Err(format!("page `{}`: visuals not sorted by unique id", p.id));
            }
            for v in &p.visuals {
                let at = format!("visual `{}` ({})", v.id, v.kind.id());
                for (w, fields) in &v.wells {
                    if !v.kind.wells().contains(w) || fields.is_empty() {
                        return Err(format!("{at}: well {w:?} {fields:?}"));
                    }
                    if fields.len() > v.kind.max_fields(*w) {
                        return Err(format!("{at}: too many in {w:?}"));
                    }
                    let measure = |f: &FieldRef| matches!(f, FieldRef::Measure { .. });
                    if *w != Well::Values && fields.iter().any(measure) {
                        return Err(format!("{at}: a measure in {w:?}"));
                    }
                }
                let chart = matches!(
                    v.kind,
                    VisualType::ClusteredBar | VisualType::ClusteredColumn | VisualType::Line
                );
                if chart && v.wells.contains_key(&Well::Legend) && v.fields(Well::Values).len() > 1
                {
                    return Err(format!("{at}: a legend and several values"));
                }
                if let Some(s) = &v.sort
                    && !v.wells.values().flatten().any(|f| *f == s.field)
                {
                    return Err(format!("{at}: sorted by a field not in it"));
                }
            }
        }
        Ok(())
    }

    /// (d): the Document's parts agree with each other. Returns what doesn't.
    fn check(doc: &Document) -> Result<(), String> {
        check_report(doc)?;
        let queries: Vec<&str> = doc.queries.iter().map(|q| q.name.as_str()).collect();
        for (i, a) in queries.iter().enumerate() {
            if queries.iter().skip(i + 1).any(|b| same_name(a, b)) {
                return Err(format!("two queries named `{a}`"));
            }
        }
        for set in [&doc.stale, &doc.unloaded] {
            if let Some(n) = set.iter().find(|n| !queries.contains(&n.as_str())) {
                return Err(format!("flag on `{n}`, not a query"));
            }
        }
        for q in &doc.queries {
            for s in &q.steps {
                if let Step::MergedQueries(m) = s
                    && !queries.contains(&m.query.as_str())
                {
                    return Err(format!("`{}` merges missing `{}`", q.name, m.query));
                }
            }
            let loaded = doc.tables.iter().any(|t| t.name() == q.name);
            let unloaded = doc.unloaded.contains(&q.name);
            if loaded == unloaded && !doc.stale.contains(&q.name) {
                return Err(format!(
                    "`{}`: table/enable load disagree, not Stale",
                    q.name
                ));
            }
        }
        let tables: BTreeSet<&str> = doc.tables.iter().map(|t| t.name()).collect();
        let models: BTreeSet<&str> = doc.model.tables.iter().map(|t| t.name.as_str()).collect();
        if tables != models || tables.len() != doc.tables.len() {
            return Err(format!("tables {tables:?} vs model {models:?}"));
        }
        for t in &doc.tables {
            if !queries.contains(&t.name()) {
                return Err(format!("table `{}` has no query", t.name()));
            }
            let schema = t.batch().schema();
            let data: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
            let Some(m) = doc.model.table(t.name()) else {
                return Err(format!("no model for `{}`", t.name()));
            };
            let model: Vec<&str> = m.columns.iter().map(|c| c.name.as_str()).collect();
            if data != model {
                return Err(format!("`{}`: data {data:?} vs model {model:?}", t.name()));
            }
            for (i, c) in m.columns.iter().enumerate() {
                if c.is_calculated() && m.columns.iter().skip(i).any(|x| !x.is_calculated()) {
                    return Err(format!(
                        "`{}`[{}]: a Calculated column before a loaded one",
                        m.name, c.name
                    ));
                }
                if m.measures.iter().any(|x| same_name(&x.name, &c.name)) {
                    return Err(format!("`{}`[{}] is also a Measure", m.name, c.name));
                }
            }
            for c in &m.columns {
                if let Some(by) = &c.sort_by
                    && (by == &c.name || m.column(by).is_none())
                {
                    return Err(format!("`{}`[{}] sorts by `{by}`", m.name, c.name));
                }
            }
        }
        let measures: Vec<&str> = doc
            .model
            .tables
            .iter()
            .flat_map(|t| t.measures.iter().map(|m| m.name.as_str()))
            .collect();
        for (i, a) in measures.iter().enumerate() {
            if measures.iter().skip(i + 1).any(|b| same_name(a, b)) {
                return Err(format!("two Measures named `{a}`"));
            }
        }
        for r in &doc.model.relationships {
            let has = |t: &str, c: &str| doc.model.table(t).is_some_and(|t| t.column(c).is_some());
            if !has(&r.from_table, &r.from_column) || !has(&r.to_table, &r.to_column) {
                return Err(format!("relationship {r} points at a missing column"));
            }
            if r.from_table == r.to_table {
                return Err(format!("relationship {r} links a table to itself"));
            }
        }
        Ok(())
    }

    /// A loaded Table that isn't Stale has the columns its Query gives now (its last step's
    /// Preview, when that runs): e.g. a model column rename, which adds a step without making the
    /// Table Stale, must leave the two agreeing.
    fn fresh_tables_match_their_queries(app: &App) -> Result<(), String> {
        let doc = app.document();
        for t in doc.tables.iter().filter(|t| !doc.stale.contains(t.name())) {
            let Some(q) = doc.queries.iter().find(|q| q.name == t.name()) else {
                continue;
            };
            let Ok(p) = app.preview(&q.name, q.steps.len().checked_sub(1)) else {
                continue;
            };
            let want: Vec<String> = p
                .batch
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .collect();
            // Its Calculated columns aren't the Query's.
            let calculated = |c: &str| {
                doc.model
                    .table(t.name())
                    .and_then(|m| m.column(c))
                    .is_some_and(|c| c.is_calculated())
            };
            let got: Vec<String> = t
                .batch()
                .schema()
                .fields()
                .iter()
                .map(|f| f.name().clone())
                .filter(|n| !calculated(n))
                .collect();
            if want != got {
                return Err(format!(
                    "`{}` loaded {got:?}, its Query gives {want:?}",
                    q.name
                ));
            }
        }
        Ok(())
    }

    /// `doc`'s Queries as they are saved: an in-memory Source file is written as its name (the
    /// path printed in M), so it opens as a path.
    fn saved_queries(doc: &Document) -> Vec<Query> {
        let mut queries = doc.queries.clone();
        for q in &mut queries {
            let (Source::Csv { file, .. } | Source::Parquet { file } | Source::Excel { file, .. }) =
                &mut q.source;
            if let File::Bytes { name, .. } = file {
                *file = File::Path(name.as_str().into());
            }
        }
        queries
    }

    /// Where two values' pretty debug output first differs (with a little context).
    fn first_difference(a: &impl std::fmt::Debug, b: &impl std::fmt::Debug) -> String {
        let (a, b) = (format!("{a:#?}"), format!("{b:#?}"));
        let (a, b): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
        let at = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
        let from = at.saturating_sub(6);
        let show = |l: &[&str]| {
            l.iter()
                .skip(from)
                .take(10)
                .copied()
                .collect::<Vec<_>>()
                .join("\n")
        };
        format!(
            "at line {at}:\n{}\n--- vs saved ---\n{}",
            show(&a),
            show(&b)
        )
    }

    /// (e): `doc` encoded to an in-memory FileSet, decoded, gives back its definitions; again
    /// through the app (a Package's bytes, opened). Compared exactly: the Project culture,
    /// Queries (name, source, steps, culture; an in-memory source as its name, see
    /// [`saved_queries`]), Enable load, and the whole Model (Tables, Columns and all their
    /// properties, Relationships in order, autodetect). Not compared: data and Stale (no Data
    /// cache until M2.6, so an opened Document's Tables are empty and its loaded Queries Stale);
    /// only that the opened Tables are the Model's, empty. Saving what was opened gives the same
    /// bytes.
    fn round_trip(doc: &Document) -> Result<(), String> {
        let project = Project {
            culture: "en-US".into(),
            queries: saved_queries(doc),
            unloaded: doc.unloaded.clone(),
            model: doc.model.clone(),
            // The report is checked through the app below (its JSON is the engine's).
            report: BTreeMap::new(),
        };
        let files = opendash_file::encode(&project, None, &Preserved::default())
            .map_err(|e| format!("encode: {e}"))?;
        let (back, preserved) =
            opendash_file::decode(&files, None).map_err(|e| format!("decode: {e}"))?;
        if back != project {
            return Err(format!("decoded {}", first_difference(&back, &project)));
        }
        let again = opendash_file::encode(&back, None, &preserved)
            .map_err(|e| format!("encode again: {e}"))?;
        if again != files {
            return Err("saving what was opened changes the bytes".into());
        }

        let mut app = App::with_document(doc.clone());
        let bytes = app.package_bytes().map_err(|e| format!("package: {e}"))?;
        app.open_package_bytes(&bytes)
            .map_err(|e| format!("open package: {e}"))?;
        let opened = app.document();
        if opened.queries != project.queries
            || opened.unloaded != doc.unloaded
            || opened.model != doc.model
        {
            return Err("the opened Package's definitions differ".into());
        }
        let names = |d: &Document| {
            d.tables
                .iter()
                .map(|t| t.name().to_owned())
                .collect::<Vec<_>>()
        };
        // The Package carries the Data cache (M2.6): tables reopen with the data they had.
        if names(opened) != names(doc) || opened.tables != doc.tables {
            return Err(format!(
                "opened tables {:?}, saved {:?}",
                names(opened),
                names(doc)
            ));
        }
        if opened.report != doc.report {
            return Err(format!(
                "the opened report {}",
                first_difference(&opened.report, &doc.report)
            ));
        }
        if app
            .package_bytes()
            .map_err(|e| format!("package again: {e}"))?
            != bytes
        {
            return Err("saving the opened Package changes the bytes".into());
        }
        check(app.document()).map_err(|e| format!("opened: {e}"))
    }

    /// Per command id: (runs, successes).
    type Stats = BTreeMap<&'static str, (usize, usize)>;

    /// One sequence from `seed`; panics with the seed and the commands run on a broken invariant.
    fn run(seed: u64, stats: &mut Stats) {
        let mut rng = Rng(seed);
        let original = document();
        check(&original).unwrap();
        round_trip(&original).unwrap();
        let mut app = App::with_document(original.clone());
        let mut log: Vec<String> = Vec::new();
        for _ in 0..STEPS {
            let (id, params) = command(&mut rng, &app);
            let before = app.document().clone();
            let broken = before.broken_fields().len();
            let result = app.run(id, &params);
            let entry = stats.entry(id).or_default();
            entry.0 += 1;
            entry.1 += usize::from(result.is_ok());
            log.push(format!(
                "{id} {params} -> {}",
                result
                    .as_ref()
                    .map_or_else(|e| format!("Err({e})"), |()| "Ok".into())
            ));
            let fail = |what: String| -> ! {
                panic!(
                    "seed {seed}: {what}\nreplay: ODASH_FUZZ_SEED={seed}\n{}",
                    log.join("\n")
                )
            };
            if result.is_err() && app.document() != &before {
                fail(format!("{id} failed but changed the document"));
            }
            if let Err(e) = check(app.document()) {
                fail(format!("after {id}: {e}"));
            }
            let rename = [
                "query.rename",
                "modeling.renameTable",
                "modeling.renameColumn",
                "modeling.renameMeasure",
            ];
            if rename.contains(&id) && app.document().broken_fields().len() > broken {
                fail(format!("{id} broke a Field of the report"));
            }
            if let Err(e) = fresh_tables_match_their_queries(&app) {
                fail(format!("after {id}: {e}"));
            }
        }
        let last = app.document().clone();
        if let Err(e) = round_trip(&last) {
            panic!(
                "seed {seed}: save → open: {e}\nreplay: ODASH_FUZZ_SEED={seed}\n{}",
                log.join("\n")
            );
        }
        let mut undone = 0usize;
        while app.can_undo() {
            app.run("edit.undo", &Value::Null).unwrap();
            undone += 1;
        }
        assert!(undone <= UNDO_LIMIT);
        if app.document() != &original {
            panic!(
                "seed {seed}: undo-all doesn't give the original\n{}",
                log.join("\n")
            );
        }
        // As many redos as undos: a sequence ending in undos leaves more on the redo stack.
        for _ in 0..undone {
            app.run("edit.redo", &Value::Null).unwrap();
        }
        if app.document() != &last {
            panic!(
                "seed {seed}: redo-all doesn't give the final document\n{}",
                log.join("\n")
            );
        }
    }

    #[test]
    fn command_sequences_keep_the_document_consistent_and_undoable() {
        if let Some(seed) = std::env::var("ODASH_FUZZ_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
        {
            run(seed, &mut Stats::new());
            return;
        }
        let scale: u64 = std::env::var("ODASH_FUZZ_SCALE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1)
            .clamp(1, 10_000);
        let mut stats = Stats::new();
        for seed in 0..SEEDS.saturating_mul(scale) {
            run(seed, &mut stats);
        }
        // Coverage, shown with --nocapture: how often each command ran and succeeded.
        for (id, (runs, ok)) in &stats {
            eprintln!("{id:40} {ok:5} ok / {runs:5}");
        }
        for id in [
            "modeling.renameColumn",
            "query.rename",
            "home.closeApply",
            "home.mergeQueries",
            "modeling.newMeasure",
            "modeling.newColumn",
            "modeling.editExpression",
            "modeling.delete",
            "insert.newPage",
            "insert.newVisual",
            "visual.addField",
            "visual.changeType",
            "visual.moveField",
            "visual.removeField",
            "visual.sort",
            "visual.format",
            "page.delete",
        ] {
            assert!(
                stats.get(id).is_some_and(|(_, ok)| *ok > 0),
                "`{id}` never succeeded: the generator is too far off"
            );
        }
    }
}
