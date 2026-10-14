//! `FORMAT(value, format_string[, locale_name])` (M3.8): `opendash_core::format` in a Locale.
//!
//! Without a locale name the query's Locale is en-GB: the evaluator carries none yet, and parity
//! cases are observed with English (United Kingdom) regional settings. BLANK formats to `""`
//! (text, not BLANK), as documented.

use opendash_core::{EN_GB, Locale, Value};

// A child of `functions` (its `#[path]` mod line), so the evaluator is reached through `crate`.
use super::Call;
use crate::dax::eval::{Evaluator, Val};
use crate::dax::{DaxError, Result};

pub(super) fn format(ev: &mut Evaluator<'_, '_>, call: &Call<'_>) -> Result<Val> {
    call.arity(2, 3)?;
    let value = ev.scalar(call.arg(0)?, call.fc)?;
    let code_arg = call.arg(1)?;
    let code = ev.scalar(code_arg, call.fc)?.to_text();
    let locale = match call.args.get(2) {
        None => &EN_GB,
        Some(arg) => {
            let name = ev.scalar(arg, call.fc)?.to_text();
            Locale::get(&name).ok_or_else(|| {
                DaxError::new(
                    arg.span,
                    format!(
                        "FORMAT doesn't know the locale \"{name}\"; OpenDash BI has en-US, \
                         en-GB, de-DE and fr-FR"
                    ),
                )
            })?
        }
    };
    let text = opendash_core::format(&value, &code, locale)
        .map_err(|e| DaxError::new(code_arg.span, format!("FORMAT: {e}")))?;
    Ok(Val::Scalar(Value::text(&text)))
}

#[cfg(test)]
mod tests {
    use crate::Model;
    use crate::dax::{Database, ErrorKind, Options, run_query_text};

    fn one(expr: &str) -> Result<String, String> {
        let db = Database::new(&Model::default(), &[]);
        let q = format!("EVALUATE ROW(\"x\", {expr})");
        let options = Options {
            cancel: None,
            memory_cap: usize::MAX,
        };
        match run_query_text(&db, &q, options) {
            Ok(r) => Ok(r
                .first()
                .and_then(|t| t.rows.first())
                .and_then(|row| row.first())
                .map(|v| v.copy_text())
                .unwrap_or_default()),
            Err(e) => {
                assert_eq!(e.kind, ErrorKind::Eval, "{e:?}");
                Err(e.message)
            }
        }
    }

    #[test]
    fn format_in_dax() {
        // Parity case 0002.
        assert_eq!(one("FORMAT(1 / 0, \"General Number\")").unwrap(), "inf");
        assert_eq!(one("FORMAT(1234.5, \"Currency\")").unwrap(), "£1,234.50");
        assert_eq!(
            one("FORMAT(1234.5, \"Currency\", \"en-US\")").unwrap(),
            "$1,234.50"
        );
        assert_eq!(
            one("FORMAT(1234.5, \"Currency\", \"de-DE\")").unwrap(),
            "1.234,50 €"
        );
        assert_eq!(one("FORMAT(-0.5, \"0%;(0%)\")").unwrap(), "(50%)");
        assert_eq!(one("FORMAT(BLANK(), \"0.00\")").unwrap(), "");
        assert_eq!(one("FORMAT(1.5, BLANK())").unwrap(), "1.5");
        assert_eq!(one("FORMAT(2, \"0\") & \"x\"").unwrap(), "2x");
    }

    #[test]
    fn format_errors() {
        assert!(one("FORMAT(1)").unwrap_err().contains("2 to 3"));
        assert!(
            one("FORMAT(1, \"0\", \"xx-XX\")")
                .unwrap_err()
                .contains("xx-XX")
        );
        let long = "0".repeat(2000);
        let e = one(&format!("FORMAT(1, \"{long}\")")).unwrap_err();
        assert!(e.contains("the most is 1024"), "{e}");
        let e = one("FORMAT(1e9, \"Short Date\")").unwrap_err();
        assert!(e.contains("dates"), "{e}");
    }
}
