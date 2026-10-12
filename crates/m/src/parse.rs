//! Parses the M subset. Input is hostile: sizes and nesting are capped, nothing indexes.

use crate::{BinOp, Binding, Error, Expr, KEYWORDS, Let, PRIMITIVE_TYPES};

/// Deepest expression nesting accepted (brackets, calls, operators, `let`, `each`…).
pub const MAX_DEPTH: usize = 200;
/// Largest M text accepted, in bytes (a Query's M is a few KB).
pub const MAX_SOURCE: usize = 4 << 20;

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// An identifier; `quoted` for `#"…"`.
    Ident {
        name: String,
        quoted: bool,
    },
    Keyword(&'static str),
    Text(String),
    Number(f64),
    /// Operators and punctuators.
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    line: usize,
    column: usize,
    start: usize,
    end: usize,
}

const PUNCTS: &[&str] = &[
    "<>", "<=", ">=", ",", "=", "<", ">", "+", "-", "*", "/", "&", "(", ")", "[", "]", "{", "}",
];

fn is_newline(c: char) -> bool {
    matches!(c, '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    line: usize,
    column: usize,
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> Option<char> {
        self.src.get(self.pos..).and_then(|s| s.chars().next())
    }

    fn peek2(&self) -> Option<char> {
        self.src.get(self.pos..).and_then(|s| s.chars().nth(1))
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos = self.pos.saturating_add(c.len_utf8());
        // CR LF is one newline: count it at the LF.
        if is_newline(c) && !(c == '\r' && self.peek() == Some('\n')) {
            self.line = self.line.saturating_add(1);
            self.column = 1;
        } else if !is_newline(c) {
            self.column = self.column.saturating_add(1);
        }
        Some(c)
    }

    fn err(&self, message: impl Into<String>) -> Error {
        Error::new(self.line, self.column, message)
    }

    fn tokens(mut self) -> Result<Vec<Token>, Error> {
        let mut out = Vec::new();
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            let (line, column, start) = (self.line, self.column, self.pos);
            let tok = match self.peek() {
                None => Tok::Eof,
                Some(c) => self.token(c)?,
            };
            let done = tok == Tok::Eof;
            out.push(Token {
                tok,
                line,
                column,
                start,
                end: self.pos,
            });
            if done {
                return Ok(out);
            }
        }
    }

    fn token(&mut self, c: char) -> Result<Tok, Error> {
        if c == '/' && matches!(self.peek2(), Some('/' | '*')) {
            return Err(self.err("comments are not supported"));
        }
        if c == '"' {
            return Ok(Tok::Text(self.text()?));
        }
        if c == '#' {
            if self.peek2() == Some('"') {
                self.bump();
                let name = self.text()?;
                return Ok(Tok::Ident { name, quoted: true });
            }
            // `#date(…)` / `#datetime(…)`: literal constructors, read as function names.
            let rest = self.src.get(self.pos..).unwrap_or("");
            for name in ["#datetime", "#date"] {
                let after = rest.get(name.len()..).unwrap_or("");
                if rest.starts_with(name)
                    && !after.starts_with(|c: char| c.is_alphanumeric() || c == '_')
                {
                    for _ in 0..name.len() {
                        self.bump();
                    }
                    return Ok(Tok::Ident {
                        name: name.into(),
                        quoted: false,
                    });
                }
            }
            return Err(self
                .err("`#` forms other than `#\"…\"`, `#date` and `#datetime` are not supported"));
        }
        if c.is_ascii_digit() || (c == '.' && self.peek2().is_some_and(|d| d.is_ascii_digit())) {
            return self.number();
        }
        if c.is_alphabetic() || c == '_' {
            let name = self.identifier();
            return Ok(match KEYWORDS.iter().find(|k| **k == name) {
                Some(k) => Tok::Keyword(k),
                None => Tok::Ident {
                    name,
                    quoted: false,
                },
            });
        }
        let rest = self.src.get(self.pos..).unwrap_or("");
        for p in PUNCTS {
            if rest.starts_with(p) {
                for _ in 0..p.len() {
                    self.bump();
                }
                return Ok(Tok::Punct(p));
            }
        }
        Err(self.err(format!("unexpected character `{c}`")))
    }

    /// A regular identifier: segments of letters, digits and `_`, joined by dots.
    fn identifier(&mut self) -> String {
        let mut name = String::new();
        loop {
            while let Some(c) = self.peek().filter(|c| c.is_alphanumeric() || *c == '_') {
                name.push(c);
                self.bump();
            }
            if self.peek() == Some('.')
                && self.peek2().is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                name.push('.');
                self.bump();
            } else {
                return name;
            }
        }
    }

    fn number(&mut self) -> Result<Tok, Error> {
        let start = self.pos;
        if self.peek() == Some('0') && matches!(self.peek2(), Some('x' | 'X')) {
            self.bump();
            self.bump();
            let digits = self.take_while(|c| c.is_ascii_hexdigit());
            return u64::from_str_radix(&digits, 16)
                .map(|n| Tok::Number(n as f64))
                .map_err(|_| self.err("invalid hexadecimal number"));
        }
        self.take_while(|c| c.is_ascii_digit());
        if self.peek() == Some('.') && self.peek2().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            self.take_while(|c| c.is_ascii_digit());
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if self.take_while(|c| c.is_ascii_digit()).is_empty() {
                return Err(self.err("exponent needs digits"));
            }
        }
        let lexeme = self.src.get(start..self.pos).unwrap_or("");
        match lexeme.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Tok::Number(n)),
            _ => Err(self.err(format!("number `{lexeme}` is out of range"))),
        }
    }

    fn take_while(&mut self, f: impl Fn(char) -> bool) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek().filter(|c| f(*c)) {
            s.push(c);
            self.bump();
        }
        s
    }

    /// The body of `"…"` (the opening quote is next), with escapes resolved.
    fn text(&mut self) -> Result<String, Error> {
        let (line, column) = (self.line, self.column);
        self.bump();
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(Error::new(line, column, "text has no closing `\"`")),
                Some('"') if self.peek() == Some('"') => {
                    self.bump();
                    s.push('"');
                }
                Some('"') => return Ok(s),
                Some('#') if self.peek() == Some('(') => {
                    self.bump();
                    self.escapes(&mut s)?;
                }
                Some(c) => s.push(c),
            }
        }
    }

    /// `cr,lf,#,000D,0000000D)` after `#(`.
    fn escapes(&mut self, s: &mut String) -> Result<(), Error> {
        loop {
            let code = self.take_while(|c| c.is_ascii_alphanumeric() || c == '#');
            let c = match code.as_str() {
                "cr" => Some('\r'),
                "lf" => Some('\n'),
                "tab" => Some('\t'),
                "#" => Some('#'),
                hex if (hex.len() == 4 || hex.len() == 8)
                    && hex.chars().all(|c| c.is_ascii_hexdigit()) =>
                {
                    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
                }
                _ => None,
            };
            let Some(c) = c else {
                return Err(self.err(format!("invalid escape `#({code})`")));
            };
            s.push(c);
            match self.bump() {
                Some(',') => {}
                Some(')') => return Ok(()),
                _ => return Err(self.err("escape sequence needs a closing `)`")),
            }
        }
    }
}

/// Parses an M expression (e.g. a Query's `let … in …`). Errors carry the line and column.
pub fn parse(src: &str) -> Result<Expr, Error> {
    if src.len() > MAX_SOURCE {
        return Err(Error::new(
            1,
            1,
            format!("M text is larger than {} MB", MAX_SOURCE >> 20),
        ));
    }
    let tokens = Lexer {
        src,
        pos: 0,
        line: 1,
        column: 1,
    }
    .tokens()?;
    let mut p = Parser {
        src,
        tokens,
        pos: 0,
        depth: 0,
    };
    let e = p.expr()?;
    match p.peek() {
        Tok::Eof => Ok(e),
        _ => Err(p.unexpected("the end")),
    }
}

struct Parser<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn token(&self) -> Option<&Token> {
        self.tokens.get(self.pos).or_else(|| self.tokens.last())
    }

    fn peek(&self) -> &Tok {
        self.token().map_or(&Tok::Eof, |t| &t.tok)
    }

    fn next(&mut self) -> Tok {
        let t = self.peek().clone();
        if t != Tok::Eof {
            self.pos = self.pos.saturating_add(1);
        }
        t
    }

    fn at(&self) -> (usize, usize) {
        self.token().map_or((1, 1), |t| (t.line, t.column))
    }

    fn err(&self, message: impl Into<String>) -> Error {
        let (l, c) = self.at();
        Error::new(l, c, message)
    }

    fn unexpected(&self, wanted: &str) -> Error {
        let found = match self.peek() {
            Tok::Ident { name, .. } => format!("`{name}`"),
            Tok::Keyword(k) => format!("`{k}` (not supported here)"),
            Tok::Text(_) => "text".into(),
            Tok::Number(_) => "a number".into(),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Eof => "the end of the M".into(),
        };
        self.err(format!("expected {wanted}, found {found}"))
    }

    fn eat(&mut self, p: &str) -> bool {
        if matches!(self.peek(), Tok::Punct(q) if *q == p) {
            self.pos = self.pos.saturating_add(1);
            true
        } else {
            false
        }
    }

    fn expect(&mut self, p: &str) -> Result<(), Error> {
        if self.eat(p) {
            Ok(())
        } else {
            Err(self.unexpected(&format!("`{p}`")))
        }
    }

    fn keyword(&mut self, k: &str) -> bool {
        if matches!(self.peek(), Tok::Keyword(q) if *q == k) {
            self.pos = self.pos.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// One level deeper; errors past [`MAX_DEPTH`]. Callers restore `self.depth` on return.
    fn enter(&mut self) -> Result<(), Error> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > MAX_DEPTH {
            return Err(self.err(format!("nested more than {MAX_DEPTH} levels deep")));
        }
        Ok(())
    }

    /// Whether the token after the one just consumed follows it with no whitespace.
    fn glued(&self) -> bool {
        match (
            self.tokens.get(self.pos.wrapping_sub(1)),
            self.tokens.get(self.pos),
        ) {
            (Some(a), Some(b)) => a.end == b.start,
            _ => false,
        }
    }

    fn expr(&mut self) -> Result<Expr, Error> {
        let saved = self.depth;
        self.enter()?;
        let e = if self.keyword("let") {
            self.let_expr()
        } else if self.keyword("each") {
            self.expr().map(|e| Expr::Each(Box::new(e)))
        } else {
            self.binary(1)
        };
        self.depth = saved;
        e
    }

    fn let_expr(&mut self) -> Result<Expr, Error> {
        let mut bindings = Vec::new();
        loop {
            let (line, column) = self.at();
            let name = match self.next() {
                Tok::Ident { name, .. } => name,
                _ => {
                    self.pos = self.pos.saturating_sub(1);
                    return Err(self.unexpected("a step name"));
                }
            };
            self.expect("=")?;
            let value = self.expr()?;
            bindings.push(Binding {
                name,
                value,
                line,
                column,
            });
            if !self.eat(",") {
                break;
            }
        }
        if !self.keyword("in") {
            return Err(self.unexpected("`,` or `in`"));
        }
        let body = self.expr()?;
        Ok(Expr::Let(Let {
            bindings,
            body: Box::new(body),
        }))
    }

    fn binop(&self) -> Option<BinOp> {
        Some(match self.peek() {
            Tok::Keyword("or") => BinOp::Or,
            Tok::Keyword("and") => BinOp::And,
            Tok::Punct("=") => BinOp::Equal,
            Tok::Punct("<>") => BinOp::NotEqual,
            Tok::Punct("<") => BinOp::Less,
            Tok::Punct("<=") => BinOp::LessEqual,
            Tok::Punct(">") => BinOp::Greater,
            Tok::Punct(">=") => BinOp::GreaterEqual,
            Tok::Punct("+") => BinOp::Add,
            Tok::Punct("-") => BinOp::Subtract,
            Tok::Punct("&") => BinOp::Concat,
            Tok::Punct("*") => BinOp::Multiply,
            Tok::Punct("/") => BinOp::Divide,
            _ => return None,
        })
    }

    /// Precedence climbing; every operator applied counts as a nesting level so left-leaning
    /// chains (`a + b + c …`) are bounded too.
    fn binary(&mut self, min: u8) -> Result<Expr, Error> {
        let saved = self.depth;
        let result = (|| {
            let mut lhs = self.unary()?;
            while let Some(op) = self.binop().filter(|op| op.precedence() >= min) {
                self.next();
                self.enter()?;
                let rhs = self.binary(op.precedence().saturating_add(1))?;
                lhs = Expr::Binary(op, Box::new(lhs), Box::new(rhs));
            }
            Ok(lhs)
        })();
        self.depth = saved;
        result
    }

    fn unary(&mut self) -> Result<Expr, Error> {
        let saved = self.depth;
        self.enter()?;
        let e = if self.keyword("not") {
            self.unary().map(|e| Expr::Not(Box::new(e)))
        } else if self.eat("-") {
            self.unary().map(|e| Expr::Negate(Box::new(e)))
        } else {
            self.postfix()
        };
        self.depth = saved;
        e
    }

    fn postfix(&mut self) -> Result<Expr, Error> {
        let saved = self.depth;
        let result = (|| {
            let mut e = self.primary()?;
            loop {
                if self.eat("(") {
                    self.enter()?;
                    let (args, tight) = self.seq(")", Self::expr)?;
                    e = Expr::Call {
                        func: Box::new(e),
                        args,
                        tight,
                    };
                } else if self.eat("{") {
                    self.enter()?;
                    let sel = self.expr()?;
                    self.expect("}")?;
                    e = Expr::Item(Box::new(e), Box::new(sel));
                } else if self.eat("[") {
                    self.enter()?;
                    let name = self.field_name()?;
                    self.expect("]")?;
                    e = Expr::Field(Some(Box::new(e)), name);
                } else {
                    return Ok(e);
                }
            }
        })();
        self.depth = saved;
        result
    }

    /// Comma-separated items up to `close`; also whether the first separator had no space.
    fn seq<T>(
        &mut self,
        close: &str,
        item: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<(Vec<T>, bool), Error> {
        if self.eat(close) {
            return Ok((Vec::new(), false));
        }
        self.seq_after_first(close, item)
    }

    /// A field name: a quoted identifier, or a generalized identifier (words of identifier or
    /// keyword characters separated by single spaces, as in `[Unit Price]`).
    fn field_name(&mut self) -> Result<String, Error> {
        if let Tok::Ident { name, quoted: true } = self.peek() {
            let name = name.clone();
            self.next();
            return Ok(name);
        }
        let mut name = String::new();
        let mut prev_end = None;
        while let Some(t) = self.token() {
            if !matches!(t.tok, Tok::Ident { quoted: false, .. } | Tok::Keyword(_)) {
                break;
            }
            if let Some(end) = prev_end {
                if self.src.get(end..t.start) != Some(" ") {
                    break;
                }
                name.push(' ');
            }
            name.push_str(self.src.get(t.start..t.end).unwrap_or(""));
            prev_end = Some(t.end);
            self.pos = self.pos.saturating_add(1);
        }
        if name.is_empty() {
            return Err(self.unexpected("a field name"));
        }
        Ok(name)
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        let saved = self.depth;
        let result = self.primary_inner();
        self.depth = saved;
        result
    }

    fn primary_inner(&mut self) -> Result<Expr, Error> {
        match self.next() {
            Tok::Ident { name, .. } => Ok(Expr::Ident(name)),
            Tok::Text(s) => Ok(Expr::Text(s)),
            Tok::Number(n) => Ok(Expr::Number(n)),
            Tok::Keyword("true") => Ok(Expr::Logical(true)),
            Tok::Keyword("false") => Ok(Expr::Logical(false)),
            Tok::Keyword("null") => Ok(Expr::Null),
            Tok::Keyword("type") => {
                let name = match self.next() {
                    Tok::Ident {
                        name,
                        quoted: false,
                    } => name,
                    Tok::Keyword("null") => "null".into(),
                    _ => String::new(),
                };
                match PRIMITIVE_TYPES.iter().find(|t| **t == name) {
                    Some(t) => Ok(Expr::Type((*t).into())),
                    None => {
                        self.pos = self.pos.saturating_sub(1);
                        Err(self.unexpected("a primitive type name (`text`, `number`…)"))
                    }
                }
            }
            Tok::Punct("(") => {
                self.enter()?;
                let e = self.expr()?;
                self.expect(")")?;
                Ok(Expr::Paren(Box::new(e)))
            }
            Tok::Punct("{") => {
                self.enter()?;
                let (items, tight) = self.seq("}", Self::expr)?;
                Ok(Expr::List { items, tight })
            }
            Tok::Punct("[") => {
                self.enter()?;
                if self.eat("]") {
                    return Ok(Expr::Record {
                        fields: Vec::new(),
                        tight: false,
                    });
                }
                let first = self.field_name()?;
                if self.eat("]") {
                    return Ok(Expr::Field(None, first));
                }
                self.expect("=")?;
                let mut first = Some(first);
                let (fields, tight) = self.seq_after_first("]", |p| {
                    let name = match first.take() {
                        Some(n) => n,
                        None => {
                            let n = p.field_name()?;
                            p.expect("=")?;
                            n
                        }
                    };
                    Ok((name, p.expr()?))
                })?;
                Ok(Expr::Record { fields, tight })
            }
            _ => {
                self.pos = self.pos.saturating_sub(1);
                Err(self.unexpected("an expression"))
            }
        }
    }

    /// [`Self::seq`] for a sequence known not to be empty.
    fn seq_after_first<T>(
        &mut self,
        close: &str,
        mut item: impl FnMut(&mut Self) -> Result<T, Error>,
    ) -> Result<(Vec<T>, bool), Error> {
        let mut items = Vec::new();
        let mut tight = None;
        loop {
            items.push(item(self)?);
            if self.eat(",") {
                tight.get_or_insert(self.glued());
            } else if self.eat(close) {
                return Ok((items, tight.unwrap_or(false)));
            } else {
                return Err(self.unexpected(&format!("`,` or `{close}`")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print;

    fn round_trip(src: &str) {
        let e = parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(print(&e), src);
    }

    #[test]
    fn round_trips_power_query_text() {
        round_trip(
            "let\n    Source = Csv.Document(File.Contents(\"C:\\Data\\sales.csv\"),[Delimiter=\",\", Columns=3, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),\n    #\"Promoted Headers\" = Table.PromoteHeaders(Source, [PromoteAllScalars=true]),\n    #\"Changed Type\" = Table.TransformColumnTypes(#\"Promoted Headers\",{{\"Region\", type text}, {\"Qty\", Int64.Type}})\nin\n    #\"Changed Type\"",
        );
        round_trip("Source{[Item=\"Sheet1\",Kind=\"Sheet\"]}[Data]");
        round_trip("Table.SelectRows(T, each ([Unit Price] > 5 and not [Flag]) or [x] = null)");
        round_trip("{1, 0.5, -2, true, false, null, type number, \"a\"\"#(lf)\"}");
        round_trip("[]");
        round_trip("{}");
        round_trip("let\n    a = let\n        b = 1\n    in\n        b\nin\n    a & \"x\"");
        round_trip("1 + 2 * 3 - 4 / 5");
        round_trip("[#\"Qty (x)\"]");
    }

    #[test]
    fn reads_escapes_numbers_and_spacing() {
        assert_eq!(
            parse("\"#(cr,lf)#(#)(#(0041)#(00000042)\"").unwrap(),
            Expr::Text("\r\n#(AB".into())
        );
        assert_eq!(parse("0xff").unwrap(), Expr::Number(255.0));
        assert_eq!(parse("1.5e3").unwrap(), Expr::Number(1500.0));
        assert_eq!(parse(".5").unwrap(), Expr::Number(0.5));
        // Whitespace is free; printing normalises it.
        let e = parse("let  a=f( 1 , 2 )\r\n in a").unwrap();
        assert_eq!(print(&e), "let\n    a = f(1, 2)\nin\n    a");
        assert_eq!(
            parse("#\"a b\"").unwrap(),
            Expr::Ident("a b".into()),
            "quoted identifier"
        );
    }

    #[test]
    fn errors_name_the_line() {
        let cases = [
            (
                "let\n    a = 1,\n    b = if a then 1 else 2\nin\n    b",
                3,
                "`if`",
            ),
            ("let\n    a = 1\n    b = 2\nin\n    b", 3, "`,` or `in`"),
            ("let\n    a = \"open\nin a", 2, "closing"),
            ("let\n  a = 1 // note\nin a", 2, "comments"),
            ("{1,\n2,\n#table({},{})}", 3, "`#`"),
            ("\"#(xyz)\"", 1, "escape"),
            ("type foo", 1, "primitive type"),
            ("1e99999", 1, "out of range"),
            ("f(1", 1, "`,` or `)`"),
            ("a b", 1, "the end"),
            ("", 1, "an expression"),
        ];
        for (src, line, needle) in cases {
            let e = parse(src).expect_err(src);
            assert_eq!(e.line, line, "{src}: {e}");
            assert!(e.message.contains(needle), "{src}: {e}");
        }
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed() {
        for open in ["(", "{", "[A=", "-", "not ", "each ", "f(", "let a="] {
            let src = open.repeat(100_000);
            let e = parse(&src).expect_err(open);
            assert!(e.message.contains("nested"), "{open}: {e}");
        }
        // Long left-leaning operator chains are bounded too.
        let chain = vec!["1"; 100_000].join(" + ");
        assert!(parse(&chain).expect_err("chain").message.contains("nested"));
        let posts = format!("a{}", "[b]".repeat(100_000));
        assert!(
            parse(&posts)
                .expect_err("postfix")
                .message
                .contains("nested")
        );
        // But reasonable nesting is fine.
        assert!(parse(&format!("{}1{}", "(".repeat(20), ")".repeat(20))).is_ok());
    }

    #[test]
    fn oversized_input_is_rejected() {
        let big = " ".repeat(MAX_SOURCE + 1);
        assert!(parse(&big).is_err());
    }

    /// 20,000 cases a run; soak with `ODASH_FUZZ_SCALE` / `ODASH_FUZZ_SEED` (see `crate::fuzz`).
    #[test]
    fn fuzzed_input_never_panics() {
        let seeds = [
            "let\n    Source = Csv.Document(File.Contents(\"a.csv\"),[Delimiter=\",\", Columns=3, Encoding=65001, QuoteStyle=QuoteStyle.Csv]),\n    #\"Promoted Headers\" = Table.PromoteHeaders(Source, [PromoteAllScalars=true])\nin\n    #\"Promoted Headers\"",
            "Source{[Item=\"Sheet1\",Kind=\"Sheet\"]}[Data]",
            "each ([A] > 0.5e3 and not [B]) or -[C] <> null & \"#(cr,lf)\"",
            "Table.SelectRows(#\"Merged Queries1\", each [Day] < #date(2024, 10, 4) or [At] <> #datetime(2023, 11, 14, 22, 13, 20.123456) or Text.StartsWith([R], \"N\"\"o#(#)(x)\"))",
            "Table.TransformColumnTypes(Source,{{\"A\", type text}, {\"B\", Int64.Type}}, \"en-GB\")",
        ];
        let alphabet: Vec<char> = "#\"()[]{},=<>+-*/&.e0x9_ \n\r\tlet in each not type"
            .chars()
            .collect();
        let mut rng = crate::fuzz::Fuzz::new("m-parse", 0x9E37_79B9_7F4A_7C15);
        for i in 0..rng.scaled(20_000) {
            let src: String = if i % 4 == 0 {
                // Random bytes, lossily decoded.
                let len = rng.below(64);
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
            // Whatever parses must print to text that parses back to the same tree.
            rng.case(src.as_bytes(), || {
                if let Ok(e) = parse(&src) {
                    let printed = print(&e);
                    let again = parse(&printed)
                        .unwrap_or_else(|err| panic!("{src:?} → {printed:?}: {err}"));
                    assert_eq!(print(&again), printed, "{src:?}");
                }
            });
        }
        rng.finish();
    }
}
