//! The HTTP side of Calin: REST under `/api/v1`, the event WebSocket, the
//! terminal socket and the embedded web app. Handlers stay thin and call
//! `calin-core`.

mod api;
mod assets;
mod guard;
mod term;
mod ws;

#[cfg(test)]
mod tests;

use std::ffi::OsString;
use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::routing::{delete, get, post};
use calin_core::SessionManager;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

pub use assets::Assets;

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
}

impl AppState {
    /// A state serving the web app that was embedded at compile time.
    pub fn new(manager: SessionManager) -> Self {
        Self {
            manager,
            assets: Assets::embedded(),
            shutdown: CancellationToken::new(),
            terminal_command: None,
            terminals: term::Terminals::default(),
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

    pub fn manager(&self) -> &SessionManager {
        &self.manager
    }
}

/// The whole application as a router, for `serve` and for tests that drive
/// it without a socket.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(api::health))
        .route("/repos", get(api::list_repos).post(api::register_repo))
        .route("/repos/{id}", delete(api::remove_repo))
        .route(
            "/sessions",
            get(api::list_sessions).post(api::create_session),
        )
        .route("/sessions/{id}", get(api::get_session))
        .route("/sessions/{id}/messages", post(api::send_message))
        .route("/sessions/{id}/interrupt", post(api::interrupt_session))
        .route("/sessions/{id}/stop", post(api::stop_session))
        .route(
            "/sessions/{id}/permissions/{request_id}",
            post(api::resolve_permission),
        )
        .route("/sessions/{id}/changes", get(api::session_changes))
        .route(
            "/sessions/{id}/repos/{session_repo_id}/diff",
            get(api::file_diff),
        )
        .route(
            "/sessions/{id}/repos/{session_repo_id}/tree",
            get(api::tree),
        )
        .route(
            "/sessions/{id}/repos/{session_repo_id}/file",
            get(api::read_file).put(api::write_file),
        )
        .route("/sessions/{id}/terminals/ws", get(term::upgrade))
        .route("/ws", get(ws::upgrade));
    Router::new()
        .nest("/api/v1", api)
        // Everything else: a JSON 404 under `/api`, the web app otherwise.
        .fallback(assets::serve)
        .method_not_allowed_fallback(api::method_not_allowed)
        .layer(axum::middleware::from_fn(guard::guard))
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
        axum::serve(self.listener, router(self.state))
            .with_graceful_shutdown(async move {
                shutdown.await;
                sockets.cancel();
            })
            .await
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
