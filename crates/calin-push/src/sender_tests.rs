//! Against a fake push service on a loopback port, which checks what a real
//! one would and decrypts every message with the subscription's keys.

use std::sync::atomic::{AtomicUsize, Ordering};

use calin_proto::PushSubscriptionRequest;
use p256::ecdsa::signature::Verifier as _;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::clock::ManualClock;
use crate::ece::tests::{decrypt, subscriber};
use crate::secret::base64url_decode;

const DEV: EndpointPolicy = EndpointPolicy::AllowLoopbackHttpForDevelopment;
const START: u64 = 1_800_000_000_000;
const SUBJECT: &str = "mailto:ops@example.com";

/// One request as the fake service saw it.
#[derive(Debug, Clone)]
struct Received {
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

#[derive(Default)]
struct FakeState {
    received: Mutex<Vec<Received>>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

struct FakeService {
    origin: String,
    state: Arc<FakeState>,
}

impl FakeService {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(FakeState::default());
        let shared = state.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(handle(stream, shared.clone()));
            }
        });
        Self { origin, state }
    }

    fn received(&self) -> Vec<Received> {
        self.state.received.lock().unwrap().clone()
    }
}

/// Answers according to the path: `/s/<status>`, `/slow`, `/redirect`,
/// `/big/<status>`, `/wait/<millis>` (then 201).
async fn handle(mut stream: TcpStream, state: Arc<FakeState>) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let now = state.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    state.max_in_flight.fetch_max(now, Ordering::SeqCst);
    state.received.lock().unwrap().push(request.clone());

    let segments: Vec<&str> = request.path.trim_start_matches('/').split('/').collect();
    let response: Vec<u8> = match segments.as_slice() {
        ["s", status, ..] => format!(
            "HTTP/1.1 {status} X\r\nContent-Length: 14\r\nConnection: close\r\n\r\nservice said\n\u{1b}"
        )
        .into_bytes(),
        ["slow", ..] => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
        ["redirect", ..] => 
            b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /s/201/after-redirect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        .to_vec(),
        ["big", status, ..] => {
            let head = format!(
                "HTTP/1.1 {status} X\r\nContent-Length: 104857600\r\nConnection: close\r\n\r\n"
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let chunk = vec![b'z'; 65536];
            // Until the client stops reading and closes.
            for _ in 0..1600 {
                if stream.write_all(&chunk).await.is_err() {
                    break;
                }
            }
            Vec::new()
        }
        ["wait", millis, ..] => {
            tokio::time::sleep(Duration::from_millis(millis.parse().unwrap())).await;
            b"HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec()
        }
        _ => b"HTTP/1.1 400 Bad\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    };
    let _ = stream.write_all(&response).await;
    let _ = stream.shutdown().await;
    state.in_flight.fetch_sub(1, Ordering::SeqCst);
}

async fn read_request(stream: &mut TcpStream) -> Option<Received> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break at;
        }
    };
    let head = String::from_utf8(buffer[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split(' ');
    assert_eq!(parts.next(), Some("POST"), "{request_line}");
    let path = parts.next()?.to_owned();
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers.get("content-length")?.parse().ok()?;
    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Received {
        path,
        headers,
        body,
    })
}

struct Fixture {
    _dir: tempfile::TempDir,
    clock: Arc<ManualClock>,
    keys: Arc<VapidKeys>,
    store: SubscriptionStore,
    sender: PushSender,
    service: FakeService,
    /// The private key and auth secret of each subscription, by id.
    devices: HashMap<String, (p256::SecretKey, [u8; 16])>,
}

async fn fixture() -> Fixture {
    fixture_with(Duration::from_secs(5), 4).await
}

async fn fixture_with(timeout: Duration, concurrency: usize) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::new(START));
    let keys = Arc::new(
        VapidKeys::load_or_generate(dir.path().join("vapid.json"))
            .await
            .unwrap(),
    );
    let store = SubscriptionStore::open(dir.path().join("push.json"), clock.clone())
        .await
        .unwrap();
    let sender = PushSender::new(
        keys.clone(),
        store.clone(),
        clock.clone(),
        PushConfig {
            endpoint_policy: DEV,
            request_timeout: timeout,
            concurrency,
            ..PushConfig::new(SUBJECT)
        },
    )
    .unwrap();
    Fixture {
        _dir: dir,
        clock,
        keys,
        store,
        sender,
        service: FakeService::start().await,
        devices: HashMap::new(),
    }
}

impl Fixture {
    /// Subscribes a new device of `identity` whose endpoint is `path` on
    /// the fake service. Returns its id.
    async fn device(&mut self, identity: &str, path: &str) -> String {
        let (secret, keys) = subscriber();
        let request = PushSubscriptionRequest {
            endpoint: format!("{}{path}", self.service.origin),
            p256dh: base64url(&keys.p256dh),
            auth: base64url(&keys.auth),
            label: None,
        };
        let info = self
            .store
            .subscribe(identity, &request, None, DEV)
            .await
            .unwrap();
        self.devices.insert(info.id.clone(), (secret, keys.auth));
        info.id
    }

    async fn send(&self, identity: &str, kind: PushKind, session: Option<&str>) -> SendReport {
        self.sender
            .send(identity, kind, payload(kind, session))
            .await
            .unwrap()
    }
}

fn payload(kind: PushKind, session: Option<&str>) -> PushPayload {
    PushPayload {
        kind,
        title: "Fix the login bug".to_owned(),
        body: "Claude asks to run a command".to_owned(),
        path: session.map_or("/".to_owned(), |s| format!("/s/{s}")),
        session_id: session.map(str::to_owned),
    }
}

/// The outcome of a report with exactly one delivery.
fn single(report: &SendReport) -> DeliveryOutcome {
    let all = outcomes(report);
    assert_eq!(all.len(), 1, "{report:?}");
    all.into_iter().next().unwrap()
}

fn outcomes(report: &SendReport) -> Vec<DeliveryOutcome> {
    let SendReport::Sent(deliveries) = report else {
        panic!("{report:?}");
    };
    deliveries.iter().map(|d| d.outcome.clone()).collect()
}

#[tokio::test]
async fn a_message_round_trips_through_the_fake_service() {
    let mut f = fixture().await;
    let id = f.device("alice", "/s/201/device-a").await;
    let report = f.send("alice", PushKind::Waiting, Some("sess-1")).await;
    let SendReport::Sent(deliveries) = &report else {
        panic!("{report:?}");
    };
    assert_eq!(
        deliveries,
        &vec![Delivery {
            subscription_id: id.clone(),
            service: "127.0.0.1".to_owned(),
            outcome: DeliveryOutcome::Accepted,
        }]
    );
    let received = f.service.received();
    assert_eq!(received.len(), 1);
    let request = &received[0];
    assert_eq!(request.path, "/s/201/device-a");
    assert_eq!(request.headers["content-encoding"], "aes128gcm");
    assert_eq!(request.headers["ttl"], "86400");
    assert_eq!(request.headers["urgency"], "high");
    let topic = &request.headers["topic"];
    assert_eq!(topic.len(), 32);
    assert!(
        topic
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    assert!(!topic.contains("sess-1"));

    // The aes128gcm header: salt, record size 4096, the sender's key id.
    let body = &request.body;
    assert!(body.len() <= 4096);
    assert_eq!(u32::from_be_bytes(body[16..20].try_into().unwrap()), 4096);
    assert_eq!(body[20], 65);
    assert_eq!(body[21], 0x04, "an uncompressed point");
    assert_ne!(
        base64url(&body[21..86]),
        f.keys.public_key_base64url(),
        "the ephemeral key, not the VAPID key"
    );

    // VAPID: the token verifies with the advertised public key.
    let authorization = &request.headers["authorization"];
    let rest = authorization.strip_prefix("vapid t=").unwrap();
    let (token, k) = rest.split_once(", k=").unwrap();
    assert_eq!(k, f.keys.push_key().public_key);
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3);
    let verifying =
        p256::ecdsa::VerifyingKey::from_sec1_bytes(&base64url_decode(k).unwrap()).unwrap();
    let signature =
        p256::ecdsa::Signature::from_slice(&base64url_decode(parts[2]).unwrap()).unwrap();
    verifying
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .unwrap();
    let header: serde_json::Value =
        serde_json::from_slice(&base64url_decode(parts[0]).unwrap()).unwrap();
    assert_eq!(header["alg"], "ES256");
    let claims: serde_json::Value =
        serde_json::from_slice(&base64url_decode(parts[1]).unwrap()).unwrap();
    assert_eq!(claims["aud"], f.service.origin.as_str());
    assert_eq!(claims["sub"], SUBJECT);
    let exp = claims["exp"].as_u64().unwrap();
    assert_eq!(exp, START / 1000 + 12 * 3600);
    assert!(exp - START / 1000 <= 24 * 3600);

    // The subscriber decrypts it to the original JSON.
    let (secret, auth) = &f.devices[&id];
    let plaintext = decrypt(body, secret, auth);
    let decoded: PushPayload = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(decoded, payload(PushKind::Waiting, Some("sess-1")));

    // And the success is recorded.
    assert_eq!(f.store.list("alice")[0].last_sent_at, Some(START as i64));
}

#[tokio::test]
async fn status_handling() {
    let mut f = fixture().await;
    let cases = [
        ("/s/201/a", DeliveryOutcome::Accepted),
        ("/s/202/a", DeliveryOutcome::Accepted),
        ("/s/404/a", DeliveryOutcome::Gone { removed: true }),
        ("/s/410/a", DeliveryOutcome::Gone { removed: true }),
    ];
    for (path, expected) in cases {
        let mut f2 = fixture().await;
        f2.device("alice", path).await;
        let report = f2.send("alice", PushKind::Review, Some("s")).await;
        assert_eq!(outcomes(&report), vec![expected.clone()], "{path}");
        let still_listed = !f2.store.list("alice").is_empty();
        assert_eq!(
            still_listed,
            expected == DeliveryOutcome::Accepted,
            "{path}"
        );
    }

    for (path, check) in [
        (
            "/s/413/a",
            (|failure: &Failure| {
                matches!(failure, Failure::PayloadTooLarge { .. }) && !failure.is_retryable()
            }) as fn(&Failure) -> bool,
        ),
        ("/s/429/a", |failure| {
            matches!(failure, Failure::RateLimited { .. }) && failure.is_retryable()
        }),
        ("/s/500/a", |failure| {
            matches!(failure, Failure::ServiceError { status: 500, .. }) && failure.is_retryable()
        }),
        ("/s/503/a", |failure| {
            matches!(failure, Failure::ServiceError { status: 503, .. }) && failure.is_retryable()
        }),
        ("/s/403/a", |failure| {
            matches!(failure, Failure::Refused { status: 403, .. }) && !failure.is_retryable()
        }),
        ("/s/200/a", |failure| {
            matches!(failure, Failure::Refused { status: 200, .. })
        }),
    ] {
        let id = f.device(path, path).await;
        let report = f.send(path, PushKind::Review, Some("s")).await;
        let DeliveryOutcome::Failed(failure) = single(&report) else {
            panic!("{path}: {report:?}");
        };
        assert!(check(&failure), "{path}: {failure:?}");
        // The excerpt is one safe line.
        let shown = failure.to_string();
        assert!(
            shown.chars().all(|c| c.is_ascii_graphic() || c == ' '),
            "{shown:?}"
        );
        assert!(!shown.contains("127.0.0.1"), "{shown}");
        // Still subscribed, nothing recorded.
        assert_eq!(f.store.list(path)[0].id, id);
        assert_eq!(f.store.list(path)[0].last_sent_at, None);
    }
    let shown = match &outcomes(&f.send("/s/500/a", PushKind::Test, None).await)[0] {
        DeliveryOutcome::Failed(failure) => failure.to_string(),
        other => panic!("{other:?}"),
    };
    assert_eq!(shown, "the push service failed with 500: service said??");
}

#[tokio::test]
async fn a_slow_service_times_out() {
    let mut f = fixture_with(Duration::from_millis(300), 4).await;
    f.device("alice", "/slow/a").await;
    let started = std::time::Instant::now();
    let report = f.send("alice", PushKind::Failed, Some("s")).await;
    assert!(started.elapsed() < Duration::from_secs(3));
    let DeliveryOutcome::Failed(failure) = single(&report) else {
        panic!("{report:?}");
    };
    assert_eq!(failure, Failure::Timeout(Duration::from_millis(300)));
    assert!(failure.is_retryable());
}

#[tokio::test]
async fn a_service_that_refuses_the_connection_is_a_retryable_transport_failure() {
    let f = fixture().await;
    // A port nothing listens on any more.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = closed.local_addr().unwrap().port();
    drop(closed);
    let (_, keys) = subscriber();
    let request = PushSubscriptionRequest {
        endpoint: format!("http://127.0.0.1:{port}/gone"),
        p256dh: base64url(&keys.p256dh),
        auth: base64url(&keys.auth),
        label: None,
    };
    f.store
        .subscribe("alice", &request, None, DEV)
        .await
        .unwrap();
    let report = f.send("alice", PushKind::Failed, Some("s")).await;
    let DeliveryOutcome::Failed(failure) = single(&report) else {
        panic!("{report:?}");
    };
    assert!(matches!(failure, Failure::Transport(_)), "{failure:?}");
    assert!(failure.is_retryable());
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let mut f = fixture().await;
    f.device("alice", "/redirect/a").await;
    let report = f.send("alice", PushKind::Merged, Some("s")).await;
    assert_eq!(
        outcomes(&report),
        vec![DeliveryOutcome::Failed(Failure::Redirect { status: 307 })]
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let paths: Vec<String> = f.service.received().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, vec!["/redirect/a".to_owned()]);
}

#[tokio::test]
async fn oversized_responses_are_cut_short() {
    let mut f = fixture_with(Duration::from_secs(5), 4).await;
    f.device("ok", "/big/201/a").await;
    f.device("bad", "/big/500/a").await;
    let started = std::time::Instant::now();
    assert_eq!(
        outcomes(&f.send("ok", PushKind::Review, Some("s")).await),
        vec![DeliveryOutcome::Accepted]
    );
    let report = f.send("bad", PushKind::Review, Some("s")).await;
    let DeliveryOutcome::Failed(Failure::ServiceError {
        status: 500,
        excerpt,
    }) = single(&report)
    else {
        panic!("{report:?}");
    };
    assert!(excerpt.len() <= 203, "{}", excerpt.len());
    assert!(excerpt.starts_with("zzzz"));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "did not read 100 MB"
    );
}

#[tokio::test]
async fn preferences_filter_by_kind_and_test_always_goes() {
    let mut f = fixture().await;
    f.device("alice", "/s/201/a").await;
    f.store
        .set_preferences(
            "alice",
            PushPreferences {
                waiting: false,
                review: true,
                failed: false,
                merged: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.send("alice", PushKind::Waiting, Some("1")).await,
        SendReport::Disabled
    );
    assert_eq!(
        f.send("alice", PushKind::Failed, Some("2")).await,
        SendReport::Disabled
    );
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Review, Some("3")).await).len(),
        1
    );
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Merged, Some("4")).await).len(),
        1
    );
    f.store
        .set_preferences(
            "alice",
            PushPreferences {
                waiting: false,
                review: false,
                failed: false,
                merged: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Test, None).await).len(),
        1
    );
    assert_eq!(f.service.received().len(), 3);
    // Review and merged use normal urgency; a test has no topic.
    let received = f.service.received();
    assert!(received.iter().all(|r| r.headers["urgency"] == "normal"));
    assert!(!received[2].headers.contains_key("topic"));
    let decoded: PushPayload = serde_json::from_slice(&decrypt(
        &received[2].body,
        &f.devices.values().next().unwrap().0,
        &f.devices.values().next().unwrap().1,
    ))
    .unwrap();
    assert_eq!(decoded.kind, PushKind::Test);
}

#[tokio::test]
async fn no_subscription_sends_nothing() {
    let f = fixture().await;
    assert_eq!(
        f.send("nobody", PushKind::Waiting, Some("s")).await,
        SendReport::Sent(vec![])
    );
    // And does not use up the throttle.
    let mut f = f;
    f.device("nobody", "/s/201/a").await;
    assert_eq!(
        outcomes(&f.send("nobody", PushKind::Waiting, Some("s")).await).len(),
        1
    );
}

#[tokio::test]
async fn one_notification_per_session_per_minute() {
    let mut f = fixture().await;
    f.device("alice", "/s/201/a").await;
    f.device("bob", "/s/201/b").await;
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Waiting, Some("s1")).await).len(),
        1
    );
    assert_eq!(
        f.send("alice", PushKind::Review, Some("s1")).await,
        SendReport::Throttled
    );
    // Another session, another identity: not throttled.
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Waiting, Some("s2")).await).len(),
        1
    );
    assert_eq!(
        outcomes(&f.send("bob", PushKind::Waiting, Some("s1")).await).len(),
        1
    );
    // A test is not held back by sessions.
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Test, None).await).len(),
        1
    );

    f.clock.advance(Duration::from_secs(59));
    assert_eq!(
        f.send("alice", PushKind::Failed, Some("s1")).await,
        SendReport::Throttled
    );
    f.clock.advance(Duration::from_secs(1));
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Failed, Some("s1")).await).len(),
        1
    );

    // Disabled kinds do not use up the slot.
    f.clock.advance(Duration::from_secs(60));
    f.store
        .set_preferences(
            "alice",
            PushPreferences {
                waiting: false,
                ..PushPreferences::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.send("alice", PushKind::Waiting, Some("s1")).await,
        SendReport::Disabled
    );
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Review, Some("s1")).await).len(),
        1
    );
}

#[tokio::test]
async fn one_test_per_identity_per_ten_seconds() {
    let mut f = fixture().await;
    f.device("alice", "/s/201/a").await;
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Test, None).await).len(),
        1
    );
    f.clock.advance(Duration::from_secs(9));
    assert_eq!(
        f.send("alice", PushKind::Test, None).await,
        SendReport::Throttled
    );
    f.clock.advance(Duration::from_secs(1));
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Test, None).await).len(),
        1
    );
}

#[tokio::test]
async fn a_clock_that_steps_back_does_not_hold_notifications() {
    let mut f = fixture().await;
    f.device("alice", "/s/201/a").await;
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Waiting, Some("s")).await).len(),
        1
    );
    f.clock.set(START - 3_600_000);
    assert_eq!(
        outcomes(&f.send("alice", PushKind::Waiting, Some("s")).await).len(),
        1
    );
}

#[tokio::test]
async fn sends_run_concurrently_within_the_limit() {
    let mut f = fixture_with(Duration::from_secs(5), 3).await;
    for index in 0..7 {
        f.device("alice", &format!("/wait/300/{index}")).await;
    }
    f.device("alice", "/s/410/gone").await;
    let started = std::time::Instant::now();
    let report = f.send("alice", PushKind::Waiting, Some("s")).await;
    let elapsed = started.elapsed();
    let outcomes = outcomes(&report);
    assert_eq!(outcomes.len(), 8);
    assert_eq!(
        outcomes
            .iter()
            .filter(|o| **o == DeliveryOutcome::Accepted)
            .count(),
        7
    );
    // In the store's order, whatever order they finished in.
    let SendReport::Sent(deliveries) = &report else {
        unreachable!()
    };
    let listed: Vec<String> = f.store.list("alice").into_iter().map(|i| i.id).collect();
    let reported: Vec<String> = deliveries
        .iter()
        .map(|d| d.subscription_id.clone())
        .collect();
    assert_eq!(&reported[..7], &listed[..]);
    let max = f.service.state.max_in_flight.load(Ordering::SeqCst);
    assert!(max > 1, "concurrent: {max}");
    assert!(max <= 3, "limited: {max}");
    // 7 × 300 ms one by one would take 2.1 s.
    assert!(elapsed < Duration::from_millis(1800), "{elapsed:?}");
}

#[tokio::test]
async fn concurrent_sends_for_one_session_claim_the_slot_once() {
    let mut f = fixture().await;
    f.device("alice", "/s/201/a").await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let sender = f.sender.clone();
        tasks.push(tokio::spawn(async move {
            sender
                .send(
                    "alice",
                    PushKind::Waiting,
                    payload(PushKind::Waiting, Some("s")),
                )
                .await
                .unwrap()
        }));
    }
    let mut sent = 0;
    for task in tasks {
        if matches!(task.await.unwrap(), SendReport::Sent(_)) {
            sent += 1;
        }
    }
    assert_eq!(sent, 1);
    assert_eq!(f.service.received().len(), 1);
}

#[tokio::test]
async fn long_text_is_cut_and_huge_payloads_refused_before_any_request() {
    let mut f = fixture().await;
    let id = f.device("alice", "/s/201/a").await;
    let long = PushPayload {
        title: format!("{}\u{7}", "t".repeat(1000)),
        body: "é".repeat(2000),
        ..payload(PushKind::Waiting, Some("s"))
    };
    let report = f
        .sender
        .send("alice", PushKind::Review, long)
        .await
        .unwrap();
    assert_eq!(outcomes(&report), vec![DeliveryOutcome::Accepted]);
    let received = f.service.received();
    let (secret, auth) = &f.devices[&id];
    let decoded: PushPayload =
        serde_json::from_slice(&decrypt(&received[0].body, secret, auth)).unwrap();
    assert_eq!(decoded.title, "t".repeat(MAX_TITLE_CHARS));
    assert_eq!(decoded.body, "é".repeat(MAX_BODY_CHARS));
    assert_eq!(decoded.kind, PushKind::Review, "the kind argument wins");

    let huge = PushPayload {
        path: format!("/s/{}", "p".repeat(4000)),
        ..payload(PushKind::Waiting, Some("other"))
    };
    let error = f
        .sender
        .send("alice", PushKind::Waiting, huge)
        .await
        .unwrap_err();
    assert!(
        matches!(error, SendError::PayloadTooLarge { .. }),
        "{error}"
    );
    assert_eq!(
        f.service.received().len(),
        1,
        "no request for the refused one"
    );

    // The largest text allowed always fits, even with characters that
    // JSON escapes.
    let worst = PushPayload {
        title: "\u{10FFFF}".repeat(MAX_TITLE_CHARS),
        body: "\"".repeat(MAX_BODY_CHARS),
        path: format!("/s/{}", "x".repeat(200)),
        session_id: Some("x".repeat(200)),
        kind: PushKind::Waiting,
    };
    f.clock.advance(Duration::from_secs(120));
    assert!(
        f.sender
            .send("alice", PushKind::Waiting, worst)
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn a_strict_sender_refuses_a_loopback_endpoint_from_the_store() {
    let mut f = fixture().await;
    f.device("alice", "/s/201/a").await;
    let strict = PushSender::new(
        f.keys.clone(),
        f.store.clone(),
        f.clock.clone(),
        PushConfig::new(SUBJECT),
    )
    .unwrap();
    let report = strict
        .send(
            "alice",
            PushKind::Waiting,
            payload(PushKind::Waiting, Some("s")),
        )
        .await
        .unwrap();
    assert_eq!(
        outcomes(&report),
        vec![DeliveryOutcome::Failed(Failure::Endpoint(
            EndpointError::NotHttps
        ))]
    );
    assert!(f.service.received().is_empty());
}

#[tokio::test]
async fn the_subject_must_be_a_contact_uri() {
    let f = fixture().await;
    for (subject, ok) in [
        ("mailto:ops@example.com", true),
        ("https://example.com/contact", true),
        ("mailto:", false),
        ("mailto:not an address", false),
        ("ops@example.com", false),
        ("http://example.com", false),
        ("https://", false),
        ("", false),
    ] {
        let result = PushSender::new(
            f.keys.clone(),
            f.store.clone(),
            f.clock.clone(),
            PushConfig::new(subject),
        );
        assert_eq!(result.is_ok(), ok, "{subject}");
    }
}

#[tokio::test]
async fn nothing_secret_in_debug_or_display() {
    let mut f = fixture().await;
    let id = f.device("alice", "/s/500/secret-capability").await;
    let report = f.send("alice", PushKind::Waiting, Some("s")).await;
    let received = f.service.received();
    let token = received[0].headers["authorization"].clone();
    let jwt = token
        .strip_prefix("vapid t=")
        .unwrap()
        .split(", k=")
        .next()
        .unwrap();
    let (_, targets) = f.store.targets("alice");
    let auth = base64url(&f.devices[&id].1);
    let p256dh = base64url(&targets[0].keys.p256dh);
    let SendReport::Sent(deliveries) = &report else {
        unreachable!()
    };
    let DeliveryOutcome::Failed(failure) = &deliveries[0].outcome else {
        unreachable!()
    };
    for shown in [
        format!("{:?}", f.sender),
        format!("{report:?}"),
        format!("{failure}"),
        format!("{:?}", targets),
        format!("{:?}", targets[0].keys),
        format!("{:?}", f.store),
        format!("{:?}", f.keys),
    ] {
        for secret in ["secret-capability", jwt, auth.as_str(), p256dh.as_str()] {
            assert!(!shown.contains(secret), "{secret} in {shown}");
        }
    }
}
