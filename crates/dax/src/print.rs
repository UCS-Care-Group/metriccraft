//! Prints DAX in one canonical layout, with parentheses only where precedence needs them, so
//! printing then parsing gives the same tree back.

use std::fmt::Write;

use crate::{Define, Expr, ExprKind, NOT_PRECEDENCE, Query, SIGN_PRECEDENCE, UnaryOp, is_keyword};

/// Prints an expression on one line.
pub fn print_expr(expr: &Expr) -> String {
    let mut out = String::new();
    write_expr(&mut out, expr);
    out
}

/// Prints a query: `DEFINE` with one definition per line, then each `EVALUATE` (and its
/// `ORDER BY`) with its parts indented four spaces. No trailing newline.
pub fn print_query(query: &Query) -> String {
    let mut out = String::new();
    if !query.defines.is_empty() {
        out.push_str("DEFINE");
        for d in &query.defines {
            out.push_str("\n    ");
            match d {
                Define::Measure {
                    table, name, expr, ..
                } => {
                    out.push_str("MEASURE ");
                    out.push_str(&table_name(table));
                    out.push_str(&column_name(name));
                    out.push_str(" = ");
                    write_expr(&mut out, expr);
                }
                Define::Var { name, expr, .. } => {
                    out.push_str("VAR ");
                    out.push_str(name);
                    out.push_str(" = ");
                    write_expr(&mut out, expr);
                }
            }
        }
        out.push('\n');
    }
    for (i, ev) in query.evaluates.iter().enumerate() {
        if i > 0 {
            out.push_str("\n\n");
        }
        out.push_str("EVALUATE\n    ");
        write_expr(&mut out, &ev.table);
        for (j, key) in ev.order_by.iter().enumerate() {
            out.push_str(if j == 0 {
                "\nORDER BY\n    "
            } else {
                ",\n    "
            });
            write_expr(&mut out, &key.expr);
            if key.descending {
                out.push_str(" DESC");
            }
        }
    }
    out
}

/// A Table name: plain when it reads back as one name, else `'…'`.
fn table_name(name: &str) -> String {
    let plain = !is_keyword(name)
        && !name.eq_ignore_ascii_case("TRUE")
        && !name.eq_ignore_ascii_case("FALSE")
        && name.split('.').enumerate().all(|(i, seg)| {
            let mut chars = seg.chars();
            chars.next().is_some_and(|c| {
                c == '_'
                    || if i == 0 {
                        c.is_alphabetic()
                    } else {
                        c.is_alphanumeric()
                    }
            }) && chars.all(|c| c.is_alphanumeric() || c == '_')
        });
    if plain {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "''"))
    }
}

fn column_name(name: &str) -> String {
    format!("[{}]", name.replace(']', "]]"))
}

fn precedence(e: &Expr) -> u8 {
    match &e.kind {
        ExprKind::Binary(op, ..) => op.precedence(),
        ExprKind::Unary(UnaryOp::Not, _) => NOT_PRECEDENCE,
        ExprKind::Unary(..) => SIGN_PRECEDENCE,
        // Extends to the right: lowest, so it is parenthesised as an operand.
        ExprKind::Var { .. } => 0,
        _ => u8::MAX,
    }
}

fn write_operand(out: &mut String, e: &Expr, min: u8) {
    if precedence(e) < min {
        out.push('(');
        write_expr(out, e);
        out.push(')');
    } else {
        write_expr(out, e);
    }
}

fn write_list(out: &mut String, items: &[Expr]) {
    for (i, e) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_expr(out, e);
    }
}

fn write_expr(out: &mut String, expr: &Expr) {
    match &expr.kind {
        ExprKind::Integer(n) => {
            let _ = write!(out, "{n}");
        }
        // `Debug` is the shortest round-trip form and always has a `.` or an exponent, so it
        // reads back as a decimal (`1.0`, `1e20`).
        ExprKind::Decimal(n) => {
            let _ = write!(out, "{n:?}");
        }
        ExprKind::Text(s) => {
            out.push('"');
            out.push_str(&s.replace('"', "\"\""));
            out.push('"');
        }
        ExprKind::Name(n) => out.push_str(&table_name(n)),
        ExprKind::Column { table, column } => {
            if let Some(t) = table {
                out.push_str(&table_name(t));
            }
            out.push_str(&column_name(column));
        }
        ExprKind::Call { name, args } => {
            out.push_str(name);
            out.push('(');
            write_list(out, args);
            out.push(')');
        }
        ExprKind::Unary(UnaryOp::Not, e) => {
            out.push_str("NOT ");
            write_operand(out, e, NOT_PRECEDENCE.saturating_add(1));
        }
        ExprKind::Unary(op, e) => {
            out.push(if *op == UnaryOp::Negate { '-' } else { '+' });
            // `--` would start a comment.
            if matches!(e.kind, ExprKind::Unary(UnaryOp::Negate | UnaryOp::Plus, _)) {
                out.push(' ');
            }
            write_operand(out, e, SIGN_PRECEDENCE);
        }
        ExprKind::Binary(op, l, r) => {
            let p = op.precedence();
            write_operand(out, l, p);
            out.push(' ');
            out.push_str(op.symbol());
            out.push(' ');
            write_operand(out, r, p.saturating_add(1));
        }
        ExprKind::Var { vars, body } => {
            for (name, value) in vars {
                out.push_str("VAR ");
                out.push_str(name);
                out.push_str(" = ");
                write_expr(out, value);
                out.push(' ');
            }
            out.push_str("RETURN ");
            write_expr(out, body);
        }
        ExprKind::Table(rows) => {
            out.push('{');
            for (i, row) in rows.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                match row.as_slice() {
                    [one] => write_expr(out, one),
                    many => {
                        out.push('(');
                        write_list(out, many);
                        out.push(')');
                    }
                }
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BinOp, Span};

    fn e(kind: ExprKind) -> Expr {
        Expr {
            kind,
            span: Span::default(),
        }
    }

    fn b(kind: ExprKind) -> Box<Expr> {
        Box::new(e(kind))
    }

    #[test]
    fn quotes_names_and_escapes_text() {
        assert_eq!(table_name("Sales"), "Sales");
        assert_eq!(table_name("Sales Data"), "'Sales Data'");
        assert_eq!(table_name("O'Brien"), "'O''Brien'");
        assert_eq!(table_name("return"), "'return'");
        assert_eq!(table_name("True"), "'True'");
        assert_eq!(table_name("1st"), "'1st'");
        assert_eq!(column_name("a]b"), "[a]]b]");
        assert_eq!(
            print_expr(&e(ExprKind::Text("say \"hi\"".into()))),
            "\"say \"\"hi\"\"\""
        );
        assert_eq!(print_expr(&e(ExprKind::Decimal(1.0))), "1.0");
        assert_eq!(print_expr(&e(ExprKind::Decimal(1e20))), "1e20");
    }

    #[test]
    fn operands_get_parentheses_only_when_needed() {
        let n = |v| b(ExprKind::Integer(v));
        let sum = ExprKind::Binary(BinOp::Add, n(1), n(2));
        let prod = e(ExprKind::Binary(BinOp::Multiply, b(sum.clone()), n(3)));
        assert_eq!(print_expr(&prod), "(1 + 2) * 3");
        let right = e(ExprKind::Binary(BinOp::Subtract, n(3), b(sum)));
        assert_eq!(print_expr(&right), "3 - (1 + 2)");
        let neg = |x| b(ExprKind::Unary(UnaryOp::Negate, x));
        assert_eq!(print_expr(&neg(neg(n(1)))), "- -1");
        let pow = e(ExprKind::Binary(BinOp::Power, neg(n(2)), n(2)));
        assert_eq!(print_expr(&pow), "(-2) ^ 2");
    }
}
