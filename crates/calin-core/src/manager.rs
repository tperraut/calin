//! The session manager: creates sessions and hands every command for one to
//! its driver, which prepares the session's repos, runs its lead agent and
//! turns what the agent says into appended events.
//!
//! Each session that may still run an agent has one driver task (see
//! `driver.rs`). Everything that changes the session goes through that task,
//! one step at a time, so the order of a session's events never depends on a
//! race.

mod automate;
mod delivery;
mod driver;
mod issues;
mod queue;
mod unreported;

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::{Duration, Instant};

use calin_agents::{Runner, RunnerRegistry};
use calin_git::GitHub;
use calin_proto::{
    AgentMessage, AgentStatus, AttachRepoRequest, CreateSessionRequest, DecisionSource, EventBody,
    Graph, IssueRef, MessageKind, NodePhase, NodeRunState, PermissionDecision, RegisterRepoRequest,
    Repo, RepoMode, RepoSet, RunnerLimits, Session, SessionDetail, SessionRepo, SessionStatus,
    Settings, ShipRequest, Timeline, Usage, UsageBy, WorkflowInfo, WorkflowRun, WorkflowStatus,
};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::MissedTickBehavior;

use self::automate::Scheduler;
use self::driver::Driver;
use self::queue::Queue;
use crate::automations::AutomationStore;
use crate::briefing;
use crate::clock::{SharedClock, TimeZone};
use crate::error::{Error, Result};
use crate::limits::LimitBoard;
use crate::repos::{RepoRegistry, Reservation};
use crate::settings::{DEFAULT_MAX_PARALLEL, SettingsStore};
use crate::store::{EventDraft, NewSession, Store, UsageQuery, resolve_range};
use crate::title::default_title;
use crate::tokens::{AgentRef, Tokens};
use crate::workflows::WorkflowCatalog;

/// How long the setup command of a new worktree may run (the task of a
/// dependency install, not of a build farm).
const SETUP_LIMIT: Duration = Duration::from_secs(10 * 60);

/// The GitHub CLI, found on `PATH`.
const GH_BIN: &str = "gh";
/// One call to GitHub through `gh`: a round trip or two, never a transfer.
const GH_TIMEOUT: Duration = Duration::from_secs(60);
/// How often the pull requests of a `shipping` session are looked at. A
/// merge or a finished check shows within the minute, at one `gh` call per
/// pull request.
const PULL_REQUEST_POLL: Duration = Duration::from_secs(60);
/// How long an answer about `gh` (is it there, is it signed in) is trusted.
const GITHUB_STATUS_TTL: Duration = Duration::from_secs(60);
/// How often issue and pull request triggers ask GitHub (integrations spec, section 3).
const TRIGGER_POLL: Duration = Duration::from_secs(5 * 60);
/// How long a commit Calin makes may take, its hooks included: a hook that
/// waits for something nobody will give must not hold up a ship for good.
const COMMIT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a push may take: a large first push over a slow link, but not
/// a remote that never answers.
const PUSH_TIMEOUT: Duration = Duration::from_secs(300);

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
    /// The GitHub CLI that opens and reads pull requests: a name looked up
    /// on `PATH`, or a path. Never run in dry run.
    pub gh_bin: PathBuf,
    /// How long one invocation of it may take before it is killed.
    pub gh_timeout: Duration,
    /// How often the pull requests of a `shipping` session are looked at.
    pub pull_request_poll: Duration,
    /// What time it is, for usage ranges, limits and automations.
    pub clock: SharedClock,
    /// The server's local time zone: where usage days begin and what
    /// automation schedules mean. The system's by default.
    pub time_zone: TimeZone,
    /// How often issue and pull request triggers ask GitHub.
    pub trigger_poll: Duration,
    /// Runs automations in the background. Tests turn it off and drive
    /// them with [`SessionManager::automations_tick`].
    pub automations: bool,
    /// How long a commit Calin makes (when shipping, or at a workflow's
    /// `commit` node) may take before git and its hooks are killed.
    pub commit_timeout: Duration,
    /// How long a push may take before git and what it started are killed.
    pub push_timeout: Duration,
}

impl Default for ManagerConfig {
    fn default() -> Self {
        Self {
            setup_limit: SETUP_LIMIT,
            gh_bin: PathBuf::from(GH_BIN),
            gh_timeout: GH_TIMEOUT,
            pull_request_poll: PULL_REQUEST_POLL,
            clock: SharedClock::default(),
            time_zone: TimeZone::system(),
            trigger_poll: TRIGGER_POLL,
            automations: true,
            commit_timeout: COMMIT_TIMEOUT,
            push_timeout: PUSH_TIMEOUT,
        }
    }
}

/// A child session an agent just dispatched.
#[derive(Clone, Debug, PartialEq)]
pub struct Dispatched {
    pub session: Session,
    /// The repos the child was given, in the parent's order.
    pub repos: Vec<DispatchedRepo>,
}

/// One repo of a child session, as it was when the child was dispatched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchedRepo {
    pub name: String,
    /// The parent's commit, where the child's branch starts.
    pub commit: String,
    /// The parent's worktree held uncommitted work, which the child does not have.
    pub dirty: bool,
}

/// What became of a note an agent sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteOutcome {
    /// Addressed to the user: it shows in the transcript.
    Shown,
    /// Handed to the other session, whose agent gets it as its next input.
    Delivered,
    /// Recorded, but the other session is over and nobody will act on it.
    Unread(SessionStatus),
}

/// Who an agent's note is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum NoteTarget {
    User,
    Parent,
    Child(String),
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
    /// The home repo's workflows, read again when their files change.
    workflows: WorkflowCatalog,
    /// Who may prepare or run now, and who waits for it.
    queue: Queue,
    config: ManagerConfig,
    /// How `gh` is run, from the config.
    github: GitHub,
    /// Why GitHub could not be used when that was last checked (`None` in
    /// the pair when it could), and when. Held while checking, so that a
    /// burst of requests asks `gh` once.
    github_status: tokio::sync::Mutex<Option<(Instant, Option<String>)>>,
    /// Looks at the pull requests of `shipping` sessions. `None` in dry run.
    watcher: Mutex<Option<JoinHandle<()>>>,
    tokens: Arc<Tokens>,
    /// Where agents reach Calin's tools, without the token: known once the
    /// server has bound its port. Until then agents start without tools.
    tool_endpoint: Mutex<Option<String>>,
    /// What each running agent's repos looked like at its last turn end,
    /// to find the changes it made without a tool call (`unreported.rs`).
    trackers: unreported::Trackers,
    /// The command queue of every running driver. Commands are sent and
    /// drivers retire under this lock, so a command is never queued on a
    /// driver that has already decided to end.
    drivers: Mutex<Drivers>,
    /// The newest plan limits of each runner, and which were logged.
    limits: LimitBoard,
    automations: AutomationStore,
    scheduler: Scheduler,
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

    /// Why pull requests cannot be opened from this server, or `None` when
    /// they can. Dry run never uses GitHub, so it has no problem with it.
    /// The answer is remembered for a minute; with `recheck`, a remembered
    /// problem is asked about again, so that fixing it works at once.
    async fn github_problem(&self, recheck: bool) -> Option<String> {
        if self.registry.dry_run() {
            return None;
        }
        let mut status = self.github_status.lock().await;
        if let Some((at, problem)) = &*status {
            let stale = at.elapsed() >= GITHUB_STATUS_TTL || (recheck && problem.is_some());
            if !stale {
                return problem.clone();
            }
        }
        let github = self.github.clone();
        let problem = match tokio::task::spawn_blocking(move || github.status()).await {
            Ok(Ok(())) => None,
            Ok(Err(reason)) => Some(reason),
            Err(err) => Some(format!("the GitHub CLI could not be checked: {err}")),
        };
        *status = Some((Instant::now(), problem.clone()));
        problem
    }
}

enum Command {
    /// Record the prompt of a session that was just created, answer with the
    /// session as it is then, and go on to prepare its repos and run its
    /// first turn.
    Start {
        runner: Arc<dyn Runner>,
        reservation: Option<Reservation>,
        /// For a child session: the dispatch message that is its prompt, and
        /// the `message_sent` event of it in the parent's log.
        brief: Option<(AgentMessage, String)>,
        /// The workflow the session runs once its repos are ready; `None`
        /// for the built-in `quick` behavior.
        workflow: Option<Box<calin_workflow::Workflow>>,
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
        /// Stopped because an ancestor is: its report would go to an agent
        /// that is going away, so none is sent.
        cascade: bool,
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
    /// Commit, push and open or update the session's pull requests. The
    /// answer comes as soon as that is under way.
    Ship {
        title: Option<String>,
        body: Option<String>,
        draft: bool,
        reply: Reply,
    },
    /// Look at the session's pull requests on GitHub and record what
    /// changed. The answer comes once that is done.
    Poll {
        reply: Reply,
    },
    /// Start a child session for the agent (the "Child sessions" semantics
    /// in `calin-proto/src/api.rs`).
    Dispatch {
        agent_id: String,
        title: String,
        brief: String,
        /// Names of the repos to give the child; every `write` repo when `None`.
        repos: Option<Vec<String>>,
        reply: Reply<Dispatched>,
    },
    /// Send a note for the agent.
    Note {
        agent_id: String,
        to: NoteTarget,
        text: String,
        reply: Reply<NoteOutcome>,
    },
    /// Keep what a child's agent wants reported when its turn ends.
    Report {
        summary: String,
        reply: Reply,
    },
    /// Put a question of the agent's to the user. Answers with the question's id.
    Ask {
        agent_id: String,
        question: String,
        options: Vec<String>,
        reply: Reply<String>,
    },
    Answer {
        question_id: String,
        answer: String,
        /// The note that goes with the answer at a workflow gate.
        comment: Option<String>,
        reply: Reply,
    },
    /// A message from another session arrived for this one's agent: record
    /// it and hand `text` to the agent as its next input. Nobody waits for
    /// this, so that two sessions writing to each other never wait on each
    /// other.
    Deliver {
        message: AgentMessage,
        /// The `message_sent` event in the sender's log.
        cause: String,
        text: String,
        /// For a report: how the child session behind it ended.
        ended: Option<ChildEnd>,
    },
    /// A child session's agents cost more than they did: a workflow run of
    /// this session has a budget to check. Nobody waits for this.
    Spent,
}

/// How a child session ended when it reported. A child that was stopped
/// before it reported counts as failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChildEnd {
    Review,
    Failed,
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
            Self::Dispatch { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Self::Note { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Self::Ask { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            Self::Message { reply, .. }
            | Self::Interrupt { reply }
            | Self::Stop { reply, .. }
            | Self::Ship { reply, .. }
            | Self::Poll { reply }
            | Self::Report { reply, .. }
            | Self::Answer { reply, .. }
            | Self::Permission { reply, .. } => {
                let _ = reply.send(Err(err));
            }
            // Nobody waits for these.
            Self::Deliver { .. } | Self::Spent => {}
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
        let in_effect = settings.get().await;
        // The repo list must be whole before the first request reads it.
        if let Err(err) = repos.refresh(&in_effect).await {
            tracing::warn!(%err, "could not scan for repos; listing the registered ones only");
        }
        let workflows = WorkflowCatalog::default();
        workflows.reload(in_effect.home_repo.as_deref()).await;
        let queue = Queue::new(in_effect.max_parallel.unwrap_or(DEFAULT_MAX_PARALLEL));
        let github = GitHub::new(&config.gh_bin).with_timeout(config.gh_timeout);
        let automations = AutomationStore::load(store.home()).await;
        // Nothing missed before the start is made up (integrations spec, section 3).
        let scheduler = Scheduler::new(config.clock.now_ms());
        let manager = Self {
            inner: Arc::new(Inner {
                store,
                registry,
                repos,
                settings,
                workflows,
                queue,
                config,
                github,
                github_status: tokio::sync::Mutex::new(None),
                watcher: Mutex::new(None),
                tokens: Arc::default(),
                tool_endpoint: Mutex::new(None),
                trackers: Mutex::new(HashMap::new()),
                drivers: Mutex::new(HashMap::new()),
                limits: LimitBoard::default(),
                automations,
                scheduler,
            }),
        };
        manager.recover().await?;
        if manager.inner.config.automations {
            automate::start(&manager);
        }
        // Dry run has made-up pull requests: there is nothing to look at.
        if !manager.inner.registry.dry_run() {
            let every = manager.inner.config.pull_request_poll;
            let watcher = tokio::spawn(watch(Arc::downgrade(&manager.inner), every));
            let slot = manager.inner.watcher.lock();
            *slot.unwrap_or_else(PoisonError::into_inner) = Some(watcher);
        }
        Ok(manager)
    }

    pub fn store(&self) -> &Store {
        &self.inner.store
    }

    pub fn registry(&self) -> &RunnerRegistry {
        &self.inner.registry
    }

    /// The settings in effect, with the default clone root and the default
    /// session limit filled in.
    pub async fn settings(&self) -> Settings {
        self.inner.settings.get().await
    }

    /// Replaces the settings, then reads the home repo again and rescans.
    /// A higher session limit starts the sessions that wait at once.
    /// `Invalid` when the home repo is not the root of a git work tree, a
    /// scan root is not an existing directory or the session limit is not
    /// between 1 and 64; nothing is changed then.
    pub async fn update_settings(&self, settings: Settings) -> Result<Settings> {
        let stored = self.inner.settings.replace(settings).await?;
        let limit = stored.max_parallel.unwrap_or(DEFAULT_MAX_PARALLEL);
        self.inner.queue.set_limit(limit);
        let home = stored.home_repo.as_deref();
        self.inner.workflows.reload(home).await;
        self.inner.repos.refresh(&stored).await?;
        Ok(stored)
    }

    /// The workflows a session can run: the built-in `quick` first, then the
    /// home repo's by id. A file that does not validate is listed with its
    /// problems and no graph. The files are read again when they changed.
    pub async fn workflows(&self) -> Vec<WorkflowInfo> {
        self.inner.workflows.list().await
    }

    /// The session's workflow run: the graph it started with and where each
    /// node stands. `NotFound` for an unknown session, `Missing` for one
    /// that runs no workflow.
    pub async fn workflow_run(&self, session_id: &str) -> Result<WorkflowRun> {
        let run = self.inner.store.workflow_run(session_id).await?;
        run.ok_or_else(|| Error::Missing(format!("session {session_id} runs no workflow")))
    }

    /// Every repo this machine knows: the home repo first, then by name.
    pub async fn list_repos(&self) -> Vec<Repo> {
        self.inner.repos.list().await
    }

    /// Reads the home repo's `.calin/repos.yaml` again, rescans for clones
    /// and returns the list that results.
    pub async fn refresh_repos(&self) -> Result<Vec<Repo>> {
        let settings = self.inner.settings.get().await;
        let home = settings.home_repo.as_deref();
        self.inner.workflows.reload(home).await;
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
    /// started in the background, or its workflow when it names one; all of
    /// it shows up as events. With every slot taken it stays `queued` until
    /// one is free (the "queue" semantics in `calin-proto/src/api.rs`).
    /// `Invalid` also for a workflow that is unknown or does not validate.
    ///
    /// With `issue`, the issue is fetched first (`Invalid` when its repo or
    /// the issue is unknown, `Conflict` when GitHub cannot be asked), its
    /// repo is attached as the primary repo unless `repos` lists it, and
    /// its text follows the prompt in what the agent is handed (the
    /// transcript keeps the prompt as written).
    pub async fn create_session(&self, request: CreateSessionRequest) -> Result<SessionDetail> {
        self.create(request, None).await
    }

    /// [`Self::create_session`], for an automation when `automation_id` is set.
    pub(crate) async fn create(
        &self,
        mut request: CreateSessionRequest,
        automation_id: Option<String>,
    ) -> Result<SessionDetail> {
        // An issue can stand in for the prompt: the web app offers "extra
        // instructions (optional)" once one is picked (integrations spec, section 2).
        let blank_prompt = request.prompt.trim().is_empty();
        if blank_prompt && request.issue.is_none() {
            return Err(Error::Invalid("the prompt must not be blank".to_owned()));
        }
        // `None` for the built-in `quick`, however the request spells it.
        let workflow = self
            .inner
            .workflows
            .resolve(request.workflow.as_deref())
            .await?;
        let requested = request.runner.as_deref().filter(|id| !id.trim().is_empty());
        let runner = self.inner.registry.resolve(requested).ok_or_else(|| {
            Error::Invalid(format!("unknown runner {}", requested.unwrap_or_default()))
        })?;
        let model = request
            .model
            .map(|model| model.trim().to_owned())
            .filter(|model| !model.is_empty());
        let (issue, context) = match request.issue.take() {
            None => (None, None),
            Some(from) => {
                // The request names something that is not there: its fault, not the state's.
                let detail =
                    self.issue(&from.repo_id, from.number)
                        .await
                        .map_err(|err| match err {
                            Error::Missing(what) => Error::Invalid(what),
                            other => other,
                        })?;
                if !request
                    .repos
                    .iter()
                    .any(|repo| repo.repo_id == from.repo_id)
                {
                    let primary = AttachRepoRequest {
                        repo_id: from.repo_id.clone(),
                        mode: None,
                    };
                    request.repos.insert(0, primary);
                }
                let context = briefing::issue_context(&detail);
                if blank_prompt {
                    // The transcript and the title need something to show.
                    request.prompt = format!(
                        "Work on issue #{}: {}",
                        detail.issue.number, detail.issue.title
                    );
                }
                let issue = IssueRef {
                    repo_id: from.repo_id,
                    number: detail.issue.number,
                    title: detail.issue.title,
                    url: detail.issue.url,
                };
                (Some(issue), Some(context))
            }
        };
        let title = request
            .title
            .take()
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| default_title(&request.prompt));
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
                workflow: workflow.as_ref().map(|workflow| workflow.id().to_owned()),
                model,
                repos: request.repos,
                dispatch: None,
                issue,
                context,
                automation_id,
            })
            .await?;
        // The workflow as it is now: a later edit of its file changes
        // nothing for this session.
        self.command(&session.id, |reply| Command::Start {
            runner,
            reservation,
            brief: None,
            workflow: workflow.map(Box::new),
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
        // A turn may be waiting for a slot; the driver knows whether one is.
        if !has_turn(status) && status != SessionStatus::Queued {
            return Err(no_turn(status));
        }
        self.command(session_id, |reply| Command::Interrupt { reply })
            .await
    }

    /// Ends the session's agent and cancels the session, after doing the
    /// same to every session dispatched from it that is still at work.
    pub async fn stop(&self, session_id: &str) -> Result<()> {
        let status = self.get(session_id).await?.session.status;
        if is_closed(status) {
            return Err(closed(status));
        }
        // Deepest first, so that no child is left running under a parent
        // that is gone. They send no report: everyone they would report to
        // is being stopped too. The session itself reports to its own parent.
        for descendant in self.descendants(session_id).await?.into_iter().rev() {
            if descendant.status.is_settled() {
                continue;
            }
            match self.stop_one(&descendant.id, true).await {
                // It ended on its own meanwhile.
                Ok(()) | Err(Error::Conflict(_)) => {}
                Err(err) => return Err(err),
            }
        }
        self.stop_one(session_id, false).await
    }

    async fn stop_one(&self, session_id: &str, cascade: bool) -> Result<()> {
        self.command(session_id, |reply| Command::Stop { cascade, reply })
            .await
    }

    /// Every session dispatched from this one, directly or not: a parent
    /// before its children.
    async fn descendants(&self, session_id: &str) -> Result<Vec<Session>> {
        let mut sessions = self.inner.store.sessions().await?;
        // Oldest first, which is the order they were dispatched in.
        sessions.reverse();
        let mut found: Vec<Session> = Vec::new();
        let mut parents = vec![session_id.to_owned()];
        while let Some(parent) = parents.pop() {
            for session in &sessions {
                if session.parent_session_id.as_deref() == Some(parent.as_str()) {
                    parents.push(session.id.clone());
                    found.push(session.clone());
                }
            }
        }
        Ok(found)
    }

    /// Starts a child session for the agent: a new session with the
    /// parent's runner and permission mode, whose prompt is `brief` and
    /// whose branch in each given repo starts at the parent's current commit
    /// there (the "Child sessions" semantics in `calin-proto/src/api.rs`).
    /// `repos` names a subset of the parent's `write` repos; all of them
    /// when `None`.
    ///
    /// `Invalid` for a blank title or brief and for a repo that is not
    /// attached `write`; `Missing` for a repo the parent does not have;
    /// `Conflict` when the agent's session is itself a child, when four
    /// children of it are still running, and when it is over.
    pub async fn agent_dispatch(
        &self,
        agent: &AgentRef,
        title: &str,
        brief: &str,
        repos: Option<Vec<String>>,
    ) -> Result<Dispatched> {
        let (title, brief) = (title.trim().to_owned(), brief.trim().to_owned());
        if title.is_empty() || title.contains(['\n', '\r']) {
            return Err(Error::Invalid(
                "the title must be one line that is not blank".to_owned(),
            ));
        }
        if brief.is_empty() {
            return Err(Error::Invalid("the brief must not be blank".to_owned()));
        }
        let agent_id = agent.agent_id.clone();
        self.command(&agent.session_id, |reply| Command::Dispatch {
            agent_id,
            title,
            brief,
            repos,
            reply,
        })
        .await
    }

    /// Sends a note for the agent: to `parent` (the agent that dispatched
    /// its session), to `user`, or to a child session by id. `Invalid` for a
    /// blank text, `Missing` for a recipient that does not exist for this
    /// agent.
    pub async fn agent_send_message(
        &self,
        agent: &AgentRef,
        to: &str,
        text: &str,
    ) -> Result<NoteOutcome> {
        if text.trim().is_empty() {
            return Err(Error::Invalid("the message must not be blank".to_owned()));
        }
        let to = match to.trim() {
            "" => {
                return Err(Error::Invalid(
                    "the recipient must be `parent`, `user` or the id of a child session"
                        .to_owned(),
                ));
            }
            "user" => NoteTarget::User,
            "parent" => NoteTarget::Parent,
            id => NoteTarget::Child(id.to_owned()),
        };
        let (agent_id, text) = (agent.agent_id.clone(), text.trim().to_owned());
        self.command(&agent.session_id, |reply| Command::Note {
            agent_id,
            to,
            text,
            reply,
        })
        .await
    }

    /// Keeps `summary` as what the agent's child session reports when it
    /// next reaches `review` or `failed`. `Conflict` when the session was
    /// not dispatched by another, `Invalid` for a blank summary.
    pub async fn agent_report(&self, agent: &AgentRef, summary: &str) -> Result<()> {
        if summary.trim().is_empty() {
            return Err(Error::Invalid("the summary must not be blank".to_owned()));
        }
        let summary = summary.trim().to_owned();
        self.command(&agent.session_id, |reply| Command::Report {
            summary,
            reply,
        })
        .await
    }

    /// Puts a question of the agent's to the user and returns its id. The
    /// session waits; the answer is the agent's next input. `Conflict` while
    /// another question of the session is open, `Invalid` for a blank one.
    pub async fn agent_ask_user(
        &self,
        agent: &AgentRef,
        question: &str,
        options: Vec<String>,
    ) -> Result<String> {
        if question.trim().is_empty() {
            return Err(Error::Invalid("the question must not be blank".to_owned()));
        }
        let (agent_id, question) = (agent.agent_id.clone(), question.trim().to_owned());
        let options = options
            .into_iter()
            .map(|option| option.trim().to_owned())
            .filter(|option| !option.is_empty())
            .collect();
        self.command(&agent.session_id, |reply| Command::Ask {
            agent_id,
            question,
            options,
            reply,
        })
        .await
    }

    /// Answers a question an agent put to the user. `Invalid` for a blank
    /// answer, `Missing` when the session never asked such a question,
    /// `Conflict` when it was already answered.
    pub async fn answer_question(
        &self,
        session_id: &str,
        question_id: &str,
        answer: &str,
    ) -> Result<()> {
        self.answer(session_id, question_id, answer, None).await
    }

    /// [`Self::answer_question`] with the note that goes with the decision
    /// at a workflow gate. A gate only takes `approve` or `reject`
    /// (`Invalid` otherwise); the comment of any other question is dropped.
    pub async fn answer(
        &self,
        session_id: &str,
        question_id: &str,
        answer: &str,
        comment: Option<String>,
    ) -> Result<()> {
        self.get(session_id).await?;
        if answer.trim().is_empty() {
            return Err(Error::Invalid("the answer must not be blank".to_owned()));
        }
        let (question_id, answer) = (question_id.to_owned(), answer.trim().to_owned());
        self.command(session_id, |reply| Command::Answer {
            question_id,
            answer,
            comment,
            reply,
        })
        .await
    }

    /// The graph of the whole tree the session belongs to, as it was at
    /// `at` (Unix milliseconds) or as it is now.
    pub async fn graph(&self, session_id: &str, at: Option<i64>) -> Result<Graph> {
        self.inner.store.graph(session_id, at).await
    }

    /// The timeline of the whole tree the session belongs to.
    pub async fn timeline(&self, session_id: &str) -> Result<Timeline> {
        self.inner.store.timeline(session_id).await
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

    /// What agents spent in `[from, to)` (Unix milliseconds; the last 30
    /// days by default, at most 366), in total and broken down `by` day in
    /// the server's time zone, root session, primary repo, runner or model
    /// (integrations spec, section 1). `Invalid` for a range that ends
    /// before it starts.
    pub async fn usage(&self, from: Option<i64>, to: Option<i64>, by: UsageBy) -> Result<Usage> {
        let now = self.inner.config.clock.now_ms();
        let (from, to) = resolve_range(from, to, now)?;
        let zone = self.inner.config.time_zone.clone();
        self.inner
            .store
            .usage(UsageQuery { from, to, by, zone })
            .await
    }

    /// The newest plan limits each runner reported since the server started.
    pub fn limits(&self) -> Vec<RunnerLimits> {
        self.inner.limits.newest()
    }

    /// Why pull requests cannot be opened from this server (`gh` is missing
    /// or not signed in), or `None` when they can. Always `None` in dry run.
    /// Asks `gh` at most once a minute.
    pub async fn github_problem(&self) -> Option<String> {
        self.inner.github_problem(false).await
    }

    /// Delivers a session that is in `review` or `shipping`: commits what is
    /// uncommitted in each `write` repo that differs from its base, pushes
    /// the session's branch and opens a pull request per repo, or updates
    /// the ones there are. Returns as soon as that is under way; every step
    /// shows as an event (the semantics in `calin-proto/src/api.rs`).
    ///
    /// `Conflict` in any other status, while another ship of the session is
    /// running, and with the reason when GitHub cannot be used. `Invalid`
    /// for a title of more than one line.
    pub async fn ship(&self, session_id: &str, request: ShipRequest) -> Result<()> {
        let status = self.get(session_id).await?.session.status;
        let title = request
            .title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty());
        // It becomes the subject of a commit and the title of a pull request.
        if title
            .as_ref()
            .is_some_and(|title| title.contains(['\n', '\r']))
        {
            return Err(Error::Invalid("the title must be a single line".to_owned()));
        }
        if !ships(status) {
            return Err(no_shipping(status));
        }
        if let Some(problem) = self.inner.github_problem(true).await {
            return Err(Error::Conflict(format!(
                "pull requests cannot be opened: {problem}"
            )));
        }
        let (body, draft) = (request.body, request.draft.unwrap_or(true));
        self.command(session_id, |reply| Command::Ship {
            title,
            body,
            draft,
            reply,
        })
        .await
    }

    /// Looks at the pull requests of a `shipping` session now instead of at
    /// the next tick, in the background. `Conflict` in any other status.
    pub async fn refresh_pull_requests(&self, session_id: &str) -> Result<()> {
        let status = self.get(session_id).await?.session.status;
        if status != SessionStatus::Shipping {
            return Err(Error::Conflict(format!(
                "pull requests are only watched while the session is shipping; it is {}",
                status_name(status)
            )));
        }
        let (manager, session_id) = (self.clone(), session_id.to_owned());
        tokio::spawn(async move { manager.look_at_pull_requests(&session_id).await });
        Ok(())
    }

    /// Looks at the session's pull requests on GitHub, records the ones
    /// that changed, and ends the session as `done` once all of them are
    /// merged or closed. Returns when that is on record. A session that is
    /// not `shipping` is left alone, as is every session in dry run.
    pub(crate) async fn poll_pull_requests(&self, session_id: &str) -> Result<()> {
        self.command(session_id, |reply| Command::Poll { reply })
            .await
    }

    /// [`Self::poll_pull_requests`] for callers with nobody to tell: a look
    /// that fails is tried again at the next tick.
    async fn look_at_pull_requests(&self, session_id: &str) {
        if let Err(err) = self.poll_pull_requests(session_id).await {
            tracing::warn!(session = %session_id, %err, "could not look at the session's pull requests");
        }
    }

    /// Ends every agent without recording anything, for a clean exit. The
    /// sessions that were running are failed by the recovery of the next start.
    pub async fn shutdown(&self) {
        let watcher = {
            let slot = self.inner.watcher.lock();
            slot.unwrap_or_else(PoisonError::into_inner).take()
        };
        if let Some(watcher) = watcher {
            watcher.abort();
        }
        self.inner.scheduler.stop();
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

    /// A session, its agents, its repo set and its pull requests.
    pub async fn get(&self, session_id: &str) -> Result<SessionDetail> {
        self.inner.store.session(session_id).await
    }

    /// Runs `save`, the user's write of a file of `repo` and its record,
    /// apart from the end of the agent's turn, then brings the snapshot of
    /// that file up to date: the agent is never credited with the user's
    /// save. `path` names the file once it is saved.
    pub(crate) async fn user_save<T>(
        &self,
        session_id: &str,
        repo: &SessionRepo,
        save: impl Future<Output = Result<T>>,
        path: impl FnOnce(&T) -> &str,
    ) -> Result<T> {
        let Some(mut tracker) = unreported::current(&self.inner.trackers, session_id).await else {
            return save.await;
        };
        let saved = save.await?;
        if let Err(err) = tracker.saved(repo, path(&saved)).await {
            tracing::warn!(session = %session_id, repo = %repo.name, %err, "could not take the user's save into the snapshot");
        }
        Ok(saved)
    }

    /// Hands a command to the session's driver, starting one if the session
    /// has none (after a restart, or once its agent exited).
    async fn command<T>(
        &self,
        session_id: &str,
        make: impl FnOnce(Reply<T>) -> Command,
    ) -> Result<T> {
        let (reply, answer) = oneshot::channel();
        self.post(session_id, make(reply));
        // A driver that ended before answering could not go on.
        answer.await.unwrap_or_else(|_| Err(no_longer_active()))
    }

    /// Queues a command on the session's driver without waiting for what
    /// comes of it. Commands posted to one session are handled in the order
    /// they were posted.
    fn post(&self, session_id: &str, command: Command) {
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
        let mut asked: HashMap<String, Vec<_>> = HashMap::new();
        for question in store.open_questions(None).await? {
            asked
                .entry(question.session_id.clone())
                .or_default()
                .push(question);
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
            // A question only outlives its agent with the session: one that
            // is not lost keeps its question for the user to answer.
            let questions = asked
                .remove(&session.id)
                .filter(|_| lost)
                .unwrap_or_default();
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
                for question in questions {
                    let mut closing = EventDraft::new(unanswered(question.question_id))
                        .parent(Some(question.event_id));
                    closing.agent_id = question.agent_id;
                    store.append(&session.id, closing).await?;
                }
                // A workflow run is not resumed: it ends where the restart
                // cut it, so that its picture agrees with the session's.
                let run = store.workflow_run(&session.id).await?;
                if let Some(run) = run.filter(|run| run.status == WorkflowStatus::Running) {
                    for node in run.nodes {
                        if node.state != NodeRunState::Active {
                            continue;
                        }
                        let cut = EventBody::WorkflowNode {
                            node_id: node.id,
                            phase: NodePhase::Finished,
                            port: None,
                            outcome: Some(format!("stopped: {RESTART_REASON}")),
                        };
                        store.append(&session.id, EventDraft::new(cut)).await?;
                    }
                    let finished = EventBody::WorkflowFinished {
                        status: WorkflowStatus::Failed,
                        reason: Some(RESTART_REASON.to_owned()),
                    };
                    store.append(&session.id, EventDraft::new(finished)).await?;
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
                let failed = store.append(&session.id, EventDraft::new(failed)).await?;
                report_restart(store, &session.id, &failed.id).await?;
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

/// A child session the restart failed tells the agent that dispatched it,
/// as its report would have (the "Child sessions" semantics in
/// `calin-proto/src/api.rs`). Nobody runs to hand it over now: the parent's
/// driver finds it in the log and hands it to its agent with the next input
/// (`Driver::unheard_reports`). Children are recovered before their parents
/// (newest first), so the report precedes the parent's own closing events.
async fn report_restart(store: &Store, session_id: &str, failed: &str) -> Result<()> {
    let Some(dispatch) = store.setup(session_id).await?.dispatch else {
        return Ok(());
    };
    let agents = store.session(session_id).await?.agents;
    let lead = agents
        .into_iter()
        .rev()
        .find(|agent| agent.role == driver::LEAD_ROLE);
    let from = lead.map_or_else(|| dispatch.lead_agent_id.clone(), |agent| agent.id);
    let message = AgentMessage {
        id: ulid::Ulid::generate().to_string(),
        kind: MessageKind::Report,
        from_session_id: session_id.to_owned(),
        from_agent_id: Some(from.clone()),
        to_session_id: dispatch.parent_session_id.clone(),
        to_agent_id: Some(dispatch.agent_id),
        body: briefing::child_failed_report(RESTART_REASON, None),
        in_reply_to: Some(dispatch.message_id),
        ts: now_ms(),
    };
    let sent = EventBody::MessageSent {
        message: message.clone(),
    };
    let sent = EventDraft::new(sent)
        .agent(from)
        .parent(Some(failed.to_owned()));
    let sent = store.append(session_id, sent).await?;
    let received = EventDraft::new(EventBody::MessageReceived { message });
    store
        .append(
            &dispatch.parent_session_id,
            received.parent(Some(sent.id.clone())),
        )
        .await?;
    Ok(())
}

/// Looks at the pull requests of every `shipping` session, at once and then
/// every `every`: also the sessions that were shipping when the server was
/// last stopped. Ends with the manager or its store.
async fn watch(inner: Weak<Inner>, every: Duration) {
    // An interval cannot be zero.
    let mut ticks = tokio::time::interval(every.max(Duration::from_millis(1)));
    // A slow round is followed by a full wait, not by a burst.
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let Some(inner) = inner.upgrade() else {
            return;
        };
        let manager = SessionManager { inner };
        let Ok(sessions) = manager.inner.store.sessions().await else {
            return;
        };
        for session in sessions {
            if session.status == SessionStatus::Shipping {
                let manager = manager.clone();
                // Each on its own: a slow `gh` for one session holds up no other.
                tokio::spawn(async move { manager.look_at_pull_requests(&session.id).await });
            }
        }
    }
}

/// True while the session can be delivered: its agent is done for now, and
/// nothing says the work is over (the semantics in `calin-proto/src/api.rs`).
fn ships(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Review | SessionStatus::Shipping)
}

fn no_shipping(status: SessionStatus) -> Error {
    Error::Conflict(format!(
        "a session can only be shipped while it is in review or shipping; it is {}",
        status_name(status)
    ))
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

/// The closing of a question the user never answered: the session ended first.
fn unanswered(question_id: String) -> EventBody {
    EventBody::QuestionAnswered {
        question_id,
        answer: String::new(),
        by: DecisionSource::Expired,
    }
}

/// Now, in Unix milliseconds: the time a message says it was sent at.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests;
