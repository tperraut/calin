//! Sign-in for a Calin server: OpenID Connect, Calin's own sessions and
//! device tokens. Implements sections 1 to 4 of
//! `docs/specs/2026-11-27-remote-access.md`, minus the HTTP routes.
//!
//! Whoever is signed in gets a shell, so everything here refuses when in
//! doubt, and the API is shaped so that the HTTP layer passes header values
//! through and cannot assemble a cookie, compare a secret or pick a
//! validation option itself. There is no web framework in this crate.
//!
//! # Pieces
//!
//! - [`AuthMode`] reads the `CALIN_*` variables: local mode, a complete
//!   [`PublicConfig`], or a [`ConfigError`] listing every problem.
//! - [`AllowList`] and [`Identity`]: who may sign in, and the one matching
//!   rule ([`AllowList::allows`]).
//! - [`OidcClient`] runs the authorization code flow with PKCE:
//!   [`OidcClient::begin_login`] and [`OidcClient::finish_login`]. It holds
//!   the pending logins, the provider's metadata and keys, and the limit on
//!   failed callbacks ([`FailureLimiter`]).
//! - [`AuthStore`] holds sessions and device tokens in one file and answers
//!   "who is this request?" ([`AuthStore::authenticate_request`]).
//! - [`SessionToken`], [`DeviceToken`] and [`PendingLoginRef`] read secrets
//!   from request headers and write the `Set-Cookie` values.
//! - [`client_address`] is the client-address rule of section 3.
//! - [`Clock`] is how all of it reads time; [`ManualClock`] is for tests.
//!
//! # A login
//!
//! ```no_run
//! # use std::{net::IpAddr, sync::Arc};
//! # use calin_auth::*;
//! # async fn example(cookie_headers: Vec<&str>, forwarded_for: Vec<&str>, peer: IpAddr,
//! #     raw_query: &str, user_agent: Option<&str>) -> Result<(), Box<dyn std::error::Error>> {
//! // At startup.
//! let AuthMode::Public(config) = AuthMode::from_process_environment(LoopbackHttp::Refuse)? else {
//!     return Ok(()); // Local mode: no sign-in at all.
//! };
//! let clock: Arc<dyn Clock> = Arc::new(SystemClock);
//! let store = AuthStore::open("/home/calin/.calin/auth.json", config.allow_list().clone(), clock.clone()).await?;
//! println!("{:?}", store.load_report()); // log it once
//! let oidc = OidcClient::new(&config, clock)?;
//! if let Err(error) = oidc.warm_up().await {
//!     println!("{error}"); // the provider is down: logins will retry
//! }
//!
//! // GET /auth/login?next=/sessions/42
//! let address = client_address(peer, forwarded_for.iter().copied());
//! let start = oidc.begin_login(address, Some("/sessions/42")).await?;
//! // -> 303, Location: start.authorization_url(), Set-Cookie: start.set_cookie()
//!
//! // GET /auth/callback?state=...&code=...
//! let pending = PendingLoginRef::from_cookie_headers(cookie_headers.iter().copied());
//! let params = CallbackParams::from_query(raw_query);
//! match oidc.finish_login(address, pending.as_ref(), &params).await {
//!     Ok(login) => {
//!         let session = store.create_session(&login.identity, user_agent).await?;
//!         // -> 303, Location: login.next.as_str(),
//!         //    Set-Cookie: session.set_cookie(),
//!         //    Set-Cookie: PendingLoginRef::clear_cookie()
//!         # let _ = (session.set_cookie(), login.next.as_str());
//!     }
//!     Err(error) => {
//!         println!("{error}"); // the reason, for the log only
//!         // -> a plain page saying LoginError::PUBLIC_MESSAGE,
//!         //    Set-Cookie: PendingLoginRef::clear_cookie()
//!     }
//! }
//! # let _ = start; Ok(()) }
//! ```
//!
//! # A request
//!
//! ```no_run
//! # use calin_auth::*;
//! # async fn example(store: AuthStore, cookie_headers: Vec<&str>, authorization_headers: Vec<&str>) {
//! let who = store
//!     .authenticate_request(cookie_headers.iter().copied(), authorization_headers.iter().copied())
//!     .await;
//! match who {
//!     Some(authenticated) => {
//!         // Serve the request as authenticated.identity. When
//!         // authenticated.set_cookie() is Some, add it to the response:
//!         // that is the daily re-issue of the session cookie.
//!         # let _ = authenticated.set_cookie();
//!     }
//!     None => { /* 401 */ }
//! }
//!
//! // POST /auth/logout
//! let written = store.sign_out_request(cookie_headers.iter().copied()).await;
//! // -> Set-Cookie: SessionToken::clear_cookie(); a 500 if `written` is an error
//! # let _ = written;
//! # }
//! ```
//!
//! # What the HTTP layer still owes
//!
//! - The Host and Origin guards of section 1, with
//!   [`PublicConfig::public_authority`] and [`PublicConfig::public_origin`].
//!   The session cookie is `SameSite=Lax`: the Origin guard is what stops a
//!   cross-site `POST`.
//! - `Cache-Control: no-store` on `/auth/*` answers, and never logging a
//!   request's query string or `Cookie` and `Authorization` headers.
//! - [`AuthStore::flush`] on shutdown, and one [`AuthStore`] per file.
//! - A server error when a revocation or sign-out returns one: it took
//!   effect in memory but is not in the file yet.
//!
//! # Where the spec was silent
//!
//! Each of these is the stricter reading; the item it sits on says more.
//!
//! - Sign-in variables without `CALIN_PUBLIC_URL` are an error, not local
//!   mode ([`AuthMode::from_lookup`]).
//! - An identity is the verified email or else the subject, never both: a
//!   `sub:` entry does not admit a user whose email is verified
//!   ([`AllowList::allows`]). Email case is folded for ASCII letters only.
//!   `email_verified` counts only as the JSON boolean `true`.
//! - The issuer in the discovery document may differ from the configured
//!   one by one trailing `/`; the token's `iss` must then equal the
//!   document's issuer exactly.
//! - ID tokens must be signed with RS256 or ES256 and name their key
//!   (`kid`). Clock skew allowed on `exp`, `iat` and `nbf`: 60 seconds.
//! - No redirect from the provider is followed, to any origin.
//! - The session cookie has `Max-Age` of 30 days besides the attributes
//!   section 3 lists: without an expiry there would be nothing to re-issue.
//! - A request with two different `__Host-calin_session` cookies is not signed in.
//! - Sessions and device tokens are listed and revoked per identity.
//! - The failure limit counts IPv6 addresses per /64, and counts a failed
//!   callback only when it presented a live pending login of this server:
//!   junk sent in a visitor's name cannot lock that visitor out.
//! - The cookies are `__Host-calin_session` and `__Secure-calin_login`,
//!   with or without the development flag; the names without their prefix
//!   are not read.
//! - The provider's certificate must chain to a root in the Mozilla set
//!   compiled in (`webpki-roots`); the system trust store is not read, and
//!   the provider is reached directly: proxy variables are not honored.
//! - Pending logins are bounded per client address as well as in total, so
//!   one client cannot push out the logins of others.
//! - A session or device token created later than the clock now reads
//!   (beyond a minute) authenticates nobody: the clock is behind.
//! - A callback uses up its pending login even when it fails. Two logins
//!   started in two tabs of one browser share one cookie, so the first to
//!   come back fails both; the user starts again.

mod client_addr;
mod clock;
mod config;
mod cookie;
mod identity;
mod json;
mod next;
mod oidc;
mod ratelimit;
mod secret;
mod store;
#[cfg(feature = "test-provider")]
pub mod test_provider;

pub use client_addr::client_address;
pub use clock::{Clock, ManualClock, SystemClock};
pub use config::{
    AuthMode, CALLBACK_PATH, ConfigError, ConfigProblem, ENV_ALLOWED_USERS, ENV_OIDC_CLIENT_ID,
    ENV_OIDC_CLIENT_SECRET, ENV_OIDC_ISSUER, ENV_PUBLIC_URL, LoopbackHttp, PublicConfig,
};
pub use cookie::{
    DEVICE_TOKEN_PREFIX, DeviceToken, PENDING_LOGIN_COOKIE, PendingLoginRef, SESSION_COOKIE,
    SessionToken,
};
pub use identity::{AllowList, AllowListProblem, Identity, InvalidIdentity};
pub use next::NextPath;
pub use oidc::{
    CallbackParams, IdTokenError, LoginError, LoginStart, LoginSuccess, OidcClient, OidcSetupError,
};
pub use ratelimit::{Attempt, FailureLimiter, RateLimited};
pub use secret::RandomError;
pub use store::{
    AuthStore, Authenticated, Credential, DeviceTokenInfo, LoadReport, LoadedFile, NewDeviceToken,
    NewSession, SessionInfo, StoreError,
};
