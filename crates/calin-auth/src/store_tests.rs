//! Sessions and device tokens: lifecycle, expiry, revocation and the file.

use std::sync::atomic::AtomicBool;

use super::*;
use crate::clock::ManualClock;

const START: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;
const ALLOWED: &str = "alice@example.com, bob@example.com, sub:robot-1";

struct Fixture {
    directory: tempfile::TempDir,
    clock: Arc<ManualClock>,
    store: AuthStore,
}

impl Fixture {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::new(START));
        let store = open(directory.path(), ALLOWED, &clock).await;
        Self {
            directory,
            clock,
            store,
        }
    }

    fn path(&self) -> PathBuf {
        self.directory.path().join("auth.json")
    }

    fn file(&self) -> String {
        std::fs::read_to_string(self.path()).unwrap()
    }

    fn file_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.file()).unwrap()
    }

    async fn reopen(&self, allowed: &str) -> AuthStore {
        open(self.directory.path(), allowed, &self.clock).await
    }
}

async fn open(directory: &Path, allowed: &str, clock: &Arc<ManualClock>) -> AuthStore {
    AuthStore::open(directory.join("auth.json"), allow(allowed), clock.clone())
        .await
        .unwrap()
}

fn allow(text: &str) -> AllowList {
    AllowList::parse(text).unwrap()
}

fn alice() -> Identity {
    Identity::email("alice@example.com").unwrap()
}

fn bob() -> Identity {
    Identity::email("bob@example.com").unwrap()
}

fn robot() -> Identity {
    Identity::subject("robot-1").unwrap()
}

fn session_id(authenticated: &Authenticated) -> &str {
    match &authenticated.credential {
        Credential::Session { id } => id,
        Credential::DeviceToken { .. } => panic!("expected a session"),
    }
}

/// The token as the browser holds it, read back from the `Set-Cookie` value.
fn cookie_pair(set_cookie: &str) -> &str {
    set_cookie.split(';').next().unwrap()
}

fn token_text(session: &NewSession) -> String {
    let set_cookie = session.set_cookie();
    cookie_pair(&set_cookie)
        .split_once('=')
        .unwrap()
        .1
        .to_owned()
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---------------------------------------------------------------- sessions

#[tokio::test]
async fn session_lifecycle() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    assert_eq!(store.load_report().file, LoadedFile::Missing);
    assert!(store.list_sessions(&alice()).is_empty());

    let session = store
        .create_session(&alice(), Some("Firefox/140"))
        .await
        .unwrap();
    assert_eq!(session.info.identity, alice());
    assert_eq!(session.info.created, START);
    assert_eq!(session.info.last_used, START);
    assert_eq!(session.info.user_agent, "Firefox/140");
    assert!(session.set_cookie().starts_with("__Host-calin_session="));
    assert!(
        session
            .set_cookie()
            .ends_with("; Path=/; Max-Age=2592000; HttpOnly; Secure; SameSite=Lax")
    );

    // The browser sends the cookie back.
    let set_cookie = session.set_cookie();
    let presented = SessionToken::from_cookie_headers([cookie_pair(&set_cookie)]).unwrap();
    assert_eq!(&presented, session.token());
    let authenticated = store.authenticate_session(&presented).await.unwrap();
    assert_eq!(authenticated.identity, alice());
    assert_eq!(session_id(&authenticated), session.info.id);
    assert_eq!(authenticated.set_cookie(), None);

    assert_eq!(store.list_sessions(&alice()), vec![session.info.clone()]);
    assert!(store.list_sessions(&bob()).is_empty());

    // Sign-out ends it, once.
    assert!(store.sign_out(&presented).await.unwrap());
    assert!(!store.sign_out(&presented).await.unwrap());
    assert_eq!(store.authenticate_session(&presented).await, None);
    assert!(store.list_sessions(&alice()).is_empty());
}

#[tokio::test]
async fn an_unknown_token_authenticates_nobody() {
    let fixture = Fixture::new().await;
    fixture.store.create_session(&alice(), None).await.unwrap();
    let stranger = SessionToken::generate().unwrap();
    assert_eq!(fixture.store.authenticate_session(&stranger).await, None);
    assert!(!fixture.store.sign_out(&stranger).await.unwrap());
}

#[tokio::test]
async fn a_session_is_created_only_for_an_allowed_identity() {
    let fixture = Fixture::new().await;
    let mallory = Identity::email("mallory@example.com").unwrap();
    let error = fixture
        .store
        .create_session(&mallory, None)
        .await
        .unwrap_err();
    assert!(matches!(&error, StoreError::NotAllowed(identity) if identity == &mallory));
    assert_eq!(
        error.to_string(),
        "mallory@example.com is not on the allow-list"
    );
    // A subject does not pass for an email entry with the same text.
    let lookalike = Identity::subject("alice@example.com").unwrap();
    assert!(
        fixture
            .store
            .create_session(&lookalike, None)
            .await
            .is_err()
    );
    assert!(
        fixture
            .store
            .create_device_token(&mallory, "laptop")
            .await
            .is_err()
    );
    assert!(!fixture.path().exists());
}

#[tokio::test]
async fn a_session_lasts_thirty_days_from_its_last_use() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let token = session.token().clone();

    // Unused, it is still valid one second before thirty days...
    fixture.clock.set(START + 30 * DAY - 1);
    assert!(store.authenticate_session(&token).await.is_some());
    // ...and that use moved the end: thirty days later it would have
    // expired, but it has until thirty days after the last use.
    fixture.clock.set(START + 60 * DAY - 2);
    assert!(store.authenticate_session(&token).await.is_some());
    assert_eq!(
        store.list_sessions(&alice())[0].last_used,
        START + 60 * DAY - 2
    );
    assert_eq!(store.list_sessions(&alice())[0].created, START);

    // Exactly thirty days without use: gone, also from the list.
    fixture.clock.advance(30 * DAY);
    assert!(store.list_sessions(&alice()).is_empty());
    assert_eq!(store.authenticate_session(&token).await, None);
    // And it does not come back when the clock does.
    fixture.clock.set(START);
    assert_eq!(store.authenticate_session(&token).await, None);
}

#[tokio::test]
async fn an_unused_session_expires_after_thirty_days() {
    let fixture = Fixture::new().await;
    let session = fixture.store.create_session(&alice(), None).await.unwrap();
    fixture.clock.set(START + 30 * DAY);
    assert_eq!(
        fixture.store.authenticate_session(session.token()).await,
        None
    );
}

#[tokio::test]
async fn expired_sessions_are_purged() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let old = store.create_session(&alice(), None).await.unwrap();
    fixture.clock.advance(10 * DAY);
    let recent = store.create_session(&bob(), None).await.unwrap();
    assert_eq!(store.purge_expired().await.unwrap(), 0);

    fixture.clock.set(START + 30 * DAY);
    assert_eq!(store.purge_expired().await.unwrap(), 1);
    assert_eq!(store.purge_expired().await.unwrap(), 0);
    let file = fixture.file();
    assert!(!file.contains(&old.info.id));
    assert!(file.contains(&recent.info.id));
    assert!(store.authenticate_session(recent.token()).await.is_some());
}

#[tokio::test]
async fn expired_sessions_in_the_file_are_dropped_for_good_at_startup() {
    let fixture = Fixture::new().await;
    let session = fixture.store.create_session(&alice(), None).await.unwrap();
    let kept = fixture
        .store
        .create_device_token(&alice(), "ci")
        .await
        .unwrap();
    fixture.clock.set(START + 30 * DAY);
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(
        *reopened.load_report(),
        LoadReport {
            file: LoadedFile::Loaded {
                sessions: 1,
                device_tokens: 1
            },
            permissions_corrected: false,
            revoked: 0,
            expired: 1,
        }
    );
    assert_eq!(reopened.authenticate_session(session.token()).await, None);
    // Written back at once: the file no longer has it...
    assert!(!fixture.file().contains(&session.info.id));
    assert!(fixture.file().contains(&kept.info.id));

    // ...so a later start with the clock running behind cannot revive it.
    fixture.clock.set(START + 5);
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reopened.load_report().expired, 0);
    assert_eq!(reopened.authenticate_session(session.token()).await, None);
    assert!(
        reopened
            .authenticate_device_token(&kept.token)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn a_clock_running_behind_proves_nothing_live() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let device = store.create_device_token(&alice(), "ci").await.unwrap();

    // The clock now reads earlier than when both were created (no battery,
    // no time sync yet): they are refused, kept, and not touched.
    for behind in [61, DAY, 400 * DAY, START] {
        fixture.clock.set(START - behind);
        assert_eq!(
            store.authenticate_session(session.token()).await,
            None,
            "{behind}"
        );
        assert_eq!(
            store.authenticate_device_token(&device.token).await,
            None,
            "{behind}"
        );
    }
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reopened.authenticate_session(session.token()).await, None);
    assert_eq!(store.list_sessions(&alice())[0].last_used, START);
    assert_eq!(store.list_device_tokens(&alice())[0].last_used, None);

    // Two clocks may disagree by a minute.
    fixture.clock.set(START - 60);
    assert!(store.authenticate_session(session.token()).await.is_some());
    assert!(
        store
            .authenticate_device_token(&device.token)
            .await
            .is_some()
    );
    // The clock is right again: both work as before.
    fixture.clock.set(START + 10);
    assert!(store.authenticate_session(session.token()).await.is_some());
    assert!(
        store
            .authenticate_device_token(&device.token)
            .await
            .is_some()
    );
}

#[tokio::test]
async fn the_cookie_is_reissued_at_most_once_a_day() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let token = session.token().clone();
    let reissued = |authenticated: Option<Authenticated>| {
        authenticated.unwrap().set_cookie().map(str::to_owned)
    };

    // Many uses within the first day: no re-issue.
    for seconds in [0, 1, 3600, DAY - 1] {
        fixture.clock.set(START + seconds);
        assert_eq!(
            reissued(store.authenticate_session(&token).await),
            None,
            "{seconds}"
        );
    }
    // One day after it was issued: once, with the same token and attributes.
    fixture.clock.set(START + DAY);
    assert_eq!(
        reissued(store.authenticate_session(&token).await),
        Some(session.set_cookie())
    );
    for seconds in [0, 1, DAY - 1] {
        fixture.clock.set(START + DAY + seconds);
        assert_eq!(
            reissued(store.authenticate_session(&token).await),
            None,
            "{seconds}"
        );
    }
    // A long silence, then use: once again, counted from that use.
    fixture.clock.set(START + 20 * DAY);
    assert_eq!(
        reissued(store.authenticate_session(&token).await),
        Some(session.set_cookie())
    );
    assert_eq!(reissued(store.authenticate_session(&token).await), None);
    fixture.clock.set(START + 21 * DAY - 1);
    assert_eq!(reissued(store.authenticate_session(&token).await), None);
    fixture.clock.set(START + 21 * DAY);
    assert!(reissued(store.authenticate_session(&token).await).is_some());

    // The re-issue survives a restart: it is not offered again.
    fixture.store.flush().await.unwrap();
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reissued(reopened.authenticate_session(&token).await), None);
}

#[tokio::test]
async fn the_last_use_follows_a_clock_that_steps_back() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    // The clock is twenty days ahead for a moment, and the session is used.
    fixture.clock.set(START + 20 * DAY);
    assert!(store.authenticate_session(session.token()).await.is_some());

    // The clock is right again. The session is not ended by that...
    fixture.clock.set(START + 1);
    let authenticated = store.authenticate_session(session.token()).await.unwrap();
    // ...its cookie, of unknown age now, is issued afresh...
    assert!(authenticated.set_cookie().is_some());
    // ...and it lasts thirty days from this use, not from the wrong stamp.
    assert_eq!(store.list_sessions(&alice())[0].last_used, START + 1);
    fixture.clock.set(START + 30 * DAY);
    assert_eq!(store.list_sessions(&alice()).len(), 1);
    fixture.clock.set(START + 30 * DAY + 1);
    assert!(store.list_sessions(&alice()).is_empty());
    assert_eq!(store.authenticate_session(session.token()).await, None);
}

#[tokio::test]
async fn sessions_are_listed_and_revoked_by_their_owner_only() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let first = store.create_session(&alice(), Some("phone")).await.unwrap();
    fixture.clock.advance(10);
    let second = store
        .create_session(&alice(), Some("laptop"))
        .await
        .unwrap();
    let bobs = store.create_session(&bob(), Some("desktop")).await.unwrap();

    // Most recently used first.
    let listed: Vec<String> = store
        .list_sessions(&alice())
        .into_iter()
        .map(|s| s.user_agent)
        .collect();
    assert_eq!(listed, ["laptop", "phone"]);
    fixture.clock.advance(10);
    store.authenticate_session(first.token()).await.unwrap();
    let listed: Vec<String> = store
        .list_sessions(&alice())
        .into_iter()
        .map(|s| s.user_agent)
        .collect();
    assert_eq!(listed, ["phone", "laptop"]);

    // Bob cannot revoke Alice's session, and learns nothing by trying.
    assert!(!store.revoke_session(&bob(), &first.info.id).await.unwrap());
    assert!(!store.revoke_session(&alice(), "no-such-id").await.unwrap());
    assert!(!store.revoke_session(&alice(), "").await.unwrap());
    assert!(store.authenticate_session(first.token()).await.is_some());

    assert!(
        store
            .revoke_session(&alice(), &first.info.id)
            .await
            .unwrap()
    );
    assert!(
        !store
            .revoke_session(&alice(), &first.info.id)
            .await
            .unwrap()
    );
    assert_eq!(store.authenticate_session(first.token()).await, None);
    assert!(store.authenticate_session(second.token()).await.is_some());
    assert!(store.authenticate_session(bobs.token()).await.is_some());

    // Revocation is on disk at once.
    assert!(!fixture.file().contains(&first.info.id));
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reopened.authenticate_session(first.token()).await, None);
    assert!(
        reopened
            .authenticate_session(second.token())
            .await
            .is_some()
    );
}

#[tokio::test]
async fn ids_and_the_file_reveal_nothing_about_tokens() {
    let fixture = Fixture::new().await;
    let session = fixture.store.create_session(&alice(), None).await.unwrap();
    let device = fixture
        .store
        .create_device_token(&alice(), "laptop")
        .await
        .unwrap();
    let session_token = token_text(&session);
    let device_token = device.token.expose().to_owned();

    // The id is 128 random bits of its own.
    assert_eq!(session.info.id.len(), 22);
    assert_ne!(session.info.id, device.info.id);
    assert!(!session_token.contains(&session.info.id));
    assert!(!device_token.contains(&device.info.id));
    let other = fixture.store.create_session(&alice(), None).await.unwrap();
    assert_ne!(session.info.id, other.info.id);
    assert_ne!(session_token, token_text(&other));

    // The file holds the SHA-256 of each token, never the token.
    let file = fixture.file();
    assert!(!file.contains(&session_token));
    assert!(!file.contains(&device_token));
    assert!(!file.contains(device_token.strip_prefix("calin_").unwrap()));
    let hex = |bytes: [u8; 32]| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    assert!(file.contains(&hex(crate::secret::sha256(session_token.as_bytes()))));
    assert!(file.contains(&hex(crate::secret::sha256(device_token.as_bytes()))));
    assert!(!file.contains(&hex(crate::secret::sha256(session.info.id.as_bytes()))));

    let json = fixture.file_json();
    assert_eq!(json["version"], 1);
    assert_eq!(
        json["sessions"][0]["identity"],
        serde_json::json!({"kind": "email", "value": "alice@example.com"})
    );
    assert_eq!(json["sessions"][0]["created"], START);
    assert_eq!(json["device_tokens"][0]["name"], "laptop");
    assert_eq!(
        json["device_tokens"][0]["last_used"],
        serde_json::Value::Null
    );
}

#[tokio::test]
async fn the_user_agent_is_cleaned_and_truncated() {
    let fixture = Fixture::new().await;
    let long = format!("Mozilla\r\n\u{0}\u{1b}[31m {}", "\u{e9}".repeat(500));
    let session = fixture
        .store
        .create_session(&alice(), Some(&long))
        .await
        .unwrap();
    assert_eq!(session.info.user_agent.chars().count(), 200);
    assert!(session.info.user_agent.starts_with("Mozilla[31m \u{e9}"));
    let none = fixture.store.create_session(&alice(), None).await.unwrap();
    assert_eq!(none.info.user_agent, "");
}

#[tokio::test]
async fn sessions_per_identity_are_capped_and_the_least_recently_used_goes() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let mut sessions = Vec::new();
    for _ in 0..MAX_SESSIONS_PER_IDENTITY {
        fixture.clock.advance(1);
        sessions.push(store.create_session(&alice(), None).await.unwrap());
    }
    let bobs = store.create_session(&bob(), None).await.unwrap();
    // The first one is used, so the second is now the least recently used.
    fixture.clock.advance(1);
    store
        .authenticate_session(sessions[0].token())
        .await
        .unwrap();
    store.create_session(&alice(), None).await.unwrap();

    assert_eq!(
        store.list_sessions(&alice()).len(),
        MAX_SESSIONS_PER_IDENTITY
    );
    assert!(
        store
            .authenticate_session(sessions[0].token())
            .await
            .is_some()
    );
    assert_eq!(store.authenticate_session(sessions[1].token()).await, None);
    assert!(
        store
            .authenticate_session(sessions[2].token())
            .await
            .is_some()
    );
    assert!(store.authenticate_session(bobs.token()).await.is_some());
}

// ------------------------------------------------------------ device tokens

#[tokio::test]
async fn device_token_lifecycle() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let created = store
        .create_device_token(&alice(), "  work laptop ")
        .await
        .unwrap();
    assert_eq!(created.info.name, "work laptop");
    assert_eq!(created.info.identity, alice());
    assert_eq!(created.info.created, START);
    assert_eq!(created.info.last_used, None);
    assert!(created.token.expose().starts_with("calin_"));
    assert_eq!(created.token.expose().len(), 49);

    // Sent as a bearer token.
    let header = format!("Bearer {}", created.token.expose());
    let presented = DeviceToken::from_authorization_header(&header).unwrap();
    fixture.clock.advance(90);
    let authenticated = store.authenticate_device_token(&presented).await.unwrap();
    assert_eq!(authenticated.identity, alice());
    assert_eq!(
        authenticated.credential,
        Credential::DeviceToken {
            id: created.info.id.clone()
        }
    );
    assert_eq!(authenticated.set_cookie(), None);

    // Listed with name, creation and last use; to its owner only.
    fixture.clock.advance(10);
    let second = store.create_device_token(&alice(), "ci").await.unwrap();
    let listed = store.list_device_tokens(&alice());
    assert_eq!(listed.len(), 2);
    assert_eq!(
        (
            listed[0].name.as_str(),
            listed[0].created,
            listed[0].last_used
        ),
        ("work laptop", START, Some(START + 90))
    );
    assert_eq!(
        (
            listed[1].name.as_str(),
            listed[1].created,
            listed[1].last_used
        ),
        ("ci", START + 100, None)
    );
    assert!(store.list_device_tokens(&bob()).is_empty());

    // Revoked by its owner only, and at once on disk.
    assert!(
        !store
            .revoke_device_token(&bob(), &created.info.id)
            .await
            .unwrap()
    );
    assert!(
        !store
            .revoke_device_token(&alice(), "no-such-id")
            .await
            .unwrap()
    );
    assert!(
        store
            .revoke_device_token(&alice(), &created.info.id)
            .await
            .unwrap()
    );
    assert!(
        !store
            .revoke_device_token(&alice(), &created.info.id)
            .await
            .unwrap()
    );
    assert_eq!(store.authenticate_device_token(&presented).await, None);
    assert!(
        store
            .authenticate_device_token(&second.token)
            .await
            .is_some()
    );
    assert!(!fixture.file().contains(&created.info.id));

    // Device tokens do not expire.
    fixture.clock.advance(400 * DAY);
    assert!(
        store
            .authenticate_device_token(&second.token)
            .await
            .is_some()
    );
    let reopened = fixture.reopen(ALLOWED).await;
    assert!(
        reopened
            .authenticate_device_token(&second.token)
            .await
            .is_some()
    );
    assert_eq!(reopened.authenticate_device_token(&presented).await, None);
}

#[tokio::test]
async fn an_unknown_device_token_authenticates_nobody() {
    let fixture = Fixture::new().await;
    fixture
        .store
        .create_device_token(&alice(), "ci")
        .await
        .unwrap();
    let stranger = DeviceToken::generate().unwrap();
    assert_eq!(
        fixture.store.authenticate_device_token(&stranger).await,
        None
    );
}

#[tokio::test]
async fn device_token_names_are_validated() {
    let fixture = Fixture::new().await;
    let long = "n".repeat(65);
    for name in ["", "   ", "a\nb", "tab\there", long.as_str()] {
        let error = fixture
            .store
            .create_device_token(&alice(), name)
            .await
            .unwrap_err();
        assert!(matches!(error, StoreError::InvalidName), "{name:?}");
    }
    let longest = "\u{e9}".repeat(64);
    for name in ["x", "CI runner #2", longest.as_str()] {
        assert!(
            fixture
                .store
                .create_device_token(&alice(), name)
                .await
                .is_ok(),
            "{name:?}"
        );
    }
}

#[tokio::test]
async fn device_tokens_per_identity_are_capped_by_refusing() {
    let fixture = Fixture::new().await;
    for n in 0..MAX_DEVICE_TOKENS_PER_IDENTITY {
        fixture
            .store
            .create_device_token(&alice(), &format!("token {n}"))
            .await
            .unwrap();
    }
    let error = fixture
        .store
        .create_device_token(&alice(), "one too many")
        .await
        .unwrap_err();
    assert!(matches!(error, StoreError::TooManyDeviceTokens));
    // The limit is per identity.
    fixture
        .store
        .create_device_token(&bob(), "bob's")
        .await
        .unwrap();
    assert_eq!(
        fixture.store.list_device_tokens(&alice()).len(),
        MAX_DEVICE_TOKENS_PER_IDENTITY
    );
}

// ------------------------------------------------------------ the allow-list

#[tokio::test]
async fn leaving_the_allow_list_revokes_everything_at_once() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let alices = store.create_session(&alice(), None).await.unwrap();
    let alices_token = store.create_device_token(&alice(), "laptop").await.unwrap();
    let bobs = store.create_session(&bob(), None).await.unwrap();
    let robots = store.create_device_token(&robot(), "ci").await.unwrap();

    assert_eq!(
        store
            .set_allow_list(allow("bob@example.com, sub:robot-1"))
            .await
            .unwrap(),
        2
    );
    assert_eq!(store.authenticate_session(alices.token()).await, None);
    assert_eq!(
        store.authenticate_device_token(&alices_token.token).await,
        None
    );
    assert!(store.authenticate_session(bobs.token()).await.is_some());
    assert!(
        store
            .authenticate_device_token(&robots.token)
            .await
            .is_some()
    );
    assert!(store.create_session(&alice(), None).await.is_err());
    let file = fixture.file();
    assert!(!file.contains("alice@example.com"));
    assert!(file.contains("bob@example.com"));

    // Coming back on the list does not bring the old session back.
    assert_eq!(store.set_allow_list(allow(ALLOWED)).await.unwrap(), 0);
    assert_eq!(store.authenticate_session(alices.token()).await, None);
    assert_eq!(
        store.authenticate_device_token(&alices_token.token).await,
        None
    );
}

#[tokio::test]
async fn the_allow_list_is_checked_on_every_lookup() {
    // A list that changed without the purge having run: the lookup itself
    // must notice, refuse, and revoke everything the identity holds.
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let other_session = store.create_session(&alice(), None).await.unwrap();
    let device = store.create_device_token(&alice(), "laptop").await.unwrap();
    let bobs = store.create_session(&bob(), None).await.unwrap();
    store.inner.state().allow_list = allow("bob@example.com");

    assert_eq!(store.authenticate_session(session.token()).await, None);
    // Written at once, not at the next coalesced write.
    let file = fixture.file();
    assert!(!file.contains("alice@example.com"));
    assert!(file.contains(&bobs.info.id));
    assert!(store.list_sessions(&alice()).is_empty());
    assert!(store.list_device_tokens(&alice()).is_empty());
    assert_eq!(
        store.authenticate_session(other_session.token()).await,
        None
    );
    assert_eq!(store.authenticate_device_token(&device.token).await, None);
    assert!(store.authenticate_session(bobs.token()).await.is_some());

    // The same through a device token.
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&robot(), None).await.unwrap();
    let device = store.create_device_token(&robot(), "ci").await.unwrap();
    store.inner.state().allow_list = allow("bob@example.com");
    assert_eq!(store.authenticate_device_token(&device.token).await, None);
    assert!(!fixture.file().contains("robot-1"));
    assert_eq!(store.authenticate_session(session.token()).await, None);
}

#[tokio::test]
async fn opening_with_a_shorter_allow_list_revokes_and_writes_at_once() {
    let fixture = Fixture::new().await;
    let alices = fixture.store.create_session(&alice(), None).await.unwrap();
    let robots = fixture
        .store
        .create_device_token(&robot(), "ci")
        .await
        .unwrap();
    let bobs = fixture.store.create_session(&bob(), None).await.unwrap();

    let reopened = fixture.reopen("bob@example.com").await;
    assert_eq!(
        *reopened.load_report(),
        LoadReport {
            file: LoadedFile::Loaded {
                sessions: 2,
                device_tokens: 1
            },
            permissions_corrected: false,
            revoked: 2,
            expired: 0,
        }
    );
    assert_eq!(reopened.authenticate_session(alices.token()).await, None);
    assert!(reopened.authenticate_session(bobs.token()).await.is_some());
    assert!(!fixture.file().contains("alice@example.com"));
    assert!(!fixture.file().contains("robot-1"));

    // Back on the list at the next start: still signed out.
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reopened.load_report().revoked, 0);
    assert_eq!(reopened.authenticate_session(alices.token()).await, None);
    assert_eq!(
        reopened.authenticate_device_token(&robots.token).await,
        None
    );
    assert!(reopened.authenticate_session(bobs.token()).await.is_some());
}

// ------------------------------------------------------- request credentials

/// What is tested, the `Cookie` headers, the `Authorization` headers, and
/// who the request is then.
type RequestCase<'a> = (&'a str, Vec<&'a str>, Vec<&'a str>, Option<Identity>);

#[tokio::test]
async fn request_authentication_table() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let device = store.create_device_token(&bob(), "ci").await.unwrap();
    let set_cookie = session.set_cookie();
    let cookie = cookie_pair(&set_cookie).to_owned();
    let bearer = format!("Bearer {}", device.token.expose());
    let unknown_bearer = format!("Bearer {}", DeviceToken::generate().unwrap().expose());
    let unknown_cookie = SessionToken::generate().unwrap().set_cookie();
    let unknown_cookie = cookie_pair(&unknown_cookie).to_owned();
    let with_others = format!("theme=dark; {cookie}; lang=fr");
    let conflicting = format!("{cookie}; {unknown_cookie}");

    let cases: Vec<RequestCase<'_>> = vec![
        ("nothing", vec![], vec![], None),
        ("session cookie", vec![&cookie], vec![], Some(alice())),
        (
            "cookie among others",
            vec![&with_others],
            vec![],
            Some(alice()),
        ),
        (
            "cookie in a second header",
            vec!["theme=dark", &cookie],
            vec![],
            Some(alice()),
        ),
        ("unknown cookie", vec![&unknown_cookie], vec![], None),
        (
            "two different session cookies",
            vec![&conflicting],
            vec![],
            None,
        ),
        ("device token", vec![], vec![&bearer], Some(bob())),
        ("unknown device token", vec![], vec![&unknown_bearer], None),
        // A bearer header decides alone: no fallback to the cookie.
        (
            "device token and cookie",
            vec![&cookie],
            vec![&bearer],
            Some(bob()),
        ),
        (
            "bad bearer with a good cookie",
            vec![&cookie],
            vec![&unknown_bearer],
            None,
        ),
        (
            "malformed bearer with a good cookie",
            vec![&cookie],
            vec!["Bearer"],
            None,
        ),
        (
            "bearer that is no device token",
            vec![&cookie],
            vec!["Bearer abc"],
            None,
        ),
        // Another scheme is not ours to judge: the cookie decides.
        (
            "basic auth and a cookie",
            vec![&cookie],
            vec!["Basic dXNlcjpwdw=="],
            Some(alice()),
        ),
        ("basic auth alone", vec![], vec!["Basic dXNlcjpwdw=="], None),
        (
            "two authorization headers",
            vec![&cookie],
            vec![&bearer, &bearer],
            None,
        ),
        (
            "two authorization headers, one basic",
            vec![&cookie],
            vec!["Basic dXNlcjpwdw==", &bearer],
            None,
        ),
        (
            "session token as bearer",
            vec![],
            vec![cookie.strip_prefix("__Host-calin_session=").unwrap()],
            None,
        ),
    ];
    for (what, cookies, authorization, expected) in cases {
        let authenticated = store.authenticate_request(cookies, authorization).await;
        assert_eq!(authenticated.map(|a| a.identity), expected, "{what}");
    }

    // A session token is not accepted as a bearer token, prefixed or not.
    let as_bearer = format!("Bearer calin_{}", token_text(&session));
    assert_eq!(
        store.authenticate_request([], [as_bearer.as_str()]).await,
        None
    );
    let as_cookie = format!("__Host-calin_session={}", device.token.expose());
    assert_eq!(
        store.authenticate_request([as_cookie.as_str()], []).await,
        None
    );
}

#[tokio::test]
async fn a_session_cookie_without_its_prefix_authenticates_nobody() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let token = token_text(&session);
    assert!(session.set_cookie().starts_with("__Host-calin_session="));

    for name in [
        "calin_session",
        "__Secure-calin_session",
        "__host-calin_session",
        "Host-calin_session",
    ] {
        let header = format!("{name}={token}");
        assert_eq!(
            store.authenticate_request([header.as_str()], []).await,
            None,
            "{name}"
        );
        // Nor does it sign anybody out.
        assert_eq!(
            store.sign_out_request([header.as_str()]).await.unwrap(),
            0,
            "{name}"
        );
    }
    // A plain one next to the real one changes nothing.
    let header = format!("calin_session={token}; __Host-calin_session={token}; calin_session=x");
    let authenticated = store.authenticate_request([header.as_str()], []).await;
    assert_eq!(authenticated.map(|a| a.identity), Some(alice()));
}

#[tokio::test]
async fn signing_out_ends_every_session_the_request_presents() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let mine = store.create_session(&alice(), None).await.unwrap();
    let planted = store.create_session(&bob(), None).await.unwrap();
    let untouched = store.create_session(&alice(), None).await.unwrap();
    let mine_cookie = mine.set_cookie();
    let planted_cookie = planted.set_cookie();
    let stranger = SessionToken::generate().unwrap().set_cookie();
    // Two different session cookies: nobody is authenticated...
    let header = format!(
        "{}; theme=dark; {}; {}; __Host-calin_session=junk",
        cookie_pair(&planted_cookie),
        cookie_pair(&mine_cookie),
        cookie_pair(&stranger)
    );
    assert_eq!(
        store.authenticate_request([header.as_str()], []).await,
        None
    );
    // ...but signing out ends both of them, on disk too.
    assert_eq!(store.sign_out_request([header.as_str()]).await.unwrap(), 2);
    assert_eq!(store.authenticate_session(mine.token()).await, None);
    assert_eq!(store.authenticate_session(planted.token()).await, None);
    assert!(
        store
            .authenticate_session(untouched.token())
            .await
            .is_some()
    );
    assert!(!fixture.file().contains(&mine.info.id));
    assert!(!fixture.file().contains(&planted.info.id));

    assert_eq!(store.sign_out_request([header.as_str()]).await.unwrap(), 0);
    assert_eq!(store.sign_out_request([]).await.unwrap(), 0);
    assert_eq!(store.sign_out_request(["theme=dark"]).await.unwrap(), 0);
}

// ------------------------------------------------------------------ the file

#[cfg(unix)]
#[tokio::test]
async fn the_file_is_created_with_mode_0600_and_stays_so() {
    let fixture = Fixture::new().await;
    assert!(!fixture.path().exists());
    fixture.store.create_session(&alice(), None).await.unwrap();
    assert_eq!(mode(&fixture.path()), 0o600);
    assert!(!fixture.store.load_report().permissions_corrected);

    for _ in 0..3 {
        fixture.store.create_session(&alice(), None).await.unwrap();
        assert_eq!(mode(&fixture.path()), 0o600);
    }
    // No temporary file is left behind.
    let names: Vec<String> = std::fs::read_dir(fixture.directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["auth.json"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_found_with_wider_permissions_is_corrected() {
    use std::os::unix::fs::PermissionsExt as _;
    for wide in [0o644, 0o660, 0o604, 0o640, 0o666, 0o777] {
        let fixture = Fixture::new().await;
        let session = fixture.store.create_session(&alice(), None).await.unwrap();
        std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(wide)).unwrap();

        let reopened = fixture.reopen(ALLOWED).await;
        assert!(reopened.load_report().permissions_corrected, "{wide:o}");
        assert_eq!(mode(&fixture.path()), 0o600, "{wide:o}");
        // The content was kept.
        assert!(
            reopened
                .authenticate_session(session.token())
                .await
                .is_some()
        );
    }
    // A file that is already narrow, or narrower, is left alone.
    for narrow in [0o600, 0o400] {
        let fixture = Fixture::new().await;
        fixture.store.create_session(&alice(), None).await.unwrap();
        std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(narrow)).unwrap();
        let reopened = fixture.reopen(ALLOWED).await;
        assert!(!reopened.load_report().permissions_corrected);
        assert_eq!(mode(&fixture.path()), narrow);
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_corrupt_file_with_wide_permissions_is_corrected_too() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new().await;
    std::fs::write(fixture.path(), "{ not json").unwrap();
    std::fs::set_permissions(fixture.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
    let reopened = fixture.reopen(ALLOWED).await;
    assert!(reopened.load_report().permissions_corrected);
    // The file was moved aside, and it is the moved file that is narrow.
    assert_eq!(
        mode(&fixture.directory.path().join("auth.json.corrupt")),
        0o600
    );
}

#[tokio::test]
async fn a_missing_file_starts_empty() {
    let fixture = Fixture::new().await;
    assert_eq!(
        *fixture.store.load_report(),
        LoadReport {
            file: LoadedFile::Missing,
            permissions_corrected: false,
            revoked: 0,
            expired: 0,
        }
    );
    // Opening writes nothing.
    assert!(!fixture.path().exists());
    assert_eq!(fixture.store.writes(), 0);
}

#[tokio::test]
async fn a_corrupt_file_starts_empty_and_says_so_once() {
    let hash = "0".repeat(64);
    let good_session = format!(
        r#"{{"id":"a","token_sha256":"{hash}","identity":{{"kind":"email","value":"alice@example.com"}},"created":1,"last_used":1,"cookie_issued":1,"user_agent":""}}"#
    );
    let cases: Vec<(&str, String, &str)> = vec![
        ("empty", String::new(), "not a JSON object"),
        (
            "truncated",
            r#"{"version":1,"sessions":[{"id":"abc","tok"#.to_owned(),
            "EOF while parsing",
        ),
        (
            "not json",
            "\u{0}\u{1}garbage\u{7f}".to_owned(),
            "not a JSON object",
        ),
        (
            "another shape",
            r#"["version", 1]"#.to_owned(),
            "not a JSON object",
        ),
        (
            "no version",
            r#"{"sessions":[],"device_tokens":[]}"#.to_owned(),
            "missing field `version`",
        ),
        (
            "a newer version",
            r#"{"version":2,"sessions":[],"device_tokens":[]}"#.to_owned(),
            "unsupported version 2",
        ),
        (
            "a short hash",
            format!(
                r#"{{"version":1,"sessions":[{}],"device_tokens":[]}}"#,
                good_session.replace(&hash, "abcd")
            ),
            "a hash is not 64 lowercase hex digits",
        ),
        (
            "an uppercase hash",
            format!(
                r#"{{"version":1,"sessions":[{}],"device_tokens":[]}}"#,
                good_session.replace(&hash, &"A".repeat(64))
            ),
            "a hash is not 64 lowercase hex digits",
        ),
        (
            "an identity that is not one",
            format!(
                r#"{{"version":1,"sessions":[{}],"device_tokens":[]}}"#,
                good_session.replace("alice@example.com", "not an address")
            ),
            "not a plausible email address",
        ),
    ];
    for (what, content, reason_part) in cases {
        let fixture = Fixture::new().await;
        std::fs::write(fixture.path(), &content).unwrap();
        let store = fixture.reopen(ALLOWED).await;
        match &store.load_report().file {
            LoadedFile::Corrupt { reason, kept_as } => {
                assert!(reason.contains(reason_part), "{what}: {reason}");
                assert!(
                    reason.chars().all(|c| c.is_ascii_graphic() || c == ' '),
                    "{what}: {reason:?}"
                );
                // The file is kept under another name, byte for byte...
                assert_eq!(kept_as, &fixture.directory.path().join("auth.json.corrupt"));
                assert_eq!(std::fs::read_to_string(kept_as).unwrap(), content, "{what}");
            }
            other => panic!("{what}: expected a corrupt file, got {other:?}"),
        }
        assert!(!fixture.path().exists(), "{what}");
        assert!(store.list_sessions(&alice()).is_empty());
        // ...and the store works from empty; the next start finds a good file.
        let session = store.create_session(&alice(), None).await.unwrap();
        let reopened = fixture.reopen(ALLOWED).await;
        assert_eq!(
            reopened.load_report().file,
            LoadedFile::Loaded {
                sessions: 1,
                device_tokens: 0
            },
            "{what}"
        );
        assert!(
            reopened
                .authenticate_session(session.token())
                .await
                .is_some()
        );
    }

    // The well-formed session used above does load.
    let fixture = Fixture::new().await;
    let content = format!(
        r#"{{"version":1,"sessions":[{good_session}],"device_tokens":[],"future_field":true}}"#
    );
    std::fs::write(fixture.path(), content).unwrap();
    fixture.clock.set(2);
    let store = fixture.reopen(ALLOWED).await;
    assert_eq!(store.list_sessions(&alice()).len(), 1);
}

#[tokio::test]
async fn a_file_that_cannot_be_read_is_an_error_not_an_empty_store() {
    let directory = tempfile::tempdir().unwrap();
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(START));

    // The path is a directory.
    let path = directory.path().join("auth.json");
    std::fs::create_dir(&path).unwrap();
    let error = AuthStore::open(&path, allow(ALLOWED), clock.clone())
        .await
        .unwrap_err();
    assert!(
        matches!(&error, StoreError::Io { path: p, .. } if p == &path),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .starts_with(&format!("cannot access {}", path.display()))
    );

    // The directory does not exist.
    let path = directory.path().join("missing").join("auth.json");
    assert!(matches!(
        AuthStore::open(&path, allow(ALLOWED), clock.clone()).await,
        Err(StoreError::Io { .. })
    ));
    // The "directory" is a file.
    std::fs::write(directory.path().join("file"), "x").unwrap();
    let path = directory.path().join("file").join("auth.json");
    assert!(matches!(
        AuthStore::open(&path, allow(ALLOWED), clock).await,
        Err(StoreError::Io { .. })
    ));
}

#[tokio::test]
async fn a_write_that_fails_creates_nothing_and_revokes_anyway() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let before = fixture.file();

    // Make every write fail: the temporary file's name is taken by a
    // directory that is not empty.
    let blocker = fixture.directory.path().join("auth.json.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();

    assert!(matches!(
        store.create_session(&alice(), None).await,
        Err(StoreError::Io { .. })
    ));
    assert!(matches!(
        store.create_device_token(&alice(), "ci").await,
        Err(StoreError::Io { .. })
    ));
    assert_eq!(store.list_sessions(&alice()).len(), 1);
    assert!(store.list_device_tokens(&alice()).is_empty());
    assert_eq!(fixture.file(), before);

    // A revocation holds for this process even if it could not be written,
    // and the caller is told.
    assert!(matches!(
        store.revoke_session(&alice(), &session.info.id).await,
        Err(StoreError::Io { .. })
    ));
    assert_eq!(store.authenticate_session(session.token()).await, None);
    assert_eq!(fixture.file(), before);

    // Told again on every retry, for as long as the file still holds what
    // was revoked: "there was nothing to revoke" would be a false comfort.
    assert!(matches!(
        store.revoke_session(&alice(), &session.info.id).await,
        Err(StoreError::Io { .. })
    ));
    assert!(matches!(
        store.sign_out(session.token()).await,
        Err(StoreError::Io { .. })
    ));
    assert!(matches!(
        store.revoke_device_token(&alice(), "no-such-id").await,
        Err(StoreError::Io { .. })
    ));
    assert!(matches!(
        store.purge_expired().await,
        Err(StoreError::Io { .. })
    ));
    assert!(matches!(store.flush().await, Err(StoreError::Io { .. })));
    assert_eq!(fixture.file(), before);

    // Once writing works again, the retry itself writes what was owed.
    std::fs::remove_dir_all(&blocker).unwrap();
    assert!(
        !store
            .revoke_session(&alice(), &session.info.id)
            .await
            .unwrap()
    );
    assert!(!fixture.file().contains(&session.info.id));
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reopened.authenticate_session(session.token()).await, None);
}

#[tokio::test]
async fn a_revocation_that_could_not_be_written_is_retried_by_later_requests() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let device = store.create_device_token(&alice(), "ci").await.unwrap();
    let blocker = fixture.directory.path().join("auth.json.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();
    assert!(
        store
            .revoke_device_token(&alice(), &device.info.id)
            .await
            .is_err()
    );
    assert!(fixture.file().contains(&device.info.id));
    std::fs::remove_dir_all(&blocker).unwrap();

    // Nobody retries by hand. Thirty seconds later any request does, even
    // one that presents nothing valid.
    fixture.clock.advance(29);
    assert_eq!(store.authenticate_device_token(&device.token).await, None);
    assert!(fixture.file().contains(&device.info.id));
    fixture.clock.advance(1);
    assert_eq!(store.authenticate_device_token(&device.token).await, None);
    assert!(!fixture.file().contains(&device.info.id));
    assert!(fixture.file().contains(&session.info.id));
}

#[tokio::test]
async fn a_directory_that_cannot_be_written_is_an_error_at_startup() {
    let directory = tempfile::tempdir().unwrap();
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(START));
    let blocker = directory.path().join("auth.json.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();
    let path = directory.path().join("auth.json");
    assert!(matches!(
        AuthStore::open(&path, allow(ALLOWED), clock.clone()).await,
        Err(StoreError::Io { .. })
    ));
    std::fs::remove_dir_all(&blocker).unwrap();
    assert!(AuthStore::open(&path, allow(ALLOWED), clock).await.is_ok());
    // The probe left nothing behind.
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn a_crash_between_write_and_rename_leaves_the_old_file_intact() {
    let fixture = Fixture::new().await;
    let session = fixture.store.create_session(&alice(), None).await.unwrap();
    let before = fixture.file();

    // The first half of a write, then "the process dies": a partial new
    // file sits next to the real one, which nothing has touched.
    let temporary = write_temporary(
        &fixture.path(),
        b"{\"version\":1,\"sessions\":[{\"id\":\"half",
    )
    .unwrap();
    assert_eq!(temporary, fixture.directory.path().join("auth.json.tmp"));
    assert_eq!(fixture.file(), before);

    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(
        reopened.load_report().file,
        LoadedFile::Loaded {
            sessions: 1,
            device_tokens: 0
        }
    );
    assert!(
        reopened
            .authenticate_session(session.token())
            .await
            .is_some()
    );

    // The leftover does not get in the way of the next write, and is gone
    // after it.
    reopened.create_session(&bob(), None).await.unwrap();
    assert!(!temporary.exists());
    assert_eq!(fixture.file_json()["sessions"].as_array().unwrap().len(), 2);
}

#[cfg(unix)]
#[tokio::test]
async fn a_leftover_temporary_file_does_not_lend_its_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let fixture = Fixture::new().await;
    let leftover = fixture.directory.path().join("auth.json.tmp");
    std::fs::write(&leftover, "stale").unwrap();
    std::fs::set_permissions(&leftover, std::fs::Permissions::from_mode(0o666)).unwrap();
    fixture.store.create_session(&alice(), None).await.unwrap();
    assert_eq!(mode(&fixture.path()), 0o600);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_never_sees_a_partial_file() {
    let fixture = Fixture::new().await;
    fixture
        .store
        .create_session(&alice(), Some("first"))
        .await
        .unwrap();
    let path = fixture.path();
    let stop = Arc::new(AtomicBool::new(false));

    let reader = {
        let (path, stop) = (path.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut reads = 0u32;
            let mut most_sessions = 0;
            while !stop.load(Ordering::SeqCst) {
                // Whenever the file is read, it is a whole, valid store.
                let bytes = std::fs::read(&path).expect("the file always exists");
                let file = parse_file(&bytes).unwrap_or_else(|reason| {
                    panic!("read {} bytes that are not a store: {reason}", bytes.len())
                });
                most_sessions = most_sessions.max(file.sessions.len());
                reads += 1;
            }
            (reads, most_sessions)
        })
    };

    let agent = "a".repeat(200);
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let store = fixture.store.clone();
        let agent = agent.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..25 {
                let identity = if rand_bit() { alice() } else { bob() };
                let session = store.create_session(&identity, Some(&agent)).await.unwrap();
                if rand_bit() {
                    store.sign_out(session.token()).await.unwrap();
                }
            }
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    stop.store(true, Ordering::SeqCst);
    let (reads, most_sessions) = reader.join().unwrap();
    assert!(reads > 0);
    assert!(most_sessions >= 1);
}

fn rand_bit() -> bool {
    crate::secret::random_bytes::<1>().unwrap()[0] & 1 == 1
}

// ------------------------------------------------------------- coalescing

#[tokio::test]
async fn last_used_updates_are_written_at_most_every_thirty_seconds() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let device = store.create_device_token(&alice(), "ci").await.unwrap();
    assert_eq!(store.writes(), 2);
    let last_used_on_disk = |fixture: &Fixture| {
        fixture.file_json()["sessions"][0]["last_used"]
            .as_u64()
            .unwrap()
    };

    // A request every second for 29 seconds: nothing is written.
    for _ in 0..29 {
        fixture.clock.advance(1);
        store.authenticate_session(session.token()).await.unwrap();
        store
            .authenticate_device_token(&device.token)
            .await
            .unwrap();
    }
    assert_eq!(store.writes(), 2);
    assert_eq!(last_used_on_disk(&fixture), START);
    assert_eq!(store.list_sessions(&alice())[0].last_used, START + 29);

    // The thirtieth second: one write, carrying both updates.
    fixture.clock.advance(1);
    store.authenticate_session(session.token()).await.unwrap();
    assert_eq!(store.writes(), 3);
    assert_eq!(last_used_on_disk(&fixture), START + 30);
    assert_eq!(
        fixture.file_json()["device_tokens"][0]["last_used"],
        START + 29
    );
    store
        .authenticate_device_token(&device.token)
        .await
        .unwrap();
    for _ in 0..100 {
        store.authenticate_session(session.token()).await.unwrap();
    }
    assert_eq!(store.writes(), 3);

    // Ten minutes of traffic: one write per thirty seconds, no more.
    for _ in 0..600 {
        fixture.clock.advance(1);
        store.authenticate_session(session.token()).await.unwrap();
    }
    assert_eq!(store.writes(), 3 + 20);

    // Nothing changed: nothing written, however long the wait.
    fixture.clock.advance(1000);
    assert!(
        store
            .authenticate_session(&SessionToken::generate().unwrap())
            .await
            .is_none()
    );
    assert_eq!(store.writes(), 23);
}

#[tokio::test]
async fn a_failed_write_is_not_retried_on_every_request() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let blocker = fixture.directory.path().join("auth.json.tmp");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("x"), "x").unwrap();

    // The write that is due fails. The request is served all the same.
    fixture.clock.advance(30);
    assert!(store.authenticate_session(session.token()).await.is_some());
    assert_eq!(store.writes(), 1);

    // Writing works again, but the failed attempt used up the interval.
    std::fs::remove_dir_all(&blocker).unwrap();
    fixture.clock.advance(29);
    assert!(store.authenticate_session(session.token()).await.is_some());
    assert_eq!(store.writes(), 1);
    fixture.clock.advance(1);
    assert!(store.authenticate_session(session.token()).await.is_some());
    assert_eq!(store.writes(), 2);
    assert_eq!(fixture.file_json()["sessions"][0]["last_used"], START + 60);
}

#[tokio::test]
async fn creations_and_revocations_are_written_at_once() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let a = store.create_session(&alice(), None).await.unwrap();
    assert_eq!(store.writes(), 1);
    let b = store.create_session(&alice(), None).await.unwrap();
    assert_eq!(store.writes(), 2);
    let token = store.create_device_token(&alice(), "ci").await.unwrap();
    assert_eq!(store.writes(), 3);
    store.revoke_session(&alice(), &a.info.id).await.unwrap();
    assert_eq!(store.writes(), 4);
    store.sign_out(b.token()).await.unwrap();
    assert_eq!(store.writes(), 5);
    store
        .revoke_device_token(&alice(), &token.info.id)
        .await
        .unwrap();
    assert_eq!(store.writes(), 6);
    // What found nothing writes nothing.
    store.revoke_session(&alice(), &a.info.id).await.unwrap();
    store.sign_out(b.token()).await.unwrap();
    store.revoke_device_token(&alice(), "nope").await.unwrap();
    store.set_allow_list(allow(ALLOWED)).await.unwrap();
    store.purge_expired().await.unwrap();
    assert_eq!(store.writes(), 6);
}

#[tokio::test]
async fn flush_writes_pending_updates_and_only_those() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    store.flush().await.unwrap();
    assert_eq!(store.writes(), 1);

    fixture.clock.advance(5);
    store.authenticate_session(session.token()).await.unwrap();
    assert_eq!(store.writes(), 1);
    store.flush().await.unwrap();
    assert_eq!(store.writes(), 2);
    assert_eq!(fixture.file_json()["sessions"][0]["last_used"], START + 5);
    store.flush().await.unwrap();
    assert_eq!(store.writes(), 2);

    // Without a flush, a restart loses at most the unwritten updates: the
    // session is still there, with its older last use.
    fixture.clock.advance(5);
    store.authenticate_session(session.token()).await.unwrap();
    let reopened = fixture.reopen(ALLOWED).await;
    assert_eq!(reopened.list_sessions(&alice())[0].last_used, START + 5);
}

// ------------------------------------------------------------- concurrency

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_use_from_many_tasks() {
    const TASKS: usize = 18;
    let fixture = Fixture::new().await;
    let identities = [alice(), bob(), robot()];
    let mut tasks = Vec::new();
    for task in 0..TASKS {
        let store = fixture.store.clone();
        let clock = fixture.clock.clone();
        let identity = identities[task % 3].clone();
        tasks.push(tokio::spawn(async move {
            let mut kept = Vec::new();
            for round in 0..12usize {
                let session = store
                    .create_session(&identity, Some("agent"))
                    .await
                    .unwrap();
                let device = store
                    .create_device_token(&identity, &format!("t{task}-{round}"))
                    .await
                    .unwrap();
                clock.advance(1);
                // Every credential works as soon as it exists, whatever the
                // other tasks are doing.
                let via_session = store.authenticate_session(session.token()).await.unwrap();
                assert_eq!(via_session.identity, identity);
                let via_device = store
                    .authenticate_device_token(&device.token)
                    .await
                    .unwrap();
                assert_eq!(via_device.identity, identity);
                assert!(
                    store
                        .list_sessions(&identity)
                        .iter()
                        .any(|s| s.id == session.info.id)
                );

                assert!(
                    store
                        .revoke_device_token(&identity, &device.info.id)
                        .await
                        .unwrap()
                );
                assert!(
                    store
                        .authenticate_device_token(&device.token)
                        .await
                        .is_none()
                );
                if round % 2 == 0 {
                    assert!(store.sign_out(session.token()).await.unwrap());
                    assert!(store.authenticate_session(session.token()).await.is_none());
                } else if kept.len() < 3 {
                    kept.push(session);
                } else {
                    assert!(
                        store
                            .revoke_session(&identity, &session.info.id)
                            .await
                            .unwrap()
                    );
                }
            }
            (identity, kept)
        }));
    }
    let mut kept = Vec::new();
    for task in tasks {
        kept.push(task.await.unwrap());
    }
    fixture.store.flush().await.unwrap();

    // Memory, the file, and a store opened from the file all agree.
    let expected: usize = kept.iter().map(|(_, sessions)| sessions.len()).sum();
    assert_eq!(expected, TASKS * 3);
    let in_memory: usize = identities
        .iter()
        .map(|i| fixture.store.list_sessions(i).len())
        .sum();
    assert_eq!(in_memory, expected);
    let json = fixture.file_json();
    assert_eq!(json["sessions"].as_array().unwrap().len(), expected);
    assert_eq!(json["device_tokens"].as_array().unwrap().len(), 0);

    let reopened = fixture.reopen(ALLOWED).await;
    for (identity, sessions) in &kept {
        for session in sessions {
            let authenticated = reopened
                .authenticate_session(session.token())
                .await
                .unwrap();
            assert_eq!(&authenticated.identity, identity);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
async fn concurrent_lookups_coalesce_into_one_write() {
    let fixture = Fixture::new().await;
    let session = fixture.store.create_session(&alice(), None).await.unwrap();
    fixture.clock.advance(30);
    let mut tasks = Vec::new();
    for _ in 0..64 {
        let store = fixture.store.clone();
        let token = session.token().clone();
        tasks.push(tokio::spawn(async move {
            store.authenticate_session(&token).await.is_some()
        }));
    }
    for task in tasks {
        assert!(task.await.unwrap());
    }
    assert_eq!(fixture.store.writes(), 2);
}

// ------------------------------------------------------------------ secrets

#[tokio::test]
async fn no_secret_appears_in_any_debug_or_display_output() {
    let fixture = Fixture::new().await;
    let store = &fixture.store;
    let session = store.create_session(&alice(), None).await.unwrap();
    let device = store.create_device_token(&alice(), "laptop").await.unwrap();
    let session_secret = token_text(&session);
    let device_secret = device.token.expose().to_owned();

    fixture.clock.advance(DAY);
    let reissued = store.authenticate_session(session.token()).await.unwrap();
    assert!(reissued.set_cookie().unwrap().contains(&session_secret));
    let via_device = store
        .authenticate_device_token(&device.token)
        .await
        .unwrap();
    let mallory = Identity::email("mallory@example.com").unwrap();
    let error = store.create_session(&mallory, None).await.unwrap_err();

    let outputs = [
        format!("{session:?}"),
        format!("{:?}", session.token()),
        format!("{device:?}"),
        format!("{:?}", device.token),
        format!("{reissued:?}"),
        format!("{via_device:?}"),
        format!("{store:?}"),
        format!("{:?}", store.load_report()),
        format!("{:?}", store.list_sessions(&alice())),
        format!("{:?}", store.list_device_tokens(&alice())),
        format!("{error} {error:?}"),
    ];
    for output in &outputs {
        assert!(
            !output.contains(&session_secret),
            "session token in: {output}"
        );
        assert!(
            !output.contains(&device_secret),
            "device token in: {output}"
        );
        assert!(
            !output.contains(device_secret.strip_prefix("calin_").unwrap()),
            "device token in: {output}"
        );
    }
    assert!(
        outputs[4].contains("set_cookie: Some(\"REDACTED\")"),
        "{}",
        outputs[4]
    );
    assert!(
        outputs[0].contains("SessionToken(REDACTED)"),
        "{}",
        outputs[0]
    );
    assert!(
        outputs[2].contains("DeviceToken(REDACTED)"),
        "{}",
        outputs[2]
    );
}
