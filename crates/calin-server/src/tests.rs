use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, HOST, ORIGIN};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use calin_agents::{MockConfig, RegistryConfig, RunnerRegistry};
use calin_core::{EventDraft, Home, SessionManager, Store};
use calin_proto::{
    ApiError, ClientMsg, CreateSessionRequest, Event, EventBody, Health, ServerMsg, Session,
    SessionDetail, SessionStatus, TOPIC_SESSIONS, session_topic,
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

const TIMEOUT: Duration = Duration::from_secs(20);
const LOCAL: &str = "127.0.0.1:4680";
const UNKNOWN_SESSION: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

static WEB_APP: &[(&str, &[u8])] = &[
    ("assets/app-abc123.js", b"console.log('calin')"),
    ("favicon.svg", b"<svg/>"),
    ("index.html", b"<!doctype html><title>the app</title>"),
];

/// A state on a temporary home, with instant mock agents and no web build.
async fn state(dir: &TempDir, dry_run: bool) -> AppState {
    let store = Store::open(Home::new(dir.path())).await.unwrap();
    let registry = RunnerRegistry::new(RegistryConfig {
        dry_run,
        mock: MockConfig {
            delay: Duration::ZERO,
        },
    });
    let manager = SessionManager::start(store, registry).await.unwrap();
    AppState::new(manager).with_assets(Assets::empty())
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

async fn create(app: &Router, prompt: &str) -> SessionDetail {
    let reply = post(app, "/api/v1/sessions", json!({ "prompt": prompt })).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    reply.json()
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
        }
    );

    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, false).await);
    assert!(!get(&app, "/api/v1/health").await.json::<Health>().dry_run);
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
    assert_eq!(first.agents.len(), 1);
    assert_eq!(first.agents[0].role, "lead");
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

    let listed: Vec<Session> = get(&app, "/api/v1/sessions").await.json();
    let ids: Vec<&str> = listed.iter().map(|session| session.id.as_str()).collect();
    assert_eq!(ids, [second.session.id.as_str(), first.session.id.as_str()]);

    let done = wait_for(&app, &first.session.id, SessionStatus::Review).await;
    assert_eq!(done.session.id, first.session.id);
    assert_eq!(done.agents[0].tokens_in, 1200);
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
async fn messages_are_accepted_until_the_session_is_closed() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
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

    post(&app, &messages, json!({ "text": "too late" }))
        .await
        .assert_error(StatusCode::CONFLICT);
    post_empty(&app, &stop)
        .await
        .assert_error(StatusCode::CONFLICT);
    post_empty(&app, &format!("/api/v1/sessions/{UNKNOWN_SESSION}/stop"))
        .await
        .assert_error(StatusCode::NOT_FOUND);
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

    let unknown = session_topic(UNKNOWN_SESSION);
    for topic in [unknown.as_str(), "session:", "nonsense", "", "sessions:all"] {
        send(&mut client, &subscribe(topic, None)).await;
        assert!(
            matches!(next(&mut client).await, ServerMsg::Error { .. }),
            "{topic}"
        );
    }
    for raw in [
        "not json",
        "{}",
        r#"{"op":"dance"}"#,
        r#"{"op":"subscribe"}"#,
    ] {
        client.send(Message::Text(raw.into())).await.unwrap();
        assert!(
            matches!(next(&mut client).await, ServerMsg::Error { .. }),
            "{raw}"
        );
    }
    client
        .send(Message::Binary(vec![1, 2, 3].into()))
        .await
        .unwrap();
    assert!(matches!(next(&mut client).await, ServerMsg::Error { .. }));

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
