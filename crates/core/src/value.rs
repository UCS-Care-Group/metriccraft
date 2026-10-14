//! DAX values (ADR 0006): the scalar a DAX expression evaluates to, with DAX's type promotion,
//! checked arithmetic and BLANK rules.
//!
//! - **Promotion.** Integer → Currency → Decimal: an operation on two values takes the wider
//!   type. Integer and Currency arithmetic is checked (overflow is a [`ValueError`]); Decimal is
//!   IEEE `f64`. `/` and `^` always give a Decimal, so `1 / 0` is `+∞` and `0 / 0` NaN — neither
//!   is BLANK (parity cases 0001, 0002).
//! - **BLANK** (Microsoft's documented table): `BLANK + 1 = 1`, `BLANK - 1 = -1`,
//!   `BLANK * 2 = BLANK`, `BLANK / 2 = BLANK`, `2 / BLANK = ∞`, `BLANK & "x" = "x"`. In `=` and the
//!   ordering comparisons BLANK is the other side's zero (`0`, `""`, `FALSE`); `==` is strict.
//! - **Text** compares ignoring case ([`fold`]: invariant lowercase, no accent folding).
//! - **Conversions.** Text in arithmetic is read as a number; TRUE/FALSE are 1/0. Comparing text
//!   with a number (or TRUE/FALSE with either) is an error, as in DAX.
//!
//! Parity uncertainties (to become cases): TRUE/FALSE as text (`True`/`False`), dates as text
//! (`dd/MM/yyyy HH:mm:ss`), numbers as text beyond the observed `inf`/`-inf`/`-nan(ind)` (we use
//! shortest round-trip, exponent form `1E+21` at ≥ 1e15 or < 1e-5), `BLANK && BLANK` (BLANK, per
//! the docs), Currency × Currency rounding (half away from zero), `BLANK ^ 2`, `"" + 1`
//! (an error here), and whether `TRUE = 1` compares.

use std::cmp::Ordering;
use std::fmt;
use std::sync::Arc;

/// Currency is stored as an integer count of 1/10⁴ units.
pub const CURRENCY_SCALE: i64 = 10_000;

/// Days from DateTime's epoch (1899-12-30) to 1970-01-01.
const UNIX_EPOCH_DAYS: i64 = 25_569;

/// A DAX scalar value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Blank,
    Boolean(bool),
    Integer(i64),
    Decimal(f64),
    /// Fixed decimal: the value × 10⁴.
    Currency(i64),
    Text(Arc<str>),
    /// Days since 1899-12-30 00:00; the fraction is the time of day.
    DateTime(f64),
}

/// A value's type, as error messages name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataType {
    Blank,
    Boolean,
    Integer,
    Decimal,
    Currency,
    Text,
    DateTime,
}

impl DataType {
    pub fn name(self) -> &'static str {
        match self {
            Self::Blank => "Blank",
            Self::Boolean => "True/False",
            Self::Integer => "Whole number",
            Self::Decimal => "Decimal number",
            Self::Currency => "Fixed decimal number",
            Self::Text => "Text",
            Self::DateTime => "Date/time",
        }
    }
}

impl fmt::Display for DataType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Why an operation on values failed. Our own wording; the evaluator adds where.
#[derive(Clone, Debug, PartialEq)]
pub enum ValueError {
    /// Whole-number or fixed-decimal arithmetic out of range.
    Overflow(DataType),
    /// A value that can't be read as `to`.
    Convert {
        value: String,
        from: DataType,
        to: DataType,
    },
    /// Two values of types DAX doesn't compare.
    Compare { left: DataType, right: DataType },
}

impl fmt::Display for ValueError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Overflow(t) => write!(f, "the result is too large for a {t} value"),
            Self::Convert { value, from, to } => {
                write!(f, "the {from} value '{value}' can't be read as {to}")
            }
            Self::Compare { left, right } => write!(
                f,
                "a {left} value can't be compared with a {right} value; convert one of them \
                 first (VALUE, FORMAT)"
            ),
        }
    }
}

impl std::error::Error for ValueError {}

type Result<T> = std::result::Result<T, ValueError>;

/// Text as DAX compares it: case folded (invariant lowercase).
pub fn fold(s: &str) -> String {
    s.to_lowercase()
}

/// A number in arithmetic, after promotion.
#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Cur(i64),
    Dec(f64),
}

impl Num {
    fn to_f64(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Cur(c) => c as f64 / CURRENCY_SCALE as f64,
            Num::Dec(d) => d,
        }
    }

    fn value(self) -> Value {
        match self {
            Num::Int(i) => Value::Integer(i),
            Num::Cur(c) => Value::Currency(c),
            Num::Dec(d) => Value::Decimal(d),
        }
    }
}

/// Integer → Currency, checked.
fn to_currency(i: i64) -> Result<i64> {
    i.checked_mul(CURRENCY_SCALE)
        .ok_or(ValueError::Overflow(DataType::Currency))
}

#[derive(Clone, Copy)]
enum Arith {
    Add,
    Sub,
    Mul,
}

/// `x op y` after promotion.
fn arith(op: Arith, x: Num, y: Num) -> Result<Num> {
    let int = |r: Option<i64>| r.ok_or(ValueError::Overflow(DataType::Integer));
    let cur = |r: Option<i64>| r.ok_or(ValueError::Overflow(DataType::Currency));
    Ok(match (x, y) {
        (Num::Int(a), Num::Int(b)) => Num::Int(int(match op {
            Arith::Add => a.checked_add(b),
            Arith::Sub => a.checked_sub(b),
            Arith::Mul => a.checked_mul(b),
        })?),
        (Num::Dec(_), _) | (_, Num::Dec(_)) => {
            let (a, b) = (x.to_f64(), y.to_f64());
            Num::Dec(match op {
                Arith::Add => a + b,
                Arith::Sub => a - b,
                Arith::Mul => a * b,
            })
        }
        // Currency with Integer or Currency.
        (a, b) => match op {
            Arith::Mul => {
                let (a, b, scaled) = match (a, b) {
                    (Num::Cur(a), Num::Cur(b)) => (a, b, true),
                    (Num::Cur(c), Num::Int(i)) | (Num::Int(i), Num::Cur(c)) => (c, i, false),
                    _ => return Err(ValueError::Overflow(DataType::Currency)),
                };
                if scaled {
                    let p = i128::from(a) * i128::from(b);
                    let s = i128::from(CURRENCY_SCALE);
                    let half = s / 2;
                    let q = if p >= 0 {
                        (p + half) / s
                    } else {
                        (p - half) / s
                    };
                    Num::Cur(cur(i64::try_from(q).ok())?)
                } else {
                    Num::Cur(cur(a.checked_mul(b))?)
                }
            }
            _ => {
                let c = |n: Num| match n {
                    Num::Int(i) => to_currency(i),
                    Num::Cur(c) => Ok(c),
                    Num::Dec(_) => Err(ValueError::Overflow(DataType::Currency)),
                };
                let (a, b) = (c(a)?, c(b)?);
                Num::Cur(cur(match op {
                    Arith::Sub => a.checked_sub(b),
                    _ => a.checked_add(b),
                })?)
            }
        },
    })
}

/// Text read as a number in arithmetic: plain decimal notation only (no `inf`, `NaN`).
fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let ok = !t.is_empty()
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if ok { t.parse().ok() } else { None }
}

impl Value {
    pub fn text(s: &str) -> Self {
        Value::Text(Arc::from(s))
    }

    pub fn is_blank(&self) -> bool {
        matches!(self, Value::Blank)
    }

    pub fn data_type(&self) -> DataType {
        match self {
            Value::Blank => DataType::Blank,
            Value::Boolean(_) => DataType::Boolean,
            Value::Integer(_) => DataType::Integer,
            Value::Decimal(_) => DataType::Decimal,
            Value::Currency(_) => DataType::Currency,
            Value::Text(_) => DataType::Text,
            Value::DateTime(_) => DataType::DateTime,
        }
    }

    /// The value as a number for arithmetic (`None` for BLANK) and whether it is a DateTime.
    fn number(&self) -> Result<(Option<Num>, bool)> {
        Ok(match self {
            Value::Blank => (None, false),
            Value::Boolean(b) => (Some(Num::Int(i64::from(*b))), false),
            Value::Integer(i) => (Some(Num::Int(*i)), false),
            Value::Currency(c) => (Some(Num::Cur(*c)), false),
            Value::Decimal(d) => (Some(Num::Dec(*d)), false),
            Value::DateTime(d) => (Some(Num::Dec(*d)), true),
            Value::Text(s) => match parse_number(s) {
                Some(d) => (Some(Num::Dec(d)), false),
                None => {
                    return Err(ValueError::Convert {
                        value: s.to_string(),
                        from: DataType::Text,
                        to: DataType::Decimal,
                    });
                }
            },
        })
    }

    /// The value as a number, BLANK as 0 (`/`, `^`).
    fn number_or_zero(&self) -> Result<f64> {
        Ok(self.number()?.0.map_or(0.0, Num::to_f64))
    }

    /// `self + other` (`subtract`: `self - other`). A DateTime with a number stays a DateTime.
    fn add_sub(&self, other: &Value, subtract: bool) -> Result<Value> {
        let ((x, xd), (y, yd)) = (self.number()?, other.number()?);
        let op = if subtract { Arith::Sub } else { Arith::Add };
        let result = match (x, y) {
            (None, None) => return Ok(Value::Blank),
            (Some(x), None) => x,
            (None, Some(y)) => arith(op, Num::Int(0), y)?,
            (Some(x), Some(y)) => arith(op, x, y)?,
        };
        Ok(if xd || yd {
            Value::DateTime(result.to_f64())
        } else {
            result.value()
        })
    }

    pub fn add(&self, other: &Value) -> Result<Value> {
        self.add_sub(other, false)
    }

    pub fn subtract(&self, other: &Value) -> Result<Value> {
        self.add_sub(other, true)
    }

    /// `self * other`: BLANK if either is BLANK.
    pub fn multiply(&self, other: &Value) -> Result<Value> {
        let ((x, _), (y, _)) = (self.number()?, other.number()?);
        match (x, y) {
            (Some(x), Some(y)) => Ok(arith(Arith::Mul, x, y)?.value()),
            _ => Ok(Value::Blank),
        }
    }

    /// `self / other`, always Decimal: BLANK when `self` is BLANK, else `other` BLANK is 0.
    pub fn divide(&self, other: &Value) -> Result<Value> {
        let (x, _) = self.number()?;
        let y = other.number_or_zero()?;
        Ok(match x {
            None => Value::Blank,
            Some(x) => Value::Decimal(x.to_f64() / y),
        })
    }

    /// `self ^ other`, always Decimal; BLANK is 0.
    pub fn power(&self, other: &Value) -> Result<Value> {
        Ok(Value::Decimal(
            self.number_or_zero()?.powf(other.number_or_zero()?),
        ))
    }

    /// `-self`: BLANK stays BLANK.
    pub fn negate(&self) -> Result<Value> {
        let (x, date) = self.number()?;
        Ok(match x {
            None => Value::Blank,
            Some(Num::Int(i)) => Value::Integer(
                i.checked_neg()
                    .ok_or(ValueError::Overflow(DataType::Integer))?,
            ),
            Some(Num::Cur(c)) => Value::Currency(
                c.checked_neg()
                    .ok_or(ValueError::Overflow(DataType::Currency))?,
            ),
            Some(Num::Dec(d)) if date => Value::DateTime(-d),
            Some(Num::Dec(d)) => Value::Decimal(-d),
        })
    }

    /// `self & other`: both as text, BLANK as `""`.
    pub fn concat(&self, other: &Value) -> Value {
        let mut s = self.to_text();
        s.push_str(&other.to_text());
        Value::Text(Arc::from(s))
    }

    /// The value as TRUE/FALSE (`NOT`, `&&`, `||`, conditions): BLANK is FALSE, a number is
    /// TRUE unless 0, text must read `true`/`false`.
    pub fn to_bool(&self) -> Result<bool> {
        Ok(match self {
            Value::Blank => false,
            Value::Boolean(b) => *b,
            Value::Integer(i) => *i != 0,
            Value::Currency(c) => *c != 0,
            Value::Decimal(d) | Value::DateTime(d) => *d != 0.0,
            Value::Text(s) => match fold(s.trim()).as_str() {
                "true" => true,
                "false" => false,
                _ => {
                    return Err(ValueError::Convert {
                        value: s.to_string(),
                        from: DataType::Text,
                        to: DataType::Boolean,
                    });
                }
            },
        })
    }

    /// `NOT self`.
    pub fn not(&self) -> Result<Value> {
        Ok(Value::Boolean(!self.to_bool()?))
    }

    /// `self && other`: BLANK when both are BLANK (documented), else TRUE/FALSE.
    pub fn and(&self, other: &Value) -> Result<Value> {
        if self.is_blank() && other.is_blank() {
            return Ok(Value::Blank);
        }
        Ok(Value::Boolean(self.to_bool()? && other.to_bool()?))
    }

    /// `self || other`: BLANK when both are BLANK (documented), else TRUE/FALSE.
    pub fn or(&self, other: &Value) -> Result<Value> {
        if self.is_blank() && other.is_blank() {
            return Ok(Value::Blank);
        }
        let (a, b) = (self.to_bool()?, other.to_bool()?);
        Ok(Value::Boolean(a || b))
    }

    /// The zero of this value's type: what BLANK compares as against it.
    fn zero_like(&self) -> Value {
        match self {
            Value::Blank => Value::Blank,
            Value::Boolean(_) => Value::Boolean(false),
            Value::Integer(_) => Value::Integer(0),
            Value::Decimal(_) => Value::Decimal(0.0),
            Value::Currency(_) => Value::Currency(0),
            Value::Text(_) => Value::text(""),
            Value::DateTime(_) => Value::DateTime(0.0),
        }
    }

    /// How `=`, `<`, `>`… order `self` and `other`; `None` when unordered (NaN). BLANK is the
    /// other side's zero. Text against a number, or TRUE/FALSE against either, is an error.
    pub fn compare(&self, other: &Value) -> Result<Option<Ordering>> {
        use Value::*;
        match (self, other) {
            (Blank, Blank) => Ok(Some(Ordering::Equal)),
            (Blank, x) => x.zero_like().compare(x),
            (x, Blank) => x.compare(&x.zero_like()),
            (Text(a), Text(b)) => Ok(Some(fold(a).cmp(&fold(b)))),
            (Boolean(a), Boolean(b)) => Ok(Some(a.cmp(b))),
            (Integer(a), Integer(b)) => Ok(Some(a.cmp(b))),
            (Currency(a), Currency(b)) => Ok(Some(a.cmp(b))),
            (Integer(i), Currency(c)) => Ok(Some(
                (i128::from(*i) * i128::from(CURRENCY_SCALE)).cmp(&i128::from(*c)),
            )),
            (Currency(c), Integer(i)) => Ok(Some(
                i128::from(*c).cmp(&(i128::from(*i) * i128::from(CURRENCY_SCALE))),
            )),
            (a, b) => match (a.number_only(), b.number_only()) {
                (Some(x), Some(y)) => Ok(x.partial_cmp(&y)),
                _ => Err(ValueError::Compare {
                    left: a.data_type(),
                    right: b.data_type(),
                }),
            },
        }
    }

    /// A number (Integer, Currency, Decimal, DateTime) as `f64`; `None` for anything else.
    fn number_only(&self) -> Option<f64> {
        match self {
            Value::Integer(i) => Some(*i as f64),
            Value::Currency(c) => Some(*c as f64 / CURRENCY_SCALE as f64),
            Value::Decimal(d) | Value::DateTime(d) => Some(*d),
            _ => None,
        }
    }

    /// `self = other` (BLANK equals 0, `""` and FALSE).
    pub fn equals(&self, other: &Value) -> Result<bool> {
        Ok(self.compare(other)? == Some(Ordering::Equal))
    }

    /// `self == other`: like `=`, but BLANK equals only BLANK.
    pub fn strict_equals(&self, other: &Value) -> Result<bool> {
        if self.is_blank() != other.is_blank() {
            return Ok(false);
        }
        self.equals(other)
    }

    /// A total order for sorting (`ORDER BY`): BLANK first, then numbers (NaN last), text
    /// (ignoring case), TRUE/FALSE.
    // ponytail: where DAX sorts mixed types relative to each other is a parity guess.
    pub fn order(&self, other: &Value) -> Ordering {
        let rank = |v: &Value| match v {
            Value::Blank => 0,
            Value::Integer(_) | Value::Decimal(_) | Value::Currency(_) | Value::DateTime(_) => 1,
            Value::Text(_) => 2,
            Value::Boolean(_) => 3,
        };
        match (self, other) {
            (Value::Text(a), Value::Text(b)) => fold(a).cmp(&fold(b)),
            (Value::Boolean(a), Value::Boolean(b)) => a.cmp(b),
            (a, b) if rank(a) == 1 && rank(b) == 1 => match (a.sort_key(), b.sort_key()) {
                (Some((ca, ia, fa)), Some((cb, ib, fb))) => {
                    ca.cmp(&cb).then(ia.cmp(&ib)).then(fa.total_cmp(&fb))
                }
                _ => Ordering::Equal,
            },
            (a, b) => rank(a).cmp(&rank(b)),
        }
    }

    /// A number as an exact sort key `(class, whole part, fraction)`: class 0 is -∞, 1 finite,
    /// 2 +∞, 3 NaN; a finite number is its floor (exact, in `i128`) and its fraction in
    /// `[0, 1)`. Integers compare exactly with Decimals beyond 2⁵³ (no rounding to `f64`), so
    /// [`Value::order`] is a total order over mixed numbers.
    fn sort_key(&self) -> Option<(u8, i128, f64)> {
        let float = |d: f64| {
            if d.is_nan() {
                (3, 0, 0.0)
            } else if d == f64::INFINITY {
                (2, 0, 0.0)
            } else if d == f64::NEG_INFINITY {
                (0, 0, 0.0)
            } else {
                let floor = d.floor();
                // Every finite f64 is below 2^1024, but its floor fits i128 only below 2^127:
                // beyond that `as` saturates, which still orders correctly against any i64.
                (1, floor as i128, (d - floor).max(0.0))
            }
        };
        Some(match self {
            Value::Integer(i) => (1, i128::from(*i), 0.0),
            Value::Currency(c) => {
                let (whole, frac) = (c.div_euclid(CURRENCY_SCALE), c.rem_euclid(CURRENCY_SCALE));
                (1, i128::from(whole), frac as f64 / CURRENCY_SCALE as f64)
            }
            Value::Decimal(d) | Value::DateTime(d) => float(*d),
            _ => return None,
        })
    }

    /// The value as DAX converts it to text (`&`, text functions).
    pub fn to_text(&self) -> String {
        match self {
            Value::Blank => String::new(),
            Value::Boolean(true) => "True".into(),
            Value::Boolean(false) => "False".into(),
            Value::Integer(i) => i.to_string(),
            Value::Decimal(d) => number_text(*d),
            Value::Currency(c) => currency_text(*c),
            Value::Text(s) => s.to_string(),
            Value::DateTime(d) => date_text(*d),
        }
    }

    /// The value as Power BI copies it out of a query result (ADR 0004): BLANK empty,
    /// `true`/`false`, numbers shortest round-trip, `Infinity`.
    pub fn copy_text(&self) -> String {
        match self {
            Value::Blank => String::new(),
            Value::Boolean(b) => b.to_string(),
            Value::Decimal(d) if d.is_nan() => "NaN".into(),
            Value::Decimal(d) if d.is_infinite() && *d > 0.0 => "Infinity".into(),
            Value::Decimal(d) if d.is_infinite() => "-Infinity".into(),
            Value::Decimal(d) => shortest(*d),
            other => other.to_text(),
        }
    }
}

/// A Decimal as DAX converts it to text: the Microsoft C runtime's spellings for the
/// non-finite values (parity case 0002: `inf`, `-inf`, `-nan(ind)`), else shortest round-trip.
pub fn number_text(d: f64) -> String {
    if d.is_nan() {
        // 0/0 on x86 is the negative "indefinite" NaN; Rust may fold it to a positive one.
        "-nan(ind)".into()
    } else if d.is_infinite() {
        if d > 0.0 { "inf" } else { "-inf" }.into()
    } else {
        shortest(d)
    }
}

/// The shortest text that reads back as `d`; exponent form (`1.5E+20`) outside 1e-5..1e15.
fn shortest(d: f64) -> String {
    let a = d.abs();
    if d == 0.0 {
        return "0".into();
    }
    if (1e-5..1e15).contains(&a) {
        return d.to_string();
    }
    let e = format!("{d:E}");
    match e.split_once('E') {
        Some((mantissa, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            format!("{mantissa}E{sign}{digits:0>2}")
        }
        None => e,
    }
}

fn currency_text(c: i64) -> String {
    let sign = if c < 0 { "-" } else { "" };
    let a = c.unsigned_abs();
    let scale = CURRENCY_SCALE.unsigned_abs();
    let (int, frac) = (a / scale, a % scale);
    if frac == 0 {
        format!("{sign}{int}")
    } else {
        let f = format!("{frac:04}");
        format!("{sign}{int}.{}", f.trim_end_matches('0'))
    }
}

/// A DateTime as `dd/MM/yyyy HH:mm:ss` (a guess; see the module doc).
fn date_text(d: f64) -> String {
    // Beyond ±10⁷ days (≈ 27,000 years) it isn't a date; show the number.
    if !d.is_finite() || d.abs() > 1e7 {
        return number_text(d);
    }
    let secs = (d * 86_400.0).round() as i64;
    let days = secs.div_euclid(86_400) - UNIX_EPOCH_DAYS;
    let tod = secs.rem_euclid(86_400);
    let (y, m, dd) = civil(days);
    format!(
        "{dd:02}/{m:02}/{y:04} {:02}:{:02}:{:02}",
        tod / 3600,
        tod / 60 % 60,
        tod % 60
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use Value::*;

    fn t(s: &str) -> Value {
        Value::text(s)
    }

    #[test]
    fn promotion_takes_the_wider_type() {
        assert_eq!(Integer(2).add(&Integer(3)), Ok(Integer(5)));
        assert_eq!(Integer(2).add(&Currency(15_000)), Ok(Currency(35_000)));
        assert_eq!(Currency(15_000).subtract(&Integer(1)), Ok(Currency(5_000)));
        assert_eq!(Integer(2).add(&Decimal(0.5)), Ok(Decimal(2.5)));
        assert_eq!(Currency(15_000).add(&Decimal(0.5)), Ok(Decimal(2.0)));
        assert_eq!(Currency(15_000).multiply(&Integer(3)), Ok(Currency(45_000)));
        // 1.5 × 1.5 = 2.25, exact in fixed decimal.
        assert_eq!(
            Currency(15_000).multiply(&Currency(15_000)),
            Ok(Currency(22_500))
        );
        // 0.0001 × 0.5 = 0.00005: rounds half away from zero to 0.0001.
        assert_eq!(Currency(1).multiply(&Currency(5_000)), Ok(Currency(1)));
        assert_eq!(Currency(-1).multiply(&Currency(5_000)), Ok(Currency(-1)));
        assert_eq!(Boolean(true).add(&Integer(1)), Ok(Integer(2)));
        assert_eq!(t("1.5").add(&Integer(1)), Ok(Decimal(2.5)));
        // `/` and `^` are always Decimal.
        assert_eq!(Integer(6).divide(&Integer(3)), Ok(Decimal(2.0)));
        assert_eq!(Integer(2).power(&Integer(3)), Ok(Decimal(8.0)));
        // A DateTime with a number stays a DateTime; times a number it's a number.
        assert_eq!(DateTime(10.5).add(&Integer(1)), Ok(DateTime(11.5)));
        assert_eq!(Integer(1).add(&DateTime(10.0)), Ok(DateTime(11.0)));
        assert_eq!(DateTime(10.0).multiply(&Integer(2)), Ok(Decimal(20.0)));
    }

    #[test]
    fn overflow_is_an_error() {
        let max = Integer(i64::MAX);
        assert_eq!(
            max.add(&Integer(1)),
            Err(ValueError::Overflow(DataType::Integer))
        );
        assert!(Integer(i64::MIN).subtract(&Integer(1)).is_err());
        assert!(max.multiply(&Integer(2)).is_err());
        assert!(Integer(i64::MIN).negate().is_err());
        // Promoting a huge Integer to Currency overflows too.
        assert_eq!(
            max.add(&Currency(1)),
            Err(ValueError::Overflow(DataType::Currency))
        );
        assert!(Currency(i64::MAX).multiply(&Currency(i64::MAX)).is_err());
        assert!(Currency(i64::MIN).negate().is_err());
        // Decimal arithmetic doesn't overflow: it goes to infinity.
        assert_eq!(
            Decimal(f64::MAX).multiply(&Integer(2)),
            Ok(Decimal(f64::INFINITY))
        );
        let e = max.add(&Integer(1)).unwrap_err().to_string();
        assert!(e.contains("too large"), "{e}");
    }

    #[test]
    fn blank_in_operators() {
        let ok = |r: Result<Value>| r.unwrap();
        // Documented: BLANK + BLANK, BLANK + 5, BLANK * 5, 5 / BLANK, 0 / BLANK, BLANK / BLANK.
        assert_eq!(ok(Blank.add(&Blank)), Blank);
        assert_eq!(ok(Blank.add(&Integer(1))), Integer(1));
        assert_eq!(ok(Integer(1).add(&Blank)), Integer(1));
        assert_eq!(ok(Blank.subtract(&Integer(1))), Integer(-1));
        assert_eq!(ok(Blank.subtract(&Currency(5))), Currency(-5));
        assert_eq!(ok(Blank.multiply(&Integer(2))), Blank);
        assert_eq!(ok(Integer(2).multiply(&Blank)), Blank);
        assert_eq!(ok(Integer(5).divide(&Blank)), Decimal(f64::INFINITY));
        assert!(matches!(ok(Integer(0).divide(&Blank)), Decimal(d) if d.is_nan()));
        assert_eq!(ok(Blank.divide(&Blank)), Blank);
        assert_eq!(ok(Blank.divide(&Integer(2))), Blank);
        assert_eq!(ok(Blank.negate()), Blank);
        assert_eq!(Blank.concat(&t("x")), t("x"));
        assert_eq!(Blank.concat(&Blank), t(""));
        // FALSE OR BLANK = FALSE, TRUE OR BLANK = TRUE, TRUE AND BLANK = FALSE, BLANK OR BLANK = BLANK.
        assert_eq!(ok(Boolean(false).or(&Blank)), Boolean(false));
        assert_eq!(ok(Boolean(true).or(&Blank)), Boolean(true));
        assert_eq!(ok(Boolean(true).and(&Blank)), Boolean(false));
        assert_eq!(ok(Boolean(false).and(&Blank)), Boolean(false));
        assert_eq!(ok(Blank.or(&Blank)), Blank);
        assert_eq!(ok(Blank.and(&Blank)), Blank);
        assert_eq!(ok(Blank.not()), Boolean(true));
        // 1 / 0 is infinity, 0 / 0 NaN: not BLANK.
        assert_eq!(ok(Integer(1).divide(&Integer(0))), Decimal(f64::INFINITY));
        assert_eq!(
            ok(Integer(-1).divide(&Integer(0))),
            Decimal(f64::NEG_INFINITY)
        );
    }

    #[test]
    fn blank_in_comparisons() {
        let eq = |a: &Value, b: &Value| a.equals(b).unwrap();
        let strict = |a: &Value, b: &Value| a.strict_equals(b).unwrap();
        let lt = |a: &Value, b: &Value| a.compare(b).unwrap() == Some(Ordering::Less);
        for zero in [Integer(0), Decimal(0.0), Currency(0), t(""), Boolean(false)] {
            assert!(eq(&Blank, &zero), "BLANK = {zero:?}");
            assert!(eq(&zero, &Blank), "{zero:?} = BLANK");
            assert!(!strict(&Blank, &zero), "BLANK == {zero:?}");
        }
        assert!(eq(&Blank, &Blank) && strict(&Blank, &Blank));
        assert!(!eq(&Blank, &Integer(1)));
        assert!(lt(&Blank, &Integer(1)));
        assert!(lt(&Integer(-1), &Blank));
        assert!(lt(&Blank, &t("a")));
        assert!(lt(&Blank, &Boolean(true)));
        assert!(strict(&Integer(2), &Decimal(2.0)));
    }

    #[test]
    fn comparisons_across_types() {
        assert!(Integer(2).equals(&Currency(20_000)).unwrap());
        assert!(Integer(2).equals(&Decimal(2.0)).unwrap());
        assert!(DateTime(1.0).equals(&Integer(1)).unwrap());
        assert_eq!(
            Decimal(f64::INFINITY).compare(&Integer(1_000_000)),
            Ok(Some(Ordering::Greater))
        );
        // NaN is unordered: `=` and `<` are false.
        assert_eq!(Decimal(f64::NAN).compare(&Integer(1)), Ok(None));
        for (a, b) in [
            (Integer(1), t("1")),
            (t("a"), Decimal(1.0)),
            (Boolean(true), Integer(1)),
            (t("true"), Boolean(true)),
        ] {
            assert!(
                matches!(a.compare(&b), Err(ValueError::Compare { .. })),
                "{a:?} vs {b:?}"
            );
        }
    }

    #[test]
    fn text_compares_ignoring_case() {
        assert!(t("North").equals(&t("NORTH")).unwrap());
        assert!(t("Ärger").equals(&t("äRGER")).unwrap());
        assert!(!t("e").equals(&t("é")).unwrap(), "no accent folding");
        assert_eq!(t("apple").compare(&t("Banana")), Ok(Some(Ordering::Less)));
        assert!(t("a").strict_equals(&t("A")).unwrap());
    }

    #[test]
    fn text_conversions() {
        assert_eq!(t("").concat(&Decimal(f64::INFINITY)), t("inf"));
        assert_eq!(t("").concat(&Decimal(f64::NEG_INFINITY)), t("-inf"));
        assert_eq!(t("").concat(&Decimal(f64::NAN)), t("-nan(ind)"));
        assert_eq!(t("").concat(&Decimal(0.1 + 0.2)), t("0.30000000000000004"));
        assert_eq!(Integer(-3).concat(&Currency(15_000)), t("-31.5"));
        assert_eq!(Currency(-5).to_text(), "-0.0005");
        assert_eq!(Decimal(1e21).to_text(), "1E+21");
        assert_eq!(Decimal(1.5e-7).to_text(), "1.5E-07");
        assert_eq!(Decimal(-0.0).to_text(), "0");
        assert_eq!(Boolean(true).to_text(), "True");
        // 2026-10-12 is 46307 days after 1899-12-30.
        assert_eq!(DateTime(46_307.5).to_text(), "12/10/2026 12:00:00");
        assert!(matches!(
            t("abc").add(&Integer(1)),
            Err(ValueError::Convert { .. })
        ));
        assert!(t("inf").add(&Integer(1)).is_err(), "not a DAX number");
        assert!(t("maybe").to_bool().is_err());
        assert_eq!(t("TRUE").to_bool(), Ok(true));
    }

    #[test]
    fn copy_text_is_what_power_bi_copies() {
        let copy = |v: Value| v.copy_text();
        assert_eq!(copy(Blank), "");
        assert_eq!(copy(Boolean(true)), "true");
        assert_eq!(copy(Decimal(f64::INFINITY)), "Infinity");
        assert_eq!(copy(Decimal(f64::NEG_INFINITY)), "-Infinity");
        assert_eq!(copy(Decimal(14.5)), "14.5");
        assert_eq!(copy(Decimal(4.0)), "4");
        assert_eq!(copy(Decimal(29.0 / 12.0)), "2.4166666666666665");
        assert_eq!(copy(Currency(72_500)), "7.25");
        assert_eq!(copy(Integer(4)), "4");
    }

    #[test]
    fn sort_order_is_total() {
        let mut v = [
            t("b"),
            Integer(2),
            Blank,
            Decimal(f64::NAN),
            Boolean(false),
            t("A"),
            Decimal(1.5),
            Currency(10_000),
        ];
        v.sort_by(Value::order);
        assert_eq!(
            v.iter().map(Value::copy_text).collect::<Vec<_>>(),
            ["", "1", "1.5", "2", "NaN", "A", "b", "false"]
        );
    }

    /// Integers beyond 2^53 order exactly against Decimals (review M3 #6): `2^53`, `2^53 + 1` and
    /// `2^53` as a Decimal used to be "equal" pairwise yet unequal as Integers, which isn't a
    /// total order (`sort_by` may panic on that).
    #[test]
    fn sort_order_is_exact_beyond_2_pow_53() {
        let big = 1i64 << 53;
        let (a, b, c) = (Integer(big), Integer(big + 1), Decimal(big as f64));
        assert_eq!(a.order(&c), Ordering::Equal);
        assert_eq!(b.order(&c), Ordering::Greater);
        assert_eq!(c.order(&b), Ordering::Less);
        assert_eq!(a.order(&b), Ordering::Less);
        assert_eq!(Integer(i64::MAX).order(&Decimal(9.3e18)), Ordering::Less);
        assert_eq!(
            Integer(i64::MIN).order(&Decimal(-9.3e18)),
            Ordering::Greater
        );
        assert_eq!(Currency(-15_000).order(&Decimal(-1.5)), Ordering::Equal);
        assert_eq!(Currency(-15_000).order(&Decimal(-1.4)), Ordering::Less);
        assert_eq!(
            Decimal(f64::INFINITY).order(&Integer(i64::MAX)),
            Ordering::Greater
        );
        assert_eq!(
            Decimal(f64::NEG_INFINITY).order(&Integer(i64::MIN)),
            Ordering::Less
        );
        // Many mixed values near 2^53: sorting is consistent (every adjacent pair ordered).
        let mut v: Vec<Value> = (0..200)
            .map(|i: i64| {
                let n = big - 50 + i / 2;
                if i % 2 == 0 {
                    Integer(n)
                } else {
                    Decimal(n as f64)
                }
            })
            .collect();
        v.reverse();
        v.sort_by(Value::order);
        assert!(v.windows(2).all(|w| w[0].order(&w[1]) != Ordering::Greater));
    }
}
