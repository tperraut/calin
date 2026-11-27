//! Delivery over HTTP: shipping a session, refreshing its pull requests,
//! and the fields that say what GitHub can do and what was opened.

use calin_proto::{CheckState, PullRequest, PullRequestState, ReviewState};
use serde_json::Value;

use super::*;

/// A stand-in for the GitHub CLI that only answers whether somebody is
/// signed in: nobody is while a file `signed-out` sits next to it.
#[cfg(unix)]
fn fake_gh(dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("gh");
    let script = "#!/bin/sh\nif [ -f \"$(dirname \"$0\")/signed-out\" ]; then exit 1; fi\n";
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

/// Gives a repository an identity of its own to commit with, so that a ship
/// does not depend on the machine the tests run on.
fn give_identity(repo: &Path) {
    for (key, value) in [
        ("user.name", "Calin Test"),
        ("user.email", "test@calin.invalid"),
        ("commit.gpgsign", "false"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git(repo, &["config", key, value]);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn health_says_why_pull_requests_cannot_be_opened() {
    // No `gh` on this machine.
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, false).await);
    let health: Health = get(&app, "/api/v1/health").await.json();
    let problem = health.github_problem.unwrap();
    assert!(problem.contains("was not found"), "{problem}");

    // A `gh` nobody is signed in to, then one that works.
    let bin = TempDir::new().unwrap();
    let config = ManagerConfig {
        gh_bin: fake_gh(bin.path()),
        ..config()
    };
    std::fs::write(bin.path().join("signed-out"), "").unwrap();
    let dir = TempDir::new().unwrap();
    let app = router(state_on(&dir, false, None, config.clone()).await);
    let health: Health = get(&app, "/api/v1/health").await.json();
    let problem = health.github_problem.unwrap();
    assert!(problem.contains("not signed in"), "{problem}");

    std::fs::remove_file(bin.path().join("signed-out")).unwrap();
    let dir = TempDir::new().unwrap();
    let app = router(state_on(&dir, false, None, config.clone()).await);
    let reply = get(&app, "/api/v1/health").await;
    assert_eq!(reply.json::<Health>().github_problem, None);
    // Absent means null on the wire, not a missing field.
    assert_eq!(reply.json::<Value>()["github_problem"], Value::Null);

    // A dry run never uses GitHub, whatever state `gh` is in.
    std::fs::write(bin.path().join("signed-out"), "").unwrap();
    let dir = TempDir::new().unwrap();
    let app = router(state_on(&dir, true, None, config).await);
    let health: Health = get(&app, "/api/v1/health").await.json();
    assert_eq!(health.github_problem, None);
}

#[tokio::test]
async fn shipping_and_refreshing_answer_with_the_documented_statuses() {
    let ws = workspace().await;
    let app = &ws.app;
    give_identity(&ws.worktree);
    let session = format!("/api/v1/sessions/{}", ws.session_id);
    let ship = format!("{session}/ship");
    let refresh = format!("{session}/pull-requests/refresh");
    let before: Value = get(app, &session).await.json();
    assert_eq!(before["pull_requests"], json!([]));

    // 404 for a session nobody knows.
    let unknown = format!("/api/v1/sessions/{UNKNOWN_SESSION}");
    post(app, &format!("{unknown}/ship"), json!({}))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    post_empty(app, &format!("{unknown}/pull-requests/refresh"))
        .await
        .assert_error(StatusCode::NOT_FOUND);

    // 400 for a request that is not a `ShipRequest`.
    for body in [
        json!({ "title": "one\ntwo" }),
        json!({ "title": 7 }),
        json!({ "draft": "yes" }),
        json!([]),
    ] {
        post(app, &ship, body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    let not_json = json_post(&ship).body(Body::from("{")).unwrap();
    call(app, not_json)
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    post_empty(app, &ship)
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    // 409: in review there is nothing to refresh yet.
    post_empty(app, &refresh)
        .await
        .assert_error(StatusCode::CONFLICT);
    let detail: SessionDetail = get(app, &session).await.json();
    assert_eq!(detail.session.status, SessionStatus::Review);
    assert!(detail.pull_requests.is_empty());

    // 202 at once; the work shows as events and in the session.
    let request = json!({ "title": "Add the note", "body": "What it does." });
    let reply = post(app, &ship, request).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    let shipped = wait_for(app, &ws.session_id, SessionStatus::Shipping).await;
    let api = &shipped.repos[0];
    assert_eq!(
        shipped.pull_requests,
        [PullRequest {
            session_repo_id: api.id.clone(),
            number: 1,
            url: "https://example.invalid/api/pull/1".to_owned(),
            title: "Add the note".to_owned(),
            state: PullRequestState::Draft,
            checks: CheckState::None,
            review: ReviewState::None,
            updated_at: shipped.pull_requests[0].updated_at,
        }]
    );
    // The wire names, as the web app reads them.
    let wire: Value = get(app, &session).await.json();
    let on_wire = &wire["pull_requests"][0];
    assert_eq!(on_wire["session_repo_id"], json!(api.id));
    assert_eq!(on_wire["url"], "https://example.invalid/api/pull/1");
    assert_eq!(on_wire["state"], "draft");
    assert_eq!(on_wire["checks"], "none");
    assert_eq!(on_wire["review"], "none");
    assert!(on_wire["updated_at"].is_i64());
    let is_shipping = |event: &Event| {
        matches!(
            event.body,
            EventBody::SessionStatus {
                status: SessionStatus::Shipping,
                ..
            }
        )
    };
    let events = events_until(&ws.state, &ws.session_id, is_shipping).await;
    let opened: Vec<&Event> = events
        .iter()
        .filter(|event| matches!(event.body, EventBody::PullRequestOpened { .. }))
        .collect();
    assert_eq!(opened.len(), 1);
    assert_eq!(
        opened[0].body,
        EventBody::PullRequestOpened {
            pull_request: shipped.pull_requests[0].clone()
        }
    );
    // The agent's uncommitted note is a commit now; the read-only repo is as it was.
    assert_eq!(
        git(&ws.worktree, &["log", "-1", "--format=%s"]),
        "Add the note"
    );
    assert_eq!(git(&ws.worktree, &["status", "--porcelain"]), "");
    assert_eq!(git(&ws.docs, &["log", "--format=%s"]), "initial");

    // 202 for a refresh while shipping, and for shipping again.
    let reply = post_empty(app, &refresh).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    let reply = post(app, &ship, json!({})).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());

    // A message sends the agent back in; the pull requests stay listed.
    let messages = format!("{session}/messages");
    let reply = post(app, &messages, json!({ "text": "one more thing" })).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    let again = wait_for(app, &ws.session_id, SessionStatus::Review).await;
    assert_eq!(again.pull_requests, shipped.pull_requests);

    // 409 once the session is over.
    let reply = post_empty(app, &format!("{session}/stop")).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    post(app, &ship, json!({}))
        .await
        .assert_error(StatusCode::CONFLICT);
    post_empty(app, &refresh)
        .await
        .assert_error(StatusCode::CONFLICT);
    // Only POST exists on these routes.
    let reply = get(app, &ship).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn shipping_without_github_is_a_409_that_says_why() {
    // Not a dry run, and the GitHub CLI of these tests does not exist.
    let home = TempDir::new().unwrap();
    let state = state(&home, false).await;
    let app = router(state.clone());
    let id = create(&app, "Make a note").await.session.id;
    let before = wait_for(&app, &id, SessionStatus::Review).await;

    let reply = post(&app, &format!("/api/v1/sessions/{id}/ship"), json!({})).await;
    reply.assert_error(StatusCode::CONFLICT);
    let error = reply.json::<ApiError>().error;
    assert!(error.contains("pull requests cannot be opened"), "{error}");
    assert!(error.contains("was not found"), "{error}");
    // The same reason health gives, and nothing happened to the session.
    let health: Health = get(&app, "/api/v1/health").await.json();
    assert!(error.contains(&health.github_problem.unwrap()));
    let after: SessionDetail = get(&app, &format!("/api/v1/sessions/{id}")).await.json();
    assert_eq!(after, before);
}
