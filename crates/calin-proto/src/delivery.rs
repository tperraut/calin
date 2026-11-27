use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PullRequestState {
    Draft,
    Open,
    Merged,
    Closed,
}

/// The combined result of a pull request's checks.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum CheckState {
    /// The repo runs no checks, or none has been reported yet.
    None,
    Pending,
    Passing,
    Failing,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum ReviewState {
    None,
    Approved,
    ChangesRequested,
}

/// The pull request of one repo of a session. A session has at most one per repo.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PullRequest {
    pub session_repo_id: String,
    pub number: u32,
    pub url: String,
    pub title: String,
    pub state: PullRequestState,
    pub checks: CheckState,
    pub review: ReviewState,
    /// Unix time in milliseconds of the last change Calin observed.
    #[ts(type = "number")]
    pub updated_at: i64,
}

/// Opens, or updates, the session's pull requests.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct ShipRequest {
    /// Title of each pull request and of the commit Calin makes for
    /// uncommitted work. The session's title when absent.
    #[serde(default)]
    #[ts(optional)]
    pub title: Option<String>,
    /// Markdown placed at the top of each pull request's description.
    #[serde(default)]
    #[ts(optional)]
    pub body: Option<String>,
    /// Open as drafts. True when absent.
    #[serde(default)]
    #[ts(optional)]
    pub draft: Option<bool>,
}
