//! Sessions, the store (SQLite index + NDJSON logs), the event bus, the
//! settings, the repo catalog and the session manager. No HTTP and no process
//! spawning here: git and setup commands run in `calin-git`, agents in
//! `calin-agents`.
//!
//! The log is the source of truth: an event is written to its session's log
//! before it is indexed or shown to anyone, and the index can always be
//! rebuilt from the logs.

mod automations;
mod briefing;
mod clock;
mod error;
mod home;
mod json_file;
mod limits;
mod manager;
mod policy;
mod repos;
mod settings;
mod store;
#[cfg(test)]
mod testing;
mod title;
mod tokens;
mod workflows;
mod workspace;

pub use clock::{Clock, ManualClock, SharedClock, SystemClock, TimeZone};
pub use error::{Error, Result};
pub use home::{ENV_HOME, Home, HomeError, resolve_home};
pub use manager::{
    AgentRepo, Dispatched, DispatchedRepo, ManagerConfig, NoteOutcome, SessionManager,
};
pub use store::{
    Dispatch, EventDraft, Live, LiveEnd, NewSession, OpenPermission, OpenQuestion, SessionSetup,
    Store, Subscription,
};
pub use tokens::AgentRef;
pub use workspace::MAX_FILE_BYTES;
