//! Notifications to an installed web app through Web Push. The rules are in
//! `docs/specs/2026-11-27-remote-access.md`, section 6.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// `GET /api/v1/push/key`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PushKey {
    /// The server's VAPID public key, base64url without padding: what a
    /// browser passes as `applicationServerKey` when it subscribes.
    pub public_key: String,
}

/// A browser's push subscription, as `PushSubscription.toJSON()` gives it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PushSubscriptionRequest {
    /// The push service URL for this browser. Must be `https`.
    pub endpoint: String,
    /// The subscription's public key (`keys.p256dh`), base64url.
    pub p256dh: String,
    /// The subscription's authentication secret (`keys.auth`), base64url.
    pub auth: String,
    /// What the user calls this device. Derived from the user agent when absent.
    #[serde(default)]
    #[ts(optional)]
    pub label: Option<String>,
}

/// A device that receives notifications for the signed-in identity.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PushSubscriptionInfo {
    pub id: String,
    pub label: String,
    /// Lowercase hex SHA-256 of the endpoint URL, so a browser can recognize
    /// its own subscription without the server listing endpoints.
    pub endpoint_sha256: String,
    /// The push service's host name, for example `fcm.googleapis.com`.
    pub service: String,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub created_at: i64,
    /// When a notification was last accepted by the push service.
    #[ts(type = "number | null")]
    pub last_sent_at: Option<i64>,
}

/// Which events notify. All true by default.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct PushPreferences {
    /// A session started waiting: a permission request, a question or a gate.
    pub waiting: bool,
    /// A session reached review.
    pub review: bool,
    /// A session failed.
    pub failed: bool,
    /// Every pull request of a session was merged.
    pub merged: bool,
}

impl Default for PushPreferences {
    fn default() -> Self {
        Self {
            waiting: true,
            review: true,
            failed: true,
            merged: true,
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PushKind {
    Waiting,
    Review,
    Failed,
    Merged,
    /// Sent by `POST /api/v1/push/test`.
    Test,
}

/// The decrypted content of a push message: what the app's service worker receives.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PushPayload {
    pub kind: PushKind,
    /// The session's title, or the product name for a test.
    pub title: String,
    /// One line saying why the user is needed.
    pub body: String,
    /// The path to open, for example `/s/<id>`.
    pub path: String,
    /// Absent for a test.
    pub session_id: Option<String>,
}
