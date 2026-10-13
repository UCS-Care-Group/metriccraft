//! Splits DAX text into tokens. Input is hostile: nothing indexes, every error has a span.

use crate::{Error, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    /// A regular name (`SUM`, `Sales`, `NORM.DIST`, keywords).
    Name(String),
    /// `'Sales Data'`, with `''` resolved.
    Quoted(String),
    /// `[Amount]`, with `]]` resolved.
    Bracket(String),
    Text(String),
    Integer(i64),
    Decimal(f64),
    /// Operators and punctuators.
    Punct(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
}

/// Longest first, so `<=` wins over `<`.
const PUNCTS: &[&str] = &[
    "==", "<>", "<=", ">=", "&&", "||", "=", "<", ">", "+", "-", "*", "/", "^", "&", "(", ")", "{",
    "}", ",",
];

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
}

/// Tokens of `src`, ending with [`Tok::Eof`]. Comments (`//`, `--`, `/* */`) are skipped.
pub fn tokens(src: &str) -> Result<Vec<Token>, Error> {
    let mut lx = Lexer { src, pos: 0 };
    let mut out = Vec::new();
    loop {
        lx.skip_blank()?;
        let start = lx.pos;
        let tok = match lx.peek() {
            None => Tok::Eof,
            Some(c) => lx.token(c)?,
        };
        let done = tok == Tok::Eof;
        out.push(Token {
            tok,
            span: Span::new(start, lx.pos),
        });
        if done {
            return Ok(out);
        }
    }
}

impl Lexer<'_> {
    fn rest(&self) -> &str {
        self.src.get(self.pos..).unwrap_or("")
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn peek2(&self) -> Option<char> {
        self.rest().chars().nth(1)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos = self.pos.saturating_add(c.len_utf8());
        Some(c)
    }

    fn err_from(&self, start: usize, message: impl Into<String>) -> Error {
        Error::new(Span::new(start, self.pos), message)
    }

    fn skip_blank(&mut self) -> Result<(), Error> {
        loop {
            let rest = self.rest();
            if self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            } else if rest.starts_with("//") || rest.starts_with("--") {
                while self.peek().is_some_and(|c| c != '\n' && c != '\r') {
                    self.bump();
                }
            } else if rest.starts_with("/*") {
                let start = self.pos;
                match rest.get(2..).and_then(|r| r.find("*/")) {
                    Some(i) => self.pos = self.pos.saturating_add(i).saturating_add(4),
                    None => {
                        self.pos = self.src.len();
                        return Err(self.err_from(start, "comment has no closing `*/`"));
                    }
                }
            } else {
                return Ok(());
            }
        }
    }

    fn token(&mut self, c: char) -> Result<Tok, Error> {
        let start = self.pos;
        match c {
            '"' => {
                return self
                    .delimited('"', "text has no closing `\"`")
                    .map(Tok::Text);
            }
            '\'' => {
                let name = self.delimited('\'', "table name has no closing `'`")?;
                return self.non_empty(start, name, "a table name").map(Tok::Quoted);
            }
            '[' => {
                let name = self.delimited(']', "column name has no closing `]`")?;
                return self
                    .non_empty(start, name, "a column name")
                    .map(Tok::Bracket);
            }
            _ => {}
        }
        if c.is_ascii_digit() || (c == '.' && self.peek2().is_some_and(|d| d.is_ascii_digit())) {
            return self.number();
        }
        if c.is_alphabetic() || c == '_' {
            return Ok(Tok::Name(self.name()));
        }
        let rest = self.rest();
        if let Some(p) = PUNCTS.iter().find(|p| rest.starts_with(**p)) {
            self.pos = self.pos.saturating_add(p.len());
            return Ok(Tok::Punct(p));
        }
        self.bump();
        Err(self.err_from(start, format!("unexpected character `{c}`")))
    }

    fn non_empty(&self, start: usize, name: String, what: &str) -> Result<String, Error> {
        if name.trim().is_empty() {
            Err(self.err_from(start, format!("{what} is empty")))
        } else {
            Ok(name)
        }
    }

    /// Segments of letters, digits and `_`, joined by dots (`NORM.DIST`).
    fn name(&mut self) -> String {
        let mut name = String::new();
        loop {
            while let Some(c) = self.peek().filter(|c| c.is_alphanumeric() || *c == '_') {
                name.push(c);
                self.bump();
            }
            if self.peek() == Some('.')
                && self
                    .peek2()
                    .is_some_and(|c| c.is_alphanumeric() || c == '_')
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
        let mut decimal = false;
        self.digits();
        if self.peek() == Some('.') {
            decimal = true;
            self.bump();
            self.digits();
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            decimal = true;
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                self.bump();
            }
            if self.digits() == 0 {
                return Err(self.err_from(start, "exponent needs digits"));
            }
        }
        let lexeme = self.src.get(start..self.pos).unwrap_or("");
        if !decimal && let Ok(n) = lexeme.parse::<i64>() {
            return Ok(Tok::Integer(n));
        }
        match lexeme.parse::<f64>() {
            Ok(n) if n.is_finite() => Ok(Tok::Decimal(n)),
            _ => Err(self.err_from(start, format!("number `{lexeme}` is out of range"))),
        }
    }

    fn digits(&mut self) -> usize {
        let mut n = 0usize;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.bump();
            n = n.saturating_add(1);
        }
        n
    }

    /// The body of a `close`-delimited token (the opener is next); a doubled `close` is one.
    fn delimited(&mut self, close: char, unclosed: &str) -> Result<String, Error> {
        let start = self.pos;
        self.bump();
        let mut s = String::new();
        loop {
            match self.bump() {
                None => return Err(self.err_from(start, unclosed)),
                Some(c) if c == close && self.peek() == Some(close) => {
                    self.bump();
                    s.push(close);
                }
                Some(c) if c == close => return Ok(s),
                Some(c) => s.push(c),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        tokens(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn reads_names_literals_and_operators() {
        assert_eq!(
            toks("'Sales ''24'[Net]]x] NORM.DIST(1.5e3, .5, 7) \"a\"\"b\" <=&&"),
            vec![
                Tok::Quoted("Sales '24".into()),
                Tok::Bracket("Net]x".into()),
                Tok::Name("NORM.DIST".into()),
                Tok::Punct("("),
                Tok::Decimal(1500.0),
                Tok::Punct(","),
                Tok::Decimal(0.5),
                Tok::Punct(","),
                Tok::Integer(7),
                Tok::Punct(")"),
                Tok::Text("a\"b".into()),
                Tok::Punct("<="),
                Tok::Punct("&&"),
                Tok::Eof,
            ]
        );
        assert_eq!(
            toks("99999999999999999999"),
            vec![Tok::Decimal(1e20), Tok::Eof]
        );
    }

    #[test]
    fn skips_comments() {
        assert_eq!(
            toks("1 // a\n-- b\r\n/* c\n */ 2"),
            vec![Tok::Integer(1), Tok::Integer(2), Tok::Eof]
        );
    }

    #[test]
    fn errors_have_spans() {
        let cases = [
            ("1 + \"open", 4, "closing `\"`"),
            ("'T", 0, "closing `'`"),
            ("x[col", 1, "closing `]`"),
            ("[]", 0, "empty"),
            ("1 /* x", 2, "`*/`"),
            ("1e", 0, "exponent"),
            ("1e999", 0, "out of range"),
            ("a ; b", 2, "`;`"),
        ];
        for (src, start, needle) in cases {
            let e = tokens(src).expect_err(src);
            assert_eq!(e.span.start, start, "{src}: {e}");
            assert!(e.message.contains(needle), "{src}: {e}");
        }
    }
}
