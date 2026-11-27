//! A session's driver: the one task that changes the session. It takes the
//! manager's commands, the agent's output and the results of preparing the
//! repos, one at a time, and appends what follows from each.

mod run;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calin_agents::{
    AgentHandle, DirGrant, Exit, PermissionAsk, Runner, RunnerOutput, StartParams, SubagentStart,
    ToolAccess,
};
use calin_proto::{
    Agent, AgentKind, AgentMessage, AgentStatus, AttachRepoRequest, DecisionSource, Event,
    EventBody, FileOp, IssueRef, Item, ItemContent, ItemStatus, LimitWindow, MessageKind,
    PermissionDecision, PermissionMode, PermissionRequest, PullRequest, PullRequestState, Question,
    Repo, RepoMode, SessionDetail, SessionRepo, SessionStatus, ToolKind,
};
use tokio::sync::{mpsc, oneshot};
use ulid::Ulid;

use self::run::{Abort, Run, Worked, next_worked};
use super::delivery::{self, ShipPlan, Shipped};
use super::queue::Slot;
use super::unreported::{self, MAX_CHANGES_PER_TURN, Tracker};
use super::{
    ChildEnd, Command, Dispatched, DispatchedRepo, Inner, NoteOutcome, NoteTarget, Reply,
    SessionManager, agent_ended, closed, expired, is_closed, no_attaching, no_longer_active,
    no_shipping, no_turn, now_ms, ships, status_name, takes_messages, takes_repos, unanswered,
};
use crate::briefing::{self, ChildOf, MAX_RUNNING_CHILDREN, ReportedBranch};
use crate::error::{Error, Result};
use crate::policy::{self, Target, Verdict};
use crate::repos::{Reservation, blocking};
use crate::store::{Dispatch, EventDraft, NewSession, OpenQuestion, SessionSetup};
use crate::tokens::{AgentRef, Token};
use crate::workspace::{locate, primary, working_directory};

/// How long a stopped agent, or a git command of a stopped preparation, may
/// take to end before the session is cancelled without it.
const STOP_GRACE: Duration = Duration::from_secs(5);
pub(super) const LEAD_ROLE: &str = "lead";
/// The reason of `waiting` while a permission request is open.
const WAITING_FOR_PERMISSION: &str = "permission";
/// The reason of `waiting` once an agent asked the user a question.
const WAITING_FOR_ANSWER: &str = "question";
/// The body of a child's report when its agent said nothing at all.
const NO_REPORT: &str = "(The child session ended its turn without a report.)";
/// The reason of `queued` while every slot is taken (the "queue" semantics
/// in `calin-proto/src/api.rs`).
pub(super) const WAITING_FOR_SLOT: &str = "waiting for a slot: as many sessions as the settings \
     allow are running";

/// The lead agent as the driver knows it.
struct Lead {
    id: String,
    status: AgentStatus,
    /// The runner's conversation id, passed back to resume the agent.
    native_session_id: Option<String>,
    model: Option<String>,
}

/// An input waiting for the current turn to end: a user message, or what
/// Calin delivers (a report, a note, an answer).
struct Queued {
    /// What the agent is handed.
    text: String,
    /// The event that recorded the input: the cause of the turn it starts.
    event_id: String,
}

/// An item of the current turn that is still running.
struct TurnItem {
    /// Its `item_started` event.
    event_id: String,
    /// For a tool call: the tool's name.
    tool: Option<String>,
}

/// A permission request waiting for the user.
struct OpenRequest {
    request_id: String,
    /// The `permission_requested` event: the cause of the answer.
    event_id: String,
    /// The repos of the session the request reaches into, as indexes into
    /// the driver's list: what an allow may have to open to the agent.
    repos: Vec<usize>,
    /// The agent that asked: the lead, or one of its subagents.
    agent_id: Option<String>,
}

/// One repo to make ready for a session.
struct AttachPlan {
    session_id: String,
    repo: Repo,
    mode: RepoMode,
    /// Where the worktree of a `write` repo goes.
    worktree: PathBuf,
    branch: String,
    primary: bool,
    /// The commit a new branch starts at; the clone's `HEAD` when `None`.
    /// Set for a child session: its parent's commit when it was dispatched.
    start_point: Option<String>,
    /// How long the repo's setup command may run.
    setup_limit: Duration,
}

/// A step of making repos ready, reported to the driver, which is the one
/// that writes to the log.
enum Prepared {
    Repo(SessionRepo),
    /// The setup command of a new worktree: the tool item as it starts, then
    /// as it ended, with what to tell the user when it failed.
    SetupStarted(Item),
    SetupEnded {
        item: Item,
        failure: Option<String>,
    },
    Failed {
        repo: String,
        error: Error,
    },
}

/// A repo being attached to a session that is already at work.
struct Attaching {
    results: mpsc::Receiver<Prepared>,
    /// Set to end the setup command early.
    cancel: Arc<AtomicBool>,
    /// The agent that asked through Calin's tools; `None` for the user.
    agent_id: Option<String>,
    reply: Reply<SessionRepo>,
    /// Set once the repo is on record; its setup may still be running.
    attached: Option<SessionRepo>,
    /// Keeps the repo registered until it is attached.
    _reservation: Reservation,
}

/// An attach request waiting for the one in progress.
struct QueuedAttach {
    request: AttachRepoRequest,
    agent_id: Option<String>,
    reply: Reply<SessionRepo>,
}

/// Repos being made ready in the background, and the turn that waits for them.
struct Preparation {
    /// One message per repo as it becomes ready; closed when all are.
    results: mpsc::Receiver<Prepared>,
    /// Set to skip the repos not started yet.
    cancel: Arc<AtomicBool>,
    text: String,
    cause: String,
    /// Keeps the repos registered until they are attached.
    _reservation: Option<Reservation>,
}

/// A ship in progress: the background work reports each step here.
struct Shipping {
    results: mpsc::Receiver<Shipped>,
    /// Set to end the ship between two steps.
    cancel: Arc<AtomicBool>,
    /// A pull request was opened or found: the session is being shipped.
    delivered: bool,
}

/// A look at the session's pull requests on GitHub, in progress.
struct Polling {
    /// The pull requests that could be read, as they are now.
    observed: oneshot::Receiver<Vec<PullRequest>>,
    /// Everyone who asked for a look and is told when this one is on record.
    waiting: Vec<Reply>,
}

/// A session that waits for a queue slot, and what it does with one.
struct Parked {
    /// Resolves when the queue gave the session a slot.
    granted: oneshot::Receiver<()>,
    then: Resume,
}

/// What a session goes on with once it holds a slot.
enum Resume {
    /// A turn on this input, after making the session's repos ready.
    Launch {
        text: String,
        cause: String,
        reservation: Option<Reservation>,
    },
    /// The turn in progress, which the session may now be said to run; or,
    /// when that turn has ended meanwhile, the inputs that waited behind it.
    Turn,
    /// The next node of the workflow run.
    Action(Box<calin_workflow::Action>),
}

/// What a child session is dispatched with when it is not an agent's tool
/// call that asks: a workflow node, which may name a runner and a model.
#[derive(Default)]
struct ChildWith {
    runner: Option<String>,
    model: Option<String>,
    /// The event that caused the dispatch; the running `dispatch` tool
    /// call when `None`.
    cause: Option<String>,
}

/// The driver hit a storage error and cannot go on.
struct Halt;

enum Step {
    Command(Command),
    Output(Option<RunnerOutput>),
    Prepared(Option<Prepared>),
    Attached(Option<Prepared>),
    Shipped(Option<Shipped>),
    Polled(Vec<PullRequest>),
    /// The queue gave the session a slot; false when it forgot the session.
    Granted(bool),
    Worked(Option<Worked>),
}

pub(super) struct Driver {
    inner: Arc<Inner>,
    session_id: String,
    commands: mpsc::UnboundedReceiver<Command>,
    status: SessionStatus,
    title: String,
    prompt: String,
    /// The issue the session was started from: its pull requests close it.
    issue: Option<IssueRef>,
    /// The root of the session's tree: the session itself unless it is a child.
    root_session_id: String,
    runner_id: String,
    permission_mode: PermissionMode,
    /// The model and the repos the session was created with.
    setup: SessionSetup,
    head_seq: u64,
    /// The repos attached so far, in order.
    repos: Vec<SessionRepo>,
    runner: Option<Arc<dyn Runner>>,
    lead: Option<Lead>,
    /// The running agent. `None` before the first turn, after the agent
    /// exited and after a restart; the next turn starts it (again).
    handle: Option<AgentHandle>,
    preparation: Option<Preparation>,
    in_turn: bool,
    /// The current turn was asked to end: it leads to `review` even when
    /// messages are waiting. They run with the next message.
    interrupted: bool,
    /// The user message the current turn answers: the parent of its events.
    cause: Option<String>,
    pending: VecDeque<Queued>,
    /// Permission requests of the current turn that wait for the user.
    open: Vec<OpenRequest>,
    /// The running items of the current turn, by item id.
    turn_items: HashMap<String, TurnItem>,
    /// The subagents the running agent has open: the runner's key for each
    /// to the id of the agent it is on record as.
    subagents: HashMap<String, String>,
    /// The question that waits for the user: one at a time.
    question: Option<OpenQuestion>,
    /// What a child's agent passed to the `report` tool since its last report.
    report: Option<String>,
    /// The lead's last finished text since the last report: what a child
    /// reports when its agent called no `report`.
    last_text: Option<String>,
    /// The repo being attached mid-session, and the requests behind it: one
    /// at a time, so two never fight over a worktree or a name.
    attaching: Option<Attaching>,
    attach_queue: VecDeque<QueuedAttach>,
    /// What to tell the agent with the next user message it is handed. The
    /// transcript keeps the user's own text: notes only exist on that path.
    notes: Vec<String>,
    /// Ids of the session repos the running agent can work in without
    /// asking: the ones it was started with and the ones opened to it since.
    granted: HashSet<String>,
    /// The running agent's key to Calin's tools. Revoked when dropped.
    token: Option<Token>,
    /// The session's pull requests on record, at most one per repo.
    pull_requests: Vec<PullRequest>,
    /// The ship in progress: one at a time, so two never push the same
    /// branch or open the same pull request twice.
    shipping: Option<Shipping>,
    polling: Option<Polling>,
    /// Set while the session waits for a queue slot.
    parked: Option<Parked>,
    /// Permission decisions of the user's that wait with the session for a
    /// slot: handing one over lets the agent's tool call run.
    held: Vec<(OpenRequest, PermissionDecision, Option<String>)>,
    /// The workflow the session was created with, until its run starts.
    workflow: Option<Box<calin_workflow::Workflow>>,
    /// The workflow run in progress.
    run: Option<Run>,
    /// The child sessions workflow nodes dispatched. Their reports are the
    /// workflow's, never a turn of the lead agent.
    workflow_children: HashSet<String>,
    /// The id the lead agent will have, fixed before it starts when a
    /// workflow node dispatches in its name.
    reserved_lead: Option<String>,
    /// A workflow run just ended well: the inputs that waited for the lead
    /// start a turn once the step that ended it is over.
    flush_pending: bool,
}

async fn next_granted(parked: &mut Option<Parked>) -> bool {
    match parked {
        Some(parked) => (&mut parked.granted).await.is_ok(),
        None => std::future::pending().await,
    }
}

async fn next_output(handle: &mut Option<AgentHandle>) -> Option<RunnerOutput> {
    match handle {
        Some(handle) => handle.next_output().await,
        None => std::future::pending().await,
    }
}

async fn next_prepared(preparation: &mut Option<Preparation>) -> Option<Prepared> {
    match preparation {
        Some(preparation) => preparation.results.recv().await,
        None => std::future::pending().await,
    }
}

async fn next_attached(attaching: &mut Option<Attaching>) -> Option<Prepared> {
    match attaching {
        Some(attaching) => attaching.results.recv().await,
        None => std::future::pending().await,
    }
}

async fn next_shipped(shipping: &mut Option<Shipping>) -> Option<Shipped> {
    match shipping {
        Some(shipping) => shipping.results.recv().await,
        None => std::future::pending().await,
    }
}

async fn next_polled(polling: &mut Option<Polling>) -> Vec<PullRequest> {
    match polling {
        // A look that died saw nothing; the next one starts over.
        Some(polling) => (&mut polling.observed).await.unwrap_or_default(),
        None => std::future::pending().await,
    }
}

impl Driver {
    /// A driver that knows nothing yet: it loads the session when it runs.
    pub(super) fn new(
        inner: Arc<Inner>,
        session_id: String,
        commands: mpsc::UnboundedReceiver<Command>,
    ) -> Self {
        Self {
            inner,
            session_id,
            commands,
            status: SessionStatus::Queued,
            title: String::new(),
            prompt: String::new(),
            issue: None,
            root_session_id: String::new(),
            runner_id: String::new(),
            permission_mode: PermissionMode::default(),
            setup: SessionSetup::default(),
            head_seq: 0,
            repos: Vec::new(),
            runner: None,
            lead: None,
            handle: None,
            preparation: None,
            in_turn: false,
            interrupted: false,
            cause: None,
            pending: VecDeque::new(),
            open: Vec::new(),
            turn_items: HashMap::new(),
            subagents: HashMap::new(),
            question: None,
            report: None,
            last_text: None,
            attaching: None,
            attach_queue: VecDeque::new(),
            notes: Vec::new(),
            granted: HashSet::new(),
            token: None,
            pull_requests: Vec::new(),
            shipping: None,
            polling: None,
            parked: None,
            held: Vec::new(),
            workflow: None,
            run: None,
            workflow_children: HashSet::new(),
            reserved_lead: None,
            flush_pending: false,
        }
    }

    pub(super) async fn run(mut self) {
        let retired = self.drive().await;
        if !retired {
            // However it ended, it runs nothing any more. Given back while
            // still on the map: a later driver of this session may hold a
            // slot of its own, which is not this one's to give.
            self.leave_queue();
            self.inner.drivers().remove(&self.session_id);
        }
        self.abort_attaching();
        self.abort_shipping();
        self.end_polling(Err(no_longer_active()));
        // Off the map, so nothing new arrives; what is queued came too late.
        self.commands.close();
        while let Ok(command) = self.commands.try_recv() {
            command.refuse(no_longer_active());
        }
    }

    /// Runs until the session has no agent, no preparation and nothing to do.
    /// Returns true when the driver already took itself off the map.
    async fn drive(&mut self) -> bool {
        if let Err(err) = self.load().await {
            tracing::error!(session = %self.session_id, %err, "could not load the session");
            return false;
        }
        loop {
            // A run and a place in the queue only exist in this task.
            let idle = self.handle.is_none()
                && self.preparation.is_none()
                && self.attaching.is_none()
                && self.shipping.is_none()
                && self.polling.is_none()
                && self.parked.is_none()
                && self.run.is_none();
            let step = if idle {
                match self.commands.try_recv() {
                    Ok(command) => Step::Command(command),
                    Err(_) => {
                        let mut drivers = self.inner.drivers();
                        if self.commands.is_empty() {
                            // A driver with nothing to do runs nothing.
                            if self.setup.dispatch.is_none() {
                                self.inner.queue.leave(&self.session_id);
                            }
                            drivers.remove(&self.session_id);
                            return true;
                        }
                        continue;
                    }
                }
            } else {
                tokio::select! {
                    command = self.commands.recv() => match command {
                        Some(command) => Step::Command(command),
                        None => return false,
                    },
                    output = next_output(&mut self.handle) => Step::Output(output),
                    prepared = next_prepared(&mut self.preparation) => Step::Prepared(prepared),
                    attached = next_attached(&mut self.attaching) => Step::Attached(attached),
                    shipped = next_shipped(&mut self.shipping) => Step::Shipped(shipped),
                    polled = next_polled(&mut self.polling) => Step::Polled(polled),
                    granted = next_granted(&mut self.parked) => Step::Granted(granted),
                    worked = next_worked(&mut self.run) => Step::Worked(worked),
                }
            };
            let result = match step {
                Step::Command(command) => self.on_command(command).await,
                Step::Output(output) => self.on_output(output).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record agent output");
                    Halt
                }),
                Step::Prepared(prepared) => self.on_prepared(prepared).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record the session's repos");
                    Halt
                }),
                Step::Attached(attached) => self.on_attached(attached).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record an attached repo");
                    Halt
                }),
                Step::Shipped(shipped) => self.on_shipped(shipped).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record a step of the ship");
                    Halt
                }),
                Step::Polled(observed) => self.on_polled(observed).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record the pull requests");
                    Halt
                }),
                Step::Granted(granted) => self.on_granted(granted).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not start the session from the queue");
                    Halt
                }),
                Step::Worked(worked) => self.on_worked(worked).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record a step of the workflow");
                    Halt
                }),
            };
            if result.is_err() {
                return false;
            }
            // Whatever the step ended of the workflow run, the run goes on from.
            if let Err(err) = self.advance_run().await {
                tracing::error!(session = %self.session_id, %err, "could not go on with the workflow");
                return false;
            }
            if std::mem::take(&mut self.flush_pending)
                && !self.defers_input()
                && let Err(err) = self.run_pending().await
            {
                tracing::error!(session = %self.session_id, %err, "could not go on after the workflow");
                return false;
            }
            // A slot is for a session that prepares or runs. Every path
            // above gives it back with the status; this holds for the one
            // that does not.
            if !self.status.is_active() && self.parked.is_none() {
                self.leave_queue();
            }
        }
    }

    async fn load(&mut self) -> Result<()> {
        let detail = self.inner.store.session(&self.session_id).await?;
        self.setup = self.inner.store.setup(&self.session_id).await?;
        self.status = detail.session.status;
        self.title = detail.session.title;
        self.prompt = detail.session.prompt;
        self.issue = detail.session.issue;
        self.root_session_id = detail.session.root_session_id;
        self.runner_id = detail.session.runner;
        self.permission_mode = detail.session.permission_mode;
        self.head_seq = detail.session.head_seq;
        self.repos = detail.repos;
        self.pull_requests = detail.pull_requests;
        self.lead = detail
            .agents
            .into_iter()
            .rev()
            .find(|agent| agent.role == LEAD_ROLE)
            .map(|agent| Lead {
                id: agent.id,
                status: agent.status,
                native_session_id: agent.native_session_id,
                model: agent.model,
            });
        // A question outlives the driver that recorded it: the agent may
        // have exited while the user was still thinking.
        let open = self.inner.store.open_questions(Some(&self.session_id));
        self.question = open.await?.into_iter().next();
        // Missing them is better than not starting at all.
        match self.unheard_reports().await {
            Ok(notes) => self.notes = notes,
            Err(err) => {
                tracing::warn!(session = %self.session_id, %err, "could not look for reports the agent never got");
            }
        }
        Ok(())
    }

    fn manager(&self) -> SessionManager {
        SessionManager {
            inner: Arc::clone(&self.inner),
        }
    }

    async fn on_command(&mut self, command: Command) -> std::result::Result<(), Halt> {
        match command {
            Command::Start {
                runner,
                reservation,
                brief,
                workflow,
                reply,
            } => {
                self.runner = Some(runner);
                self.workflow = workflow;
                self.start(reservation, brief, reply).await
            }
            Command::Dispatch {
                agent_id,
                title,
                brief,
                repos,
                reply,
            } => {
                let with = ChildWith::default();
                let result = self.dispatch(agent_id, title, brief, repos, with).await;
                self.answer_with(reply, result)
            }
            Command::Note {
                agent_id,
                to,
                text,
                reply,
            } => {
                let result = self.note(agent_id, to, text).await;
                self.answer_with(reply, result)
            }
            Command::Report { summary, reply } => {
                let result = self.keep_report(summary);
                self.answer(reply, result)
            }
            Command::Ask {
                agent_id,
                question,
                options,
                reply,
            } => {
                let result = self.ask(agent_id, question, options).await;
                self.answer_with(reply, result)
            }
            Command::Answer {
                question_id,
                answer,
                comment,
                reply,
            } => {
                let result = self.answer_question(question_id, answer, comment).await;
                self.answer(reply, result)
            }
            Command::Deliver {
                message,
                cause,
                text,
                ended,
            } => self.deliver(message, cause, text, ended).await.map_err(|err| {
                tracing::error!(session = %self.session_id, %err, "could not record a delivered message");
                Halt
            }),
            Command::Spent => self.check_budget().await.map_err(|err| {
                tracing::error!(session = %self.session_id, %err, "could not check the workflow's budget");
                Halt
            }),
            Command::Message { text, reply } => {
                let result = self.message(text).await;
                self.answer(reply, result)
            }
            Command::Interrupt { reply } => {
                let result = self.interrupt().await;
                self.answer(reply, result)
            }
            Command::Stop { cascade, reply } => {
                let result = self.stop(cascade).await;
                self.answer(reply, result)
            }
            Command::Permission {
                request_id,
                decision,
                message,
                reply,
            } => {
                let result = self.answer_permission(request_id, decision, message).await;
                self.answer(reply, result)
            }
            Command::Attach {
                request,
                agent_id,
                reply,
            } => {
                let queued = QueuedAttach {
                    request,
                    agent_id,
                    reply,
                };
                if self.attaching.is_some() {
                    self.attach_queue.push_back(queued);
                    return Ok(());
                }
                self.begin_attach(queued).await
            }
            Command::Ship {
                title,
                body,
                draft,
                reply,
            } => {
                let result = self.begin_ship(title, body, draft);
                self.answer(reply, result)
            }
            Command::Poll { reply } => self.begin_poll(reply).await.map_err(|err| {
                tracing::error!(session = %self.session_id, %err, "could not record the pull requests");
                Halt
            }),
        }
    }

    /// Sends the result of a command to its caller. A refusal is an answer
    /// like any other; anything else means the driver cannot go on.
    fn answer(&self, reply: Reply, result: Result<()>) -> std::result::Result<(), Halt> {
        self.answer_with(reply, result)
    }

    fn answer_with<T>(&self, reply: Reply<T>, result: Result<T>) -> std::result::Result<(), Halt> {
        let halt = match &result {
            Err(err) if !err.is_refusal() => {
                tracing::error!(session = %self.session_id, %err, "session command failed");
                Err(Halt)
            }
            _ => Ok(()),
        };
        let _ = reply.send(result);
        halt
    }

    async fn on_output(&mut self, output: Option<RunnerOutput>) -> Result<()> {
        match output {
            Some(RunnerOutput::Event(body)) => self.agent_event(body, None).await,
            Some(RunnerOutput::SubagentEvent { key, body }) => {
                self.agent_event(body, Some(&key)).await
            }
            Some(RunnerOutput::SubagentStarted(start)) => self.subagent_started(start).await,
            Some(RunnerOutput::SubagentEnded { key, ok }) => self.subagent_ended(&key, ok).await,
            Some(RunnerOutput::PermissionRequest(ask)) => self.permission_requested(ask).await,
            Some(RunnerOutput::NativeSession { id, model }) => self.native_session(id, model).await,
            Some(RunnerOutput::Limits(windows)) => self.limits(windows).await,
            Some(RunnerOutput::TurnEnded) => self.turn_ended().await,
            Some(RunnerOutput::Exited(Exit::Ok)) => self.agent_exited().await,
            Some(RunnerOutput::Exited(Exit::Error(message))) => self.fail(message).await,
            None => {
                self.fail("the agent ended without a result".to_owned())
                    .await
            }
        }
    }

    /// Records the prompt, answers the caller, then goes on to the repos and
    /// the first turn without making the caller wait for either.
    async fn start(
        &mut self,
        reservation: Option<Reservation>,
        brief: Option<(AgentMessage, String)>,
        reply: Reply<SessionDetail>,
    ) -> std::result::Result<(), Halt> {
        let dispatched = brief.is_some();
        let recorded = async {
            // A session that has to wait says so from its first event on.
            let reason = (!self.take_slot()).then(|| WAITING_FOR_SLOT.to_owned());
            self.set_status(SessionStatus::Queued, reason).await?;
            let prompt = match brief {
                // A child's prompt is the brief it was dispatched with: it
                // arrives as a message, caused by the one its parent sent.
                Some((message, sent)) => {
                    EventDraft::new(EventBody::MessageReceived { message }).parent(Some(sent))
                }
                None => EventDraft::new(EventBody::UserMessage {
                    text: self.prompt.clone(),
                }),
            };
            let message = self.append(prompt).await?;
            let detail = self.inner.store.session(&self.session_id).await?;
            Ok::<_, Error>((message.id.clone(), detail))
        }
        .await;
        let cause = match recorded {
            Ok((cause, detail)) => {
                let _ = reply.send(Ok(detail));
                cause
            }
            Err(err) => {
                tracing::error!(session = %self.session_id, %err, "could not record the new session");
                let _ = reply.send(Err(err));
                return Err(Halt);
            }
        };
        let prompt = if dispatched {
            let parent = self.parent_title().await.unwrap_or_default();
            briefing::dispatch_text(&parent, &self.prompt)
        } else {
            self.task()
        };
        self.launch(prompt, cause, reservation)
            .await
            .map_err(|err| {
                tracing::error!(session = %self.session_id, %err, "could not start the session");
                Halt
            })
    }

    /// What the session asks of its agent: the prompt the user wrote, then
    /// what only the agent is handed (the issue it starts from).
    fn task(&self) -> String {
        briefing::with_context(&self.prompt, self.setup.context.as_deref())
    }

    /// Keeps the agent's report of the plan limits and logs it when the
    /// rule of the integrations spec, section 1, says to.
    async fn limits(&mut self, windows: Vec<LimitWindow>) -> Result<()> {
        let Some(agent_id) = self.lead.as_ref().map(|lead| lead.id.clone()) else {
            return Ok(());
        };
        let now = self.inner.config.clock.now_ms();
        let board = &self.inner.limits;
        if board.report(&self.runner_id, &self.session_id, &agent_id, &windows, now) {
            let draft = self.by_agent(EventBody::Limits { windows });
            self.append(draft.parent(self.cause.clone())).await?;
        }
        Ok(())
    }

    /// The title of the session that dispatched this one, when one did.
    async fn parent_title(&self) -> Option<String> {
        let parent = &self.setup.dispatch.as_ref()?.parent_session_id;
        let detail = self.inner.store.session(parent).await.ok()?;
        Some(detail.session.title)
    }

    async fn message(&mut self, text: String) -> Result<()> {
        if !takes_messages(self.status) {
            return Err(closed(self.status));
        }
        let message = self
            .append(EventDraft::new(EventBody::UserMessage {
                text: text.clone(),
            }))
            .await?;
        self.pending.push_back(Queued {
            text,
            event_id: message.id.clone(),
        });
        if self.defers_input() {
            return Ok(());
        }
        self.run_pending().await
    }

    /// True while an input for the lead agent has to wait: for the turn in
    /// progress, for the repos, for a slot, for a ship (which commits and
    /// pushes what is in the worktrees, so the agent stays out of them), or
    /// for a workflow node that is not the lead's to end.
    fn defers_input(&self) -> bool {
        self.in_turn
            || self.preparation.is_some()
            || self.shipping.is_some()
            || self.parked.is_some()
            || self.run_holds_lead()
    }

    /// True when the session may be `preparing` or `running`: it holds a
    /// queue slot, or took a free one just now. A child session always may:
    /// the queue is for root sessions.
    fn take_slot(&self) -> bool {
        self.setup.dispatch.is_some() || self.inner.queue.try_acquire(&self.session_id)
    }

    /// Gives the session's slot back, or its place in the queue.
    fn leave_queue(&mut self) {
        self.parked = None;
        if self.setup.dispatch.is_none() {
            self.inner.queue.leave(&self.session_id);
        }
    }

    /// Joins the queue: the session is `queued` until a slot is free, then
    /// goes on with `then`.
    async fn park(&mut self, then: Resume) -> Result<()> {
        let granted = match self.inner.queue.acquire(&self.session_id) {
            Slot::Wait(granted) => granted,
            // One was freed since the caller looked: the wait is over
            // before it began.
            Slot::Held => {
                let (now, granted) = oneshot::channel();
                let _ = now.send(());
                granted
            }
        };
        self.parked = Some(Parked { granted, then });
        if self.status != SessionStatus::Queued {
            let reason = Some(WAITING_FOR_SLOT.to_owned());
            self.set_status(SessionStatus::Queued, reason).await?;
        }
        Ok(())
    }

    /// The session holds a slot now: it goes on with what waited for one.
    async fn on_granted(&mut self, granted: bool) -> Result<()> {
        let Some(parked) = self.parked.take() else {
            return Ok(());
        };
        if !granted {
            // The queue lost track of the session; join it again.
            return self.park(parked.then).await;
        }
        match parked.then {
            Resume::Launch {
                text,
                cause,
                reservation,
            } => self.launch(text, cause, reservation).await,
            Resume::Turn if self.in_turn => {
                self.resume_status().await?;
                self.forward_held();
                Ok(())
            }
            Resume::Turn if self.pending.is_empty() || self.run_holds_lead() => self.rest().await,
            Resume::Turn => self.run_pending().await,
            Resume::Action(action) => self.begin(*action).await,
        }
    }

    /// Hands the agent the decisions that waited for a slot with the session.
    fn forward_held(&mut self) {
        for (open, decision, message) in std::mem::take(&mut self.held) {
            self.forward_decision(open, decision, message);
        }
    }

    /// The id of the lead agent, also before it starts.
    fn lead_agent_id(&mut self) -> String {
        match &self.lead {
            Some(lead) => lead.id.clone(),
            None => self
                .reserved_lead
                .get_or_insert_with(|| Ulid::generate().to_string())
                .clone(),
        }
    }

    /// Starts a turn on the message that waited longest, if one waits.
    async fn run_pending(&mut self) -> Result<()> {
        // The front of the queue is the newest message, unless an interrupt
        // or a ship held earlier ones back: those run first.
        let Some(next) = self.pending.pop_front() else {
            return Ok(());
        };
        let text = if self.lead.is_some() {
            next.text
        } else {
            // No agent ever saw the prompt (the session failed before one
            // started), so the message alone would have no context.
            format!("{}\n\n{}", self.task(), next.text)
        };
        self.launch(text, next.event_id, None).await
    }

    /// Runs a turn on `text`, after making the session's repos ready if some
    /// are not yet.
    async fn launch(
        &mut self,
        text: String,
        cause: String,
        reservation: Option<Reservation>,
    ) -> Result<()> {
        if !self.take_slot() {
            let waits = Resume::Launch {
                text,
                cause,
                reservation,
            };
            return self.park(waits).await;
        }
        self.cause = Some(cause.clone());
        // By repo, not by count: repos attached later are in the list too.
        let attached = |wanted: &AttachRepoRequest| {
            let same = |repo: &SessionRepo| repo.repo_id == wanted.repo_id;
            self.repos.iter().any(same)
        };
        let wanted: Vec<AttachRepoRequest> = self
            .setup
            .repos
            .iter()
            .filter(|wanted| !attached(wanted))
            .cloned()
            .collect();
        if wanted.is_empty() {
            return self.first_turn(text, cause).await;
        }
        self.set_status(SessionStatus::Preparing, None).await?;
        // The first repo the session was created with is where its agent works.
        let first = self.setup.repos.first().map(|repo| repo.repo_id.clone());
        let mut plans = Vec::with_capacity(wanted.len());
        for wanted in wanted {
            let Some(repo) = self.inner.repos.get(&wanted.repo_id).await else {
                let message = format!("repo {} is no longer registered", wanted.repo_id);
                return self.fail(message).await;
            };
            let primary = first.as_ref() == Some(&repo.id);
            plans.push(self.plan(repo, wanted.mode.unwrap_or_default(), primary));
        }
        let (results, receiver) = mpsc::channel(plans.len().max(1));
        let cancel = Arc::new(AtomicBool::new(false));
        tokio::spawn(prepare(plans, results, Arc::clone(&cancel)));
        self.preparation = Some(Preparation {
            results: receiver,
            cancel,
            text,
            cause,
            _reservation: reservation,
        });
        Ok(())
    }

    /// The repos are ready: the session's workflow starts if it has one that
    /// did not start yet, else a turn on `text`.
    async fn first_turn(&mut self, text: String, cause: String) -> Result<()> {
        match self.workflow.take() {
            Some(workflow) => self.start_run(*workflow, cause).await,
            None => self.begin_turn(text, cause).await,
        }
    }

    /// How to make `repo` ready for this session.
    fn plan(&self, repo: Repo, mode: RepoMode, primary: bool) -> AttachPlan {
        let work_dir = self.inner.store.home().work_dir(&self.session_id);
        let repo_id = repo.id.clone();
        AttachPlan {
            session_id: self.session_id.clone(),
            worktree: work_dir.join(&repo.name),
            repo,
            mode,
            branch: calin_git::session_branch_name(&self.title, &self.session_id),
            primary,
            start_point: self
                .setup
                .dispatch
                .as_ref()
                .and_then(|dispatch| dispatch.start_points.get(&repo_id).cloned()),
            setup_limit: self.inner.config.setup_limit,
        }
    }

    async fn on_prepared(&mut self, prepared: Option<Prepared>) -> Result<()> {
        match prepared {
            Some(Prepared::Repo(repo)) => self.attach(repo, None).await,
            Some(Prepared::SetupStarted(item)) => self.setup_started(item).await,
            Some(Prepared::SetupEnded { item, failure }) => self.setup_ended(item, failure).await,
            Some(Prepared::Failed { repo, error }) => {
                self.preparation = None;
                self.fail(format!("could not prepare repo {repo}: {error}"))
                    .await
            }
            None => match self.preparation.take() {
                Some(preparation) => self.first_turn(preparation.text, preparation.cause).await,
                None => Ok(()),
            },
        }
    }

    /// Records a repo that is ready. `agent_id` is the agent that asked for
    /// it through Calin's tools, if one did.
    async fn attach(&mut self, repo: SessionRepo, agent_id: Option<&str>) -> Result<()> {
        let mut attached =
            EventDraft::new(EventBody::RepoAttached { repo }).parent(self.cause.clone());
        if let Some(agent_id) = agent_id {
            attached = attached.agent(agent_id);
        }
        self.append(attached).await?;
        Ok(())
    }

    /// The setup command shows in the transcript as a tool call nobody but
    /// the orchestrator made.
    async fn setup_started(&mut self, item: Item) -> Result<()> {
        let started = EventBody::ItemStarted { item };
        self.append(EventDraft::new(started).parent(self.cause.clone()))
            .await?;
        Ok(())
    }

    /// A setup that failed is said, and the session goes on: the agent can
    /// often work without it, or run it again itself.
    async fn setup_ended(&mut self, item: Item, failure: Option<String>) -> Result<()> {
        let completed = EventBody::ItemCompleted { item };
        self.append(EventDraft::new(completed).parent(self.cause.clone()))
            .await?;
        if let Some(message) = failure {
            let error = EventBody::Error { message };
            self.append(EventDraft::new(error).parent(self.cause.clone()))
                .await?;
        }
        Ok(())
    }

    /// Starts making a repo ready for a session that is already at work.
    /// The caller is answered when the repo is ready, or at once when the
    /// request cannot be met.
    async fn begin_attach(&mut self, queued: QueuedAttach) -> std::result::Result<(), Halt> {
        let QueuedAttach {
            request,
            agent_id,
            reply,
        } = queued;
        match self.plan_attach(&request).await {
            Ok((plan, reservation)) => {
                let (results, receiver) = mpsc::channel(4);
                let cancel = Arc::new(AtomicBool::new(false));
                tokio::spawn(prepare(vec![plan], results, Arc::clone(&cancel)));
                self.attaching = Some(Attaching {
                    results: receiver,
                    cancel,
                    agent_id,
                    reply,
                    attached: None,
                    _reservation: reservation,
                });
                Ok(())
            }
            Err(err) => {
                let halt = if err.is_refusal() { Ok(()) } else { Err(Halt) };
                if halt.is_err() {
                    tracing::error!(session = %self.session_id, %err, "could not attach a repo");
                }
                let _ = reply.send(Err(err));
                halt
            }
        }
    }

    async fn plan_attach(&self, request: &AttachRepoRequest) -> Result<(AttachPlan, Reservation)> {
        if !takes_repos(self.status) {
            return Err(no_attaching(self.status));
        }
        let Some(repo) = self.inner.repos.get(&request.repo_id).await else {
            return Err(Error::Missing(format!("unknown repo {}", request.repo_id)));
        };
        if let Some(attached) = self.repos.iter().find(|r| r.repo_id == repo.id) {
            return Err(Error::Conflict(format!(
                "{} is already attached to this session, at {}",
                attached.name, attached.path
            )));
        }
        // The name is the repo's folder in the session.
        if self.repos.iter().any(|attached| attached.name == repo.name) {
            return Err(Error::Conflict(format!(
                "another repo named {} is attached to this session",
                repo.name
            )));
        }
        // Refuses a repo without a clone, and holds the others in place.
        let reservation = self.inner.repos.reserve(&[&repo.id]).await?;
        // Never the primary: the agent already works somewhere.
        let plan = self.plan(repo, request.mode.unwrap_or_default(), false);
        Ok((plan, reservation))
    }

    async fn on_attached(&mut self, prepared: Option<Prepared>) -> Result<()> {
        match prepared {
            Some(Prepared::Repo(repo)) => {
                let agent_id = self.attaching.as_ref().and_then(|a| a.agent_id.clone());
                self.attach(repo.clone(), agent_id.as_deref()).await?;
                if let Some(attaching) = &mut self.attaching {
                    attaching.attached = Some(repo);
                }
                Ok(())
            }
            Some(Prepared::SetupStarted(item)) => self.setup_started(item).await,
            Some(Prepared::SetupEnded { item, failure }) => self.setup_ended(item, failure).await,
            Some(Prepared::Failed { repo, error }) => {
                tracing::warn!(session = %self.session_id, %repo, %error, "could not attach a repo");
                if let Some(attaching) = self.attaching.take() {
                    let _ = attaching.reply.send(Err(error));
                }
                self.next_attach().await
            }
            None => {
                if let Some(attaching) = self.attaching.take() {
                    let result = attaching.attached.ok_or_else(no_longer_active);
                    // An agent that asked is told by its tool's result. One
                    // that did not is told with the next message, unless it
                    // is not running: its next start names every repo.
                    if let (Ok(repo), None, true) =
                        (&result, &attaching.agent_id, self.handle.is_some())
                    {
                        self.notes.push(briefing::attach_note(repo));
                    }
                    // Tracked from now on, before anyone is told it is there.
                    let tracker = unreported::tracker(&self.inner.trackers, &self.session_id);
                    if let (Ok(repo), Some(tracker)) = (&result, tracker) {
                        tracker.lock().await.attached(&self.session_id, repo).await;
                    }
                    let _ = attaching.reply.send(result);
                }
                self.next_attach().await
            }
        }
    }

    /// Takes up the next attach request that waited for the one that ended.
    async fn next_attach(&mut self) -> Result<()> {
        while self.attaching.is_none() {
            let Some(queued) = self.attach_queue.pop_front() else {
                break;
            };
            if self.begin_attach(queued).await.is_err() {
                return Err(Error::Closed);
            }
        }
        Ok(())
    }

    /// Ends the attach in progress, as far as it can be ended, and answers
    /// everyone who waits for one.
    fn abort_attaching(&mut self) {
        if let Some(attaching) = self.attaching.take() {
            attaching.cancel.store(true, Ordering::Relaxed);
            let _ = attaching
                .reply
                .send(attaching.attached.ok_or_else(no_longer_active));
        }
        for queued in self.attach_queue.drain(..) {
            let _ = queued.reply.send(Err(no_longer_active()));
        }
    }

    /// Starts a turn on `text`, starting the agent first if it is not running.
    async fn begin_turn(&mut self, text: String, cause: String) -> Result<()> {
        // A session that waits on the user stays `waiting` through the turn.
        if !self.waits_on_user() && !self.take_slot() {
            let waits = Resume::Launch {
                text,
                cause,
                reservation: None,
            };
            return self.park(waits).await;
        }
        self.cause = Some(cause);
        self.interrupted = false;
        self.resume_status().await?;
        // What the agent is handed, which may say more than the user wrote.
        let text = briefing::with_notes(std::mem::take(&mut self.notes), text);
        // What the repos hold now is not this turn's doing: neither what was
        // there when the agent starts, nor what the user changed in a
        // terminal or another editor since its last turn.
        let mut tracker = unreported::fresh(&self.inner.trackers, &self.session_id).await;
        unreported::baseline(&mut tracker, &self.session_id, &self.repos).await;
        drop(tracker);
        if let Some(handle) = &self.handle {
            let sent = handle.send_message(text);
            if sent.is_err() {
                return self.fail("the agent is no longer running".to_owned()).await;
            }
            self.set_agent_status(AgentStatus::Running).await?;
        } else {
            let runner = self
                .runner
                .clone()
                .or_else(|| self.inner.registry.resolve(Some(&self.runner_id)));
            let Some(runner) = runner else {
                let message = format!("runner {} is not available", self.runner_id);
                return self.fail(message).await;
            };
            let work_dir = self.inner.store.home().work_dir(&self.session_id);
            let cwd = match working_directory(&work_dir, &self.repos).await {
                Ok(cwd) => cwd,
                Err(err) => {
                    let message = format!("the working directory cannot be used: {err}");
                    return self.fail(message).await;
                }
            };
            let main = primary(&self.repos).map(|repo| repo.id.clone());
            // A repo attached `read` is not a directory to work in: the
            // agent asks before it touches one, and the policy answers.
            let open: Vec<&SessionRepo> = self
                .repos
                .iter()
                .filter(|repo| Some(&repo.id) == main.as_ref() || repo.mode != RepoMode::Read)
                .collect();
            let add_dirs = open
                .iter()
                .filter(|repo| Some(&repo.id) != main.as_ref())
                .map(|repo| PathBuf::from(&repo.path))
                .collect();
            let granted = open.iter().map(|repo| repo.id.clone()).collect();
            // An agent that accepts edits does not ask about the directory
            // it works in. When that is a repo attached `read`, it has to ask
            // about every edit, so that the policy can refuse those.
            let read_only_home = primary(&self.repos).is_some_and(|r| r.mode == RepoMode::Read);
            let asks = match self.permission_mode {
                PermissionMode::AcceptEdits if read_only_home => PermissionMode::Ask,
                mode => mode,
            };
            let agent_id = match (&self.lead, &self.setup.dispatch) {
                (Some(lead), _) => Some(lead.id.clone()),
                // The id its dispatch message was addressed to.
                (None, Some(dispatch)) => Some(dispatch.lead_agent_id.clone()),
                (None, None) => None,
            };
            let agent_id = agent_id.unwrap_or_else(|| self.lead_agent_id());
            // A token of its own for every start; the previous one is revoked here.
            self.token = self.inner.tool_endpoint().and_then(|_| {
                self.inner.tokens.issue(AgentRef {
                    session_id: self.session_id.clone(),
                    agent_id: agent_id.clone(),
                })
            });
            let tool_endpoint = match (self.inner.tool_endpoint(), &self.token) {
                (Some(base), Some(token)) => Some(format!("{base}/{}", token.value())),
                _ => None,
            };
            let parent_title = self.parent_title().await;
            let child = self.setup.dispatch.as_ref().map(|_| ChildOf {
                parent_title: parent_title.as_deref().unwrap_or_default(),
            });
            let system_prompt =
                briefing::system_prompt(&self.repos, &cwd, tool_endpoint.is_some(), child);
            let started = runner.start(StartParams {
                session_id: self.session_id.clone(),
                agent_id: agent_id.clone(),
                prompt: text,
                cwd,
                add_dirs,
                permission_mode: asks,
                model: self.setup.model.clone(),
                resume: self
                    .lead
                    .as_ref()
                    .and_then(|lead| lead.native_session_id.clone()),
                start_seq: self.head_seq,
                tool_endpoint,
                system_prompt: Some(system_prompt),
            });
            match started {
                Ok(handle) => {
                    self.handle = Some(handle);
                    self.granted = granted;
                }
                Err(err) => return self.fail(err.to_string()).await,
            }
            if self.lead.is_some() {
                self.set_agent_status(AgentStatus::Running).await?;
            } else {
                let model = self.setup.model.clone().or_else(|| runner.model());
                let agent = Agent {
                    id: agent_id.clone(),
                    session_id: self.session_id.clone(),
                    parent_agent_id: None,
                    kind: AgentKind::Process,
                    role: LEAD_ROLE.to_owned(),
                    runner: runner.id().to_owned(),
                    model: model.clone(),
                    native_session_id: None,
                    status: AgentStatus::Running,
                    cost_usd: 0.0,
                    tokens_in: 0,
                    tokens_out: 0,
                };
                let spawned = EventBody::AgentSpawned { agent };
                self.append(EventDraft::new(spawned).parent(self.cause.clone()))
                    .await?;
                self.lead = Some(Lead {
                    id: agent_id,
                    status: AgentStatus::Running,
                    native_session_id: None,
                    model,
                });
            }
        }
        self.in_turn = true;
        Ok(())
    }

    async fn turn_ended(&mut self) -> Result<()> {
        if !self.in_turn {
            return Ok(());
        }
        // The runner voided what was still unanswered; say so in the log.
        self.expire_permissions().await?;
        self.held.clear();
        self.turn_items.clear();
        let interrupted = std::mem::take(&mut self.interrupted);
        self.capture_lead(interrupted);
        // The next turn follows at once when the session may run. One that
        // may not waits for a slot, with what it was handed.
        let follows = !interrupted && !self.pending.is_empty();
        let runs = self.waits_on_user()
            || (self.parked.is_none()
                && (self.status == SessionStatus::Running || (follows && self.take_slot())));
        if follows && !runs {
            self.in_turn = false;
            let ended = self.set_agent_status(AgentStatus::Idle).await?;
            self.report_unreported(ended).await?;
            if self.parked.is_none() {
                self.park(Resume::Turn).await?;
            }
            return Ok(());
        }
        if !interrupted && let Some(next) = self.pending.pop_front() {
            // The log has no event for the end of a turn but the agent's
            // status: one turn ends and the next begins, each with its cause.
            let ended = self.set_agent_status(AgentStatus::Idle).await?;
            self.report_unreported(ended).await?;
            self.cause = Some(next.event_id);
            self.set_agent_status(AgentStatus::Running).await?;
            self.resume_status().await?;
            let text = briefing::with_notes(std::mem::take(&mut self.notes), next.text);
            let sent = self
                .handle
                .as_ref()
                .is_some_and(|handle| handle.send_message(text).is_ok());
            if !sent {
                return self.fail("the agent is no longer running".to_owned()).await;
            }
            return Ok(());
        }
        self.in_turn = false;
        let ended = self.set_agent_status(AgentStatus::Idle).await?;
        self.report_unreported(ended).await?;
        self.rest().await
    }

    /// Appends a `file_changed` for each change the turn made that no tool
    /// call reported, caused by `ended`, the event that ended the turn. The
    /// shell does not say which subagent ran a command: they are the lead's.
    async fn report_unreported(&mut self, ended: Option<String>) -> Result<()> {
        let Some(tracker) = unreported::tracker(&self.inner.trackers, &self.session_id) else {
            return Ok(());
        };
        let tracker = tracker.lock_owned().await;
        self.report_unreported_with(tracker, ended).await
    }

    async fn report_unreported_with(
        &mut self,
        mut tracker: tokio::sync::OwnedMutexGuard<Tracker>,
        ended: Option<String>,
    ) -> Result<()> {
        let repos = self.repos.clone();
        // Held while appending, so that a save in between is not lost.
        let found = tracker.end_turn(&self.session_id, &repos).await;
        let parent = ended.or_else(|| self.cause.clone());
        for (repo, changes) in found {
            let more = changes.len().saturating_sub(MAX_CHANGES_PER_TURN);
            for (path, op) in changes.into_iter().take(MAX_CHANGES_PER_TURN) {
                let changed = EventBody::FileChanged {
                    session_repo_id: Some(repo.id.clone()),
                    path,
                    op,
                };
                self.append(self.by_agent(changed).parent(parent.clone()))
                    .await?;
            }
            if more > 0 {
                let error = EventBody::Error {
                    message: format!(
                        "{more} more files changed in {} during this turn than are listed \
                         one by one; the Changes view shows them all.",
                        repo.name
                    ),
                };
                self.append(self.by_agent(error).parent(parent.clone()))
                    .await?;
            }
        }
        Ok(())
    }

    /// The agent has nothing left to do for now: the session is in `review`
    /// and, when it is a child, reports. A session with a question open
    /// keeps waiting for its answer instead.
    async fn rest(&mut self) -> Result<()> {
        if self.question.is_some() {
            if self.status != SessionStatus::Waiting {
                let reason = Some(WAITING_FOR_ANSWER.to_owned());
                self.set_status(SessionStatus::Waiting, reason).await?;
            }
            return Ok(());
        }
        if self.run.is_some() {
            // The lead resting ends its node, not the session.
            self.lead_rested();
            return Ok(());
        }
        if self.status == SessionStatus::Review {
            return Ok(());
        }
        let owes_report = !self.status.is_settled();
        self.set_status(SessionStatus::Review, None).await?;
        if owes_report {
            self.send_report(Ending::Rested).await?;
        }
        Ok(())
    }

    /// True while the user owes the session an answer.
    fn waits_on_user(&self) -> bool {
        !self.open.is_empty() || self.question.is_some()
    }

    /// Moves a session whose turn goes on to `running`, unless it still
    /// waits on the user: it is `waiting` while a request or a question is
    /// open. Running takes a slot; without one the session is `queued`
    /// until there is one.
    async fn resume_status(&mut self) -> Result<()> {
        if self.waits_on_user() || self.status == SessionStatus::Running {
            return Ok(());
        }
        if !self.take_slot() {
            return self.park(Resume::Turn).await;
        }
        self.set_status(SessionStatus::Running, None).await
    }

    /// Forgets the agent that is gone: its handle, its token, and what was
    /// only ever true of that process.
    fn drop_agent(&mut self) {
        self.handle = None;
        unreported::forget(&self.inner.trackers, &self.session_id);
        self.token = None;
        self.granted.clear();
        // Its next start is told about every repo in its system prompt.
        self.notes.clear();
    }

    /// The tracker of the turn in progress, locked, taken before the agent
    /// is dropped: a turn that ends without its end (the agent exits, fails
    /// or is stopped) still has what it changed looked for, once the
    /// agent's status says how it ended. `None` between turns.
    async fn turn_tracker(&self) -> Option<tokio::sync::OwnedMutexGuard<Tracker>> {
        match unreported::tracker(&self.inner.trackers, &self.session_id) {
            Some(tracker) if self.in_turn => Some(tracker.lock_owned().await),
            _ => None,
        }
    }

    async fn agent_exited(&mut self) -> Result<()> {
        let tracker = self.turn_tracker().await;
        self.drop_agent();
        if self.in_turn {
            let interrupted = self.interrupted;
            self.capture_lead(interrupted);
        }
        self.in_turn = false;
        self.interrupted = false;
        self.turn_items.clear();
        self.held.clear();
        self.expire_permissions().await?;
        // Whatever it left running inside its process went with it.
        self.close_subagents(AgentStatus::Stopped).await?;
        let ended = self.set_agent_status(AgentStatus::Done).await?;
        if let Some(tracker) = tracker {
            self.report_unreported_with(tracker, ended).await?;
        }
        // A workflow node that is not the lead's holds what waits for it.
        if self.run_holds_lead() {
            return Ok(());
        }
        match self.pending.pop_front() {
            // Messages are still waiting: the next turn starts the agent again.
            Some(next) => self.begin_turn(next.text, next.event_id).await,
            None => self.rest().await,
        }
    }

    async fn interrupt(&mut self) -> Result<()> {
        if !self.in_turn {
            return Err(no_turn(self.status));
        }
        // An interrupted turn asks for nothing any more.
        self.expire_permissions().await?;
        self.held.clear();
        // The turn is about to end: a session without a slot does not join
        // the queue for that, it stays as it is until it rests.
        if self.parked.is_none() && !self.waits_on_user() && self.take_slot() {
            self.resume_status().await?;
        }
        self.interrupted = true;
        let asked = self
            .handle
            .as_ref()
            .is_some_and(|handle| handle.interrupt().is_ok());
        if !asked {
            return self.fail("the agent is no longer running".to_owned()).await;
        }
        Ok(())
    }

    /// Ends the agent and cancels the session. A child that had not come
    /// to rest since its last report tells its parent it was stopped,
    /// unless the parent is being stopped too (`cascade`).
    async fn stop(&mut self, cascade: bool) -> Result<()> {
        if is_closed(self.status) {
            return Err(closed(self.status));
        }
        let owes_report = !self.status.is_settled();
        // The run ends with the session (workflows spec, section 6).
        self.abort_run(Abort::Stopped).await?;
        self.workflow = None;
        // First: an agent waiting for a repo it asked for would otherwise
        // hold up its own stop.
        self.abort_attaching();
        if let Some(mut preparation) = self.preparation.take() {
            preparation.cancel.store(true, Ordering::Relaxed);
            // The git command already running cannot be recalled. Wait for
            // it, so its repo is on record and a later message can go on
            // from there.
            let grace = tokio::time::sleep(STOP_GRACE);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    prepared = preparation.results.recv() => match prepared {
                        Some(Prepared::Repo(repo)) => self.attach(repo, None).await?,
                        Some(Prepared::SetupStarted(item)) => self.setup_started(item).await?,
                        Some(Prepared::SetupEnded { item, failure }) => {
                            self.setup_ended(item, failure).await?;
                        }
                        Some(Prepared::Failed { .. }) | None => break,
                    },
                    () = &mut grace => break,
                }
            }
        }
        if let Some(mut shipping) = self.shipping.take() {
            shipping.cancel.store(true, Ordering::Relaxed);
            // The push or the `gh` call already running cannot be recalled.
            // Wait for it, so a pull request it opens is on record.
            let grace = tokio::time::sleep(STOP_GRACE);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    shipped = shipping.results.recv() => match shipped {
                        Some(shipped) => self.record_shipped(shipped).await?,
                        None => break,
                    },
                    () = &mut grace => break,
                }
            }
        }
        self.end_polling(Ok(()));
        self.expire_permissions().await?;
        self.expire_question().await?;
        if let Some(mut handle) = self.handle.take()
            && handle.stop().is_ok()
        {
            // Keep what the agent said before it noticed the request.
            let grace = tokio::time::sleep(STOP_GRACE);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    output = handle.next_output() => match output {
                        Some(RunnerOutput::Event(body)) => self.agent_event(body, None).await?,
                        Some(RunnerOutput::SubagentEvent { key, body }) => {
                            self.agent_event(body, Some(&key)).await?;
                        }
                        Some(RunnerOutput::SubagentStarted(start)) => {
                            self.subagent_started(start).await?;
                        }
                        Some(RunnerOutput::SubagentEnded { key, ok }) => {
                            self.subagent_ended(&key, ok).await?;
                        }
                        Some(RunnerOutput::PermissionRequest(ask)) => {
                            // Asked while stopping: nobody will answer it.
                            let open = self
                                .record_request(ask.request, ask.subagent.as_deref())
                                .await?;
                            self.expire(open).await?;
                        }
                        Some(RunnerOutput::Limits(windows)) => self.limits(windows).await?,
                        Some(RunnerOutput::NativeSession { id, model }) => {
                            self.native_session(id, model).await?;
                        }
                        Some(RunnerOutput::TurnEnded) => {}
                        Some(RunnerOutput::Exited(_)) | None => break,
                    },
                    // Dropping the handle ends an agent that ignores the request.
                    () = &mut grace => break,
                }
            }
        }
        let tracker = self.turn_tracker().await;
        self.drop_agent();
        self.in_turn = false;
        self.interrupted = false;
        self.pending.clear();
        self.held.clear();
        self.turn_items.clear();
        self.close_subagents(AgentStatus::Stopped).await?;
        let ended = self.set_agent_status(AgentStatus::Stopped).await?;
        // Before the session's last status: the changes belong to the turn.
        if let Some(tracker) = tracker {
            self.report_unreported_with(tracker, ended).await?;
        }
        self.set_status(SessionStatus::Cancelled, None).await?;
        if owes_report && !cascade {
            self.send_report(Ending::Stopped).await?;
        }
        Ok(())
    }

    /// Records that the session cannot go on and fails it.
    async fn fail(&mut self, message: String) -> Result<()> {
        // In a workflow run an agent that fails fails its node, and the run
        // decides what becomes of the session (workflows spec, section 3).
        if self.run.is_some() {
            return self.lead_lost(message).await;
        }
        self.workflow = None;
        let owes_report = !self.status.is_settled();
        let tracker = self.turn_tracker().await;
        self.drop_agent();
        self.abort_attaching();
        self.abort_shipping();
        self.preparation = None;
        self.in_turn = false;
        self.interrupted = false;
        self.pending.clear();
        self.held.clear();
        self.turn_items.clear();
        self.expire_permissions().await?;
        self.expire_question().await?;
        let mut error = EventDraft::new(EventBody::Error {
            message: message.clone(),
        })
        .parent(self.cause.clone());
        if let Some(lead) = &self.lead {
            error = error.agent(lead.id.clone());
        }
        self.append(error).await?;
        self.close_subagents(AgentStatus::Failed).await?;
        let ended = self.set_agent_status(AgentStatus::Failed).await?;
        if let Some(tracker) = tracker {
            self.report_unreported_with(tracker, ended).await?;
        }
        self.set_status(SessionStatus::Failed, Some(message.clone()))
            .await?;
        if owes_report {
            self.send_report(Ending::Failed(&message)).await?;
        }
        Ok(())
    }

    async fn permission_requested(&mut self, ask: PermissionAsk) -> Result<()> {
        let PermissionAsk {
            request,
            access,
            subagent,
        } = ask;
        let mut open = self.record_request(request, subagent.as_deref()).await?;
        if !self.in_turn {
            // A request lives no longer than its turn, and there is none.
            return self.expire(open).await;
        }
        let target = self.place(access).await?;
        if let Target::Files { repos, .. } = &target {
            open.repos = repos.iter().flatten().copied().collect();
        }
        let (decision, message) = match policy::decide(self.permission_mode, &target, &self.repos) {
            Verdict::Allow => (PermissionDecision::Allow, None),
            Verdict::Deny(message) => (PermissionDecision::Deny, Some(message)),
            Verdict::Ask => {
                self.open.push(open);
                if self.status != SessionStatus::Waiting {
                    let reason = Some(WAITING_FOR_PERMISSION.to_owned());
                    self.set_status(SessionStatus::Waiting, reason).await?;
                }
                return Ok(());
            }
        };
        let resolved = EventBody::PermissionResolved {
            request_id: open.request_id.clone(),
            decision,
            by: DecisionSource::Policy,
            message: message.clone(),
        };
        let draft = as_agent(open.agent_id.clone(), resolved);
        self.append(draft.parent(Some(open.event_id.clone())))
            .await?;
        self.forward_decision(open, decision, message);
        Ok(())
    }

    /// Appends `permission_requested`, caused by the tool item it is about
    /// when the runner named one. `subagent` is the runner's key of the
    /// subagent that asks, when one does.
    async fn record_request(
        &mut self,
        request: PermissionRequest,
        subagent: Option<&str>,
    ) -> Result<OpenRequest> {
        let request_id = request.id.clone();
        let parent = request
            .item_id
            .as_ref()
            .and_then(|item_id| self.turn_items.get(item_id))
            .map(|item| item.event_id.clone())
            .or_else(|| self.cause.clone());
        let requested = EventBody::PermissionRequested { request };
        let draft = self.by(subagent, requested).parent(parent);
        let agent_id = draft.agent_id.clone();
        let event = self.append(draft).await?;
        Ok(OpenRequest {
            request_id,
            event_id: event.id.clone(),
            repos: Vec::new(),
            agent_id,
        })
    }

    /// Finds the repos a request reaches into. Resolving links reads the disk.
    async fn place(&self, access: ToolAccess) -> Result<Target> {
        if !matches!(access, ToolAccess::Files { .. }) || self.repos.is_empty() {
            return Ok(policy::place(&access, &[]));
        }
        let repos = self.repos.clone();
        blocking(move || Ok(policy::place(&access, &repos))).await
    }

    async fn answer_permission(
        &mut self,
        request_id: String,
        decision: PermissionDecision,
        message: Option<String>,
    ) -> Result<()> {
        let store = &self.inner.store;
        // The `permission_requested` event: the cause of the answer.
        let waiting = self
            .open
            .iter()
            .position(|open| open.request_id == request_id)
            .map(|index| self.open.remove(index));
        let lead = self.lead.as_ref().map(|lead| lead.id.clone());
        let (asked, asker) = match &waiting {
            Some(open) => (Some(open.event_id.clone()), open.agent_id.clone()),
            None => match store
                .permission_resolved(&self.session_id, &request_id)
                .await?
            {
                None => {
                    return Err(Error::Missing(format!(
                        "unknown permission request {request_id}"
                    )));
                }
                Some(true) => {
                    return Err(Error::Conflict(
                        "this permission request was already answered".to_owned(),
                    ));
                }
                // Open in the log but not waiting here: its agent is gone.
                // The answer is still recorded, so the card closes.
                Some(false) => store
                    .open_permissions(Some(&self.session_id))
                    .await?
                    .into_iter()
                    .find(|open| open.request_id == request_id)
                    .map_or((None, lead), |open| (Some(open.event_id), open.agent_id)),
            },
        };
        let resolved = EventBody::PermissionResolved {
            request_id: request_id.clone(),
            decision,
            by: DecisionSource::User,
            message: message.clone(),
        };
        self.append(as_agent(asker, resolved).parent(asked)).await?;
        if let Some(open) = waiting {
            // The last answer the session waited for lets it run again,
            // which takes a slot. Without one the decision waits with the
            // session: handing it over is what lets the agent go on.
            let resumes = self.status == SessionStatus::Waiting && !self.waits_on_user();
            if resumes && !self.take_slot() {
                self.held.push((open, decision, message));
                return self.park(Resume::Turn).await;
            }
            self.forward_decision(open, decision, message);
        }
        if self.status == SessionStatus::Waiting {
            self.resume_status().await?;
        }
        Ok(())
    }

    /// Hands a decision to the agent. An allow also opens the repos the
    /// request reached into, when the agent was not started with them: it
    /// then stops asking about every file there.
    fn forward_decision(
        &mut self,
        open: OpenRequest,
        decision: PermissionDecision,
        message: Option<String>,
    ) {
        let Some(handle) = &self.handle else {
            return;
        };
        let mut grants = Vec::new();
        if decision == PermissionDecision::Allow {
            for repo in open.repos.iter().filter_map(|index| self.repos.get(*index)) {
                if self.granted.insert(repo.id.clone()) {
                    grants.push(DirGrant {
                        dir: PathBuf::from(&repo.path),
                        write: repo.mode != RepoMode::Read,
                    });
                }
            }
        }
        // An agent that is gone says so on its output, which is handled there.
        let _ = if grants.is_empty() {
            handle.resolve_permission(open.request_id, decision, message)
        } else {
            handle.allow_granting(open.request_id, grants)
        };
    }

    /// Closes every request still waiting for the user.
    async fn expire_permissions(&mut self) -> Result<()> {
        for open in std::mem::take(&mut self.open) {
            self.expire(open).await?;
        }
        Ok(())
    }

    async fn expire(&mut self, open: OpenRequest) -> Result<()> {
        let closed = as_agent(open.agent_id, expired(open.request_id)).parent(Some(open.event_id));
        self.append(closed).await?;
        Ok(())
    }

    /// Closes the question nobody answered: the session ended first.
    async fn expire_question(&mut self) -> Result<()> {
        if let Some(open) = self.question.take() {
            let closing =
                as_agent(open.agent_id, unanswered(open.question_id)).parent(Some(open.event_id));
            self.append(closing).await?;
        }
        Ok(())
    }

    /// Records the runner's conversation id and model when either is news.
    async fn native_session(&mut self, id: String, model: Option<String>) -> Result<()> {
        let Some(lead) = &self.lead else {
            return Ok(());
        };
        let new_id = lead.native_session_id.as_deref() != Some(id.as_str());
        let new_model = model.is_some() && model != lead.model;
        if !new_id && !new_model {
            return Ok(());
        }
        let announced = EventBody::AgentSession {
            native_session_id: id,
            model,
        };
        self.append(self.by_agent(announced).parent(self.cause.clone()))
            .await?;
        Ok(())
    }

    async fn set_status(&mut self, status: SessionStatus, reason: Option<String>) -> Result<()> {
        let body = EventBody::SessionStatus { status, reason };
        self.append(EventDraft::new(body).parent(self.cause.clone()))
            .await?;
        Ok(())
    }

    /// Moves the lead agent to `status`, unless it has none, is already
    /// there, or has ended for good. Returns the event's id when one was appended.
    async fn set_agent_status(&mut self, status: AgentStatus) -> Result<Option<String>> {
        let Some(lead) = &self.lead else {
            return Ok(None);
        };
        let restarting = status == AgentStatus::Running;
        if lead.status == status || (agent_ended(lead.status) && !restarting) {
            return Ok(None);
        }
        let draft = self
            .by_agent(EventBody::AgentStatus { status })
            .parent(self.cause.clone());
        Ok(Some(self.append(draft).await?.id.clone()))
    }

    /// A draft attributed to the lead agent, when there is one.
    fn by_agent(&self, body: EventBody) -> EventDraft {
        let draft = EventDraft::new(body);
        match &self.lead {
            Some(lead) => draft.agent(lead.id.clone()),
            None => draft,
        }
    }

    /// A draft attributed to the subagent the runner calls `subagent`, or
    /// to the lead when it names none or one that is not open.
    fn by(&self, subagent: Option<&str>, body: EventBody) -> EventDraft {
        match subagent.and_then(|key| self.subagents.get(key)) {
            Some(agent_id) => EventDraft::new(body).agent(agent_id.clone()),
            None => self.by_agent(body),
        }
    }

    /// Appends what the agent said (or, with `subagent`, what one of its
    /// subagents did), and what follows from it.
    async fn agent_event(&mut self, body: EventBody, subagent: Option<&str>) -> Result<()> {
        let draft = self.by(subagent, body).parent(self.cause.clone());
        let event = self.append(draft).await?;
        match &event.body {
            EventBody::ItemStarted { item } => {
                let tool = match &item.content {
                    ItemContent::Tool { name, .. } => Some(name.clone()),
                    _ => None,
                };
                let started = TurnItem {
                    event_id: event.id.clone(),
                    tool,
                };
                self.turn_items.insert(item.id.clone(), started);
            }
            EventBody::ItemCompleted { item } => {
                self.turn_items.remove(&item.id);
                if let (None, ItemContent::Text { text }) = (subagent, &item.content)
                    && !text.trim().is_empty()
                {
                    self.last_text = Some(text.clone());
                    self.lead_said(text);
                }
                self.file_changes(&event, item).await?;
            }
            // The tree costs more now: a workflow's budget is checked here,
            // and in the session that dispatched this one.
            EventBody::Usage { .. } => {
                if let Some(dispatch) = &self.setup.dispatch {
                    self.manager()
                        .post(&dispatch.parent_session_id, Command::Spent);
                }
                self.check_budget().await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// The agent started a subagent inside its process: it becomes an agent
    /// of the session, spawned by the tool call that started it (the
    /// "Subagents" semantics in `calin-proto/src/api.rs`).
    async fn subagent_started(&mut self, start: SubagentStart) -> Result<()> {
        let Some(lead) = &self.lead else {
            return Ok(());
        };
        let parent_agent = start
            .parent
            .as_ref()
            .and_then(|key| self.subagents.get(key))
            .unwrap_or(&lead.id)
            .clone();
        let agent = Agent {
            id: Ulid::generate().to_string(),
            session_id: self.session_id.clone(),
            parent_agent_id: Some(parent_agent),
            kind: AgentKind::Subagent,
            role: start.role,
            runner: self.runner_id.clone(),
            // It runs in its parent's process; the totals stay there too.
            model: None,
            native_session_id: None,
            status: AgentStatus::Running,
            cost_usd: 0.0,
            tokens_in: 0,
            tokens_out: 0,
        };
        let cause = start
            .item_id
            .as_ref()
            .and_then(|item_id| self.turn_items.get(item_id))
            .map(|item| item.event_id.clone())
            .or_else(|| self.cause.clone());
        self.subagents.insert(start.key, agent.id.clone());
        let spawned = EventBody::AgentSpawned { agent };
        self.append(EventDraft::new(spawned).parent(cause)).await?;
        Ok(())
    }

    async fn subagent_ended(&mut self, key: &str, ok: bool) -> Result<()> {
        let Some(agent_id) = self.subagents.remove(key) else {
            return Ok(());
        };
        let status = if ok {
            AgentStatus::Done
        } else {
            AgentStatus::Failed
        };
        let ended = EventDraft::new(EventBody::AgentStatus { status }).agent(agent_id);
        self.append(ended.parent(self.cause.clone())).await?;
        Ok(())
    }

    /// Closes the subagents the runner never closed: their process is gone.
    async fn close_subagents(&mut self, status: AgentStatus) -> Result<()> {
        let mut open: Vec<String> = std::mem::take(&mut self.subagents).into_values().collect();
        // Newest first, so that a subagent ends before the one that started it.
        open.sort_unstable_by(|a, b| b.cmp(a));
        for agent_id in open {
            let ended = EventDraft::new(EventBody::AgentStatus { status }).agent(agent_id);
            self.append(ended.parent(self.cause.clone())).await?;
        }
        Ok(())
    }

    /// The event that caused something an agent did through `tool`, one of
    /// Calin's own: the call of it that is running (a runner may prefix the
    /// name with its server's), else the input the turn answers.
    fn tool_cause(&self, tool: &str) -> Option<String> {
        let suffix = format!("__{tool}");
        self.turn_items
            .values()
            .filter(|item| {
                item.tool
                    .as_deref()
                    .is_some_and(|name| name == tool || name.ends_with(&suffix))
            })
            .map(|item| &item.event_id)
            .max()
            .cloned()
            .or_else(|| self.cause.clone())
    }

    /// Starts a child session for the agent (the "Child sessions" semantics
    /// in `calin-proto/src/api.rs`).
    async fn dispatch(
        &mut self,
        agent_id: String,
        title: String,
        brief: String,
        repos: Option<Vec<String>>,
        with: ChildWith,
    ) -> Result<Dispatched> {
        if self.setup.dispatch.is_some() {
            return Err(Error::Conflict(
                "this session is itself a child session, and a child session cannot dispatch. \
                 Do the work yourself, or say in your report what else should be dispatched."
                    .to_owned(),
            ));
        }
        if is_closed(self.status) {
            return Err(closed(self.status));
        }
        let sessions = self.inner.store.sessions().await?;
        let running = sessions
            .iter()
            .filter(|session| session.parent_session_id.as_deref() == Some(&self.session_id))
            .filter(|session| !session.status.is_settled())
            .count();
        if running >= MAX_RUNNING_CHILDREN {
            return Err(Error::Conflict(format!(
                "{running} child sessions of this session are still running, which is the \
                 limit. End your turn: their reports arrive as messages. Dispatch again once \
                 one of them has reported."
            )));
        }
        let given = self.child_repos(repos)?;
        let mut start_points = BTreeMap::new();
        let mut dispatched = Vec::with_capacity(given.len());
        for repo in &given {
            let dir = PathBuf::from(&repo.path);
            // Never committed on the agent's behalf: the child starts from
            // the last commit, and the agent is told when that leaves work behind.
            let (commit, dirty) =
                blocking(move || Ok((calin_git::head_commit(&dir)?, calin_git::is_dirty(&dir)?)))
                    .await?;
            start_points.insert(repo.repo_id.clone(), commit.clone());
            dispatched.push(DispatchedRepo {
                name: repo.name.clone(),
                commit,
                dirty,
            });
        }
        let ids: Vec<&str> = given.iter().map(|repo| repo.repo_id.as_str()).collect();
        let reservation = match ids.is_empty() {
            true => None,
            false => Some(self.inner.repos.reserve(&ids).await?),
        };
        let runner = match &with.runner {
            Some(id) => self.inner.registry.resolve(Some(id)).ok_or_else(|| {
                let known = self.inner.registry.ids().join(", ");
                Error::Invalid(format!("unknown runner {id}; this server has {known}"))
            })?,
            None => self
                .runner
                .clone()
                .or_else(|| self.inner.registry.resolve(Some(&self.runner_id)))
                .ok_or_else(|| {
                    Error::Conflict(format!("runner {} is not available", self.runner_id))
                })?,
        };
        let link = Dispatch {
            parent_session_id: self.session_id.clone(),
            root_session_id: self.root_session_id.clone(),
            agent_id: agent_id.clone(),
            lead_agent_id: Ulid::generate().to_string(),
            message_id: Ulid::generate().to_string(),
            start_points,
        };
        let repos = given
            .iter()
            .map(|repo| AttachRepoRequest {
                repo_id: repo.repo_id.clone(),
                mode: Some(RepoMode::Write),
            })
            .collect();
        let child = self
            .inner
            .store
            .create_session(NewSession {
                title,
                prompt: brief.clone(),
                // The runner that actually runs, which in dry run is the mock.
                runner: runner.id().to_owned(),
                permission_mode: self.permission_mode,
                // A child session never runs a workflow of its own.
                workflow: None,
                model: with.model.or_else(|| self.setup.model.clone()),
                repos,
                dispatch: Some(link.clone()),
                // A child's work is its parent's: the parent's pull request closes the issue.
                issue: None,
                context: None,
                automation_id: None,
            })
            .await?;
        let message = AgentMessage {
            id: link.message_id,
            kind: MessageKind::Dispatch,
            from_session_id: self.session_id.clone(),
            from_agent_id: Some(agent_id.clone()),
            to_session_id: child.id.clone(),
            to_agent_id: Some(link.lead_agent_id),
            body: brief,
            in_reply_to: None,
            ts: now_ms(),
        };
        let sent = EventBody::MessageSent {
            message: message.clone(),
        };
        let cause = with.cause.or_else(|| self.tool_cause("dispatch"));
        let sent = EventDraft::new(sent).agent(agent_id).parent(cause);
        let sent = self.append(sent).await?.id.clone();
        // The child's driver is new and waits on nobody: it answers as soon
        // as the brief is in its log.
        let started = self
            .manager()
            .command(&child.id, |reply| Command::Start {
                runner,
                reservation,
                brief: Some((message, sent)),
                workflow: None,
                reply,
            })
            .await?;
        Ok(Dispatched {
            session: started.session,
            repos: dispatched,
        })
    }

    /// The repos a child is given: the named ones, which must be `write`
    /// repos of this session, or every `write` repo of it.
    fn child_repos(&self, names: Option<Vec<String>>) -> Result<Vec<SessionRepo>> {
        let writable = |repo: &&SessionRepo| repo.mode == RepoMode::Write;
        let Some(names) = names else {
            return Ok(self.repos.iter().filter(writable).cloned().collect());
        };
        let attached: Vec<&str> = self.repos.iter().map(|repo| repo.name.as_str()).collect();
        let mut given: Vec<SessionRepo> = Vec::with_capacity(names.len());
        for name in names {
            let name = name.trim();
            let Some(repo) = self.repos.iter().find(|repo| repo.name == name) else {
                return Err(Error::Missing(format!(
                    "no repository named {name} is attached to this session. Attached: {}. \
                     Attach it first with attach_repo if the child needs it.",
                    list(&attached)
                )));
            };
            if repo.mode != RepoMode::Write {
                return Err(Error::Invalid(format!(
                    "{name} is attached to this session in {} mode. A child session can only \
                     be given repositories attached in write mode, because it works on a \
                     branch of its own.",
                    briefing::mode_name(repo.mode)
                )));
            }
            if !given.iter().any(|known| known.id == repo.id) {
                given.push(repo.clone());
            }
        }
        Ok(given)
    }

    /// Sends a note for the agent (the "Notes" semantics in
    /// `calin-proto/src/api.rs`).
    async fn note(
        &mut self,
        agent_id: String,
        to: NoteTarget,
        text: String,
    ) -> Result<NoteOutcome> {
        let store = self.inner.store.clone();
        // Where it goes: the session, its agent, and what that agent is handed.
        let (to_session, to_agent, delivery) = match &to {
            NoteTarget::User => (self.session_id.clone(), None, None),
            NoteTarget::Parent => {
                let Some(dispatch) = &self.setup.dispatch else {
                    return Err(Error::Missing(
                        "this session was not dispatched by another session, so it has no \
                         parent. Use `user` to leave a note for the user."
                            .to_owned(),
                    ));
                };
                let handed = briefing::note_from_child(&self.title, &self.session_id, &text);
                let parent = dispatch.parent_session_id.clone();
                (parent, Some(dispatch.agent_id.clone()), Some(handed))
            }
            NoteTarget::Child(child_id) => {
                let sessions = store.sessions().await?;
                let children: Vec<String> = sessions
                    .iter()
                    .rev()
                    .filter(|session| {
                        session.parent_session_id.as_deref() == Some(&self.session_id)
                    })
                    .map(|session| format!("{} (\"{}\")", session.id, session.title))
                    .collect();
                let known = sessions.iter().any(|session| {
                    session.id == *child_id
                        && session.parent_session_id.as_deref() == Some(&self.session_id)
                });
                if !known {
                    let children = match children.is_empty() {
                        true => "This session has no child sessions.".to_owned(),
                        false => format!("Its child sessions: {}.", children.join(", ")),
                    };
                    return Err(Error::Missing(format!(
                        "{child_id} is not `parent`, `user` or the id of a child session of \
                         this session. {children}"
                    )));
                }
                let lead = store.setup(child_id).await?.dispatch;
                let handed = briefing::note_from_parent(&self.title, &text);
                (
                    child_id.clone(),
                    lead.map(|dispatch| dispatch.lead_agent_id),
                    Some(handed),
                )
            }
        };
        let message = AgentMessage {
            id: Ulid::generate().to_string(),
            kind: MessageKind::Note,
            from_session_id: self.session_id.clone(),
            from_agent_id: Some(agent_id.clone()),
            to_session_id: to_session.clone(),
            to_agent_id: to_agent,
            body: text,
            in_reply_to: None,
            ts: now_ms(),
        };
        let sent = EventBody::MessageSent {
            message: message.clone(),
        };
        let sent = EventDraft::new(sent)
            .agent(agent_id)
            .parent(self.tool_cause("send_message"));
        let sent = self.append(sent).await?.id.clone();
        let Some(text) = delivery else {
            // For the user: it arrives where the user reads, which is here.
            let received = EventBody::MessageReceived { message };
            self.append(EventDraft::new(received).parent(Some(sent)))
                .await?;
            return Ok(NoteOutcome::Shown);
        };
        let status = store.session(&to_session).await?.session.status;
        let delivery = Command::Deliver {
            message,
            cause: sent,
            text,
            ended: None,
        };
        self.manager().post(&to_session, delivery);
        Ok(match is_closed(status) {
            true => NoteOutcome::Unread(status),
            false => NoteOutcome::Delivered,
        })
    }

    /// A message from another session: recorded, then handed to the agent
    /// as its next input, at once when it is idle. A session that is over
    /// records it and starts nothing.
    ///
    /// `ended` says how the child behind a report ended. The report of a
    /// child a workflow node dispatched is that node's result: it is
    /// recorded, and the lead agent gets no turn for it.
    async fn deliver(
        &mut self,
        message: AgentMessage,
        cause: String,
        text: String,
        ended: Option<ChildEnd>,
    ) -> Result<()> {
        let (from, body) = (message.from_session_id.clone(), message.body.clone());
        let received = EventBody::MessageReceived { message };
        let received = self
            .append(EventDraft::new(received).parent(Some(cause)))
            .await?;
        if is_closed(self.status) {
            // Nothing starts for it; the agent gets it if the user goes on.
            self.notes.push(text);
            return Ok(());
        }
        if let Some(ended) = ended
            && self.workflow_children.contains(&from)
        {
            return self.child_ended(&from, body, ended).await;
        }
        self.pending.push_back(Queued {
            text,
            event_id: received.id.clone(),
        });
        if self.defers_input() {
            return Ok(());
        }
        self.run_pending().await
    }

    /// Keeps what a child's agent wants reported when its turn ends.
    fn keep_report(&mut self, summary: String) -> Result<()> {
        if self.setup.dispatch.is_none() {
            return Err(Error::Conflict(
                "this session was not dispatched by another session, so there is nobody to \
                 report to. Say what you did in your answer to the user instead."
                    .to_owned(),
            ));
        }
        self.report = Some(summary);
        Ok(())
    }

    /// A child session came to rest, failed or was stopped for the first
    /// time since it was dispatched or last reported: its report goes to the
    /// agent that dispatched it (the "Child sessions" semantics in
    /// `calin-proto/src/api.rs`). Exactly one per such ending.
    async fn send_report(&mut self, ending: Ending<'_>) -> Result<()> {
        let Some(dispatch) = self.setup.dispatch.clone() else {
            return Ok(());
        };
        let said = self.report.take().or_else(|| self.last_text.take());
        self.last_text = None;
        let (body, status, ended) = match ending {
            Ending::Rested => (
                said.filter(|said| !said.is_empty())
                    .unwrap_or_else(|| NO_REPORT.to_owned()),
                "review",
                ChildEnd::Review,
            ),
            Ending::Failed(reason) => (
                briefing::child_failed_report(reason, said.as_deref()),
                "failed",
                ChildEnd::Failed,
            ),
            Ending::Stopped => (
                briefing::child_stopped_report(said.as_deref()),
                "cancelled",
                ChildEnd::Failed,
            ),
        };
        let branches = reported_branches(&self.repos).await;
        // A child that failed before its agent started still reports, as the
        // agent it would have had.
        let from = match &self.lead {
            Some(lead) => lead.id.clone(),
            None => dispatch.lead_agent_id.clone(),
        };
        let message = AgentMessage {
            id: Ulid::generate().to_string(),
            kind: MessageKind::Report,
            from_session_id: self.session_id.clone(),
            from_agent_id: Some(from.clone()),
            to_session_id: dispatch.parent_session_id.clone(),
            to_agent_id: Some(dispatch.agent_id),
            body,
            in_reply_to: Some(dispatch.message_id),
            ts: now_ms(),
        };
        let text = briefing::report_text(&self.title, status, &message.body, &branches);
        let sent = EventBody::MessageSent {
            message: message.clone(),
        };
        let sent = EventDraft::new(sent).agent(from).parent(self.cause.clone());
        let cause = self.append(sent).await?.id.clone();
        let delivery = Command::Deliver {
            message,
            cause,
            text,
            ended: Some(ended),
        };
        self.manager().post(&dispatch.parent_session_id, delivery);
        Ok(())
    }

    /// Reports that reached this session after its lead agent was last
    /// handed an input, so that no agent ever read them: they arrived while
    /// the session was over, or before a restart cut the turn they waited
    /// for (`report_restart` in `manager.rs`). The log is the only place
    /// they are kept, so a new driver looks there; they go with the agent's
    /// next input, as notes.
    async fn unheard_reports(&self) -> Result<Vec<String>> {
        let sessions = self.inner.store.sessions().await?;
        let parent = |session: &calin_proto::Session| {
            session.parent_session_id.as_deref() == Some(self.session_id.as_str())
        };
        // Only a session that dispatched has reports; the log is read only then.
        if !sessions.iter().any(parent) {
            return Ok(Vec::new());
        }
        let events = self
            .inner
            .store
            .subscribe(&self.session_id, 0)
            .await?
            .backlog;
        let lead = self.lead.as_ref().map(|lead| lead.id.as_str());
        // A turn begins with the lead's `running` (or its spawn): what it was
        // handed is on record before that.
        let handed = events.iter().rposition(|event| match &event.body {
            EventBody::AgentStatus {
                status: AgentStatus::Running,
            } => lead.is_some() && event.agent_id.as_deref() == lead,
            EventBody::AgentSpawned { agent } => lead == Some(agent.id.as_str()),
            _ => false,
        });
        let mut notes = Vec::new();
        for event in &events[handed.map_or(0, |at| at + 1)..] {
            let EventBody::MessageReceived { message } = &event.body else {
                continue;
            };
            if message.kind != MessageKind::Report {
                continue;
            }
            let Ok(child) = self.inner.store.session(&message.from_session_id).await else {
                continue;
            };
            let branches = reported_branches(&child.repos).await;
            let status = status_name(child.session.status);
            let title = &child.session.title;
            notes.push(briefing::report_text(
                title,
                &status,
                &message.body,
                &branches,
            ));
        }
        Ok(notes)
    }

    /// Puts the agent's question to the user (the "Questions" semantics in
    /// `calin-proto/src/api.rs`). Answers with the question's id.
    async fn ask(
        &mut self,
        agent_id: String,
        text: String,
        options: Vec<String>,
    ) -> Result<String> {
        if is_closed(self.status) {
            return Err(closed(self.status));
        }
        if self.question.is_some() {
            return Err(Error::Conflict(
                "a question to the user is already open, and a session has one at a time. \
                 End your turn: the answer arrives as your next message. Ask the next \
                 question after that."
                    .to_owned(),
            ));
        }
        let question = Question {
            id: Ulid::generate().to_string(),
            text,
            options,
            gate: false,
        };
        let question_id = question.id.clone();
        let asked = EventDraft::new(EventBody::QuestionAsked { question })
            .agent(agent_id.clone())
            .parent(self.tool_cause("ask_user"));
        let asked = self.append(asked).await?;
        self.question = Some(OpenQuestion {
            session_id: self.session_id.clone(),
            question_id: question_id.clone(),
            agent_id: Some(agent_id),
            event_id: asked.id.clone(),
        });
        if self.status != SessionStatus::Waiting {
            let reason = Some(WAITING_FOR_ANSWER.to_owned());
            self.set_status(SessionStatus::Waiting, reason).await?;
        }
        Ok(question_id)
    }

    /// Records the user's answer and hands it to the agent as its next
    /// input. The answer to a question of a workflow's own (a gate, an `ask`
    /// node) is that node's result instead; `comment` goes with a gate's.
    async fn answer_question(
        &mut self,
        question_id: String,
        answer: String,
        comment: Option<String>,
    ) -> Result<()> {
        if self
            .question
            .as_ref()
            .is_some_and(|open| open.question_id == question_id)
        {
            // Refused before anything is on record: the gate stays open.
            self.check_answer(&question_id, &answer)?;
        }
        let Some(open) = self
            .question
            .take_if(|open| open.question_id == question_id)
        else {
            let answered = self
                .inner
                .store
                .question_answered(&self.session_id, &question_id);
            return Err(match answered.await? {
                None => Error::Missing(format!("unknown question {question_id}")),
                Some(_) => Error::Conflict("this question was already answered".to_owned()),
            });
        };
        let answered = EventBody::QuestionAnswered {
            question_id: question_id.clone(),
            answer: answer.clone(),
            by: DecisionSource::User,
        };
        let answered = as_agent(open.agent_id, answered).parent(Some(open.event_id));
        let answered = self.append(answered).await?;
        if self.run_answered(&question_id, &answer, comment.as_deref()) {
            return Ok(());
        }
        self.pending.push_back(Queued {
            text: briefing::answer_text(&answer),
            event_id: answered.id.clone(),
        });
        if self.defers_input() {
            // The agent has not ended its turn yet: the answer waits for that.
            if self.in_turn && self.status == SessionStatus::Waiting {
                self.resume_status().await?;
            }
            return Ok(());
        }
        self.run_pending().await
    }

    /// One `file_changed` per file a completed edit, delete or move touched,
    /// caused by the tool call: the link a graph view follows from a change
    /// back to what made it.
    async fn file_changes(&mut self, completed: &Event, item: &Item) -> Result<()> {
        let ItemContent::Tool {
            tool_kind,
            locations,
            ..
        } = &item.content
        else {
            return Ok(());
        };
        let op = match tool_kind {
            ToolKind::Edit => FileOp::Edit,
            ToolKind::Delete => FileOp::Delete,
            ToolKind::Move => FileOp::Rename,
            _ => return Ok(()),
        };
        if item.status != ItemStatus::Completed {
            return Ok(());
        }
        let tracker = unreported::tracker(&self.inner.trackers, &self.session_id);
        for location in locations {
            let (session_repo_id, path) = locate(&self.repos, location);
            // The file as the call left it, so that the turn's end reports
            // it again only if it changed after this.
            let repo = session_repo_id
                .as_ref()
                .and_then(|id| self.repos.iter().find(|repo| repo.id == *id))
                .filter(|repo| tracker.is_some() && unreported::is_tracked(repo))
                .cloned();
            let state = match repo {
                Some(repo) => {
                    let (repo_id, rel) = (repo.id.clone(), path.clone());
                    let state = blocking(move || Ok(unreported::state_of(&repo, &rel)));
                    state.await.ok().flatten().map(|state| (repo_id, state))
                }
                None => None,
            };
            let changed = EventBody::FileChanged {
                session_repo_id,
                path: path.clone(),
                op,
            };
            // By whoever made the call: the lead or one of its subagents.
            let draft = as_agent(completed.agent_id.clone(), changed);
            self.append(draft.parent(Some(completed.id.clone())))
                .await?;
            if let (Some(tracker), Some((repo_id, state))) = (&tracker, state) {
                tracker.lock().await.reported(&repo_id, &path, state);
            }
        }
        Ok(())
    }

    /// Appends to the session's log and keeps the driver's view of the
    /// session in step with what it just wrote.
    async fn append(&mut self, draft: EventDraft) -> Result<Arc<Event>> {
        let event = self.inner.store.append(&self.session_id, draft).await?;
        self.head_seq = event.seq;
        let from_lead = |lead: &Lead| event.agent_id.as_deref() == Some(lead.id.as_str());
        match &event.body {
            EventBody::SessionStatus { status, .. } => {
                self.status = *status;
                // Only a session that prepares or runs holds a slot, and
                // only a `queued` one waits for one.
                if !status.is_active() && *status != SessionStatus::Queued {
                    self.leave_queue();
                    // What waited for a slot with the session is the
                    // agent's to have: its turn, if any, goes on.
                    self.forward_held();
                }
            }
            EventBody::AgentStatus { status } => {
                if let Some(lead) = self.lead.as_mut().filter(|lead| from_lead(lead)) {
                    lead.status = *status;
                }
            }
            EventBody::AgentSession {
                native_session_id,
                model,
            } => {
                if let Some(lead) = self.lead.as_mut().filter(|lead| from_lead(lead)) {
                    lead.native_session_id = Some(native_session_id.clone());
                    if model.is_some() {
                        lead.model.clone_from(model);
                    }
                }
            }
            EventBody::RepoAttached { repo } => self.repos.push(repo.clone()),
            EventBody::PullRequestOpened { pull_request }
            | EventBody::PullRequestUpdated { pull_request } => {
                let same_repo = |known: &&mut PullRequest| {
                    known.session_repo_id == pull_request.session_repo_id
                };
                match self.pull_requests.iter_mut().find(same_repo) {
                    Some(known) => *known = pull_request.clone(),
                    None => self.pull_requests.push(pull_request.clone()),
                }
            }
            _ => {}
        }
        Ok(event)
    }

    /// Starts delivering the session in the background (the "Shipping"
    /// semantics in `calin-proto/src/api.rs`). The caller is answered at
    /// once; what happens shows as events.
    fn begin_ship(
        &mut self,
        title: Option<String>,
        body: Option<String>,
        draft: bool,
    ) -> Result<()> {
        if !ships(self.status) || self.in_turn || self.preparation.is_some() {
            return Err(no_shipping(self.status));
        }
        if self.shipping.is_some() {
            return Err(Error::Conflict(
                "the session is already being shipped; wait for that to finish".to_owned(),
            ));
        }
        self.start_ship(title, body, draft);
        Ok(())
    }

    /// Starts the background work of a ship, for the Ship action and for a
    /// workflow's `ship` node alike.
    fn start_ship(&mut self, title: Option<String>, body: Option<String>, draft: bool) {
        let known = |repo: &SessionRepo| {
            let its = |pull_request: &&PullRequest| pull_request.session_repo_id == repo.id;
            self.pull_requests.iter().find(its).cloned()
        };
        let plan = ShipPlan {
            session_id: self.session_id.clone(),
            title: title.unwrap_or_else(|| one_line(&self.title)),
            body,
            draft,
            dry_run: self.inner.registry.dry_run(),
            github: self.inner.github.clone(),
            store: self.inner.store.clone(),
            repos: self
                .repos
                .iter()
                .map(|repo| (repo.clone(), known(repo)))
                .collect(),
            issue: self.issue.clone(),
            commit_timeout: self.inner.config.commit_timeout,
            push_timeout: self.inner.config.push_timeout,
        };
        let (results, receiver) = mpsc::channel(8);
        let cancel = Arc::new(AtomicBool::new(false));
        tokio::spawn(delivery::ship(plan, results, Arc::clone(&cancel)));
        self.shipping = Some(Shipping {
            results: receiver,
            cancel,
            delivered: false,
        });
    }

    async fn on_shipped(&mut self, shipped: Option<Shipped>) -> Result<()> {
        if let Some(shipped) = shipped {
            return self.record_shipped(shipped).await;
        }
        // Every repo was attempted.
        let Some(shipping) = self.shipping.take() else {
            return Ok(());
        };
        // A workflow's `ship` node: the run goes on from its result.
        if self.run_shipped() {
            return Ok(());
        }
        // Without a pull request the session stays where it was, with the
        // reasons in its log.
        if shipping.delivered && self.status == SessionStatus::Review {
            self.settle(SessionStatus::Shipping).await?;
        }
        // Messages sent meanwhile waited for the worktrees to be left alone.
        if self.defers_input() {
            return Ok(());
        }
        self.run_pending().await
    }

    /// Appends what one step of a ship did. Its cause is the user's
    /// request, which is no event, so these have no parent.
    async fn record_shipped(&mut self, shipped: Shipped) -> Result<()> {
        let body = match shipped {
            Shipped::Opened(pull_request) => EventBody::PullRequestOpened { pull_request },
            Shipped::Updated(pull_request) => EventBody::PullRequestUpdated { pull_request },
            Shipped::Kept => {
                if let Some(shipping) = &mut self.shipping {
                    shipping.delivered = true;
                }
                return Ok(());
            }
            // A repo that fails does not fail the session: the others carry on.
            Shipped::Failed(message) => {
                self.append(EventDraft::new(EventBody::Error { message }))
                    .await?;
                return Ok(());
            }
        };
        if let Some(shipping) = &mut self.shipping {
            shipping.delivered = true;
        }
        self.append(EventDraft::new(body)).await?;
        Ok(())
    }

    /// Ends the ship in progress between two of its steps. What it already
    /// did stays done; what it still reports is dropped.
    fn abort_shipping(&mut self) {
        if let Some(shipping) = self.shipping.take() {
            shipping.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Starts a look at the session's pull requests (the "While `shipping`"
    /// semantics in `calin-proto/src/api.rs`). `reply` is answered once what
    /// the look saw is on record.
    async fn begin_poll(&mut self, reply: Reply) -> Result<()> {
        if let Some(polling) = &mut self.polling {
            polling.waiting.push(reply);
            return Ok(());
        }
        // Not watched: any other status, a dry run (its pull requests are
        // made up), and a ship in progress, which reads them itself.
        let watched = self.status == SessionStatus::Shipping
            && self.shipping.is_none()
            && !self.inner.registry.dry_run();
        if !watched {
            let _ = reply.send(Ok(()));
            return Ok(());
        }
        // A merged pull request stays merged; a closed one can be reopened.
        let targets: Vec<(SessionRepo, PullRequest)> = self
            .pull_requests
            .iter()
            .filter(|pull_request| pull_request.state != PullRequestState::Merged)
            .filter_map(|pull_request| {
                let repo = self
                    .repos
                    .iter()
                    .find(|repo| repo.id == pull_request.session_repo_id)?;
                Some((repo.clone(), pull_request.clone()))
            })
            .collect();
        if targets.is_empty() {
            let result = self.finish_when_delivered().await;
            let _ = reply.send(Ok(()));
            return result;
        }
        let (done, observed) = oneshot::channel();
        let (session_id, github) = (self.session_id.clone(), self.inner.github.clone());
        tokio::spawn(async move {
            // The driver may be gone by then; nobody is left to tell.
            let _ = done.send(delivery::poll(session_id, github, targets).await);
        });
        self.polling = Some(Polling {
            observed,
            waiting: vec![reply],
        });
        Ok(())
    }

    /// Records the pull requests that are not as they were on record.
    async fn on_polled(&mut self, observed: Vec<PullRequest>) -> Result<()> {
        for now in observed {
            let on_record = self.pull_requests.iter().find(|known| {
                known.session_repo_id == now.session_repo_id && known.number == now.number
            });
            // No longer the repo's pull request: a ship replaced it meanwhile.
            let Some(known) = on_record else {
                continue;
            };
            if !delivery::same(known, &now) {
                let updated = EventBody::PullRequestUpdated { pull_request: now };
                self.append(EventDraft::new(updated)).await?;
            }
        }
        let result = self.finish_when_delivered().await;
        self.end_polling(Ok(()));
        result
    }

    /// Answers everyone who waits for the look in progress, and forgets it.
    fn end_polling(&mut self, result: Result<()>) {
        if let Some(polling) = self.polling.take() {
            for reply in polling.waiting {
                let answer = match &result {
                    Ok(()) => Ok(()),
                    Err(_) => Err(no_longer_active()),
                };
                let _ = reply.send(answer);
            }
        }
    }

    /// A `shipping` session whose pull requests are all merged or closed is
    /// `done`. Not while a ship is still at work on them.
    async fn finish_when_delivered(&mut self) -> Result<()> {
        let over = |pull_request: &PullRequest| {
            matches!(
                pull_request.state,
                PullRequestState::Merged | PullRequestState::Closed
            )
        };
        let delivered = self.status == SessionStatus::Shipping
            && self.shipping.is_none()
            && !self.pull_requests.is_empty()
            && self.pull_requests.iter().all(over);
        if !delivered {
            return Ok(());
        }
        // A done session takes no more messages: its agent, idle since the
        // last turn, has nothing left to wait for.
        self.drop_agent();
        self.set_agent_status(AgentStatus::Done).await?;
        self.settle(SessionStatus::Done).await
    }

    /// A status the session reaches through delivery: nothing the user
    /// said caused it, so it has no parent.
    async fn settle(&mut self, status: SessionStatus) -> Result<()> {
        let body = EventBody::SessionStatus {
            status,
            reason: None,
        };
        self.append(EventDraft::new(body)).await?;
        Ok(())
    }
}

/// A draft attributed to `agent_id`, when there is one.
/// How a child session came to an end that its parent is told about.
enum Ending<'a> {
    /// It came to rest in `review`.
    Rested,
    /// It failed, for this reason.
    Failed(&'a str),
    /// It was stopped before it came to rest.
    Stopped,
}

/// A session's branches as a report describes them: where each stands
/// against the commit it started from.
async fn reported_branches(repos: &[SessionRepo]) -> Vec<ReportedBranch> {
    let mut branches = Vec::new();
    for repo in repos {
        let (Some(branch), Some(base)) = (&repo.branch, &repo.base_ref) else {
            continue;
        };
        let (dir, base, name) = (PathBuf::from(&repo.path), base.clone(), branch.clone());
        // A count git cannot give is left out; the report still goes.
        let counted = blocking(move || {
            let ahead = calin_git::commits_ahead(&dir, &base, &name).ok();
            Ok((ahead, calin_git::is_dirty(&dir).unwrap_or(false)))
        });
        let (ahead, dirty) = counted.await.unwrap_or((None, false));
        branches.push(ReportedBranch {
            repo: repo.name.clone(),
            branch: branch.clone(),
            ahead,
            dirty,
        });
    }
    branches
}

fn as_agent(agent_id: Option<String>, body: EventBody) -> EventDraft {
    let mut draft = EventDraft::new(body);
    draft.agent_id = agent_id;
    draft
}

/// `a, b and c`, for a sentence that names what there is.
fn list(names: &[&str]) -> String {
    match names {
        [] => "none".to_owned(),
        [only] => (*only).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The first line of a session's title that says something: what a commit
/// and a pull request are called when the ship request names nothing.
fn one_line(title: &str) -> String {
    let line = title.lines().map(str::trim).find(|line| !line.is_empty());
    line.unwrap_or("Calin session").to_owned()
}

/// Makes the repos ready one after the other and reports each step. Ends at
/// the first failure, or early when cancelled.
async fn prepare(plans: Vec<AttachPlan>, results: mpsc::Sender<Prepared>, cancel: Arc<AtomicBool>) {
    for plan in plans {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let name = plan.repo.name.clone();
        let setup = plan.repo.setup.clone();
        let limit = plan.setup_limit;
        let (repo, fresh) = match blocking(move || attach(plan)).await {
            Ok(attached) => attached,
            Err(error) => {
                let _ = results.send(Prepared::Failed { repo: name, error }).await;
                return;
            }
        };
        let dir = PathBuf::from(&repo.path);
        if results.send(Prepared::Repo(repo)).await.is_err() {
            return;
        }
        // Setup is for a worktree that was just made (design spec, section
        // 3): one that was already there has had it.
        let Some(command) = setup.filter(|_| fresh) else {
            continue;
        };
        let mut item = setup_item(&name, &command);
        if results
            .send(Prepared::SetupStarted(item.clone()))
            .await
            .is_err()
        {
            return;
        }
        let stop = Arc::clone(&cancel);
        let ran = blocking(move || Ok(calin_git::run_setup(&dir, &command, limit, &stop)?)).await;
        let (output, failure) = match ran {
            Ok(run) => {
                let failure = match run.end {
                    calin_git::SetupEnd::Succeeded => None,
                    calin_git::SetupEnd::Failed(how) => Some(format!("failed ({how})")),
                    calin_git::SetupEnd::TimedOut => {
                        Some(format!("was stopped: it ran longer than {limit:?}"))
                    }
                    calin_git::SetupEnd::Cancelled => Some("was cancelled".to_owned()),
                };
                (run.output, failure)
            }
            Err(err) => (String::new(), Some(format!("could not be run: {err}"))),
        };
        item.status = match failure {
            None => ItemStatus::Completed,
            Some(_) => ItemStatus::Failed,
        };
        if let ItemContent::Tool { output: slot, .. } = &mut item.content {
            *slot = Some(output);
        }
        let failure = failure.map(|how| format!("the setup command of {name} {how}"));
        if results
            .send(Prepared::SetupEnded { item, failure })
            .await
            .is_err()
        {
            return;
        }
    }
}

/// The tool item a setup command shows as. Its id is new for every run.
fn setup_item(repo: &str, command: &str) -> Item {
    Item {
        id: format!("setup-{}", Ulid::generate()),
        status: ItemStatus::Running,
        content: ItemContent::Tool {
            name: "setup".to_owned(),
            tool_kind: ToolKind::Execute,
            title: command.to_owned(),
            input: serde_json::json!({ "command": command, "repo": repo }),
            output: None,
            locations: Vec::new(),
        },
    }
}

/// Blocking: a worktree on the session's branch for a `write` repo, the
/// clone as it is for the others (design spec, section 3, "Repo set"). Also
/// says whether a worktree was made that was not there before.
fn attach(plan: AttachPlan) -> Result<(SessionRepo, bool)> {
    let clone = PathBuf::from(&plan.repo.path);
    let (path, branch, base_ref, fresh) = match plan.mode {
        RepoMode::Write => {
            let start = plan.start_point.as_deref();
            let worktree = calin_git::create_worktree(&clone, &plan.worktree, &plan.branch, start)?;
            let path = utf8(&worktree.path)?;
            let fresh = worktree.outcome != calin_git::WorktreeOutcome::AlreadyPresent;
            (path, Some(worktree.branch), Some(worktree.base_ref), fresh)
        }
        RepoMode::Read | RepoMode::Live => {
            // A clone without commits has nothing to measure changes against.
            let base = match calin_git::head_commit(&clone) {
                Ok(commit) => Some(commit),
                Err(calin_git::Error::Conflict(_)) => None,
                Err(err) => return Err(err.into()),
            };
            (plan.repo.path.clone(), None, base, false)
        }
    };
    let repo = SessionRepo {
        id: Ulid::generate().to_string(),
        session_id: plan.session_id,
        repo_id: plan.repo.id,
        name: plan.repo.name,
        mode: plan.mode,
        path,
        branch,
        base_ref,
        primary: plan.primary,
    };
    Ok((repo, fresh))
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("{} is not valid UTF-8", path.to_string_lossy())))
}
