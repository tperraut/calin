//! Issues and open pull requests, for starting work from them
//! (integrations spec, sections 2 and 3). Only a repository whose `origin`
//! is on GitHub is asked about; any other is refused before `gh` runs.

use std::path::Path;

use calin_proto::{Issue, IssueDetail, IssueState};
use serde::Deserialize;

use super::{GitHub, parse, unexpected};
use crate::cmd::{Git, work_dir};
use crate::error::{Error, Result};

/// The remote whose host decides whether a repository is on GitHub.
const REMOTE: &str = "origin";
/// What is asked of `gh` about an issue, in its `--json` vocabulary.
const ISSUE_FIELDS: &str = "number,title,state,labels,author,updatedAt,url,body";
/// And about a pull request that may start an automation.
const PULL_REQUEST_FIELDS: &str = "number,title,body,url,author,updatedAt";
/// `gh` says this when the number names nothing in the repository.
const NO_SUCH_NUMBER: &str = "Could not resolve to";

/// Which issues to list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueQuery {
    pub state: IssueState,
    /// GitHub's search syntax; plain words match the title and the body.
    pub search: Option<String>,
    /// Only issues with this label.
    pub label: Option<String>,
    /// At most this many, newest first.
    pub limit: u32,
}

/// An open pull request, as an automation sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestItem {
    pub number: u32,
    pub title: String,
    /// Markdown.
    pub body: String,
    pub url: String,
    pub author: Option<String>,
    /// Unix milliseconds.
    pub updated_at: i64,
}

impl GitHub {
    /// The issues of the repository `dir` is a clone of, with their bodies,
    /// newest first.
    pub fn list_issues(&self, dir: &Path, query: &IssueQuery) -> Result<Vec<IssueDetail>> {
        let dir = on_github(dir)?;
        let state = match query.state {
            IssueState::Open => "--state=open",
            IssueState::Closed => "--state=closed",
        };
        let limit = format!("--limit={}", query.limit.max(1));
        let json = format!("--json={ISSUE_FIELDS}");
        let mut args = vec![
            "issue".to_owned(),
            "list".to_owned(),
            state.to_owned(),
            limit,
            json,
        ];
        // `--flag=value`: a search that starts with a hyphen stays a search.
        if let Some(search) = query.search.as_deref().filter(|s| !s.trim().is_empty()) {
            args.push(format!("--search={}", search.trim()));
        }
        if let Some(label) = query.label.as_deref().filter(|l| !l.trim().is_empty()) {
            args.push(format!("--label={}", label.trim()));
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let ran = self.gh(&dir, "issue list", &args, None)?;
        let listed: Vec<RawIssue> = parse("issue list", &ran.stdout)?;
        let mut issues = listed
            .into_iter()
            .map(|raw| raw.into_detail("issue list"))
            .collect::<Result<Vec<_>>>()?;
        // Highest number first is newest first, whatever order `gh` keeps.
        issues.sort_by_key(|detail| std::cmp::Reverse(detail.issue.number));
        Ok(issues)
    }

    /// Issue `number` of the repository `dir` is a clone of. `NotFound` when
    /// there is no such issue, or the number is a pull request's.
    pub fn issue(&self, dir: &Path, number: u32) -> Result<IssueDetail> {
        let dir = on_github(dir)?;
        let (reference, json) = (number.to_string(), format!("--json={ISSUE_FIELDS}"));
        let ran = self
            .gh(
                &dir,
                "issue view",
                &["issue", "view", &reference, &json],
                None,
            )
            .map_err(|err| not_found(err, "issue", number))?;
        let raw: RawIssue = parse("issue view", &ran.stdout)?;
        let detail = raw.into_detail("issue view")?;
        if detail.issue.number != number || detail.issue.url.contains("/pull/") {
            return Err(Error::NotFound(format!("issue #{number}")));
        }
        Ok(detail)
    }

    /// The open pull requests of the repository `dir` is a clone of, forks'
    /// included, newest first.
    pub fn list_open_pull_requests(&self, dir: &Path, limit: u32) -> Result<Vec<PullRequestItem>> {
        let dir = on_github(dir)?;
        let limit = format!("--limit={}", limit.max(1));
        let json = format!("--json={PULL_REQUEST_FIELDS}");
        let args = ["pr", "list", "--state=open", &limit, &json];
        let ran = self.gh(&dir, "pr list", &args, None)?;
        let listed: Vec<RawPullRequestItem> = parse("pr list", &ran.stdout)?;
        let mut items = listed
            .into_iter()
            .map(|raw| raw.into_item("pr list"))
            .collect::<Result<Vec<_>>>()?;
        items.sort_by_key(|item| std::cmp::Reverse(item.number));
        Ok(items)
    }

    /// Pull request `number`, as an automation sees it. `NotFound` when
    /// there is none.
    pub fn pull_request_item(&self, dir: &Path, number: u32) -> Result<PullRequestItem> {
        let dir = on_github(dir)?;
        let (reference, json) = (number.to_string(), format!("--json={PULL_REQUEST_FIELDS}"));
        let ran = self
            .gh(&dir, "pr view", &["pr", "view", &reference, &json], None)
            .map_err(|err| not_found(err, "pull request", number))?;
        let raw: RawPullRequestItem = parse("pr view", &ran.stdout)?;
        let item = raw.into_item("pr view")?;
        if item.number != number {
            return Err(Error::NotFound(format!("pull request #{number}")));
        }
        Ok(item)
    }
}

/// `dir`, canonical, when its `origin` is on GitHub. `Conflict` with the
/// reason otherwise: nothing else can answer for its issues.
pub fn on_github(dir: &Path) -> Result<std::path::PathBuf> {
    let dir = work_dir(dir)?;
    let url = Git::read(&dir)
        .args(["config", "--get", &format!("remote.{REMOTE}.url")])
        .probe()?;
    match url {
        None => Err(Error::Conflict(format!(
            "the repository has no `{REMOTE}` remote, so it has no GitHub issues"
        ))),
        Some(url) if is_github_url(&url) => Ok(dir),
        Some(_) => Err(Error::Conflict(format!(
            "the repository's `{REMOTE}` remote is not on GitHub"
        ))),
    }
}

/// True for a remote URL on github.com or on a GitHub Enterprise host
/// (`github.<company>` or `<name>.ghe.com`), in any of git's URL forms.
pub(crate) fn is_github_url(url: &str) -> bool {
    let url = url.trim();
    let rest = match url.split_once("://") {
        Some((scheme, rest)) => {
            if !matches!(scheme, "https" | "http" | "ssh" | "git" | "git+ssh") {
                return false;
            }
            rest
        }
        // The scp-like form, `user@host:owner/repo`; a local path has no colon before a slash.
        None => match url.split_once(':') {
            Some((host, _)) if !host.contains('/') => host,
            _ => return false,
        },
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or_default();
    let host = host
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    host == "github.com"
        || host == "www.github.com"
        || host.starts_with("github.")
            && host.len() > "github.".len()
            && !host.starts_with("github.com.")
        || host.ends_with(".ghe.com")
}

/// A failure of `gh` that says the number names nothing becomes `NotFound`.
fn not_found(err: Error, what: &str, number: u32) -> Error {
    match &err {
        Error::Git { stderr, .. } if stderr.contains(NO_SUCH_NUMBER) => {
            Error::NotFound(format!("{what} #{number}"))
        }
        _ => err,
    }
}

/// An issue in `gh`'s JSON. Every field asked for must be there; fields
/// `gh` adds are ignored. `author` is null for a deleted account.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIssue {
    number: u32,
    title: String,
    state: RawIssueState,
    labels: Vec<RawLabel>,
    author: Option<RawAuthor>,
    updated_at: String,
    url: String,
    body: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum RawIssueState {
    Open,
    Closed,
}

#[derive(Deserialize)]
struct RawLabel {
    name: String,
}

#[derive(Deserialize)]
struct RawAuthor {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequestItem {
    number: u32,
    title: String,
    body: Option<String>,
    url: String,
    author: Option<RawAuthor>,
    updated_at: String,
}

impl RawIssue {
    fn into_detail(self, what: &str) -> Result<IssueDetail> {
        Ok(IssueDetail {
            issue: Issue {
                number: self.number,
                title: self.title,
                state: match self.state {
                    RawIssueState::Open => IssueState::Open,
                    RawIssueState::Closed => IssueState::Closed,
                },
                labels: self.labels.into_iter().map(|label| label.name).collect(),
                author: self.author.map(|author| author.login),
                updated_at: timestamp(what, &self.updated_at)?,
                url: self.url,
            },
            body: self.body.unwrap_or_default(),
        })
    }
}

impl RawPullRequestItem {
    fn into_item(self, what: &str) -> Result<PullRequestItem> {
        Ok(PullRequestItem {
            number: self.number,
            title: self.title,
            body: self.body.unwrap_or_default(),
            author: self.author.map(|author| author.login),
            updated_at: timestamp(what, &self.updated_at)?,
            url: self.url,
        })
    }
}

/// GitHub's RFC 3339 time, in Unix milliseconds.
fn timestamp(what: &str, text: &str) -> Result<i64> {
    text.parse::<jiff::Timestamp>()
        .map(jiff::Timestamp::as_millisecond)
        .map_err(|err| unexpected(what, format!("its output is not what was asked for: {err}")))
}

#[cfg(test)]
mod tests;
