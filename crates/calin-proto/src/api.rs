//! REST bodies. Every route lives under `/api/v1`.
//!
//! | Method | Path | Request | Response |
//! | --- | --- | --- | --- |
//! | GET | `/health` | | [`Health`] |
//! | GET | `/settings` | | [`Settings`] |
//! | PUT | `/settings` | [`Settings`] | [`Settings`], as stored |
//! | GET | `/repos` | | `Vec<Repo>`: the home repo first, then by name |
//! | POST | `/repos` | [`RegisterRepoRequest`] | `201` [`Repo`] |
//! | POST | `/repos/refresh` | | `Vec<Repo>`, after re-reading the home repo and rescanning |
//! | POST | `/repos/{id}/clone` | | `202`; the repo reports `cloning` until it is `available` |
//! | DELETE | `/repos/{id}` | | `204`; removes the registry entry only |
//! | GET | `/repo-sets` | | `Vec<RepoSet>`, in the home repo's order |
//! | GET | `/sessions` | | `Vec<Session>`, newest first |
//! | POST | `/sessions` | [`CreateSessionRequest`] | `201` [`SessionDetail`] |
//! | GET | `/sessions/{id}` | | [`SessionDetail`] |
//! | POST | `/sessions/{id}/messages` | [`SendMessageRequest`] | `202`, empty body |
//! | POST | `/sessions/{id}/interrupt` | | `202`, empty body |
//! | POST | `/sessions/{id}/stop` | | `202`, empty body |
//! | POST | `/sessions/{id}/permissions/{request_id}` | [`ResolvePermissionRequest`] | `202`, empty body |
//! | POST | `/sessions/{id}/repos` | [`AttachRepoRequest`] | `201` [`SessionRepo`] |
//! | GET | `/sessions/{id}/changes` | | `Vec<RepoChanges>`, one per attached repo |
//! | GET | `/sessions/{id}/repos/{session_repo_id}/diff?path=` | | [`FileDiff`] |
//! | GET | `/sessions/{id}/repos/{session_repo_id}/tree?path=` | | `Vec<TreeEntry>`, one level |
//! | GET | `/sessions/{id}/repos/{session_repo_id}/file?path=` | | [`FileContent`] |
//! | PUT | `/sessions/{id}/repos/{session_repo_id}/file?path=` | [`WriteFileRequest`] | [`FileContent`] |
//! | GET | `/sessions/{id}/terminals/ws` | WebSocket upgrade | see `terminal.rs` |
//! | GET | `/ws` | WebSocket upgrade | frames of `ClientMsg` / `ServerMsg` |
//! | POST | `/mcp/{token}` | JSON-RPC 2.0 | the tool endpoint agents call; see below |
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
//! - **Settings**: `PUT` replaces the stored settings. `home_repo` must be the
//!   root of a git work tree and each scan root an existing directory (400).
//!   Changing them re-reads the home repo's `.calin/repos.yaml` and rescans.
//! - **The repo list** is the union of repos registered by hand and repos the
//!   home repo declares. A declared repo is matched to a local clone by its
//!   git URL, searched under the scan roots; with no match it is listed with
//!   `available: false`. The home repo itself is listed with `home: true`.
//!   A missing or invalid `repos.yaml` never fails a request: the list then
//!   holds only the hand-registered repos, and the home repo's `problem` says why.
//! - **Cloning** is only possible for a declared repo that is not available
//!   and has a `url` (409 otherwise). A declared repo cannot be deleted (409);
//!   it disappears when the home repo stops declaring it.
//! - **Attaching mid-session**: allowed while the session is `running`,
//!   `waiting` or `review`. 409 otherwise, when the repo is already attached,
//!   or when it is not available. The repo is ready when the response arrives,
//!   and a `repo_attached` event is appended (its `agent_id` is set when an
//!   agent asked for it through the tool endpoint, absent when the user did).
//! - **The tool endpoint** speaks the Model Context Protocol over HTTP
//!   (JSON-RPC 2.0, one request per POST, JSON response). `{token}` identifies
//!   one agent of one session and is only ever given to that agent. It offers
//!   `list_repos` (every repo, with whether it is attached and available) and
//!   `attach_repo` (`name`, optional `mode` of `write` or `read`), which
//!   returns the path the agent can now work in. An unknown token is a 404.
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
