//! Parses DAX expressions and queries. Input is hostile: sizes and nesting are capped, nothing
//! indexes.

use crate::lex::{Tok, Token, tokens};
use crate::{
    BinOp, Define, Error, Evaluate, Expr, ExprKind, NOT_PRECEDENCE, OrderKey, Query,
    SIGN_PRECEDENCE, Span, UnaryOp, is_keyword,
};

/// Deepest expression nesting accepted (brackets, calls, operators, `VAR`…; spec N-1).
pub const MAX_DEPTH: usize = 200;
/// Largest DAX text accepted, in bytes (a Measure or query is a few KB).
pub const MAX_SOURCE: usize = 1 << 20;

/// Parses a DAX expression (a Measure's or Calculated column's formula).
pub fn parse_expr(src: &str) -> Result<Expr, Error> {
    let mut p = Parser::new(src)?;
    let e = p.expr()?;
    p.end()?;
    Ok(e)
}

/// Parses a DAX query: `[DEFINE (MEASURE T[M] = … | VAR v = …)…] EVALUATE table [ORDER BY …]…`.
pub fn parse_query(src: &str) -> Result<Query, Error> {
    let mut p = Parser::new(src)?;
    let defines = if p.keyword("DEFINE") {
        p.defines()?
    } else {
        Vec::new()
    };
    let mut evaluates = Vec::new();
    loop {
        let start = p.span();
        if !p.keyword("EVALUATE") {
            break;
        }
        let table = p.expr()?;
        let mut order_by = Vec::new();
        if p.keyword("ORDER") {
            if !p.keyword("BY") {
                return Err(p.unexpected("`BY`"));
            }
            loop {
                let expr = p.expr()?;
                let descending = p.keyword("DESC");
                if !descending {
                    p.keyword("ASC");
                }
                order_by.push(OrderKey { expr, descending });
                if !p.eat(",") {
                    break;
                }
            }
        }
        if p.at_keyword("START") {
            return Err(p.err("`START AT` is not supported"));
        }
        evaluates.push(Evaluate {
            table,
            order_by,
            span: start.to(p.prev_span()),
        });
    }
    if evaluates.is_empty() {
        return Err(p.unexpected("`EVALUATE`"));
    }
    p.end()?;
    Ok(Query { defines, evaluates })
}

fn is_bool(name: &str) -> bool {
    name.eq_ignore_ascii_case("TRUE") || name.eq_ignore_ascii_case("FALSE")
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
}

impl Parser {
    fn new(src: &str) -> Result<Self, Error> {
        if src.len() > MAX_SOURCE {
            return Err(Error::new(
                Span::default(),
                format!("DAX text is larger than {} MB", MAX_SOURCE >> 20),
            ));
        }
        Ok(Self {
            tokens: tokens(src)?,
            pos: 0,
            depth: 0,
        })
    }

    fn token(&self) -> Option<&Token> {
        self.tokens.get(self.pos).or_else(|| self.tokens.last())
    }

    fn peek(&self) -> &Tok {
        self.token().map_or(&Tok::Eof, |t| &t.tok)
    }

    fn span(&self) -> Span {
        self.token().map_or(Span::default(), |t| t.span)
    }

    /// The span of the token just consumed.
    fn prev_span(&self) -> Span {
        self.tokens
            .get(self.pos.wrapping_sub(1))
            .map_or(Span::default(), |t| t.span)
    }

    fn next(&mut self) -> Tok {
        let t = self.peek().clone();
        if t != Tok::Eof {
            self.pos = self.pos.saturating_add(1);
        }
        t
    }

    fn back(&mut self) {
        self.pos = self.pos.saturating_sub(1);
    }

    fn err(&self, message: impl Into<String>) -> Error {
        Error::new(self.span(), message)
    }

    fn unexpected(&self, wanted: &str) -> Error {
        let found = match self.peek() {
            Tok::Name(n) => format!("`{n}`"),
            Tok::Quoted(n) => format!("`'{n}'`"),
            Tok::Bracket(n) => format!("`[{n}]`"),
            Tok::Text(_) => "text".into(),
            Tok::Integer(_) | Tok::Decimal(_) => "a number".into(),
            Tok::Punct(p) => format!("`{p}`"),
            Tok::Eof => "the end".into(),
        };
        self.err(format!("expected {wanted}, found {found}"))
    }

    fn end(&self) -> Result<(), Error> {
        match self.peek() {
            Tok::Eof => Ok(()),
            _ => Err(self.unexpected("the end")),
        }
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

    fn at_keyword(&self, k: &str) -> bool {
        matches!(self.peek(), Tok::Name(n) if n.eq_ignore_ascii_case(k))
    }

    fn keyword(&mut self, k: &str) -> bool {
        let at = self.at_keyword(k);
        if at {
            self.pos = self.pos.saturating_add(1);
        }
        at
    }

    /// One level deeper; errors past [`MAX_DEPTH`]. Callers restore `self.depth` on return.
    fn enter(&mut self) -> Result<(), Error> {
        self.depth = self.depth.saturating_add(1);
        if self.depth > MAX_DEPTH {
            return Err(self.err(format!("nested more than {MAX_DEPTH} levels deep")));
        }
        Ok(())
    }

    fn defines(&mut self) -> Result<Vec<Define>, Error> {
        let mut out = Vec::new();
        loop {
            let start = self.span();
            if self.keyword("MEASURE") {
                let (table, name) = self.measure_name()?;
                self.expect("=")?;
                let expr = self.expr()?;
                out.push(Define::Measure {
                    table,
                    name,
                    span: start.to(expr.span),
                    expr,
                });
            } else if self.keyword("VAR") {
                let name = self.var_name()?;
                self.expect("=")?;
                let expr = self.expr()?;
                out.push(Define::Var {
                    name,
                    span: start.to(expr.span),
                    expr,
                });
            } else if let Some(k) = ["TABLE", "COLUMN", "FUNCTION"]
                .into_iter()
                .find(|k| self.at_keyword(k))
            {
                return Err(self.err(format!("`DEFINE {k}` is not supported")));
            } else if out.is_empty() {
                return Err(self.unexpected("`MEASURE` or `VAR`"));
            } else {
                return Ok(out);
            }
        }
    }

    /// `Sales[Name]` or `'Sales Data'[Name]` after `MEASURE`.
    fn measure_name(&mut self) -> Result<(String, String), Error> {
        let table = match self.peek() {
            Tok::Name(n) if !is_keyword(n) => n.clone(),
            Tok::Quoted(n) => n.clone(),
            Tok::Bracket(_) => {
                return Err(self.err("a Measure needs its Table: `MEASURE Sales[Name] = …`"));
            }
            _ => return Err(self.unexpected("a Table name")),
        };
        self.next();
        match self.next() {
            Tok::Bracket(name) => Ok((table, name)),
            _ => {
                self.back();
                Err(self.unexpected("`[Measure name]`"))
            }
        }
    }

    fn var_name(&mut self) -> Result<String, Error> {
        match self.peek() {
            Tok::Name(n) if !is_keyword(n) && !is_bool(n) && !n.contains('.') => {
                let n = n.clone();
                self.next();
                Ok(n)
            }
            _ => Err(self.unexpected("a variable name")),
        }
    }

    fn expr(&mut self) -> Result<Expr, Error> {
        self.binary(1)
    }

    fn binary(&mut self, min: u8) -> Result<Expr, Error> {
        let lhs = self.prefix()?;
        self.binary_rest(lhs, min)
    }

    fn binop(&self) -> Option<BinOp> {
        Some(match self.peek() {
            Tok::Punct("||") => BinOp::Or,
            Tok::Punct("&&") => BinOp::And,
            Tok::Punct("=") => BinOp::Equal,
            Tok::Punct("==") => BinOp::StrictEqual,
            Tok::Punct("<>") => BinOp::NotEqual,
            Tok::Punct("<") => BinOp::Less,
            Tok::Punct("<=") => BinOp::LessEqual,
            Tok::Punct(">") => BinOp::Greater,
            Tok::Punct(">=") => BinOp::GreaterEqual,
            Tok::Name(n) if n.eq_ignore_ascii_case("IN") => BinOp::In,
            Tok::Punct("&") => BinOp::Concat,
            Tok::Punct("+") => BinOp::Add,
            Tok::Punct("-") => BinOp::Subtract,
            Tok::Punct("*") => BinOp::Multiply,
            Tok::Punct("/") => BinOp::Divide,
            Tok::Punct("^") => BinOp::Power,
            _ => return None,
        })
    }

    /// Precedence climbing from `lhs`; every operator applied counts as a nesting level so
    /// left-leaning chains (`a + b + c …`) are bounded too.
    fn binary_rest(&mut self, lhs: Expr, min: u8) -> Result<Expr, Error> {
        let saved = self.depth;
        let result = (|| {
            let mut lhs = lhs;
            while let Some(op) = self.binop().filter(|op| op.precedence() >= min) {
                self.next();
                self.enter()?;
                let rhs = self.binary(op.precedence().saturating_add(1))?;
                let span = lhs.span.to(rhs.span);
                lhs = Expr {
                    kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
                    span,
                };
            }
            Ok(lhs)
        })();
        self.depth = saved;
        result
    }

    /// An operand: `VAR … RETURN`, `NOT`, a sign, or a primary.
    fn prefix(&mut self) -> Result<Expr, Error> {
        let saved = self.depth;
        let result = self.prefix_inner();
        self.depth = saved;
        result
    }

    fn prefix_inner(&mut self) -> Result<Expr, Error> {
        self.enter()?;
        let start = self.span();
        if self.keyword("VAR") {
            return self.var_block(start);
        }
        let op = match self.peek() {
            Tok::Name(n) if n.eq_ignore_ascii_case("NOT") => UnaryOp::Not,
            Tok::Punct("-") => UnaryOp::Negate,
            Tok::Punct("+") => UnaryOp::Plus,
            _ => return self.primary(),
        };
        self.next();
        let operand = self.binary(match op {
            UnaryOp::Not => NOT_PRECEDENCE.saturating_add(1),
            _ => SIGN_PRECEDENCE,
        })?;
        let span = start.to(operand.span);
        Ok(Expr {
            kind: ExprKind::Unary(op, Box::new(operand)),
            span,
        })
    }

    fn var_block(&mut self, start: Span) -> Result<Expr, Error> {
        let mut vars = Vec::new();
        loop {
            let name = self.var_name()?;
            self.expect("=")?;
            vars.push((name, self.expr()?));
            if !self.keyword("VAR") {
                break;
            }
        }
        if !self.keyword("RETURN") {
            return Err(self.unexpected("`VAR` or `RETURN`"));
        }
        let body = self.expr()?;
        let span = start.to(body.span);
        Ok(Expr {
            kind: ExprKind::Var {
                vars,
                body: Box::new(body),
            },
            span,
        })
    }

    fn primary(&mut self) -> Result<Expr, Error> {
        let start = self.span();
        let kind = match self.next() {
            Tok::Integer(n) => ExprKind::Integer(n),
            Tok::Decimal(n) => ExprKind::Decimal(n),
            Tok::Text(s) => ExprKind::Text(s),
            Tok::Bracket(column) => ExprKind::Column {
                table: None,
                column,
            },
            Tok::Quoted(name) => self.name_or_column(name),
            Tok::Name(name) if is_keyword(&name) => {
                self.back();
                return Err(self.unexpected("an expression"));
            }
            Tok::Name(name) => {
                if self.eat("(") {
                    self.enter()?;
                    let args = self.args()?;
                    ExprKind::Call { name, args }
                } else if is_bool(&name) {
                    ExprKind::Call {
                        name,
                        args: Vec::new(),
                    }
                } else {
                    self.name_or_column(name)
                }
            }
            Tok::Punct("(") => {
                self.enter()?;
                let e = self.expr()?;
                self.expect(")")?;
                e.kind
            }
            Tok::Punct("{") => {
                self.enter()?;
                ExprKind::Table(self.rows()?)
            }
            _ => {
                self.back();
                return Err(self.unexpected("an expression"));
            }
        };
        Ok(Expr {
            kind,
            span: start.to(self.prev_span()),
        })
    }

    /// `T[Column]` when a `[…]` follows the table name, else the bare name.
    fn name_or_column(&mut self, name: String) -> ExprKind {
        if let Tok::Bracket(column) = self.peek() {
            let column = column.clone();
            self.next();
            ExprKind::Column {
                table: Some(name),
                column,
            }
        } else {
            ExprKind::Name(name)
        }
    }

    /// Call arguments after `(`.
    fn args(&mut self) -> Result<Vec<Expr>, Error> {
        let mut args = Vec::new();
        if self.eat(")") {
            return Ok(args);
        }
        loop {
            args.push(self.expr()?);
            if self.eat(",") {
                continue;
            }
            if self.eat(")") {
                return Ok(args);
            }
            return Err(self.unexpected("`,` or `)`"));
        }
    }

    /// Table constructor rows after `{`.
    fn rows(&mut self) -> Result<Vec<Vec<Expr>>, Error> {
        let mut rows = Vec::new();
        if self.eat("}") {
            return Ok(rows);
        }
        loop {
            rows.push(self.row()?);
            if self.eat(",") {
                continue;
            }
            if self.eat("}") {
                return Ok(rows);
            }
            return Err(self.unexpected("`,` or `}`"));
        }
    }

    /// One row: a value, or a tuple `(a, b, …)`. `(a)` is a parenthesised value, which may go on
    /// as an expression (`(a) + 1`) — read without backtracking, so nesting stays linear.
    fn row(&mut self) -> Result<Vec<Expr>, Error> {
        let start = self.span();
        if !self.eat("(") {
            return Ok(vec![self.expr()?]);
        }
        let saved = self.depth;
        let result = (|| {
            self.enter()?;
            let first = self.expr()?;
            if !self.eat(",") {
                self.expect(")")?;
                let first = Expr {
                    kind: first.kind,
                    span: start.to(self.prev_span()),
                };
                return Ok(vec![self.binary_rest(first, 1)?]);
            }
            let mut row = vec![first];
            loop {
                row.push(self.expr()?);
                if self.eat(",") {
                    continue;
                }
                if self.eat(")") {
                    return Ok(row);
                }
                return Err(self.unexpected("`,` or `)`"));
            }
        })();
        self.depth = saved;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{print_expr, print_query, unspanned, unspanned_query};

    fn dax_block(md: &str) -> &str {
        let body = md.split("```dax\n").nth(1).unwrap();
        body.split("\n```").next().unwrap()
    }

    /// A fully bracketed form of the tree, for checking precedence and associativity.
    fn shape(e: &Expr) -> String {
        match &e.kind {
            ExprKind::Integer(n) => n.to_string(),
            ExprKind::Decimal(n) => format!("{n:?}"),
            ExprKind::Text(s) => format!("{s:?}"),
            ExprKind::Name(n) => n.clone(),
            ExprKind::Column { table, column } => {
                format!("{}[{column}]", table.as_deref().unwrap_or(""))
            }
            ExprKind::Call { name, args } => {
                let args: Vec<_> = args.iter().map(shape).collect();
                format!("{name}({})", args.join(" "))
            }
            ExprKind::Unary(op, e) => format!("({op:?} {})", shape(e)),
            ExprKind::Binary(op, l, r) => format!("({} {} {})", op.symbol(), shape(l), shape(r)),
            ExprKind::Var { vars, body } => {
                let vars: Vec<_> = vars
                    .iter()
                    .map(|(n, v)| format!("{n}={}", shape(v)))
                    .collect();
                format!("(var {} {})", vars.join(" "), shape(body))
            }
            ExprKind::Table(rows) => {
                let rows: Vec<_> = rows
                    .iter()
                    .map(|r| r.iter().map(shape).collect::<Vec<_>>().join(" "))
                    .collect();
                format!("{{{}}}", rows.join("; "))
            }
        }
    }

    fn assert_round_trips_expr(src: &str) {
        let e = parse_expr(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        let printed = print_expr(&e);
        let again = parse_expr(&printed).unwrap_or_else(|e| panic!("{printed}: {e}"));
        assert_eq!(unspanned(&again), unspanned(&e), "{src} → {printed}");
        assert_eq!(print_expr(&again), printed, "{src}");
    }

    fn assert_round_trips_query(src: &str) {
        let q = parse_query(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        let printed = print_query(&q);
        let again = parse_query(&printed).unwrap_or_else(|e| panic!("{printed}: {e}"));
        assert_eq!(
            unspanned_query(&again),
            unspanned_query(&q),
            "{src} → {printed}"
        );
        assert_eq!(print_query(&again), printed, "{src}");
    }

    #[test]
    fn parity_queries_parse() {
        for md in [
            include_str!("../../../parity/observed/0001-blank-aggregates.md"),
            include_str!("../../../parity/observed/0002-division-by-zero.md"),
        ] {
            let src = dax_block(md);
            let q = parse_query(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(q.evaluates.len(), 1);
            let Some(ExprKind::Call { name, args }) = q.evaluates.first().map(|e| &e.table.kind)
            else {
                panic!("{src}: not a call");
            };
            assert_eq!(name, "ROW");
            assert!(args.len() >= 18, "{src}");
            assert_round_trips_query(src);
        }
    }

    #[test]
    fn precedence_and_associativity_follow_dax() {
        let cases = [
            ("1 + 2 * 3 - 4", "(- (+ 1 (* 2 3)) 4)"),
            ("-2 ^ 2", "(Negate (^ 2 2))"),
            ("2 ^ -1", "(^ 2 (Negate 1))"),
            ("2 ^ 3 ^ 2", "(^ (^ 2 3) 2)"),
            ("-a * b", "(* (Negate a) b)"),
            ("\"a\" & 1 + 2 = \"a3\"", "(= (& \"a\" (+ 1 2)) \"a3\")"),
            ("NOT a = b && c || d", "(|| (&& (Not (= a b)) c) d)"),
            ("NOT(TRUE()) && FALSE", "(&& (Not TRUE()) FALSE())"),
            ("x IN {1, 2} && y", "(&& (IN x {1; 2}) y)"),
            ("a == b <> c", "(<> (== a b) c)"),
            ("(1 + 2) * 3", "(* (+ 1 2) 3)"),
            ("{(1, \"a\"), (2, \"b\")}", "{1 \"a\"; 2 \"b\"}"),
            ("{(1) + 2, (3)}", "{(+ 1 2); 3}"),
            (
                "VAR a = 1 VAR b = a + 1 RETURN a * b",
                "(var a=1 b=(+ a 1) (* a b))",
            ),
            (
                "'Sales Data'[Amount] + Sales[Qty] - [M]",
                "(- (+ Sales Data[Amount] Sales[Qty]) [M])",
            ),
            ("NORM.DIST(1, 0, 1, TRUE)", "NORM.DIST(1 0 1 TRUE())"),
            ("1.5 + .5e1 + 7", "(+ (+ 1.5 5.0) 7)"),
        ];
        for (src, want) in cases {
            let e = parse_expr(src).unwrap_or_else(|e| panic!("{src}: {e}"));
            assert_eq!(shape(&e), want, "{src}");
            assert_round_trips_expr(src);
        }
    }

    #[test]
    fn names_and_keywords_are_case_insensitive_and_keep_their_spelling() {
        let q = parse_query(
            "define measure 'T'[M] = sum(T[x]) var v = 2\nevaluate row(\"a\", [M] in {1}, \"b\", v)\norder by [a] desc, [b] Asc",
        )
        .unwrap();
        let Some(Define::Measure { expr, .. }) = q.defines.first() else {
            panic!("no measure");
        };
        assert!(matches!(&expr.kind, ExprKind::Call { name, .. } if name == "sum"));
        let ev = q.evaluates.first().unwrap();
        assert_eq!(
            ev.order_by.iter().map(|k| k.descending).collect::<Vec<_>>(),
            [true, false]
        );
        assert_round_trips_query(
            "DEFINE\n    MEASURE Sales[Total] = SUM(Sales[Amount])\nEVALUATE\n    {1}\n\nEVALUATE\n    Sales\nORDER BY\n    Sales[Amount] DESC,\n    [Total]",
        );
    }

    #[test]
    fn spans_cover_each_node() {
        let src = "Sales[Amount] + (1)";
        let e = parse_expr(src).unwrap();
        assert_eq!(e.span, Span::new(0, 19));
        let ExprKind::Binary(_, l, r) = &e.kind else {
            panic!("not binary");
        };
        assert_eq!(l.span, Span::new(0, 13));
        assert_eq!(r.span, Span::new(16, 19));
        let q = parse_query("DEFINE VAR x = 1\nEVALUATE {x} ORDER BY [Value]").unwrap();
        assert_eq!(q.evaluates.first().unwrap().span, Span::new(17, 46));
    }

    #[test]
    fn errors_are_located() {
        let cases: [(&str, bool, (usize, usize), &str); 13] = [
            ("SUM(Sales[Amount]", false, (1, 18), "`,` or `)`"),
            ("1 +\n  * 2", false, (2, 3), "an expression"),
            ("VAR a = 1 a", false, (1, 11), "`VAR` or `RETURN`"),
            ("VAR NOT = 1 RETURN 1", false, (1, 5), "a variable name"),
            ("a b", false, (1, 3), "the end"),
            ("", false, (1, 1), "an expression"),
            ("RETURN 1", false, (1, 1), "`RETURN`"),
            ("{(1, 2) + 3}", false, (1, 9), "`,` or `}`"),
            (
                "EVALUATE T ORDER BY [x] START AT 1",
                true,
                (1, 25),
                "`START AT`",
            ),
            (
                "DEFINE MEASURE [M] = 1 EVALUATE {1}",
                true,
                (1, 16),
                "needs its Table",
            ),
            (
                "DEFINE TABLE t = {1} EVALUATE t",
                true,
                (1, 8),
                "`DEFINE TABLE`",
            ),
            ("DEFINE EVALUATE {1}", true, (1, 8), "`MEASURE` or `VAR`"),
            ("SUM(x)", true, (1, 1), "`EVALUATE`"),
        ];
        for (src, query, at, needle) in cases {
            let e = if query {
                parse_query(src).map(|_| ())
            } else {
                parse_expr(src).map(|_| ())
            }
            .expect_err(src);
            assert_eq!(e.line_column(src), at, "{src}: {e}");
            assert!(e.message.contains(needle), "{src}: {e}");
        }
    }

    #[test]
    fn deep_nesting_is_rejected_not_overflowed() {
        for open in ["(", "{", "{(", "- ", "NOT ", "f(", "VAR a = ", "1 ^ "] {
            let src = open.repeat(100_000);
            let e = parse_expr(&src).expect_err(open);
            assert!(e.message.contains("nested"), "{open}: {e}");
        }
        // Long left-leaning operator chains are bounded too.
        let chain = vec!["1"; 100_000].join(" + ");
        assert!(
            parse_expr(&chain)
                .expect_err("chain")
                .message
                .contains("nested")
        );
        // But reasonable nesting is fine, as are long flat lists.
        assert!(parse_expr(&format!("{}1{}", "(".repeat(40), ")".repeat(40))).is_ok());
        let flat = format!("{{{}}}", vec!["1"; 100_000].join(", "));
        assert!(parse_expr(&flat).is_ok());
    }

    #[test]
    fn oversized_input_is_rejected() {
        let big = " ".repeat(MAX_SOURCE + 1);
        assert!(parse_expr(&big).is_err());
        assert!(parse_query(&big).is_err());
    }

    /// 20,000 cases a run; soak with `ODASH_FUZZ_SCALE` / `ODASH_FUZZ_SEED` (see `crate::fuzz`).
    #[test]
    fn fuzzed_input_never_panics() {
        let seeds = [
            dax_block(include_str!(
                "../../../parity/observed/0001-blank-aggregates.md"
            )),
            dax_block(include_str!(
                "../../../parity/observed/0002-division-by-zero.md"
            )),
            "DEFINE\n    MEASURE 'Sales Data'[Total] = CALCULATE(SUM(Sales[Amount]), Sales[Region] IN {\"a\", \"b\"})\n    VAR v = 1.5e3\nEVALUATE\n    {(1, \"x\"\"y\"), (2, BLANK())}\nORDER BY\n    [Value1] DESC, [Value2]",
            "VAR a = -[M] ^ 2 // c\nVAR b = NOT a = 1 && TRUE || FALSE() -- d\nRETURN /* e */ IF(a <= b, a & \"t\", b == .5)",
        ];
        let alphabet: Vec<char> = "'\"()[]{},=<>+-*/&|^.e09_ \n\tVAR RETURN NOT IN EVALUATE DEFINE"
            .chars()
            .collect();
        let mut rng = crate::fuzz::Fuzz::new("dax-parse", 0x9E37_79B9_7F4A_7C15);
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
                if let Ok(q) = parse_query(&src) {
                    let printed = print_query(&q);
                    let again = parse_query(&printed)
                        .unwrap_or_else(|err| panic!("{src:?} → {printed:?}: {err}"));
                    assert_eq!(unspanned_query(&again), unspanned_query(&q), "{src:?}");
                }
                if let Ok(e) = parse_expr(&src) {
                    let printed = print_expr(&e);
                    let again = parse_expr(&printed)
                        .unwrap_or_else(|err| panic!("{src:?} → {printed:?}: {err}"));
                    assert_eq!(unspanned(&again), unspanned(&e), "{src:?}");
                }
                if let Err(e) = parse_query(&src) {
                    let _ = e.line_column(&src);
                }
            });
        }
        rng.finish();
    }
}
