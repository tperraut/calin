use std::collections::HashSet;
use std::future::Future;

use calin_agents::{AgentIo, MockConfig, RegistryConfig, RunnerError, RunnerInput};
use calin_proto::{Item, ItemContent, ItemStatus};
use tempfile::TempDir;

use super::*;
use crate::home::Home;

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
    });
    let (starts, started) = mpsc::unbounded_channel();
    registry.register(Arc::new(Puppet { starts }));
    registry.register(Arc::new(Broken));
    (registry, started)
}

async fn manager_in(dir: &TempDir, dry_run: bool) -> (SessionManager, Starts) {
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let (registry, starts) = registry(dry_run);
    let manager = SessionManager::start(store, registry).await.unwrap();
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

fn request(prompt: &str, runner: Option<&str>) -> CreateSessionRequest {
    CreateSessionRequest {
        prompt: prompt.to_owned(),
        title: None,
        runner: runner.map(str::to_owned),
    }
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

async fn emit(io: &AgentIo, output: RunnerOutput) {
    io.outputs.send(output).await.unwrap();
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
    let session = &detail.session;
    assert_eq!(session.title, "Fix the login bug");
    assert_eq!(session.prompt, "Fix the login bug\nIt fails on Fridays.");
    assert_eq!(session.runner, "mock");
    assert_eq!(session.root_session_id, session.id);
    assert_eq!(detail.agents.len(), 1);
    let lead = &detail.agents[0];
    assert_eq!(lead.role, "lead");
    assert_eq!(lead.kind, AgentKind::Process);
    assert_eq!(lead.runner, "mock");
    assert_eq!(lead.session_id, session.id);

    let events = events_until(&manager, &session.id, 0, is_status(SessionStatus::Review)).await;
    let labels = labels(&events);
    assert_eq!(
        labels[..4],
        [
            "session_status:queued",
            "user_message:Fix the login bug\nIt fails on Fridays.",
            "session_status:running",
            "agent_spawned",
        ]
    );
    assert_eq!(
        labels[labels.len() - 2..],
        ["agent_status:idle", "session_status:review"]
    );
    let turn = &events[4..events.len() - 2];
    assert!(turn.len() > 10);
    for event in turn {
        assert!(
            matches!(
                event.body,
                EventBody::ItemStarted { .. }
                    | EventBody::ItemDelta { .. }
                    | EventBody::ItemCompleted { .. }
                    | EventBody::PlanUpdated { .. }
                    | EventBody::Usage { .. }
            ),
            "unexpected {} inside the turn",
            label(event)
        );
        assert_eq!(event.agent_id.as_deref(), Some(lead.id.as_str()));
        // The prompt caused the turn.
        assert_eq!(event.parent_id.as_ref(), Some(&events[1].id));
    }
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
    assert!(!labels.contains(&"agent_spawned".to_owned()));
    assert_eq!(second[0].seq, after + 1);

    let ids: Vec<String> = [started_item_ids(&first), started_item_ids(&second)].concat();
    assert_eq!(ids.len(), 10);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 10);

    let detail = manager.get(&id).await.unwrap();
    assert_eq!(detail.agents.len(), 1);
    assert_eq!(detail.agents[0].tokens_in, 2400);
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
    assert!(!params.resume);
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
            "item_started",
            "agent_status:idle",
            "session_status:review",
        ]
    );
    // Each turn's events point at the message that caused it.
    assert_eq!(events[4].parent_id.as_ref(), Some(&events[1].id));
    assert_eq!(events[6].parent_id.as_ref(), Some(&events[5].id));
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

    assert!(matches!(
        manager.send_message(&id, "hello?").await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(manager.stop(&id).await, Err(Error::Conflict(_))));
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
            "agent_status:stopped",
            "session_status:cancelled"
        ]
    );
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
    assert!(matches!(
        manager.send_message(&id, "again").await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(manager.stop(&id).await, Err(Error::Conflict(_))));
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
    assert_eq!(detail.session.status, SessionStatus::Failed);
    assert!(detail.agents.is_empty());
    let id = &detail.session.id;
    let events = events_until(&manager, id, 0, is_status(SessionStatus::Failed)).await;
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
    let (_, io) = within(starts.recv()).await.unwrap();
    emit(&io, RunnerOutput::Exited(Exit::Ok)).await;
    let events = events_until(&manager, &id, 0, is_status(SessionStatus::Review)).await;
    assert_eq!(
        labels(&events)[4..],
        ["agent_status:done", "session_status:review"]
    );

    manager.send_message(&id, "more").await.unwrap();
    let (params, io) = within(starts.recv()).await.unwrap();
    assert!(params.resume);
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
    assert!(manager.list().await.unwrap().is_empty());

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
        assert_eq!(detail.agents[0].runner, "mock");
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
    assert!(matches!(
        manager.send_message(&interrupted, "hi").await,
        Err(Error::Conflict(_))
    ));

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
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 10);
    assert_eq!(manager.get(&settled).await.unwrap().agents.len(), 1);
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
