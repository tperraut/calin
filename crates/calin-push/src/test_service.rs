//! A fake push service for the tests of other crates (the `test-service`
//! feature; never part of the `calin` binary). It listens on a loopback
//! port, accepts every message with 201, and decrypts each one with the keys
//! of the device it was sent to, as a browser would (RFC 8291 over RFC 8188).
//!
//! Its endpoints are plain `http://127.0.0.1:<port>`, so the sender and the
//! store under test must use
//! [`EndpointPolicy::AllowLoopbackHttpForDevelopment`](crate::EndpointPolicy).
//!
//! Test code: it panics instead of returning errors.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use calin_proto::{PushPayload, PushSubscriptionRequest};
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hmac;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use crate::secret::{base64url, random_bytes};

/// Paths under this prefix answer 410 Gone: a subscription the browser dropped.
const GONE_PREFIX: &str = "/gone/";

/// One message as a device received it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceivedPush {
    /// The name the device was created with.
    pub device: String,
    /// The decrypted payload.
    pub payload: PushPayload,
    /// The request's headers, names in lowercase.
    pub headers: HashMap<String, String>,
}

struct Device {
    name: String,
    secret: p256::SecretKey,
    auth: [u8; 16],
}

#[derive(Default)]
struct State {
    /// By endpoint path.
    devices: Mutex<HashMap<String, Arc<Device>>>,
    received: Mutex<Vec<ReceivedPush>>,
    arrived: Notify,
}

/// The fake service. Dropping it does not stop the listener; the test's
/// runtime does.
pub struct FakePushService {
    origin: String,
    state: Arc<State>,
}

impl FakePushService {
    /// Starts the service on a free loopback port. Needs a tokio runtime.
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind a loopback port");
        let origin = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("a bound listener has an address")
        );
        let state = Arc::new(State::default());
        let shared = state.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(handle(stream, shared.clone()));
            }
        });
        Self { origin, state }
    }

    /// `http://127.0.0.1:<port>`.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// A new device named `name`: the subscription its browser would hand
    /// the server. Messages to it are accepted and decrypted.
    pub fn subscription(&self, name: &str) -> PushSubscriptionRequest {
        self.device(name, &format!("/push/{name}"))
    }

    /// A device whose push service answers 410 Gone to every message.
    pub fn gone_subscription(&self, name: &str) -> PushSubscriptionRequest {
        self.device(name, &format!("{GONE_PREFIX}{name}"))
    }

    fn device(&self, name: &str, path: &str) -> PushSubscriptionRequest {
        let secret = loop {
            let bytes = random_bytes::<32>().expect("random bytes");
            if let Ok(secret) = p256::SecretKey::from_slice(&bytes) {
                break secret;
            }
        };
        let auth = random_bytes::<16>().expect("random bytes");
        let request = PushSubscriptionRequest {
            endpoint: format!("{}{path}", self.origin),
            p256dh: base64url(&secret.public_key().to_sec1_bytes()),
            auth: base64url(&auth),
            label: Some(name.to_owned()),
        };
        let device = Device {
            name: name.to_owned(),
            secret,
            auth,
        };
        self.state
            .devices
            .lock()
            .expect("not poisoned")
            .insert(path.to_owned(), Arc::new(device));
        request
    }

    /// Every message accepted so far, in the order they arrived.
    pub fn received(&self) -> Vec<ReceivedPush> {
        self.state.received.lock().expect("not poisoned").clone()
    }

    /// Waits until at least `count` messages arrived; panics after `timeout`.
    pub async fn wait_for(&self, count: usize, timeout: Duration) -> Vec<ReceivedPush> {
        let waited = tokio::time::timeout(timeout, async {
            loop {
                let arrived = self.state.arrived.notified();
                let received = self.received();
                if received.len() >= count {
                    return received;
                }
                arrived.await;
            }
        })
        .await;
        match waited {
            Ok(received) => received,
            Err(_) => panic!("expected {count} push messages, got {:?}", self.received()),
        }
    }
}

async fn handle(mut stream: TcpStream, state: Arc<State>) {
    let Some((path, headers, body)) = read_request(&mut stream).await else {
        return;
    };
    let device = state
        .devices
        .lock()
        .expect("not poisoned")
        .get(&path)
        .cloned();
    let status = match device {
        Some(_) if path.starts_with(GONE_PREFIX) => "410 Gone",
        Some(device) => {
            let plaintext = decrypt(&body, &device.secret, &device.auth);
            let payload: PushPayload =
                serde_json::from_slice(&plaintext).expect("the payload is a PushPayload");
            state
                .received
                .lock()
                .expect("not poisoned")
                .push(ReceivedPush {
                    device: device.name.clone(),
                    payload,
                    headers,
                });
            state.arrived.notify_waiters();
            "201 Created"
        }
        None => "404 Not Found",
    };
    let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

type Request = (String, HashMap<String, String>, Vec<u8>);

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
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
    let mut request_line = lines.next()?.split(' ');
    assert_eq!(request_line.next(), Some("POST"), "push services take POST");
    let path = request_line.next()?.to_owned();
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
    Some((path, headers, body))
}

/// The subscriber's side, written from RFC 8291 section 3.4 and RFC 8188
/// with HMAC directly, independently of the sender's HKDF.
fn decrypt(body: &[u8], ua_private: &p256::SecretKey, auth: &[u8]) -> Vec<u8> {
    let hmac_sha256 = |key: &[u8], data: &[&[u8]]| -> Vec<u8> {
        let key = hmac::Key::new(hmac::HMAC_SHA256, key);
        let mut context = hmac::Context::with_key(&key);
        for part in data {
            context.update(part);
        }
        context.sign().as_ref().to_vec()
    };
    let salt = &body[..16];
    let idlen = usize::from(body[20]);
    let as_public = &body[21..21 + idlen];
    let record = &body[21 + idlen..];

    let ua_public = ua_private.public_key().to_sec1_bytes();
    let peer = p256::PublicKey::from_sec1_bytes(as_public).expect("the sender's key");
    let shared = ua_private.diffie_hellman(&peer);
    let prk_key = hmac_sha256(auth, &[shared.raw_secret_bytes().as_slice()]);
    let ikm = hmac_sha256(&prk_key, &[b"WebPush: info\0", &ua_public, as_public, &[1]]);
    let prk = hmac_sha256(salt, &[&ikm]);
    let cek = &hmac_sha256(&prk, &[b"Content-Encoding: aes128gcm\0", &[1]])[..16];
    let nonce = &hmac_sha256(&prk, &[b"Content-Encoding: nonce\0", &[1]])[..12];

    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, cek).expect("a 16-byte key"));
    let mut buffer = record.to_vec();
    let plain = key
        .open_in_place(
            Nonce::try_assume_unique_for_key(nonce).expect("a 12-byte nonce"),
            Aad::empty(),
            &mut buffer,
        )
        .expect("the message decrypts with the device's keys")
        .to_vec();
    // The last record: trailing zero padding, then the 0x02 delimiter.
    let end = plain.iter().rposition(|&b| b != 0).expect("a delimiter");
    assert_eq!(plain[end], 0x02, "last-record delimiter");
    plain[..end].to_vec()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use calin_proto::PushKind;

    use super::*;
    use crate::{
        EndpointPolicy, ManualClock, PushConfig, PushSender, SendReport, SubscriptionStore,
        VapidKeys,
    };

    #[tokio::test]
    async fn a_device_of_the_fake_service_decrypts_what_the_sender_sends() {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(1_800_000_000_000));
        let keys = Arc::new(
            VapidKeys::load_or_generate(dir.path().join("vapid.json"))
                .await
                .unwrap(),
        );
        let store = SubscriptionStore::open(dir.path().join("push.json"), clock.clone())
            .await
            .unwrap();
        let policy = EndpointPolicy::AllowLoopbackHttpForDevelopment;
        let sender = PushSender::new(
            keys,
            store.clone(),
            clock,
            PushConfig {
                endpoint_policy: policy,
                ..PushConfig::new("mailto:ops@example.com")
            },
        )
        .unwrap();
        let service = FakePushService::start().await;
        store
            .subscribe("alice", &service.subscription("phone"), None, policy)
            .await
            .unwrap();
        store
            .subscribe("alice", &service.gone_subscription("old"), None, policy)
            .await
            .unwrap();
        let payload = PushPayload {
            kind: PushKind::Test,
            title: "Calin".to_owned(),
            body: "It works.".to_owned(),
            path: "/".to_owned(),
            session_id: None,
        };
        let report = sender
            .send("alice", PushKind::Test, payload.clone())
            .await
            .unwrap();
        assert!(
            matches!(report, SendReport::Sent(ref d) if d.len() == 2),
            "{report:?}"
        );
        let received = service.wait_for(1, Duration::from_secs(5)).await;
        assert_eq!(received.len(), 1);
        assert_eq!(received[0].device, "phone");
        assert_eq!(received[0].payload, payload);
        // The gone one was removed by the sender.
        assert_eq!(store.list("alice").len(), 1);
    }
}
