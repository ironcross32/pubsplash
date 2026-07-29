//! Calendar arithmetic for the two engines that put a date on the wire.
//!
//! Edge wants a JavaScript `Date.toString()` in `X-Timestamp`, and SigV4 wants
//! `YYYYMMDDTHHMMSSZ`. Both are UTC and neither needs a timezone database, so
//! this is a dozen lines rather than a dependency.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, or 0 if the system clock predates it.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Splits a Unix timestamp into `(year, month, day, hour, minute, second)`,
/// all UTC. Months and days are 1-based.
pub fn utc_parts(unix_seconds: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (unix_seconds / 86_400) as i64;
    let seconds = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60,
    )
}

/// Day of the week for a Unix timestamp, 0 = Sunday.
pub fn weekday(unix_seconds: u64) -> usize {
    // 1970-01-01 was a Thursday, so day 0 is weekday 4.
    (((unix_seconds / 86_400) as i64 + 4).rem_euclid(7)) as usize
}

/// Howard Hinnant's days-to-civil conversion. `days` counts from 1970-01-01.
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants_decompose_correctly() {
        assert_eq!(utc_parts(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(utc_parts(1_704_067_200), (2024, 1, 1, 0, 0, 0));
        assert_eq!(utc_parts(1_440_938_160), (2015, 8, 30, 12, 36, 0));
    }

    /// Leap years are where hand-rolled calendar code usually goes wrong.
    #[test]
    fn leap_days_are_handled() {
        assert_eq!(utc_parts(1_709_209_845), (2024, 2, 29, 12, 30, 45));
        // 2000 was a leap year; 1900 and 2100 are not.
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(utc_parts(4_107_542_400), (2100, 3, 1, 0, 0, 0));
    }

    #[test]
    fn weekdays_advance_with_the_day() {
        assert_eq!(weekday(0), 4, "1970-01-01 was a Thursday");
        assert_eq!(weekday(1_704_067_200), 1, "2024-01-01 was a Monday");
        assert_eq!(weekday(1_709_209_845), 4, "2024-02-29 was a Thursday");
        for day in 0..14u64 {
            assert_eq!(weekday(day * 86_400), ((day as usize) + 4) % 7);
        }
    }

    /// Every day across a decade must round-trip through the split.
    #[test]
    fn the_day_boundary_never_slips() {
        let mut previous = None;
        for day in 18_000..21_650i64 {
            let (year, month, date) = civil_from_days(day);
            assert!((1..=12).contains(&month), "day {day} gave month {month}");
            assert!((1..=31).contains(&date), "day {day} gave date {date}");
            if let Some(prior) = previous {
                assert!((year, month, date) > prior, "went backwards at day {day}");
            }
            previous = Some((year, month, date));
        }
    }
}
