//! The graph and the timeline of a session tree, built from events appended
//! by hand so that every id and every moment is known.

use std::collections::BTreeMap;
use std::time::Instant;

use calin_proto::{
    AgentMessage, EdgeKind, Graph, GraphEdge, Item, ItemContent, ItemStatus, MessageKind, NodeKind,
    SpanKind, Timeline, TimelineSpan,
};

use super::*;

const API: &str = "repo-api";
const DOCS: &str = "repo-docs";

async fn add(store: &Store, session_id: &str, draft: EventDraft) -> Arc<Event> {
    store.append(session_id, draft).await.unwrap()
}

/// Lets the clock move on, so that what follows is later than what came before.
async fn tick() {
    tokio::time::sleep(Duration::from_millis(3)).await;
}

fn spawned(agent: Agent) -> EventBody {
    EventBody::AgentSpawned { agent }
}

fn subagent(id: &str, session_id: &str, parent: &str, role: &str) -> Agent {
    Agent {
        parent_agent_id: Some(parent.to_owned()),
        kind: AgentKind::Subagent,
        role: role.to_owned(),
        model: None,
        ..agent(id, session_id)
    }
}

fn agent_status(status: AgentStatus) -> EventBody {
    EventBody::AgentStatus { status }
}

fn tool(id: &str, kind: ToolKind, title: &str, status: ItemStatus) -> Item {
    Item {
        id: id.to_owned(),
        status,
        content: ItemContent::Tool {
            name: "Tool".to_owned(),
            tool_kind: kind,
            title: title.to_owned(),
            input: serde_json::json!({}),
            output: None,
            locations: Vec::new(),
        },
    }
}

fn started(item: Item) -> EventBody {
    EventBody::ItemStarted { item }
}

fn completed(item: Item) -> EventBody {
    EventBody::ItemCompleted { item }
}

fn repo(id: &str, session_id: &str, repo_id: &str, mode: RepoMode) -> EventBody {
    EventBody::RepoAttached {
        repo: SessionRepo {
            id: id.to_owned(),
            session_id: session_id.to_owned(),
            repo_id: repo_id.to_owned(),
            name: repo_id.trim_start_matches("repo-").to_owned(),
            mode,
            path: format!("/work/{id}"),
            branch: None,
            base_ref: None,
            primary: false,
        },
    }
}

fn changed(session_repo_id: &str, path: &str) -> EventBody {
    EventBody::FileChanged {
        session_repo_id: Some(session_repo_id.to_owned()),
        path: path.to_owned(),
        op: FileOp::Edit,
    }
}

fn letter(
    id: &str,
    kind: MessageKind,
    from: (&str, Option<&str>),
    to: (&str, Option<&str>),
    body: &str,
) -> AgentMessage {
    AgentMessage {
        id: id.to_owned(),
        kind,
        from_session_id: from.0.to_owned(),
        from_agent_id: from.1.map(str::to_owned),
        to_session_id: to.0.to_owned(),
        to_agent_id: to.1.map(str::to_owned),
        body: body.to_owned(),
        in_reply_to: None,
        ts: 0,
    }
}

/// Appends `message_sent` to the sender's log and `message_received` to the
/// recipient's, as the manager does. Returns the received event.
async fn send(store: &Store, message: AgentMessage) -> Arc<Event> {
    let mut sent = EventDraft::new(EventBody::MessageSent {
        message: message.clone(),
    });
    sent.agent_id = message.from_agent_id.clone();
    let sent = add(store, &message.from_session_id.clone(), sent).await;
    let to = message.to_session_id.clone();
    let received = EventDraft::new(EventBody::MessageReceived { message });
    add(store, &to, received.parent(Some(sent.id.clone()))).await
}

/// A root session with a lead (`L`), a subagent of it (`S`) and a child
/// session whose lead (`CL`) it dispatched, and the moments in between.
struct Scene {
    root: String,
    child: String,
    /// Timestamps of the newest event at five moments: the lead's first
    /// change, the subagent done, the dispatch sent, the child's lead
    /// started, and everything.
    moments: [i64; 5],
    /// The user's prompt and the brief as the child received it.
    prompt: Arc<Event>,
    brief: Arc<Event>,
    /// The root lead going idle: the end of its turn.
    lead_idle: Arc<Event>,
}

const LONG_NOTE: &str = "This second note is deliberately much longer than eighty characters so that its excerpt has to be cut";

async fn scene(store: &Store) -> Scene {
    let root = store
        .create_session(NewSession {
            title: "Root task".to_owned(),
            ..new_session("Build it")
        })
        .await
        .unwrap()
        .id;
    let r = root.as_str();
    add(store, r, status(SessionStatus::Queued)).await;
    let prompt = add(store, r, message(1)).await;
    tick().await;
    add(store, r, status(SessionStatus::Running)).await;
    add(
        store,
        r,
        EventDraft::new(repo("r-api", r, API, RepoMode::Write)),
    )
    .await;
    add(
        store,
        r,
        EventDraft::new(repo("r-docs", r, DOCS, RepoMode::Read)),
    )
    .await;
    let lead = EventDraft::new(spawned(agent("L", r))).parent(Some(prompt.id.clone()));
    add(store, r, lead).await;
    let by_lead = |body| EventDraft::new(body).agent("L");
    let by_sub = |body| EventDraft::new(body).agent("S");
    let edit = tool("i1", ToolKind::Edit, "src/a.rs", ItemStatus::Running);
    add(store, r, by_lead(started(edit.clone()))).await;
    tick().await;
    add(
        store,
        r,
        by_lead(completed(Item {
            status: ItemStatus::Completed,
            ..edit
        })),
    )
    .await;
    let first = add(store, r, by_lead(changed("r-api", "src/a.rs"))).await;
    tick().await;

    let task = tool(
        "task-1",
        ToolKind::Task,
        "Explore the code",
        ItemStatus::Running,
    );
    let call = add(store, r, by_lead(started(task.clone()))).await;
    let sub = spawned(subagent("S", r, "L", "explorer"));
    add(store, r, EventDraft::new(sub).parent(Some(call.id.clone()))).await;
    let read = tool("i2", ToolKind::Read, "src/b.rs", ItemStatus::Running);
    add(store, r, by_sub(started(read.clone()))).await;
    tick().await;
    add(
        store,
        r,
        by_sub(completed(Item {
            status: ItemStatus::Failed,
            ..read
        })),
    )
    .await;
    add(store, r, by_sub(changed("r-api", "src/b.rs"))).await;
    add(store, r, by_sub(agent_status(AgentStatus::Done))).await;
    let done = by_lead(completed(Item {
        status: ItemStatus::Completed,
        ..task
    }));
    let second = add(store, r, done).await;
    tick().await;

    let child = store
        .create_session(NewSession {
            title: "Docs child".to_owned(),
            dispatch: Some(Dispatch {
                parent_session_id: root.clone(),
                root_session_id: root.clone(),
                agent_id: "L".to_owned(),
                lead_agent_id: "CL".to_owned(),
                message_id: "m-dispatch".to_owned(),
                start_points: BTreeMap::new(),
            }),
            ..new_session("Write the docs")
        })
        .await
        .unwrap()
        .id;
    let c = child.as_str();
    add(store, c, status(SessionStatus::Queued)).await;
    let dispatch = letter(
        "m-dispatch",
        MessageKind::Dispatch,
        (r, Some("L")),
        (c, Some("CL")),
        "\n  Write the docs  \nand more",
    );
    let brief = send(store, dispatch).await;
    let third = brief.ts;
    tick().await;

    add(store, c, status(SessionStatus::Running)).await;
    add(
        store,
        c,
        EventDraft::new(repo("c-api", c, API, RepoMode::Write)),
    )
    .await;
    // Attached for writing here and for reading in the root.
    add(
        store,
        c,
        EventDraft::new(repo("c-docs", c, DOCS, RepoMode::Write)),
    )
    .await;
    let child_lead = EventDraft::new(spawned(agent("CL", c))).parent(Some(brief.id.clone()));
    let fourth = add(store, c, child_lead).await;
    tick().await;

    let by_child = |body| EventDraft::new(body).agent("CL");
    add(store, c, by_child(changed("c-docs", "guide.md"))).await;
    add(store, c, by_child(changed("c-docs", "index.md"))).await;
    let note =
        |id: &str, body: &str| letter(id, MessageKind::Note, (r, Some("L")), (c, Some("CL")), body);
    send(
        store,
        letter(
            "m-up",
            MessageKind::Note,
            (c, Some("CL")),
            (r, Some("L")),
            "Half way",
        ),
    )
    .await;
    send(
        store,
        letter(
            "m-user",
            MessageKind::Note,
            (r, Some("L")),
            (r, None),
            "FYI",
        ),
    )
    .await;
    send(store, note("m-down-1", "First note")).await;
    tick().await;
    send(store, note("m-down-2", LONG_NOTE)).await;
    add(store, c, by_child(agent_status(AgentStatus::Idle))).await;
    add(store, c, status(SessionStatus::Review)).await;
    let report = letter(
        "m-report",
        MessageKind::Report,
        (c, Some("CL")),
        (r, Some("L")),
        "Done.\nDetails",
    );
    send(store, report).await;
    // The user saves a file in the editor.
    add(store, r, EventDraft::new(changed("r-api", "README.md"))).await;
    let asked = EventDraft::new(EventBody::SessionStatus {
        status: SessionStatus::Waiting,
        reason: Some("question".to_owned()),
    });
    add(store, r, asked).await;
    tick().await;
    add(store, r, status(SessionStatus::Running)).await;
    let lead_idle = add(store, r, by_lead(agent_status(AgentStatus::Idle))).await;
    let last = add(store, r, status(SessionStatus::Review)).await;
    Scene {
        root,
        child,
        moments: [first.ts, second.ts, third, fourth.ts, last.ts],
        prompt,
        brief,
        lead_idle,
    }
}

fn node_ids(graph: &Graph) -> Vec<&str> {
    graph.nodes.iter().map(|node| node.id.as_str()).collect()
}

fn edge<'a>(graph: &'a Graph, id: &str) -> &'a GraphEdge {
    let found = graph.edges.iter().find(|edge| edge.id == id);
    found.unwrap_or_else(|| panic!("no edge {id} among {:?}", edge_ids(graph)))
}

fn edge_ids(graph: &Graph) -> Vec<&str> {
    graph.edges.iter().map(|edge| edge.id.as_str()).collect()
}

#[tokio::test]
async fn the_graph_has_stable_ids_one_node_per_repo_and_one_edge_per_relation() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let scene = scene(&store).await;
    let graph = store.graph(&scene.root, None).await.unwrap();
    // Whichever session of the tree is asked.
    assert_eq!(store.graph(&scene.child, None).await.unwrap(), graph);
    assert_eq!(graph.root_session_id, scene.root);
    assert_eq!(graph.at, graph.to_ts);
    assert!(graph.from_ts <= scene.prompt.ts && graph.to_ts == scene.moments[4]);

    // The user, the agents in spawn-tree order, the repos by name.
    assert_eq!(
        node_ids(&graph),
        [
            "user",
            "agent:L",
            "agent:S",
            "agent:CL",
            "repo:repo-api",
            "repo:repo-docs"
        ]
    );
    let node = |id: &str| graph.nodes.iter().find(|node| node.id == id).unwrap();
    let user = node("user");
    assert_eq!(
        (user.kind, user.parent_id.as_deref()),
        (NodeKind::User, None)
    );
    assert!(user.agent.is_none() && user.session_id.is_none());

    let lead = node("agent:L");
    assert_eq!(lead.kind, NodeKind::Agent);
    assert_eq!(lead.label, "Root task");
    assert_eq!(lead.parent_id.as_deref(), Some("user"));
    assert_eq!(lead.session_id.as_ref(), Some(&scene.root));
    assert_eq!(lead.session_status, Some(SessionStatus::Review));
    assert_eq!(lead.session_title.as_deref(), Some("Root task"));
    assert_eq!(lead.agent.as_ref().unwrap().status, AgentStatus::Idle);

    let sub = node("agent:S");
    assert_eq!(sub.label, "Explore the code");
    assert_eq!(sub.parent_id.as_deref(), Some("agent:L"));
    assert_eq!(sub.agent.as_ref().unwrap().status, AgentStatus::Done);
    assert_eq!(sub.agent.as_ref().unwrap().role, "explorer");
    // Only a session's lead carries the session.
    assert_eq!(
        (sub.session_status, sub.session_title.as_deref()),
        (None, None)
    );
    assert_eq!(sub.session_id.as_ref(), Some(&scene.root));

    let child = node("agent:CL");
    assert_eq!(child.label, "Docs child");
    assert_eq!(child.parent_id.as_deref(), Some("agent:L"));
    assert_eq!(child.session_id.as_ref(), Some(&scene.child));
    assert_eq!(child.session_status, Some(SessionStatus::Review));

    // One node per registry repo, with the strongest mode it was attached in.
    let api = node("repo:repo-api");
    assert_eq!((api.kind, api.label.as_str()), (NodeKind::Repo, "api"));
    assert_eq!(api.repo_id.as_deref(), Some(API));
    assert_eq!(api.repo_mode, Some(RepoMode::Write));
    assert_eq!(api.parent_id, None);
    assert_eq!(node("repo:repo-docs").repo_mode, Some(RepoMode::Write));

    let mut ids = edge_ids(&graph);
    ids.sort_unstable();
    assert_eq!(
        ids,
        [
            "changed:agent:CL:repo:repo-docs",
            "changed:agent:L:repo:repo-api",
            "changed:agent:S:repo:repo-api",
            "changed:user:repo:repo-api",
            "dispatched:agent:L:agent:CL",
            "message:agent:CL:agent:L",
            "message:agent:L:agent:CL",
            "message:agent:L:user",
            "report:agent:CL:agent:L",
            "spawned:agent:L:agent:S",
        ]
    );
    for edge in &graph.edges {
        let kind = serde_json::to_value(edge.kind).unwrap();
        let id = format!("{}:{}:{}", kind.as_str().unwrap(), edge.from, edge.to);
        assert_eq!(edge.id, id);
        assert!(node_ids(&graph).contains(&edge.from.as_str()), "{id}");
        assert!(node_ids(&graph).contains(&edge.to.as_str()), "{id}");
        assert!(edge.last_ts >= graph.from_ts && edge.last_ts <= graph.to_ts);
    }
    let spawned = edge(&graph, "spawned:agent:L:agent:S");
    assert_eq!((spawned.kind, spawned.count), (EdgeKind::Spawned, 1));
    assert_eq!(spawned.label.as_deref(), Some("Explore the code"));
    let dispatched = edge(&graph, "dispatched:agent:L:agent:CL");
    assert_eq!(
        (dispatched.kind, dispatched.count),
        (EdgeKind::Dispatched, 1)
    );
    // The first line that says something.
    assert_eq!(dispatched.label.as_deref(), Some("Write the docs"));
    assert!(dispatched.last_ts > scene.moments[1] && dispatched.last_ts <= scene.moments[2]);
    // Two notes one way are one edge: its label is the newest, cut to fit.
    let notes = edge(&graph, "message:agent:L:agent:CL");
    assert_eq!((notes.kind, notes.count), (EdgeKind::Message, 2));
    let label = notes.label.as_deref().unwrap();
    assert_eq!(label.chars().count(), 80);
    assert!(label.ends_with('…') && LONG_NOTE.starts_with(label.trim_end_matches('…')));
    assert_eq!(
        edge(&graph, "message:agent:CL:agent:L").label.as_deref(),
        Some("Half way")
    );
    assert_eq!(edge(&graph, "message:agent:L:user").count, 1);
    let report = edge(&graph, "report:agent:CL:agent:L");
    assert_eq!(
        (report.kind, report.label.as_deref()),
        (EdgeKind::Report, Some("Done."))
    );
    // Files changed: counted, labelled with the newest path.
    let docs = edge(&graph, "changed:agent:CL:repo:repo-docs");
    assert_eq!((docs.kind, docs.count), (EdgeKind::Changed, 2));
    assert_eq!(docs.label.as_deref(), Some("index.md"));
    assert_eq!(
        edge(&graph, "changed:agent:S:repo:repo-api")
            .label
            .as_deref(),
        Some("src/b.rs")
    );
    assert_eq!(
        edge(&graph, "changed:user:repo:repo-api").label.as_deref(),
        Some("README.md")
    );

    assert!(matches!(
        store.graph("01ARZ3NDEKTSV4RRFFQ69G5FAV", None).await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn a_graph_in_the_past_holds_only_what_had_happened_by_then() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let scene = scene(&store).await;
    let at = |moment: usize| store.graph(&scene.root, Some(scene.moments[moment]));
    let status_of = |graph: &Graph, id: &str| {
        let node = graph.nodes.iter().find(|node| node.id == id).unwrap();
        (node.agent.as_ref().unwrap().status, node.session_status)
    };

    // The lead made its first change; nobody else exists yet.
    let first = at(0).await.unwrap();
    assert_eq!(first.at, scene.moments[0]);
    assert_eq!(
        node_ids(&first),
        ["user", "agent:L", "repo:repo-api", "repo:repo-docs"]
    );
    assert_eq!(edge_ids(&first), ["changed:agent:L:repo:repo-api"]);
    let running = (AgentStatus::Running, Some(SessionStatus::Running));
    assert_eq!(status_of(&first, "agent:L"), running);
    // Read-only then: the child that writes to it comes later.
    let docs = first
        .nodes
        .iter()
        .find(|node| node.id == "repo:repo-docs")
        .unwrap();
    assert_eq!(docs.repo_mode, Some(RepoMode::Read));

    // The subagent came and went.
    let second = at(1).await.unwrap();
    assert_eq!(node_ids(&second)[..3], ["user", "agent:L", "agent:S"]);
    assert_eq!(status_of(&second, "agent:S"), (AgentStatus::Done, None));
    assert_eq!(
        edge_ids(&second),
        [
            "changed:agent:L:repo:repo-api",
            "changed:agent:S:repo:repo-api",
            "spawned:agent:L:agent:S"
        ]
    );

    // The brief is sent, but the child's agent has not started: an edge
    // needs both its ends.
    let third = at(2).await.unwrap();
    assert_eq!(node_ids(&third), node_ids(&second));
    assert_eq!(edge_ids(&third), edge_ids(&second));

    // Now it has, and the dispatch shows; its session is running.
    let fourth = at(3).await.unwrap();
    assert!(node_ids(&fourth).contains(&"agent:CL"));
    assert!(edge_ids(&fourth).contains(&"dispatched:agent:L:agent:CL"));
    assert_eq!(status_of(&fourth, "agent:CL"), running);
    assert_eq!(status_of(&fourth, "agent:L"), running);
    assert!(
        !edge_ids(&fourth)
            .iter()
            .any(|id| id.starts_with("message:"))
    );
    let docs = fourth
        .nodes
        .iter()
        .find(|node| node.id == "repo:repo-docs")
        .unwrap();
    assert_eq!(docs.repo_mode, Some(RepoMode::Write));
    assert_eq!((fourth.from_ts, fourth.to_ts), (first.from_ts, first.to_ts));

    // The newest moment is the present; outside the tree's range `at` clamps.
    let now = store.graph(&scene.root, None).await.unwrap();
    assert_eq!(at(4).await.unwrap(), now);
    assert_eq!(store.graph(&scene.root, Some(i64::MAX)).await.unwrap(), now);
    let earliest = store.graph(&scene.root, Some(now.from_ts)).await.unwrap();
    for before in [0, -5, now.from_ts - 1] {
        let clamped = store.graph(&scene.root, Some(before)).await.unwrap();
        assert_eq!(clamped.at, now.from_ts);
        assert_eq!(clamped, earliest);
    }
    // At the very start there is the user, and whatever the first
    // millisecond already held.
    assert_eq!(earliest.nodes[0].id, "user");
    assert!(earliest.edges.is_empty());
}

/// An arrow's id, kind, origin, target and label.
type ArrowShape<'a> = (
    &'a str,
    EdgeKind,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

fn lane_ids(timeline: &Timeline) -> Vec<&str> {
    timeline
        .lanes
        .iter()
        .map(|lane| lane.agent.id.as_str())
        .collect()
}

fn spans<'a>(timeline: &'a Timeline, agent: &str) -> &'a [TimelineSpan] {
    let lane = timeline.lanes.iter().find(|lane| lane.agent.id == agent);
    &lane.unwrap_or_else(|| panic!("no lane of {agent}")).spans
}

fn span_shapes(spans: &[TimelineSpan]) -> Vec<(SpanKind, &str, Option<ItemStatus>, bool)> {
    spans
        .iter()
        .map(|span| {
            (
                span.kind,
                span.title.as_str(),
                span.status,
                span.end.is_some(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_timeline_has_a_lane_per_agent_spans_that_nest_and_arrows_between_lanes() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let scene = scene(&store).await;
    let timeline = store.timeline(&scene.child).await.unwrap();
    assert_eq!(store.timeline(&scene.root).await.unwrap(), timeline);
    assert_eq!(timeline.root_session_id, scene.root);
    let graph = store.graph(&scene.root, None).await.unwrap();
    assert_eq!(
        (timeline.from_ts, timeline.to_ts),
        (graph.from_ts, graph.to_ts)
    );

    // A parent before its children, siblings by start time.
    assert_eq!(lane_ids(&timeline), ["L", "S", "CL"]);
    let parents: Vec<Option<&str>> = timeline
        .lanes
        .iter()
        .map(|lane| lane.parent_id.as_deref())
        .collect();
    assert_eq!(parents, [Some("user"), Some("agent:L"), Some("agent:L")]);
    assert_eq!(timeline.lanes[2].session_id, scene.child);
    assert_eq!(timeline.lanes[1].session_id, scene.root);

    let lead = spans(&timeline, "L");
    assert_eq!(
        span_shapes(lead),
        [
            (SpanKind::Turn, "message 1", None, true),
            (
                SpanKind::Tool,
                "src/a.rs",
                Some(ItemStatus::Completed),
                true
            ),
            (
                SpanKind::Tool,
                "Explore the code",
                Some(ItemStatus::Completed),
                true
            ),
            (SpanKind::Waiting, "Waiting for an answer", None, true),
        ]
    );
    // A turn runs from the input that started it to the end of the turn.
    let turn = &lead[0];
    assert_eq!(turn.start, scene.prompt.ts);
    assert_eq!(turn.end, Some(scene.lead_idle.ts));
    assert_eq!(lead[1].tool_kind, Some(ToolKind::Edit));
    assert_eq!((turn.tool_kind, turn.status), (None, None));
    for lane in &timeline.lanes {
        let spans = &lane.spans;
        assert!(spans.is_sorted_by_key(|span| span.start));
        let turns: Vec<&TimelineSpan> = spans
            .iter()
            .filter(|span| span.kind == SpanKind::Turn)
            .collect();
        for span in spans.iter().filter(|span| span.kind != SpanKind::Turn) {
            let end = span.end.unwrap();
            assert!(end > span.start, "{} is empty", span.title);
            // Tool and waiting spans lie inside turn spans.
            assert!(
                turns
                    .iter()
                    .any(|turn| turn.start <= span.start && end <= turn.end.unwrap()),
                "{} is outside every turn",
                span.title
            );
        }
        let ids: HashSet<&str> = spans.iter().map(|span| span.id.as_str()).collect();
        assert_eq!(ids.len(), spans.len());
    }

    // A subagent's one turn is its whole life, named by what it was asked.
    let sub = spans(&timeline, "S");
    assert_eq!(
        span_shapes(sub),
        [
            (SpanKind::Turn, "Explore the code", None, true),
            (SpanKind::Tool, "src/b.rs", Some(ItemStatus::Failed), true),
        ]
    );
    assert!(lead[2].start <= sub[0].start && sub[0].end.unwrap() <= lead[2].end.unwrap());
    // A child's first turn starts with its brief.
    let child = spans(&timeline, "CL");
    assert_eq!(
        span_shapes(child),
        [(SpanKind::Turn, "Write the docs", None, true)]
    );
    assert_eq!(child[0].start, scene.brief.ts);

    let arrows: Vec<ArrowShape<'_>> = timeline
        .arrows
        .iter()
        .map(|arrow| {
            (
                arrow.id.as_str(),
                arrow.kind,
                arrow.from_agent_id.as_deref(),
                arrow.to_agent_id.as_deref(),
                arrow.label.as_deref(),
            )
        })
        .collect();
    assert!(timeline.arrows.is_sorted_by_key(|arrow| arrow.ts));
    let cut: String = LONG_NOTE.chars().take(79).chain(['…']).collect();
    assert_eq!(
        arrows,
        [
            (
                "spawned:S",
                EdgeKind::Spawned,
                Some("L"),
                Some("S"),
                Some("Explore the code")
            ),
            (
                "message:m-dispatch",
                EdgeKind::Dispatched,
                Some("L"),
                Some("CL"),
                Some("Write the docs")
            ),
            (
                "message:m-up",
                EdgeKind::Message,
                Some("CL"),
                Some("L"),
                Some("Half way")
            ),
            (
                "message:m-user",
                EdgeKind::Message,
                Some("L"),
                None,
                Some("FYI")
            ),
            (
                "message:m-down-1",
                EdgeKind::Message,
                Some("L"),
                Some("CL"),
                Some("First note")
            ),
            (
                "message:m-down-2",
                EdgeKind::Message,
                Some("L"),
                Some("CL"),
                Some(cut.as_str())
            ),
            (
                "message:m-report",
                EdgeKind::Report,
                Some("CL"),
                Some("L"),
                Some("Done.")
            ),
        ]
    );
    let sent = timeline.arrows[1].ts;
    assert!(sent > scene.moments[1] && sent <= scene.moments[2]);

    assert!(matches!(
        store.timeline("01ARZ3NDEKTSV4RRFFQ69G5FAV").await,
        Err(Error::NotFound(_))
    ));
}

#[tokio::test]
async fn open_spans_have_no_end_and_a_queued_input_starts_its_turn_where_the_last_one_ended() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("p")).await.unwrap().id;
    let s = id.as_str();
    let first = add(&store, s, message(1)).await;
    add(&store, s, status(SessionStatus::Running)).await;
    // The setup command of a repo: nobody's tool call, so in nobody's lane.
    let setup = tool(
        "setup-1",
        ToolKind::Execute,
        "make deps",
        ItemStatus::Running,
    );
    add(&store, s, EventDraft::new(started(setup))).await;
    // Nothing to show before an agent exists.
    let empty = store.timeline(s).await.unwrap();
    assert!(empty.lanes.is_empty() && empty.arrows.is_empty());
    assert_eq!(store.graph(s, None).await.unwrap().nodes.len(), 1);

    let lead = EventDraft::new(spawned(agent("L", s))).parent(Some(first.id.clone()));
    add(&store, s, lead).await;
    let by_lead = |body| EventDraft::new(body).agent("L");
    let run = tool("i1", ToolKind::Execute, "cargo test", ItemStatus::Running);
    add(&store, s, by_lead(started(run))).await;
    let asked = EventDraft::new(EventBody::SessionStatus {
        status: SessionStatus::Waiting,
        reason: Some("permission".to_owned()),
    });
    add(&store, s, asked).await;
    // A message sent while the turn runs waits for it.
    let queued = add(&store, s, message(2)).await;

    let open_now = store.timeline(s).await.unwrap();
    assert_eq!(
        span_shapes(spans(&open_now, "L")),
        [
            (SpanKind::Turn, "message 1", None, false),
            (
                SpanKind::Tool,
                "cargo test",
                Some(ItemStatus::Running),
                false
            ),
            (SpanKind::Waiting, "Waiting for permission", None, false),
        ]
    );

    tick().await;
    add(&store, s, status(SessionStatus::Running)).await;
    // The turn ends with a call the agent never completed: it ends with it.
    let ended = add(&store, s, by_lead(agent_status(AgentStatus::Idle))).await;
    let next = by_lead(agent_status(AgentStatus::Running)).parent(Some(queued.id.clone()));
    add(&store, s, next).await;
    tick().await;
    let timeline = store.timeline(s).await.unwrap();
    let lead = spans(&timeline, "L");
    assert_eq!(
        span_shapes(lead),
        [
            (SpanKind::Turn, "message 1", None, true),
            (SpanKind::Tool, "cargo test", Some(ItemStatus::Failed), true),
            (SpanKind::Waiting, "Waiting for permission", None, true),
            (SpanKind::Turn, "message 2", None, false),
        ]
    );
    assert_eq!(lead[0].end, Some(ended.ts));
    assert_eq!(lead[1].end, Some(ended.ts));
    // Not where the message was sent: the agent was busy until then.
    assert!(queued.ts < ended.ts);
    assert_eq!(lead[3].start, ended.ts);
    assert!(lead[3].id.starts_with("turn:"));
}

#[tokio::test]
async fn a_starting_agent_keeps_its_turn_and_a_wait_without_a_reason_is_plain_waiting() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("w")).await.unwrap().id;
    let s = id.as_str();
    let first = add(&store, s, message(1)).await;
    let lead = EventDraft::new(spawned(agent("L", s))).parent(Some(first.id.clone()));
    add(&store, s, lead).await;
    let starting = EventDraft::new(agent_status(AgentStatus::Starting)).agent("L");
    add(&store, s, starting).await;
    let waiting = EventDraft::new(EventBody::SessionStatus {
        status: SessionStatus::Waiting,
        reason: None,
    });
    add(&store, s, waiting).await;

    let timeline = store.timeline(s).await.unwrap();
    assert_eq!(
        span_shapes(spans(&timeline, "L")),
        [
            (SpanKind::Turn, "message 1", None, false),
            (SpanKind::Waiting, "Waiting", None, false),
        ]
    );
}

/// Tool calls per session of the large tree.
const TOOL_CALLS: usize = 21;

/// A tree of 20 sessions and 5,000 events, shaped like real ones: turns of
/// streamed text, tool calls, file changes, a subagent and messages.
async fn large_tree(store: &Store) -> String {
    let root = store.create_session(new_session("root")).await.unwrap().id;
    let mut sessions = vec![root.clone()];
    for index in 1..20 {
        let child = NewSession {
            dispatch: Some(Dispatch {
                parent_session_id: root.clone(),
                root_session_id: root.clone(),
                agent_id: "lead-0".to_owned(),
                lead_agent_id: format!("lead-{index}"),
                message_id: format!("dispatch-{index}"),
                start_points: BTreeMap::new(),
            }),
            ..new_session(&format!("child {index}"))
        };
        sessions.push(store.create_session(child).await.unwrap().id);
    }
    for (index, session) in sessions.iter().enumerate() {
        let s = session.as_str();
        let lead = format!("lead-{index}");
        let sub = format!("sub-{index}");
        let by_lead = |body| EventDraft::new(body).agent(lead.clone());
        add(store, s, status(SessionStatus::Queued)).await;
        let prompt = match index {
            0 => add(store, s, message(0)).await,
            _ => {
                let id = format!("dispatch-{index}");
                let from = (root.as_str(), Some("lead-0"));
                send(
                    store,
                    letter(&id, MessageKind::Dispatch, from, (s, Some(&lead)), "do it"),
                )
                .await
            }
        };
        add(store, s, status(SessionStatus::Running)).await;
        let attached = format!("{s}-api");
        add(
            store,
            s,
            EventDraft::new(repo(&attached, s, API, RepoMode::Write)),
        )
        .await;
        add(
            store,
            s,
            EventDraft::new(spawned(agent(&lead, s))).parent(Some(prompt.id.clone())),
        )
        .await;
        let task = tool("task", ToolKind::Task, "Look around", ItemStatus::Running);
        let call = add(store, s, by_lead(started(task.clone()))).await;
        let spawn = spawned(subagent(&sub, s, &lead, "explorer"));
        add(
            store,
            s,
            EventDraft::new(spawn).parent(Some(call.id.clone())),
        )
        .await;
        add(
            store,
            s,
            EventDraft::new(agent_status(AgentStatus::Done)).agent(sub.clone()),
        )
        .await;
        add(
            store,
            s,
            by_lead(completed(Item {
                status: ItemStatus::Completed,
                ..task
            })),
        )
        .await;
        // Twelve events a tool call: with the dozen around them, over 250
        // a session.
        for step in 1..=TOOL_CALLS {
            let id = format!("item-{step}");
            let edit = tool(
                &id,
                ToolKind::Edit,
                &format!("src/file{step}.rs"),
                ItemStatus::Running,
            );
            add(store, s, by_lead(started(edit.clone()))).await;
            for chunk in 0..9 {
                let delta = EventBody::ItemDelta {
                    item_id: id.clone(),
                    delta: calin_proto::ItemDelta::Output {
                        text: format!("chunk {chunk}\n"),
                    },
                };
                add(store, s, by_lead(delta)).await;
            }
            add(
                store,
                s,
                by_lead(completed(Item {
                    status: ItemStatus::Completed,
                    ..edit
                })),
            )
            .await;
            add(
                store,
                s,
                by_lead(changed(&attached, &format!("src/file{step}.rs"))),
            )
            .await;
        }
        add(store, s, by_lead(agent_status(AgentStatus::Idle))).await;
        add(store, s, status(SessionStatus::Review)).await;
        if index > 0 {
            let id = format!("report-{index}");
            let to = (root.as_str(), Some("lead-0"));
            send(
                store,
                letter(&id, MessageKind::Report, (s, Some(&lead)), to, "done"),
            )
            .await;
        }
    }
    root
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_graph_and_the_timeline_of_a_large_tree_are_read_quickly() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let root = large_tree(&store).await;
    let sessions = store.sessions().await.unwrap();
    let events: u64 = sessions.iter().map(|session| session.head_seq).sum();
    assert_eq!(sessions.len(), 20);
    assert!(events >= 5_000, "only {events} events");

    let graph = store.graph(&root, None).await.unwrap();
    // The user, 20 leads, 20 subagents and the one repo they all share.
    assert_eq!(graph.nodes.len(), 42);
    assert_eq!(graph.edges.len(), 20 + 19 + 19 + 20);
    let changed = graph
        .edges
        .iter()
        .filter(|edge| edge.kind == EdgeKind::Changed);
    assert_eq!(
        changed.map(|edge| edge.count).sum::<u32>(),
        20 * TOOL_CALLS as u32
    );
    let timeline = store.timeline(&root).await.unwrap();
    assert_eq!(timeline.lanes.len(), 40);
    assert_eq!(timeline.arrows.len(), 20 + 19 + 19);
    let spans: usize = timeline.lanes.iter().map(|lane| lane.spans.len()).sum();
    // Per session: the lead's turn, its calls and the one that ran the
    // subagent, and the subagent's turn.
    assert_eq!(spans, 20 * (2 + TOOL_CALLS) + 20);

    // The target is 50 ms a request (the "Graph and timeline" task of M3).
    // The bound here is ten times that, so that only a change of order (a
    // scan per event, a log read per request) fails it, not a slow machine.
    let middle = (graph.from_ts + graph.to_ts) / 2;
    let rounds = 10;
    let started = Instant::now();
    for _ in 0..rounds {
        store.graph(&root, None).await.unwrap();
        store.graph(&root, Some(middle)).await.unwrap();
    }
    let per_graph = started.elapsed() / (2 * rounds);
    let started = Instant::now();
    for _ in 0..rounds {
        store.timeline(&root).await.unwrap();
    }
    let per_timeline = started.elapsed() / rounds;
    println!(
        "graph: {per_graph:?} a request, timeline: {per_timeline:?} a request, over {events} events"
    );
    let bound = Duration::from_millis(500);
    assert!(per_graph < bound, "a graph took {per_graph:?}");
    assert!(per_timeline < bound, "a timeline took {per_timeline:?}");
}

#[tokio::test]
async fn the_views_are_the_same_after_the_index_is_rebuilt_from_the_logs() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let scene = scene(&store).await;
    let mut before = Vec::new();
    for moment in scene.moments {
        before.push(store.graph(&scene.root, Some(moment)).await.unwrap());
    }
    before.push(store.graph(&scene.child, None).await.unwrap());
    let timeline = store.timeline(&scene.root).await.unwrap();
    assert!(store.open_questions(None).await.unwrap().is_empty());
    store.close().await;

    // The index is gone: everything comes back from the logs.
    for name in ["calin.db", "calin.db-wal", "calin.db-shm"] {
        let _ = std::fs::remove_file(dir.path().join(name));
    }
    let store = open(&dir).await;
    let mut after = Vec::new();
    for moment in scene.moments {
        after.push(store.graph(&scene.root, Some(moment)).await.unwrap());
    }
    after.push(store.graph(&scene.child, None).await.unwrap());
    assert_eq!(after, before);
    assert_eq!(store.timeline(&scene.root).await.unwrap(), timeline);
    assert_eq!(store.timeline(&scene.child).await.unwrap(), timeline);
    // The child is still a child.
    let child = store.session(&scene.child).await.unwrap().session;
    assert_eq!(child.parent_session_id.as_ref(), Some(&scene.root));
    assert_eq!(child.root_session_id, scene.root);
    let setup = store.setup(&scene.child).await.unwrap();
    assert_eq!(setup.dispatch.unwrap().lead_agent_id, "CL");
}

#[tokio::test]
async fn questions_are_projected_as_open_until_they_are_answered() {
    let dir = TempDir::new().unwrap();
    let store = open(&dir).await;
    let id = store.create_session(new_session("p")).await.unwrap().id;
    let question = calin_proto::Question {
        id: "q1".to_owned(),
        text: "Which color?".to_owned(),
        options: vec!["blue".to_owned()],
        gate: false,
    };
    let asked = EventDraft::new(EventBody::QuestionAsked { question }).agent("L");
    let asked = add(&store, &id, asked).await;
    assert_eq!(
        store.question_answered(&id, "q1").await.unwrap(),
        Some(false)
    );
    assert_eq!(store.question_answered(&id, "q2").await.unwrap(), None);
    let open_now = store.open_questions(Some(&id)).await.unwrap();
    assert_eq!(
        open_now,
        [OpenQuestion {
            session_id: id.clone(),
            question_id: "q1".to_owned(),
            agent_id: Some("L".to_owned()),
            event_id: asked.id.clone(),
        }]
    );
    assert_eq!(store.open_questions(None).await.unwrap(), open_now);

    let answered = EventBody::QuestionAnswered {
        question_id: "q1".to_owned(),
        answer: "blue".to_owned(),
        by: DecisionSource::User,
    };
    add(&store, &id, EventDraft::new(answered).agent("L")).await;
    assert_eq!(
        store.question_answered(&id, "q1").await.unwrap(),
        Some(true)
    );
    assert!(store.open_questions(None).await.unwrap().is_empty());
}
