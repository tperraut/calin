//! Web Push (remote-access spec, section 6): the server's key and the
//! subscription store, the seven `/push` routes, and who has a session open.
//! Encryption, sending, preferences and the throttle are `calin-push`'s;
//! when to notify is [`crate::notify`]'s.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Path as UrlPath, State};
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use calin_auth::{Authenticated, Credential};
use calin_proto::{
    PushKey, PushKind, PushPayload, PushPreferences, PushSubscriptionInfo, PushSubscriptionRequest,
};
use calin_push::{
    Clock, ConfigError, EndpointPolicy, LoadedFile, PushConfig, PushSender, StoreError,
    SubscribeError, SubscriptionStore, VapidError, VapidKeys,
};

use crate::AppState;
use crate::account::signed_in;
use crate::api::{ApiFailure, ApiJson, ApiResult};
use crate::auth::Caller;

/// The file under the data directory that holds the server's VAPID key pair.
pub const PUSH_KEY_FILE: &str = "push-vapid-key.json";
/// The file under the data directory that holds every identity's push
/// subscriptions and preferences.
pub const PUSH_SUBSCRIPTIONS_FILE: &str = "push-subscriptions.json";

/// The product name: the title of a test notification.
const PRODUCT: &str = "Calin";

/// How the server sends notifications.
#[derive(Debug, Clone)]
pub struct PushSetup {
    /// The VAPID `sub` claim: a `mailto:` or `https:` URI (see [`vapid_subject`]).
    pub subject: String,
    /// [`EndpointPolicy::PublicHttpsOnly`] on a real server.
    pub endpoint_policy: EndpointPolicy,
}

/// The VAPID contact: `contact` when given (it must be a `mailto:` URI),
/// else the public URL's origin, which RFC 8292 accepts as an `https:` URI.
/// A development origin on plain `http` gives a placeholder address.
pub fn vapid_subject(public_origin: &str, contact: Option<&str>) -> Result<String, String> {
    match contact {
        Some(contact) => {
            let address = contact.strip_prefix("mailto:").unwrap_or("");
            let (user, host) = address.split_once('@').unwrap_or(("", ""));
            if user.is_empty()
                || host.is_empty()
                || address.contains(char::is_whitespace)
                || address.chars().any(char::is_control)
            {
                return Err(format!(
                    "{contact:?} is not a mailto: URI, for example mailto:admin@example.com"
                ));
            }
            Ok(contact.to_owned())
        }
        None if public_origin.starts_with("https://") => Ok(public_origin.to_owned()),
        // Only the development switch allows an `http` public URL.
        None => Ok("mailto:calin@localhost.invalid".to_owned()),
    }
}

/// Everything Web Push needs, opened once in public mode.
pub struct Push {
    keys: Arc<VapidKeys>,
    store: SubscriptionStore,
    sender: PushSender,
    endpoint_policy: EndpointPolicy,
}

/// Web Push could not be set up. Each message names the file at fault.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    /// Includes a corrupt key file, which is never replaced.
    #[error(transparent)]
    Keys(#[from] VapidError),
    #[error("cannot open the push subscription store: {0}")]
    Store(#[from] StoreError),
    #[error("cannot set up push notifications: {0}")]
    Config(#[from] ConfigError),
}

impl Push {
    /// Opens (or creates) [`PUSH_KEY_FILE`] and [`PUSH_SUBSCRIPTIONS_FILE`]
    /// in `data_dir` and builds the sender.
    pub async fn open(
        data_dir: &Path,
        setup: PushSetup,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, PushError> {
        let keys = Arc::new(VapidKeys::load_or_generate(data_dir.join(PUSH_KEY_FILE)).await?);
        if keys.was_generated() {
            tracing::info!("created the push key pair");
        }
        let store =
            SubscriptionStore::open(data_dir.join(PUSH_SUBSCRIPTIONS_FILE), clock.clone()).await?;
        let report = store.load_report();
        match &report.file {
            LoadedFile::Corrupt { .. } => tracing::warn!(
                ?report,
                "the push subscription store could not be read and starts empty: \
                 every device subscribes again"
            ),
            _ => tracing::info!(?report, "push subscription store opened"),
        }
        let sender = PushSender::new(
            keys.clone(),
            store.clone(),
            clock,
            PushConfig {
                endpoint_policy: setup.endpoint_policy,
                ..PushConfig::new(setup.subject)
            },
        )?;
        Ok(Self {
            keys,
            store,
            sender,
            endpoint_policy: setup.endpoint_policy,
        })
    }

    /// Writes what the store still owes its file. For shutdown.
    pub async fn flush(&self) {
        if let Err(error) = self.store.flush().await {
            tracing::error!(%error, "could not write the push subscription store on shutdown");
        }
    }

    pub(crate) fn sender(&self) -> &PushSender {
        &self.sender
    }
}

impl std::fmt::Debug for Push {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Push")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

// --- Who has a session open ---

/// Which identities have an authenticated event socket subscribed to which
/// session's topic. The server cannot see whether a window is visible; a
/// subscribed socket is the closest it knows (the service worker does the
/// rest on the device).
#[derive(Clone, Default)]
pub(crate) struct Viewers(Arc<Mutex<HashMap<(String, String), usize>>>);

impl Viewers {
    /// Counts `identity` as viewing `session_id` until the guard is dropped.
    pub(crate) fn watch(&self, identity: &str, session_id: &str) -> ViewGuard {
        let key = (identity.to_owned(), session_id.to_owned());
        *self.lock().entry(key.clone()).or_default() += 1;
        ViewGuard {
            viewers: self.clone(),
            key,
        }
    }

    pub(crate) fn is_viewing(&self, identity: &str, session_id: &str) -> bool {
        self.lock()
            .contains_key(&(identity.to_owned(), session_id.to_owned()))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), usize>> {
        // A count is valid whatever panicked while it was held.
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One subscription of one socket to one session's topic.
pub(crate) struct ViewGuard {
    viewers: Viewers,
    key: (String, String),
}

impl Drop for ViewGuard {
    fn drop(&mut self) {
        let mut counts = self.viewers.lock();
        if let Some(count) = counts.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                counts.remove(&self.key);
            }
        }
    }
}

// --- Routes ---

/// The signed-in caller and the push setup, or the refusal: 409 in local
/// mode, where push does not exist (contract, **Push**).
fn enabled(state: &AppState, caller: Caller) -> ApiResult<(Arc<Push>, Authenticated)> {
    let (_, who) = signed_in(state, caller)?;
    let push = state.push.clone().ok_or_else(|| {
        ApiFailure::new(
            StatusCode::CONFLICT,
            "push notifications are not set up on this server",
        )
    })?;
    Ok((push, who))
}

fn internal(error: &dyn std::error::Error) -> ApiFailure {
    tracing::error!(%error, "a push request failed");
    ApiFailure::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

pub(crate) async fn key(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<PushKey>> {
    let (push, _) = enabled(&state, caller)?;
    Ok(Json(push.keys.push_key()))
}

pub(crate) async fn list_subscriptions(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<PushSubscriptionInfo>>> {
    let (push, who) = enabled(&state, caller)?;
    let mut subscriptions = push.store.list(&who.identity.to_string());
    // The store lists oldest first, the contract newest first.
    subscriptions.reverse();
    Ok(Json(subscriptions))
}

pub(crate) async fn subscribe(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    body: Result<ApiJson<PushSubscriptionRequest>, ApiFailure>,
) -> ApiResult<(StatusCode, Json<PushSubscriptionInfo>)> {
    let (push, who) = enabled(&state, caller)?;
    // A subscription belongs to a browser; a device token is not one, as it
    // cannot create device tokens either.
    if matches!(who.credential, Credential::DeviceToken { .. }) {
        return Err(ApiFailure::new(
            StatusCode::FORBIDDEN,
            "a device token cannot subscribe to notifications: subscribe from a browser",
        ));
    }
    let ApiJson(request) = body?;
    let user_agent = headers
        .get(USER_AGENT)
        .and_then(|value| value.to_str().ok());
    let info = push
        .store
        .subscribe(
            &who.identity.to_string(),
            &request,
            user_agent,
            push.endpoint_policy,
        )
        .await
        .map_err(|error| match error {
            SubscribeError::Endpoint(_)
            | SubscribeError::InvalidP256dh
            | SubscribeError::InvalidAuth => {
                ApiFailure::new(StatusCode::BAD_REQUEST, error.to_string())
            }
            error => internal(&error),
        })?;
    Ok((StatusCode::CREATED, Json(info)))
}

pub(crate) async fn unsubscribe(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    UrlPath(id): UrlPath<String>,
) -> ApiResult<StatusCode> {
    let (push, who) = enabled(&state, caller)?;
    let removed = push
        .store
        .remove(&who.identity.to_string(), &id)
        .await
        .map_err(|error| internal(&error))?;
    if !removed {
        return Err(ApiFailure::new(
            StatusCode::NOT_FOUND,
            "no such push subscription",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn get_preferences(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<PushPreferences>> {
    let (push, who) = enabled(&state, caller)?;
    Ok(Json(push.store.preferences(&who.identity.to_string())))
}

pub(crate) async fn set_preferences(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    body: Result<ApiJson<PushPreferences>, ApiFailure>,
) -> ApiResult<Json<PushPreferences>> {
    let (push, who) = enabled(&state, caller)?;
    let ApiJson(preferences) = body?;
    let identity = who.identity.to_string();
    push.store
        .set_preferences(&identity, preferences)
        .await
        .map_err(|error| internal(&error))?;
    Ok(Json(push.store.preferences(&identity)))
}

/// Answers at once; the push services are called in the background.
pub(crate) async fn test(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<StatusCode> {
    let (push, who) = enabled(&state, caller)?;
    let identity = who.identity.to_string();
    tokio::spawn(async move {
        let payload = PushPayload {
            kind: PushKind::Test,
            title: PRODUCT.to_owned(),
            body: "Notifications work on this device.".to_owned(),
            path: "/settings".to_owned(),
            session_id: None,
        };
        match push.sender.send(&identity, PushKind::Test, payload).await {
            Ok(report) => tracing::debug!(?report, "test notification sent"),
            Err(error) => tracing::warn!(%error, "test notification not sent"),
        }
    });
    Ok(StatusCode::ACCEPTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subject_is_the_contact_or_the_public_origin() {
        let origin = "https://calin.example.com";
        assert_eq!(vapid_subject(origin, None).unwrap(), origin);
        assert_eq!(
            vapid_subject(origin, Some("mailto:ops@example.com")).unwrap(),
            "mailto:ops@example.com"
        );
        for bad in [
            "ops@example.com",
            "mailto:",
            "mailto:ops",
            "mailto:@example.com",
            "mailto:ops@",
            "mailto:o ps@example.com",
            "https://calin.example.com",
        ] {
            let error = vapid_subject(origin, Some(bad)).unwrap_err();
            assert!(error.contains("mailto:"), "{bad}: {error}");
        }
        // The development switch's http origin is not a valid subject.
        let dev = vapid_subject("http://localhost:4680", None).unwrap();
        assert!(dev.starts_with("mailto:"), "{dev}");
    }

    #[test]
    fn a_viewer_counts_until_its_last_guard_is_dropped() {
        let viewers = Viewers::default();
        assert!(!viewers.is_viewing("alice", "s1"));
        let first = viewers.watch("alice", "s1");
        let second = viewers.watch("alice", "s1");
        assert!(viewers.is_viewing("alice", "s1"));
        assert!(!viewers.is_viewing("bob", "s1"));
        assert!(!viewers.is_viewing("alice", "s2"));
        drop(first);
        assert!(viewers.is_viewing("alice", "s1"));
        drop(second);
        assert!(!viewers.is_viewing("alice", "s1"));
        assert!(viewers.0.lock().unwrap().is_empty());
    }
}
