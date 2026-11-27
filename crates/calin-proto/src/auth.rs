//! Sign-in, sessions and device tokens. The rules are in
//! `docs/specs/2026-11-27-remote-access.md`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AuthModeKind {
    /// Loopback only; nobody signs in.
    Local,
    /// Reached through a public URL; sign-in is required.
    Public,
}

/// Who is calling. `GET /api/v1/me`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Me {
    pub mode: AuthModeKind,
    /// The signed-in identity: an email address, or `sub:<subject>`. Absent in local mode.
    pub identity: Option<String>,
    /// True when the call was authenticated by a device token rather than a browser session.
    #[serde(default)]
    pub device_token: bool,
}

/// What `GET /api/v1/health` returns to a caller that is not signed in, in public mode.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PublicHealth {
    /// Always `calin`.
    pub name: String,
    /// Always `required`.
    pub auth: String,
}

/// A browser session of the signed-in identity.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AuthSessionInfo {
    /// Identifies the session for revocation. It is not the session's secret.
    pub id: String,
    pub user_agent: Option<String>,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub last_used_at: i64,
    /// True for the session making this request.
    pub current: bool,
}

/// A device token of the signed-in identity. Its secret is never listed.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct DeviceTokenInfo {
    pub id: String,
    pub name: String,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number | null")]
    pub last_used_at: Option<i64>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CreateDeviceTokenRequest {
    /// What the token is for, 1 to 80 characters.
    pub name: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CreatedDeviceToken {
    pub info: DeviceTokenInfo,
    /// The secret, shown this once. It starts with `calin_`.
    pub token: String,
}
