//! REST bodies. Every route lives under `/api/v1`.
//!
//! | Method | Path | Request | Response |
//! | --- | --- | --- | --- |
//! | GET | `/health` | | [`Health`] |
//! | GET | `/sessions` | | `Vec<Session>`, newest first |
//! | POST | `/sessions` | [`CreateSessionRequest`] | `201` [`SessionDetail`] |
//! | GET | `/sessions/{id}` | | [`SessionDetail`] |
//! | POST | `/sessions/{id}/messages` | [`SendMessageRequest`] | `202`, empty body |
//! | POST | `/sessions/{id}/stop` | | `202`, empty body |
//! | GET | `/ws` | WebSocket upgrade | frames of `ClientMsg` / `ServerMsg` |
//!
//! Errors are an [`ApiError`] with status 400 (invalid input), 404 (unknown
//! session) or 409 (not possible in the session's current state).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Agent, Session};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Health {
    /// Always `calin`.
    pub name: String,
    pub version: String,
    /// True when every agent runs on the mock runner (`CALIN_DRY_RUN=1`).
    pub dry_run: bool,
    /// Runner ids this server can start, the default one first.
    pub runners: Vec<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CreateSessionRequest {
    /// What the agent should do. Must not be blank.
    pub prompt: String,
    /// Shown in lists. Derived from the prompt when absent.
    #[serde(default)]
    #[ts(optional)]
    pub title: Option<String>,
    /// Runner id. The server's default runner when absent.
    #[serde(default)]
    #[ts(optional)]
    pub runner: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct SendMessageRequest {
    /// Must not be blank.
    pub text: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct SessionDetail {
    pub session: Session,
    pub agents: Vec<Agent>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ApiError {
    pub error: String,
}
