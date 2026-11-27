//! The caller's own account: `GET /me`, and the sessions and device tokens
//! of the signed-in identity (remote-access spec, sections 3 and 4). Every
//! route acts on the caller's identity only; someone else's id is unknown.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use calin_auth::{Authenticated, Credential, SessionToken, StoreError};
use calin_proto::{
    AuthModeKind, AuthSessionInfo, CreateDeviceTokenRequest, CreatedDeviceToken, DeviceTokenInfo,
    Me,
};

use crate::AppState;
use crate::api::{ApiFailure, ApiJson, ApiResult};
use crate::auth::{Caller, SignIn};

/// The contract's bound on a device token's name, in characters.
const MAX_TOKEN_NAME_CHARS: usize = 80;

/// Unix seconds, as the store keeps time, to the milliseconds of the wire.
fn millis(seconds: u64) -> i64 {
    i64::try_from(seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000)
}

fn session_required() -> ApiFailure {
    ApiFailure::new(StatusCode::UNAUTHORIZED, "sign-in is required")
}

/// The signed-in caller, or the refusal: 409 in local mode, where there is
/// nobody to have sessions or tokens.
pub(crate) fn signed_in(
    state: &AppState,
    caller: Caller,
) -> ApiResult<(Arc<SignIn>, Authenticated)> {
    match (state.sign_in.clone(), caller) {
        (Some(sign_in), Caller::SignedIn(who)) => Ok((sign_in, who)),
        (None, _) | (_, Caller::Local) => Err(ApiFailure::new(
            StatusCode::CONFLICT,
            "this server runs in local mode: nobody signs in",
        )),
        (Some(_), Caller::Anonymous) => Err(session_required()),
    }
}

/// A revocation that took effect in memory but is not in the file yet: the
/// caller must not believe it is durable, since a restart could undo it.
fn not_durable(error: &StoreError) -> ApiFailure {
    tracing::error!(%error, "a revocation is not on disk yet");
    ApiFailure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "revoked, but not yet saved: try again",
    )
}

pub(crate) async fn me(Extension(caller): Extension<Caller>) -> ApiResult<Json<Me>> {
    match caller {
        Caller::Local => Ok(Json(Me {
            mode: AuthModeKind::Local,
            identity: None,
            device_token: false,
        })),
        Caller::SignedIn(who) => Ok(Json(Me {
            mode: AuthModeKind::Public,
            identity: Some(who.identity.to_string()),
            device_token: matches!(who.credential, Credential::DeviceToken { .. }),
        })),
        Caller::Anonymous => Err(session_required()),
    }
}

pub(crate) async fn list_auth_sessions(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<AuthSessionInfo>>> {
    let (sign_in, who) = signed_in(&state, caller)?;
    let current = match &who.credential {
        Credential::Session { id } => Some(id.as_str()),
        Credential::DeviceToken { .. } => None,
    };
    // The store lists most recently used first; the current one goes on top.
    let mut sessions: Vec<AuthSessionInfo> = sign_in
        .store()
        .list_sessions(&who.identity)
        .into_iter()
        .map(|session| AuthSessionInfo {
            current: current == Some(session.id.as_str()),
            id: session.id,
            user_agent: Some(session.user_agent).filter(|agent| !agent.is_empty()),
            created_at: millis(session.created),
            last_used_at: millis(session.last_used),
        })
        .collect();
    sessions.sort_by_key(|session| !session.current);
    Ok(Json(sessions))
}

pub(crate) async fn revoke_auth_session(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let (sign_in, who) = signed_in(&state, caller)?;
    let revoked = sign_in.store().revoke_session(&who.identity, &id).await;
    sign_in.credentials_ended();
    if !revoked.map_err(|error| not_durable(&error))? {
        return Err(ApiFailure::new(StatusCode::NOT_FOUND, "no such session"));
    }
    let mut response = StatusCode::NO_CONTENT.into_response();
    if who.credential == (Credential::Session { id }) {
        // The caller just ended its own session: its cookie goes too.
        response.headers_mut().append(
            SET_COOKIE,
            HeaderValue::from_static(SessionToken::clear_cookie()),
        );
    }
    Ok(response)
}

pub(crate) async fn list_device_tokens(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
) -> ApiResult<Json<Vec<DeviceTokenInfo>>> {
    let (sign_in, who) = signed_in(&state, caller)?;
    let mut tokens = sign_in.store().list_device_tokens(&who.identity);
    // The store lists oldest first, the contract newest first.
    tokens.reverse();
    Ok(Json(tokens.into_iter().map(token_info).collect()))
}

fn token_info(token: calin_auth::DeviceTokenInfo) -> DeviceTokenInfo {
    DeviceTokenInfo {
        id: token.id,
        name: token.name,
        created_at: millis(token.created),
        last_used_at: token.last_used.map(millis),
    }
}

pub(crate) async fn create_device_token(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    ApiJson(request): ApiJson<CreateDeviceTokenRequest>,
) -> ApiResult<(StatusCode, Json<CreatedDeviceToken>)> {
    let (sign_in, who) = signed_in(&state, caller)?;
    // Spec section 4: a token is created by a signed-in user. A token that
    // could mint tokens would outlive its own revocation.
    if matches!(who.credential, Credential::DeviceToken { .. }) {
        return Err(ApiFailure::new(
            StatusCode::FORBIDDEN,
            "a device token cannot create device tokens: sign in with a browser",
        ));
    }
    let invalid_name = || {
        ApiFailure::new(
            StatusCode::BAD_REQUEST,
            format!(
                "the name must be 1 to {MAX_TOKEN_NAME_CHARS} characters, without control characters"
            ),
        )
    };
    let name = request.name.trim();
    let length = name.chars().count();
    if length == 0 || length > MAX_TOKEN_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(invalid_name());
    }
    let created = sign_in
        .store()
        .create_device_token(&who.identity, name)
        .await
        .map_err(|error| match error {
            StoreError::InvalidName => invalid_name(),
            StoreError::TooManyDeviceTokens => {
                ApiFailure::new(StatusCode::CONFLICT, error.to_string())
            }
            // The identity left the allow-list between the two checks.
            StoreError::NotAllowed(_) => session_required(),
            error => {
                tracing::error!(%error, "could not create a device token");
                ApiFailure::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
            }
        })?;
    let body = CreatedDeviceToken {
        token: created.token.expose().to_owned(),
        info: token_info(created.info),
    };
    Ok((StatusCode::CREATED, Json(body)))
}

pub(crate) async fn revoke_device_token(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    let (sign_in, who) = signed_in(&state, caller)?;
    let revoked = sign_in
        .store()
        .revoke_device_token(&who.identity, &id)
        .await;
    sign_in.credentials_ended();
    if !revoked.map_err(|error| not_durable(&error))? {
        return Err(ApiFailure::new(
            StatusCode::NOT_FOUND,
            "no such device token",
        ));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seconds_become_milliseconds_without_overflow() {
        assert_eq!(millis(0), 0);
        assert_eq!(millis(1_800_000_000), 1_800_000_000_000);
        assert_eq!(millis(u64::MAX), i64::MAX);
    }
}
