//! Sending one notification to every device of an identity (RFC 8030).

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use calin_proto::{PushKind, PushPayload, PushPreferences};
use hyper::header::{self, HeaderName, HeaderValue};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::clock::Clock;
use crate::ece::{MAX_PLAINTEXT_LEN, encrypt};
use crate::endpoint::{EndpointError, EndpointPolicy, audience, validate_endpoint};
use crate::http::{HttpError, PushHttp};
use crate::secret::{base64url, clean_text, sha256};
use crate::store::{SubscriptionStore, Target};
use crate::vapid::VapidKeys;

/// `title` is cut to this many characters rather than refused.
pub const MAX_TITLE_CHARS: usize = 120;
/// `body` is cut to this many characters rather than refused.
pub const MAX_BODY_CHARS: usize = 300;
/// How long a push service keeps an undelivered message: one day. A phone
/// that is off longer does not need a stale "waiting" on waking.
pub const TTL_SECS: u32 = 24 * 60 * 60;
/// At most one notification per session per identity in this window (spec
/// section 6).
pub const SESSION_THROTTLE: Duration = Duration::from_secs(60);
/// At most one test notification per identity in this window.
pub const TEST_THROTTLE: Duration = Duration::from_secs(10);

/// How the sender identifies itself and how it calls push services.
#[derive(Debug, Clone)]
pub struct PushConfig {
    /// The VAPID `sub` claim: a `mailto:` or `https:` URI where the push
    /// service's operator can reach whoever runs this server.
    pub subject: String,
    /// Which endpoints may be called. [`EndpointPolicy::PublicHttpsOnly`]
    /// outside of tests.
    pub endpoint_policy: EndpointPolicy,
    /// Time allowed for one request to a push service, start to finish.
    pub request_timeout: Duration,
    /// Requests in flight at once for one [`PushSender::send`].
    pub concurrency: usize,
}

impl PushConfig {
    /// Defaults for a real server: public `https` only, 10 seconds per
    /// request, four at a time.
    pub fn new(subject: impl Into<String>) -> Self {
        Self {
            subject: subject.into(),
            endpoint_policy: EndpointPolicy::PublicHttpsOnly,
            request_timeout: Duration::from_secs(10),
            concurrency: 4,
        }
    }
}

/// The configuration cannot work.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// RFC 8292 section 2.1 wants a `mailto:` or `https:` contact URI.
    #[error("the VAPID subject must be a mailto: or https: URI")]
    Subject,
    /// The TLS configuration could not be built.
    #[error("cannot set up TLS: {0}")]
    Tls(String),
}

/// A notification was refused before any request was made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SendError {
    /// Even with `title` and `body` cut, the payload does not fit in one
    /// push message: `path` or `session_id` is far too long.
    #[error("the payload is {len} bytes; at most {MAX_PLAINTEXT_LEN} fit in one push message")]
    PayloadTooLarge {
        /// The encoded payload's length.
        len: usize,
    },
}

/// What [`PushSender::send`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendReport {
    /// The identity switched this kind of notification off.
    Disabled,
    /// A notification for the same session (or a test) went out too
    /// recently. Nothing was sent.
    Throttled,
    /// One entry per subscription, in the order they are listed. Empty when
    /// the identity has no subscription.
    Sent(Vec<Delivery>),
}

/// The outcome for one subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delivery {
    /// The subscription's id, as listed.
    pub subscription_id: String,
    /// The push service's host.
    pub service: String,
    /// What happened.
    pub outcome: DeliveryOutcome,
}

/// What a push service said about one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryOutcome {
    /// 201 or 202: the push service has it.
    Accepted,
    /// 404 or 410: the subscription expired or was revoked by the browser.
    Gone {
        /// Whether it was removed from the store (false if it had already
        /// been removed or re-subscribed with another endpoint).
        removed: bool,
    },
    /// Not delivered. Safe to log.
    Failed(Failure),
}

/// Why a message was not delivered. Its `Display` is safe to log: it names
/// no endpoint, key or payload.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Failure {
    /// 413: the push service will not take a message this size.
    #[error("the push service refused the payload as too large: {excerpt}")]
    PayloadTooLarge {
        /// The start of the response body.
        excerpt: String,
    },
    /// 429: the push service asks to slow down.
    #[error("the push service is rate limiting: {excerpt}")]
    RateLimited {
        /// The start of the response body.
        excerpt: String,
    },
    /// 5xx: the push service failed.
    #[error("the push service failed with {status}: {excerpt}")]
    ServiceError {
        /// The status.
        status: u16,
        /// The start of the response body.
        excerpt: String,
    },
    /// A 3xx, which is never followed.
    #[error("the push service redirected with {status}; redirects are not followed")]
    Redirect {
        /// The status.
        status: u16,
    },
    /// Any other status, such as 400, or 401 and 403 when the push service
    /// does not accept the VAPID token.
    #[error("the push service refused the message with {status}: {excerpt}")]
    Refused {
        /// The status.
        status: u16,
        /// The start of the response body.
        excerpt: String,
    },
    /// No answer in time.
    #[error("no answer within {0:?}")]
    Timeout(Duration),
    /// Connecting or exchanging failed.
    #[error("{0}")]
    Transport(String),
    /// The stored endpoint is not allowed by this sender's policy.
    #[error(transparent)]
    Endpoint(EndpointError),
    /// The message could not be encrypted or signed.
    #[error("{0}")]
    Crypto(String),
}

impl Failure {
    /// Whether trying again later may succeed: 429, 5xx, a timeout or a
    /// connection failure. This crate does not retry by itself.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::ServiceError { .. }
                | Self::Timeout(_)
                | Self::Transport(_)
        )
    }
}

/// Sends notifications. Cheap to clone; clones share the throttle.
#[derive(Clone)]
pub struct PushSender {
    inner: Arc<Inner>,
}

struct Inner {
    keys: Arc<VapidKeys>,
    store: SubscriptionStore,
    http: PushHttp,
    clock: Arc<dyn Clock>,
    config: PushConfig,
    throttle: Mutex<Throttle>,
}

#[derive(Default)]
struct Throttle {
    /// Last send, in milliseconds, per (identity, session id).
    sessions: HashMap<(String, String), u64>,
    /// Last test, in milliseconds, per identity.
    tests: HashMap<String, u64>,
}

impl PushSender {
    /// A sender signing with `keys` and sending to the subscriptions in
    /// `store`, which it also updates (last success, gone subscriptions).
    pub fn new(
        keys: Arc<VapidKeys>,
        store: SubscriptionStore,
        clock: Arc<dyn Clock>,
        config: PushConfig,
    ) -> Result<Self, ConfigError> {
        let subject = config.subject.as_str();
        let valid_subject = match subject.split_once(':') {
            Some(("mailto", rest)) => rest.contains('@') && !rest.contains(char::is_whitespace),
            Some(("https", _)) => url::Url::parse(subject).is_ok_and(|url| url.host().is_some()),
            _ => false,
        };
        if !valid_subject {
            return Err(ConfigError::Subject);
        }
        let http = PushHttp::new(config.endpoint_policy, config.request_timeout)
            .map_err(ConfigError::Tls)?;
        Ok(Self {
            inner: Arc::new(Inner {
                keys,
                store,
                http,
                clock,
                config: PushConfig {
                    concurrency: config.concurrency.max(1),
                    ..config
                },
                throttle: Mutex::new(Throttle::default()),
            }),
        })
    }

    /// Sends `payload` as a notification of `kind` to every subscription of
    /// `identity` whose preferences allow `kind` (a test is always allowed).
    ///
    /// `payload.kind` is set to `kind`. `title` and `body` are cut to
    /// [`MAX_TITLE_CHARS`] and [`MAX_BODY_CHARS`] and lose control
    /// characters. At most one notification per session per minute goes out
    /// ([`SESSION_THROTTLE`]), and one test per ten seconds; a notification
    /// without a session id counts as one session. Subscriptions the push
    /// service reports gone are removed.
    pub async fn send(
        &self,
        identity: &str,
        kind: PushKind,
        payload: PushPayload,
    ) -> Result<SendReport, SendError> {
        let payload = PushPayload {
            kind,
            title: clean_text(&payload.title, MAX_TITLE_CHARS),
            body: clean_text(&payload.body, MAX_BODY_CHARS),
            ..payload
        };
        // Encoding a struct of strings into JSON cannot fail; if it ever
        // did, an empty object is refused by the app rather than misread.
        let plaintext = serde_json::to_vec(&payload).unwrap_or_else(|_| b"{}".to_vec());
        if plaintext.len() > MAX_PLAINTEXT_LEN {
            return Err(SendError::PayloadTooLarge {
                len: plaintext.len(),
            });
        }

        let (preferences, targets) = self.inner.store.targets(identity);
        if !allows(preferences, kind) {
            return Ok(SendReport::Disabled);
        }
        if targets.is_empty() {
            return Ok(SendReport::Sent(Vec::new()));
        }
        if !self.try_throttle(identity, kind, payload.session_id.as_deref()) {
            return Ok(SendReport::Throttled);
        }

        let urgency = if kind == PushKind::Waiting {
            "high"
        } else {
            "normal"
        };
        // RFC 8030 section 5.4: at most 32 characters of the base64url
        // alphabet. A hash, so the topic does not reveal the session id.
        let topic = payload
            .session_id
            .as_deref()
            .map(|session| base64url(&sha256(session.as_bytes())[..24]));
        let message = Arc::new(Message {
            plaintext,
            urgency,
            topic,
        });

        let limit = Arc::new(Semaphore::new(self.inner.config.concurrency));
        let mut tasks = JoinSet::new();
        for (index, target) in targets.into_iter().enumerate() {
            let inner = self.inner.clone();
            let identity = identity.to_owned();
            let message = message.clone();
            let limit = limit.clone();
            tasks.spawn(async move {
                // The semaphore is never closed, so acquiring cannot fail.
                let _permit = limit.acquire_owned().await;
                let delivery = inner.deliver(&identity, target, &message).await;
                (index, delivery)
            });
        }
        let mut deliveries = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(result) => deliveries.push(result),
                // A delivery task does not panic; if one did, its
                // subscription is missing from the report rather than the
                // whole send failing.
                Err(error) => tracing::error!(%error, "a push delivery task failed"),
            }
        }
        deliveries.sort_by_key(|(index, _)| *index);
        Ok(SendReport::Sent(
            deliveries
                .into_iter()
                .map(|(_, delivery)| delivery)
                .collect(),
        ))
    }

    /// Claims the throttle slot for this notification, or says it is taken.
    fn try_throttle(&self, identity: &str, kind: PushKind, session_id: Option<&str>) -> bool {
        let now = self.inner.clock.now_millis();
        let recent = |last: u64, window: Duration| {
            // A clock that stepped back does not hold notifications for long.
            now >= last && now - last < u64::try_from(window.as_millis()).unwrap_or(u64::MAX)
        };
        let mut throttle = self
            .inner
            .throttle
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        // Forget what can no longer throttle, so the maps stay small.
        throttle
            .sessions
            .retain(|_, last| recent(*last, SESSION_THROTTLE));
        throttle
            .tests
            .retain(|_, last| recent(*last, TEST_THROTTLE));
        if kind == PushKind::Test {
            if throttle.tests.contains_key(identity) {
                return false;
            }
            throttle.tests.insert(identity.to_owned(), now);
        } else {
            let key = (identity.to_owned(), session_id.unwrap_or("").to_owned());
            if throttle.sessions.contains_key(&key) {
                return false;
            }
            throttle.sessions.insert(key, now);
        }
        true
    }
}

impl fmt::Debug for PushSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PushSender")
            .field("keys", &self.inner.keys)
            .field("store", &self.inner.store)
            .field("config", &self.inner.config)
            .finish_non_exhaustive()
    }
}

struct Message {
    plaintext: Vec<u8>,
    urgency: &'static str,
    topic: Option<String>,
}

impl Inner {
    async fn deliver(&self, identity: &str, target: Target, message: &Message) -> Delivery {
        let outcome = match self.post(&target, message).await {
            Ok(outcome) => outcome,
            Err(failure) => DeliveryOutcome::Failed(failure),
        };
        let outcome = match outcome {
            DeliveryOutcome::Accepted => {
                self.store.record_success(identity, &target.id).await;
                DeliveryOutcome::Accepted
            }
            DeliveryOutcome::Gone { .. } => {
                let removed = match self
                    .store
                    .remove_gone(identity, &target.id, &target.url)
                    .await
                {
                    Ok(removed) => removed,
                    Err(error) => {
                        tracing::warn!(%error, "could not remove a gone push subscription");
                        false
                    }
                };
                DeliveryOutcome::Gone { removed }
            }
            failed => failed,
        };
        if let DeliveryOutcome::Failed(failure) = &outcome {
            tracing::warn!(
                subscription = %target.id,
                service = %target.service,
                retryable = failure.is_retryable(),
                %failure,
                "push not delivered"
            );
        }
        Delivery {
            subscription_id: target.id,
            service: target.service,
            outcome,
        }
    }

    async fn post(&self, target: &Target, message: &Message) -> Result<DeliveryOutcome, Failure> {
        // Checked again here: the store may hold an endpoint a development
        // server accepted, and this sender's policy may be stricter.
        validate_endpoint(target.url.as_str(), self.config.endpoint_policy)
            .map_err(Failure::Endpoint)?;
        let body = encrypt(&message.plaintext, &target.keys)
            .map_err(|error| Failure::Crypto(error.to_string()))?;
        let authorization = self
            .keys
            .authorization(
                &audience(&target.url),
                &self.config.subject,
                self.clock.now_secs(),
            )
            .map_err(|error| Failure::Crypto(error.to_string()))?;
        let mut authorization = HeaderValue::try_from(authorization)
            .map_err(|error| Failure::Crypto(error.to_string()))?;
        // Keeps the token out of any Debug output of the request.
        authorization.set_sensitive(true);

        let mut headers = vec![
            (header::AUTHORIZATION, authorization),
            (HeaderName::from_static("ttl"), HeaderValue::from(TTL_SECS)),
            (
                header::CONTENT_ENCODING,
                HeaderValue::from_static("aes128gcm"),
            ),
            (
                HeaderName::from_static("urgency"),
                HeaderValue::from_static(message.urgency),
            ),
        ];
        if let Some(topic) = &message.topic
            && let Ok(value) = HeaderValue::try_from(topic.as_str())
        {
            headers.push((HeaderName::from_static("topic"), value));
        }

        let response =
            self.http
                .post(&target.url, headers, body)
                .await
                .map_err(|error| match error {
                    HttpError::Timeout(after) => Failure::Timeout(after),
                    HttpError::Transport(text) => Failure::Transport(text),
                })?;
        let excerpt = response.excerpt;
        match response.status {
            201 | 202 => Ok(DeliveryOutcome::Accepted),
            404 | 410 => Ok(DeliveryOutcome::Gone { removed: false }),
            413 => Err(Failure::PayloadTooLarge { excerpt }),
            429 => Err(Failure::RateLimited { excerpt }),
            status @ 300..=399 => Err(Failure::Redirect { status }),
            status @ 500..=599 => Err(Failure::ServiceError { status, excerpt }),
            status => Err(Failure::Refused { status, excerpt }),
        }
    }
}

fn allows(preferences: PushPreferences, kind: PushKind) -> bool {
    match kind {
        PushKind::Waiting => preferences.waiting,
        PushKind::Review => preferences.review,
        PushKind::Failed => preferences.failed,
        PushKind::Merged => preferences.merged,
        PushKind::Test => true,
    }
}

#[cfg(test)]
#[path = "sender_tests.rs"]
mod tests;
