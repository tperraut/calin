use std::collections::HashSet;
use std::io::Write;
use std::time::Duration;

use calin_proto::{
    Agent, AgentKind, AgentStatus, AttachRepoRequest, CheckState, DecisionSource, Event, EventBody,
    FileOp, PermissionDecision, PermissionMode, PermissionRequest, PullRequest, PullRequestState,
    RepoMode, ReviewState, SessionRepo, SessionStatus, ToolKind,
};
use tempfile::TempDir;
use tokio::task::JoinSet;

use super::writer::{LIVE_BUFFER, UPSERT_INTERVAL};
use super::*;

const TIMEOUT: Duration = Duration::from_secs(20);

async fn open(dir: &TempDir) -> Store {
    Store::open(Home::new(dir.path())).await.unwrap()
}

fn new_session(prompt: &str) -> NewSession {
    NewSession {
        title: format!("title of {prompt}"),
        prompt: prompt.to_owned(),
        runner: "mock".to_owned(),
        permission_mode: PermissionMode::Ask,
        model: None,
        repos: Vec::new(),
    }
}

fn message(n: u64) -> EventDraft {
    EventDraft::new(EventBody::UserMessage {
        text: format!("message {n}"),
    })
}

fn status(status: SessionStatus) -> EventDraft {
    EventDraft::new(EventBody::SessionStatus {
        status,
        reason: None,
    })
}

fn agent(id: &str, session_id: &str) -> Agent {
    Agent {
        id: id.to_owned(),
        session_id: session_id.to_owned(),
        parent_agent_id: None,
        kind: AgentKind::Process,
        role: "lead".to_owned(),
        runner: "mock".to_owned(),
        model: Some("mock-1".to_owned()),
        native_session_id: None,
        status: AgentStatus::Running,
        cost_usd: 0.0,
        tokens_in: 0,
        tokens_out: 0,
    }
}

fn read_log(store: &Store, session_id: &str) -> Vec<Event> {
    std::fs::read_to_string(store.home().log_path(session_id))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn recv(live: &mut Live<Arc<Event>>) -> Arc<Event> {
    tokio::time::timeout(TIMEOUT, live.recv())
        .await
        .expect("no live event arrived")
        .expect("the live stream ended")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sequence_numbers_are_gapless_per_session() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let one = store.create_session(new_session("one")).await.unwrap().id;
    let two = store.create_session(new_session("two")).await.unwrap().id;

    // Eight tasks race on two sessions.
    let mut tasks = JoinSet::new();
    for task in 0..8u64 {
        let store = store.clone();
        let session = if task % 2 == 0 {
            one.clone()
        } else {
            two.clone()
        };
        tasks.spawn(async move {
            for n in 0..50 {
                store.append(&session, message(n)).await.unwrap();
            }
        });
    }
    while let Some(done) = tasks.join_next().await {
        done.unwrap();
    }

    let mut ids = HashSet::new();
    for session in [&one, &two] {
        let log = read_log(&store, session);
        assert_eq!(log.len(), 200);
        for (index, event) in log.iter().enumerate() {
            assert_eq!(event.seq, index as u64 + 1);
            assert_eq!(&event.session_id, session);
            assert!(ids.insert(event.id.clone()), "duplicate event id");
        }
        assert!(log.windows(2).all(|pair| pair[0].ts <= pair[1].ts));
        assert!(log.windows(2).all(|pair| pair[0].id < pair[1].id));
        assert_eq!(store.session(session).await.unwrap().session.head_seq, 200);
        // The log and the replay are the same events.
        assert_eq!(store.subscribe(session, 0).await.unwrap().backlog, log);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_event_is_in_the_log_before_a_subscriber_sees_it() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let session = store
        .create_session(new_session("log first"))
        .await
        .unwrap();
    let mut subscription = store.subscribe(&session.id, 0).await.unwrap();
    assert!(subscription.backlog.is_empty());
    assert_eq!(subscription.head_seq, 0);

    let appender = tokio::spawn({
        let (store, id) = (store.clone(), session.id.clone());
        async move {
            for n in 0..100 {
                store.append(&id, message(n)).await.unwrap();
            }
        }
    });
    for seq in 1..=100u64 {
        let event = recv(&mut subscription.live).await;
        assert_eq!(event.seq, seq);
        // Later lines may be half written at this instant; this one may not.
        let log = std::fs::read(store.home().log_path(&session.id)).unwrap();
        let line = log
            .split_inclusive(|byte| *byte == b'\n')
            .nth(seq as usize - 1)
            .filter(|line| line.ends_with(b"\n"))
            .unwrap_or_else(|| panic!("event {seq} was delivered before it was logged"));
        assert_eq!(serde_json::from_slice::<Event>(line).unwrap(), *event);
    }
    appender.await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn replay_is_exact_while_events_are_appended() {
    const TOTAL: u64 = 500;
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store
        .create_session(new_session("replay"))
        .await
        .unwrap()
        .id;

    let appender = tokio::spawn({
        let (store, id) = (store.clone(), id.clone());
        async move {
            let mut appended = Vec::new();
            for n in 0..TOTAL {
                appended.push(store.append(&id, message(n)).await.unwrap());
                if n % 7 == 0 {
                    tokio::task::yield_now().await;
                }
            }
            appended
        }
    });

    // Subscribe over and over while the appender runs, from the start, the
    // middle, just behind the head, the head itself and beyond it.
    let mut subscribers = JoinSet::new();
    let mut round = 0u64;
    loop {
        let head = store.session(&id).await.unwrap().session.head_seq;
        let after_seq = match round % 5 {
            0 => 0,
            1 => head / 2,
            2 => head.saturating_sub(1),
            3 => head,
            _ => (head + 3).min(TOTAL),
        };
        let mut subscription = store.subscribe(&id, after_seq).await.unwrap();
        subscribers.spawn(async move {
            // Ahead of the head nothing is replayed and delivery starts live.
            let first = after_seq.min(subscription.head_seq) + 1;
            let mut seen = subscription.backlog;
            if let Some(last) = seen.last() {
                assert_eq!(last.seq, subscription.head_seq);
            }
            while seen.len() < (TOTAL + 1 - first) as usize {
                seen.push((*recv(&mut subscription.live).await).clone());
            }
            (after_seq, first, seen)
        });
        round += 1;
        if head >= TOTAL {
            break;
        }
    }
    assert!(round > 5, "the test did not overlap with the appender");

    let appended = appender.await.unwrap();
    while let Some(result) = subscribers.join_next().await {
        let (after_seq, first, seen) = result.unwrap();
        let expected: Vec<Event> = appended[first as usize - 1..]
            .iter()
            .map(|event| (**event).clone())
            .collect();
        assert_eq!(
            seen.iter().map(|event| event.seq).collect::<Vec<_>>(),
            (first..=TOTAL).collect::<Vec<_>>(),
            "after_seq {after_seq}: duplicates or gaps"
        );
        assert_eq!(seen, expected);
    }
}

#[tokio::test]
async fn a_subscriber_that_falls_behind_is_cut_off() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("lag")).await.unwrap().id;
    let mut subscription = store.subscribe(&id, 0).await.unwrap();
    for n in 0..LIVE_BUFFER as u64 + 10 {
        store.append(&id, message(n)).await.unwrap();
    }
    assert!(matches!(
        subscription.live.recv().await,
        Err(LiveEnd::Lagged(_))
    ));
    // Subscribing again replays what was missed.
    let again = store.subscribe(&id, 0).await.unwrap();
    assert_eq!(again.backlog.len(), LIVE_BUFFER + 10);
}

#[tokio::test]
async fn events_keep_the_projections_current() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let older = store.create_session(new_session("older")).await.unwrap();
    tokio::time::sleep(Duration::from_millis(3)).await;
    let session = store.create_session(new_session("newer")).await.unwrap();
    assert_eq!(session.status, SessionStatus::Queued);
    assert_eq!(session.head_seq, 0);
    assert_eq!(session.root_session_id, session.id);
    let id = session.id.as_str();

    store
        .append(id, status(SessionStatus::Running))
        .await
        .unwrap();
    let detail = store.session(id).await.unwrap();
    assert_eq!(detail.session.status, SessionStatus::Running);
    assert!(detail.agents.is_empty());

    let spawned = EventBody::AgentSpawned {
        agent: agent("a1", id),
    };
    store.append(id, EventDraft::new(spawned)).await.unwrap();
    let spawned = EventBody::AgentSpawned {
        agent: agent("a2", id),
    };
    store.append(id, EventDraft::new(spawned)).await.unwrap();
    assert_eq!(
        store.session(id).await.unwrap().agents,
        [agent("a1", id), agent("a2", id)]
    );

    let idle = EventBody::AgentStatus {
        status: AgentStatus::Idle,
    };
    store
        .append(id, EventDraft::new(idle).agent("a1"))
        .await
        .unwrap();
    let usage = EventBody::Usage {
        cost_usd: 0.5,
        tokens_in: 1200,
        tokens_out: 350,
    };
    let last = store
        .append(id, EventDraft::new(usage).agent("a1"))
        .await
        .unwrap();

    let detail = store.session(id).await.unwrap();
    let expected = Agent {
        status: AgentStatus::Idle,
        cost_usd: 0.5,
        tokens_in: 1200,
        tokens_out: 350,
        ..agent("a1", id)
    };
    assert_eq!(detail.agents, [expected, agent("a2", id)]);
    assert_eq!(detail.session.head_seq, 5);
    assert_eq!(detail.session.head_seq, last.seq);
    assert_eq!(detail.session.updated_at, last.ts);
    assert_eq!(detail.session.created_at, session.created_at);

    // The other session is untouched, and the list is newest first.
    let sessions = store.sessions().await.unwrap();
    assert_eq!(
        sessions.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
        [id, older.id.as_str()]
    );
    assert_eq!(sessions[1], older);
}

#[tokio::test]
async fn session_upserts_follow_creation_and_status_changes() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let first = store.create_session(new_session("first")).await.unwrap();
    let (existing, mut updates) = store.subscribe_sessions().await.unwrap();
    assert_eq!(existing, std::slice::from_ref(&first));

    let second = store.create_session(new_session("second")).await.unwrap();
    assert_eq!(updates.recv().await.unwrap(), second);

    store.append(&first.id, message(1)).await.unwrap();
    store
        .append(&first.id, status(SessionStatus::Running))
        .await
        .unwrap();
    let upserted = updates.recv().await.unwrap();
    assert_eq!(upserted.id, first.id);
    assert_eq!(upserted.status, SessionStatus::Running);
    assert_eq!(upserted.head_seq, 2);
}

#[tokio::test]
async fn unknown_sessions_are_not_found() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    for id in ["01ARZ3NDEKTSV4RRFFQ69G5FAV", "../../etc", ""] {
        assert!(matches!(
            store.append(id, message(1)).await,
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            store.subscribe(id, 0).await.map(|_| ()),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(store.session(id).await, Err(Error::NotFound(_))));
    }
    // A lookup must not create anything on disk.
    assert_eq!(
        std::fs::read_dir(store.home().sessions_dir())
            .unwrap()
            .count(),
        0
    );
}

fn session_repo(session_id: &str, name: &str, primary: bool) -> SessionRepo {
    SessionRepo {
        id: format!("{session_id}-{name}"),
        session_id: session_id.to_owned(),
        repo_id: format!("repo-{name}"),
        name: name.to_owned(),
        mode: if primary {
            RepoMode::Live
        } else {
            RepoMode::Write
        },
        path: format!("/work/{name}"),
        // The primary repo here has no branch: the second one names the session's.
        branch: (!primary).then(|| "calin/topic-abc123".to_owned()),
        base_ref: Some("0123abcd".to_owned()),
        primary,
    }
}

fn permission_request(id: &str) -> PermissionRequest {
    PermissionRequest {
        id: id.to_owned(),
        tool_name: "Bash".to_owned(),
        tool_kind: ToolKind::Execute,
        title: "cargo test".to_owned(),
        input: serde_json::json!({ "command": "cargo test" }),
        item_id: None,
    }
}

fn resolved(request_id: &str) -> EventBody {
    EventBody::PermissionResolved {
        request_id: request_id.to_owned(),
        decision: PermissionDecision::Allow,
        by: DecisionSource::User,
        message: None,
    }
}

/// Two sessions with agents, usage, repos, permission requests and
/// different statuses.
async fn populate(store: &Store) -> Vec<String> {
    let mut ids = Vec::new();
    for (index, prompt) in ["alpha", "beta"].into_iter().enumerate() {
        let id = store.create_session(new_session(prompt)).await.unwrap().id;
        store
            .append(&id, status(SessionStatus::Queued))
            .await
            .unwrap();
        store.append(&id, message(1)).await.unwrap();
        store
            .append(&id, status(SessionStatus::Running))
            .await
            .unwrap();
        let lead = format!("agent-{prompt}");
        let spawned = EventBody::AgentSpawned {
            agent: agent(&lead, &id),
        };
        store.append(&id, EventDraft::new(spawned)).await.unwrap();
        let usage = EventBody::Usage {
            cost_usd: 0.25,
            tokens_in: 10,
            tokens_out: 20,
        };
        store
            .append(&id, EventDraft::new(usage).agent(lead.clone()))
            .await
            .unwrap();
        let announced = EventBody::AgentSession {
            native_session_id: format!("native-{prompt}"),
            model: Some("mock-2".to_owned()),
        };
        store
            .append(&id, EventDraft::new(announced).agent(lead.clone()))
            .await
            .unwrap();
        for (n, name) in ["api", "web"].into_iter().enumerate() {
            let attached = EventBody::RepoAttached {
                repo: session_repo(&id, name, n == 0),
            };
            store.append(&id, EventDraft::new(attached)).await.unwrap();
        }
        for request in ["answered", "open"] {
            let asked = EventBody::PermissionRequested {
                request: permission_request(request),
            };
            store
                .append(&id, EventDraft::new(asked).agent(lead.clone()))
                .await
                .unwrap();
        }
        store
            .append(
                &id,
                EventDraft::new(resolved("answered")).agent(lead.clone()),
            )
            .await
            .unwrap();
        if index == 0 {
            let idle = EventBody::AgentStatus {
                status: AgentStatus::Idle,
            };
            store
                .append(&id, EventDraft::new(idle).agent(lead))
                .await
                .unwrap();
            store
                .append(&id, status(SessionStatus::Review))
                .await
                .unwrap();
        }
        ids.push(id);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    ids
}

#[tokio::test]
async fn the_index_is_rebuilt_from_the_logs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let ids = populate(&store).await;
    let sessions = store.sessions().await.unwrap();
    let mut details = Vec::new();
    let mut logs = Vec::new();
    for id in &ids {
        details.push(store.session(id).await.unwrap());
        logs.push(store.subscribe(id, 0).await.unwrap().backlog);
    }
    assert_eq!(details[0].session.status, SessionStatus::Review);
    assert_eq!(details[1].session.status, SessionStatus::Running);
    for (detail, prompt) in details.iter().zip(["alpha", "beta"]) {
        let id = &detail.session.id;
        assert_eq!(
            detail.agents[0].native_session_id,
            Some(format!("native-{prompt}"))
        );
        assert_eq!(detail.agents[0].model.as_deref(), Some("mock-2"));
        assert_eq!(
            detail.repos,
            [
                session_repo(id, "api", true),
                session_repo(id, "web", false)
            ]
        );
        assert_eq!(detail.session.branch.as_deref(), Some("calin/topic-abc123"));
    }
    let open_before = store.open_permissions(None).await.unwrap();
    assert_eq!(open_before.len(), 2);
    store.close().await;
    assert!(matches!(store.sessions().await, Err(Error::Closed)));

    let db = store.home().db_path();
    std::fs::remove_file(&db).unwrap();
    for suffix in ["-wal", "-shm"] {
        let mut side = db.clone().into_os_string();
        side.push(suffix);
        let _ = std::fs::remove_file(side);
    }

    let reopened = open(&dir).await;
    assert_eq!(reopened.sessions().await.unwrap(), sessions);
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(reopened.session(id).await.unwrap(), details[index]);
        assert_eq!(
            reopened.subscribe(id, 0).await.unwrap().backlog,
            logs[index]
        );
        // Replay from the middle needs the rebuilt byte offsets.
        assert_eq!(
            reopened.subscribe(id, 3).await.unwrap().backlog,
            logs[index][3..]
        );
    }
    // The permission and repo projections come back with the rest.
    assert_eq!(reopened.open_permissions(None).await.unwrap(), open_before);
    for id in &ids {
        let resolved = |request: &'static str| reopened.permission_resolved(id, request);
        assert_eq!(resolved("answered").await.unwrap(), Some(true));
        assert_eq!(resolved("open").await.unwrap(), Some(false));
        assert_eq!(resolved("never asked").await.unwrap(), None);
    }
    // `alpha` is in review and `beta` running: both still hold their repos.
    assert!(reopened.repo_in_use("repo-api").await.unwrap());
    assert!(!reopened.repo_in_use("repo-other").await.unwrap());
    // Appending continues where the log ended.
    let next = reopened.append(&ids[1], message(9)).await.unwrap();
    assert_eq!(next.seq, logs[1].len() as u64 + 1);
}

#[tokio::test]
async fn agent_sessions_repos_and_permissions_are_projected() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store
        .create_session(new_session("project"))
        .await
        .unwrap()
        .id;
    let spawned = EventBody::AgentSpawned {
        agent: agent("a1", &id),
    };
    store.append(&id, EventDraft::new(spawned)).await.unwrap();

    // The first report sets the id; one without a model keeps the model.
    let announce = |native: &str, model: Option<&str>| {
        let body = EventBody::AgentSession {
            native_session_id: native.to_owned(),
            model: model.map(str::to_owned),
        };
        store.append(&id, EventDraft::new(body).agent("a1"))
    };
    announce("n1", None).await.unwrap();
    let lead = store.session(&id).await.unwrap().agents.remove(0);
    assert_eq!(lead.native_session_id.as_deref(), Some("n1"));
    assert_eq!(lead.model.as_deref(), Some("mock-1"));
    announce("n2", Some("mock-9")).await.unwrap();
    let lead = store.session(&id).await.unwrap().agents.remove(0);
    assert_eq!(lead.native_session_id.as_deref(), Some("n2"));
    assert_eq!(lead.model.as_deref(), Some("mock-9"));

    // Repos keep their order; the branch is the first one any repo has.
    assert!(store.session(&id).await.unwrap().repos.is_empty());
    for (n, name) in ["api", "web"].into_iter().enumerate() {
        let attached = EventBody::RepoAttached {
            repo: session_repo(&id, name, n == 0),
        };
        store.append(&id, EventDraft::new(attached)).await.unwrap();
        let detail = store.session(&id).await.unwrap();
        assert_eq!(detail.repos.len(), n + 1);
        assert_eq!(detail.session.branch.is_some(), n == 1);
    }
    assert!(store.repo_in_use("repo-web").await.unwrap());

    // A request is open until it is resolved, whoever resolves it.
    assert_eq!(store.permission_resolved(&id, "p1").await.unwrap(), None);
    let asked = EventBody::PermissionRequested {
        request: permission_request("p1"),
    };
    let asked = store
        .append(&id, EventDraft::new(asked).agent("a1"))
        .await
        .unwrap();
    assert_eq!(
        store.open_permissions(Some(&id)).await.unwrap(),
        [OpenPermission {
            session_id: id.clone(),
            request_id: "p1".to_owned(),
            agent_id: Some("a1".to_owned()),
            event_id: asked.id.clone(),
        }]
    );
    assert_eq!(
        store.permission_resolved(&id, "p1").await.unwrap(),
        Some(false)
    );
    store
        .append(&id, EventDraft::new(resolved("p1")).agent("a1"))
        .await
        .unwrap();
    assert_eq!(
        store.permission_resolved(&id, "p1").await.unwrap(),
        Some(true)
    );
    assert!(store.open_permissions(None).await.unwrap().is_empty());

    // A file change projects nothing, and a closed session frees its repos.
    let changed = EventBody::FileChanged {
        session_repo_id: None,
        path: "/tmp/x".to_owned(),
        op: FileOp::Edit,
    };
    store.append(&id, EventDraft::new(changed)).await.unwrap();
    for (closed, in_use) in [
        (SessionStatus::Review, true),
        (SessionStatus::Cancelled, false),
        (SessionStatus::Running, true),
        (SessionStatus::Failed, false),
        (SessionStatus::Done, false),
    ] {
        store.append(&id, status(closed)).await.unwrap();
        assert_eq!(store.repo_in_use("repo-web").await.unwrap(), in_use);
    }
}

#[tokio::test]
async fn what_a_session_was_created_with_survives_a_reindex() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let wanted = vec![
        AttachRepoRequest {
            repo_id: "r1".to_owned(),
            mode: Some(RepoMode::Live),
        },
        AttachRepoRequest {
            repo_id: "r2".to_owned(),
            mode: None,
        },
    ];
    let configured = store
        .create_session(NewSession {
            permission_mode: PermissionMode::Autonomous,
            model: Some("opus".to_owned()),
            repos: wanted.clone(),
            ..new_session("configured")
        })
        .await
        .unwrap();
    assert_eq!(configured.permission_mode, PermissionMode::Autonomous);
    let plain = store.create_session(new_session("plain")).await.unwrap();
    let expected = SessionSetup {
        model: Some("opus".to_owned()),
        repos: wanted,
    };
    assert_eq!(store.setup(&configured.id).await.unwrap(), expected);
    assert_eq!(
        store.setup(&plain.id).await.unwrap(),
        SessionSetup::default()
    );
    assert!(matches!(
        store.setup("01ARZ3NDEKTSV4RRFFQ69G5FAV").await,
        Err(Error::NotFound(_))
    ));
    store.close().await;
    std::fs::remove_file(store.home().db_path()).unwrap();

    let reopened = open(&dir).await;
    assert_eq!(reopened.setup(&configured.id).await.unwrap(), expected);
    assert_eq!(
        reopened.session(&configured.id).await.unwrap().session,
        configured
    );
    assert_eq!(
        reopened
            .session(&plain.id)
            .await
            .unwrap()
            .session
            .permission_mode,
        PermissionMode::Ask
    );

    // A session file written before M1 has none of the new fields.
    let path = reopened.home().meta_path(&plain.id);
    let mut old: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    old.as_object_mut().unwrap().remove("permission_mode");
    std::fs::write(&path, old.to_string()).unwrap();
    reopened.close().await;
    std::fs::remove_file(reopened.home().db_path()).unwrap();
    let again = open(&dir).await;
    assert_eq!(again.session(&plain.id).await.unwrap().session, plain);
    assert_eq!(
        again.setup(&plain.id).await.unwrap(),
        SessionSetup::default()
    );
}

fn pull_request(session_repo_id: &str, number: u32, state: PullRequestState) -> PullRequest {
    PullRequest {
        session_repo_id: session_repo_id.to_owned(),
        number,
        url: format!("https://example.invalid/x/pull/{number}"),
        title: "Add the thing".to_owned(),
        state,
        checks: CheckState::None,
        review: ReviewState::None,
        updated_at: 1_000 + i64::from(number),
    }
}

#[tokio::test]
async fn pull_requests_are_projected_per_repo_and_survive_a_reindex() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let session = store.create_session(new_session("ship")).await.unwrap();
    let other = store.create_session(new_session("other")).await.unwrap();
    let id = session.id.as_str();
    let (api, web, docs) = (
        session_repo(id, "api", true),
        session_repo(id, "web", false),
        session_repo(id, "docs", false),
    );
    let elsewhere = session_repo(&other.id, "api", true);
    let append = async |session_id: &str, body: EventBody| {
        store
            .append(session_id, EventDraft::new(body))
            .await
            .unwrap();
    };
    for repo in [&api, &web, &docs] {
        append(id, EventBody::RepoAttached { repo: repo.clone() }).await;
    }
    let attached = EventBody::RepoAttached {
        repo: elsewhere.clone(),
    };
    append(&other.id, attached).await;
    assert!(store.session(id).await.unwrap().pull_requests.is_empty());

    // Opened in another order than the repos were attached in.
    let opened = |pull_request: PullRequest| EventBody::PullRequestOpened { pull_request };
    let updated = |pull_request: PullRequest| EventBody::PullRequestUpdated { pull_request };
    append(
        id,
        opened(pull_request(&docs.id, 7, PullRequestState::Draft)),
    )
    .await;
    append(
        id,
        opened(pull_request(&api.id, 3, PullRequestState::Draft)),
    )
    .await;
    append(
        &other.id,
        opened(pull_request(&elsewhere.id, 5, PullRequestState::Open)),
    )
    .await;
    // An update replaces the whole value; so does a newer pull request for the same repo.
    let approved = PullRequest {
        state: PullRequestState::Open,
        checks: CheckState::Passing,
        review: ReviewState::Approved,
        title: "Renamed on GitHub".to_owned(),
        updated_at: 9_000,
        ..pull_request(&api.id, 3, PullRequestState::Draft)
    };
    append(id, updated(approved.clone())).await;
    append(
        id,
        opened(pull_request(&docs.id, 8, PullRequestState::Open)),
    )
    .await;
    // An update for a repo that has none on record yet still counts.
    append(
        id,
        updated(pull_request(&web.id, 4, PullRequestState::Merged)),
    )
    .await;

    let expected = vec![
        approved,
        pull_request(&web.id, 4, PullRequestState::Merged),
        pull_request(&docs.id, 8, PullRequestState::Open),
    ];
    let detail = store.session(id).await.unwrap();
    assert_eq!(detail.pull_requests, expected);
    assert_eq!(
        store.session(&other.id).await.unwrap().pull_requests,
        [pull_request(&elsewhere.id, 5, PullRequestState::Open)]
    );
    // Numbers are counted per repo across sessions, among URLs that match.
    let last = async |store: &Store, repo_id: &str, prefix: &str| {
        store
            .last_pull_request_number(repo_id, prefix)
            .await
            .unwrap()
    };
    assert_eq!(
        last(&store, "repo-api", "https://example.invalid/").await,
        5
    );
    assert_eq!(
        last(&store, "repo-docs", "https://example.invalid/").await,
        8
    );
    assert_eq!(last(&store, "repo-api", "https://github.com/").await, 0);
    assert_eq!(
        last(&store, "repo-none", "https://example.invalid/").await,
        0
    );

    store.close().await;
    std::fs::remove_file(store.home().db_path()).unwrap();
    let reopened = open(&dir).await;
    assert_eq!(reopened.session(id).await.unwrap(), detail);
    assert_eq!(
        last(&reopened, "repo-api", "https://example.invalid/").await,
        5
    );
}

/// Collects the upserts of one session until none arrives for `quiet`.
async fn upserts_until_quiet(
    updates: &mut Live<Session>,
    session_id: &str,
    quiet: Duration,
) -> Vec<Session> {
    let mut seen = Vec::new();
    while let Ok(upserted) = tokio::time::timeout(quiet, updates.recv()).await {
        let session = upserted.expect("the upsert stream ended");
        if session.id == session_id {
            seen.push(session);
        }
    }
    seen
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upserts_are_throttled_while_events_arrive_and_end_on_the_newest() {
    const BURST: u64 = 300;
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("busy")).await.unwrap().id;
    let other = store.create_session(new_session("quiet")).await.unwrap().id;
    let (_, mut updates) = store.subscribe_sessions().await.unwrap();

    let started = std::time::Instant::now();
    for n in 0..BURST {
        store.append(&id, message(n)).await.unwrap();
    }
    let burst = started.elapsed();
    let seen = upserts_until_quiet(&mut updates, &id, UPSERT_INTERVAL * 2).await;
    // One per interval that the burst spanned, and the one that was owed at
    // its end: nowhere near one per event.
    let allowed = 2 + (burst.as_millis() / UPSERT_INTERVAL.as_millis()) as usize;
    assert!(
        (1..=allowed).contains(&seen.len()),
        "{} upserts for a burst of {BURST} events in {burst:?}",
        seen.len()
    );
    let last = seen.last().unwrap();
    assert_eq!(last.head_seq, BURST);
    assert_eq!(
        last.updated_at,
        store.session(&id).await.unwrap().session.updated_at
    );
    assert!(
        seen.windows(2)
            .all(|pair| pair[0].head_seq < pair[1].head_seq)
    );

    // After a quiet spell the first event is announced without delay. The
    // next one is owed, and a status change is announced at once and settles
    // it: no frame follows the status.
    store.append(&id, message(BURST)).await.unwrap();
    store.append(&id, message(BURST + 1)).await.unwrap();
    store
        .append(&id, status(SessionStatus::Running))
        .await
        .unwrap();
    let seen = upserts_until_quiet(&mut updates, &id, UPSERT_INTERVAL * 2).await;
    let heads: Vec<u64> = seen.iter().map(|session| session.head_seq).collect();
    assert_eq!(heads, [BURST + 1, BURST + 3]);
    assert_eq!(seen[1].status, SessionStatus::Running);

    // The session nothing happened to was never announced again.
    assert_eq!(store.session(&other).await.unwrap().session.head_seq, 0);
}

#[tokio::test]
async fn a_session_without_its_metadata_file_is_rebuilt_from_its_events() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let ids = populate(&store).await;
    let before = store.session(&ids[0]).await.unwrap();
    store.close().await;
    std::fs::remove_file(store.home().db_path()).unwrap();
    std::fs::remove_file(store.home().meta_path(&ids[0])).unwrap();

    let reopened = open(&dir).await;
    let after = reopened.session(&ids[0]).await.unwrap();
    assert_eq!(after.agents, before.agents);
    // The title was only in the metadata; it falls back to the prompt.
    assert_eq!(after.session.title, "message 1");
    assert_eq!(after.session.prompt, "message 1");
    assert_eq!(after.session.status, before.session.status);
    assert_eq!(after.session.head_seq, before.session.head_seq);
}

#[tokio::test]
async fn events_logged_but_not_indexed_are_caught_up_on_startup() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("crash")).await.unwrap().id;
    let last = store.append(&id, message(1)).await.unwrap();
    store.close().await;

    // A crash right after the log write: one whole line the index never saw,
    // then half of another.
    let orphan = Event {
        id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        seq: last.seq + 1,
        body: EventBody::SessionStatus {
            status: SessionStatus::Running,
            reason: None,
        },
        ..(*last).clone()
    };
    let path = store.home().log_path(&id);
    let mut log = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(log, "{}", serde_json::to_string(&orphan).unwrap()).unwrap();
    write!(log, "{{\"id\":\"torn").unwrap();
    drop(log);

    let reopened = open(&dir).await;
    let session = reopened.session(&id).await.unwrap().session;
    assert_eq!(session.head_seq, 2);
    assert_eq!(session.status, SessionStatus::Running);
    let next = reopened.append(&id, message(2)).await.unwrap();
    assert_eq!(next.seq, 3);
    let log = read_log(&reopened, &id);
    assert_eq!(log.len(), 3);
    assert_eq!(log[1], orphan);
    assert_eq!(reopened.subscribe(&id, 1).await.unwrap().backlog, log[1..]);
}

#[tokio::test]
async fn a_session_deleted_from_disk_leaves_the_index() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let ids = populate(&store).await;
    store.close().await;
    std::fs::remove_dir_all(store.home().session_dir(&ids[0])).unwrap();

    let reopened = open(&dir).await;
    let sessions = reopened.sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, ids[1]);
}

#[tokio::test]
async fn a_data_directory_belongs_to_one_store_at_a_time() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let second = Store::open(Home::new(dir.path())).await;
    assert!(matches!(second.map(|_| ()), Err(Error::InUse(_))));
    // The first store is unharmed, and closing it frees the directory.
    store.create_session(new_session("mine")).await.unwrap();
    store.close().await;
    assert_eq!(open(&dir).await.sessions().await.unwrap().len(), 1);
}
