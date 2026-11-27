//! The routes of `docs/specs/2026-11-28-integrations.md`, with each status
//! the contract documents. Issues come from the dry run's samples; GitHub
//! itself is never asked.

use calin_proto::{
    Automation, AutomationOutcome, AutomationRun, CronPreview, Issue, IssueDetail, RunnerLimits,
    Usage, UsageBy,
};

use super::*;

struct Served {
    _dir: TempDir,
    _clones: TempDir,
    _state: AppState,
    app: Router,
    repo: Repo,
}

async fn served(dry_run: bool) -> Served {
    let dir = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let state = state(&dir, dry_run).await;
    let app = router(state.clone());
    let repo = register(&app, &git_repo(clones.path(), "api")).await;
    Served {
        _dir: dir,
        _clones: clones,
        _state: state,
        app,
        repo,
    }
}

async fn put(app: &Router, path: &str, body: serde_json::Value) -> Reply {
    send_json(app, Method::PUT, path, body).await
}

#[tokio::test]
async fn issues_are_listed_and_shown_with_the_documented_statuses() {
    let s = served(true).await;
    let base = format!("/api/v1/repos/{}/issues", s.repo.id);
    let reply = get(&s.app, &base).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let open: Vec<Issue> = reply.json();
    assert_eq!(open.iter().map(|i| i.number).collect::<Vec<_>>(), [4, 3, 2]);
    let closed: Vec<Issue> = get(&s.app, &format!("{base}?state=closed")).await.json();
    assert_eq!(closed.len(), 1);
    let found: Vec<Issue> = get(&s.app, &format!("{base}?state=open&q=dark%20theme"))
        .await
        .json();
    assert_eq!(found.len(), 1);
    get(&s.app, &format!("{base}?state=merged"))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    get(&s.app, "/api/v1/repos/nope/issues")
        .await
        .assert_error(StatusCode::NOT_FOUND);

    let reply = get(&s.app, &format!("{base}/3")).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let detail: IssueDetail = reply.json();
    assert_eq!(detail.issue, open[1]);
    assert!(!detail.body.is_empty());
    get(&s.app, &format!("{base}/99"))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    get(&s.app, &format!("{base}/abc"))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    get(&s.app, &format!("{base}/-1"))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    get(&s.app, "/api/v1/repos/nope/issues/1")
        .await
        .assert_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn without_a_usable_github_issues_are_a_conflict() {
    // Not a dry run, and `gh` is a program that does not exist.
    let s = served(false).await;
    let base = format!("/api/v1/repos/{}/issues", s.repo.id);
    for path in [base.clone(), format!("{base}/1")] {
        let reply = get(&s.app, &path).await;
        reply.assert_error(StatusCode::CONFLICT);
        assert!(
            reply.json::<ApiError>().error.contains("GitHub"),
            "{}",
            reply.text()
        );
    }
    let body = json!({
        "prompt": "Fix it",
        "issue": { "repo_id": s.repo.id, "number": 1 },
    });
    post(&s.app, "/api/v1/sessions", body)
        .await
        .assert_error(StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_session_starts_from_an_issue_or_is_refused() {
    let s = served(true).await;
    let detail = create_with(
        &s.app,
        json!({
            "prompt": "Make it work on phones",
            "permission_mode": "autonomous",
            "issue": { "repo_id": s.repo.id, "number": 4 },
        }),
    )
    .await;
    let issue = detail.session.issue.clone().unwrap();
    assert_eq!(
        (issue.repo_id.as_str(), issue.number),
        (s.repo.id.as_str(), 4)
    );
    assert!(issue.url.ends_with("/api/issues/4"));
    assert_eq!(detail.session.prompt, "Make it work on phones");
    let fetched: SessionDetail = get(&s.app, &format!("/api/v1/sessions/{}", detail.session.id))
        .await
        .json();
    assert_eq!(fetched.session.issue, Some(issue));
    wait_for(&s.app, &detail.session.id, SessionStatus::Review).await;
    let repos: SessionDetail = get(&s.app, &format!("/api/v1/sessions/{}", detail.session.id))
        .await
        .json();
    assert_eq!(repos.repos[0].repo_id, s.repo.id);

    for issue in [
        json!({ "repo_id": s.repo.id, "number": 99 }),
        json!({ "repo_id": "nope", "number": 1 }),
    ] {
        let body = json!({ "prompt": "Fix it", "issue": issue });
        post(&s.app, "/api/v1/sessions", body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    // An issue can stand in for the prompt; without one a blank prompt is refused.
    let body = json!({ "prompt": " ", "issue": { "repo_id": s.repo.id, "number": 4 } });
    let reply = post(&s.app, "/api/v1/sessions", body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let created: SessionDetail = reply.json();
    assert!(created.session.prompt.starts_with("Work on issue #4: "));
    assert!(created.session.title.starts_with("Work on issue #4"));
    post(&s.app, "/api/v1/sessions", json!({ "prompt": " " }))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn usage_and_limits_are_served() {
    let s = served(true).await;
    let reply = get(&s.app, "/api/v1/usage").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let empty: Usage = reply.json();
    assert_eq!(empty.by, UsageBy::Day);
    assert_eq!(empty.totals.sessions, 0);
    assert!((30..=31).contains(&empty.rows.len()));
    let limits: Vec<RunnerLimits> = get(&s.app, "/api/v1/usage/limits").await.json();
    assert_eq!(limits, []);

    let session = create(&s.app, "spend something").await.session;
    wait_for(&s.app, &session.id, SessionStatus::Review).await;
    let usage: Usage = get(&s.app, "/api/v1/usage?by=session").await.json();
    assert_eq!(usage.by, UsageBy::Session);
    assert_eq!(usage.rows.len(), 1);
    assert_eq!(usage.rows[0].key, session.id);
    assert_eq!(usage.rows[0].label, session.title);
    assert_eq!(usage.totals.tokens_in, 1200);
    let from = usage.from;
    let ranged: Usage = get(&s.app, &format!("/api/v1/usage?from={from}&by=model"))
        .await
        .json();
    assert_eq!(ranged.from, from);
    assert_eq!(ranged.rows[0].key, "mock-1");
    let limits: Vec<RunnerLimits> = get(&s.app, "/api/v1/usage/limits").await.json();
    assert_eq!(limits.len(), 1);
    assert_eq!(limits[0].runner, "mock");

    for query in [
        "by=week",
        "from=yesterday",
        "to=1.5",
        "from=10&to=5",
        "from=-5",
    ] {
        let reply = get(&s.app, &format!("/api/v1/usage?{query}")).await;
        reply.assert_error(StatusCode::BAD_REQUEST);
    }
}

fn schedule_request(name: &str, enabled: bool) -> serde_json::Value {
    json!({
        "name": name,
        "enabled": enabled,
        "trigger": { "kind": "schedule", "cron": "0 8 * * 1-5" },
        "session": { "prompt": "Triage on {{date}}", "permission_mode": "autonomous" },
    })
}

#[tokio::test]
async fn automations_are_created_read_changed_run_and_deleted() {
    let s = served(true).await;
    let list: Vec<Automation> = get(&s.app, "/api/v1/automations").await.json();
    assert_eq!(list, []);

    // Invalid: every problem named.
    let mut wrong = schedule_request(" ", false);
    wrong["trigger"]["cron"] = json!("0 8 * *");
    wrong["session"]["permission_mode"] = json!("ask");
    let reply = post(&s.app, "/api/v1/automations", wrong).await;
    reply.assert_error(StatusCode::BAD_REQUEST);
    let error = reply.json::<ApiError>().error;
    for problem in ["name", "schedule", "permission mode"] {
        assert!(error.contains(problem), "{error}");
    }
    post(&s.app, "/api/v1/automations", json!({ "name": "x" }))
        .await
        .assert_error(StatusCode::BAD_REQUEST);

    let reply = post(
        &s.app,
        "/api/v1/automations",
        schedule_request("Weekdays", false),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let created: Automation = reply.json();
    assert_eq!(created.trigger_text, "Every weekday at 08:00");
    assert!(!created.enabled);
    let path = format!("/api/v1/automations/{}", created.id);
    assert_eq!(get(&s.app, &path).await.json::<Automation>(), created);
    get(&s.app, "/api/v1/automations/nope")
        .await
        .assert_error(StatusCode::NOT_FOUND);

    let reply = put(&s.app, &path, schedule_request("Weekday mornings", true)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let changed: Automation = reply.json();
    assert_eq!(changed.name, "Weekday mornings");
    assert!(changed.next_run_at.is_some());
    put(&s.app, &path, schedule_request("", true))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    put(
        &s.app,
        "/api/v1/automations/nope",
        schedule_request("x", true),
    )
    .await
    .assert_error(StatusCode::NOT_FOUND);
    let list: Vec<Automation> = get(&s.app, "/api/v1/automations").await.json();
    assert_eq!(list, std::slice::from_ref(&changed));

    // Run now. The mock asks before its command in this mode, so the session
    // waits for the user: still at work, and a second run is a conflict.
    let mut asks = schedule_request("Asks", false);
    asks["session"]["permission_mode"] = json!("accept_edits");
    let asks: Automation = post(&s.app, "/api/v1/automations", asks).await.json();
    let run_path = format!("/api/v1/automations/{}/run", asks.id);
    let reply = post(&s.app, &run_path, json!({})).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let run: AutomationRun = reply.json();
    assert_eq!(
        (run.cause.as_str(), run.outcome),
        ("manual", AutomationOutcome::Started)
    );
    let session_id = run.session_id.clone().unwrap();
    post(&s.app, &run_path, json!({}))
        .await
        .assert_error(StatusCode::CONFLICT);
    post(&s.app, &run_path, json!([]))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    post(&s.app, &run_path, json!({ "number": 3 }))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    post(&s.app, "/api/v1/automations/nope/run", json!({}))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let runs: Vec<AutomationRun> = get(&s.app, &format!("{run_path}s")).await.json();
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].id, run.id);
    assert!(runs[0].session_status.is_some());
    get(&s.app, "/api/v1/automations/nope/runs")
        .await
        .assert_error(StatusCode::NOT_FOUND);

    // An issue trigger runs on the issue the request names.
    let issues = json!({
        "name": "Issues",
        "enabled": false,
        "trigger": { "kind": "issue", "repo_id": s.repo.id, "label": "agent" },
        "session": { "prompt": "Fix #{{number}}: {{title}}", "permission_mode": "autonomous" },
    });
    let issues: Automation = post(&s.app, "/api/v1/automations", issues).await.json();
    assert_eq!(issues.trigger_text, "New issue labelled `agent` in api");
    let run_path = format!("/api/v1/automations/{}/run", issues.id);
    post(&s.app, &run_path, json!({}))
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    post(&s.app, &run_path, json!({ "number": 99 }))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let run: AutomationRun = post(&s.app, &run_path, json!({ "number": 2 })).await.json();
    let started: SessionDetail = get(
        &s.app,
        &format!("/api/v1/sessions/{}", run.session_id.unwrap()),
    )
    .await
    .json();
    assert_eq!(started.session.issue.unwrap().number, 2);
    assert_eq!(
        started.session.automation_id.as_deref(),
        Some(issues.id.as_str())
    );

    // Deleting leaves the sessions it started.
    let asks_path = format!("/api/v1/automations/{}", asks.id);
    assert_eq!(
        delete(&s.app, &asks_path).await.status,
        StatusCode::NO_CONTENT
    );
    delete(&s.app, &asks_path)
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let kept: SessionDetail = get(&s.app, &format!("/api/v1/sessions/{session_id}"))
        .await
        .json();
    assert_eq!(
        kept.session.automation_id.as_deref(),
        Some(asks.id.as_str())
    );
    // Stopped, so the test leaves nothing waiting behind it.
    let stop = post_empty(&s.app, &format!("/api/v1/sessions/{session_id}/stop")).await;
    assert_eq!(stop.status, StatusCode::ACCEPTED, "{}", stop.text());
}

#[tokio::test]
async fn the_cron_preview_answers_whatever_the_expression() {
    let s = served(true).await;
    let reply = get(&s.app, "/api/v1/automations/cron?expr=*/15%20*%20*%20*%20*").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let preview: CronPreview = reply.json();
    assert_eq!(preview.text.as_deref(), Some("Every 15 minutes"));
    assert_eq!(preview.next.len(), 3);
    assert!(
        preview
            .next
            .windows(2)
            .all(|pair| pair[1] - pair[0] == 15 * 60 * 1000)
    );
    assert_eq!(preview.problem, None);
    for query in ["?expr=0%2025%20*%20*%20*", "?expr=", ""] {
        let preview: CronPreview = get(&s.app, &format!("/api/v1/automations/cron{query}"))
            .await
            .json();
        assert_eq!(preview.text, None, "{query}");
        assert_eq!(preview.next, Vec::<i64>::new());
        assert!(preview.problem.is_some(), "{query}");
    }
}
