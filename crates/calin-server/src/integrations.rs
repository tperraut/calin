//! Usage and limits, issues and automations: the routes of
//! `docs/specs/2026-11-28-integrations.md`. Thin: `calin-core` does the work.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use calin_proto::{
    Automation, AutomationRequest, AutomationRun, CronPreview, Issue, IssueDetail, IssueState,
    RunAutomationRequest, RunnerLimits, Usage, UsageBy,
};
use serde::Deserialize;

use crate::AppState;
use crate::api::{ApiFailure, ApiJson, ApiQuery, ApiResult};

#[derive(Deserialize)]
pub(crate) struct IssuesQuery {
    #[serde(default)]
    state: Option<IssueState>,
    #[serde(default)]
    q: Option<String>,
}

/// `GET /repos/{id}/issues?state=&q=`: the repo's GitHub issues through
/// `gh`, newest first, at most 50. 409 with the reason when the repo is not
/// on GitHub or `gh` cannot be used. In dry run they come from a small
/// built-in set of made-up issues per repo (titles start with "Sample
/// issue:"), so the flow can be shown and tested without GitHub.
pub(crate) async fn list_issues(
    State(state): State<AppState>,
    Path(repo_id): Path<String>,
    ApiQuery(query): ApiQuery<IssuesQuery>,
) -> ApiResult<Json<Vec<Issue>>> {
    let wanted = query.state.unwrap_or(IssueState::Open);
    let issues = state.manager.list_issues(&repo_id, wanted, query.q).await?;
    Ok(Json(issues))
}

/// `GET /repos/{id}/issues/{number}`: one issue with its body. 404 for an
/// unknown repo or issue. In dry run, one of the made-up issues.
pub(crate) async fn get_issue(
    State(state): State<AppState>,
    Path((repo_id, number)): Path<(String, String)>,
) -> ApiResult<Json<IssueDetail>> {
    let number: u32 = number.parse().map_err(|_| {
        ApiFailure::new(
            StatusCode::BAD_REQUEST,
            format!("the issue number must be a positive whole number, not {number:?}"),
        )
    })?;
    Ok(Json(state.manager.issue(&repo_id, number).await?))
}

#[derive(Deserialize)]
pub(crate) struct UsageQuery {
    #[serde(default)]
    from: Option<i64>,
    #[serde(default)]
    to: Option<i64>,
    #[serde(default)]
    by: Option<UsageBy>,
}

/// `GET /usage?from=&to=&by=`.
pub(crate) async fn usage(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<UsageQuery>,
) -> ApiResult<Json<Usage>> {
    let by = query.by.unwrap_or_default();
    Ok(Json(state.manager.usage(query.from, query.to, by).await?))
}

/// `GET /usage/limits`: the newest limits of each runner since the start.
pub(crate) async fn limits(State(state): State<AppState>) -> Json<Vec<RunnerLimits>> {
    Json(state.manager.limits())
}

pub(crate) async fn list_automations(
    State(state): State<AppState>,
) -> ApiResult<Json<Vec<Automation>>> {
    Ok(Json(state.manager.list_automations().await?))
}

pub(crate) async fn create_automation(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<AutomationRequest>,
) -> ApiResult<(StatusCode, Json<Automation>)> {
    let automation = state.manager.create_automation(request).await?;
    Ok((StatusCode::CREATED, Json(automation)))
}

#[derive(Deserialize)]
pub(crate) struct CronQuery {
    /// Absent reads as empty, which says that it is.
    #[serde(default)]
    expr: String,
}

/// `GET /automations/cron?expr=`: always 200; an invalid expression says why.
pub(crate) async fn cron_preview(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<CronQuery>,
) -> Json<CronPreview> {
    Json(state.manager.cron_preview(&query.expr))
}

pub(crate) async fn get_automation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Automation>> {
    Ok(Json(state.manager.automation(&id).await?))
}

pub(crate) async fn update_automation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(request): ApiJson<AutomationRequest>,
) -> ApiResult<Json<Automation>> {
    Ok(Json(state.manager.update_automation(&id, request).await?))
}

pub(crate) async fn delete_automation(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    state.manager.delete_automation(&id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn run_automation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<serde_json::Value>,
) -> ApiResult<(StatusCode, Json<AutomationRun>)> {
    // Every field is optional, so a list would read as a request for nothing in particular.
    if !body.is_object() {
        return Err(ApiFailure::new(
            StatusCode::BAD_REQUEST,
            "the request must be a JSON object",
        ));
    }
    let request: RunAutomationRequest = serde_json::from_value(body)
        .map_err(|err| ApiFailure::new(StatusCode::BAD_REQUEST, err.to_string()))?;
    let run = state.manager.run_automation(&id, request).await?;
    Ok((StatusCode::CREATED, Json(run)))
}

pub(crate) async fn automation_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<Vec<AutomationRun>>> {
    Ok(Json(state.manager.automation_runs(&id).await?))
}
