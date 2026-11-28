//! The typed workflow model (spec sections 2 and 3).

use serde::{Deserialize, Serialize};

use crate::problem::Problems;
use crate::template::{Segment, Template};

/// Ports where taking an unwired edge ends the run as failed (spec section 2). Decided by name
/// alone, so an agent verdict called `reject` is a failure port too.
pub const FAILURE_PORTS: [&str; 4] = ["failed", "fail", "exhausted", "reject"];

/// Whether leaving through an unwired port called `port` fails the run.
pub fn is_failure_port(port: &str) -> bool {
    FAILURE_PORTS.contains(&port)
}

/// The only `version` the format has.
pub const FORMAT_VERSION: u32 = 1;

/// Longest node id allowed.
pub const MAX_ID_LEN: usize = 40;

/// A validated workflow. It can only be obtained from [`crate::parse`], [`crate::quick`] or by
/// deserializing (which validates again), so the engine can rely on every rule of the spec.
///
/// Serializes as a plain JSON-friendly structure (not the YAML file): `id`, `version`, `name`,
/// `description`, `budget_usd`, `nodes` (each `{ id, type, ...fields }`) and `edges` (each
/// `{ from, port, to }` with the port always explicit).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "WorkflowRepr", into = "WorkflowRepr")]
pub struct Workflow {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) description: Option<String>,
    pub(crate) budget_usd: Option<f64>,
    pub(crate) nodes: Vec<Node>,
    pub(crate) edges: Vec<Edge>,
}

impl Workflow {
    /// The id: the file name without extension, or `quick`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The display name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The optional one-line description.
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// The optional budget in US dollars. The engine ignores it; the orchestrator enforces it.
    pub fn budget_usd(&self) -> Option<f64> {
        self.budget_usd
    }

    /// Nodes in file order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// Edges in file order, with ports resolved.
    pub fn edges(&self) -> &[Edge] {
        &self.edges
    }

    /// The node called `id`.
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The edge leaving `node` through `port`, with its index, if that port is wired.
    pub fn edge_from(&self, node: &str, port: &str) -> Option<(usize, &Edge)> {
        self.edges
            .iter()
            .enumerate()
            .find(|(_, e)| e.from == node && e.port == port)
    }

    /// The `start` node. Validation guarantees exactly one.
    pub fn start_node(&self) -> Option<&Node> {
        self.nodes
            .iter()
            .find(|n| matches!(n.kind, NodeKind::Start))
    }

    /// The graph for display: nodes with a short label and their ports, then edges, both in
    /// file order.
    pub fn graph(&self) -> Graph {
        Graph {
            nodes: self
                .nodes
                .iter()
                .map(|n| GraphNode {
                    id: n.id.clone(),
                    node_type: n.node_type(),
                    label: n.label(),
                    ports: n.ports(),
                })
                .collect(),
            edges: self.edges.clone(),
        }
    }
}

/// A node: an id and what it does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// Matches `[a-z][a-z0-9-]*`, at most 40 characters, unique in the workflow.
    pub id: String,
    /// The type and its fields.
    #[serde(flatten)]
    pub kind: NodeKind,
}

impl Node {
    /// The node's type.
    pub fn node_type(&self) -> NodeType {
        self.kind.node_type()
    }

    /// The node's ports, in a stable order.
    pub fn ports(&self) -> Vec<Port> {
        self.kind.ports()
    }

    /// Whether `port` is one of this node's ports.
    pub fn has_port(&self, port: &str) -> bool {
        self.kind.port_names().any(|p| p == port)
    }

    /// The outputs this node records.
    pub fn outputs(&self) -> &'static [&'static str] {
        self.node_type().outputs()
    }

    /// Every template field of the node with its key, branch prompts as `branches[<id>].prompt`.
    pub fn templates(&self) -> Vec<(String, &Template)> {
        if let NodeKind::Parallel { branches } = &self.kind {
            return branches
                .iter()
                .map(|b| (format!("branches[{}].prompt", b.id), &b.prompt))
                .collect();
        }
        self.kind
            .main_template()
            .map(|(key, t)| (key.to_owned(), t))
            .into_iter()
            .collect()
    }

    /// A short label for a picture of the graph: the first line of the node's main text,
    /// at most 60 characters.
    pub fn label(&self) -> String {
        let text = match (&self.kind, self.kind.main_template()) {
            (_, Some((_, template))) => template.source().to_owned(),
            (NodeKind::Start, None) => "Start".to_owned(),
            (NodeKind::End { status }, None) => format!("End: {}", status.as_str()),
            (NodeKind::Loop { max }, None) => format!("Up to {max} times"),
            (NodeKind::Parallel { branches }, None) => format!("{} branches", branches.len()),
            (NodeKind::Check(_), None) => "Repository checks".to_owned(),
            // A `ship` node without a title: every other type always has its main text.
            (_, None) => "Open pull requests".to_owned(),
        };
        shorten(&text, 60)
    }
}

fn shorten(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() <= max {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// The type of a node and its fields (spec section 3). Serialized with a `type` tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NodeKind {
    /// Where the run begins.
    Start,
    /// Ends the run with `status`.
    End {
        /// The run's final status.
        status: EndStatus,
    },
    /// An agent turn.
    Agent(AgentNode),
    /// Shell commands in worktrees.
    Check(CheckNode),
    /// A counter that bounds cycles.
    Loop {
        /// 1 to 20.
        max: u32,
    },
    /// The user approves or rejects.
    Gate {
        /// Shown to the user.
        message: Template,
    },
    /// The user answers a question.
    Ask {
        /// Shown to the user.
        question: Template,
        /// Suggested answers, if any. Never an empty list.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        options: Option<Vec<String>>,
    },
    /// Child sessions run together.
    Parallel {
        /// 2 to 4 branches with unique ids.
        branches: Vec<Branch>,
    },
    /// Commits uncommitted work.
    Commit {
        /// The commit message.
        message: Template,
    },
    /// Opens or updates pull requests.
    Ship {
        /// Pull request title; the orchestrator's default when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<Template>,
        /// Open as drafts. Default true.
        draft: bool,
    },
}

/// Fields of an `agent` node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentNode {
    /// What the agent is asked.
    pub prompt: Template,
    /// Run in a new child session instead of the lead agent.
    #[serde(default)]
    pub fresh: bool,
    /// When non-empty, the node leaves through the port named by the agent's verdict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verdicts: Vec<String>,
    /// Runner for the child session; only with `fresh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Model for the child session; only with `fresh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// Fields of a `check` node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckNode {
    /// A command to run; each changed repo's own checks when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Template>,
    /// The repo whose worktree runs `command`; the primary repo when absent. Only with `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// Default 600.
    pub timeout_seconds: u64,
}

/// One branch of a `parallel` node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Branch {
    /// Unique within the node; same rule as node ids.
    pub id: String,
    /// The child's prompt.
    pub prompt: Template,
    /// Repos the child works in; the session's when absent. Never an empty list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
}

/// How an `end` node, or a run, finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndStatus {
    /// The default.
    Success,
    /// The run failed.
    Failed,
}

impl EndStatus {
    /// `success` or `failed`.
    pub fn as_str(self) -> &'static str {
        match self {
            EndStatus::Success => "success",
            EndStatus::Failed => "failed",
        }
    }
}

/// The ten node types, without their fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeType {
    Start,
    End,
    Agent,
    Check,
    Loop,
    Gate,
    Ask,
    Parallel,
    Commit,
    Ship,
}

impl NodeType {
    /// Every type, in the order of the spec's table.
    pub const ALL: [NodeType; 10] = [
        NodeType::Start,
        NodeType::End,
        NodeType::Agent,
        NodeType::Check,
        NodeType::Loop,
        NodeType::Gate,
        NodeType::Ask,
        NodeType::Parallel,
        NodeType::Commit,
        NodeType::Ship,
    ];

    /// The name used in files.
    pub fn as_str(self) -> &'static str {
        match self {
            NodeType::Start => "start",
            NodeType::End => "end",
            NodeType::Agent => "agent",
            NodeType::Check => "check",
            NodeType::Loop => "loop",
            NodeType::Gate => "gate",
            NodeType::Ask => "ask",
            NodeType::Parallel => "parallel",
            NodeType::Commit => "commit",
            NodeType::Ship => "ship",
        }
    }

    /// The type named `name` in a file.
    pub fn from_name(name: &str) -> Option<NodeType> {
        NodeType::ALL.into_iter().find(|t| t.as_str() == name)
    }

    /// The keys a node of this type may have besides `id` and `type`.
    pub fn fields(self) -> &'static [&'static str] {
        match self {
            NodeType::Start => &[],
            NodeType::End => &["status"],
            NodeType::Agent => &["prompt", "fresh", "verdicts", "runner", "model"],
            NodeType::Check => &["command", "repo", "timeout_seconds"],
            NodeType::Loop => &["max"],
            NodeType::Gate => &["message"],
            NodeType::Ask => &["question", "options"],
            NodeType::Parallel => &["branches"],
            NodeType::Commit => &["message"],
            NodeType::Ship => &["title", "draft"],
        }
    }

    /// The outputs a node of this type records.
    pub fn outputs(self) -> &'static [&'static str] {
        match self {
            NodeType::Start | NodeType::End | NodeType::Commit => &[],
            NodeType::Agent => &["summary", "verdict"],
            NodeType::Check => &["output", "exit_code"],
            NodeType::Loop => &["iteration", "max"],
            NodeType::Gate => &["comment"],
            NodeType::Ask => &["answer"],
            NodeType::Parallel => &["summary"],
            NodeType::Ship => &["urls"],
        }
    }

    /// The ports of a node of this type. For `agent` this is the no-verdict form; an agent
    /// with verdicts has one port per verdict plus `failed` (see [`NodeKind::ports`]).
    pub fn default_ports(self) -> &'static [&'static str] {
        match self {
            NodeType::Start => &["next"],
            NodeType::End => &[],
            NodeType::Agent | NodeType::Parallel | NodeType::Ship => &["done", "failed"],
            NodeType::Check => &["pass", "fail"],
            NodeType::Loop => &["repeat", "exhausted"],
            NodeType::Gate => &["approve", "reject"],
            NodeType::Ask => &["answered"],
            NodeType::Commit => &["done", "nothing", "failed"],
        }
    }
}

/// An output port of a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Port {
    /// The name used in edges (`node.port`) and results.
    pub name: String,
    /// Whether leaving through it unwired fails the run.
    pub failure: bool,
}

impl NodeKind {
    /// The type without fields.
    pub fn node_type(&self) -> NodeType {
        match self {
            NodeKind::Start => NodeType::Start,
            NodeKind::End { .. } => NodeType::End,
            NodeKind::Agent(_) => NodeType::Agent,
            NodeKind::Check(_) => NodeType::Check,
            NodeKind::Loop { .. } => NodeType::Loop,
            NodeKind::Gate { .. } => NodeType::Gate,
            NodeKind::Ask { .. } => NodeType::Ask,
            NodeKind::Parallel { .. } => NodeType::Parallel,
            NodeKind::Commit { .. } => NodeType::Commit,
            NodeKind::Ship { .. } => NodeType::Ship,
        }
    }

    /// The node's one main template with its key, when it has one; a `parallel` node's
    /// prompts are per branch, so it has none.
    fn main_template(&self) -> Option<(&'static str, &Template)> {
        match self {
            NodeKind::Agent(a) => Some(("prompt", &a.prompt)),
            NodeKind::Check(c) => c.command.as_ref().map(|t| ("command", t)),
            NodeKind::Gate { message } | NodeKind::Commit { message } => Some(("message", message)),
            NodeKind::Ask { question, .. } => Some(("question", question)),
            NodeKind::Ship { title, .. } => title.as_ref().map(|t| ("title", t)),
            NodeKind::Start
            | NodeKind::End { .. }
            | NodeKind::Loop { .. }
            | NodeKind::Parallel { .. } => None,
        }
    }

    /// Port names in order: verdicts then `failed` for an agent with verdicts, else the type's.
    pub fn port_names(&self) -> impl Iterator<Item = &str> {
        let (verdicts, fixed): (&[String], &'static [&'static str]) = match self {
            NodeKind::Agent(a) if !a.verdicts.is_empty() => (&a.verdicts, &["failed"]),
            other => (&[], other.node_type().default_ports()),
        };
        verdicts
            .iter()
            .map(String::as_str)
            .chain(fixed.iter().copied())
    }

    /// The ports, with their failure flag.
    pub fn ports(&self) -> Vec<Port> {
        self.port_names()
            .map(|name| Port {
                name: name.to_owned(),
                failure: is_failure_port(name),
            })
            .collect()
    }
}

/// An edge from a node's port to a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    /// The source node id.
    pub from: String,
    /// The source port, resolved even when the file wrote only the node.
    pub port: String,
    /// The target node id.
    pub to: String,
}

/// The graph for display (spec section 6): what a UI draws, independent of the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Graph {
    /// In file order.
    pub nodes: Vec<GraphNode>,
    /// In file order.
    pub edges: Vec<Edge>,
}

/// A node for display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphNode {
    /// The node id.
    pub id: String,
    /// The node type.
    #[serde(rename = "type")]
    pub node_type: NodeType,
    /// A short one-line label.
    pub label: String,
    /// The node's ports, in order.
    pub ports: Vec<Port>,
}

/// The serialized shape of a [`Workflow`]; deserializing it validates again.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WorkflowRepr {
    pub id: String,
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_usd: Option<f64>,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
}

impl From<Workflow> for WorkflowRepr {
    fn from(w: Workflow) -> Self {
        WorkflowRepr {
            id: w.id,
            version: FORMAT_VERSION,
            name: w.name,
            description: w.description,
            budget_usd: w.budget_usd,
            nodes: w.nodes,
            edges: w.edges,
        }
    }
}

impl TryFrom<WorkflowRepr> for Workflow {
    type Error = Problems;
    fn try_from(r: WorkflowRepr) -> Result<Self, Problems> {
        crate::validate::validate_repr(r)
    }
}

/// The built-in `quick` workflow: one agent turn on the task, then the run ends (spec 1).
pub fn quick() -> Workflow {
    Workflow {
        id: "quick".to_owned(),
        name: "Quick".to_owned(),
        description: Some("One agent turn on the task.".to_owned()),
        budget_usd: None,
        nodes: vec![
            Node {
                id: "start".to_owned(),
                kind: NodeKind::Start,
            },
            Node {
                id: "agent".to_owned(),
                kind: NodeKind::Agent(AgentNode {
                    prompt: Template::from_parts("{{task}}", vec![Segment::Task]),
                    fresh: false,
                    verdicts: Vec::new(),
                    runner: None,
                    model: None,
                }),
            },
            Node {
                id: "end".to_owned(),
                kind: NodeKind::End {
                    status: EndStatus::Success,
                },
            },
        ],
        edges: vec![
            Edge {
                from: "start".to_owned(),
                port: "next".to_owned(),
                to: "agent".to_owned(),
            },
            Edge {
                from: "agent".to_owned(),
                port: "done".to_owned(),
                to: "end".to_owned(),
            },
        ],
    }
}
