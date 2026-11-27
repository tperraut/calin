//! The orchestration views: who spawned whom, who said what to whom, who
//! changed which repo. Both views cover a whole session tree (a root session
//! and every session dispatched from it), whichever session of the tree is asked.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Agent, ItemStatus, RepoMode, SessionStatus, ToolKind};

/// A message between two participants of a session tree. A participant is an
/// agent, or the user when the agent id is absent.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AgentMessage {
    /// Unique across sessions. The same id appears in the sender's
    /// `message_sent` and the recipient's `message_received`.
    pub id: String,
    pub kind: MessageKind,
    pub from_session_id: String,
    /// Absent when the user sent it.
    pub from_agent_id: Option<String>,
    pub to_session_id: String,
    /// Absent when it is addressed to the user.
    pub to_agent_id: Option<String>,
    pub body: String,
    /// The message this one answers, when there is one.
    pub in_reply_to: Option<String>,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub ts: i64,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum MessageKind {
    /// The brief that starts a child session.
    Dispatch,
    /// Free-form text between agents, or from an agent to the user.
    Note,
    /// A child session's result, delivered to the agent that dispatched it.
    Report,
}

/// A question an agent put to the user. The session waits until it is answered.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Question {
    /// Unique within the session.
    pub id: String,
    pub text: String,
    /// Suggested answers. The user may still answer in free text.
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AnswerQuestionRequest {
    /// Must not be blank.
    pub answer: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NodeKind {
    User,
    Agent,
    Repo,
}

/// A participant in the session graph.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct GraphNode {
    /// Stable across requests: `user`, `agent:<agent id>` or `repo:<repo id>`.
    pub id: String,
    pub kind: NodeKind,
    pub label: String,
    /// The node this one hangs under in the spawn tree: the user for a root
    /// session's lead agent, the dispatching agent for a child session's lead,
    /// the parent agent for a subagent. Absent for the user and for repos.
    pub parent_id: Option<String>,
    /// Set for agent nodes.
    pub agent: Option<Agent>,
    /// The agent's session.
    pub session_id: Option<String>,
    /// The status and title of the agent's session, for a session's lead agent.
    pub session_status: Option<SessionStatus>,
    pub session_title: Option<String>,
    /// Set for repo nodes: the repo's registry id. One node per repo, however
    /// many sessions of the tree attach it.
    pub repo_id: Option<String>,
    /// For repo nodes: the strongest mode any session attached it with.
    pub repo_mode: Option<RepoMode>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum EdgeKind {
    /// An agent started a subagent inside its own process.
    Spawned,
    /// An agent started a child session.
    Dispatched,
    /// Notes between two participants, in this direction.
    Message,
    /// A child session's report to the agent that dispatched it.
    Report,
    /// An agent, or the user, changed files in a repo.
    Changed,
}

/// A relation between two nodes. All events of one kind in one direction
/// between the same two nodes are one edge with a count.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct GraphEdge {
    /// Stable across requests: `<kind>:<from>:<to>`.
    pub id: String,
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
    /// How many events the edge stands for (messages sent, files changed).
    pub count: u32,
    /// Unix time in milliseconds of the newest of them.
    #[ts(type = "number")]
    pub last_ts: i64,
    /// A short excerpt of the newest one: the message's first line, or the file path.
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Graph {
    pub root_session_id: String,
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Time of the tree's first event, in Unix milliseconds.
    #[ts(type = "number")]
    pub from_ts: i64,
    /// Time of the tree's newest event.
    #[ts(type = "number")]
    pub to_ts: i64,
    /// The moment this graph describes: the `at` that was asked for, or `to_ts`.
    #[ts(type = "number")]
    pub at: i64,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum SpanKind {
    /// From a user message (or the start of the agent) to the end of the turn.
    Turn,
    /// One tool call.
    Tool,
    /// The agent's session was waiting on the user: a permission request or a question.
    Waiting,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TimelineSpan {
    pub id: String,
    pub kind: SpanKind,
    pub title: String,
    /// For tool spans.
    pub tool_kind: Option<ToolKind>,
    /// For tool spans.
    pub status: Option<ItemStatus>,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub start: i64,
    /// Absent while the span is still open.
    #[ts(type = "number | null")]
    pub end: Option<i64>,
}

/// One agent's activity over time.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TimelineLane {
    pub agent: Agent,
    pub session_id: String,
    /// The graph node id of the agent's parent, as in [`GraphNode::parent_id`].
    pub parent_id: Option<String>,
    /// Sorted by `start`. Tool and waiting spans lie inside turn spans.
    pub spans: Vec<TimelineSpan>,
}

/// Something that crossed from one lane to another at a moment.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct TimelineArrow {
    pub id: String,
    pub kind: EdgeKind,
    /// Absent when the user is the origin.
    pub from_agent_id: Option<String>,
    /// Absent when the user is the target.
    pub to_agent_id: Option<String>,
    #[ts(type = "number")]
    pub ts: i64,
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Timeline {
    pub root_session_id: String,
    /// In spawn-tree order: a parent before its children, siblings by start time.
    pub lanes: Vec<TimelineLane>,
    /// Sorted by `ts`.
    pub arrows: Vec<TimelineArrow>,
    #[ts(type = "number")]
    pub from_ts: i64,
    #[ts(type = "number")]
    pub to_ts: i64,
}
