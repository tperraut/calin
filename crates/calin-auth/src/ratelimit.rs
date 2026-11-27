//! The limit on failed sign-in callbacks per client address (spec section 3).

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::client_addr::AddressKey;
use crate::clock::Clock;

/// Failed callbacks allowed per address within [`WINDOW_SECS`].
pub(crate) const MAX_FAILURES: usize = 10;
/// The window the failures are counted over: 10 minutes.
pub(crate) const WINDOW_SECS: u64 = 10 * 60;
/// Addresses remembered at once. Past this the stalest is forgotten, so an
/// attacker with many addresses costs a bounded amount of memory.
const MAX_ADDRESSES: usize = 4096;

/// Counts failed sign-in callbacks per client address: 10 per 10 minutes.
///
/// An attempt takes its slot when it begins and gives it back only if it
/// succeeds or turns out not to be a sign-in attempt at all, so attempts
/// running at the same time cannot all slip under the limit, and a caller
/// cannot forget to count a failure.
///
/// IPv6 addresses are counted per /64, the smallest block one customer
/// usually controls: counting single addresses would not limit anyone.
#[derive(Debug)]
pub struct FailureLimiter {
    clock: Arc<dyn Clock>,
    max_failures: usize,
    max_addresses: usize,
    entries: Mutex<HashMap<AddressKey, VecDeque<u64>>>,
}

/// The address is over the limit. Nothing was attempted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("too many failed sign-in attempts from this address")]
pub struct RateLimited;

impl FailureLimiter {
    /// A limiter with the limits of spec section 3.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self::with_limits(clock, MAX_FAILURES, MAX_ADDRESSES)
    }

    pub(crate) fn with_limits(
        clock: Arc<dyn Clock>,
        max_failures: usize,
        max_addresses: usize,
    ) -> Self {
        Self {
            clock,
            max_failures,
            max_addresses,
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Starts an attempt for `address`, or refuses when the address already
    /// has 10 failures in the last 10 minutes.
    ///
    /// The attempt counts as failed unless [`Attempt::succeeded`] is called.
    pub fn begin(&self, address: IpAddr) -> Result<Attempt<'_>, RateLimited> {
        let key = AddressKey::of(address);
        let now = self.clock.now();
        let mut entries = self.lock();
        let live = |stamp: &u64| now < stamp.saturating_add(WINDOW_SECS);

        if let Some(stamps) = entries.get_mut(&key) {
            stamps.retain(live);
            if stamps.len() >= self.max_failures {
                return Err(RateLimited);
            }
        } else if entries.len() >= self.max_addresses {
            entries.retain(|_, stamps| {
                stamps.retain(live);
                !stamps.is_empty()
            });
            if entries.len() >= self.max_addresses {
                let stalest = entries
                    .iter()
                    .min_by_key(|(_, stamps)| stamps.back().copied().unwrap_or(0))
                    .map(|(key, _)| *key);
                if let Some(stalest) = stalest {
                    entries.remove(&stalest);
                }
            }
        }
        entries.entry(key).or_default().push_back(now);
        Ok(Attempt {
            limiter: self,
            key,
            at: now,
        })
    }

    #[cfg(test)]
    fn tracked_addresses(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<AddressKey, VecDeque<u64>>> {
        // The map is consistent after every statement, so a panic elsewhere
        // while it was held leaves nothing to repair.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One counted sign-in attempt. Dropping it leaves it counted as a failure.
#[derive(Debug)]
#[must_use = "an attempt that is dropped counts as a failure"]
pub struct Attempt<'a> {
    limiter: &'a FailureLimiter,
    key: AddressKey,
    at: u64,
}

impl Attempt<'_> {
    /// The attempt succeeded: it no longer counts against the address.
    pub fn succeeded(self) {
        self.release();
    }

    /// The request was not a sign-in attempt from this client's browser (it
    /// carried no live pending login): it does not count either. Counting
    /// it would let anyone block an address by sending junk in its name,
    /// which a page can make a visitor's browser do.
    pub fn not_an_attempt(self) {
        self.release();
    }

    fn release(self) {
        let mut entries = self.limiter.lock();
        if let Some(stamps) = entries.get_mut(&self.key) {
            if let Some(index) = stamps.iter().rposition(|stamp| *stamp == self.at) {
                stamps.remove(index);
            }
            if stamps.is_empty() {
                entries.remove(&self.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;

    const START: u64 = 1_800_000_000;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn limiter() -> (FailureLimiter, Arc<ManualClock>) {
        let clock = Arc::new(ManualClock::new(START));
        (FailureLimiter::new(clock.clone()), clock)
    }

    fn fail(limiter: &FailureLimiter, address: &str, times: usize) {
        for _ in 0..times {
            drop(limiter.begin(ip(address)).unwrap());
        }
    }

    #[test]
    fn the_eleventh_failure_in_ten_minutes_is_refused() {
        let (limiter, _clock) = limiter();
        fail(&limiter, "198.51.100.1", 10);
        assert_eq!(limiter.begin(ip("198.51.100.1")).unwrap_err(), RateLimited);
        // Another address is unaffected.
        assert!(limiter.begin(ip("198.51.100.2")).is_ok());
    }

    #[test]
    fn failures_age_out_after_the_window() {
        let (limiter, clock) = limiter();
        fail(&limiter, "198.51.100.1", 4);
        clock.advance(300);
        fail(&limiter, "198.51.100.1", 6);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());

        // One second before the first four age out.
        clock.set(START + 599);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
        // At ten minutes the first four are gone: four more are allowed.
        clock.set(START + 600);
        fail(&limiter, "198.51.100.1", 4);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
        // Refused attempts did not extend the block.
        clock.set(START + 900);
        fail(&limiter, "198.51.100.1", 6);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
    }

    #[test]
    fn a_success_gives_its_slot_back() {
        let (limiter, _clock) = limiter();
        for _ in 0..100 {
            limiter.begin(ip("198.51.100.1")).unwrap().succeeded();
        }
        assert_eq!(limiter.tracked_addresses(), 0);
        fail(&limiter, "198.51.100.1", 9);
        limiter.begin(ip("198.51.100.1")).unwrap().succeeded();
        fail(&limiter, "198.51.100.1", 1);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
    }

    #[test]
    fn what_was_not_an_attempt_gives_its_slot_back() {
        let (limiter, _clock) = limiter();
        for _ in 0..100 {
            limiter.begin(ip("198.51.100.1")).unwrap().not_an_attempt();
        }
        assert_eq!(limiter.tracked_addresses(), 0);
        fail(&limiter, "198.51.100.1", 10);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
    }

    #[test]
    fn attempts_in_flight_count_against_the_limit() {
        let (limiter, _clock) = limiter();
        let in_flight: Vec<Attempt<'_>> = (0..10)
            .map(|_| limiter.begin(ip("198.51.100.1")).unwrap())
            .collect();
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
        for attempt in in_flight {
            attempt.succeeded();
        }
        assert!(limiter.begin(ip("198.51.100.1")).is_ok());
    }

    #[test]
    fn one_ipv6_network_shares_a_limit() {
        let (limiter, _clock) = limiter();
        for host in 0..10 {
            fail(&limiter, &format!("2001:db8:1:2::{host:x}"), 1);
        }
        assert!(limiter.begin(ip("2001:db8:1:2::abcd")).is_err());
        assert!(limiter.begin(ip("2001:db8:1:3::abcd")).is_ok());
    }

    #[test]
    fn memory_is_bounded_and_the_stalest_address_goes_first() {
        let clock = Arc::new(ManualClock::new(START));
        let limiter = FailureLimiter::with_limits(clock.clone(), 2, 3);
        fail(&limiter, "10.0.0.1", 2);
        clock.advance(1);
        fail(&limiter, "10.0.0.2", 2);
        clock.advance(1);
        fail(&limiter, "10.0.0.3", 2);
        clock.advance(1);
        assert_eq!(limiter.tracked_addresses(), 3);

        // A fourth address evicts the stalest, 10.0.0.1, and only that one.
        fail(&limiter, "10.0.0.4", 1);
        assert_eq!(limiter.tracked_addresses(), 3);
        assert!(limiter.begin(ip("10.0.0.2")).is_err());
        assert!(limiter.begin(ip("10.0.0.3")).is_err());
        assert!(limiter.begin(ip("10.0.0.1")).is_ok());
        assert_eq!(limiter.tracked_addresses(), 3);

        // Expired addresses are dropped before any live one is evicted.
        clock.advance(WINDOW_SECS);
        fail(&limiter, "10.0.0.9", 1);
        assert_eq!(limiter.tracked_addresses(), 1);
    }

    #[test]
    fn many_addresses_never_exceed_the_bound() {
        let (limiter, _clock) = limiter();
        for n in 0..(MAX_ADDRESSES as u32 + 500) {
            drop(
                limiter
                    .begin(IpAddr::from((0x0a00_0000 + n).to_be_bytes()))
                    .unwrap(),
            );
        }
        assert_eq!(limiter.tracked_addresses(), MAX_ADDRESSES);
    }

    #[test]
    fn a_clock_that_steps_back_does_not_unblock() {
        let (limiter, clock) = limiter();
        fail(&limiter, "198.51.100.1", 10);
        clock.set(START - 5_000);
        assert!(limiter.begin(ip("198.51.100.1")).is_err());
    }
}
