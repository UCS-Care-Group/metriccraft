use super::*;

const TABLE: &str = "table Sales
  column Region
    dataType: text
    summarizeBy: none
  column 'Unit Price'
    dataType: fixedDecimal
    formatString: \"#,0.00\"
    sortByColumn: Region
    hidden
";

const RELATIONSHIPS: &str = "relationship Sales[Region] -> Regions[Region]
  inactive
";

const QUERY: &str = "query Sales
  load: true
  m:
    let
        Source = Csv.Document(File.Contents(\"data/sales.csv\"),[…]),
        #\"Promoted Headers\" = Table.PromoteHeaders(Source, [PromoteAllScalars=true])
    in
        #\"Promoted Headers\"
";

fn round_trip(src: &str) -> Vec<Item> {
    let doc = parse(src).unwrap_or_else(|e| panic!("{src:?}: {e}"));
    assert_eq!(print(&doc), src);
    doc
}

fn err(src: &str) -> Error {
    match parse(src) {
        Ok(doc) => panic!("{src:?} parsed: {doc:?}"),
        Err(e) => e,
    }
}

fn node(item: &Item) -> &Node {
    match item {
        Item::Node(n) => n,
        other => panic!("not a node: {other:?}"),
    }
}

#[test]
fn adr_table_example_round_trips() {
    let doc = round_trip(TABLE);
    let table = node(&doc[0]);
    assert_eq!(
        (table.kind.as_str(), table.name()),
        ("table", Some("Sales".into()))
    );
    let cols: Vec<_> = table.children().collect();
    assert_eq!(cols[1].name().as_deref(), Some("Unit Price"));
    assert_eq!(
        cols[1].prop("formatString"),
        Some(&Value::Text("#,0.00".into()))
    );
    assert_eq!(
        cols[1].prop("dataType").map(Value::as_str),
        Some("fixedDecimal")
    );
    assert!(cols[1].has_flag("hidden") && !cols[0].has_flag("hidden"));
}

#[test]
fn adr_relationship_example_round_trips() {
    let doc = round_trip(RELATIONSHIPS);
    let rel = node(&doc[0]);
    assert_eq!(rel.header, "Sales[Region] -> Regions[Region]");
    assert_eq!(rel.name(), None);
    assert!(rel.has_flag("inactive"));
    assert_eq!(
        read_name(&rel.header),
        Some(("Sales".into(), "[Region] -> Regions[Region]"))
    );
}

#[test]
fn adr_query_example_round_trips() {
    let doc = round_trip(QUERY);
    let q = node(&doc[0]);
    assert_eq!(q.prop("load"), Some(&Value::Bare("true".into())));
    let m = q.block("m").unwrap_or_default();
    assert!(m.starts_with("let\n    Source = Csv.Document("));
    assert!(m.ends_with("in\n    #\"Promoted Headers\""));
}

#[test]
fn many_top_level_nodes_and_measure_headers_round_trip() {
    round_trip(
        "relationship Sales[Region] -> Regions[Region]\n  inactive\nrelationship 'Sales Lines'[Product Key] -> Products[Key]\nmeasure 'Total' = SUM ( Sales[Amount] )\n",
    );
}

#[test]
fn verbatim_block_keeps_odd_indentation_blank_lines_and_trailing_spaces() {
    let src = "query Q\n  m:\n    let  \n\n       Source = 1,\t\n          \n   \n      X = Source // not a comment here\n    in\n      X\n  load: false\n";
    let doc = parse(src).unwrap_or_default();
    let q = node(&doc[0]);
    assert_eq!(
        q.block("m"),
        Some("let  \n\n   Source = 1,\t\n      \n\n  X = Source // not a comment here\nin\n  X")
    );
    assert_eq!(q.prop("load").map(Value::as_str), Some("false"));
    // The canonical form drops only the spaces of a short blank line; it is then stable.
    let printed = print(&doc);
    assert_eq!(printed, src.replace("\n   \n", "\n\n"));
    round_trip(&printed);
}

#[test]
fn trailing_blank_lines_separate_a_block_from_what_follows() {
    let doc = parse("q A\n  m:\n    x\n\n\n  load: true\n").unwrap_or_default();
    assert_eq!(node(&doc[0]).block("m"), Some("x"));
    assert_eq!(print(&doc), "q A\n  m:\n    x\n  load: true\n");
}

#[test]
fn empty_block_and_top_level_block_round_trip() {
    round_trip("q A\n  m:\n  load: true\n");
    round_trip("expression:\n  1 +\n    2\n");
}

#[test]
fn unknown_kinds_keys_flags_and_blocks_survive() {
    let src = "table T\n  futureKey: 'Some Name'\n  shiny\n  widget 'W 1' (x, y) => z\n    deep: \"a\\\"b\\\\c\\nd\\te\\r\"\n    script:\n      anything goes: here\n  column C\n    lineage: 9f3a-77\n";
    let doc = round_trip(src);
    let w = node(&doc[0]).children().find(|n| n.kind == "widget");
    assert_eq!(w.map(|n| n.header.as_str()), Some("'W 1' (x, y) => z"));
    assert_eq!(
        w.and_then(|n| n.prop("deep")),
        Some(&Value::Text("a\"b\\c\nd\te\r".into()))
    );
}

#[test]
fn item_order_is_preserved() {
    round_trip("t A\n  z: 1\n  flag\n  column B\n  a: 2\n  m:\n    x\n  column C\n");
}

#[test]
fn quoted_names_double_quotes_like_dax() {
    let doc = round_trip("column 'It''s ''here'''\n  sortByColumn: 'Bob''s'\n");
    let c = node(&doc[0]);
    assert_eq!(c.name().as_deref(), Some("It's 'here'"));
    assert_eq!(c.prop("sortByColumn"), Some(&Value::Name("Bob's".into())));
    assert_eq!(quote_name("It's"), "'It''s'");
    assert_eq!(quote_name("Région_1"), "Région_1");
    assert_eq!(quote_name(""), "''");
    assert_eq!(Node::named("column", "Unit Price").header, "'Unit Price'");
}

#[test]
fn crlf_and_bom_are_accepted_and_normalised() {
    let crlf = TABLE.replace('\n', "\r\n");
    let doc = parse(&format!("\u{feff}{crlf}")).unwrap_or_default();
    assert_eq!(print(&doc), TABLE);
    let q = parse(&QUERY.replace('\n', "\r\n")).unwrap_or_default();
    assert_eq!(print(&q), QUERY);
}

#[test]
fn canonical_spacing_and_no_trailing_newline_input() {
    let doc = parse("table   Sales  \n  dataType:    text\n  hidden   ").unwrap_or_default();
    assert_eq!(print(&doc), "table Sales\n  dataType: text\n  hidden\n");
}

#[test]
fn bare_values_that_cannot_print_bare_print_as_text() {
    let doc = vec![Item::Prop("k".into(), Value::Bare("two words".into()))];
    let printed = print(&doc);
    assert_eq!(printed, "k: \"two words\"\n");
    assert_eq!(print(&parse(&printed).unwrap_or_default()), printed);
}

#[test]
fn malformed_input_names_line_column_and_expectation() {
    let cases: &[(&str, usize, usize, &str)] = &[
        ("table T\n   column C\n", 2, 1, "expected 2 spaces, found 3"),
        (
            "table T\n  column C\n      x: 1\n",
            3,
            1,
            "expected 4 spaces, found 6",
        ),
        ("table T\n\tcolumn C\n", 2, 1, "tab in indentation"),
        ("table T\n  // note\n", 2, 3, "comments are not supported"),
        ("// header\ntable T\n", 1, 1, "comments are not supported"),
        (
            "table T\n  hidden\n    x: 1\n",
            3,
            1,
            "unexpected indentation",
        ),
        ("table T\n  k: \"open\n", 2, 6, "unterminated text"),
        ("table T\n  k: \"a\\qb\"\n", 2, 8, "unknown escape"),
        ("table T\n  k: 'open\n", 2, 6, "unterminated name"),
        ("table T\n  k: two words\n", 2, 9, "expected end of line"),
        ("table T\n  k: \"a\" b\n", 2, 9, "expected end of line"),
        ("table T\n  k : v\n", 2, 4, "space before `:`"),
        ("table T\n  [x]\n", 2, 3, "expected an object"),
        ("table-T\n", 1, 6, "expected `:`, a space and a name"),
        ("table T\r\n  a\rb\n", 2, 4, "carriage return"),
    ];
    for &(src, line, column, msg) in cases {
        let e = err(src);
        assert_eq!((e.line, e.column), (line, column), "{src:?}: {e}");
        assert!(e.message.contains(msg), "{src:?}: {e}");
        assert!(
            e.to_string()
                .starts_with(&format!("line {line}, column {column}: "))
        );
    }
}

#[test]
fn nesting_is_bounded() {
    let deep = |levels: usize| -> String {
        (0..levels)
            .map(|d| format!("{}n x\n", "  ".repeat(d)))
            .collect()
    };
    round_trip(&deep(MAX_DEPTH));
    let e = err(&deep(MAX_DEPTH + 1));
    assert!(e.message.contains("levels deep"), "{e}");
    // Far deeper than the limit: still an error, not a stack overflow.
    assert!(parse(&deep(5_000)).is_err());
}

#[test]
fn oversized_input_and_lines_are_rejected() {
    assert!(parse(&" ".repeat(MAX_INPUT + 1)).is_err());
    let e = err(&format!("k: {}\n", "x".repeat(MAX_LINE)));
    assert_eq!(e.line, 1);
    assert!(e.message.contains("longer than"));
}

/// xorshift: deterministic fuzzing without a dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

#[test]
fn fuzzed_input_never_panics_and_reprints_stably() {
    let seeds = [
        TABLE,
        RELATIONSHIPS,
        QUERY,
        "t 'a''b'\n  k: \"x\\ny\"\n  m:\n      odd\n\n    back\n  f\n",
    ];
    let alphabet: Vec<char> = " \n\r\t:'\"\\/[]-_aZ9é\u{feff}".chars().collect();
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for i in 0..20_000 {
        let src: String = if i % 4 == 0 {
            let len = rng.below(96);
            let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            String::from_utf8_lossy(&bytes).into_owned()
        } else {
            let mut chars: Vec<char> = seeds[rng.below(seeds.len())].chars().collect();
            for _ in 0..=rng.below(4) {
                let at = rng.below(chars.len() + 1);
                match rng.below(3) {
                    0 if at < chars.len() => {
                        chars.remove(at);
                    }
                    1 => chars.insert(at, alphabet[rng.below(alphabet.len())]),
                    _ => {
                        let end = (at + rng.below(8)).min(chars.len());
                        let piece: Vec<char> = chars[at.min(end)..end].to_vec();
                        let to = rng.below(chars.len() + 1);
                        for (k, c) in piece.into_iter().enumerate() {
                            chars.insert(to + k, c);
                        }
                    }
                }
            }
            chars.into_iter().collect()
        };
        if let Ok(doc) = parse(&src) {
            let printed = print(&doc);
            let again = parse(&printed).unwrap_or_else(|e| panic!("{src:?} → {printed:?}: {e}"));
            assert_eq!(again, doc, "{src:?} → {printed:?}");
            assert_eq!(print(&again), printed);
        }
    }
}
