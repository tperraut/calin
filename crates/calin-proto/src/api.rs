//! REST bodies. Every route lives under `/api/v1`.
//!
//! | Method | Path | Request | Response |
//! | --- | --- | --- | --- |
//! | GET | `/health` | | [`Health`] |
//! | GET | `/repos` | | `Vec<Repo>`, by name |
//! | POST | `/repos` | [`RegisterRepoRequest`] | `201` [`Repo`] |
//! | DELETE | `/repos/{id}` | | `204`; removes the registry entry only |
//! | GET | `/sessions` | | `Vec<Session>`, newest first |
//! | POST | `/sessions` | [`CreateSessionRequest`] | `201` [`SessionDetail`] |
//! | GET | `/sessions/{id}` | | [`SessionDetail`] |
//! | POST | `/sessions/{id}/messages` | [`SendMessageRequest`] | `202`, empty body |
//! | POST | `/sessions/{id}/interrupt` | | `202`, empty body |
//! | POST | `/sessions/{id}/stop` | | `202`, empty body |
//! | POST | `/sessions/{id}/permissions/{request_id}` | [`ResolvePermissionRequest`] | `202`, empty body |
//! | GET | `/sessions/{id}/changes` | | `Vec<RepoChanges>`, one per attached repo |
//! | GET | `/sessions/{id}/repos/{session_repo_id}/diff?path=` | | [`FileDiff`] |
//! | GET | `/sessions/{id}/repos/{session_repo_id}/tree?path=` | | `Vec<TreeEntry>`, one level |
//! | GET | `/sessions/{id}/repos/{session_repo_id}/file?path=` | | [`FileContent`] |
//! | PUT | `/sessions/{id}/repos/{session_repo_id}/file?path=` | [`WriteFileRequest`] | [`FileContent`] |
//! | GET | `/sessions/{id}/terminals/ws` | WebSocket upgrade | see `terminal.rs` |
//! | GET | `/ws` | WebSocket upgrade | frames of `ClientMsg` / `ServerMsg` |
//!
//! Errors are an [`ApiError`] with status 400 (invalid input), 404 (unknown
//! session, repo, request or path) or 409 (not possible in the current state).
//!
//! ## Semantics
//!
//! - **Messages** are accepted while the session is `running` (delivered after
//!   the current turn), `review`, `waiting`, `failed` or `cancelled`. For the
//!   last two the lead agent is restarted and continues its conversation. A
//!   `done` session answers 409.
//! - **Interrupt** ends the current turn and leaves the session in `review`.
//!   409 unless the session is `running` or `waiting`.
//! - **Stop** ends the agent and sets `cancelled`. 409 once the session is
//!   `done`, `failed` or `cancelled`.
//! - **Permissions**: 404 for an unknown `request_id`, 409 when it was already answered.
//! - **`path` query parameters** are relative to the repo root, use `/`, and
//!   must stay inside it: an absolute path, a `..` segment, or a symlink that
//!   leads outside is a 400. An empty or absent `path` on `tree` lists the root.
//!   `.git` is never listed, read or written.
//! - **Writing a file** to a `read` repo is a 409.
//! - **Registering a repo**: the path must be the root of a git work tree
//!   (400 otherwise); a name already in use is a 409. A repo attached to a
//!   session that is not `done`, `failed` or `cancelled` cannot be deleted (409).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Agent, AttachRepoRequest, PermissionMode, Session, SessionRepo};

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
    /// Model name passed to the runner. The runner's own default when absent.
    #[serde(default)]
    #[ts(optional)]
    pub model: Option<String>,
    /// `ask` when absent.
    #[serde(default)]
    #[ts(optional)]
    pub permission_mode: Option<PermissionMode>,
    /// Repos to attach, in order. The first one is the primary repo: the
    /// agent's working directory. With none, the agent works in an empty
    /// folder inside the session's own directory.
    #[serde(default)]
    pub repos: Vec<AttachRepoRequest>,
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
    /// The session's repo set, the primary repo first.
    #[serde(default)]
    pub repos: Vec<SessionRepo>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ApiError {
    pub error: String,
}
