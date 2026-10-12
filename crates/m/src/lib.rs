//! M-subset AST, printer and parser.
//!
//! Layer L1 — see `docs/plan/0001-mvp.md` §1.
//!
//! The subset is what Power Query writes for the Applied Steps we support (ADR 0005): `let … in`,
//! identifiers (regular, dotted like `Table.PromoteHeaders`, and quoted `#"Promoted Headers"`),
//! text/number/logical/null literals, lists, records, calls, item access `x{…}`, field access
//! `x[F]` and `[F]`, `each`, primitive types (`type text`), unary `not`/`-`, and the logical,
//! comparison, arithmetic and `&` operators. Anything else is a parse error naming the line.

mod parse;
mod print;

pub use parse::{MAX_DEPTH, MAX_SOURCE, parse};
pub use print::{print, quote_identifier};

/// An M expression.
#[derive(Clone, Debug, PartialEq)]
pub enum Expr {
    Let(Let),
    /// A variable: `Source`, `Table.PromoteHeaders`, `#"Changed Type"`, `Int64.Type`.
    Ident(String),
    Text(String),
    Number(f64),
    Logical(bool),
    Null,
    /// A primitive type: `type text` is `Type("text")`.
    Type(String),
    /// `{a, b}`; `tight` prints `{a,b}` (Power Query mixes both styles).
    List {
        items: Vec<Expr>,
        tight: bool,
    },
    /// `[A=1, B=2]`; `tight` prints `[A=1,B=2]`.
    Record {
        fields: Vec<(String, Expr)>,
        tight: bool,
    },
    /// `f(a, b)`; `tight` prints `f(a,b)`.
    Call {
        func: Box<Expr>,
        args: Vec<Expr>,
        tight: bool,
    },
    /// Item access `x{selector}`.
    Item(Box<Expr>, Box<Expr>),
    /// Field access `x[Field]`, or `[Field]` on `_` (inside `each`) when the target is `None`.
    Field(Option<Box<Expr>>, String),
    Each(Box<Expr>),
    Not(Box<Expr>),
    Negate(Box<Expr>),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// Parentheses the author wrote, kept so printing reproduces them.
    Paren(Box<Expr>),
}

/// `let` bindings `in` body. Lines are where each binding starts (1-based; 0 if built in code), so
/// callers mapping M to their own model can name the line in errors.
#[derive(Clone, Debug, PartialEq)]
pub struct Let {
    pub bindings: Vec<Binding>,
    pub body: Box<Expr>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub name: String,
    pub value: Expr,
    pub line: usize,
    pub column: usize,
}

impl Binding {
    pub fn new(name: impl Into<String>, value: Expr) -> Self {
        Self {
            name: name.into(),
            value,
            line: 0,
            column: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Add,
    Subtract,
    Concat,
    Multiply,
    Divide,
}

impl BinOp {
    /// Binding strength (M spec: or < and < equality < relational < additive < multiplicative).
    pub fn precedence(self) -> u8 {
        match self {
            Self::Or => 1,
            Self::And => 2,
            Self::Equal | Self::NotEqual => 3,
            Self::Less | Self::LessEqual | Self::Greater | Self::GreaterEqual => 4,
            Self::Add | Self::Subtract | Self::Concat => 5,
            Self::Multiply | Self::Divide => 6,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::Equal => "=",
            Self::NotEqual => "<>",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Concat => "&",
            Self::Multiply => "*",
            Self::Divide => "/",
        }
    }
}

/// Convenience constructors for building M in code.
impl Expr {
    pub fn ident(name: impl Into<String>) -> Self {
        Self::Ident(name.into())
    }

    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn call(func: &str, args: Vec<Expr>, tight: bool) -> Self {
        Self::Call {
            func: Box::new(Self::ident(func)),
            args,
            tight,
        }
    }

    pub fn list(items: Vec<Expr>, tight: bool) -> Self {
        Self::List { items, tight }
    }

    pub fn record(fields: Vec<(&str, Expr)>, tight: bool) -> Self {
        Self::Record {
            fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            tight,
        }
    }
}

/// A parse error, or an M-to-model mapping error, at a 1-based line and column.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl Error {
    pub fn new(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line {}, column {}: {}",
            self.line, self.column, self.message
        )
    }
}

impl std::error::Error for Error {}

/// M's keywords: a name equal to one of these prints quoted.
pub const KEYWORDS: &[&str] = &[
    "and",
    "as",
    "each",
    "else",
    "error",
    "false",
    "if",
    "in",
    "is",
    "let",
    "meta",
    "not",
    "null",
    "or",
    "otherwise",
    "section",
    "shared",
    "then",
    "true",
    "try",
    "type",
];

/// Primitive type names accepted after `type`.
pub const PRIMITIVE_TYPES: &[&str] = &[
    "any",
    "anynonnull",
    "binary",
    "date",
    "datetime",
    "datetimezone",
    "duration",
    "function",
    "list",
    "logical",
    "none",
    "null",
    "number",
    "record",
    "table",
    "text",
    "time",
    "type",
];
