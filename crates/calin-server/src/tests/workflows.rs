//! Workflows over HTTP: the catalog, a session that runs one, its run, the
//! gate, and the session limit in the settings.

use calin_proto::{NodeRunState, Question, Settings, WorkflowInfo, WorkflowRun, WorkflowStatus};

use super::*;

const GATED: &str = r#"
version: 1
name: Gated
description: One turn, a check and a gate.
budget_usd: 3
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: tests, type: check, command: "true" }
  - { id: retry, type: loop, max: 2 }
  - { id: gate, type: gate, message: "Good enough?" }
  - { id: end, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: tests }
  - { from: tests.pass, to: gate }
  - { from: tests.fail, to: retry }
  - { from: retry.repeat, to: work }
  - { from: gate.approve, to: end }
"#;

const BROKEN: &str = "version: 3\nname: Broken\nnodes:\n  - { id: a, type: agent }\nedges: []\n";

/// A dry-run server whose home repo holds these workflow files.
struct Served {
    app: Router,
    state: AppState,
    home: PathBuf,
    _data: TempDir,
    _work: TempDir,
}

async fn served(files: &[(&str, &str)]) -> Served {
    let data = TempDir::new().unwrap();
    let work = TempDir::new().unwrap();
    let home = git_repo(work.path(), "home");
    for (name, yaml) in files {
        let path = home.join(".calin/workflows").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, yaml).unwrap();
    }
    let state = state(&data, true).await;
    let app = router(state.clone());
    let settings = json!({ "home_repo": home });
    let reply = send_json(&app, Method::PUT, "/api/v1/settings", settings).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    Served {
        app,
        state,
        home,
        _data: data,
        _work: work,
    }
}

async fn workflows(app: &Router) -> Vec<WorkflowInfo> {
    let reply = get(app, "/api/v1/workflows").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    reply.json()
}

#[tokio::test]
async fn workflows_are_listed_quick_first_then_by_id_with_graph_or_problem() {
    // Without a home repo there is the built-in one.
    let data = TempDir::new().unwrap();
    let bare = router(state(&data, true).await);
    let list = workflows(&bare).await;
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].id.as_str(), list[0].builtin), ("quick", true));
    assert_eq!(list[0].graph.as_ref().unwrap().nodes.len(), 3);

    let served = served(&[("gated.yaml", GATED), ("broken.yml", BROKEN)]).await;
    let list = workflows(&served.app).await;
    let ids: Vec<&str> = list.iter().map(|info| info.id.as_str()).collect();
    assert_eq!(ids, ["quick", "broken", "gated"]);

    let gated = &list[2];
    assert_eq!(gated.name, "Gated");
    assert_eq!(
        gated.description.as_deref(),
        Some("One turn, a check and a gate.")
    );
    assert_eq!(gated.budget_usd, Some(3.0));
    assert!(!gated.builtin);
    assert_eq!(gated.problem, None);
    let graph = gated.graph.as_ref().unwrap();
    assert_eq!(graph.nodes.len(), 6);
    assert_eq!(graph.edges.len(), 6);
    assert_eq!(graph.nodes[1].kind, "agent");
    assert_eq!(graph.nodes[4].ports[1].name, "reject");
    assert!(graph.nodes[4].ports[1].failure);

    let broken = &list[1];
    assert_eq!(broken.graph, None);
    let problem = broken.problem.as_deref().unwrap();
    assert!(problem.lines().count() >= 2, "{problem}");

    // An edit shows without a restart or a refresh.
    let renamed = GATED.replace("name: Gated", "name: Gated twice");
    std::fs::write(served.home.join(".calin/workflows/gated.yaml"), renamed).unwrap();
    assert_eq!(workflows(&served.app).await[2].name, "Gated twice");
    // Refreshing the repos reads them again too.
    std::fs::remove_file(served.home.join(".calin/workflows/broken.yml")).unwrap();
    let reply = post_empty(&served.app, "/api/v1/repos/refresh").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(workflows(&served.app).await.len(), 2);
    let reply = post_empty(&served.app, "/api/v1/workflows").await;
    reply.assert_error(StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn a_session_names_a_workflow_that_exists_and_validates_or_none() {
    let served = served(&[("gated.yaml", GATED), ("broken.yml", BROKEN)]).await;
    let app = &served.app;
    for (workflow, says) in [("nope", "unknown workflow nope"), ("broken", "version")] {
        let body = json!({ "prompt": "do it", "workflow": workflow });
        let reply = post(app, "/api/v1/sessions", body).await;
        reply.assert_error(StatusCode::BAD_REQUEST);
        let error = reply.json::<ApiError>().error;
        assert!(error.contains(says), "{workflow}: {error}");
    }
    let reply = post(
        app,
        "/api/v1/sessions",
        json!({ "prompt": "p", "workflow": 7 }),
    )
    .await;
    reply.assert_error(StatusCode::BAD_REQUEST);
    let sessions: Vec<Session> = get(app, "/api/v1/sessions").await.json();
    assert!(sessions.is_empty());

    // `quick` and no workflow are the same session: it has no run.
    for body in [
        json!({ "prompt": "plain", "permission_mode": "autonomous" }),
        json!({ "prompt": "plain", "permission_mode": "autonomous", "workflow": "quick" }),
    ] {
        let detail = create_with(app, body).await;
        assert_eq!(detail.session.workflow, None);
        let id = detail.session.id;
        wait_for(app, &id, SessionStatus::Review).await;
        let reply = get(app, &format!("/api/v1/sessions/{id}/workflow")).await;
        reply.assert_error(StatusCode::NOT_FOUND);
        let events = events_until(&served.state, &id, is_review).await;
        let run_events = events.iter().filter(|event| {
            matches!(
                event.body,
                EventBody::WorkflowStarted { .. }
                    | EventBody::WorkflowNode { .. }
                    | EventBody::WorkflowFinished { .. }
            )
        });
        assert_eq!(run_events.count(), 0);
    }
    let path = format!("/api/v1/sessions/{UNKNOWN_SESSION}/workflow");
    get(app, &path).await.assert_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_run_is_served_and_its_gate_takes_only_approve_or_reject() {
    let served = served(&[("gated.yaml", GATED)]).await;
    let app = &served.app;
    let body = json!({ "prompt": "Add it", "permission_mode": "autonomous", "workflow": "gated" });
    let detail = create_with(app, body).await;
    assert_eq!(detail.session.workflow.as_deref(), Some("gated"));
    let id = detail.session.id;
    let waiting = wait_for(app, &id, SessionStatus::Waiting).await;
    assert_eq!(waiting.session.workflow.as_deref(), Some("gated"));
    let listed: Vec<Session> = get(app, "/api/v1/sessions").await.json();
    assert_eq!(listed[0].workflow.as_deref(), Some("gated"));

    let run_path = format!("/api/v1/sessions/{id}/workflow");
    let reply = get(app, &run_path).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let run: WorkflowRun = reply.json();
    assert_eq!(run.workflow_id, "gated");
    assert_eq!(run.name, "Gated");
    assert_eq!(run.status, WorkflowStatus::Running);
    assert_eq!(run.budget_usd, Some(3.0));
    // One turn of the mock agent.
    assert_eq!(run.cost_usd, 0.0125);
    assert_eq!(run.nodes.len(), run.graph.nodes.len());
    let states: Vec<NodeRunState> = run.nodes.iter().map(|node| node.state).collect();
    use NodeRunState::{Active, Finished, Idle};
    assert_eq!(states, [Finished, Finished, Finished, Idle, Active, Idle]);
    assert_eq!(
        (run.nodes[3].iteration, run.nodes[3].max),
        (Some(0), Some(2))
    );
    assert_eq!(run.taken.len(), 3);

    let events = events_until(&served.state, &id, is_waiting).await;
    let question: Question = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::QuestionAsked { question } => Some(question.clone()),
            _ => None,
        })
        .unwrap();
    assert!(question.gate);
    assert_eq!(question.text, "Good enough?");
    assert_eq!(question.options, ["approve", "reject"]);

    let answer_path = format!("/api/v1/sessions/{id}/questions/{}", question.id);
    for body in [
        json!({ "answer": "maybe" }),
        json!({ "answer": "yes", "comment": "approve" }),
        json!({ "answer": "  " }),
        json!({ "comment": "no answer" }),
    ] {
        let reply = post(app, &answer_path, body.clone()).await;
        reply.assert_error(StatusCode::BAD_REQUEST);
    }
    let detail: SessionDetail = get(app, &format!("/api/v1/sessions/{id}")).await.json();
    assert_eq!(detail.session.status, SessionStatus::Waiting);
    let elsewhere = format!("/api/v1/sessions/{id}/questions/nope");
    let reply = post(app, &elsewhere, json!({ "answer": "approve" })).await;
    reply.assert_error(StatusCode::NOT_FOUND);

    let body = json!({ "answer": "approve", "comment": "ship it" });
    let reply = post(app, &answer_path, body).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    wait_for(app, &id, SessionStatus::Review).await;
    let reply = post(app, &answer_path, json!({ "answer": "reject" })).await;
    reply.assert_error(StatusCode::CONFLICT);

    let run: WorkflowRun = get(app, &run_path).await.json();
    assert_eq!(run.status, WorkflowStatus::Success);
    assert_eq!(run.reason, None);
    assert_eq!(run.nodes[4].last_port.as_deref(), Some("approve"));
    assert_eq!(run.nodes[4].outcome.as_deref(), Some("approve: ship it"));
    assert_eq!(run.taken.len(), 4);
}

#[tokio::test]
async fn the_session_limit_is_a_setting_with_a_default_and_a_range() {
    let data = TempDir::new().unwrap();
    let app = router(state(&data, true).await);
    let defaults: Settings = get(&app, "/api/v1/settings").await.json();
    assert_eq!(defaults.max_parallel, Some(4));
    for limit in [json!(0), json!(65), json!(-1), json!("many")] {
        let body = json!({ "max_parallel": limit });
        let reply = send_json(&app, Method::PUT, "/api/v1/settings", body).await;
        reply.assert_error(StatusCode::BAD_REQUEST);
    }
    let body = json!({ "max_parallel": 1 });
    let reply = send_json(&app, Method::PUT, "/api/v1/settings", body).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.json::<Settings>().max_parallel, Some(1));
}

const HOLD: &str = r#"
version: 1
name: Hold a slot
nodes:
  - { id: start, type: start }
  - { id: hold, type: check, command: "sleep 30" }
  - { id: end, type: end }
edges:
  - { from: start, to: hold }
  - { from: hold.pass, to: end }
"#;

#[tokio::test]
async fn a_session_beyond_the_limit_stays_queued_until_a_slot_frees() {
    let served = served(&[("hold.yaml", HOLD)]).await;
    let app = &served.app;
    let settings = json!({ "home_repo": served.home, "max_parallel": 1 });
    let reply = send_json(app, Method::PUT, "/api/v1/settings", settings).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());

    let holder = create_with(app, json!({ "prompt": "hold", "workflow": "hold" })).await;
    let holder = holder.session.id;
    wait_for(app, &holder, SessionStatus::Running).await;
    let mut waiting = Vec::new();
    for prompt in ["second", "third"] {
        let body = json!({ "prompt": prompt, "permission_mode": "autonomous" });
        let created = create_with(app, body).await;
        assert_eq!(created.session.status, SessionStatus::Queued);
        let events = events_until(&served.state, &created.session.id, |event| {
            matches!(event.body, EventBody::UserMessage { .. })
        })
        .await;
        let EventBody::SessionStatus { status, reason } = &events[0].body else {
            panic!("the first event is the session's status");
        };
        assert_eq!(*status, SessionStatus::Queued);
        assert!(reason.as_deref().unwrap().starts_with("waiting for a slot"));
        waiting.push(created.session.id);
    }

    // Stopping the one in the queue cancels it; it never ran.
    let path = format!("/api/v1/sessions/{}/stop", waiting[0]);
    assert_eq!(post_empty(app, &path).await.status, StatusCode::ACCEPTED);
    let cancelled = wait_for(app, &waiting[0], SessionStatus::Cancelled).await;
    assert!(cancelled.agents.is_empty());
    let third: SessionDetail = get(app, &format!("/api/v1/sessions/{}", waiting[1]))
        .await
        .json();
    assert_eq!(third.session.status, SessionStatus::Queued);

    // Stopping the one that runs frees its slot for the next in line.
    let path = format!("/api/v1/sessions/{holder}/stop");
    assert_eq!(post_empty(app, &path).await.status, StatusCode::ACCEPTED);
    wait_for(app, &waiting[1], SessionStatus::Review).await;
    let run: WorkflowRun = get(app, &format!("/api/v1/sessions/{holder}/workflow"))
        .await
        .json();
    assert_eq!(run.status, WorkflowStatus::Failed);
    assert_eq!(run.reason.as_deref(), Some("the session was stopped"));
}
