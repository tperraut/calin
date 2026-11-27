use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use calin_agents::{
    AgentHandle, AgentIo, Exit, MockConfig, RegistryConfig, RunnerError, RunnerInput, RunnerOutput,
    StartParams,
};
use calin_proto::{
    AgentKind, AttachRepoRequest, ChangeStatus, EntryKind, Event, FileOp, Item, ItemContent,
    ItemStatus, PermissionMode, PermissionRequest, RepoMode, SessionRepo, ToolKind,
    WriteFileRequest,
};
use tempfile::TempDir;

use super::*;
use crate::home::Home;
use crate::testing::{git, git_repo, write_file};

mod attach;
mod orchestration;
mod shipping;
mod unreported;

const TIMEOUT: Duration = Duration::from_secs(20);

/// A runner the test plays by hand: every start hands over the agent's end.
struct Puppet {
    starts: mpsc::UnboundedSender<(StartParams, AgentIo)>,
}

impl Runner for Puppet {
    fn id(&self) -> &str {
        "puppet"
    }

    fn start(&self, params: StartParams) -> std::result::Result<AgentHandle, RunnerError> {
        let (handle, io) = AgentHandle::channel();
        self.starts
            .send((params, io))
            .map_err(|_| RunnerError::Start("the test is over".to_owned()))?;
        Ok(handle)
    }
}

/// A runner whose agents never start.
struct Broken;

impl Runner for Broken {
    fn id(&self) -> &str {
        "broken"
    }

    fn start(&self, _params: StartParams) -> std::result::Result<AgentHandle, RunnerError> {
        Err(RunnerError::Start("no such binary".to_owned()))
    }
}

/// The agents the puppet runner was asked to start.
type Starts = mpsc::UnboundedReceiver<(StartParams, AgentIo)>;

struct Fixture {
    dir: TempDir,
    manager: SessionManager,
    starts: Starts,
}

fn registry(dry_run: bool) -> (RunnerRegistry, Starts) {
    let mut registry = RunnerRegistry::new(RegistryConfig {
        dry_run,
        mock: MockConfig {
            delay: Duration::ZERO,
        },
        claude: None,
    });
    let (starts, started) = mpsc::unbounded_channel();
    registry.register(Arc::new(Puppet { starts }));
    registry.register(Arc::new(Broken));
    (registry, started)
}

/// The defaults, except that the GitHub CLI is one that does not exist: no
/// test may run the one this machine has.
fn config() -> ManagerConfig {
    ManagerConfig {
        gh_bin: PathBuf::from("/nonexistent/calin/gh"),
        ..ManagerConfig::default()
    }
}

async fn manager_in(dir: &TempDir, dry_run: bool) -> (SessionManager, Starts) {
    manager_with(dir, dry_run, config()).await
}

async fn manager_with(
    dir: &TempDir,
    dry_run: bool,
    config: ManagerConfig,
) -> (SessionManager, Starts) {
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let (registry, starts) = registry(dry_run);
    let manager = SessionManager::start_with(store, registry, config)
        .await
        .unwrap();
    (manager, starts)
}

async fn fixture() -> Fixture {
    let dir = TempDir::new().unwrap();
    let (manager, starts) = manager_in(&dir, false).await;
    Fixture {
        dir,
        manager,
        starts,
    }
}

/// A session that never waits for the user: the mock asks nothing in
/// `autonomous` mode, and the puppet only asks when a test makes it.
fn request(prompt: &str, runner: Option<&str>) -> CreateSessionRequest {
    request_in(PermissionMode::Autonomous, prompt, runner)
}

fn request_in(mode: PermissionMode, prompt: &str, runner: Option<&str>) -> CreateSessionRequest {
    CreateSessionRequest {
        prompt: prompt.to_owned(),
        title: None,
        runner: runner.map(str::to_owned),
        model: None,
        permission_mode: Some(mode),
        repos: Vec::new(),
    }
}

fn attach(repo: &Repo, mode: Option<RepoMode>) -> AttachRepoRequest {
    AttachRepoRequest {
        repo_id: repo.id.clone(),
        mode,
    }
}

/// A clone `parent/name` with one commit, registered under its folder name.
async fn registered(manager: &SessionManager, parent: &Path, name: &str) -> Repo {
    let path = git_repo(parent, name);
    manager
        .register_repo(RegisterRepoRequest {
            path: path.to_str().unwrap().to_owned(),
            name: None,
        })
        .await
        .unwrap()
}

async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TIMEOUT, future)
        .await
        .expect("timed out")
}

/// The session's events after `after_seq`, up to the first one matching `last`.
async fn events_until(
    manager: &SessionManager,
    session_id: &str,
    after_seq: u64,
    last: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let mut subscription = manager
        .store()
        .subscribe(session_id, after_seq)
        .await
        .unwrap();
    let mut events = Vec::new();
    for event in subscription.backlog {
        let done = last(&event);
        events.push(event);
        if done {
            return events;
        }
    }
    loop {
        let event = within(subscription.live.recv()).await.unwrap();
        let done = last(&event);
        events.push((*event).clone());
        if done {
            return events;
        }
    }
}

fn is_status(status: SessionStatus) -> impl Fn(&Event) -> bool {
    move |event| matches!(&event.body, EventBody::SessionStatus { status: s, .. } if *s == status)
}

fn label(event: &Event) -> String {
    match &event.body {
        EventBody::SessionStatus { status, .. } => {
            format!("session_status:{}", format!("{status:?}").to_lowercase())
        }
        EventBody::AgentStatus { status } => {
            format!("agent_status:{}", format!("{status:?}").to_lowercase())
        }
        EventBody::UserMessage { text } => format!("user_message:{text}"),
        EventBody::AgentSpawned { .. } => "agent_spawned".to_owned(),
        EventBody::ItemStarted { .. } => "item_started".to_owned(),
        EventBody::ItemDelta { .. } => "item_delta".to_owned(),
        EventBody::ItemCompleted { .. } => "item_completed".to_owned(),
        EventBody::PlanUpdated { .. } => "plan_updated".to_owned(),
        EventBody::Usage { .. } => "usage".to_owned(),
        EventBody::Error { message } => format!("error:{message}"),
        EventBody::AgentSession { .. } => "agent_session".to_owned(),
        EventBody::RepoAttached { repo } => format!("repo_attached:{}", repo.name),
        EventBody::PermissionRequested { request } => {
            format!("permission_requested:{}", request.tool_name)
        }
        EventBody::PermissionResolved { decision, by, .. } => format!(
            "permission_resolved:{}:{}",
            format!("{decision:?}").to_lowercase(),
            format!("{by:?}").to_lowercase()
        ),
        EventBody::FileChanged { op, .. } => {
            format!("file_changed:{}", format!("{op:?}").to_lowercase())
        }
        EventBody::PullRequestOpened { pull_request } => {
            format!("pull_request_opened:{}", pull_request.url)
        }
        EventBody::PullRequestUpdated { pull_request } => {
            format!("pull_request_updated:{}", pull_request.url)
        }
        EventBody::MessageSent { message } => {
            format!(
                "message_sent:{}",
                format!("{:?}", message.kind).to_lowercase()
            )
        }
        EventBody::MessageReceived { message } => {
            format!(
                "message_received:{}",
                format!("{:?}", message.kind).to_lowercase()
            )
        }
        EventBody::QuestionAsked { question } => format!("question_asked:{}", question.text),
        EventBody::QuestionAnswered { answer, by, .. } => format!(
            "question_answered:{answer}:{}",
            format!("{by:?}").to_lowercase()
        ),
    }
}

fn labels(events: &[Event]) -> Vec<String> {
    events.iter().map(label).collect()
}

fn item(id: &str) -> Item {
    Item {
        id: id.to_owned(),
        status: ItemStatus::Running,
        content: ItemContent::Text {
            text: String::new(),
        },
    }
}

fn tool(id: &str, tool_kind: ToolKind, status: ItemStatus, locations: &[&str]) -> Item {
    Item {
        id: id.to_owned(),
        status,
        content: ItemContent::Tool {
            name: "Tool".to_owned(),
            tool_kind,
            title: id.to_owned(),
            input: serde_json::json!({}),
            output: None,
            locations: locations.iter().map(|path| (*path).to_owned()).collect(),
        },
    }
}

fn ask(id: &str, item_id: Option<&str>) -> RunnerOutput {
    let request = PermissionRequest {
        id: id.to_owned(),
        tool_name: "Bash".to_owned(),
        tool_kind: ToolKind::Execute,
        title: "cargo test".to_owned(),
        input: serde_json::json!({ "command": "cargo test" }),
        item_id: item_id.map(str::to_owned),
    };
    RunnerOutput::PermissionRequest(request.into())
}

async fn emit(io: &AgentIo, output: RunnerOutput) {
    io.outputs.send(output).await.unwrap();
}

/// The open request the session is waiting on, with the event that recorded it.
async fn waiting_request(
    manager: &SessionManager,
    session_id: &str,
    after_seq: u64,
) -> (Vec<Event>, Event, PermissionRequest) {
    let events = events_until(
        manager,
        session_id,
        after_seq,
        is_status(SessionStatus::Waiting),
    )
    .await;
    assert_eq!(
        events.last().unwrap().body,
        EventBody::SessionStatus {
            status: SessionStatus::Waiting,
            reason: Some("permission".to_owned()),
        }
    );
    let asked = events[events.len() - 2].clone();
    let EventBody::PermissionRequested { request } = asked.body.clone() else {
        panic!("waiting must follow a request, not {}", label(&asked));
    };
    (events, asked, request)
}

fn lead_id(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::AgentSpawned { agent } => Some(agent.id.clone()),
            _ => None,
        })
        .expect("no agent was spawned")
}

fn expired_body(request_id: &str) -> EventBody {
    EventBody::PermissionResolved {
        request_id: request_id.to_owned(),
        decision: PermissionDecision::Deny,
        by: DecisionSource::Expired,
        message: Some("the turn ended before this request was answered".to_owned()),
    }
}

fn started_item_ids(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemStarted { item } => Some(item.id.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_new_session_runs_one_turn_and_waits_for_review() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let detail = manager
        .create_session(request("Fix the login bug\nIt fails on Fridays.", None))
        .await
        .unwrap();
    // Creation answers as soon as the prompt is on record: the agent comes later.
    let session = &detail.session;
    assert_eq!(session.status, SessionStatus::Queued);
    assert_eq!(session.head_seq, 2);
    assert_eq!(session.title, "Fix the login bug");
    assert_eq!(session.prompt, "Fix the login bug\nIt fails on Fridays.");
    assert_eq!(session.runner, "mock");
    assert_eq!(session.permission_mode, PermissionMode::Autonomous);
    assert_eq!(session.branch, None);
    assert_eq!(session.root_session_id, session.id);
    assert!(detail.agents.is_empty());
    assert!(detail.repos.is_empty());

    let events = events_until(&manager, &session.id, 0, is_status(SessionStatus::Review)).await;
    let detail = manager.get(&session.id).await.unwrap();
    // The lead, and the one subagent the mock's turn starts.
    assert_eq!(detail.agents.len(), 2);
    let lead = &detail.agents[0];
    assert_eq!(lead.role, "lead");
    assert_eq!(lead.kind, AgentKind::Process);
    assert_eq!(lead.runner, "mock");
    assert_eq!(lead.model.as_deref(), Some("mock-1"));
    assert_eq!(lead.session_id, session.id);
    let subagent = &detail.agents[1];
    assert_eq!(subagent.kind, AgentKind::Subagent);
    assert_eq!(subagent.parent_agent_id.as_ref(), Some(&lead.id));
    assert_eq!(subagent.status, AgentStatus::Done);
    let labels = labels(&events);
    assert_eq!(
        labels[..5],
        [
            "session_status:queued",
            "user_message:Fix the login bug\nIt fails on Fridays.",
            "session_status:running",
            "agent_spawned",
            "agent_session",
        ]
    );
    assert_eq!(
        labels[labels.len() - 2..],
        ["agent_status:idle", "session_status:review"]
    );
    // Without a repo there is nothing to prepare.
    assert!(!labels.contains(&"session_status:preparing".to_owned()));
    let turn = &events[4..events.len() - 2];
    assert!(turn.len() > 10);
    let mut by_subagent = 0;
    for event in turn {
        match event.body {
            EventBody::AgentSession { .. }
            | EventBody::ItemStarted { .. }
            | EventBody::ItemDelta { .. }
            | EventBody::ItemCompleted { .. }
            | EventBody::PlanUpdated { .. }
            | EventBody::Usage { .. } => {
                // The prompt caused the turn.
                assert_eq!(event.parent_id.as_ref(), Some(&events[1].id));
                if event.agent_id.as_ref() == Some(&subagent.id) {
                    by_subagent += 1;
                } else {
                    assert_eq!(event.agent_id.as_ref(), Some(&lead.id));
                }
            }
            // Caused by the tool call that made the change, checked elsewhere.
            EventBody::FileChanged { .. } => assert_eq!(event.agent_id.as_ref(), Some(&lead.id)),
            // The subagent, from start to end; checked in `orchestration`.
            EventBody::AgentSpawned { .. } => assert_eq!(event.agent_id, None),
            EventBody::AgentStatus { .. } => {
                assert_eq!(event.agent_id.as_ref(), Some(&subagent.id));
            }
            _ => panic!("unexpected {} inside the turn", label(event)),
        }
    }
    assert!(by_subagent > 3, "the subagent's own items are its events");
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.seq, index as u64 + 1);
    }
    // The user and the orchestrator are not agents.
    for index in [0, 1, 2, 3, events.len() - 1] {
        assert_eq!(events[index].agent_id, None, "{}", labels[index]);
    }
    assert_eq!(
        events[events.len() - 2].agent_id.as_deref(),
        Some(lead.id.as_str())
    );

    let detail = manager.get(&session.id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Review);
    assert_eq!(detail.session.head_seq, events.len() as u64);
    assert_eq!(detail.agents[0].status, AgentStatus::Idle);
    assert_eq!(detail.agents[0].tokens_in, 1200);
    assert_eq!(detail.agents[0].tokens_out, 350);
    assert_eq!(manager.list().await.unwrap(), [detail.session]);
}

#[tokio::test]
async fn a_message_in_review_runs_another_turn() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request("first", None))
        .await
        .unwrap()
        .session
        .id;
    let first = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;

    manager.send_message(&id, "and now the docs").await.unwrap();
    let after = first.len() as u64;
    let second = events_until(&manager, &id, after, is_status(SessionStatus::Review)).await;
    let labels = labels(&second);
    assert_eq!(
        labels[..3],
        [
            "user_message:and now the docs",
            "session_status:running",
            "agent_status:running",
        ]
    );
    assert_eq!(
        labels[labels.len() - 2..],
        ["agent_status:idle", "session_status:review"]
    );
    // No second lead: the one spawn of the turn is the mock's subagent.
    let spawned: Vec<&Event> = second
        .iter()
        .filter(|event| matches!(event.body, EventBody::AgentSpawned { .. }))
        .collect();
    assert_eq!(spawned.len(), 1);
    assert!(matches!(
        &spawned[0].body,
        EventBody::AgentSpawned { agent } if agent.kind == AgentKind::Subagent
    ));
    assert_eq!(second[0].seq, after + 1);

    let ids: Vec<String> = [started_item_ids(&first), started_item_ids(&second)].concat();
    assert_eq!(ids.len(), 16);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 16);

    let detail = manager.get(&id).await.unwrap();
    // The lead and the subagent of each turn; the totals stay on the lead.
    assert_eq!(detail.agents.len(), 3);
    assert_eq!(detail.agents[0].tokens_in, 2400);
    assert_eq!(detail.agents[1].tokens_in + detail.agents[2].tokens_in, 0);
    assert_eq!(detail.session.status, SessionStatus::Review);
}

#[tokio::test]
async fn a_message_sent_while_running_waits_for_the_current_turn() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("first", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (params, mut io) = within(starts.recv()).await.unwrap();
    assert_eq!(params.prompt, "first");
    assert_eq!(params.session_id, id);
    assert_eq!(params.resume, None);
    assert_eq!(params.start_seq, 3);

    emit(
        &io,
        RunnerOutput::Event(EventBody::ItemStarted { item: item("i1") }),
    )
    .await;
    // The driver takes commands and output in either order: wait for the item.
    events_until(&manager, &id, 0, |event| {
        matches!(event.body, EventBody::ItemStarted { .. })
    })
    .await;
    manager.send_message(&id, "second").await.unwrap();
    // The agent is busy: it has not been told yet.
    assert!(io.inputs.try_recv().is_err());
    assert_eq!(
        manager.get(&id).await.unwrap().session.status,
        SessionStatus::Running
    );

    emit(&io, RunnerOutput::TurnEnded).await;
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::UserMessage("second".to_owned()))
    );
    emit(
        &io,
        RunnerOutput::Event(EventBody::ItemStarted { item: item("i2") }),
    )
    .await;
    emit(&io, RunnerOutput::TurnEnded).await;

    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:first",
            "session_status:running",
            "agent_spawned",
            "item_started",
            "user_message:second",
            // One turn ends and the next begins: the timeline reads both.
            "agent_status:idle",
            "agent_status:running",
            "item_started",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    // Each turn's events point at the message that caused it.
    assert_eq!(events[4].parent_id.as_ref(), Some(&events[1].id));
    assert_eq!(events[6].parent_id.as_ref(), Some(&events[1].id));
    assert_eq!(events[7].parent_id.as_ref(), Some(&events[5].id));
    assert_eq!(events[8].parent_id.as_ref(), Some(&events[5].id));
}

#[tokio::test]
async fn stop_while_running_ends_the_agent_and_cancels_the_session() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("work", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, mut io) = within(starts.recv()).await.unwrap();
    emit(
        &io,
        RunnerOutput::Event(EventBody::ItemStarted { item: item("i1") }),
    )
    .await;

    let stopping = tokio::spawn({
        let (manager, id) = (manager.clone(), id.clone());
        async move { manager.stop(&id).await }
    });
    assert_eq!(within(io.inputs.recv()).await, Some(RunnerInput::Stop));
    emit(&io, RunnerOutput::Exited(Exit::Ok)).await;
    within(stopping).await.unwrap().unwrap();

    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Cancelled)).await;
    assert_eq!(
        labels(&events)[4..],
        [
            "item_started",
            "agent_status:stopped",
            "session_status:cancelled"
        ]
    );
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Cancelled);
    assert_eq!(detail.agents[0].status, AgentStatus::Stopped);

    // Stopping twice is refused; a message would continue the session.
    assert!(matches!(manager.stop(&id).await, Err(Error::Conflict(_))));
    assert!(matches!(
        manager.interrupt(&id).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(manager.get(&id).await.unwrap().session.head_seq, 7);
}

#[tokio::test]
async fn stop_in_review_cancels_the_session() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request("work", None))
        .await
        .unwrap()
        .session
        .id;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    manager.stop(&id).await.unwrap();
    let after = events.len() as u64;
    let rest = events_until(&manager, &id, after, is_status(SessionStatus::Cancelled)).await;
    assert_eq!(
        labels(&rest),
        ["agent_status:stopped", "session_status:cancelled"]
    );
}

#[tokio::test]
async fn stop_mid_turn_of_a_slow_agent_is_prompt() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    // A minute per chunk: the turn cannot end by itself during the test.
    let registry = RunnerRegistry::new(RegistryConfig {
        dry_run: false,
        mock: MockConfig {
            delay: Duration::from_secs(60),
        },
        claude: None,
    });
    let manager = SessionManager::start(store, registry).await.unwrap();
    let id = manager
        .create_session(request("slow", None))
        .await
        .unwrap()
        .session
        .id;
    within(manager.stop(&id)).await.unwrap();
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Cancelled)).await;
    assert_eq!(
        labels(&events)[3..],
        [
            "agent_spawned",
            "agent_session",
            "agent_status:stopped",
            "session_status:cancelled"
        ]
    );
}

#[tokio::test]
async fn interrupt_mid_turn_of_a_slow_agent_is_prompt_and_leaves_the_session_in_review() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let registry = RunnerRegistry::new(RegistryConfig {
        dry_run: false,
        mock: MockConfig {
            delay: Duration::from_secs(60),
        },
        claude: None,
    });
    let manager = SessionManager::start(store, registry).await.unwrap();
    let id = manager
        .create_session(request("slow", None))
        .await
        .unwrap()
        .session
        .id;
    events_until(&manager, &id, 0, is_status(SessionStatus::Running)).await;
    within(manager.interrupt(&id)).await.unwrap();
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events)[3..],
        [
            "agent_spawned",
            "agent_session",
            "agent_status:idle",
            "session_status:review"
        ]
    );
    // The agent is still there for the next message.
    manager.stop(&id).await.unwrap();
}

#[tokio::test]
async fn a_runner_that_exits_with_an_error_fails_the_session() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("work", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, io) = within(starts.recv()).await.unwrap();
    emit(&io, RunnerOutput::Exited(Exit::Error("boom".to_owned()))).await;

    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Failed)).await;
    assert_eq!(
        labels(&events)[4..],
        ["error:boom", "agent_status:failed", "session_status:failed"]
    );
    assert_eq!(
        events.last().unwrap().body,
        EventBody::SessionStatus {
            status: SessionStatus::Failed,
            reason: Some("boom".to_owned()),
        }
    );
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.agents[0].status, AgentStatus::Failed);
    // There is nothing left to stop; a message would continue the session.
    assert!(matches!(manager.stop(&id).await, Err(Error::Conflict(_))));
    assert!(matches!(
        manager.interrupt(&id).await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn an_agent_that_vanishes_fails_the_session() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("work", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, io) = within(starts.recv()).await.unwrap();
    drop(io);
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Failed)).await;
    assert_eq!(
        labels(&events)[4..],
        [
            "error:the agent ended without a result",
            "agent_status:failed",
            "session_status:failed"
        ]
    );
}

#[tokio::test]
async fn a_runner_that_cannot_start_fails_the_session() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let detail = manager
        .create_session(request("work", Some("broken")))
        .await
        .unwrap();
    let id = &detail.session.id;
    let events = events_until(&manager, id, 0, is_status(SessionStatus::Failed)).await;
    let detail = manager.get(id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Failed);
    assert!(detail.agents.is_empty());
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:work",
            "session_status:running",
            "error:the agent could not be started: no such binary",
            "session_status:failed",
        ]
    );
}

#[tokio::test]
async fn an_agent_that_exits_cleanly_leaves_the_session_in_review_and_can_be_resumed() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("one shot", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (first, io) = within(starts.recv()).await.unwrap();
    assert_eq!(first.resume, None);
    let announced = RunnerOutput::NativeSession {
        id: "conversation-1".to_owned(),
        model: None,
    };
    emit(&io, announced.clone()).await;
    // Said again without news, it is not recorded again.
    emit(&io, announced).await;
    emit(&io, RunnerOutput::Exited(Exit::Ok)).await;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events)[4..],
        [
            "agent_session",
            "agent_status:done",
            "session_status:review"
        ]
    );

    manager.send_message(&id, "more").await.unwrap();
    let (params, io) = within(starts.recv()).await.unwrap();
    assert_eq!(params.resume.as_deref(), Some("conversation-1"));
    assert_eq!(params.agent_id, first.agent_id);
    assert!(params.start_seq > first.start_seq);
    assert_eq!(params.prompt, "more");
    assert_eq!(
        params.agent_id,
        manager.get(&id).await.unwrap().agents[0].id
    );
    emit(&io, RunnerOutput::TurnEnded).await;
    let after = events.len() as u64;
    let rest = events_until(&manager, &id, after, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&rest),
        [
            "user_message:more",
            "session_status:running",
            "agent_status:running",
            "agent_status:idle",
            "session_status:review",
        ]
    );
}

#[tokio::test]
async fn invalid_input_and_unknown_sessions_are_rejected() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    for prompt in ["", "  \n\t "] {
        assert!(matches!(
            manager.create_session(request(prompt, None)).await,
            Err(Error::Invalid(_))
        ));
    }
    assert!(matches!(
        manager.create_session(request("x", Some("claude"))).await,
        Err(Error::Invalid(_))
    ));
    // An unknown repo, or the same repo twice, before anything is created.
    let repos = TempDir::new().unwrap();
    let repo = registered(&manager, repos.path(), "api").await;
    let unknown_repo = AttachRepoRequest {
        repo_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        mode: None,
    };
    for wanted in [
        vec![unknown_repo.clone()],
        vec![attach(&repo, None), unknown_repo],
        vec![attach(&repo, None), attach(&repo, Some(RepoMode::Read))],
    ] {
        let mut invalid = request("x", None);
        invalid.repos = wanted;
        assert!(matches!(
            manager.create_session(invalid).await,
            Err(Error::Invalid(_))
        ));
    }
    assert!(manager.list().await.unwrap().is_empty());
    assert!(!repos.path().join("api/.git/worktrees").exists());
    // None of the refused sessions holds the repo.
    manager.remove_repo(&repo.id).await.unwrap();

    let id = manager
        .create_session(request("x", None))
        .await
        .unwrap()
        .session
        .id;
    assert!(matches!(
        manager.send_message(&id, "  ").await,
        Err(Error::Invalid(_))
    ));

    let unknown = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    assert!(matches!(
        manager.get(unknown).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        manager.send_message(unknown, "hi").await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        manager.stop(unknown).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        manager.interrupt(unknown).await,
        Err(Error::NotFound(_))
    ));
    let answer = manager.resolve_permission(unknown, "p1", PermissionDecision::Allow, None);
    assert!(matches!(answer.await, Err(Error::NotFound(_))));
    assert!(matches!(
        manager.changes(unknown).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        manager.terminal_dir(unknown, None).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn titles_come_from_the_request_or_the_prompt() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let long = "word ".repeat(40);
    let detail = manager.create_session(request(&long, None)).await.unwrap();
    assert_eq!(detail.session.title.chars().count(), 80);
    assert!(long.starts_with(&detail.session.title));

    let mut titled = request("the prompt", None);
    titled.title = Some("  My title ".to_owned());
    let detail = manager.create_session(titled).await.unwrap();
    assert_eq!(detail.session.title, "My title");

    let mut blank = request("the prompt", Some(""));
    blank.title = Some("   ".to_owned());
    let detail = manager.create_session(blank).await.unwrap();
    assert_eq!(detail.session.title, "the prompt");
    assert_eq!(detail.session.runner, "mock");
}

#[tokio::test]
async fn dry_run_puts_every_session_on_the_mock_runner() {
    let dir = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, true).await;
    for runner in [None, Some("puppet"), Some("claude")] {
        let detail = manager.create_session(request("x", runner)).await.unwrap();
        assert_eq!(detail.session.runner, "mock");
        let id = &detail.session.id;
        events_until(&manager, id, 0, is_status(SessionStatus::Review)).await;
        assert_eq!(manager.get(id).await.unwrap().agents[0].runner, "mock");
    }
}

#[tokio::test]
async fn messages_and_stop_conflict_once_a_session_is_done() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request("x", None))
        .await
        .unwrap()
        .session
        .id;
    events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let done = EventBody::SessionStatus {
        status: SessionStatus::Done,
        reason: None,
    };
    manager
        .store()
        .append(&id, EventDraft::new(done))
        .await
        .unwrap();
    let head = manager.get(&id).await.unwrap().session.head_seq;

    assert!(matches!(
        manager.send_message(&id, "one more thing").await,
        Err(Error::Conflict(message)) if message == "the session is done"
    ));
    assert!(matches!(manager.stop(&id).await, Err(Error::Conflict(_))));
    assert!(matches!(
        manager.interrupt(&id).await,
        Err(Error::Conflict(_))
    ));
    assert_eq!(manager.get(&id).await.unwrap().session.head_seq, head);
}

#[tokio::test]
async fn a_restart_fails_interrupted_sessions_and_keeps_settled_ones() {
    let dir = TempDir::new().unwrap();
    let (manager, mut starts) = manager_in(&dir, false).await;
    let settled = manager
        .create_session(request("settled", None))
        .await
        .unwrap()
        .session
        .id;
    let settled_events =
        events_until(&manager, &settled, 0, is_status(SessionStatus::Review)).await;
    let interrupted = manager
        .create_session(request("interrupted", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, io) = within(starts.recv()).await.unwrap();
    emit(
        &io,
        RunnerOutput::Event(EventBody::ItemStarted { item: item("i1") }),
    )
    .await;
    events_until(&manager, &interrupted, 0, |event| {
        matches!(event.body, EventBody::ItemStarted { .. })
    })
    .await;
    // The process dies here.
    manager.store().close().await;
    drop((manager, io, starts));

    let (manager, mut starts) = manager_in(&dir, false).await;
    let detail = manager.get(&interrupted).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Failed);
    assert_eq!(detail.agents[0].status, AgentStatus::Failed);
    let events = events_until(&manager, &interrupted, 0, is_status(SessionStatus::Failed)).await;
    assert_eq!(
        labels(&events)[4..],
        [
            "item_started",
            "agent_status:failed",
            "session_status:failed"
        ]
    );
    assert_eq!(
        events.last().unwrap().body,
        EventBody::SessionStatus {
            status: SessionStatus::Failed,
            reason: Some("server restarted".to_owned()),
        }
    );

    // The settled session is as it was and can go on with the same agent.
    let detail = manager.get(&settled).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Review);
    assert_eq!(detail.session.head_seq, settled_events.len() as u64);
    manager.send_message(&settled, "continue").await.unwrap();
    let after = settled_events.len() as u64;
    let resumed = events_until(&manager, &settled, after, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&resumed)[..3],
        [
            "user_message:continue",
            "session_status:running",
            "agent_status:running",
        ]
    );
    let ids: Vec<String> = [
        started_item_ids(&settled_events),
        started_item_ids(&resumed),
    ]
    .concat();
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 16);
    // One lead, and the subagent of each of its two turns.
    let agents = manager.get(&settled).await.unwrap().agents;
    let leads = agents.iter().filter(|a| a.kind == AgentKind::Process);
    assert_eq!((agents.len(), leads.count()), (3, 1));
    assert!(starts.try_recv().is_err());
}

#[tokio::test]
async fn shutdown_ends_the_agents_and_records_nothing() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let idle = manager
        .create_session(request("idle", None))
        .await
        .unwrap()
        .session
        .id;
    events_until(&manager, &idle, 0, is_status(SessionStatus::Review)).await;
    let busy = manager
        .create_session(request("busy", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, mut io) = within(starts.recv()).await.unwrap();
    let before = manager.list().await.unwrap();

    within(manager.shutdown()).await;
    // The agent's handle is gone, which is how a runner learns it must end.
    assert_eq!(within(io.inputs.recv()).await, None);
    assert_eq!(manager.list().await.unwrap(), before);
    assert_eq!(
        manager.get(&busy).await.unwrap().session.status,
        SessionStatus::Running
    );
}

// --- Repos, worktrees and the agent's directories ---

#[tokio::test]
async fn a_session_without_repos_works_in_its_own_folder() {
    let Fixture {
        manager,
        mut starts,
        dir,
    } = fixture().await;
    let mut wanted = request_in(PermissionMode::Ask, "no repo", Some("puppet"));
    wanted.model = Some("  ".to_owned());
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (params, io) = within(starts.recv()).await.unwrap();
    let work = Home::new(dir.path()).work_dir(&id);
    assert_eq!(params.cwd, work);
    assert_eq!(work, dir.path().join("sessions").join(&id).join("work"));
    assert!(work.is_dir());
    assert!(params.add_dirs.is_empty());
    assert_eq!(params.permission_mode, PermissionMode::Ask);
    // A blank model is no model: the runner chooses.
    assert_eq!(params.model, None);
    assert_eq!(params.resume, None);

    emit(&io, RunnerOutput::TurnEnded).await;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:no repo",
            "session_status:running",
            "agent_spawned",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    let detail = manager.get(&id).await.unwrap();
    assert!(detail.repos.is_empty());
    assert_eq!(detail.session.branch, None);
    assert_eq!(detail.agents[0].model, None);
    assert_eq!(manager.terminal_dir(&id, None).await.unwrap(), work);
    assert!(manager.changes(&id).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_session_with_two_repos_gets_a_worktree_a_branch_and_its_directories() {
    let Fixture {
        manager,
        mut starts,
        dir,
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let web = registered(&manager, clones.path(), "web").await;
    let api_head = git(Path::new(&api.path), &["rev-parse", "HEAD"]);
    let web_head = git(Path::new(&web.path), &["rev-parse", "HEAD"]);

    let mut wanted = request_in(
        PermissionMode::AcceptEdits,
        "Fix the login bug",
        Some("puppet"),
    );
    wanted.model = Some("opus".to_owned());
    wanted.repos = vec![attach(&api, None), attach(&web, Some(RepoMode::Live))];
    let created = manager.create_session(wanted).await.unwrap();
    // The answer does not wait for git.
    assert_eq!(created.session.status, SessionStatus::Queued);
    assert!(created.repos.is_empty());
    let id = created.session.id;

    let (params, io) = within(starts.recv()).await.unwrap();
    let events = events_until(&manager, &id, 0, |event| {
        matches!(event.body, EventBody::AgentSpawned { .. })
    })
    .await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "user_message:Fix the login bug",
            "session_status:preparing",
            "repo_attached:api",
            "repo_attached:web",
            "session_status:running",
            "agent_spawned",
        ]
    );
    // The prompt is the cause of everything that made the session ready.
    for event in &events[2..] {
        assert_eq!(event.parent_id.as_ref(), Some(&events[1].id));
        assert_eq!(event.agent_id, None);
    }

    let branch = calin_git::session_branch_name("Fix the login bug", &id);
    let worktree = std::fs::canonicalize(Home::new(dir.path()).work_dir(&id).join("api")).unwrap();
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.session.branch.as_deref(), Some(branch.as_str()));
    let [first, second] = detail.repos.as_slice() else {
        panic!("expected two repos, got {:?}", detail.repos);
    };
    assert_eq!(
        *first,
        SessionRepo {
            id: first.id.clone(),
            session_id: id.clone(),
            repo_id: api.id.clone(),
            name: "api".to_owned(),
            mode: RepoMode::Write,
            path: worktree.to_str().unwrap().to_owned(),
            branch: Some(branch.clone()),
            base_ref: Some(api_head.clone()),
            primary: true,
        }
    );
    assert_eq!(
        *second,
        SessionRepo {
            id: second.id.clone(),
            session_id: id.clone(),
            repo_id: web.id.clone(),
            name: "web".to_owned(),
            mode: RepoMode::Live,
            path: web.path.clone(),
            branch: None,
            base_ref: Some(web_head),
            primary: false,
        }
    );
    assert_ne!(first.id, second.id);
    // The events carry exactly what the index answers with.
    let attached: Vec<&SessionRepo> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::RepoAttached { repo } => Some(repo),
            _ => None,
        })
        .collect();
    assert_eq!(attached, [first, second]);

    // A real worktree on the session's branch; the clones are where they were.
    assert_eq!(
        git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
        branch
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), api_head);
    assert!(worktree.join("README.md").is_file());
    for clone in [&api.path, &web.path] {
        let on = git(Path::new(clone), &["rev-parse", "--abbrev-ref", "HEAD"]);
        assert_eq!(on, "main");
    }
    assert!(!Path::new(&web.path).join(".git/worktrees").exists());

    // The agent starts in the primary repo and is granted the other one.
    assert_eq!(params.cwd, worktree);
    assert_eq!(params.add_dirs, [PathBuf::from(&web.path)]);
    assert_eq!(params.permission_mode, PermissionMode::AcceptEdits);
    assert_eq!(params.model.as_deref(), Some("opus"));
    assert_eq!(params.resume, None);
    assert_eq!(params.start_seq, 6);
    assert_eq!(detail.agents[0].model.as_deref(), Some("opus"));

    // A terminal starts in the repo it names, else where the agent works.
    assert_eq!(manager.terminal_dir(&id, None).await.unwrap(), worktree);
    assert_eq!(
        manager.terminal_dir(&id, Some(&second.id)).await.unwrap(),
        PathBuf::from(&web.path)
    );
    assert!(matches!(
        manager.terminal_dir(&id, Some("nope")).await,
        Err(Error::Missing(_))
    ));
    drop(io);
}

#[tokio::test]
async fn a_repo_that_cannot_be_prepared_fails_the_session_and_a_message_tries_again() {
    let Fixture {
        manager,
        mut starts,
        dir,
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let web = registered(&manager, clones.path(), "web").await;
    // The second clone vanishes between registration and use.
    let away = clones.path().join("web.away");
    std::fs::rename(&web.path, &away).unwrap();

    let mut wanted = request("the prompt", Some("puppet"));
    wanted.repos = vec![attach(&api, None), attach(&web, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Failed)).await;
    let labels = labels(&events);
    assert_eq!(
        labels[..4],
        [
            "session_status:queued",
            "user_message:the prompt",
            "session_status:preparing",
            "repo_attached:api",
        ]
    );
    assert!(
        labels[4].starts_with("error:could not prepare repo web: "),
        "{}",
        labels[4]
    );
    assert_eq!(labels[5], "session_status:failed");
    assert_eq!(labels.len(), 6);
    let EventBody::Error { message } = &events[4].body else {
        unreachable!()
    };
    assert_eq!(
        events[5].body,
        EventBody::SessionStatus {
            status: SessionStatus::Failed,
            reason: Some(message.clone()),
        }
    );
    let detail = manager.get(&id).await.unwrap();
    assert!(detail.agents.is_empty());
    assert_eq!(detail.repos.len(), 1);
    assert!(starts.try_recv().is_err());
    // A failed session no longer holds its repos.
    assert!(matches!(manager.stop(&id).await, Err(Error::Conflict(_))));

    // With the clone back, a message picks the preparation up where it broke.
    std::fs::rename(&away, &web.path).unwrap();
    manager.send_message(&id, "try again").await.unwrap();
    let (params, _io) = within(starts.recv()).await.unwrap();
    let rest = events_until(&manager, &id, 6, |event| {
        matches!(event.body, EventBody::AgentSpawned { .. })
    })
    .await;
    assert_eq!(
        self::labels(&rest),
        [
            "user_message:try again",
            "session_status:preparing",
            "repo_attached:web",
            "session_status:running",
            "agent_spawned",
        ]
    );
    let work = std::fs::canonicalize(Home::new(dir.path()).work_dir(&id)).unwrap();
    assert_eq!(params.cwd, work.join("api"));
    assert_eq!(params.add_dirs, [work.join("web")]);
    // No agent had seen the prompt, so this one gets it with the message.
    assert_eq!(params.prompt, "the prompt\n\ntry again");
    assert_eq!(params.resume, None);
    let detail = manager.get(&id).await.unwrap();
    let flags: Vec<(&str, bool)> = detail
        .repos
        .iter()
        .map(|repo| (repo.name.as_str(), repo.primary))
        .collect();
    assert_eq!(flags, [("api", true), ("web", false)]);
    assert_eq!(detail.repos[0].branch, detail.repos[1].branch);
}

#[tokio::test]
async fn a_session_stopped_while_its_repos_are_prepared_can_be_continued() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let web = registered(&manager, clones.path(), "web").await;
    let mut wanted = request("stop early", Some("puppet"));
    wanted.repos = vec![attach(&api, None), attach(&web, None)];
    let id = manager.create_session(wanted).await.unwrap().session.id;

    // The stop lands before, between or after the two worktrees.
    within(manager.stop(&id)).await.unwrap();
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Cancelled)).await;
    let head = events.len() as u64;
    assert_eq!(manager.get(&id).await.unwrap().session.head_seq, head);

    manager.send_message(&id, "go on").await.unwrap();
    let (params, _io) = within(starts.recv()).await.unwrap();
    events_until(&manager, &id, head, is_status(SessionStatus::Running)).await;
    let detail = manager.get(&id).await.unwrap();
    let names: Vec<&str> = detail.repos.iter().map(|repo| repo.name.as_str()).collect();
    assert_eq!(names, ["api", "web"]);
    assert_eq!(params.cwd, PathBuf::from(&detail.repos[0].path));
    assert_eq!(params.add_dirs, [PathBuf::from(&detail.repos[1].path)]);
    for repo in &detail.repos {
        assert!(Path::new(&repo.path).join("README.md").is_file());
    }
}

#[tokio::test]
async fn the_repo_registry_survives_a_restart_and_guards_repos_in_use() {
    let dir = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, false).await;
    assert!(manager.list_repos().await.is_empty());
    let web = registered(&manager, clones.path(), "web").await;
    let api = registered(&manager, clones.path(), "api").await;
    assert_eq!(manager.list_repos().await, [api.clone(), web.clone()]);
    // The file is the registry; the index knows nothing of it.
    let file = std::fs::read_to_string(dir.path().join("repos.json")).unwrap();
    assert!(file.contains(&api.id) && file.contains(&web.path));

    let again = manager
        .register_repo(RegisterRepoRequest {
            path: api.path.clone(),
            name: Some("other".to_owned()),
        })
        .await;
    assert!(matches!(again, Err(Error::Conflict(_))));
    let not_a_repo = manager
        .register_repo(RegisterRepoRequest {
            path: clones.path().to_str().unwrap().to_owned(),
            name: None,
        })
        .await;
    assert!(matches!(not_a_repo, Err(Error::Invalid(_))));

    let mut wanted = request("use the api", None);
    wanted.repos = vec![attach(&api, Some(RepoMode::Live))];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    // From the moment the session exists, attached or not yet.
    assert!(matches!(
        manager.remove_repo(&api.id).await,
        Err(Error::Conflict(_))
    ));
    events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert!(matches!(
        manager.remove_repo(&api.id).await,
        Err(Error::Conflict(_))
    ));
    manager.remove_repo(&web.id).await.unwrap();
    assert!(matches!(
        manager.remove_repo(&web.id).await,
        Err(Error::Missing(_))
    ));

    manager.store().close().await;
    drop(manager);
    std::fs::remove_file(dir.path().join("calin.db")).unwrap();
    let (manager, _starts) = manager_in(&dir, false).await;
    assert_eq!(manager.list_repos().await, std::slice::from_ref(&api));
    // Still in review after the restart, so still held.
    assert!(matches!(
        manager.remove_repo(&api.id).await,
        Err(Error::Conflict(_))
    ));
    manager.stop(&id).await.unwrap();
    manager.remove_repo(&api.id).await.unwrap();
    assert!(manager.list_repos().await.is_empty());
    // Only the entry went: the clone and the session's record of it stay.
    assert!(Path::new(&api.path).join(".git").exists());
    assert_eq!(manager.get(&id).await.unwrap().repos.len(), 1);
}

// --- Permissions ---

#[tokio::test]
async fn in_ask_mode_each_request_waits_for_the_user_and_an_allow_runs_the_call() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request_in(PermissionMode::Ask, "ask me", None))
        .await
        .unwrap()
        .session
        .id;

    let (first, asked, request) = waiting_request(&manager, &id, 0).await;
    let lead = lead_id(&first);
    assert_eq!(request.tool_kind, ToolKind::Execute);
    assert_eq!(asked.agent_id.as_deref(), Some(lead.as_str()));
    // The request hangs off the tool call it is about.
    let started = first
        .iter()
        .find(|event| {
            matches!(&event.body, EventBody::ItemStarted { item } if Some(&item.id) == request.item_id.as_ref())
        })
        .expect("the tool call started before it asked");
    assert_eq!(asked.parent_id.as_ref(), Some(&started.id));
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Waiting);
    assert_eq!(detail.agents[0].status, AgentStatus::Running);

    manager
        .resolve_permission(&id, &request.id, PermissionDecision::Allow, None)
        .await
        .unwrap();
    let seen = first.len() as u64;
    let (second, asked_again, edit) = waiting_request(&manager, &id, seen).await;
    assert_eq!(
        labels(&second)[..2],
        ["permission_resolved:allow:user", "session_status:running"]
    );
    assert_eq!(
        second[0].body,
        EventBody::PermissionResolved {
            request_id: request.id.clone(),
            decision: PermissionDecision::Allow,
            by: DecisionSource::User,
            message: None,
        }
    );
    assert_eq!(second[0].parent_id.as_ref(), Some(&asked.id));
    assert_eq!(second[0].agent_id.as_deref(), Some(lead.as_str()));
    assert_eq!(edit.tool_kind, ToolKind::Edit);
    assert_ne!(edit.id, request.id);

    // An answer comes once, and only to a request that exists.
    let again = manager.resolve_permission(&id, &request.id, PermissionDecision::Deny, None);
    assert!(matches!(again.await, Err(Error::Conflict(_))));
    let unknown = manager.resolve_permission(&id, "nope", PermissionDecision::Allow, None);
    assert!(matches!(unknown.await, Err(Error::Missing(_))));
    assert_eq!(
        manager.get(&id).await.unwrap().session.status,
        SessionStatus::Waiting
    );

    manager
        .resolve_permission(&id, &edit.id, PermissionDecision::Allow, None)
        .await
        .unwrap();
    let seen = seen + second.len() as u64;
    let rest = events_until(&manager, &id, seen, is_status(SessionStatus::Review)).await;
    assert_eq!(rest[0].parent_id.as_ref(), Some(&asked_again.id));
    let all = [first, second, rest].concat();
    let tools: Vec<ItemStatus> = all
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemCompleted { item } => match item.content {
                ItemContent::Tool { .. } => Some(item.status),
                _ => None,
            },
            _ => None,
        })
        .collect();
    // The command, the subagent's read, the subagent's own call, the edit.
    assert_eq!(tools, [ItemStatus::Completed; 4]);
    let waits = labels(&all)
        .iter()
        .filter(|label| *label == "session_status:waiting")
        .count();
    assert_eq!(waits, 2);
    assert!(!labels(&all).iter().any(|label| label.ends_with(":expired")));
    assert!(
        manager
            .store()
            .open_permissions(None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_denied_request_declines_the_call_and_the_turn_goes_on() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request_in(PermissionMode::Ask, "deny it", None))
        .await
        .unwrap()
        .session
        .id;
    let (first, _, request) = waiting_request(&manager, &id, 0).await;
    let reason = Some("not on a Friday".to_owned());
    manager
        .resolve_permission(&id, &request.id, PermissionDecision::Deny, reason.clone())
        .await
        .unwrap();
    let seen = first.len() as u64;
    let (second, _, edit) = waiting_request(&manager, &id, seen).await;
    assert_eq!(
        second[0].body,
        EventBody::PermissionResolved {
            request_id: request.id.clone(),
            decision: PermissionDecision::Deny,
            by: DecisionSource::User,
            message: reason.clone(),
        }
    );
    // The agent was told, and says so in the declined call.
    let declined = second
        .iter()
        .find_map(|event| match &event.body {
            EventBody::ItemCompleted { item } if item.status == ItemStatus::Declined => {
                Some(item.content.clone())
            }
            _ => None,
        })
        .expect("the denied call was not declined");
    assert!(matches!(declined, ItemContent::Tool { output, .. } if output == reason));

    manager
        .resolve_permission(&id, &edit.id, PermissionDecision::Deny, None)
        .await
        .unwrap();
    let seen = seen + second.len() as u64;
    let rest = events_until(&manager, &id, seen, is_status(SessionStatus::Review)).await;
    // A declined edit changed no file.
    assert!(
        !labels(&rest)
            .iter()
            .any(|label| label.starts_with("file_changed"))
    );
    assert_eq!(
        labels(&rest)[..2],
        ["permission_resolved:deny:user", "session_status:running"]
    );
}

#[tokio::test]
async fn accept_edits_waits_only_for_what_the_runner_still_asks() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request_in(PermissionMode::AcceptEdits, "edits", None))
        .await
        .unwrap()
        .session
        .id;
    let (first, _, request) = waiting_request(&manager, &id, 0).await;
    assert_eq!(request.tool_kind, ToolKind::Execute);
    manager
        .resolve_permission(&id, &request.id, PermissionDecision::Allow, None)
        .await
        .unwrap();
    let rest = events_until(
        &manager,
        &id,
        first.len() as u64,
        is_status(SessionStatus::Review),
    )
    .await;
    let labels = labels(&rest);
    assert!(
        !labels
            .iter()
            .any(|label| label.starts_with("permission_requested"))
    );
    assert!(labels.contains(&"file_changed:edit".to_owned()));
}

#[tokio::test]
async fn in_autonomous_mode_the_policy_allows_at_once_and_records_it() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("on your own", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (params, mut io) = within(starts.recv()).await.unwrap();
    assert_eq!(params.permission_mode, PermissionMode::Autonomous);
    let call = tool("t1", ToolKind::Execute, ItemStatus::Running, &[]);
    emit(
        &io,
        RunnerOutput::Event(EventBody::ItemStarted { item: call }),
    )
    .await;
    emit(&io, ask("p1", Some("t1"))).await;
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision {
            request_id: "p1".to_owned(),
            decision: PermissionDecision::Allow,
            message: None,
            grants: Vec::new(),
        })
    );
    // A request that names no item is caused by the turn's message.
    emit(&io, ask("p2", None)).await;
    assert!(matches!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision { request_id, .. }) if request_id == "p2"
    ));
    emit(&io, RunnerOutput::TurnEnded).await;

    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events)[4..],
        [
            "item_started",
            "permission_requested:Bash",
            "permission_resolved:allow:policy",
            "permission_requested:Bash",
            "permission_resolved:allow:policy",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    let lead = lead_id(&events);
    assert_eq!(events[5].parent_id.as_ref(), Some(&events[4].id));
    assert_eq!(events[6].parent_id.as_ref(), Some(&events[5].id));
    assert_eq!(events[7].parent_id.as_ref(), Some(&events[1].id));
    assert_eq!(events[8].parent_id.as_ref(), Some(&events[7].id));
    for event in &events[5..9] {
        assert_eq!(event.agent_id.as_deref(), Some(lead.as_str()));
    }
    // The session never waited, and the user has nothing left to answer.
    let answer = manager.resolve_permission(&id, "p1", PermissionDecision::Deny, None);
    assert!(matches!(answer.await, Err(Error::Conflict(_))));
}

#[tokio::test]
async fn a_request_still_open_when_its_turn_or_its_agent_ends_expires() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request_in(PermissionMode::Ask, "ask", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, mut io) = within(starts.recv()).await.unwrap();
    emit(&io, ask("p1", None)).await;
    emit(&io, ask("p2", None)).await;
    let (first, _, _) = waiting_request(&manager, &id, 0).await;
    // The second request arrives while the session already waits.
    let seen = first.len() as u64;
    events_until(&manager, &id, seen, |event| {
        matches!(event.body, EventBody::PermissionRequested { .. })
    })
    .await;
    let seen = seen + 1;

    // One answered: the other still holds the session.
    manager
        .resolve_permission(&id, "p2", PermissionDecision::Allow, None)
        .await
        .unwrap();
    assert!(matches!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision { request_id, .. }) if request_id == "p2"
    ));
    assert_eq!(
        manager.get(&id).await.unwrap().session.status,
        SessionStatus::Waiting
    );

    emit(&io, RunnerOutput::TurnEnded).await;
    let ended = events_until(&manager, &id, seen, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&ended),
        [
            "permission_resolved:allow:user",
            "permission_resolved:deny:expired",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    assert_eq!(ended[1].body, expired_body("p1"));
    assert_eq!(ended[1].agent_id.as_deref(), Some(lead_id(&first).as_str()));
    assert_eq!(
        ended[1].parent_id.as_ref(),
        Some(&first[first.len() - 2].id)
    );
    // Nothing was sent to the agent for the request it had already dropped.
    assert!(io.inputs.try_recv().is_err());
    let late = manager.resolve_permission(&id, "p1", PermissionDecision::Allow, None);
    assert!(matches!(late.await, Err(Error::Conflict(_))));

    // The same when the agent exits instead of ending its turn.
    let seen = seen + ended.len() as u64;
    manager.send_message(&id, "again").await.unwrap();
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::UserMessage("again".to_owned()))
    );
    emit(&io, ask("p3", None)).await;
    let (second, _, _) = waiting_request(&manager, &id, seen).await;
    emit(&io, RunnerOutput::Exited(Exit::Ok)).await;
    let seen = seen + second.len() as u64;
    let exited = events_until(&manager, &id, seen, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&exited),
        [
            "permission_resolved:deny:expired",
            "agent_status:done",
            "session_status:review",
        ]
    );
    assert_eq!(exited[0].body, expired_body("p3"));
    assert!(
        manager
            .store()
            .open_permissions(None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn interrupting_a_waiting_session_expires_its_request_and_ends_the_turn() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request_in(PermissionMode::Ask, "interrupt me", None))
        .await
        .unwrap()
        .session
        .id;
    let (first, asked, request) = waiting_request(&manager, &id, 0).await;
    manager.interrupt(&id).await.unwrap();
    let rest = events_until(
        &manager,
        &id,
        first.len() as u64,
        is_status(SessionStatus::Review),
    )
    .await;
    assert_eq!(
        labels(&rest),
        [
            "permission_resolved:deny:expired",
            "session_status:running",
            "item_completed",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    assert_eq!(rest[0].body, expired_body(&request.id));
    assert_eq!(rest[0].parent_id.as_ref(), Some(&asked.id));
    // The call that was waiting did not run.
    assert!(matches!(
        &rest[2].body,
        EventBody::ItemCompleted { item } if item.status == ItemStatus::Failed
    ));
    let late = manager.resolve_permission(&id, &request.id, PermissionDecision::Allow, None);
    assert!(matches!(late.await, Err(Error::Conflict(_))));
    assert!(matches!(
        manager.interrupt(&id).await,
        Err(Error::Conflict(_))
    ));

    // The agent is alive and takes the next message.
    manager.send_message(&id, "and now?").await.unwrap();
    let seen = (first.len() + rest.len()) as u64;
    let (_, _, next) = waiting_request(&manager, &id, seen).await;
    assert_ne!(next.id, request.id);
}

#[tokio::test]
async fn stopping_a_waiting_session_expires_its_request() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request_in(PermissionMode::Ask, "stop me", None))
        .await
        .unwrap()
        .session
        .id;
    let (first, asked, request) = waiting_request(&manager, &id, 0).await;
    manager.stop(&id).await.unwrap();
    let rest = events_until(
        &manager,
        &id,
        first.len() as u64,
        is_status(SessionStatus::Cancelled),
    )
    .await;
    assert_eq!(
        labels(&rest),
        [
            "permission_resolved:deny:expired",
            "item_completed",
            "agent_status:stopped",
            "session_status:cancelled",
        ]
    );
    assert_eq!(rest[0].body, expired_body(&request.id));
    assert_eq!(rest[0].parent_id.as_ref(), Some(&asked.id));
    let late = manager.resolve_permission(&id, &request.id, PermissionDecision::Allow, None);
    assert!(matches!(late.await, Err(Error::Conflict(_))));
    assert!(
        manager
            .store()
            .open_permissions(None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_restart_expires_the_requests_a_crash_left_open() {
    let dir = TempDir::new().unwrap();
    let (manager, _starts) = manager_in(&dir, false).await;
    let id = manager
        .create_session(request_in(PermissionMode::Ask, "crash", None))
        .await
        .unwrap()
        .session
        .id;
    let (first, asked, request) = waiting_request(&manager, &id, 0).await;
    let lead = lead_id(&first);
    // The process dies while the card is on screen.
    manager.store().close().await;
    drop(manager);

    let (manager, _starts) = manager_in(&dir, false).await;
    let rest = events_until(
        &manager,
        &id,
        first.len() as u64,
        is_status(SessionStatus::Failed),
    )
    .await;
    assert_eq!(
        labels(&rest),
        [
            "permission_resolved:deny:expired",
            "agent_status:failed",
            "session_status:failed",
        ]
    );
    assert_eq!(rest[0].body, expired_body(&request.id));
    assert_eq!(rest[0].parent_id.as_ref(), Some(&asked.id));
    assert_eq!(rest[0].agent_id.as_deref(), Some(lead.as_str()));
    assert_eq!(
        rest[2].body,
        EventBody::SessionStatus {
            status: SessionStatus::Failed,
            reason: Some("server restarted".to_owned()),
        }
    );
    assert!(
        manager
            .store()
            .open_permissions(None)
            .await
            .unwrap()
            .is_empty()
    );
    let late = manager.resolve_permission(&id, &request.id, PermissionDecision::Allow, None);
    assert!(matches!(late.await, Err(Error::Conflict(_))));

    // A second restart finds nothing left to settle.
    let head = manager.get(&id).await.unwrap().session.head_seq;
    manager.store().close().await;
    drop(manager);
    let (manager, _starts) = manager_in(&dir, false).await;
    assert_eq!(manager.get(&id).await.unwrap().session.head_seq, head);

    // The failed session is continued by a message, in the same conversation.
    manager.send_message(&id, "back again").await.unwrap();
    let (resumed, _, next) = waiting_request(&manager, &id, head).await;
    assert_eq!(
        labels(&resumed)[..3],
        [
            "user_message:back again",
            "session_status:running",
            "agent_status:running",
        ]
    );
    // The same conversation id as before, so it is not announced again.
    assert!(!labels(&resumed).contains(&"agent_session".to_owned()));
    assert_ne!(next.id, request.id);
    assert_eq!(manager.get(&id).await.unwrap().agents.len(), 1);
}

// --- Interrupt and continue ---

#[tokio::test]
async fn interrupt_ends_the_turn_and_holds_queued_messages_for_the_next_one() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let id = manager
        .create_session(request("first", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (_, mut io) = within(starts.recv()).await.unwrap();
    emit(
        &io,
        RunnerOutput::Event(EventBody::ItemStarted { item: item("i1") }),
    )
    .await;
    events_until(&manager, &id, 0, |event| {
        matches!(event.body, EventBody::ItemStarted { .. })
    })
    .await;
    manager.send_message(&id, "queued").await.unwrap();
    manager.interrupt(&id).await.unwrap();
    assert_eq!(within(io.inputs.recv()).await, Some(RunnerInput::Interrupt));
    // The turn is over when the agent says so, not before.
    assert_eq!(
        manager.get(&id).await.unwrap().session.status,
        SessionStatus::Running
    );
    emit(&io, RunnerOutput::TurnEnded).await;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events)[4..],
        [
            "item_started",
            "user_message:queued",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    // Interrupted means review: the queued message did not start a turn.
    assert!(io.inputs.try_recv().is_err());
    assert!(matches!(
        manager.interrupt(&id).await,
        Err(Error::Conflict(_))
    ));

    // It runs first when the user speaks again, then the new message.
    manager.send_message(&id, "next").await.unwrap();
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::UserMessage("queued".to_owned()))
    );
    emit(&io, RunnerOutput::TurnEnded).await;
    assert_eq!(
        within(io.inputs.recv()).await,
        Some(RunnerInput::UserMessage("next".to_owned()))
    );
    emit(&io, RunnerOutput::TurnEnded).await;
    let seen = events.len() as u64;
    let rest = events_until(&manager, &id, seen, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&rest),
        [
            "user_message:next",
            "session_status:running",
            "agent_status:running",
            // The held message's turn ends, the new message's turn begins.
            "agent_status:idle",
            "agent_status:running",
            "agent_status:idle",
            "session_status:review",
        ]
    );
}

#[tokio::test]
async fn a_cancelled_or_failed_session_continues_its_conversation_on_a_message() {
    let dir = TempDir::new().unwrap();
    let (manager, mut starts) = manager_in(&dir, false).await;
    let id = manager
        .create_session(request("first", Some("puppet")))
        .await
        .unwrap()
        .session
        .id;
    let (first, mut io) = within(starts.recv()).await.unwrap();
    assert_eq!(first.resume, None);
    let announced = RunnerOutput::NativeSession {
        id: "conversation-1".to_owned(),
        model: Some("model-a".to_owned()),
    };
    emit(&io, announced).await;
    emit(&io, RunnerOutput::TurnEnded).await;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        events[4].body,
        EventBody::AgentSession {
            native_session_id: "conversation-1".to_owned(),
            model: Some("model-a".to_owned()),
        }
    );
    assert_eq!(events[4].agent_id.as_deref(), Some(first.agent_id.as_str()));

    // Stopped: cancelled, and the agent is gone.
    let stopping = tokio::spawn({
        let (manager, id) = (manager.clone(), id.clone());
        async move { manager.stop(&id).await }
    });
    assert_eq!(within(io.inputs.recv()).await, Some(RunnerInput::Stop));
    emit(&io, RunnerOutput::Exited(Exit::Ok)).await;
    within(stopping).await.unwrap().unwrap();
    let head = manager.get(&id).await.unwrap().session.head_seq;

    manager.send_message(&id, "carry on").await.unwrap();
    let (second, io) = within(starts.recv()).await.unwrap();
    assert_eq!(second.resume.as_deref(), Some("conversation-1"));
    assert_eq!(second.agent_id, first.agent_id);
    assert_eq!(second.prompt, "carry on");
    assert_eq!(second.cwd, first.cwd);
    assert!(second.start_seq > first.start_seq);
    emit(&io, RunnerOutput::Exited(Exit::Error("boom".to_owned()))).await;
    let rest = events_until(&manager, &id, head, is_status(SessionStatus::Failed)).await;
    assert_eq!(
        labels(&rest),
        [
            "user_message:carry on",
            "session_status:running",
            "agent_status:running",
            "error:boom",
            "agent_status:failed",
            "session_status:failed",
        ]
    );

    // Failed: the same again, and the runner may move to a new conversation.
    let head = head + rest.len() as u64;
    manager.send_message(&id, "once more").await.unwrap();
    let (third, io) = within(starts.recv()).await.unwrap();
    assert_eq!(third.resume.as_deref(), Some("conversation-1"));
    assert_eq!(third.agent_id, first.agent_id);
    assert_eq!(third.prompt, "once more");
    let moved = RunnerOutput::NativeSession {
        id: "conversation-2".to_owned(),
        model: None,
    };
    emit(&io, moved).await;
    emit(&io, RunnerOutput::TurnEnded).await;
    let rest = events_until(&manager, &id, head, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&rest),
        [
            "user_message:once more",
            "session_status:running",
            "agent_status:running",
            "agent_session",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.agents.len(), 1);
    let lead = &detail.agents[0];
    assert_eq!(lead.native_session_id.as_deref(), Some("conversation-2"));
    // A report without a model keeps the one known.
    assert_eq!(lead.model.as_deref(), Some("model-a"));

    // The id is in the log: it survives the process and even the index.
    manager.store().close().await;
    drop((manager, io, starts));
    std::fs::remove_file(dir.path().join("calin.db")).unwrap();
    let (manager, mut starts) = manager_in(&dir, false).await;
    assert_eq!(manager.get(&id).await.unwrap(), detail);
    manager
        .send_message(&id, "after the restart")
        .await
        .unwrap();
    let (fourth, _io) = within(starts.recv()).await.unwrap();
    assert_eq!(fourth.resume.as_deref(), Some("conversation-2"));
    assert_eq!(fourth.agent_id, first.agent_id);
    assert_eq!(fourth.cwd, first.cwd);
}

#[tokio::test]
async fn a_session_whose_runner_never_started_gets_the_prompt_with_the_first_message() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let id = manager
        .create_session(request("the prompt", Some("broken")))
        .await
        .unwrap()
        .session
        .id;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Failed)).await;
    // Still broken: the session fails again, and says why again.
    manager.send_message(&id, "retry").await.unwrap();
    let rest = events_until(
        &manager,
        &id,
        events.len() as u64,
        is_status(SessionStatus::Failed),
    )
    .await;
    assert_eq!(
        labels(&rest),
        [
            "user_message:retry",
            "session_status:running",
            "error:the agent could not be started: no such binary",
            "session_status:failed",
        ]
    );
}

// --- File changes ---

#[tokio::test]
async fn completed_edits_become_file_changes_caused_by_their_tool_call() {
    let Fixture {
        manager,
        mut starts,
        dir: _dir,
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let web = registered(&manager, clones.path(), "web").await;
    let mut wanted = request("edit things", Some("puppet"));
    wanted.repos = vec![attach(&api, None), attach(&web, Some(RepoMode::Live))];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let (params, io) = within(starts.recv()).await.unwrap();
    let detail = manager.get(&id).await.unwrap();
    let (api_id, web_id) = (detail.repos[0].id.clone(), detail.repos[1].id.clone());
    let in_api = |rel: &str| params.cwd.join(rel).to_str().unwrap().to_owned();
    let in_web = format!("{}/docs/guide.md", web.path);

    let done = |id: &str, kind, status, locations: &[&str]| {
        let item = tool(id, kind, status, locations);
        RunnerOutput::Event(EventBody::ItemCompleted { item })
    };
    let completed = ItemStatus::Completed;
    let outputs = [
        done(
            "e1",
            ToolKind::Edit,
            completed,
            &[&in_api("src/new.rs"), &in_web, "/etc/hosts"],
        ),
        done("d1", ToolKind::Delete, completed, &[&in_api("old.txt")]),
        done(
            "m1",
            ToolKind::Move,
            completed,
            &[&in_api("a.txt"), &in_api("deep/b.txt")],
        ),
        // None of these changed a file.
        done("r1", ToolKind::Read, completed, &[&in_api("README.md")]),
        done("x1", ToolKind::Execute, completed, &[&in_api("README.md")]),
        done("e2", ToolKind::Edit, ItemStatus::Failed, &[&in_api("f.rs")]),
        done(
            "e3",
            ToolKind::Edit,
            ItemStatus::Declined,
            &[&in_api("g.rs")],
        ),
        done("e4", ToolKind::Edit, completed, &[]),
        RunnerOutput::Event(EventBody::ItemCompleted { item: item("t1") }),
        RunnerOutput::TurnEnded,
    ];
    for output in outputs {
        emit(&io, output).await;
    }
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let lead = lead_id(&events);

    // Each change follows its tool call and points back at it.
    let mut changes = Vec::new();
    let mut last_completed: Option<(&Event, String)> = None;
    for event in &events {
        match &event.body {
            EventBody::ItemCompleted { item } => last_completed = Some((event, item.id.clone())),
            EventBody::FileChanged {
                session_repo_id,
                path,
                op,
            } => {
                let (cause, item_id) = last_completed.clone().expect("a change without a call");
                assert_eq!(event.parent_id.as_ref(), Some(&cause.id));
                assert_eq!(event.agent_id.as_deref(), Some(lead.as_str()));
                changes.push((item_id, session_repo_id.clone(), path.clone(), *op));
            }
            _ => {}
        }
    }
    let change = |item: &str, repo: Option<&String>, path: &str, op| {
        (item.to_owned(), repo.cloned(), path.to_owned(), op)
    };
    assert_eq!(
        changes,
        [
            change("e1", Some(&api_id), "src/new.rs", FileOp::Edit),
            change("e1", Some(&web_id), "docs/guide.md", FileOp::Edit),
            // Outside every repo: no repo, and the path as the agent gave it.
            change("e1", None, "/etc/hosts", FileOp::Edit),
            change("d1", Some(&api_id), "old.txt", FileOp::Delete),
            change("m1", Some(&api_id), "a.txt", FileOp::Rename),
            change("m1", Some(&api_id), "deep/b.txt", FileOp::Rename),
        ]
    );
}

#[tokio::test]
async fn the_mock_agent_leaves_a_real_change_that_the_files_api_shows_and_edits() {
    let Fixture {
        manager, dir: _dir, ..
    } = fixture().await;
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let docs = registered(&manager, clones.path(), "docs").await;
    write_file(Path::new(&docs.path), "notes.txt", "uncommitted\n");
    let mut wanted = request("Make a note", None);
    wanted.repos = vec![attach(&api, None), attach(&docs, Some(RepoMode::Read))];
    let id = manager.create_session(wanted).await.unwrap().session.id;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    let detail = manager.get(&id).await.unwrap();
    let (write, read) = (&detail.repos[0], &detail.repos[1]);

    // The mock's edit landed in the worktree and is on record as a change.
    let changed: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event.body, EventBody::FileChanged { .. }))
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(
        changed[0].body,
        EventBody::FileChanged {
            session_repo_id: Some(write.id.clone()),
            path: "calin-mock-notes.md".to_owned(),
            op: FileOp::Edit,
        }
    );
    let cause = events
        .iter()
        .find(|event| Some(&event.id) == changed[0].parent_id.as_ref())
        .unwrap();
    assert!(matches!(
        &cause.body,
        EventBody::ItemCompleted { item }
            if matches!(item.content, ItemContent::Tool { tool_kind: ToolKind::Edit, .. })
    ));

    let changes = manager.changes(&id).await.unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0].session_repo_id, write.id);
    assert_eq!(changes[0].name, "api");
    assert_eq!(changes[0].branch, write.branch);
    assert_eq!(changes[0].base_ref, write.base_ref);
    assert_eq!((changes[0].added, changes[0].removed), (1, 0));
    let [file] = changes[0].files.as_slice() else {
        panic!("expected one changed file, got {:?}", changes[0].files);
    };
    assert_eq!(file.path, "calin-mock-notes.md");
    assert_eq!(file.status, ChangeStatus::Untracked);
    // A repo attached read-only reports nothing, whatever is in its checkout.
    assert_eq!(changes[1].session_repo_id, read.id);
    assert!(changes[1].files.is_empty());
    assert_eq!((changes[1].added, changes[1].removed), (0, 0));

    let note = "- turn 1: Make a note\n";
    let diff = manager
        .file_diff(&id, &write.id, "calin-mock-notes.md")
        .await
        .unwrap();
    assert_eq!(
        (diff.old.as_deref(), diff.new.as_deref()),
        (None, Some(note))
    );
    let tree = manager.tree(&id, &write.id, "").await.unwrap();
    let names: Vec<(&str, EntryKind)> = tree
        .iter()
        .map(|entry| (entry.name.as_str(), entry.kind))
        .collect();
    assert_eq!(
        names,
        [
            ("calin-mock-notes.md", EntryKind::File),
            ("README.md", EntryKind::File)
        ]
    );
    let content = manager
        .read_file(&id, &write.id, "calin-mock-notes.md")
        .await
        .unwrap();
    assert_eq!(content.text.as_deref(), Some(note));

    // Saving from the editor is an event of the user's, not of an agent.
    let head = manager.get(&id).await.unwrap().session.head_seq;
    let save = |text: &str, hash: Option<&str>| WriteFileRequest {
        text: text.to_owned(),
        expected_hash: hash.map(str::to_owned),
    };
    let saved = manager
        .write_file(
            &id,
            &write.id,
            "calin-mock-notes.md",
            save("edited\n", Some(&content.hash)),
        )
        .await
        .unwrap();
    assert_ne!(saved.hash, content.hash);
    let created = manager
        .write_file(&id, &write.id, "src/new.rs", save("fn new() {}\n", None))
        .await
        .unwrap();
    assert_eq!(created.path, "src/new.rs");
    let logged = events_until(&manager, &id, head, |event| event.seq == head + 2).await;
    let bodies: Vec<&EventBody> = logged.iter().map(|event| &event.body).collect();
    let changed = |path: &str, op| EventBody::FileChanged {
        session_repo_id: Some(write.id.clone()),
        path: path.to_owned(),
        op,
    };
    assert_eq!(
        bodies,
        [
            &changed("calin-mock-notes.md", FileOp::Edit),
            &changed("src/new.rs", FileOp::Create)
        ]
    );
    assert!(logged.iter().all(|event| event.agent_id.is_none()));

    // A stale hash is refused and changes neither the file nor the log.
    let stale = manager
        .write_file(
            &id,
            &write.id,
            "calin-mock-notes.md",
            save("lost\n", Some(&content.hash)),
        )
        .await;
    assert!(matches!(stale, Err(Error::Conflict(_))));
    let unsafe_path = manager
        .write_file(&id, &write.id, "../escape", save("x", None))
        .await;
    assert!(matches!(unsafe_path, Err(Error::Invalid(_))));
    let read_only = manager
        .write_file(&id, &read.id, "new.txt", save("x", None))
        .await;
    assert!(matches!(read_only, Err(Error::Conflict(_))));
    assert!(!Path::new(&docs.path).join("new.txt").exists());
    assert_eq!(manager.get(&id).await.unwrap().session.head_seq, head + 2);
    let now = manager
        .read_file(&id, &write.id, "calin-mock-notes.md")
        .await
        .unwrap();
    assert_eq!(now.text.as_deref(), Some("edited\n"));
    // Reading is fine in a read-only repo.
    let readme = manager.read_file(&id, &read.id, "README.md").await.unwrap();
    assert_eq!(readme.text.as_deref(), Some("# readme\n"));
    assert!(matches!(
        manager.read_file(&id, "nope", "README.md").await,
        Err(Error::Missing(_))
    ));
    assert!(matches!(
        manager.read_file(&id, &write.id, "missing.txt").await,
        Err(Error::Missing(_))
    ));
}
