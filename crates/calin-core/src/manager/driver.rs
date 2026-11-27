//! A session's driver: the one task that changes the session. It takes the
//! manager's commands, the agent's output and the results of preparing the
//! repos, one at a time, and appends what follows from each.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use calin_agents::{AgentHandle, Exit, Runner, RunnerOutput, StartParams};
use calin_proto::{
    Agent, AgentKind, AgentStatus, AttachRepoRequest, DecisionSource, Event, EventBody, FileOp,
    Item, ItemContent, ItemStatus, PermissionDecision, PermissionMode, PermissionRequest, Repo,
    RepoMode, SessionDetail, SessionRepo, SessionStatus, ToolKind,
};
use tokio::sync::mpsc;
use ulid::Ulid;

use super::{
    Command, Inner, Reply, agent_ended, closed, expired, has_turn, is_closed, no_longer_active,
    no_turn, takes_messages,
};
use crate::error::{Error, Result};
use crate::repos::{Reservation, blocking};
use crate::store::{EventDraft, SessionSetup};
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
}

enum Prepared {
    Repo(SessionRepo),
    Failed(String),
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
        }
    }

    pub(super) async fn run(mut self) {
        let retired = self.drive().await;
        if !retired {
            self.inner.drivers().remove(&self.session_id);
        }
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
            let step = if self.handle.is_none() && self.preparation.is_none() {
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
            Some(RunnerOutput::PermissionRequest(request)) => {
                self.permission_requested(request).await
            }
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
        if self.setup.repos.len() <= self.repos.len() {
            return self.begin_turn(text, cause).await;
        }
        self.set_status(SessionStatus::Preparing, None).await?;
        let wanted: Vec<AttachRepoRequest> = self.setup.repos[self.repos.len()..].to_vec();
        let branch = calin_git::session_branch_name(&self.title, &self.session_id);
        let work_dir = self.inner.store.home().work_dir(&self.session_id);
        let mut plans = Vec::with_capacity(wanted.len());
        for wanted in wanted {
            let Some(repo) = self.inner.repos.get(&wanted.repo_id).await else {
                let message = format!("repo {} is no longer registered", wanted.repo_id);
                return self.fail(message).await;
            };
            plans.push(AttachPlan {
                session_id: self.session_id.clone(),
                worktree: work_dir.join(&repo.name),
                repo,
                mode: wanted.mode.unwrap_or_default(),
                branch: branch.clone(),
                // Repos attach in order, so the first one ever is the primary.
                primary: self.repos.is_empty() && plans.is_empty(),
            });
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

    async fn on_prepared(&mut self, prepared: Option<Prepared>) -> Result<()> {
        match prepared {
            Some(Prepared::Repo(repo)) => self.attach(repo).await,
            Some(Prepared::Failed(message)) => {
                self.preparation = None;
                self.fail(message).await
            }
            None => match self.preparation.take() {
                Some(preparation) => self.begin_turn(preparation.text, preparation.cause).await,
                None => Ok(()),
            },
        }
    }

    async fn attach(&mut self, repo: SessionRepo) -> Result<()> {
        let attached = EventBody::RepoAttached { repo };
        self.append(EventDraft::new(attached).parent(self.cause.clone()))
            .await?;
        Ok(())
    }

    /// Starts a turn on `text`, starting the agent first if it is not running.
    async fn begin_turn(&mut self, text: String, cause: String) -> Result<()> {
        self.cause = Some(cause);
        self.interrupted = false;
        if self.status != SessionStatus::Running {
            self.set_status(SessionStatus::Running, None).await?;
        }
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
            let add_dirs = self
                .repos
                .iter()
                .filter(|repo| Some(&repo.id) != main.as_ref())
                .map(|repo| PathBuf::from(&repo.path))
                .collect();
            let agent_id = match &self.lead {
                Some(lead) => lead.id.clone(),
                None => Ulid::generate().to_string(),
            };
            let started = runner.start(StartParams {
                session_id: self.session_id.clone(),
                agent_id: agent_id.clone(),
                prompt: text,
                cwd,
                add_dirs,
                permission_mode: self.permission_mode,
                model: self.setup.model.clone(),
                resume: self
                    .lead
                    .as_ref()
                    .and_then(|lead| lead.native_session_id.clone()),
                start_seq: self.head_seq,
            });
            match started {
                Ok(handle) => self.handle = Some(handle),
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
            let sent = self
                .handle
                .as_ref()
                .is_some_and(|handle| handle.send_message(next.text).is_ok());
            if !sent {
                return self.fail("the agent is no longer running".to_owned()).await;
            }
            return Ok(());
        }
        self.in_turn = false;
        self.set_agent_status(AgentStatus::Idle).await?;
        self.set_status(SessionStatus::Review, None).await
    }

    async fn agent_exited(&mut self) -> Result<()> {
        self.handle = None;
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
                        Some(Prepared::Repo(repo)) => self.attach(repo).await?,
                        Some(Prepared::Failed(_)) | None => break,
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
                        Some(RunnerOutput::PermissionRequest(request)) => {
                            // Asked while stopping: nobody will answer it.
                            let open = self.record_request(request).await?;
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
        self.in_turn = false;
        self.interrupted = false;
        self.pending.clear();
        self.turn_items.clear();
        self.set_agent_status(AgentStatus::Stopped).await?;
        self.set_status(SessionStatus::Cancelled, None).await
    }

    /// Records that the session cannot go on and fails it.
    async fn fail(&mut self, message: String) -> Result<()> {
        self.handle = None;
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

    async fn permission_requested(&mut self, request: PermissionRequest) -> Result<()> {
        let open = self.record_request(request).await?;
        if !self.in_turn {
            // A request lives no longer than its turn, and there is none.
            return self.expire(open).await;
        }
        if self.permission_mode == PermissionMode::Autonomous {
            let allowed = EventBody::PermissionResolved {
                request_id: open.request_id.clone(),
                decision: PermissionDecision::Allow,
                by: DecisionSource::Policy,
                message: None,
            };
            self.append(self.by_agent(allowed).parent(Some(open.event_id)))
                .await?;
            self.forward_decision(open.request_id, PermissionDecision::Allow, None);
            return Ok(());
        }
        self.open.push(open);
        if self.status != SessionStatus::Waiting {
            let reason = Some(WAITING_FOR_PERMISSION.to_owned());
            self.set_status(SessionStatus::Waiting, reason).await?;
        }
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
        })
    }

    async fn answer_permission(
        &mut self,
        request_id: String,
        decision: PermissionDecision,
        message: Option<String>,
    ) -> Result<()> {
        let store = &self.inner.store;
        // The `permission_requested` event: the cause of the answer.
        let asked = match self
            .open
            .iter()
            .position(|open| open.request_id == request_id)
        {
            Some(index) => Some(self.open.remove(index).event_id),
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
        self.forward_decision(request_id, decision, message);
        if self.open.is_empty() && self.status == SessionStatus::Waiting {
            self.set_status(SessionStatus::Running, None).await?;
        }
        Ok(())
    }

    fn forward_decision(
        &self,
        request_id: String,
        decision: PermissionDecision,
        message: Option<String>,
    ) {
        if let Some(handle) = &self.handle {
            // An agent that is gone says so on its output, which is handled there.
            let _ = handle.resolve_permission(request_id, decision, message);
        }
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

/// Makes the repos ready one after the other and reports each. Ends at the
/// first failure, or early when cancelled.
async fn prepare(plans: Vec<AttachPlan>, results: mpsc::Sender<Prepared>, cancel: Arc<AtomicBool>) {
    for plan in plans {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let name = plan.repo.name.clone();
        let prepared = match blocking(move || attach(plan)).await {
            Ok(repo) => Prepared::Repo(repo),
            Err(err) => Prepared::Failed(format!("could not prepare repo {name}: {err}")),
        };
        let failed = matches!(prepared, Prepared::Failed(_));
        if results.send(prepared).await.is_err() || failed {
            return;
        }
    }
}

/// Blocking: a worktree on the session's branch for a `write` repo, the
/// clone as it is for the others (design spec, section 3, "Repo set").
fn attach(plan: AttachPlan) -> Result<SessionRepo> {
    let clone = PathBuf::from(&plan.repo.path);
    let (path, branch, base_ref) = match plan.mode {
        RepoMode::Write => {
            let worktree = calin_git::create_worktree(&clone, &plan.worktree, &plan.branch, None)?;
            let path = worktree.path.to_str().map(str::to_owned).ok_or_else(|| {
                Error::Invalid(format!(
                    "{} is not valid UTF-8",
                    worktree.path.to_string_lossy()
                ))
            })?;
            (path, Some(worktree.branch), Some(worktree.base_ref))
        }
        RepoMode::Read | RepoMode::Live => {
            // A clone without commits has nothing to measure changes against.
            let base = match calin_git::head_commit(&clone) {
                Ok(commit) => Some(commit),
                Err(calin_git::Error::Conflict(_)) => None,
                Err(err) => return Err(err.into()),
            };
            (plan.repo.path.clone(), None, base)
        }
    };
    Ok(SessionRepo {
        id: Ulid::generate().to_string(),
        session_id: plan.session_id,
        repo_id: plan.repo.id,
        name: plan.repo.name,
        mode: plan.mode,
        path,
        branch,
        base_ref,
        primary: plan.primary,
    })
}
