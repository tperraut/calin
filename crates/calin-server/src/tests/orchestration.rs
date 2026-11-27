//! Child sessions, notes, reports and questions through the tool endpoint,
//! and the routes for answering a question and reading the graph and the
//! timeline.

use calin_agents::{RunnerInput, RunnerOutput};
use calin_proto::{EdgeKind, Graph, NodeKind, Timeline};
use serde_json::Value;

use super::catalog::{Agent, agent, call_tool, endpoint_of};
use super::*;

/// The agent of a session dispatched by `parent`: its id and its endpoint.
struct Child {
    session_id: String,
    endpoint: String,
    io: calin_agents::AgentIo,
}

/// Dispatches a child through the parent's tools and takes its agent.
async fn dispatch(parent: &mut Agent, title: &str) -> (String, Child) {
    let arguments = json!({ "title": title, "brief": "Do the thing.\nThen report." });
    let (text, is_error) = parent.tool("dispatch", arguments).await;
    assert!(!is_error, "{text}");
    let (params, io) = tokio::time::timeout(TIMEOUT, parent.starts.recv())
        .await
        .unwrap()
        .unwrap();
    let child = Child {
        session_id: params.session_id.clone(),
        endpoint: endpoint_of(&params),
        io,
    };
    (text, child)
}

async fn next_input(io: &mut calin_agents::AgentIo) -> String {
    match tokio::time::timeout(TIMEOUT, io.inputs.recv())
        .await
        .unwrap()
    {
        Some(RunnerInput::UserMessage(text)) => text,
        other => panic!("expected a message for the agent, got {other:?}"),
    }
}

#[tokio::test]
async fn an_agent_dispatches_a_child_and_gets_its_report() {
    let mut lead = agent().await;
    let app = lead.app.clone();
    // Uncommitted work in the parent's worktree is said, not handed over.
    let detail: SessionDetail = get(&app, &format!("/api/v1/sessions/{}", lead.session_id))
        .await
        .json();
    std::fs::write(Path::new(&detail.repos[0].path).join("scratch.txt"), "wip").unwrap();

    let (text, child) = dispatch(&mut lead, "Write the docs").await;
    assert!(text.starts_with(&format!(
        "Dispatched child session \"Write the docs\" (id {}).\n",
        child.session_id
    )));
    assert!(
        text.contains("starts at your current commit in: api (commit "),
        "{text}"
    );
    assert!(
        text.contains("your worktree of api has uncommitted changes"),
        "{text}"
    );
    assert!(text.contains("end your turn"), "{text}");

    // The child is a session like any other, under its parent.
    let sessions: Vec<Session> = get(&app, "/api/v1/sessions").await.json();
    let listed = sessions
        .iter()
        .find(|session| session.id == child.session_id);
    let listed = listed.expect("children are listed");
    assert_eq!(listed.parent_session_id.as_ref(), Some(&lead.session_id));
    assert_eq!(listed.root_session_id, lead.session_id);
    assert_eq!(listed.title, "Write the docs");
    wait_for(&app, &child.session_id, SessionStatus::Running).await;

    // A child has `report`, and cannot dispatch.
    let (text, is_error) = call_tool(
        &app,
        &child.endpoint,
        "dispatch",
        json!({ "title": "x", "brief": "y" }),
    )
    .await;
    assert!(
        is_error && text.contains("a child session cannot dispatch"),
        "{text}"
    );
    let (text, is_error) = call_tool(&app, &child.endpoint, "report", json!({})).await;
    assert!(
        is_error && text.contains("report needs `summary`"),
        "{text}"
    );
    let summary = json!({ "summary": "Wrote the page.\nNothing left." });
    let (text, is_error) = call_tool(&app, &child.endpoint, "report", summary).await;
    assert!(!is_error && text.starts_with("Recorded."), "{text}");
    assert!(text.contains("End your turn now"));
    // Only a child reports.
    let (text, is_error) = lead.tool("report", json!({ "summary": "done" })).await;
    assert!(is_error && text.contains("nobody to report to"), "{text}");

    // The parent is busy; the report reaches it when its turn ends.
    child
        .io
        .outputs
        .send(RunnerOutput::TurnEnded)
        .await
        .unwrap();
    wait_for(&app, &child.session_id, SessionStatus::Review).await;
    lead.io.outputs.send(RunnerOutput::TurnEnded).await.unwrap();
    let handed = next_input(&mut lead.io).await;
    assert!(handed.starts_with(
        "[Calin] Report from child session \"Write the docs\" (review)\n\nWrote the page.\n"
    ));
    // It committed nothing, and the report says so.
    assert!(handed.contains("- api: branch `calin/"), "{handed}");
    assert!(handed.contains("nothing to merge"), "{handed}");

    // Four children at a time.
    let mut running = Vec::new();
    for title in ["two", "three", "four", "five"] {
        running.push(dispatch(&mut lead, title).await.1);
    }
    let (text, is_error) = lead
        .tool("dispatch", json!({ "title": "six", "brief": "too many" }))
        .await;
    assert!(is_error, "{text}");
    assert!(
        text.contains("4 child sessions of this session are still running"),
        "{text}"
    );
    assert!(text.contains("End your turn"), "{text}");
}

#[tokio::test]
async fn the_orchestration_tools_say_what_is_wrong_in_words_a_model_can_act_on() {
    let mut lead = agent().await;
    let app = lead.app.clone();
    let sessions = || async {
        let listed: Vec<Session> = get(&app, "/api/v1/sessions").await.json();
        listed.len()
    };
    let refused = |outcome: (String, bool)| {
        assert!(outcome.1, "{} should be an error", outcome.0);
        outcome.0
    };

    // dispatch: its arguments, then the repos it names.
    for arguments in [
        json!({}),
        json!({ "brief": "no title" }),
        json!({ "title": "  ", "brief": "blank title" }),
        json!({ "title": 7, "brief": "not a string" }),
    ] {
        let text = refused(lead.tool("dispatch", arguments).await);
        assert_eq!(text, "dispatch needs `title`: a string that is not blank.");
    }
    let text = refused(lead.tool("dispatch", json!({ "title": "t" })).await);
    assert_eq!(text, "dispatch needs `brief`: a string that is not blank.");
    for repos in [json!("api"), json!(["api", 3])] {
        let arguments = json!({ "title": "t", "brief": "b", "repos": repos });
        let text = refused(lead.tool("dispatch", arguments).await);
        assert_eq!(text, "`repos` must be a list of strings.");
    }
    let arguments = json!({ "title": "t", "brief": "b", "repos": ["nope"] });
    let text = refused(lead.tool("dispatch", arguments).await);
    assert!(
        text.contains("no repository named nope is attached to this session"),
        "{text}"
    );
    assert!(text.contains("Attached: api."), "{text}");
    // `docs` joins read-only: not something a child can be given.
    let (_, is_error) = lead
        .tool("attach_repo", json!({ "name": "docs", "mode": "read" }))
        .await;
    assert!(!is_error);
    let arguments = json!({ "title": "t", "brief": "b", "repos": ["docs"] });
    let text = refused(lead.tool("dispatch", arguments).await);
    assert!(
        text.contains("docs is attached to this session in read mode"),
        "{text}"
    );
    assert!(text.contains("write mode"), "{text}");
    assert_eq!(sessions().await, 1);

    // send_message.
    let text = refused(lead.tool("send_message", json!({ "text": "hi" })).await);
    assert_eq!(text, "send_message needs `to`: a string that is not blank.");
    let text = refused(lead.tool("send_message", json!({ "to": "user" })).await);
    assert_eq!(
        text,
        "send_message needs `text`: a string that is not blank."
    );
    let to_parent = json!({ "to": "parent", "text": "hi" });
    let text = refused(lead.tool("send_message", to_parent.clone()).await);
    assert!(text.contains("has no parent"), "{text}");
    let (_, child) = dispatch(&mut lead, "Helper").await;
    let stranger = json!({ "to": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "text": "hi" });
    let text = refused(lead.tool("send_message", stranger).await);
    assert!(
        text.contains("is not `parent`, `user` or the id of a child session"),
        "{text}"
    );
    assert!(
        text.contains(&format!("{} (\"Helper\")", child.session_id)),
        "{text}"
    );

    let (text, is_error) = lead
        .tool("send_message", json!({ "to": "user", "text": "FYI" }))
        .await;
    assert!(
        !is_error && text.starts_with("Noted for the user."),
        "{text}"
    );
    let down = json!({ "to": child.session_id, "text": "Use tabs." });
    let (text, is_error) = lead.tool("send_message", down).await;
    assert!(!is_error && text.starts_with("Sent."), "{text}");
    let (text, is_error) = call_tool(&app, &child.endpoint, "send_message", to_parent).await;
    assert!(!is_error && text.starts_with("Sent."), "{text}");

    // ask_user.
    let text = refused(lead.tool("ask_user", json!({ "question": "" })).await);
    assert_eq!(
        text,
        "ask_user needs `question`: a string that is not blank."
    );
    let wrong = json!({ "question": "Which?", "options": "a or b" });
    let text = refused(lead.tool("ask_user", wrong).await);
    assert_eq!(text, "`options` must be a list of strings.");
    let asked = json!({ "question": "Which?", "options": ["a", "b"] });
    let (text, is_error) = lead.tool("ask_user", asked).await;
    assert!(!is_error && text.contains("End your turn now"), "{text}");
    let text = refused(lead.tool("ask_user", json!({ "question": "And?" })).await);
    assert!(
        text.contains("a question to the user is already open"),
        "{text}"
    );

    // A tool this server does not have is a protocol error, not a tool error.
    let unknown = lead
        .rpc(
            json!(9),
            "tools/call",
            json!({ "name": "merge", "arguments": {} }),
        )
        .await;
    assert_eq!(unknown["error"]["code"], -32602);
}

#[tokio::test]
async fn a_question_is_answered_through_the_api() {
    let mut lead = agent().await;
    let app = lead.app.clone();
    let id = lead.session_id.clone();
    let asked = json!({ "question": "Which color?", "options": ["blue", "red"] });
    let (text, is_error) = lead.tool("ask_user", asked).await;
    assert!(!is_error, "{text}");
    let events = events_until(&lead.state, &id, is_waiting).await;
    let question = events.iter().find_map(|event| match &event.body {
        EventBody::QuestionAsked { question } => Some(question.clone()),
        _ => None,
    });
    let question = question.expect("the question is on record");
    assert_eq!(question.text, "Which color?");
    assert_eq!(question.options, ["blue", "red"]);
    assert!(
        text.contains(&format!("question {}", question.id)),
        "{text}"
    );
    // The agent ends its turn; the session waits for the user.
    lead.io.outputs.send(RunnerOutput::TurnEnded).await.unwrap();
    events_until(&lead.state, &id, |event| {
        matches!(event.body, EventBody::AgentStatus { .. })
    })
    .await;
    wait_for(&app, &id, SessionStatus::Waiting).await;

    let path = format!("/api/v1/sessions/{id}/questions/{}", question.id);
    // 400: a blank answer, a body that is not the request.
    post(&app, &path, json!({ "answer": "  " }))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    post(&app, &path, json!({ "text": "blue" }))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    // 404: an unknown question, an unknown session.
    let unknown = format!("/api/v1/sessions/{id}/questions/nope");
    post(&app, &unknown, json!({ "answer": "blue" }))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let nowhere = format!(
        "/api/v1/sessions/{UNKNOWN_SESSION}/questions/{}",
        question.id
    );
    post(&app, &nowhere, json!({ "answer": "blue" }))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    // Only POST.
    assert_eq!(
        get(&app, &path).await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );

    let reply = post(&app, &path, json!({ "answer": "blue" })).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    assert_eq!(
        next_input(&mut lead.io).await,
        "[Calin] The user answered: blue"
    );
    let detail = wait_for(&app, &id, SessionStatus::Running).await;
    assert_eq!(detail.agents[0].id, lead.agent_id);
    // 409: once it is answered.
    post(&app, &path, json!({ "answer": "red" }))
        .await
        .assert_error(StatusCode::CONFLICT);
    // On record once, as the user's answer.
    let subscription = lead.state.manager().store().subscribe(&id, 0).await;
    let answers: Vec<(String, DecisionSource)> = subscription
        .unwrap()
        .backlog
        .into_iter()
        .filter_map(|event| match event.body {
            EventBody::QuestionAnswered { answer, by, .. } => Some((answer, by)),
            _ => None,
        })
        .collect();
    assert_eq!(answers, [("blue".to_owned(), DecisionSource::User)]);
}

#[tokio::test]
async fn the_graph_and_the_timeline_of_a_session_tree_are_served() {
    let mut lead = agent().await;
    let app = lead.app.clone();
    let id = lead.session_id.clone();
    let (_, child) = dispatch(&mut lead, "Helper").await;
    wait_for(&app, &child.session_id, SessionStatus::Running).await;
    events_until(&lead.state, &child.session_id, |event| {
        matches!(event.body, EventBody::AgentSpawned { .. })
    })
    .await;

    let reply = get(&app, &format!("/api/v1/sessions/{id}/graph")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.content_type(), "application/json");
    let graph: Graph = reply.json();
    assert_eq!(graph.root_session_id, id);
    assert_eq!(graph.at, graph.to_ts);
    let kinds: Vec<NodeKind> = graph.nodes.iter().map(|node| node.kind).collect();
    assert_eq!(
        kinds,
        [
            NodeKind::User,
            NodeKind::Agent,
            NodeKind::Agent,
            NodeKind::Repo
        ]
    );
    assert_eq!(graph.nodes[1].id, format!("agent:{}", lead.agent_id));
    let dispatched = graph
        .edges
        .iter()
        .find(|edge| edge.kind == EdgeKind::Dispatched);
    assert_eq!(dispatched.unwrap().label.as_deref(), Some("Do the thing."));
    // The same tree from any of its sessions.
    let from_child: Graph = get(
        &app,
        &format!("/api/v1/sessions/{}/graph", child.session_id),
    )
    .await
    .json();
    assert_eq!(from_child, graph);

    // `at` asks for the past, and clamps to the tree's time range.
    let first: Graph = get(&app, &format!("/api/v1/sessions/{id}/graph?at=0"))
        .await
        .json();
    assert_eq!(first.at, graph.from_ts);
    assert!(first.nodes.len() < graph.nodes.len());
    assert!(first.edges.is_empty());
    let last: Graph = get(
        &app,
        &format!("/api/v1/sessions/{id}/graph?at={}", i64::MAX),
    )
    .await
    .json();
    assert_eq!(last, graph);
    let exact = format!("/api/v1/sessions/{id}/graph?at={}", graph.to_ts);
    assert_eq!(get(&app, &exact).await.json::<Graph>(), graph);
    // 400 for an `at` that is no time, 404 for an unknown session.
    for at in ["yesterday", "1.5", ""] {
        get(&app, &format!("/api/v1/sessions/{id}/graph?at={at}"))
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    get(&app, &format!("/api/v1/sessions/{UNKNOWN_SESSION}/graph"))
        .await
        .assert_error(StatusCode::NOT_FOUND);

    let reply = get(
        &app,
        &format!("/api/v1/sessions/{}/timeline", child.session_id),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let timeline: Timeline = reply.json();
    assert_eq!(timeline.root_session_id, id);
    assert_eq!(
        (timeline.from_ts, timeline.to_ts),
        (graph.from_ts, graph.to_ts)
    );
    let lanes: Vec<&str> = timeline
        .lanes
        .iter()
        .map(|lane| lane.session_id.as_str())
        .collect();
    assert_eq!(lanes, [id.as_str(), child.session_id.as_str()]);
    assert_eq!(
        timeline.lanes[1].parent_id,
        Some(format!("agent:{}", lead.agent_id))
    );
    // Both turns are still running.
    assert!(
        timeline
            .lanes
            .iter()
            .all(|lane| lane.spans[0].end.is_none())
    );
    assert_eq!(timeline.arrows.len(), 1);
    assert_eq!(timeline.arrows[0].kind, EdgeKind::Dispatched);
    get(
        &app,
        &format!("/api/v1/sessions/{UNKNOWN_SESSION}/timeline"),
    )
    .await
    .assert_error(StatusCode::NOT_FOUND);
    // Reading only.
    for view in ["graph", "timeline"] {
        let reply = post_empty(&app, &format!("/api/v1/sessions/{id}/{view}")).await;
        assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    }
    let _: Value = get(&app, &format!("/api/v1/sessions/{id}/timeline"))
        .await
        .json();
}
