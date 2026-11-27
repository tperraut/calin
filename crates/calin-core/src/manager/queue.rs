//! The session queue (the "queue" semantics in `calin-proto/src/api.rs`): at
//! most `Settings::max_parallel` root sessions prepare or run at a time. A
//! session's driver takes a slot before it sets either status and gives it
//! back with any other; a driver that finds none waits here, oldest session
//! first. Child sessions never come here: they have a limit of their own.

use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard, PoisonError};

use tokio::sync::oneshot;

pub(super) struct Queue {
    state: Mutex<State>,
}

struct State {
    limit: usize,
    /// The sessions that hold a slot.
    active: HashSet<String>,
    /// The sessions that wait for one, oldest first. Session ids sort in
    /// the order the sessions were created.
    waiting: Vec<Waiter>,
}

struct Waiter {
    session_id: String,
    /// Told when the session was given a slot. Closed when its driver
    /// stopped waiting.
    granted: oneshot::Sender<()>,
}

/// What a session that asked for a slot got.
pub(super) enum Slot {
    Held,
    /// No slot is free: this resolves when the session was given one, which
    /// it then holds.
    Wait(oneshot::Receiver<()>),
}

impl Queue {
    pub(super) fn new(limit: u32) -> Self {
        Self {
            state: Mutex::new(State {
                limit: limit as usize,
                active: HashSet::new(),
                waiting: Vec::new(),
            }),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Every critical section leaves the sets consistent, panic or not.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes how many sessions may run at once. A higher limit starts the
    /// sessions that wait at once; a lower one stops nothing that runs.
    pub(super) fn set_limit(&self, limit: u32) {
        let mut state = self.state();
        state.limit = limit as usize;
        state.grant();
    }

    /// True when the session holds a slot after the call: it did already,
    /// or one was free. Never waits.
    pub(super) fn try_acquire(&self, session_id: &str) -> bool {
        let mut state = self.state();
        state.take(session_id)
    }

    /// Takes a slot, or joins the queue when none is free.
    pub(super) fn acquire(&self, session_id: &str) -> Slot {
        let mut state = self.state();
        if state.take(session_id) {
            return Slot::Held;
        }
        let (granted, wait) = oneshot::channel();
        state.waiting.retain(|other| other.session_id != session_id);
        let at = state
            .waiting
            .partition_point(|other| other.session_id.as_str() < session_id);
        state.waiting.insert(
            at,
            Waiter {
                session_id: session_id.to_owned(),
                granted,
            },
        );
        Slot::Wait(wait)
    }

    /// Gives the session's slot back, or takes it out of the queue, and
    /// starts whoever is next. Does nothing for a session that has neither.
    pub(super) fn leave(&self, session_id: &str) {
        let mut state = self.state();
        state.active.remove(session_id);
        state.waiting.retain(|other| other.session_id != session_id);
        state.grant();
    }

    #[cfg(test)]
    pub(super) fn holds(&self, session_id: &str) -> bool {
        self.state().active.contains(session_id)
    }
}

impl State {
    /// True when `session_id` holds a slot, taking a free one if it must.
    fn take(&mut self, session_id: &str) -> bool {
        if self.active.contains(session_id) {
            return true;
        }
        // Whoever waits is older than a newcomer and goes first.
        self.grant();
        if self.active.len() >= self.limit {
            return false;
        }
        self.active.insert(session_id.to_owned());
        true
    }

    /// Hands free slots to the sessions that waited longest.
    fn grant(&mut self) {
        while self.active.len() < self.limit && !self.waiting.is_empty() {
            let next = self.waiting.remove(0);
            // A driver that stopped waiting is passed over.
            if next.granted.send(()).is_ok() {
                self.active.insert(next.session_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn waits(slot: Slot) -> oneshot::Receiver<()> {
        match slot {
            Slot::Wait(wait) => wait,
            Slot::Held => panic!("a slot was free"),
        }
    }

    #[test]
    fn slots_are_taken_up_to_the_limit_and_given_to_the_oldest_waiter() {
        let queue = Queue::new(2);
        assert!(queue.try_acquire("01"));
        assert!(matches!(queue.acquire("02"), Slot::Held));
        // Holding one already is holding one.
        assert!(queue.try_acquire("01"));
        assert!(!queue.try_acquire("05"));
        assert!(!queue.holds("05"));

        // They join out of order and are served by age.
        let mut fifth = waits(queue.acquire("05"));
        let mut third = waits(queue.acquire("03"));
        let mut fourth = waits(queue.acquire("04"));
        queue.leave("01");
        assert!(third.try_recv().is_ok());
        assert!(fourth.try_recv().is_err() && fifth.try_recv().is_err());
        assert!(queue.holds("03") && !queue.holds("01"));

        // One that gave up is passed over; leaving twice changes nothing.
        drop(fourth);
        queue.leave("02");
        queue.leave("02");
        assert!(fifth.try_recv().is_ok());
        assert!(queue.holds("05") && !queue.holds("04"));
        assert!(!queue.try_acquire("06"));
    }

    #[test]
    fn a_higher_limit_starts_the_waiting_at_once_and_a_lower_one_stops_nothing() {
        let queue = Queue::new(1);
        assert!(queue.try_acquire("01"));
        let mut second = waits(queue.acquire("02"));
        let mut third = waits(queue.acquire("03"));
        let mut fourth = waits(queue.acquire("04"));
        queue.set_limit(3);
        assert!(second.try_recv().is_ok() && third.try_recv().is_ok());
        assert!(fourth.try_recv().is_err());

        queue.set_limit(1);
        assert!(queue.holds("01") && queue.holds("02") && queue.holds("03"));
        // Two must leave before the next one starts.
        queue.leave("01");
        queue.leave("02");
        assert!(fourth.try_recv().is_err());
        queue.leave("03");
        assert!(fourth.try_recv().is_ok());

        // Leaving the queue is not leaving a slot.
        let fifth = waits(queue.acquire("05"));
        queue.leave("05");
        drop(fifth);
        queue.leave("04");
        assert!(queue.try_acquire("06"));
    }
}
