//! The session manager: creates sessions and hands every command for one to
//! its driver, which prepares the session's repos, runs its lead agent and
//! turns what the agent says into appended events.
//!
//! Each session that may still run an agent has one driver task (see
//! `driver.rs`). Everything that changes the session goes through that task,
//! one step at a time, so the order of a session's events never depends on a
//! race.

mod driver;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use calin_agents::{Runner, RunnerRegistry};
use calin_proto::{
    AgentStatus, AttachRepoRequest, CreateSessionRequest, DecisionSource, EventBody,
    PermissionDecision, RegisterRepoRequest, Repo, RepoMode, RepoSet, Session, SessionDetail,
    SessionRepo, SessionStatus, Settings,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use self::driver::Driver;
use crate::error::{Error, Result};
use crate::repos::{RepoRegistry, Reservation};
use crate::settings::SettingsStore;
use crate::store::{EventDraft, NewSession, Store};
use crate::title::default_title;
use crate::tokens::{AgentRef, Tokens};

/// How long the setup command of a new worktree may run (the task of a
/// dependency install, not of a build farm).
const SETUP_LIMIT: Duration = Duration::from_secs(10 * 60);

const RESTART_REASON: &str = "server restarted";
/// Recorded with a request that nobody answered while it could be answered.
const EXPIRED_MESSAGE: &str = "the turn ended before this request was answered";

type Reply<T = ()> = oneshot::Sender<Result<T>>;
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

/// What a manager can be told to do differently. Every value has a working
/// default; tests shorten the waits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagerConfig {
    /// How long the setup command of a new worktree may run before it is
    /// stopped and counted as failed.
    pub setup_limit: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            setup_limit: SETUP_LIMIT,
        }
    }
}

/// A repo as an agent sees it through Calin's tools.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentRepo {
    pub repo: Repo,
    /// How the repo takes part in the agent's session, when it does.
    pub attached: Option<SessionRepo>,
}

struct Inner {
    store: Store,
    registry: RunnerRegistry,
    repos: Arc<RepoRegistry>,
    settings: SettingsStore,
    config: ManagerConfig,
    tokens: Arc<Tokens>,
    /// Where agents reach Calin's tools, without the token: known once the
    /// server has bound its port. Until then agents start without tools.
    tool_endpoint: Mutex<Option<String>>,
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

    fn tool_endpoint(&self) -> Option<String> {
        // One read or one write of the value per critical section.
        let endpoint = self.tool_endpoint.lock();
        endpoint.unwrap_or_else(PoisonError::into_inner).clone()
    }
}

enum Command {
    /// Record the prompt of a session that was just created, answer with the
    /// session as it is then, and go on to prepare its repos and run its
    /// first turn.
    Start {
        runner: Arc<dyn Runner>,
        reservation: Option<Reservation>,
        reply: Reply<SessionDetail>,
    },
    Message {
        text: String,
        reply: Reply,
    },
    Interrupt {
        reply: Reply,
    },
    Stop {
        reply: Reply,
    },
    Permission {
        request_id: String,
        decision: PermissionDecision,
        message: Option<String>,
        reply: Reply,
    },
    /// Make a repo ready and add it to the session's repo set. `agent_id` is
    /// the agent that asked through Calin's tools; `None` when the user did.
    Attach {
        request: AttachRepoRequest,
        agent_id: Option<String>,
        reply: Reply<SessionRepo>,
    },
}

impl Command {
    fn refuse(self, err: Error) {
        // The caller may have stopped waiting.
        match self {
            Self::Start { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Self::Attach { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Self::Message { reply, .. }
            | Self::Interrupt { reply }
            | Self::Stop { reply }
            | Self::Permission { reply, .. } => {
                let _ = reply.send(Err(err));
            }
        }
    }
}

impl SessionManager {
    /// Wraps the store, loads the settings and the repo registry, scans for
    /// the repos the home repo declares, and settles the sessions that a
    /// previous process left in the middle of their work.
    pub async fn start(store: Store, registry: RunnerRegistry) -> Result<Self> {
        Self::start_with(store, registry, ManagerConfig::default()).await
    }

    pub async fn start_with(
        store: Store,
        registry: RunnerRegistry,
        config: ManagerConfig,
    ) -> Result<Self> {
        let settings = SettingsStore::load(store.home()).await;
        let repos = Arc::new(RepoRegistry::load(store.home().repos_path()).await);
        // The repo list must be whole before the first request reads it.
        if let Err(err) = repos.refresh(&settings.get().await).await {
            tracing::warn!(%err, "could not scan for repos; listing the registered ones only");
        }
        let manager = Self {
            inner: Arc::new(Inner {
                store,
                registry,
                repos,
                settings,
                config,
                tokens: Arc::default(),
                tool_endpoint: Mutex::new(None),
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

    /// The settings in effect, with the default clone root filled in.
    pub async fn settings(&self) -> Settings {
        self.inner.settings.get().await
    }

    /// Replaces the settings, then reads the home repo again and rescans.
    /// `Invalid` when the home repo is not the root of a git work tree or a
    /// scan root is not an existing directory; nothing is changed then.
    pub async fn update_settings(&self, settings: Settings) -> Result<Settings> {
        let stored = self.inner.settings.replace(settings).await?;
        self.inner.repos.refresh(&stored).await?;
        Ok(stored)
    }

    /// Every repo this machine knows: the home repo first, then by name.
    pub async fn list_repos(&self) -> Vec<Repo> {
        self.inner.repos.list().await
    }

    /// Reads the home repo's `.calin/repos.yaml` again, rescans for clones
    /// and returns the list that results.
    pub async fn refresh_repos(&self) -> Result<Vec<Repo>> {
        let settings = self.inner.settings.get().await;
        self.inner.repos.refresh(&settings).await?;
        Ok(self.inner.repos.list().await)
    }

    /// The home repo's named sets, in its order.
    pub async fn repo_sets(&self) -> Vec<RepoSet> {
        self.inner.repos.sets().await
    }

    /// Starts cloning a declared repo into the clone root and returns at
    /// once; the repo list shows how it goes. `Missing` for an unknown id,
    /// `Conflict` unless the repo is declared with a URL, has no clone and is
    /// not being cloned already.
    pub async fn clone_repo(&self, repo_id: &str) -> Result<()> {
        let settings = self.inner.settings.get().await;
        self.inner.repos.start_clone(repo_id, settings).await
    }

    /// Registers a local clone. `Invalid` when the path is not the root of a
    /// git work tree or the name cannot be a folder name, `Conflict` when
    /// the name or the clone is already registered.
    pub async fn register_repo(&self, request: RegisterRepoRequest) -> Result<Repo> {
        self.inner
            .repos
            .register(&request.path, request.name.as_deref())
            .await
    }

    /// Forgets a repo; nothing on disk is touched. `Conflict` while it is
    /// attached to a session that is not `done`, `failed` or `cancelled`,
    /// and for a repo the home repo declares.
    pub async fn remove_repo(&self, repo_id: &str) -> Result<()> {
        self.inner.repos.remove(repo_id, &self.inner.store).await
    }

    /// Tells the manager where agents reach Calin's tool endpoint: `base`
    /// followed by `/` and the agent's token is the URL an agent is given.
    /// Called by the server once it knows the port it listens on.
    pub fn set_tool_endpoint(&self, base: impl Into<String>) {
        let endpoint = self.inner.tool_endpoint.lock();
        *endpoint.unwrap_or_else(PoisonError::into_inner) = Some(base.into());
    }

    /// The agent a tool endpoint token was issued to, while that agent runs.
    pub fn agent_for_token(&self, token: &str) -> Option<AgentRef> {
        self.inner.tokens.agent(token)
    }

    /// Every repo, with how it takes part in the agent's session.
    pub async fn agent_repos(&self, agent: &AgentRef) -> Result<Vec<AgentRepo>> {
        let attached = self.get(&agent.session_id).await?.repos;
        let repos = self.inner.repos.list().await;
        Ok(repos
            .into_iter()
            .map(|repo| AgentRepo {
                attached: attached.iter().find(|a| a.repo_id == repo.id).cloned(),
                repo,
            })
            .collect())
    }

    /// Attaches the repo called `name` to the agent's session, for the
    /// agent. `Missing` when no repo has that name; otherwise as
    /// [`Self::attach_repo`]. An agent cannot ask for `live`: editing the
    /// user's own checkout is for the user to decide.
    pub async fn agent_attach_repo(
        &self,
        agent: &AgentRef,
        name: &str,
        mode: Option<RepoMode>,
    ) -> Result<SessionRepo> {
        if mode == Some(RepoMode::Live) {
            return Err(Error::Invalid("the mode must be write or read".to_owned()));
        }
        let repos = self.inner.repos.list().await;
        let Some(repo) = repos.into_iter().find(|repo| repo.name == name) else {
            return Err(Error::Missing(format!("no repository is named {name}")));
        };
        let request = AttachRepoRequest {
            repo_id: repo.id,
            mode,
        };
        self.attach(&agent.session_id, request, Some(agent.agent_id.clone()))
            .await
    }

    /// Adds a repo to a session that is `running`, `waiting` or `review`:
    /// a worktree on the session's branch for `write`, the clone itself for
    /// `live` and `read`. Returns once the repo is ready, its setup command
    /// included. `Missing` for an unknown repo; `Conflict` in any other
    /// status, when the repo is already attached and when it has no clone.
    pub async fn attach_repo(
        &self,
        session_id: &str,
        request: AttachRepoRequest,
    ) -> Result<SessionRepo> {
        self.attach(session_id, request, None).await
    }

    async fn attach(
        &self,
        session_id: &str,
        request: AttachRepoRequest,
        agent_id: Option<String>,
    ) -> Result<SessionRepo> {
        let status = self.get(session_id).await?.session.status;
        if !takes_repos(status) {
            return Err(no_attaching(status));
        }
        self.command(session_id, |reply| Command::Attach {
            request,
            agent_id,
            reply,
        })
        .await
    }

    /// Creates a session and returns it as soon as its prompt is recorded,
    /// with status `queued`. Its repos are then prepared and its lead agent
    /// started in the background; both show up as events.
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
        let model = request
            .model
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty());
        // Before anything is created: every repo must exist, once.
        let reservation = if request.repos.is_empty() {
            None
        } else {
            let ids: Vec<&str> = request
                .repos
                .iter()
                .map(|repo| repo.repo_id.as_str())
                .collect();
            Some(self.inner.repos.reserve(&ids).await?)
        };
        let session = self
            .inner
            .store
            .create_session(NewSession {
                title,
                prompt: request.prompt,
                // The runner that actually runs, which in dry run is the mock.
                runner: runner.id().to_owned(),
                permission_mode: request.permission_mode.unwrap_or_default(),
                model,
                repos: request.repos,
            })
            .await?;
        self.command(&session.id, |reply| Command::Start {
            runner,
            reservation,
            reply,
        })
        .await
    }

    /// Sends user text to the session's lead agent. It starts a turn right
    /// away, or after the current one when the agent is busy. On a `failed`
    /// or `cancelled` session the agent is started again and continues its
    /// conversation.
    pub async fn send_message(&self, session_id: &str, text: &str) -> Result<()> {
        if text.trim().is_empty() {
            return Err(Error::Invalid("the message must not be blank".to_owned()));
        }
        let status = self.get(session_id).await?.session.status;
        if !takes_messages(status) {
            return Err(closed(status));
        }
        let text = text.to_owned();
        self.command(session_id, |reply| Command::Message { text, reply })
            .await
    }

    /// Asks the agent to end its current turn. The session returns to
    /// `review` when the agent has done so.
    pub async fn interrupt(&self, session_id: &str) -> Result<()> {
        let status = self.get(session_id).await?.session.status;
        if !has_turn(status) {
            return Err(no_turn(status));
        }
        self.command(session_id, |reply| Command::Interrupt { reply })
            .await
    }

    /// Ends the session's agent and cancels the session.
    pub async fn stop(&self, session_id: &str) -> Result<()> {
        let status = self.get(session_id).await?.session.status;
        if is_closed(status) {
            return Err(closed(status));
        }
        self.command(session_id, |reply| Command::Stop { reply })
            .await
    }

    /// Answers a permission request. `Missing` when the session never had
    /// such a request, `Conflict` when it was already answered.
    pub async fn resolve_permission(
        &self,
        session_id: &str,
        request_id: &str,
        decision: PermissionDecision,
        message: Option<String>,
    ) -> Result<()> {
        self.get(session_id).await?;
        let request_id = request_id.to_owned();
        self.command(session_id, |reply| Command::Permission {
            request_id,
            decision,
            message,
            reply,
        })
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

    /// A session, its agents and its repo set.
    pub async fn get(&self, session_id: &str) -> Result<SessionDetail> {
        self.inner.store.session(session_id).await
    }

    /// Hands a command to the session's driver, starting one if the session
    /// has none (after a restart, or once its agent exited).
    async fn command<T>(
        &self,
        session_id: &str,
        make: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T> {
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
        // A driver that ended before answering could not go on.
        answer.await.unwrap_or_else(|_| Err(no_longer_active()))
    }

    fn spawn_driver(&self, session_id: &str) -> DriverHandle {
        let (sender, commands) = mpsc::unbounded_channel();
        let driver = Driver::new(Arc::clone(&self.inner), session_id.to_owned(), commands);
        DriverHandle {
            commands: sender,
            task: tokio::spawn(driver.run()),
        }
    }

    /// A session that was queued, preparing, running or waiting when the
    /// process started lost its agent with the previous process, and a
    /// permission request left open has nobody waiting for its answer.
    async fn recover(&self) -> Result<()> {
        let store = &self.inner.store;
        let mut open: HashMap<String, Vec<_>> = HashMap::new();
        for request in store.open_permissions(None).await? {
            open.entry(request.session_id.clone())
                .or_default()
                .push(request);
        }
        for session in store.sessions().await? {
            let lost = matches!(
                session.status,
                SessionStatus::Queued
                    | SessionStatus::Preparing
                    | SessionStatus::Running
                    | SessionStatus::Waiting
            );
            let requests = open.remove(&session.id).unwrap_or_default();
            if !lost && requests.is_empty() {
                continue;
            }
            let result = async {
                for request in requests {
                    let mut closing =
                        EventDraft::new(expired(request.request_id)).parent(Some(request.event_id));
                    closing.agent_id = request.agent_id;
                    store.append(&session.id, closing).await?;
                }
                if !lost {
                    return Ok(());
                }
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
                store.append(&session.id, EventDraft::new(failed)).await?;
                Ok::<(), Error>(())
            }
            .await;
            match result {
                Ok(()) => {
                    tracing::info!(session = %session.id, "settled a session cut short by a restart");
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

/// True once the session is over for good or until the user continues it.
fn is_closed(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Done | SessionStatus::Failed | SessionStatus::Cancelled
    )
}

/// Everything but `done` takes a message: a `failed` or `cancelled` session
/// is continued by one (the semantics in `calin-proto/src/api.rs`).
fn takes_messages(status: SessionStatus) -> bool {
    status != SessionStatus::Done
}

/// True while a repo can join the session: its agent is at work or waits for
/// the next message (the semantics in `calin-proto/src/api.rs`).
fn takes_repos(status: SessionStatus) -> bool {
    matches!(
        status,
        SessionStatus::Running | SessionStatus::Waiting | SessionStatus::Review
    )
}

fn no_attaching(status: SessionStatus) -> Error {
    Error::Conflict(format!(
        "a repo can only be attached while the session is running, waiting or in review; it is {}",
        status_name(status)
    ))
}

/// True while an agent's turn is in progress.
fn has_turn(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Running | SessionStatus::Waiting)
}

fn agent_ended(status: AgentStatus) -> bool {
    matches!(
        status,
        AgentStatus::Done | AgentStatus::Failed | AgentStatus::Stopped
    )
}

fn status_name(status: SessionStatus) -> String {
    format!("{status:?}").to_lowercase()
}

fn closed(status: SessionStatus) -> Error {
    Error::Conflict(format!("the session is {}", status_name(status)))
}

fn no_turn(status: SessionStatus) -> Error {
    Error::Conflict(format!(
        "there is no turn to interrupt: the session's status is {}",
        status_name(status)
    ))
}

fn no_longer_active() -> Error {
    Error::Conflict("the session is no longer active".to_owned())
}

/// The closing of a request nobody answered in time.
fn expired(request_id: String) -> EventBody {
    EventBody::PermissionResolved {
        request_id,
        decision: PermissionDecision::Deny,
        by: DecisionSource::Expired,
        message: Some(EXPIRED_MESSAGE.to_owned()),
    }
}

#[cfg(test)]
mod tests;
