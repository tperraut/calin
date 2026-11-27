//! A repo's GitHub issues and open pull requests (integrations spec,
//! section 2), through `gh`. In dry run they come from a small set of
//! samples instead, so the flow works without GitHub.

use std::path::{Path, PathBuf};

use calin_git::{IssueQuery, PullRequestItem};
use calin_proto::{Issue, IssueDetail, IssueState, Repo};

use super::SessionManager;
use crate::error::{Error, Result};
use crate::repos::blocking;

/// The most issues a list returns.
pub(crate) const MAX_ISSUES: u32 = 50;
/// How many open items a trigger looks at per poll; older ones are not new.
pub(crate) const TRIGGER_WINDOW: u32 = 100;

/// Where the made-up items of a dry run pretend to be. `.invalid` never resolves.
const SAMPLE_ROOT: &str = "https://example.invalid/";
const SAMPLE_AUTHOR: &str = "sample-user";
/// 2026-01-15T09:00:00Z, and a day apart from there: fixed, so samples never change.
const SAMPLE_TIME: i64 = 1_768_467_600_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

impl SessionManager {
    /// The repo's issues, newest first, at most 50: `state` ones whose text
    /// matches `q` when it is given. `Missing` for an unknown repo;
    /// `Conflict` with the reason when the repo is not on GitHub or `gh`
    /// cannot be used. In dry run: the samples.
    pub async fn list_issues(
        &self,
        repo_id: &str,
        state: IssueState,
        q: Option<String>,
    ) -> Result<Vec<Issue>> {
        let q = q.filter(|q| !q.trim().is_empty());
        let query = IssueQuery {
            state,
            search: q,
            label: None,
            limit: MAX_ISSUES,
        };
        let found = self.find_issues(repo_id, query).await?;
        Ok(found.into_iter().map(|detail| detail.issue).collect())
    }

    /// Issue `number` of the repo, with its body. `Missing` for an unknown
    /// repo or issue; `Conflict` as for [`Self::list_issues`].
    pub async fn issue(&self, repo_id: &str, number: u32) -> Result<IssueDetail> {
        let repo = self.issue_repo(repo_id).await?;
        if self.inner.registry.dry_run() {
            return sample_issues(&repo.name)
                .into_iter()
                .find(|detail| detail.issue.number == number)
                .ok_or_else(|| Error::Missing(format!("{} has no issue #{number}", repo.name)));
        }
        let dir = self.github_dir(&repo).await?;
        let github = self.inner.github.clone();
        let found = blocking(move || github.issue(&dir, number).map_err(from_github)).await;
        found.map_err(|err| named(err, &repo))
    }

    /// The open issues of the repo, with `label` when it is given, for a trigger.
    pub(crate) async fn open_issues(
        &self,
        repo_id: &str,
        label: Option<String>,
    ) -> Result<Vec<IssueDetail>> {
        let query = IssueQuery {
            state: IssueState::Open,
            search: None,
            label,
            limit: TRIGGER_WINDOW,
        };
        self.find_issues(repo_id, query).await
    }

    /// The repo's open pull requests, newest first, for a trigger.
    pub(crate) async fn open_pull_requests(&self, repo_id: &str) -> Result<Vec<PullRequestItem>> {
        let repo = self.issue_repo(repo_id).await?;
        if self.inner.registry.dry_run() {
            return Ok(sample_pull_requests(&repo.name));
        }
        let dir = self.github_dir(&repo).await?;
        let github = self.inner.github.clone();
        blocking(move || {
            github
                .list_open_pull_requests(&dir, TRIGGER_WINDOW)
                .map_err(from_github)
        })
        .await
    }

    /// Pull request `number` of the repo, for a manual run of a trigger.
    pub(crate) async fn pull_request_item(
        &self,
        repo_id: &str,
        number: u32,
    ) -> Result<PullRequestItem> {
        let repo = self.issue_repo(repo_id).await?;
        if self.inner.registry.dry_run() {
            return sample_pull_requests(&repo.name)
                .into_iter()
                .find(|item| item.number == number)
                .ok_or_else(|| {
                    Error::Missing(format!("{} has no pull request #{number}", repo.name))
                });
        }
        let dir = self.github_dir(&repo).await?;
        let github = self.inner.github.clone();
        let found =
            blocking(move || github.pull_request_item(&dir, number).map_err(from_github)).await;
        found.map_err(|err| named(err, &repo))
    }

    async fn find_issues(&self, repo_id: &str, query: IssueQuery) -> Result<Vec<IssueDetail>> {
        let repo = self.issue_repo(repo_id).await?;
        if self.inner.registry.dry_run() {
            return Ok(filtered_samples(&repo.name, &query));
        }
        let dir = self.github_dir(&repo).await?;
        let github = self.inner.github.clone();
        let found = blocking(move || github.list_issues(&dir, &query).map_err(from_github)).await;
        found.map_err(|err| named(err, &repo))
    }

    /// A registered repo with a clone on this machine.
    async fn issue_repo(&self, repo_id: &str) -> Result<Repo> {
        let repo = self
            .inner
            .repos
            .get(repo_id)
            .await
            .ok_or_else(|| Error::Missing(format!("unknown repo {repo_id}")))?;
        if !repo.available || repo.path.is_empty() {
            return Err(Error::Conflict(format!(
                "{} has no clone on this machine to ask GitHub about",
                repo.name
            )));
        }
        Ok(repo)
    }

    /// The clone to run `gh` in, once `gh` is known to work.
    async fn github_dir(&self, repo: &Repo) -> Result<PathBuf> {
        if let Some(problem) = self.inner.github_problem(true).await {
            return Err(Error::Conflict(format!(
                "GitHub cannot be asked about {}: {problem}",
                repo.name
            )));
        }
        Ok(Path::new(&repo.path).to_owned())
    }
}

/// What `gh` said, in the terms of the routes: a number that names nothing
/// is missing, and anything else that keeps GitHub from answering is a
/// state the user can fix, never an internal error.
fn from_github(err: calin_git::Error) -> Error {
    use calin_git::Error as Git;
    match err {
        Git::NotFound(what)
            if what.starts_with("issue #") || what.starts_with("pull request #") =>
        {
            Error::Missing(format!("there is no {what}"))
        }
        Git::Conflict(reason) => Error::Conflict(reason),
        other => Error::Conflict(format!("GitHub could not be asked: {other}")),
    }
}

/// Says which repo an error is about.
fn named(err: Error, repo: &Repo) -> Error {
    match err {
        Error::Missing(what) => Error::Missing(format!("{} on {}", what, repo.name)),
        Error::Conflict(reason) => Error::Conflict(format!("{}: {reason}", repo.name)),
        other => other,
    }
}

/// The made-up issues of a repo in dry run. Their titles say what they are.
pub(crate) fn sample_issues(repo: &str) -> Vec<IssueDetail> {
    let issue =
        |number: u32, title: &str, state: IssueState, labels: &[&str], body: &str| IssueDetail {
            issue: Issue {
                number,
                title: format!("Sample issue: {title}"),
                state,
                labels: labels.iter().map(|label| (*label).to_owned()).collect(),
                author: Some(SAMPLE_AUTHOR.to_owned()),
                updated_at: SAMPLE_TIME + i64::from(number) * DAY_MS,
                url: format!("{SAMPLE_ROOT}{repo}/issues/{number}"),
            },
            body: format!("{body}\n\n(A made-up issue: Calin runs in dry run.)"),
        };
    vec![
        issue(
            4,
            "the login button does nothing on small screens",
            IssueState::Open,
            &["bug", "agent"],
            "Tapping **Log in** at 360 px wide does nothing.",
        ),
        issue(
            3,
            "add a dark theme",
            IssueState::Open,
            &["enhancement"],
            "Follow the system setting.",
        ),
        issue(
            2,
            "document the release steps",
            IssueState::Open,
            &["docs", "agent"],
            "Write down how a release is cut.",
        ),
        issue(
            1,
            "fix the typo in the README",
            IssueState::Closed,
            &["docs"],
            "\"teh\" should read \"the\".",
        ),
    ]
}

/// The samples a query would find: its state, its label, its words in the
/// title or the body (any case), newest first.
fn filtered_samples(repo: &str, query: &IssueQuery) -> Vec<IssueDetail> {
    let words = query.search.as_deref().unwrap_or_default().to_lowercase();
    sample_issues(repo)
        .into_iter()
        .filter(|detail| detail.issue.state == query.state)
        .filter(|detail| {
            query
                .label
                .as_ref()
                .is_none_or(|label| detail.issue.labels.contains(label))
        })
        .filter(|detail| {
            let text = format!("{}\n{}", detail.issue.title, detail.body).to_lowercase();
            words.split_whitespace().all(|word| text.contains(word))
        })
        .take(query.limit as usize)
        .collect()
}

/// The made-up open pull requests of a repo in dry run.
pub(crate) fn sample_pull_requests(repo: &str) -> Vec<PullRequestItem> {
    vec![PullRequestItem {
        number: 5,
        title: "Sample pull request: bump the dependencies".to_owned(),
        body: "(A made-up pull request: Calin runs in dry run.)".to_owned(),
        url: format!("{SAMPLE_ROOT}{repo}/pull/5"),
        author: Some(SAMPLE_AUTHOR.to_owned()),
        updated_at: SAMPLE_TIME + 5 * DAY_MS,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query(state: IssueState, search: Option<&str>, label: Option<&str>) -> IssueQuery {
        IssueQuery {
            state,
            search: search.map(str::to_owned),
            label: label.map(str::to_owned),
            limit: MAX_ISSUES,
        }
    }

    fn numbers(found: &[IssueDetail]) -> Vec<u32> {
        found.iter().map(|detail| detail.issue.number).collect()
    }

    #[test]
    fn samples_are_filtered_like_github_would() {
        let open = filtered_samples("api", &query(IssueState::Open, None, None));
        assert_eq!(numbers(&open), [4, 3, 2]);
        let closed = filtered_samples("api", &query(IssueState::Closed, None, None));
        assert_eq!(numbers(&closed), [1]);
        let dark = filtered_samples("api", &query(IssueState::Open, Some("DARK theme"), None));
        assert_eq!(numbers(&dark), [3]);
        let agent = filtered_samples("api", &query(IssueState::Open, None, Some("agent")));
        assert_eq!(numbers(&agent), [4, 2]);
        let none = filtered_samples(
            "api",
            &query(IssueState::Open, Some("nothing like it"), None),
        );
        assert_eq!(numbers(&none), Vec::<u32>::new());
        for detail in sample_issues("api") {
            assert!(detail.issue.title.starts_with("Sample issue: "));
            assert!(
                detail
                    .issue
                    .url
                    .starts_with("https://example.invalid/api/issues/")
            );
        }
    }

    #[test]
    fn what_gh_says_becomes_a_missing_item_or_a_conflict() {
        let missing = from_github(calin_git::Error::NotFound("issue #9".into()));
        assert!(matches!(missing, Error::Missing(_)), "{missing:?}");
        // A directory that is gone is not a missing issue.
        let gone = from_github(calin_git::Error::NotFound("/x".into()));
        assert!(matches!(gone, Error::Conflict(_)), "{gone:?}");
        let failed = from_github(calin_git::Error::Git {
            command: "gh issue list".into(),
            stderr: "HTTP 502".into(),
        });
        assert!(
            matches!(&failed, Error::Conflict(reason) if reason.contains("HTTP 502")),
            "{failed:?}"
        );
    }
}
