//! Push subscriptions and notification preferences, per identity, in one
//! file (spec section 6).
//!
//! The file holds each subscription's endpoint and keys, which together
//! let anyone send to that device, so it is mode 0600. All state lives in
//! memory behind a mutex and the file is a copy of it: rewritten at once on
//! every change a user makes or a push service reports, and at most every
//! 30 seconds for "last sent" updates, always as a new file renamed over
//! the old one.

use std::collections::BTreeMap;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use calin_proto::{PushPreferences, PushSubscriptionInfo, PushSubscriptionRequest};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::clock::Clock;
use crate::ece::{AUTH_LEN, PUBLIC_KEY_LEN, SubscriberKeys, is_valid_public_key};
use crate::endpoint::{EndpointError, EndpointPolicy, service, validate_endpoint};
use crate::fsio::{corrupt_path, tighten_permissions, write_atomically, write_temporary};
use crate::secret::{
    RandomError, base64url, base64url_decode, base64url_decode_browser, clean_text, hex, log_safe,
    random_id, sha256,
};
use crate::user_agent::describe_user_agent;

/// Subscriptions kept per identity; the oldest goes first.
pub const MAX_SUBSCRIPTIONS_PER_IDENTITY: usize = 20;
/// A device label's length, in characters.
pub const MAX_LABEL_CHARS: usize = 80;
/// "Last sent" updates reach the disk at most this often.
const SUCCESS_WRITE_INTERVAL_MILLIS: u64 = 30_000;
const FILE_VERSION: u32 = 1;

/// The store of subscriptions and preferences. Cheap to clone; clones share
/// one state.
///
/// Its `async` methods hand file writes to tokio's blocking pool, so they
/// need a tokio runtime and never block it.
#[derive(Clone)]
pub struct SubscriptionStore {
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
    /// When a write was last attempted, in milliseconds. Failed attempts
    /// count, so a disk that refuses writes is not retried on every push.
    last_attempt: AtomicU64,
    writes: AtomicU64,
    load_report: LoadReport,
}

struct State {
    identities: BTreeMap<String, IdentityRecord>,
    /// Incremented by every change, so a writer knows what is on disk.
    generation: u64,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct IdentityRecord {
    #[serde(default)]
    preferences: PushPreferences,
    #[serde(default)]
    subscriptions: Vec<SubscriptionRecord>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SubscriptionRecord {
    id: String,
    endpoint: String,
    /// base64url, as received.
    p256dh: String,
    auth: String,
    label: String,
    created_at: i64,
    last_sent_at: Option<i64>,
}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    identities: BTreeMap<String, IdentityRecord>,
}

/// What [`SubscriptionStore::open`] found. Log it once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    /// The state of the file.
    pub file: LoadedFile,
    /// The file was readable by others and has been set back to mode 0600.
    pub permissions_corrected: bool,
    /// Subscriptions dropped because their endpoint or keys no longer pass
    /// validation (a hand-edited file, or stricter rules since).
    pub invalid_dropped: usize,
}

/// The state of the store's file at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadedFile {
    /// No file yet: a first start.
    Missing,
    /// The file was read.
    Loaded {
        /// Subscriptions it held, over all identities.
        subscriptions: usize,
    },
    /// The file could not be understood. The store starts empty (every
    /// device subscribes again) and the file was moved aside.
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
    /// The blocking task doing the write was cancelled (runtime shutdown).
    #[error("the task writing the push store did not complete")]
    Task,
}

/// A subscription was refused. Every variant but `Store` and `Random` is
/// the client's mistake, and its message is safe to show.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SubscribeError {
    /// The endpoint is not one the server will call.
    #[error(transparent)]
    Endpoint(#[from] EndpointError),
    /// `p256dh` is not a base64url uncompressed P-256 point on the curve.
    #[error("p256dh must be a base64url uncompressed P-256 public key")]
    InvalidP256dh,
    /// `auth` is not 16 bytes of base64url.
    #[error("auth must be 16 bytes, base64url")]
    InvalidAuth,
    /// The subscription could not be written, so it was not added.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// No randomness for the id.
    #[error(transparent)]
    Random(#[from] RandomError),
}

/// One subscription to send to, with everything decoded.
#[derive(Clone)]
pub(crate) struct Target {
    pub(crate) id: String,
    pub(crate) url: Url,
    pub(crate) keys: SubscriberKeys,
    pub(crate) service: String,
}

impl fmt::Debug for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The endpoint is a capability: only its host is shown.
        f.debug_struct("Target")
            .field("id", &self.id)
            .field("service", &self.service)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy)]
enum Persist {
    /// The state at this generation must be on disk before returning.
    Now(u64),
    /// Write only if something changed and the last write is old enough.
    IfDue,
}

impl SubscriptionStore {
    /// Opens the store kept in the file at `path`, whose directory must exist
    /// and be writable.
    ///
    /// A missing file is a first start. A file that cannot be parsed is
    /// moved aside and the store starts empty, said once in
    /// [`SubscriptionStore::load_report`] and in a warning. A file other
    /// users can read is set back to mode 0600.
    ///
    /// Open one store per file. Two would overwrite each other's writes.
    pub async fn open(path: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        let path = path.into();
        tokio::task::spawn_blocking(move || Self::open_blocking(path, clock))
            .await
            .map_err(|_| StoreError::Task)?
    }

    fn open_blocking(path: PathBuf, clock: Arc<dyn Clock>) -> Result<Self, StoreError> {
        // Found out now rather than at the first subscription.
        let probe = write_temporary(&path, b"").map_err(io_error(&path))?;
        std::fs::remove_file(probe).map_err(io_error(&path))?;

        let (mut identities, mut report) = load(&path)?;
        report.invalid_dropped = drop_invalid(&mut identities);
        let dropped = report.invalid_dropped > 0;
        let store = Self {
            inner: Arc::new(Inner {
                path,
                persisted: AtomicU64::new(0),
                last_attempt: AtomicU64::new(clock.now_millis()),
                clock,
                writes: AtomicU64::new(0),
                state: Mutex::new(State {
                    identities,
                    generation: u64::from(dropped),
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

    /// Adds a browser's subscription for `identity`, or updates it if this
    /// identity already has one with the same endpoint (the id is kept).
    ///
    /// The label is the request's, or derived from `user_agent`. An endpoint
    /// is one browser profile, so it is taken away from any other identity
    /// that had it: notifications follow whoever subscribed last. Past
    /// [`MAX_SUBSCRIPTIONS_PER_IDENTITY`] the oldest subscription goes.
    /// The subscription is on disk when this returns.
    pub async fn subscribe(
        &self,
        identity: &str,
        request: &PushSubscriptionRequest,
        user_agent: Option<&str>,
        policy: EndpointPolicy,
    ) -> Result<PushSubscriptionInfo, SubscribeError> {
        let url = validate_endpoint(&request.endpoint, policy)?;
        let p256dh = base64url_decode_browser(&request.p256dh)
            .filter(|bytes| is_valid_public_key(bytes))
            .ok_or(SubscribeError::InvalidP256dh)?;
        let auth = base64url_decode_browser(&request.auth)
            .filter(|bytes| bytes.len() == AUTH_LEN)
            .ok_or(SubscribeError::InvalidAuth)?;
        let candidate = SubscriptionRecord {
            id: random_id()?,
            // Stored as the URL serializes it, so the same endpoint written
            // two ways is recognized as one.
            endpoint: url.to_string(),
            p256dh: base64url(&p256dh),
            auth: base64url(&auth),
            label: subscription_label(request.label.as_deref(), user_agent),
            created_at: self.inner.now(),
            last_sent_at: None,
        };
        let (info, generation) = {
            let mut state = self.inner.state();
            let subscription = state.subscribe(identity, candidate);
            (subscription.info(), state.changed())
        };
        self.persist(Persist::Now(generation)).await?;
        Ok(info)
    }

    /// The subscriptions of `identity`, oldest first.
    pub fn list(&self, identity: &str) -> Vec<PushSubscriptionInfo> {
        self.inner
            .state()
            .identities
            .get(identity)
            .map(|record| record.subscriptions.iter().map(|s| s.info()).collect())
            .unwrap_or_default()
    }

    /// Removes the subscription `id` of `identity`. Returns whether there
    /// was one; another identity's id is not found. On disk when this returns.
    pub async fn remove(&self, identity: &str, id: &str) -> Result<bool, StoreError> {
        let generation = {
            let mut state = self.inner.state();
            let removed = state
                .identities
                .get_mut(identity)
                .is_some_and(|record| record.remove_where(|s| s.id == id));
            if !removed {
                return Ok(false);
            }
            state.identities.retain(|_, record| !record.is_empty());
            state.changed()
        };
        self.persist(Persist::Now(generation)).await?;
        Ok(true)
    }

    /// Which events notify `identity`. All of them until it says otherwise.
    pub fn preferences(&self, identity: &str) -> PushPreferences {
        self.inner
            .state()
            .identities
            .get(identity)
            .map(|record| record.preferences)
            .unwrap_or_default()
    }

    /// Replaces the preferences of `identity`. On disk when this returns.
    pub async fn set_preferences(
        &self,
        identity: &str,
        preferences: PushPreferences,
    ) -> Result<(), StoreError> {
        let generation = {
            let mut state = self.inner.state();
            state
                .identities
                .entry(identity.to_owned())
                .or_default()
                .preferences = preferences;
            state.identities.retain(|_, record| !record.is_empty());
            state.changed()
        };
        self.persist(Persist::Now(generation)).await
    }

    /// Writes everything that is not in the file yet, such as pending "last
    /// sent" times. Call it on shutdown.
    pub async fn flush(&self) -> Result<(), StoreError> {
        let generation = self.inner.state().generation;
        self.persist(Persist::Now(generation)).await
    }

    /// The preferences and decoded subscriptions of `identity`.
    pub(crate) fn targets(&self, identity: &str) -> (PushPreferences, Vec<Target>) {
        let state = self.inner.state();
        match state.identities.get(identity) {
            None => (PushPreferences::default(), Vec::new()),
            Some(record) => (
                record.preferences,
                record
                    .subscriptions
                    .iter()
                    .filter_map(SubscriptionRecord::target)
                    .collect(),
            ),
        }
    }

    /// A push service accepted a message: records when. Reaches the disk
    /// within 30 seconds or at [`SubscriptionStore::flush`]; losing it in a
    /// crash costs nothing but the timestamp.
    pub(crate) async fn record_success(&self, identity: &str, id: &str) {
        let now = self.inner.now();
        {
            let mut state = self.inner.state();
            let Some(subscription) = state
                .identities
                .get_mut(identity)
                .and_then(|record| record.subscriptions.iter_mut().find(|s| s.id == id))
            else {
                return;
            };
            subscription.last_sent_at = Some(now);
            state.changed();
        }
        if self.inner.write_is_due()
            && let Err(error) = self.persist(Persist::IfDue).await
        {
            tracing::warn!(%error, "could not write the push subscription store");
        }
    }

    /// A push service said the subscription no longer exists. Removes it,
    /// unless it was re-subscribed with another endpoint meanwhile.
    pub(crate) async fn remove_gone(
        &self,
        identity: &str,
        id: &str,
        url: &Url,
    ) -> Result<bool, StoreError> {
        let endpoint = url.as_str();
        let generation = {
            let mut state = self.inner.state();
            let removed = state.identities.get_mut(identity).is_some_and(|record| {
                record.remove_where(|s| s.id == id && s.endpoint == endpoint)
            });
            if !removed {
                return Ok(false);
            }
            state.identities.retain(|_, record| !record.is_empty());
            state.changed()
        };
        self.persist(Persist::Now(generation)).await?;
        Ok(true)
    }

    async fn persist(&self, persist: Persist) -> Result<(), StoreError> {
        if let Persist::Now(generation) = persist
            && self.inner.persisted.load(Ordering::SeqCst) >= generation
        {
            return Ok(());
        }
        let inner = self.inner.clone();
        tokio::task::spawn_blocking(move || inner.persist_blocking(persist))
            .await
            .map_err(|_| StoreError::Task)?
    }

    #[cfg(test)]
    pub(crate) fn writes(&self) -> u64 {
        self.inner.writes.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.inner.path
    }
}

impl fmt::Debug for SubscriptionStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.state();
        f.debug_struct("SubscriptionStore")
            .field("path", &self.inner.path)
            .field("identities", &state.identities.len())
            .field(
                "subscriptions",
                &state
                    .identities
                    .values()
                    .map(|i| i.subscriptions.len())
                    .sum::<usize>(),
            )
            .finish()
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        // Every critical section leaves the state consistent before it can
        // panic, so a poisoned lock holds nothing to repair.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now(&self) -> i64 {
        i64::try_from(self.clock.now_millis()).unwrap_or(i64::MAX)
    }

    /// Whether there is something unwritten and the last attempt to write
    /// is at least 30 seconds old (or the clock stepped back).
    fn write_is_due(&self) -> bool {
        let now = self.clock.now_millis();
        let last = self.last_attempt.load(Ordering::SeqCst);
        let old_enough = now >= last.saturating_add(SUCCESS_WRITE_INTERVAL_MILLIS) || now < last;
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
        self.last_attempt
            .store(self.clock.now_millis(), Ordering::SeqCst);
        let (file, generation) = {
            let state = self.state();
            let file = StoreFile {
                version: FILE_VERSION,
                identities: state.identities.clone(),
            };
            (file, state.generation)
        };
        let io_error = io_error(&self.path);
        let mut bytes =
            serde_json::to_vec_pretty(&file).map_err(|error| io_error(io::Error::other(error)))?;
        bytes.push(b'\n');
        write_atomically(&self.path, &bytes).map_err(io_error)?;
        self.persisted.fetch_max(generation, Ordering::SeqCst);
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

    /// Gives `candidate`'s endpoint to `identity` alone: taken away from
    /// every other identity, and added to or updated in this one's. Returns
    /// the subscription as stored.
    fn subscribe(&mut self, identity: &str, candidate: SubscriptionRecord) -> SubscriptionRecord {
        for (other, record) in self.identities.iter_mut() {
            if other != identity {
                record
                    .subscriptions
                    .retain(|s| s.endpoint != candidate.endpoint);
            }
        }
        let subscription = self
            .identities
            .entry(identity.to_owned())
            .or_default()
            .upsert(candidate);
        self.identities.retain(|_, record| !record.is_empty());
        subscription
    }
}

/// The request's label, cleaned, or one derived from the user agent.
fn subscription_label(label: Option<&str>, user_agent: Option<&str>) -> String {
    label
        .map(|label| clean_text(label, MAX_LABEL_CHARS))
        .filter(|label| !label.is_empty())
        .unwrap_or_else(|| {
            clean_text(
                &describe_user_agent(user_agent.unwrap_or("")),
                MAX_LABEL_CHARS,
            )
        })
}

impl IdentityRecord {
    fn is_empty(&self) -> bool {
        self.subscriptions.is_empty() && self.preferences == PushPreferences::default()
    }

    /// Updates the keys and label of the subscription with `candidate`'s
    /// endpoint, keeping its id, or adds `candidate` and drops the oldest
    /// past [`MAX_SUBSCRIPTIONS_PER_IDENTITY`].
    fn upsert(&mut self, candidate: SubscriptionRecord) -> SubscriptionRecord {
        if let Some(existing) = self
            .subscriptions
            .iter_mut()
            .find(|s| s.endpoint == candidate.endpoint)
        {
            existing.p256dh = candidate.p256dh;
            existing.auth = candidate.auth;
            existing.label = candidate.label;
            return existing.clone();
        }
        self.subscriptions.push(candidate.clone());
        // Oldest first, so the one dropped is the front; a stable sort keeps
        // the new one last among equal times.
        self.subscriptions.sort_by_key(|s| s.created_at);
        while self.subscriptions.len() > MAX_SUBSCRIPTIONS_PER_IDENTITY {
            let oldest = self
                .subscriptions
                .iter()
                .position(|s| s.id != candidate.id)
                .unwrap_or(0);
            self.subscriptions.remove(oldest);
        }
        candidate
    }

    fn remove_where(&mut self, matches: impl Fn(&SubscriptionRecord) -> bool) -> bool {
        let before = self.subscriptions.len();
        self.subscriptions.retain(|s| !matches(s));
        self.subscriptions.len() != before
    }
}

impl SubscriptionRecord {
    fn info(&self) -> PushSubscriptionInfo {
        let service = Url::parse(&self.endpoint)
            .map(|url| service(&url))
            .unwrap_or_default();
        PushSubscriptionInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            endpoint_sha256: hex(&sha256(self.endpoint.as_bytes())),
            service,
            created_at: self.created_at,
            last_sent_at: self.last_sent_at,
        }
    }

    /// Decoded for sending. `None` if a field no longer passes the rules,
    /// checked with the strictest endpoint policy unless it is loopback,
    /// which only a development server could have stored.
    fn target(&self) -> Option<Target> {
        let url = validate_endpoint(&self.endpoint, EndpointPolicy::PublicHttpsOnly)
            .or_else(|_| {
                validate_endpoint(
                    &self.endpoint,
                    EndpointPolicy::AllowLoopbackHttpForDevelopment,
                )
            })
            .ok()?;
        let p256dh: [u8; PUBLIC_KEY_LEN] = base64url_decode(&self.p256dh)?.try_into().ok()?;
        let auth: [u8; AUTH_LEN] = base64url_decode(&self.auth)?.try_into().ok()?;
        if !is_valid_public_key(&p256dh) {
            return None;
        }
        Some(Target {
            id: self.id.clone(),
            service: service(&url),
            url,
            keys: SubscriberKeys { p256dh, auth },
        })
    }
}

/// What the store's file at `path` holds, and what reading it found. A
/// missing file is an empty store; an unreadable one is set aside.
fn load(path: &Path) -> Result<(BTreeMap<String, IdentityRecord>, LoadReport), StoreError> {
    let mut report = LoadReport {
        file: LoadedFile::Missing,
        permissions_corrected: false,
        invalid_dropped: 0,
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), report));
        }
        Err(error) => return Err(io_error(path)(error)),
    };
    report.permissions_corrected = tighten_permissions(path).map_err(io_error(path))?;
    match parse_file(&bytes) {
        Ok(file) => {
            report.file = LoadedFile::Loaded {
                subscriptions: file
                    .identities
                    .values()
                    .map(|i| i.subscriptions.len())
                    .sum(),
            };
            Ok((file.identities, report))
        }
        Err(reason) => {
            report.file = set_aside(path, reason)?;
            Ok((BTreeMap::new(), report))
        }
    }
}

/// Moves an unreadable store file aside. Kept, not overwritten: it may be
/// a newer version's file, and its subscriptions are worth recovering.
fn set_aside(path: &Path, reason: String) -> Result<LoadedFile, StoreError> {
    let kept_as = corrupt_path(path);
    std::fs::rename(path, &kept_as).map_err(io_error(path))?;
    tracing::warn!(
        path = %path.display(),
        kept_as = %kept_as.display(),
        %reason,
        "the push subscription store was unreadable; starting empty"
    );
    Ok(LoadedFile::Corrupt { reason, kept_as })
}

/// Drops the subscriptions that no longer pass validation. Returns how many.
fn drop_invalid(identities: &mut BTreeMap<String, IdentityRecord>) -> usize {
    let mut dropped = 0;
    for record in identities.values_mut() {
        let before = record.subscriptions.len();
        record.subscriptions.retain(|s| s.target().is_some());
        dropped += before - record.subscriptions.len();
    }
    dropped
}

fn io_error(path: &Path) -> impl Fn(io::Error) -> StoreError + '_ {
    move |source| StoreError::Io {
        path: path.to_owned(),
        source,
    }
}

fn parse_file(bytes: &[u8]) -> Result<StoreFile, String> {
    let first = bytes.iter().find(|byte| !b" \t\r\n".contains(byte));
    if first != Some(&b'{') {
        return Err("not a JSON object".to_owned());
    }
    let file: StoreFile =
        serde_json::from_slice(bytes).map_err(|error| log_safe(&error.to_string(), 120))?;
    if file.version != FILE_VERSION {
        return Err(format!("unsupported version {}", file.version));
    }
    Ok(file)
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
