//! Sign-in over HTTP: who a request is, and the three routes a browser
//! navigates to (`/auth/login`, `/auth/callback`, `/auth/logout`). The rules
//! are sections 1 to 4 of `docs/specs/2026-11-27-remote-access.md`; every
//! decision about a secret is `calin-auth`'s, and this module only carries
//! header values to it and its answers back.
//!
//! Nothing here logs a header value, a query string or a token.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{
    AUTHORIZATION, CONTENT_TYPE, COOKIE, HeaderName, LOCATION, SET_COOKIE, USER_AGENT,
    WWW_AUTHENTICATE,
};
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use calin_auth::{
    AuthStore, Authenticated, CallbackParams, Clock, Credential, LoadedFile, LoginError,
    OidcClient, OidcSetupError, PendingLoginRef, PublicConfig, SESSION_COOKIE, SessionToken,
    StoreError, client_address,
};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::api::ApiFailure;
use crate::guard::{PublicSite, is_api_path};
use crate::{API_HEALTH, API_MCP_PREFIX, AppState};

/// The file under the data directory that holds sessions and device tokens.
pub const AUTH_FILE: &str = "auth.json";

/// An open socket checks at least this often that the credential it was
/// opened with still exists; a revocation made by this server wakes it at once.
const SOCKET_RECHECK: Duration = Duration::from_secs(60);

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");

/// Everything public mode adds to the server: the configuration, the
/// OpenID Connect client and the store of sessions and device tokens.
pub struct SignIn {
    config: PublicConfig,
    oidc: OidcClient,
    store: AuthStore,
    /// Bumped when a session or a token may have ended, so that the sockets
    /// opened with one look again.
    ended: watch::Sender<u64>,
}

/// Public mode could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum SignInError {
    #[error(transparent)]
    Client(#[from] OidcSetupError),
    #[error("cannot open the sign-in store: {0}")]
    Store(#[from] StoreError),
}

impl SignIn {
    /// Builds the provider client and opens the store in `auth_file`
    /// (`<home>/auth.json`). Contacts nobody: see [`SignIn::warm_up`].
    pub async fn open(
        config: PublicConfig,
        auth_file: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, SignInError> {
        let oidc = OidcClient::new(&config, clock.clone())?;
        let store = AuthStore::open(auth_file, config.allow_list().clone(), clock).await?;
        let report = store.load_report();
        match &report.file {
            LoadedFile::Corrupt { .. } => tracing::warn!(
                ?report,
                "the sign-in store could not be read and starts empty: everyone signs in again"
            ),
            _ => tracing::info!(?report, "sign-in store opened"),
        }
        Ok(Self {
            config,
            oidc,
            store,
            ended: watch::Sender::new(0),
        })
    }

    /// Fetches the provider's metadata and keys. A provider that is down
    /// delays sign-in, never startup: the error is logged and logins retry.
    pub async fn warm_up(&self) {
        if let Err(error) = self.oidc.warm_up().await {
            tracing::warn!(%error, "the sign-in provider is not reachable yet; sign-in will retry");
        }
    }

    /// Writes what the store still owes its file. For shutdown.
    pub async fn flush(&self) {
        if let Err(error) = self.store.flush().await {
            tracing::error!(%error, "could not write the sign-in store on shutdown");
        }
    }

    pub(crate) fn site(&self) -> PublicSite<'_> {
        PublicSite {
            origin: self.config.public_origin(),
            authority: self.config.public_authority(),
        }
    }

    /// Everyone who may sign in, as the stores key identities.
    pub(crate) fn allowed_identities(&self) -> Vec<String> {
        self.config
            .allow_list()
            .identities()
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    pub(crate) fn store(&self) -> &AuthStore {
        &self.store
    }

    /// Call after anything that may have ended a session or a token.
    pub(crate) fn credentials_ended(&self) {
        self.ended
            .send_modify(|count| *count = count.wrapping_add(1));
    }

    fn still_holds(&self, who: &Authenticated) -> bool {
        match &who.credential {
            Credential::Session { id } => self
                .store
                .list_sessions(&who.identity)
                .iter()
                .any(|session| &session.id == id),
            Credential::DeviceToken { id } => self
                .store
                .list_device_tokens(&who.identity)
                .iter()
                .any(|token| &token.id == id),
        }
    }
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignIn")
            .field("origin", &self.config.public_origin())
            .finish_non_exhaustive()
    }
}

/// Who a request to the API is, as the middleware established it.
#[derive(Clone, Debug)]
pub(crate) enum Caller {
    /// Local mode: nobody signs in.
    Local,
    /// Public mode, no credentials. Only `GET /health` gets this far.
    Anonymous,
    /// Public mode, with a session or a device token.
    SignedIn(Authenticated),
}

/// Every value of a header, as text. A value that is not text is passed on
/// empty rather than dropped, so it still counts as a header that was sent.
pub(crate) fn header_values(headers: &HeaderMap, name: HeaderName) -> Vec<&str> {
    headers
        .get_all(name)
        .iter()
        .map(|value| value.to_str().unwrap_or(""))
        .collect()
}

/// The address of the TCP peer, which exists when the server was started
/// with connection info ([`crate::Server::run`] always does).
pub(crate) fn peer_ip(extensions: &Extensions) -> Option<IpAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| peer.ip())
}

/// The path of the tool endpoint with a token in it and nothing after.
fn is_tool_endpoint(path: &str) -> bool {
    path.strip_prefix(API_MCP_PREFIX)
        .is_some_and(|token| !token.is_empty() && !token.contains('/'))
}

fn unauthorized() -> Response {
    let mut response =
        ApiFailure::new(StatusCode::UNAUTHORIZED, "sign-in is required").into_response();
    response
        .headers_mut()
        .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    response
}

fn sets_session_cookie(response: &Response) -> bool {
    response
        .headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|value| value.starts_with(SESSION_COOKIE))
}

/// A `Set-Cookie` value, kept out of `Debug` output.
fn cookie_header(value: &str) -> Option<HeaderValue> {
    let mut value = HeaderValue::from_str(value).ok()?;
    value.set_sensitive(true);
    Some(value)
}

/// Establishes the [`Caller`] of every request under `/api`. In public mode
/// that is where a request without credentials ends, with a 401: whatever
/// the path, known or not, except `GET /health` and the tool endpoint,
/// whose token is its credential. The `/auth` routes and the web app's
/// files are outside `/api` and pass.
pub(crate) async fn authenticate(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(sign_in) = state.sign_in.clone() else {
        request.extensions_mut().insert(Caller::Local);
        return next.run(request).await;
    };
    let path = request.uri().path();
    if !is_api_path(path) || is_tool_endpoint(path) {
        return next.run(request).await;
    }
    let who = {
        let cookies = header_values(request.headers(), COOKIE);
        let authorizations = header_values(request.headers(), AUTHORIZATION);
        sign_in
            .store
            .authenticate_request(cookies, authorizations)
            .await
    };
    let caller = match who {
        Some(who) => Caller::SignedIn(who),
        None if path == API_HEALTH && matches!(*request.method(), Method::GET | Method::HEAD) => {
            Caller::Anonymous
        }
        None => return unauthorized(),
    };
    // The daily re-issue of the session cookie (spec section 3).
    let reissue = match &caller {
        Caller::SignedIn(who) => who.set_cookie().and_then(cookie_header),
        _ => None,
    };
    request.extensions_mut().insert(caller);
    let mut response = next.run(request).await;
    // Not over a handler's own word on the cookie: revoking the current
    // session clears it.
    if let Some(cookie) = reissue
        && !sets_session_cookie(&response)
    {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
    response
}

/// On every route that needs a signed-in caller: refuses unless
/// [`authenticate`] ran and found one. It always has; this is the second
/// lock, so that a route cannot be reached past a mistake in the first.
pub(crate) async fn require_caller(request: Request, next: Next) -> Response {
    match request.extensions().get::<Caller>() {
        Some(Caller::Local | Caller::SignedIn(_)) => next.run(request).await,
        Some(Caller::Anonymous) | None => unauthorized(),
    }
}

/// Ends a socket when the credential it was opened with ends: cancels
/// `closing` once the session or token of `caller` is gone (signed out,
/// revoked or expired). A terminal is a shell; it must not outlive the
/// sign-in that opened it.
pub(crate) fn close_with_credential(
    state: &AppState,
    caller: &Caller,
    closing: &CancellationToken,
) {
    let (Some(sign_in), Caller::SignedIn(who)) = (state.sign_in.clone(), caller) else {
        return;
    };
    let (who, closing) = (who.clone(), closing.clone());
    tokio::spawn(async move {
        // Subscribed before the first look, so no revocation falls between.
        let mut ended = sign_in.ended.subscribe();
        loop {
            if !sign_in.still_holds(&who) {
                closing.cancel();
                return;
            }
            tokio::select! {
                () = closing.cancelled() => return,
                // Never an error: this task keeps the sender alive through `sign_in`.
                _ = ended.changed() => {}
                () = tokio::time::sleep(SOCKET_RECHECK) => {}
            }
        }
    });
}

// --- The routes a browser navigates to ---

const UNAVAILABLE_PAGE: &str = "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Calin</title>
</head>
<body>
<h1>Sign-in is temporarily unavailable</h1>
<p>Please try again in a moment.</p>
<p><a href=\"/\">Back to Calin</a></p>
</body>
</html>
";

fn html_page(status: StatusCode, body: String) -> Response {
    (status, [(CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

/// The one page every failed sign-in gets. Static: nothing of the request
/// is in it, and nothing that says which check failed.
fn failure_page(status: StatusCode) -> Response {
    let body = format!(
        "<!doctype html>
<html lang=\"en\">
<head>
<meta charset=\"utf-8\">
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">
<title>Calin</title>
</head>
<body>
<h1>{}</h1>
<p><a href=\"/\">Back to Calin</a></p>
</body>
</html>
",
        LoginError::PUBLIC_MESSAGE
    );
    html_page(status, body)
}

/// What `/auth/*` answers in local mode, and for anything else under `/auth`.
pub(crate) fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "not found").into_response()
}

fn server_error() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response()
}

/// A 303 to `location` that sets `cookie`.
fn see_other(location: &str, cookie: &str) -> Option<Response> {
    let mut location = HeaderValue::from_str(location).ok()?;
    // The authorization URL carries the state and the nonce.
    location.set_sensitive(true);
    let mut response = StatusCode::SEE_OTHER.into_response();
    response.headers_mut().insert(LOCATION, location);
    response
        .headers_mut()
        .append(SET_COOKIE, cookie_header(cookie)?);
    Some(response)
}

/// The address failed callbacks are counted under (spec section 3).
fn client_of(request: &Parts) -> Option<IpAddr> {
    let peer = peer_ip(&request.extensions)?;
    let forwarded = header_values(&request.headers, X_FORWARDED_FOR);
    Some(client_address(peer, forwarded.iter().copied()))
}

/// The `next` parameter, decoded. Given twice it is not given: choosing one
/// would be a guess.
fn next_parameter(raw_query: &str) -> Option<String> {
    let mut values = url::form_urlencoded::parse(raw_query.as_bytes())
        .filter(|(name, _)| name == "next")
        .map(|(_, value)| value.into_owned());
    match (values.next(), values.next()) {
        (Some(next), None) => Some(next),
        _ => None,
    }
}

/// `GET /auth/login?next=`: to the provider, with the pending-login cookie.
pub(crate) async fn login(State(state): State<AppState>, request: Parts) -> Response {
    let Some(sign_in) = state.sign_in.as_deref() else {
        return not_found();
    };
    let Some(client) = client_of(&request) else {
        tracing::error!("a request arrived without its peer address");
        return server_error();
    };
    let next = next_parameter(request.uri.query().unwrap_or(""));
    match sign_in.oidc.begin_login(client, next.as_deref()).await {
        Ok(start) => {
            see_other(start.authorization_url(), start.set_cookie()).unwrap_or_else(server_error)
        }
        Err(error) => {
            tracing::warn!(reason = %error, "sign-in cannot start");
            html_page(StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE_PAGE.to_owned())
        }
    }
}

/// `GET /auth/callback`: finishes the login. Whatever happens, the
/// pending-login cookie is cleared and the query is never echoed.
pub(crate) async fn callback(State(state): State<AppState>, request: Parts) -> Response {
    let Some(sign_in) = state.sign_in.as_deref() else {
        return not_found();
    };
    let mut response = finish_login(sign_in, &request).await;
    response.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_static(PendingLoginRef::clear_cookie()),
    );
    response
}

async fn finish_login(sign_in: &SignIn, request: &Parts) -> Response {
    let Some(client) = client_of(request) else {
        tracing::error!("a request arrived without its peer address");
        return server_error();
    };
    let headers = &request.headers;
    let pending = PendingLoginRef::from_cookie_headers(header_values(headers, COOKIE));
    let params = CallbackParams::from_query(request.uri.query().unwrap_or(""));
    let login = match sign_in
        .oidc
        .finish_login(client, pending.as_ref(), &params)
        .await
    {
        Ok(login) => login,
        Err(error) => {
            // The reason is for the log only; `LoginError` keeps it safe.
            tracing::warn!(reason = %error, "sign-in failed");
            let status = match error {
                LoginError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
                _ => StatusCode::BAD_REQUEST,
            };
            return failure_page(status);
        }
    };
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok());
    match sign_in
        .store
        .create_session(&login.identity, user_agent)
        .await
    {
        Ok(session) => {
            tracing::info!(identity = %login.identity, "signed in");
            see_other(login.next.as_str(), &session.set_cookie()).unwrap_or_else(server_error)
        }
        Err(error) => {
            tracing::error!(%error, "sign-in failed: the session could not be created");
            failure_page(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// `POST /auth/logout`: ends the session the request presents.
pub(crate) async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(sign_in) = state.sign_in.as_deref() else {
        return not_found();
    };
    let cookies = header_values(&headers, COOKIE);
    // Looked up only to name who signed out in the log.
    let identity = match SessionToken::from_cookie_headers(cookies.iter().copied()) {
        Some(token) => sign_in.store.authenticate_session(&token).await,
        None => None,
    }
    .map(|who| who.identity);
    let ended = sign_in
        .store
        .sign_out_request(cookies.iter().copied())
        .await;
    sign_in.credentials_ended();
    let status = match ended {
        Ok(_) => {
            if let Some(identity) = identity {
                tracing::info!(%identity, "signed out");
            }
            StatusCode::NO_CONTENT
        }
        // Ended in memory but maybe not in the file: the caller must know
        // that a restart could bring the session back.
        Err(error) => {
            tracing::error!(%error, "a sign-out is not on disk yet");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let mut response = status.into_response();
    response.headers_mut().append(
        SET_COOKIE,
        HeaderValue::from_static(SessionToken::clear_cookie()),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_endpoint_is_exactly_one_segment_after_its_prefix() {
        for path in ["/api/v1/mcp/abc", "/api/v1/mcp/a.b-c_d"] {
            assert!(is_tool_endpoint(path), "{path}");
        }
        for path in [
            "/api/v1/mcp",
            "/api/v1/mcp/",
            "/api/v1/mcp/abc/",
            "/api/v1/mcp/abc/def",
            "/api/v1/mcp/../sessions",
            "/api/v1/mcpx/abc",
            "/api/v1/sessions",
            "/mcp/abc",
        ] {
            assert!(!is_tool_endpoint(path), "{path}");
        }
    }

    #[test]
    fn next_is_read_once_or_not_at_all() {
        assert_eq!(next_parameter(""), None);
        assert_eq!(next_parameter("other=1"), None);
        assert_eq!(
            next_parameter("next=%2Fsessions%2F42%3Ftab%3Ddiff"),
            Some("/sessions/42?tab=diff".to_owned())
        );
        assert_eq!(next_parameter("next=/a&next=/b"), None);
        assert_eq!(next_parameter("next=/a&next=/a"), None);
    }

    #[test]
    fn the_failure_page_is_the_public_message_and_a_link_home() {
        let response = failure_page(StatusCode::BAD_REQUEST);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers()[CONTENT_TYPE], "text/html; charset=utf-8");
    }
}
