//! Chart axes (M4.4): "nice" value ticks with consistent display units, and nice date ticks for
//! a continuous line axis. Pure numbers in, numbers and labels out; no drawing.

use metriccraft_core::{Locale, Value, date};

use crate::layout::value_text;
use crate::query::Header;

/// Most ticks any axis gets, whatever the caller asks (hostile sizes).
const TICK_LIMIT: usize = 64;

/// Ticks covering `lo..=hi`: multiples of a step of 1, 2, 2.5 or 5 × 10ⁿ, at most `max_ticks`
/// of them (at least 2), the first ≤ `lo` and the last ≥ `hi`. Non-finite or reversed input,
/// or a span too large or too small for `f64`, gives `[lo, hi]` as they are (or `[0, 1]`).
pub fn nice_ticks(lo: f64, hi: f64, max_ticks: usize) -> Vec<f64> {
    if !lo.is_finite() || !hi.is_finite() || lo > hi {
        return vec![0.0, 1.0];
    }
    if lo == hi {
        return nice_ticks(lo - lo.abs().max(1.0), hi + hi.abs().max(1.0), max_ticks);
    }
    let max_ticks = max_ticks.clamp(2, TICK_LIMIT);
    let span = hi - lo;
    if !span.is_finite() || span <= 0.0 {
        return vec![lo, hi];
    }
    // The smallest step with at most `max_ticks` ticks.
    let raw = span / (max_ticks - 1) as f64;
    let exp = raw.log10().floor();
    for e in [exp - 1.0, exp, exp + 1.0] {
        let pow = 10f64.powf(e);
        for m in [1.0, 2.0, 2.5, 5.0] {
            let step = m * pow;
            if !step.is_finite() || step <= 0.0 {
                continue;
            }
            let (first, last) = ((lo / step).floor(), (hi / step).ceil());
            let count = last - first + 1.0;
            if count.is_finite() && count <= max_ticks as f64 {
                let ticks: Vec<f64> = (0..count as usize)
                    .map(|i| clean((first + i as f64) * step))
                    .collect();
                if ticks.iter().all(|t| t.is_finite()) {
                    return ticks;
                }
            }
        }
    }
    vec![lo, hi]
}

/// `v` with float noise removed (`0.30000000000000004` → `0.3`) and `-0` as `0`.
fn clean(v: f64) -> f64 {
    let r: f64 = format!("{v:.12e}").parse().unwrap_or(v);
    if r == 0.0 { 0.0 } else { r }
}

/// Tick labels in one display unit for every tick, chosen by the largest |tick| (Power BI's
/// "Auto" units: K, M, bn, T), with as many decimals as the step needs. Below 1,000 a percent
/// format string formats the ticks; otherwise plain numbers in the Locale's decimal separator.
/// Without `units`, every tick is a plain number (a numeric category axis: years stay `2024`).
pub fn tick_labels(
    ticks: &[f64],
    header: Option<&Header>,
    units: bool,
    locale: &Locale,
) -> Vec<String> {
    let max = ticks.iter().fold(0f64, |m, t| m.max(t.abs()));
    let (div, suffix) = match max {
        a if !units || !a.is_finite() || a < 1e3 => (1.0, ""),
        a if a < 1e6 => (1e3, "K"),
        a if a < 1e9 => (1e6, "M"),
        a if a < 1e12 => (1e9, "bn"),
        _ => (1e12, "T"),
    };
    if suffix.is_empty()
        && let Some(h) = header
        && h.format_string.contains('%')
    {
        return ticks
            .iter()
            .map(|t| value_text(&Value::Decimal(*t), h, locale))
            .collect();
    }
    let step = match ticks {
        [a, b, ..] => (b - a).abs() / div,
        _ => 1.0,
    };
    let decimals = if step > 0.0 && step.is_finite() {
        // 2.5 × 10ⁿ needs one decimal more than 10ⁿ.
        let d = -(step.log10().floor());
        let extra = if (step / 10f64.powf(step.log10().floor()) - 2.5).abs() < 1e-9 {
            1.0
        } else {
            0.0
        };
        (d + extra).clamp(0.0, 12.0) as usize
    } else {
        0
    };
    ticks
        .iter()
        .map(|t| {
            let v = t / div;
            let s = if v.abs() >= 1e15 {
                format!("{v:e}")
            } else {
                format!("{v:.decimals$}")
            };
            let s = if s == "-0" { "0".to_string() } else { s };
            format!("{}{suffix}", s.replace('.', &locale.decimal.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Dates: serials are days since 1899-12-30 (metriccraft_core::Value::DateTime).

/// The serials VBA can show (100-01-01..=9999-12-31); date ticks stay inside them.
const MIN_SERIAL: f64 = -657_434.0;
const MAX_SERIAL: f64 = 2_958_465.0;
/// (year, month 1–12, day) of whole-day serial `s` (clamped to the VBA range).
fn civil(s: f64) -> (i64, i64, i64) {
    let days = s.clamp(MIN_SERIAL, MAX_SERIAL).floor() as i64;
    date::civil(days - date::UNIX_EPOCH_DAYS)
}

/// The serial of (year, month 1–12, day 1–31), years clamped to 100..=9999.
fn serial(y: i64, m: i64, d: i64) -> f64 {
    (date::days_from_civil(y.clamp(100, 9_999), m, d) + date::UNIX_EPOCH_DAYS) as f64
}

/// Date steps, smallest first: (days, months, years, label code).
const DATE_STEPS: [(f64, i64, i64, &str); 16] = [
    (1.0 / 24.0, 0, 0, "hh:nn"),
    (2.0 / 24.0, 0, 0, "hh:nn"),
    (3.0 / 24.0, 0, 0, "hh:nn"),
    (6.0 / 24.0, 0, 0, "d mmm hh:nn"),
    (0.5, 0, 0, "d mmm hh:nn"),
    (1.0, 0, 0, "d mmm"),
    (2.0, 0, 0, "d mmm"),
    (7.0, 0, 0, "d mmm"),
    (14.0, 0, 0, "d mmm"),
    (0.0, 1, 0, "mmm yyyy"),
    (0.0, 3, 0, "mmm yyyy"),
    (0.0, 6, 0, "mmm yyyy"),
    (0.0, 0, 1, "yyyy"),
    (0.0, 0, 2, "yyyy"),
    (0.0, 0, 5, "yyyy"),
    (0.0, 0, 10, "yyyy"),
];

/// Ticks inside `lo..=hi` (serials) at nice dates — whole hours, days, weeks, month or year
/// starts — at most `max_ticks`, with their labels in `locale`.
pub fn date_ticks(lo: f64, hi: f64, max_ticks: usize, locale: &Locale) -> Vec<(f64, String)> {
    let (lo, hi) = (
        lo.clamp(MIN_SERIAL, MAX_SERIAL),
        hi.clamp(MIN_SERIAL, MAX_SERIAL),
    );
    if lo.is_nan() || hi.is_nan() || lo > hi {
        return Vec::new();
    }
    let max_ticks = max_ticks.clamp(1, TICK_LIMIT);
    let span = hi - lo;
    for (days, months, years, code) in DATE_STEPS {
        let approx = days + months as f64 * 30.44 + years as f64 * 365.25;
        if span / approx > max_ticks as f64 {
            continue;
        }
        let ticks = date_steps(lo, hi, days, months, years);
        if ticks.len() <= max_ticks {
            return label_dates(ticks, code, locale);
        }
    }
    // Centuries and more: nice year numbers.
    let (y0, y1) = (civil(lo).0, civil(hi).0);
    let years = nice_ticks(y0 as f64, y1 as f64, max_ticks);
    let ticks = years
        .into_iter()
        .map(|y| serial(y as i64, 1, 1))
        .filter(|s| *s >= lo && *s <= hi)
        .collect();
    label_dates(ticks, "yyyy", locale)
}

fn date_steps(lo: f64, hi: f64, days: f64, months: i64, years: i64) -> Vec<f64> {
    let mut out = Vec::new();
    if days > 0.0 {
        let mut t = (lo / days).ceil() * days;
        while t <= hi && out.len() <= TICK_LIMIT {
            out.push(t);
            t += days;
        }
    } else {
        let (y, m, _) = civil(lo);
        // Months since year 0, rounded down to the step.
        let step = if years > 0 { years * 12 } else { months.max(1) };
        let mut k = (y * 12 + m - 1).div_euclid(step) * step;
        while out.len() <= TICK_LIMIT {
            let s = serial(k.div_euclid(12), k.rem_euclid(12) + 1, 1);
            if s > hi {
                break;
            }
            if s >= lo {
                out.push(s);
            }
            k += step;
        }
    }
    out
}

fn label_dates(ticks: Vec<f64>, code: &str, locale: &Locale) -> Vec<(f64, String)> {
    ticks
        .into_iter()
        .map(|t| {
            let v = Value::DateTime(t);
            let label = metriccraft_core::format(&v, code, locale).unwrap_or_else(|_| v.to_text());
            (t, label)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nice_ticks_cover_the_range_with_nice_steps() {
        assert_eq!(
            nice_ticks(0.0, 100.0, 6),
            vec![0.0, 20.0, 40.0, 60.0, 80.0, 100.0]
        );
        assert_eq!(nice_ticks(0.0, 9.0, 5), vec![0.0, 2.5, 5.0, 7.5, 10.0]);
        assert_eq!(
            nice_ticks(-30.0, 45.0, 8),
            vec![-40.0, -20.0, 0.0, 20.0, 40.0, 60.0]
        );
        assert_eq!(nice_ticks(0.0, 0.3, 4), vec![0.0, 0.1, 0.2, 0.3]);
        assert_eq!(
            nice_ticks(0.0, 1234.0, 6),
            vec![0.0, 250.0, 500.0, 750.0, 1000.0, 1250.0]
        );
        assert_eq!(nice_ticks(-5.0, 0.0, 3), vec![-5.0, -2.5, 0.0]);
        // Every result: nice steps, covering, within the limit.
        for (lo, hi, n) in [
            (0.0, 1.0, 2),
            (-1e300, 1e300, 8),
            (0.0, 1e-300, 5),
            (3.0, 7.0, 100),
        ] {
            let t = nice_ticks(lo, hi, n);
            assert!(t.len() >= 2 && t.len() <= n.clamp(2, TICK_LIMIT), "{t:?}");
            assert!(t.first().is_some_and(|f| *f <= lo) && t.last().is_some_and(|l| *l >= hi));
            assert!(t.iter().all(|v| v.is_finite()), "{t:?}");
        }
        // Hostile: equal, reversed, non-finite, overflowing span.
        assert_eq!(nice_ticks(5.0, 5.0, 3), vec![0.0, 5.0, 10.0]);
        assert_eq!(nice_ticks(0.0, 0.0, 3), vec![-1.0, 0.0, 1.0]);
        assert_eq!(nice_ticks(2.0, 1.0, 5), vec![0.0, 1.0]);
        assert_eq!(nice_ticks(f64::NAN, 1.0, 5), vec![0.0, 1.0]);
        assert_eq!(
            nice_ticks(-f64::MAX, f64::MAX, 5),
            vec![-f64::MAX, f64::MAX]
        );
        assert!(nice_ticks(0.0, 1.0, 0).len() >= 2);
    }

    #[test]
    fn tick_labels_share_one_unit() {
        let en = &metriccraft_core::EN_GB;
        let l = |t: &[f64]| tick_labels(t, None, true, en);
        assert_eq!(l(&[0.0, 5000.0, 10000.0]), ["0K", "5K", "10K"]);
        assert_eq!(l(&[0.0, 2500.0, 5000.0]), ["0.0K", "2.5K", "5.0K"]);
        assert_eq!(l(&[-2e6, 0.0, 2e6]), ["-2M", "0M", "2M"]);
        assert_eq!(l(&[0.0, 0.1, 0.2]), ["0.0", "0.1", "0.2"]);
        assert_eq!(l(&[0.0, 20.0, 40.0]), ["0", "20", "40"]);
        assert_eq!(l(&[0.0, 1e300]), ["0T", "1e288T"]);
        assert_eq!(
            tick_labels(&[0.0, 500.0], None, true, &metriccraft_core::DE_DE),
            ["0", "500"]
        );
        assert_eq!(
            tick_labels(&[0.0, 2500.0], None, true, &metriccraft_core::DE_DE),
            ["0,0K", "2,5K"]
        );
        assert_eq!(
            tick_labels(&[2023.0, 2024.0], None, false, en),
            ["2023", "2024"]
        );
    }

    #[test]
    fn civil_round_trips() {
        assert_eq!(civil(0.0), (1899, 12, 30));
        assert_eq!(civil(46_309.0), (2026, 10, 14));
        assert_eq!(serial(2026, 10, 14), 46_309.0);
        for s in [-657_434.0, -1.0, 59.0, 60.0, 61.0, 2_958_465.0] {
            let (y, m, d) = civil(s);
            assert_eq!(serial(y, m, d), s, "{s}");
        }
        assert_eq!(civil(f64::INFINITY), (9999, 12, 31));
    }

    #[test]
    fn date_ticks_at_nice_dates() {
        let en = &metriccraft_core::EN_GB;
        let jan = serial(2024, 1, 1);
        let t = date_ticks(jan, serial(2024, 12, 31), 6, en);
        let labels: Vec<&str> = t.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["Jan 2024", "Apr 2024", "Jul 2024", "Oct 2024"]);
        let t = date_ticks(jan, jan + 6.0, 8, en);
        assert_eq!(t.len(), 7);
        assert_eq!(t.first().map(|(_, l)| l.as_str()), Some("1 Jan"));
        let t = date_ticks(serial(1990, 5, 1), serial(2024, 1, 1), 5, en);
        let labels: Vec<&str> = t.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, ["2000", "2010", "2020"]);
        let t = date_ticks(serial(100, 1, 1), serial(9999, 1, 1), 6, en);
        assert!(!t.is_empty() && t.len() <= 6, "{t:?}");
        assert!(date_ticks(f64::NAN, 1.0, 5, en).is_empty());
        assert!(date_ticks(-1e300, 1e300, 5, en).len() <= 5);
    }
}
