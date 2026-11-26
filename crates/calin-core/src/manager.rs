//! The session manager: creates sessions, runs their lead agent and turns
//! what the agent says into appended events.
//!
//! Each session that may still run an agent has one driver task. Everything
//! that changes the session goes through that task, one step at a time, so
//! the order of a session's events never depends on a race.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use calin_agents::{AgentHandle, Exit, Runner, RunnerOutput, RunnerRegistry, StartParams};
use calin_proto::{
    Agent, AgentKind, AgentStatus, CreateSessionRequest, Event, EventBody, Session, SessionDetail,
    SessionStatus,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::store::{EventDraft, NewSession, Store};
use crate::title::default_title;

/// How long a stopped agent may take to exit before it is dropped.
const STOP_GRACE: Duration = Duration::from_secs(5);
const LEAD_ROLE: &str = "lead";
const RESTART_REASON: &str = "server restarted";

type Reply = oneshot::Sender<Result<()>>;
type Drivers = HashMap<String, DriverHandle>;

/// How the manager reaches a session's driver.
struct DriverHandle {
    commands: mpsc::UnboundedSender<Command>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
pub struct SessionManager {
    inner: Arc<Inner>,
}

struct Inner {
    store: Store,
    registry: RunnerRegistry,
    /// The command queue of every running driver. Commands are sent and
    /// drivers retire under this lock, so a command is never queued on a
    /// driver that has already decided to end.
    drivers: Mutex<Drivers>,
}

impl Inner {
    fn drivers(&self) -> MutexGuard<'_, Drivers> {
        // The map stays consistent even if a holder panicked: every critical
        // section is a single insert, remove or send.
        self.drivers.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

enum Command {
    /// Run the first turn of a session that was just created.
    Start {
        runner: Arc<dyn Runner>,
        reply: Reply,
    },
    Message {
        text: String,
        reply: Reply,
    },
    Stop {
        reply: Reply,
    },
}

impl Command {
    fn reply(self, result: Result<()>) {
        let (Self::Start { reply, .. } | Self::Message { reply, .. } | Self::Stop { reply }) = self;
        // The caller may have stopped waiting.
        let _ = reply.send(result);
    }
}

impl SessionManager {
    /// Wraps the store and fails the sessions that a previous process left
    /// in the middle of their work.
    pub async fn start(store: Store, registry: RunnerRegistry) -> Result<Self> {
        let manager = Self {
            inner: Arc::new(Inner {
                store,
                registry,
                drivers: Mutex::new(HashMap::new()),
            }),
        };
        manager.recover().await?;
        Ok(manager)
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn registry(&self) -> &RunnerRegistry {
        &self.inner.registry
    }

    /// Creates a session and starts its lead agent on the prompt.
    pub async fn create_session(&self, request: CreateSessionRequest) -> Result<SessionDetail> {
        if request.prompt.trim().is_empty() {
            return Err(Error::Invalid("the prompt must not be blank".to_owned()));
        }
        let requested = request.runner.as_deref().filter(|id| !id.trim().is_empty());
        let runner = self.inner.registry.resolve(requested).ok_or_else(|| {
            Error::Invalid(format!("unknown runner {}", requested.unwrap_or_default()))
        })?;
        let title = request
            .title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| default_title(&request.prompt));
        let session = self
            .inner
            .store
            .create_session(NewSession {
                title,
                prompt: request.prompt,
                // The runner that actually runs, which in dry run is the mock.
                runner: runner.id().to_owned(),
            })
            .await?;
        self.command(&session.id, |reply| Command::Start { runner, reply })
            .await?;
        self.get(&session.id).await
    }

    /// Sends user text to the session's lead agent. It starts a turn right
    /// away, or after the current one when the agent is busy.
    pub async fn send_message(&self, session_id: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Err(Error::Invalid("the message must not be blank".to_owned()));
        }
        self.ensure_open(session_id).await?;
        let text = text.to_owned();
        self.command(session_id, |reply| Command::Message { text, reply })
            .await
    }

    /// Ends the session's agent and cancels the session.
    pub async fn stop(&self, session_id: &str) -> Result<()> {
        self.ensure_open(session_id).await?;
        self.command(session_id, |reply| Command::Stop { reply })
            .await
    }

    /// Ends every agent without recording anything, for a clean exit. The
    /// sessions that were running are failed by the recovery of the next start.
    pub async fn shutdown(&self) {
        let drivers: Vec<DriverHandle> = self.inner.drivers().drain().map(|(_, d)| d).collect();
        for driver in drivers {
            // A driver ends, and drops its agent, when its command queue closes.
            drop(driver.commands);
            let _ = driver.task.await;
        }
    }

    /// Every session, newest first.
    pub async fn list(&self) -> Result<Vec<Session>> {
        self.inner.store.sessions().await
    }

    /// A session and its agents.
    pub async fn get(&self, session_id: &str) -> Result<SessionDetail> {
        self.inner.store.session(session_id).await
    }

    async fn ensure_open(&self, session_id: &str) -> Result<()> {
        let status = self.get(session_id).await?.session.status;
        if is_closed(status) {
            return Err(closed(status));
        }
        Ok(())
    }

    /// Hands a command to the session's driver, starting one if the session
    /// has none (after a restart, or once its agent exited).
    async fn command(&self, session_id: &str, make: impl FnOnce(Reply) -> Command) -> Result<()> {
        let (reply, answer) = oneshot::channel();
        let command = make(reply);
        {
            let mut drivers = self.inner.drivers();
            let driver = drivers
                .entry(session_id.to_owned())
                .or_insert_with(|| self.spawn_driver(session_id));
            if let Err(returned) = driver.commands.send(command) {
                // The driver died without retiring, which takes a panic. Replace it.
                let driver = self.spawn_driver(session_id);
                let _ = driver.commands.send(returned.0);
                drivers.insert(session_id.to_owned(), driver);
            }
        }
        // A driver that ended before answering found the session closed.
        answer.await.unwrap_or_else(|_| Err(no_longer_active()))
    }

    fn spawn_driver(&self, session_id: &str) -> DriverHandle {
        let (sender, commands) = mpsc::unbounded_channel();
        let driver = Driver {
            inner: Arc::clone(&self.inner),
            session_id: session_id.to_owned(),
            commands,
            status: SessionStatus::Queued,
            prompt: String::new(),
            runner_id: String::new(),
            head_seq: 0,
            runner: None,
            lead: None,
            handle: None,
            in_turn: false,
            cause: None,
            pending: VecDeque::new(),
        };
        DriverHandle {
            commands: sender,
            task: tokio::spawn(driver.run()),
        }
    }

    /// A session that was queued, preparing or running when the process
    /// started lost its agent with the previous process.
    async fn recover(&self) -> Result<()> {
        let store = &self.inner.store;
        for session in store.sessions().await? {
            if !matches!(
                session.status,
                SessionStatus::Queued | SessionStatus::Preparing | SessionStatus::Running
            ) {
                continue;
            }
            let result = async {
                for agent in store.session(&session.id).await?.agents {
                    if !agent_ended(agent.status) {
                        let failed = EventBody::AgentStatus {
                            status: AgentStatus::Failed,
                        };
                        store
                            .append(&session.id, EventDraft::new(failed).agent(agent.id))
                            .await?;
                    }
                }
                let failed = EventBody::SessionStatus {
                    status: SessionStatus::Failed,
                    reason: Some(RESTART_REASON.to_owned()),
                };
                store.append(&session.id, EventDraft::new(failed)).await
            }
            .await;
            match result {
                Ok(_) => {
                    tracing::info!(session = %session.id, "failed a session cut short by a restart");
                }
                // One broken session must not keep the server from starting.
                Err(err) => {
                    tracing::error!(session = %session.id, %err, "could not recover the session");
                }
            }
        }
        Ok(())
    }
}

/// True once nothing can be sent to the session any more.
fn is_closed(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Done | SessionStatus::Failed | SessionStatus::Cancelled
    )
}

fn agent_ended(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Done | AgentStatus::Failed | AgentStatus::Stopped
    )
}

fn closed(status: SessionStatus) -> Error {
    let name = format!("{status:?}").to_lowercase();
    Error::Conflict(format!("the session is {name}"))
}

fn no_longer_active() -> Error {
    Error::Conflict("the session is no longer active".to_owned())
}

/// The lead agent as the driver knows it.
struct Lead {
    id: String,
    status: AgentStatus,
}

/// A user message waiting for the current turn to end.
struct Queued {
    text: String,
    event_id: String,
}

/// The driver hit a storage error and cannot go on.
struct Halt;

enum Step {
    Command(Command),
    Output(Option<RunnerOutput>),
}

struct Driver {
    inner: Arc<Inner>,
    session_id: String,
    commands: mpsc::UnboundedReceiver<Command>,
    status: SessionStatus,
    prompt: String,
    runner_id: String,
    head_seq: u64,
    runner: Option<Arc<dyn Runner>>,
    lead: Option<Lead>,
    /// The running agent. `None` before the first turn, after the agent
    /// exited and after a restart; the next turn starts it (again).
    handle: Option<AgentHandle>,
    in_turn: bool,
    /// The user message the current turn answers: the parent of its events.
    cause: Option<String>,
    pending: VecDeque<Queued>,
}

impl Driver {
    async fn run(mut self) {
        let retired = self.drive().await;
        if !retired {
            self.inner.drivers().remove(&self.session_id);
        }
        // Off the map, so nothing new arrives; what is queued came too late.
        self.commands.close();
        while let Ok(command) = self.commands.try_recv() {
            command.reply(Err(if is_closed(self.status) {
                closed(self.status)
            } else {
                no_longer_active()
            }));
        }
    }

    /// Runs until the session closes or has no agent and nothing to do.
    /// Returns true when the driver already took itself off the map.
    async fn drive(&mut self) -> bool {
        match self.inner.store.session(&self.session_id).await {
            Ok(detail) => self.adopt(detail),
            Err(err) => {
                tracing::error!(session = %self.session_id, %err, "could not load the session");
                return false;
            }
        }
        loop {
            if is_closed(self.status) {
                return false;
            }
            let step = match self.handle.as_mut() {
                Some(handle) => tokio::select! {
                    command = self.commands.recv() => match command {
                        Some(command) => Step::Command(command),
                        None => return false,
                    },
                    output = handle.next_output() => Step::Output(output),
                },
                None => match self.commands.try_recv() {
                    Ok(command) => Step::Command(command),
                    Err(_) => {
                        let mut drivers = self.inner.drivers();
                        if self.commands.is_empty() {
                            drivers.remove(&self.session_id);
                            return true;
                        }
                        continue;
                    }
                },
            };
            let result = match step {
                Step::Command(command) => self.on_command(command).await,
                Step::Output(output) => self.on_output(output).await.map_err(|err| {
                    tracing::error!(session = %self.session_id, %err, "could not record agent output");
                    Halt
                }),
            };
            if result.is_err() {
                return false;
            }
        }
    }

    fn adopt(&mut self, detail: SessionDetail) {
        self.status = detail.session.status;
        self.prompt = detail.session.prompt;
        self.runner_id = detail.session.runner;
        self.head_seq = detail.session.head_seq;
        self.lead = detail
            .agents
            .into_iter()
            .rev()
            .find(|agent| agent.role == LEAD_ROLE)
            .map(|agent| Lead {
                id: agent.id,
                status: agent.status,
            });
    }

    async fn on_command(&mut self, command: Command) -> std::result::Result<(), Halt> {
        let (result, reply) = match command {
            Command::Start { runner, reply } => {
                self.runner = Some(runner);
                (self.first_turn().await, reply)
            }
            Command::Message { text, reply } => (self.message(text).await, reply),
            Command::Stop { reply } => (self.stop().await, reply),
        };
        let halt = match &result {
            Ok(()) => Ok(()),
            Err(err) => {
                tracing::error!(session = %self.session_id, %err, "session command failed");
                Err(Halt)
            }
        };
        let _ = reply.send(result);
        halt
    }

    async fn on_output(&mut self, output: Option<RunnerOutput>) -> Result<()> {
        match output {
            Some(RunnerOutput::Event(body)) => self.append_from_agent(body).await,
            Some(RunnerOutput::TurnEnded) => self.turn_ended().await,
            Some(RunnerOutput::Exited(Exit::Ok)) => {
                self.handle = None;
                self.in_turn = false;
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
            Some(RunnerOutput::Exited(Exit::Error(message))) => self.fail(message).await,
            None => {
                self.fail("the agent ended without a result".to_owned())
                    .await
            }
        }
    }

    async fn first_turn(&mut self) -> Result<()> {
        self.set_status(SessionStatus::Queued, None).await?;
        let prompt = self.prompt.clone();
        let message = self
            .append(EventDraft::new(EventBody::UserMessage {
                text: prompt.clone(),
            }))
            .await?;
        self.begin_turn(prompt, message.id.clone()).await
    }

    async fn message(&mut self, text: String) -> Result<()> {
        let message = self
            .append(EventDraft::new(EventBody::UserMessage {
                text: text.clone(),
            }))
            .await?;
        if self.in_turn {
            self.pending.push_back(Queued {
                text,
                event_id: message.id.clone(),
            });
            Ok(())
        } else {
            self.begin_turn(text, message.id.clone()).await
        }
    }

    /// Starts a turn on `text`, starting the agent first if it is not running.
    async fn begin_turn(&mut self, text: String, cause: String) -> Result<()> {
        self.cause = Some(cause);
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
            let agent_id = match &self.lead {
                Some(lead) => lead.id.clone(),
                None => Ulid::generate().to_string(),
            };
            let started = runner.start(StartParams {
                session_id: self.session_id.clone(),
                agent_id: agent_id.clone(),
                prompt: text,
                resume: self.lead.is_some(),
                start_seq: self.head_seq,
            });
            match started {
                Ok(handle) => self.handle = Some(handle),
                Err(err) => return self.fail(err.to_string()).await,
            }
            if self.lead.is_some() {
                self.set_agent_status(AgentStatus::Running).await?;
            } else {
                let agent = Agent {
                    id: agent_id.clone(),
                    session_id: self.session_id.clone(),
                    parent_agent_id: None,
                    kind: AgentKind::Process,
                    role: LEAD_ROLE.to_owned(),
                    runner: runner.id().to_owned(),
                    model: runner.model(),
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
        if let Some(next) = self.pending.pop_front() {
            self.cause = Some(next.event_id);
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

    async fn stop(&mut self) -> Result<()> {
        if let Some(mut handle) = self.handle.take()
            && handle.stop().is_ok()
        {
            // Keep what the agent said before it noticed the request.
            let grace = tokio::time::sleep(STOP_GRACE);
            tokio::pin!(grace);
            loop {
                tokio::select! {
                    output = handle.next_output() => match output {
                        Some(RunnerOutput::Event(body)) => self.append_from_agent(body).await?,
                        Some(RunnerOutput::TurnEnded) => {}
                        Some(RunnerOutput::Exited(_)) | None => break,
                    },
                    // Dropping the handle ends an agent that ignores the request.
                    () = &mut grace => break,
                }
            }
        }
        self.in_turn = false;
        self.pending.clear();
        self.set_agent_status(AgentStatus::Stopped).await?;
        self.set_status(SessionStatus::Cancelled, None).await
    }

    /// Records that the agent cannot go on and fails the session.
    async fn fail(&mut self, message: String) -> Result<()> {
        self.handle = None;
        self.in_turn = false;
        self.pending.clear();
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
        let draft = EventDraft::new(EventBody::AgentStatus { status })
            .agent(lead.id.clone())
            .parent(self.cause.clone());
        self.append(draft).await?;
        Ok(())
    }

    async fn append_from_agent(&mut self, body: EventBody) -> Result<()> {
        let mut draft = EventDraft::new(body).parent(self.cause.clone());
        if let Some(lead) = &self.lead {
            draft = draft.agent(lead.id.clone());
        }
        self.append(draft).await?;
        Ok(())
    }

    /// Appends to the session's log and keeps the driver's view of the
    /// session in step with what it just wrote.
    async fn append(&mut self, draft: EventDraft) -> Result<Arc<Event>> {
        let event = self.inner.store.append(&self.session_id, draft).await?;
        self.head_seq = event.seq;
        match &event.body {
            EventBody::SessionStatus { status, .. } => self.status = *status,
            EventBody::AgentStatus { status } => {
                if let Some(lead) = &mut self.lead
                    && event.agent_id.as_deref() == Some(lead.id.as_str())
                {
                    lead.status = *status;
                }
            }
            _ => {}
        }
        Ok(event)
    }
}

#[cfg(test)]
mod tests;
