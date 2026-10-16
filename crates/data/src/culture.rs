//! Cultures: the locale rules used to read text as numbers and dates (glossary). Our own
//! small table, grown on demand — no ICU.
//!
//! Parity uncertainties, to settle with M1.10 cases in the VM:
//! - thousands separators must group by three (`1,234`); .NET-style parsing may accept `1,2,3`;
//! - two-digit years map to 1950–2049 (the .NET Gregorian default `TwoDigitYearMax` = 2049);
//! - a whole-number conversion of `2.5` rounds half to even (`Int64.From`'s default).
//! - fixed decimal text is rounded to 4 places half to even (`Currency.From`'s documented
//!   default) on the exact decimal digits; Power Query may round the double `Number.FromText`
//!   gives (`"0.00005"` is exactly half here, a hair above half as a double).

/// The order of day, month and year in a numeric date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DateOrder {
    Dmy,
    Mdy,
    Ymd,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Culture {
    /// BCP 47 name, e.g. `en-GB`.
    pub name: &'static str,
    pub decimal: char,
    /// Accepted digit-group separators (first is the canonical one).
    pub thousands: &'static [char],
    pub date_order: DateOrder,
}

pub static CULTURES: &[Culture] = &[
    Culture {
        name: "en-US",
        decimal: '.',
        thousands: &[','],
        date_order: DateOrder::Mdy,
    },
    Culture {
        name: "en-GB",
        decimal: '.',
        thousands: &[','],
        date_order: DateOrder::Dmy,
    },
    Culture {
        name: "de-DE",
        decimal: ',',
        thousands: &['.'],
        date_order: DateOrder::Dmy,
    },
    Culture {
        name: "fr-FR",
        decimal: ',',
        thousands: &['\u{202F}', '\u{A0}', ' '],
        date_order: DateOrder::Dmy,
    },
];

const MICROS_PER_DAY: i64 = 86_400_000_000;

impl Culture {
    /// Looks a culture up by name (case-insensitive).
    pub fn get(name: &str) -> Option<&'static Culture> {
        CULTURES.iter().find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// `true` / `false`, any case.
    pub fn parse_boolean(&self, s: &str) -> Option<bool> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("true") {
            Some(true)
        } else if s.eq_ignore_ascii_case("false") {
            Some(false)
        } else {
            None
        }
    }

    pub fn parse_decimal(&self, s: &str) -> Option<f64> {
        let (n, _) = self.number(s)?;
        n.parse::<f64>().ok().filter(|v| v.is_finite())
    }

    /// A whole number. Text with a fraction converts too (rounded half to even).
    pub fn parse_whole(&self, s: &str) -> Option<i64> {
        let (n, whole) = self.number(s)?;
        if whole {
            return n.parse().ok();
        }
        let v = n.parse::<f64>().ok()?.round_ties_even();
        // i64::MAX as f64 rounds up to 2^63, so the upper bound is exclusive.
        let bound = 2f64.powi(63);
        (-bound..bound).contains(&v).then_some(v as i64)
    }

    /// A fixed decimal number (`Currency.Type`): the value × 10⁴, rounded half to even
    /// (`Currency.From`'s default rounding). `None` outside ±922,337,203,685,477.5807.
    pub fn parse_fixed(&self, s: &str) -> Option<i64> {
        fixed(&self.number(s)?.0)
    }

    /// `true` when `s` is written as a whole number (no fraction, no exponent) that fits i64.
    pub fn is_whole(&self, s: &str) -> bool {
        self.number(s)
            .is_some_and(|(n, whole)| whole && n.parse::<i64>().is_ok())
    }

    /// Days since 1970-01-01 (Arrow `Date32`).
    pub fn parse_date(&self, s: &str) -> Option<i32> {
        match self.date_prefix(s.trim())? {
            (days, "") => Some(days),
            _ => None,
        }
    }

    /// Microseconds since 1970-01-01 00:00 (Arrow `Timestamp(Microsecond, None)`). A date alone
    /// is midnight.
    pub fn parse_datetime(&self, s: &str) -> Option<i64> {
        let (days, rest) = self.date_prefix(s.trim())?;
        let day = i64::from(days).checked_mul(MICROS_PER_DAY)?;
        if rest.is_empty() {
            return Some(day);
        }
        let rest = rest
            .strip_prefix('T')
            .unwrap_or(rest)
            .trim_start_matches(' ');
        day.checked_add(time(rest)?)
    }

    /// Normalises a number to Rust's syntax; `true` when written as a whole number.
    fn number(&self, s: &str) -> Option<(String, bool)> {
        let s = s.trim();
        let mut out = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        if let Some(&sign @ ('+' | '-')) = chars.peek() {
            out.push(sign);
            chars.next();
        }
        // Integer part: digits, optionally grouped by three.
        let (mut digits, mut group) = (0usize, None::<usize>);
        while let Some(&c) = chars.peek() {
            if c.is_ascii_digit() {
                out.push(c);
                digits += 1;
                group = group.map(|g| g + 1);
            } else if self.thousands.contains(&c)
                && match group {
                    None => (1..=3).contains(&digits),
                    Some(g) => g == 3,
                }
            {
                group = Some(0);
            } else {
                break;
            }
            chars.next();
        }
        if group.is_some_and(|g| g != 3) {
            return None;
        }
        let mut whole = true;
        if chars.peek() == Some(&self.decimal) {
            chars.next();
            out.push('.');
            whole = false;
            while let Some(&c) = chars.peek().filter(|c| c.is_ascii_digit()) {
                out.push(c);
                digits += 1;
                chars.next();
            }
        }
        if digits == 0 {
            return None;
        }
        if let Some(&('e' | 'E')) = chars.peek() {
            chars.next();
            out.push('e');
            whole = false;
            if let Some(&sign @ ('+' | '-')) = chars.peek() {
                out.push(sign);
                chars.next();
            }
            let mut exp = 0;
            while let Some(&c) = chars.peek().filter(|c| c.is_ascii_digit()) {
                out.push(c);
                exp += 1;
                chars.next();
            }
            if exp == 0 {
                return None;
            }
        }
        chars.next().is_none().then_some((out, whole))
    }

    /// A numeric date (`/`, `-` or `.` separated; a 4-digit first part means year first) and the
    /// text after it.
    fn date_prefix<'a>(&self, s: &'a str) -> Option<(i32, &'a str)> {
        let (a, alen, s) = digits(s, 4)?;
        let sep = s.chars().next().filter(|c| matches!(c, '/' | '-' | '.'))?;
        let (b, blen, s) = digits(s.get(1..)?, 2)?;
        let s = s.strip_prefix(sep)?;
        let (c, clen, rest) = digits(s, 4)?;
        let ((y, ylen), m, d) = match (alen, self.date_order) {
            (4, _) | (_, DateOrder::Ymd) => ((a, alen), (b, blen), (c, clen)),
            (_, DateOrder::Dmy) => ((c, clen), (b, blen), (a, alen)),
            (_, DateOrder::Mdy) => ((c, clen), (a, alen), (b, blen)),
        };
        if m.1 > 2 || d.1 > 2 {
            return None;
        }
        let y = match ylen {
            4 => y,
            1 | 2 if y < 50 => 2000 + y,
            1 | 2 => 1900 + y,
            _ => return None,
        };
        Some((days_from_civil(y, m.0, d.0)?, rest))
    }
}

/// A number in Rust's syntax (`-12.5`, `1.5e3`) as a fixed decimal: × 10⁴, rounded half to
/// even on its exact digits; `None` if it overflows `i64`.
pub(crate) fn fixed(n: &str) -> Option<i64> {
    let (neg, n) = match n.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, n.strip_prefix('+').unwrap_or(n)),
    };
    let (mantissa, exp) = match n.split_once(['e', 'E']) {
        Some((m, e)) => (m, e.parse::<i64>().ok()?),
        None => (n, 0),
    };
    let (int, frac) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits: Vec<u8> = int
        .bytes()
        .chain(frac.bytes())
        .map(|b| b.wrapping_sub(b'0'))
        .collect();
    if digits.iter().any(|d| *d > 9) {
        return None;
    }
    // Digits before the point of value × 10⁴.
    let keep = i64::try_from(int.len())
        .ok()?
        .checked_add(exp)?
        .checked_add(4)?;
    let mut acc: i128 = 0;
    let mut i: i64 = 0;
    while i < keep {
        let d = usize::try_from(i).ok().and_then(|i| digits.get(i)).copied();
        if d.is_none() && acc == 0 {
            break; // only zeros left to append to zero
        }
        acc = acc
            .checked_mul(10)?
            .checked_add(i128::from(d.unwrap_or(0)))?;
        if acc > i128::from(u64::MAX) {
            return None;
        }
        i = i.saturating_add(1);
    }
    // Round half to even on the dropped digits.
    let dropped = usize::try_from(keep.max(0)).ok()?;
    if keep >= 0 {
        let first = digits.get(dropped).copied().unwrap_or(0);
        let rest = digits.get(dropped.saturating_add(1)..).unwrap_or_default();
        let up = first > 5 || (first == 5 && (rest.iter().any(|d| *d != 0) || acc % 2 == 1));
        acc = acc.checked_add(i128::from(up))?;
    }
    i64::try_from(if neg { -acc } else { acc }).ok()
}

/// Up to `max` leading ASCII digits: value, count and the rest.
fn digits(s: &str, max: usize) -> Option<(u32, usize, &str)> {
    let len = s.bytes().take_while(u8::is_ascii_digit).count();
    if len == 0 || len > max {
        return None;
    }
    let (d, rest) = s.split_at_checked(len)?;
    Some((d.parse().ok()?, len, rest))
}

/// `H:MM[:SS[.fffffff]][ AM|PM]` as microseconds after midnight.
fn time(s: &str) -> Option<i64> {
    let (h, _, s) = digits(s, 2)?;
    let (m, mlen, s) = digits(s.strip_prefix(':')?, 2)?;
    let (sec, micros, s) = match s.strip_prefix(':') {
        Some(s) => {
            let (sec, _, s) = digits(s, 2)?;
            match s.strip_prefix('.') {
                Some(s) => {
                    let len = s.bytes().take_while(u8::is_ascii_digit).count();
                    let (frac, s) = s.split_at_checked(len)?;
                    if !(1..=7).contains(&len) {
                        return None;
                    }
                    // Pad/truncate to six digits.
                    let six: String = frac.chars().chain("00000".chars()).take(6).collect();
                    (sec, six.parse::<i64>().ok()?, s)
                }
                None => (sec, 0, s),
            }
        }
        None => (0, 0, s),
    };
    let ampm = s.trim_start_matches(' ');
    let h = if ampm.is_empty() {
        h
    } else if !(1..=12).contains(&h) {
        return None;
    } else if ampm.eq_ignore_ascii_case("am") {
        h % 12
    } else if ampm.eq_ignore_ascii_case("pm") {
        h % 12 + 12
    } else {
        return None;
    };
    if mlen != 2 || h > 23 || m > 59 || sec > 59 {
        return None;
    }
    Some((i64::from(h) * 3600 + i64::from(m) * 60 + i64::from(sec)) * 1_000_000 + micros)
}

/// Days since 1970-01-01 for a valid proleptic-Gregorian date (H. Hinnant's algorithm).
pub(crate) fn days_from_civil(y: u32, m: u32, d: u32) -> Option<i32> {
    let leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let days_in_month = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if y == 0 || d == 0 || d > days_in_month {
        return None;
    }
    let days = metriccraft_core::date::days_from_civil(i64::from(y), i64::from(m), i64::from(d));
    i32::try_from(days).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str) -> &'static Culture {
        Culture::get(name).unwrap()
    }

    #[test]
    fn fixed_decimals_round_half_to_even_and_overflow_to_none() {
        let us = c("en-US");
        for (text, want) in [
            ("1.23455", Some(12_346)),
            ("1.23445", Some(12_344)),
            ("1.234451", Some(12_345)),
            ("0.00005", Some(0)),
            ("0.00015", Some(2)),
            ("-2.5", Some(-25_000)),
            ("1,234.5", Some(12_345_000)),
            ("1.5e3", Some(15_000_000)),
            ("15e-5", Some(2)),
            ("1e-300", Some(0)),
            ("0e999999", Some(0)),
            ("922,337,203,685,477.5807", Some(i64::MAX)),
            ("-922337203685477.5808", Some(i64::MIN)),
            ("922337203685477.5808", None),
            ("922337203685477.58075", None),
            ("1e300", None),
            ("abc", None),
        ] {
            assert_eq!(us.parse_fixed(text), want, "{text}");
        }
        assert_eq!(c("de-DE").parse_fixed("1.234,56789"), Some(12_345_679));
    }

    #[test]
    fn numbers_follow_the_culture() {
        let (us, gb, de, fr) = (c("en-US"), c("en-gb"), c("de-DE"), c("fr-FR"));
        assert_eq!(us.parse_decimal("1,234.5"), Some(1234.5));
        assert_eq!(gb.parse_decimal("1,234.5"), Some(1234.5));
        assert_eq!(de.parse_decimal("1.234,5"), Some(1234.5));
        assert_eq!(fr.parse_decimal("1 234,5"), Some(1234.5));
        assert_eq!(fr.parse_decimal("1\u{202F}234,5"), Some(1234.5));
        assert_eq!(us.parse_decimal("1.234,5"), None);
        assert_eq!(de.parse_decimal("1,234.5"), None);
        assert_eq!(de.parse_decimal("1,5"), Some(1.5));
        assert_eq!(us.parse_decimal(" -2.5e3 "), Some(-2500.0));
        assert_eq!(us.parse_decimal(".5"), Some(0.5));
        for bad in [
            "", "-", "1,23", "12,3456", "1e", "abc", "1.2.3", "inf", "NaN", "1 2",
        ] {
            assert_eq!(us.parse_decimal(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn whole_numbers() {
        let us = c("en-US");
        assert!(us.is_whole("1,234"));
        assert!(us.is_whole("-007"));
        assert!(!us.is_whole("1.0"));
        assert!(!us.is_whole("99999999999999999999"));
        assert_eq!(us.parse_whole("9223372036854775807"), Some(i64::MAX));
        assert_eq!(us.parse_whole("2.5"), Some(2));
        assert_eq!(us.parse_whole("3.5"), Some(4));
        assert_eq!(us.parse_whole("1e30"), None);
    }

    #[test]
    fn dates_follow_the_culture() {
        // 2026-03-04 and 2026-04-03 as days since 1970-01-01.
        let (march4, april3) = (20_516, 20_546);
        assert_eq!(c("en-US").parse_date("03/04/2026"), Some(march4));
        assert_eq!(c("en-GB").parse_date("03/04/2026"), Some(april3));
        assert_eq!(c("de-DE").parse_date("03.04.2026"), Some(april3));
        assert_eq!(c("fr-FR").parse_date("3/4/2026"), Some(april3));
        for name in ["en-US", "en-GB", "de-DE", "fr-FR"] {
            assert_eq!(c(name).parse_date("2026-03-04"), Some(march4), "{name}");
        }
        assert_eq!(c("en-GB").parse_date("01/01/70"), Some(0));
        assert_eq!(c("en-GB").parse_date("01/01/49"), Some(28_855));
        assert_eq!(c("en-GB").parse_date("29/02/2000"), Some(11_016));
        for bad in [
            "13/13/2026",
            "29/02/2026",
            "1/2",
            "2026-03-04x",
            "1/2-2026",
            "0/1/2026",
        ] {
            assert_eq!(c("en-GB").parse_date(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn datetimes() {
        let gb = c("en-GB");
        let day = 20_516 * MICROS_PER_DAY;
        let hour = 3_600_000_000;
        assert_eq!(gb.parse_datetime("2026-03-04"), Some(day));
        assert_eq!(
            gb.parse_datetime("04/03/2026 13:30"),
            Some(day + 13 * hour + hour / 2)
        );
        assert_eq!(
            gb.parse_datetime("2026-03-04T01:00:02.5"),
            Some(day + hour + 2_500_000)
        );
        assert_eq!(
            c("en-US").parse_datetime("3/4/2026 1:00 PM"),
            Some(day + 13 * hour)
        );
        assert_eq!(gb.parse_datetime("2026-03-04 12:00 am"), Some(day));
        for bad in [
            "2026-03-04 24:00",
            "2026-03-04 13:00 PM",
            "2026-03-04 1:5",
            "2026-03-04 x",
        ] {
            assert_eq!(gb.parse_datetime(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn booleans() {
        let us = c("en-US");
        assert_eq!(us.parse_boolean("TRUE"), Some(true));
        assert_eq!(us.parse_boolean(" false"), Some(false));
        assert_eq!(us.parse_boolean("yes"), None);
    }
}
