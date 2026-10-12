//! Prints M the way Power Query's Advanced Editor shows it: `let`, one binding per line indented
//! four spaces, `in`, then the body indented four spaces.

use std::fmt::Write;

use crate::{Expr, KEYWORDS};

/// Prints an expression (no trailing newline).
pub fn print(expr: &Expr) -> String {
    let mut out = String::new();
    write_expr(&mut out, expr, 0);
    out
}

/// A name as an M identifier: plain when it is a regular identifier, else `#"…"`.
pub fn quote_identifier(name: &str) -> String {
    if is_regular(name) && !KEYWORDS.contains(&name) {
        name.to_string()
    } else {
        format!("#{}", quote_text(name))
    }
}

/// A field name in `[…]`: plain when it is a generalized identifier, else `#"…"`.
fn field_name(name: &str) -> String {
    if !name.is_empty() && name.split(' ').all(is_regular) {
        name.to_string()
    } else {
        format!("#{}", quote_text(name))
    }
}

/// `Table.PromoteHeaders`, `x_1`: identifier segments joined by dots.
fn is_regular(name: &str) -> bool {
    !name.is_empty()
        && name.split('.').all(|seg| {
            let mut chars = seg.chars();
            chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
                && chars.all(|c| c.is_alphanumeric() || c == '_')
        })
}

/// A text literal with M's escapes: `""` for a quote, `#(lf)`/`#(cr)`/`#(tab)`/`#(cr,lf)`,
/// `#(#)(` for a literal `#(`, and `#(XXXX)` for other control characters.
fn quote_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len().saturating_add(2));
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\"\""),
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                out.push_str("#(cr,lf)");
            }
            '\r' => out.push_str("#(cr)"),
            '\n' => out.push_str("#(lf)"),
            '\t' => out.push_str("#(tab)"),
            '#' if chars.peek() == Some(&'(') => out.push_str("#(#)"),
            c if c.is_control() => {
                let _ = write!(out, "#({:04X})", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn number(n: f64) -> String {
    // Rust's `Display` for f64 is the shortest round-trip form without an exponent, which is
    // valid M (`3`, `0.5`, `1000000`).
    format!("{n}")
}

fn precedence(e: &Expr) -> u8 {
    match e {
        Expr::Binary(op, ..) => op.precedence(),
        // Prefix forms that extend to the right: lowest, so they get parenthesised as operands.
        Expr::Let(_) | Expr::Each(_) => 0,
        Expr::Not(_) | Expr::Negate(_) => 7,
        _ => u8::MAX,
    }
}

fn write_operand(out: &mut String, e: &Expr, indent: usize, min: u8) {
    if precedence(e) < min {
        out.push('(');
        write_expr(out, e, indent);
        out.push(')');
    } else {
        write_expr(out, e, indent);
    }
}

fn write_seq<T>(out: &mut String, items: &[T], tight: bool, mut each: impl FnMut(&mut String, &T)) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(if tight { "," } else { ", " });
        }
        each(out, item);
    }
}

fn write_expr(out: &mut String, expr: &Expr, indent: usize) {
    match expr {
        Expr::Let(l) => {
            let pad = " ".repeat(indent.saturating_add(4));
            out.push_str("let\n");
            for (i, b) in l.bindings.iter().enumerate() {
                if i > 0 {
                    out.push_str(",\n");
                }
                out.push_str(&pad);
                out.push_str(&quote_identifier(&b.name));
                out.push_str(" = ");
                write_expr(out, &b.value, indent.saturating_add(4));
            }
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            out.push_str("in\n");
            out.push_str(&pad);
            write_expr(out, &l.body, indent.saturating_add(4));
        }
        Expr::Ident(name) => out.push_str(&quote_identifier(name)),
        Expr::Text(s) => out.push_str(&quote_text(s)),
        Expr::Number(n) => out.push_str(&number(*n)),
        Expr::Logical(b) => out.push_str(if *b { "true" } else { "false" }),
        Expr::Null => out.push_str("null"),
        Expr::Type(t) => {
            out.push_str("type ");
            out.push_str(t);
        }
        Expr::List { items, tight } => {
            out.push('{');
            write_seq(out, items, *tight, |o, e| write_expr(o, e, indent));
            out.push('}');
        }
        Expr::Record { fields, tight } => {
            out.push('[');
            write_seq(out, fields, *tight, |o, (k, v)| {
                o.push_str(&field_name(k));
                o.push('=');
                write_expr(o, v, indent);
            });
            out.push(']');
        }
        Expr::Call { func, args, tight } => {
            write_operand(out, func, indent, u8::MAX);
            out.push('(');
            write_seq(out, args, *tight, |o, e| write_expr(o, e, indent));
            out.push(')');
        }
        Expr::Item(target, selector) => {
            write_operand(out, target, indent, u8::MAX);
            out.push('{');
            write_expr(out, selector, indent);
            out.push('}');
        }
        Expr::Field(target, name) => {
            if let Some(t) = target {
                write_operand(out, t, indent, u8::MAX);
            }
            out.push('[');
            out.push_str(&field_name(name));
            out.push(']');
        }
        Expr::Each(body) => {
            out.push_str("each ");
            write_expr(out, body, indent);
        }
        Expr::Not(e) => {
            out.push_str("not ");
            write_operand(out, e, indent, 7);
        }
        Expr::Negate(e) => {
            out.push('-');
            write_operand(out, e, indent, 7);
        }
        Expr::Binary(op, l, r) => {
            let p = op.precedence();
            write_operand(out, l, indent, p);
            out.push(' ');
            out.push_str(op.symbol());
            out.push(' ');
            write_operand(out, r, indent, p.saturating_add(1));
        }
        Expr::Paren(e) => {
            out.push('(');
            write_expr(out, e, indent);
            out.push(')');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BinOp;

    #[test]
    fn quotes_identifiers_and_escapes_text() {
        assert_eq!(quote_identifier("Source"), "Source");
        assert_eq!(
            quote_identifier("Table.PromoteHeaders"),
            "Table.PromoteHeaders"
        );
        assert_eq!(
            quote_identifier("Promoted Headers"),
            "#\"Promoted Headers\""
        );
        assert_eq!(quote_identifier("let"), "#\"let\"");
        assert_eq!(quote_identifier("1st"), "#\"1st\"");
        assert_eq!(
            quote_text("a\"b\nc\r\nd\te#(f\u{1}"),
            "\"a\"\"b#(lf)c#(cr,lf)d#(tab)e#(#)(f#(0001)\""
        );
        assert_eq!(field_name("Unit Price"), "Unit Price");
        assert_eq!(field_name("Qty (x)"), "#\"Qty (x)\"");
    }

    #[test]
    fn binary_operands_get_parentheses_only_when_needed() {
        let n = |v| Box::new(Expr::Number(v));
        let sum = Expr::Binary(BinOp::Add, n(1.0), n(2.0));
        let e = Expr::Binary(BinOp::Multiply, Box::new(sum.clone()), n(3.0));
        assert_eq!(print(&e), "(1 + 2) * 3");
        let e = Expr::Binary(BinOp::Add, n(3.0), Box::new(sum));
        assert_eq!(print(&e), "3 + (1 + 2)");
    }
}
