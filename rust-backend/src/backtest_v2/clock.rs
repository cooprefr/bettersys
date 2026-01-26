//! Simulation Clock
//!
//! Monotonic simulated clock with nanosecond resolution.
//! Single source of truth for all simulation time - NEVER call system time.

use std::fmt;

/// Nanoseconds since Unix epoch (1970-01-01 00:00:00 UTC).
/// i64 gives us ~292 years of range, sufficient for any backtest.
pub type Nanos = i64;

/// Conversion constants
pub const NANOS_PER_MICRO: i64 = 1_000;
pub const NANOS_PER_MILLI: i64 = 1_000_000;
pub const NANOS_PER_SEC: i64 = 1_000_000_000;

/// Monotonic simulation clock.
///
/// # Determinism Contract
/// - `now()` returns the current simulation time, never system time
/// - `advance_to()` only moves forward, panics on backward movement
/// - All components must use this clock exclusively for timestamps
#[derive(Debug, Clone)]
pub struct SimClock {
    current: Nanos,
}

impl SimClock {
    /// Create a new clock starting at the given time.
    #[inline]
    pub fn new(start_time: Nanos) -> Self {
        Self {
            current: start_time,
        }
    }

    /// Create a clock from a Unix timestamp in seconds.
    #[inline]
    pub fn from_unix_secs(secs: i64) -> Self {
        Self::new(secs * NANOS_PER_SEC)
    }

    /// Current simulation time in nanoseconds.
    #[inline]
    pub fn now(&self) -> Nanos {
        self.current
    }

    /// Current simulation time in microseconds.
    #[inline]
    pub fn now_micros(&self) -> i64 {
        self.current / NANOS_PER_MICRO
    }

    /// Current simulation time in milliseconds.
    #[inline]
    pub fn now_millis(&self) -> i64 {
        self.current / NANOS_PER_MILLI
    }

    /// Current simulation time in seconds.
    #[inline]
    pub fn now_secs(&self) -> i64 {
        self.current / NANOS_PER_SEC
    }

    /// Advance clock to a new time. Panics if time would go backward.
    #[inline]
    pub fn advance_to(&mut self, new_time: Nanos) {
        debug_assert!(
            new_time >= self.current,
            "SimClock: cannot go backward from {} to {}",
            self.current,
            new_time
        );
        self.current = new_time;
    }

    /// Advance clock by a delta. Panics if delta is negative.
    #[inline]
    pub fn advance_by(&mut self, delta: Nanos) {
        debug_assert!(delta >= 0, "SimClock: delta must be non-negative");
        self.current += delta;
    }

    /// Check if a time is in the past relative to current clock.
    #[inline]
    pub fn is_past(&self, time: Nanos) -> bool {
        time < self.current
    }

    /// Check if a time is at or after the current clock.
    #[inline]
    pub fn is_future_or_now(&self, time: Nanos) -> bool {
        time >= self.current
    }

    /// Duration since a past time.
    #[inline]
    pub fn elapsed_since(&self, past: Nanos) -> Nanos {
        (self.current - past).max(0)
    }
}

impl Default for SimClock {
    fn default() -> Self {
        Self::new(0)
    }
}

impl fmt::Display for SimClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let secs = self.current / NANOS_PER_SEC;
        let nanos = self.current % NANOS_PER_SEC;
        write!(f, "{}.{:09}s", secs, nanos)
    }
}

/// Helper to convert chrono DateTime to Nanos.
#[inline]
pub fn datetime_to_nanos(dt: &chrono::DateTime<chrono::Utc>) -> Nanos {
    dt.timestamp_nanos_opt().unwrap_or(0)
}

/// Helper to convert Nanos to chrono DateTime.
#[inline]
pub fn nanos_to_datetime(nanos: Nanos) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    let secs = nanos / NANOS_PER_SEC;
    let nsecs = (nanos % NANOS_PER_SEC) as u32;
    chrono::Utc.timestamp_opt(secs, nsecs).unwrap()
}

/// Helper to parse ISO8601/RFC3339 string to Nanos.
pub fn parse_timestamp(s: &str) -> Option<Nanos> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| datetime_to_nanos(&dt.with_timezone(&chrono::Utc)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clock_monotonic() {
        let mut clock = SimClock::new(1_000_000_000);
        assert_eq!(clock.now(), 1_000_000_000);

        clock.advance_to(2_000_000_000);
        assert_eq!(clock.now(), 2_000_000_000);

        clock.advance_by(500_000_000);
        assert_eq!(clock.now(), 2_500_000_000);
    }

    #[test]
    fn test_clock_conversions() {
        let clock = SimClock::from_unix_secs(1700000000);
        assert_eq!(clock.now_secs(), 1700000000);
        assert_eq!(clock.now_millis(), 1700000000000);
        assert_eq!(clock.now_micros(), 1700000000000000);
    }

    #[test]
    #[should_panic(expected = "cannot go backward")]
    fn test_clock_backward_panics() {
        let mut clock = SimClock::new(1_000_000_000);
        clock.advance_to(500_000_000); // Should panic
    }

    #[test]
    fn test_datetime_roundtrip() {
        let original = chrono::Utc::now();
        let nanos = datetime_to_nanos(&original);
        let recovered = nanos_to_datetime(nanos);
        // Should be within 1 nanosecond due to potential rounding
        assert!((datetime_to_nanos(&original) - datetime_to_nanos(&recovered)).abs() <= 1);
    }
}
