//! REST handlers. The route table lives in `calin-proto/src/api.rs`; the
//! handlers only translate between HTTP and `calin-core`.

use axum::Json;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use calin_proto::{
    ApiError, CreateSessionRequest, FileContent, FileDiff, Health, RegisterRepoRequest, Repo,
    RepoChanges, ResolvePermissionRequest, SendMessageRequest, Session, SessionDetail, TreeEntry,
    WriteFileRequest,
};
use serde::Deserialize;
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
            Error::NotFound(_) | Error::Missing(_) => StatusCode::NOT_FOUND,
            Error::Conflict(_) => StatusCode::CONFLICT,
            Error::Closed
            | Error::InUse(_)
            | Error::Corrupt { .. }
            | Error::Git(_)
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

/// `Query`, but a query string that does not parse is a 400 with an
/// [`ApiError`] like every other invalid input.
pub(crate) struct ApiQuery<T>(pub(crate) T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiFailure;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        match Query::<T>::from_request_parts(parts, state).await {
            Ok(Query(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiFailure::new(
                StatusCode::BAD_REQUEST,
                rejection.body_text(),
            )),
        }
    }
}

/// The `path` parameter of the file routes: relative to the repo root.
/// Absent means the root, which only `tree` accepts.
#[derive(Deserialize)]
pub(crate) struct PathQuery {
    #[serde(default)]
    path: String,
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

pub(crate) async fn list_repos(State(state): State<AppState>) -> Json<Vec<Repo>> {
    Json(state.manager.list_repos().await)
}

pub(crate) async fn register_repo(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<RegisterRepoRequest>,
) -> ApiResult<(StatusCode, Json<Repo>)> {
    let repo = state.manager.register_repo(request).await?;
    Ok((StatusCode::CREATED, Json(repo)))
}

pub(crate) async fn remove_repo(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
) -> ApiResult<StatusCode> {
    state.manager.remove_repo(&repo_id).await?;
    Ok(StatusCode::NO_CONTENT)
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

pub(crate) async fn interrupt_session(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<StatusCode> {
    state.manager.interrupt(&session_id).await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn resolve_permission(
    State(state): State<AppState>,
    Path((session_id, request_id)): Path<(String, String)>,
    ApiJson(request): ApiJson<ResolvePermissionRequest>,
) -> ApiResult<StatusCode> {
    state
        .manager
        .resolve_permission(&session_id, &request_id, request.decision, request.message)
        .await?;
    Ok(StatusCode::ACCEPTED)
}

pub(crate) async fn session_changes(
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> ApiResult<Json<Vec<RepoChanges>>> {
    Ok(Json(state.manager.changes(&session_id).await?))
}

pub(crate) async fn file_diff(
    State(state): State<AppState>,
    Path((session_id, session_repo_id)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<PathQuery>,
) -> ApiResult<Json<FileDiff>> {
    let diff = state
        .manager
        .file_diff(&session_id, &session_repo_id, &query.path)
        .await?;
    Ok(Json(diff))
}

pub(crate) async fn tree(
    State(state): State<AppState>,
    Path((session_id, session_repo_id)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<PathQuery>,
) -> ApiResult<Json<Vec<TreeEntry>>> {
    let entries = state
        .manager
        .tree(&session_id, &session_repo_id, &query.path)
        .await?;
    Ok(Json(entries))
}

pub(crate) async fn read_file(
    State(state): State<AppState>,
    Path((session_id, session_repo_id)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<PathQuery>,
) -> ApiResult<Json<FileContent>> {
    let content = state
        .manager
        .read_file(&session_id, &session_repo_id, &query.path)
        .await?;
    Ok(Json(content))
}

pub(crate) async fn write_file(
    State(state): State<AppState>,
    Path((session_id, session_repo_id)): Path<(String, String)>,
    ApiQuery(query): ApiQuery<PathQuery>,
    ApiJson(request): ApiJson<WriteFileRequest>,
) -> ApiResult<Json<FileContent>> {
    let content = state
        .manager
        .write_file(&session_id, &session_repo_id, &query.path, request)
        .await?;
    Ok(Json(content))
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
