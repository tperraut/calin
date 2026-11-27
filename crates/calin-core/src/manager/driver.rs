//! A session's driver: the one task that changes the session. It takes the
//! manager's commands, the agent's output and the results of preparing the
//! repos, one at a time, and appends what follows from each.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calin_agents::{
    AgentHandle, DirGrant, Exit, PermissionAsk, Runner, RunnerOutput, StartParams, ToolAccess,
};
use calin_proto::{
    Agent, AgentKind, AgentStatus, AttachRepoRequest, DecisionSource, Event, EventBody, FileOp,
    Item, ItemContent, ItemStatus, PermissionDecision, PermissionMode, PermissionRequest, Repo,
    RepoMode, SessionDetail, SessionRepo, SessionStatus, ToolKind,
};
use tokio::sync::mpsc;
use ulid::Ulid;

use super::{
    Command, Inner, Reply, agent_ended, closed, expired, has_turn, is_closed, no_attaching,
    no_longer_active, no_turn, takes_messages, takes_repos,
};
use crate::briefing;
use crate::error::{Error, Result};
use crate::policy::{self, Target, Verdict};
use crate::repos::{Reservation, blocking};
use crate::store::{EventDraft, SessionSetup};
use crate::tokens::{AgentRef, Token};
use crate::workspace::{locate, primary, working_directory};

/// How long a stopped agent, or a git command of a stopped preparation, may
/// take to end before the session is cancelled without it.
const STOP_GRACE: Duration = Duration::from_secs(5);
const LEAD_ROLE: &str = "lead";
/// The reason of `waiting` while a permission request is open.
const WAITING_FOR_PERMISSION: &str = "permission";

/// The lead agent as the driver knows it.
struct Lead {
    id: String,
    status: AgentStatus,
    /// The runner's conversation id, passed back to resume the agent.
    native_session_id: Option<String>,
    model: Option<String>,
}

/// A user message waiting for the current turn to end.
struct Queued {
    text: String,
    event_id: String,
}

/// A permission request waiting for the user.
struct OpenRequest {
    request_id: String,
    /// The `permission_requested` event: the cause of the answer.
    event_id: String,
    /// The repos of the session the request reaches into, as indexes into
    /// the driver's list: what an allow may have to open to the agent.
    repos: Vec<usize>,
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

/// The driver hit a storage error and cannot go on.
struct Halt;

enum Step {
    Command(Command),
    Output(Option<RunnerOutput>),
    Prepared(Option<Prepared>),
    Attached(Option<Prepared>),
}

pub(super) struct Driver {
    inner: Arc<Inner>,
    session_id: String,
    commands: mpsc::UnboundedReceiver<Command>,
    status: SessionStatus,
    title: String,
    prompt: String,
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
    /// Item id to its `item_started` event, for the items of the current turn.
    turn_items: HashMap<String, String>,
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
            attaching: None,
            attach_queue: VecDeque::new(),
            notes: Vec::new(),
            granted: HashSet::new(),
            token: None,
        }
    }

    pub(super) async fn run(mut self) {
        let retired = self.drive().await;
        if !retired {
            self.inner.drivers().remove(&self.session_id);
        }
        self.abort_attaching();
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
            let idle =
                self.handle.is_none() && self.preparation.is_none() && self.attaching.is_none();
            let step = if idle {
                match self.commands.try_recv() {
                    Ok(command) => Step::Command(command),
                    Err(_) => {
                        let mut drivers = self.inner.drivers();
                        if self.commands.is_empty() {
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
            };
            if result.is_err() {
                return false;
            }
        }
    }

    async fn load(&mut self) -> Result<()> {
        let detail = self.inner.store.session(&self.session_id).await?;
        self.setup = self.inner.store.setup(&self.session_id).await?;
        self.status = detail.session.status;
        self.title = detail.session.title;
        self.prompt = detail.session.prompt;
        self.runner_id = detail.session.runner;
        self.permission_mode = detail.session.permission_mode;
        self.head_seq = detail.session.head_seq;
        self.repos = detail.repos;
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
        Ok(())
    }

    async fn on_command(&mut self, command: Command) -> std::result::Result<(), Halt> {
        match command {
            Command::Start {
                runner,
                reservation,
                reply,
            } => {
                self.runner = Some(runner);
                self.start(reservation, reply).await
            }
            Command::Message { text, reply } => {
                let result = self.message(text).await;
                self.answer(reply, result)
            }
            Command::Interrupt { reply } => {
                let result = self.interrupt().await;
                self.answer(reply, result)
            }
            Command::Stop { reply } => {
                let result = self.stop().await;
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
        }
    }

    /// Sends the result of a command to its caller. A refusal is an answer
    /// like any other; anything else means the driver cannot go on.
    fn answer(&self, reply: Reply, result: Result<()>) -> std::result::Result<(), Halt> {
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
            Some(RunnerOutput::Event(body)) => self.agent_event(body).await,
            Some(RunnerOutput::PermissionRequest(ask)) => self.permission_requested(ask).await,
            Some(RunnerOutput::NativeSession { id, model }) => self.native_session(id, model).await,
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
        reply: Reply<SessionDetail>,
    ) -> std::result::Result<(), Halt> {
        let recorded = async {
            self.set_status(SessionStatus::Queued, None).await?;
            let prompt = self.prompt.clone();
            let message = self
                .append(EventDraft::new(EventBody::UserMessage { text: prompt }))
                .await?;
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
        let prompt = self.prompt.clone();
        self.launch(prompt, cause, reservation)
            .await
            .map_err(|err| {
                tracing::error!(session = %self.session_id, %err, "could not start the session");
                Halt
            })
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
        if self.in_turn || self.preparation.is_some() {
            return Ok(());
        }
        // The front of the queue is this message, unless an interrupt held
        // earlier ones back: those run first.
        let Some(next) = self.pending.pop_front() else {
            return Ok(());
        };
        let text = if self.lead.is_some() {
            next.text
        } else {
            // No agent ever saw the prompt (the session failed before one
            // started), so the message alone would have no context.
            format!("{}\n\n{}", self.prompt, next.text)
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
            return self.begin_turn(text, cause).await;
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

    /// How to make `repo` ready for this session.
    fn plan(&self, repo: Repo, mode: RepoMode, primary: bool) -> AttachPlan {
        let work_dir = self.inner.store.home().work_dir(&self.session_id);
        AttachPlan {
            session_id: self.session_id.clone(),
            worktree: work_dir.join(&repo.name),
            repo,
            mode,
            branch: calin_git::session_branch_name(&self.title, &self.session_id),
            primary,
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
                Some(preparation) => self.begin_turn(preparation.text, preparation.cause).await,
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
        self.cause = Some(cause);
        self.interrupted = false;
        if self.status != SessionStatus::Running {
            self.set_status(SessionStatus::Running, None).await?;
        }
        // What the agent is handed, which may say more than the user wrote.
        let text = briefing::with_notes(std::mem::take(&mut self.notes), text);
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
            let agent_id = match &self.lead {
                Some(lead) => lead.id.clone(),
                None => Ulid::generate().to_string(),
            };
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
            let system_prompt = briefing::system_prompt(&self.repos, &cwd, tool_endpoint.is_some());
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
        self.turn_items.clear();
        let interrupted = std::mem::take(&mut self.interrupted);
        if !interrupted && let Some(next) = self.pending.pop_front() {
            self.cause = Some(next.event_id);
            if self.status != SessionStatus::Running {
                self.set_status(SessionStatus::Running, None).await?;
            }
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
        self.set_agent_status(AgentStatus::Idle).await?;
        self.set_status(SessionStatus::Review, None).await
    }

    /// Forgets the agent that is gone: its handle, its token, and what was
    /// only ever true of that process.
    fn drop_agent(&mut self) {
        self.handle = None;
        self.token = None;
        self.granted.clear();
        // Its next start is told about every repo in its system prompt.
        self.notes.clear();
    }

    async fn agent_exited(&mut self) -> Result<()> {
        self.drop_agent();
        self.in_turn = false;
        self.interrupted = false;
        self.turn_items.clear();
        self.expire_permissions().await?;
        self.set_agent_status(AgentStatus::Done).await?;
        match self.pending.pop_front() {
            // Messages are still waiting: the next turn starts the agent again.
            Some(next) => self.begin_turn(next.text, next.event_id).await,
            None if self.status != SessionStatus::Review => {
                self.set_status(SessionStatus::Review, None).await
            }
            None => Ok(()),
        }
    }

    async fn interrupt(&mut self) -> Result<()> {
        if !has_turn(self.status) || !self.in_turn {
            return Err(no_turn(self.status));
        }
        // An interrupted turn asks for nothing any more.
        self.expire_permissions().await?;
        if self.status == SessionStatus::Waiting {
            self.set_status(SessionStatus::Running, None).await?;
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

    async fn stop(&mut self) -> Result<()> {
        if is_closed(self.status) {
            return Err(closed(self.status));
        }
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
        self.expire_permissions().await?;
        if let Some(mut handle) = self.handle.take()
            && handle.stop().is_ok()
        {
            // Keep what the agent said before it noticed the request.
            let grace = tokio::time::sleep(STOP_GRACE);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    output = handle.next_output() => match output {
                        Some(RunnerOutput::Event(body)) => self.agent_event(body).await?,
                        Some(RunnerOutput::PermissionRequest(ask)) => {
                            // Asked while stopping: nobody will answer it.
                            let open = self.record_request(ask.request).await?;
                            self.expire(open).await?;
                        }
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
        self.drop_agent();
        self.in_turn = false;
        self.interrupted = false;
        self.pending.clear();
        self.turn_items.clear();
        self.set_agent_status(AgentStatus::Stopped).await?;
        self.set_status(SessionStatus::Cancelled, None).await
    }

    /// Records that the session cannot go on and fails it.
    async fn fail(&mut self, message: String) -> Result<()> {
        self.drop_agent();
        self.abort_attaching();
        self.preparation = None;
        self.in_turn = false;
        self.interrupted = false;
        self.pending.clear();
        self.turn_items.clear();
        self.expire_permissions().await?;
        let mut error = EventDraft::new(EventBody::Error {
            message: message.clone(),
        })
        .parent(self.cause.clone());
        if let Some(lead) = &self.lead {
            error = error.agent(lead.id.clone());
        }
        self.append(error).await?;
        self.set_agent_status(AgentStatus::Failed).await?;
        self.set_status(SessionStatus::Failed, Some(message)).await
    }

    async fn permission_requested(&mut self, ask: PermissionAsk) -> Result<()> {
        let PermissionAsk { request, access } = ask;
        let mut open = self.record_request(request).await?;
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
        self.append(self.by_agent(resolved).parent(Some(open.event_id.clone())))
            .await?;
        self.forward_decision(open, decision, message);
        Ok(())
    }

    /// Appends `permission_requested`, caused by the tool item it is about
    /// when the runner named one.
    async fn record_request(&mut self, request: PermissionRequest) -> Result<OpenRequest> {
        let request_id = request.id.clone();
        let parent = request
            .item_id
            .as_ref()
            .and_then(|item_id| self.turn_items.get(item_id))
            .cloned()
            .or_else(|| self.cause.clone());
        let requested = EventBody::PermissionRequested { request };
        let event = self.append(self.by_agent(requested).parent(parent)).await?;
        Ok(OpenRequest {
            request_id,
            event_id: event.id.clone(),
            repos: Vec::new(),
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
        let asked = match &waiting {
            Some(open) => Some(open.event_id.clone()),
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
                    .map(|open| open.event_id),
            },
        };
        let resolved = EventBody::PermissionResolved {
            request_id: request_id.clone(),
            decision,
            by: DecisionSource::User,
            message: message.clone(),
        };
        self.append(self.by_agent(resolved).parent(asked)).await?;
        if let Some(open) = waiting {
            self.forward_decision(open, decision, message);
        }
        if self.open.is_empty() && self.status == SessionStatus::Waiting {
            self.set_status(SessionStatus::Running, None).await?;
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
        let closed = self
            .by_agent(expired(open.request_id))
            .parent(Some(open.event_id));
        self.append(closed).await?;
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
    /// there, or has ended for good.
    async fn set_agent_status(&mut self, status: AgentStatus) -> Result<()> {
        let Some(lead) = &self.lead else {
            return Ok(());
        };
        let restarting = status == AgentStatus::Running;
        if lead.status == status || (agent_ended(lead.status) && !restarting) {
            return Ok(());
        }
        let draft = self
            .by_agent(EventBody::AgentStatus { status })
            .parent(self.cause.clone());
        self.append(draft).await?;
        Ok(())
    }

    /// A draft attributed to the lead agent, when there is one.
    fn by_agent(&self, body: EventBody) -> EventDraft {
        let draft = EventDraft::new(body);
        match &self.lead {
            Some(lead) => draft.agent(lead.id.clone()),
            None => draft,
        }
    }

    /// Appends what the agent said, and what follows from it.
    async fn agent_event(&mut self, body: EventBody) -> Result<()> {
        let draft = self.by_agent(body).parent(self.cause.clone());
        let event = self.append(draft).await?;
        match &event.body {
            EventBody::ItemStarted { item } => {
                self.turn_items.insert(item.id.clone(), event.id.clone());
            }
            EventBody::ItemCompleted { item } => {
                self.turn_items.remove(&item.id);
                self.file_changes(&event.id, item).await?;
            }
            _ => {}
        }
        Ok(())
    }

    /// One `file_changed` per file a completed edit, delete or move touched,
    /// caused by the tool call: the link a graph view follows from a change
    /// back to what made it.
    async fn file_changes(&mut self, completed_id: &str, item: &Item) -> Result<()> {
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
        for location in locations {
            let (session_repo_id, path) = locate(&self.repos, location);
            let changed = EventBody::FileChanged {
                session_repo_id,
                path,
                op,
            };
            let draft = self.by_agent(changed).parent(Some(completed_id.to_owned()));
            self.append(draft).await?;
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
            EventBody::SessionStatus { status, .. } => self.status = *status,
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
            _ => {}
        }
        Ok(event)
    }
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
            let worktree = calin_git::create_worktree(&clone, &plan.worktree, &plan.branch, None)?;
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
