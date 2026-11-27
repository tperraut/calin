//! What time it is, and where: the clock and the time zone that usage days
//! and automation schedules are read in. Both are injected through
//! [`crate::ManagerConfig`], so tests neither sleep nor depend on the machine.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

pub use jiff::tz::TimeZone;

/// The current time in Unix milliseconds.
pub trait Clock: Send + Sync + 'static {
    fn now_ms(&self) -> i64;
}

/// The machine's clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as i64)
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: AtomicI64,
}

impl ManualClock {
    pub fn new(now_ms: i64) -> Self {
        Self {
            now: AtomicI64::new(now_ms),
        }
    }

    pub fn set(&self, now_ms: i64) {
        self.now.store(now_ms, Ordering::SeqCst);
    }

    pub fn advance(&self, ms: i64) {
        self.now.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> i64 {
        self.now.load(Ordering::SeqCst)
    }
}

impl Clock for Arc<ManualClock> {
    fn now_ms(&self) -> i64 {
        self.as_ref().now_ms()
    }
}

/// A clock shared by everything one manager runs. Two are equal when they
/// are the same clock.
#[derive(Clone)]
pub struct SharedClock(Arc<dyn Clock>);

impl SharedClock {
    pub fn new(clock: impl Clock) -> Self {
        Self(Arc::new(clock))
    }

    pub fn now_ms(&self) -> i64 {
        self.0.now_ms()
    }
}

impl Default for SharedClock {
    fn default() -> Self {
        Self::new(SystemClock)
    }
}

impl fmt::Debug for SharedClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SharedClock({})", self.now_ms())
    }
}

impl PartialEq for SharedClock {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SharedClock {}

/// The local date of `at` (Unix milliseconds) in `zone`, as `YYYY-MM-DD`.
pub(crate) fn local_date(zone: &TimeZone, at_ms: i64) -> jiff::civil::Date {
    let timestamp = jiff::Timestamp::from_millisecond(at_ms).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
    zone.to_datetime(timestamp).date()
}
