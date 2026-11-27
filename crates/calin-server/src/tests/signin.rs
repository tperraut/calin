//! Public mode: sign-in, sessions, device tokens, and everything the server
//! refuses without them (`docs/specs/2026-11-27-remote-access.md`, sections
//! 1 to 4). The provider is `calin-auth`'s test provider: real signed
//! tokens over loopback HTTP.

use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use axum::http::header::{
    AUTHORIZATION, CONTENT_SECURITY_POLICY, COOKIE, LOCATION, REFERRER_POLICY, SET_COOKIE,
    STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use calin_auth::test_provider::TestProvider;
use calin_auth::{AuthMode, LoopbackHttp, ManualClock, PublicConfig};
use calin_proto::{
    AuthModeKind, AuthSessionInfo, CreatedDeviceToken, DeviceTokenInfo, Me, PublicHealth,
};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

mod push;

const START: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;
const PUBLIC_URL: &str = "https://calin.example.com";
const PUBLIC_HOST: &str = "calin.example.com";
const CLIENT_ID: &str = "calin-test";
const CLIENT_SECRET: &str = "client-secret-9f8e7d6c5b4a";
const ALICE: &str = "alice@example.com";
const BOB: &str = "bob@example.com";
const BOTH: &str = "alice@example.com, bob@example.com";
const SESSION_COOKIE: &str = "__Host-calin_session";
const LOGIN_COOKIE: &str = "__Secure-calin_login";
const FAILURE_PAGE: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Calin</title>
</head>
<body>
<h1>Sign-in failed. Please try again.</h1>
<p><a href=\"/\">Back to Calin</a></p>
</body>
</html>
";

// --- A server in public mode ---

fn public_config(issuer: &str, allowed: &str) -> PublicConfig {
    let vars: BTreeMap<&str, &str> = BTreeMap::from([
        ("CALIN_PUBLIC_URL", PUBLIC_URL),
        ("CALIN_OIDC_ISSUER", issuer),
        ("CALIN_OIDC_CLIENT_ID", CLIENT_ID),
        ("CALIN_OIDC_CLIENT_SECRET", CLIENT_SECRET),
        ("CALIN_ALLOWED_USERS", allowed),
    ]);
    // The development allowance covers the provider on its loopback port;
    // the public URL is https as in production.
    let lookup = |name: &str| vars.get(name).map(|value| (*value).to_owned());
    match AuthMode::from_lookup(lookup, LoopbackHttp::AllowForDevelopment) {
        Ok(AuthMode::Public(config)) => config,
        other => panic!("not a public configuration: {other:?}"),
    }
}

struct Public {
    app: Router,
    state: AppState,
    provider: TestProvider,
    clock: Arc<ManualClock>,
    /// Push has a clock of its own, in milliseconds: the throttle's.
    push_clock: Arc<calin_push::ManualClock>,
    home: TempDir,
}

impl Public {
    fn auth_file(&self) -> PathBuf {
        self.home.path().join(AUTH_FILE)
    }
}

async fn open_sign_in(
    provider: &TestProvider,
    clock: &Arc<ManualClock>,
    auth_file: &Path,
    allowed: &str,
) -> Arc<SignIn> {
    let config = public_config(provider.issuer(), allowed);
    Arc::new(
        SignIn::open(config, auth_file, clock.clone())
            .await
            .unwrap(),
    )
}

/// A server in public mode on a temporary home, with the provider up and
/// `allowed` as its allow-list.
async fn public_mode(allowed: &str) -> Public {
    let home = TempDir::new().unwrap();
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    let state = state(&home, true).await;
    public_on(home, state, clock, provider, allowed).await
}

/// `state` in public mode, with sign-in and push, on its home `home`.
async fn public_on(
    home: TempDir,
    state: AppState,
    clock: Arc<ManualClock>,
    provider: TestProvider,
    allowed: &str,
) -> Public {
    let sign_in = open_sign_in(&provider, &clock, &home.path().join(AUTH_FILE), allowed).await;
    let push_clock = Arc::new(calin_push::ManualClock::new(START * 1000));
    let push = open_push(home.path(), &push_clock).await;
    let state = state
        .with_assets(Assets::new(WEB_APP))
        .with_sign_in(sign_in)
        .with_push(Arc::new(push));
    Public {
        app: router(state.clone()),
        state,
        provider,
        clock,
        push_clock,
        home,
    }
}

/// Push as public mode opens it, except that the push service may be the
/// tests' fake one on a loopback port.
async fn open_push(home: &Path, clock: &Arc<calin_push::ManualClock>) -> Push {
    let setup = PushSetup {
        subject: vapid_subject(PUBLIC_URL, None).unwrap(),
        endpoint_policy: calin_push::EndpointPolicy::AllowLoopbackHttpForDevelopment,
    };
    Push::open(home, setup, clock.clone()).await.unwrap()
}

// --- Clients ---

/// How a test reaches the server: the router itself, or a real listener
/// when connection info or a WebSocket matters.
#[derive(Clone)]
enum Via {
    Router(Router),
    Listener(SocketAddr),
}

type Headers = Vec<(String, String)>;

fn header(name: &str, value: impl Into<String>) -> (String, String) {
    (name.to_owned(), value.into())
}

async fn send_via(
    via: &Via,
    method: Method,
    path: &str,
    headers: &Headers,
    body: Option<String>,
) -> Reply {
    match via {
        Via::Router(app) => {
            let mut request = Request::builder().method(method).uri(path);
            for (name, value) in headers {
                request = request.header(name, value);
            }
            let body = body.map(Body::from).unwrap_or_else(Body::empty);
            call(app, request.body(body).unwrap()).await
        }
        Via::Listener(addr) => http(*addr, &method, path, headers, body).await,
    }
}

/// One HTTP/1.1 request over a real socket.
async fn http(
    addr: SocketAddr,
    method: &Method,
    path: &str,
    headers: &Headers,
    body: Option<String>,
) -> Reply {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mut request = format!("{method} {path} HTTP/1.1\r\nConnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    let body = body.unwrap_or_default();
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(TIMEOUT, stream.read_to_end(&mut response))
        .await
        .expect("no response")
        .unwrap();
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a complete response head");
    let head = String::from_utf8(response[..split].to_vec()).unwrap();
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .unwrap()
        .parse()
        .unwrap();
    let mut reply_headers = HeaderMap::new();
    for line in lines {
        let (name, value) = line.split_once(':').unwrap();
        reply_headers.append(
            axum::http::HeaderName::from_bytes(name.trim().as_bytes()).unwrap(),
            value.trim().parse().unwrap(),
        );
    }
    Reply {
        status: StatusCode::from_u16(status).unwrap(),
        headers: reply_headers,
        body: response[split + 4..].to_vec(),
    }
}

fn set_cookies(reply: &Reply) -> Vec<String> {
    reply
        .headers
        .get_all(SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_owned())
        .collect()
}

/// The `Set-Cookie` of `reply` for the cookie `name`.
fn set_cookie(reply: &Reply, name: &str) -> Option<String> {
    set_cookies(reply)
        .into_iter()
        .find(|cookie| cookie.starts_with(&format!("{name}=")))
}

/// What a browser keeps of the cookies it is sent: the value and the path.
#[derive(Clone, Default)]
struct Jar {
    cookies: BTreeMap<String, (String, String)>,
}

impl Jar {
    fn absorb(&mut self, reply: &Reply) {
        for cookie in set_cookies(reply) {
            let mut parts = cookie.split(';').map(str::trim);
            let (name, value) = parts.next().unwrap().split_once('=').unwrap();
            let attributes: Vec<&str> = parts.collect();
            let path = attributes
                .iter()
                .find_map(|attribute| attribute.strip_prefix("Path="))
                .unwrap_or("/");
            if attributes.contains(&"Max-Age=0") {
                self.cookies.remove(name);
            } else {
                self.cookies
                    .insert(name.to_owned(), (value.to_owned(), path.to_owned()));
            }
        }
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(|(value, _)| value.as_str())
    }

    /// The `Cookie` header for a request to `path`.
    fn header_for(&self, path: &str) -> Option<String> {
        let path = path.split('?').next().unwrap_or(path);
        let pairs: Vec<String> = self
            .cookies
            .iter()
            .filter(|(_, (_, scope))| {
                scope == "/" || path == scope || path.starts_with(&format!("{scope}/"))
            })
            .map(|(name, (value, _))| format!("{name}={value}"))
            .collect();
        (!pairs.is_empty()).then(|| pairs.join("; "))
    }
}

/// A browser: a cookie jar, and an `Origin` on everything that changes state.
struct Browser {
    via: Via,
    host: String,
    origin: String,
    jar: Jar,
    /// Extra headers on every request, as a reverse proxy would add.
    extra: Headers,
}

impl Browser {
    /// A browser that reached the server through the reverse proxy.
    fn new(app: &Router) -> Self {
        Self {
            via: Via::Router(app.clone()),
            host: PUBLIC_HOST.to_owned(),
            origin: PUBLIC_URL.to_owned(),
            jar: Jar::default(),
            extra: Vec::new(),
        }
    }

    /// A browser talking to a real listener.
    fn over(addr: SocketAddr) -> Self {
        Self {
            via: Via::Listener(addr),
            host: addr.to_string(),
            origin: PUBLIC_URL.to_owned(),
            jar: Jar::default(),
            extra: Vec::new(),
        }
    }

    fn headers(&self, method: &Method, path: &str) -> Headers {
        let mut headers = vec![
            header("host", &self.host),
            header("user-agent", "TestBrowser/1.0"),
        ];
        if let Some(cookie) = self.jar.header_for(path) {
            headers.push(header("cookie", cookie));
        }
        if !matches!(*method, Method::GET | Method::HEAD) {
            headers.push(header("origin", &self.origin));
        }
        headers.extend(self.extra.iter().cloned());
        headers
    }

    async fn request(&mut self, method: Method, path: &str, body: Option<Value>) -> Reply {
        let mut headers = self.headers(&method, path);
        if body.is_some() {
            headers.push(header("content-type", "application/json"));
        }
        let body = body.map(|body| body.to_string());
        let reply = send_via(&self.via, method, path, &headers, body).await;
        self.jar.absorb(&reply);
        reply
    }

    async fn get(&mut self, path: &str) -> Reply {
        self.request(Method::GET, path, None).await
    }

    async fn post(&mut self, path: &str, body: Value) -> Reply {
        self.request(Method::POST, path, Some(body)).await
    }

    async fn delete(&mut self, path: &str) -> Reply {
        self.request(Method::DELETE, path, None).await
    }

    /// `GET /auth/login`: the redirect to the provider.
    async fn begin(&mut self, next: Option<&str>) -> Reply {
        let path = match next {
            Some(next) => {
                let next: String = url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
                format!("/auth/login?next={next}")
            }
            None => "/auth/login".to_owned(),
        };
        self.get(&path).await
    }

    /// The whole sign-in: to the provider and back. The callback's reply.
    async fn sign_in(&mut self, provider: &TestProvider, next: Option<&str>) -> Reply {
        let redirect = self.begin(next).await;
        assert_eq!(
            redirect.status,
            StatusCode::SEE_OTHER,
            "{}",
            redirect.text()
        );
        let query = provider.authorize(location(&redirect)).await;
        self.get(&format!("/auth/callback?{query}")).await
    }

    async fn signed_in(app: &Router, provider: &TestProvider) -> Self {
        let mut browser = Self::new(app);
        let reply = browser.sign_in(provider, None).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        browser
    }

    async fn me(&mut self) -> Reply {
        self.get("/api/v1/me").await
    }

    fn session_cookie(&self) -> String {
        format!(
            "{SESSION_COOKIE}={}",
            self.jar.value(SESSION_COOKIE).unwrap()
        )
    }

    async fn create_token(&mut self, name: &str) -> CreatedDeviceToken {
        let reply = self.post("/api/v1/tokens", json!({ "name": name })).await;
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        reply.json()
    }
}

fn location(reply: &Reply) -> &str {
    reply.headers[LOCATION].to_str().unwrap()
}

/// A request with these headers and no cookie jar, through the router.
async fn plain(app: &Router, method: Method, path: &str, headers: &[(&str, &str)]) -> Reply {
    let headers: Headers = headers
        .iter()
        .map(|(name, value)| header(name, *value))
        .collect();
    send_via(&Via::Router(app.clone()), method, path, &headers, None).await
}

/// What a script with a device token sends: the token, and no cookie.
async fn with_token(app: &Router, method: Method, path: &str, token: &str) -> Reply {
    let bearer = format!("Bearer {token}");
    plain(
        app,
        method,
        path,
        &[("host", PUBLIC_HOST), ("authorization", &bearer)],
    )
    .await
}

fn assert_unauthorized(reply: &Reply) {
    reply.assert_error(StatusCode::UNAUTHORIZED);
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
}

// --- Local mode ---

#[tokio::test]
async fn local_mode_has_nobody_to_sign_in() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await.with_assets(Assets::new(WEB_APP)));

    let me: Me = get(&app, "/api/v1/me").await.json();
    assert_eq!(
        me,
        Me {
            mode: AuthModeKind::Local,
            identity: None,
            device_token: false
        }
    );
    // Credentials mean nothing here: they are neither needed nor read.
    let ignored = plain(
        &app,
        Method::GET,
        "/api/v1/me",
        &[("host", LOCAL), ("authorization", "Bearer calin_nonsense")],
    )
    .await;
    assert_eq!(ignored.json::<Me>(), me);

    // The five account routes: there is no account.
    for (method, path) in [
        (Method::GET, "/api/v1/auth/sessions"),
        (Method::DELETE, "/api/v1/auth/sessions/abc"),
        (Method::GET, "/api/v1/tokens"),
        (Method::POST, "/api/v1/tokens"),
        (Method::DELETE, "/api/v1/tokens/abc"),
    ] {
        let reply = send_json(&app, method.clone(), path, json!({ "name": "ci" })).await;
        reply.assert_error(StatusCode::CONFLICT);
        assert!(reply.set_cookie_free(), "{method} {path}");
    }

    // The three sign-in routes do not exist, and nothing else under /auth
    // is the web app's.
    for (method, path) in [
        (Method::GET, "/auth/login"),
        (Method::GET, "/auth/login?next=/x"),
        (Method::GET, "/auth/callback?code=a&state=b"),
        (Method::POST, "/auth/logout"),
        (Method::GET, "/auth"),
        (Method::GET, "/auth/anything"),
    ] {
        let reply = call(
            &app,
            request(method.clone(), path).body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{method} {path}");
        assert!(!reply.text().contains("the app"), "{method} {path}");
        assert!(reply.set_cookie_free(), "{method} {path}");
    }
    // The full health, as before.
    let health: Health = get(&app, "/api/v1/health").await.json();
    assert_eq!(health.name, "calin");

    assert!(!dir.path().join(AUTH_FILE).exists());
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("auth"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

impl Reply {
    fn set_cookie_free(&self) -> bool {
        !self.headers.contains_key(SET_COOKIE)
    }
}

// --- The route table ---

/// A path of the table with its parameters filled in.
fn concrete(path: &str) -> String {
    let segments: Vec<&str> = path
        .split('/')
        .map(|segment| {
            if segment.starts_with('{') {
                "x"
            } else {
                segment
            }
        })
        .collect();
    format!("{API_ROOT}{}", segments.join("/"))
}

fn every_method() -> [Method; 7] {
    [
        Method::GET,
        Method::HEAD,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
    ]
}

/// The rows of the contract's table: the method and the path.
fn contract_routes() -> BTreeSet<(String, String)> {
    include_str!("../../../calin-proto/src/api.rs")
        .lines()
        .filter_map(|line| line.strip_prefix("//! | "))
        .filter_map(|row| {
            let mut cells = row.split(" | ");
            let method = cells.next()?.trim();
            let path = cells.next()?.trim().strip_prefix('`')?;
            // The query a row documents is not part of the route.
            let path = path.split(['?', '`']).next()?;
            matches!(method, "GET" | "POST" | "PUT" | "DELETE")
                .then(|| (method.to_owned(), path.to_owned()))
        })
        .collect()
}

#[test]
fn the_route_table_is_the_contracts_table() {
    let served: BTreeSet<(String, String)> = api_routes()
        .iter()
        .map(|route| (route.method.to_string(), route.path.to_owned()))
        .collect();
    assert_eq!(served.len(), api_routes().len(), "a route is listed twice");
    let contract = contract_routes();
    assert!(contract.len() > 30, "the contract's table was not read");
    // Nothing is served that the contract does not list.
    let undocumented: Vec<_> = served.difference(&contract).collect();
    assert!(undocumented.is_empty(), "{undocumented:?}");
    // And everything the contract lists is served.
    let missing: Vec<_> = contract.difference(&served).collect();
    assert!(missing.is_empty(), "{missing:?}");

    // The two exceptions to sign-in, and no third.
    let exceptions: Vec<(String, &str, Access)> = api_routes()
        .iter()
        .filter(|route| route.access != Access::SignedIn)
        .map(|route| (route.method.to_string(), route.path, route.access))
        .collect();
    assert_eq!(
        exceptions,
        [
            ("GET".to_owned(), "/health", Access::Anyone),
            ("POST".to_owned(), "/mcp/{token}", Access::AgentToken),
        ]
    );
    // What `auth::authenticate` compares paths with.
    assert_eq!(API_HEALTH, format!("{API_ROOT}{HEALTH_ROUTE}"));
    assert_eq!(API_MCP_PREFIX, format!("{API_ROOT}{MCP_PATH}/"));
    assert_eq!(MCP_ROUTE, format!("{MCP_PATH}/{{token}}"));
}

#[tokio::test]
async fn the_router_serves_exactly_the_methods_the_table_lists() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await);
    let routes = api_routes();
    let paths: BTreeSet<&str> = routes.iter().map(|route| route.path).collect();
    for path in paths {
        for method in every_method() {
            // A GET route answers HEAD as well.
            let wanted = if method == Method::HEAD {
                Method::GET
            } else {
                method.clone()
            };
            let listed = routes
                .iter()
                .any(|route| route.path == path && route.method == wanted);
            let reply = plain(&app, method.clone(), &concrete(path), &[("host", LOCAL)]).await;
            assert_eq!(
                reply.status != StatusCode::METHOD_NOT_ALLOWED,
                listed,
                "{method} {path}: {}",
                reply.status
            );
        }
    }
}

#[tokio::test]
async fn without_credentials_every_route_is_a_401_but_health_and_the_tool_endpoint() {
    let public = public_mode(ALICE).await;
    let app = &public.app;
    let routes = api_routes();
    assert!(routes.len() > 30);
    let mut checked = 0;
    for route in &routes {
        // Whatever the method: the one the table lists, and the others.
        for method in every_method() {
            let path = concrete(route.path);
            let listed = method == route.method;
            for host in [PUBLIC_HOST, LOCAL] {
                let reply = plain(app, method.clone(), &path, &[("host", host)]).await;
                let what = format!("{method} {path} for {host}: {}", reply.status);
                // The exceptions are spelled out here, not read from the
                // table: a route marked open by mistake fails this test.
                if route.path == "/health" {
                    if matches!(method, Method::GET | Method::HEAD) {
                        assert_eq!(reply.status, StatusCode::OK, "{what}");
                    } else {
                        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{what}");
                    }
                } else if route.path == "/mcp/{token}" {
                    // Its own credential; never a success without it.
                    let expected = if listed {
                        StatusCode::NOT_FOUND
                    } else {
                        StatusCode::METHOD_NOT_ALLOWED
                    };
                    assert_eq!(reply.status, expected, "{what}");
                } else {
                    assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{what}");
                    assert!(reply.set_cookie_free(), "{what}");
                    // A HEAD answer has no body to read the error from.
                    if method != Method::HEAD {
                        assert_unauthorized(&reply);
                    }
                }
                checked += 1;
            }
        }
    }
    assert_eq!(checked, routes.len() * 7 * 2);

    // What is not a route is refused the same way: nothing under /api
    // tells a stranger whether it exists.
    for path in [
        "/api",
        "/api/",
        "/api/v1",
        "/api/v1/nope",
        "/api/v2/health",
        "/api/v1/health/",
        "/api/v1/health/x",
        "/api/v1/mcp",
        "/api/v1/mcp/",
        "/api/v1/mcp/x/y",
        "/api/v1/sessions/x/y/z",
    ] {
        for method in [Method::GET, Method::POST] {
            let reply = plain(app, method.clone(), path, &[("host", PUBLIC_HOST)]).await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method} {path}");
        }
    }

    // Credentials that are not credentials.
    let garbage = "A".repeat(43);
    for (name, value) in [
        ("cookie", format!("{SESSION_COOKIE}={garbage}")),
        ("cookie", format!("calin_session={garbage}")),
        ("cookie", format!("{SESSION_COOKIE}=")),
        ("authorization", format!("Bearer calin_{garbage}")),
        ("authorization", format!("Bearer {garbage}")),
        ("authorization", "Bearer".to_owned()),
        ("authorization", "Basic YWxpY2U6cGFzc3dvcmQ=".to_owned()),
        ("x-calin-token", format!("calin_{garbage}")),
    ] {
        let headers = [("host", PUBLIC_HOST), (name, value.as_str())];
        let reply = plain(app, Method::GET, "/api/v1/sessions", &headers).await;
        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{name}: {value}");
    }
    // Nobody signed in, so nothing was written.
    assert!(!public.auth_file().exists());
}

#[tokio::test]
async fn with_a_session_or_a_device_token_no_route_is_a_401() {
    let public = public_mode(ALICE).await;
    let mut browser = Browser::signed_in(&public.app, &public.provider).await;
    let token = browser.create_token("table").await.token;
    for route in api_routes() {
        let path = concrete(route.path);
        let what = format!("{} {path}", route.method);

        let by_session = browser.request(route.method.clone(), &path, None).await;
        assert_ne!(by_session.status, StatusCode::UNAUTHORIZED, "{what}");
        assert_ne!(by_session.status, StatusCode::FORBIDDEN, "{what}");

        let by_token = with_token(&public.app, route.method.clone(), &path, &token).await;
        assert_ne!(by_token.status, StatusCode::UNAUTHORIZED, "{what}");
        // A token never leaves a cookie behind.
        assert!(by_token.set_cookie_free(), "{what}");
    }
}

// --- The whole flow, over a real socket ---

struct Serving {
    addr: SocketAddr,
    stop: tokio::sync::oneshot::Sender<()>,
    server: JoinHandle<std::io::Result<()>>,
}

async fn serve_public(public: &Public) -> Serving {
    let server = Server::bind("127.0.0.1:0".parse().unwrap(), public.state.clone())
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(server.run(async move {
        let _ = stopped.await;
    }));
    Serving { addr, stop, server }
}

type Handshake = Result<Client, tokio_tungstenite::tungstenite::Error>;

/// A WebSocket handshake at `path` with these headers.
async fn handshake(addr: SocketAddr, path: &str, headers: &[(&str, &str)]) -> Handshake {
    let mut request = format!("ws://{addr}{path}").into_client_request().unwrap();
    for (name, value) in headers {
        request.headers_mut().insert(
            axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
            value.parse().unwrap(),
        );
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(client, _)| client)
}

fn refused(handshake: Handshake) -> StatusCode {
    match handshake {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => response.status(),
        Ok(_) => panic!("the handshake was accepted"),
        Err(other) => panic!("expected a refused handshake, got {other:?}"),
    }
}

fn terminal_path(session_id: &str) -> String {
    format!("/api/v1/sessions/{session_id}/terminals/ws")
}

/// Waits until the session's first turn is over: its directory exists.
async fn in_review(state: &AppState, session_id: &str) {
    events_until(state, session_id, is_review).await;
}

/// Waits for the server to close a socket it had accepted.
async fn closed_by_the_server(client: &mut Client) {
    loop {
        let frame = tokio::time::timeout(TIMEOUT, client.next())
            .await
            .expect("the socket stayed open");
        if matches!(frame, Some(Ok(Message::Close(_)) | Err(_)) | None) {
            return;
        }
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn signing_in_opens_everything_and_signing_out_closes_it_again() {
    let public = public_mode(ALICE).await;
    let serving = serve_public(&public).await;
    let addr = serving.addr;
    let mut browser = Browser::over(addr);

    // Before: nothing but the reduced health.
    assert_unauthorized(&browser.me().await);
    let health = browser.get("/api/v1/health").await;
    assert_eq!(
        health.text(),
        r#"{"name":"calin","auth":"required"}"#,
        "a stranger learns nothing else"
    );
    let _: PublicHealth = health.json();

    // To the provider, with everything the code flow needs and the cookie
    // that finds the pending login again.
    let redirect = browser.begin(Some("/sessions/42?tab=diff")).await;
    assert_eq!(redirect.status, StatusCode::SEE_OTHER);
    assert_eq!(redirect.headers[CACHE_CONTROL], "no-store");
    let authorization = url::Url::parse(location(&redirect)).unwrap();
    assert_eq!(
        format!("{}/authorize", public.provider.issuer()),
        authorization.as_str().split('?').next().unwrap()
    );
    let asked: BTreeMap<String, String> = authorization.query_pairs().into_owned().collect();
    assert_eq!(asked["response_type"], "code");
    assert_eq!(asked["client_id"], CLIENT_ID);
    assert_eq!(asked["redirect_uri"], format!("{PUBLIC_URL}/auth/callback"));
    assert_eq!(asked["scope"], "openid email");
    assert_eq!(asked["code_challenge_method"], "S256");
    for secret in ["state", "nonce", "code_challenge"] {
        assert!(asked[secret].len() >= 43, "{secret}");
    }
    let pending = set_cookie(&redirect, LOGIN_COOKIE).unwrap();
    let pending_value = browser.jar.value(LOGIN_COOKIE).unwrap().to_owned();
    assert_eq!(
        pending,
        format!(
            "{LOGIN_COOKIE}={pending_value}; Path=/auth; Max-Age=600; HttpOnly; Secure; SameSite=Lax"
        )
    );

    // Back from the provider: a session, and the way to the page asked for.
    let query = public.provider.authorize(location(&redirect)).await;
    let back = browser.get(&format!("/auth/callback?{query}")).await;
    assert_eq!(back.status, StatusCode::SEE_OTHER, "{}", back.text());
    assert_eq!(location(&back), "/sessions/42?tab=diff");
    assert_eq!(back.headers[CACHE_CONTROL], "no-store");
    let token = browser.jar.value(SESSION_COOKIE).unwrap().to_owned();
    assert_eq!(token.len(), 43);
    assert_eq!(
        set_cookie(&back, SESSION_COOKIE).unwrap(),
        format!(
            "{SESSION_COOKIE}={token}; Path=/; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax"
        )
    );
    assert_eq!(
        set_cookie(&back, LOGIN_COOKIE).unwrap(),
        format!("{LOGIN_COOKIE}=; Path=/auth; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
    );
    assert_eq!(browser.jar.value(LOGIN_COOKIE), None);
    // The file holds a hash, never the token.
    let stored = std::fs::read_to_string(public.auth_file()).unwrap();
    assert!(stored.contains(ALICE));
    assert!(!stored.contains(&token));

    // Signed in.
    let me: Me = browser.me().await.json();
    assert_eq!(
        me,
        Me {
            mode: AuthModeKind::Public,
            identity: Some(ALICE.to_owned()),
            device_token: false
        }
    );
    let health: Health = browser.get("/api/v1/health").await.json();
    assert!(!health.version.is_empty());
    assert!(health.dry_run);

    // The API.
    let created = browser
        .post(
            "/api/v1/sessions",
            json!({ "prompt": "signed in", "permission_mode": "autonomous" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text());
    let session_id = created.json::<SessionDetail>().session.id;
    in_review(&public.state, &session_id).await;
    let listed: Vec<Session> = browser.get("/api/v1/sessions").await.json();
    assert_eq!(listed.len(), 1);

    // The event socket, as the browser opens it: cookie and origin.
    let cookie = browser.session_cookie();
    let as_browser = [("cookie", cookie.as_str()), ("origin", PUBLIC_URL)];
    let mut events = handshake(addr, "/api/v1/ws", &as_browser).await.unwrap();
    send(&mut events, &subscribe(TOPIC_SESSIONS, None)).await;
    match next(&mut events).await {
        ServerMsg::SessionUpserted { session } => assert_eq!(session.id, session_id),
        other => panic!("unexpected frame {other:?}"),
    }

    // A terminal: a shell on the server.
    let mut terminal = handshake(addr, &terminal_path(&session_id), &as_browser)
        .await
        .unwrap();
    let mut screen = Screen::default();
    type_in(&mut terminal, "echo calin-$((6*7))\n").await;
    screen.until(&mut terminal, "calin-42").await;

    // Signing out ends the session, its cookie, and the sockets it opened.
    let out = browser.request(Method::POST, "/auth/logout", None).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    assert_eq!(out.headers[CACHE_CONTROL], "no-store");
    assert_eq!(
        set_cookie(&out, SESSION_COOKIE).unwrap(),
        format!("{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax")
    );
    assert_eq!(browser.jar.value(SESSION_COOKIE), None);
    closed_by_the_server(&mut events).await;
    screen.until_closed(&mut terminal).await;

    // After: 401 everywhere, with no cookie and with the old one.
    assert_unauthorized(&browser.me().await);
    let old = [("host", LOCAL), ("cookie", cookie.as_str())];
    for path in ["/api/v1/me", "/api/v1/sessions", "/api/v1/settings"] {
        let reply = plain(&public.app, Method::GET, path, &old).await;
        assert_unauthorized(&reply);
    }
    let with_old_cookie = [("cookie", cookie.as_str())];
    for path in ["/api/v1/ws".to_owned(), terminal_path(&session_id)] {
        assert_eq!(
            refused(handshake(addr, &path, &with_old_cookie).await),
            StatusCode::UNAUTHORIZED,
            "{path}"
        );
    }
    let health = plain(&public.app, Method::GET, "/api/v1/health", &old).await;
    assert_eq!(health.text(), r#"{"name":"calin","auth":"required"}"#);
    // Signing out again is not an error; there is just nothing to end.
    let again = browser.request(Method::POST, "/auth/logout", None).await;
    assert_eq!(again.status, StatusCode::NO_CONTENT);

    serving.stop.send(()).unwrap();
    serving.server.await.unwrap().unwrap();
}

// --- WebSockets ---

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_sockets_refuse_the_upgrade_without_live_credentials() {
    let public = public_mode(ALICE).await;
    let serving = serve_public(&public).await;
    let addr = serving.addr;
    let mut browser = Browser::signed_in(&public.app, &public.provider).await;
    let session_id = new_session(&public.state, "sockets").await;
    in_review(&public.state, &session_id).await;
    let token = browser.create_token("desktop").await;
    let bearer = format!("Bearer {}", token.token);
    let cookie = browser.session_cookie();
    let paths = ["/api/v1/ws".to_owned(), terminal_path(&session_id)];
    let garbage = format!("{SESSION_COOKIE}={}", "A".repeat(43));

    for path in &paths {
        // No credentials, and things that are not credentials.
        for headers in [
            vec![],
            vec![("cookie", garbage.as_str())],
            vec![("authorization", "Bearer calin_nope")],
            // A device token is not a cookie, and a session is not a bearer token.
            vec![("cookie", bearer.as_str())],
            vec![("authorization", cookie.as_str())],
        ] {
            let status = refused(handshake(addr, path, &headers).await);
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} with {headers:?}");
        }
        // The same handshake through the router: an error body, not an upgrade.
        let bare = plain(
            &public.app,
            Method::GET,
            path,
            &[
                ("host", PUBLIC_HOST),
                ("connection", "upgrade"),
                ("upgrade", "websocket"),
                ("sec-websocket-version", "13"),
                ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ],
        )
        .await;
        assert_unauthorized(&bare);
        // A signed-in browser on another site: the origin guard, before sign-in.
        let foreign = [
            ("cookie", cookie.as_str()),
            ("origin", "https://evil.example"),
        ];
        let status = refused(handshake(addr, path, &foreign).await);
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }

    // With a session, and with a device token and no cookie: both open.
    let mut by_session = Vec::new();
    let mut by_token = Vec::new();
    for path in &paths {
        let session = [("cookie", cookie.as_str()), ("origin", PUBLIC_URL)];
        by_session.push(handshake(addr, path, &session).await.unwrap());
        let device = [("authorization", bearer.as_str())];
        by_token.push(handshake(addr, path, &device).await.unwrap());
    }
    send(&mut by_token[0], &ClientMsg::Ping).await;
    assert_eq!(next(&mut by_token[0]).await, ServerMsg::Pong);

    // Revoking the token closes what it opened and opens nothing more...
    let revoked = browser
        .delete(&format!("/api/v1/tokens/{}", token.info.id))
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    for client in &mut by_token {
        closed_by_the_server(client).await;
    }
    for path in &paths {
        let device = [("authorization", bearer.as_str())];
        let status = refused(handshake(addr, path, &device).await);
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
    // ...and leaves the session's sockets alone.
    send(&mut by_session[0], &ClientMsg::Ping).await;
    assert_eq!(next(&mut by_session[0]).await, ServerMsg::Pong);

    // Revoking the session (from itself) does the same to them.
    let sessions: Vec<AuthSessionInfo> = browser.get("/api/v1/auth/sessions").await.json();
    let current = sessions.iter().find(|session| session.current).unwrap();
    let revoked = browser
        .delete(&format!("/api/v1/auth/sessions/{}", current.id))
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    for client in &mut by_session {
        closed_by_the_server(client).await;
    }
    for path in &paths {
        let session = [("cookie", cookie.as_str())];
        let status = refused(handshake(addr, path, &session).await);
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
    }
    serving.stop.send(()).unwrap();
    serving.server.await.unwrap().unwrap();
}

// --- The callback ---

/// A browser with a pending login; the query the provider sent it back with.
async fn at_the_callback(public: &Public) -> (Browser, String) {
    let mut browser = Browser::new(&public.app);
    let redirect = browser.begin(None).await;
    assert_eq!(redirect.status, StatusCode::SEE_OTHER);
    let query = public.provider.authorize(location(&redirect)).await;
    (browser, query)
}

/// Asserts the one page a failed sign-in gets, and that the pending-login
/// cookie is cleared and no session is given.
fn assert_refused(reply: &Reply, status: StatusCode) {
    assert_eq!(reply.status, status);
    assert_eq!(reply.content_type(), "text/html; charset=utf-8");
    assert_eq!(reply.text(), FAILURE_PAGE);
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
    assert_eq!(
        set_cookies(reply),
        [format!(
            "{LOGIN_COOKIE}=; Path=/auth; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
        )]
    );
    assert!(!reply.headers.contains_key(LOCATION));
}

fn replace_parameter(query: &str, name: &str, value: &str) -> String {
    let pairs: Vec<String> = url::form_urlencoded::parse(query.as_bytes())
        .map(|(key, old)| {
            let value = if key == name { value } else { old.as_ref() };
            format!("{key}={value}")
        })
        .collect();
    pairs.join("&")
}

#[tokio::test]
async fn a_refused_callback_is_one_static_page_and_no_session() {
    let public = public_mode(ALICE).await;

    // The wrong state.
    let (mut browser, query) = at_the_callback(&public).await;
    let pending = browser.jar.clone();
    let wrong = replace_parameter(&query, "state", &"B".repeat(43));
    let reply = browser.get(&format!("/auth/callback?{wrong}")).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    assert_eq!(browser.jar.value(LOGIN_COOKIE), None);
    // That used the pending login up: the right state is now too late,
    // even for a browser that kept the cookie.
    browser.jar = pending;
    let reply = browser.get(&format!("/auth/callback?{query}")).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    assert_unauthorized(&browser.me().await);

    // A callback replayed after it succeeded, cookie and all.
    let (mut browser, query) = at_the_callback(&public).await;
    let pending = browser.jar.clone();
    let reply = browser.get(&format!("/auth/callback?{query}")).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    let mut replayer = Browser::new(&public.app);
    replayer.jar = pending;
    let reply = replayer.get(&format!("/auth/callback?{query}")).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    assert_unauthorized(&replayer.me().await);
    // A used code under a fresh login of the attacker's own: the provider says no.
    let (mut thief, fresh) = at_the_callback(&public).await;
    let code: String = url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == "code")
        .map(|(_, code)| code.into_owned())
        .unwrap();
    let stolen = replace_parameter(&fresh, "code", &code);
    let reply = thief.get(&format!("/auth/callback?{stolen}")).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    assert_unauthorized(&thief.me().await);

    // No pending-login cookie at all, or no query.
    let (_, query) = at_the_callback(&public).await;
    let mut stranger = Browser::new(&public.app);
    let reply = stranger.get(&format!("/auth/callback?{query}")).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    let reply = stranger.get("/auth/callback").await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);

    // Someone the provider knows and the allow-list does not.
    public
        .provider
        .set_user("user-7", Some("mallory@example.com"), true);
    let mut mallory = Browser::new(&public.app);
    let reply = mallory.sign_in(&public.provider, None).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    assert_unauthorized(&mallory.me().await);
    // Alice's address, not verified: that is not Alice.
    public.provider.set_user("user-8", Some(ALICE), false);
    let reply = mallory.sign_in(&public.provider, None).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    public.provider.set_user("user-1", Some(ALICE), true);

    // The provider goes down between the redirect and the callback.
    let (mut browser, query) = at_the_callback(&public).await;
    public.provider.set_down(true);
    let reply = browser.get(&format!("/auth/callback?{query}")).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    assert_unauthorized(&browser.me().await);
    public.provider.set_down(false);

    // The provider says no itself.
    let (mut browser, query) = at_the_callback(&public).await;
    let state: String = url::form_urlencoded::parse(query.as_bytes())
        .find(|(name, _)| name == "state")
        .map(|(_, state)| state.into_owned())
        .unwrap();
    let denied = format!("/auth/callback?error=access_denied&state={state}");
    assert_refused(&browser.get(&denied).await, StatusCode::BAD_REQUEST);

    // Through all of it, only the one success left a session behind.
    let stored = std::fs::read_to_string(public.auth_file()).unwrap();
    assert_eq!(stored.matches("\"token_sha256\"").count(), 1, "{stored}");
}

#[tokio::test]
async fn the_failure_page_holds_nothing_of_the_request() {
    let public = public_mode(ALICE).await;
    let marker = "MARKER-7c1f";
    let script = "%3Cscript%3Ealert(1)%3C%2Fscript%3E";
    for query in [
        format!("state={marker}&code={marker}"),
        format!("error={marker}&error_description={marker}&state={marker}"),
        format!("state={script}&code={script}&error={script}&next={script}"),
        format!("{marker}={marker}"),
        format!("state=a&state=b&code={marker}"),
    ] {
        // With a live pending login, so the query is really looked at.
        let mut browser = Browser::new(&public.app);
        browser.begin(None).await;
        browser.extra = vec![
            header("user-agent", marker),
            header("referer", format!("https://evil.example/{marker}")),
            header("x-forwarded-for", "203.0.113.50"),
        ];
        let reply = browser.get(&format!("/auth/callback?{query}")).await;
        assert_refused(&reply, StatusCode::BAD_REQUEST);
        for (name, value) in &reply.headers {
            let value = value.to_str().unwrap();
            assert!(!value.contains(marker), "{name}: {value}");
            assert!(!value.contains("script%3E"), "{name}: {value}");
        }
    }
}

#[tokio::test]
async fn failed_callbacks_are_limited_per_client_address() {
    let public = public_mode(ALICE).await;
    let behind_proxy = |client: &str| {
        let mut browser = Browser::new(&public.app);
        browser.extra = vec![header("x-forwarded-for", format!("198.51.100.1, {client}"))];
        browser
    };
    // Ten real attempts that fail: each with a live pending login.
    for attempt in 0..10 {
        let mut browser = behind_proxy("203.0.113.7");
        browser.begin(None).await;
        let reply = browser.get("/auth/callback?state=wrong&code=x").await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "attempt {attempt}");
    }
    // The eleventh is refused before anything is looked at, even a good one.
    let mut browser = behind_proxy("203.0.113.7");
    let reply = browser.sign_in(&public.provider, None).await;
    assert_refused(&reply, StatusCode::TOO_MANY_REQUESTS);
    assert_unauthorized(&browser.me().await);
    let reply = behind_proxy("203.0.113.7").get("/auth/callback").await;
    assert_refused(&reply, StatusCode::TOO_MANY_REQUESTS);

    // Another client behind the same proxy is not affected, and neither is
    // one that only claims the limited address further left in the header.
    let mut other = behind_proxy("203.0.113.8");
    let reply = other.sign_in(&public.provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
    let mut claims = Browser::new(&public.app);
    claims.extra = vec![header("x-forwarded-for", "203.0.113.7, 203.0.113.9")];
    let reply = claims.sign_in(&public.provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());

    // Callbacks without a live pending login never count: nobody is locked
    // out by junk sent in their name.
    for _ in 0..12 {
        let reply = behind_proxy("203.0.113.20")
            .get("/auth/callback?state=x&code=y")
            .await;
        assert_refused(&reply, StatusCode::BAD_REQUEST);
    }
    let mut victim = behind_proxy("203.0.113.20");
    let reply = victim.sign_in(&public.provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());

    // The header counts only when the peer is the proxy, on loopback. From
    // any other peer it is the peer that is counted, whatever it claims.
    let from = |peer: &str, forwarded: &'static str, path: String, cookie: Option<String>| {
        let mut request = Request::builder()
            .uri(path)
            .header(HOST, PUBLIC_HOST)
            .header("x-forwarded-for", forwarded);
        if let Some(cookie) = cookie {
            request = request.header(COOKIE, cookie);
        }
        let mut request = request.body(Body::empty()).unwrap();
        let peer: IpAddr = peer.parse().unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(peer, 40_000)));
        call(&public.app, request)
    };
    for attempt in 0..10 {
        // Each attempt claims another address; all come from one peer.
        let forwarded: &'static str = ["192.0.2.1", "192.0.2.2"][attempt % 2];
        let start = from("192.0.2.200", forwarded, "/auth/login".to_owned(), None).await;
        let pending = set_cookie(&start, LOGIN_COOKIE).unwrap();
        let pending = pending.split(';').next().unwrap().to_owned();
        let path = "/auth/callback?state=wrong&code=x".to_owned();
        let reply = from("192.0.2.200", forwarded, path, Some(pending)).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "attempt {attempt}");
    }
    let path = "/auth/callback".to_owned();
    let reply = from("192.0.2.200", "192.0.2.77", path.clone(), None).await;
    assert_refused(&reply, StatusCode::TOO_MANY_REQUESTS);
    // The addresses it claimed are not limited.
    let reply = from("127.0.0.1", "192.0.2.1", path, None).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn sign_in_is_unavailable_while_the_provider_is_down_and_comes_back() {
    let home = TempDir::new().unwrap();
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    provider.set_down(true);
    let sign_in = open_sign_in(&provider, &clock, &home.path().join(AUTH_FILE), ALICE).await;
    // What startup does: it logs the failure and carries on.
    sign_in.warm_up().await;
    let app = router(state(&home, true).await.with_sign_in(sign_in));

    let mut browser = Browser::new(&app);
    let reply = browser.begin(Some("/sessions/1")).await;
    assert_eq!(reply.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(reply.content_type(), "text/html; charset=utf-8");
    assert!(reply.text().contains("Sign-in is temporarily unavailable"));
    assert!(!reply.text().contains("/sessions/1"));
    assert!(!reply.text().contains("127.0.0.1"), "{}", reply.text());
    assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
    assert!(reply.set_cookie_free());
    assert!(!reply.headers.contains_key(LOCATION));
    // The rest of the server is up.
    let health = browser.get("/api/v1/health").await;
    assert_eq!(health.status, StatusCode::OK);

    // The provider returns; after the backoff, so does sign-in.
    provider.set_down(false);
    clock.advance(120);
    let reply = browser.sign_in(&provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
    assert_eq!(browser.me().await.status, StatusCode::OK);
}

// --- `next` ---

#[tokio::test]
async fn next_survives_sign_in_only_as_a_path_on_this_origin() {
    let public = public_mode(ALICE).await;
    // (what the login was started with, where the callback sends the browser)
    for (next, expected) in [
        (Some("/sessions/42"), "/sessions/42"),
        (
            Some("/sessions/42/files?path=src/lib.rs&line=3#L3"),
            "/sessions/42/files?path=src/lib.rs&line=3#L3",
        ),
        (Some("/"), "/"),
        (None, "/"),
        (Some(""), "/"),
        (Some("https://evil.example/"), "/"),
        (Some("https://calin.example.com/sessions/42"), "/"),
        (Some("//evil.example"), "/"),
        (Some("//evil.example/sessions"), "/"),
        (Some("/\\evil.example"), "/"),
        (Some("\\\\evil.example"), "/"),
        (Some("sessions/42"), "/"),
        (Some("javascript:alert(1)"), "/"),
        (Some("/sessions/4 2"), "/"),
        (Some("/sessions\n/42"), "/"),
    ] {
        let mut browser = Browser::new(&public.app);
        let reply = browser.sign_in(&public.provider, next).await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{next:?}");
        assert_eq!(location(&reply), expected, "{next:?}");
    }
    // Given twice, it is not given.
    let mut browser = Browser::new(&public.app);
    let redirect = browser.get("/auth/login?next=/a&next=/b").await;
    let query = public.provider.authorize(location(&redirect)).await;
    let reply = browser.get(&format!("/auth/callback?{query}")).await;
    assert_eq!(location(&reply), "/");
    // A `next` on the callback itself is nobody's to give.
    let mut browser = Browser::new(&public.app);
    let redirect = browser.begin(Some("/kept")).await;
    let query = public.provider.authorize(location(&redirect)).await;
    let reply = browser
        .get(&format!("/auth/callback?{query}&next=https://evil.example"))
        .await;
    assert_eq!(location(&reply), "/kept");
}

// --- Sessions and device tokens ---

#[tokio::test]
async fn sessions_are_listed_and_revoked_by_their_owner_only() {
    let public = public_mode(BOTH).await;
    let mut laptop = Browser::signed_in(&public.app, &public.provider).await;
    public.clock.advance(60);
    let mut phone = Browser::signed_in(&public.app, &public.provider).await;
    public.provider.set_user("user-2", Some(BOB), true);
    let mut bob = Browser::signed_in(&public.app, &public.provider).await;
    public.clock.advance(60);

    let sessions = |reply: Reply| reply.json::<Vec<AuthSessionInfo>>();
    // From the laptop: both of Alice's, the laptop's first, none of Bob's.
    let listed = sessions(laptop.get("/api/v1/auth/sessions").await);
    assert_eq!(listed.len(), 2);
    assert!(listed[0].current && !listed[1].current);
    assert_eq!(listed[0].created_at, i64::try_from(START).unwrap() * 1000);
    assert_eq!(
        listed[1].created_at,
        i64::try_from(START + 60).unwrap() * 1000
    );
    assert_eq!(
        listed[0].last_used_at,
        i64::try_from(START + 120).unwrap() * 1000
    );
    assert_eq!(listed[0].user_agent.as_deref(), Some("TestBrowser/1.0"));
    let (laptop_id, phone_id) = (listed[0].id.clone(), listed[1].id.clone());
    // The id is not the secret.
    assert!(!laptop.session_cookie().contains(&laptop_id));
    // From the phone: the same two, the phone's first.
    let listed = sessions(phone.get("/api/v1/auth/sessions").await);
    assert_eq!(
        listed
            .iter()
            .map(|session| (session.id.as_str(), session.current))
            .collect::<Vec<_>>(),
        [(phone_id.as_str(), true), (laptop_id.as_str(), false)]
    );
    let bobs = sessions(bob.get("/api/v1/auth/sessions").await);
    assert_eq!(bobs.len(), 1);
    assert!(bobs[0].current);

    // Bob cannot revoke Alice's session, nor learn that it exists.
    for id in [laptop_id.as_str(), "nope", ""] {
        bob.delete(&format!("/api/v1/auth/sessions/{id}"))
            .await
            .assert_error(StatusCode::NOT_FOUND);
    }
    assert_eq!(laptop.me().await.status, StatusCode::OK);

    // Alice revokes the phone from the laptop.
    let reply = laptop
        .delete(&format!("/api/v1/auth/sessions/{phone_id}"))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert!(reply.set_cookie_free(), "the laptop keeps its cookie");
    assert_unauthorized(&phone.me().await);
    laptop
        .delete(&format!("/api/v1/auth/sessions/{phone_id}"))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    assert_eq!(sessions(laptop.get("/api/v1/auth/sessions").await).len(), 1);

    // Revoking the session one is using signs out: the cookie goes with it.
    let reply = laptop
        .delete(&format!("/api/v1/auth/sessions/{laptop_id}"))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_eq!(
        set_cookies(&reply),
        [format!(
            "{SESSION_COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax"
        )]
    );
    assert_unauthorized(&laptop.me().await);
    // Bob never noticed.
    assert_eq!(bob.me().await.status, StatusCode::OK);
}

#[tokio::test]
async fn device_tokens_are_created_used_and_revoked() {
    let public = public_mode(BOTH).await;
    let app = &public.app;
    let mut alice = Browser::signed_in(app, &public.provider).await;
    assert!(
        alice
            .get("/api/v1/tokens")
            .await
            .json::<Vec<DeviceTokenInfo>>()
            .is_empty()
    );

    let created = alice.create_token("  laptop CLI ").await;
    assert_eq!(created.info.name, "laptop CLI");
    assert_eq!(created.info.last_used_at, None);
    assert_eq!(
        created.info.created_at,
        i64::try_from(START).unwrap() * 1000
    );
    assert!(created.token.starts_with("calin_"));
    assert_eq!(created.token.len(), "calin_".len() + 43);
    assert!(!created.token.contains(&created.info.id));
    // Shown once: the file holds a hash, and the list never has the secret.
    let stored = std::fs::read_to_string(public.auth_file()).unwrap();
    assert!(!stored.contains(&created.token));
    assert!(!stored.contains(&created.token["calin_".len()..]));
    public.clock.advance(10);
    let second = alice.create_token("ci").await;

    // The token works on its own: no cookie, and it says what it is.
    let me = with_token(app, Method::GET, "/api/v1/me", &created.token).await;
    assert_eq!(
        me.json::<Me>(),
        Me {
            mode: AuthModeKind::Public,
            identity: Some(ALICE.to_owned()),
            device_token: true
        }
    );
    assert!(me.set_cookie_free());
    let bearer = format!("bearer {}", created.token);
    let lowercase = [("host", PUBLIC_HOST), ("authorization", bearer.as_str())];
    let reply = plain(app, Method::GET, "/api/v1/sessions", &lowercase).await;
    assert_eq!(reply.status, StatusCode::OK);
    let health = with_token(app, Method::GET, "/api/v1/health", &created.token).await;
    assert!(!health.json::<Health>().version.is_empty());
    // It changes state without an Origin, as a script does.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/sessions")
        .header(HOST, PUBLIC_HOST)
        .header(AUTHORIZATION, format!("Bearer {}", created.token))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "prompt": "from a script" }).to_string()))
        .unwrap();
    assert_eq!(call(app, request).await.status, StatusCode::CREATED);

    // Newest first, with the use recorded.
    let listed: Vec<DeviceTokenInfo> = alice.get("/api/v1/tokens").await.json();
    assert_eq!(
        listed
            .iter()
            .map(|token| token.name.as_str())
            .collect::<Vec<_>>(),
        ["ci", "laptop CLI"]
    );
    assert_eq!(
        listed[1].last_used_at,
        Some(i64::try_from(START + 10).unwrap() * 1000)
    );
    assert_eq!(listed[0].last_used_at, None);
    let listed = alice.get("/api/v1/tokens").await.text();
    assert!(!listed.contains("calin_"), "{listed}");
    // With a token there is no current session.
    let sessions: Vec<AuthSessionInfo> =
        with_token(app, Method::GET, "/api/v1/auth/sessions", &created.token)
            .await
            .json();
    assert_eq!(sessions.len(), 1);
    assert!(!sessions[0].current);

    // A token cannot make tokens: it would outlive its own revocation.
    let request = Request::builder()
        .method(Method::POST)
        .uri("/api/v1/tokens")
        .header(HOST, PUBLIC_HOST)
        .header(AUTHORIZATION, format!("Bearer {}", created.token))
        .header(CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "name": "offspring" }).to_string()))
        .unwrap();
    call(app, request).await.assert_error(StatusCode::FORBIDDEN);

    // Bob sees none of it and can revoke none of it.
    public.provider.set_user("user-2", Some(BOB), true);
    let mut bob = Browser::signed_in(app, &public.provider).await;
    assert_eq!(bob.get("/api/v1/tokens").await.text(), "[]");
    bob.delete(&format!("/api/v1/tokens/{}", created.info.id))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let me = with_token(app, Method::GET, "/api/v1/me", &created.token).await;
    assert_eq!(me.status, StatusCode::OK);

    // Revoked: 401 from then on, and only that token.
    let reply = alice
        .delete(&format!("/api/v1/tokens/{}", created.info.id))
        .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    for path in ["/api/v1/me", "/api/v1/sessions", "/api/v1/tokens"] {
        assert_unauthorized(&with_token(app, Method::GET, path, &created.token).await);
    }
    alice
        .delete(&format!("/api/v1/tokens/{}", created.info.id))
        .await
        .assert_error(StatusCode::NOT_FOUND);
    let me = with_token(app, Method::GET, "/api/v1/me", &second.token).await;
    assert_eq!(me.status, StatusCode::OK);
    // A token can revoke itself.
    let reply = with_token(
        app,
        Method::DELETE,
        &format!("/api/v1/tokens/{}", second.info.id),
        &second.token,
    )
    .await;
    assert_eq!(reply.status, StatusCode::NO_CONTENT);
    assert_unauthorized(&with_token(app, Method::GET, "/api/v1/me", &second.token).await);
}

#[tokio::test]
async fn a_device_token_name_is_1_to_80_characters_without_control_characters() {
    let public = public_mode(ALICE).await;
    let mut alice = Browser::signed_in(&public.app, &public.provider).await;
    let too_long = "n".repeat(81);
    let padded_too_long = format!("  {}  ", "n".repeat(81));
    for name in [
        "",
        " ",
        "\t\n",
        "line\nbreak",
        "tab\tinside",
        "nul\0",
        "escape\u{1b}[31m",
        too_long.as_str(),
        padded_too_long.as_str(),
    ] {
        let reply = alice.post("/api/v1/tokens", json!({ "name": name })).await;
        reply.assert_error(StatusCode::BAD_REQUEST);
        assert!(!reply.text().contains("calin_"), "{name:?}");
    }
    // Not a request at all.
    for body in [json!({}), json!({ "name": 7 }), json!([]), json!("ci")] {
        alice
            .post("/api/v1/tokens", body)
            .await
            .assert_error(StatusCode::BAD_REQUEST);
    }
    assert_eq!(alice.get("/api/v1/tokens").await.text(), "[]");

    // The longest allowed, counted in characters and after trimming.
    let longest = "\u{e9}".repeat(80);
    let padded = format!("  {}  ", "n".repeat(80));
    for name in [
        "x",
        longest.as_str(),
        padded.as_str(),
        "CI runner #2 (prod)",
    ] {
        let created = alice.create_token(name).await;
        assert_eq!(created.info.name, name.trim());
    }
}

#[tokio::test]
async fn a_device_token_is_only_ever_read_from_the_authorization_header() {
    let public = public_mode(ALICE).await;
    let app = &public.app;
    let mut alice = Browser::signed_in(app, &public.provider).await;
    let token = alice.create_token("ambient").await.token;
    let me = "/api/v1/me";

    // Nothing a browser attaches by itself carries a device token: not a
    // cookie of any name, not the URL.
    let secret = &token["calin_".len()..];
    for cookie in [
        format!("{SESSION_COOKIE}={token}"),
        format!("{SESSION_COOKIE}={secret}"),
        format!("calin_token={token}"),
        format!("token={token}; authorization=Bearer {token}"),
        format!("Authorization=Bearer {token}"),
    ] {
        let headers = [("host", PUBLIC_HOST), ("cookie", cookie.as_str())];
        assert_unauthorized(&plain(app, Method::GET, me, &headers).await);
    }
    for query in ["token", "access_token", "authorization", "bearer"] {
        let path = format!("{me}?{query}={token}");
        assert_unauthorized(&plain(app, Method::GET, &path, &[("host", PUBLIC_HOST)]).await);
    }
    // Nor any header but the one.
    for name in ["x-authorization", "x-api-key", "proxy-authorization"] {
        let value = format!("Bearer {token}");
        let headers = [("host", PUBLIC_HOST), (name, value.as_str())];
        assert_unauthorized(&plain(app, Method::GET, me, &headers).await);
    }
    // A bearer token that is wrong is not rescued by a good cookie, and two
    // Authorization headers are refused whatever they hold.
    let cookie = alice.session_cookie();
    let bearer = format!("Bearer {token}");
    for headers in [
        vec![
            ("host", PUBLIC_HOST),
            ("cookie", cookie.as_str()),
            ("authorization", "Bearer calin_wrong"),
        ],
        vec![
            ("host", PUBLIC_HOST),
            ("authorization", bearer.as_str()),
            ("authorization", bearer.as_str()),
        ],
        vec![
            ("host", PUBLIC_HOST),
            ("cookie", cookie.as_str()),
            ("authorization", "Basic abc"),
            ("authorization", bearer.as_str()),
        ],
    ] {
        assert_unauthorized(&plain(app, Method::GET, me, &headers).await);
    }
    // The token, sent the one way it is meant to be, needs nothing else:
    // no cookie comes back, so nothing ambient is ever created from it.
    let reply = with_token(app, Method::GET, me, &token).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(reply.set_cookie_free());
    // Two session cookies that disagree: nobody.
    let two = format!("{cookie}; {SESSION_COOKIE}={}", "A".repeat(43));
    let headers = [("host", PUBLIC_HOST), ("cookie", two.as_str())];
    assert_unauthorized(&plain(app, Method::GET, me, &headers).await);
}

#[tokio::test]
async fn a_revocation_that_did_not_reach_the_disk_is_a_500() {
    let public = public_mode(ALICE).await;
    let app = &public.app;
    let mut alice = Browser::signed_in(app, &public.provider).await;
    let mut other = Browser::signed_in(app, &public.provider).await;
    let token = alice.create_token("ci").await;
    let sessions: Vec<AuthSessionInfo> = alice.get("/api/v1/auth/sessions").await.json();
    let other_id = sessions.iter().find(|session| !session.current).unwrap();

    // A directory where the store writes its next file: every write fails.
    let blocker = public.home.path().join(format!("{AUTH_FILE}.tmp"));
    std::fs::create_dir(&blocker).unwrap();

    let token_path = format!("/api/v1/tokens/{}", token.info.id);
    alice
        .delete(&token_path)
        .await
        .assert_error(StatusCode::INTERNAL_SERVER_ERROR);
    let session_path = format!("/api/v1/auth/sessions/{}", other_id.id);
    alice
        .delete(&session_path)
        .await
        .assert_error(StatusCode::INTERNAL_SERVER_ERROR);
    // Revoked for this process all the same...
    assert_unauthorized(&with_token(app, Method::GET, "/api/v1/me", &token.token).await);
    assert_unauthorized(&other.me().await);
    // ...but a restart would bring both back, which is what the 500 says.
    let stored = std::fs::read_to_string(public.auth_file()).unwrap();
    assert!(stored.contains(&token.info.id));
    assert!(stored.contains(&other_id.id));
    // Asking again while the disk still refuses is still an error, not a
    // 404 that would read as "already done".
    alice
        .delete(&token_path)
        .await
        .assert_error(StatusCode::INTERNAL_SERVER_ERROR);

    // Signing out under the same conditions: the cookie goes, the status warns.
    let cookie = alice.session_cookie();
    let out = alice.request(Method::POST, "/auth/logout", None).await;
    assert_eq!(out.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        set_cookie(&out, SESSION_COOKIE)
            .unwrap()
            .contains("Max-Age=0")
    );
    let old = [("host", PUBLIC_HOST), ("cookie", cookie.as_str())];
    assert_unauthorized(&plain(app, Method::GET, "/api/v1/me", &old).await);

    // The disk recovers: the next sign-out attempt writes everything owed.
    std::fs::remove_dir(&blocker).unwrap();
    let out = alice.request(Method::POST, "/auth/logout", None).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    let stored = std::fs::read_to_string(public.auth_file()).unwrap();
    assert!(!stored.contains(&token.info.id), "{stored}");
    assert!(!stored.contains(&other_id.id), "{stored}");
    assert_eq!(stored.matches("\"token_sha256\"").count(), 0, "{stored}");
}

// --- The allow-list, the clock, the file ---

#[tokio::test]
async fn leaving_the_allow_list_ends_sessions_and_tokens_at_the_next_start() {
    let home = TempDir::new().unwrap();
    let auth_file = home.path().join(AUTH_FILE);
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    let state = state(&home, true).await;
    let serve = |sign_in: Arc<SignIn>| router(state.clone().with_sign_in(sign_in));

    let app = serve(open_sign_in(&provider, &clock, &auth_file, BOTH).await);
    let mut alice = Browser::signed_in(&app, &provider).await;
    let alices_token = alice.create_token("ci").await.token;
    provider.set_user("user-2", Some(BOB), true);
    let mut bob = Browser::signed_in(&app, &provider).await;
    let bobs_token = bob.create_token("ci").await.token;
    drop(app);

    // The same file, and a list without Alice.
    let app = serve(open_sign_in(&provider, &clock, &auth_file, BOB).await);
    alice.via = Via::Router(app.clone());
    bob.via = Via::Router(app.clone());
    assert_unauthorized(&alice.me().await);
    assert_unauthorized(&with_token(&app, Method::GET, "/api/v1/me", &alices_token).await);
    assert_eq!(bob.me().await.status, StatusCode::OK);
    let me = with_token(&app, Method::GET, "/api/v1/me", &bobs_token).await;
    assert_eq!(me.status, StatusCode::OK);
    // Alice cannot sign in again either.
    provider.set_user("user-1", Some(ALICE), true);
    let reply = Browser::new(&app).sign_in(&provider, None).await;
    assert_refused(&reply, StatusCode::BAD_REQUEST);
    let stored = std::fs::read_to_string(&auth_file).unwrap();
    assert!(!stored.contains(ALICE), "{stored}");
    drop(app);

    // Back on the list: what was revoked stays revoked.
    let app = serve(open_sign_in(&provider, &clock, &auth_file, BOTH).await);
    alice.via = Via::Router(app.clone());
    assert_unauthorized(&alice.me().await);
    assert_unauthorized(&with_token(&app, Method::GET, "/api/v1/me", &alices_token).await);
    let reply = alice.sign_in(&provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);
    assert_eq!(alice.me().await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_session_older_than_a_day_gets_its_cookie_again() {
    let public = public_mode(ALICE).await;
    let mut browser = Browser::signed_in(&public.app, &public.provider).await;
    let cookie = browser.session_cookie();
    let token = browser.create_token("ci").await.token;

    // Within the day: nothing.
    public.clock.advance(DAY - 1);
    assert!(browser.me().await.set_cookie_free());

    // A day after it was issued: the same cookie, for another 30 days.
    public.clock.advance(1);
    let reply = browser.get("/api/v1/sessions").await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(
        set_cookies(&reply),
        [format!(
            "{cookie}; Path=/; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax"
        )]
    );
    // Once: the next request gets none, and a device token never does.
    assert!(browser.me().await.set_cookie_free());
    public.clock.advance(2 * DAY);
    assert!(
        with_token(&public.app, Method::GET, "/api/v1/me", &token)
            .await
            .set_cookie_free()
    );
    // On an error answer as well: the session was used all the same.
    let reply = browser.get("/api/v1/sessions/nope").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(set_cookies(&reply).len(), 1);

    // Used every few weeks, it lives on; left alone for 30 days, it ends.
    public.clock.advance(29 * DAY);
    assert_eq!(browser.me().await.status, StatusCode::OK);
    public.clock.advance(30 * DAY);
    assert_unauthorized(&browser.me().await);
    // The device token does not expire.
    let me = with_token(&public.app, Method::GET, "/api/v1/me", &token).await;
    assert_eq!(me.status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutting_down_writes_what_the_store_still_owed_its_file() {
    let public = public_mode(ALICE).await;
    let serving = serve_public(&public).await;
    let mut browser = Browser::over(serving.addr);
    let reply = browser.sign_in(&public.provider, None).await;
    assert_eq!(reply.status, StatusCode::SEE_OTHER);

    // A use too recent for the periodic write.
    public.clock.advance(7);
    assert_eq!(browser.me().await.status, StatusCode::OK);
    let last_used = |file: &Path| -> u64 {
        let stored: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        stored["sessions"][0]["last_used"].as_u64().unwrap()
    };
    assert_eq!(last_used(&public.auth_file()), START);

    serving.stop.send(()).unwrap();
    serving.server.await.unwrap().unwrap();
    assert_eq!(last_used(&public.auth_file()), START + 7);
}

// --- The tool endpoint ---

const FORWARDING_HEADERS: [&str; 5] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
];

fn ping() -> Body {
    Body::from(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }).to_string())
}

/// A call to a tool endpoint from `peer`, with these headers.
async fn tool_call(
    app: &Router,
    endpoint: &str,
    host: &str,
    peer: Option<&str>,
    headers: &[(&str, &str)],
) -> Reply {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(endpoint)
        .header(HOST, host)
        .header(CONTENT_TYPE, "application/json");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(ping()).unwrap();
    match peer {
        Some(peer) => {
            let peer: IpAddr = peer.parse().unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(SocketAddr::new(peer, 40_000)));
            call(app, request).await
        }
        // Not through `call`: a request that carries no peer address at all.
        None => {
            let response = app.clone().oneshot(request).await.unwrap();
            let (parts, body) = response.into_parts();
            Reply {
                status: parts.status,
                headers: parts.headers,
                body: body.collect().await.unwrap().to_bytes().to_vec(),
            }
        }
    }
}

#[tokio::test]
async fn the_tool_endpoint_answers_only_to_this_machine_in_both_modes() {
    let agent = catalog::agent().await;
    let unknown_token = "/api/v1/mcp/0000";

    // The same manager behind a second router, in public mode.
    let home = TempDir::new().unwrap();
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    let sign_in = open_sign_in(&provider, &clock, &home.path().join(AUTH_FILE), ALICE).await;
    let public = router(agent.state.clone().with_sign_in(sign_in));

    for (mode, app) in [("local", &agent.app), ("public", &public)] {
        // What an unknown token gets: the answer every refusal must equal.
        let unknown = tool_call(app, unknown_token, LOCAL, Some("127.0.0.1"), &[]).await;
        unknown.assert_error(StatusCode::NOT_FOUND);
        let assert_hidden = |reply: Reply, what: &str| {
            assert_eq!(reply.status, unknown.status, "{mode}: {what}");
            assert_eq!(reply.body, unknown.body, "{mode}: {what}");
            assert_eq!(reply.headers, unknown.headers, "{mode}: {what}");
        };

        // Straight from loopback, with no session and no cookie: it answers.
        for peer in ["127.0.0.1", "127.0.0.9", "::1", "::ffff:127.0.0.1"] {
            let reply = tool_call(app, &agent.endpoint, LOCAL, Some(peer), &[]).await;
            assert_eq!(reply.status, StatusCode::OK, "{mode}: {peer}");
            assert_eq!(reply.headers[CACHE_CONTROL], "no-store");
        }
        // Each forwarding header alone hides it, whatever it says.
        for name in FORWARDING_HEADERS {
            for value in ["127.0.0.1", "", "for=127.0.0.1;proto=http"] {
                let headers = [(name, value)];
                let reply =
                    tool_call(app, &agent.endpoint, LOCAL, Some("127.0.0.1"), &headers).await;
                assert_hidden(reply, name);
            }
        }
        // So does a peer that is not this machine, or none at all.
        for peer in [
            Some("192.168.1.20"),
            Some("203.0.113.7"),
            Some("::ffff:10.0.0.1"),
            None,
        ] {
            let reply = tool_call(app, &agent.endpoint, LOCAL, peer, &[]).await;
            assert_hidden(reply, &format!("{peer:?}"));
        }
    }

    // In public mode a request for the public name did not start here
    // either: agents are only ever given the loopback address.
    let reply = tool_call(
        &public,
        &agent.endpoint,
        PUBLIC_HOST,
        Some("127.0.0.1"),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    // A session does not open it from outside...
    let mut browser = Browser::signed_in(&public, &provider).await;
    browser.extra = vec![header("x-forwarded-for", "203.0.113.7")];
    let reply = browser
        .post(
            &agent.endpoint,
            json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }),
        )
        .await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    // ...and the token does not open the rest of the API.
    let token = agent.endpoint.rsplit('/').next().unwrap();
    for (name, value) in [
        ("authorization", format!("Bearer {token}")),
        ("cookie", format!("{SESSION_COOKIE}={token}")),
    ] {
        let headers = [("host", LOCAL), (name, value.as_str())];
        assert_unauthorized(&plain(&public, Method::GET, "/api/v1/sessions", &headers).await);
    }
}

#[tokio::test]
async fn in_public_mode_agents_are_still_given_the_loopback_address() {
    let data = TempDir::new().unwrap();
    let (state, mut starts) = catalog::capturing_state(&data).await;
    let clock = Arc::new(ManualClock::new(START));
    let provider = TestProvider::start(clock.clone(), CLIENT_ID, Some(CLIENT_SECRET)).await;
    let sign_in = open_sign_in(&provider, &clock, &data.path().join(AUTH_FILE), ALICE).await;
    let state = state.with_sign_in(sign_in);
    let server = Server::bind("127.0.0.1:0".parse().unwrap(), state.clone())
        .await
        .unwrap();
    let addr = server.local_addr().unwrap();
    let serving = tokio::spawn(server.run(std::future::pending()));

    let request = CreateSessionRequest {
        prompt: "work".to_owned(),
        title: None,
        runner: Some("capture".to_owned()),
        model: None,
        permission_mode: None,
        workflow: None,
        repos: Vec::new(),
    };
    state.manager().create_session(request).await.unwrap();
    let (params, _io) = tokio::time::timeout(TIMEOUT, starts.recv())
        .await
        .unwrap()
        .unwrap();
    let endpoint = params.tool_endpoint.unwrap();
    assert!(
        endpoint.starts_with(&format!("http://{addr}/api/v1/mcp/")),
        "{endpoint}"
    );
    assert!(!endpoint.contains(PUBLIC_HOST));

    // Over a real connection: the agent's own call works with no session,
    // and the same call as a proxy would pass it on does not exist.
    let path = endpoint.strip_prefix(&format!("http://{addr}")).unwrap();
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#.to_owned();
    let direct = vec![header("host", addr.to_string())];
    let reply = http(addr, &Method::POST, path, &direct, Some(body.clone())).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert!(reply.text().contains("attach_repo"));
    for name in FORWARDING_HEADERS {
        let proxied = vec![
            header("host", addr.to_string()),
            header(name, "203.0.113.7"),
        ];
        let reply = http(addr, &Method::POST, path, &proxied, Some(body.clone())).await;
        assert_eq!(reply.status, StatusCode::NOT_FOUND, "{name}");
    }
    let named = vec![header("host", PUBLIC_HOST)];
    let reply = http(addr, &Method::POST, path, &named, Some(body)).await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    serving.abort();
}

// --- The guard, through the router ---

#[tokio::test]
async fn in_public_mode_the_host_must_be_the_public_one_or_loopback() {
    let public = public_mode(ALICE).await;
    let app = &public.app;
    for host in [
        PUBLIC_HOST,
        "CALIN.example.com",
        "localhost:4680",
        LOCAL,
        "[::1]:4680",
    ] {
        // Past the guard, and then asked to sign in.
        for path in ["/api/v1/me", "/api/v1/nope"] {
            let reply = plain(app, Method::GET, path, &[("host", host)]).await;
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{host} {path}");
        }
        let health = plain(app, Method::GET, "/api/v1/health", &[("host", host)]).await;
        assert_eq!(health.status, StatusCode::OK, "{host}");
        let login = plain(app, Method::GET, "/auth/login", &[("host", host)]).await;
        assert_eq!(login.status, StatusCode::SEE_OTHER, "{host}");
    }
    for host in [
        "evil.example",
        "calin.example.com.evil.example",
        "evil.calin.example.com",
        "calin.example.com:8443",
        "calin.example.com:443",
        "192.168.1.10:4680",
        "0.0.0.0:4680",
    ] {
        for path in [
            "/api/v1/health",
            "/api/v1/me",
            "/api/v1/nope",
            "/api/v1/mcp/0000",
            "/auth/login",
            "/auth/callback",
            "/auth/nope",
        ] {
            let reply = plain(app, Method::GET, path, &[("host", host)]).await;
            reply.assert_error(StatusCode::FORBIDDEN);
            assert!(reply.set_cookie_free(), "{host} {path}");
        }
        // The web app's files hold no secret and are served to any name.
        let page = plain(app, Method::GET, "/", &[("host", host)]).await;
        assert_eq!(page.status, StatusCode::OK, "{host}");
    }
    // No Host, or two.
    plain(app, Method::GET, "/api/v1/health", &[])
        .await
        .assert_error(StatusCode::FORBIDDEN);
    let two = [("host", PUBLIC_HOST), ("host", "evil.example")];
    plain(app, Method::GET, "/api/v1/health", &two)
        .await
        .assert_error(StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn in_public_mode_only_the_public_origin_changes_state_with_a_session() {
    let public = public_mode(ALICE).await;
    let app = &public.app;
    let browser = Browser::signed_in(app, &public.provider).await;
    let cookie = browser.session_cookie();
    let create = |host: &'static str, origin: Option<&'static str>| {
        let mut request = Request::builder()
            .method(Method::POST)
            .uri("/api/v1/sessions")
            .header(HOST, host)
            .header(COOKIE, &cookie)
            .header(CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            request = request.header(ORIGIN, origin);
        }
        let body = json!({ "prompt": "x", "permission_mode": "autonomous" });
        call(app, request.body(Body::from(body.to_string())).unwrap())
    };
    let count = || async {
        let headers = [("host", PUBLIC_HOST), ("cookie", cookie.as_str())];
        let reply = plain(app, Method::GET, "/api/v1/sessions", &headers).await;
        reply.json::<Vec<Session>>().len()
    };

    // (host, origin, allowed)
    let cases = [
        (PUBLIC_HOST, Some(PUBLIC_URL), true),
        // A client that is not a browser sends none.
        (PUBLIC_HOST, None, true),
        // Straight to loopback: the public origin, or a page on loopback.
        (LOCAL, Some(PUBLIC_URL), true),
        (LOCAL, Some("http://localhost:5173"), true),
        (LOCAL, Some("http://127.0.0.1:4680"), true),
        // A page on the user's machine must not drive the public site.
        (PUBLIC_HOST, Some("http://localhost:5173"), false),
        (PUBLIC_HOST, Some("http://127.0.0.1:4680"), false),
        (PUBLIC_HOST, Some("https://localhost"), false),
        (PUBLIC_HOST, Some("http://[::1]:5173"), false),
        // Other sites, a sibling, the same name over http or another port.
        (PUBLIC_HOST, Some("https://evil.example"), false),
        (PUBLIC_HOST, Some("https://evil.calin.example.com"), false),
        (
            PUBLIC_HOST,
            Some("https://calin.example.com.evil.example"),
            false,
        ),
        (PUBLIC_HOST, Some("http://calin.example.com"), false),
        (PUBLIC_HOST, Some("https://calin.example.com:8443"), false),
        (PUBLIC_HOST, Some("null"), false),
        (LOCAL, Some("https://evil.example"), false),
        (LOCAL, Some("null"), false),
    ];
    let mut expected = 0;
    for (host, origin, allowed) in cases {
        let reply = create(host, origin).await;
        if allowed {
            assert_eq!(reply.status, StatusCode::CREATED, "{host} from {origin:?}");
            expected += 1;
        } else {
            reply.assert_error(StatusCode::FORBIDDEN);
        }
        assert_eq!(count().await, expected, "{host} from {origin:?}");
    }

    // Signing out is a state change like any other: another site cannot do
    // it to the user, on the public name or through loopback.
    for (host, origin) in [
        (PUBLIC_HOST, "https://evil.example"),
        (PUBLIC_HOST, "http://localhost:5173"),
        (LOCAL, "https://evil.example"),
    ] {
        let headers = [
            ("host", host),
            ("origin", origin),
            ("cookie", cookie.as_str()),
        ];
        let reply = plain(app, Method::POST, "/auth/logout", &headers).await;
        reply.assert_error(StatusCode::FORBIDDEN);
        assert!(reply.set_cookie_free());
    }
    assert_eq!(count().await, expected, "still signed in");
    // Reads carry no risk and pass, whatever the origin.
    let read = [
        ("host", PUBLIC_HOST),
        ("origin", "https://evil.example"),
        ("cookie", cookie.as_str()),
    ];
    let reply = plain(app, Method::GET, "/api/v1/sessions", &read).await;
    assert_eq!(reply.status, StatusCode::OK);
    // Signing out is not a GET.
    let headers = [("host", PUBLIC_HOST), ("cookie", cookie.as_str())];
    let reply = plain(app, Method::GET, "/auth/logout", &headers).await;
    assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(count().await, expected, "still signed in");
}

// --- Headers ---

const CSP_LOCAL: &str = "default-src 'none'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
    connect-src 'self' ws://127.0.0.1:4680; manifest-src 'self'; worker-src 'self'; \
    frame-ancestors 'none'; base-uri 'self'; form-action 'self'";
const CSP_PUBLIC: &str = "default-src 'none'; script-src 'self'; \
    style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:; \
    connect-src 'self' wss://calin.example.com; manifest-src 'self'; worker-src 'self'; \
    frame-ancestors 'none'; base-uri 'self'; form-action 'self'";

fn assert_security_headers(reply: &Reply, policy: &str, public: bool, what: &str) {
    let one = |name: &axum::http::HeaderName| {
        let values: Vec<&str> = reply
            .headers
            .get_all(name)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(values.len(), 1, "{what}: {name} {values:?}");
        values[0].to_owned()
    };
    assert_eq!(one(&X_CONTENT_TYPE_OPTIONS), "nosniff", "{what}");
    assert_eq!(one(&REFERRER_POLICY), "same-origin", "{what}");
    assert_eq!(one(&X_FRAME_OPTIONS), "DENY", "{what}");
    assert_eq!(one(&CONTENT_SECURITY_POLICY), policy, "{what}");
    if public {
        assert_eq!(
            one(&STRICT_TRANSPORT_SECURITY),
            "max-age=31536000",
            "{what}"
        );
    } else {
        assert!(
            !reply.headers.contains_key(STRICT_TRANSPORT_SECURITY),
            "{what}"
        );
    }
}

/// What each kind of answer must say about caching.
async fn assert_headers_everywhere(app: &Router, host: &str, policy: &str, public: bool) {
    // (method, path, the Cache-Control it must carry)
    let cases = [
        (Method::GET, "/api/v1/health", "no-store"),
        (Method::GET, "/api/v1/me", "no-store"),
        (Method::GET, "/api/v1/sessions", "no-store"),
        (Method::GET, "/api/v1/nope", "no-store"),
        (Method::POST, "/api/v1/sessions", "no-store"),
        (Method::DELETE, "/api/v1/sessions", "no-store"),
        (Method::POST, "/api/v1/mcp/0000", "no-store"),
        (Method::GET, "/api/v1/ws", "no-store"),
        (Method::GET, "/auth/login", "no-store"),
        (Method::GET, "/auth/callback?code=x&state=y", "no-store"),
        (Method::POST, "/auth/logout", "no-store"),
        (Method::GET, "/auth/nope", "no-store"),
        (Method::GET, "/", "no-cache"),
        (Method::GET, "/index.html", "no-cache"),
        (Method::GET, "/sessions/abc", "no-cache"),
        (Method::GET, "/favicon.svg", "no-cache"),
        (
            Method::GET,
            "/assets/app-abc123.js",
            "public, max-age=31536000, immutable",
        ),
    ];
    for (method, path, cache) in cases {
        let reply = plain(app, method.clone(), path, &[("host", host)]).await;
        let what = format!("{method} {path} ({})", reply.status);
        assert_security_headers(&reply, policy, public, &what);
        assert_eq!(reply.headers[CACHE_CONTROL], cache, "{what}");
    }
    // A refusal by the guard carries them as well.
    let refused = plain(
        app,
        Method::GET,
        "/api/v1/health",
        &[("host", "evil.example")],
    )
    .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let bare = policy
        .replace(" ws://127.0.0.1:4680", "")
        .replace(" wss://calin.example.com", "");
    assert_security_headers(&refused, &bare, public, "a refused host");
    assert_eq!(refused.headers[CACHE_CONTROL], "no-store");
    // A name the server does not answer to never reaches the policy.
    let hostile = "evil.example; script-src *";
    let page = plain(app, Method::GET, "/", &[("host", hostile)]).await;
    assert_security_headers(&page, &bare, public, "a hostile host");
}

#[tokio::test]
async fn every_response_carries_the_security_headers_in_local_mode() {
    let dir = TempDir::new().unwrap();
    let app = router(state(&dir, true).await.with_assets(Assets::new(WEB_APP)));
    assert_headers_everywhere(&app, LOCAL, CSP_LOCAL, false).await;
}

#[tokio::test]
async fn every_response_carries_the_security_headers_and_hsts_in_public_mode() {
    let public = public_mode(ALICE).await;
    assert_headers_everywhere(&public.app, PUBLIC_HOST, CSP_PUBLIC, true).await;
    // Reached on loopback, the page's sockets are on loopback too.
    assert_headers_everywhere(&public.app, LOCAL, CSP_LOCAL, true).await;

    // Signed in, on answers that succeed.
    let mut browser = Browser::signed_in(&public.app, &public.provider).await;
    for path in [
        "/api/v1/me",
        "/api/v1/sessions",
        "/api/v1/tokens",
        "/api/v1/health",
    ] {
        let reply = browser.get(path).await;
        assert_eq!(reply.status, StatusCode::OK, "{path}");
        assert_security_headers(&reply, CSP_PUBLIC, true, path);
        assert_eq!(reply.headers[CACHE_CONTROL], "no-store", "{path}");
    }
    // The answer that shows a secret, and the redirects of sign-in.
    let created = browser
        .post("/api/v1/tokens", json!({ "name": "ci" }))
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_security_headers(&created, CSP_PUBLIC, true, "a new token");
    assert_eq!(created.headers[CACHE_CONTROL], "no-store");
    let mut second = Browser::new(&public.app);
    let redirect = second.begin(None).await;
    assert_security_headers(&redirect, CSP_PUBLIC, true, "the login redirect");
    let query = public.provider.authorize(location(&redirect)).await;
    let back = second.get(&format!("/auth/callback?{query}")).await;
    assert_eq!(back.status, StatusCode::SEE_OTHER);
    assert_security_headers(&back, CSP_PUBLIC, true, "the callback");
    assert_eq!(back.headers[CACHE_CONTROL], "no-store");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_websocket_upgrade_carries_the_security_headers() {
    let public = public_mode(ALICE).await;
    let serving = serve_public(&public).await;
    let browser = Browser::signed_in(&public.app, &public.provider).await;
    let cookie = browser.session_cookie();
    let mut request = format!("ws://{}/api/v1/ws", serving.addr)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(COOKIE, cookie.parse().unwrap());
    let (_client, response) = tokio_tungstenite::connect_async(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(response.headers()[X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(response.headers()[X_FRAME_OPTIONS], "DENY");
    assert_eq!(
        response.headers()[STRICT_TRANSPORT_SECURITY],
        "max-age=31536000"
    );
    assert!(response.headers().contains_key(CONTENT_SECURITY_POLICY));
    serving.stop.send(()).unwrap();
    serving.server.await.unwrap().unwrap();
}

// --- The log ---

/// Collects what `tracing` writes while it is the default subscriber.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

/// The log of this whole test process from now on. A subscriber scoped to
/// one test would miss events: `tracing` caches, per call site, whether
/// anyone listens, and a test on another thread can fill that cache first.
fn capture_the_log() -> Captured {
    static LOG: std::sync::OnceLock<Captured> = std::sync::OnceLock::new();
    LOG.get_or_init(|| {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(captured.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("no other test installs a subscriber");
        captured
    })
    .clone()
}

/// An identity no other test signs in, so its lines can be told apart in a
/// log that every test of this process writes to.
const CAROL: &str = "carol.logtest@example.com";

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nothing_secret_reaches_the_log_during_a_whole_sign_in() {
    let captured = capture_the_log();

    let public = public_mode(CAROL).await;
    public.provider.set_user("user-3", Some(CAROL), true);
    let serving = serve_public(&public).await;
    let mut secrets: Vec<(&str, String)> = vec![("the client secret", CLIENT_SECRET.to_owned())];
    let mut browser = Browser::over(serving.addr);
    browser.extra = vec![header("x-forwarded-for", "203.0.113.99")];

    // A sign-in that fails first, with a query worth recognizing.
    let marker = "QUERY-MARKER-51c2";
    let redirect = browser.begin(Some("/sessions/NEXT-MARKER-77")).await;
    secrets.push((
        "a pending-login cookie",
        browser.jar.value(LOGIN_COOKIE).unwrap().to_owned(),
    ));
    let failed = browser
        .get(&format!(
            "/auth/callback?state={marker}&code={marker}&x={marker}"
        ))
        .await;
    assert_eq!(failed.status, StatusCode::BAD_REQUEST);
    secrets.push(("a callback query", marker.to_owned()));
    drop(redirect);

    // Then the real thing.
    let redirect = browser.begin(Some("/sessions/NEXT-MARKER-77")).await;
    let authorization = url::Url::parse(location(&redirect)).unwrap();
    for (name, value) in authorization.query_pairs() {
        if matches!(name.as_ref(), "state" | "nonce" | "code_challenge") {
            secrets.push(("an authorization parameter", value.into_owned()));
        }
    }
    secrets.push((
        "the pending-login cookie",
        browser.jar.value(LOGIN_COOKIE).unwrap().to_owned(),
    ));
    let query = public.provider.authorize(location(&redirect)).await;
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        if name == "code" {
            secrets.push(("the authorization code", value.into_owned()));
        }
    }
    let back = browser.get(&format!("/auth/callback?{query}")).await;
    assert_eq!(back.status, StatusCode::SEE_OTHER);
    secrets.push((
        "the session token",
        browser.jar.value(SESSION_COOKIE).unwrap().to_owned(),
    ));
    secrets.push(("the ID token", public.provider.last_id_token().unwrap()));
    secrets.push((
        "the PKCE verifier",
        public.provider.last_code_verifier().unwrap(),
    ));
    secrets.push(("the login's next", "NEXT-MARKER-77".to_owned()));

    // Use it: the API, a device token, both sockets, the daily re-issue.
    assert_eq!(browser.me().await.status, StatusCode::OK);
    let token = browser.create_token("log test").await;
    secrets.push(("the device token", token.token.clone()));
    secrets.push((
        "the device token's secret part",
        token.token["calin_".len()..].to_owned(),
    ));
    let bearer = format!("Bearer {}", token.token);
    let session_id = new_session(&public.state, "logged").await;
    in_review(&public.state, &session_id).await;
    let cookie = browser.session_cookie();
    let mut events = handshake(serving.addr, "/api/v1/ws", &[("cookie", cookie.as_str())])
        .await
        .unwrap();
    let mut terminal = handshake(
        serving.addr,
        &terminal_path(&session_id),
        &[("authorization", bearer.as_str())],
    )
    .await
    .unwrap();
    public.clock.advance(DAY);
    assert!(!browser.me().await.set_cookie_free());
    // Refusals: bad credentials of each kind, a foreign origin, a foreign host.
    let bad_cookie = "C".repeat(43);
    let bad_token = format!("calin_{}", "D".repeat(43));
    secrets.push(("a refused cookie", bad_cookie.clone()));
    secrets.push(("a refused token", bad_token.clone()));
    let addr = serving.addr;
    let host = addr.to_string();
    for headers in [
        vec![
            header("host", &host),
            header("cookie", format!("{SESSION_COOKIE}={bad_cookie}")),
        ],
        vec![
            header("host", &host),
            header("authorization", format!("Bearer {bad_token}")),
        ],
        vec![
            header("host", &host),
            header("authorization", bearer.clone()),
            header("origin", "https://evil.example"),
        ],
        vec![
            header("host", "evil.example"),
            header("cookie", cookie.clone()),
        ],
    ] {
        let reply = http(addr, &Method::POST, "/api/v1/sessions", &headers, None).await;
        assert!(reply.status.is_client_error(), "{}", reply.status);
    }
    let revoked = browser
        .delete(&format!("/api/v1/tokens/{}", token.info.id))
        .await;
    assert_eq!(revoked.status, StatusCode::NO_CONTENT);
    closed_by_the_server(&mut terminal).await;
    let out = browser.request(Method::POST, "/auth/logout", None).await;
    assert_eq!(out.status, StatusCode::NO_CONTENT);
    closed_by_the_server(&mut events).await;
    serving.stop.send(()).unwrap();
    serving.server.await.unwrap().unwrap();

    let log = captured.text();
    // The lines that hold `needle`, for a failure message of readable size.
    let lines_with =
        |needle: &str| -> Vec<&str> { log.lines().filter(|line| line.contains(needle)).collect() };
    // The capture works, and says what the spec wants said, once.
    for expected in [
        format!("signed in identity={CAROL}"),
        format!("signed out identity={CAROL}"),
    ] {
        assert_eq!(lines_with(&expected).len(), 1, "{expected:?}");
    }
    assert!(log.contains("sign-in failed reason=the callback's state is missing"));
    for (what, secret) in &secrets {
        assert!(secret.len() >= 12, "{what} is too short to look for");
        let found = lines_with(secret);
        assert!(found.is_empty(), "{what} is in the log: {found:?}");
    }
    // No header value of either kind, and no query string of sign-in.
    for name in [
        SESSION_COOKIE,
        LOGIN_COOKIE,
        "Bearer ",
        "/auth/callback?",
        "/auth/login?",
    ] {
        let found = lines_with(name);
        assert!(found.is_empty(), "{name:?} is in the log: {found:?}");
    }
    assert_eq!(secrets.len(), 16);
}
