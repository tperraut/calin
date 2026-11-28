//! Sessions and device tokens, persisted in one file (spec sections 3 and 4).
//!
//! The file holds SHA-256 hashes, never a token. All state lives in memory
//! behind a mutex and the file is a copy of it: rewritten at once on every
//! creation and revocation, and at most every 30 seconds for "last used"
//! updates, always as a new file renamed over the old one.

use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use crate::clock::Clock;
use crate::cookie::{DeviceToken, SESSION_LIFETIME_SECS, SessionToken, is_bearer};
use crate::identity::{AllowList, Identity};
use crate::json::parse_object;
use crate::secret::{RandomError, constant_time_eq, random_id};

/// The session cookie is re-issued at most this often: once a day.
const COOKIE_REISSUE_SECS: u64 = 24 * 60 * 60;
/// "Last used" updates reach the disk at most this often.
const TOUCH_WRITE_INTERVAL_SECS: u64 = 30;
/// Sessions kept per identity; the least recently used goes first.
const MAX_SESSIONS_PER_IDENTITY: usize = 100;
/// Device tokens per identity; creation is refused past this, because a
/// token silently evicted would break a script somewhere.
const MAX_DEVICE_TOKENS_PER_IDENTITY: usize = 100;
/// How far in the future a record's creation may lie before the clock is
/// taken to be wrong: the allowance ID tokens get.
const CLOCK_TOLERANCE_SECS: u64 = 60;
const MAX_USER_AGENT_CHARS: usize = 200;
/// The contract's limit (`CreateDeviceTokenRequest::name`).
const MAX_TOKEN_NAME_CHARS: usize = 80;
const FILE_VERSION: u32 = 1;

/// The store of sessions and device tokens. Cheap to clone; clones share
/// one state.
///
/// Its `async` methods hand file writes to tokio's blocking pool, so they
/// need a tokio runtime and never block it.
#[derive(Clone)]
pub struct AuthStore {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    /// Held for the whole of a write, so files reach the disk in the order
    /// their snapshots were taken. Always taken before `state`.
    writer: Mutex<()>,
    /// Generation of the state the file currently holds.
    persisted: AtomicU64,
    /// Generation of the latest creation or revocation: what must reach the
    /// file even if the write made for it failed.
    owed: AtomicU64,
    /// When a write was last attempted. Failed attempts count, so a disk
    /// that refuses writes is not retried on every request.
    last_attempt: AtomicU64,
    writes: AtomicU64,
    load_report: LoadReport,
}

struct State {
    sessions: Vec<SessionRecord>,
    device_tokens: Vec<DeviceTokenRecord>,
    allow_list: AllowList,
    /// Incremented by every change, so a writer knows what is on disk.
    generation: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct SessionRecord {
    id: String,
    #[serde(with = "hex_hash")]
    token_sha256: [u8; 32],
    identity: Identity,
    created: u64,
    last_used: u64,
    cookie_issued: u64,
    user_agent: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct DeviceTokenRecord {
    id: String,
    #[serde(with = "hex_hash")]
    token_sha256: [u8; 32],
    identity: Identity,
    name: String,
    created: u64,
    last_used: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    sessions: Vec<SessionRecord>,
    device_tokens: Vec<DeviceTokenRecord>,
}

/// A session as listed to its owner. The id is random and unrelated to the
/// token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// What `DELETE /api/v1/auth/sessions/{id}` takes.
    pub id: String,
    /// Who is signed in.
    pub identity: Identity,
    /// When it was created, in seconds since the Unix epoch.
    pub created: u64,
    /// When it was last used.
    pub last_used: u64,
    /// The browser's user agent at sign-in, truncated.
    pub user_agent: String,
}

/// A device token as listed to its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceTokenInfo {
    /// What `DELETE /api/v1/tokens/{id}` takes.
    pub id: String,
    /// Who created it.
    pub identity: Identity,
    /// The name its creator gave it.
    pub name: String,
    /// When it was created, in seconds since the Unix epoch.
    pub created: u64,
    /// When it last authenticated a request, if ever.
    pub last_used: Option<u64>,
}

/// A session that was just created. The token exists only here and in the
/// browser: the store kept its hash.
#[derive(Debug)]
pub struct NewSession {
    /// The session as it will be listed.
    pub info: SessionInfo,
    token: SessionToken,
}

impl NewSession {
    /// The `Set-Cookie` value that signs the browser in.
    pub fn set_cookie(&self) -> String {
        self.token.set_cookie()
    }

    /// The token, for a caller that authenticates with it directly.
    pub fn token(&self) -> &SessionToken {
        &self.token
    }
}

/// A device token that was just created. Show [`DeviceToken::expose`] once;
/// it cannot be recovered afterwards.
#[derive(Debug)]
pub struct NewDeviceToken {
    /// The token as it will be listed.
    pub info: DeviceTokenInfo,
    /// The secret.
    pub token: DeviceToken,
}

/// The outcome of authenticating a request.
#[derive(Clone, PartialEq, Eq)]
pub struct Authenticated {
    /// Who the request acts as.
    pub identity: Identity,
    /// What proved it.
    pub credential: Credential,
    set_cookie: Option<String>,
}

impl Authenticated {
    /// A `Set-Cookie` value to add to the response when the session cookie
    /// is due for its daily re-issue. Always absent for a device token.
    pub fn set_cookie(&self) -> Option<&str> {
        self.set_cookie.as_deref()
    }
}

impl fmt::Debug for Authenticated {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Authenticated")
            .field("identity", &self.identity)
            .field("credential", &self.credential)
            .field("set_cookie", &self.set_cookie.as_ref().map(|_| "REDACTED"))
            .finish()
    }
}

/// The credential a request was authenticated with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    /// The session cookie.
    Session {
        /// The session's id, as listed.
        id: String,
    },
    /// A device token.
    DeviceToken {
        /// The token's id, as listed.
        id: String,
    },
}

/// What [`AuthStore::open`] found. Log it once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    /// The state of the file.
    pub file: LoadedFile,
    /// The file was readable by others and has been set back to mode 0600.
    pub permissions_corrected: bool,
    /// Sessions and tokens dropped because their identity is no longer on
    /// the allow-list.
    pub revoked: usize,
    /// Sessions dropped because they had expired.
    pub expired: usize,
}

/// The state of the store's file at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedFile {
    /// No file yet: a first start.
    Missing,
    /// The file was read.
    Loaded {
        /// Sessions it held, expired ones included.
        sessions: usize,
        /// Device tokens it held.
        device_tokens: usize,
    },
    /// The file could not be understood. The store starts empty (everyone
    /// signs in again, device tokens are gone) and the file was moved aside
    /// for whoever wants to look at it.
    Corrupt {
        /// Why, safe to log.
        reason: String,
        /// Where the file is now: next to the store, with `.corrupt` added.
        kept_as: PathBuf,
    },
}

/// A store operation failed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The file could not be read or written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// The store's file.
        path: PathBuf,
        /// The underlying error.
        source: io::Error,
    },
    /// The identity may not sign in, so nothing is created for it.
    #[error("{0} is not on the allow-list")]
    NotAllowed(Identity),
    /// A device token name must be 1 to 80 characters, none of them control
    /// characters.
    #[error("the device token name is empty, too long or has a control character")]
    InvalidName,
    /// The identity already has as many device tokens as allowed.
    #[error("this identity already has {MAX_DEVICE_TOKENS_PER_IDENTITY} device tokens")]
    TooManyDeviceTokens,
    /// No randomness, no token.
    #[error(transparent)]
    Random(#[from] RandomError),
    /// The blocking task doing the write was cancelled (runtime shutdown).
    #[error("the task writing the store did not complete")]
    Task,
}

/// Which write a caller needs.
#[derive(Clone, Copy)]
enum Persist {
    /// The state at this generation must be on disk before returning.
    Now(u64),
    /// Write only if something changed and the last write is old enough.
    IfDue,
}

impl AuthStore {
    /// Opens the store kept in the file at `path`, whose directory must exist
    /// and be writable.
    ///
    /// A missing file is a first start. A file that cannot be parsed is
    /// moved aside and the store starts empty, said once in
    /// [`AuthStore::load_report`]. A file other users can read is set back
    /// to mode 0600. Expired sessions, and sessions and tokens of identities
    /// that are not on `allow_list`, are dropped, and that is written back
    /// at once: neither a clock that later runs behind nor re-adding the
    /// identity revives them. An error here is a file that exists but
    /// cannot be read, or a directory that cannot be written: starting
    /// then would lose state, or keep none.
    ///
    /// Open one store per file. Two would overwrite each other's writes.
    pub async fn open(
        path: impl Into<PathBuf>,
        allow_list: AllowList,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        let path = path.into();
        tokio::task::spawn_blocking(move || Self::open_blocking(path, allow_list, clock))
            .await
            .map_err(|_| StoreError::Task)?
    }

    fn open_blocking(
        path: PathBuf,
        allow_list: AllowList,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        // Found out now rather than at the first sign-in or, worse, at the
        // first revocation: a store that cannot write keeps nothing.
        let probe = write_temporary(&path, b"").map_err(io_error(&path))?;
        std::fs::remove_file(probe).map_err(io_error(&path))?;

        let now = clock.now();
        let loaded = load(&path)?;
        let mut report = LoadReport {
            file: loaded.file,
            permissions_corrected: loaded.permissions_corrected,
            revoked: 0,
            expired: 0,
        };
        let mut state = State {
            sessions: loaded.sessions,
            device_tokens: loaded.device_tokens,
            allow_list,
            generation: 0,
        };
        report.expired = state.drop_expired(now);
        report.revoked = state.drop_not_allowed();

        // What was dropped is one change the file does not have yet.
        let dropped = report.expired + report.revoked > 0;
        let store = Self {
            inner: Arc::new(Inner {
                path,
                clock,
                persisted: AtomicU64::new(0),
                owed: AtomicU64::new(0),
                last_attempt: AtomicU64::new(now),
                writes: AtomicU64::new(0),
                state: Mutex::new(State {
                    generation: u64::from(dropped),
                    ..state
                }),
                writer: Mutex::new(()),
                load_report: report,
            }),
        };
        if dropped {
            store.inner.persist_blocking(Persist::Now(1))?;
        }
        Ok(store)
    }

    /// What was found when the store was opened.
    pub fn load_report(&self) -> &LoadReport {
        &self.inner.load_report
    }

    /// Creates a session for `identity`, which must be on the allow-list.
    ///
    /// The session is on disk when this returns; if it cannot be written it
    /// is not created.
    pub async fn create_session(
        &self,
        identity: &Identity,
        user_agent: Option<&str>,
    ) -> Result<NewSession, StoreError> {
        let token = SessionToken::generate()?;
        let now = self.inner.clock.now();
        let record = SessionRecord {
            id: random_id()?,
            token_sha256: token.hash(),
            identity: identity.clone(),
            created: now,
            last_used: now,
            cookie_issued: now,
            user_agent: clean_text(user_agent.unwrap_or(""), MAX_USER_AGENT_CHARS),
        };
        let info = record.info();
        let generation = {
            let mut state = self.inner.state();
            if !state.allow_list.allows(identity) {
                return Err(StoreError::NotAllowed(identity.clone()));
            }
            state.drop_expired(now);
            let owned = |s: &&SessionRecord| &s.identity == identity;
            while state.sessions.iter().filter(owned).count() >= MAX_SESSIONS_PER_IDENTITY {
                let oldest = state
                    .sessions
                    .iter()
                    .filter(owned)
                    .min_by_key(|s| s.last_used)
                    .map(|s| s.id.clone());
                state.sessions.retain(|s| Some(&s.id) != oldest.as_ref());
            }
            state.sessions.push(record);
            state.changed()
        };
        if let Err(error) = self.persist(Persist::Now(generation)).await {
            let mut state = self.inner.state();
            state.sessions.retain(|s| s.id != info.id);
            state.changed();
            return Err(error);
        }
        Ok(NewSession { info, token })
    }

    /// Authenticates a request from its headers, in one call.
    ///
    /// `cookie_headers` and `authorization_headers` are every value of the
    /// request's `Cookie` and `Authorization` headers. An `Authorization:
    /// Bearer` header must be a valid device token and the cookie is then
    /// not consulted; any other scheme is ignored and the session cookie
    /// decides. More than one `Authorization` header is refused.
    pub async fn authenticate_request<'a, 'b>(
        &self,
        cookie_headers: impl IntoIterator<Item = &'a str>,
        authorization_headers: impl IntoIterator<Item = &'b str>,
    ) -> Option<Authenticated> {
        let mut authorization = authorization_headers.into_iter();
        match (authorization.next(), authorization.next()) {
            (Some(_), Some(_)) => None,
            (Some(header), None) if is_bearer(header) => {
                let token = DeviceToken::from_authorization_header(header)?;
                self.authenticate_device_token(&token).await
            }
            _ => {
                let token = SessionToken::from_cookie_headers(cookie_headers)?;
                self.authenticate_session(&token).await
            }
        }
    }

    /// Looks a session up by its token and extends it.
    ///
    /// Returns nothing for an unknown or expired token, for a session whose
    /// identity has left the allow-list (everything that identity holds is
    /// then revoked), and for a session created "in the future": the clock
    /// is then behind, and a clock that is behind proves no session live.
    pub async fn authenticate_session(&self, token: &SessionToken) -> Option<Authenticated> {
        let hash = token.hash();
        let now = self.inner.clock.now();
        let (found, revoked_generation) = {
            let mut state = self.inner.state();
            // An index is used under the lock it was found under: in bounds.
            match find_by_hash(&state.sessions, &hash, |s| &s.token_sha256) {
                None => (None, None),
                Some(index) if from_the_future(state.sessions[index].created, now) => (None, None),
                Some(index) if state.sessions[index].expired(now) => {
                    state.sessions.remove(index);
                    state.changed();
                    (None, None)
                }
                Some(index) if !state.allow_list.allows(&state.sessions[index].identity) => {
                    let identity = state.sessions[index].identity.clone();
                    state.drop_identity(&identity);
                    (None, Some(state.changed()))
                }
                Some(index) => {
                    let session = &mut state.sessions[index];
                    // The last use follows the clock even backwards: a stamp
                    // left in the future by a clock that was wrong would
                    // otherwise keep the session alive long after.
                    let mut changed = now != session.last_used;
                    session.last_used = now;
                    let since_issue = now.checked_sub(session.cookie_issued);
                    let reissue = since_issue.is_none_or(|age| age >= COOKIE_REISSUE_SECS);
                    if reissue {
                        session.cookie_issued = now;
                        changed = true;
                    }
                    let authenticated = Authenticated {
                        identity: session.identity.clone(),
                        credential: Credential::Session {
                            id: session.id.clone(),
                        },
                        set_cookie: reissue.then(|| token.set_cookie()),
                    };
                    if changed {
                        state.changed();
                    }
                    (Some(authenticated), None)
                }
            }
        };
        self.persist_after_lookup(revoked_generation).await;
        found
    }

    /// Looks a device token up and records its use. Same refusals as
    /// [`AuthStore::authenticate_session`], except that a device token does
    /// not expire.
    pub async fn authenticate_device_token(&self, token: &DeviceToken) -> Option<Authenticated> {
        let hash = token.hash();
        let now = self.inner.clock.now();
        let (found, revoked_generation) = {
            let mut state = self.inner.state();
            match find_by_hash(&state.device_tokens, &hash, |t| &t.token_sha256) {
                None => (None, None),
                Some(index) if from_the_future(state.device_tokens[index].created, now) => {
                    (None, None)
                }
                Some(index)
                    if !state
                        .allow_list
                        .allows(&state.device_tokens[index].identity) =>
                {
                    let identity = state.device_tokens[index].identity.clone();
                    state.drop_identity(&identity);
                    (None, Some(state.changed()))
                }
                Some(index) => {
                    let record = &mut state.device_tokens[index];
                    let authenticated = Authenticated {
                        identity: record.identity.clone(),
                        credential: Credential::DeviceToken {
                            id: record.id.clone(),
                        },
                        set_cookie: None,
                    };
                    if record.last_used != Some(now) {
                        record.last_used = Some(now);
                        state.changed();
                    }
                    (Some(authenticated), None)
                }
            }
        };
        self.persist_after_lookup(revoked_generation).await;
        found
    }

    /// Ends the session `token` belongs to. `Ok(false)` if there was none.
    ///
    /// On an error the session is still ended for this process, but the
    /// file may hold it until a later write succeeds: answer with a server
    /// error. Every later revocation, lookup and [`AuthStore::flush`] tries
    /// that write again.
    pub async fn sign_out(&self, token: &SessionToken) -> Result<bool, StoreError> {
        self.end_sessions(std::slice::from_ref(token))
            .await
            .map(|ended| ended > 0)
    }

    /// Signs a request out (`POST /auth/logout`): ends every session whose
    /// token is among the request's `__Host-calin_session` cookies, and returns
    /// how many that was. Unlike authentication, this does not give up when
    /// two different cookies are present. Errors as [`AuthStore::sign_out`].
    pub async fn sign_out_request<'a>(
        &self,
        cookie_headers: impl IntoIterator<Item = &'a str>,
    ) -> Result<usize, StoreError> {
        let tokens = SessionToken::all_from_cookie_headers(cookie_headers);
        self.end_sessions(&tokens).await
    }

    async fn end_sessions(&self, tokens: &[SessionToken]) -> Result<usize, StoreError> {
        let (ended, generation) = {
            let mut state = self.inner.state();
            let mut ended = 0;
            for token in tokens {
                let hash = token.hash();
                if let Some(index) = find_by_hash(&state.sessions, &hash, |s| &s.token_sha256) {
                    state.sessions.remove(index);
                    ended += 1;
                }
            }
            (ended, (ended > 0).then(|| state.changed()))
        };
        self.persist_revocation(generation).await.map(|()| ended)
    }

    /// The live sessions of `identity`, most recently used first. A user
    /// sees and revokes only their own.
    pub fn list_sessions(&self, identity: &Identity) -> Vec<SessionInfo> {
        let now = self.inner.clock.now();
        let state = self.inner.state();
        let mut sessions: Vec<SessionInfo> = state
            .sessions
            .iter()
            .filter(|s| &s.identity == identity && !s.expired(now))
            .map(SessionRecord::info)
            .collect();
        sessions.sort_by(|a, b| b.last_used.cmp(&a.last_used).then(a.id.cmp(&b.id)));
        sessions
    }

    /// Revokes the session `id` of `identity`. `Ok(false)` if `identity` has
    /// no such session, which is also the answer for someone else's id.
    ///
    /// Errors as [`AuthStore::sign_out`]: revoked for this process, possibly
    /// not yet in the file.
    pub async fn revoke_session(&self, identity: &Identity, id: &str) -> Result<bool, StoreError> {
        self.revoke_one(
            |state| &mut state.sessions,
            |s| s.id == id && &s.identity == identity,
        )
        .await
    }

    /// Creates a device token named `name` for `identity`, which must be on
    /// the allow-list. The secret is in the result and nowhere else.
    pub async fn create_device_token(
        &self,
        identity: &Identity,
        name: &str,
    ) -> Result<NewDeviceToken, StoreError> {
        let name = name.trim();
        let name_ok = !name.is_empty()
            && name.chars().count() <= MAX_TOKEN_NAME_CHARS
            && !name.chars().any(char::is_control);
        if !name_ok {
            return Err(StoreError::InvalidName);
        }
        let token = DeviceToken::generate()?;
        let record = DeviceTokenRecord {
            id: random_id()?,
            token_sha256: token.hash(),
            identity: identity.clone(),
            name: name.to_owned(),
            created: self.inner.clock.now(),
            last_used: None,
        };
        let info = record.info();
        let generation = {
            let mut state = self.inner.state();
            if !state.allow_list.allows(identity) {
                return Err(StoreError::NotAllowed(identity.clone()));
            }
            let owned = state
                .device_tokens
                .iter()
                .filter(|t| &t.identity == identity)
                .count();
            if owned >= MAX_DEVICE_TOKENS_PER_IDENTITY {
                return Err(StoreError::TooManyDeviceTokens);
            }
            state.device_tokens.push(record);
            state.changed()
        };
        if let Err(error) = self.persist(Persist::Now(generation)).await {
            let mut state = self.inner.state();
            state.device_tokens.retain(|t| t.id != info.id);
            state.changed();
            return Err(error);
        }
        Ok(NewDeviceToken { info, token })
    }

    /// The device tokens of `identity`, oldest first.
    pub fn list_device_tokens(&self, identity: &Identity) -> Vec<DeviceTokenInfo> {
        let state = self.inner.state();
        let mut tokens: Vec<DeviceTokenInfo> = state
            .device_tokens
            .iter()
            .filter(|t| &t.identity == identity)
            .map(DeviceTokenRecord::info)
            .collect();
        tokens.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        tokens
    }

    /// Revokes the device token `id` of `identity`. `Ok(false)` if
    /// `identity` has no such token.
    ///
    /// Errors as [`AuthStore::sign_out`]: revoked for this process, possibly
    /// not yet in the file.
    pub async fn revoke_device_token(
        &self,
        identity: &Identity,
        id: &str,
    ) -> Result<bool, StoreError> {
        self.revoke_one(
            |state| &mut state.device_tokens,
            |t| t.id == id && &t.identity == identity,
        )
        .await
    }

    /// Removes what `revoked` picks from the list `held` names, then persists
    /// as [`AuthStore::sign_out`] does. `Ok(false)` when nothing was picked.
    async fn revoke_one<T>(
        &self,
        held: impl FnOnce(&mut State) -> &mut Vec<T>,
        revoked: impl Fn(&T) -> bool,
    ) -> Result<bool, StoreError> {
        let generation = {
            let mut state = self.inner.state();
            let list = held(&mut state);
            let before = list.len();
            list.retain(|item| !revoked(item));
            (list.len() < before).then(|| state.changed())
        };
        self.persist_revocation(generation)
            .await
            .map(|()| generation.is_some())
    }

    /// Replaces the allow-list and revokes everything held by identities
    /// that are no longer on it. Test-only: the list comes from the
    /// environment once, and [`crate::OidcClient`] holds its own copy.
    #[cfg(test)]
    async fn set_allow_list(&self, allow_list: AllowList) -> Result<usize, StoreError> {
        let (revoked, generation) = {
            let mut state = self.inner.state();
            state.allow_list = allow_list;
            let revoked = state.drop_not_allowed();
            (revoked, (revoked > 0).then(|| state.changed()))
        };
        self.persist_revocation(generation).await.map(|()| revoked)
    }

    /// Forgets expired sessions now instead of when they are next presented
    /// or the file is next written. Returns how many went.
    pub async fn purge_expired(&self) -> Result<usize, StoreError> {
        let now = self.inner.clock.now();
        let (purged, generation) = {
            let mut state = self.inner.state();
            let purged = state.drop_expired(now);
            (purged, (purged > 0).then_some(state.generation))
        };
        self.persist_revocation(generation).await.map(|()| purged)
    }

    /// Writes everything that is not in the file yet: pending "last used"
    /// updates, and any change whose own write failed. Call it on shutdown.
    pub async fn flush(&self) -> Result<(), StoreError> {
        let generation = self.inner.state().generation;
        self.persist(Persist::Now(generation)).await
    }

    /// After a lookup: a revocation is written at once, a "last used" update
    /// only when the file is old enough. Neither failing changes the answer
    /// the caller already has, so a failure is logged, not returned.
    async fn persist_after_lookup(&self, revoked_generation: Option<u64>) {
        let persist = match revoked_generation {
            Some(generation) => Persist::Now(generation),
            None if self.inner.write_is_due() => Persist::IfDue,
            None => return,
        };
        if let Err(error) = self.persist(persist).await {
            tracing::warn!(%error, "could not write the sign-in store");
        }
    }

    /// Writes a revocation at once. When there was nothing to revoke, makes
    /// sure no earlier change is still owed to the file: a caller retrying
    /// a revocation whose write failed must not be told all is well while
    /// the file still holds what was revoked.
    async fn persist_revocation(&self, generation: Option<u64>) -> Result<(), StoreError> {
        let generation = generation.unwrap_or_else(|| self.inner.owed.load(Ordering::SeqCst));
        self.persist(Persist::Now(generation)).await
    }

    async fn persist(&self, persist: Persist) -> Result<(), StoreError> {
        if let Persist::Now(generation) = persist {
            self.inner.owed.fetch_max(generation, Ordering::SeqCst);
            // Nothing to do, and no reason to visit the blocking pool.
            if self.inner.persisted.load(Ordering::SeqCst) >= generation {
                return Ok(());
            }
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.persist_blocking(persist))
            .await
            .map_err(|_| StoreError::Task)?
    }

    #[cfg(test)]
    fn writes(&self) -> u64 {
        self.inner.writes.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for AuthStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state();
        f.debug_struct("AuthStore")
            .field("path", &self.inner.path)
            .field("sessions", &state.sessions.len())
            .field("device_tokens", &state.device_tokens.len())
            .finish()
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // Every critical section leaves the state consistent before it can
        // panic, so a poisoned lock holds nothing to repair.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether there is something unwritten and the last attempt to write
    /// is at least 30 seconds old (or the clock stepped back).
    fn write_is_due(&self) -> bool {
        let now = self.clock.now();
        let last = self.last_attempt.load(Ordering::SeqCst);
        let old_enough = now >= last.saturating_add(TOUCH_WRITE_INTERVAL_SECS) || now < last;
        old_enough && self.state().generation > self.persisted.load(Ordering::SeqCst)
    }

    fn persist_blocking(&self, persist: Persist) -> Result<(), StoreError> {
        let _writer = self.writer.lock().unwrap_or_else(PoisonError::into_inner);
        // Checked again under the writer lock: whoever held it before may
        // have written what this caller needs.
        match persist {
            Persist::Now(generation) if self.persisted.load(Ordering::SeqCst) >= generation => {
                return Ok(());
            }
            Persist::IfDue if !self.write_is_due() => return Ok(()),
            _ => {}
        }
        let now = self.clock.now();
        self.last_attempt.store(now, Ordering::SeqCst);
        let (file, generation) = {
            let mut state = self.state();
            state.drop_expired(now);
            (state.to_file(), state.generation)
        };
        let mut bytes = serde_json::to_vec_pretty(&file)
            .map_err(io::Error::other)
            .map_err(io_error(&self.path))?;
        bytes.push(b'\n');
        write_atomically(&self.path, &bytes).map_err(io_error(&self.path))?;
        self.persisted.store(generation, Ordering::SeqCst);
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl State {
    /// Marks a change and returns the generation that contains it.
    fn changed(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    fn drop_expired(&mut self, now: u64) -> usize {
        let before = self.sessions.len();
        self.sessions.retain(|s| !s.expired(now));
        let dropped = before - self.sessions.len();
        if dropped > 0 {
            self.changed();
        }
        dropped
    }

    fn drop_not_allowed(&mut self) -> usize {
        let before = self.sessions.len() + self.device_tokens.len();
        let allow_list = &self.allow_list;
        self.sessions.retain(|s| allow_list.allows(&s.identity));
        self.device_tokens
            .retain(|t| allow_list.allows(&t.identity));
        before - self.sessions.len() - self.device_tokens.len()
    }

    fn drop_identity(&mut self, identity: &Identity) {
        self.sessions.retain(|s| &s.identity != identity);
        self.device_tokens.retain(|t| &t.identity != identity);
    }

    fn to_file(&self) -> StoreFile {
        StoreFile {
            version: FILE_VERSION,
            sessions: self.sessions.clone(),
            device_tokens: self.device_tokens.clone(),
        }
    }
}

impl SessionRecord {
    /// A session lasts 30 days from its last use (spec section 3).
    fn expired(&self, now: u64) -> bool {
        now >= self.last_used.saturating_add(SESSION_LIFETIME_SECS)
    }

    fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            identity: self.identity.clone(),
            created: self.created,
            last_used: self.last_used,
            user_agent: self.user_agent.clone(),
        }
    }
}

impl DeviceTokenRecord {
    fn info(&self) -> DeviceTokenInfo {
        DeviceTokenInfo {
            id: self.id.clone(),
            identity: self.identity.clone(),
            name: self.name.clone(),
            created: self.created,
            last_used: self.last_used,
        }
    }
}

/// Index of the record whose hash equals `hash`. Every record is compared,
/// each in constant time, so the duration says nothing about the stored
/// hashes or about where a match sits.
fn find_by_hash<T>(records: &[T], hash: &[u8; 32], of: impl Fn(&T) -> &[u8; 32]) -> Option<usize> {
    let mut found = None;
    for (index, record) in records.iter().enumerate() {
        if constant_time_eq(of(record), hash) {
            found = Some(index);
        }
    }
    found
}

/// Text from a client, made safe to store and list: no control characters,
/// bounded length.
fn clean_text(text: &str, max_chars: usize) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(max_chars)
        .collect()
}

/// What the store's file held at startup, before anything was dropped.
struct Loaded {
    file: LoadedFile,
    permissions_corrected: bool,
    sessions: Vec<SessionRecord>,
    device_tokens: Vec<DeviceTokenRecord>,
}

impl Loaded {
    fn empty(file: LoadedFile, permissions_corrected: bool) -> Self {
        Self {
            file,
            permissions_corrected,
            sessions: Vec::new(),
            device_tokens: Vec::new(),
        }
    }
}

/// Reads the store's file: missing is a first start, and a file that
/// cannot be parsed is set aside.
fn load(path: &Path) -> Result<Loaded, StoreError> {
    let Some(bytes) = read_if_present(path)? else {
        return Ok(Loaded::empty(LoadedFile::Missing, false));
    };
    let permissions_corrected = tighten_permissions(path).map_err(io_error(path))?;
    match parse_file(&bytes) {
        Ok(file) => Ok(Loaded {
            file: LoadedFile::Loaded {
                sessions: file.sessions.len(),
                device_tokens: file.device_tokens.len(),
            },
            permissions_corrected,
            sessions: file.sessions,
            device_tokens: file.device_tokens,
        }),
        Err(reason) => Ok(Loaded::empty(
            set_aside(path, reason)?,
            permissions_corrected,
        )),
    }
}

fn read_if_present(path: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path)(error)),
    }
}

/// Kept, not overwritten: it may be a newer version's file, or hold device
/// tokens worth recovering.
fn set_aside(path: &Path, reason: String) -> Result<LoadedFile, StoreError> {
    let kept_as = beside(path, ".corrupt");
    std::fs::rename(path, &kept_as).map_err(io_error(path))?;
    Ok(LoadedFile::Corrupt { reason, kept_as })
}

/// Names the store's file in an I/O error.
fn io_error(path: &Path) -> impl Fn(io::Error) -> StoreError + '_ {
    |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

fn parse_file(bytes: &[u8]) -> Result<StoreFile, String> {
    let file: StoreFile = parse_object(bytes)?;
    if file.version != FILE_VERSION {
        return Err(format!("unsupported version {}", file.version));
    }
    Ok(file)
}

/// Whether a record claims to have been created later than now, beyond what
/// two clocks may disagree by.
fn from_the_future(created: u64, now: u64) -> bool {
    created > now.saturating_add(CLOCK_TOLERANCE_SECS)
}

/// The file next to `path` whose name is `path`'s with `suffix` added.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Replaces the file at `path` with `bytes`, so that a reader, or a restart
/// after a crash at any point, sees the old content or the new, never a mix.
fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = write_temporary(path, bytes)?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    // Makes the rename itself durable. Not every filesystem can sync a
    // directory, and the content is already safe either way.
    if let Some(directory) = path.parent().filter(|p| !p.as_os_str().is_empty())
        && let Ok(directory) = std::fs::File::open(directory)
    {
        let _ = directory.sync_all();
    }
    Ok(())
}

/// Writes `bytes` to a new mode-0600 file next to `path` and syncs it.
fn write_temporary(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let temporary = beside(path, ".tmp");
    // A leftover from a crash is removed, not reused: `create_new` then
    // guarantees a file this process created, with the mode asked for.
    match std::fs::remove_file(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(temporary)
}

/// Sets the file to mode 0600 if anyone but its owner can access it.
/// Returns whether it had to.
#[cfg(unix)]
fn tighten_permissions(path: &Path) -> io::Result<bool> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(path)?.permissions().mode();
    if mode & 0o077 == 0 {
        return Ok(false);
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(true)
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) -> io::Result<bool> {
    Ok(false)
}

/// A SHA-256 as 64 lowercase hex digits in the file.
mod hex_hash {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub(super) fn serialize<S: Serializer>(
        hash: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
        serializer.serialize_str(&hex)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        let bad = || D::Error::custom("a hash is not 64 lowercase hex digits");
        if text.len() != 64 || !text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
            return Err(bad());
        }
        let mut hash = [0u8; 32];
        for (index, byte) in hash.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| bad())?;
        }
        Ok(hash)
    }
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
