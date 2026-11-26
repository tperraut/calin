use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Where a session is in its lifecycle. See the lifecycle diagram in the design spec.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SessionStatus {
    Queued,
    Preparing,
    Running,
    /// Parked on the user: a question, a permission request or a workflow gate.
    Waiting,
    /// The agent finished; the user reads the changes.
    Review,
    /// Pull requests are open and being watched.
    Shipping,
    Done,
    Failed,
    Cancelled,
}

impl SessionStatus {
    /// True while the session holds a queue slot.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Preparing | Self::Running)
    }

    /// True once no agent will run again unless the user continues the session.
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            Self::Review | Self::Shipping | Self::Done | Self::Failed | Self::Cancelled
        )
    }
}

/// Who answers when an agent asks to use a tool.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionMode {
    /// Every request waits for the user.
    #[default]
    Ask,
    /// File edits inside the session's repos are allowed; everything else waits for the user.
    AcceptEdits,
    /// Every request is allowed by policy. Each one is still recorded.
    Autonomous,
}

/// A unit of work started by the user (or dispatched by an agent).
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub prompt: String,
    pub status: SessionStatus,
    /// Runner id of the lead agent, for example `mock` or `claude`.
    pub runner: String,
    /// How tool permission requests are answered. Absent in logs written before M1.
    #[serde(default)]
    pub permission_mode: PermissionMode,
    /// Branch name used in every attached repo. Set once worktrees exist.
    pub branch: Option<String>,
    pub parent_session_id: Option<String>,
    /// Id of the root of the session tree; equals `id` for a root session.
    pub root_session_id: String,
    /// Sequence number of the newest event in this session's log.
    #[ts(type = "number")]
    pub head_seq: u64,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub created_at: i64,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// How an agent exists: its own process, or a subagent reported by a parent process.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentKind {
    Process,
    Subagent,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AgentStatus {
    Starting,
    Running,
    /// Alive and waiting for the next message.
    Idle,
    Done,
    Failed,
    Stopped,
}

/// One agent taking part in a session. Agents are the nodes of the session graph.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Agent {
    pub id: String,
    pub session_id: String,
    /// The agent that spawned this one, if any.
    pub parent_agent_id: Option<String>,
    pub kind: AgentKind,
    /// Free-form role: `lead`, `worker`, `reviewer`, or a subagent type name.
    pub role: String,
    pub runner: String,
    pub model: Option<String>,
    /// The runner's own conversation id, used to resume the agent after a restart.
    #[serde(default)]
    pub native_session_id: Option<String>,
    pub status: AgentStatus,
    pub cost_usd: f64,
    #[ts(type = "number")]
    pub tokens_in: u64,
    #[ts(type = "number")]
    pub tokens_out: u64,
}
