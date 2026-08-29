//! Unix time to a civil date, without `chrono`.
//!
//! This is Howard Hinnant's `civil_from_days` algorithm: it shifts the epoch to
//! 0000-03-01 so that the leap day lands at the *end* of the year, which makes
//! the month-length pattern regular and removes every special case. It is exact
//! for the whole proleptic Gregorian range and uses no lookup tables.
//!
//! **Everything zql reports is UTC**, because the standard library ships no
//! time-zone database and inventing one is not on the schedule. The README says
//! so rather than letting a reader assume local time.

/// Days from 1970-01-01 to the civil date `(year, month, day)`.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    // Shift to an era beginning 0000-03-01.
    let z = days_since_epoch + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097); // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153; // March-based month, [0, 11]
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Splits a Unix timestamp into `(days, seconds-within-day)`.
///
/// Euclidean division is what makes pre-1970 timestamps come out right: a
/// truncating divide would round `-1` toward zero and put the result on the
/// wrong day.
fn split_days(unix_seconds: i64) -> (i64, u32) {
    let days = unix_seconds.div_euclid(86_400);
    let secs = unix_seconds.rem_euclid(86_400) as u32;
    (days, secs)
}

/// `YYYY-MM-DD`, as `date()` returns it.
pub fn format_date(unix_seconds: i64) -> String {
    let (days, _) = split_days(unix_seconds);
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// `YYYY-MM-DD HH:MM:SS`, the text form of a Postgres `timestamp`.
pub fn format_timestamp(unix_seconds: i64) -> String {
    let (days, secs) = split_days(unix_seconds);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (secs / 3600, (secs / 60) % 60, secs % 60);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_epoch_itself() {
        assert_eq!(format_timestamp(0), "1970-01-01 00:00:00");
    }

    #[test]
    fn a_known_instant() {
        // 2001-09-09T01:46:40Z — the SOURCE_DATE_EPOCH used by the build recipe.
        assert_eq!(format_timestamp(1_000_000_000), "2001-09-09 01:46:40");
    }

    #[test]
    fn leap_day() {
        assert_eq!(format_date(951_782_400), "2000-02-29");
    }

    #[test]
    fn before_the_epoch_rounds_the_right_way() {
        assert_eq!(format_timestamp(-1), "1969-12-31 23:59:59");
        assert_eq!(format_date(-86_400), "1969-12-31");
    }

    #[test]
    fn extremes_do_not_panic() {
        format_timestamp(i64::MAX);
        format_timestamp(i64::MIN);
    }
}
