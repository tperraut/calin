//! The HTTP side of Calin: REST under `/api/v1`, the event WebSocket, the
//! terminal socket and the embedded web app. Handlers stay thin and call
//! `calin-core`.

mod account;
mod api;
mod assets;
mod auth;
mod guard;
mod headers;
mod mcp;
mod notify;
mod push;
mod term;
mod ws;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::handler::Handler;
use axum::http::Method;
use axum::routing::{MethodFilter, MethodRouter, get, on, post};
use calin_core::SessionManager;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub use assets::Assets;
pub use auth::{AUTH_FILE, SignIn, SignInError};
pub use push::{PUSH_KEY_FILE, PUSH_SUBSCRIPTIONS_FILE, Push, PushError, PushSetup, vapid_subject};

/// Where agents reach Calin's tools, under [`API_ROOT`]: the token of one
/// agent follows.
const MCP_PATH: &str = "/mcp";
const MCP_ROUTE: &str = "/mcp/{token}";
const HEALTH_ROUTE: &str = "/health";
const API_ROOT: &str = "/api/v1";
/// The two routes sign-in treats differently, as full paths. A test holds
/// them to the constants above.
const API_HEALTH: &str = "/api/v1/health";
const API_MCP_PREFIX: &str = "/api/v1/mcp/";

/// What the handlers share.
#[derive(Clone)]
pub struct AppState {
    manager: SessionManager,
    assets: Assets,
    /// Cancelled on shutdown so open WebSockets end and the server can drain.
    shutdown: CancellationToken,
    /// What a terminal runs; the user's interactive shell when `None`.
    terminal_command: Option<Arc<term::TerminalCommand>>,
    terminals: term::Terminals,
    /// Public mode. `None` is local mode: loopback only, nobody signs in.
    sign_in: Option<Arc<SignIn>>,
    /// Web Push, in public mode. `None` in local mode, where it does not exist.
    push: Option<Arc<Push>>,
    /// Who has which session open on an event socket.
    viewers: push::Viewers,
}

impl AppState {
    /// A state serving the web app that was embedded at compile time, in
    /// local mode.
    pub fn new(manager: SessionManager) -> Self {
        Self {
            manager,
            assets: Assets::embedded(),
            shutdown: CancellationToken::new(),
            terminal_command: None,
            terminals: term::Terminals::default(),
            sign_in: None,
            push: None,
            viewers: push::Viewers::default(),
        }
    }

    /// Runs `program` with `args` in every terminal instead of the user's
    /// shell. Tests use it to get a shell that reads no profile.
    pub fn with_terminal_command<I, S>(mut self, program: impl Into<OsString>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.terminal_command = Some(Arc::new(term::TerminalCommand {
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
        }));
        self
    }

    /// Serves `assets` instead of the embedded web app.
    pub fn with_assets(mut self, assets: Assets) -> Self {
        self.assets = assets;
        self
    }

    /// Public mode: every request to the API must be signed in.
    pub fn with_sign_in(mut self, sign_in: Arc<SignIn>) -> Self {
        self.sign_in = Some(sign_in);
        self
    }

    /// Web Push: the `/push` routes and the notifications. Public mode only;
    /// without sign-in the routes still answer 409.
    pub fn with_push(mut self, push: Arc<Push>) -> Self {
        self.push = Some(push);
        self
    }

    pub fn manager(&self) -> &SessionManager {
        &self.manager
    }

    fn public_site(&self) -> Option<guard::PublicSite<'_>> {
        self.sign_in.as_deref().map(SignIn::site)
    }
}

/// Who may call a route in public mode. Local mode asks nobody anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Access {
    /// A browser session or a device token.
    SignedIn,
    /// `GET /health` answers everyone, and says less to a stranger.
    Anyone,
    /// The tool endpoint: its token is the credential, and it answers only
    /// to this machine.
    AgentToken,
}

/// The methods the API uses.
#[derive(Clone, Copy)]
enum Verb {
    Get,
    Post,
    Put,
    Delete,
}

/// One route of the API: a row of the table in `calin-proto/src/api.rs`.
struct ApiRoute {
    /// Read by the tests that walk the table.
    #[cfg_attr(not(test), allow(dead_code))]
    method: Method,
    /// Relative to [`API_ROOT`].
    path: &'static str,
    access: Access,
    handler: MethodRouter<AppState>,
}

fn route<H, T>(verb: Verb, path: &'static str, access: Access, handler: H) -> ApiRoute
where
    H: Handler<T, AppState>,
    T: 'static,
{
    let (method, filter) = match verb {
        Verb::Get => (Method::GET, MethodFilter::GET),
        Verb::Post => (Method::POST, MethodFilter::POST),
        Verb::Put => (Method::PUT, MethodFilter::PUT),
        Verb::Delete => (Method::DELETE, MethodFilter::DELETE),
    };
    ApiRoute {
        method,
        path,
        access,
        handler: on(filter, handler),
    }
}

/// Every route under [`API_ROOT`], in the order of the contract's table.
/// The router is built from this list and from nothing else, and the tests
/// walk the same list: a route added here is checked for sign-in, and a
/// route cannot exist without being here.
fn api_routes() -> Vec<ApiRoute> {
    use Access::{AgentToken, Anyone, SignedIn};
    use Verb::{Delete, Get, Post, Put};
    const SESSION: &str = "/sessions/{id}";
    vec![
        route(Get, HEALTH_ROUTE, Anyone, api::health),
        route(Get, "/me", SignedIn, account::me),
        route(Get, "/auth/sessions", SignedIn, account::list_auth_sessions),
        route(
            Delete,
            "/auth/sessions/{id}",
            SignedIn,
            account::revoke_auth_session,
        ),
        route(Get, "/tokens", SignedIn, account::list_device_tokens),
        route(Post, "/tokens", SignedIn, account::create_device_token),
        route(
            Delete,
            "/tokens/{id}",
            SignedIn,
            account::revoke_device_token,
        ),
        route(Get, "/push/key", SignedIn, push::key),
        route(
            Get,
            "/push/subscriptions",
            SignedIn,
            push::list_subscriptions,
        ),
        route(Post, "/push/subscriptions", SignedIn, push::subscribe),
        route(
            Delete,
            "/push/subscriptions/{id}",
            SignedIn,
            push::unsubscribe,
        ),
        route(Get, "/push/preferences", SignedIn, push::get_preferences),
        route(Put, "/push/preferences", SignedIn, push::set_preferences),
        route(Post, "/push/test", SignedIn, push::test),
        route(Get, "/settings", SignedIn, api::get_settings),
        route(Put, "/settings", SignedIn, api::update_settings),
        route(Get, "/repos", SignedIn, api::list_repos),
        route(Post, "/repos", SignedIn, api::register_repo),
        route(Post, "/repos/refresh", SignedIn, api::refresh_repos),
        route(Post, "/repos/{id}/clone", SignedIn, api::clone_repo),
        route(Delete, "/repos/{id}", SignedIn, api::remove_repo),
        route(Get, "/repo-sets", SignedIn, api::list_repo_sets),
        route(Get, "/workflows", SignedIn, api::list_workflows),
        route(Get, "/sessions", SignedIn, api::list_sessions),
        route(Post, "/sessions", SignedIn, api::create_session),
        route(Get, SESSION, SignedIn, api::get_session),
        route(Post, "/sessions/{id}/messages", SignedIn, api::send_message),
        route(
            Post,
            "/sessions/{id}/interrupt",
            SignedIn,
            api::interrupt_session,
        ),
        route(Post, "/sessions/{id}/stop", SignedIn, api::stop_session),
        route(
            Post,
            "/sessions/{id}/permissions/{request_id}",
            SignedIn,
            api::resolve_permission,
        ),
        route(
            Post,
            "/sessions/{id}/questions/{question_id}",
            SignedIn,
            api::answer_question,
        ),
        route(Get, "/sessions/{id}/graph", SignedIn, api::session_graph),
        route(
            Get,
            "/sessions/{id}/timeline",
            SignedIn,
            api::session_timeline,
        ),
        route(
            Get,
            "/sessions/{id}/workflow",
            SignedIn,
            api::session_workflow,
        ),
        route(Post, "/sessions/{id}/repos", SignedIn, api::attach_repo),
        route(Post, "/sessions/{id}/ship", SignedIn, api::ship_session),
        route(
            Post,
            "/sessions/{id}/pull-requests/refresh",
            SignedIn,
            api::refresh_pull_requests,
        ),
        route(
            Get,
            "/sessions/{id}/changes",
            SignedIn,
            api::session_changes,
        ),
        route(
            Get,
            "/sessions/{id}/repos/{session_repo_id}/diff",
            SignedIn,
            api::file_diff,
        ),
        route(
            Get,
            "/sessions/{id}/repos/{session_repo_id}/tree",
            SignedIn,
            api::tree,
        ),
        route(
            Get,
            "/sessions/{id}/repos/{session_repo_id}/file",
            SignedIn,
            api::read_file,
        ),
        route(
            Put,
            "/sessions/{id}/repos/{session_repo_id}/file",
            SignedIn,
            api::write_file,
        ),
        route(Get, "/sessions/{id}/terminals/ws", SignedIn, term::upgrade),
        route(Get, "/ws", SignedIn, ws::upgrade),
        route(Post, MCP_ROUTE, AgentToken, mcp::call),
    ]
}

/// The whole application as a router, for `serve` and for tests that drive
/// it without a socket.
pub fn router(state: AppState) -> Router {
    let (mut signed_in, mut open) = (Router::new(), Router::new());
    for route in api_routes() {
        match route.access {
            Access::SignedIn => signed_in = signed_in.route(route.path, route.handler),
            Access::Anyone | Access::AgentToken => open = open.route(route.path, route.handler),
        }
    }
    // The second lock on these routes; `auth::authenticate` below is the first.
    let api = open.merge(signed_in.route_layer(axum::middleware::from_fn(auth::require_caller)));
    Router::new()
        .nest(API_ROOT, api)
        // Outside `/api/v1` because a browser navigates to them. 404 in local mode.
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        // Everything else: a JSON 404 under `/api`, the web app otherwise.
        .fallback(assets::serve)
        .method_not_allowed_fallback(api::method_not_allowed)
        // Outermost last: the headers go on every answer, the guard refuses
        // before anyone is asked who they are.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            guard::guard,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            headers::security_headers,
        ))
        .with_state(state)
}

/// A bound listener that has not started serving yet, so the caller can
/// report the address before blocking on [`Server::run`].
pub struct Server {
    listener: TcpListener,
    state: AppState,
}

impl Server {
    pub async fn bind(addr: SocketAddr, state: AppState) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr).await?;
        // Only now is the port known, and with it the address agents are
        // given for Calin's tools. They run on this machine: loopback it is.
        let bound = listener.local_addr()?;
        state
            .manager
            .set_tool_endpoint(format!("http://{bound}{API_ROOT}{MCP_PATH}"));
        Ok(Self { listener, state })
    }

    /// The address actually bound, which matters when the port was 0.
    pub fn local_addr(&self) -> std::io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Serves until `shutdown` completes, then closes the WebSockets and
    /// lets requests in flight finish.
    pub async fn run(
        self,
        shutdown: impl Future<Output = ()> + Send + 'static,
    ) -> std::io::Result<()> {
        let sockets = self.state.shutdown.clone();
        let sign_in = self.state.sign_in.clone();
        let push = self.state.push.clone();
        // Ends with `sockets`, when the server shuts down.
        let notifications = notify::spawn(&self.state, None);
        // With connection info: the tool endpoint and the sign-in rate limit
        // need the peer's address.
        let app = router(self.state).into_make_service_with_connect_info::<SocketAddr>();
        let served = axum::serve(self.listener, app)
            .with_graceful_shutdown(async move {
                shutdown.await;
                sockets.cancel();
            })
            .await;
        if let Some(notifications) = notifications {
            let _ = notifications.await;
        }
        // "Last used" and "last sent" times reach their files at most every
        // 30 seconds otherwise.
        if let Some(push) = push {
            push.flush().await;
        }
        if let Some(sign_in) = sign_in {
            sign_in.flush().await;
        }
        served
    }
}

/// Binds `addr` and serves until `shutdown` completes.
pub async fn serve(
    addr: SocketAddr,
    state: AppState,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    Server::bind(addr, state).await?.run(shutdown).await
}
