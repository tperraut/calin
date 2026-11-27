//! Web Push notifications to Calin's installed web app
//! (`docs/specs/2026-11-27-remote-access.md`, section 6).
//!
//! A library with no web framework in it: the server adds the routes and
//! decides when to notify. The pieces:
//!
//! - [`VapidKeys`]: the server's P-256 key pair (RFC 8292), created on first
//!   use in a file the caller names and loaded afterwards. Its public key is
//!   what `GET /api/v1/push/key` returns. A corrupt key file is an error,
//!   never silently replaced, because every subscription is bound to it.
//! - [`SubscriptionStore`]: subscriptions and [`PushPreferences`] per
//!   identity (an opaque string from the caller), in one mode-0600 file.
//!   Validates endpoints ([`validate_endpoint`]) and keys on the way in.
//! - [`PushSender`]: encrypts a [`PushPayload`] for each subscription
//!   (RFC 8291 over the `aes128gcm` encoding of RFC 8188), signs a VAPID
//!   token, and posts it to the push service (RFC 8030), several at a time.
//!   It applies the preferences and the throttle, and removes subscriptions
//!   the push service reports gone.
//!
//! Cryptography is `ring`; TLS is rustls on `ring` with the Mozilla roots.
//!
//! # Sequence
//!
//! ```no_run
//! # async fn example(data_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
//! use std::sync::Arc;
//! use calin_proto::{PushKind, PushPayload, PushSubscriptionRequest};
//! use calin_push::{
//!     EndpointPolicy, PushConfig, PushSender, SubscriptionStore, SystemClock, VapidKeys,
//! };
//!
//! // At startup: open both files and build one sender.
//! let clock = Arc::new(SystemClock);
//! let keys = Arc::new(VapidKeys::load_or_generate(data_dir.join("vapid.json")).await?);
//! let store = SubscriptionStore::open(data_dir.join("push.json"), clock.clone()).await?;
//! let sender = PushSender::new(
//!     keys.clone(),
//!     store.clone(),
//!     clock,
//!     PushConfig::new("mailto:admin@example.com"),
//! )?;
//!
//! // GET /api/v1/push/key
//! let key = keys.push_key();
//!
//! // POST /api/v1/push/subscriptions, for the signed-in identity.
//! # let request: PushSubscriptionRequest = unimplemented!();
//! let info = store
//!     .subscribe("alice@example.com", &request, Some("Mozilla/5.0 ..."), EndpointPolicy::PublicHttpsOnly)
//!     .await?;
//!
//! // When a session starts waiting.
//! let report = sender
//!     .send(
//!         "alice@example.com",
//!         PushKind::Waiting,
//!         PushPayload {
//!             kind: PushKind::Waiting,
//!             title: "Fix the login bug".into(),
//!             body: "Claude asks to run a command".into(),
//!             path: "/s/0193".into(),
//!             session_id: Some("0193".into()),
//!         },
//!     )
//!     .await?;
//!
//! // On shutdown: write pending "last sent" times.
//! store.flush().await?;
//! # let _ = (key, info, report);
//! # Ok(())
//! # }
//! ```

mod clock;
mod ece;
mod endpoint;
mod fsio;
mod http;
mod secret;
mod sender;
mod store;
mod user_agent;
mod vapid;

pub use calin_proto::{
    PushKey, PushKind, PushPayload, PushPreferences, PushSubscriptionInfo, PushSubscriptionRequest,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use ece::EncryptError;
pub use endpoint::{EndpointError, EndpointPolicy, MAX_ENDPOINT_LEN, validate_endpoint};
pub use secret::RandomError;
pub use sender::{
    ConfigError, Delivery, DeliveryOutcome, Failure, MAX_BODY_CHARS, MAX_TITLE_CHARS, PushConfig,
    PushSender, SESSION_THROTTLE, SendError, SendReport, TEST_THROTTLE, TTL_SECS,
};
pub use store::{
    LoadReport, LoadedFile, MAX_LABEL_CHARS, MAX_SUBSCRIPTIONS_PER_IDENTITY, StoreError,
    SubscribeError, SubscriptionStore,
};
pub use user_agent::describe_user_agent;
pub use vapid::{VapidError, VapidKeys};
