//! The engine (spec section 5): a pure state machine over an owned, serializable [`RunState`].
//!
//! [`start`] and [`advance`] each return the new state and the actions for the orchestrator.
//! `start`, `end` and `loop` nodes are resolved inside the engine; every other node becomes an
//! action and waits for one [`NodeResult`]. Each call currently yields exactly one action:
//! the next node's, or [`Action::Finish`].

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::model::{AgentNode, CheckNode, EndStatus, Node, NodeKind, Workflow, is_failure_port};
use crate::template::{Context, Outputs};
use crate::verdict::verdict_instruction;

/// Most node activations in one run, counting `start`, `loop` and `end`. Validation already
/// makes every run finite; this guard holds even if that reasoning has a hole.
pub const MAX_STEPS: u32 = 1000;

/// What the orchestrator must do next. Templates are already rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// Give an agent a turn. With `verdicts`, `prompt` already ends with
    /// [`verdict_instruction`]; parse the answer with [`crate::extract_verdict`].
    RunAgent {
        /// The node to answer with a result.
        node: String,
        /// Rendered prompt.
        prompt: String,
        /// In a new child session rather than the lead agent.
        fresh: bool,
        /// Possible verdicts; empty for a plain `done`/`failed` node.
        verdicts: Vec<String>,
        /// Child runner, only with `fresh`.
        runner: Option<String>,
        /// Child model, only with `fresh`.
        model: Option<String>,
    },
    /// Run a check.
    RunCheck {
        /// The node to answer with a result.
        node: String,
        /// What to run.
        check: CheckCommand,
        /// Give up after this many seconds.
        timeout_seconds: u64,
    },
    /// Ask the user to approve or reject. The session waits.
    AskGate {
        /// The node to answer with a result.
        node: String,
        /// Rendered message.
        message: String,
    },
    /// Ask the user a question. The session waits.
    AskQuestion {
        /// The node to answer with a result.
        node: String,
        /// Rendered question.
        question: String,
        /// Suggested answers; empty when the node has none.
        options: Vec<String>,
    },
    /// Dispatch one child session per branch and wait for all of them.
    StartParallel {
        /// The node to answer with a result.
        node: String,
        /// The branches, prompts rendered.
        branches: Vec<ParallelBranch>,
    },
    /// Commit uncommitted work in every `write` repo.
    Commit {
        /// The node to answer with a result.
        node: String,
        /// Rendered commit message.
        message: String,
    },
    /// Open or update the session's pull requests.
    Ship {
        /// The node to answer with a result.
        node: String,
        /// Rendered title; the orchestrator's default when absent.
        title: Option<String>,
        /// Open as drafts.
        draft: bool,
    },
    /// The run is over.
    Finish(Finish),
}

impl Action {
    /// The node this action waits on, or `None` for [`Action::Finish`].
    pub fn node(&self) -> Option<&str> {
        match self {
            Action::RunAgent { node, .. }
            | Action::RunCheck { node, .. }
            | Action::AskGate { node, .. }
            | Action::AskQuestion { node, .. }
            | Action::StartParallel { node, .. }
            | Action::Commit { node, .. }
            | Action::Ship { node, .. } => Some(node),
            Action::Finish(_) => None,
        }
    }
}

/// What a check runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckCommand {
    /// A rendered command in `repo`'s worktree, or the primary repo's when `None`.
    Command {
        /// Rendered shell command.
        command: String,
        /// Repo name; the primary repo when absent.
        repo: Option<String>,
    },
    /// Each changed `write` repo's own checks from `repos.yaml` (spec section 3).
    RepoDefaults,
}

/// One branch of a [`Action::StartParallel`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParallelBranch {
    /// The branch id from the file.
    pub id: String,
    /// Rendered prompt.
    pub prompt: String,
    /// Repos for the child; the session's when absent.
    pub repos: Option<Vec<String>>,
}

/// How and why a run finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finish {
    /// The run's status.
    pub status: EndStatus,
    /// Why it finished.
    pub reason: FinishReason,
}

/// Why a run finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FinishReason {
    /// An `end` node was reached.
    EndNode {
        /// The `end` node.
        node: String,
    },
    /// A node left through a port with no outgoing edge (spec section 2).
    UnwiredPort {
        /// The node.
        node: String,
        /// The port taken.
        port: String,
    },
    /// [`MAX_STEPS`] activations were reached.
    StepLimit {
        /// The limit.
        limit: u32,
    },
    /// The workflow contradicted what validation guarantees. Never expected.
    Internal {
        /// What was wrong.
        message: String,
    },
}

impl fmt::Display for FinishReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FinishReason::EndNode { node } => write!(f, "reached end node `{node}`"),
            FinishReason::UnwiredPort { node, port } => write!(
                f,
                "node `{node}` left through `{port}`, which has no outgoing edge"
            ),
            FinishReason::StepLimit { limit } => write!(
                f,
                "stopped after {limit} node activations, the most one run may take"
            ),
            FinishReason::Internal { message } => write!(f, "internal error: {message}"),
        }
    }
}

/// What the orchestrator reports when a node's action is done.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeResult {
    /// The node it answers; must be the active node.
    pub node: String,
    /// The port it leaves through; must be one of the node's ports.
    pub port: String,
    /// Outputs; each key must be an output of the node's type. Missing ones render empty.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

impl NodeResult {
    /// A result with no outputs.
    pub fn new(node: impl Into<String>, port: impl Into<String>) -> Self {
        NodeResult {
            node: node.into(),
            port: port.into(),
            outputs: BTreeMap::new(),
        }
    }

    /// Adds an output.
    pub fn with(mut self, output: impl Into<String>, value: impl Into<String>) -> Self {
        self.outputs.insert(output.into(), value.into());
        self
    }
}

/// Why [`advance`] rejected a result. The state is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EngineError {
    /// The run has finished; it accepts no more results.
    #[error("the run has finished; no node is waiting for a result")]
    Finished,
    /// The result is for another node than the active one.
    #[error("node `{node}` is not active{}", match active {
        Some(a) => format!("; the active node is `{a}`"),
        None => String::new(),
    })]
    NotActive {
        /// The node named by the result.
        node: String,
        /// The node that is active.
        active: Option<String>,
    },
    /// The node has no such port.
    #[error("node `{node}` has no port `{port}`; its ports are {}", ports.join(", "))]
    UnknownPort {
        /// The node.
        node: String,
        /// The port named by the result.
        port: String,
        /// The node's ports.
        ports: Vec<String>,
    },
    /// The node's type declares no such output.
    #[error("node `{node}` has no output `{output}`; its outputs are {}", if outputs.is_empty() { "none".to_owned() } else { outputs.join(", ") })]
    UnknownOutput {
        /// The node.
        node: String,
        /// The output named by the result.
        output: String,
        /// The outputs its type declares.
        outputs: Vec<String>,
    },
}

/// Per-node bookkeeping.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct NodeRun {
    /// Activations.
    runs: u32,
    /// The port of the newest finished activation; `None` for `end` or while first active.
    last_port: Option<String>,
    /// Whether some activation finished (an `end` node finishes without a port).
    finished: bool,
    /// For `loop` nodes: repeats so far. Never reset (spec section 2).
    iteration: u32,
}

/// An edge taken during a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TakenEdge {
    /// Position in [`Workflow::edges`].
    pub edge: usize,
    /// Source node.
    pub from: String,
    /// Source port.
    pub port: String,
    /// Target node.
    pub to: String,
}

/// A run in progress or finished. Owned and serializable both ways, so it can be stored and
/// resumed; the workflow travels with it so a changed file cannot alter a running graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunState {
    workflow: Workflow,
    context: Context,
    steps: u32,
    active: Option<String>,
    finish: Option<Finish>,
    nodes: BTreeMap<String, NodeRun>,
    outputs: Outputs,
    taken: Vec<TakenEdge>,
}

/// Starts a run: resolves `start` and everything up to the first node that needs the
/// orchestrator, and returns its action (or the finish).
pub fn start(workflow: Workflow, context: Context) -> (RunState, Vec<Action>) {
    let mut state = RunState {
        workflow,
        context,
        steps: 0,
        active: None,
        finish: None,
        nodes: BTreeMap::new(),
        outputs: Outputs::new(),
        taken: Vec::new(),
    };
    let action = match state.workflow.start_node().map(|n| n.id.clone()) {
        Some(id) => state.enter(id),
        None => state.finish_with(
            EndStatus::Failed,
            FinishReason::Internal {
                message: "the workflow has no start node".to_owned(),
            },
        ),
    };
    (state, vec![action])
}

/// Applies the result of the active node and runs on to the next action.
///
/// Rejects, with the state untouched, a result for a node that is not active, through a port
/// the node does not have, or with an output its type does not declare.
pub fn advance(
    state: &RunState,
    result: NodeResult,
) -> Result<(RunState, Vec<Action>), EngineError> {
    if state.finish.is_some() {
        return Err(EngineError::Finished);
    }
    let node = match (&state.active, state.workflow.node(&result.node)) {
        (Some(active), Some(node)) if *active == result.node => node,
        _ => {
            return Err(EngineError::NotActive {
                node: result.node,
                active: state.active.clone(),
            });
        }
    };
    if !node.has_port(&result.port) {
        return Err(EngineError::UnknownPort {
            node: result.node,
            port: result.port,
            ports: node.ports().into_iter().map(|p| p.name).collect(),
        });
    }
    let declared = node.outputs();
    if let Some(output) = result
        .outputs
        .keys()
        .find(|k| !declared.contains(&k.as_str()))
    {
        return Err(EngineError::UnknownOutput {
            node: result.node.clone(),
            output: output.clone(),
            outputs: declared.iter().map(|s| (*s).to_owned()).collect(),
        });
    }

    let mut next = state.clone();
    next.active = None;
    next.outputs.record(&result.node, result.outputs);
    let action = next.leave(&result.node, &result.port);
    Ok((next, vec![action]))
}

impl RunState {
    /// The workflow being run.
    pub fn workflow(&self) -> &Workflow {
        &self.workflow
    }

    /// The session context templates render with.
    pub fn context(&self) -> &Context {
        &self.context
    }

    /// The node waiting for a result, if any.
    pub fn active_node(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// How the run finished, once it has.
    pub fn finished(&self) -> Option<&Finish> {
        self.finish.as_ref()
    }

    /// Node activations so far.
    pub fn steps(&self) -> u32 {
        self.steps
    }

    /// Outputs recorded so far.
    pub fn outputs(&self) -> &Outputs {
        &self.outputs
    }

    /// Edges taken so far, in order.
    pub fn taken_edges(&self) -> &[TakenEdge] {
        &self.taken
    }

    /// The action the run is waiting on, rendered again from the current state: the active
    /// node's action, or the finish. Lets an orchestrator re-issue it after a restart.
    pub fn pending_action(&self) -> Option<Action> {
        if let Some(finish) = &self.finish {
            return Some(Action::Finish(finish.clone()));
        }
        let node = self.workflow.node(self.active.as_deref()?)?;
        self.action_for(node)
    }

    /// Per node, in file order: idle, active, or finished and through which port; how many
    /// times it ran; each loop's iteration. Plus the edges taken so far.
    pub fn snapshot(&self) -> RunSnapshot {
        let nodes = self
            .workflow
            .nodes()
            .iter()
            .map(|n| {
                let run = self.nodes.get(&n.id);
                let state = if self.active.as_deref() == Some(n.id.as_str()) {
                    NodeState::Active
                } else if let Some(run) = run.filter(|r| r.finished) {
                    NodeState::Finished {
                        port: run.last_port.clone(),
                    }
                } else {
                    NodeState::Idle
                };
                NodeSnapshot {
                    id: n.id.clone(),
                    state,
                    runs: run.map_or(0, |r| r.runs),
                    iteration: matches!(n.kind, NodeKind::Loop { .. })
                        .then(|| run.map_or(0, |r| r.iteration)),
                }
            })
            .collect();
        RunSnapshot {
            nodes,
            taken_edges: self.taken.clone(),
            finish: self.finish.clone(),
            steps: self.steps,
        }
    }

    fn finish_with(&mut self, status: EndStatus, reason: FinishReason) -> Action {
        let finish = Finish { status, reason };
        self.active = None;
        self.finish = Some(finish.clone());
        Action::Finish(finish)
    }

    /// Marks `node` finished through `port` and follows the port to the next action.
    fn leave(&mut self, node: &str, port: &str) -> Action {
        match self.follow(node, port) {
            Ok(next) => self.enter(next),
            Err(finish) => finish,
        }
    }

    /// Marks `node` finished through `port`; returns the node the port leads to, or the
    /// finish when the port is unwired.
    fn follow(&mut self, node: &str, port: &str) -> Result<String, Action> {
        let run = self.nodes.entry(node.to_owned()).or_default();
        run.finished = true;
        run.last_port = Some(port.to_owned());
        if let Some((index, edge)) = self.workflow.edge_from(node, port) {
            let to = edge.to.clone();
            self.taken.push(TakenEdge {
                edge: index,
                from: node.to_owned(),
                port: port.to_owned(),
                to: to.clone(),
            });
            return Ok(to);
        }
        let status = if is_failure_port(port) {
            EndStatus::Failed
        } else {
            EndStatus::Success
        };
        Err(self.finish_with(
            status,
            FinishReason::UnwiredPort {
                node: node.to_owned(),
                port: port.to_owned(),
            },
        ))
    }

    /// Activates `id` and runs through nodes the engine resolves itself until one needs the
    /// orchestrator or the run ends. Iterative, so a long chain cannot grow the stack.
    fn enter(&mut self, id: String) -> Action {
        let mut id = id;
        loop {
            if self.steps >= MAX_STEPS {
                return self.finish_with(
                    EndStatus::Failed,
                    FinishReason::StepLimit { limit: MAX_STEPS },
                );
            }
            let Some(node) = self.workflow.node(&id).cloned() else {
                return self.finish_with(
                    EndStatus::Failed,
                    FinishReason::Internal {
                        message: format!("an edge leads to unknown node `{id}`"),
                    },
                );
            };
            self.steps += 1;
            let run = self.nodes.entry(id.clone()).or_default();
            run.runs += 1;
            let port = match &node.kind {
                NodeKind::Start => "next",
                NodeKind::End { status } => {
                    run.finished = true;
                    run.last_port = None;
                    return self.finish_with(*status, FinishReason::EndNode { node: id });
                }
                NodeKind::Loop { max } => {
                    let port = if run.iteration < *max {
                        run.iteration += 1;
                        "repeat"
                    } else {
                        "exhausted"
                    };
                    let values = [
                        ("iteration".to_owned(), run.iteration.to_string()),
                        ("max".to_owned(), max.to_string()),
                    ];
                    self.outputs.record(&id, values.into());
                    port
                }
                _ => {
                    // The node needs the orchestrator; a previous finish of it stays visible
                    // in `last_port` until this activation finishes.
                    self.active = Some(id.clone());
                    return match self.action_for(&node) {
                        Some(action) => action,
                        None => self.finish_with(
                            EndStatus::Failed,
                            FinishReason::Internal {
                                message: format!("node `{id}` has no action"),
                            },
                        ),
                    };
                }
            };
            match self.follow(&id, port) {
                Ok(next) => id = next,
                Err(finish) => return finish,
            }
        }
    }

    /// The rendered action for a node the orchestrator handles; `None` for the engine's own.
    fn action_for(&self, node: &Node) -> Option<Action> {
        let render = |t: &crate::Template| t.render(&self.context, &self.outputs);
        let id = node.id.clone();
        Some(match &node.kind {
            NodeKind::Start | NodeKind::End { .. } | NodeKind::Loop { .. } => return None,
            NodeKind::Agent(AgentNode {
                prompt,
                fresh,
                verdicts,
                runner,
                model,
            }) => {
                let mut prompt = render(prompt);
                if !verdicts.is_empty() {
                    prompt.push_str("\n\n");
                    prompt.push_str(&verdict_instruction(verdicts));
                }
                Action::RunAgent {
                    node: id,
                    prompt,
                    fresh: *fresh,
                    verdicts: verdicts.clone(),
                    runner: runner.clone(),
                    model: model.clone(),
                }
            }
            NodeKind::Check(CheckNode {
                command,
                repo,
                timeout_seconds,
            }) => Action::RunCheck {
                node: id,
                check: match command {
                    Some(cmd) => CheckCommand::Command {
                        command: render(cmd),
                        repo: repo.clone(),
                    },
                    None => CheckCommand::RepoDefaults,
                },
                timeout_seconds: *timeout_seconds,
            },
            NodeKind::Gate { message } => Action::AskGate {
                node: id,
                message: render(message),
            },
            NodeKind::Ask { question, options } => Action::AskQuestion {
                node: id,
                question: render(question),
                options: options.clone().unwrap_or_default(),
            },
            NodeKind::Parallel { branches } => Action::StartParallel {
                node: id,
                branches: branches
                    .iter()
                    .map(|b| ParallelBranch {
                        id: b.id.clone(),
                        prompt: render(&b.prompt),
                        repos: b.repos.clone(),
                    })
                    .collect(),
            },
            NodeKind::Commit { message } => Action::Commit {
                node: id,
                message: render(message),
            },
            NodeKind::Ship { title, draft } => Action::Ship {
                node: id,
                title: title.as_ref().map(render),
                draft: *draft,
            },
        })
    }
}

/// A run as a UI overlays it on the graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSnapshot {
    /// Every node, in file order.
    pub nodes: Vec<NodeSnapshot>,
    /// Edges taken so far, in order.
    pub taken_edges: Vec<TakenEdge>,
    /// How the run finished, once it has.
    pub finish: Option<Finish>,
    /// Node activations so far.
    pub steps: u32,
}

/// One node of a [`RunSnapshot`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSnapshot {
    /// The node id.
    pub id: String,
    /// Where it stands.
    pub state: NodeState,
    /// Activations so far, including a current one.
    pub runs: u32,
    /// For `loop` nodes, the iteration (0 before the first entry); `None` for other types.
    pub iteration: Option<u32>,
}

/// Where a node stands in a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum NodeState {
    /// Not run yet.
    Idle,
    /// Waiting for its result.
    Active,
    /// Ran at least once; `port` is the newest activation's, `None` for an `end` node.
    Finished {
        /// The port it last left through.
        port: Option<String>,
    },
}
