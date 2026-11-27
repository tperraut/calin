//! Workflows: the graph that decides what a session does after it starts.
//! The file format and the engine's rules are in
//! `docs/specs/2026-11-27-workflows.md`; these are the shapes a client sees.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// An output port of a workflow node.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowPort {
    pub name: String,
    /// True for ports that end the run as failed when nothing is wired to them.
    pub failure: bool,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowNodeInfo {
    pub id: String,
    /// The node type as written in the file: `start`, `end`, `agent`, `check`,
    /// `loop`, `gate`, `ask`, `parallel`, `commit` or `ship`.
    pub kind: String,
    /// One short line describing the node, for example the start of its prompt
    /// or its command.
    pub label: String,
    pub ports: Vec<WorkflowPort>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowEdge {
    pub from: String,
    pub port: String,
    pub to: String,
}

/// A workflow as a picture: nodes and edges in file order.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowGraph {
    pub nodes: Vec<WorkflowNodeInfo>,
    pub edges: Vec<WorkflowEdge>,
}

/// A workflow that can be chosen for a session, or one that cannot and why.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowInfo {
    /// The file name without its extension, or `quick` for the built-in one.
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub builtin: bool,
    pub budget_usd: Option<f64>,
    /// Absent when the file does not validate.
    pub graph: Option<WorkflowGraph>,
    /// Why the file cannot be used: every problem found, one per line. Absent
    /// for a valid workflow.
    pub problem: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum WorkflowStatus {
    Running,
    Success,
    Failed,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NodeRunState {
    Idle,
    Active,
    Finished,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum NodePhase {
    Started,
    Finished,
}

/// Where one node stands in a run.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowNodeRun {
    pub id: String,
    pub state: NodeRunState,
    /// How many times the node has started.
    pub runs: u32,
    /// The port it last left through.
    pub last_port: Option<String>,
    /// One short line about its last result: a check's failing command, an
    /// agent's verdict, the answer at a gate.
    pub outcome: Option<String>,
    /// For loop nodes: how many times it has been entered, and its limit.
    pub iteration: Option<u32>,
    pub max: Option<u32>,
}

/// A workflow run inside a session.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct WorkflowRun {
    pub workflow_id: String,
    pub name: String,
    /// The graph as it was when the run started.
    pub graph: WorkflowGraph,
    /// One entry per node of `graph`, in the same order.
    pub nodes: Vec<WorkflowNodeRun>,
    /// The edges the run went through, in order. An edge appears once per traversal.
    pub taken: Vec<WorkflowEdge>,
    pub status: WorkflowStatus,
    /// Why it failed, when it did: the port that ended it, the budget, a stop.
    pub reason: Option<String>,
    pub budget_usd: Option<f64>,
    /// What the session tree has cost so far.
    pub cost_usd: f64,
}
