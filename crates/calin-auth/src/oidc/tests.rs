//! Sign-in against the test provider: the full flow and every refusal.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::testing::{FakeProvider, Misbehavior, Signing, SigningKey};
use super::*;
use crate::clock::ManualClock;
use crate::config::testing::public_config;

const START: u64 = 1_800_000_000;
const CLIENT_ID: &str = "calin-test-client";
const ALLOWED: &str = "alice@example.com, sub:user-2";

struct Harness {
    provider: FakeProvider,
    client: OidcClient,
    clock: Arc<ManualClock>,
}

struct Setup {
    client_secret: Option<&'static str>,
    allowed_users: &'static str,
    /// Appended to the provider's origin to form the configured issuer.
    issuer_suffix: &'static str,
    options: Options,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            client_secret: None,
            allowed_users: ALLOWED,
            issuer_suffix: "",
            options: Options::default(),
        }
    }
}

impl Harness {
    async fn new() -> Self {
        Self::with(Setup::default()).await
    }

    async fn with(setup: Setup) -> Self {
        let clock = Arc::new(ManualClock::new(START));
        let provider = FakeProvider::start(clock.clone(), CLIENT_ID, setup.client_secret).await;
        let issuer = format!("{}{}", provider.origin, setup.issuer_suffix);
        let config = public_config(&issuer, setup.client_secret, setup.allowed_users);
        let client = OidcClient::with_options(&config, clock.clone(), setup.options).unwrap();
        Self {
            provider,
            client,
            clock,
        }
    }

    /// `GET /auth/login`: returns the authorization URL and the reference
    /// the browser now holds in its cookie.
    async fn begin(&self, next: Option<&str>) -> (String, PendingLoginRef) {
        self.begin_from(address(1), next).await
    }

    async fn begin_from(&self, from: IpAddr, next: Option<&str>) -> (String, PendingLoginRef) {
        let start = self.client.begin_login(from, next).await.unwrap();
        let pair = start.set_cookie().split(';').next().unwrap();
        let reference = PendingLoginRef::from_cookie_headers([pair]).unwrap();
        (start.authorization_url().to_owned(), reference)
    }

    /// `GET /auth/callback` from the default address.
    async fn finish(
        &self,
        reference: &PendingLoginRef,
        query: &str,
    ) -> Result<LoginSuccess, LoginError> {
        self.client
            .finish_login(
                address(1),
                Some(reference),
                &CallbackParams::from_query(query),
            )
            .await
    }

    /// The whole flow as a browser would run it.
    async fn login(&self) -> Result<LoginSuccess, LoginError> {
        self.login_from(address(1)).await
    }

    async fn login_from(&self, from: IpAddr) -> Result<LoginSuccess, LoginError> {
        let (url, reference) = self.begin_from(from, Some("/sessions")).await;
        let query = self.provider.authorize(&url).await;
        self.client
            .finish_login(from, Some(&reference), &CallbackParams::from_query(&query))
            .await
    }
}

/// A distinct client address per `n`, so one test's failures do not trip
/// the limiter for its next case.
fn address(n: u8) -> IpAddr {
    IpAddr::from([198, 51, 100, n])
}

fn email(address: &str) -> Identity {
    Identity::email(address).unwrap()
}

fn subject(sub: &str) -> Identity {
    Identity::subject(sub).unwrap()
}

fn query_of(url: &str) -> HashMap<String, String> {
    Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

/// Asserts that a login succeeded.
fn signed_in(result: Result<LoginSuccess, LoginError>) -> Identity {
    match result {
        Ok(success) => success.identity,
        Err(error) => panic!("expected a successful login, got: {error}"),
    }
}

fn id_token_error(result: Result<LoginSuccess, LoginError>) -> IdTokenError {
    match result {
        Err(LoginError::IdToken(error)) => error,
        other => panic!("expected an ID token refusal, got {other:?}"),
    }
}

// ---------------------------------------------------------------- the flow

#[tokio::test]
async fn full_flow_as_a_public_client() {
    let harness = Harness::new().await;
    harness.client.warm_up().await.unwrap();
    assert!(harness.client.is_ready());

    let start = harness
        .client
        .begin_login(address(1), Some("/sessions/42?tab=diff#top"))
        .await
        .unwrap();
    let cookie = start.set_cookie().to_owned();
    assert!(cookie.starts_with("__Secure-calin_login="));
    assert!(cookie.ends_with("; Path=/auth; Max-Age=600; HttpOnly; Secure; SameSite=Lax"));

    let url = start.authorization_url();
    assert!(url.starts_with(&format!("{}/authorize?", harness.provider.origin)));
    let sent = query_of(url);
    assert_eq!(sent["response_type"], "code");
    assert_eq!(sent["client_id"], CLIENT_ID);
    assert_eq!(sent["redirect_uri"], "http://localhost:4680/auth/callback");
    assert_eq!(sent["scope"], "openid email");
    assert_eq!(sent["code_challenge_method"], "S256");
    for name in ["state", "nonce", "code_challenge"] {
        assert_eq!(sent[name].len(), 43, "{name}");
    }
    assert_eq!(sent.len(), 8);

    let reference =
        PendingLoginRef::from_cookie_headers([cookie.split(';').next().unwrap()]).unwrap();
    let query = harness.provider.authorize(url).await;
    let success = harness.finish(&reference, &query).await.unwrap();
    assert_eq!(success.identity, email("alice@example.com"));
    assert_eq!(success.next.as_str(), "/sessions/42?tab=diff#top");

    // A public client names itself in the form and proves itself with PKCE.
    let state = harness.provider.state();
    let (authorization, form) = state.last_token_request.clone().unwrap();
    assert_eq!(authorization, None);
    assert_eq!(form["grant_type"], "authorization_code");
    assert_eq!(form["client_id"], CLIENT_ID);
    assert_eq!(form["redirect_uri"], "http://localhost:4680/auth/callback");
    assert_eq!(
        base64url(&sha256(form["code_verifier"].as_bytes())),
        sent["code_challenge"]
    );
    assert!(!form.contains_key("client_secret"));
    // Metadata and keys were fetched once, at warm-up.
    assert_eq!(
        (state.hits.discovery, state.hits.keys, state.hits.token),
        (1, 1, 1)
    );
}

#[tokio::test]
async fn full_flow_as_a_confidential_client_uses_http_basic() {
    // A secret with characters that must be form-encoded inside Basic.
    let secret = "s3cr3t~+/ :%&=";
    let harness = Harness::with(Setup {
        client_secret: Some(secret),
        ..Setup::default()
    })
    .await;
    assert_eq!(
        harness.login().await.unwrap().identity,
        email("alice@example.com")
    );

    let state = harness.provider.state();
    let (authorization, form) = state.last_token_request.clone().unwrap();
    assert!(authorization.unwrap().starts_with("Basic "));
    assert!(!form.contains_key("client_id"));
    assert!(!form.contains_key("client_secret"));
}

#[tokio::test]
async fn a_wrong_client_secret_is_refused_by_the_provider() {
    let harness = Harness::with(Setup {
        client_secret: Some("configured-secret"),
        ..Setup::default()
    })
    .await;
    harness.provider.state().client_secret = Some("another-secret".to_owned());
    assert_eq!(
        harness.login().await.unwrap_err(),
        LoginError::TokenEndpoint(
            "it answered HTTP 401 with the error \"invalid_client\"".to_owned()
        )
    );
}

#[tokio::test]
async fn full_flow_with_an_rs256_key() {
    let harness = Harness::new().await;
    harness
        .provider
        .rotate_key(SigningKey::rs256("rsa-1"), true);
    assert_eq!(
        harness.login().await.unwrap().identity,
        email("alice@example.com")
    );
}

#[tokio::test]
async fn without_warm_up_the_first_login_fetches_the_metadata() {
    let harness = Harness::new().await;
    assert!(!harness.client.is_ready());
    assert_eq!(harness.provider.state().hits.connections, 0);
    signed_in(harness.login().await);
    assert!(harness.client.is_ready());
    signed_in(harness.login().await);
    assert_eq!(harness.provider.state().hits.discovery, 1);
    assert_eq!(harness.provider.state().hits.keys, 1);
}

#[tokio::test]
async fn an_authorization_endpoint_with_a_query_keeps_it() {
    let harness = Harness::new().await;
    let endpoint = format!("{}/authorize?tenant=a%20b", harness.provider.origin);
    harness
        .provider
        .state()
        .discovery_extra
        .insert("authorization_endpoint".to_owned(), json!(endpoint));
    let (url, _) = harness.begin(None).await;
    let sent = query_of(&url);
    assert_eq!(sent["tenant"], "a b");
    assert_eq!(sent["response_type"], "code");
}

#[tokio::test]
async fn many_logins_at_once_all_succeed() {
    let harness = Arc::new(Harness::new().await);
    let mut tasks = Vec::new();
    for n in 0..16u8 {
        let harness = harness.clone();
        tasks.push(tokio::spawn(
            async move { harness.login_from(address(n)).await },
        ));
    }
    for task in tasks {
        assert_eq!(
            task.await.unwrap().unwrap().identity,
            email("alice@example.com")
        );
    }
    assert_eq!(harness.provider.state().hits.discovery, 1);
    assert_eq!(harness.client.pending.len(), 0);
}

// ------------------------------------------------- state and pending login

#[tokio::test]
async fn a_wrong_or_missing_state_is_refused() {
    let harness = Harness::new().await;
    let cases: [(&str, BuildQuery); 4] = [
        ("wrong state", |code, _| {
            format!("code={code}&state=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")
        }),
        ("no state", |code, _| format!("code={code}")),
        ("empty state", |code, _| format!("code={code}&state=")),
        ("state with a suffix", |code, state| {
            format!("code={code}&state={state}x")
        }),
    ];
    for (what, build) in cases {
        let (url, reference) = harness.begin(None).await;
        let genuine = form_of(&harness.provider.authorize(&url).await);
        let query = build(&genuine["code"], &genuine["state"]);
        assert_eq!(
            harness.finish(&reference, &query).await.unwrap_err(),
            LoginError::StateMismatch,
            "{what}"
        );
    }
    // Nothing was sent to the token endpoint for any of them.
    assert_eq!(harness.provider.state().hits.token, 0);
}

/// Builds a callback query from the genuine code and state.
type BuildQuery = fn(&str, &str) -> String;

fn form_of(query: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

#[tokio::test]
async fn the_state_of_another_pending_login_is_refused() {
    let harness = Harness::new().await;
    let (url_a, reference_a) = harness.begin(None).await;
    let (url_b, _reference_b) = harness.begin(None).await;
    let _query_a = harness.provider.authorize(&url_a).await;
    let query_b = harness.provider.authorize(&url_b).await;
    // An attacker's code and state, delivered to the victim's browser.
    assert_eq!(
        harness.finish(&reference_a, &query_b).await.unwrap_err(),
        LoginError::StateMismatch
    );
}

#[tokio::test]
async fn a_callback_without_its_cookie_or_with_an_unknown_one_is_refused() {
    let harness = Harness::new().await;
    let (url, _reference) = harness.begin(None).await;
    let query = harness.provider.authorize(&url).await;
    let params = CallbackParams::from_query(&query);
    assert_eq!(
        harness
            .client
            .finish_login(address(1), None, &params)
            .await
            .unwrap_err(),
        LoginError::NoPendingLogin
    );
    let stranger = PendingLoginRef::generate().unwrap();
    assert_eq!(
        harness.finish(&stranger, &query).await.unwrap_err(),
        LoginError::UnknownPendingLogin
    );
    assert_eq!(harness.provider.state().hits.token, 0);
}

#[tokio::test]
async fn a_pending_login_is_used_once_even_by_a_failed_callback() {
    let harness = Harness::new().await;
    let (url, reference) = harness.begin(None).await;
    let query = harness.provider.authorize(&url).await;
    signed_in(harness.finish(&reference, &query).await);
    // The same callback again: the replayed code never reaches the provider.
    assert_eq!(
        harness.finish(&reference, &query).await.unwrap_err(),
        LoginError::UnknownPendingLogin
    );
    assert_eq!(harness.provider.state().hits.token, 1);

    // A failed first attempt also uses the pending login up.
    let (url, reference) = harness.begin(None).await;
    let query = harness.provider.authorize(&url).await;
    assert_eq!(
        harness
            .finish(&reference, "code=x&state=wrong")
            .await
            .unwrap_err(),
        LoginError::StateMismatch
    );
    assert_eq!(
        harness.finish(&reference, &query).await.unwrap_err(),
        LoginError::UnknownPendingLogin
    );
}

#[tokio::test]
async fn a_pending_login_expires_after_ten_minutes() {
    let harness = Harness::new().await;
    let (url, reference) = harness.begin(None).await;
    let query = harness.provider.authorize(&url).await;
    harness.clock.advance(599);
    signed_in(harness.finish(&reference, &query).await);

    let (url, reference) = harness.begin(None).await;
    let query = harness.provider.authorize(&url).await;
    harness.clock.advance(600);
    assert_eq!(
        harness.finish(&reference, &query).await.unwrap_err(),
        LoginError::UnknownPendingLogin
    );
}

#[tokio::test]
async fn pending_logins_are_capped_and_the_oldest_is_dropped() {
    let harness = Harness::with(Setup {
        options: Options {
            max_pending_logins: 3,
            ..Options::default()
        },
        ..Setup::default()
    })
    .await;
    let mut started = Vec::new();
    for _ in 0..4 {
        started.push(harness.begin(None).await);
        assert!(harness.client.pending.len() <= 3);
    }
    let (first_url, first_reference) = &started[0];
    let query = harness.provider.authorize(first_url).await;
    assert_eq!(
        harness.finish(first_reference, &query).await.unwrap_err(),
        LoginError::UnknownPendingLogin
    );
    let (last_url, last_reference) = &started[3];
    let query = harness.provider.authorize(last_url).await;
    signed_in(harness.finish(last_reference, &query).await);
}

#[tokio::test]
async fn the_default_cap_bounds_memory() {
    let harness = Harness::new().await;
    for n in 0..(MAX_PENDING_LOGINS as u32 + 50) {
        let from = IpAddr::from((0x0a00_0000 + n).to_be_bytes());
        harness.client.begin_login(from, None).await.unwrap();
    }
    assert_eq!(harness.client.pending.len(), MAX_PENDING_LOGINS);
}

#[tokio::test]
async fn a_flood_of_logins_from_one_address_does_not_void_another_one() {
    let harness = Harness::new().await;
    // A user is at the provider, signing in.
    let (url, reference) = harness.begin_from(address(1), Some("/mine")).await;
    let query = harness.provider.authorize(&url).await;

    // Meanwhile someone starts logins as fast as they can.
    for _ in 0..(2 * MAX_PENDING_LOGINS) {
        harness.client.begin_login(address(66), None).await.unwrap();
    }
    assert_eq!(harness.client.pending.len(), 1 + MAX_PENDING_PER_ADDRESS);

    let success = harness.finish(&reference, &query).await.unwrap();
    assert_eq!(success.next.as_str(), "/mine");
}

#[tokio::test]
async fn next_is_sanitized_when_the_login_starts() {
    let harness = Harness::new().await;
    let cases = [
        (Some("/ok?x=1#y"), "/ok?x=1#y"),
        (Some("/sessions/7"), "/sessions/7"),
        (Some("//evil.example"), "/"),
        (Some("/\\evil"), "/"),
        (Some("https://evil.example"), "/"),
        (Some("/\t/evil.example"), "/"),
        (Some(""), "/"),
        (None, "/"),
    ];
    for (next, expected) in cases {
        let (url, reference) = harness.begin(next).await;
        // The destination stays on the server: the provider never sees it.
        assert!(!query_of(&url).contains_key("next"));
        let query = harness.provider.authorize(&url).await;
        let success = harness.finish(&reference, &query).await.unwrap();
        assert_eq!(success.next.as_str(), expected, "{next:?}");
    }
}

// ------------------------------------------------------- callback parameters

#[tokio::test]
async fn a_provider_error_or_a_missing_code_is_refused() {
    let harness = Harness::new().await;
    let state_of = |url: &str| query_of(url)["state"].clone();

    let (url, reference) = harness.begin(None).await;
    let query = format!(
        "error=access_denied&error_description=nope&state={}",
        state_of(&url)
    );
    assert_eq!(
        harness.finish(&reference, &query).await.unwrap_err(),
        LoginError::ProviderRefused("access_denied".to_owned())
    );

    // The error text is the attacker's to choose: it is cut down for the log.
    let (url, reference) = harness.begin(None).await;
    let query = format!("error=a%0Ab%1B%5B0m&state={}", state_of(&url));
    assert_eq!(
        harness.finish(&reference, &query).await.unwrap_err(),
        LoginError::ProviderRefused("a?b?[0m".to_owned())
    );

    // An error without the right state is a state mismatch first.
    let (_url, reference) = harness.begin(None).await;
    assert_eq!(
        harness
            .finish(&reference, "error=access_denied&state=wrong")
            .await
            .unwrap_err(),
        LoginError::StateMismatch
    );

    for query in ["state=STATE", "state=STATE&code=", "state=STATE&cod=x"] {
        let (url, reference) = harness.begin(None).await;
        let query = query.replace("STATE", &state_of(&url));
        assert_eq!(
            harness.finish(&reference, &query).await.unwrap_err(),
            LoginError::NoCode
        );
    }
    let (url, reference) = harness.begin(None).await;
    let query = format!("state={}&code={}", state_of(&url), "c".repeat(5000));
    assert_eq!(
        harness.finish(&reference, &query).await.unwrap_err(),
        LoginError::NoCode
    );
    assert_eq!(harness.provider.state().hits.token, 0);
}

#[tokio::test]
async fn a_repeated_or_oversized_query_is_refused() {
    let harness = Harness::new().await;
    for build in [
        (|code: &str, state: &str| format!("code=other&code={code}&state={state}")) as BuildQuery,
        |code, state| format!("code={code}&state=other&state={state}"),
        |code, state| format!("code={code}&state={state}&error=x&error=y"),
        |code, state| format!("code={code}&state={state}&pad={}", "p".repeat(9000)),
    ] {
        let (url, reference) = harness.begin(None).await;
        let genuine = form_of(&harness.provider.authorize(&url).await);
        let query = build(&genuine["code"], &genuine["state"]);
        assert_eq!(
            harness.finish(&reference, &query).await.unwrap_err(),
            LoginError::MalformedCallback
        );
    }
}

#[tokio::test]
async fn the_callback_checks_run_in_a_fixed_order() {
    let harness = Harness::new().await;
    let state_of = |url: &str| query_of(url)["state"].clone();
    let cases: [(&str, LoginError); 4] = [
        // Malformed first, even with a wrong state and an error.
        (
            "state=wrong&state=wrong&error=x",
            LoginError::MalformedCallback,
        ),
        // A provider error with the right state wins over a code.
        (
            "state=STATE&error=access_denied&code=c",
            LoginError::ProviderRefused("access_denied".to_owned()),
        ),
        (
            "state=STATE&error=access_denied&code=",
            LoginError::ProviderRefused("access_denied".to_owned()),
        ),
        ("state=wrong&code=", LoginError::StateMismatch),
    ];
    for (query, error) in cases {
        let (url, reference) = harness.begin(None).await;
        let query = query.replace("STATE", &state_of(&url));
        assert_eq!(
            harness.finish(&reference, &query).await.unwrap_err(),
            error,
            "{query}"
        );
    }
    assert_eq!(harness.provider.state().hits.token, 0);
}

#[test]
fn callback_parameters_are_decoded_and_unknown_ones_ignored() {
    let params = CallbackParams::from_query("code=a%2Fb+c&state=s%3D1&session_state=x&iss=y");
    assert_eq!(params.code.as_deref(), Some("a/b c"));
    assert_eq!(params.state.as_deref(), Some("s=1"));
    assert_eq!(params.error, None);
    assert!(!params.malformed);

    let params = CallbackParams::new(Some("s"), Some("c"), None);
    assert_eq!(
        (params.state.as_deref(), params.code.as_deref()),
        (Some("s"), Some("c"))
    );
    assert!(CallbackParams::from_query("").code.is_none());
}

#[test]
fn the_longest_code_and_query_are_read_and_one_byte_more_is_refused() {
    // A caller that decoded the query itself gets the same checks.
    let longest = "c".repeat(MAX_CODE_BYTES);
    let params = CallbackParams::new(Some("s"), Some(&longest), None);
    assert_eq!(params.code_for("s"), Ok(longest.as_str()));
    let too_long = "c".repeat(MAX_CODE_BYTES + 1);
    assert_eq!(
        CallbackParams::new(Some("s"), Some(&too_long), None).code_for("s"),
        Err(LoginError::NoCode)
    );

    let query = |total: usize| {
        let head = "state=s&code=c&pad=";
        format!("{head}{}", "p".repeat(total - head.len()))
    };
    assert_eq!(
        CallbackParams::from_query(&query(MAX_QUERY_BYTES)).code_for("s"),
        Ok("c")
    );
    assert_eq!(
        CallbackParams::from_query(&query(MAX_QUERY_BYTES + 1)).code_for("s"),
        Err(LoginError::MalformedCallback)
    );
}

// ------------------------------------------------------------- the ID token

#[tokio::test]
async fn a_wrong_or_missing_nonce_is_refused() {
    let harness = Harness::new().await;
    harness.provider.state().claim_edits = vec![("nonce", Some(json!("another-nonce")))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongNonce
    );
    harness.provider.state().claim_edits = vec![("nonce", None)];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongNonce
    );
}

#[tokio::test]
async fn a_wrong_audience_is_refused() {
    let harness = Harness::new().await;
    for audience in [
        json!("another-client"),
        json!(["another-client", "an-api"]),
        json!([]),
    ] {
        harness.provider.state().claim_edits = vec![("aud", Some(audience))];
        assert_eq!(
            id_token_error(harness.login().await),
            IdTokenError::WrongAudience
        );
    }
}

#[tokio::test]
async fn several_audiences_need_an_authorized_party() {
    let harness = Harness::new().await;
    let audiences = json!([CLIENT_ID, "an-api"]);

    harness.provider.state().claim_edits = vec![
        ("aud", Some(audiences.clone())),
        ("azp", Some(json!(CLIENT_ID))),
    ];
    signed_in(harness.login().await);

    harness.provider.state().claim_edits = vec![("aud", Some(audiences.clone()))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongAuthorizedParty
    );

    harness.provider.state().claim_edits =
        vec![("aud", Some(audiences)), ("azp", Some(json!("an-api")))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongAuthorizedParty
    );

    // One audience in an array is fine without azp, and a wrong azp never is.
    harness.provider.state().claim_edits = vec![("aud", Some(json!([CLIENT_ID])))];
    signed_in(harness.login().await);
    harness.provider.state().claim_edits = vec![("azp", Some(json!("another-client")))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongAuthorizedParty
    );
}

#[tokio::test]
async fn an_expired_token_is_refused() {
    let harness = Harness::new().await;
    // Inside the 60-second allowance.
    harness.provider.state().claim_edits = vec![
        ("exp", Some(json!(START - 59))),
        ("iat", Some(json!(START - 400))),
    ];
    signed_in(harness.login().await);
    harness.provider.state().claim_edits = vec![
        ("exp", Some(json!(START - 60))),
        ("iat", Some(json!(START - 400))),
    ];
    assert_eq!(id_token_error(harness.login().await), IdTokenError::Expired);
    harness.provider.state().claim_edits = vec![("exp", Some(json!(START - 86_400)))];
    assert_eq!(id_token_error(harness.login().await), IdTokenError::Expired);
    harness.provider.state().claim_edits = vec![("exp", None)];
    assert!(matches!(
        id_token_error(harness.login().await),
        IdTokenError::MalformedClaims(_)
    ));
}

#[tokio::test]
async fn a_token_from_the_future_is_refused() {
    let harness = Harness::new().await;
    harness.provider.state().claim_edits = vec![("iat", Some(json!(START + 60)))];
    signed_in(harness.login().await);
    harness.provider.state().claim_edits = vec![("iat", Some(json!(START + 61)))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::IssuedInFuture
    );
    harness.provider.state().claim_edits = vec![("nbf", Some(json!(START + 61)))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::NotYetValid
    );
    harness.provider.state().claim_edits = vec![("nbf", Some(json!(START - 10)))];
    signed_in(harness.login().await);
}

#[tokio::test]
async fn a_token_for_another_issuer_is_refused() {
    let harness = Harness::new().await;
    harness.provider.state().token_issuer = Some("https://evil.example".to_owned());
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongIssuer
    );
    // Not even a trailing slash apart from what discovery published.
    let slashed = format!("{}/", harness.provider.origin);
    harness.provider.state().token_issuer = Some(slashed);
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::WrongIssuer
    );
}

#[tokio::test]
async fn an_unsigned_token_is_refused() {
    let harness = Harness::new().await;
    harness.provider.state().signing = Signing::Unsigned;
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnsupportedAlgorithm("none".to_owned())
    );
    // Also when it claims a real algorithm over an empty signature.
    harness.provider.state().header_edits = vec![("alg", Some(json!("ES256")))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::BadSignature
    );
}

#[tokio::test]
async fn a_token_signed_with_hs256_and_the_client_secret_is_refused() {
    let secret = "the-client-secret";
    let harness = Harness::with(Setup {
        client_secret: Some(secret),
        ..Setup::default()
    })
    .await;
    harness.provider.state().signing = Signing::Hmac(secret.to_owned());
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnsupportedAlgorithm("HS256".to_owned())
    );
}

#[tokio::test]
async fn a_token_signed_with_hs256_and_the_public_key_is_refused() {
    // The classic confusion: the published key used as an HMAC secret.
    let harness = Harness::new().await;
    harness
        .provider
        .rotate_key(SigningKey::rs256("rsa-1"), true);
    let modulus = harness.provider.state().signing_key.jwk()["n"]
        .as_str()
        .unwrap()
        .to_owned();
    harness.provider.state().signing = Signing::Hmac(modulus);
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnsupportedAlgorithm("HS256".to_owned())
    );
    // Relabelled as RS256 the same bytes are simply a bad signature.
    harness.provider.state().header_edits = vec![("alg", Some(json!("RS256")))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::BadSignature
    );
}

#[tokio::test]
async fn a_token_signed_by_a_key_that_is_not_published_is_refused() {
    let harness = Harness::new().await;
    harness.client.warm_up().await.unwrap();
    // Same key id as the published key, different key.
    harness.provider.state().signing_key = SigningKey::es256("key-1");
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::BadSignature
    );

    // A key id the provider does not publish, also after a refresh.
    harness.clock.advance(61);
    harness.provider.state().signing_key = SigningKey::es256("ghost");
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnknownKey("ghost".to_owned())
    );
    assert_eq!(harness.provider.state().hits.keys, 2);
}

#[tokio::test]
async fn a_token_altered_after_signing_is_refused() {
    let harness = Harness::new().await;
    // The payload replaced by one naming another user, signature kept.
    harness.provider.state().tamper = Some(|token| {
        let parts: Vec<&str> = token.split('.').collect();
        let mut claims: Value =
            serde_json::from_slice(&crate::secret::base64url_decode(parts[1]).unwrap()).unwrap();
        claims["email"] = json!("mallory@example.com");
        format!(
            "{}.{}.{}",
            parts[0],
            base64url(claims.to_string().as_bytes()),
            parts[2]
        )
    });
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::BadSignature
    );

    harness.provider.state().tamper = Some(|token| format!("{token}.extra"));
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::Malformed
    );
    harness.provider.state().tamper = Some(|_| "not-a-token".to_owned());
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::Malformed
    );
}

#[tokio::test]
async fn a_key_is_used_only_with_its_own_algorithm() {
    let harness = Harness::new().await;
    // An ES256 signature labelled RS256, under the ES256 key's id.
    harness.provider.state().header_edits = vec![("alg", Some(json!("RS256")))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::BadSignature
    );
    harness.provider.state().header_edits = vec![("kid", None)];
    assert_eq!(id_token_error(harness.login().await), IdTokenError::NoKeyId);
    harness.provider.state().header_edits = vec![("crit", Some(json!(["b64"])))];
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::Malformed
    );
}

// ------------------------------------------------------------ key rotation

#[tokio::test]
async fn a_new_key_id_triggers_one_refresh_then_success() {
    let harness = Harness::new().await;
    signed_in(harness.login().await);
    assert_eq!(harness.provider.state().hits.keys, 1);

    harness.clock.advance(3600);
    harness
        .provider
        .rotate_key(SigningKey::es256("key-2"), false);
    signed_in(harness.login().await);
    assert_eq!(harness.provider.state().hits.keys, 2);
    // The new key is now held: no further fetch.
    signed_in(harness.login().await);
    signed_in(harness.login().await);
    assert_eq!(harness.provider.state().hits.keys, 2);
    assert_eq!(harness.provider.state().hits.discovery, 2);
}

#[tokio::test]
async fn keys_are_refreshed_at_most_once_a_minute() {
    let harness = Harness::new().await;
    signed_in(harness.login().await);
    harness.clock.advance(3600);
    harness
        .provider
        .rotate_key(SigningKey::es256("key-2"), true);
    signed_in(harness.login().await);
    assert_eq!(harness.provider.state().hits.keys, 2);

    // A second rotation 59 seconds later is not picked up yet, however
    // many logins ask.
    harness.clock.advance(59);
    harness
        .provider
        .rotate_key(SigningKey::es256("key-3"), true);
    for n in 0..5 {
        assert_eq!(
            id_token_error(harness.login_from(address(10 + n)).await),
            IdTokenError::UnknownKey("key-3".to_owned())
        );
    }
    assert_eq!(harness.provider.state().hits.keys, 2);

    // At one minute it is.
    harness.clock.advance(1);
    signed_in(harness.login().await);
    assert_eq!(harness.provider.state().hits.keys, 3);
}

#[tokio::test]
async fn a_failed_refresh_keeps_the_old_keys_and_uses_up_the_minute() {
    let harness = Harness::new().await;
    signed_in(harness.login().await);
    harness.clock.advance(120);
    harness.provider.state().keys = Some(Misbehavior::Answer(503, "{}".to_owned()));
    harness.provider.state().signing_key = SigningKey::es256("ghost");
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnknownKey("ghost".to_owned())
    );
    assert_eq!(harness.provider.state().hits.keys, 2);
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnknownKey("ghost".to_owned())
    );
    assert_eq!(harness.provider.state().hits.keys, 2);

    // The key that was held still verifies.
    let original = harness.provider.state().published_keys[0].clone();
    harness.provider.state().signing_key = original;
    signed_in(harness.login().await);
}

// ---------------------------------------------------------------- identity

#[tokio::test]
async fn an_unverified_email_is_never_the_identity() {
    let harness = Harness::new().await;
    for verified in [
        Some(json!(false)),
        None,
        Some(json!("true")),
        Some(json!(1)),
        Some(Value::Null),
    ] {
        harness.provider.state().user.email_verified = verified.clone();
        // alice@example.com is on the list, but only when verified.
        assert_eq!(
            harness.login().await.unwrap_err(),
            LoginError::NotAllowed(subject("user-1")),
            "{verified:?}"
        );
    }
}

#[tokio::test]
async fn a_subject_entry_admits_a_user_without_a_verified_email() {
    let harness = Harness::new().await;
    {
        let mut state = harness.provider.state();
        state.user.subject = "user-2".to_owned();
        state.user.email = Some("someone@example.com".to_owned());
        state.user.email_verified = None;
    }
    assert_eq!(harness.login().await.unwrap().identity, subject("user-2"));

    harness.provider.state().user.email = None;
    assert_eq!(harness.login().await.unwrap().identity, subject("user-2"));

    // Subjects are exact.
    harness.provider.state().user.subject = "USER-2".to_owned();
    assert_eq!(
        harness.login().await.unwrap_err(),
        LoginError::NotAllowed(subject("USER-2"))
    );
}

#[tokio::test]
async fn a_verified_email_is_the_identity_even_when_the_subject_is_listed() {
    let harness = Harness::new().await;
    {
        let mut state = harness.provider.state();
        state.user.subject = "user-2".to_owned();
        state.user.email = Some("bob@example.com".to_owned());
    }
    assert_eq!(
        harness.login().await.unwrap_err(),
        LoginError::NotAllowed(email("bob@example.com"))
    );
}

#[tokio::test]
async fn an_identity_that_is_not_on_the_list_is_refused() {
    let harness = Harness::new().await;
    for address in [
        "mallory@example.com",
        "alice@example.org",
        "alice@example.com.evil.example",
    ] {
        harness.provider.state().user.email = Some(address.to_owned());
        assert_eq!(
            harness.login().await.unwrap_err(),
            LoginError::NotAllowed(email(address))
        );
    }
}

#[tokio::test]
async fn email_case_does_not_matter() {
    let harness = Harness::with(Setup {
        allowed_users: "Alice@Example.COM",
        ..Setup::default()
    })
    .await;
    for address in [
        "alice@example.com",
        "ALICE@EXAMPLE.COM",
        "aLiCe@eXaMpLe.CoM",
    ] {
        harness.provider.state().user.email = Some(address.to_owned());
        assert_eq!(
            harness.login().await.unwrap().identity,
            email("alice@example.com")
        );
    }
}

#[tokio::test]
async fn a_verified_email_that_is_not_an_address_is_refused() {
    let harness = Harness::new().await;
    for address in [
        "",
        "alice",
        "sub:user-2",
        "alice@example.com, mallory@example.com",
    ] {
        harness.provider.state().user.email = Some(address.to_owned());
        harness.provider.state().user.subject = "user-2".to_owned();
        assert_eq!(
            id_token_error(harness.login().await),
            IdTokenError::BadEmail,
            "{address:?}"
        );
    }
    // An unusable subject, when the subject is the identity.
    harness.provider.state().user.email = None;
    harness.provider.state().user.subject = " user-2".to_owned();
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::BadSubject
    );
}

// --------------------------------------------------------------- the code

#[tokio::test]
async fn a_replayed_code_is_refused_by_the_provider() {
    let harness = Harness::new().await;
    let (url, reference) = harness.begin(None).await;
    let first = form_of(&harness.provider.authorize(&url).await);
    let replay = format!("code={}&state={}", first["code"], first["state"]);
    signed_in(harness.finish(&reference, &replay).await);

    // The same code under a fresh pending login of the attacker's own.
    let (url, reference) = harness.begin(None).await;
    let state = query_of(&url)["state"].clone();
    assert_eq!(
        harness
            .finish(&reference, &format!("code={}&state={state}", first["code"]))
            .await
            .unwrap_err(),
        LoginError::TokenEndpoint(
            "it answered HTTP 400 with the error \"invalid_grant\"".to_owned()
        )
    );
}

#[tokio::test]
async fn a_replayed_code_fails_on_the_nonce_even_if_the_provider_accepts_it() {
    let harness = Harness::new().await;
    harness.provider.state().allow_code_replay = true;
    // Without PKCE binding the replay would reach the nonce check; with it
    // the provider refuses the foreign verifier first. Both are refusals.
    let (url, reference) = harness.begin(None).await;
    let first = form_of(&harness.provider.authorize(&url).await);
    let replay = format!("code={}&state={}", first["code"], first["state"]);
    signed_in(harness.finish(&reference, &replay).await);
    let (url, reference) = harness.begin(None).await;
    let state = query_of(&url)["state"].clone();
    let error = harness
        .finish(&reference, &format!("code={}&state={state}", first["code"]))
        .await
        .unwrap_err();
    assert!(matches!(error, LoginError::TokenEndpoint(_)), "{error:?}");

    // A provider that also ignores PKCE hands back the first login's token:
    // its nonce is not this login's.
    harness.provider.state().token = None;
    let (url_a, reference_a) = harness.begin(None).await;
    let _ = harness.provider.authorize(&url_a).await;
    let stolen = harness.provider.state().last_id_token.clone().unwrap();
    harness.provider.state().token = Some(Misbehavior::Answer(
        200,
        json!({ "id_token": stolen }).to_string(),
    ));
    let state = query_of(&url_a)["state"].clone();
    assert_eq!(
        id_token_error(
            harness
                .finish(&reference_a, &format!("code=any&state={state}"))
                .await
        ),
        IdTokenError::WrongNonce
    );
}

// ------------------------------------------------------- the token endpoint

#[tokio::test]
async fn token_endpoint_failures_are_refusals() {
    let harness = Harness::new().await;
    harness.client.warm_up().await.unwrap();
    let too_large = "the response is larger than 1048576 bytes";
    let cases: Vec<(Misbehavior, &str)> = vec![
        (
            Misbehavior::Answer(500, "boom".to_owned()),
            "it answered HTTP 500",
        ),
        (
            Misbehavior::Answer(
                400,
                r#"{"error":"invalid_grant","error_description":"secret detail"}"#.to_owned(),
            ),
            "it answered HTTP 400 with the error \"invalid_grant\"",
        ),
        (
            Misbehavior::Answer(400, "{\"error\":\"line1\\nline2\"}".to_owned()),
            "it answered HTTP 400 with the error \"line1?line2\"",
        ),
        (
            Misbehavior::Answer(200, "<html>garbage</html>".to_owned()),
            "its answer holds no ID token",
        ),
        (
            Misbehavior::Answer(200, String::new()),
            "its answer holds no ID token",
        ),
        (
            Misbehavior::Answer(
                200,
                r#"{"access_token":"x","token_type":"Bearer"}"#.to_owned(),
            ),
            "its answer holds no ID token",
        ),
        (
            Misbehavior::Answer(200, r#"{"id_token":42}"#.to_owned()),
            "its answer holds no ID token",
        ),
        (
            Misbehavior::Answer(200, r#"["id_token"]"#.to_owned()),
            "its answer holds no ID token",
        ),
        (
            Misbehavior::Oversized {
                bytes: 2 * 1024 * 1024,
                declare_length: true,
            },
            too_large,
        ),
        (
            Misbehavior::Oversized {
                bytes: 2 * 1024 * 1024,
                declare_length: false,
            },
            too_large,
        ),
        (
            Misbehavior::Redirect("http://127.0.0.1:9/token".to_owned()),
            "it answered HTTP 302",
        ),
    ];
    for (n, (misbehavior, expected)) in cases.into_iter().enumerate() {
        harness.provider.state().token = Some(misbehavior);
        assert_eq!(
            harness.login_from(address(n as u8)).await.unwrap_err(),
            LoginError::TokenEndpoint(expected.to_owned())
        );
    }
    // Back to normal, sign-in works again.
    harness.provider.state().token = None;
    signed_in(harness.login().await);
}

#[tokio::test]
async fn a_token_endpoint_that_never_answers_times_out() {
    // The only test that waits: for the client's own timeout, shortened.
    let harness = Harness::with(Setup {
        options: Options {
            request_timeout: Duration::from_millis(1500),
            ..Options::default()
        },
        ..Setup::default()
    })
    .await;
    harness.client.warm_up().await.unwrap();
    harness.provider.state().token = Some(Misbehavior::Hang);
    assert_eq!(
        harness.login().await.unwrap_err(),
        LoginError::TokenEndpoint("no answer within 1.5s".to_owned())
    );
    harness.provider.state().token = None;
    signed_in(harness.login().await);
}

#[tokio::test]
async fn a_token_that_is_not_a_jwt_is_refused() {
    let harness = Harness::new().await;
    for token in ["", "abc", "a.b.c", "e30.e30."] {
        harness.provider.state().token = Some(Misbehavior::Answer(
            200,
            json!({ "id_token": token }).to_string(),
        ));
        assert_eq!(
            id_token_error(harness.login().await),
            IdTokenError::Malformed,
            "{token:?}"
        );
    }
}

// ---------------------------------------------------------------- discovery

#[tokio::test]
async fn discovery_for_another_issuer_is_refused() {
    let harness = Harness::new().await;
    harness.provider.state().issuer = "https://evil.example".to_owned();
    let expected = LoginError::ProviderUnavailable(
        "the discovery document is for another issuer than the one configured".to_owned(),
    );
    assert_eq!(harness.client.warm_up().await.unwrap_err(), expected);
    assert!(!harness.client.is_ready());
    // The key set of a document that was refused is not even fetched.
    assert_eq!(harness.provider.state().hits.keys, 0);
}

#[tokio::test]
async fn the_issuer_may_differ_by_one_trailing_slash() {
    // Configured without the slash, published with it (Auth0's habit).
    let harness = Harness::new().await;
    let published = format!("{}/", harness.provider.origin);
    harness.provider.state().issuer = published;
    signed_in(harness.login().await);

    // Configured with the slash, published without.
    let harness = Harness::with(Setup {
        issuer_suffix: "/",
        ..Setup::default()
    })
    .await;
    signed_in(harness.login().await);

    // Two slashes are not one.
    let harness = Harness::new().await;
    let published = format!("{}//", harness.provider.origin);
    harness.provider.state().issuer = published;
    assert!(matches!(
        harness.client.warm_up().await,
        Err(LoginError::ProviderUnavailable(_))
    ));
}

#[tokio::test]
async fn an_issuer_with_a_path_is_discovered_under_that_path() {
    let harness = Harness::with(Setup {
        issuer_suffix: "/realms/calin",
        ..Setup::default()
    })
    .await;
    let published = format!("{}/realms/calin", harness.provider.origin);
    harness.provider.state().issuer = published;
    signed_in(harness.login().await);
}

#[tokio::test]
async fn bad_discovery_answers_leave_the_provider_unavailable() {
    let cases: Vec<(Misbehavior, &str)> = vec![
        (
            Misbehavior::Answer(500, "boom".to_owned()),
            "cannot fetch the discovery document: HTTP 500",
        ),
        (
            Misbehavior::Answer(404, "{}".to_owned()),
            "cannot fetch the discovery document: HTTP 404",
        ),
        (
            Misbehavior::Answer(200, "<html>".to_owned()),
            "the discovery document cannot be read: not a JSON object",
        ),
        (
            Misbehavior::Answer(200, "{}".to_owned()),
            "the discovery document cannot be read: missing field `issuer` at line 1 column 2",
        ),
        (
            Misbehavior::Oversized {
                bytes: 2 * 1024 * 1024,
                declare_length: false,
            },
            "cannot fetch the discovery document: the response is larger than 1048576 bytes",
        ),
    ];
    for (misbehavior, expected) in cases {
        let harness = Harness::new().await;
        harness.provider.state().discovery = Some(misbehavior);
        assert_eq!(
            harness.client.warm_up().await.unwrap_err(),
            LoginError::ProviderUnavailable(expected.to_owned())
        );
        // Starting a login fails the same way: no half-working sign-in.
        harness.clock.advance(120);
        assert!(matches!(
            harness.client.begin_login(address(1), None).await,
            Err(LoginError::ProviderUnavailable(_))
        ));
    }
}

#[tokio::test]
async fn a_redirect_from_the_provider_is_not_followed() {
    // A second, perfectly valid provider, to which the first redirects.
    let elsewhere = Harness::new().await;
    let harness = Harness::new().await;
    let target = format!(
        "{}/.well-known/openid-configuration",
        elsewhere.provider.origin
    );
    harness.provider.state().discovery = Some(Misbehavior::Redirect(target));
    assert_eq!(
        harness.client.warm_up().await.unwrap_err(),
        LoginError::ProviderUnavailable("cannot fetch the discovery document: HTTP 302".to_owned())
    );
    assert_eq!(elsewhere.provider.state().hits.connections, 0);

    // The same for the key set.
    let harness = Harness::new().await;
    let target = format!("{}/keys", elsewhere.provider.origin);
    harness.provider.state().keys = Some(Misbehavior::Redirect(target));
    assert_eq!(
        harness.client.warm_up().await.unwrap_err(),
        LoginError::ProviderUnavailable("cannot fetch the key set: HTTP 302".to_owned())
    );
    assert_eq!(elsewhere.provider.state().hits.connections, 0);
}

#[tokio::test]
async fn endpoints_must_be_https_or_loopback() {
    let cases = [
        (
            "token_endpoint",
            "http://example.com/token",
            "the discovery document's token_endpoint must be an https URL",
        ),
        (
            "jwks_uri",
            "http://10.0.0.1:8080/keys",
            "the discovery document's jwks_uri must be an https URL",
        ),
        (
            "authorization_endpoint",
            "javascript:alert(1)",
            "the discovery document's authorization_endpoint must be an https URL",
        ),
    ];
    for (name, value, expected) in cases {
        let harness = Harness::new().await;
        harness
            .provider
            .state()
            .discovery_extra
            .insert(name.to_owned(), json!(value));
        assert_eq!(
            harness.client.warm_up().await.unwrap_err(),
            LoginError::ProviderUnavailable(expected.to_owned())
        );
    }
}

#[tokio::test]
async fn a_provider_without_s256_is_refused() {
    let harness = Harness::new().await;
    harness.provider.state().discovery_extra.insert(
        "code_challenge_methods_supported".to_owned(),
        json!(["plain"]),
    );
    assert_eq!(
        harness.client.warm_up().await.unwrap_err(),
        LoginError::ProviderUnavailable("the provider does not support PKCE with S256".to_owned())
    );
}

#[tokio::test]
async fn a_bad_key_set_leaves_the_provider_unavailable() {
    let harness = Harness::new().await;
    harness.provider.state().keys = Some(Misbehavior::Answer(200, "garbage".to_owned()));
    assert!(matches!(
        harness.client.warm_up().await,
        Err(LoginError::ProviderUnavailable(reason)) if reason.starts_with("the key set cannot be read")
    ));
    assert!(!harness.client.is_ready());
}

#[tokio::test]
async fn unusable_published_keys_are_ignored() {
    let harness = Harness::new().await;
    // Only an encryption key and a key without an id are published.
    let mut for_encryption = harness.provider.state().signing_key.jwk();
    for_encryption["use"] = json!("enc");
    let mut without_id = harness.provider.state().signing_key.jwk();
    without_id.as_object_mut().unwrap().remove("kid");
    let body = json!({ "keys": [for_encryption, without_id] }).to_string();
    harness.provider.state().keys = Some(Misbehavior::Answer(200, body));
    assert_eq!(
        id_token_error(harness.login().await),
        IdTokenError::UnknownKey("key-1".to_owned())
    );
}

// ------------------------------------------------- the provider being down

#[tokio::test]
async fn the_provider_down_at_startup_does_not_prevent_construction() {
    let harness = Harness::new().await;
    harness.provider.state().down = true;

    // Startup: the attempt fails, the client exists anyway.
    let error = harness.client.warm_up().await.unwrap_err();
    assert!(
        matches!(&error, LoginError::ProviderUnavailable(reason) if reason.starts_with("cannot fetch the discovery document: ")),
        "{error}"
    );
    assert_eq!(harness.provider.state().hits.connections, 1);

    // Within the backoff nothing is sent, and sign-in says so.
    let error = harness
        .client
        .begin_login(address(1), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, LoginError::ProviderUnavailable(reason) if reason.ends_with("(next attempt in 1 s)")),
        "{error}"
    );
    assert_eq!(harness.provider.state().hits.connections, 1);

    // Each failure doubles the wait: 1, 2, 4, ... capped at 60 seconds.
    let mut waits = Vec::new();
    for _ in 0..8 {
        let before = harness.clock.now();
        loop {
            harness.clock.advance(1);
            let connections = harness.provider.state().hits.connections;
            assert!(harness.client.begin_login(address(1), None).await.is_err());
            if harness.provider.state().hits.connections > connections {
                break;
            }
        }
        waits.push(harness.clock.now() - before);
    }
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60]);

    // The provider comes back: after the wait, sign-in works.
    harness.provider.state().down = false;
    assert!(harness.client.begin_login(address(1), None).await.is_err());
    harness.clock.advance(60);
    assert_eq!(
        harness.login().await.unwrap().identity,
        email("alice@example.com")
    );
    assert!(harness.client.is_ready());
}

#[tokio::test]
async fn a_provider_that_does_not_listen_at_all_is_unavailable() {
    // A port that was just free: nothing accepts on it.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let clock = Arc::new(ManualClock::new(START));
    let config = public_config(&format!("http://127.0.0.1:{port}"), None, ALLOWED);
    let client = OidcClient::new(&config, clock).unwrap();
    assert!(matches!(
        client.warm_up().await,
        Err(LoginError::ProviderUnavailable(_))
    ));
    assert!(matches!(
        client.begin_login(address(1), None).await,
        Err(LoginError::ProviderUnavailable(_))
    ));
    assert!(!client.is_ready());
}

#[tokio::test]
async fn a_clock_that_steps_back_does_not_block_discovery_forever() {
    let harness = Harness::new().await;
    harness.provider.state().down = true;
    assert!(harness.client.warm_up().await.is_err());
    harness.provider.state().down = false;
    harness.clock.set(START - 86_400);
    harness.client.warm_up().await.unwrap();
}

#[tokio::test]
async fn the_provider_going_down_later_fails_only_the_exchange() {
    let harness = Harness::new().await;
    signed_in(harness.login().await);
    harness.provider.state().down = true;
    // Starting a login needs nothing from the provider any more.
    let (url, reference) = harness.begin(None).await;
    let state = query_of(&url)["state"].clone();
    assert!(matches!(
        harness
            .finish(&reference, &format!("code=x&state={state}"))
            .await,
        Err(LoginError::TokenEndpoint(_))
    ));
}

// ------------------------------------------------------------ rate limiting

#[tokio::test]
async fn ten_failures_with_live_pending_logins_block_the_address_for_ten_minutes() {
    let harness = Harness::new().await;
    harness.client.warm_up().await.unwrap();
    let attacker = address(66);
    let junk = CallbackParams::from_query("code=x&state=y");
    for _ in 0..10 {
        // A login this server started, finished with the wrong state.
        let (_url, reference) = harness.begin_from(attacker, None).await;
        assert_eq!(
            harness
                .client
                .finish_login(attacker, Some(&reference), &junk)
                .await
                .unwrap_err(),
            LoginError::StateMismatch
        );
    }
    // The eleventh is refused whatever it carries, even a genuine callback,
    // and the genuine pending login is not consumed by the refusal.
    let (url, reference) = harness.begin(None).await;
    let query = harness.provider.authorize(&url).await;
    let genuine = CallbackParams::from_query(&query);
    assert_eq!(
        harness
            .client
            .finish_login(attacker, Some(&reference), &genuine)
            .await
            .unwrap_err(),
        LoginError::RateLimited
    );
    // So is a callback with no cookie at all.
    assert_eq!(
        harness
            .client
            .finish_login(attacker, None, &junk)
            .await
            .unwrap_err(),
        LoginError::RateLimited
    );
    assert_eq!(harness.provider.state().hits.token, 0);

    // Another address is not affected, and that callback still works there.
    signed_in(
        harness
            .client
            .finish_login(address(1), Some(&reference), &genuine)
            .await,
    );

    // Ten minutes after the failures the address may try again.
    harness.clock.advance(599);
    let (_url, reference) = harness.begin_from(attacker, None).await;
    assert_eq!(
        harness
            .client
            .finish_login(attacker, Some(&reference), &junk)
            .await
            .unwrap_err(),
        LoginError::RateLimited
    );
    harness.clock.advance(1);
    assert_eq!(
        harness.login_from(attacker).await.unwrap().identity,
        email("alice@example.com")
    );
}

#[tokio::test]
async fn junk_callbacks_without_a_live_pending_login_do_not_block_the_address() {
    let harness = Harness::new().await;
    harness.client.warm_up().await.unwrap();
    let victim = address(7);
    let junk = CallbackParams::from_query("code=x&state=y");

    // What a hostile page makes the victim's browser send: callbacks with
    // no pending-login cookie. Ten of them, and many more.
    for _ in 0..10 {
        assert_eq!(
            harness
                .client
                .finish_login(victim, None, &junk)
                .await
                .unwrap_err(),
            LoginError::NoPendingLogin
        );
    }
    signed_in(harness.login_from(victim).await);
    for _ in 0..100 {
        assert_eq!(
            harness
                .client
                .finish_login(victim, None, &junk)
                .await
                .unwrap_err(),
            LoginError::NoPendingLogin
        );
    }

    // A cookie this server never issued, one already used, one expired.
    let stranger = PendingLoginRef::generate().unwrap();
    let (url, used) = harness.begin_from(victim, None).await;
    let query = harness.provider.authorize(&url).await;
    signed_in(
        harness
            .client
            .finish_login(victim, Some(&used), &CallbackParams::from_query(&query))
            .await,
    );
    let (_url, expired) = harness.begin_from(victim, None).await;
    harness.clock.advance(600);
    for reference in [&stranger, &used, &expired] {
        for _ in 0..10 {
            assert_eq!(
                harness
                    .client
                    .finish_login(victim, Some(reference), &junk)
                    .await
                    .unwrap_err(),
                LoginError::UnknownPendingLogin
            );
        }
    }
    assert_eq!(harness.provider.state().hits.token, 2);

    // None of it counted: the victim signs in, and still has all ten
    // failures of their own before being limited.
    signed_in(harness.login_from(victim).await);
    for _ in 0..10 {
        let (_url, reference) = harness.begin_from(victim, None).await;
        assert_eq!(
            harness
                .client
                .finish_login(victim, Some(&reference), &junk)
                .await
                .unwrap_err(),
            LoginError::StateMismatch
        );
    }
    assert_eq!(
        harness.login_from(victim).await.unwrap_err(),
        LoginError::RateLimited
    );
}

#[tokio::test]
async fn successful_logins_do_not_count_and_every_failure_after_the_pending_login_does() {
    let harness = Harness::new().await;
    for _ in 0..25 {
        signed_in(harness.login().await);
    }
    // Ten failures of different kinds, from one address, each with a
    // pending login of its own.
    harness.provider.state().claim_edits = vec![("aud", Some(json!("another-client")))];
    for _ in 0..3 {
        assert_eq!(
            id_token_error(harness.login().await),
            IdTokenError::WrongAudience
        );
    }
    harness.provider.state().claim_edits.clear();
    harness.provider.state().user.email = Some("mallory@example.com".to_owned());
    assert!(matches!(
        harness.login().await,
        Err(LoginError::NotAllowed(_))
    ));
    harness.provider.state().user.email = Some("alice@example.com".to_owned());
    harness.provider.state().token = Some(Misbehavior::Answer(500, String::new()));
    assert!(matches!(
        harness.login().await,
        Err(LoginError::TokenEndpoint(_))
    ));
    harness.provider.state().token = None;
    let cases: [(&str, LoginError); 5] = [
        ("code=x&state=wrong", LoginError::StateMismatch),
        ("code=x", LoginError::StateMismatch),
        ("code=x&code=y&state=STATE", LoginError::MalformedCallback),
        ("state=STATE", LoginError::NoCode),
        (
            "error=access_denied&state=STATE",
            LoginError::ProviderRefused("access_denied".to_owned()),
        ),
    ];
    for (query, expected) in cases {
        let (url, reference) = harness.begin(None).await;
        let query = query.replace("STATE", &query_of(&url)["state"]);
        assert_eq!(
            harness.finish(&reference, &query).await.unwrap_err(),
            expected
        );
    }
    assert_eq!(harness.login().await.unwrap_err(), LoginError::RateLimited);
}

// ------------------------------------------------------------------ secrets

#[tokio::test]
async fn no_secret_appears_in_any_debug_or_display_output() {
    let client_secret = "client-secret-0123456789";
    let harness = Harness::with(Setup {
        client_secret: Some(client_secret),
        ..Setup::default()
    })
    .await;

    let start = harness
        .client
        .begin_login(address(1), Some("/x"))
        .await
        .unwrap();
    let sent = query_of(start.authorization_url());
    let reference_text = start
        .set_cookie()
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_owned();
    let reference =
        PendingLoginRef::from_cookie_headers([
            format!("__Secure-calin_login={reference_text}").as_str()
        ])
        .unwrap();
    let query = harness.provider.authorize(start.authorization_url()).await;
    let code = form_of(&query)["code"].clone();
    let params = CallbackParams::from_query(&query);

    let mut outputs = vec![
        format!("{start:?}"),
        format!("{reference:?}"),
        format!("{params:?}"),
        format!("{:?}", harness.client),
    ];

    // One refusal of each kind that handles a secret, then a success.
    harness.provider.state().claim_edits = vec![("nonce", Some(json!("other")))];
    let refused = harness.finish(&reference, &query).await.unwrap_err();
    let id_token = harness.provider.state().last_id_token.clone().unwrap();
    let (_, form) = harness.provider.state().last_token_request.clone().unwrap();
    let code_verifier = form["code_verifier"].clone();
    outputs.push(format!("{refused} {refused:?}"));

    harness.provider.state().claim_edits.clear();
    let mut failures = Vec::new();
    for misbehavior in [
        Some(Misbehavior::Answer(
            400,
            json!({ "error": "invalid_grant" }).to_string(),
        )),
        Some(Misbehavior::Answer(
            200,
            json!({ "id_token": format!("{id_token}x") }).to_string(),
        )),
        None,
    ] {
        harness.provider.state().token = misbehavior;
        harness.provider.state().signing = Signing::Hmac(client_secret.to_owned());
        let (url, reference) = harness.begin(None).await;
        let query = harness.provider.authorize(&url).await;
        failures.push(harness.finish(&reference, &query).await.unwrap_err());
    }
    harness.provider.state().signing = Signing::Key;
    harness.provider.state().user.email = Some("mallory@example.com".to_owned());
    failures.push(harness.login().await.unwrap_err());
    failures.push(
        harness
            .finish(&reference, "code=c0de&state=wrong")
            .await
            .unwrap_err(),
    );
    for failure in &failures {
        outputs.push(format!("{failure} {failure:?}"));
        assert_eq!(
            failure.public_message(),
            "Sign-in failed. Please try again."
        );
    }

    harness.provider.state().user.email = Some("alice@example.com".to_owned());
    let success = harness.login().await.unwrap();
    outputs.push(format!("{success:?}"));

    let secrets = [
        ("client secret", client_secret.to_owned()),
        ("pending-login reference", reference_text),
        ("state", sent["state"].clone()),
        ("nonce", sent["nonce"].clone()),
        ("code challenge", sent["code_challenge"].clone()),
        ("code verifier", code_verifier),
        ("authorization code", code),
        ("ID token", id_token.clone()),
        (
            "ID token signature",
            id_token.rsplit('.').next().unwrap().to_owned(),
        ),
    ];
    for output in &outputs {
        for (what, secret) in &secrets {
            assert!(
                !output.contains(secret.as_str()),
                "{what} appears in: {output}"
            );
        }
    }
    // The outputs are not empty shells: the reasons are in them.
    assert!(
        outputs
            .iter()
            .any(|o| o.contains("its nonce is missing or belongs to another login"))
    );
    assert!(
        outputs
            .iter()
            .any(|o| o.contains("mallory@example.com is not on the allow-list"))
    );
}

#[test]
fn every_error_has_the_same_public_message() {
    let errors = [
        LoginError::RateLimited,
        LoginError::NoPendingLogin,
        LoginError::UnknownPendingLogin,
        LoginError::MalformedCallback,
        LoginError::StateMismatch,
        LoginError::ProviderRefused("access_denied".to_owned()),
        LoginError::NoCode,
        LoginError::ProviderUnavailable("down".to_owned()),
        LoginError::TokenEndpoint("x".to_owned()),
        LoginError::IdToken(IdTokenError::Expired),
        LoginError::NotAllowed(email("mallory@example.com")),
        LoginError::Random(RandomError),
    ];
    for error in errors {
        assert_eq!(error.public_message(), LoginError::PUBLIC_MESSAGE);
        // The message for the log is specific, the public one is not.
        assert_ne!(error.to_string(), LoginError::PUBLIC_MESSAGE);
    }
    assert_eq!(
        LoginError::IdToken(IdTokenError::Expired).to_string(),
        "the ID token was refused: it has expired"
    );
}

#[test]
fn backoff_doubles_up_to_a_minute() {
    let waits: Vec<u64> = (1..=9).map(backoff_secs).collect();
    assert_eq!(waits, [1, 2, 4, 8, 16, 32, 60, 60, 60]);
    assert_eq!(backoff_secs(0), 1);
    assert_eq!(backoff_secs(64), 60);
    assert_eq!(backoff_secs(u32::MAX), 60);
}
