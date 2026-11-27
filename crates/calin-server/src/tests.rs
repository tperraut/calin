use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HOST, ORIGIN};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use calin_agents::{ClaudeConfig, MockConfig, RegistryConfig, RunnerRegistry};
use calin_core::{EventDraft, Home, MAX_FILE_BYTES, ManagerConfig, SessionManager, Store};
use calin_proto::{
    AgentKind, ApiError, ChangeStatus, ClientMsg, CreateSessionRequest, DecisionSource, EntryKind,
    Event, EventBody, FileContent, FileDiff, FileOp, Health, PermissionDecision, PermissionMode,
    PermissionRequest, Repo, RepoChanges, RepoMode, ServerMsg, Session, SessionDetail,
    SessionStatus, TOPIC_SESSIONS, TermClientMsg, TermServerMsg, TreeEntry, session_topic,
};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde::de::DeserializeOwned;
use serde_json::json;
use tempfile::TempDir;
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;

use super::*;

mod catalog;
mod delivery;
mod orchestration;
mod workflows;

const TIMEOUT: Duration = Duration::from_secs(20);
const LOCAL: &str = "127.0.0.1:4680";
const UNKNOWN_SESSION: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

static WEB_APP: &[(&str, &[u8])] = &[
    ("assets/app-abc123.js", b"console.log('calin')"),
    ("favicon.svg", b"<svg/>"),
    ("index.html", b"<!doctype html><title>the app</title>"),
];

/// A state on a temporary home, with instant mock agents, no web build and
/// a plain `sh` in its terminals rather than whatever shell the user has.
async fn state(dir: &TempDir, dry_run: bool) -> AppState {
    state_with(dir, dry_run, None).await
}

async fn state_with(dir: &TempDir, dry_run: bool, claude: Option<ClaudeConfig>) -> AppState {
    state_on(dir, dry_run, claude, config()).await
}

async fn state_on(
    dir: &TempDir,
    dry_run: bool,
    claude: Option<ClaudeConfig>,
    config: ManagerConfig,
) -> AppState {
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let registry = RunnerRegistry::new(RegistryConfig {
        dry_run,
        mock: MockConfig {
            delay: Duration::ZERO,
        },
        claude,
    });
    let manager = SessionManager::start_with(store, registry, config)
        .await
        .unwrap();
    AppState::new(manager)
        .with_assets(Assets::empty())
        .with_terminal_command("/bin/sh", [] as [&str; 0])
}

/// The defaults, except that the GitHub CLI is one that does not exist: no
/// test may run the one this machine has.
fn config() -> ManagerConfig {
    ManagerConfig {
        gh_bin: PathBuf::from("/nonexistent/calin/gh"),
        ..ManagerConfig::default()
    }
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|err| {
            panic!(
                "not the expected JSON ({err}): {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8(self.body.clone()).unwrap()
    }

    fn content_type(&self) -> &str {
        self.headers[CONTENT_TYPE].to_str().unwrap()
    }

    /// Asserts an `ApiError` body with this status.
    fn assert_error(&self, status: StatusCode) {
        assert_eq!(self.status, status, "{}", self.text());
        assert_eq!(self.content_type(), "application/json");
        assert!(!self.json::<ApiError>().error.is_empty());
    }
}

fn request(method: Method, path: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, LOCAL)
}

async fn call(app: &Router, request: Request<Body>) -> Reply {
    let response = app.clone().oneshot(request).await.unwrap();
    let (parts, body) = response.into_parts();
    Reply {
        status: parts.status,
        headers: parts.headers,
        body: body.collect().await.unwrap().to_bytes().to_vec(),
    }
}

async fn get(app: &Router, path: &str) -> Reply {
    call(app, request(Method::GET, path).body(Body::empty()).unwrap()).await
}

fn json_post(path: &str) -> axum::http::request::Builder {
    request(Method::POST, path).header(CONTENT_TYPE, "application/json")
}

async fn post(app: &Router, path: &str, body: serde_json::Value) -> Reply {
    let request = json_post(path).body(Body::from(body.to_string())).unwrap();
    call(app, request).await
}

async fn post_empty(app: &Router, path: &str) -> Reply {
    call(
        app,
        request(Method::POST, path).body(Body::empty()).unwrap(),
    )
    .await
}

/// A session that never waits for the user: the mock asks nothing in
/// `autonomous` mode.
async fn create(app: &Router, prompt: &str) -> SessionDetail {
    create_with(
        app,
        json!({ "prompt": prompt, "permission_mode": "autonomous" }),
    )
    .await
}

async fn create_with(app: &Router, body: serde_json::Value) -> SessionDetail {
    let reply = post(app, "/api/v1/sessions", body).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    reply.json()
}

async fn send_json(app: &Router, method: Method, path: &str, body: serde_json::Value) -> Reply {
    let request = request(method, path)
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    call(app, request).await
}

async fn delete(app: &Router, path: &str) -> Reply {
    call(
        app,
        request(Method::DELETE, path).body(Body::empty()).unwrap(),
    )
    .await
}

/// Runs git in `dir` with a fixed identity and none of this machine's
/// configuration; panics when it fails.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Calin Test")
        .env("GIT_AUTHOR_EMAIL", "test@calin.invalid")
        .env("GIT_COMMITTER_NAME", "Calin Test")
        .env("GIT_COMMITTER_EMAIL", "test@calin.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git should start");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A repository `parent/name` on `main` with one commit holding `README.md`
/// and `src/lib.rs`. Returns its canonical path.
fn git_repo(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(path.join("src")).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    std::fs::write(path.join("README.md"), "# readme\n").unwrap();
    std::fs::write(path.join("src/lib.rs"), "pub fn one() -> u8 {\n    1\n}\n").unwrap();
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "initial"]);
    std::fs::canonicalize(path).unwrap()
}

async fn register(app: &Router, path: &Path) -> Repo {
    let reply = post(app, "/api/v1/repos", json!({ "path": path })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    reply.json()
}

/// The session's log from the start, up to the first event matching `last`.
async fn events_until(
    state: &AppState,
    session_id: &str,
    last: impl Fn(&Event) -> bool,
) -> Vec<Event> {
    let mut subscription = state
        .manager()
        .store()
        .subscribe(session_id, 0)
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
        let event = tokio::time::timeout(TIMEOUT, subscription.live.recv())
            .await
            .expect("the awaited event never came")
            .unwrap();
        let done = last(&event);
        events.push((*event).clone());
        if done {
            return events;
        }
    }
}

/// The requests in `events` that nothing has answered yet.
fn pending_requests(events: &[Event]) -> Vec<PermissionRequest> {
    let mut pending: Vec<PermissionRequest> = Vec::new();
    for event in events {
        match &event.body {
            EventBody::PermissionRequested { request } => pending.push(request.clone()),
            EventBody::PermissionResolved { request_id, .. } => {
                pending.retain(|request| &request.id != request_id);
            }
            _ => {}
        }
    }
    pending
}

fn is_waiting(event: &Event) -> bool {
    matches!(
        event.body,
        EventBody::SessionStatus {
            status: SessionStatus::Waiting,
            ..
        }
    )
}

/// Polls the session until it reaches `status`.
async fn wait_for(app: &Router, session_id: &str, status: SessionStatus) -> SessionDetail {
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let detail: SessionDetail = get(app, &format!("/api/v1/sessions/{session_id}"))
                .await
                .json();
            if detail.session.status == status {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the session never reached the status")
}

#[tokio::test]
async fn health_reports_version_dry_run_and_runners() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    let reply = get(&app, "/api/v1/health").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        reply.json::<Health>(),
        Health {
            name: "calin".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            dry_run: true,
            runners: vec!["mock".to_owned()],
            github_problem: None,
        }
    );

    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, false).await);
    let health: Health = get(&app, "/api/v1/health").await.json();
    assert!(!health.dry_run);
    assert_eq!(health.runners, ["mock"]);
}

/// A stand-in for the Claude Code CLI that only answers `--version`, which
/// is all the registry asks of it. The real CLI is never run by tests.
#[cfg(unix)]
fn fake_claude(dir: &Path) -> ClaudeConfig {
    use std::os::unix::fs::PermissionsExt;
    let bin = dir.join("claude");
    std::fs::write(&bin, "#!/bin/sh\necho '2.1.332 (Claude Code)'\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    ClaudeConfig {
        bin: Some(bin),
        ..ClaudeConfig::default()
    }
}

#[cfg(unix)]
#[tokio::test]
async fn health_lists_the_claude_runner_first_when_its_cli_is_available() {
    let bin = TempDir::new().unwrap();
    let dir = TempDir::new().unwrap();
    let app = router(state_with(&dir, false, Some(fake_claude(bin.path()))).await);
    let health: Health = get(&app, "/api/v1/health").await.json();
    assert_eq!(health.runners, ["claude", "mock"]);
    assert!(!health.dry_run);

    // In dry run it is not even looked for.
    let dir = TempDir::new().unwrap();
    let app = router(state_with(&dir, true, Some(fake_claude(bin.path()))).await);
    assert_eq!(
        get(&app, "/api/v1/health").await.json::<Health>().runners,
        ["mock"]
    );

    // A CLI that is not there leaves the mock as the only runner.
    let missing = ClaudeConfig {
        bin: Some(bin.path().join("nope")),
        ..ClaudeConfig::default()
    };
    let dir = TempDir::new().unwrap();
    let app = router(state_with(&dir, false, Some(missing)).await);
    assert_eq!(
        get(&app, "/api/v1/health").await.json::<Health>().runners,
        ["mock"]
    );
}

#[tokio::test]
async fn sessions_are_created_listed_and_fetched() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    let reply = get(&app, "/api/v1/sessions").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.json::<Vec<Session>>().is_empty());

    let first = create(&app, "Fix the login bug\nwith details").await;
    assert_eq!(first.session.title, "Fix the login bug");
    assert_eq!(first.session.runner, "mock");
    // The answer comes as soon as the session exists; its agent follows.
    assert_eq!(first.session.status, SessionStatus::Queued);
    assert_eq!(first.session.permission_mode, PermissionMode::Autonomous);
    assert!(first.agents.is_empty());
    assert!(first.repos.is_empty());
    tokio::time::sleep(Duration::from_millis(3)).await;

    let reply = post(
        &app,
        "/api/v1/sessions",
        json!({ "prompt": "second", "title": "Named", "runner": "claude" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let second: SessionDetail = reply.json();
    assert_eq!(second.session.title, "Named");
    // Dry run: whatever was asked for runs on the mock runner.
    assert_eq!(second.session.runner, "mock");
    // `ask` is the mode of a request that names none.
    assert_eq!(second.session.permission_mode, PermissionMode::Ask);

    let listed: Vec<Session> = get(&app, "/api/v1/sessions").await.json();
    let ids: Vec<&str> = listed.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, [second.session.id.as_str(), first.session.id.as_str()]);

    let done = wait_for(&app, &first.session.id, SessionStatus::Review).await;
    assert_eq!(done.session.id, first.session.id);
    // The lead, and the subagent the mock's turn starts.
    assert_eq!(done.agents.len(), 2);
    assert_eq!(done.agents[0].role, "lead");
    assert_eq!(done.agents[1].kind, AgentKind::Subagent);
    assert_eq!(
        done.agents[1].parent_agent_id,
        Some(done.agents[0].id.clone())
    );
    assert_eq!(done.agents[0].tokens_in, 1200);
    assert!(done.agents[0].native_session_id.is_some());
    assert!(done.session.head_seq > 10);

    get(&app, &format!("/api/v1/sessions/{UNKNOWN_SESSION}"))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    get(&app, "/api/v1/sessions/..%2F..%2Fetc")
        .await
        .assert_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_session_requests_are_400() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, false).await);
    for body in [
        json!({ "prompt": "" }),
        json!({ "prompt": "  \n " }),
        json!({ "title": "no prompt" }),
        json!({ "prompt": 7 }),
        json!({ "prompt": "x", "runner": "claude" }),
        json!({ "prompt": "x", "permission_mode": "yolo" }),
        json!({ "prompt": "x", "repos": [{ "repo_id": UNKNOWN_SESSION }] }),
        json!({ "prompt": "x", "repos": [{ "mode": "write" }] }),
        json!("just a string"),
    ] {
        post(&app, "/api/v1/sessions", body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    let not_json = json_post("/api/v1/sessions")
        .body(Body::from("{\"prompt\": "))
        .unwrap();
    call(&app, not_json)
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    let no_content_type = request(Method::POST, "/api/v1/sessions")
        .body(Body::from(json!({ "prompt": "x" }).to_string()))
        .unwrap();
    call(&app, no_content_type)
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    post_empty(&app, "/api/v1/sessions")
        .await
        .assert_error(StatusCode::BAD_REQUEST);

    assert!(
        get(&app, "/api/v1/sessions")
            .await
            .json::<Vec<Session>>()
            .is_empty()
    );
}

#[tokio::test]
async fn messages_are_accepted_until_the_session_is_done() {
    let dir = TempDir::new().unwrap();
    let state = state(&dir, true).await;
    let app = router(state.clone());
    let id = create(&app, "first").await.session.id;
    let messages = format!("/api/v1/sessions/{id}/messages");
    let before = wait_for(&app, &id, SessionStatus::Review).await;

    let reply = post(&app, &messages, json!({ "text": "and the docs" })).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    // The message is logged and the session running before the reply.
    let running: SessionDetail = get(&app, &format!("/api/v1/sessions/{id}")).await.json();
    assert!(running.session.head_seq > before.session.head_seq);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let detail = wait_for(&app, &id, SessionStatus::Review).await;
            if detail.agents[0].tokens_in == 2400 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("the second turn never finished");

    for body in [json!({ "text": " " }), json!({}), json!({ "text": null })] {
        post(&app, &messages, body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    let unknown = format!("/api/v1/sessions/{UNKNOWN_SESSION}/messages");
    post(&app, &unknown, json!({ "text": "hi" }))
        .await
        .assert_error(StatusCode::NOT_FOUND);

    let stop = format!("/api/v1/sessions/{id}/stop");
    let reply = post_empty(&app, &stop).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    let detail: SessionDetail = get(&app, &format!("/api/v1/sessions/{id}")).await.json();
    assert_eq!(detail.session.status, SessionStatus::Cancelled);

    post_empty(&app, &stop)
        .await
        .assert_error(StatusCode::CONFLICT);
    post_empty(&app, &format!("/api/v1/sessions/{UNKNOWN_SESSION}/stop"))
        .await
        .assert_error(StatusCode::NOT_FOUND);

    // A cancelled session is continued by a message: the agent starts again.
    let reply = post(&app, &messages, json!({ "text": "not done yet" })).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    let detail = wait_for(&app, &id, SessionStatus::Review).await;
    // Still one lead; each turn that got that far added its subagent.
    let leads = detail.agents.iter().filter(|agent| agent.role == "lead");
    assert_eq!(leads.count(), 1);

    // Only `done` is final.
    let done = EventBody::SessionStatus {
        status: SessionStatus::Done,
        reason: None,
    };
    state
        .manager()
        .store()
        .append(&id, EventDraft::new(done))
        .await
        .unwrap();
    post(&app, &messages, json!({ "text": "too late" }))
        .await
        .assert_error(StatusCode::CONFLICT);
    post_empty(&app, &stop)
        .await
        .assert_error(StatusCode::CONFLICT);
}

#[tokio::test]
async fn unknown_api_routes_are_json_errors_never_html() {
    let dir = TempDir::new().unwrap();
    let state = state(&dir, true).await.with_assets(Assets::new(WEB_APP));
    let app = router(state);
    for path in [
        "/api",
        "/api/",
        "/api/v1",
        "/api/v1/nope",
        "/api/v2/health",
        "/api/v1/sessions/x/y/z",
        "/api/v1/health/",
    ] {
        get(&app, path).await.assert_error(StatusCode::NOT_FOUND);
    }
    post_empty(&app, "/api/v1/nope")
        .await
        .assert_error(StatusCode::NOT_FOUND);

    for (method, path) in [
        (Method::DELETE, "/api/v1/sessions"),
        (Method::POST, "/api/v1/health"),
        (Method::GET, "/api/v1/sessions/x/stop"),
        (Method::PUT, "/api/v1/sessions/x/messages"),
        (Method::PUT, "/api/v1/repos"),
        (Method::GET, "/api/v1/repos/x"),
        (Method::GET, "/api/v1/sessions/x/interrupt"),
        (Method::GET, "/api/v1/sessions/x/permissions/p"),
        (Method::POST, "/api/v1/sessions/x/changes"),
        (Method::POST, "/api/v1/sessions/x/repos/r/diff"),
        (Method::PUT, "/api/v1/sessions/x/repos/r/tree"),
        (Method::POST, "/api/v1/sessions/x/repos/r/file"),
        (Method::POST, "/api/v1/sessions/x/terminals/ws"),
    ] {
        let reply = call(&app, request(method, path).body(Body::empty()).unwrap()).await;
        reply.assert_error(StatusCode::METHOD_NOT_ALLOWED);
    }

    // The WebSocket route without a WebSocket handshake.
    get(&app, "/api/v1/ws")
        .await
        .assert_error(StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn api_requests_must_name_a_loopback_host() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    let health = |host: Option<&str>| {
        let mut builder = Request::builder().uri("/api/v1/health");
        if let Some(host) = host {
            builder = builder.header(HOST, host);
        }
        call(&app, builder.body(Body::empty()).unwrap())
    };
    for host in [
        "localhost",
        "localhost:4680",
        "127.0.0.1:4680",
        "127.0.0.2",
        "[::1]:4680",
    ] {
        assert_eq!(health(Some(host)).await.status, StatusCode::OK, "{host}");
    }
    for host in [
        "evil.example",
        "evil.example:4680",
        "127.0.0.1.evil.example",
        "192.168.1.10:4680",
        "0.0.0.0:4680",
    ] {
        health(Some(host)).await.assert_error(StatusCode::FORBIDDEN);
    }
    health(None).await.assert_error(StatusCode::FORBIDDEN);

    // Unknown API routes are guarded too; the web app itself is not.
    let unknown = Request::builder()
        .uri("/api/v1/nope")
        .header(HOST, "evil.example")
        .body(Body::empty())
        .unwrap();
    call(&app, unknown)
        .await
        .assert_error(StatusCode::FORBIDDEN);
    let page = Request::builder()
        .uri("/")
        .header(HOST, "evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&app, page).await.status, StatusCode::OK);
}

#[tokio::test]
async fn state_changing_requests_from_other_origins_are_refused() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    let create_from = |origin: &'static str| {
        let body = json!({ "prompt": "x" });
        let request = json_post("/api/v1/sessions")
            .header(ORIGIN, origin)
            .body(Body::from(body.to_string()))
            .unwrap();
        call(&app, request)
    };
    for origin in [
        "https://evil.example",
        "http://evil.example:4680",
        "http://localhost.evil.example",
        "null",
    ] {
        create_from(origin)
            .await
            .assert_error(StatusCode::FORBIDDEN);
    }
    assert!(
        get(&app, "/api/v1/sessions")
            .await
            .json::<Vec<Session>>()
            .is_empty()
    );

    // The server's own origin, and the Vite dev server on another loopback port.
    for origin in [
        "http://127.0.0.1:4680",
        "http://localhost:5173",
        "http://[::1]:5173",
    ] {
        assert_eq!(
            create_from(origin).await.status,
            StatusCode::CREATED,
            "{origin}"
        );
    }
    // No Origin at all: curl and native clients.
    create(&app, "x").await;

    // Reads carry no risk and pass; a WebSocket handshake does not.
    let read = request(Method::GET, "/api/v1/sessions")
        .header(ORIGIN, "https://evil.example")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&app, read).await.status, StatusCode::OK);
    let handshake = request(Method::GET, "/api/v1/ws")
        .header(ORIGIN, "https://evil.example")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    call(&app, handshake)
        .await
        .assert_error(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn without_a_web_build_every_page_says_how_to_build_it() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    for path in ["/", "/index.html", "/sessions/abc", "/assets/app.js"] {
        let reply = get(&app, path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.content_type(), "text/html; charset=utf-8");
        assert!(reply.text().contains("make build"), "{path}");
        assert!(reply.text().contains("has not been built"), "{path}");
    }
    // The API is unaffected, and its errors stay JSON.
    assert_eq!(get(&app, "/api/v1/health").await.status, StatusCode::OK);
    get(&app, "/api/v1/nope")
        .await
        .assert_error(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_web_app_is_served_with_a_single_page_fallback() {
    let dir = TempDir::new().unwrap();
    let state = state(&dir, true).await.with_assets(Assets::new(WEB_APP));
    let app = router(state);

    for path in ["/", "/index.html", "/sessions/abc", "/assets/missing.js"] {
        let reply = get(&app, path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_eq!(reply.content_type(), "text/html; charset=utf-8");
        assert_eq!(reply.text(), "<!doctype html><title>the app</title>");
        assert_eq!(reply.headers[CACHE_CONTROL], "no-cache");
    }

    let script = get(&app, "/assets/app-abc123.js").await;
    assert_eq!(script.status, StatusCode::OK);
    assert_eq!(script.content_type(), "text/javascript; charset=utf-8");
    assert_eq!(script.text(), "console.log('calin')");
    assert!(
        script.headers[CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("immutable")
    );

    let icon = get(&app, "/favicon.svg").await;
    assert_eq!(icon.content_type(), "image/svg+xml");
    assert_eq!(icon.headers[CACHE_CONTROL], "no-cache");

    assert_eq!(
        post_empty(&app, "/").await.status,
        StatusCode::METHOD_NOT_ALLOWED
    );
}

// --- WebSocket, over a real socket ---

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Running {
    addr: SocketAddr,
    state: AppState,
    stop: tokio::sync::oneshot::Sender<()>,
    server: JoinHandle<std::io::Result<()>>,
}

async fn start(dir: &TempDir) -> Running {
    let state = state(dir, true).await;
    let server = Server::bind("127.0.0.1:0".parse().unwrap(), state.clone())
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(server.run(async move {
        let _ = stopped.await;
    }));
    Running {
        addr,
        state,
        stop,
        server,
    }
}

async fn connect(addr: SocketAddr) -> Client {
    let (client, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/api/v1/ws"))
        .await
        .unwrap();
    client
}

async fn send(client: &mut Client, message: &ClientMsg) {
    let text = serde_json::to_string(message).unwrap();
    client.send(Message::Text(text.into())).await.unwrap();
}

/// The next server frame, or `None` once the server closed the socket.
async fn try_next(client: &mut Client) -> Option<ServerMsg> {
    loop {
        let frame = tokio::time::timeout(TIMEOUT, client.next())
            .await
            .expect("no frame arrived");
        match frame {
            Some(Ok(Message::Text(text))) => return Some(serde_json::from_str(&text).unwrap()),
            Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
            Some(Ok(_)) => {}
        }
    }
}

async fn next(client: &mut Client) -> ServerMsg {
    try_next(client)
        .await
        .expect("the server closed the socket")
}

fn subscribe(topic: &str, after_seq: Option<u64>) -> ClientMsg {
    ClientMsg::Subscribe {
        topic: topic.to_owned(),
        after_seq,
    }
}

fn is_review(event: &Event) -> bool {
    matches!(
        event.body,
        EventBody::SessionStatus {
            status: SessionStatus::Review,
            ..
        }
    )
}

/// Reads a session topic until the event matching `last` and `subscribed`
/// have both arrived. Returns the events and the head from `subscribed`.
async fn read_session(
    client: &mut Client,
    topic: &str,
    last: impl Fn(&Event) -> bool,
) -> (Vec<Event>, u64) {
    let mut events: Vec<Event> = Vec::new();
    let mut head = None;
    let mut finished = false;
    while head.is_none() || !finished {
        match next(client).await {
            ServerMsg::Event { topic: on, event } => {
                assert_eq!(on, topic);
                // Replay comes before `subscribed`, live events after it.
                match head {
                    None => {}
                    Some(head) => assert!(event.seq > head, "replayed after subscribed"),
                }
                finished |= last(&event);
                events.push(event);
            }
            ServerMsg::Subscribed {
                topic: on,
                head_seq,
            } => {
                assert_eq!(on, topic);
                assert!(head.is_none(), "subscribed twice");
                assert!(events.iter().all(|event| event.seq <= head_seq));
                head = Some(head_seq);
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    (events, head.unwrap_or_default())
}

async fn new_session(state: &AppState, prompt: &str) -> String {
    let request = CreateSessionRequest {
        prompt: prompt.to_owned(),
        title: None,
        runner: None,
        model: None,
        permission_mode: Some(PermissionMode::Autonomous),
        workflow: None,
        repos: Vec::new(),
    };
    let detail = state.manager().create_session(request).await.unwrap();
    detail.session.id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_replays_then_streams_and_resumes_from_any_point() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let id = new_session(&running.state, "stream me").await;
    let topic = session_topic(&id);

    // No `after_seq`: everything from the first event to the end of the turn,
    // whether it was replayed or arrived live.
    let mut client = connect(running.addr).await;
    send(&mut client, &subscribe(&topic, None)).await;
    let (all, _) = read_session(&mut client, &topic, is_review).await;
    let seqs: Vec<u64> = all.iter().map(|event| event.seq).collect();
    assert_eq!(seqs, (1..=all.len() as u64).collect::<Vec<_>>());
    assert!(all.len() > 20);
    assert!(matches!(
        all[0].body,
        EventBody::SessionStatus {
            status: SessionStatus::Queued,
            ..
        }
    ));
    let total = all.len() as u64;

    // Reconnect in the middle: exactly the suffix, then `subscribed`.
    let middle = total / 2;
    let mut again = connect(running.addr).await;
    send(&mut again, &subscribe(&topic, Some(middle))).await;
    let (suffix, head) = read_session(&mut again, &topic, is_review).await;
    assert_eq!(suffix, all[middle as usize..]);
    assert_eq!(head, total);

    // The same on the socket that is already subscribed: it starts over.
    send(&mut client, &subscribe(&topic, Some(total - 3))).await;
    let (tail, head) = read_session(&mut client, &topic, is_review).await;
    assert_eq!(tail, all[total as usize - 3..]);
    assert_eq!(head, total);

    // Fully caught up: nothing to replay, then the next turn arrives live on both.
    let mut live = connect(running.addr).await;
    send(&mut live, &subscribe(&topic, Some(total))).await;
    assert_eq!(
        next(&mut live).await,
        ServerMsg::Subscribed {
            topic: topic.clone(),
            head_seq: total
        }
    );
    running
        .state
        .manager()
        .send_message(&id, "once more")
        .await
        .unwrap();
    let mut turns = Vec::new();
    for socket in [&mut live, &mut again] {
        let mut turn = Vec::new();
        loop {
            match next(socket).await {
                ServerMsg::Event { event, .. } => {
                    let done = is_review(&event);
                    turn.push(event);
                    if done {
                        break;
                    }
                }
                other => panic!("unexpected frame {other:?}"),
            }
        }
        assert_eq!(turn[0].seq, total + 1);
        assert!(turn.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
        turns.push(turn);
    }
    assert_eq!(turns[0], turns[1]);

    drop(running.stop);
    running.server.await.unwrap().unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_sessions_topic_lists_then_follows() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let first = new_session(&running.state, "first").await;
    let second = new_session(&running.state, "second").await;
    for id in [&first, &second] {
        let mut subscription = running
            .state
            .manager()
            .store()
            .subscribe(id, 0)
            .await
            .unwrap();
        while !subscription.backlog.iter().any(is_review) {
            let event = subscription.live.recv().await.unwrap();
            subscription.backlog.push((*event).clone());
        }
    }

    let mut client = connect(running.addr).await;
    send(&mut client, &subscribe(TOPIC_SESSIONS, None)).await;
    let mut listed = Vec::new();
    loop {
        match next(&mut client).await {
            ServerMsg::SessionUpserted { session } => listed.push(session),
            ServerMsg::Subscribed { topic, head_seq } => {
                assert_eq!(topic, TOPIC_SESSIONS);
                assert_eq!(head_seq, 0);
                break;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    let mut ids: Vec<&str> = listed.iter().map(|session| session.id.as_str()).collect();
    ids.sort_unstable();
    let mut expected = [first.as_str(), second.as_str()];
    expected.sort_unstable();
    assert_eq!(ids, expected);
    assert!(
        listed
            .iter()
            .all(|session| session.status == SessionStatus::Review)
    );

    // A new session shows up, and so does each of its status changes.
    let third = new_session(&running.state, "third").await;
    let mut statuses = Vec::new();
    while statuses.last() != Some(&SessionStatus::Review) {
        match next(&mut client).await {
            ServerMsg::SessionUpserted { session } => {
                assert_eq!(session.id, third);
                statuses.push(session.status);
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert_eq!(statuses[0], SessionStatus::Queued);
    assert!(statuses.contains(&SessionStatus::Running));

    // After unsubscribing, nothing more arrives on the topic.
    send(
        &mut client,
        &ClientMsg::Unsubscribe {
            topic: TOPIC_SESSIONS.to_owned(),
        },
    )
    .await;
    send(&mut client, &ClientMsg::Ping).await;
    assert_eq!(next(&mut client).await, ServerMsg::Pong);
    let fourth = new_session(&running.state, "fourth").await;
    let mut subscription = running
        .state
        .manager()
        .store()
        .subscribe(&fourth, 0)
        .await
        .unwrap();
    while !subscription.backlog.iter().any(is_review) {
        let event = subscription.live.recv().await.unwrap();
        subscription.backlog.push((*event).clone());
    }
    send(&mut client, &ClientMsg::Ping).await;
    assert_eq!(next(&mut client).await, ServerMsg::Pong);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bad_frames_get_an_error_and_the_socket_stays_open() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let mut client = connect(running.addr).await;

    send(&mut client, &ClientMsg::Ping).await;
    assert_eq!(next(&mut client).await, ServerMsg::Pong);

    // A refused subscribe names its topic, so the client knows which one failed.
    let unknown = session_topic(UNKNOWN_SESSION);
    for topic in [unknown.as_str(), "session:", "nonsense", "", "sessions:all"] {
        send(&mut client, &subscribe(topic, None)).await;
        match next(&mut client).await {
            ServerMsg::Error {
                topic: named,
                message,
            } => {
                assert_eq!(named.as_deref(), Some(topic));
                assert!(!message.is_empty());
            }
            other => panic!("expected an error for {topic:?}, got {other:?}"),
        }
    }
    // So does a refused unsubscribe.
    let nonsense = ClientMsg::Unsubscribe {
        topic: "nonsense".to_owned(),
    };
    send(&mut client, &nonsense).await;
    assert!(matches!(
        next(&mut client).await,
        ServerMsg::Error { topic: Some(topic), .. } if topic == "nonsense"
    ));
    // A frame that is about no topic names none.
    for raw in [
        "not json",
        "{}",
        r#"{"op":"dance"}"#,
        r#"{"op":"subscribe"}"#,
    ] {
        client.send(Message::Text(raw.into())).await.unwrap();
        assert!(
            matches!(
                next(&mut client).await,
                ServerMsg::Error { topic: None, .. }
            ),
            "{raw}"
        );
    }
    client
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert!(matches!(
        next(&mut client).await,
        ServerMsg::Error { topic: None, .. }
    ));

    // Unsubscribing from something never subscribed is not an error.
    send(
        &mut client,
        &ClientMsg::Unsubscribe {
            topic: "sessions".to_owned(),
        },
    )
    .await;
    // Still open and still working after all of that.
    send(&mut client, &ClientMsg::Ping).await;
    assert_eq!(next(&mut client).await, ServerMsg::Pong);
    let id = new_session(&running.state, "still works").await;
    let topic = session_topic(&id);
    send(&mut client, &subscribe(&topic, None)).await;
    let (events, _) = read_session(&mut client, &topic, is_review).await;
    assert_eq!(events[0].seq, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_websocket_handshake_is_guarded_by_origin() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let handshake = |origin: &'static str| {
        let mut request = format!("ws://{}/api/v1/ws", running.addr)
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(ORIGIN, origin.parse().unwrap());
        tokio_tungstenite::connect_async(request)
    };
    let (mut client, _) = handshake("http://localhost:5173").await.unwrap();
    send(&mut client, &ClientMsg::Ping).await;
    assert_eq!(next(&mut client).await, ServerMsg::Pong);

    let refused = handshake("https://evil.example").await.unwrap_err();
    match refused {
        tokio_tungstenite::tungstenite::Error::Http(response) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        other => panic!("expected a refused handshake, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_stops_reading_is_disconnected_and_can_replay() {
    const EVENTS: u64 = 3000;
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let store = running.state.manager().store().clone();
    let id = new_session(&running.state, "flood").await;
    let topic = session_topic(&id);
    let mut subscription = store.subscribe(&id, 0).await.unwrap();
    while !subscription.backlog.iter().any(is_review) {
        let event = subscription.live.recv().await.unwrap();
        subscription.backlog.push((*event).clone());
    }
    let before = subscription.backlog.len() as u64;
    drop(subscription);

    let mut slow = connect(running.addr).await;
    send(&mut slow, &subscribe(&topic, Some(before))).await;
    assert!(matches!(
        next(&mut slow).await,
        ServerMsg::Subscribed { .. }
    ));

    // The client now reads nothing while far more arrives than the socket
    // and the server's queues can hold. Appending must not wait for it.
    let chunk = "x".repeat(24 * 1024);
    tokio::time::timeout(TIMEOUT, async {
        for _ in 0..EVENTS {
            let body = EventBody::UserMessage {
                text: chunk.clone(),
            };
            store.append(&id, EventDraft::new(body)).await.unwrap();
        }
    })
    .await
    .expect("appends waited for a client that does not read");
    let head = before + EVENTS;

    // What it does get is in order, and then the server hangs up on it.
    let mut last = before;
    while let Some(frame) = try_next(&mut slow).await {
        match frame {
            ServerMsg::Event { event, .. } => {
                assert_eq!(event.seq, last + 1);
                last = event.seq;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert!(last < head, "the slow client was never disconnected");

    // Reconnecting from where it stopped fills the gap exactly.
    let mut back = connect(running.addr).await;
    send(&mut back, &subscribe(&topic, Some(last))).await;
    let (rest, subscribed_head) = read_session(&mut back, &topic, |event| event.seq == head).await;
    let seqs: Vec<u64> = rest.iter().map(|event| event.seq).collect();
    assert_eq!(seqs, (last + 1..=head).collect::<Vec<_>>());
    assert_eq!(subscribed_head, head);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_closes_open_websockets_and_returns() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let mut client = connect(running.addr).await;
    send(&mut client, &subscribe(TOPIC_SESSIONS, None)).await;
    assert!(matches!(
        next(&mut client).await,
        ServerMsg::Subscribed { .. }
    ));

    running.stop.send(()).unwrap();
    tokio::time::timeout(TIMEOUT, running.server)
        .await
        .expect("shutdown hung on an open websocket")
        .unwrap()
        .unwrap();
    assert_eq!(try_next(&mut client).await, None);
}

// --- Repos ---

#[tokio::test]
async fn repos_are_registered_listed_and_removed() {
    let dir = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    let listed = get(&app, "/api/v1/repos").await;
    assert_eq!(listed.status, StatusCode::OK);
    assert!(listed.json::<Vec<Repo>>().is_empty());

    let web = git_repo(clones.path(), "web");
    let api = git_repo(clones.path(), "api");
    let reply = post(&app, "/api/v1/repos", json!({ "path": web })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let web_repo: Repo = reply.json();
    assert_eq!(web_repo.name, "web");
    assert_eq!(web_repo.path, web.to_str().unwrap());
    assert_eq!(web_repo.default_branch.as_deref(), Some("main"));
    let reply = post(
        &app,
        "/api/v1/repos",
        json!({ "path": api, "name": "backend" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let api_repo: Repo = reply.json();
    assert_eq!(api_repo.name, "backend");

    // By name.
    let listed: Vec<Repo> = get(&app, "/api/v1/repos").await.json();
    assert_eq!(listed, [api_repo.clone(), web_repo.clone()]);

    // 400: not the root of a git work tree, or not a usable request.
    let plain = clones.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let other = git_repo(clones.path(), "other");
    for body in [
        json!({ "path": plain }),
        json!({ "path": web.join("src") }),
        json!({ "path": clones.path().join("missing") }),
        json!({ "path": "relative/path" }),
        json!({ "path": "" }),
        json!({ "path": other, "name": "a/b" }),
        json!({ "path": other, "name": ".." }),
        json!({ "name": "no path" }),
        json!({ "path": 7 }),
    ] {
        post(&app, "/api/v1/repos", body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    // 409: the name or the clone is already registered.
    for body in [
        json!({ "path": other, "name": "web" }),
        json!({ "path": web, "name": "again" }),
    ] {
        post(&app, "/api/v1/repos", body)
            .await
            .assert_error(StatusCode::CONFLICT);
    }
    assert_eq!(
        get(&app, "/api/v1/repos").await.json::<Vec<Repo>>().len(),
        2
    );

    // A repo an open session uses cannot go; one nobody uses can.
    let session = create_with(
        &app,
        json!({
            "prompt": "use it",
            "permission_mode": "autonomous",
            "repos": [{ "repo_id": api_repo.id, "mode": "live" }],
        }),
    )
    .await;
    let in_use = format!("/api/v1/repos/{}", api_repo.id);
    delete(&app, &in_use)
        .await
        .assert_error(StatusCode::CONFLICT);
    wait_for(&app, &session.session.id, SessionStatus::Review).await;
    delete(&app, &in_use)
        .await
        .assert_error(StatusCode::CONFLICT);

    let unused = format!("/api/v1/repos/{}", web_repo.id);
    let reply = delete(&app, &unused).await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text());
    assert!(reply.body.is_empty());
    delete(&app, &unused)
        .await
        .assert_error(StatusCode::NOT_FOUND);
    delete(&app, "/api/v1/repos/nope")
        .await
        .assert_error(StatusCode::NOT_FOUND);
    // Only the registry entry went.
    assert!(web.join(".git").is_dir());

    // Once the session is cancelled, its repo is free.
    let stop = format!("/api/v1/sessions/{}/stop", session.session.id);
    assert_eq!(post_empty(&app, &stop).await.status, StatusCode::ACCEPTED);
    assert_eq!(delete(&app, &in_use).await.status, StatusCode::NO_CONTENT);
    assert!(
        get(&app, "/api/v1/repos")
            .await
            .json::<Vec<Repo>>()
            .is_empty()
    );
}

/// A session in review with two repos: `api` as a worktree (where the mock
/// left its notes file) and `docs` attached read-only.
struct Workspace {
    app: Router,
    state: AppState,
    session_id: String,
    /// The worktree of `api`.
    worktree: PathBuf,
    /// The clone of `docs`.
    docs: PathBuf,
    /// `/api/v1/sessions/{id}/repos/{session_repo_id}` of each.
    api_url: String,
    docs_url: String,
    // Held for their destructors.
    _home: TempDir,
    clones: TempDir,
}

async fn workspace() -> Workspace {
    let home = TempDir::new().unwrap();
    let clones = TempDir::new().unwrap();
    let state = state(&home, true).await;
    let app = router(state.clone());
    let api = register(&app, &git_repo(clones.path(), "api")).await;
    let docs = register(&app, &git_repo(clones.path(), "docs")).await;
    let created = create_with(
        &app,
        json!({
            "prompt": "Make a note",
            "permission_mode": "autonomous",
            "repos": [
                { "repo_id": api.id },
                { "repo_id": docs.id, "mode": "read" },
            ],
        }),
    )
    .await;
    let session_id = created.session.id;
    let detail = wait_for(&app, &session_id, SessionStatus::Review).await;
    assert_eq!(detail.repos.len(), 2);
    assert_eq!(detail.repos[0].mode, RepoMode::Write);
    assert!(detail.repos[0].primary);
    assert_eq!(detail.session.branch, detail.repos[0].branch);
    assert!(detail.session.branch.is_some());
    let url = |index: usize| {
        format!(
            "/api/v1/sessions/{session_id}/repos/{}",
            detail.repos[index].id
        )
    };
    Workspace {
        api_url: url(0),
        docs_url: url(1),
        worktree: PathBuf::from(&detail.repos[0].path),
        docs: PathBuf::from(&detail.repos[1].path),
        app,
        state,
        session_id,
        _home: home,
        clones,
    }
}

#[tokio::test]
async fn changes_and_diffs_show_what_the_session_did() {
    let ws = workspace().await;
    let app = &ws.app;
    // On top of the agent's new file: an edit to a tracked file.
    std::fs::write(
        ws.worktree.join("src/lib.rs"),
        "pub fn one() -> u8 {\n    2\n}\n\npub fn two() {}\n",
    )
    .unwrap();
    // A change in the read-only repo is not the session's to report.
    std::fs::write(ws.docs.join("draft.md"), "draft\n").unwrap();

    let reply = get(app, &format!("/api/v1/sessions/{}/changes", ws.session_id)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let changes: Vec<RepoChanges> = reply.json();
    assert_eq!(changes.len(), 2);
    let api = &changes[0];
    assert_eq!(api.name, "api");
    assert!(ws.api_url.ends_with(&api.session_repo_id));
    assert!(
        api.branch
            .as_deref()
            .unwrap()
            .starts_with("calin/make-a-note-")
    );
    assert!(api.base_ref.is_some());
    let files: Vec<(&str, ChangeStatus, u32, u32)> = api
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.status, file.added, file.removed))
        .collect();
    assert_eq!(
        files,
        [
            ("calin-mock-notes.md", ChangeStatus::Untracked, 1, 0),
            ("src/lib.rs", ChangeStatus::Modified, 3, 1),
        ]
    );
    assert_eq!((api.added, api.removed), (4, 1));
    let docs = &changes[1];
    assert_eq!(docs.name, "docs");
    assert!(docs.files.is_empty());
    assert_eq!((docs.added, docs.removed, &docs.branch), (0, 0, &None));

    let diff: FileDiff = get(app, &format!("{}/diff?path=src/lib.rs", ws.api_url))
        .await
        .json();
    assert_eq!(diff.path, "src/lib.rs");
    assert_eq!(
        diff.old.as_deref(),
        Some("pub fn one() -> u8 {\n    1\n}\n")
    );
    assert!(diff.new.as_deref().unwrap().contains("pub fn two() {}"));
    assert!(!diff.binary && !diff.too_large);
    let diff: FileDiff = get(
        app,
        &format!("{}/diff?path=calin-mock-notes.md", ws.api_url),
    )
    .await
    .json();
    assert_eq!(diff.old, None);
    assert_eq!(diff.new.as_deref(), Some("- turn 1: Make a note\n"));

    // A renamed file is diffed against where it came from.
    git(&ws.worktree, &["mv", "README.md", "GUIDE.md"]);
    let diff: FileDiff = get(app, &format!("{}/diff?path=GUIDE.md", ws.api_url))
        .await
        .json();
    assert_eq!(diff.old_path.as_deref(), Some("README.md"));
    assert_eq!(diff.old, diff.new);
    assert_eq!(diff.new.as_deref(), Some("# readme\n"));

    // A side over the limit is left out and flagged; so is a binary file.
    let large = "x".repeat(MAX_FILE_BYTES as usize + 1);
    std::fs::write(ws.worktree.join("large.txt"), &large).unwrap();
    std::fs::write(ws.worktree.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
    let diff: FileDiff = get(app, &format!("{}/diff?path=large.txt", ws.api_url))
        .await
        .json();
    assert!(diff.too_large && diff.old.is_none() && diff.new.is_none());
    let diff: FileDiff = get(app, &format!("{}/diff?path=blob.bin", ws.api_url))
        .await
        .json();
    assert!(diff.binary && diff.new.is_none());

    // 404: no file at that path on either side, an unknown repo or session.
    for path in [
        format!("{}/diff?path=nope.txt", ws.api_url),
        format!("{}/diff?path=src", ws.api_url),
        format!(
            "/api/v1/sessions/{}/repos/nope/diff?path=README.md",
            ws.session_id
        ),
        format!("/api/v1/sessions/{UNKNOWN_SESSION}/repos/x/diff?path=README.md"),
        format!("/api/v1/sessions/{UNKNOWN_SESSION}/changes"),
    ] {
        get(app, &path).await.assert_error(StatusCode::NOT_FOUND);
    }
    // 400: a diff needs a file.
    for path in ["/diff", "/diff?path="] {
        get(app, &format!("{}{path}", ws.api_url))
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn the_tree_and_files_are_read_one_level_and_one_file_at_a_time() {
    let ws = workspace().await;
    let app = &ws.app;
    std::fs::write(ws.worktree.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir(ws.worktree.join("target")).unwrap();

    // An absent or empty `path` lists the root; `.git` is never in it.
    for root in ["/tree", "/tree?path="] {
        let reply = get(app, &format!("{}{root}", ws.api_url)).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        let entries: Vec<TreeEntry> = reply.json();
        let listed: Vec<(&str, EntryKind, bool)> = entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry.kind, entry.ignored))
            .collect();
        assert_eq!(
            listed,
            [
                ("src", EntryKind::Dir, false),
                ("target", EntryKind::Dir, true),
                (".gitignore", EntryKind::File, false),
                ("calin-mock-notes.md", EntryKind::File, false),
                ("README.md", EntryKind::File, false),
            ]
        );
    }
    let entries: Vec<TreeEntry> = get(app, &format!("{}/tree?path=src", ws.api_url))
        .await
        .json();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path, "src/lib.rs");
    assert_eq!(entries[0].name, "lib.rs");
    assert_eq!(entries[0].size, Some(29));
    // The read-only repo can be browsed like any other.
    let entries: Vec<TreeEntry> = get(app, &format!("{}/tree", ws.docs_url)).await.json();
    assert!(entries.iter().any(|entry| entry.path == "README.md"));

    let reply = get(app, &format!("{}/file?path=src/lib.rs", ws.api_url)).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let content: FileContent = reply.json();
    assert_eq!(content.path, "src/lib.rs");
    assert_eq!(
        content.text.as_deref(),
        Some("pub fn one() -> u8 {\n    1\n}\n")
    );
    assert_eq!(content.size, 29);
    assert_eq!(content.hash.len(), 64);
    assert!(!content.binary && !content.too_large);

    // Exactly at the limit is still text; one byte more is not sent.
    let at_limit = "y".repeat(MAX_FILE_BYTES as usize);
    std::fs::write(ws.worktree.join("limit.txt"), &at_limit).unwrap();
    let content: FileContent = get(app, &format!("{}/file?path=limit.txt", ws.api_url))
        .await
        .json();
    assert_eq!(content.text.map(|text| text.len()), Some(at_limit.len()));
    std::fs::write(ws.worktree.join("large.txt"), format!("{at_limit}y")).unwrap();
    let content: FileContent = get(app, &format!("{}/file?path=large.txt", ws.api_url))
        .await
        .json();
    assert!(content.too_large && content.text.is_none());
    assert_eq!(content.size, MAX_FILE_BYTES + 1);
    std::fs::write(ws.worktree.join("blob.bin"), [0u8, 1, 2]).unwrap();
    let content: FileContent = get(app, &format!("{}/file?path=blob.bin", ws.api_url))
        .await
        .json();
    assert!(content.binary && content.text.is_none());

    // 404: no such entry, repo or session.
    for path in [
        format!("{}/tree?path=nope", ws.api_url),
        format!("{}/file?path=nope.txt", ws.api_url),
        format!("{}/file?path=src/nope/deep.txt", ws.api_url),
        format!("/api/v1/sessions/{}/repos/nope/tree", ws.session_id),
        format!(
            "/api/v1/sessions/{}/repos/nope/file?path=README.md",
            ws.session_id
        ),
        format!("/api/v1/sessions/{UNKNOWN_SESSION}/repos/x/tree"),
        format!("/api/v1/sessions/{UNKNOWN_SESSION}/repos/x/file?path=README.md"),
    ] {
        get(app, &path).await.assert_error(StatusCode::NOT_FOUND);
    }
    // 400: the wrong kind of entry.
    for path in [
        "/tree?path=README.md",
        "/file?path=src",
        "/file",
        "/file?path=",
    ] {
        get(app, &format!("{}{path}", ws.api_url))
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn paths_that_leave_the_repo_are_refused_on_every_file_route() {
    let ws = workspace().await;
    let app = &ws.app;
    // Something worth stealing next to the repo, and links that reach for it.
    let secret = ws.clones.path().join("secret.txt");
    std::fs::write(&secret, "secret\n").unwrap();
    std::os::unix::fs::symlink(&secret, ws.worktree.join("link.txt")).unwrap();
    std::os::unix::fs::symlink(ws.clones.path(), ws.worktree.join("outside")).unwrap();
    std::os::unix::fs::symlink(ws.worktree.join(".git"), ws.worktree.join("dotgit")).unwrap();

    let escapes = [
        "..",
        "../secret.txt",
        "src/../../secret.txt",
        "src/%2E%2E/%2E%2E/secret.txt",
        "..%2Fsecret.txt",
        "/etc/passwd",
        "%2Fetc%2Fpasswd",
        ".git/config",
        ".git",
        ".GIT/config",
        "src/../.git/HEAD",
        "link.txt",
        "outside/secret.txt",
        "outside",
        "dotgit/config",
    ];
    for escape in escapes {
        for route in ["diff", "tree", "file"] {
            let path = format!("{}/{route}?path={escape}", ws.api_url);
            let reply = get(app, &path).await;
            reply.assert_error(StatusCode::BAD_REQUEST);
            assert!(!reply.text().contains("secret\n"), "{path}");
        }
        let path = format!("{}/file?path={escape}", ws.api_url);
        send_json(app, Method::PUT, &path, json!({ "text": "pwned\n" }))
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    // Nothing was written through any of them.
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret\n");
    assert!(!ws.clones.path().join("pwned").exists());
    let config = std::fs::read_to_string(ws.worktree.join(".git")).unwrap();
    assert!(config.starts_with("gitdir:"), "{config}");
    // A link that stays inside the repo is an ordinary file to read.
    std::os::unix::fs::symlink("README.md", ws.worktree.join("inside.md")).unwrap();
    let content: FileContent = get(app, &format!("{}/file?path=inside.md", ws.api_url))
        .await
        .json();
    assert_eq!(content.text.as_deref(), Some("# readme\n"));
}

#[tokio::test]
async fn saving_a_file_checks_the_hash_and_is_recorded_as_the_users_change() {
    let ws = workspace().await;
    let app = &ws.app;
    let file = format!("{}/file?path=src/lib.rs", ws.api_url);
    let put = |path: String, body: serde_json::Value| async move {
        send_json(app, Method::PUT, &path, body).await
    };
    let before: FileContent = get(app, &file).await.json();
    let head = wait_for(app, &ws.session_id, SessionStatus::Review)
        .await
        .session
        .head_seq;

    let reply = put(
        file.clone(),
        json!({ "text": "pub fn two() {}\n", "expected_hash": before.hash }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let saved: FileContent = reply.json();
    assert_eq!(saved.path, "src/lib.rs");
    assert_eq!(saved.text.as_deref(), Some("pub fn two() {}\n"));
    assert_ne!(saved.hash, before.hash);
    assert_eq!(
        std::fs::read_to_string(ws.worktree.join("src/lib.rs")).unwrap(),
        "pub fn two() {}\n"
    );
    // What the save returned is what a read returns now.
    assert_eq!(get(app, &file).await.json::<FileContent>(), saved);

    // 409: the hash the edit started from is stale. Nothing changes.
    let stale = put(
        file.clone(),
        json!({ "text": "lost\n", "expected_hash": before.hash }),
    )
    .await;
    stale.assert_error(StatusCode::CONFLICT);
    // 409: no hash means "create", and the file exists.
    put(file.clone(), json!({ "text": "lost\n" }))
        .await
        .assert_error(StatusCode::CONFLICT);
    // 409: a hash for a file that is not there.
    let gone = format!("{}/file?path=src/gone.rs", ws.api_url);
    put(gone, json!({ "text": "x", "expected_hash": before.hash }))
        .await
        .assert_error(StatusCode::CONFLICT);
    assert_eq!(get(app, &file).await.json::<FileContent>(), saved);

    // A new file, in a directory that does not exist yet.
    let new = format!("{}/file?path=docs/notes/new.md", ws.api_url);
    let reply = put(new, json!({ "text": "# new\n" })).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.json::<FileContent>().path, "docs/notes/new.md");
    assert_eq!(
        std::fs::read_to_string(ws.worktree.join("docs/notes/new.md")).unwrap(),
        "# new\n"
    );

    // 409: the repo is attached read-only. 404: unknown repo or session.
    let read_only = format!("{}/file?path=new.md", ws.docs_url);
    put(read_only, json!({ "text": "x" }))
        .await
        .assert_error(StatusCode::CONFLICT);
    assert!(!ws.docs.join("new.md").exists());
    for path in [
        format!(
            "/api/v1/sessions/{}/repos/nope/file?path=a.txt",
            ws.session_id
        ),
        format!("/api/v1/sessions/{UNKNOWN_SESSION}/repos/x/file?path=a.txt"),
    ] {
        put(path, json!({ "text": "x" }))
            .await
            .assert_error(StatusCode::NOT_FOUND);
    }
    // 400: a body that is not a save, or a path that is not a file.
    for body in [json!({}), json!({ "text": 7 }), json!("text")] {
        put(file.clone(), body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    let directory = format!("{}/file?path=src", ws.api_url);
    put(directory, json!({ "text": "x" }))
        .await
        .assert_error(StatusCode::BAD_REQUEST);

    // The two saves that happened are in the log, as the user's; the
    // refused ones left no trace.
    let events = events_until(&ws.state, &ws.session_id, |event| event.seq == head + 2).await;
    let saves: Vec<(&Event, &str, FileOp)> = events[head as usize..]
        .iter()
        .map(|event| match &event.body {
            EventBody::FileChanged {
                session_repo_id: Some(repo),
                path,
                op,
            } => {
                assert!(ws.api_url.ends_with(repo.as_str()));
                (event, path.as_str(), *op)
            }
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(saves.len(), 2);
    assert_eq!((saves[0].1, saves[0].2), ("src/lib.rs", FileOp::Edit));
    assert_eq!(
        (saves[1].1, saves[1].2),
        ("docs/notes/new.md", FileOp::Create)
    );
    assert!(saves.iter().all(|(event, ..)| event.agent_id.is_none()));
    let detail: SessionDetail = get(app, &format!("/api/v1/sessions/{}", ws.session_id))
        .await
        .json();
    assert_eq!(detail.session.head_seq, head + 2);
}

// --- Permissions and interrupt ---

#[tokio::test]
async fn permission_requests_are_answered_through_the_api() {
    let dir = TempDir::new().unwrap();
    let state = state(&dir, true).await;
    let app = router(state.clone());
    // `ask` is the default mode: the mock asks before its command.
    let id = create_with(&app, json!({ "prompt": "ask me" }))
        .await
        .session
        .id;
    let events = events_until(&state, &id, is_waiting).await;
    let [request] = pending_requests(&events).try_into().expect("one request");
    let detail = wait_for(&app, &id, SessionStatus::Waiting).await;
    assert_eq!(detail.session.permission_mode, PermissionMode::Ask);
    let answer = |request_id: &str| format!("/api/v1/sessions/{id}/permissions/{request_id}");

    // 400: not an answer.
    for body in [json!({}), json!({ "decision": "maybe" }), json!("allow")] {
        post(&app, &answer(&request.id), body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    // 404: no such request, or no such session.
    post(&app, &answer("nope"), json!({ "decision": "allow" }))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let elsewhere = format!(
        "/api/v1/sessions/{UNKNOWN_SESSION}/permissions/{}",
        request.id
    );
    post(&app, &elsewhere, json!({ "decision": "allow" }))
        .await
        .assert_error(StatusCode::NOT_FOUND);

    let reply = post(
        &app,
        &answer(&request.id),
        json!({ "decision": "deny", "message": "not now" }),
    )
    .await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    // 409: it was answered.
    post(&app, &answer(&request.id), json!({ "decision": "allow" }))
        .await
        .assert_error(StatusCode::CONFLICT);

    // The turn goes on to the edit, which asks too.
    let is_second_wait = |event: &Event| is_waiting(event) && event.seq > events.len() as u64;
    let events = events_until(&state, &id, is_second_wait).await;
    let [edit] = pending_requests(&events).try_into().expect("one request");
    assert_ne!(edit.id, request.id);
    let reply = post(&app, &answer(&edit.id), json!({ "decision": "allow" })).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    wait_for(&app, &id, SessionStatus::Review).await;

    let events = events_until(&state, &id, is_review).await;
    let answers: Vec<(PermissionDecision, DecisionSource, Option<&str>)> = events
        .iter()
        .filter_map(|event| match &event.body {
            EventBody::PermissionResolved {
                decision,
                by,
                message,
                ..
            } => Some((*decision, *by, message.as_deref())),
            _ => None,
        })
        .collect();
    assert_eq!(
        answers,
        [
            (
                PermissionDecision::Deny,
                DecisionSource::User,
                Some("not now")
            ),
            (PermissionDecision::Allow, DecisionSource::User, None),
        ]
    );
    assert!(pending_requests(&events).is_empty());
}

#[tokio::test]
async fn interrupt_ends_the_turn_and_is_refused_without_one() {
    let dir = TempDir::new().unwrap();
    let state = state(&dir, true).await;
    let app = router(state.clone());
    let id = create_with(&app, json!({ "prompt": "interrupt me" }))
        .await
        .session
        .id;
    let interrupt = format!("/api/v1/sessions/{id}/interrupt");
    let events = events_until(&state, &id, is_waiting).await;
    let [request] = pending_requests(&events).try_into().expect("one request");

    // Waiting on a permission request is still inside a turn.
    let reply = post_empty(&app, &interrupt).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    wait_for(&app, &id, SessionStatus::Review).await;
    let events = events_until(&state, &id, is_review).await;
    // The request nobody answered was closed for the user.
    let expired = events
        .iter()
        .find_map(|event| match &event.body {
            EventBody::PermissionResolved {
                request_id,
                decision,
                by,
                message,
            } => Some((request_id.clone(), *decision, *by, message.clone())),
            _ => None,
        })
        .expect("the open request was not closed");
    assert_eq!(expired.0, request.id);
    assert_eq!(
        (expired.1, expired.2),
        (PermissionDecision::Deny, DecisionSource::Expired)
    );
    assert!(expired.3.unwrap().contains("turn ended"));
    let answer = format!("/api/v1/sessions/{id}/permissions/{}", request.id);
    post(&app, &answer, json!({ "decision": "allow" }))
        .await
        .assert_error(StatusCode::CONFLICT);

    // 409: in review there is no turn; nor once the session is cancelled.
    post_empty(&app, &interrupt)
        .await
        .assert_error(StatusCode::CONFLICT);
    let stop = format!("/api/v1/sessions/{id}/stop");
    assert_eq!(post_empty(&app, &stop).await.status, StatusCode::ACCEPTED);
    post_empty(&app, &interrupt)
        .await
        .assert_error(StatusCode::CONFLICT);
    post_empty(
        &app,
        &format!("/api/v1/sessions/{UNKNOWN_SESSION}/interrupt"),
    )
    .await
    .assert_error(StatusCode::NOT_FOUND);
}

// --- The terminal socket, over a real socket ---

fn terminal_url(addr: SocketAddr, session_id: &str, query: &str) -> String {
    format!("ws://{addr}/api/v1/sessions/{session_id}/terminals/ws{query}")
}

async fn open_terminal(addr: SocketAddr, session_id: &str, query: &str) -> Client {
    let (client, _) = tokio_tungstenite::connect_async(terminal_url(addr, session_id, query))
        .await
        .unwrap();
    client
}

/// The status a terminal handshake is refused with.
async fn refused_terminal(addr: SocketAddr, session_id: &str, query: &str) -> StatusCode {
    match tokio_tungstenite::connect_async(terminal_url(addr, session_id, query)).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status(),
        Ok(_) => panic!("the handshake was accepted"),
        Err(other) => panic!("expected a refused handshake, got {other:?}"),
    }
}

async fn type_in(client: &mut Client, text: &str) {
    client
        .send(Message::Binary(text.as_bytes().to_vec().into()))
        .await
        .unwrap();
}

/// What a terminal socket said until it closed, or until `stop` was true.
#[derive(Default)]
struct Screen {
    /// Every binary frame so far, decoded.
    text: String,
    frames: Vec<TermServerMsg>,
    closed: bool,
}

impl Screen {
    /// Reads until the shell has printed `needle`.
    async fn until(&mut self, client: &mut Client, needle: &str) {
        self.read(client, |screen| screen.text.contains(needle))
            .await;
        assert!(
            self.text.contains(needle),
            "the terminal never printed {needle:?}; it printed {:?} and sent {:?}",
            self.text,
            self.frames
        );
    }

    async fn until_closed(&mut self, client: &mut Client) {
        self.read(client, |_| false).await;
        assert!(self.closed);
    }

    async fn read(&mut self, client: &mut Client, stop: impl Fn(&Screen) -> bool) {
        while !stop(self) && !self.closed {
            let frame = tokio::time::timeout(TIMEOUT, client.next())
                .await
                .unwrap_or_else(|_| panic!("the terminal went quiet after {:?}", self.text));
            match frame {
                Some(Ok(Message::Binary(bytes))) => {
                    self.text.push_str(&String::from_utf8_lossy(&bytes));
                }
                Some(Ok(Message::Text(text))) => {
                    self.frames.push(serde_json::from_str(&text).unwrap());
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => self.closed = true,
                Some(Ok(_)) => {}
            }
        }
    }
}

/// The number printed between `before` and `after` in the terminal's output.
fn number_between(text: &str, before: &str, after: &str) -> u32 {
    let start = text.rfind(before).expect("the marker was printed") + before.len();
    let end = text[start..]
        .find(after)
        .expect("the end marker was printed");
    text[start..start + end].parse().expect("a number")
}

/// A session in review with one worktree, on a running server.
async fn running_with_a_repo(home: &TempDir, clones: &TempDir) -> (Running, SessionDetail) {
    let running = start(home).await;
    let app = router(running.state.clone());
    let repo = register(&app, &git_repo(clones.path(), "api")).await;
    let created = create_with(
        &app,
        json!({
            "prompt": "terminal",
            "permission_mode": "autonomous",
            "repos": [{ "repo_id": repo.id }],
        }),
    )
    .await;
    let detail = wait_for(&app, &created.session.id, SessionStatus::Review).await;
    (running, detail)
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_runs_a_shell_in_the_session_and_reports_its_exit() {
    let (home, clones) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (running, detail) = running_with_a_repo(&home, &clones).await;
    let id = &detail.session.id;
    let mut client = open_terminal(running.addr, id, "?cols=90&rows=20").await;
    let mut screen = Screen::default();

    // Bytes in, bytes out. The arithmetic tells the shell's answer from the
    // terminal's echo of what was typed.
    type_in(&mut client, "echo calin-$((6*7))\n").await;
    screen.until(&mut client, "calin-42").await;

    // It runs in the session's working directory and knows its session.
    type_in(&mut client, "echo \"dir=$(pwd)=\"\n").await;
    screen
        .until(&mut client, &format!("dir={}=", detail.repos[0].path))
        .await;
    type_in(&mut client, "echo \"sid=$CALIN_SESSION_ID=\"\n").await;
    screen.until(&mut client, &format!("sid={id}=")).await;

    // The size asked for at connect, then the one a resize frame sets.
    type_in(&mut client, "echo \"size=$(stty size)=\"\n").await;
    screen.until(&mut client, "size=20 90=").await;
    let resize = TermClientMsg::Resize {
        cols: 100,
        rows: 30,
    };
    let frame = serde_json::to_string(&resize).unwrap();
    client.send(Message::Text(frame.into())).await.unwrap();
    // Frames are handled in order, so the resize is in before this command.
    type_in(&mut client, "echo \"size=$(stty size)=\"\n").await;
    screen.until(&mut client, "size=30 100=").await;
    // Text that is not a known frame is ignored, not fatal.
    client.send(Message::Text("{}".into())).await.unwrap();
    type_in(&mut client, "echo still-$((1+1))\n").await;
    screen.until(&mut client, "still-2").await;
    assert!(screen.frames.is_empty(), "{:?}", screen.frames);

    // The program's exit is one frame with its code, then the socket closes.
    type_in(&mut client, "exit 7\n").await;
    screen.until_closed(&mut client).await;
    assert_eq!(screen.frames, [TermServerMsg::Exit { code: Some(7) }]);

    // A terminal can start in a repo by id, and its slot came back.
    let query = format!("?session_repo_id={}", detail.repos[0].id);
    let mut client = open_terminal(running.addr, id, &query).await;
    let mut screen = Screen::default();
    type_in(&mut client, "echo \"size=$(stty size)=\"\n").await;
    screen.until(&mut client, "size=24 80=").await;
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn closing_the_socket_kills_the_shell_and_what_it_started() {
    let (home, clones) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (running, detail) = running_with_a_repo(&home, &clones).await;
    let mut client = open_terminal(running.addr, &detail.session.id, "").await;
    let mut screen = Screen::default();
    // A background job outlives a shell that is merely hung up on.
    type_in(&mut client, "sleep 300 &\necho \"pids=$$,$!=\"\n").await;
    screen.until(&mut client, "=\r").await;
    let shell = number_between(&screen.text, "pids=", ",");
    let job = number_between(&screen.text, &format!("pids={shell},"), "=");
    let alive = |pid: u32| Path::new(&format!("/proc/{pid}")).exists();
    assert!(alive(shell) && alive(job));

    client.close(None).await.unwrap();
    drop(client);
    tokio::time::timeout(TIMEOUT, async {
        while alive(shell) || alive(job) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the shell survived its socket");

    // The same when the server shuts down with a terminal open.
    let mut client = open_terminal(running.addr, &detail.session.id, "").await;
    let mut screen = Screen::default();
    type_in(&mut client, "echo \"pid=$$=\"\n").await;
    screen.until(&mut client, "=\r").await;
    let shell = number_between(&screen.text, "pid=", "=");
    running.stop.send(()).unwrap();
    tokio::time::timeout(TIMEOUT, running.server)
        .await
        .expect("shutdown hung on an open terminal")
        .unwrap()
        .unwrap();
    screen.until_closed(&mut client).await;
    tokio::time::timeout(TIMEOUT, async {
        while alive(shell) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the shell survived the server");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_session_has_a_bounded_number_of_terminals() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let id = new_session(&running.state, "many terminals").await;
    let other = new_session(&running.state, "one terminal").await;

    let mut open = Vec::new();
    for _ in 0..term::MAX_TERMINALS_PER_SESSION {
        open.push(open_terminal(running.addr, &id, "").await);
    }
    // One more is told why not, in a frame, and hung up on.
    let mut extra = open_terminal(running.addr, &id, "").await;
    let mut screen = Screen::default();
    screen.until_closed(&mut extra).await;
    assert!(
        matches!(
            screen.frames.as_slice(),
            [TermServerMsg::Error { message }] if message.contains("terminals open")
        ),
        "{:?}",
        screen.frames
    );
    assert_eq!(screen.text, "");

    // The limit is per session, and the ones that are open still work.
    let mut elsewhere = open_terminal(running.addr, &other, "").await;
    for client in [&mut elsewhere, &mut open[0]] {
        let mut screen = Screen::default();
        type_in(client, "echo ok-$((2+3))\n").await;
        screen.until(client, "ok-5").await;
    }

    // Closing one makes room again, once its shell is gone.
    let mut closed = open.pop().unwrap();
    closed.close(None).await.unwrap();
    drop(closed);
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let mut client = open_terminal(running.addr, &id, "").await;
            let mut screen = Screen::default();
            type_in(&mut client, "echo ok-$((2+3))\n").await;
            screen
                .read(&mut client, |screen| screen.text.contains("ok-5"))
                .await;
            if !screen.closed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the closed terminal never gave its slot back");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_terminal_for_something_unknown_is_refused_before_the_upgrade() {
    let (home, clones) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let (running, detail) = running_with_a_repo(&home, &clones).await;
    let id = &detail.session.id;
    assert_eq!(
        refused_terminal(running.addr, UNKNOWN_SESSION, "").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        refused_terminal(running.addr, id, "?session_repo_id=nope").await,
        StatusCode::NOT_FOUND
    );
    for query in ["?cols=wide", "?rows=-1", "?cols=70000"] {
        assert_eq!(
            refused_terminal(running.addr, id, query).await,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }

    // Without a WebSocket handshake the answers are the usual JSON errors.
    let app = router(running.state.clone());
    let plain = format!("/api/v1/sessions/{id}/terminals/ws");
    get(&app, &plain)
        .await
        .assert_error(StatusCode::BAD_REQUEST);
    get(
        &app,
        &format!("/api/v1/sessions/{UNKNOWN_SESSION}/terminals/ws"),
    )
    .await
    .assert_error(StatusCode::NOT_FOUND);
    get(&app, &format!("{plain}?session_repo_id=nope"))
        .await
        .assert_error(StatusCode::NOT_FOUND);

    // The guard that protects the rest of the API stands in front of it: a
    // page on another site cannot open a shell here.
    let mut request = terminal_url(running.addr, id, "")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(ORIGIN, "https://evil.example".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        other => panic!("expected a refused handshake, got {:?}", other.map(|_| ())),
    }
    let handshake = |host: &'static str, origin: Option<&'static str>| {
        let mut builder = Request::builder()
            .uri(&plain)
            .header(HOST, host)
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==");
        if let Some(origin) = origin {
            builder = builder.header(ORIGIN, origin);
        }
        call(&app, builder.body(Body::empty()).unwrap())
    };
    handshake("evil.example", None)
        .await
        .assert_error(StatusCode::FORBIDDEN);
    handshake(LOCAL, Some("null"))
        .await
        .assert_error(StatusCode::FORBIDDEN);
    handshake(LOCAL, Some("http://localhost.evil.example"))
        .await
        .assert_error(StatusCode::FORBIDDEN);
    // A loopback origin passes the guard; what is left is that this test
    // connection cannot be upgraded, which is not a 403.
    let passed = handshake(LOCAL, Some("http://localhost:5173")).await;
    assert_ne!(passed.status, StatusCode::FORBIDDEN);

    // And one that starts a program that does not exist says so in a frame.
    let state = running
        .state
        .clone()
        .with_terminal_command("/nonexistent/calin/shell", [] as [&str; 0]);
    let broken = Server::bind("127.0.0.1:0".parse().unwrap(), state)
        .await
        .unwrap();
    let addr = broken.local_addr().unwrap();
    let serving = tokio::spawn(broken.run(std::future::pending()));
    let mut client = open_terminal(addr, id, "").await;
    let mut screen = Screen::default();
    screen.until_closed(&mut client).await;
    assert!(
        matches!(screen.frames.as_slice(), [TermServerMsg::Error { .. }]),
        "{:?}",
        screen.frames
    );
    serving.abort();
}
