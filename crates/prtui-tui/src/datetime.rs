use chrono::{DateTime, Local, TimeZone};
use std::fmt::Display;

pub fn format_local(value: &str) -> String {
    format_in_zone(value, &Local)
}

fn format_in_zone<T: TimeZone>(value: &str, time_zone: &T) -> String
where
    T::Offset: Display,
{
    let Ok(timestamp) = DateTime::parse_from_rfc3339(value) else {
        return value.to_owned();
    };

    timestamp
        .with_timezone(time_zone)
        .format("%b %-d, %Y at %-I:%M %p")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{FixedOffset, Utc};

    #[test]
    fn timestamps_convert_offsets_and_roll_over_dates() {
        let time_zone = FixedOffset::west_opt(7 * 3600).unwrap();

        for (value, expected) in [
            ("2026-08-28T20:04:55Z", "Aug 28, 2026 at 1:04 PM"),
            ("2026-01-01T02:04:55Z", "Dec 31, 2025 at 7:04 PM"),
            ("2026-08-28T22:04:55.123+02:00", "Aug 28, 2026 at 1:04 PM"),
            ("2026-08-01T07:04:00Z", "Aug 1, 2026 at 12:04 AM"),
            ("2026-08-01T19:04:00Z", "Aug 1, 2026 at 12:04 PM"),
        ] {
            assert_eq!(format_in_zone(value, &time_zone), expected);
        }
    }

    #[test]
    fn values_without_a_valid_timestamp_are_preserved() {
        for value in ["", "2026-08-28", "invalid"] {
            assert_eq!(format_in_zone(value, &Utc), value);
        }
    }
}
