//! Sign-in with OpenID Connect: the authorization code flow with PKCE
//! (spec section 2).

mod discovery;
mod http;
mod jwks;
mod jwt;
mod pending;

#[cfg(test)]
mod testing;
#[cfg(test)]
mod tests;

use std::fmt;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use url::Url;

pub use self::jwt::IdTokenError;

use self::discovery::{ProviderMetadata, discovery_url, parse_metadata};
use self::http::{ProviderHttp, REQUEST_TIMEOUT};
use self::jwks::{VerificationKey, parse_key_set};
use self::jwt::{Claims, Expected, UnverifiedToken, validate_claims};
use self::pending::{MAX_PENDING_LOGINS, MAX_PENDING_PER_ADDRESS, PendingLogin, PendingLogins};
use crate::clock::Clock;
use crate::config::PublicConfig;
use crate::cookie::PendingLoginRef;
use crate::identity::Identity;
use crate::json::parse_object;
use crate::next::NextPath;
use crate::ratelimit::FailureLimiter;
use crate::secret::{RandomError, base64url, constant_time_eq, log_safe, random_token, sha256};

/// The provider's keys are fetched again at most this often (spec section 2).
const KEY_REFRESH_INTERVAL_SECS: u64 = 60;
/// Longest wait between two attempts to reach a provider that is down.
const MAX_BACKOFF_SECS: u64 = 60;
/// Longest callback query looked at.
const MAX_QUERY_BYTES: usize = 8 * 1024;
/// Longest authorization code sent on to the provider.
const MAX_CODE_BYTES: usize = 4 * 1024;

/// The OpenID Connect client: starts logins, finishes them, and keeps the
/// provider's metadata and keys current.
///
/// Building one touches no network. The provider is contacted on
/// [`OidcClient::warm_up`] and, if that failed or was never called, again
/// on the next login, with a backoff: a provider that is down delays
/// sign-in, never startup.
pub struct OidcClient {
    config: PublicConfig,
    clock: Arc<dyn Clock>,
    http: ProviderHttp,
    /// What is held of the provider. Read by every login, never held
    /// across a wait, so a slow fetch delays only who needs its result.
    held: std::sync::Mutex<Option<Arc<Ready>>>,
    /// Held for the whole of a fetch, so one runs at a time.
    fetching: tokio::sync::Mutex<Fetching>,
    pending: PendingLogins,
    limiter: FailureLimiter,
}

#[derive(Default)]
struct Fetching {
    /// Consecutive failed attempts at the first fetch.
    failures: u32,
    /// No attempt before this time.
    retry_at: u64,
    last_error: String,
    /// When the keys were last asked for, successfully or not.
    keys_requested_at: u64,
}

struct Ready {
    metadata: ProviderMetadata,
    keys: Vec<VerificationKey>,
}

impl Ready {
    fn holds_key(&self, key_id: &str) -> bool {
        self.keys.iter().any(|key| key.key_id == key_id)
    }
}

/// Tunables that only tests change.
pub(crate) struct Options {
    pub(crate) request_timeout: Duration,
    pub(crate) max_pending_logins: usize,
    pub(crate) max_pending_per_address: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            request_timeout: REQUEST_TIMEOUT,
            max_pending_logins: MAX_PENDING_LOGINS,
            max_pending_per_address: MAX_PENDING_PER_ADDRESS,
        }
    }
}

/// The client could not be built: the TLS stack refused its configuration.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot set up the HTTPS client for the provider: {0}")]
pub struct OidcSetupError(String);

/// A sign-in did not succeed.
///
/// `Display` says why, for the log: it never contains a code, a token, a
/// state or a secret, and text that came from outside is reduced to one
/// line of printable ASCII. The user is shown
/// [`LoginError::PUBLIC_MESSAGE`], the same sentence for every failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LoginError {
    /// The client address has too many failed callbacks (spec section 3).
    #[error("too many failed sign-in attempts from this address")]
    RateLimited,
    /// The callback came without the pending-login cookie.
    #[error("the callback carries no pending-login cookie")]
    NoPendingLogin,
    /// The cookie points to no pending login: never started here, older
    /// than 10 minutes, already used, or dropped by the cap.
    #[error("the pending login is unknown, expired or already used")]
    UnknownPendingLogin,
    /// The query is too long or repeats `state`, `code` or `error`.
    #[error("the callback query is malformed")]
    MalformedCallback,
    /// `state` is absent or is not the pending login's.
    #[error("the callback's state is missing or does not match the pending login")]
    StateMismatch,
    /// The provider redirected back with an error instead of a code.
    #[error("the provider answered the authorization request with the error \"{0}\"")]
    ProviderRefused(String),
    /// The callback has no usable `code`.
    #[error("the callback carries no authorization code")]
    NoCode,
    /// Discovery or the key set could not be fetched or was refused.
    #[error("the provider is not available: {0}")]
    ProviderUnavailable(String),
    /// The token endpoint did not hand over an ID token.
    #[error("the code was not exchanged for an ID token: {0}")]
    TokenEndpoint(String),
    /// The ID token failed validation.
    #[error("the ID token was refused: {0}")]
    IdToken(#[from] IdTokenError),
    /// The identity is valid but may not sign in.
    #[error("{} is not on the allow-list", log_safe(&.0.to_string()))]
    NotAllowed(Identity),
    /// No randomness, no login.
    #[error(transparent)]
    Random(#[from] RandomError),
}

impl LoginError {
    /// The one sentence shown to the user, whatever went wrong.
    pub const PUBLIC_MESSAGE: &'static str = "Sign-in failed. Please try again.";

    /// [`LoginError::PUBLIC_MESSAGE`]: the same for every error.
    pub fn public_message(&self) -> &'static str {
        Self::PUBLIC_MESSAGE
    }
}

/// A login that was started: where to send the browser, and the cookie that
/// lets the callback find the pending login again.
#[derive(Clone)]
pub struct LoginStart {
    authorization_url: String,
    set_cookie: String,
}

impl LoginStart {
    /// The provider URL to redirect the browser to.
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// The `Set-Cookie` value to send with that redirect.
    pub fn set_cookie(&self) -> &str {
        &self.set_cookie
    }
}

impl fmt::Debug for LoginStart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Both fields hold what a callback must prove it knows.
        f.write_str("LoginStart(REDACTED)")
    }
}

/// A login that succeeded. The caller creates the session and redirects.
#[derive(Debug, Clone, PartialEq, Eq)]
#[must_use]
pub struct LoginSuccess {
    /// Who signed in. It is on the allow-list.
    pub identity: Identity,
    /// Where to redirect: a path on this origin.
    pub next: NextPath,
}

/// The parameters of a request to `/auth/callback`.
#[derive(Clone, Default)]
pub struct CallbackParams {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
    malformed: bool,
}

impl CallbackParams {
    /// Reads the parameters from the request's raw query string (what
    /// follows `?`, still percent-encoded). A query that is oversized or
    /// names `state`, `code` or `error` twice is kept as malformed and
    /// fails the login: two values for one parameter is what parameter
    /// pollution looks like, and picking one would be a guess.
    pub fn from_query(raw_query: &str) -> Self {
        let mut params = Self::default();
        if raw_query.len() > MAX_QUERY_BYTES {
            params.malformed = true;
            return params;
        }
        for (name, value) in url::form_urlencoded::parse(raw_query.as_bytes()) {
            let slot = match name.as_ref() {
                "state" => &mut params.state,
                "code" => &mut params.code,
                "error" => &mut params.error,
                _ => continue,
            };
            if slot.is_some() {
                params.malformed = true;
            }
            *slot = Some(value.into_owned());
        }
        params
    }

    /// For a caller whose framework already decoded the query.
    pub fn new(state: Option<&str>, code: Option<&str>, error: Option<&str>) -> Self {
        Self {
            state: state.map(str::to_owned),
            code: code.map(str::to_owned),
            error: error.map(str::to_owned),
            malformed: false,
        }
    }
}

impl fmt::Debug for CallbackParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let presence = |value: &Option<String>| value.as_ref().map(|_| "REDACTED");
        f.debug_struct("CallbackParams")
            .field("state", &presence(&self.state))
            .field("code", &presence(&self.code))
            .field("error", &self.error.as_deref().map(log_safe))
            .field("malformed", &self.malformed)
            .finish()
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

#[derive(Deserialize)]
struct TokenErrorResponse {
    error: String,
}

impl OidcClient {
    /// Builds the client for `config`. Contacts nobody.
    pub fn new(config: &PublicConfig, clock: Arc<dyn Clock>) -> Result<Self, OidcSetupError> {
        Self::with_options(config, clock, Options::default())
    }

    pub(crate) fn with_options(
        config: &PublicConfig,
        clock: Arc<dyn Clock>,
        options: Options,
    ) -> Result<Self, OidcSetupError> {
        let http = ProviderHttp::new(config.loopback_http(), options.request_timeout)
            .map_err(OidcSetupError)?;
        Ok(Self {
            config: config.clone(),
            http,
            held: std::sync::Mutex::new(None),
            fetching: tokio::sync::Mutex::new(Fetching::default()),
            pending: PendingLogins::new(
                options.max_pending_logins,
                options.max_pending_per_address,
            ),
            limiter: FailureLimiter::new(clock.clone()),
            clock,
        })
    }

    /// Fetches the provider's metadata and keys now, if they are not held
    /// yet. Call it once at startup and log an error; nothing else needs
    /// doing about one, since every login retries.
    pub async fn warm_up(&self) -> Result<(), LoginError> {
        self.ready().await.map(|_| ())
    }

    /// Whether the provider's metadata and keys are held. Never waits.
    pub fn is_ready(&self) -> bool {
        self.held().is_some()
    }

    /// Starts a login (`GET /auth/login`).
    ///
    /// `client_address` comes from [`crate::client_address`]. `next` is the
    /// request's `next` parameter, decoded; it is sanitized here. Redirect
    /// the browser to [`LoginStart::authorization_url`] and send
    /// [`LoginStart::set_cookie`] with that response.
    ///
    /// Starting a login needs no credentials, so each address may hold only
    /// a few pending logins: one more replaces that address's oldest, and a
    /// flood from one place cannot push out the logins of everyone else.
    pub async fn begin_login(
        &self,
        client_address: IpAddr,
        next: Option<&str>,
    ) -> Result<LoginStart, LoginError> {
        let mut url = self.ready().await?.metadata.authorization_endpoint.clone();
        let reference = PendingLoginRef::generate()?;
        let login = PendingLogin {
            state: random_token()?,
            nonce: random_token()?,
            code_verifier: random_token()?,
            next: NextPath::sanitize(next),
        };
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", self.config.client_id())
            .append_pair("redirect_uri", &self.config.redirect_uri())
            .append_pair("scope", "openid email")
            .append_pair("state", &login.state)
            .append_pair("nonce", &login.nonce)
            .append_pair(
                "code_challenge",
                &base64url(&sha256(login.code_verifier.as_bytes())),
            )
            .append_pair("code_challenge_method", "S256");
        self.pending
            .insert(&reference, client_address, login, self.clock.now());
        Ok(LoginStart {
            authorization_url: url.into(),
            set_cookie: reference.set_cookie(),
        })
    }

    /// Finishes a login (`GET /auth/callback`).
    ///
    /// `client_address` comes from [`crate::client_address`], `pending` from
    /// [`PendingLoginRef::from_cookie_headers`], `params` from the query.
    /// Every failure counts against the address; the eleventh attempt
    /// within 10 minutes of 10 failures is refused before anything else
    /// happens, with [`LoginError::RateLimited`]. In every other case the
    /// pending login is used up, whatever the outcome. Always answer with
    /// [`PendingLoginRef::clear_cookie`].
    pub async fn finish_login(
        &self,
        client_address: IpAddr,
        pending: Option<&PendingLoginRef>,
        params: &CallbackParams,
    ) -> Result<LoginSuccess, LoginError> {
        let attempt = self
            .limiter
            .begin(client_address)
            .map_err(|_| LoginError::RateLimited)?;
        let success = self.finish(pending, params).await?;
        attempt.succeeded();
        Ok(success)
    }

    async fn finish(
        &self,
        pending: Option<&PendingLoginRef>,
        params: &CallbackParams,
    ) -> Result<LoginSuccess, LoginError> {
        let reference = pending.ok_or(LoginError::NoPendingLogin)?;
        // Taken before anything is checked: one callback per pending login.
        let login = self
            .pending
            .take(reference, self.clock.now())
            .ok_or(LoginError::UnknownPendingLogin)?;
        if params.malformed {
            return Err(LoginError::MalformedCallback);
        }
        let state_matches = params
            .state
            .as_deref()
            .is_some_and(|state| constant_time_eq(state.as_bytes(), login.state.as_bytes()));
        if !state_matches {
            return Err(LoginError::StateMismatch);
        }
        if let Some(error) = &params.error {
            return Err(LoginError::ProviderRefused(log_safe(error)));
        }
        let code = params
            .code
            .as_deref()
            .filter(|code| !code.is_empty() && code.len() <= MAX_CODE_BYTES)
            .ok_or(LoginError::NoCode)?;

        let (token_endpoint, issuer) = {
            let ready = self.ready().await?;
            (
                ready.metadata.token_endpoint.clone(),
                ready.metadata.issuer.clone(),
            )
        };
        let id_token = self
            .exchange_code(&token_endpoint, code, &login.code_verifier)
            .await?;
        let token = UnverifiedToken::parse(&id_token)?;
        let payload = self.verify_signature(&token).await?;
        let claims = validate_claims(
            &payload,
            &Expected {
                issuer: &issuer,
                client_id: self.config.client_id(),
                nonce: &login.nonce,
                now: self.clock.now(),
            },
        )?;
        let identity = identity_of(&claims)?;
        if !self.config.allow_list().allows(&identity) {
            return Err(LoginError::NotAllowed(identity));
        }
        Ok(LoginSuccess {
            identity,
            next: login.next,
        })
    }

    /// Exchanges the code at the token endpoint and returns the ID token.
    async fn exchange_code(
        &self,
        token_endpoint: &Url,
        code: &str,
        code_verifier: &str,
    ) -> Result<String, LoginError> {
        let redirect_uri = self.config.redirect_uri();
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri.as_str()),
            ("code_verifier", code_verifier),
        ];
        // A confidential client authenticates with HTTP Basic; a public one
        // only names itself, and PKCE carries the proof.
        let basic = match self.config.client_secret() {
            Some(secret) => Some((self.config.client_id(), secret)),
            None => {
                form.push(("client_id", self.config.client_id()));
                None
            }
        };
        let response = self
            .http
            .post_form(token_endpoint, &form, basic)
            .await
            .map_err(|error| LoginError::TokenEndpoint(error.to_string()))?;
        // The body is never quoted: on success it holds tokens.
        if response.status != 200 {
            let reason = parse_object::<TokenErrorResponse>(&response.body)
                .map(|body| format!(" with the error \"{}\"", log_safe(&body.error)))
                .unwrap_or_default();
            return Err(LoginError::TokenEndpoint(format!(
                "it answered HTTP {}{reason}",
                response.status
            )));
        }
        parse_object::<TokenResponse>(&response.body)
            .map(|body| body.id_token)
            .map_err(|_| LoginError::TokenEndpoint("its answer holds no ID token".to_owned()))
    }

    /// Verifies the token's signature against the provider's keys, fetching
    /// them again if the token names a key that is not held and the last
    /// fetch is at least a minute old.
    async fn verify_signature(&self, token: &UnverifiedToken<'_>) -> Result<Vec<u8>, LoginError> {
        let mut ready = self.ready().await?;
        if !ready.holds_key(&token.key_id) {
            let mut fetching = self.fetching.lock().await;
            // Whoever held the lock before may have fetched this very key.
            ready = self.held().unwrap_or(ready);
            let now = self.clock.now();
            // A clock that stepped back gives no elapsed time; refresh then
            // rather than wait for an unknown while.
            let refresh_due = now
                .checked_sub(fetching.keys_requested_at)
                .is_none_or(|elapsed| elapsed >= KEY_REFRESH_INTERVAL_SECS);
            if !ready.holds_key(&token.key_id) && refresh_due {
                // Counted before the attempt: a failed fetch uses the minute too.
                fetching.keys_requested_at = now;
                // The metadata is fetched with the keys, as the key set's own
                // address can change. On failure what is held stays.
                if let Ok(fresh) = self.discover().await {
                    ready = self.hold(fresh);
                }
            }
        }
        Ok(token.verify(&ready.keys)?.to_vec())
    }

    /// Returns the provider's metadata and keys, fetching them first when
    /// they are not held and the backoff allows an attempt.
    async fn ready(&self) -> Result<Arc<Ready>, LoginError> {
        if let Some(ready) = self.held() {
            return Ok(ready);
        }
        let mut fetching = self.fetching.lock().await;
        // Whoever held the lock before may have succeeded, or just failed.
        if let Some(ready) = self.held() {
            return Ok(ready);
        }
        let now = self.clock.now();
        // The wait is ignored when it is longer than any backoff could have
        // set: the clock stepped back.
        let wait = fetching.retry_at.saturating_sub(now);
        if fetching.failures > 0 && wait > 0 && wait <= MAX_BACKOFF_SECS {
            return Err(LoginError::ProviderUnavailable(format!(
                "{} (next attempt in {wait} s)",
                fetching.last_error
            )));
        }
        match self.discover().await {
            Ok(ready) => {
                fetching.failures = 0;
                fetching.keys_requested_at = now;
                Ok(self.hold(ready))
            }
            Err(reason) => {
                fetching.failures = fetching.failures.saturating_add(1);
                // Counted from the failure, not from the start of an attempt
                // that may have used its whole timeout: callers queued
                // behind it then fail at once instead of each trying in turn.
                fetching.retry_at = self
                    .clock
                    .now()
                    .saturating_add(backoff_secs(fetching.failures));
                fetching.last_error.clone_from(&reason);
                Err(LoginError::ProviderUnavailable(reason))
            }
        }
    }

    fn held(&self) -> Option<Arc<Ready>> {
        // Only ever replaced whole, so there is nothing to repair after a
        // panic elsewhere.
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn hold(&self, ready: Ready) -> Arc<Ready> {
        let ready = Arc::new(ready);
        *self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ready.clone());
        ready
    }

    /// Fetches and checks the discovery document, then the keys it points to.
    async fn discover(&self) -> Result<Ready, String> {
        let issuer = self.config.issuer();
        let url = discovery_url(issuer).map_err(|reason| format!("the discovery URL {reason}"))?;
        let body = self.fetch(&url, "the discovery document").await?;
        let metadata = parse_metadata(&body, issuer, self.config.loopback_http())
            .map_err(|error| error.to_string())?;
        let body = self.fetch(&metadata.jwks_uri, "the key set").await?;
        Ok(Ready {
            keys: parse_key_set(&body)?,
            metadata,
        })
    }

    async fn fetch(&self, url: &Url, what: &str) -> Result<Vec<u8>, String> {
        let response = self
            .http
            .get(url)
            .await
            .map_err(|error| format!("cannot fetch {what}: {error}"))?;
        if response.status == 200 {
            Ok(response.body)
        } else {
            // A redirect lands here too: none is followed.
            Err(format!("cannot fetch {what}: HTTP {}", response.status))
        }
    }
}

impl fmt::Debug for OidcClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OidcClient")
            .field("issuer", &self.config.issuer())
            .field("client_id", &self.config.client_id())
            .finish_non_exhaustive()
    }
}

/// 1, 2, 4, ... seconds, capped at a minute.
fn backoff_secs(failures: u32) -> u64 {
    1u64.checked_shl(failures.saturating_sub(1))
        .unwrap_or(MAX_BACKOFF_SECS)
        .min(MAX_BACKOFF_SECS)
}

/// Spec section 2, step 3: the email when the provider vouches for it,
/// otherwise the subject. A vouched-for email that is not an address is
/// refused, not replaced by the subject.
fn identity_of(claims: &Claims) -> Result<Identity, IdTokenError> {
    match &claims.email {
        Some(email) if claims.email_verified => {
            Identity::email(email).map_err(|_| IdTokenError::BadEmail)
        }
        _ => Identity::subject(&claims.subject).map_err(|_| IdTokenError::BadSubject),
    }
}
