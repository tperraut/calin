//! A workflow run inside a session (workflows spec, section 6). The engine
//! in `calin-workflow` decides what comes next; this turns each of its
//! actions into work of the session and each piece of finished work into
//! the result the engine waits for.
//!
//! Nothing here calls back into itself: whatever ends a node only leaves its
//! result in [`Run::done`], and the driver feeds it to the engine once the
//! step that produced it is over ([`Driver::advance_run`]).

use std::time::Instant;

use calin_proto::{NodePhase, WorkflowStatus};
use calin_workflow::{
    Action, CheckCommand, Context, EndStatus, Finish, FinishReason, NodeKind, NodeResult,
    ParallelBranch, RunState, Workflow, extract_verdict,
};

use super::*;
use crate::manager::ChildEnd;
use crate::title::excerpt;
use crate::workflows;

/// The reason of `waiting` at a workflow gate.
const WAITING_AT_GATE: &str = "gate";
/// The two answers a gate takes (the "gate" semantics in `calin-proto/src/api.rs`).
const APPROVE: &str = "approve";
const REJECT: &str = "reject";
/// What a node that was cut short says, and the run with it.
const STOPPED: &str = "the session was stopped";

/// A workflow run in progress.
pub(super) struct Run {
    state: RunState,
    budget_usd: Option<f64>,
    /// The `workflow_started` event: the cause of every node's start.
    started_event: String,
    /// The active node's `started` event: the cause of what the node does.
    node_event: Option<String>,
    /// What the active node waits for.
    wait: Wait,
    /// The active node's result, ready for the engine.
    done: Option<Done>,
    /// How many of the engine's taken edges are on record as node events.
    edges_seen: usize,
    /// How often each loop was entered.
    loops: HashMap<String, u32>,
    /// The check or the commit of the active node, at work in the background.
    work: Option<mpsc::Receiver<Worked>>,
    /// Set to end that work early.
    cancel: Arc<AtomicBool>,
    /// A `ship` node left pull requests: the session ends in `shipping`.
    shipped: bool,
    /// What the next lead turn is told first: the branches of the child
    /// sessions that reported since the last one.
    notes: Vec<String>,
}

/// What the active node waits for.
enum Wait {
    /// Nothing: between two nodes, within one step of the driver.
    Nothing,
    /// The lead agent to come to rest.
    Lead {
        node: String,
        verdicts: Vec<String>,
        /// The lead's last text since the node started.
        text: Option<String>,
        /// The node's result, fixed when its own turn ended. Turns on what
        /// the user sent meanwhile may still follow before it is fed.
        captured: Option<Done>,
    },
    /// The reports of the child sessions the node dispatched.
    Children {
        node: String,
        verdicts: Vec<String>,
        parallel: bool,
        children: Vec<ChildWait>,
    },
    Check {
        node: String,
    },
    /// The user's answer at a gate or to an `ask` node.
    Answer {
        node: String,
        question_id: String,
        gate: bool,
    },
    Commit {
        node: String,
    },
    Ship {
        node: String,
    },
}

impl Wait {
    fn node(&self) -> Option<&str> {
        match self {
            Self::Nothing => None,
            Self::Lead { node, .. }
            | Self::Children { node, .. }
            | Self::Check { node }
            | Self::Answer { node, .. }
            | Self::Commit { node }
            | Self::Ship { node } => Some(node),
        }
    }
}

/// A child session a node waits for.
struct ChildWait {
    session_id: String,
    /// The branch of a `parallel` node it runs; the node's id for a fresh agent.
    label: String,
    report: Option<(String, ChildEnd)>,
}

/// What a node asks a child session to do.
struct Brief {
    label: String,
    title: String,
    prompt: String,
    repos: Option<Vec<String>>,
    runner: Option<String>,
    model: Option<String>,
}

/// A node's result and the one line its `finished` event says about it.
pub(super) struct Done {
    result: NodeResult,
    outcome: Option<String>,
}

impl Done {
    fn new(node: &str, port: &str, outcome: impl Into<String>) -> Self {
        Self {
            result: NodeResult::new(node, port),
            outcome: Some(outcome.into()).filter(|line: &String| !line.is_empty()),
        }
    }

    fn with(mut self, output: &str, value: impl Into<String>) -> Self {
        self.result = self.result.with(output, value);
        self
    }
}

/// A step of a node's background work, for the driver to record.
pub(super) enum Worked {
    /// A check command, as the tool item it shows as: started, then ended.
    CheckStarted(Item),
    CheckEnded(Item),
    CheckDone(Done),
    /// The names of the repos a commit was made in, or why one failed.
    Committed(std::result::Result<Vec<String>, String>),
}

/// One command of a check node.
struct CheckJob {
    repo: String,
    dir: PathBuf,
    command: String,
    /// For a repo's own checks: its base and branch. They only run when the
    /// repo differs from its base.
    changed_since: Option<(String, String)>,
}

/// Why a run is ended from outside.
pub(super) enum Abort {
    /// The session tree has cost more than the workflow allows.
    Budget {
        budget: f64,
        spent: f64,
    },
    Stopped,
}

pub(super) async fn next_worked(run: &mut Option<Run>) -> Option<Worked> {
    match run.as_mut().and_then(|run| run.work.as_mut()) {
        Some(work) => work.recv().await,
        None => std::future::pending().await,
    }
}

impl Driver {
    /// True while a run is at a node that is not a turn of the lead: what
    /// arrives for the lead then waits for its next turn to end, or for the
    /// run to (spec section 6).
    pub(super) fn run_holds_lead(&self) -> bool {
        let wait = self.run.as_ref().map(|run| &run.wait);
        wait.is_some_and(|wait| !matches!(wait, Wait::Lead { .. }))
    }

    /// Starts the session's workflow: the run is on record, then its first
    /// node begins. The session holds its queue slot by now.
    pub(super) async fn start_run(&mut self, workflow: Workflow, cause: String) -> Result<()> {
        self.cause = Some(cause);
        if self.status != SessionStatus::Running {
            self.set_status(SessionStatus::Running, None).await?;
        }
        let branch = self.repos.iter().find_map(|repo| repo.branch.clone());
        let context = Context {
            task: self.task(),
            session_title: self.title.clone(),
            session_branch: branch
                .unwrap_or_else(|| calin_git::session_branch_name(&self.title, &self.session_id)),
        };
        let started = EventBody::WorkflowStarted {
            workflow_id: workflow.id().to_owned(),
            name: workflow.name().to_owned(),
            graph: workflows::graph(&workflow),
            budget_usd: workflow.budget_usd(),
        };
        let started = EventDraft::new(started).parent(self.cause.clone());
        let started_event = self.append(started).await?.id.clone();
        let budget_usd = workflow.budget_usd();
        let (state, actions) = calin_workflow::start(workflow, context);
        self.run = Some(Run {
            state,
            budget_usd,
            started_event,
            node_event: None,
            wait: Wait::Nothing,
            done: None,
            edges_seen: 0,
            loops: HashMap::new(),
            work: None,
            cancel: Arc::new(AtomicBool::new(false)),
            shipped: false,
            notes: Vec::new(),
        });
        self.follow(None, actions).await
    }

    /// Feeds the engine every result that is ready, each followed by what
    /// the engine wants next. Called after every step of the driver.
    pub(super) async fn advance_run(&mut self) -> Result<()> {
        loop {
            let Some(run) = &mut self.run else {
                return Ok(());
            };
            let Some(done) = run.done.take() else {
                return Ok(());
            };
            run.wait = Wait::Nothing;
            run.work = None;
            let node = done.result.node.clone();
            let advanced = calin_workflow::advance(&run.state, done.result.clone());
            let actions = match advanced {
                Ok((state, actions)) => {
                    run.state = state;
                    actions
                }
                // The result names a node or a port the engine does not
                // expect: a fault of this file, which must not hang the run.
                Err(err) => {
                    let reason = format!("internal error: {err}");
                    return self.end_run(WorkflowStatus::Failed, Some(reason)).await;
                }
            };
            let port = Some(done.result.port);
            self.node_event(&node, NodePhase::Finished, port, done.outcome)
                .await?;
            self.follow(Some(&node), actions).await?;
        }
    }

    /// Records the nodes the engine went through on its own (`start`, `loop`
    /// and `end`), then begins what it asks for. `from` is the node whose
    /// result led here, already on record.
    async fn follow(&mut self, from: Option<&str>, actions: Vec<Action>) -> Result<()> {
        let Some(run) = &mut self.run else {
            return Ok(());
        };
        let taken = run.state.taken_edges();
        let mut passed: Vec<(String, Option<String>)> = taken[run.edges_seen.min(taken.len())..]
            .iter()
            .filter(|edge| Some(edge.from.as_str()) != from)
            .map(|edge| (edge.from.clone(), Some(edge.port.clone())))
            .collect();
        run.edges_seen = taken.len();
        // The engine gives one action per call.
        let action = actions.into_iter().next();
        if let Some(Action::Finish(finish)) = &action {
            match &finish.reason {
                // A loop left through a port nothing is wired to took no edge.
                FinishReason::UnwiredPort { node, port } if Some(node.as_str()) != from => {
                    passed.push((node.clone(), Some(port.clone())));
                }
                FinishReason::EndNode { node } => passed.push((node.clone(), None)),
                _ => {}
            }
        }
        for (node, port) in passed {
            let outcome = self.loop_outcome(&node, port.as_deref());
            self.node_started(&node, None).await?;
            self.node_event(&node, NodePhase::Finished, port, outcome)
                .await?;
        }
        match action {
            Some(action) => self.begin(action).await,
            None => {
                let reason = "internal error: the engine asked for nothing".to_owned();
                self.end_run(WorkflowStatus::Failed, Some(reason)).await
            }
        }
    }

    /// Counts an entry of a loop node and says where it stands; `None` for
    /// any other node.
    fn loop_outcome(&mut self, node: &str, port: Option<&str>) -> Option<String> {
        let run = self.run.as_mut()?;
        let NodeKind::Loop { max } = run.state.workflow().node(node)?.kind else {
            return None;
        };
        let entries = run.loops.entry(node.to_owned()).or_default();
        *entries += 1;
        Some(match port {
            Some("repeat") => format!("iteration {} of {max}", (*entries).min(max)),
            _ => format!("exhausted after {max}"),
        })
    }

    /// Appends a `workflow_node` event. A start is caused by the run's own
    /// start, an end by the node's start.
    async fn node_event(
        &mut self,
        node: &str,
        phase: NodePhase,
        port: Option<String>,
        outcome: Option<String>,
    ) -> Result<String> {
        let parent = self.run.as_ref().map(|run| match phase {
            NodePhase::Started => run.started_event.clone(),
            NodePhase::Finished => {
                let started = run.node_event.as_ref();
                started.unwrap_or(&run.started_event).clone()
            }
        });
        let body = EventBody::WorkflowNode {
            node_id: node.to_owned(),
            phase,
            port,
            outcome,
        };
        let event = self.append(EventDraft::new(body).parent(parent)).await?;
        Ok(event.id.clone())
    }

    /// Records that a node started, with one line saying on what. What the
    /// node does from here on is caused by this event.
    async fn node_started(&mut self, node: &str, line: Option<String>) -> Result<String> {
        let line = line.filter(|line| !line.is_empty());
        let started = self
            .node_event(node, NodePhase::Started, None, line)
            .await?;
        if let Some(run) = &mut self.run {
            run.node_event = Some(started.clone());
        }
        self.cause = Some(started.clone());
        Ok(started)
    }

    /// Begins what the engine asks for. A node that is over at once leaves
    /// its result for [`Self::advance_run`].
    pub(super) async fn begin(&mut self, action: Action) -> Result<()> {
        let action = match action {
            Action::Finish(finish) => return self.finish_run(finish).await,
            Action::AskGate { node, message } => {
                let options = vec![APPROVE.to_owned(), REJECT.to_owned()];
                return self.begin_question(node, message, options, true).await;
            }
            Action::AskQuestion {
                node,
                question,
                options,
            } => return self.begin_question(node, question, options, false).await,
            work => work,
        };
        // Everything else is work: the session runs, which takes a slot.
        if self.status != SessionStatus::Running {
            if !self.take_slot() {
                return self.park(Resume::Action(Box::new(action))).await;
            }
            self.set_status(SessionStatus::Running, None).await?;
        }
        match action {
            Action::RunAgent {
                node,
                prompt,
                fresh: false,
                verdicts,
                ..
            } => self.begin_lead(node, prompt, verdicts).await,
            Action::RunAgent {
                node,
                prompt,
                verdicts,
                runner,
                model,
                ..
            } => {
                let line = excerpt(&prompt);
                let brief = Brief {
                    label: node.clone(),
                    title: node.clone(),
                    prompt,
                    repos: None,
                    runner,
                    model,
                };
                self.begin_children(node, line, verdicts, false, vec![brief])
                    .await
            }
            Action::StartParallel { node, branches } => {
                let line = format!("{} branches", branches.len());
                let briefs = branches.into_iter().map(|branch| {
                    let ParallelBranch { id, prompt, repos } = branch;
                    Brief {
                        title: format!("{node}: {id}"),
                        label: id,
                        prompt,
                        repos,
                        runner: None,
                        model: None,
                    }
                });
                let briefs = briefs.collect();
                self.begin_children(node, line, Vec::new(), true, briefs)
                    .await
            }
            Action::RunCheck {
                node,
                check,
                timeout_seconds,
            } => self.begin_check(node, check, timeout_seconds).await,
            Action::Commit { node, message } => self.begin_commit(node, message).await,
            Action::Ship { node, title, draft } => self.begin_ship_node(node, title, draft).await,
            // Taken care of above.
            Action::Finish(_) | Action::AskGate { .. } | Action::AskQuestion { .. } => Ok(()),
        }
    }

    fn waiting(&mut self, wait: Wait) {
        if let Some(run) = &mut self.run {
            run.wait = wait;
        }
    }

    fn done(&mut self, done: Done) {
        if let Some(run) = &mut self.run {
            run.done = Some(done);
        }
    }

    /// An `agent` node: a turn of the lead on the node's prompt.
    async fn begin_lead(
        &mut self,
        node: String,
        prompt: String,
        verdicts: Vec<String>,
    ) -> Result<()> {
        let started = self.node_started(&node, Some(excerpt(&prompt))).await?;
        if prompt.trim().is_empty() {
            self.done(Done::new(&node, "failed", "the prompt is empty"));
            return Ok(());
        }
        // The text that starts this turn is the workflow's, not the user's:
        // it is on record as text nobody said, under the node that says it.
        let item = Item {
            id: format!("workflow-{}", Ulid::generate()),
            status: ItemStatus::Completed,
            content: ItemContent::Text {
                text: prompt.clone(),
            },
        };
        let shown = EventBody::ItemCompleted { item };
        self.append(EventDraft::new(shown).parent(Some(started.clone())))
            .await?;
        let notes = self.run.as_mut().map(|run| std::mem::take(&mut run.notes));
        self.waiting(Wait::Lead {
            node,
            verdicts,
            text: None,
            captured: None,
        });
        // Only the agent is told about the branches: the transcript keeps
        // the node's own prompt.
        let text = briefing::with_notes(notes.unwrap_or_default(), prompt);
        self.begin_turn(text, started).await
    }

    /// Keeps the lead's newest text while its node's result is open.
    pub(super) fn lead_said(&mut self, said: &str) {
        if let Some(run) = &mut self.run
            && let Wait::Lead {
                text,
                captured: None,
                ..
            } = &mut run.wait
        {
            *text = Some(said.to_owned());
        }
    }

    /// The lead's turn is over: fixes the result of the node it works on,
    /// unless that is fixed already. A turn that ends on a question of the
    /// agent's own goes on with the answer, so it fixes nothing.
    pub(super) fn capture_lead(&mut self, interrupted: bool) {
        let asked = self.question.is_some();
        let Some(run) = &mut self.run else {
            return;
        };
        let Wait::Lead {
            node,
            verdicts,
            text,
            captured,
        } = &mut run.wait
        else {
            return;
        };
        if captured.is_some() {
            return;
        }
        if interrupted {
            *captured = Some(Done::new(node, "failed", "interrupted"));
        } else if !asked {
            *captured = Some(agent_result(
                node,
                verdicts,
                text.take().unwrap_or_default(),
            ));
        }
    }

    /// The lead has nothing left to do: the node it worked on is over.
    pub(super) fn lead_rested(&mut self) {
        self.capture_lead(false);
        if let Some(run) = &mut self.run
            && let Wait::Lead { captured, .. } = &mut run.wait
            && let Some(done) = captured.take()
        {
            run.done = Some(done);
        }
    }

    /// The lead agent failed during a run: the agent is closed as failed,
    /// and the node it worked on leaves through `failed` (spec section 3).
    /// The session goes on; the next lead turn starts the agent again.
    pub(super) async fn lead_lost(&mut self, message: String) -> Result<()> {
        let tracker = self.turn_tracker().await;
        self.drop_agent();
        self.abort_attaching();
        self.forget_turn();
        self.expire_permissions().await?;
        // Its own question goes with it; a gate is the workflow's and stays.
        if self.question.as_ref().is_some_and(|q| q.agent_id.is_some()) {
            self.expire_question().await?;
        }
        self.record_error(message.clone()).await?;
        self.close_agents(tracker, AgentStatus::Failed, AgentStatus::Failed)
            .await?;
        if let Some(run) = &mut self.run
            && let Wait::Lead { node, captured, .. } = &mut run.wait
        {
            // A result fixed before the agent died stands: it died on a
            // later turn, on something the user sent.
            let failed = Done::new(node, "failed", excerpt(&message));
            run.done = Some(captured.take().unwrap_or(failed));
        }
        Ok(())
    }

    /// An `agent` node with `fresh: true`, or a `parallel` node: one child
    /// session per brief, through the same dispatch an agent's tool uses.
    async fn begin_children(
        &mut self,
        node: String,
        line: String,
        verdicts: Vec<String>,
        parallel: bool,
        briefs: Vec<Brief>,
    ) -> Result<()> {
        let started = self.node_started(&node, Some(line)).await?;
        let agent_id = self.lead_agent_id();
        let mut children: Vec<ChildWait> = Vec::with_capacity(briefs.len());
        for brief in briefs {
            let dispatched = if brief.prompt.trim().is_empty() {
                Err(Error::Invalid("the prompt is empty".to_owned()))
            } else {
                let with = ChildWith {
                    runner: brief.runner,
                    model: brief.model,
                    cause: Some(started.clone()),
                };
                let (title, prompt) = (brief.title, brief.prompt.trim().to_owned());
                self.dispatch(agent_id.clone(), title, prompt, brief.repos, with)
                    .await
            };
            match dispatched {
                Ok(dispatched) => {
                    self.workflow_children.insert(dispatched.session.id.clone());
                    children.push(ChildWait {
                        session_id: dispatched.session.id,
                        label: brief.label,
                        report: None,
                    });
                }
                Err(err) if err.is_refusal() => {
                    // The node fails as a whole: what it already started
                    // has nobody left to report to.
                    self.stop_children(children.iter().map(|child| child.session_id.clone()));
                    let why = format!("could not dispatch {}: {err}", brief.label);
                    self.done(Done::new(&node, "failed", excerpt(&why)).with("summary", why));
                    return Ok(());
                }
                Err(err) => return Err(err),
            }
        }
        self.waiting(Wait::Children {
            node,
            verdicts,
            parallel,
            children,
        });
        Ok(())
    }

    /// Stops child sessions nobody waits for any more, without waiting for
    /// them: a child being stopped may be writing to this session.
    fn stop_children(&self, children: impl Iterator<Item = String>) {
        for child in children {
            let manager = self.manager();
            tokio::spawn(async move {
                match manager.stop(&child).await {
                    // It ended on its own meanwhile.
                    Ok(()) | Err(Error::Conflict(_)) => {}
                    Err(err) => {
                        tracing::warn!(session = %child, %err, "could not stop a workflow's child session");
                    }
                }
            });
        }
    }

    /// A child session a node dispatched reported: it rested, failed or was
    /// stopped (`ended` is `Failed` for both, and `body` says which). Once
    /// every child of the node has, the node is over; a child that failed
    /// makes it leave through `failed`, with what the child's report says
    /// first as the outcome.
    pub(super) async fn child_ended(
        &mut self,
        session_id: &str,
        body: String,
        ended: ChildEnd,
    ) -> Result<()> {
        let Some(run) = &mut self.run else {
            return Ok(());
        };
        let Wait::Children { children, .. } = &mut run.wait else {
            return Ok(());
        };
        let waited = children
            .iter_mut()
            .find(|child| child.session_id == session_id && child.report.is_none());
        // A later report of a child the run no longer waits for.
        let Some(child) = waited else {
            return Ok(());
        };
        child.report = Some((body, ended));
        if children.iter().any(|child| child.report.is_none()) {
            return Ok(());
        }
        let Wait::Children {
            node,
            verdicts,
            parallel,
            children,
        } = std::mem::replace(&mut run.wait, Wait::Nothing)
        else {
            return Ok(());
        };
        let mut reports = Vec::with_capacity(children.len());
        let mut worked = Vec::with_capacity(children.len());
        for child in children {
            // Every child has reported by now.
            let (body, ended) = child.report.unwrap_or((String::new(), ChildEnd::Review));
            // Its work stays on its own branches: the lead is told which.
            let repos = match self.inner.store.session(&child.session_id).await {
                Ok(detail) => detail.repos,
                Err(_) => Vec::new(),
            };
            let branches = repos
                .into_iter()
                .filter_map(|repo| repo.branch.map(|branch| (repo.name, branch)))
                .collect();
            worked.push(briefing::WorkflowChild {
                label: child.label.clone(),
                failed: ended == ChildEnd::Failed,
                branches,
            });
            reports.push((child.label, body, ended));
        }
        let note = briefing::workflow_children_note(&node, &worked);
        let failed = reports
            .iter()
            .filter(|(_, _, ended)| *ended == ChildEnd::Failed);
        let failed: Vec<String> = failed
            .map(|(label, body, _)| format!("{label}: {}", excerpt(body)))
            .collect();
        let done = if parallel {
            let sections = reports
                .iter()
                .map(|(label, body, _)| format!("## {label}\n\n{}", body.trim_end()));
            let summary = sections.collect::<Vec<_>>().join("\n\n");
            match failed.is_empty() {
                true => Done::new(&node, "done", format!("{} branches done", reports.len())),
                false => Done::new(&node, "failed", format!("failed: {}", failed.join("; "))),
            }
            .with("summary", summary)
        } else {
            let summary = reports.into_iter().next().map(|(_, body, _)| body);
            let summary = summary.unwrap_or_default();
            match failed.is_empty() {
                true => agent_result(&node, &verdicts, summary),
                false => Done::new(&node, "failed", excerpt(&summary)).with("summary", summary),
            }
        };
        if let Some(run) = &mut self.run {
            run.notes.extend(note);
            run.done = Some(done);
        }
        Ok(())
    }

    /// A `check` node: its commands run one after the other, off the runtime.
    async fn begin_check(
        &mut self,
        node: String,
        check: CheckCommand,
        timeout_seconds: u64,
    ) -> Result<()> {
        let line = match &check {
            CheckCommand::Command { command, .. } => excerpt(command),
            CheckCommand::RepoDefaults => "the checks of each changed repository".to_owned(),
        };
        self.node_started(&node, Some(line)).await?;
        let jobs = match check {
            CheckCommand::Command { command, repo } => {
                let place = match &repo {
                    Some(name) => {
                        let named = self.repos.iter().find(|repo| repo.name == *name);
                        named.map(|repo| (repo.name.clone(), PathBuf::from(&repo.path)))
                    }
                    None => {
                        let name = primary(&self.repos).map(|repo| repo.name.clone());
                        let work_dir = self.inner.store.home().work_dir(&self.session_id);
                        match working_directory(&work_dir, &self.repos).await {
                            Ok(dir) => Some((name.unwrap_or_default(), dir)),
                            Err(_) => None,
                        }
                    }
                };
                let Some((repo, dir)) = place else {
                    let why = match repo {
                        Some(name) => {
                            format!("no repository named {name} is attached to this session")
                        }
                        None => "the session has no directory to run the command in".to_owned(),
                    };
                    self.done(Done::new(&node, "fail", why.clone()).with("output", why));
                    return Ok(());
                };
                vec![CheckJob {
                    repo,
                    dir,
                    command,
                    changed_since: None,
                }]
            }
            CheckCommand::RepoDefaults => {
                let mut jobs = Vec::new();
                for repo in &self.repos {
                    let (Some(base), Some(branch)) = (&repo.base_ref, &repo.branch) else {
                        continue;
                    };
                    if repo.mode != RepoMode::Write {
                        continue;
                    }
                    let declared = self.inner.repos.get(&repo.repo_id).await;
                    for command in declared.map(|repo| repo.checks).unwrap_or_default() {
                        jobs.push(CheckJob {
                            repo: repo.name.clone(),
                            dir: PathBuf::from(&repo.path),
                            command,
                            changed_since: Some((base.clone(), branch.clone())),
                        });
                    }
                }
                jobs
            }
        };
        let (results, work) = mpsc::channel(4);
        let cancel = Arc::new(AtomicBool::new(false));
        let limit = Duration::from_secs(timeout_seconds);
        tokio::spawn(run_checks(
            node.clone(),
            jobs,
            limit,
            results,
            Arc::clone(&cancel),
        ));
        if let Some(run) = &mut self.run {
            run.wait = Wait::Check { node };
            run.work = Some(work);
            run.cancel = cancel;
        }
        Ok(())
    }

    /// A `commit` node: what is uncommitted in every `write` repo.
    async fn begin_commit(&mut self, node: String, message: String) -> Result<()> {
        self.node_started(&node, Some(excerpt(&message))).await?;
        let dirs: Vec<(String, PathBuf)> = self
            .repos
            .iter()
            .filter(|repo| repo.mode == RepoMode::Write)
            .map(|repo| (repo.name.clone(), PathBuf::from(&repo.path)))
            .collect();
        let (results, work) = mpsc::channel(1);
        let limit = self.inner.config.commit_timeout;
        tokio::spawn(async move {
            let committed = blocking(move || {
                let mut committed = Vec::new();
                for (name, dir) in dirs {
                    match calin_git::commit_all(&dir, &message, limit) {
                        Ok(calin_git::CommitOutcome::Committed(_)) => committed.push(name),
                        Ok(calin_git::CommitOutcome::NothingToCommit) => {}
                        Err(err) => return Ok(Err(format!("{name}: {err}"))),
                    }
                }
                Ok(Ok(committed))
            });
            let committed = committed.await.unwrap_or_else(|err| Err(err.to_string()));
            // The run may be over by now; nobody is left to tell.
            let _ = results.send(Worked::Committed(committed)).await;
        });
        if let Some(run) = &mut self.run {
            run.wait = Wait::Commit { node };
            run.work = Some(work);
        }
        Ok(())
    }

    /// A `ship` node: the session's pull requests, as the Ship action
    /// opens them.
    async fn begin_ship_node(
        &mut self,
        node: String,
        title: Option<String>,
        draft: bool,
    ) -> Result<()> {
        // It names a commit and a pull request: one line, whatever the
        // template made of it.
        let title = title
            .map(|title| one_line(&title))
            .filter(|title| !title.is_empty());
        let line = title.clone();
        self.node_started(
            &node,
            line.or_else(|| Some("Open pull requests".to_owned())),
        )
        .await?;
        if let Some(problem) = self.inner.github_problem(true).await {
            let why = format!("pull requests cannot be opened: {problem}");
            self.done(Done::new(&node, "failed", why));
            return Ok(());
        }
        if self.shipping.is_some() {
            let why = "the session is already being shipped";
            self.done(Done::new(&node, "failed", why));
            return Ok(());
        }
        self.start_ship(title, None, draft);
        self.waiting(Wait::Ship { node });
        Ok(())
    }

    /// The ship a node started has attempted every repo. True when a run
    /// took the result.
    pub(super) fn run_shipped(&mut self) -> bool {
        let urls: Vec<&str> = self
            .pull_requests
            .iter()
            .map(|pull_request| pull_request.url.as_str())
            .collect();
        let Some(run) = &mut self.run else {
            return false;
        };
        let Wait::Ship { node } = &run.wait else {
            return false;
        };
        run.done = Some(if urls.is_empty() {
            Done::new(node, "failed", "no pull request was opened")
        } else {
            run.shipped = true;
            let count = match urls.len() {
                1 => "1 pull request".to_owned(),
                count => format!("{count} pull requests"),
            };
            Done::new(node, "done", count).with("urls", urls.join("\n"))
        });
        true
    }

    /// A `gate` or an `ask` node: a question of the workflow's own, through
    /// what an agent's question goes through. The session waits.
    async fn begin_question(
        &mut self,
        node: String,
        text: String,
        options: Vec<String>,
        gate: bool,
    ) -> Result<()> {
        let started = self.node_started(&node, Some(excerpt(&text))).await?;
        let question = Question {
            id: Ulid::generate().to_string(),
            text,
            options,
            gate,
        };
        let question_id = question.id.clone();
        let asked = EventDraft::new(EventBody::QuestionAsked { question }).parent(Some(started));
        let asked = self.append(asked).await?;
        self.question = Some(OpenQuestion {
            session_id: self.session_id.clone(),
            question_id: question_id.clone(),
            agent_id: None,
            event_id: asked.id.clone(),
        });
        self.waiting(Wait::Answer {
            node,
            question_id,
            gate,
        });
        if self.status != SessionStatus::Waiting {
            let reason = match gate {
                true => WAITING_AT_GATE,
                false => WAITING_FOR_ANSWER,
            };
            self.set_status(SessionStatus::Waiting, Some(reason.to_owned()))
                .await?;
        }
        Ok(())
    }

    /// Refuses an answer a gate cannot take (the "gate" semantics in
    /// `calin-proto/src/api.rs`). Any other question takes any answer.
    pub(super) fn check_answer(&self, question_id: &str, answer: &str) -> Result<()> {
        let wait = self.run.as_ref().map(|run| &run.wait);
        match wait {
            Some(Wait::Answer {
                question_id: asked,
                gate: true,
                ..
            }) if asked == question_id && answer != APPROVE && answer != REJECT => {
                Err(Error::Invalid(format!(
                    "a gate is answered with `{APPROVE}` or `{REJECT}`, not with `{answer}`"
                )))
            }
            _ => Ok(()),
        }
    }

    /// Takes the user's answer when it is to a question of the run's own.
    /// True when it was: the answer is then the node's result and nothing
    /// for the lead agent.
    pub(super) fn run_answered(
        &mut self,
        question_id: &str,
        answer: &str,
        comment: Option<&str>,
    ) -> bool {
        let Some(run) = &mut self.run else {
            return false;
        };
        let Wait::Answer {
            node,
            question_id: asked,
            gate,
        } = &run.wait
        else {
            return false;
        };
        if asked != question_id {
            return false;
        }
        run.done = Some(if *gate {
            let comment = comment.map(str::trim).unwrap_or_default();
            let line = match comment.is_empty() {
                true => answer.to_owned(),
                false => format!("{answer}: {}", excerpt(comment)),
            };
            Done::new(node, answer, line).with("comment", comment)
        } else {
            Done::new(node, "answered", excerpt(answer)).with("answer", answer)
        });
        true
    }

    /// Records a step of the active node's background work.
    pub(super) async fn on_worked(&mut self, worked: Option<Worked>) -> Result<()> {
        let parent = self.run.as_ref().and_then(|run| run.node_event.clone());
        match worked {
            // A check shows in the transcript as a tool call nobody but the
            // orchestrator made, like a setup command.
            Some(Worked::CheckStarted(item)) => {
                let started = EventBody::ItemStarted { item };
                self.append(EventDraft::new(started).parent(parent)).await?;
            }
            Some(Worked::CheckEnded(item)) => {
                let completed = EventBody::ItemCompleted { item };
                self.append(EventDraft::new(completed).parent(parent))
                    .await?;
            }
            Some(Worked::CheckDone(done)) => self.done(done),
            Some(Worked::Committed(committed)) => {
                let Some(run) = &mut self.run else {
                    return Ok(());
                };
                let Wait::Commit { node } = &run.wait else {
                    return Ok(());
                };
                run.done = Some(match committed {
                    Ok(repos) if repos.is_empty() => {
                        Done::new(node, "nothing", "nothing to commit")
                    }
                    Ok(repos) => {
                        Done::new(node, "done", format!("committed in {}", repos.join(", ")))
                    }
                    Err(why) => Done::new(node, "failed", excerpt(&why)),
                });
            }
            // The work ended without its last word: it died.
            None => {
                let Some(run) = &mut self.run else {
                    return Ok(());
                };
                run.work = None;
                if run.done.is_none() {
                    let port = match &run.wait {
                        Wait::Check { .. } => "fail",
                        _ => "failed",
                    };
                    if let Some(node) = run.wait.node() {
                        run.done = Some(Done::new(node, port, "the work ended unexpectedly"));
                    }
                }
            }
        }
        Ok(())
    }

    /// The engine says the run is over.
    async fn finish_run(&mut self, finish: Finish) -> Result<()> {
        let (status, reason) = match finish.status {
            EndStatus::Success => (WorkflowStatus::Success, None),
            EndStatus::Failed => (WorkflowStatus::Failed, Some(finish.reason.to_string())),
        };
        self.end_run(status, reason).await
    }

    /// Records the end of the run and brings the session to rest: `shipping`
    /// when a `ship` node left pull requests, `review` otherwise, also for
    /// a run that failed (spec section 6). From here on the session is an
    /// ordinary one.
    async fn end_run(&mut self, status: WorkflowStatus, reason: Option<String>) -> Result<()> {
        let Some(run) = self.run.take() else {
            return Ok(());
        };
        let finished = EventBody::WorkflowFinished { status, reason };
        self.append(EventDraft::new(finished).parent(Some(run.started_event.clone())))
            .await?;
        // Branches no lead turn was told about go with its next message.
        if self.handle.is_some() {
            self.notes.extend(run.notes);
        }
        self.settle_run(run.shipped).await?;
        // What arrived for the lead meanwhile starts a turn once this step
        // is over. After a failure it waits for the user's next message, as
        // after an interrupt.
        self.flush_pending = status == WorkflowStatus::Success;
        Ok(())
    }

    async fn settle_run(&mut self, shipped: bool) -> Result<()> {
        let wanted = match shipped && !self.pull_requests.is_empty() {
            true => SessionStatus::Shipping,
            false => SessionStatus::Review,
        };
        if self.status != wanted {
            self.set_status(wanted, None).await?;
        }
        Ok(())
    }

    /// Ends the run from outside: the budget is spent, or the session is
    /// being stopped. What the active node waits for is called off, the
    /// node and the run are closed in the log, and, for the budget, the
    /// session goes to `review`.
    pub(super) async fn abort_run(&mut self, why: Abort) -> Result<()> {
        let Some(mut run) = self.run.take() else {
            return Ok(());
        };
        let reason = match &why {
            Abort::Budget { budget, spent } => format!(
                "the budget of {} was passed: the session and its child sessions have cost {}",
                dollars(*budget),
                dollars(*spent)
            ),
            Abort::Stopped => STOPPED.to_owned(),
        };
        let budget = matches!(why, Abort::Budget { .. });
        run.cancel.store(true, Ordering::Relaxed);
        match &run.wait {
            Wait::Children { children, .. } => {
                let unsettled = children.iter().filter(|child| child.report.is_none());
                self.stop_children(unsettled.map(|child| child.session_id.clone()));
            }
            // The command is killed; its tool item still gets its end.
            Wait::Check { .. } => {
                if let Some(mut work) = run.work.take() {
                    let parent = run.node_event.clone();
                    let grace = tokio::time::sleep(STOP_GRACE);
                    tokio::pin!(grace);
                    loop {
                        let body = tokio::select! {
                            worked = work.recv() => match worked {
                                Some(Worked::CheckStarted(item)) => EventBody::ItemStarted { item },
                                Some(Worked::CheckEnded(item)) => EventBody::ItemCompleted { item },
                                Some(Worked::CheckDone(_) | Worked::Committed(_)) | None => break,
                            },
                            () = &mut grace => break,
                        };
                        self.append(EventDraft::new(body).parent(parent.clone()))
                            .await?;
                    }
                }
            }
            Wait::Answer { .. } => self.expire_question().await?,
            // A stop waits for the ship itself, to record what it opened.
            Wait::Ship { .. } if budget => self.abort_shipping(),
            Wait::Lead { .. } | Wait::Commit { .. } | Wait::Ship { .. } | Wait::Nothing => {}
        }
        if let Some(node) = run.wait.node() {
            let line = match budget {
                true => "stopped: the budget was passed".to_owned(),
                false => format!("stopped: {STOPPED}"),
            };
            let closed = EventBody::WorkflowNode {
                node_id: node.to_owned(),
                phase: NodePhase::Finished,
                port: None,
                outcome: Some(line),
            };
            self.append(EventDraft::new(closed).parent(run.node_event.clone()))
                .await?;
        }
        let finished = EventBody::WorkflowFinished {
            status: WorkflowStatus::Failed,
            reason: Some(reason),
        };
        self.append(EventDraft::new(finished).parent(Some(run.started_event.clone())))
            .await?;
        if !budget {
            // The stop that called this cancels the session.
            return Ok(());
        }
        if self.in_turn {
            // The turn is asked to end; the session rests when it has.
            self.expire_permissions().await?;
            self.held.clear();
            return self.ask_to_end_turn().await;
        }
        self.settle_run(run.shipped).await
    }

    /// Ends the run when the session tree has cost more than its budget.
    /// Called after every usage event of the tree (spec section 6).
    pub(super) async fn check_budget(&mut self) -> Result<()> {
        let Some(budget) = self.run.as_ref().and_then(|run| run.budget_usd) else {
            return Ok(());
        };
        let spent = self.inner.store.tree_cost(&self.session_id).await?;
        if spent <= budget {
            return Ok(());
        }
        self.abort_run(Abort::Budget { budget, spent }).await
    }
}

/// The result of an `agent` node from the text its agent ended on: `done`,
/// or the port of the verdict the text ends with (spec section 3).
fn agent_result(node: &str, verdicts: &[String], summary: String) -> Done {
    if verdicts.is_empty() {
        return Done::new(node, "done", excerpt(&summary)).with("summary", summary);
    }
    let verdict = extract_verdict(&summary, verdicts);
    let line = match verdict.word() {
        "" => "no verdict in the answer",
        word => word,
    };
    Done::new(node, verdict.port(), line)
        .with("summary", summary)
        .with("verdict", verdict.word())
}

/// An amount of money in a sentence: cents, or more digits when it has them.
fn dollars(amount: f64) -> String {
    let exact = format!("{amount:.4}");
    let trimmed = exact.trim_end_matches('0');
    let cents = exact.len() - 2;
    format!("${}", &exact[..trimmed.len().max(cents)])
}

/// The last `cap` bytes of `text`, cut at a character.
fn tail(text: String, cap: usize) -> String {
    if text.len() <= cap {
        return text;
    }
    let mut start = text.len() - cap;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// The tool item a check command shows as. Its id is new for every run.
fn check_item(repo: &str, command: &str) -> Item {
    Item {
        id: format!("check-{}", Ulid::generate()),
        status: ItemStatus::Running,
        content: ItemContent::Tool {
            name: "check".to_owned(),
            tool_kind: ToolKind::Execute,
            title: command.to_owned(),
            input: serde_json::json!({ "command": command, "repo": repo }),
            output: None,
            locations: Vec::new(),
        },
    }
}

/// Runs the commands of a check node one after the other, all within
/// `limit`, and reports each step. Ends at the first command that fails; the
/// node's `output` is the last command's (spec section 3).
async fn run_checks(
    node: String,
    jobs: Vec<CheckJob>,
    limit: Duration,
    results: mpsc::Sender<Worked>,
    cancel: Arc<AtomicBool>,
) {
    let deadline = Instant::now() + limit;
    let mut unchanged: HashSet<String> = HashSet::new();
    let mut changed: HashSet<String> = HashSet::new();
    let mut last: Option<(String, String)> = None;
    let mut ran = 0;
    for job in jobs {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        if let Some((base, branch)) = &job.changed_since {
            if unchanged.contains(&job.repo) {
                continue;
            }
            if !changed.contains(&job.repo) {
                let (dir, base, branch) = (job.dir.clone(), base.clone(), branch.clone());
                let differs = blocking(move || {
                    Ok(!calin_git::changes(&dir, &base)?.is_empty()
                        || calin_git::commits_ahead(&dir, &base, &branch)? > 0)
                });
                // A repo git cannot answer for is checked rather than skipped.
                if differs.await.unwrap_or(true) {
                    changed.insert(job.repo.clone());
                } else {
                    unchanged.insert(job.repo.clone());
                    continue;
                }
            }
        }
        let mut item = check_item(&job.repo, &job.command);
        if results
            .send(Worked::CheckStarted(item.clone()))
            .await
            .is_err()
        {
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let (dir, command, stop) = (job.dir.clone(), job.command.clone(), Arc::clone(&cancel));
        let ran_command =
            blocking(move || Ok(calin_git::run_check(&dir, &command, left, &stop)?)).await;
        let place = match job.repo.is_empty() {
            true => String::new(),
            false => format!(" in {}", job.repo),
        };
        // `Err` holds why the node fails; the output is the node's either way.
        let (output, exit_code, failure) = match ran_command {
            Ok(run) => match run.end {
                calin_git::CheckEnd::Exited(Some(0)) => (run.output, Some(0), None),
                calin_git::CheckEnd::Exited(code) => {
                    let how = match code {
                        Some(code) => format!("exit status {code}"),
                        None => "killed by a signal".to_owned(),
                    };
                    let why = format!("failed{place} ({how}): `{}`", job.command);
                    (run.output, code, Some(why))
                }
                calin_git::CheckEnd::TimedOut => {
                    let seconds = limit.as_secs();
                    let note = format!("[stopped: the check ran longer than {seconds} s]\n");
                    let mut output = run.output;
                    if !output.is_empty() && !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str(&note);
                    let why = format!("timed out{place} after {seconds} s: `{}`", job.command);
                    (output, None, Some(why))
                }
                calin_git::CheckEnd::Cancelled => {
                    (run.output, None, Some("was cancelled".to_owned()))
                }
            },
            Err(err) => {
                let why = format!("could not be run{place}: {err} (`{}`)", job.command);
                (format!("{why}\n"), None, Some(why))
            }
        };
        let output = tail(output, calin_git::CHECK_OUTPUT_CAP);
        item.status = match failure {
            None => ItemStatus::Completed,
            Some(_) => ItemStatus::Failed,
        };
        if let ItemContent::Tool { output: slot, .. } = &mut item.content {
            *slot = Some(output.clone());
        }
        if results.send(Worked::CheckEnded(item)).await.is_err() || cancel.load(Ordering::Relaxed) {
            return;
        }
        let code = exit_code.map(|code| code.to_string());
        if let Some(why) = failure {
            let mut done = Done::new(&node, "fail", excerpt(&why)).with("output", output);
            if let Some(code) = code {
                done = done.with("exit_code", code);
            }
            let _ = results.send(Worked::CheckDone(done)).await;
            return;
        }
        ran += 1;
        last = Some((output, code.unwrap_or_default()));
    }
    let line = match ran {
        0 => "nothing to check".to_owned(),
        1 => "passed".to_owned(),
        ran => format!("{ran} commands passed"),
    };
    let mut done = Done::new(&node, "pass", line);
    if let Some((output, code)) = last {
        done = done.with("output", output).with("exit_code", code);
    }
    let _ = results.send(Worked::CheckDone(done)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_is_said_to_the_cent_or_as_exactly_as_it_is_known() {
        assert_eq!(dollars(5.0), "$5.00");
        assert_eq!(dollars(0.5), "$0.50");
        assert_eq!(dollars(0.0125), "$0.0125");
        assert_eq!(dollars(0.025), "$0.025");
        assert_eq!(dollars(12.3456789), "$12.3457");
    }

    #[test]
    fn a_tail_keeps_the_end_and_whole_characters() {
        assert_eq!(tail("short".to_owned(), 16), "short");
        assert_eq!(tail("abcdef".to_owned(), 3), "def");
        // Never half a character: the cut moves forward to the next one.
        assert_eq!(tail("aé€b".to_owned(), 4), "€b");
        assert_eq!(tail("aé€b".to_owned(), 3), "b");
    }

    #[test]
    fn an_agents_answer_becomes_a_port_and_outputs() {
        let verdicts = vec!["approve".to_owned(), "changes".to_owned()];
        let plain = agent_result("work", &[], "Did it.\nAll of it.".to_owned());
        assert_eq!(plain.result.port, "done");
        assert_eq!(plain.outcome.as_deref(), Some("Did it."));
        assert_eq!(plain.result.outputs["summary"], "Did it.\nAll of it.");
        assert!(!plain.result.outputs.contains_key("verdict"));
        // An agent that said nothing is done all the same.
        assert_eq!(agent_result("work", &[], String::new()).outcome, None);

        let approved = agent_result("review", &verdicts, "Fine.\nVERDICT: approve".to_owned());
        assert_eq!(approved.result.port, "approve");
        assert_eq!(approved.outcome.as_deref(), Some("approve"));
        assert_eq!(approved.result.outputs["verdict"], "approve");
        let none = agent_result("review", &verdicts, "Fine, I think.".to_owned());
        assert_eq!(none.result.port, "failed");
        assert_eq!(none.outcome.as_deref(), Some("no verdict in the answer"));
        assert_eq!(none.result.outputs["verdict"], "");
        assert_eq!(none.result.outputs["summary"], "Fine, I think.");
    }
}
