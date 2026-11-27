use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    Agent, AgentStatus, DecisionSource, FileOp, PermissionDecision, PermissionRequest, PullRequest,
    SessionRepo, SessionStatus,
};

/// One entry in a session's append-only log.
///
/// `seq` starts at 1 and increases by one per event within a session. Clients
/// remember the last `seq` they applied and ask for everything after it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Event {
    /// ULID, unique across sessions and sortable by time.
    pub id: String,
    pub session_id: String,
    #[ts(type = "number")]
    pub seq: u64,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub ts: i64,
    /// The agent that emitted the event; `None` for the user and the orchestrator.
    pub agent_id: Option<String>,
    /// The event that caused this one, when there is one. This is what the
    /// graph views are drawn from.
    pub parent_id: Option<String>,
    pub body: EventBody,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export)]
pub enum EventBody {
    /// The session moved to another lifecycle state.
    SessionStatus {
        status: SessionStatus,
        reason: Option<String>,
    },
    /// The user sent text to the session's lead agent.
    UserMessage {
        text: String,
    },
    /// A new agent joined the session.
    AgentSpawned {
        agent: Agent,
    },
    AgentStatus {
        status: AgentStatus,
    },
    /// The runner reported its own conversation id (and the model in use) for
    /// the emitting agent. Recorded so the agent can be resumed after a restart.
    AgentSession {
        native_session_id: String,
        model: Option<String>,
    },
    /// An item (text, reasoning or tool call) began. Deltas for it follow.
    ItemStarted {
        item: Item,
    },
    ItemDelta {
        item_id: String,
        delta: ItemDelta,
    },
    /// The final state of an item. It replaces whatever the client accumulated.
    ItemCompleted {
        item: Item,
    },
    /// The agent's plan. Always the full list, never a patch.
    PlanUpdated {
        entries: Vec<PlanEntry>,
    },
    /// Running totals for the emitting agent.
    Usage {
        cost_usd: f64,
        #[ts(type = "number")]
        tokens_in: u64,
        #[ts(type = "number")]
        tokens_out: u64,
    },
    /// A repo joined the session's repo set.
    RepoAttached {
        repo: SessionRepo,
    },
    /// An agent asked to use a tool and waits for a decision.
    PermissionRequested {
        request: PermissionRequest,
    },
    /// A permission request was answered. `agent_id` is the agent that asked.
    PermissionResolved {
        request_id: String,
        decision: PermissionDecision,
        by: DecisionSource,
        message: Option<String>,
    },
    /// A file in one of the session's repos changed. Emitted when an edit tool
    /// call completes (`agent_id` is the agent) and when the user saves a file
    /// in the editor (`agent_id` is absent).
    FileChanged {
        /// Absent when the path is outside every attached repo.
        session_repo_id: Option<String>,
        /// Relative to the repo root when `session_repo_id` is set, absolute otherwise.
        path: String,
        op: FileOp,
    },
    /// A pull request was opened for one of the session's repos, or an
    /// existing one for its branch was found.
    PullRequestOpened {
        pull_request: PullRequest,
    },
    /// The pull request changed: its state, checks or review. Always the full value.
    PullRequestUpdated {
        pull_request: PullRequest,
    },
    /// Something went wrong that the user should see in the transcript.
    Error {
        message: String,
    },
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ItemStatus {
    Running,
    Completed,
    Failed,
    /// A tool call the user or the policy refused.
    Declined,
}

/// A piece of an agent's turn with a stable id and a lifecycle.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Item {
    pub id: String,
    pub status: ItemStatus,
    pub content: ItemContent,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum ItemContent {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Tool {
        /// The tool's own name, for example `Bash` or `Edit`.
        name: String,
        tool_kind: ToolKind,
        /// One line describing this call, for example the command or the file path.
        title: String,
        input: serde_json::Value,
        output: Option<String>,
        /// Absolute paths of the files this call reads or writes, when known.
        #[serde(default)]
        locations: Vec<String>,
    },
}

/// What a tool does, in the vocabulary of the Agent Client Protocol.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    /// Delegation to another agent.
    Task,
    Other,
}

/// A chunk appended to a running item.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum ItemDelta {
    /// Appends to a `Text` item.
    Text { text: String },
    /// Appends to a `Reasoning` item.
    Reasoning { text: String },
    /// Appends to a `Tool` item's output.
    Output { text: String },
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
}
