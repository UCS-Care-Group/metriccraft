//! The `.mcm` / `.mcq` text syntax (M2): a generic, order-preserving document
//! tree with a parser and a deterministic printer. Mapping the tree to the model is the caller's
//! job; this module knows nothing about tables or queries.
//!
//! ```text
//! table Sales                  <- Node: kind `table`, header `Sales`
//!   column 'Unit Price'        <- child Node; header is a quoted name
//!     formatString: "#,0.00"   <- Prop with a Text value
//!     hidden                   <- Flag
//! query Sales
//!   m:                         <- Block: verbatim text indented one level deeper
//!     let … in …
//! ```
//!
//! Lexical rules:
//! - Indentation is 2 spaces per level; tabs in indentation are an error.
//! - A line is one of: `kind header` (a [`Node`]; children follow one level deeper), `key: value`
//!   (a [`Item::Prop`]), `key:` alone (a [`Item::Block`]) or a single word (an [`Item::Flag`]).
//!   Kinds, keys and flags are words of ASCII letters, digits and `_`.
//! - Values ([`Value`]) are a bare token (`true`, `12.5`, `Region`, `Sales[Region]`), a
//!   `"text"` with the escapes `\\ \" \n \r \t`, or a `'name'` with `''` for a quote, as in DAX.
//! - A node header is kept raw (after the kind and its separating spaces), so headers such as
//!   `Sales[Region] -> Regions[Region]` round-trip exactly; [`Node::name`] reads a lone name.
//! - Blocks: the lines after `key:` indented deeper than the key belong to it, dedented by the key's
//!   indent plus 2; the block ends at the first non-blank line indented less. Content is kept
//!   byte-exact (trailing spaces included). Blank lines inside are kept, trailing blank lines are
//!   not (they separate the block from what follows).
//! - Expression blocks (M3.6): a node whose header ends with `=` (`measure 'Total' =`) is
//!   followed by verbatim DAX indented one level deeper than its items, kept as its first item,
//!   a block with the key [`EXPRESSION`] (empty when there are no such lines), and printed
//!   without a `key:` line. The same block rules apply.
//! - `//` comment lines are rejected (outside blocks): the printer could not reproduce them, and
//!   silently dropping them would lose what the author wrote.
//! - CRLF line endings are accepted and normalised to LF; a lone CR is an error. A leading UTF-8
//!   BOM is ignored. Trailing spaces on structural lines are ignored.
//! - Limits: [`MAX_INPUT`] bytes, [`MAX_LINE`] bytes per line, [`MAX_DEPTH`] nesting levels.
//!
//! The printer writes 2-space indentation, LF endings, one space after `:`, no blank lines and a
//! trailing newline. `print(parse(print(x)))` is byte-identical to `print(x)`.

use std::fmt;

/// Largest accepted input, in bytes.
pub const MAX_INPUT: usize = 16 << 20;
/// Longest accepted line, in bytes (the M parser's own cap, so any M it accepts fits on one line).
pub const MAX_LINE: usize = 4 << 20;
/// Deepest accepted nesting (top level is depth 0).
pub const MAX_DEPTH: usize = 32;
/// The key of an expression block: the verbatim text after a node whose header ends with `=`.
/// Not a word, so no `key:` block can have it.
pub const EXPRESSION: &str = "=";

/// One line-level element of a document, in source order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Item {
    Node(Node),
    /// `key: value`
    Prop(String, Value),
    /// A bare word such as `hidden`.
    Flag(String),
    /// `key:` followed by verbatim text (M, DAX); lines joined with `\n`, no trailing newline.
    Block(String, String),
}

/// An object: `kind header`, then its items one level deeper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub kind: String,
    /// Raw text after the kind, e.g. `Sales`, `'Unit Price'`, `Sales[Region] -> Regions[Region]`.
    pub header: String,
    pub items: Vec<Item>,
}

impl Node {
    /// A node whose header is `name`, quoted when needed.
    pub fn named(kind: impl Into<String>, name: &str) -> Self {
        Self {
            kind: kind.into(),
            header: quote_name(name),
            items: Vec::new(),
        }
    }

    /// The header as a single name (bare or `'quoted'`), or `None` if it is anything else.
    pub fn name(&self) -> Option<String> {
        match read_name(&self.header) {
            Some((name, "")) => Some(name),
            _ => None,
        }
    }

    /// The first `key: value` with this key.
    pub fn prop(&self, key: &str) -> Option<&Value> {
        self.items.iter().find_map(|i| match i {
            Item::Prop(k, v) if k == key => Some(v),
            _ => None,
        })
    }

    pub fn has_flag(&self, flag: &str) -> bool {
        self.items
            .iter()
            .any(|i| matches!(i, Item::Flag(f) if f == flag))
    }

    /// The first verbatim block with this key.
    pub fn block(&self, key: &str) -> Option<&str> {
        self.items.iter().find_map(|i| match i {
            Item::Block(k, t) if k == key => Some(t.as_str()),
            _ => None,
        })
    }

    pub fn children(&self) -> impl Iterator<Item = &Node> {
        self.items.iter().filter_map(|i| match i {
            Item::Node(n) => Some(n),
            _ => None,
        })
    }
}

/// A property value. The variant records how it was written, so it prints back the same way.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// An unquoted token: no whitespace, not starting with a quote.
    Bare(String),
    /// `"…"` with escapes.
    Text(String),
    /// `'…'` with `''` escaping.
    Name(String),
}

impl Value {
    /// The value's text with quoting removed.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Bare(s) | Self::Text(s) | Self::Name(s) => s,
        }
    }
}

/// A parse error at a 1-based line and column (in characters).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for Error {}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `name` as written in a header or value: bare if it is all letters, digits and `_`, otherwise
/// `'quoted'` with `'` doubled.
pub fn quote_name(name: &str) -> String {
    if !name.is_empty() && name.chars().all(is_name_char) {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

/// Reads one name (bare letters/digits/`_`, or `'quoted'`) from the start of `s`; returns it and
/// the rest of `s`. For callers splitting headers like `'Unit Price'[Region] -> …`.
pub fn read_name(s: &str) -> Option<(String, &str)> {
    if let Some(body) = s.strip_prefix('\'') {
        let mut name = String::new();
        let mut chars = body.char_indices();
        while let Some((i, c)) = chars.next() {
            if c == '\'' {
                if body.get(i + 1..)?.starts_with('\'') {
                    name.push('\'');
                    chars.next();
                } else {
                    return Some((name, body.get(i + 1..)?));
                }
            } else {
                name.push(c);
            }
        }
        None
    } else {
        let end = s.find(|c| !is_name_char(c)).unwrap_or(s.len());
        if end == 0 {
            return None;
        }
        Some((s.get(..end)?.to_string(), s.get(end..)?))
    }
}

/// Parses a document into its top-level items.
pub fn parse(text: &str) -> Result<Vec<Item>, Error> {
    if text.len() > MAX_INPUT {
        return Err(Error {
            line: 1,
            column: 1,
            message: format!("file is larger than {} MB", MAX_INPUT >> 20),
        });
    }
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines = Vec::new();
    for (i, raw) in text.split('\n').enumerate() {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(at) = line.find('\r') {
            return Err(err_at(
                i + 1,
                line,
                at,
                "carriage return inside a line; expected LF or CRLF line endings",
            ));
        }
        if line.len() > MAX_LINE {
            return Err(Error {
                line: i + 1,
                column: 1,
                message: format!("line is longer than {} MB", MAX_LINE >> 20),
            });
        }
        lines.push(line);
    }
    let mut p = Parser { lines, at: 0 };
    p.items(0)
}

fn err_at(line: usize, text: &str, byte: usize, message: impl Into<String>) -> Error {
    let column = text.get(..byte).map_or(0, |s| s.chars().count()) + 1;
    Error {
        line,
        column,
        message: message.into(),
    }
}

fn is_blank(line: &str) -> bool {
    line.chars().all(|c| c == ' ' || c == '\t')
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

struct Parser<'a> {
    lines: Vec<&'a str>,
    at: usize,
}

impl Parser<'_> {
    fn err(&self, byte: usize, message: impl Into<String>) -> Error {
        let line = self.lines.get(self.at).copied().unwrap_or("");
        err_at(self.at + 1, line, byte, message)
    }

    /// Items at `depth` until a line indented less (or the end).
    fn items(&mut self, depth: usize) -> Result<Vec<Item>, Error> {
        let want = depth * 2;
        let mut items = Vec::new();
        while let Some(&line) = self.lines.get(self.at) {
            if is_blank(line) {
                self.at += 1;
                continue;
            }
            let indent = leading_spaces(line);
            if line.get(indent..).is_some_and(|r| r.starts_with('\t')) {
                return Err(self.err(indent, "tab in indentation; indent with 2 spaces per level"));
            }
            if indent < want {
                break;
            }
            if indent > want {
                return Err(self.err(
                    0,
                    format!("unexpected indentation: expected {want} spaces, found {indent}"),
                ));
            }
            items.push(self.item(line, indent, depth)?);
        }
        Ok(items)
    }

    /// One item starting on the current line; advances past it (and its children or block).
    fn item(&mut self, line: &str, indent: usize, depth: usize) -> Result<Item, Error> {
        let content = line
            .get(indent..)
            .unwrap_or("")
            .trim_end_matches([' ', '\t']);
        if content.starts_with("//") {
            return Err(self.err(
                indent,
                "comments are not supported; expected an object, `key: value`, `key:` or a flag",
            ));
        }
        let word_len = content.find(|c| !is_word_char(c)).unwrap_or(content.len());
        if word_len == 0 {
            return Err(self.err(
                indent,
                "expected an object (`kind name`), `key: value`, `key:` or a flag",
            ));
        }
        let word = content.get(..word_len).unwrap_or("").to_string();
        let rest = content.get(word_len..).unwrap_or("");
        let rest_at = indent + word_len;

        if rest.is_empty() {
            self.at += 1;
            return Ok(Item::Flag(word));
        }
        if let Some(after) = rest.strip_prefix(':') {
            let value = after.trim_start_matches(' ');
            if value.is_empty() {
                return Ok(Item::Block(word, self.block(indent + 2)));
            }
            let value_at = rest_at + 1 + (after.len() - value.len());
            let v = self.value(value, value_at)?;
            self.at += 1;
            return Ok(Item::Prop(word, v));
        }
        if rest.starts_with(' ') {
            let header = rest.trim_start_matches(' ');
            if header.starts_with(':') {
                return Err(self.err(
                    rest_at,
                    "space before `:`; expected `key: value` with no space before the colon",
                ));
            }
            if depth >= MAX_DEPTH {
                return Err(self.err(indent, format!("nested more than {MAX_DEPTH} levels deep")));
            }
            let mut items = Vec::new();
            if header.ends_with('=') {
                // `measure 'Total' =`: the DAX follows, one level deeper than the properties.
                items.push(Item::Block(EXPRESSION.into(), self.block(indent + 4)));
            } else {
                self.at += 1;
            }
            items.extend(self.items(depth + 1)?);
            return Ok(Item::Node(Node {
                kind: word,
                header: header.to_string(),
                items,
            }));
        }
        Err(self.err(
            rest_at,
            "expected `:`, a space and a name, or end of line after the word",
        ))
    }

    /// The lines after the current `key:` line indented at least `indent`, dedented.
    fn block(&mut self, indent: usize) -> String {
        self.at += 1;
        let mut out: Vec<&str> = Vec::new();
        while let Some(&line) = self.lines.get(self.at) {
            if leading_spaces(line) >= indent {
                out.push(line.get(indent..).unwrap_or(""));
            } else if is_blank(line) {
                out.push("");
            } else {
                break;
            }
            self.at += 1;
        }
        while out.last().is_some_and(|l| is_blank(l)) {
            out.pop();
            // Leave trailing blank lines to the caller (they are skipped as separators).
            self.at = self.at.saturating_sub(1);
        }
        out.join("\n")
    }

    fn value(&self, s: &str, at: usize) -> Result<Value, Error> {
        let (value, used) = if let Some(body) = s.strip_prefix('"') {
            let mut text = String::new();
            let mut chars = body.char_indices();
            let mut end = None;
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => {
                        end = Some(i + 2);
                        break;
                    }
                    '\\' => match chars.next() {
                        Some((_, '\\')) => text.push('\\'),
                        Some((_, '"')) => text.push('"'),
                        Some((_, 'n')) => text.push('\n'),
                        Some((_, 'r')) => text.push('\r'),
                        Some((_, 't')) => text.push('\t'),
                        _ => {
                            return Err(self.err(
                                at + 1 + i,
                                "unknown escape; expected \\\\, \\\", \\n, \\r or \\t",
                            ));
                        }
                    },
                    c => text.push(c),
                }
            }
            match end {
                Some(e) => (Value::Text(text), e),
                None => return Err(self.err(at, "unterminated text; expected a closing `\"`")),
            }
        } else if s.starts_with('\'') {
            match read_name(s) {
                Some((name, rest)) => (Value::Name(name), s.len() - rest.len()),
                None => return Err(self.err(at, "unterminated name; expected a closing `'`")),
            }
        } else {
            let end = s.find(char::is_whitespace).unwrap_or(s.len());
            (Value::Bare(s.get(..end).unwrap_or("").to_string()), end)
        };
        if used < s.len() {
            return Err(self.err(
                at + used,
                "expected end of line after the value; quote text that contains spaces",
            ));
        }
        Ok(value)
    }
}

/// Prints items canonically (see the module docs). Kinds, keys and flags are assumed to be words
/// and headers single-line, as the parser produces; a bare value that would not read back as one
/// token prints as `"text"`.
pub fn print(items: &[Item]) -> String {
    let mut out = String::new();
    print_items(&mut out, items, 0);
    out
}

fn print_items(out: &mut String, items: &[Item], depth: usize) {
    let pad = "  ".repeat(depth);
    for item in items {
        match item {
            Item::Node(n) => {
                out.push_str(&format!("{pad}{} {}\n", n.kind, n.header));
                print_items(out, &n.items, depth + 1);
            }
            Item::Prop(k, v) => out.push_str(&format!("{pad}{k}: {}\n", print_value(v))),
            Item::Flag(f) => out.push_str(&format!("{pad}{f}\n")),
            Item::Block(k, text) => {
                if k != EXPRESSION {
                    out.push_str(&format!("{pad}{k}:\n"));
                }
                let lines: Vec<&str> = text.split('\n').collect();
                let keep = lines
                    .iter()
                    .rposition(|l| !is_blank(l))
                    .map_or(0, |i| i + 1);
                for line in lines.iter().take(keep) {
                    if line.is_empty() {
                        out.push('\n');
                    } else {
                        out.push_str(&format!("{pad}  {line}\n"));
                    }
                }
            }
        }
    }
}

fn print_value(v: &Value) -> String {
    match v {
        Value::Bare(s)
            if !s.is_empty() && !s.starts_with(['"', '\'']) && !s.contains(char::is_whitespace) =>
        {
            s.clone()
        }
        Value::Name(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Bare(s) | Value::Text(s) => {
            let mut t = String::from("\"");
            for c in s.chars() {
                match c {
                    '\\' => t.push_str("\\\\"),
                    '"' => t.push_str("\\\""),
                    '\n' => t.push_str("\\n"),
                    '\r' => t.push_str("\\r"),
                    '\t' => t.push_str("\\t"),
                    c => t.push(c),
                }
            }
            t.push('"');
            t
        }
    }
}

#[cfg(test)]
mod tests;
