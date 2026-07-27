//! Relative ages, computed at render time only.
//!
//! The pane never redraws on a timer, so an age is a pure function of a
//! timestamp and the clock at the moment of the render.

use std::time::{SystemTime, UNIX_EPOCH};

/// Unix seconds for right now.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Parses the RFC 3339 UTC timestamps GitHub returns (`2026-07-27T09:14:03Z`)
/// into unix seconds. Anything else is `None` and renders as a blank age.
pub fn parse_timestamp(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<i64> { text.get(from..to)?.parse().ok() };
    let (year, month, day) = (number(0, 4)?, number(5, 7)?, number(8, 10)?);
    let (hour, minute, second) = (number(11, 13)?, number(14, 16)?, number(17, 19)?);
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A compact age for a list row: `now`, `18m`, `4h`, `12d`.
pub fn relative_age(then: i64, now: i64) -> String {
    let seconds = (now - then).max(0);
    if seconds < 60 {
        "now".to_string()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

/// The header's phrasing for the age of the data on screen.
pub fn fetched_phrase(fetched_at: i64, now: i64) -> String {
    if now - fetched_at < 60 {
        "fetched just now".to_string()
    } else {
        format!("fetched {} ago", relative_age(fetched_at, now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_github_timestamp() {
        assert_eq!(parse_timestamp("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_timestamp("2026-07-27T09:14:03Z"), Some(1_785_143_643));
        assert_eq!(parse_timestamp("not a timestamp"), None);
    }

    #[test]
    fn ages_shrink_to_the_largest_whole_unit() {
        assert_eq!(relative_age(0, 30), "now");
        assert_eq!(relative_age(0, 18 * 60), "18m");
        assert_eq!(relative_age(0, 4 * 3_600), "4h");
        assert_eq!(relative_age(0, 12 * 86_400), "12d");
    }
}
