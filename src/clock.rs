//! Wall-clock time behind a trait so timestamps are deterministic in tests.

use std::sync::atomic::{AtomicI64, Ordering};

use jiff::Timestamp;

/// A source of the current time.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// The system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock for tests: starts at a fixed second and moves only when told.
#[derive(Debug)]
pub struct FixedClock {
    seconds: AtomicI64,
}

impl FixedClock {
    /// A clock stopped at `seconds` after the Unix epoch.
    #[must_use]
    pub fn at(seconds: i64) -> Self {
        Self {
            seconds: AtomicI64::new(seconds),
        }
    }

    /// Moves the clock forward by `seconds`.
    pub fn advance(&self, seconds: i64) {
        self.seconds.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_second(self.seconds.load(Ordering::SeqCst)).unwrap_or(Timestamp::UNIX_EPOCH)
    }
}

/// Formats `at` as UTC RFC 3339 with whole seconds, e.g.
/// `2026-11-04T09:12:03Z`. This is the form used in front matter and in the
/// database, so the strings also sort chronologically.
///
/// # Examples
///
/// ```
/// use jiff::Timestamp;
/// use joedocs::clock::format_timestamp;
///
/// let at = Timestamp::from_second(1_793_000_000).unwrap();
/// assert_eq!(format_timestamp(at), "2026-10-26T07:33:20Z");
/// ```
#[must_use]
pub fn format_timestamp(at: Timestamp) -> String {
    at.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_moves_only_when_advanced() {
        let clock = FixedClock::at(1_793_000_000);
        assert_eq!(format_timestamp(clock.now()), "2026-10-26T07:33:20Z");
        clock.advance(61);
        assert_eq!(format_timestamp(clock.now()), "2026-10-26T07:34:21Z");
    }

    #[test]
    fn fixed_clock_out_of_range_falls_back_to_the_epoch() {
        let clock = FixedClock::at(i64::MAX);
        assert_eq!(clock.now(), Timestamp::UNIX_EPOCH);
    }

    #[test]
    fn timestamps_drop_fractional_seconds() {
        let at = Timestamp::new(1_793_000_000, 999_999_999).unwrap();
        assert_eq!(format_timestamp(at), "2026-10-26T07:33:20Z");
        assert!(SystemClock.now() > Timestamp::from_second(1_700_000_000).unwrap());
    }
}
