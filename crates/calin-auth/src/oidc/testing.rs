//! A small OpenID Connect provider for the tests (spec section 9): discovery,
//! key set, authorization and token endpoints over HTTP on a loopback port,
//! issuing ID tokens signed with keys generated in the test process. Every
//! way it can misbehave is a field a test sets.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use ring::rand::SystemRandom;
use ring::signature::{self, EcdsaKeyPair, KeyPair as _};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::http::ProviderHttp;
use crate::clock::{Clock as _, ManualClock};
use crate::config::LoopbackHttp;
use crate::secret::{base64url, random_id, random_token, sha256};

/// A key the provider signs with.
pub(crate) enum SigningKey {
    Es256 {
        key_id: String,
        pair: EcdsaKeyPair,
    },
    Rs256 {
        key_id: String,
        pair: ring::rsa::KeyPair,
    },
}

impl SigningKey {
    /// A fresh P-256 key.
    pub(crate) fn es256(key_id: &str) -> Arc<Self> {
        let rng = SystemRandom::new();
        let algorithm = &signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(algorithm, &rng).unwrap();
        let pair = EcdsaKeyPair::from_pkcs8(algorithm, pkcs8.as_ref(), &rng).unwrap();
        Arc::new(Self::Es256 {
            key_id: key_id.to_owned(),
            pair,
        })
    }

    /// An RSA key. Generating one takes seconds in a debug build, so the
    /// test process makes one and every caller gets it under its own key id.
    pub(crate) fn rs256(key_id: &str) -> Arc<Self> {
        static PKCS8: OnceLock<Vec<u8>> = OnceLock::new();
        let pkcs8 = PKCS8.get_or_init(|| {
            use rsa::pkcs8::EncodePrivateKey as _;
            use rsa::{Generate as _, PrivateKey};
            PrivateKey::generate()
                .to_pkcs8_der()
                .unwrap()
                .as_bytes()
                .to_vec()
        });
        Arc::new(Self::Rs256 {
            key_id: key_id.to_owned(),
            pair: ring::rsa::KeyPair::from_pkcs8(pkcs8).unwrap(),
        })
    }

    pub(crate) fn key_id(&self) -> &str {
        match self {
            Self::Es256 { key_id, .. } | Self::Rs256 { key_id, .. } => key_id,
        }
    }

    fn algorithm(&self) -> &'static str {
        match self {
            Self::Es256 { .. } => "ES256",
            Self::Rs256 { .. } => "RS256",
        }
    }

    pub(crate) fn jwk(&self) -> Value {
        match self {
            Self::Es256 { key_id, pair } => {
                let point = pair.public_key().as_ref();
                json!({
                    "kty": "EC", "crv": "P-256", "use": "sig", "alg": "ES256", "kid": key_id,
                    "x": base64url(&point[1..33]), "y": base64url(&point[33..65]),
                })
            }
            Self::Rs256 { key_id, pair } => {
                let public = ring::rsa::PublicKeyComponents::<Vec<u8>>::from(pair.public());
                json!({
                    "kty": "RSA", "use": "sig", "alg": "RS256", "kid": key_id,
                    "n": base64url(&public.n), "e": base64url(&public.e),
                })
            }
        }
    }

    fn sign(&self, message: &[u8]) -> Vec<u8> {
        let rng = SystemRandom::new();
        match self {
            Self::Es256 { pair, .. } => pair.sign(&rng, message).unwrap().as_ref().to_vec(),
            Self::Rs256 { pair, .. } => {
                let mut signature = vec![0; pair.public().modulus_len()];
                pair.sign(&signature::RSA_PKCS1_SHA256, &rng, message, &mut signature)
                    .unwrap();
                signature
            }
        }
    }
}

/// How the provider signs the ID token.
pub(crate) enum Signing {
    /// With `signing_key`, honestly.
    Key,
    /// `alg: none` and an empty signature.
    Unsigned,
    /// HS256 keyed with this text: the classic key-confusion forgery.
    Hmac(String),
}

/// What an endpoint answers instead of doing its job.
pub(crate) enum Misbehavior {
    /// This status and body.
    Answer(u16, String),
    /// A 302 to this URL.
    Redirect(String),
    /// A body of this many bytes, with or without declaring its length.
    Oversized { bytes: usize, declare_length: bool },
    /// Accept the request and never answer.
    Hang,
}

/// The user who "signs in" at the provider.
#[derive(Clone)]
pub(crate) struct User {
    pub(crate) subject: String,
    pub(crate) email: Option<String>,
    /// Sent as given; `None` leaves the claim out.
    pub(crate) email_verified: Option<Value>,
}

#[derive(Default)]
pub(crate) struct Hits {
    /// Connections accepted, including those dropped while "down".
    pub(crate) connections: usize,
    pub(crate) discovery: usize,
    pub(crate) keys: usize,
    pub(crate) authorize: usize,
    pub(crate) token: usize,
}

struct Grant {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    nonce: String,
    user: User,
    used: bool,
}

/// Everything about the provider a test can read or change.
pub(crate) struct State {
    /// What discovery publishes as `issuer` and tokens carry as `iss`.
    pub(crate) issuer: String,
    /// What tokens carry as `iss` when it should differ from discovery.
    pub(crate) token_issuer: Option<String>,
    pub(crate) client_id: String,
    pub(crate) client_secret: Option<String>,
    pub(crate) user: User,
    pub(crate) published_keys: Vec<Arc<SigningKey>>,
    pub(crate) signing_key: Arc<SigningKey>,
    pub(crate) signing: Signing,
    /// Claims set (`Some`) or removed (`None`) after the honest ones are built.
    pub(crate) claim_edits: Vec<(&'static str, Option<Value>)>,
    pub(crate) header_edits: Vec<(&'static str, Option<Value>)>,
    /// Applied to the finished token.
    pub(crate) tamper: Option<fn(String) -> String>,
    pub(crate) discovery: Option<Misbehavior>,
    pub(crate) keys: Option<Misbehavior>,
    pub(crate) token: Option<Misbehavior>,
    /// Extra members merged into the discovery document.
    pub(crate) discovery_extra: Map<String, Value>,
    /// A real provider refuses a code the second time; this one can forget to.
    pub(crate) allow_code_replay: bool,
    /// Drop every connection without answering.
    pub(crate) down: bool,
    pub(crate) hits: Hits,
    /// The last token request: its `Authorization` header and its form.
    pub(crate) last_token_request: Option<(Option<String>, HashMap<String, String>)>,
    pub(crate) last_id_token: Option<String>,
    codes: HashMap<String, Grant>,
}

pub(crate) struct FakeProvider {
    /// `http://127.0.0.1:<port>`.
    pub(crate) origin: String,
    state: Arc<Mutex<State>>,
    browser: ProviderHttp,
    server: tokio::task::JoinHandle<()>,
}

impl Drop for FakeProvider {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl FakeProvider {
    pub(crate) async fn start(
        clock: Arc<ManualClock>,
        client_id: &str,
        client_secret: Option<&str>,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let key = SigningKey::es256("key-1");
        let state = Arc::new(Mutex::new(State {
            issuer: origin.clone(),
            token_issuer: None,
            client_id: client_id.to_owned(),
            client_secret: client_secret.map(str::to_owned),
            user: User {
                subject: "user-1".to_owned(),
                email: Some("alice@example.com".to_owned()),
                email_verified: Some(json!(true)),
            },
            published_keys: vec![key.clone()],
            signing_key: key,
            signing: Signing::Key,
            claim_edits: Vec::new(),
            header_edits: Vec::new(),
            tamper: None,
            discovery: None,
            keys: None,
            token: None,
            discovery_extra: Map::new(),
            allow_code_replay: false,
            down: false,
            hits: Hits::default(),
            last_token_request: None,
            last_id_token: None,
            codes: HashMap::new(),
        }));
        let server = tokio::spawn(serve(listener, origin.clone(), state.clone(), clock));
        Self {
            origin,
            state,
            browser: ProviderHttp::new(LoopbackHttp::AllowForDevelopment, Duration::from_secs(5))
                .unwrap(),
            server,
        }
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    /// Publishes a new key and signs with it from now on; the old one stays
    /// published unless `retire_old`.
    pub(crate) fn rotate_key(&self, key: Arc<SigningKey>, retire_old: bool) {
        let mut state = self.state();
        if retire_old {
            state.published_keys.clear();
        }
        state.published_keys.push(key.clone());
        state.signing_key = key;
    }

    /// What the user's browser does with the authorization URL: asks the
    /// provider, where the user signs in, and comes back with the query
    /// string of the redirect to `/auth/callback`.
    pub(crate) async fn authorize(&self, authorization_url: &str) -> String {
        let url = url::Url::parse(authorization_url).unwrap();
        let response = self.browser.get(&url).await.unwrap();
        assert_eq!(
            response.status,
            302,
            "{}",
            String::from_utf8_lossy(&response.body)
        );
        let location = url::Url::parse(&response.location.unwrap()).unwrap();
        assert_eq!(location.path(), "/auth/callback");
        location.query().unwrap().to_owned()
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

async fn serve(
    listener: TcpListener,
    origin: String,
    state: Arc<Mutex<State>>,
    clock: Arc<ManualClock>,
) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        let down = {
            let mut state = state.lock().unwrap();
            state.hits.connections += 1;
            state.down
        };
        if down {
            continue;
        }
        let (origin, state, clock) = (origin.clone(), state.clone(), clock.clone());
        tokio::spawn(async move {
            let mut stream = stream;
            let Some(request) = read_request(&mut stream).await else {
                return;
            };
            let answer = route(&request, &origin, &state, &clock);
            let bytes = match answer {
                Answer::Hang => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    return;
                }
                Answer::Bytes(bytes) => bytes,
            };
            let _ = stream.write_all(&bytes).await;
            let _ = stream.shutdown().await;
        });
    }
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let head_end = loop {
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 || buffer.len() > 64 * 1024 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8(buffer[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut request_line = lines.next()?.split(' ');
    let method = request_line.next()?.to_owned();
    let target = request_line.next()?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let headers: HashMap<String, String> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = buffer[head_end..].to_vec();
    while body.len() < length {
        let mut chunk = [0u8; 4096];
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Request {
        method,
        path: path.to_owned(),
        query: query.to_owned(),
        headers,
        body,
    })
}

enum Answer {
    Bytes(Vec<u8>),
    Hang,
}

fn answer(status: u16, content_type: &str, extra_headers: &str, body: &[u8]) -> Answer {
    let mut bytes = format!(
        "HTTP/1.1 {status} Status\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    Answer::Bytes(bytes)
}

fn json_answer(status: u16, body: &Value) -> Answer {
    answer(status, "application/json", "", body.to_string().as_bytes())
}

fn misbehave(misbehavior: &Misbehavior) -> Answer {
    match misbehavior {
        Misbehavior::Answer(status, body) => {
            answer(*status, "application/json", "", body.as_bytes())
        }
        Misbehavior::Redirect(location) => {
            answer(302, "text/plain", &format!("Location: {location}\r\n"), b"")
        }
        Misbehavior::Oversized {
            bytes,
            declare_length: true,
        } => answer(200, "application/json", "", &vec![b' '; *bytes]),
        Misbehavior::Oversized {
            bytes,
            declare_length: false,
        } => {
            // No length: the body runs until the connection closes.
            let mut raw =
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n"
                    .to_vec();
            raw.extend(std::iter::repeat_n(b' ', *bytes));
            Answer::Bytes(raw)
        }
        Misbehavior::Hang => Answer::Hang,
    }
}

fn form(text: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(text.as_bytes())
        .into_owned()
        .collect()
}

fn route(request: &Request, origin: &str, state: &Mutex<State>, clock: &ManualClock) -> Answer {
    let mut state = state.lock().unwrap();
    match (request.method.as_str(), request.path.as_str()) {
        ("GET", path) if path.ends_with("/.well-known/openid-configuration") => {
            state.hits.discovery += 1;
            if let Some(misbehavior) = &state.discovery {
                return misbehave(misbehavior);
            }
            let mut document = json!({
                "issuer": state.issuer,
                "authorization_endpoint": format!("{origin}/authorize"),
                "token_endpoint": format!("{origin}/token"),
                "jwks_uri": format!("{origin}/keys"),
                "response_types_supported": ["code"],
                "subject_types_supported": ["public"],
                "id_token_signing_alg_values_supported": ["ES256", "RS256"],
                "code_challenge_methods_supported": ["S256"],
            });
            for (name, value) in &state.discovery_extra {
                document[name.as_str()] = value.clone();
            }
            json_answer(200, &document)
        }
        ("GET", "/keys") => {
            state.hits.keys += 1;
            if let Some(misbehavior) = &state.keys {
                return misbehave(misbehavior);
            }
            let keys: Vec<Value> = state.published_keys.iter().map(|key| key.jwk()).collect();
            json_answer(200, &json!({ "keys": keys }))
        }
        ("GET", "/authorize") => {
            state.hits.authorize += 1;
            authorize(&mut state, &form(&request.query))
        }
        ("POST", "/token") => {
            state.hits.token += 1;
            let fields = form(&String::from_utf8_lossy(&request.body));
            let authorization = request.headers.get("authorization").cloned();
            state.last_token_request = Some((authorization.clone(), fields.clone()));
            if let Some(misbehavior) = &state.token {
                return misbehave(misbehavior);
            }
            token(&mut state, authorization.as_deref(), &fields, clock.now())
        }
        _ => answer(404, "text/plain", "", b"not found"),
    }
}

fn authorize(state: &mut State, query: &HashMap<String, String>) -> Answer {
    let get = |name: &str| query.get(name).cloned().unwrap_or_default();
    let problems: Vec<&str> = [
        ("response_type must be code", get("response_type") == "code"),
        ("unknown client_id", get("client_id") == state.client_id),
        (
            "scope must be exactly \"openid email\"",
            get("scope") == "openid email",
        ),
        (
            "code_challenge_method must be S256",
            get("code_challenge_method") == "S256",
        ),
        (
            "code_challenge must be 43 characters",
            get("code_challenge").len() == 43,
        ),
        ("state is required", !get("state").is_empty()),
        ("nonce is required", !get("nonce").is_empty()),
        (
            "redirect_uri must end with /auth/callback",
            get("redirect_uri").ends_with("/auth/callback"),
        ),
    ]
    .into_iter()
    .filter_map(|(problem, ok)| (!ok).then_some(problem))
    .collect();
    if !problems.is_empty() {
        return answer(400, "text/plain", "", problems.join("; ").as_bytes());
    }
    let code = random_token().unwrap();
    state.codes.insert(
        code.clone(),
        Grant {
            client_id: get("client_id"),
            redirect_uri: get("redirect_uri"),
            code_challenge: get("code_challenge"),
            nonce: get("nonce"),
            user: state.user.clone(),
            used: false,
        },
    );
    let mut location = url::Url::parse(&get("redirect_uri")).unwrap();
    location
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &get("state"));
    answer(302, "text/plain", &format!("Location: {location}\r\n"), b"")
}

fn token(
    state: &mut State,
    authorization: Option<&str>,
    fields: &HashMap<String, String>,
    now: u64,
) -> Answer {
    let refuse = |status: u16, error: &str| json_answer(status, &json!({ "error": error }));
    let get = |name: &str| fields.get(name).cloned().unwrap_or_default();

    // Client authentication: Basic for a confidential client, the bare id
    // in the form for a public one, and never both.
    let client_ok = match &state.client_secret {
        Some(secret) => {
            let encode = |text: &str| -> String {
                url::form_urlencoded::byte_serialize(text.as_bytes()).collect()
            };
            let expected = base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                encode(&state.client_id),
                encode(secret)
            ));
            authorization == Some(format!("Basic {expected}").as_str())
                && !fields.contains_key("client_secret")
        }
        None => authorization.is_none() && get("client_id") == state.client_id,
    };
    if !client_ok {
        return refuse(401, "invalid_client");
    }
    if get("grant_type") != "authorization_code" {
        return refuse(400, "unsupported_grant_type");
    }
    let allow_replay = state.allow_code_replay;
    let client_id = state.client_id.clone();
    let Some(grant) = state.codes.get_mut(&get("code")) else {
        return refuse(400, "invalid_grant");
    };
    let challenge = base64url(&sha256(get("code_verifier").as_bytes()));
    if (grant.used && !allow_replay)
        || grant.client_id != client_id
        || grant.redirect_uri != get("redirect_uri")
        || grant.code_challenge != challenge
    {
        return refuse(400, "invalid_grant");
    }
    grant.used = true;
    let (nonce, user) = (grant.nonce.clone(), grant.user.clone());

    let mut claims = json!({
        "iss": state.token_issuer.as_ref().unwrap_or(&state.issuer),
        "sub": user.subject,
        "aud": state.client_id,
        "exp": now + 300,
        "iat": now,
        "nonce": nonce,
        "jti": random_id().unwrap(),
    });
    if let Some(email) = user.email {
        claims["email"] = json!(email);
    }
    if let Some(verified) = user.email_verified {
        claims["email_verified"] = verified;
    }
    edit(&mut claims, &state.claim_edits);

    let (algorithm, key_id) = match &state.signing {
        Signing::Key => (state.signing_key.algorithm(), state.signing_key.key_id()),
        Signing::Unsigned => ("none", state.signing_key.key_id()),
        Signing::Hmac(_) => ("HS256", state.signing_key.key_id()),
    };
    let mut header = json!({ "alg": algorithm, "typ": "JWT", "kid": key_id });
    edit(&mut header, &state.header_edits);

    let signed = format!(
        "{}.{}",
        base64url(header.to_string().as_bytes()),
        base64url(claims.to_string().as_bytes())
    );
    let signature = match &state.signing {
        Signing::Key => state.signing_key.sign(signed.as_bytes()),
        Signing::Unsigned => Vec::new(),
        Signing::Hmac(secret) => {
            let key = ring::hmac::Key::new(ring::hmac::HMAC_SHA256, secret.as_bytes());
            ring::hmac::sign(&key, signed.as_bytes()).as_ref().to_vec()
        }
    };
    let mut id_token = format!("{signed}.{}", base64url(&signature));
    if let Some(tamper) = state.tamper {
        id_token = tamper(id_token);
    }
    state.last_id_token = Some(id_token.clone());
    json_answer(
        200,
        &json!({
            "access_token": random_token().unwrap(),
            "token_type": "Bearer",
            "expires_in": 300,
            "id_token": id_token,
        }),
    )
}

fn edit(object: &mut Value, edits: &[(&'static str, Option<Value>)]) {
    let map = object.as_object_mut().unwrap();
    for (name, value) in edits {
        match value {
            Some(value) => map.insert((*name).to_owned(), value.clone()),
            None => map.remove(*name),
        };
    }
}
