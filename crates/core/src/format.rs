//! Format strings (spec R-4): Power BI's `FORMAT` codes, VBA-style, rendered in a [`Locale`].
//!
//! [`format`] takes a value, a format string and a Locale:
//! - **Named formats**: General Number, Currency, Fixed, Standard, Percent, Scientific, Yes/No,
//!   True/False, On/Off, General Date, Long/Medium/Short Date, Long/Medium/Short Time (any case).
//! - **Custom numeric codes**: `0` `#` placeholders, `.` decimal, `,` grouping (between
//!   placeholders) or ÷1000 scaling (trailing, before the decimal point), `%` (×100), `E+ E- e+
//!   e-` exponents, `"text"` and `\x` literals, up to four `;` sections (positive; negative —
//!   shown without its minus sign; zero; the fourth, for null, is ignored: BLANK is always `""`).
//!   An empty section falls back to the first.
//! - **Custom date/time codes**: `d dd ddd dddd ddddd dddddd w ww m mm mmm mmmm q y yy yyyy h hh
//!   n nn s ss ttttt c`, `AM/PM am/pm A/P a/p AMPM`; `m`/`mm` right after an hour or right before
//!   a second is minutes; `/` and `:` are the Locale's separators. Letters are case-insensitive.
//!
//! **Rounding** is half away from zero on the value's decimal digits: the shortest round-trip
//! digits of a Decimal (so `2.675` → `2.68`, as written, not as the binary double), exact digits
//! of Whole and Fixed decimal numbers.
//!
//! **Hostile input**: a format string longer than [`MAX_FORMAT_LEN`] is an error, so output and
//! work stay proportional to it; date serials outside 100-01-01..9999-12-31 are an error.
//!
//! **Defaults by type** ([`default_format`]), as Power BI Desktop assigns them to new Columns
//! and Measures: whole number `0`, decimal General (empty code), fixed decimal the Locale's
//! currency, date Short Date, date/time General Date, time Long Time.
//!
//! Parity uncertainties (case 0042 and later cases settle them):
//! - General Number shows 15 significant digits and switches to `1E+15` form at ≥ 1e15 or < 1e-4
//!   (VBA's rule); Power BI may show 17 digits or use value-to-text's thresholds.
//! - infinity and NaN render as value-to-text does (`inf`, `-inf`, `-nan(ind)`) under **every**
//!   numeric code — observed only for General Number (case 0002).
//! - a negative that rounds to zero keeps its sign (`-0.00`), as VBA and Excel do.
//! - fr-FR groups with U+202F (narrow no-break space, Windows 10 1803+); older Windows used
//!   U+00A0. de-DE and fr-FR put U+00A0 between the number and `€`.
//! - en-US negative currency is `($1,234.50)` (Windows' pattern 0); en-GB `-£1,234.50`, de-DE and
//!   fr-FR `-1.234,50 €`.
//! - Medium Date is `dd-mmm-yy` and Medium Time `hh:nn AM/PM` in every Locale (VBA); Long Time is
//!   `h:nn:ss AM/PM` in en-US, `hh:nn:ss` elsewhere. AMPM designators are `AM`/`PM` everywhere.
//! - General Date omits a midnight time and shows only the time for serials in day 0 (VBA).
//! - Text that reads as a number is formatted as one; other text comes back unchanged. TRUE is
//!   1 under numeric codes.
//! - Date columns default to Short Date (TMDL from Power BI often shows Long Date); decimal
//!   numbers to General.

use std::fmt;

use crate::value::{DataType, Value, number_text};

/// Longest format string accepted (characters of UTF-8 bytes).
pub const MAX_FORMAT_LEN: usize = 1024;

/// Days from serial 0 (1899-12-30) to 1970-01-01.
const UNIX_EPOCH_DAYS: i64 = 25_569;
/// Serial of 100-01-01 and of 10000-01-01: the dates VBA can show.
const MIN_SERIAL: f64 = -657_434.0;
const MAX_SERIAL: f64 = 2_958_466.0;

/// The locale facts formatting needs, for the four Cultures (CONTEXT.md). `opendash_data`'s
/// `Culture` maps to one by name ([`Locale::get`]).
#[derive(Debug, PartialEq, Eq)]
pub struct Locale {
    /// BCP 47 name, e.g. `en-GB`.
    pub name: &'static str,
    pub decimal: char,
    pub thousands: char,
    pub date_separator: char,
    pub time_separator: char,
    /// The Currency named format, as a custom code.
    pub currency: &'static str,
    /// Short Date, Long Date and Long Time, as custom codes.
    pub short_date: &'static str,
    pub long_date: &'static str,
    pub long_time: &'static str,
    /// January first.
    pub months: [&'static str; 12],
    pub months_short: [&'static str; 12],
    /// Sunday first.
    pub days: [&'static str; 7],
    pub days_short: [&'static str; 7],
    pub am: &'static str,
    pub pm: &'static str,
}

const EN_MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const EN_MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
const EN_DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const EN_DAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

pub static EN_US: Locale = Locale {
    name: "en-US",
    decimal: '.',
    thousands: ',',
    date_separator: '/',
    time_separator: ':',
    currency: "\"$\"#,##0.00;(\"$\"#,##0.00)",
    short_date: "m/d/yyyy",
    long_date: "dddd, mmmm d, yyyy",
    long_time: "h:nn:ss AMPM",
    months: EN_MONTHS,
    months_short: EN_MONTHS_SHORT,
    days: EN_DAYS,
    days_short: EN_DAYS_SHORT,
    am: "AM",
    pm: "PM",
};

pub static EN_GB: Locale = Locale {
    name: "en-GB",
    decimal: '.',
    thousands: ',',
    date_separator: '/',
    time_separator: ':',
    currency: "\"£\"#,##0.00;-\"£\"#,##0.00",
    short_date: "dd/mm/yyyy",
    long_date: "dd mmmm yyyy",
    long_time: "hh:nn:ss",
    months: EN_MONTHS,
    months_short: EN_MONTHS_SHORT,
    days: EN_DAYS,
    days_short: EN_DAYS_SHORT,
    am: "AM",
    pm: "PM",
};

pub static DE_DE: Locale = Locale {
    name: "de-DE",
    decimal: ',',
    thousands: '.',
    date_separator: '.',
    time_separator: ':',
    currency: "#,##0.00\"\u{A0}€\";-#,##0.00\"\u{A0}€\"",
    short_date: "dd/mm/yyyy",
    long_date: "dddd, d\". \"mmmm yyyy",
    long_time: "hh:nn:ss",
    months: [
        "Januar",
        "Februar",
        "März",
        "April",
        "Mai",
        "Juni",
        "Juli",
        "August",
        "September",
        "Oktober",
        "November",
        "Dezember",
    ],
    months_short: [
        "Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez",
    ],
    days: [
        "Sonntag",
        "Montag",
        "Dienstag",
        "Mittwoch",
        "Donnerstag",
        "Freitag",
        "Samstag",
    ],
    days_short: ["So", "Mo", "Di", "Mi", "Do", "Fr", "Sa"],
    am: "AM",
    pm: "PM",
};

pub static FR_FR: Locale = Locale {
    name: "fr-FR",
    decimal: ',',
    thousands: '\u{202F}',
    date_separator: '/',
    time_separator: ':',
    currency: "#,##0.00\"\u{A0}€\";-#,##0.00\"\u{A0}€\"",
    short_date: "dd/mm/yyyy",
    long_date: "dddd d mmmm yyyy",
    long_time: "hh:nn:ss",
    months: [
        "janvier",
        "février",
        "mars",
        "avril",
        "mai",
        "juin",
        "juillet",
        "août",
        "septembre",
        "octobre",
        "novembre",
        "décembre",
    ],
    months_short: [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ],
    days: [
        "dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi",
    ],
    days_short: ["dim.", "lun.", "mar.", "mer.", "jeu.", "ven.", "sam."],
    am: "AM",
    pm: "PM",
};

/// The four Locales.
pub static LOCALES: [&Locale; 4] = [&EN_US, &EN_GB, &DE_DE, &FR_FR];

impl Locale {
    /// Looks a Locale up by name (case-insensitive; `_` works for `-`).
    pub fn get(name: &str) -> Option<&'static Locale> {
        let name = name.trim().replace('_', "-");
        LOCALES
            .iter()
            .copied()
            .find(|l| l.name.eq_ignore_ascii_case(&name))
    }
}

/// Why a value couldn't be formatted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FormatError {
    /// The format string is longer than [`MAX_FORMAT_LEN`] bytes.
    TooLong(usize),
    /// More than four `;` sections.
    TooManySections,
    /// A date code applied to a number outside 100-01-01..9999-12-31.
    DateRange,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong(n) => write!(
                f,
                "the format string is {n} characters long; the most is {MAX_FORMAT_LEN}"
            ),
            Self::TooManySections => {
                f.write_str("a format string has at most four sections separated by ';'")
            }
            Self::DateRange => {
                f.write_str("the value is outside the dates a format can show (years 100–9999)")
            }
        }
    }
}

impl std::error::Error for FormatError {}

type Result<T> = std::result::Result<T, FormatError>;

/// What a Column or Measure of some type is formatted as until the user picks a format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatKind {
    Whole,
    Decimal,
    /// Fixed decimal number.
    Currency,
    Date,
    DateTime,
    Time,
    Boolean,
    Text,
}

impl From<DataType> for FormatKind {
    fn from(t: DataType) -> Self {
        match t {
            DataType::Integer => Self::Whole,
            DataType::Decimal => Self::Decimal,
            DataType::Currency => Self::Currency,
            DataType::DateTime => Self::DateTime,
            DataType::Boolean => Self::Boolean,
            DataType::Text | DataType::Blank => Self::Text,
        }
    }
}

/// Power BI's default format string for `kind` (see the module doc; empty is General).
pub fn default_format(kind: FormatKind, locale: &Locale) -> &'static str {
    match kind {
        FormatKind::Whole => "0",
        FormatKind::Currency => locale.currency,
        FormatKind::Date => "Short Date",
        FormatKind::DateTime => "General Date",
        FormatKind::Time => "Long Time",
        FormatKind::Decimal | FormatKind::Boolean | FormatKind::Text => "",
    }
}

/// `value` formatted with `code` in `locale` (DAX `FORMAT`). BLANK is `""`.
pub fn format(value: &Value, code: &str, locale: &Locale) -> Result<String> {
    if code.len() > MAX_FORMAT_LEN {
        return Err(FormatError::TooLong(code.len()));
    }
    let input = match Input::of(value) {
        Input::Blank => return Ok(String::new()),
        Input::Text(s) => return Ok(s),
        Input::Number(n) => n,
    };
    if code.is_empty() {
        return Ok(match value {
            Value::DateTime(_) => general_date(input.serial, locale)?,
            Value::Boolean(_) => value.to_text(),
            _ => general_number(&input, locale),
        });
    }
    if let Some(named) = Named::parse(code) {
        return named.apply(&input, locale);
    }
    custom(&input, code, locale)
}

/// A value to format, as a number.
struct Number {
    neg: bool,
    dec: Dec,
    /// Whole and fixed decimal numbers: exact, never rounded to 15 digits.
    exact: bool,
    /// The value itself (a date serial); may be infinite or NaN.
    serial: f64,
}

impl Number {
    fn finite(&self) -> bool {
        self.serial.is_finite()
    }
}

enum Input {
    Blank,
    Text(String),
    Number(Number),
}

impl Input {
    fn of(v: &Value) -> Input {
        let int = |i: i64, scale: i64| {
            Input::Number(Number {
                neg: i < 0,
                dec: Dec::from_int(u128::from(i.unsigned_abs()), scale),
                exact: true,
                serial: i as f64 / 10f64.powi(scale as i32),
            })
        };
        match v {
            Value::Blank => Input::Blank,
            Value::Boolean(b) => int(i64::from(*b), 0),
            Value::Integer(i) => int(*i, 0),
            Value::Currency(c) => int(*c, 4),
            Value::Decimal(d) | Value::DateTime(d) => Input::Number(Number {
                neg: *d < 0.0,
                dec: Dec::from_f64(d.abs()),
                exact: false,
                serial: *d,
            }),
            Value::Text(s) => match parse_number(s) {
                Some(d) => Input::of(&Value::Decimal(d)),
                None => Input::Text(s.to_string()),
            },
        }
    }
}

/// Text read as a number: plain decimal notation only.
fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let ok = t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if ok {
        t.parse().ok().filter(|d: &f64| d.is_finite())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Decimal digits

/// A non-negative decimal: `0.d₁d₂… × 10^exp`, no leading or trailing zero digits (zero: none).
#[derive(Clone, Debug, PartialEq)]
struct Dec {
    digits: Vec<u8>,
    exp: i64,
}

impl Dec {
    fn zero() -> Self {
        Self {
            digits: Vec::new(),
            exp: 0,
        }
    }

    /// The shortest round-trip digits of finite `a ≥ 0`; non-finite is zero (callers check).
    fn from_f64(a: f64) -> Self {
        if !a.is_finite() || a == 0.0 {
            return Self::zero();
        }
        let s = format!("{a:e}");
        let Some((mantissa, e)) = s.split_once('e') else {
            return Self::zero();
        };
        let e: i64 = e.parse().unwrap_or(0);
        let digits = mantissa
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|b| b - b'0')
            .collect();
        Self::normal(digits, e.saturating_add(1))
    }

    /// `u × 10^-scale`.
    fn from_int(u: u128, scale: i64) -> Self {
        let s = u.to_string();
        let digits: Vec<u8> = s.bytes().map(|b| b.saturating_sub(b'0')).collect();
        let len = i64::try_from(digits.len()).unwrap_or(0);
        Self::normal(digits, len.saturating_sub(scale))
    }

    fn normal(mut digits: Vec<u8>, mut exp: i64) -> Self {
        let lead = digits.iter().take_while(|&&d| d == 0).count();
        digits.drain(..lead);
        exp = exp.saturating_sub(i64::try_from(lead).unwrap_or(0));
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            exp = 0;
        }
        Self { digits, exp }
    }

    fn is_zero(&self) -> bool {
        self.digits.is_empty()
    }

    /// × 10^k.
    fn shift(&mut self, k: i64) {
        if !self.is_zero() {
            self.exp = self.exp.saturating_add(k);
        }
    }

    /// Keeps the first `keep` digits, rounding half away from zero.
    fn round_at(&mut self, keep: i64) {
        let Ok(keep) = usize::try_from(keep) else {
            // Every digit is below the rounding position by at least one place: < 0.5 units.
            *self = Self::zero();
            return;
        };
        if keep >= self.digits.len() {
            return;
        }
        let up = self.digits.get(keep).is_some_and(|&d| d >= 5);
        self.digits.truncate(keep);
        if up {
            while self.digits.last() == Some(&9) {
                self.digits.pop();
            }
            match self.digits.last_mut() {
                Some(d) => *d += 1,
                None => {
                    self.digits.push(1);
                    self.exp = self.exp.saturating_add(1);
                }
            }
        }
        *self = Self::normal(std::mem::take(&mut self.digits), self.exp);
    }

    /// Rounds to `n` digits after the decimal point.
    fn round_frac(&mut self, n: usize) {
        self.round_at(self.exp.saturating_add(to_i64(n)));
    }

    /// The digits before the decimal point (none for a value below 1).
    fn int_digits(&self) -> Vec<u8> {
        (0..self.exp.max(0))
            .map(|i| self.digit(i))
            .collect::<Vec<_>>()
    }

    /// The first `n` digits after the decimal point.
    fn frac_digits(&self, n: usize) -> Vec<u8> {
        (0..to_i64(n))
            .map(|j| self.digit(self.exp.saturating_add(j)))
            .collect()
    }

    fn digit(&self, i: i64) -> u8 {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.digits.get(i))
            .copied()
            .unwrap_or(0)
    }
}

fn to_i64(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn digit_char(d: u8) -> char {
    char::from(b'0'.saturating_add(d.min(9)))
}

// ---------------------------------------------------------------------------------------------
// Named formats

#[derive(Clone, Copy)]
enum Named {
    GeneralNumber,
    Currency,
    Fixed,
    Standard,
    Percent,
    Scientific,
    YesNo,
    TrueFalse,
    OnOff,
    GeneralDate,
    LongDate,
    MediumDate,
    ShortDate,
    LongTime,
    MediumTime,
    ShortTime,
}

impl Named {
    fn parse(code: &str) -> Option<Self> {
        const NAMES: [(&str, Named); 16] = [
            ("General Number", Named::GeneralNumber),
            ("Currency", Named::Currency),
            ("Fixed", Named::Fixed),
            ("Standard", Named::Standard),
            ("Percent", Named::Percent),
            ("Scientific", Named::Scientific),
            ("Yes/No", Named::YesNo),
            ("True/False", Named::TrueFalse),
            ("On/Off", Named::OnOff),
            ("General Date", Named::GeneralDate),
            ("Long Date", Named::LongDate),
            ("Medium Date", Named::MediumDate),
            ("Short Date", Named::ShortDate),
            ("Long Time", Named::LongTime),
            ("Medium Time", Named::MediumTime),
            ("Short Time", Named::ShortTime),
        ];
        let code = code.trim();
        NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(code))
            .map(|&(_, f)| f)
    }

    fn apply(self, n: &Number, locale: &Locale) -> Result<String> {
        let pair = |yes: &str, no: &str| {
            let truthy = n.serial != 0.0;
            Ok(if truthy { yes } else { no }.to_string())
        };
        match self {
            Named::GeneralNumber => Ok(general_number(n, locale)),
            Named::Currency => custom(n, locale.currency, locale),
            Named::Fixed => custom(n, "0.00", locale),
            Named::Standard => custom(n, "#,##0.00", locale),
            Named::Percent => custom(n, "0.00%", locale),
            Named::Scientific => custom(n, "0.00E+00", locale),
            Named::YesNo => pair("Yes", "No"),
            Named::TrueFalse => pair("True", "False"),
            Named::OnOff => pair("On", "Off"),
            Named::GeneralDate => general_date(n.serial, locale),
            Named::LongDate => date_code(n.serial, locale.long_date, locale),
            Named::MediumDate => date_code(n.serial, "dd-mmm-yy", locale),
            Named::ShortDate => date_code(n.serial, locale.short_date, locale),
            Named::LongTime => date_code(n.serial, locale.long_time, locale),
            Named::MediumTime => date_code(n.serial, "hh:nn AM/PM", locale),
            Named::ShortTime => date_code(n.serial, "hh:nn", locale),
        }
    }
}

/// General Number: no grouping, 15 significant digits (exact for whole and fixed decimal
/// numbers), `1.5E+20` form at ≥ 1e15 or < 1e-4.
fn general_number(n: &Number, locale: &Locale) -> String {
    if !n.finite() {
        return number_text(n.serial);
    }
    let mut dec = n.dec.clone();
    if !n.exact {
        dec.round_at(15);
    }
    let mut out = String::new();
    if n.neg && !dec.is_zero() {
        out.push('-');
    }
    let scientific = !n.exact && !dec.is_zero() && (dec.exp >= 16 || dec.exp <= -4);
    if scientific {
        let mut digits = dec.digits.iter().map(|&d| digit_char(d));
        out.extend(digits.next());
        let rest: String = digits.collect();
        if !rest.is_empty() {
            out.push(locale.decimal);
            out.push_str(&rest);
        }
        let e = dec.exp.saturating_sub(1);
        out.push_str(&format!("E{}{:02}", if e < 0 { '-' } else { '+' }, e.abs()));
        return out;
    }
    let int = dec.int_digits();
    if int.is_empty() {
        out.push('0');
    }
    out.extend(int.iter().map(|&d| digit_char(d)));
    let frac_len = usize::try_from(to_i64(dec.digits.len()).saturating_sub(dec.exp)).unwrap_or(0);
    if frac_len > 0 {
        out.push(locale.decimal);
        out.extend(dec.frac_digits(frac_len).iter().map(|&d| digit_char(d)));
    }
    out
}

// ---------------------------------------------------------------------------------------------
// Custom codes

/// A format-string character after quoting: literal text, or a character to interpret.
#[derive(Clone, Debug, PartialEq)]
enum Piece {
    Lit(String),
    Ch(char),
}

/// Splits `code` into its `;` sections of pieces.
fn sections(code: &str) -> Result<Vec<Vec<Piece>>> {
    let mut out = vec![Vec::new()];
    let mut chars = code.chars();
    while let Some(c) = chars.next() {
        let piece = match c {
            '"' => Piece::Lit(chars.by_ref().take_while(|&c| c != '"').collect()),
            '\\' => match chars.next() {
                Some(n) => Piece::Lit(n.to_string()),
                None => continue,
            },
            ';' => {
                if out.len() >= 4 {
                    return Err(FormatError::TooManySections);
                }
                out.push(Vec::new());
                continue;
            }
            c => Piece::Ch(c),
        };
        if let Some(s) = out.last_mut() {
            s.push(piece);
        }
    }
    Ok(out)
}

fn custom(n: &Number, code: &str, locale: &Locale) -> Result<String> {
    let sections = sections(code)?;
    let zero = n.dec.is_zero();
    let pick = |i: usize| sections.get(i).filter(|s| !s.is_empty());
    let first = sections.first().map(Vec::as_slice).unwrap_or_default();
    let (section, minus) = if zero {
        (pick(2).map_or(first, Vec::as_slice), false)
    } else if n.neg {
        match pick(1) {
            Some(s) => (s.as_slice(), false),
            None => (first, true),
        }
    } else {
        (first, false)
    };
    let tokens = date_tokens(section);
    if tokens.iter().any(DTok::is_field) {
        let mut out = String::new();
        render_date(&tokens, &parts(n.serial)?, locale, 0, &mut out);
        return Ok(out);
    }
    if !n.finite() {
        return Ok(number_text(n.serial));
    }
    Ok(number_code(n, section, minus, locale))
}

// ----- numbers

#[derive(Clone, Debug, PartialEq)]
enum NTok {
    /// `0` (`zero: true`) or `#`.
    Digit {
        zero: bool,
    },
    Point,
    Comma,
    /// `E+`, `e-`…
    Exp {
        upper: bool,
        plus: bool,
    },
    Lit(String),
}

fn number_tokens(section: &[Piece]) -> Vec<NTok> {
    let mut out = Vec::with_capacity(section.len());
    let mut i = 0;
    while let Some(p) = section.get(i) {
        i += 1;
        out.push(match p {
            Piece::Lit(s) => NTok::Lit(s.clone()),
            Piece::Ch('0') => NTok::Digit { zero: true },
            Piece::Ch('#') => NTok::Digit { zero: false },
            Piece::Ch('.') => NTok::Point,
            Piece::Ch(',') => NTok::Comma,
            Piece::Ch(c @ ('E' | 'e')) => match section.get(i) {
                Some(Piece::Ch(s @ ('+' | '-'))) => {
                    i += 1;
                    NTok::Exp {
                        upper: *c == 'E',
                        plus: *s == '+',
                    }
                }
                _ => NTok::Lit(c.to_string()),
            },
            Piece::Ch(c) => NTok::Lit(c.to_string()),
        });
    }
    out
}

fn number_code(n: &Number, section: &[Piece], minus: bool, locale: &Locale) -> String {
    let tokens = number_tokens(section);
    let split = |t: &NTok| matches!(t, NTok::Point | NTok::Exp { .. });
    let int_end = tokens.iter().position(split).unwrap_or(tokens.len());
    let point = matches!(tokens.get(int_end), Some(NTok::Point));
    let exp_at = tokens.iter().position(|t| matches!(t, NTok::Exp { .. }));
    let frac_end = exp_at.unwrap_or(tokens.len());
    let int = tokens.get(..int_end).unwrap_or_default();
    let frac = if point {
        tokens.get(int_end + 1..frac_end).unwrap_or_default()
    } else {
        &[]
    };
    let is_digit = |t: &NTok| matches!(t, NTok::Digit { .. });

    // Commas: grouping between integer placeholders, ÷1000 scaling after the last placeholder.
    let number = tokens.get(..frac_end).unwrap_or_default();
    let first_digit = number.iter().position(is_digit);
    let last_digit = number.iter().rposition(is_digit);
    let (mut group, mut scale) = (false, 0i64);
    for (i, t) in number.iter().enumerate() {
        if *t != NTok::Comma || first_digit.is_none_or(|f| f > i) {
            continue;
        }
        if last_digit.is_none_or(|l| l < i) {
            scale += 1;
        } else if i < int_end {
            group = true;
        }
    }
    // A quoted `"%"` is text; a bare `%` also multiplies by 100.
    let percents = to_i64(
        section
            .iter()
            .filter(|p| matches!(p, Piece::Ch('%')))
            .count(),
    );
    let mut dec = n.dec.clone();
    dec.shift(
        percents
            .saturating_mul(2)
            .saturating_sub(scale.saturating_mul(3)),
    );
    let frac_places = frac.iter().filter(|t| is_digit(t)).count();
    let any_digits = tokens.iter().any(is_digit);

    let mut out = String::new();
    if minus {
        out.push('-');
    }
    // Scientific: the mantissa has as many integer digits as placeholders (at least one).
    let mut exponent = 0i64;
    if exp_at.is_some() {
        let places = to_i64(int.iter().filter(|t| is_digit(t)).count().max(1));
        if !dec.is_zero() {
            exponent = dec.exp.saturating_sub(places);
            dec.exp = places;
            dec.round_frac(frac_places);
            if dec.exp > places {
                dec.exp = places;
                exponent = exponent.saturating_add(1);
            }
        }
    } else {
        dec.round_frac(frac_places);
    }

    render_int(int, &dec.int_digits(), group, any_digits, locale, &mut out);
    if point {
        out.push(locale.decimal);
    }
    render_frac(frac, &dec.frac_digits(frac_places), &mut out);
    if let Some(at) = exp_at {
        let rest = tokens.get(at..).unwrap_or_default();
        render_exp(rest, exponent, &mut out);
    }
    out
}

/// The integer placeholders, right-aligned; the leftmost takes any extra digits.
fn render_int(
    int: &[NTok],
    digits: &[u8],
    group: bool,
    any_digits: bool,
    locale: &Locale,
    out: &mut String,
) {
    let places = int
        .iter()
        .filter(|t| matches!(t, NTok::Digit { .. }))
        .count();
    let n = digits.len();
    let emit = |pos: usize, d: u8, out: &mut String| {
        out.push(digit_char(d));
        if group && pos > 0 && pos.is_multiple_of(3) {
            out.push(locale.thousands);
        }
    };
    if places == 0 {
        for t in int {
            if let NTok::Lit(s) = t {
                out.push_str(s);
            }
        }
        // `.00` still shows the integer digits.
        if any_digits {
            for (i, &d) in digits.iter().enumerate() {
                emit(n - 1 - i, d, out);
            }
        }
        return;
    }
    let mut k = places; // placeholders left, counting this one
    for t in int {
        match t {
            NTok::Digit { zero } => {
                k -= 1;
                if k == places - 1 && n > places {
                    for (i, &d) in digits.iter().take(n - places).enumerate() {
                        emit(n - 1 - i, d, out);
                    }
                }
                match n.checked_sub(1 + k).and_then(|i| digits.get(i)) {
                    Some(&d) => emit(k, d, out),
                    None if *zero => emit(k, 0, out),
                    None => {}
                }
            }
            NTok::Lit(s) => out.push_str(s),
            _ => {}
        }
    }
}

/// The fraction placeholders; trailing zeros under `#` are dropped.
fn render_frac(frac: &[NTok], digits: &[u8], out: &mut String) {
    let zero_flags: Vec<bool> = frac
        .iter()
        .filter_map(|t| match t {
            NTok::Digit { zero } => Some(*zero),
            _ => None,
        })
        .collect();
    let mut show = digits.len();
    while show > 0 && zero_flags.get(show - 1) == Some(&false) && digits.get(show - 1) == Some(&0) {
        show -= 1;
    }
    let mut i = 0;
    for t in frac {
        match t {
            NTok::Digit { .. } => {
                if i < show {
                    out.push(digit_char(digits.get(i).copied().unwrap_or(0)));
                }
                i += 1;
            }
            NTok::Lit(s) => out.push_str(s),
            _ => {}
        }
    }
}

/// `E+00` and what follows it.
fn render_exp(tokens: &[NTok], exponent: i64, out: &mut String) {
    let mut iter = tokens.iter();
    if let Some(NTok::Exp { upper, plus }) = iter.next() {
        out.push(if *upper { 'E' } else { 'e' });
        if exponent < 0 {
            out.push('-');
        } else if *plus {
            out.push('+');
        }
    }
    let zeros = tokens
        .iter()
        .filter(|t| matches!(t, NTok::Digit { zero: true }))
        .count();
    let digits = format!("{:0>zeros$}", exponent.unsigned_abs());
    let mut placed = false;
    for t in iter {
        match t {
            NTok::Digit { .. } if !placed => {
                out.push_str(&digits);
                placed = true;
            }
            NTok::Lit(s) => out.push_str(s),
            _ => {}
        }
    }
    if !placed {
        out.push_str(&digits);
    }
}

// ----- dates

#[derive(Clone, Copy, Debug, PartialEq)]
enum AmPm {
    /// `AM/PM` / `am/pm`.
    Full { upper: bool },
    /// `A/P` / `a/p`.
    Short { upper: bool },
    /// `AMPM`: the Locale's designators.
    Locale,
}

#[derive(Clone, Debug, PartialEq)]
enum DTok {
    Lit(String),
    DateSep,
    TimeSep,
    /// `d`…`dddd`; 5 is Short Date, 6 Long Date.
    Day(u8),
    Weekday,
    Week,
    Month(u8),
    Minute(u8),
    Quarter,
    DayOfYear,
    Year2,
    Year4,
    Hour(u8),
    Second(u8),
    AmPm(AmPm),
    /// `c`.
    General,
    /// `ttttt`.
    LongTime,
}

impl DTok {
    /// A date or time field (what makes a section a date code).
    fn is_field(&self) -> bool {
        !matches!(self, DTok::Lit(_) | DTok::DateSep | DTok::TimeSep)
    }
}

fn date_tokens(section: &[Piece]) -> Vec<DTok> {
    let lower = |i: usize| match section.get(i) {
        Some(Piece::Ch(c)) => Some(c.to_ascii_lowercase()),
        _ => None,
    };
    let matches_at = |i: usize, word: &str| {
        word.chars()
            .enumerate()
            .all(|(j, w)| lower(i + j) == Some(w))
    };
    let mut out = Vec::with_capacity(section.len());
    let mut i = 0;
    while let Some(p) = section.get(i) {
        let c = match p {
            Piece::Lit(s) => {
                out.push(DTok::Lit(s.clone()));
                i += 1;
                continue;
            }
            Piece::Ch(c) => *c,
        };
        let l = c.to_ascii_lowercase();
        let mut run = 1;
        if l.is_ascii_alphabetic() {
            while lower(i + run) == Some(l) {
                run += 1;
            }
        }
        let upper = c.is_ascii_uppercase();
        let count = u8::try_from(run).unwrap_or(u8::MAX);
        let (tok, used) = match l {
            'd' => (DTok::Day(count.min(6)), run),
            'w' if run == 1 => (DTok::Weekday, 1),
            'w' => (DTok::Week, run),
            'm' => (DTok::Month(count.min(4)), run),
            'q' => (DTok::Quarter, run),
            'y' if run == 1 => (DTok::DayOfYear, 1),
            'y' if run < 4 => (DTok::Year2, run),
            'y' => (DTok::Year4, run),
            'h' => (DTok::Hour(count.min(2)), run),
            'n' => (DTok::Minute(count.min(2)), run),
            's' => (DTok::Second(count.min(2)), run),
            'c' => (DTok::General, 1),
            't' if run == 5 => (DTok::LongTime, 5),
            'a' if matches_at(i, "am/pm") => (DTok::AmPm(AmPm::Full { upper }), 5),
            'a' if matches_at(i, "ampm") => (DTok::AmPm(AmPm::Locale), 4),
            'a' if matches_at(i, "a/p") => (DTok::AmPm(AmPm::Short { upper }), 3),
            '/' => (DTok::DateSep, 1),
            ':' => (DTok::TimeSep, 1),
            _ => (DTok::Lit(c.to_string()), 1),
        };
        out.push(tok);
        i += used;
    }
    // `m`/`mm` right after an hour or right before a second is minutes.
    let field_near = |toks: &[DTok], from: usize, back: bool| -> Option<DTok> {
        let mut j = from;
        loop {
            j = if back { j.checked_sub(1)? } else { j + 1 };
            match toks.get(j)? {
                t if t.is_field() => return Some(t.clone()),
                _ => {}
            }
        }
    };
    let snapshot = out.clone();
    for (i, t) in out.iter_mut().enumerate() {
        if let DTok::Month(n @ (1 | 2)) = *t {
            let after_hour = matches!(field_near(&snapshot, i, true), Some(DTok::Hour(_)));
            let before_second = matches!(field_near(&snapshot, i, false), Some(DTok::Second(_)));
            if after_hour || before_second {
                *t = DTok::Minute(n);
            }
        }
    }
    out
}

/// A date serial's calendar fields.
struct Parts {
    /// Days since serial 0 (1899-12-30).
    days: i64,
    year: i64,
    month: i64,
    day: i64,
    hour: i64,
    minute: i64,
    second: i64,
}

fn parts(serial: f64) -> Result<Parts> {
    if !serial.is_finite() || !(MIN_SERIAL..MAX_SERIAL).contains(&serial) {
        return Err(FormatError::DateRange);
    }
    // VBA: the integer part is the day (towards zero), the fraction's size the time of day.
    let int = serial.trunc();
    let mut days = int as i64;
    let mut secs = ((serial - int).abs() * 86_400.0).round() as i64;
    if secs >= 86_400 {
        secs -= 86_400;
        days += if serial < 0.0 { -1 } else { 1 };
    }
    let (year, month, day) = civil(days - UNIX_EPOCH_DAYS);
    Ok(Parts {
        days,
        year,
        month,
        day,
        hour: secs / 3600,
        minute: secs / 60 % 60,
        second: secs % 60,
    })
}

impl Parts {
    /// 1 = Sunday (serial 1, 1899-12-31, was a Sunday).
    fn weekday(&self) -> i64 {
        (self.days - 1).rem_euclid(7) + 1
    }

    fn day_of_year(&self) -> i64 {
        const BEFORE: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
        let y = self.year;
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let before = usize::try_from(self.month - 1)
            .ok()
            .and_then(|m| BEFORE.get(m))
            .copied()
            .unwrap_or(0);
        before + self.day + i64::from(leap && self.month > 2)
    }

    /// Week of the year, weeks starting Sunday, the week of 1 January is 1.
    fn week(&self) -> i64 {
        let doy = self.day_of_year();
        let jan1 = (self.weekday() - 1 - (doy - 1)).rem_euclid(7); // 0 = Sunday
        (doy - 1 + jan1) / 7 + 1
    }
}

/// (year, month, day) for days since 1970-01-01 (H. Hinnant's algorithm).
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

fn name<const N: usize>(names: &[&'static str; N], i: i64) -> &'static str {
    usize::try_from(i - 1)
        .ok()
        .and_then(|i| names.get(i))
        .copied()
        .unwrap_or("")
}

/// `serial` with the custom date code `code` (a Locale pattern; never nests further).
fn date_code(serial: f64, code: &str, locale: &Locale) -> Result<String> {
    let p = parts(serial)?;
    let mut out = String::new();
    let tokens = date_tokens(
        sections(code)?
            .first()
            .map(Vec::as_slice)
            .unwrap_or_default(),
    );
    render_date(&tokens, &p, locale, 0, &mut out);
    Ok(out)
}

/// General Date: Short Date and Long Time; a midnight shows the date only, day 0 the time only.
fn general_date(serial: f64, locale: &Locale) -> Result<String> {
    let p = parts(serial)?;
    let mut out = String::new();
    render_general(&p, locale, 1, &mut out);
    Ok(out)
}

fn render_general(p: &Parts, locale: &Locale, depth: u8, out: &mut String) {
    let time = p.hour != 0 || p.minute != 0 || p.second != 0;
    if p.days != 0 || !time {
        render_pattern(locale.short_date, p, locale, depth, out);
        if time {
            out.push(' ');
        }
    }
    if time {
        render_pattern(locale.long_time, p, locale, depth, out);
    }
}

fn render_pattern(code: &str, p: &Parts, locale: &Locale, depth: u8, out: &mut String) {
    if let Ok(s) = sections(code) {
        let tokens = date_tokens(s.first().map(Vec::as_slice).unwrap_or_default());
        render_date(&tokens, p, locale, depth, out);
    }
}

fn render_date(tokens: &[DTok], p: &Parts, locale: &Locale, depth: u8, out: &mut String) {
    let twelve = tokens.iter().any(|t| matches!(t, DTok::AmPm(_)));
    let num = |out: &mut String, v: i64, width: usize| out.push_str(&format!("{v:0width$}"));
    for t in tokens {
        match t {
            DTok::Lit(s) => out.push_str(s),
            DTok::DateSep => out.push(locale.date_separator),
            DTok::TimeSep => out.push(locale.time_separator),
            DTok::Day(1) => num(out, p.day, 1),
            DTok::Day(2) => num(out, p.day, 2),
            DTok::Day(3) => out.push_str(name(&locale.days_short, p.weekday())),
            DTok::Day(4) => out.push_str(name(&locale.days, p.weekday())),
            // Locale patterns contain no nested patterns; depth only guards against it.
            DTok::Day(5) if depth == 0 => render_pattern(locale.short_date, p, locale, 1, out),
            DTok::Day(_) if depth == 0 => render_pattern(locale.long_date, p, locale, 1, out),
            DTok::LongTime if depth == 0 => render_pattern(locale.long_time, p, locale, 1, out),
            DTok::General if depth == 0 => render_general(p, locale, 1, out),
            DTok::Day(_) | DTok::LongTime | DTok::General => {}
            DTok::Weekday => num(out, p.weekday(), 1),
            DTok::Week => num(out, p.week(), 1),
            DTok::Month(1) => num(out, p.month, 1),
            DTok::Month(2) => num(out, p.month, 2),
            DTok::Month(3) => out.push_str(name(&locale.months_short, p.month)),
            DTok::Month(_) => out.push_str(name(&locale.months, p.month)),
            DTok::Quarter => num(out, (p.month - 1) / 3 + 1, 1),
            DTok::DayOfYear => num(out, p.day_of_year(), 1),
            DTok::Year2 => num(out, p.year.rem_euclid(100), 2),
            DTok::Year4 => num(out, p.year, 4),
            DTok::Hour(n) => {
                let h = if twelve {
                    (p.hour + 11) % 12 + 1
                } else {
                    p.hour
                };
                num(out, h, usize::from(*n));
            }
            DTok::Minute(n) => num(out, p.minute, usize::from(*n)),
            DTok::Second(n) => num(out, p.second, usize::from(*n)),
            DTok::AmPm(kind) => {
                let pm = p.hour >= 12;
                let s = match kind {
                    AmPm::Full { upper: true } => if pm { "PM" } else { "AM" }.to_string(),
                    AmPm::Full { upper: false } => if pm { "pm" } else { "am" }.to_string(),
                    AmPm::Short { upper: true } => if pm { "P" } else { "A" }.to_string(),
                    AmPm::Short { upper: false } => if pm { "p" } else { "a" }.to_string(),
                    AmPm::Locale => if pm { locale.pm } else { locale.am }.to_string(),
                };
                out.push_str(&s);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Value::*;

    fn f(v: Value, code: &str) -> String {
        format(&v, code, &EN_GB).unwrap()
    }

    fn fl(v: Value, code: &str, locale: &str) -> String {
        format(&v, code, Locale::get(locale).unwrap()).unwrap()
    }

    /// 2026-10-14 (a Wednesday) 15:04:05 as a serial.
    const OCT14: f64 = 46_309.0 + (15.0 * 3600.0 + 4.0 * 60.0 + 5.0) / 86_400.0;

    #[test]
    fn blank_is_empty_and_text_passes_through() {
        assert_eq!(f(Blank, "0.00"), "");
        assert_eq!(f(Blank, "Currency"), "");
        assert_eq!(f(Value::text("abc"), "0.00"), "abc");
        assert_eq!(f(Value::text("1.5"), "0.00"), "1.50");
    }

    #[test]
    fn non_finite_as_observed() {
        // Parity case 0002: FORMAT(1/0, "General Number") is `inf`.
        assert_eq!(f(Decimal(f64::INFINITY), "General Number"), "inf");
        assert_eq!(f(Decimal(f64::NEG_INFINITY), "General Number"), "-inf");
        assert_eq!(f(Decimal(f64::NAN), "General Number"), "-nan(ind)");
        assert_eq!(f(Decimal(f64::INFINITY), "#,##0.00"), "inf");
        assert_eq!(
            format(&Decimal(f64::INFINITY), "Short Date", &EN_GB),
            Err(FormatError::DateRange)
        );
    }

    #[test]
    fn general_number() {
        assert_eq!(f(Integer(1234567), "General Number"), "1234567");
        assert_eq!(f(Decimal(-1234.5), "General Number"), "-1234.5");
        assert_eq!(f(Decimal(0.1 + 0.2), "General Number"), "0.3");
        assert_eq!(f(Decimal(1.0 / 3.0), "General Number"), "0.333333333333333");
        assert_eq!(f(Decimal(1.5e20), "General Number"), "1.5E+20");
        assert_eq!(f(Decimal(0.00001), "General Number"), "1E-05");
        assert_eq!(f(Decimal(0.0001), "General Number"), "0.0001");
        assert_eq!(f(Currency(12_345), "General Number"), "1.2345");
        assert_eq!(fl(Decimal(1234.5), "General Number", "de-DE"), "1234,5");
        assert_eq!(f(Decimal(2.5), ""), "2.5");
        assert_eq!(f(Boolean(true), ""), "True");
    }

    #[test]
    fn named_numeric_formats() {
        assert_eq!(f(Decimal(1234.5), "Currency"), "£1,234.50");
        assert_eq!(f(Decimal(-1234.5), "Currency"), "-£1,234.50");
        assert_eq!(fl(Decimal(1234.5), "Currency", "en-US"), "$1,234.50");
        assert_eq!(fl(Decimal(-1234.5), "currency", "en-US"), "($1,234.50)");
        assert_eq!(fl(Decimal(1234.5), "Currency", "de-DE"), "1.234,50\u{A0}€");
        assert_eq!(
            fl(Decimal(1234.5), "Currency", "fr-FR"),
            "1\u{202F}234,50\u{A0}€"
        );
        assert_eq!(f(Decimal(1234.567), "Fixed"), "1234.57");
        assert_eq!(f(Decimal(1234.567), "Standard"), "1,234.57");
        assert_eq!(fl(Decimal(1234.567), "Standard", "de-DE"), "1.234,57");
        assert_eq!(f(Decimal(0.256), "Percent"), "25.60%");
        assert_eq!(f(Decimal(1234.5), "Scientific"), "1.23E+03");
        assert_eq!(f(Decimal(0.000123), "Scientific"), "1.23E-04");
        assert_eq!(f(Integer(0), "Scientific"), "0.00E+00");
        assert_eq!(f(Integer(2), "Yes/No"), "Yes");
        assert_eq!(f(Integer(0), "Yes/No"), "No");
        assert_eq!(f(Boolean(true), "True/False"), "True");
        assert_eq!(f(Decimal(0.0), "On/Off"), "Off");
    }

    #[test]
    fn rounding_is_half_away_from_zero_on_decimal_digits() {
        assert_eq!(f(Decimal(2.5), "0"), "3");
        assert_eq!(f(Decimal(-2.5), "0"), "-3");
        assert_eq!(f(Decimal(2.675), "0.00"), "2.68");
        assert_eq!(f(Decimal(0.125), "0.00"), "0.13");
        assert_eq!(f(Decimal(9.995), "0.00"), "10.00");
        assert_eq!(f(Decimal(0.4), "0"), "0");
        assert_eq!(f(Decimal(0.004), "0.00"), "0.00");
        assert_eq!(f(Decimal(-0.001), "0.00"), "-0.00");
        assert_eq!(f(Currency(5_000), "0"), "1");
        assert_eq!(f(Integer(i64::MIN), "#,##0"), "-9,223,372,036,854,775,808");
    }

    #[test]
    fn custom_numeric_codes() {
        assert_eq!(f(Decimal(1234.5), "#,##0.00"), "1,234.50");
        assert_eq!(f(Decimal(5.0), "#,##0"), "5");
        assert_eq!(f(Decimal(5.0), "0,000"), "0,005");
        assert_eq!(f(Decimal(0.5), "#.##"), ".5");
        assert_eq!(f(Decimal(12.5), ".00"), "12.50");
        assert_eq!(f(Decimal(1.5), "0.0#"), "1.5");
        assert_eq!(f(Decimal(1.25), "0.0#"), "1.25");
        assert_eq!(f(Integer(1234567), "#,##0,"), "1,235");
        assert_eq!(f(Integer(1234567), "0.0,,"), "1.2");
        assert_eq!(f(Decimal(0.5), "0%"), "50%");
        assert_eq!(f(Integer(42), "\"N=\"0"), "N=42");
        assert_eq!(f(Integer(42), "0\\k"), "42k");
        assert_eq!(f(Integer(42), "$0"), "$42");
        assert_eq!(f(Integer(5551234), "000-0000"), "555-1234");
        assert_eq!(f(Integer(123456), "0.0e-0"), "1.2e5");
        assert_eq!(f(Decimal(0.0012), "0.0E+00"), "1.2E-03");
        assert_eq!(f(Decimal(99_999.0), "0.0E+00"), "1.0E+05");
        assert_eq!(fl(Decimal(1234.5), "#,##0.00", "fr-FR"), "1\u{202F}234,50");
        assert_eq!(f(Integer(7), "\"just text\""), "just text");
    }

    #[test]
    fn sections_pick_by_sign() {
        let code = "0.00;(0.00);\"zero\"";
        assert_eq!(f(Decimal(1.5), code), "1.50");
        assert_eq!(f(Decimal(-1.5), code), "(1.50)");
        assert_eq!(f(Integer(0), code), "zero");
        assert_eq!(f(Decimal(-1.5), "0.00;"), "-1.50");
        assert_eq!(f(Integer(0), "0;-0"), "0");
        assert_eq!(f(Integer(1), "0;-0;;\"null\""), "1");
        assert_eq!(
            format(&Integer(1), "0;0;0;0;0", &EN_GB),
            Err(FormatError::TooManySections)
        );
        // Semicolons inside quotes don't split.
        assert_eq!(f(Integer(1), "0\";\"0"), "0;1");
    }

    #[test]
    fn hostile_codes_are_bounded() {
        let long = "0".repeat(MAX_FORMAT_LEN + 1);
        assert_eq!(
            format(&Integer(1), &long, &EN_GB),
            Err(FormatError::TooLong(MAX_FORMAT_LEN + 1))
        );
        let max = "%".repeat(MAX_FORMAT_LEN - 1) + "0";
        assert!(format(&Decimal(f64::MAX), &max, &EN_GB).is_ok());
        let commas = "0".to_string() + &",".repeat(MAX_FORMAT_LEN - 1);
        assert_eq!(f(Decimal(f64::MIN_POSITIVE), &commas), "0");
        assert!(format(&Decimal(1e300), &"0".repeat(MAX_FORMAT_LEN), &EN_GB).is_ok());
        for code in [
            "\"", "\\", "E+", "e", ";;;", "0.0.0", ",", ".", "AM/", "a/p",
        ] {
            assert!(format(&Decimal(-12.5), code, &EN_GB).is_ok(), "{code}");
        }
        assert_eq!(
            format(&Decimal(1e9), "dd/mm/yyyy", &EN_GB),
            Err(FormatError::DateRange)
        );
        // Every three-character code over the special characters, on awkward values.
        let chars: Vec<char> = "0#.,%Ee+-\"\\;dmyhnsAPa/:c".chars().collect();
        let values = [
            Integer(i64::MIN),
            Decimal(-0.5),
            Decimal(f64::MAX),
            Decimal(f64::NAN),
            Currency(-1),
            DateTime(-1.5),
            Value::text("x"),
        ];
        for a in &chars {
            for b in &chars {
                for c in &chars {
                    let code: String = [a, b, c].into_iter().collect();
                    for v in &values {
                        let _ = format(v, &code, &FR_FR);
                    }
                }
            }
        }
    }

    #[test]
    fn named_date_formats() {
        let d = DateTime(OCT14);
        assert_eq!(f(d.clone(), "General Date"), "14/10/2026 15:04:05");
        assert_eq!(f(DateTime(46_309.0), "General Date"), "14/10/2026");
        assert_eq!(f(DateTime(0.25), "General Date"), "06:00:00");
        assert_eq!(f(d.clone(), ""), "14/10/2026 15:04:05");
        assert_eq!(f(d.clone(), "Long Date"), "14 October 2026");
        assert_eq!(f(d.clone(), "Medium Date"), "14-Oct-26");
        assert_eq!(f(d.clone(), "Short Date"), "14/10/2026");
        assert_eq!(f(d.clone(), "Long Time"), "15:04:05");
        assert_eq!(f(d.clone(), "Medium Time"), "03:04 PM");
        assert_eq!(f(d.clone(), "Short Time"), "15:04");
        assert_eq!(fl(d.clone(), "Short Date", "en-US"), "10/14/2026");
        assert_eq!(
            fl(d.clone(), "Long Date", "en-US"),
            "Wednesday, October 14, 2026"
        );
        assert_eq!(fl(d.clone(), "Long Time", "en-US"), "3:04:05 PM");
        assert_eq!(
            fl(d.clone(), "General Date", "en-US"),
            "10/14/2026 3:04:05 PM"
        );
        assert_eq!(fl(d.clone(), "Short Date", "de-DE"), "14.10.2026");
        assert_eq!(
            fl(d.clone(), "Long Date", "de-DE"),
            "Mittwoch, 14. Oktober 2026"
        );
        assert_eq!(
            fl(d.clone(), "Long Date", "fr-FR"),
            "mercredi 14 octobre 2026"
        );
        assert_eq!(fl(d, "Short Date", "fr-FR"), "14/10/2026");
        // A number is a date serial under a date format.
        assert_eq!(f(Integer(1), "Short Date"), "31/12/1899");
        assert_eq!(f(Decimal(-1.25), "General Date"), "29/12/1899 06:00:00");
    }

    #[test]
    fn custom_date_codes() {
        let d = DateTime(OCT14);
        assert_eq!(
            f(d.clone(), "dddd d mmmm yyyy"),
            "Wednesday 14 October 2026"
        );
        assert_eq!(f(d.clone(), "ddd dd mmm yy"), "Wed 14 Oct 26");
        assert_eq!(f(d.clone(), "d/m/yyyy"), "14/10/2026");
        assert_eq!(fl(d.clone(), "dd/mm/yyyy", "de-DE"), "14.10.2026");
        assert_eq!(f(d.clone(), "yyyy-mm-dd"), "2026-10-14");
        assert_eq!(f(d.clone(), "hh:mm:ss"), "15:04:05");
        assert_eq!(f(d.clone(), "h:nn AM/PM"), "3:04 PM");
        assert_eq!(f(d.clone(), "h:nn am/pm"), "3:04 pm");
        assert_eq!(f(d.clone(), "h a/p"), "3 p");
        assert_eq!(f(d.clone(), "mm:ss"), "04:05");
        assert_eq!(f(d.clone(), "MMMM"), "October");
        assert_eq!(f(d.clone(), "\"Q\"q yyyy"), "Q4 2026");
        assert_eq!(f(d.clone(), "y w ww"), "287 4 42");
        assert_eq!(f(d.clone(), "ddddd"), "14/10/2026");
        assert_eq!(f(d.clone(), "dddddd"), "14 October 2026");
        assert_eq!(f(d.clone(), "c"), "14/10/2026 15:04:05");
        assert_eq!(f(d.clone(), "ttttt"), "15:04:05");
        assert_eq!(fl(d.clone(), "mmm", "fr-FR"), "oct.");
        assert_eq!(fl(d, "ddd", "de-DE"), "Mi");
        assert_eq!(f(DateTime(46_309.0), "h AM/PM"), "12 AM");
        assert_eq!(f(DateTime(46_309.5), "h AM/PM"), "12 PM");
        // A date under a numeric code is its serial.
        assert_eq!(f(DateTime(46_309.5), "0.0"), "46309.5");
    }

    #[test]
    fn defaults_by_type() {
        assert_eq!(default_format(FormatKind::Whole, &EN_GB), "0");
        assert_eq!(default_format(FormatKind::Decimal, &EN_GB), "");
        assert_eq!(
            f(
                Currency(12_345_000),
                default_format(FormatKind::Currency, &EN_GB)
            ),
            "£1,234.50"
        );
        assert_eq!(
            default_format(DataType::DateTime.into(), &EN_GB),
            "General Date"
        );
        assert_eq!(default_format(FormatKind::Date, &EN_GB), "Short Date");
        assert_eq!(Locale::get("de_de"), Some(&DE_DE));
        assert_eq!(Locale::get("xx-XX"), None);
    }
}
