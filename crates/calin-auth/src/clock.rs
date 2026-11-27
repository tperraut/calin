//! Time as an input, so expiry is tested without sleeping.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of wall-clock time, in whole seconds since the Unix epoch.
///
/// Everything in this crate that expires (pending logins, sessions, the
/// failure limiter, key refresh, token validity) reads time through this.
pub trait Clock: Send + Sync + fmt::Debug + 'static {
    /// Seconds since the Unix epoch.
    fn now(&self) -> u64;
}

/// The operating system's clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> u64 {
        // A clock set before 1970 reads as 0: every token is then "not yet
        // valid" or expired, which is the refusing side.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0)
    }
}

/// A clock that only moves when told to. For tests, here and in the server.
#[derive(Debug)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// A clock stopped at `now` seconds since the Unix epoch.
    pub fn new(now: u64) -> Self {
        Self(AtomicU64::new(now))
    }

    /// Jumps to `now`.
    pub fn set(&self, now: u64) {
        self.0.store(now, Ordering::SeqCst);
    }

    /// Moves forward by `seconds`.
    pub fn advance(&self, seconds: u64) {
        self.0.fetch_add(seconds, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let clock = ManualClock::new(100);
        assert_eq!(clock.now(), 100);
        clock.advance(5);
        assert_eq!(clock.now(), 105);
        clock.set(7);
        assert_eq!(clock.now(), 7);
    }

    #[test]
    fn system_clock_is_after_2025() {
        assert!(SystemClock.now() > 1_735_689_600);
    }
}
