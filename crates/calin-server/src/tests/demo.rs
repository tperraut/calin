//! The mock agent's demo directives through the real orchestrator and the
//! real tool endpoint, served on a loopback socket: the dry run a person
//! would try in the web app.

use calin_proto::{EdgeKind, Graph, ItemContent, ItemStatus, MessageKind, NodeKind};

use super::*;

/// A server in dry run on an ephemeral port, and a router on the same
/// state for the REST calls the tests make themselves.
async fn served(dir: &TempDir) -> (Running, Router) {
    let running = start(dir).await;
    let app = router(running.state.clone());
    (running, app)
}

/// The texts the session's agents wrote, in the order they finished.
fn texts(events: &[Event]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemCompleted { item } => match &item.content {
                ItemContent::Text { text } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Each tool call of the session: its name and how it ended.
fn tool_calls(events: &[Event]) -> Vec<(String, ItemStatus)> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::ItemCompleted { item } => match &item.content {
                ItemContent::Tool { name, .. } => Some((name.clone(), item.status)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn is_summary(event: &Event) -> bool {
    matches!(
        &event.body,
        EventBody::ItemCompleted { item } if matches!(
            &item.content,
            ItemContent::Text { text } if text.starts_with("Summary: ")
        )
    )
}

fn statuses(events: &[Event]) -> Vec<SessionStatus> {
    events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::SessionStatus { status, .. } => Some(*status),
            _ => None,
        })
        .collect()
}

async fn children(app: &Router, parent: &str) -> Vec<Session> {
    let sessions: Vec<Session> = get(app, "/api/v1/sessions").await.json();
    let mut children: Vec<Session> = sessions
        .into_iter()
        .filter(|session| session.parent_session_id.as_deref() == Some(parent))
        .collect();
    children.sort_by(|a, b| a.title.cmp(&b.title));
    children
}

const PART_A: &str = "Ship the health check: part A";
const PART_B: &str = "Ship the health check: part B";

/// What every orchestrated tree ends as, whatever the permission mode.
async fn check_finished_tree(running: &Running, app: &Router, lead: &str) {
    let events = events_until(&running.state, lead, is_summary).await;
    wait_for(app, lead, SessionStatus::Review).await;

    let children = children(app, lead).await;
    let titles: Vec<&str> = children.iter().map(|child| child.title.as_str()).collect();
    assert_eq!(titles, [PART_A, PART_B]);
    for child in &children {
        let detail = wait_for(app, &child.id, SessionStatus::Review).await;
        assert_eq!(detail.session.root_session_id, lead);
        let events = events_until(&running.state, &child.id, is_review).await;
        let calls = tool_calls(&events);
        assert_eq!(
            calls,
            [
                ("Edit".to_owned(), ItemStatus::Completed),
                ("mcp__calin__report".to_owned(), ItemStatus::Completed)
            ]
        );
        // The edit is real: the notes file is in the child's own folder.
        let notes = std::fs::read_to_string(
            running
                .state
                .manager()
                .store()
                .home()
                .work_dir(&child.id)
                .join("calin-mock-notes.md"),
        )
        .unwrap();
        assert!(notes.contains("Part "), "{notes}");
    }

    // Two reports reached the lead, each acknowledged, then the summary.
    let reports = events
        .iter()
        .filter(|event| {
            matches!(
                &event.body,
                EventBody::MessageReceived { message } if message.kind == MessageKind::Report
            )
        })
        .count();
    assert_eq!(reports, 2);
    let texts = texts(&events);
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert!(texts[0].starts_with("I'll split \"Ship the health check\""));
    let mut acknowledged = vec![texts[1].clone(), texts[2].clone()];
    acknowledged.sort();
    assert_eq!(
        acknowledged,
        [
            format!("Got the report from \"{PART_A}\"."),
            format!("Got the report from \"{PART_B}\"."),
        ]
    );
    assert!(texts[3].starts_with("Summary: All 2 parts are done: "));
    assert_eq!(
        tool_calls(&events),
        [
            ("mcp__calin__dispatch".to_owned(), ItemStatus::Completed),
            ("mcp__calin__dispatch".to_owned(), ItemStatus::Completed),
            ("mcp__calin__send_message".to_owned(), ItemStatus::Completed),
        ]
    );

    // The graph has the user, the lead, both children's leads, and the
    // edges between them.
    let graph: Graph = get(app, &format!("/api/v1/sessions/{lead}/graph"))
        .await
        .json();
    let agents: Vec<(&str, Option<&str>)> = graph
        .nodes
        .iter()
        .filter(|node| node.kind == NodeKind::Agent)
        .map(|node| (node.label.as_str(), node.session_title.as_deref()))
        .collect();
    assert_eq!(agents.len(), 3, "{:?}", graph.nodes);
    assert_eq!(graph.nodes[0].kind, NodeKind::User);
    let lead_node = graph
        .nodes
        .iter()
        .find(|node| node.session_id.as_deref() == Some(lead) && node.kind == NodeKind::Agent)
        .unwrap();
    assert_eq!(lead_node.parent_id.as_deref(), Some("user"));
    for child in &children {
        let node = graph
            .nodes
            .iter()
            .find(|node| node.session_id.as_deref() == Some(child.id.as_str()))
            .expect("a node for each child's lead");
        assert_eq!(node.parent_id.as_ref(), Some(&lead_node.id));
        let between = |kind: EdgeKind, from: &str, to: &str| {
            graph
                .edges
                .iter()
                .any(|edge| edge.kind == kind && edge.from == from && edge.to == to)
        };
        assert!(
            between(EdgeKind::Dispatched, &lead_node.id, &node.id),
            "{:?}",
            graph.edges
        );
        assert!(
            between(EdgeKind::Report, &node.id, &lead_node.id),
            "{:?}",
            graph.edges
        );
    }
    let kinds: Vec<EdgeKind> = graph.edges.iter().map(|edge| edge.kind).collect();
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == EdgeKind::Dispatched)
            .count(),
        2
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == EdgeKind::Report)
            .count(),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_orchestrate_builds_a_tree_of_two_children_that_report_back() {
    let dir = TempDir::new().unwrap();
    let (running, app) = served(&dir).await;
    let lead = create(&app, "Ship the health check demo: orchestrate")
        .await
        .session
        .id;
    check_finished_tree(&running, &app, &lead).await;

    // The lead never waited on the user; it rested in review between the
    // dispatch and the reports.
    let events = events_until(&running.state, &lead, is_summary).await;
    assert!(!statuses(&events).contains(&SessionStatus::Waiting));
    // The note to the user is on record in the lead's session.
    assert!(events.iter().any(|event| matches!(
        &event.body,
        EventBody::MessageReceived { message }
            if message.kind == MessageKind::Note && message.body.starts_with("Both parts are under way")
    )));
    let _ = running.stop.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn in_ask_mode_calins_own_tools_are_allowed_by_policy() {
    let dir = TempDir::new().unwrap();
    let (running, app) = served(&dir).await;
    let lead = create_with(
        &app,
        json!({ "prompt": "Ship the health check demo: orchestrate", "permission_mode": "ask" }),
    )
    .await
    .session
    .id;
    // Each child asks the user about its edit, outside any repo; that is the
    // only thing anyone is asked.
    for _ in 0..2 {
        let child = tokio::time::timeout(TIMEOUT, async {
            loop {
                let waiting = children(&app, &lead)
                    .await
                    .into_iter()
                    .find(|child| child.status == SessionStatus::Waiting);
                if let Some(child) = waiting {
                    return child;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("a child waits for its edit");
        let events = events_until(&running.state, &child.id, is_waiting).await;
        let [edit] = pending_requests(&events).try_into().expect("one request");
        assert_eq!(edit.tool_name, "Edit");
        let path = format!("/api/v1/sessions/{}/permissions/{}", child.id, edit.id);
        let reply = post(&app, &path, json!({ "decision": "allow" })).await;
        assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
        wait_for(&app, &child.id, SessionStatus::Review).await;
    }
    check_finished_tree(&running, &app, &lead).await;

    // Every call to Calin's tools asked, and the policy allowed it at once.
    let mut sessions = vec![lead.clone()];
    sessions.extend(
        children(&app, &lead)
            .await
            .into_iter()
            .map(|child| child.id),
    );
    for id in &sessions {
        let events = events_until(&running.state, id, |event| {
            is_summary(event) || (is_review(event) && id != &lead)
        })
        .await;
        let asked: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::PermissionRequested { request }
                    if request.tool_name.starts_with("mcp__calin__") =>
                {
                    Some(request.id.as_str())
                }
                _ => None,
            })
            .collect();
        assert!(!asked.is_empty());
        let by_policy: Vec<&str> = events
            .iter()
            .filter_map(|event| match &event.body {
                EventBody::PermissionResolved {
                    request_id,
                    decision: PermissionDecision::Allow,
                    by: DecisionSource::Policy,
                    ..
                } => Some(request_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(asked, by_policy, "in {id}");
    }
    let events = events_until(&running.state, &lead, is_summary).await;
    assert!(!statuses(&events).contains(&SessionStatus::Waiting));
    let _ = running.stop.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_ask_waits_for_the_user_and_echoes_the_answer() {
    let dir = TempDir::new().unwrap();
    let (running, app) = served(&dir).await;
    let id = create(&app, "Which database? demo: ask").await.session.id;
    let events = events_until(&running.state, &id, is_waiting).await;
    let question = events.iter().find_map(|event| match &event.body {
        EventBody::QuestionAsked { question } => Some(question.clone()),
        _ => None,
    });
    let question = question.expect("the question is on record");
    assert_eq!(
        question.text,
        "Which way should I go for \"Which database?\"?"
    );
    assert_eq!(question.options, ["The simple way", "The thorough way"]);
    wait_for(&app, &id, SessionStatus::Waiting).await;

    let path = format!("/api/v1/sessions/{id}/questions/{}", question.id);
    let reply = post(&app, &path, json!({ "answer": "The simple way" })).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    let echoed = |event: &Event| {
        matches!(
            &event.body,
            EventBody::ItemCompleted { item } if matches!(
                &item.content,
                ItemContent::Text { text } if text.starts_with("You answered:")
            )
        )
    };
    let events = events_until(&running.state, &id, echoed).await;
    assert_eq!(
        texts(&events).last().map(String::as_str),
        Some("You answered: The simple way. I'll go on with that.")
    );
    assert_eq!(
        tool_calls(&events),
        [("mcp__calin__ask_user".to_owned(), ItemStatus::Completed)]
    );
    wait_for(&app, &id, SessionStatus::Review).await;
    let _ = running.stop.send(());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn demo_note_sends_a_note_from_a_child_to_the_lead() {
    let dir = TempDir::new().unwrap();
    let (running, app) = served(&dir).await;
    let lead = create(&app, "Ship the health check demo: orchestrate demo: note")
        .await
        .session
        .id;
    let events = events_until(&running.state, &lead, is_summary).await;
    let children = children(&app, &lead).await;
    let part_b = &children[1];
    assert_eq!(part_b.title, PART_B);
    let notes: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::MessageReceived { message }
                if message.kind == MessageKind::Note && message.from_session_id == part_b.id =>
            {
                Some(message.body.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        notes,
        ["Half way: the notes file has its new line. The report follows."]
    );
    assert!(
        texts(&events).contains(&format!("Noted the update from \"{PART_B}\".")),
        "{:?}",
        texts(&events)
    );
    let graph: Graph = get(&app, &format!("/api/v1/sessions/{lead}/graph"))
        .await
        .json();
    assert!(
        graph.edges.iter().any(|edge| edge.kind == EdgeKind::Message
            && edge.label.as_deref()
                == Some("Half way: the notes file has its new line. The report follows.")),
        "{:?}",
        graph.edges
    );
    wait_for(&app, &lead, SessionStatus::Review).await;
    let _ = running.stop.send(());
}
