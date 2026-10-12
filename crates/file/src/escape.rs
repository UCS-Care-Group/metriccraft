//! File names for table and query names (ADR 0003, Identity): deterministic, legal on Linux,
//! Windows and macOS, and distinct on case-insensitive file systems. The real name lives inside
//! the file; a file name is never read back as a name.
//!
//! Escaping, per name:
//! - `%XX` (uppercase hex, per UTF-8 byte) for characters Windows forbids (`< > : " / \ | ? *`),
//!   control characters, `%` and `~` (ours, so escapes and suffixes are unambiguous), a leading
//!   `.` or space (hidden files, trimmed names) and trailing dots and spaces (Windows drops them).
//! - Windows device names (`CON`, `PRN`, `AUX`, `NUL`, `COM0`–`COM9`, `LPT0`–`LPT9` and the
//!   superscript-digit forms, any case, alone or before a `.`) get their first letter escaped:
//!   `CON` → `%43ON`.
//! - The empty name is `%`.
//! - Names longer than [`MAX_STEM`] bytes are cut at a character boundary and get `~` plus
//!   the first 8 hex digits of the name's SHA-256.
//!
//! Then collisions, case-insensitively (Windows and macOS defaults): names are taken in byte
//! order and a name whose escaped form, lowercased, is already used gets `~` plus 8 hex digits of
//! its SHA-256 (`Sales`, `sales` → `Sales`, `sales~1a2b3c4d`). The result depends only on the set
//! of names, not their order.
//!
//! Not handled: macOS's Unicode normalisation (`é` composed vs decomposed would collide there).

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

/// Longest escaped stem before the hash suffix, in bytes; well under every OS's 255-byte limit.
pub const MAX_STEM: usize = 100;

const RESERVED: &[&str] = &["CON", "PRN", "AUX", "NUL"];

/// Whether `stem` (before any `.`) is a Windows device name.
fn is_reserved(stem: &str) -> bool {
    let base = stem.split('.').next().unwrap_or("").trim_end_matches(' ');
    let upper = base.to_uppercase();
    if RESERVED.contains(&upper.as_str()) {
        return true;
    }
    let mut chars = upper.chars();
    let prefix: String = chars.by_ref().take(3).collect();
    let rest: Vec<char> = chars.collect();
    (prefix == "COM" || prefix == "LPT")
        && matches!(rest.as_slice(), [c] if c.is_ascii_digit() || matches!(c, '¹' | '²' | '³'))
}

fn push_escaped(out: &mut String, c: char) {
    let mut buf = [0u8; 4];
    for b in c.encode_utf8(&mut buf).bytes() {
        out.push_str(&format!("%{b:02X}"));
    }
}

fn hash8(name: &str) -> String {
    let digest = Sha256::digest(name.as_bytes());
    digest.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// One name's escaped stem, before collision handling.
pub fn escape(name: &str) -> String {
    if name.is_empty() {
        return "%".into();
    }
    let chars: Vec<char> = name.chars().collect();
    let keep_end = chars
        .iter()
        .rposition(|&c| c != '.' && c != ' ')
        .map_or(0, |i| i + 1);
    let mut out = String::with_capacity(name.len());
    for (i, &c) in chars.iter().enumerate() {
        let bad = matches!(
            c,
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' | '%' | '~'
        ) || c.is_control()
            || (i == 0 && (c == '.' || c == ' '))
            || i >= keep_end;
        if bad {
            push_escaped(&mut out, c);
        } else {
            out.push(c);
        }
    }
    if is_reserved(&out) {
        let mut chars = out.chars();
        if let Some(first) = chars.next() {
            let mut fixed = String::new();
            push_escaped(&mut fixed, first);
            fixed.push_str(chars.as_str());
            out = fixed;
        }
    }
    if out.len() > MAX_STEM {
        let mut cut = MAX_STEM;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push('~');
        out.push_str(&hash8(name));
    }
    out
}

/// The file stems for `names`, in the same order (see the module docs). Names should be
/// distinct; a repeated name gets a stem of its own all the same.
pub fn file_stems(names: &[&str]) -> Vec<String> {
    let mut order: Vec<usize> = (0..names.len()).collect();
    order.sort_by(|&a, &b| names.get(a).cmp(&names.get(b)).then(a.cmp(&b)));
    let mut used = BTreeSet::new();
    let mut stems = vec![String::new(); names.len()];
    for i in order {
        let name = names.get(i).copied().unwrap_or("");
        let base = escape(name);
        let mut stem = base.clone();
        let mut n = 0u32;
        while used.contains(&stem.to_lowercase()) {
            n = n.saturating_add(1);
            stem = if n == 1 {
                format!("{base}~{}", hash8(name))
            } else {
                format!("{base}~{}{n}", hash8(name))
            };
        }
        used.insert(stem.to_lowercase());
        if let Some(slot) = stems.get_mut(i) {
            *slot = stem;
        }
    }
    stems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_are_kept() {
        assert_eq!(escape("Sales"), "Sales");
        assert_eq!(escape("Unit Price"), "Unit Price");
        assert_eq!(escape("Ventes été"), "Ventes été");
        assert_eq!(escape("v1.2"), "v1.2");
    }

    #[test]
    fn reserved_names_are_escaped() {
        for name in [
            "CON", "con", "Nul", "AUX", "PRN", "COM1", "lpt9", "COM¹", "CON.txt",
        ] {
            let e = escape(name);
            assert!(!is_reserved(&e), "{name} → {e}");
            assert!(e.starts_with('%'), "{name} → {e}");
        }
        assert_eq!(escape("CON"), "%43ON");
        assert_eq!(escape("CONSOLE"), "CONSOLE");
        assert_eq!(escape("COM10"), "COM10");
        assert_eq!(escape("LPT"), "LPT");
    }

    #[test]
    fn illegal_characters_and_trailing_dots_are_escaped() {
        assert_eq!(escape("a/b"), "a%2Fb");
        assert_eq!(escape(r#"<>:"\|?*"#), "%3C%3E%3A%22%5C%7C%3F%2A");
        assert_eq!(escape("50%"), "50%25");
        assert_eq!(escape("tab\there"), "tab%09here");
        assert_eq!(escape("end."), "end%2E");
        assert_eq!(escape("end. ."), "end%2E%20%2E");
        assert_eq!(escape(".hidden"), "%2Ehidden");
        assert_eq!(escape(" lead"), "%20lead");
        assert_eq!(escape(".."), "%2E%2E");
        assert_eq!(escape(""), "%");
        assert_eq!(escape("a~b"), "a%7Eb");
    }

    #[test]
    fn every_stem_is_legal_everywhere() {
        let names = [
            "CON",
            "a/b",
            "x.",
            " ",
            "..",
            "",
            "~",
            "a\u{0}b",
            "COM3.odm",
            "日本語",
            "😀",
        ];
        for stem in file_stems(&names) {
            assert!(!stem.is_empty());
            assert!(!stem.contains(['<', '>', ':', '"', '/', '\\', '|', '?', '*']));
            assert!(!stem.chars().any(char::is_control));
            assert!(!stem.ends_with(['.', ' ']) && !stem.starts_with(['.', ' ']));
            assert!(!is_reserved(&stem), "{stem}");
            assert!(crate::check_path(&format!("queries/{stem}.odq")).is_ok());
        }
    }

    #[test]
    fn long_names_are_cut_with_a_hash() {
        let long = "é".repeat(200);
        let e = escape(&long);
        assert!(e.len() <= MAX_STEM + 9, "{}", e.len());
        assert!(e.contains('~'));
        assert_ne!(
            escape(&"é".repeat(201)),
            e,
            "different names, different stems"
        );
    }

    #[test]
    fn case_only_collisions_get_distinct_stems() {
        let stems = file_stems(&["sales", "Sales", "SALES", "Other"]);
        let lower: BTreeSet<String> = stems.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(lower.len(), 4, "{stems:?}");
        assert_eq!(
            stems[2], "SALES",
            "the first in byte order keeps the plain stem"
        );
        assert!(stems[0].starts_with("sales~") && stems[1].starts_with("Sales~"));
        assert_eq!(stems[3], "Other");
    }

    #[test]
    fn stems_depend_on_the_set_not_the_order() {
        let a = file_stems(&["b", "B", "CON", "con"]);
        let b = file_stems(&["con", "CON", "B", "b"]);
        assert_eq!(a[0], b[3]);
        assert_eq!(a[1], b[2]);
        assert_eq!(a[2], b[1]);
        assert_eq!(a[3], b[0]);
    }

    #[test]
    fn a_name_that_looks_like_a_suffix_cannot_collide() {
        let h = hash8("sales");
        let stems = file_stems(&["Sales", "sales", &format!("sales~{h}")]);
        let lower: BTreeSet<String> = stems.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(lower.len(), 3, "{stems:?}");
    }
}
