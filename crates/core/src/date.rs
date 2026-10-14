//! Calendar arithmetic shared by values, `FORMAT` and visual axes: H. Hinnant's civil-date
//! algorithms over days since 1970-01-01. DAX date serials count days from 1899-12-30.

/// Days from serial 0 (1899-12-30) to 1970-01-01.
pub const UNIX_EPOCH_DAYS: i64 = 25_569;

/// (year, month 1–12, day 1–31) of `days` since 1970-01-01.
pub fn civil(days: i64) -> (i64, i64, i64) {
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

/// Days since 1970-01-01 of (year, month 1–12, day); callers keep `y` in a sane range.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(-UNIX_EPOCH_DAYS), (1899, 12, 30));
        assert_eq!(days_from_civil(2026, 10, 14), 46_309 - UNIX_EPOCH_DAYS);
        for days in (-700_000..3_000_000).step_by(997) {
            let (y, m, d) = civil(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        let _ = civil(i64::MAX);
    }
}
