//! REST handlers. The route table lives in `calin-proto/src/api.rs`; the
//! handlers only translate between HTTP and `calin-core`.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use calin_proto::{
    ApiError, CreateSessionRequest, Health, SendMessageRequest, Session, SessionDetail,
};
use serde::de::DeserializeOwned;

use crate::AppState;

/// An error response: a status and an [`ApiError`] body.
#[derive(Debug)]
pub(crate) struct ApiFailure {
    status: StatusCode,
    message: String,
}

impl ApiFailure {
    pub(crate) fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        let body = ApiError {
            error: self.message,
        };
        (self.status, Json(body)).into_response()
    }
}

impl From<calin_core::Error> for ApiFailure {
    fn from(err: calin_core::Error) -> Self {
        use calin_core::Error;
        let status = match &err {
            Error::Invalid(_) => StatusCode::BAD_REQUEST,
            Error::NotFound(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Closed
            | Error::InUse(_)
            | Error::Corrupt { .. }
            | Error::Io(_)
            | Error::Index(_)
            | Error::Json(_) => {
                // The details are for the log, not for whoever called.
                tracing::error!(%err, "request failed");
                return Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        };
        Self::new(status, err.to_string())
    }
}

type ApiResult<T> = Result<T, ApiFailure>;

/// `Json`, but a body that does not parse is a 400 with an [`ApiError`]
/// like every other invalid input.
pub(crate) struct ApiJson<T>(T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiFailure;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => {
                let status = match rejection {
                    JsonRejection::BytesRejection(_) => rejection.status(),
                    _ => StatusCode::BAD_REQUEST,
                };
                Err(ApiFailure::new(status, rejection.body_text()))
            }
        }
    }
}

pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    let registry = state.manager.registry();
    Json(Health {
        name: "calin".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        dry_run: registry.dry_run(),
        runners: registry.ids(),
    })
}

pub(crate) async fn list_sessions(State(state): State<AppState>) -> ApiResult<Json<Vec<Session>>> {
    Ok(Json(state.manager.list().await?))
}

pub(crate) async fn create_session(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateSessionRequest>,
) -> ApiResult<(StatusCode, Json<SessionDetail>)> {
    let detail = state.manager.create_session(request).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

pub(crate) async fn get_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<SessionDetail>> {
    Ok(Json(state.manager.get(&session_id).await?))
}

pub(crate) async fn send_message(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
    ApiJson(request): ApiJson<SendMessageRequest>,
) -> ApiResult<StatusCode> {
    state
        .manager
        .send_message(&session_id, &request.text)
        .await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn stop_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<StatusCode> {
    state.manager.stop(&session_id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn method_not_allowed() -> ApiFailure {
    ApiFailure::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "this route does not support that method",
    )
}
