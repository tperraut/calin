//! Logins that were started and not finished yet (spec section 2, step 1).

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::client_addr::AddressKey;
use crate::cookie::{PENDING_LOGIN_LIFETIME_SECS, PendingLoginRef};
use crate::next::NextPath;
use crate::secret::constant_time_eq;

/// Pending logins kept at once. Starting a login needs no credentials, so
/// without a bound anyone could fill memory; past it the oldest goes.
pub(crate) const MAX_PENDING_LOGINS: usize = 2048;
/// Pending logins one client address may hold. Without it a single client
/// could fill the table and push out the logins everyone else has under
/// way; with it that takes hundreds of addresses. A person needs one or two.
pub(crate) const MAX_PENDING_PER_ADDRESS: usize = 8;

/// What a callback must be matched against. Lives only in memory.
pub(crate) struct PendingLogin {
    pub(crate) state: String,
    pub(crate) nonce: String,
    pub(crate) code_verifier: String,
    pub(crate) next: NextPath,
}

struct Entry {
    reference_sha256: [u8; 32],
    owner: AddressKey,
    created: u64,
    login: PendingLogin,
}

pub(crate) struct PendingLogins {
    capacity: usize,
    per_address: usize,
    /// Oldest first.
    entries: Mutex<VecDeque<Entry>>,
}

impl PendingLogins {
    pub(crate) fn new(capacity: usize, per_address: usize) -> Self {
        Self {
            capacity,
            per_address,
            entries: Mutex::new(VecDeque::new()),
        }
    }

    /// Adds a login started from `address`. Makes room first: expired
    /// logins go, then that address's oldest while it is at its quota, then
    /// the oldest of all while the table is full.
    pub(crate) fn insert(
        &self,
        reference: &PendingLoginRef,
        address: IpAddr,
        login: PendingLogin,
        now: u64,
    ) {
        let owner = AddressKey::of(address);
        let mut entries = self.lock();
        entries.retain(|entry| !expired(entry, now));
        while entries.iter().filter(|e| e.owner == owner).count() >= self.per_address {
            match entries.iter().position(|entry| entry.owner == owner) {
                Some(oldest) => entries.remove(oldest),
                None => break,
            };
        }
        while entries.len() >= self.capacity {
            entries.pop_front();
        }
        entries.push_back(Entry {
            reference_sha256: reference.hash(),
            owner,
            created: now,
            login,
        });
    }

    /// Removes and returns the login `reference` points to, if it exists and
    /// is under 10 minutes old. Whatever the callback then does with it, it
    /// cannot be used again.
    pub(crate) fn take(&self, reference: &PendingLoginRef, now: u64) -> Option<PendingLogin> {
        let hash = reference.hash();
        let mut entries = self.lock();
        // Every entry is compared, in constant time, as in the store.
        let mut found = None;
        for (index, entry) in entries.iter().enumerate() {
            if constant_time_eq(&entry.reference_sha256, &hash) {
                found = Some(index);
            }
        }
        let entry = entries.remove(found?)?;
        (!expired(&entry, now)).then_some(entry.login)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        // The queue is consistent between statements; see FailureLimiter.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Also true for an entry "created in the future": after the clock stepped
/// back its age is unknown, and unknown is refused.
fn expired(entry: &Entry, now: u64) -> bool {
    now < entry.created || now - entry.created >= PENDING_LOGIN_LIFETIME_SECS
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: u64 = 1_800_000_000;

    fn login(tag: &str) -> PendingLogin {
        PendingLogin {
            state: format!("state-{tag}"),
            nonce: format!("nonce-{tag}"),
            code_verifier: format!("verifier-{tag}"),
            next: NextPath::root(),
        }
    }

    fn reference() -> PendingLoginRef {
        PendingLoginRef::generate().unwrap()
    }

    /// A distinct IPv4 address per `n`.
    fn address(n: u32) -> IpAddr {
        IpAddr::from((0x0a00_0000 + n).to_be_bytes())
    }

    #[test]
    fn a_pending_login_is_taken_once() {
        let pending = PendingLogins::new(8, 8);
        let (a, b) = (reference(), reference());
        pending.insert(&a, address(1), login("a"), START);
        pending.insert(&b, address(1), login("b"), START);
        assert_eq!(pending.take(&a, START + 1).unwrap().state, "state-a");
        assert!(pending.take(&a, START + 1).is_none());
        assert_eq!(pending.take(&b, START + 1).unwrap().nonce, "nonce-b");
        assert!(pending.take(&reference(), START + 1).is_none());
        assert_eq!(pending.len(), 0);
    }

    #[test]
    fn a_pending_login_expires_after_ten_minutes() {
        let pending = PendingLogins::new(8, 8);
        let (a, b, c) = (reference(), reference(), reference());
        pending.insert(&a, address(1), login("a"), START);
        pending.insert(&b, address(1), login("b"), START);
        pending.insert(&c, address(1), login("c"), START);
        assert!(pending.take(&a, START + 599).is_some());
        assert!(pending.take(&b, START + 600).is_none());
        // An expired login is consumed by the attempt, like any other.
        assert_eq!(pending.len(), 1);
        // A clock that stepped back makes the age unknown: refused.
        assert!(pending.take(&c, START - 1).is_none());
    }

    #[test]
    fn expired_logins_are_dropped_when_a_new_one_arrives() {
        let pending = PendingLogins::new(8, 8);
        for n in 0..5 {
            pending.insert(&reference(), address(n), login("old"), START);
        }
        pending.insert(&reference(), address(9), login("new"), START + 600);
        assert_eq!(pending.len(), 1);
    }

    #[test]
    fn the_cap_drops_the_oldest() {
        let pending = PendingLogins::new(3, 8);
        let references: Vec<PendingLoginRef> = (0..5).map(|_| reference()).collect();
        for (index, reference) in references.iter().enumerate() {
            let from = address(index as u32);
            pending.insert(
                reference,
                from,
                login(&index.to_string()),
                START + index as u64,
            );
            assert!(pending.len() <= 3);
        }
        assert!(pending.take(&references[0], START + 10).is_none());
        assert!(pending.take(&references[1], START + 10).is_none());
        for (index, reference) in references.iter().enumerate().skip(2) {
            assert_eq!(
                pending.take(reference, START + 10).unwrap().state,
                format!("state-{index}")
            );
        }
    }

    #[test]
    fn one_address_only_pushes_out_its_own_logins() {
        let pending = PendingLogins::new(MAX_PENDING_LOGINS, MAX_PENDING_PER_ADDRESS);
        let victim = reference();
        pending.insert(&victim, address(1), login("victim"), START);

        // A flood from one address, and from one IPv6 network.
        let mut last = reference();
        for n in 0..(3 * MAX_PENDING_LOGINS as u32) {
            last = reference();
            pending.insert(&last, address(2), login("flood"), START + 1);
            let host: IpAddr = format!("2001:db8:1:2::{n:x}").parse().unwrap();
            pending.insert(&reference(), host, login("flood"), START + 1);
        }
        assert_eq!(pending.len(), 1 + 2 * MAX_PENDING_PER_ADDRESS);
        // The flooder keeps its most recent ones; the victim's is untouched.
        assert_eq!(pending.take(&last, START + 2).unwrap().state, "state-flood");
        assert_eq!(
            pending.take(&victim, START + 2).unwrap().state,
            "state-victim"
        );
    }

    #[test]
    fn the_quota_drops_that_address_oldest_first() {
        let pending = PendingLogins::new(100, 2);
        let references: Vec<PendingLoginRef> = (0..3).map(|_| reference()).collect();
        let other = reference();
        pending.insert(&references[0], address(1), login("0"), START);
        pending.insert(&other, address(2), login("other"), START);
        pending.insert(&references[1], address(1), login("1"), START + 1);
        pending.insert(&references[2], address(1), login("2"), START + 2);
        assert_eq!(pending.len(), 3);
        assert!(pending.take(&references[0], START + 3).is_none());
        assert!(pending.take(&references[1], START + 3).is_some());
        assert!(pending.take(&references[2], START + 3).is_some());
        assert!(pending.take(&other, START + 3).is_some());
    }

    #[test]
    fn many_addresses_never_exceed_the_table() {
        let pending = PendingLogins::new(MAX_PENDING_LOGINS, MAX_PENDING_PER_ADDRESS);
        for n in 0..(MAX_PENDING_LOGINS as u32 + 500) {
            pending.insert(&reference(), address(n), login("x"), START);
        }
        assert_eq!(pending.len(), MAX_PENDING_LOGINS);
    }
}
