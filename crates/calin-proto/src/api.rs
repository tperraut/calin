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
//! | POST | `/sessions/{id}/questions/{question_id}` | [`AnswerQuestionRequest`] | `202`, empty body |
//! | GET | `/sessions/{id}/graph?at=` | | [`Graph`] of the session's whole tree |
//! | GET | `/sessions/{id}/timeline` | | [`Timeline`] of the session's whole tree |
//! | POST | `/sessions/{id}/repos` | [`AttachRepoRequest`] | `201` [`SessionRepo`] |
//! | POST | `/sessions/{id}/ship` | [`ShipRequest`] | `202`, empty body |
//! | POST | `/sessions/{id}/pull-requests/refresh` | | `202`, empty body |
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
//! - **Shipping** is accepted while the session is `review` or `shipping`
//!   (409 otherwise, and 409 with the reason when GitHub cannot be used: see
//!   `Health::github_problem`). It returns at once and works in the background,
//!   repo by repo, over every `write` repo that differs from its base:
//!   uncommitted work is committed on the session branch, the branch is pushed
//!   (never forced, and never when it is the repo's default branch), and a
//!   pull request against the repo's default branch is opened, or the open one
//!   for that branch is reused. Each step that happens is an event:
//!   `pull_request_opened` per repo, then `pull_request_updated` once every
//!   description links its sibling pull requests. A repo that fails gets an
//!   `error` event naming it and the others carry on. When at least one pull
//!   request exists the session becomes `shipping`. `live` and `read` repos
//!   are never committed to, pushed or shipped. Shipping again pushes new
//!   commits to the existing pull requests.
//! - **While `shipping`** the server watches each pull request and appends
//!   `pull_request_updated` when its state, checks or review change. When all
//!   of them are merged or closed the session becomes `done`. A message sent
//!   while `shipping` starts a turn as it does in `review`.
//! - **Dry run** never pushes and never calls GitHub: shipping records
//!   pull requests with made-up URLs under `https://example.invalid/`.
//! - **Subagents.** When an agent starts a subagent inside its own process,
//!   the session gets an `agent_spawned` for it (kind `subagent`, its
//!   `parent_agent_id` set, role from the subagent's type), every event the
//!   subagent produces carries its `agent_id`, and an `agent_status` closes it.
//! - **Child sessions.** An agent can dispatch a child session through the
//!   tool endpoint. The child is an ordinary session with `parent_session_id`
//!   and `root_session_id` set, the same runner and permission mode as its
//!   parent, and its own worktrees: for each repo it is given (a subset of the
//!   parent's `write` repos, all of them by default) a new branch starting at
//!   the parent's current commit in that repo. The brief is the child's
//!   prompt, recorded as a `dispatch` message: `message_sent` in the parent,
//!   `message_received` in the child. When the child reaches `review` or
//!   `failed`, its report (what it passed to the `report` tool, else its last
//!   text) goes back the same way as a `report` message and is delivered to
//!   the dispatching agent as its next input, with the child's branch names.
//!   At most four children of one session run at a time and a child cannot
//!   dispatch (both are tool errors). Stopping a session stops its running
//!   descendants. Children appear in `GET /sessions` like any session.
//! - **Notes.** `send_message` on the tool endpoint sends a `note` to the
//!   parent session's agent, to a child session's agent, or to the user. A
//!   note to an agent is delivered as its next input; a note to the user only
//!   appears in the transcript.
//! - **Questions.** `ask_user` records `question_asked` and the session
//!   becomes `waiting`. The tool call returns at once, telling the agent to
//!   end its turn. The user's answer (`question_answered`) is delivered as the
//!   agent's next input. 404 for an unknown question, 409 once it is answered.
//!   A question still open when the session is stopped is closed as `expired`.
//! - **The tool endpoint** therefore also offers `dispatch` (`title`, `brief`,
//!   optional `repos`), `send_message` (`to`: `parent`, `user` or a child
//!   session id; `text`), `report` (`summary`) and `ask_user` (`question`,
//!   optional `options`).
//! - **Graph and timeline** cover the whole tree of the session asked for.
//!   `at` (Unix milliseconds) asks for the graph as it was at that moment:
//!   only what had happened by then, with each agent's and session's status
//!   as of then. Without `at` it is the present.
//! - **Registering a repo**: the path must be the root of a git work tree
//!   (400 otherwise); a name already in use is a 409. A repo attached to a
//!   session that is not `done`, `failed` or `cancelled` cannot be deleted (409).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Agent, AttachRepoRequest, PermissionMode, PullRequest, Session, SessionRepo};

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
    /// Why pull requests cannot be opened from this server (the `gh` CLI is
    /// missing or not signed in), or absent when they can.
    #[serde(default)]
    pub github_problem: Option<String>,
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
    /// The session's pull requests, in the order of `repos`.
    #[serde(default)]
    pub pull_requests: Vec<PullRequest>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ApiError {
    pub error: String,
}
