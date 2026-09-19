//! Date/time helpers that match how SQLAlchemy stores `DateTime` values in
//! SQLite (`YYYY-MM-DD HH:MM:SS.ffffff`) so Rust-written rows stay readable by
//! the Python runtime and vice versa.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current UTC timestamp in the SQLAlchemy/SQLite representation.
pub fn now_python() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    python_datetime(now.as_secs() as i64, now.subsec_micros())
}

/// Format a UTC unix timestamp the way SQLAlchemy persists `DateTime` on SQLite.
pub fn python_datetime(seconds: i64, micros: u32) -> String {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}.{micros:06}")
}

/// Parse a SQLite/SQLAlchemy datetime string into a UTC unix timestamp.
///
/// Accepts the SQLAlchemy representation (`YYYY-MM-DD HH:MM:SS[.ffffff]`) and
/// the RFC 3339 form the Rust canonical store emits.
pub fn parse_python_datetime(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let normalized = trimmed.replace('T', " ");
    let normalized = normalized.strip_suffix('Z').unwrap_or(&normalized);
    let (date, rest) = normalized.split_once(' ')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: u32 = date_parts.next()?.parse().ok()?;
    let day: u32 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut time_parts = rest.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second_part = time_parts.next()?;
    if time_parts.next().is_some() {
        return None;
    }
    let (second_text, fraction_text) = match second_part.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (second_part, None),
    };
    if let Some(fraction) = fraction_text
        && (fraction.len() > 9 || !fraction.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return None;
    }
    let second: i64 = second_text.parse().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since the unix epoch for a proleptic Gregorian date.
pub fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 } as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Civil date for a number of days since the unix epoch.
pub fn civil_date(days_since_epoch: i64) -> (i64, u32, u32) {
    let shifted = days_since_epoch + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

/// Day of week for a civil date, Monday = 0 (ISO weekday order).
pub fn iso_weekday(year: i64, month: u32, day: u32) -> u32 {
    let days = days_from_civil(year, month, day);
    (days.rem_euclid(7) + 3) as u32 % 7
}

/// Midnight UTC of the day containing `timestamp`.
pub fn start_of_day(timestamp: i64) -> i64 {
    timestamp.div_euclid(86_400) * 86_400
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datetime_round_trip_matches_sqlalchemy_format() {
        let formatted = python_datetime(1_700_000_123, 456_789);
        assert_eq!(formatted, "2023-11-14 22:15:23.456789");
        assert_eq!(parse_python_datetime(&formatted), Some(1_700_000_123));
    }

    #[test]
    fn datetime_accepts_rfc3339_and_missing_fraction() {
        assert_eq!(
            parse_python_datetime("2023-11-14 22:15:23"),
            Some(1_700_000_123)
        );
        assert_eq!(
            parse_python_datetime("2023-11-14T22:15:23Z"),
            Some(1_700_000_123)
        );
        assert_eq!(
            parse_python_datetime("2023-11-14 22:15:23.500000"),
            Some(1_700_000_123)
        );
    }

    #[test]
    fn civil_date_round_trips_known_epochs() {
        assert_eq!(civil_date(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2023, 11, 14), 19_675);
        assert_eq!(civil_date(19_675), (2023, 11, 14));
    }

    #[test]
    fn weekday_matches_known_dates() {
        // 2023-11-14 was a Tuesday (ISO weekday index 1).
        assert_eq!(iso_weekday(2023, 11, 14), 1);
        // 2024-01-01 was a Monday.
        assert_eq!(iso_weekday(2024, 1, 1), 0);
    }
}
