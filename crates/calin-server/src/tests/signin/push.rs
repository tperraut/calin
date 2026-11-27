//! Web Push (remote-access spec, section 6): the seven `/push` routes, and
//! the notifications sessions trigger, received by `calin-push`'s fake push
//! service, which decrypts each one with the device's keys.

use std::future::IntoFuture;

use calin_agents::{AgentIo, Exit, RunnerOutput, StartParams};
use calin_proto::{
    PullRequestState, PushKey, PushKind, PushPayload, PushPreferences, PushSubscriptionInfo,
    PushSubscriptionRequest,
};
use calin_push::test_service::{FakePushService, ReceivedPush};
use tokio::sync::mpsc;

use super::*;
use crate::notify::{Decision, Outcome};
use crate::tests::catalog::{Starts, call_tool, capturing_state, endpoint_of};

const ALL_OFF: PushPreferences = PushPreferences {
    waiting: false,
    review: false,
    failed: false,
    merged: false,
};

/// More than the library's one-per-minute throttle.
const PAST_THROTTLE: Duration = Duration::from_secs(61);

const GATED: &str = r#"
version: 1
name: Gated
nodes:
  - { id: start, type: start }
  - { id: work, type: agent, prompt: "{{task}}" }
  - { id: gate, type: gate, message: "Good enough to ship?\nSay why not." }
  - { id: end, type: end }
edges:
  - { from: start, to: work }
  - { from: work.done, to: gate }
  - { from: gate.approve, to: end }
"#;

// --- Fixtures ---

/// A public server whose notifications go to a fake push service, with the
/// decisions of its notification task in order.
struct Notified {
    public: Public,
    service: FakePushService,
    decisions: mpsc::UnboundedReceiver<Decision>,
    /// The decisions read so far.
    seen: Vec<Decision>,
    /// With the capturing runner: the agents the test plays.
    starts: Option<Starts>,
    work: TempDir,
    alice: Browser,
}

/// `allowed` may sign in; Alice is signed in with one device, `alice-phone`.
/// With `capture`, agents of runner `capture` are the test's to play;
/// otherwise every agent is the instant mock of a dry run.
async fn notified(allowed: &str, capture: bool) -> Notified {
    let home = TempDir::new().unwrap();
    let (state, starts) = if capture {
        let (state, starts) = capturing_state(&home).await;
        state
            .manager()
            .set_tool_endpoint(format!("http://{LOCAL}/api/v1/mcp"));
        (state, Some(starts))
    } else {
        (super::state(&home, true).await, None)
    };
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    let public = public_on(home, state, clock, provider, allowed).await;
    observed(public, starts).await
}

/// Starts the notification task of `public` and signs Alice in with a device.
async fn observed(public: Public, starts: Option<Starts>) -> Notified {
    let (observer, decisions) = mpsc::unbounded_channel();
    crate::notify::spawn(&public.state, Some(observer)).expect("push is set up");
    let service = FakePushService::start().await;
    let mut alice = Browser::signed_in(&public.app, &public.provider).await;
    subscribe_device(&mut alice, &service, "alice-phone").await;
    Notified {
        public,
        service,
        decisions,
        seen: Vec::new(),
        starts,
        work: TempDir::new().unwrap(),
        alice,
    }
}

async fn subscribe_device(
    browser: &mut Browser,
    service: &FakePushService,
    name: &str,
) -> PushSubscriptionInfo {
    let request = serde_json::to_value(service.subscription(name)).unwrap();
    let reply = browser.post("/api/v1/push/subscriptions", request).await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    reply.json()
}

impl Notified {
    fn state(&self) -> &AppState {
        &self.public.state
    }

    fn manager(&self) -> &SessionManager {
        self.public.state.manager()
    }

    /// The next decision that matches, keeping the ones before it in `seen`.
    async fn decision(&mut self, matches: impl Fn(&Decision) -> bool) -> Decision {
        loop {
            let decision = tokio::time::timeout(TIMEOUT, self.decisions.recv())
                .await
                .unwrap_or_else(|_| panic!("no such decision; seen {:?}", self.seen))
                .expect("the notification task is running");
            self.seen.push(decision.clone());
            if matches(&decision) {
                return decision;
            }
        }
    }

    /// The decision for Alice about `kind` in `session_id`.
    async fn for_alice(&mut self, session_id: &str, kind: PushKind) -> Outcome {
        let decision = self
            .decision(|d| {
                d.session_id == session_id
                    && d.kind == Some(kind)
                    && d.identity.as_deref() == Some(ALICE)
            })
            .await;
        decision.outcome
    }

    fn past_throttle(&self) {
        self.public.push_clock.advance(PAST_THROTTLE);
    }

    async fn create(&self, request: Value) -> SessionDetail {
        let request = serde_json::from_value(request).unwrap();
        self.manager().create_session(request).await.unwrap()
    }

    /// A git repository registered with the server; its id.
    async fn repo(&self, name: &str) -> String {
        let path = git_repo(self.work.path(), name);
        let request = json!({ "path": path });
        let repo = self
            .manager()
            .register_repo(serde_json::from_value(request).unwrap())
            .await
            .unwrap();
        repo.id
    }

    async fn status_reaches(&self, session_id: &str, status: SessionStatus) -> SessionDetail {
        tokio::time::timeout(TIMEOUT, async {
            loop {
                let detail = self.manager().get(session_id).await.unwrap();
                if detail.session.status == status {
                    return detail;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{session_id} never reached {status:?}"))
    }

    async fn allow_the_open_request(&self, session_id: &str) {
        let open = self
            .manager()
            .store()
            .open_permissions(Some(session_id))
            .await
            .unwrap();
        assert_eq!(open.len(), 1, "{open:?}");
        self.manager()
            .resolve_permission(
                session_id,
                &open[0].request_id,
                PermissionDecision::Allow,
                None,
            )
            .await
            .unwrap();
    }

    async fn the_open_question(&self, session_id: &str) -> String {
        let open = self
            .manager()
            .store()
            .open_questions(Some(session_id))
            .await
            .unwrap();
        assert_eq!(open.len(), 1, "{open:?}");
        open[0].question_id.clone()
    }

    /// The next agent the capturing runner starts.
    async fn next_agent(&mut self) -> (StartParams, AgentIo) {
        let starts = self.starts.as_mut().expect("a capturing server");
        tokio::time::timeout(TIMEOUT, starts.recv())
            .await
            .expect("no agent started")
            .unwrap()
    }

    /// A root session of the capturing runner, running; its agent's
    /// endpoint and io.
    async fn played(&mut self, title: &str) -> (String, String, AgentIo) {
        let repo = self.repo("api").await;
        let detail = self
            .create(json!({
                "prompt": "work", "title": title, "runner": "capture",
                "permission_mode": "autonomous", "repos": [{ "repo_id": repo }],
            }))
            .await;
        let (params, io) = self.next_agent().await;
        let id = detail.session.id;
        self.status_reaches(&id, SessionStatus::Running).await;
        (id, endpoint_of(&params), io)
    }

    async fn ask(&self, endpoint: &str, question: &str) {
        let arguments = json!({ "question": question });
        let (text, is_error) = call_tool(&self.public.app, endpoint, "ask_user", arguments).await;
        assert!(!is_error, "{text}");
    }

    fn received(&self) -> Vec<ReceivedPush> {
        self.service.received()
    }
}

fn payload(kind: PushKind, title: &str, body: &str, session_id: &str) -> PushPayload {
    PushPayload {
        kind,
        title: title.to_owned(),
        body: body.to_owned(),
        path: format!("/s/{session_id}"),
        session_id: Some(session_id.to_owned()),
    }
}

async fn end_turn(io: &AgentIo) {
    io.outputs.send(RunnerOutput::TurnEnded).await.unwrap();
}

/// Gives a worktree an identity to commit with, so shipping does not depend
/// on this machine's git configuration.
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

/// Reviews, ships in dry run, then records every pull request merged and
/// the session done, as the pull request watcher does outside dry run.
async fn ship_and_merge(n: &Notified, session_id: &str) {
    let detail = n.manager().get(session_id).await.unwrap();
    give_identity(Path::new(&detail.repos[0].path));
    let request = serde_json::from_value(json!({ "title": "Add the note" })).unwrap();
    n.manager().ship(session_id, request).await.unwrap();
    let shipped = n.status_reaches(session_id, SessionStatus::Shipping).await;
    assert!(!shipped.pull_requests.is_empty());
    let store = n.manager().store();
    for pull_request in shipped.pull_requests {
        let merged = calin_proto::PullRequest {
            state: PullRequestState::Merged,
            ..pull_request
        };
        let body = EventBody::PullRequestUpdated {
            pull_request: merged,
        };
        store
            .append(session_id, EventDraft::new(body))
            .await
            .unwrap();
    }
    let done = EventBody::SessionStatus {
        status: SessionStatus::Done,
        reason: None,
    };
    store
        .append(session_id, EventDraft::new(done))
        .await
        .unwrap();
}

// --- Routes ---

#[tokio::test]
async fn in_local_mode_every_push_route_is_a_409_and_nothing_is_created() {
    let dir = TempDir::new().unwrap();
    let running = start(&dir).await;
    let app = router(running.state.clone());
    let service = FakePushService::start().await;
    let subscription = serde_json::to_value(service.subscription("phone")).unwrap();
    for (method, path, body) in [
        (Method::GET, "/api/v1/push/key", None),
        (Method::GET, "/api/v1/push/subscriptions", None),
        (
            Method::POST,
            "/api/v1/push/subscriptions",
            Some(subscription.clone()),
        ),
        // Even a body that is not a subscription: the mode is decided first.
        (Method::POST, "/api/v1/push/subscriptions", Some(json!(1))),
        (Method::DELETE, "/api/v1/push/subscriptions/abc", None),
        (Method::GET, "/api/v1/push/preferences", None),
        (
            Method::PUT,
            "/api/v1/push/preferences",
            Some(serde_json::to_value(ALL_OFF).unwrap()),
        ),
        (Method::POST, "/api/v1/push/test", None),
    ] {
        let reply = match body {
            Some(body) => send_json(&app, method.clone(), path, body).await,
            None => {
                call(
                    &app,
                    request(method.clone(), path).body(Body::empty()).unwrap(),
                )
                .await
            }
        };
        reply.assert_error(StatusCode::CONFLICT);
        assert!(
            reply.json::<ApiError>().error.contains("local mode"),
            "{method} {path}"
        );
    }
    running.stop.send(()).unwrap();
    running.server.await.unwrap().unwrap();
    let pushed: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("push"))
        .collect();
    assert!(pushed.is_empty(), "{pushed:?}");
    assert!(service.received().is_empty());
}

#[tokio::test]
async fn subscriptions_and_preferences_belong_to_the_caller() {
    let public = public_mode(BOTH).await;
    let service = FakePushService::start().await;
    let mut alice = Browser::signed_in(&public.app, &public.provider).await;
    public.provider.set_user("user-2", Some(BOB), true);
    let mut bob = Browser::signed_in(&public.app, &public.provider).await;

    // One key for the server, the one in its file.
    let key: PushKey = alice.get("/api/v1/push/key").await.json();
    assert_eq!(bob.get("/api/v1/push/key").await.json::<PushKey>(), key);
    let file: Value =
        serde_json::from_slice(&std::fs::read(public.home.path().join(PUSH_KEY_FILE)).unwrap())
            .unwrap();
    assert_eq!(file["public_key"], json!(key.public_key));
    assert_eq!(key.public_key.len(), 87, "65 bytes, base64url");

    // Nothing yet.
    let list = |reply: Reply| reply.json::<Vec<PushSubscriptionInfo>>();
    assert!(list(alice.get("/api/v1/push/subscriptions").await).is_empty());

    // Subscribing: 201, the label from the user agent when none is given.
    let mut laptop = service.subscription("laptop");
    laptop.label = None;
    let reply = alice
        .post(
            "/api/v1/push/subscriptions",
            serde_json::to_value(&laptop).unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
    let first: PushSubscriptionInfo = reply.json();
    assert_eq!(
        first.label,
        calin_push::describe_user_agent("TestBrowser/1.0")
    );
    assert_eq!(first.service, "127.0.0.1");
    assert_eq!(first.last_sent_at, None);
    public.push_clock.advance(Duration::from_secs(5));
    let phone = subscribe_device(&mut alice, &service, "phone").await;
    assert_eq!(phone.label, "phone");
    // Newest first.
    let listed = list(alice.get("/api/v1/push/subscriptions").await);
    assert_eq!(listed, [phone.clone(), first.clone()]);
    // The same endpoint again replaces it and keeps the id.
    laptop.label = Some("Work laptop".to_owned());
    let reply = alice
        .post(
            "/api/v1/push/subscriptions",
            serde_json::to_value(&laptop).unwrap(),
        )
        .await;
    assert_eq!(reply.status, StatusCode::CREATED);
    let again: PushSubscriptionInfo = reply.json();
    assert_eq!(again.id, first.id);
    assert_eq!(again.label, "Work laptop");
    assert_eq!(list(alice.get("/api/v1/push/subscriptions").await).len(), 2);

    // Bob sees none of them and cannot remove them.
    assert!(list(bob.get("/api/v1/push/subscriptions").await).is_empty());
    for id in [first.id.as_str(), "nope"] {
        bob.delete(&format!("/api/v1/push/subscriptions/{id}"))
            .await
            .assert_error(StatusCode::NOT_FOUND);
    }
    assert_eq!(list(alice.get("/api/v1/push/subscriptions").await).len(), 2);

    // 400 with the library's reason.
    let valid = service.subscription("tablet");
    let cases: Vec<(PushSubscriptionRequest, &str)> = vec![
        (
            PushSubscriptionRequest {
                endpoint: "http://push.example.com/x".to_owned(),
                ..valid.clone()
            },
            "the endpoint must be an https URL",
        ),
        (
            PushSubscriptionRequest {
                endpoint: "https://10.0.0.1/x".to_owned(),
                ..valid.clone()
            },
            "public domain name",
        ),
        (
            PushSubscriptionRequest {
                p256dh: "AAAA".to_owned(),
                ..valid.clone()
            },
            "p256dh must be",
        ),
        (
            PushSubscriptionRequest {
                auth: "AAAA".to_owned(),
                ..valid.clone()
            },
            "auth must be 16 bytes",
        ),
    ];
    for (request, reason) in cases {
        let reply = alice
            .post(
                "/api/v1/push/subscriptions",
                serde_json::to_value(&request).unwrap(),
            )
            .await;
        reply.assert_error(StatusCode::BAD_REQUEST);
        let error = reply.json::<ApiError>().error;
        assert!(error.contains(reason), "{error}");
    }
    for body in [json!({}), json!({ "endpoint": 7 }), json!([])] {
        alice
            .post("/api/v1/push/subscriptions", body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    assert_eq!(list(alice.get("/api/v1/push/subscriptions").await).len(), 2);

    // Removing: 204, then 404.
    let path = format!("/api/v1/push/subscriptions/{}", phone.id);
    assert_eq!(alice.delete(&path).await.status, StatusCode::NO_CONTENT);
    alice
        .delete(&path)
        .await
        .assert_error(StatusCode::NOT_FOUND);
    assert_eq!(list(alice.get("/api/v1/push/subscriptions").await), [again]);

    // Preferences: everything on by default, each identity its own.
    let preferences = |reply: Reply| reply.json::<PushPreferences>();
    assert_eq!(
        preferences(alice.get("/api/v1/push/preferences").await),
        PushPreferences::default()
    );
    let chosen = PushPreferences {
        review: false,
        merged: false,
        ..PushPreferences::default()
    };
    let reply = alice
        .request(
            Method::PUT,
            "/api/v1/push/preferences",
            Some(serde_json::to_value(chosen).unwrap()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(preferences(reply), chosen);
    assert_eq!(
        preferences(alice.get("/api/v1/push/preferences").await),
        chosen
    );
    assert_eq!(
        preferences(bob.get("/api/v1/push/preferences").await),
        PushPreferences::default()
    );
    for body in [
        json!({ "waiting": true }),
        json!({ "waiting": "no", "review": true, "failed": true, "merged": true }),
    ] {
        alice
            .request(Method::PUT, "/api/v1/push/preferences", Some(body))
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    // On disk, under each identity.
    let stored: Value = serde_json::from_slice(
        &std::fs::read(public.home.path().join(PUSH_SUBSCRIPTIONS_FILE)).unwrap(),
    )
    .unwrap();
    assert_eq!(stored["identities"][ALICE]["preferences"]["review"], false);
    assert_eq!(
        stored["identities"][ALICE]["subscriptions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(service.received().is_empty());
}

#[tokio::test]
async fn a_device_token_reads_but_does_not_subscribe() {
    let public = public_mode(ALICE).await;
    let service = FakePushService::start().await;
    let mut alice = Browser::signed_in(&public.app, &public.provider).await;
    let phone = subscribe_device(&mut alice, &service, "phone").await;
    let token = alice.create_token("desktop").await.token;

    let reply = with_token(
        &public.app,
        Method::GET,
        "/api/v1/push/subscriptions",
        &token,
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.json::<Vec<PushSubscriptionInfo>>(), [phone]);
    let reply = with_token(&public.app, Method::GET, "/api/v1/push/key", &token).await;
    assert_eq!(reply.status, StatusCode::OK);

    // A valid subscription, sent with the token: refused before it is read.
    let body = serde_json::to_string(&service.subscription("desktop")).unwrap();
    let bearer = format!("Bearer {token}");
    let headers: Headers = vec![
        header("host", PUBLIC_HOST),
        header("authorization", bearer),
        header("content-type", "application/json"),
    ];
    let via = Via::Router(public.app.clone());
    let reply = send_via(
        &via,
        Method::POST,
        "/api/v1/push/subscriptions",
        &headers,
        Some(body),
    )
    .await;
    reply.assert_error(StatusCode::FORBIDDEN);
    assert!(reply.json::<ApiError>().error.contains("browser"));
    let listed: Vec<PushSubscriptionInfo> = alice.get("/api/v1/push/subscriptions").await.json();
    assert_eq!(listed.len(), 1);
}

#[tokio::test]
async fn the_test_route_answers_at_once_and_sends_in_the_background() {
    let public = public_mode(BOTH).await;
    let service = FakePushService::start().await;
    let mut alice = Browser::signed_in(&public.app, &public.provider).await;
    subscribe_device(&mut alice, &service, "phone").await;
    public.provider.set_user("user-2", Some(BOB), true);
    let mut bob = Browser::signed_in(&public.app, &public.provider).await;

    let reply = alice.post("/api/v1/push/test", json!({})).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED, "{}", reply.text());
    assert!(reply.body.is_empty());
    let received = service.wait_for(1, TIMEOUT).await;
    assert_eq!(received[0].device, "phone");
    assert_eq!(
        received[0].payload,
        PushPayload {
            kind: PushKind::Test,
            title: "Calin".to_owned(),
            body: "Notifications work on this device.".to_owned(),
            path: "/settings".to_owned(),
            session_id: None,
        }
    );
    // Bob has no device: accepted all the same, and nothing goes out.
    let reply = bob.post("/api/v1/push/test", json!({})).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    // The phone records when it last got something.
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let listed: Vec<PushSubscriptionInfo> =
                alice.get("/api/v1/push/subscriptions").await.json();
            if listed[0].last_sent_at.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the delivery was recorded");
    assert_eq!(service.received().len(), 1);
}

// --- Startup and shutdown ---

#[tokio::test]
async fn the_key_is_created_once_and_a_corrupt_key_file_stops_startup_naming_it() {
    let dir = TempDir::new().unwrap();
    let clock = Arc::new(calin_push::ManualClock::new(START * 1000));
    let first = open_push(dir.path(), &clock).await;
    drop(first);
    let key_file = dir.path().join(PUSH_KEY_FILE);
    let written = std::fs::read(&key_file).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // Opened again, the same key.
    open_push(dir.path(), &clock).await;
    assert_eq!(std::fs::read(&key_file).unwrap(), written);

    std::fs::write(&key_file, b"{ not a key").unwrap();
    let setup = PushSetup {
        subject: vapid_subject(PUBLIC_URL, None).unwrap(),
        endpoint_policy: calin_push::EndpointPolicy::PublicHttpsOnly,
    };
    let error = Push::open(dir.path(), setup, clock.clone())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains(&key_file.display().to_string()), "{error}");
    assert!(error.contains("corrupt"), "{error}");
    // Left as it was: a new key would orphan every subscription.
    assert_eq!(std::fs::read(&key_file).unwrap(), b"{ not a key");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutting_down_writes_the_last_delivery_times() {
    let public = public_mode(ALICE).await;
    let serving = serve_public(&public).await;
    let service = FakePushService::start().await;
    let mut alice = Browser::over(serving.addr);
    let reply = alice.sign_in(&public.provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    subscribe_device(&mut alice, &service, "phone").await;
    let reply = alice.post("/api/v1/push/test", json!({})).await;
    assert_eq!(reply.status, StatusCode::ACCEPTED);
    service.wait_for(1, TIMEOUT).await;
    tokio::time::timeout(TIMEOUT, async {
        loop {
            let listed: Vec<PushSubscriptionInfo> =
                alice.get("/api/v1/push/subscriptions").await.json();
            if listed[0].last_sent_at.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the delivery was recorded");
    let last_sent = || {
        let stored: Value = serde_json::from_slice(
            &std::fs::read(public.home.path().join(PUSH_SUBSCRIPTIONS_FILE)).unwrap(),
        )
        .unwrap();
        stored["identities"][ALICE]["subscriptions"][0]["last_sent_at"].clone()
    };
    // Too recent for the periodic write.
    assert_eq!(last_sent(), Value::Null);

    serving.stop.send(()).unwrap();
    serving.server.await.unwrap().unwrap();
    assert_eq!(last_sent(), json!(START * 1000));
}

// --- Notifications ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_ask_session_notifies_waiting_once_then_review() {
    let mut n = notified(ALICE, false).await;
    let repo = n.repo("api").await;
    let detail = n
        .create(json!({
            "prompt": "Fix the login bug", "title": "Fix the login bug",
            "permission_mode": "ask", "repos": [{ "repo_id": repo }],
        }))
        .await;
    let id = detail.session.id;

    // The first request: the test suite.
    assert_eq!(
        n.for_alice(&id, PushKind::Waiting).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(received[0].device, "alice-phone");
    assert_eq!(
        received[0].payload,
        payload(
            PushKind::Waiting,
            "Fix the login bug",
            "Wants to run: cargo test -q",
            &id
        )
    );
    assert_eq!(received[0].headers["urgency"], "high");

    // The second, the edit, within the minute: held back by the throttle.
    n.allow_the_open_request(&id).await;
    assert_eq!(
        n.for_alice(&id, PushKind::Waiting).await,
        Outcome::Throttled
    );
    n.past_throttle();
    n.allow_the_open_request(&id).await;
    assert_eq!(
        n.for_alice(&id, PushKind::Review).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(2, TIMEOUT).await;
    assert_eq!(
        received[1].payload,
        payload(
            PushKind::Review,
            "Fix the login bug",
            "Finished. 1 file changed in 1 repo.",
            &id
        )
    );
    assert_eq!(received.len(), 2);
    // Each status change once, in order.
    let kinds: Vec<(Option<PushKind>, Outcome)> =
        n.seen.iter().map(|d| (d.kind, d.outcome)).collect();
    assert_eq!(
        kinds,
        [
            (Some(PushKind::Waiting), Outcome::Sent { accepted: 1 }),
            (Some(PushKind::Waiting), Outcome::Throttled),
            (Some(PushKind::Review), Outcome::Sent { accepted: 1 }),
        ]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_session_notifies_failed_with_the_reason() {
    let mut n = notified(ALICE, true).await;
    let (id, _, io) = n.played("Migrate the database").await;
    io.outputs
        .send(RunnerOutput::Exited(Exit::Error(
            "the model is overloaded\nretry later".to_owned(),
        )))
        .await
        .unwrap();
    assert_eq!(
        n.for_alice(&id, PushKind::Failed).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(
        received[0].payload,
        payload(
            PushKind::Failed,
            "Migrate the database",
            "the model is overloaded",
            &id
        )
    );
    assert_eq!(received[0].headers["urgency"], "normal");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_question_notifies_with_its_first_line() {
    let mut n = notified(ALICE, true).await;
    let (id, endpoint, io) = n.played("Paint the shed").await;
    n.ask(&endpoint, "Which color?\nBlue is calmer, red is bolder.")
        .await;
    end_turn(&io).await;
    assert_eq!(
        n.for_alice(&id, PushKind::Waiting).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(
        received[0].payload,
        payload(PushKind::Waiting, "Paint the shed", "Which color?", &id)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workflow_gate_notifies_with_its_message() {
    let mut n = notified(ALICE, false).await;
    let home = git_repo(n.work.path(), "home");
    std::fs::create_dir_all(home.join(".calin/workflows")).unwrap();
    std::fs::write(home.join(".calin/workflows/gated.yaml"), GATED).unwrap();
    let reply = n
        .alice
        .request(
            Method::PUT,
            "/api/v1/settings",
            Some(json!({ "home_repo": home })),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let detail = n
        .create(json!({
            "prompt": "Add it", "title": "Add the button",
            "permission_mode": "autonomous", "workflow": "gated",
        }))
        .await;
    let id = detail.session.id;
    assert_eq!(
        n.for_alice(&id, PushKind::Waiting).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(
        received[0].payload,
        payload(
            PushKind::Waiting,
            "Add the button",
            "Good enough to ship?",
            &id
        )
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shipping_then_every_pull_request_merged_notifies_merged() {
    let mut n = notified(ALICE, false).await;
    let repo = n.repo("api").await;
    let detail = n
        .create(json!({
            "prompt": "Add the note", "title": "Add the note",
            "permission_mode": "autonomous", "repos": [{ "repo_id": repo }],
        }))
        .await;
    let id = detail.session.id;
    assert_eq!(
        n.for_alice(&id, PushKind::Review).await,
        Outcome::Sent { accepted: 1 }
    );
    n.past_throttle();
    ship_and_merge(&n, &id).await;
    assert_eq!(
        n.for_alice(&id, PushKind::Merged).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(2, TIMEOUT).await;
    assert_eq!(
        received[1].payload,
        payload(
            PushKind::Merged,
            "Add the note",
            "All pull requests merged.",
            &id
        )
    );
    assert_eq!(received.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_kind_can_be_switched_off() {
    let mut n = notified(ALICE, false).await;
    let reply = n
        .alice
        .request(
            Method::PUT,
            "/api/v1/push/preferences",
            Some(serde_json::to_value(ALL_OFF).unwrap()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let repo = n.repo("api").await;

    // Waiting, twice, then review, then merged.
    let id = n
        .create(json!({
            "prompt": "Fix it", "permission_mode": "ask", "repos": [{ "repo_id": repo }],
        }))
        .await
        .session
        .id;
    assert_eq!(n.for_alice(&id, PushKind::Waiting).await, Outcome::Disabled);
    n.allow_the_open_request(&id).await;
    // A notification switched off takes no throttle slot.
    assert_eq!(n.for_alice(&id, PushKind::Waiting).await, Outcome::Disabled);
    n.allow_the_open_request(&id).await;
    assert_eq!(n.for_alice(&id, PushKind::Review).await, Outcome::Disabled);
    ship_and_merge(&n, &id).await;
    assert_eq!(n.for_alice(&id, PushKind::Merged).await, Outcome::Disabled);

    // Failed: a session the store records as failed.
    let other = n
        .create(json!({ "prompt": "Other", "permission_mode": "autonomous" }))
        .await
        .session
        .id;
    assert_eq!(
        n.for_alice(&other, PushKind::Review).await,
        Outcome::Disabled
    );
    let failed = EventBody::SessionStatus {
        status: SessionStatus::Failed,
        reason: Some("the disk is full".to_owned()),
    };
    n.manager()
        .store()
        .append(&other, EventDraft::new(failed))
        .await
        .unwrap();
    assert_eq!(
        n.for_alice(&other, PushKind::Failed).await,
        Outcome::Disabled
    );
    assert!(n.received().is_empty());

    // Switched back on, the same change notifies.
    let reply = n
        .alice
        .request(
            Method::PUT,
            "/api/v1/push/preferences",
            Some(serde_json::to_value(PushPreferences::default()).unwrap()),
        )
        .await;
    assert_eq!(reply.status, StatusCode::OK);
    let review = EventBody::SessionStatus {
        status: SessionStatus::Review,
        reason: None,
    };
    let store = n.manager().store().clone();
    store.append(&other, EventDraft::new(review)).await.unwrap();
    let failed = EventBody::SessionStatus {
        status: SessionStatus::Failed,
        reason: Some("the disk is full".to_owned()),
    };
    store.append(&other, EventDraft::new(failed)).await.unwrap();
    // The review went out; the failure right after it is within the minute.
    assert_eq!(
        n.for_alice(&other, PushKind::Review).await,
        Outcome::Sent { accepted: 1 }
    );
    assert_eq!(
        n.for_alice(&other, PushKind::Failed).await,
        Outcome::Throttled
    );
    assert_eq!(n.service.wait_for(1, TIMEOUT).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_child_notifies_when_it_waits_under_its_roots_title_and_never_for_its_review() {
    let mut n = notified(ALICE, true).await;
    let (root, lead_endpoint, _lead) = n.played("Ship the docs site").await;
    let brief = json!({ "title": "Write the docs", "brief": "Write them.\nThen report." });
    let (text, is_error) = call_tool(&n.public.app, &lead_endpoint, "dispatch", brief).await;
    assert!(!is_error, "{text}");
    let (params, child_io) = n.next_agent().await;
    let child = params.session_id.clone();
    let child_endpoint = endpoint_of(&params);
    n.status_reaches(&child, SessionStatus::Running).await;

    n.ask(&child_endpoint, "Which tone?").await;
    end_turn(&child_io).await;
    assert_eq!(
        n.for_alice(&child, PushKind::Waiting).await,
        Outcome::Sent { accepted: 1 }
    );
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(
        received[0].payload,
        payload(
            PushKind::Waiting,
            "Ship the docs site",
            "In \"Write the docs\": Which tone?",
            &child
        )
    );

    // Past the throttle, so that only the rule keeps the review quiet.
    n.past_throttle();
    let question = n.the_open_question(&child).await;
    n.manager()
        .answer_question(&child, &question, "Friendly")
        .await
        .unwrap();
    n.status_reaches(&child, SessionStatus::Running).await;
    end_turn(&child_io).await;
    let decision = n
        .decision(|d| d.session_id == child && d.status == SessionStatus::Review)
        .await;
    assert_eq!(decision.kind, None);
    assert_eq!(decision.outcome, Outcome::Ignored);
    assert_eq!(n.received().len(), 1);
    // The root carries on and was never notified.
    assert!(n.seen.iter().all(|d| d.session_id != root));
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_identity_with_the_session_open_on_a_socket_is_skipped() {
    let mut n = notified(BOTH, true).await;
    n.public.provider.set_user("user-2", Some(BOB), true);
    let mut bob = Browser::signed_in(&n.public.app, &n.public.provider).await;
    subscribe_device(&mut bob, &n.service, "bob-phone").await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(n.state().clone()).into_make_service_with_connect_info::<SocketAddr>();
    tokio::spawn(axum::serve(listener, app).into_future());

    let (id, endpoint, io) = n.played("Tidy the logs").await;
    let cookie = n.alice.session_cookie();
    let mut socket = handshake(addr, "/api/v1/ws", &[("cookie", cookie.as_str())])
        .await
        .unwrap();
    let topic = session_topic(&id);
    send(&mut socket, &subscribe(&topic, None)).await;
    loop {
        if let ServerMsg::Subscribed { .. } = next(&mut socket).await {
            break;
        }
    }
    assert!(n.state().viewers.is_viewing(ALICE, &id));

    n.ask(&endpoint, "Keep the old ones?").await;
    end_turn(&io).await;
    let alice = n
        .decision(|d| d.session_id == id && d.identity.as_deref() == Some(ALICE))
        .await;
    assert_eq!(alice.outcome, Outcome::Viewing);
    let bob_decision = n
        .decision(|d| d.session_id == id && d.identity.as_deref() == Some(BOB))
        .await;
    assert_eq!(bob_decision.outcome, Outcome::Sent { accepted: 1 });
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].device, "bob-phone");

    // Once the socket is gone, Alice is notified again.
    socket.close(None).await.unwrap();
    tokio::time::timeout(TIMEOUT, async {
        while n.state().viewers.is_viewing(ALICE, &id) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the socket's subscription ended");
    n.past_throttle();
    let question = n.the_open_question(&id).await;
    n.manager()
        .answer_question(&id, &question, "No")
        .await
        .unwrap();
    n.status_reaches(&id, SessionStatus::Running).await;
    n.ask(&endpoint, "And the archives?").await;
    end_turn(&io).await;
    assert_eq!(
        n.for_alice(&id, PushKind::Waiting).await,
        Outcome::Sent { accepted: 1 }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_is_sent_for_what_startup_recovery_settles() {
    let home = TempDir::new().unwrap();
    // A first run leaves a session waiting on a permission request.
    let first = super::state(&home, true).await;
    let detail = first
        .manager()
        .create_session(
            serde_json::from_value(json!({ "prompt": "Cut short", "permission_mode": "ask" }))
                .unwrap(),
        )
        .await
        .unwrap();
    let cut = detail.session.id;
    events_until(&first, &cut, is_waiting).await;
    first.manager().shutdown().await;
    first.manager().store().close().await;
    drop(first);

    // The next start fails it as cut short by the restart.
    let state = super::state(&home, true).await;
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    let public = public_on(home, state, clock, provider, ALICE).await;
    let mut n = observed(public, None).await;
    let recovered = n.manager().get(&cut).await.unwrap();
    assert_eq!(recovered.session.status, SessionStatus::Failed);

    // A session of this run is notified; the recovered one never was.
    let id = n
        .create(json!({ "prompt": "Fresh", "permission_mode": "ask" }))
        .await
        .session
        .id;
    assert_eq!(
        n.for_alice(&id, PushKind::Waiting).await,
        Outcome::Sent { accepted: 1 }
    );
    assert!(n.seen.iter().all(|d| d.session_id == id), "{:?}", n.seen);
    let received = n.service.wait_for(1, TIMEOUT).await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0].payload.session_id.as_deref(), Some(id.as_str()));
}
