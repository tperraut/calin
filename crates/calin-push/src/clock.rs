//! Time as an input, so the throttle and "last sent" are tested without sleeping.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A source of wall-clock time, in milliseconds since the Unix epoch.
///
/// The throttle, the VAPID token's expiry and every timestamp in the
/// subscription store read time through this.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    /// Milliseconds since the Unix epoch.
    fn now_millis(&self) -> u64;

    /// Whole seconds since the Unix epoch.
    fn now_secs(&self) -> u64 {
        self.now_millis() / 1000
    }
}

/// The operating system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_millis(&self) -> u64 {
        // A clock set before 1970 reads as 0; nothing here is a security
        // decision that a wrong clock could loosen beyond the throttle.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to. For tests, here and in the server.
#[derive(Debug)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// A clock stopped at `now_millis` milliseconds since the Unix epoch.
    pub fn new(now_millis: u64) -> Self {
        Self(AtomicU64::new(now_millis))
    }

    /// Jumps to `now_millis`.
    pub fn set(&self, now_millis: u64) {
        self.0.store(now_millis, Ordering::SeqCst);
    }

    /// Moves forward by `by`.
    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.0.fetch_add(millis, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_millis(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::new(100_000);
        assert_eq!(clock.now_secs(), 100);
        clock.advance(Duration::from_millis(5_500));
        assert_eq!(clock.now_millis(), 105_500);
        assert_eq!(clock.now_secs(), 105);
        clock.set(7);
        assert_eq!(clock.now_millis(), 7);
    }

    #[test]
    fn system_clock_is_after_2025() {
        assert!(SystemClock.now_secs() > 1_735_689_600);
    }
}
