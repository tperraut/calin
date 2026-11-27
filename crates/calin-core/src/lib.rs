//! Sessions, the store (SQLite index + NDJSON logs), the event bus, the repo
//! registry and the session manager. No HTTP and no process spawning here.
//!
//! The log is the source of truth: an event is written to its session's log
//! before it is indexed or shown to anyone, and the index can always be
//! rebuilt from the logs.

mod error;
mod home;
mod manager;
mod repos;
mod store;
#[cfg(test)]
mod testing;
mod title;
mod workspace;

pub use error::{Error, Result};
pub use home::{ENV_HOME, Home, HomeError, resolve_home};
pub use manager::SessionManager;
pub use store::{
    EventDraft, Live, LiveEnd, NewSession, OpenPermission, SessionSetup, Store, Subscription,
};
pub use workspace::MAX_FILE_BYTES;
