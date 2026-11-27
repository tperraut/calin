//! Usage, plan limits, issues and automations. The rules are in
//! `docs/specs/2026-11-28-integrations.md`.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{AttachRepoRequest, PermissionMode, SessionStatus};

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq)]
#[ts(export)]
pub struct UsageTotals {
    pub cost_usd: f64,
    #[ts(type = "number")]
    pub tokens_in: u64,
    #[ts(type = "number")]
    pub tokens_out: u64,
    /// Root sessions that used anything in the range.
    pub sessions: u32,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum UsageBy {
    #[default]
    Day,
    Session,
    Repo,
    Runner,
    Model,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct UsageRow {
    /// What the row is about: a local date `YYYY-MM-DD`, a session id, a repo
    /// id (empty for sessions without a repo), a runner id or a model name.
    pub key: String,
    /// The same, for display: the date, the session's title, the repo's name.
    pub label: String,
    pub totals: UsageTotals,
}

/// `GET /api/v1/usage`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Usage {
    /// Unix milliseconds, as applied after defaults and clamping.
    #[ts(type = "number")]
    pub from: i64,
    #[ts(type = "number")]
    pub to: i64,
    pub by: UsageBy,
    pub totals: UsageTotals,
    pub rows: Vec<UsageRow>,
}

/// One window of the agent plan's limits, as the agent CLI reports it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct LimitWindow {
    /// The CLI's name for it, for example `five_hour` or `seven_day`.
    pub name: String,
    /// The share of the window already used, from 0 to 1.
    pub used: f64,
    /// When the window resets, in Unix milliseconds, when the CLI says.
    #[ts(type = "number | null")]
    pub resets_at: Option<i64>,
    /// True while requests are refused because of this window.
    pub refused: bool,
}

/// The newest limits a runner reported. `GET /api/v1/usage/limits` returns one per runner.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct RunnerLimits {
    pub runner: String,
    pub windows: Vec<LimitWindow>,
    #[ts(type = "number")]
    pub reported_at: i64,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum IssueState {
    Open,
    Closed,
}

/// A GitHub issue of a repo, as listed.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Issue {
    pub number: u32,
    pub title: String,
    pub state: IssueState,
    pub labels: Vec<String>,
    pub author: Option<String>,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub url: String,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct IssueDetail {
    pub issue: Issue,
    /// Markdown.
    pub body: String,
}

/// The issue a session was started from.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct IssueRef {
    pub repo_id: String,
    pub number: u32,
    pub title: String,
    pub url: String,
}

/// Names the issue to start a session from.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct FromIssue {
    pub repo_id: String,
    pub number: u32,
}

/// What starts an automation.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[ts(export)]
pub enum AutomationTrigger {
    /// A five-field cron expression in the server's local time.
    Schedule { cron: String },
    /// A new open issue in a repo, with this label when one is given.
    Issue {
        repo_id: String,
        label: Option<String>,
    },
    /// A new open pull request in a repo.
    PullRequest { repo_id: String },
}

/// The session an automation starts. `prompt` is a template.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AutomationSession {
    pub prompt: String,
    #[serde(default)]
    pub workflow: Option<String>,
    #[serde(default)]
    pub repos: Vec<AttachRepoRequest>,
    #[serde(default)]
    pub runner: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// `autonomous` or `accept_edits`. `ask` is refused.
    pub permission_mode: PermissionMode,
}

/// The body of `POST /automations` and `PUT /automations/{id}`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AutomationRequest {
    /// 1 to 80 characters.
    pub name: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub session: AutomationSession,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum AutomationOutcome {
    /// A session was started.
    Started,
    /// Nothing was started: the previous session of this automation is still active.
    Skipped,
    /// The session could not be created.
    Failed,
}

/// One time an automation fired.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct AutomationRun {
    pub id: String,
    #[ts(type = "number")]
    pub at: i64,
    /// What fired it, in words: `schedule`, `issue #12`, `pull request #7` or `manual`.
    pub cause: String,
    pub outcome: AutomationOutcome,
    pub session_id: Option<String>,
    /// The started session's status now.
    pub session_status: Option<SessionStatus>,
    /// Why it was skipped or failed.
    pub detail: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Automation {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub trigger: AutomationTrigger,
    pub session: AutomationSession,
    /// The trigger in words, for example "Every weekday at 08:00".
    pub trigger_text: String,
    /// For a schedule that is enabled: the next time it will fire.
    #[ts(type = "number | null")]
    pub next_run_at: Option<i64>,
    /// For an issue or pull request trigger: when GitHub was last asked.
    #[ts(type = "number | null")]
    pub last_poll_at: Option<i64>,
    /// Why the last poll failed, when it did.
    pub last_poll_problem: Option<String>,
    pub last_run: Option<AutomationRun>,
    /// Set when Calin switched the automation off after repeated failures.
    pub disabled_reason: Option<String>,
    #[ts(type = "number")]
    pub created_at: i64,
}

/// The body of `POST /automations/{id}/run`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, Default, PartialEq)]
#[ts(export)]
pub struct RunAutomationRequest {
    /// For an issue or pull request trigger: the item to run on. Required there.
    #[serde(default)]
    #[ts(optional)]
    pub number: Option<u32>,
}

/// `GET /automations/cron?expr=`: what a cron expression means.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CronPreview {
    /// The expression in words. Absent when it is not valid.
    pub text: Option<String>,
    /// The next three times it would fire, in Unix milliseconds.
    #[ts(type = "Array<number>")]
    pub next: Vec<i64>,
    /// Why the expression is not valid.
    pub problem: Option<String>,
}
