//! The in-process OpenID Connect provider of this crate's tests, for the
//! tests of other crates (the `test-provider` feature; never part of the
//! `calin` binary). It listens on a loopback port and issues real signed
//! ID tokens, so a server under test runs the whole sign-in against it.
//!
//! Test code: it panics instead of returning errors.

use std::sync::Arc;

use serde_json::json;
use tokio::net::TcpListener;

use crate::clock::Clock;
use crate::oidc::testing::FakeProvider;

/// A provider with one user, `alice@example.com` (verified, subject
/// `user-1`), until [`TestProvider::set_user`] says otherwise.
pub struct TestProvider(FakeProvider);

impl TestProvider {
    /// Starts a provider on a free loopback port. `clock` stamps the tokens:
    /// give it the clock of the server under test. Needs a tokio runtime.
    pub async fn start(
        clock: Arc<dyn Clock>,
        client_id: &str,
        client_secret: Option<&str>,
    ) -> Self {
        Self(FakeProvider::start(clock, client_id, client_secret).await)
    }

    /// As [`TestProvider::start`], on a listener the caller bound.
    pub fn start_on(
        listener: TcpListener,
        clock: Arc<dyn Clock>,
        client_id: &str,
        client_secret: Option<&str>,
    ) -> Self {
        Self(FakeProvider::start_on(
            listener,
            clock,
            client_id,
            client_secret,
        ))
    }

    /// The issuer URL to configure: `http://127.0.0.1:<port>`.
    pub fn issuer(&self) -> &str {
        &self.0.origin
    }

    /// What the user's browser does with an authorization URL: asks the
    /// provider, where the user signs in, and comes back with the query
    /// string of the redirect to `/auth/callback`.
    pub async fn authorize(&self, authorization_url: &str) -> String {
        self.0.authorize(authorization_url).await
    }

    /// Who signs in from now on.
    pub fn set_user(&self, subject: &str, email: Option<&str>, email_verified: bool) {
        let mut state = self.0.state();
        state.user.subject = subject.to_owned();
        state.user.email = email.map(str::to_owned);
        state.user.email_verified = Some(json!(email_verified));
    }

    /// Drops every connection without answering, or answers again.
    pub fn set_down(&self, down: bool) {
        self.0.state().down = down;
    }

    /// A real provider refuses a code the second time; this one can forget to.
    pub fn set_allow_code_replay(&self, allow: bool) {
        self.0.state().allow_code_replay = allow;
    }

    /// The last ID token issued, for asserting it never reaches a log.
    pub fn last_id_token(&self) -> Option<String> {
        self.0.state().last_id_token.clone()
    }

    /// The PKCE verifier of the last token request, for the same purpose.
    pub fn last_code_verifier(&self) -> Option<String> {
        let state = self.0.state();
        let (_, form) = state.last_token_request.as_ref()?;
        form.get("code_verifier").cloned()
    }
}
