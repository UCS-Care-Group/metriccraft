//! DAX lexer, parser and AST.
//!
//! Layer L1 — see `docs/plan/0001-mvp.md` §1.
//!
//! Expressions (Measures, Calculated columns) and DAX queries (`DEFINE`, `EVALUATE`, `ORDER BY`).
//! Every node carries the byte [`Span`] it was parsed from, so the evaluator can locate its errors
//! (spec X-3). Names (functions, keywords) are case-insensitive; the AST keeps the spelling the
//! author wrote. Parentheses are not kept: the printer adds them where precedence needs them.

#[cfg(test)]
#[path = "../../m/src/fuzz.rs"]
mod fuzz;
mod lex;
mod parse;
mod print;

pub use lex::{Tok, Token, tokens};
pub use parse::{MAX_DEPTH, MAX_SOURCE, parse_expr, parse_query};
pub use print::{print_expr, print_query};

/// Byte offsets `start..end` into the source text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// From the start of `self` to the end of `other`.
    pub fn to(self, other: Span) -> Self {
        Self::new(self.start, other.end.max(self.start))
    }
}

/// A DAX expression and where it came from.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExprKind {
    Integer(i64),
    /// A literal with a `.` or an exponent, or an integer too large for `i64`.
    Decimal(f64),
    Text(String),
    /// A bare name: a variable, or a Table (`Sales`, `'Sales Data'`).
    Name(String),
    /// `Sales[Amount]`, `'Sales Data'[Amount]`; `[Amount]` (no table) is a Measure, or a Column
    /// of the row context's Table.
    Column {
        table: Option<String>,
        column: String,
    },
    /// `SUM(Sales[Amount])`; `TRUE()`/`FALSE()` (and bare `TRUE`/`FALSE`) are calls too.
    Call {
        name: String,
        args: Vec<Expr>,
    },
    Unary(UnaryOp, Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `VAR a = … VAR b = … RETURN body`.
    Var {
        vars: Vec<(String, Expr)>,
        body: Box<Expr>,
    },
    /// `{1, 2}` or `{(1, "a"), (2, "b")}`: rows of one or more values.
    Table(Vec<Vec<Expr>>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnaryOp {
    Negate,
    Plus,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    /// `=`: BLANK equals 0 and "".
    Equal,
    /// `==`: strict, BLANK equals only BLANK.
    StrictEqual,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    In,
    Concat,
    Add,
    Subtract,
    Multiply,
    Divide,
    Power,
}

/// Binding strength of `NOT` (between the comparisons and `&&`).
pub const NOT_PRECEDENCE: u8 = 3;
/// Binding strength of unary `-`/`+` (looser than `^`, tighter than `*`).
pub const SIGN_PRECEDENCE: u8 = 8;

impl BinOp {
    /// Binding strength (DAX: `||` < `&&` < `NOT` < comparisons and `IN` < `&` < `+ -` < `* /`
    /// < sign < `^`). All are left-associative.
    pub fn precedence(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::And => 2,
            Self::Equal
            | Self::StrictEqual
            | Self::NotEqual
            | Self::Less
            | Self::LessEqual
            | Self::Greater
            | Self::GreaterEqual
            | Self::In => 4,
            Self::Concat => 5,
            Self::Add | Self::Subtract => 6,
            Self::Multiply | Self::Divide => 7,
            Self::Power => 9,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Or => "||",
            Self::And => "&&",
            Self::Equal => "=",
            Self::StrictEqual => "==",
            Self::NotEqual => "<>",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::In => "IN",
            Self::Concat => "&",
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Power => "^",
        }
    }
}

/// A DAX query: optional `DEFINE` definitions, then one or more `EVALUATE`s.
#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub defines: Vec<Define>,
    pub evaluates: Vec<Evaluate>,
}

/// One definition after `DEFINE`; `span` covers it from its keyword.
#[derive(Clone, Debug, PartialEq)]
pub enum Define {
    /// `MEASURE Sales[Total] = …`.
    Measure {
        table: String,
        name: String,
        expr: Expr,
        span: Span,
    },
    /// `VAR x = …`, visible to later definitions and every `EVALUATE`.
    Var {
        name: String,
        expr: Expr,
        span: Span,
    },
}

/// `EVALUATE table [ORDER BY key, …]`; `span` covers it from `EVALUATE`.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluate {
    pub table: Expr,
    pub order_by: Vec<OrderKey>,
    pub span: Span,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrderKey {
    pub expr: Expr,
    pub descending: bool,
}

/// A parse error at a byte span; [`Error::line_column`] turns it into a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub span: Span,
    pub message: String,
}

impl Error {
    pub fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }

    /// The 1-based line and column where the error starts in `src`.
    pub fn line_column(&self, src: &str) -> (usize, usize) {
        line_column(src, self.span.start)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at byte {})", self.message, self.span.start)
    }
}

impl std::error::Error for Error {}

/// The 1-based line and column (in characters) of byte `offset` in `src`. CR LF, CR and LF each
/// end a line; an offset past the end is the end.
pub fn line_column(src: &str, offset: usize) -> (usize, usize) {
    let (mut line, mut column) = (1usize, 1usize);
    let mut chars = src.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if i >= offset {
            break;
        }
        if c == '\n' || (c == '\r' && chars.peek().map(|(_, n)| *n) != Some('\n')) {
            line = line.saturating_add(1);
            column = 1;
        } else if c != '\r' {
            column = column.saturating_add(1);
        }
    }
    (line, column)
}

/// Words with a meaning in DAX syntax: a Table with one of these names prints quoted, and none is
/// read as a bare name. Compared case-insensitively.
pub const KEYWORDS: &[&str] = &[
    "ASC", "AT", "BY", "COLUMN", "DEFINE", "DESC", "EVALUATE", "FUNCTION", "IN", "MEASURE", "NOT",
    "ORDER", "RETURN", "START", "TABLE", "VAR",
];

/// Whether `word` is one of [`KEYWORDS`], in any case.
pub fn is_keyword(word: &str) -> bool {
    KEYWORDS.iter().any(|k| k.eq_ignore_ascii_case(word))
}

/// A copy of `e` with every span zeroed, to compare trees ignoring where they came from.
#[cfg(test)]
fn unspanned(e: &Expr) -> Expr {
    fn clear(e: &mut Expr) {
        e.span = Span::default();
        match &mut e.kind {
            ExprKind::Call { args, .. } => args.iter_mut().for_each(clear),
            ExprKind::Unary(_, x) => clear(x),
            ExprKind::Binary(_, l, r) => {
                clear(l);
                clear(r);
            }
            ExprKind::Var { vars, body } => {
                vars.iter_mut().for_each(|(_, v)| clear(v));
                clear(body);
            }
            ExprKind::Table(rows) => rows.iter_mut().flatten().for_each(clear),
            _ => {}
        }
    }
    let mut e = e.clone();
    clear(&mut e);
    e
}

#[cfg(test)]
fn unspanned_query(q: &Query) -> Query {
    let mut q = q.clone();
    for d in &mut q.defines {
        match d {
            Define::Measure { expr, span, .. } | Define::Var { expr, span, .. } => {
                *expr = unspanned(expr);
                *span = Span::default();
            }
        }
    }
    for ev in &mut q.evaluates {
        ev.table = unspanned(&ev.table);
        ev.span = Span::default();
        for k in &mut ev.order_by {
            k.expr = unspanned(&k.expr);
        }
    }
    q
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_column_counts_characters_and_newlines() {
        let src = "a\r\nbé\rc\nd";
        assert_eq!(line_column(src, 0), (1, 1));
        assert_eq!(line_column(src, 3), (2, 1));
        // `é` is two bytes but one column.
        assert_eq!(line_column(src, 6), (2, 3));
        assert_eq!(line_column(src, 7), (3, 1));
        assert_eq!(line_column(src, 9), (4, 1));
        assert_eq!(line_column(src, 999), (4, 2));
    }
}
