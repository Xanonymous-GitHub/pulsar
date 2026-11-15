//! The time of day in a zone that is a fixed number of seconds away from UTC.
//!
//! This file imports nothing from the crate, so it is tested on the host:
//!
//! ```sh
//! rustc --edition 2024 --test src/clock/local_time.rs -o /tmp/t && /tmp/t
//! ```

use core::fmt;

/// Seconds in a day.
const SECONDS_PER_DAY: i64 = 24 * 60 * 60;

/// The zone furthest behind UTC that is in use (UTC-12), in seconds.
pub const MIN_UTC_OFFSET_SECONDS: i32 = -12 * 3600;

/// The zone furthest ahead of UTC that is in use (UTC+14), in seconds.
pub const MAX_UTC_OFFSET_SECONDS: i32 = 14 * 3600;

/// Whether `seconds` is an offset from UTC that a real time zone can have.
#[must_use]
pub const fn is_valid_utc_offset(seconds: i32) -> bool {
    MIN_UTC_OFFSET_SECONDS <= seconds && seconds <= MAX_UTC_OFFSET_SECONDS
}

/// A time of day without a date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    /// Hour, 0 to 23.
    pub hour: u8,
    /// Minute, 0 to 59.
    pub minute: u8,
    /// Second, 0 to 59.
    pub second: u8,
}

impl LocalTime {
    /// The time of day `unix_seconds` after 1970-01-01 00:00 UTC, in the zone that is
    /// `utc_offset_seconds` ahead of UTC (negative for zones behind it).
    #[must_use]
    pub fn from_unix(unix_seconds: u64, utc_offset_seconds: i32) -> Self {
        // A time too large for `i64` is hundreds of billions of years away: any value will do.
        let seconds = i64::try_from(unix_seconds)
            .unwrap_or(i64::MAX)
            .saturating_add(i64::from(utc_offset_seconds))
            .rem_euclid(SECONDS_PER_DAY);
        // The remainder is below one day, so each part is below 24, 60 and 60.
        let part = |value: i64| u8::try_from(value).unwrap_or_default();
        Self {
            hour: part(seconds / 3600),
            minute: part(seconds / 60 % 60),
            second: part(seconds % 60),
        }
    }
}

impl fmt::Display for LocalTime {
    /// Formats as `HH:MM:SS`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}:{:02}", self.hour, self.minute, self.second)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-11-15 00:00:00 UTC.
    const MIDNIGHT_UTC: u64 = 1_794_700_800;

    fn at(unix_seconds: u64, offset: i32) -> String {
        LocalTime::from_unix(unix_seconds, offset).to_string()
    }

    #[test]
    fn utc_is_unchanged() {
        assert_eq!(at(MIDNIGHT_UTC, 0), "00:00:00");
        assert_eq!(at(MIDNIGHT_UTC + 3600 + 62, 0), "01:01:02");
    }

    #[test]
    fn zones_ahead_of_utc() {
        assert_eq!(at(MIDNIGHT_UTC, 8 * 3600), "08:00:00"); // Taipei
        assert_eq!(at(MIDNIGHT_UTC, 20_700), "05:45:00"); // Kathmandu, UTC+5:45
        assert_eq!(at(MIDNIGHT_UTC + 11 * 3600, 14 * 3600), "01:00:00"); // wraps past midnight
    }

    #[test]
    fn zones_behind_utc_wrap_to_the_day_before() {
        assert_eq!(at(MIDNIGHT_UTC, -5 * 3600), "19:00:00"); // New York in winter
        assert_eq!(at(MIDNIGHT_UTC + 1, -12 * 3600), "12:00:01");
        assert_eq!(at(MIDNIGHT_UTC, -12_600), "20:30:00"); // Newfoundland, UTC-3:30
    }

    #[test]
    fn an_offset_larger_than_the_time_does_not_underflow() {
        assert_eq!(at(0, -3600), "23:00:00");
        assert_eq!(at(10, -86_400 * 3), "00:00:10");
    }

    #[test]
    fn the_end_of_time_does_not_overflow() {
        let _ = LocalTime::from_unix(u64::MAX, i32::MAX);
        let _ = LocalTime::from_unix(u64::MAX, i32::MIN);
    }

    #[test]
    fn valid_offsets_are_the_zones_in_use() {
        assert!(is_valid_utc_offset(0));
        assert!(is_valid_utc_offset(-12 * 3600));
        assert!(is_valid_utc_offset(14 * 3600));
        assert!(is_valid_utc_offset(20_700));
        assert!(!is_valid_utc_offset(14 * 3600 + 1));
        assert!(!is_valid_utc_offset(-12 * 3600 - 1));
        assert!(!is_valid_utc_offset(i32::MIN));
    }
}
