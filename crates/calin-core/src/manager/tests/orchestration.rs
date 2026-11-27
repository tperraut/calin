//! Agents working together: subagents inside one process, child sessions
//! dispatched through Calin's tools, the notes and reports between them and
//! the questions an agent puts to the user.

use calin_agents::{PermissionAsk, SubagentStart, ToolAccess};
use calin_proto::{Agent, AgentMessage, EdgeKind, MessageKind, Question};

use super::*;
use crate::manager::{Dispatched, DispatchedRepo, NoteOutcome};
use crate::tokens::AgentRef;

const TOOLS: &str = "http://127.0.0.1:4680/api/v1/mcp";

/// One session on the puppet runner: who its agent is and the agent's end.
struct Played {
    id: String,
    agent: AgentRef,
    params: StartParams,
    io: AgentIo,
}

/// A root session ("start") with `api` attached for writing, its agent
/// started and its first turn still running; children are dispatched from it.
struct Tree {
    manager: SessionManager,
    starts: Starts,
    _dir: TempDir,
    clones: TempDir,
    root: Played,
}

/// Takes the agent the puppet runner was just asked to start.
async fn play(starts: &mut Starts) -> Played {
    let (params, io) = within(starts.recv()).await.unwrap();
    Played {
        id: params.session_id.clone(),
        agent: AgentRef {
            session_id: params.session_id.clone(),
            agent_id: params.agent_id.clone(),
        },
        params,
        io,
    }
}

async fn tree_in(mode: PermissionMode) -> Tree {
    let Fixture {
        manager,
        mut starts,
        dir,
    } = fixture().await;
    manager.set_tool_endpoint(TOOLS);
    let clones = TempDir::new().unwrap();
    let api = registered(&manager, clones.path(), "api").await;
    let mut wanted = request_in(mode, "start", Some("puppet"));
    wanted.repos = vec![attach(&api, None)];
    manager.create_session(wanted).await.unwrap();
    let root = play(&mut starts).await;
    Tree {
        manager,
        starts,
        _dir: dir,
        clones,
        root,
    }
}

async fn tree() -> Tree {
    tree_in(PermissionMode::Autonomous).await
}

impl Tree {
    async fn dispatch_with(
        &mut self,
        title: &str,
        brief: &str,
        repos: Option<&[&str]>,
    ) -> (Dispatched, Played) {
        let repos = repos.map(|names| names.iter().map(|name| (*name).to_owned()).collect());
        let dispatched = self
            .manager
            .agent_dispatch(&self.root.agent, title, brief, repos);
        let dispatched = within(dispatched).await.unwrap();
        let child = play(&mut self.starts).await;
        assert_eq!(child.id, dispatched.session.id);
        (dispatched, child)
    }

    /// Dispatches a child of the root with every `write` repo and takes its agent.
    async fn dispatch(&mut self, title: &str) -> Played {
        self.dispatch_with(title, "Do the thing.", None).await.1
    }
}

async fn all_events(manager: &SessionManager, id: &str) -> Vec<Event> {
    manager.store().subscribe(id, 0).await.unwrap().backlog
}

async fn head(manager: &SessionManager, id: &str) -> u64 {
    manager.get(id).await.unwrap().session.head_seq
}

async fn status(manager: &SessionManager, id: &str) -> SessionStatus {
    manager.get(id).await.unwrap().session.status
}

/// The session's events after `after`, up to the first with this label.
async fn until_label(manager: &SessionManager, id: &str, after: u64, wanted: &str) -> Vec<Event> {
    events_until(manager, id, after, |event| label(event) == wanted).await
}

async fn end_turn(manager: &SessionManager, played: &Played) {
    let after = head(manager, &played.id).await;
    emit(&played.io, RunnerOutput::TurnEnded).await;
    until_label(manager, &played.id, after, "agent_status:idle").await;
}

/// What the agent is handed next.
async fn input(played: &mut Played) -> String {
    match within(played.io.inputs.recv()).await {
        Some(RunnerInput::UserMessage(text)) => text,
        other => panic!("expected a message for the agent, got {other:?}"),
    }
}

fn nothing_for(played: &mut Played) {
    assert!(
        played.io.inputs.try_recv().is_err(),
        "the agent of {} was handed something",
        played.id
    );
}

/// Obeys the stop request the agent is about to get.
async fn exit_on_stop(played: &mut Played) {
    assert_eq!(
        within(played.io.inputs.recv()).await,
        Some(RunnerInput::Stop)
    );
    emit(&played.io, RunnerOutput::Exited(Exit::Ok)).await;
}

fn sent(events: &[Event]) -> Vec<(&Event, &AgentMessage)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::MessageSent { message } => Some((event, message)),
            _ => None,
        })
        .collect()
}

fn received(events: &[Event]) -> Vec<(&Event, &AgentMessage)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::MessageReceived { message } => Some((event, message)),
            _ => None,
        })
        .collect()
}

async fn repo_path(manager: &SessionManager, id: &str, name: &str) -> PathBuf {
    let repos = manager.get(id).await.unwrap().repos;
    let repo = repos.iter().find(|repo| repo.name == name);
    PathBuf::from(
        &repo
            .unwrap_or_else(|| panic!("{id} has no repo {name}"))
            .path,
    )
}

fn commit(dir: &Path, file: &str, message: &str) -> String {
    write_file(dir, file, "content\n");
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

fn text_item(id: &str, text: &str) -> Item {
    Item {
        id: id.to_owned(),
        status: ItemStatus::Completed,
        content: ItemContent::Text {
            text: text.to_owned(),
        },
    }
}

/// The agent says `text`, as one finished item.
async fn say(played: &Played, id: &str, text: &str) {
    let running = EventBody::ItemStarted { item: item(id) };
    emit(&played.io, RunnerOutput::Event(running)).await;
    let done = EventBody::ItemCompleted {
        item: text_item(id, text),
    };
    emit(&played.io, RunnerOutput::Event(done)).await;
}

fn by_subagent(key: &str, body: EventBody) -> RunnerOutput {
    RunnerOutput::SubagentEvent {
        key: key.to_owned(),
        body,
    }
}

fn subagent(key: &str, parent: Option<&str>, role: &str, item_id: &str) -> RunnerOutput {
    RunnerOutput::SubagentStarted(SubagentStart {
        key: key.to_owned(),
        parent: parent.map(str::to_owned),
        role: role.to_owned(),
        description: format!("work of {key}"),
        item_id: Some(item_id.to_owned()),
    })
}

fn event_of_item<'a>(events: &'a [Event], item_id: &str, started: bool) -> &'a Event {
    let found = events.iter().find(|event| match &event.body {
        EventBody::ItemStarted { item } => started && item.id == item_id,
        EventBody::ItemCompleted { item } => !started && item.id == item_id,
        _ => false,
    });
    found.unwrap_or_else(|| panic!("no event of item {item_id}"))
}

#[tokio::test]
async fn subagents_are_agents_of_their_own_and_their_events_are_theirs() {
    let Tree {
        manager,
        root,
        _dir,
        clones: _clones,
        ..
    } = tree().await;
    let mut root = root;
    let io = &root.io;
    let lead = root.agent.agent_id.clone();
    let work = repo_path(&manager, &root.id, "api").await;
    let edited = work.join("src/x.rs").to_str().unwrap().to_owned();

    let task = |id: &str, status| tool(id, ToolKind::Task, status, &[]);
    let started = |item| EventBody::ItemStarted { item };
    let completed = |item| EventBody::ItemCompleted { item };
    emit(
        io,
        RunnerOutput::Event(started(task("task-1", ItemStatus::Running))),
    )
    .await;
    emit(io, subagent("k1", None, "explorer", "task-1")).await;
    let edit = |status| tool("e1", ToolKind::Edit, status, &[&edited]);
    emit(io, by_subagent("k1", started(edit(ItemStatus::Running)))).await;
    emit(
        io,
        by_subagent("k1", completed(edit(ItemStatus::Completed))),
    )
    .await;
    // The subagent asks; in `autonomous` mode the policy answers at once.
    let request = PermissionRequest {
        id: "p1".to_owned(),
        tool_name: "Bash".to_owned(),
        tool_kind: ToolKind::Execute,
        title: "ls".to_owned(),
        input: serde_json::json!({ "command": "ls" }),
        item_id: None,
    };
    let asked = PermissionAsk {
        request,
        access: ToolAccess::Other,
        subagent: Some("k1".to_owned()),
    };
    emit(io, RunnerOutput::PermissionRequest(asked)).await;
    assert!(matches!(
        within(root.io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision { request_id, decision: PermissionDecision::Allow, .. })
            if request_id == "p1"
    ));
    let io = &root.io;
    // It starts a subagent of its own, which fails.
    emit(
        io,
        by_subagent("k1", started(task("task-2", ItemStatus::Running))),
    )
    .await;
    emit(io, subagent("k2", Some("k1"), "reviewer", "task-2")).await;
    emit(io, by_subagent("k2", started(item("t2")))).await;
    emit(io, by_subagent("k2", completed(text_item("t2", "nested")))).await;
    emit(
        io,
        RunnerOutput::SubagentEnded {
            key: "k2".to_owned(),
            ok: false,
        },
    )
    .await;
    emit(
        io,
        by_subagent("k1", completed(task("task-2", ItemStatus::Failed))),
    )
    .await;
    emit(
        io,
        RunnerOutput::SubagentEnded {
            key: "k1".to_owned(),
            ok: true,
        },
    )
    .await;
    emit(
        io,
        RunnerOutput::Event(completed(task("task-1", ItemStatus::Completed))),
    )
    .await;
    // Output that names a subagent that is not running is the lead's.
    emit(io, by_subagent("k1", started(item("late")))).await;
    emit(
        io,
        by_subagent("nobody", completed(text_item("late", "late"))),
    )
    .await;
    let usage = EventBody::Usage {
        cost_usd: 0.5,
        tokens_in: 100,
        tokens_out: 10,
    };
    emit(io, RunnerOutput::Event(usage)).await;
    emit(io, RunnerOutput::TurnEnded).await;
    let events = events_until(&manager, &root.id, 0, is_status(SessionStatus::Review)).await;

    let spawns: Vec<(&Event, &Agent)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::AgentSpawned { agent } => Some((event, agent)),
            _ => None,
        })
        .collect();
    assert_eq!(spawns.len(), 3);
    let (first_event, first) = spawns[1];
    let (second_event, second) = spawns[2];
    assert_eq!(first.kind, AgentKind::Subagent);
    assert_eq!(first.parent_agent_id.as_ref(), Some(&lead));
    assert_eq!(
        (first.role.as_str(), first.runner.as_str()),
        ("explorer", "puppet")
    );
    assert_eq!(
        (first.status, first.model.as_ref()),
        (AgentStatus::Running, None)
    );
    assert_eq!(first.session_id, root.id);
    // Spawned by the tool call that started it; recorded by the orchestrator.
    let call = event_of_item(&events, "task-1", true);
    assert_eq!(first_event.parent_id.as_ref(), Some(&call.id));
    assert_eq!(first_event.agent_id, None);
    assert_eq!(call.agent_id.as_ref(), Some(&lead));
    // A subagent's subagent hangs under it.
    assert_eq!(second.kind, AgentKind::Subagent);
    assert_eq!(second.parent_agent_id.as_ref(), Some(&first.id));
    assert_eq!(second.role, "reviewer");
    let nested_call = event_of_item(&events, "task-2", true);
    assert_eq!(second_event.parent_id.as_ref(), Some(&nested_call.id));
    assert_eq!(nested_call.agent_id.as_ref(), Some(&first.id));

    let author = |event: &Event| event.agent_id.clone().unwrap();
    for item in ["e1", "task-2"] {
        assert_eq!(
            author(event_of_item(&events, item, true)),
            first.id,
            "{item}"
        );
        assert_eq!(
            author(event_of_item(&events, item, false)),
            first.id,
            "{item}"
        );
    }
    assert_eq!(author(event_of_item(&events, "t2", true)), second.id);
    assert_eq!(author(event_of_item(&events, "t2", false)), second.id);
    assert_eq!(author(event_of_item(&events, "task-1", false)), lead);
    assert_eq!(author(event_of_item(&events, "late", true)), lead);
    assert_eq!(author(event_of_item(&events, "late", false)), lead);
    for event in &events {
        match &event.body {
            // The file is the subagent's change, caused by its call.
            EventBody::FileChanged {
                session_repo_id,
                path,
                ..
            } => {
                assert_eq!(author(event), first.id);
                assert!(session_repo_id.is_some());
                assert_eq!(path, "src/x.rs");
                let call = event_of_item(&events, "e1", false);
                assert_eq!(event.parent_id.as_ref(), Some(&call.id));
            }
            EventBody::PermissionRequested { .. } | EventBody::PermissionResolved { .. } => {
                assert_eq!(author(event), first.id);
            }
            // The totals are the process's: they stay on the lead.
            EventBody::Usage { .. } => assert_eq!(author(event), lead),
            _ => {}
        }
    }
    let ends: Vec<(String, AgentStatus)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::AgentStatus { status } => Some((author(event), *status)),
            _ => None,
        })
        .collect();
    assert_eq!(
        ends,
        [
            (second.id.clone(), AgentStatus::Failed),
            (first.id.clone(), AgentStatus::Done),
            (lead.clone(), AgentStatus::Idle),
        ]
    );

    let agents = manager.get(&root.id).await.unwrap().agents;
    let seen: Vec<(&str, AgentStatus, u64)> = agents
        .iter()
        .map(|agent| (agent.id.as_str(), agent.status, agent.tokens_in))
        .collect();
    assert_eq!(
        seen,
        [
            (lead.as_str(), AgentStatus::Idle, 100),
            (first.id.as_str(), AgentStatus::Done, 0),
            (second.id.as_str(), AgentStatus::Failed, 0),
        ]
    );

    // And so the graph and the timeline have them.
    let graph = manager.graph(&root.id, None).await.unwrap();
    let ids: Vec<&str> = graph.edges.iter().map(|edge| edge.id.as_str()).collect();
    assert!(ids.contains(&format!("spawned:agent:{lead}:agent:{}", first.id).as_str()));
    assert!(ids.contains(&format!("spawned:agent:{}:agent:{}", first.id, second.id).as_str()));
    assert!(ids.iter().any(|id| *id
        == format!(
            "changed:agent:{}:repo:{}",
            first.id,
            graph.nodes.last().unwrap().repo_id.as_ref().unwrap()
        )));
    let node = |id: &str| {
        graph
            .nodes
            .iter()
            .find(|node| node.id == format!("agent:{id}"))
            .unwrap()
    };
    // Named by the call that started it.
    assert_eq!(node(&first.id).label, "task-1");
    assert_eq!(
        node(&second.id).parent_id,
        Some(format!("agent:{}", first.id))
    );
    let timeline = manager.timeline(&root.id).await.unwrap();
    let lanes: Vec<&str> = timeline
        .lanes
        .iter()
        .map(|lane| lane.agent.id.as_str())
        .collect();
    assert_eq!(
        lanes,
        [lead.as_str(), first.id.as_str(), second.id.as_str()]
    );
    assert!(
        timeline
            .lanes
            .iter()
            .all(|lane| lane.spans.iter().all(|span| span.end.is_some()))
    );
    let kinds: Vec<EdgeKind> = timeline.arrows.iter().map(|arrow| arrow.kind).collect();
    assert_eq!(kinds, [EdgeKind::Spawned, EdgeKind::Spawned]);
}

#[tokio::test]
async fn subagents_still_running_end_with_their_process() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let started = EventBody::ItemStarted {
        item: tool("task-1", ToolKind::Task, ItemStatus::Running, &[]),
    };
    emit(&tree.root.io, RunnerOutput::Event(started)).await;
    emit(&tree.root.io, subagent("k1", None, "explorer", "task-1")).await;
    until_label(&manager, &tree.root.id, 0, "item_started").await;

    let stopping = tokio::spawn({
        let (manager, id) = (manager.clone(), tree.root.id.clone());
        async move { manager.stop(&id).await }
    });
    exit_on_stop(&mut tree.root).await;
    within(stopping).await.unwrap().unwrap();
    let agents = manager.get(&tree.root.id).await.unwrap().agents;
    assert_eq!(agents.len(), 2);
    assert_eq!(agents[1].kind, AgentKind::Subagent);
    assert_eq!(agents[1].status, AgentStatus::Stopped);
    let events = all_events(&manager, &tree.root.id).await;
    // The subagent first, then the agent whose process it lived in.
    assert_eq!(
        labels(&events)[events.len() - 3..],
        [
            "agent_status:stopped",
            "agent_status:stopped",
            "session_status:cancelled"
        ]
    );
    assert_eq!(
        events[events.len() - 3].agent_id.as_ref(),
        Some(&agents[1].id)
    );
}

#[tokio::test]
async fn a_child_starts_on_its_own_branch_at_the_parents_commit() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root_id = tree.root.id.clone();
    let lead = tree.root.agent.agent_id.clone();
    let work = repo_path(&manager, &root_id, "api").await;
    let commit = commit(&work, "feature.txt", "feature");
    // Work the parent has not committed stays with the parent.
    write_file(&work, "scratch.txt", "not committed\n");
    let before = head(&manager, &root_id).await;

    let brief = "Document the feature.\nBe brief.";
    let (dispatched, child) = tree.dispatch_with("Write the docs", brief, None).await;
    assert_eq!(
        dispatched.repos,
        [DispatchedRepo {
            name: "api".to_owned(),
            commit: commit.clone(),
            dirty: true,
        }]
    );
    // An ordinary session, with the parent's runner and permission mode.
    let session = &dispatched.session;
    assert_eq!(session.parent_session_id.as_ref(), Some(&root_id));
    assert_eq!(session.root_session_id, root_id);
    assert_eq!(
        (session.title.as_str(), session.prompt.as_str()),
        ("Write the docs", brief)
    );
    assert_eq!(session.runner, "puppet");
    assert_eq!(session.permission_mode, PermissionMode::Autonomous);
    let listed = manager.list().await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, [child.id.as_str(), root_id.as_str()]);

    // The brief is a message from the parent's agent to the child's.
    let parent_events = all_events(&manager, &root_id).await;
    let sent_events = sent(&parent_events);
    assert_eq!(sent_events.len(), 1);
    let (sent_event, message) = sent_events[0];
    assert_eq!(sent_event.seq, before + 1);
    assert_eq!(sent_event.agent_id.as_ref(), Some(&lead));
    assert_eq!(message.kind, MessageKind::Dispatch);
    assert_eq!(message.body, brief);
    assert_eq!(message.from_session_id, root_id);
    assert_eq!(message.from_agent_id.as_ref(), Some(&lead));
    assert_eq!(message.to_session_id, child.id);
    assert_eq!(message.to_agent_id.as_ref(), Some(&child.agent.agent_id));
    assert_eq!(message.in_reply_to, None);
    assert!(received(&parent_events).is_empty());

    let events = until_label(&manager, &child.id, 0, "agent_spawned").await;
    assert_eq!(
        labels(&events),
        [
            "session_status:queued",
            "message_received:dispatch",
            "session_status:preparing",
            "repo_attached:api",
            "session_status:running",
            "agent_spawned",
        ]
    );
    let (received_event, got) = received(&events)[0];
    assert_eq!(got, message);
    assert_eq!(received_event.parent_id.as_ref(), Some(&sent_event.id));
    assert_eq!(received_event.agent_id, None);
    // The brief is what caused the child's turn.
    assert_eq!(events[5].parent_id.as_ref(), Some(&received_event.id));
    let EventBody::AgentSpawned { agent } = &events[5].body else {
        unreachable!()
    };
    assert_eq!(agent.id, child.agent.agent_id);
    assert_eq!(agent.role, "lead");

    // What its agent is handed says where it comes from.
    assert!(
        child
            .params
            .prompt
            .starts_with("[Calin] Brief from the session that dispatched you (\"start\").")
    );
    assert!(child.params.prompt.ends_with(brief));
    let system = child.params.system_prompt.as_deref().unwrap();
    assert!(system.contains("You are a child session"));
    assert!(system.contains("\"start\""));
    assert!(system.contains("call the `report` tool"));
    assert!(!system.contains("`dispatch` starts a child session"));
    let root_system = tree.root.params.system_prompt.as_deref().unwrap();
    assert!(root_system.contains("`dispatch` starts a child session"));
    assert!(!root_system.contains("You are a child session"));
    assert_eq!(child.params.permission_mode, PermissionMode::Autonomous);

    // A worktree of its own, on a branch of its own, from the parent's commit.
    let root_repo = manager.get(&root_id).await.unwrap().repos.remove(0);
    let repo = manager.get(&child.id).await.unwrap().repos.remove(0);
    assert_eq!(repo.mode, RepoMode::Write);
    assert_eq!(repo.repo_id, root_repo.repo_id);
    assert_eq!(repo.base_ref.as_ref(), Some(&commit));
    assert_ne!(repo.base_ref, root_repo.base_ref);
    assert_ne!(repo.branch, root_repo.branch);
    assert_ne!(repo.path, root_repo.path);
    let branch = repo.branch.clone().unwrap();
    assert!(branch.contains("write-the-docs"), "{branch}");
    let dir = PathBuf::from(&repo.path);
    assert_eq!(child.params.cwd, dir);
    assert_eq!(git(&dir, &["rev-parse", "HEAD"]), commit);
    assert_eq!(git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]), branch);
    assert!(dir.join("feature.txt").is_file());
    assert!(!dir.join("scratch.txt").exists());
    // Measured against where it started: nothing yet.
    let changes = manager.changes(&child.id).await.unwrap();
    assert!(changes[0].files.is_empty());
    // The parent's own work was not touched.
    assert_eq!(git(&work, &["rev-parse", "HEAD"]), commit);
    assert!(work.join("scratch.txt").is_file());

    // A clean worktree is not called dirty.
    std::fs::remove_file(work.join("scratch.txt")).unwrap();
    let (again, _second) = tree.dispatch_with("Second", "More.", None).await;
    assert!(!again.repos[0].dirty);
}

#[tokio::test]
async fn dispatching_is_limited_and_validated() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root = tree.root.agent.clone();
    let web = registered(&manager, tree.clones.path(), "web").await;
    let docs = registered(&manager, tree.clones.path(), "docs").await;
    let attached = manager.attach_repo(&tree.root.id, attach(&web, None));
    within(attached).await.unwrap();
    let attached = manager.attach_repo(&tree.root.id, attach(&docs, Some(RepoMode::Read)));
    within(attached).await.unwrap();

    let dispatch = |title: &'static str, brief: &'static str, repos: Option<Vec<&str>>| {
        let repos = repos.map(|names| names.into_iter().map(str::to_owned).collect());
        let (manager, root) = (manager.clone(), root.clone());
        async move { within(manager.agent_dispatch(&root, title, brief, repos)).await }
    };
    let refusal = |result: Result<Dispatched>| result.unwrap_err().to_string();

    // What is asked for must make sense before anything is created.
    assert!(matches!(
        dispatch(" ", "brief", None).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        dispatch("two\nlines", "brief", None).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        dispatch("title", " \n", None).await,
        Err(Error::Invalid(_))
    ));
    let unknown = dispatch("title", "brief", Some(vec!["api", "nope"])).await;
    assert!(matches!(unknown, Err(Error::Missing(_))));
    let said = refusal(unknown);
    assert!(said.contains("no repository named nope"), "{said}");
    assert!(said.contains("api, web and docs"), "{said}");
    let read_only = dispatch("title", "brief", Some(vec!["docs"])).await;
    assert!(matches!(read_only, Err(Error::Invalid(_))));
    assert!(refusal(read_only).contains("docs is attached to this session in read mode"));
    assert_eq!(manager.list().await.unwrap().len(), 1);
    assert!(sent(&all_events(&manager, &tree.root.id).await).is_empty());

    // A subset, every `write` repo by default, or none at all.
    let names = |dispatched: &Dispatched| -> Vec<String> {
        dispatched
            .repos
            .iter()
            .map(|repo| repo.name.clone())
            .collect()
    };
    let (only_web, first) = tree
        .dispatch_with("one", "brief", Some(&["web", "web"]))
        .await;
    assert_eq!(names(&only_web), ["web"]);
    let (both, second) = tree.dispatch_with("two", "brief", None).await;
    assert_eq!(names(&both), ["api", "web"]);
    let (none, third) = tree.dispatch_with("three", "brief", Some(&[])).await;
    assert!(none.repos.is_empty());
    assert!(third.params.cwd.ends_with("work"));
    assert!(manager.get(&third.id).await.unwrap().repos.is_empty());
    until_label(&manager, &first.id, 0, "agent_spawned").await;
    let repos = manager.get(&first.id).await.unwrap().repos;
    assert_eq!(repos.len(), 1);
    assert!(repos[0].primary && repos[0].name == "web");

    // A child cannot dispatch.
    let nested = manager.agent_dispatch(&first.agent, "nested", "brief", None);
    let nested = within(nested).await;
    assert!(matches!(nested, Err(Error::Conflict(_))));
    assert!(refusal(nested).contains("a child session cannot dispatch"));

    // Four at a time.
    let fourth = tree.dispatch("four").await;
    let fifth = dispatch("five", "brief", None).await;
    assert!(matches!(fifth, Err(Error::Conflict(_))));
    let said = refusal(fifth);
    assert!(
        said.contains("4 child sessions of this session are still running"),
        "{said}"
    );
    assert_eq!(manager.list().await.unwrap().len(), 5);
    // One of them comes to rest: there is room again.
    end_turn(&manager, &second).await;
    until_label(&manager, &second.id, 0, "session_status:review").await;
    let fifth = tree.dispatch("five").await;
    assert_eq!(manager.list().await.unwrap().len(), 6);
    drop((first, fourth, fifth));
}

#[tokio::test]
async fn a_child_reports_each_time_it_comes_to_rest_and_the_parent_takes_a_turn_on_it() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root_id = tree.root.id.clone();
    let lead = tree.root.agent.agent_id.clone();
    let mut child = tree.dispatch("Write the docs").await;
    let mut root = tree.root;
    end_turn(&manager, &root).await;
    until_label(&manager, &root_id, 0, "session_status:review").await;
    let dispatch_id = sent(&all_events(&manager, &root_id).await)[0].1.id.clone();

    // The child commits its work, says what it did, and ends its turn.
    until_label(&manager, &child.id, 0, "agent_spawned").await;
    let dir = repo_path(&manager, &child.id, "api").await;
    commit(&dir, "docs.md", "docs");
    let branch = git(&dir, &["rev-parse", "--abbrev-ref", "HEAD"]);
    say(&child, "t1", "All done here.").await;
    // Only a child has somebody to report to.
    let alone = manager.agent_report(&root.agent, "to whom?").await;
    assert!(matches!(alone, Err(Error::Conflict(_))));
    assert!(matches!(
        manager.agent_report(&child.agent, "  ").await,
        Err(Error::Invalid(_))
    ));
    manager
        .agent_report(&child.agent, "Added the docs page.\nTests pass.")
        .await
        .unwrap();
    let root_before = head(&manager, &root_id).await;
    let child_before = head(&manager, &child.id).await;
    emit(&child.io, RunnerOutput::TurnEnded).await;

    let handed = input(&mut root).await;
    assert!(handed.starts_with(
        "[Calin] Report from child session \"Write the docs\" (review)\n\n\
         Added the docs page.\nTests pass.\n"
    ));
    let merge = format!(
        "- api: branch `{branch}`, 1 commit ahead of its base. Bring it in with \
         `git merge {branch}` from your worktree of api.\n"
    );
    assert!(handed.ends_with(&merge), "{handed}");

    let child_events = until_label(&manager, &child.id, child_before, "message_sent:report").await;
    assert_eq!(
        labels(&child_events),
        [
            "agent_status:idle",
            "session_status:review",
            "message_sent:report"
        ]
    );
    let (sent_event, report) = sent(&child_events)[0];
    assert_eq!(sent_event.agent_id.as_ref(), Some(&child.agent.agent_id));
    assert_eq!(report.kind, MessageKind::Report);
    assert_eq!(report.body, "Added the docs page.\nTests pass.");
    assert_eq!(report.in_reply_to.as_ref(), Some(&dispatch_id));
    assert_eq!(report.from_session_id, child.id);
    assert_eq!(report.from_agent_id.as_ref(), Some(&child.agent.agent_id));
    assert_eq!(
        (&report.to_session_id, report.to_agent_id.as_ref()),
        (&root_id, Some(&lead))
    );

    // The parent was idle in review: the report starts its next turn, and
    // no user message is made up for it.
    let root_events = until_label(&manager, &root_id, root_before, "agent_status:running").await;
    assert_eq!(
        labels(&root_events),
        [
            "message_received:report",
            "session_status:running",
            "agent_status:running"
        ]
    );
    let (received_event, got) = received(&root_events)[0];
    assert_eq!(got, report);
    assert_eq!(received_event.parent_id.as_ref(), Some(&sent_event.id));
    assert_eq!(root_events[2].parent_id.as_ref(), Some(&received_event.id));
    assert_eq!(status(&manager, &root_id).await, SessionStatus::Running);
    end_turn(&manager, &root).await;

    // The child goes on, and reports again when it rests again: this time
    // with its last words, since it called no `report`.
    let root_before = head(&manager, &root_id).await;
    manager
        .send_message(&child.id, "one more thing")
        .await
        .unwrap();
    assert_eq!(input(&mut child).await, "one more thing");
    commit(&dir, "more.md", "more");
    say(&child, "t2", "Second pass done.").await;
    emit(&child.io, RunnerOutput::TurnEnded).await;
    let handed = input(&mut root).await;
    assert!(handed.starts_with(
        "[Calin] Report from child session \"Write the docs\" (review)\n\nSecond pass done.\n"
    ));
    assert!(handed.contains("2 commits ahead of its base"));
    let root_events = until_label(&manager, &root_id, root_before, "agent_status:running").await;
    let reports = received(&root_events);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].1.body, "Second pass done.");
    assert_ne!(reports[0].1.id, report.id);
    end_turn(&manager, &root).await;

    // The graph and the timeline tell the same story.
    let graph = manager.graph(&child.id, None).await.unwrap();
    assert_eq!(graph.root_session_id, root_id);
    let child_lead = &child.agent.agent_id;
    let edge = |id: String| {
        graph
            .edges
            .iter()
            .find(|edge| edge.id == id)
            .map(|edge| edge.count)
    };
    assert_eq!(
        edge(format!("dispatched:agent:{lead}:agent:{child_lead}")),
        Some(1)
    );
    assert_eq!(
        edge(format!("report:agent:{child_lead}:agent:{lead}")),
        Some(2)
    );
    let node = graph
        .nodes
        .iter()
        .find(|node| node.id == format!("agent:{child_lead}"));
    let node = node.unwrap();
    assert_eq!(node.parent_id, Some(format!("agent:{lead}")));
    assert_eq!(node.session_status, Some(SessionStatus::Review));
    assert_eq!(node.label, "Write the docs");
    assert_eq!(
        graph
            .nodes
            .iter()
            .filter(|node| node.repo_id.is_some())
            .count(),
        1
    );
    let timeline = manager.timeline(&root_id).await.unwrap();
    let lanes: Vec<&str> = timeline
        .lanes
        .iter()
        .map(|lane| lane.agent.id.as_str())
        .collect();
    assert_eq!(lanes, [lead.as_str(), child_lead.as_str()]);
    let kinds: Vec<EdgeKind> = timeline.arrows.iter().map(|arrow| arrow.kind).collect();
    assert_eq!(
        kinds,
        [EdgeKind::Dispatched, EdgeKind::Report, EdgeKind::Report]
    );
    // Three turns of the parent: the prompt and the two reports.
    let turns: Vec<&str> = timeline.lanes[0]
        .spans
        .iter()
        .map(|span| span.title.as_str())
        .collect();
    assert_eq!(
        turns,
        ["start", "Added the docs page.", "Second pass done."]
    );
}

#[tokio::test]
async fn a_failed_child_reports_and_a_busy_parent_gets_it_after_its_turn() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root_id = tree.root.id.clone();
    let child = tree.dispatch("Risky").await;
    let mut root = tree.root;
    until_label(&manager, &child.id, 0, "agent_spawned").await;
    let root_before = head(&manager, &root_id).await;

    say(&child, "t1", "Half of it works.").await;
    let died = RunnerOutput::Exited(Exit::Error("the agent crashed".to_owned()));
    emit(&child.io, died).await;
    let child_events = until_label(&manager, &child.id, 0, "message_sent:report").await;
    assert_eq!(
        labels(&child_events)[child_events.len() - 4..],
        [
            "error:the agent crashed",
            "agent_status:failed",
            "session_status:failed",
            "message_sent:report",
        ]
    );
    let body = &sent(&child_events)[0].1.body;
    assert_eq!(
        body,
        "Half of it works.\n\nThe session failed: the agent crashed"
    );

    // The parent is in the middle of its turn: the report is on record,
    // and waits.
    let root_events = until_label(&manager, &root_id, root_before, "message_received:report").await;
    assert_eq!(labels(&root_events), ["message_received:report"]);
    nothing_for(&mut root);
    assert_eq!(status(&manager, &root_id).await, SessionStatus::Running);

    let root_before = head(&manager, &root_id).await;
    emit(&root.io, RunnerOutput::TurnEnded).await;
    let handed = input(&mut root).await;
    assert!(handed.starts_with("[Calin] Report from child session \"Risky\" (failed)\n\n"));
    assert!(handed.contains("The session failed: the agent crashed"));
    assert!(handed.contains("no commits ahead of its base, so there is nothing to merge"));
    // One turn ended and the next began, without the session leaving `running`.
    let root_events = until_label(&manager, &root_id, root_before, "agent_status:running").await;
    assert_eq!(
        labels(&root_events),
        ["agent_status:idle", "agent_status:running"]
    );
    let everything = all_events(&manager, &root_id).await;
    let cause = &received(&everything)[0].0.id;
    assert_eq!(root_events[1].parent_id.as_ref(), Some(cause));
}

#[tokio::test]
async fn a_report_for_a_parent_that_is_over_is_recorded_and_starts_nothing() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root_id = tree.root.id.clone();
    let child = tree.dispatch("Late").await;
    until_label(&manager, &child.id, 0, "agent_spawned").await;
    // The parent's agent dies. Its child is not stopped by that.
    let died = RunnerOutput::Exited(Exit::Error("gone".to_owned()));
    emit(&tree.root.io, died).await;
    until_label(&manager, &root_id, 0, "session_status:failed").await;
    assert_eq!(status(&manager, &child.id).await, SessionStatus::Running);

    let root_before = head(&manager, &root_id).await;
    say(&child, "t1", "Finished anyway.").await;
    emit(&child.io, RunnerOutput::TurnEnded).await;
    let root_events = until_label(&manager, &root_id, root_before, "message_received:report").await;
    assert_eq!(labels(&root_events), ["message_received:report"]);
    assert_eq!(received(&root_events)[0].1.body, "Finished anyway.");
    // Nothing follows: no turn, no agent.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(head(&manager, &root_id).await, root_before + 1);
    assert_eq!(status(&manager, &root_id).await, SessionStatus::Failed);
    assert!(tree.starts.try_recv().is_err());
}

#[tokio::test]
async fn notes_go_to_the_parent_to_a_child_and_to_the_user() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root_id = tree.root.id.clone();
    let lead = tree.root.agent.clone();
    let mut child = tree.dispatch("Write the docs").await;
    let mut other = tree.dispatch("Other").await;
    let mut root = tree.root;
    until_label(&manager, &child.id, 0, "agent_spawned").await;
    until_label(&manager, &other.id, 0, "agent_spawned").await;
    let note = |from: &AgentRef, to: &str, text: &'static str| {
        let (manager, from, to) = (manager.clone(), from.clone(), to.to_owned());
        async move { within(manager.agent_send_message(&from, &to, text)).await }
    };

    // Down: to a child that is busy, so it waits for the child's turn to end.
    let before = head(&manager, &child.id).await;
    let down = note(&lead, &child.id, "Use tabs.\nNot spaces.").await;
    assert_eq!(down.unwrap(), NoteOutcome::Delivered);
    let got = until_label(&manager, &child.id, before, "message_received:note").await;
    let (received_event, message) = received(&got)[0];
    assert_eq!(message.kind, MessageKind::Note);
    assert_eq!(message.body, "Use tabs.\nNot spaces.");
    assert_eq!(message.from_agent_id.as_ref(), Some(&lead.agent_id));
    assert_eq!(message.to_agent_id.as_ref(), Some(&child.agent.agent_id));
    let root_events = all_events(&manager, &root_id).await;
    let (sent_event, sent_message) = *sent(&root_events).last().unwrap();
    assert_eq!(sent_message, message);
    assert_eq!(sent_event.agent_id.as_ref(), Some(&lead.agent_id));
    assert_eq!(received_event.parent_id.as_ref(), Some(&sent_event.id));
    nothing_for(&mut child);
    emit(&child.io, RunnerOutput::TurnEnded).await;
    assert_eq!(
        input(&mut child).await,
        "[Calin] Note from the session that dispatched you (\"start\")\n\nUse tabs.\nNot spaces."
    );

    // Up: from the child to the agent that dispatched it, which is busy too.
    let before = head(&manager, &root_id).await;
    let up = note(&child.agent, "parent", "Half way.").await;
    assert_eq!(up.unwrap(), NoteOutcome::Delivered);
    let got = until_label(&manager, &root_id, before, "message_received:note").await;
    let message = received(&got)[0].1;
    assert_eq!(message.from_session_id, child.id);
    assert_eq!(message.to_agent_id.as_ref(), Some(&lead.agent_id));
    nothing_for(&mut root);
    emit(&root.io, RunnerOutput::TurnEnded).await;
    assert_eq!(
        input(&mut root).await,
        format!(
            "[Calin] Note from child session \"Write the docs\" (id {})\n\nHalf way.",
            child.id
        )
    );

    // To the user: on record where the user reads, and nobody is handed anything.
    let before = head(&manager, &root_id).await;
    let shown = note(&lead, "user", "FYI: the docs are in progress.").await;
    assert_eq!(shown.unwrap(), NoteOutcome::Shown);
    let events = all_events(&manager, &root_id).await;
    let tail = &events[before as usize..];
    assert_eq!(labels(tail), ["message_sent:note", "message_received:note"]);
    let (sent_event, message) = sent(tail)[0];
    let (received_event, got) = received(tail)[0];
    assert_eq!(got, message);
    assert_eq!(
        (&message.to_session_id, &message.to_agent_id),
        (&root_id, &None)
    );
    assert_eq!(sent_event.agent_id.as_ref(), Some(&lead.agent_id));
    assert_eq!(received_event.agent_id, None);
    assert_eq!(received_event.parent_id.as_ref(), Some(&sent_event.id));
    nothing_for(&mut root);
    nothing_for(&mut child);
    // A child's note to the user is on record in the child.
    let before = head(&manager, &child.id).await;
    assert_eq!(
        note(&child.agent, "user", "hello").await.unwrap(),
        NoteOutcome::Shown
    );
    assert_eq!(head(&manager, &child.id).await, before + 2);

    // Nobody else can be written to.
    let refused = |result: Result<NoteOutcome>| match result {
        Err(Error::Missing(reason)) => reason,
        other => panic!("expected a missing recipient, got {other:?}"),
    };
    let orphan = refused(note(&lead, "parent", "hi").await);
    assert!(orphan.contains("has no parent"), "{orphan}");
    let stranger = refused(note(&lead, "01ARZ3NDEKTSV4RRFFQ69G5FAV", "hi").await);
    assert!(stranger.contains("is not `parent`, `user` or the id of a child session"));
    assert!(
        stranger.contains(&format!("{} (\"Write the docs\")", child.id)),
        "{stranger}"
    );
    assert!(
        stranger.contains(&format!("{} (\"Other\")", other.id)),
        "{stranger}"
    );
    // Not a sibling, not its own parent by id, not itself.
    for to in [&other.id, &root_id, &child.id] {
        let said = refused(note(&child.agent, to, "hi").await);
        assert!(
            said.contains("This session has no child sessions."),
            "{said}"
        );
    }
    for (to, text) in [("user", " "), (" ", "hi")] {
        assert!(matches!(
            note(&lead, to, text).await,
            Err(Error::Invalid(_))
        ));
    }

    // A note to a session that is over is recorded, and the sender is told.
    let stopping = tokio::spawn({
        let (manager, id) = (manager.clone(), other.id.clone());
        async move { manager.stop(&id).await }
    });
    exit_on_stop(&mut other).await;
    within(stopping).await.unwrap().unwrap();
    let before = head(&manager, &other.id).await;
    let unread = note(&lead, &other.id, "too late").await;
    assert_eq!(
        unread.unwrap(),
        NoteOutcome::Unread(SessionStatus::Cancelled)
    );
    until_label(&manager, &other.id, before, "message_received:note").await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(head(&manager, &other.id).await, before + 1);
    assert_eq!(status(&manager, &other.id).await, SessionStatus::Cancelled);
    assert!(tree.starts.try_recv().is_err());
}

fn asked(events: &[Event]) -> Vec<(&Event, &Question)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::QuestionAsked { question } => Some((event, question)),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_question_parks_the_session_until_the_user_answers() {
    let Tree {
        manager,
        root,
        _dir,
        clones: _clones,
        ..
    } = tree().await;
    let mut root = root;
    let id = root.id.clone();
    let lead = root.agent.agent_id.clone();
    assert!(matches!(
        manager.agent_ask_user(&root.agent, " ", Vec::new()).await,
        Err(Error::Invalid(_))
    ));
    let before = head(&manager, &id).await;
    let options = vec!["blue".to_owned(), " ".to_owned(), "red ".to_owned()];
    let question_id = manager
        .agent_ask_user(&root.agent, "Which color?", options)
        .await
        .unwrap();
    let events = all_events(&manager, &id).await;
    let tail = &events[before as usize..];
    assert_eq!(
        labels(tail),
        ["question_asked:Which color?", "session_status:waiting"]
    );
    let (asked_event, question) = asked(tail)[0];
    assert_eq!(question.id, question_id);
    assert_eq!(question.options, ["blue", "red"]);
    assert_eq!(asked_event.agent_id.as_ref(), Some(&lead));
    assert_eq!(
        tail[1].body,
        EventBody::SessionStatus {
            status: SessionStatus::Waiting,
            reason: Some("question".to_owned()),
        }
    );
    // One at a time.
    let second = manager
        .agent_ask_user(&root.agent, "And size?", Vec::new())
        .await;
    assert!(matches!(&second, Err(Error::Conflict(reason)) if reason.contains("already open")));

    // The agent ends its turn; the session keeps waiting instead of
    // going to review.
    end_turn(&manager, &root).await;
    assert_eq!(status(&manager, &id).await, SessionStatus::Waiting);
    assert_eq!(head(&manager, &id).await, before + 3);
    nothing_for(&mut root);

    let answer = |question: &str, text: &'static str| {
        let (manager, id, question) = (manager.clone(), id.clone(), question.to_owned());
        async move { within(manager.answer_question(&id, &question, text)).await }
    };
    assert!(matches!(
        answer(&question_id, " \n").await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        answer("nope", "blue").await,
        Err(Error::Missing(_))
    ));
    let unknown = manager.answer_question("01ARZ3NDEKTSV4RRFFQ69G5FAV", &question_id, "blue");
    assert!(matches!(unknown.await, Err(Error::NotFound(_))));
    assert_eq!(head(&manager, &id).await, before + 3);

    answer(&question_id, " blue ").await.unwrap();
    assert_eq!(input(&mut root).await, "[Calin] The user answered: blue");
    let tail = until_label(&manager, &id, before + 3, "agent_status:running").await;
    assert_eq!(
        labels(&tail),
        [
            "question_answered:blue:user",
            "session_status:running",
            "agent_status:running"
        ]
    );
    // The answer is the asker's event, caused by the question, and it is
    // what causes the next turn. No user message is made up for it.
    assert_eq!(tail[0].agent_id.as_ref(), Some(&lead));
    assert_eq!(tail[0].parent_id.as_ref(), Some(&asked_event.id));
    assert_eq!(tail[2].parent_id.as_ref(), Some(&tail[0].id));
    assert!(matches!(
        answer(&question_id, "red").await,
        Err(Error::Conflict(_))
    ));
    emit(&root.io, RunnerOutput::TurnEnded).await;
    until_label(&manager, &id, before, "session_status:review").await;

    // A new question can be asked once the last one is answered.
    let again = manager.agent_ask_user(&root.agent, "And size?", Vec::new());
    assert_ne!(again.await.unwrap(), question_id);
    let timeline = manager.timeline(&id).await.unwrap();
    let waits: Vec<(&str, bool)> = timeline.lanes[0]
        .spans
        .iter()
        .filter(|span| span.kind == calin_proto::SpanKind::Waiting)
        .map(|span| (span.title.as_str(), span.end.is_some()))
        .collect();
    assert_eq!(
        waits,
        [
            ("Waiting for an answer", true),
            ("Waiting for an answer", false)
        ]
    );
}

#[tokio::test]
async fn a_session_waits_while_a_request_or_a_question_is_open() {
    let Tree {
        manager,
        root,
        _dir,
        clones: _clones,
        ..
    } = tree_in(PermissionMode::Ask).await;
    let mut root = root;
    let id = root.id.clone();
    let waiting_events = |events: &[Event]| {
        let waits = |label: &&String| label.starts_with("session_status:");
        labels(events)
            .iter()
            .filter(waits)
            .cloned()
            .collect::<Vec<_>>()
    };

    // A permission request first, then a question.
    emit(&root.io, ask("p1", None)).await;
    until_label(&manager, &id, 0, "session_status:waiting").await;
    let before = head(&manager, &id).await;
    let first = manager
        .agent_ask_user(&root.agent, "One?", Vec::new())
        .await
        .unwrap();
    // Already waiting: asking does not say so again.
    assert_eq!(head(&manager, &id).await, before + 1);
    manager
        .resolve_permission(&id, "p1", PermissionDecision::Allow, None)
        .await
        .unwrap();
    assert!(matches!(
        within(root.io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision { .. })
    ));
    // The request is answered, the question is not.
    assert_eq!(status(&manager, &id).await, SessionStatus::Waiting);
    manager.answer_question(&id, &first, "yes").await.unwrap();
    assert_eq!(status(&manager, &id).await, SessionStatus::Running);
    // The agent has not ended its turn: the answer waits for that.
    nothing_for(&mut root);

    // A question first, then a permission request.
    let second = manager
        .agent_ask_user(&root.agent, "Two?", Vec::new())
        .await
        .unwrap();
    assert_eq!(status(&manager, &id).await, SessionStatus::Waiting);
    emit(&root.io, ask("p2", None)).await;
    until_label(&manager, &id, before, "permission_requested:Bash").await;
    manager.answer_question(&id, &second, "no").await.unwrap();
    assert_eq!(status(&manager, &id).await, SessionStatus::Waiting);
    manager
        .resolve_permission(&id, "p2", PermissionDecision::Deny, None)
        .await
        .unwrap();
    assert_eq!(status(&manager, &id).await, SessionStatus::Running);
    assert!(matches!(
        within(root.io.inputs.recv()).await,
        Some(RunnerInput::PermissionDecision { .. })
    ));
    let events = all_events(&manager, &id).await;
    assert_eq!(
        waiting_events(&events[2..]),
        [
            "session_status:preparing",
            "session_status:running",
            "session_status:waiting",
            "session_status:running",
            "session_status:waiting",
            "session_status:running",
        ]
    );

    // Both answers are handed over, each as a turn of its own.
    emit(&root.io, RunnerOutput::TurnEnded).await;
    assert_eq!(input(&mut root).await, "[Calin] The user answered: yes");
    emit(&root.io, RunnerOutput::TurnEnded).await;
    assert_eq!(input(&mut root).await, "[Calin] The user answered: no");
    emit(&root.io, RunnerOutput::TurnEnded).await;
    until_label(&manager, &id, before, "session_status:review").await;
}

#[tokio::test]
async fn a_question_nobody_answered_expires_when_the_session_ends() {
    // Stopped.
    let mut stopped = tree().await;
    let manager = stopped.manager.clone();
    let id = stopped.root.id.clone();
    let question = manager.agent_ask_user(&stopped.root.agent, "Well?", Vec::new());
    let question = question.await.unwrap();
    let before = head(&manager, &id).await;
    let stopping = tokio::spawn({
        let (manager, id) = (manager.clone(), id.clone());
        async move { manager.stop(&id).await }
    });
    exit_on_stop(&mut stopped.root).await;
    within(stopping).await.unwrap().unwrap();
    let events = all_events(&manager, &id).await;
    let tail = &events[before as usize..];
    assert_eq!(
        labels(tail),
        [
            "question_answered::expired",
            "agent_status:stopped",
            "session_status:cancelled"
        ]
    );
    assert_eq!(
        tail[0].body,
        EventBody::QuestionAnswered {
            question_id: question.clone(),
            answer: String::new(),
            by: DecisionSource::Expired,
        }
    );
    assert_eq!(
        tail[0].agent_id.as_ref(),
        Some(&stopped.root.agent.agent_id)
    );
    assert_eq!(
        tail[0].parent_id.as_ref(),
        Some(&events[before as usize - 2].id)
    );
    let late = manager.answer_question(&id, &question, "too late").await;
    assert!(matches!(late, Err(Error::Conflict(_))));
    assert!(
        manager
            .store()
            .open_questions(None)
            .await
            .unwrap()
            .is_empty()
    );

    // Failed.
    let failed = tree().await;
    let manager = failed.manager.clone();
    let id = failed.root.id.clone();
    let asked = manager.agent_ask_user(&failed.root.agent, "Well?", Vec::new());
    asked.await.unwrap();
    let died = RunnerOutput::Exited(Exit::Error("gone".to_owned()));
    emit(&failed.root.io, died).await;
    let events = until_label(&manager, &id, 0, "session_status:failed").await;
    assert!(labels(&events).contains(&"question_answered::expired".to_owned()));
    assert!(
        manager
            .store()
            .open_questions(None)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_question_outlives_its_agent_and_the_answer_starts_it_again() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let id = tree.root.id.clone();
    let question = manager.agent_ask_user(&tree.root.agent, "Well?", Vec::new());
    let question = question.await.unwrap();
    end_turn(&manager, &tree.root).await;
    // The agent exits while the user thinks; the session still waits.
    let before = head(&manager, &id).await;
    emit(&tree.root.io, RunnerOutput::Exited(Exit::Ok)).await;
    until_label(&manager, &id, before, "agent_status:done").await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(status(&manager, &id).await, SessionStatus::Waiting);
    assert_eq!(
        manager
            .store()
            .open_questions(Some(&id))
            .await
            .unwrap()
            .len(),
        1
    );

    manager
        .answer_question(&id, &question, "go on")
        .await
        .unwrap();
    let restarted = play(&mut tree.starts).await;
    assert_eq!(restarted.id, id);
    assert_eq!(restarted.agent, tree.root.agent);
    assert_eq!(restarted.params.prompt, "[Calin] The user answered: go on");
    until_label(&manager, &id, before, "session_status:running").await;
}

#[tokio::test]
async fn stopping_a_session_stops_its_running_children_first_and_interrupt_does_not() {
    let mut tree = tree().await;
    let manager = tree.manager.clone();
    let root_id = tree.root.id.clone();
    let mut busy = tree.dispatch("Busy").await;
    let mut rested = tree.dispatch("Rested").await;
    let mut root = tree.root;
    until_label(&manager, &busy.id, 0, "agent_spawned").await;
    until_label(&manager, &rested.id, 0, "agent_spawned").await;
    // One child is done and waits in review; its report waits at the parent.
    emit(&rested.io, RunnerOutput::TurnEnded).await;
    until_label(&manager, &rested.id, 0, "message_sent:report").await;
    until_label(&manager, &root_id, 0, "message_received:report").await;

    // An interrupt is for the one session.
    manager.interrupt(&root_id).await.unwrap();
    assert_eq!(
        within(root.io.inputs.recv()).await,
        Some(RunnerInput::Interrupt)
    );
    emit(&root.io, RunnerOutput::TurnEnded).await;
    until_label(&manager, &root_id, 0, "session_status:review").await;
    assert_eq!(status(&manager, &busy.id).await, SessionStatus::Running);
    nothing_for(&mut busy);

    let stopping = tokio::spawn({
        let (manager, id) = (manager.clone(), root_id.clone());
        async move { manager.stop(&id).await }
    });
    // The child that is still at work is told first, then the parent.
    exit_on_stop(&mut busy).await;
    exit_on_stop(&mut root).await;
    within(stopping).await.unwrap().unwrap();
    assert_eq!(status(&manager, &busy.id).await, SessionStatus::Cancelled);
    assert_eq!(status(&manager, &root_id).await, SessionStatus::Cancelled);
    // The one that had come to rest is left as it is, for the user to read.
    assert_eq!(status(&manager, &rested.id).await, SessionStatus::Review);
    nothing_for(&mut rested);

    let cancelled = |events: &[Event]| events.last().unwrap().id.clone();
    let busy_events = all_events(&manager, &busy.id).await;
    let root_events = all_events(&manager, &root_id).await;
    assert!(cancelled(&busy_events) < cancelled(&root_events));
    // A stopped child reports nothing.
    assert!(sent(&busy_events).is_empty());
    assert_eq!(received(&root_events).len(), 1);
    // Stopping it again is refused, as for any session that is over.
    assert!(matches!(
        manager.stop(&root_id).await,
        Err(Error::Conflict(_))
    ));
}

#[tokio::test]
async fn a_restart_expires_the_question_of_a_session_it_cut_short() {
    let dir = TempDir::new().unwrap();
    let (manager, mut starts) = manager_in(&dir, false).await;
    manager
        .create_session(request("crash", Some("puppet")))
        .await
        .unwrap();
    let root = play(&mut starts).await;
    let question = manager.agent_ask_user(&root.agent, "Well?", Vec::new());
    let question = question.await.unwrap();
    let before = head(&manager, &root.id).await;
    // The process dies while the question is on screen.
    manager.store().close().await;
    drop(manager);

    let (manager, _starts) = manager_in(&dir, false).await;
    let rest = until_label(&manager, &root.id, before, "session_status:failed").await;
    assert_eq!(
        labels(&rest),
        [
            "question_answered::expired",
            "agent_status:failed",
            "session_status:failed",
        ]
    );
    assert_eq!(rest[0].agent_id.as_ref(), Some(&root.agent.agent_id));
    assert!(
        manager
            .store()
            .open_questions(None)
            .await
            .unwrap()
            .is_empty()
    );
    let late = manager
        .answer_question(&root.id, &question, "too late")
        .await;
    assert!(matches!(late, Err(Error::Conflict(_))));
}
