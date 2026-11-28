//! Delivery: what follows the review (design spec, section 3, "Delivery").
//! The work here runs in the background and only reports; the session's
//! driver is the one that writes what happened to the log.
//!
//! Nothing here merges, forces a push or deletes a branch.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use calin_git::{CommitOutcome, GitHub, PullRequestInfo};
use calin_proto::{
    CheckState, IssueRef, PullRequest, PullRequestState, RepoMode, ReviewState, SessionRepo,
};
use tokio::sync::mpsc;

use crate::store::Store;

/// The remote a session's branches are pushed to and pull requests opened on.
const REMOTE: &str = "origin";

/// Where the pull requests of a dry run pretend to be. `.invalid` never
/// resolves, so a made-up link cannot lead anywhere.
const MADE_UP_ROOT: &str = "https://example.invalid/";

/// The part of a description Calin owns starts with a comment that begins
/// like this and ends with [`BLOCK_END`]. Comments do not show on GitHub.
const BLOCK_BEGIN_PREFIX: &str = "<!-- calin:begin";
const BLOCK_BEGIN: &str = "<!-- calin:begin (Calin rewrites this block each time the session is \
     shipped; write outside it) -->";
/// Inside the block: the user's text is above this line, the links below.
const BLOCK_RELATED: &str = "<!-- calin:related -->";
/// Inside the block, between the user's text and the links: the issue the
/// session started from.
const BLOCK_ISSUE: &str = "<!-- calin:issue -->";
const BLOCK_END: &str = "<!-- calin:end -->";
const COMMENT_END: &str = "-->";
const RELATED_HEADING: &str = "### Related pull requests";

/// What one ship request asks for, and everything needed to carry it out.
pub(super) struct ShipPlan {
    pub session_id: String,
    /// Of each pull request that is opened and of the commit Calin makes.
    pub title: String,
    /// For the top of every description. `None` keeps what a previous ship put there.
    pub body: Option<String>,
    pub draft: bool,
    /// Commit, but never push and never call GitHub.
    pub dry_run: bool,
    pub github: GitHub,
    pub store: Store,
    /// The session's repo set in order, each repo with its pull request on record.
    pub repos: Vec<(SessionRepo, Option<PullRequest>)>,
    /// The issue the session started from: its repo's pull request closes it.
    pub issue: Option<IssueRef>,
    /// How long a commit and a push may each take before they are killed.
    pub commit_timeout: Duration,
    pub push_timeout: Duration,
}

/// A step of a ship, for the driver to record.
pub(super) enum Shipped {
    /// A repo has a pull request it did not have on record.
    Opened(PullRequest),
    /// The pull request on record changed: new commits, its state or its description.
    Updated(PullRequest),
    /// The pull request on record is as it was.
    Kept,
    /// Something went wrong that the user should read; the ship went on.
    Failed(String),
}

/// A step of one repo's delivery that failed.
struct Failure {
    step: &'static str,
    error: String,
}

impl Failure {
    fn new(step: &'static str, error: impl std::fmt::Display) -> Self {
        Self {
            step,
            error: error.to_string(),
        }
    }
}

/// A repo whose branch is delivered.
struct Pushed {
    /// The pull request on GitHub. `None` in dry run.
    info: Option<PullRequestInfo>,
    /// The branch carried commits its pull request did not have yet.
    new_commits: bool,
}

/// A pull request this ship opened or found.
struct Delivered {
    /// Position of its repo in the plan.
    index: usize,
    repo: SessionRepo,
    pull_request: PullRequest,
    /// Its description on GitHub. `None` in dry run.
    body: Option<String>,
    /// Not on record before this ship.
    new: bool,
    /// On record, and different now.
    changed: bool,
}

/// Goes repo by repo over the `write` repos that differ from their base:
/// commit, push, open or reuse the pull request. Then makes every
/// description link the others. A repo that fails is reported and the rest
/// carry on. Ends early, between two steps, once `cancel` is set.
pub(super) async fn ship(plan: ShipPlan, results: mpsc::Sender<Shipped>, cancel: Arc<AtomicBool>) {
    let ship = Ship {
        plan: Arc::new(plan),
        results,
        cancel,
    };
    // A ship that stopped early has nothing left to say.
    let _ = ship.run().await;
}

/// Why a ship ends before its last step: it was cancelled, or nobody reads
/// its results any more.
struct Stop;

/// One ship under way.
struct Ship {
    plan: Arc<ShipPlan>,
    results: mpsc::Sender<Shipped>,
    cancel: Arc<AtomicBool>,
}

impl Ship {
    async fn run(&self) -> Result<(), Stop> {
        let (delivered, failed) = self.deliver_all().await?;
        if !delivered.is_empty() {
            return self.settle(&delivered).await;
        }
        if failed {
            return Ok(());
        }
        let nothing = "nothing to ship: no repository attached for writing differs from its base";
        self.send(Shipped::Failed(nothing.to_owned())).await
    }

    /// Links the delivered pull requests to each other and says what came
    /// of each. Every repo was attempted: only now are all the siblings known.
    async fn settle(&self, delivered: &[Delivered]) -> Result<(), Stop> {
        let any_new = delivered.iter().any(|d| d.new);
        for d in delivered {
            self.go_on()?;
            let described = self.link(delivered, d, any_new).await?;
            if let Some(step) = last_step(d, described) {
                self.send(step).await?;
            }
        }
        Ok(())
    }

    /// Delivers each `write` repo in turn and announces each pull request
    /// it opens. Also says whether a repo failed.
    async fn deliver_all(&self) -> Result<(Vec<Delivered>, bool), Stop> {
        let mut delivered = Vec::new();
        let mut failed = false;
        for (index, (repo, known)) in self.plan.repos.iter().enumerate() {
            self.go_on()?;
            // `live` and `read` repos are the user's own checkouts: never
            // committed to, pushed or shipped.
            if repo.mode != RepoMode::Write {
                continue;
            }
            match self.deliver_one(index, repo, known.as_ref()).await {
                Ok(d) => delivered.extend(self.announce(d).await?),
                Err(failure) => {
                    failed = true;
                    self.report(&repo.name, failure).await?;
                }
            }
        }
        Ok((delivered, failed))
    }

    /// Announces a pull request this ship opened.
    async fn announce(&self, d: Option<Delivered>) -> Result<Option<Delivered>, Stop> {
        if let Some(opened) = d.as_ref().filter(|d| d.new) {
            self.send(Shipped::Opened(opened.pull_request.clone()))
                .await?;
        }
        Ok(d)
    }

    /// Commits and pushes one repo, off the runtime, and finds its pull
    /// request. `None` when the repo has nothing to deliver.
    async fn deliver_one(
        &self,
        index: usize,
        repo: &SessionRepo,
        known: Option<&PullRequest>,
    ) -> Result<Option<Delivered>, Failure> {
        let work = {
            let (plan, repo) = (Arc::clone(&self.plan), repo.clone());
            tokio::task::spawn_blocking(move || deliver(&plan, &repo))
        };
        let pushed = work
            .await
            .map_err(|panicked| Failure::new("delivery", panicked))??;
        let Some(pushed) = pushed else {
            return Ok(None);
        };
        let (pull_request, body) = match pushed.info {
            Some(info) => {
                let body = info.body.clone();
                (on_github(&repo.id, info), Some(body))
            }
            None => {
                let made = made_up(&self.plan, repo, known).await;
                let pull_request = made.map_err(|error| Failure::new("pull request", error))?;
                (pull_request, None)
            }
        };
        let on_record = known
            .filter(|known| known.number == pull_request.number && known.url == pull_request.url);
        let changed =
            on_record.is_some_and(|known| pushed.new_commits || !same(known, &pull_request));
        Ok(Some(Delivered {
            index,
            repo: repo.clone(),
            new: on_record.is_none(),
            pull_request,
            body,
            changed,
        }))
    }

    /// Makes the description of `d` link its siblings. True when it changed
    /// on GitHub, or, in dry run, when a real ship would have changed it.
    async fn link(
        &self,
        delivered: &[Delivered],
        d: &Delivered,
        any_new: bool,
    ) -> Result<bool, Stop> {
        let related = related(&self.plan, delivered, d.index);
        let Some(current) = &d.body else {
            // Nothing is written anywhere in dry run; the events are the
            // ones a real ship gives when a sibling appears.
            return Ok(any_new && !related.is_empty());
        };
        let issue = issue_line(self.plan.issue.as_ref(), &d.repo);
        let wanted = describe(
            current,
            self.plan.body.as_deref(),
            issue.as_deref(),
            &related,
        );
        if same_text(&wanted, current) {
            return Ok(false);
        }
        let number = d.pull_request.number;
        let Err(failure) = self.edit_body(&d.repo, number, wanted).await else {
            return Ok(true);
        };
        self.report(&d.repo.name, failure).await?;
        Ok(false)
    }

    /// Rewrites a pull request's description on GitHub, off the runtime.
    async fn edit_body(
        &self,
        repo: &SessionRepo,
        number: u32,
        body: String,
    ) -> Result<(), Failure> {
        let (github, dir) = (self.plan.github.clone(), repo.path.clone());
        let edited =
            tokio::task::spawn_blocking(move || github.edit_body(Path::new(&dir), number, &body))
                .await
                .map_err(|panicked| Failure::new("description", panicked))?;
        edited.map_err(|error| Failure::new("description", error))
    }

    fn go_on(&self) -> Result<(), Stop> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Stop);
        }
        Ok(())
    }

    async fn send(&self, step: Shipped) -> Result<(), Stop> {
        self.results.send(step).await.map_err(|_| Stop)
    }

    async fn report(&self, repo: &str, failure: Failure) -> Result<(), Stop> {
        let message = format!(
            "could not ship {repo}: {} failed: {}",
            failure.step, failure.error
        );
        self.send(Shipped::Failed(message)).await
    }
}

/// What a ship says last about a pull request it delivered, once its
/// description is settled. `None` for one it announced as opened that
/// nothing happened to since.
fn last_step(d: &Delivered, described: bool) -> Option<Shipped> {
    if described || d.changed {
        return Some(Shipped::Updated(PullRequest {
            updated_at: now_ms(),
            ..d.pull_request.clone()
        }));
    }
    (!d.new).then_some(Shipped::Kept)
}

/// Blocking. Commits what is uncommitted, pushes the session's branch and
/// finds or opens its pull request. `None` when the repo has nothing to
/// deliver. In dry run it stops after the commit.
fn deliver(plan: &ShipPlan, repo: &SessionRepo) -> Result<Option<Pushed>, Failure> {
    // A `write` repo always has both; without them there is nothing to measure.
    let (Some(branch), Some(base)) = (&repo.branch, &repo.base_ref) else {
        return Ok(None);
    };
    let dir = Path::new(&repo.path);
    if !differs(dir, base, branch)? {
        return Ok(None);
    }
    let committed = commit(plan, dir, branch)?;
    // What differed may have been nothing a commit records.
    if calin_git::commits_ahead(dir, base, branch).map_err(at("changes"))? == 0 {
        return Ok(None);
    }
    if plan.dry_run {
        return Ok(Some(Pushed {
            info: None,
            new_commits: committed,
        }));
    }
    push(plan, repo, dir, branch).map(Some)
}

/// The failure of `step`, for an error of the library.
fn at(step: &'static str) -> impl Fn(calin_git::Error) -> Failure {
    move |error| Failure::new(step, error)
}

/// Changed files, or commits whose changes cancel out: either way the
/// branch is not its base any more.
fn differs(dir: &Path, base: &str, branch: &str) -> Result<bool, Failure> {
    let changed = !calin_git::changes(dir, base)
        .map_err(at("changes"))?
        .is_empty();
    Ok(changed || calin_git::commits_ahead(dir, base, branch).map_err(at("changes"))? > 0)
}

/// Commits what is uncommitted. True when that made a commit.
fn commit(plan: &ShipPlan, dir: &Path, branch: &str) -> Result<bool, Failure> {
    if !calin_git::is_dirty(dir).map_err(at("commit"))? {
        return Ok(false);
    }
    // The commit lands on whatever is checked out, and what is pushed
    // is the session's branch: they have to be the same.
    let current = calin_git::current_branch(dir).map_err(at("commit"))?;
    if current.as_deref() != Some(branch) {
        let on = current.as_deref().unwrap_or("a detached HEAD");
        return Err(Failure::new(
            "commit",
            format!("the worktree is on {on}, not on the session's branch {branch}"),
        ));
    }
    let message = commit_message(&plan.title, &plan.session_id);
    let outcome =
        calin_git::commit_all(dir, &message, plan.commit_timeout).map_err(at("commit"))?;
    Ok(matches!(outcome, CommitOutcome::Committed(_)))
}

/// Pushes the session's branch and finds or opens its pull request.
fn push(plan: &ShipPlan, repo: &SessionRepo, dir: &Path, branch: &str) -> Result<Pushed, Failure> {
    if !calin_git::has_remote(dir, REMOTE).map_err(at("push"))? {
        return Err(Failure::new(
            "push",
            format!("the repository has no `{REMOTE}` remote to push to"),
        ));
    }
    let Some(default_branch) = calin_git::default_branch(dir, REMOTE).map_err(at("push"))? else {
        return Err(Failure::new(
            "push",
            "the repository's default branch is not known",
        ));
    };
    let new_commits = calin_git::has_unpushed_commits(dir, REMOTE, branch).map_err(at("push"))?;
    calin_git::push_branch(dir, REMOTE, branch, plan.push_timeout).map_err(at("push"))?;
    let info = pull_request(plan, repo, dir, branch, &default_branch)?;
    Ok(Pushed {
        info: Some(info),
        new_commits,
    })
}

/// The open pull request of the branch, or a new one.
fn pull_request(
    plan: &ShipPlan,
    repo: &SessionRepo,
    dir: &Path,
    branch: &str,
    default_branch: &str,
) -> Result<PullRequestInfo, Failure> {
    let open = plan
        .github
        .find_open_pull_request(dir, branch)
        .map_err(at("pull request"))?;
    if let Some(info) = open {
        return Ok(info);
    }
    // The links come later, once every repo has its pull request.
    let issue = issue_line(plan.issue.as_ref(), repo);
    let body = describe("", plan.body.as_deref(), issue.as_deref(), &[]);
    plan.github
        .create_pull_request(dir, default_branch, branch, &plan.title, &body, plan.draft)
        .map_err(at("pull request"))
}

/// The title, then a trailer that leads from the commit back to its session.
fn commit_message(title: &str, session_id: &str) -> String {
    format!("{title}\n\nCalin-Session: {session_id}\n")
}

fn on_github(session_repo_id: &str, info: PullRequestInfo) -> PullRequest {
    PullRequest {
        session_repo_id: session_repo_id.to_owned(),
        number: info.number,
        url: info.url,
        title: info.title,
        state: info.state,
        checks: info.checks,
        review: info.review,
        updated_at: now_ms(),
    }
}

/// The pull request a dry run records: the one already on record, else the
/// next number of its repo, counted across sessions.
async fn made_up(
    plan: &ShipPlan,
    repo: &SessionRepo,
    known: Option<&PullRequest>,
) -> crate::Result<PullRequest> {
    if let Some(known) = known.filter(|known| known.url.starts_with(MADE_UP_ROOT)) {
        return Ok(known.clone());
    }
    let last = plan
        .store
        .last_pull_request_number(&repo.repo_id, MADE_UP_ROOT)
        .await?;
    let number = last + 1;
    Ok(PullRequest {
        session_repo_id: repo.id.clone(),
        number,
        url: format!("{MADE_UP_ROOT}{}/pull/{number}", repo.name),
        title: plan.title.clone(),
        state: PullRequestState::Draft,
        checks: CheckState::None,
        review: ReviewState::None,
        updated_at: now_ms(),
    })
}

/// The session's other pull requests, by repo name, in repo-set order: the
/// ones this ship delivered, and the ones on record for the repos it did not.
fn related(plan: &ShipPlan, delivered: &[Delivered], index: usize) -> Vec<(String, String)> {
    let mut related = Vec::new();
    for (at, (repo, known)) in plan.repos.iter().enumerate() {
        if at == index {
            continue;
        }
        let now = delivered.iter().find(|d| d.index == at);
        let url = match (now, known) {
            (Some(d), _) => &d.pull_request.url,
            (None, Some(known)) => &known.url,
            (None, None) => continue,
        };
        related.push((repo.name.clone(), url.clone()));
    }
    related
}

/// What the pull request of `repo` says about the session's issue: the
/// issue's own repo closes it, the others link to it (integrations spec,
/// section 2).
fn issue_line(issue: Option<&IssueRef>, repo: &SessionRepo) -> Option<String> {
    let issue = issue?;
    Some(if issue.repo_id == repo.repo_id {
        format!("Closes #{}", issue.number)
    } else {
        format!("Issue: {}", issue.url)
    })
}

/// True when two values describe the same pull request in the same state,
/// whenever each was observed.
pub(super) fn same(a: &PullRequest, b: &PullRequest) -> bool {
    let timeless = |pull_request: &PullRequest| PullRequest {
        updated_at: 0,
        ..pull_request.clone()
    };
    timeless(a) == timeless(b)
}

pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as i64)
}

/// Looks at each pull request on GitHub and returns the ones that answered,
/// as they are now. One that cannot be read is logged and left for the next
/// look: a failed look says nothing about the pull request.
pub(super) async fn poll(
    session_id: String,
    github: GitHub,
    targets: Vec<(SessionRepo, PullRequest)>,
) -> Vec<PullRequest> {
    let mut observed = Vec::with_capacity(targets.len());
    for (repo, known) in targets {
        let (github, dir, number) = (github.clone(), repo.path.clone(), known.number);
        let viewed =
            tokio::task::spawn_blocking(move || github.view(Path::new(&dir), number)).await;
        match viewed {
            Ok(Ok(info)) => observed.push(on_github(&repo.id, info)),
            Ok(Err(err)) => {
                tracing::warn!(session = %session_id, repo = %repo.name, number, %err, "could not read the pull request; trying again later");
            }
            Err(err) => {
                tracing::warn!(session = %session_id, repo = %repo.name, number, %err, "could not read the pull request; trying again later");
            }
        }
    }
    observed
}

/// The block of a description that Calin owns, and the text around it.
struct Block<'a> {
    before: &'a str,
    inside: &'a str,
    after: &'a str,
}

fn find_block(text: &str) -> Option<Block<'_>> {
    let start = text.find(BLOCK_BEGIN_PREFIX)?;
    let opened = start + text[start..].find(COMMENT_END)? + COMMENT_END.len();
    let end = opened + text[opened..].find(BLOCK_END)?;
    Some(Block {
        before: &text[..start],
        inside: &text[opened..end],
        after: &text[end + BLOCK_END.len()..],
    })
}

/// The description a pull request should have, given the one it has.
///
/// Calin's block holds `body`, then the line about the session's issue,
/// then the links to `related` (repo name and URL). It replaces the block
/// that is already there and leaves every character outside it alone; a
/// description without one gets it on top. `None` for `body` keeps the text
/// an earlier ship put in the block.
pub(super) fn describe(
    current: &str,
    body: Option<&str>,
    issue: Option<&str>,
    related: &[(String, String)],
) -> String {
    let found = find_block(current);
    let block = block(&block_text(body, found.as_ref()), issue, related);
    match found {
        Some(found) => format!("{}{block}{}", found.before, found.after),
        None if current.trim().is_empty() => block,
        None => format!("{block}\n\n{current}"),
    }
}

/// The user's text of Calin's block: `body` when given, else what the
/// block on the pull request holds.
fn block_text(body: Option<&str>, found: Option<&Block<'_>>) -> String {
    match (body, found) {
        // The markers are Calin's: text that brings its own would cut the block short.
        (Some(body), _) => body.trim().replace("<!-- calin:", "<!-- calin "),
        (None, Some(block)) => {
            // The user's text ends where Calin's own lines begin.
            let end = [BLOCK_ISSUE, BLOCK_RELATED]
                .iter()
                .filter_map(|marker| block.inside.find(marker))
                .min()
                .unwrap_or(block.inside.len());
            block.inside[..end].trim().to_owned()
        }
        (None, None) => String::new(),
    }
}

/// Calin's block: the text, the issue line and the links, each that is
/// there, a blank line apart.
fn block(text: &str, issue: Option<&str>, related: &[(String, String)]) -> String {
    let mut parts = Vec::new();
    if !text.is_empty() {
        parts.push(format!("{text}\n"));
    }
    if let Some(issue) = issue {
        parts.push(format!("{BLOCK_ISSUE}\n{issue}\n"));
    }
    if !related.is_empty() {
        let mut links = format!("{BLOCK_RELATED}\n{RELATED_HEADING}\n\n");
        for (name, url) in related {
            links.push_str(&format!("- {name}: {url}\n"));
        }
        parts.push(links);
    }
    format!("{BLOCK_BEGIN}\n{}{BLOCK_END}", parts.join("\n"))
}

/// True when two descriptions read the same. GitHub stores what its web
/// editor saves with other line ends, and trims what it is given.
fn same_text(a: &str, b: &str) -> bool {
    let plain = |text: &str| text.replace("\r\n", "\n").trim().to_owned();
    plain(a) == plain(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(name, url)| ((*name).to_owned(), (*url).to_owned()))
            .collect()
    }

    #[test]
    fn a_new_description_is_the_block_with_the_body_and_the_links() {
        let related = links(&[
            ("web", "https://github.com/acme/web/pull/3"),
            ("docs", "https://github.com/acme/docs/pull/9"),
        ]);
        let text = describe("", Some("What it does.\n\nAnd why.\n"), None, &related);
        assert_eq!(
            text,
            format!(
                "{BLOCK_BEGIN}\nWhat it does.\n\nAnd why.\n\n{BLOCK_RELATED}\n\
                 ### Related pull requests\n\n\
                 - web: https://github.com/acme/web/pull/3\n\
                 - docs: https://github.com/acme/docs/pull/9\n{BLOCK_END}"
            )
        );
        // Alone, or with nothing to say, the block has only what there is.
        assert_eq!(
            describe("", Some("Body."), None, &[]),
            format!("{BLOCK_BEGIN}\nBody.\n{BLOCK_END}")
        );
        assert_eq!(
            describe("  \n", None, None, &links(&[("web", "u")])),
            format!(
                "{BLOCK_BEGIN}\n{BLOCK_RELATED}\n### Related pull requests\n\n- web: u\n{BLOCK_END}"
            )
        );
        assert_eq!(
            describe("", None, None, &[]),
            format!("{BLOCK_BEGIN}\n{BLOCK_END}")
        );
    }

    #[test]
    fn the_block_is_replaced_and_what_is_outside_it_is_left_alone() {
        let first = describe("", Some("First."), None, &links(&[("web", "u1")]));
        let on_github =
            format!("Reviewer notes above.\r\n\r\n{first}\n\n## Checklist\n- [x] done\n");
        let second = describe(&on_github, Some("Second."), None, &links(&[("web", "u2")]));
        assert_eq!(
            second,
            format!(
                "Reviewer notes above.\r\n\r\n{BLOCK_BEGIN}\nSecond.\n\n{BLOCK_RELATED}\n\
                 ### Related pull requests\n\n- web: u2\n{BLOCK_END}\n\n## Checklist\n- [x] done\n"
            )
        );
        // Rewriting with the same input changes nothing.
        assert_eq!(
            describe(&second, Some("Second."), None, &links(&[("web", "u2")])),
            second
        );
        // Without a body, the one in the block stays; the links still follow.
        let third = describe(&second, None, None, &[]);
        assert!(
            third.contains(&format!("{BLOCK_BEGIN}\nSecond.\n{BLOCK_END}")),
            "{third}"
        );
        assert!(third.starts_with("Reviewer notes above."));
        assert!(third.ends_with("- [x] done\n"));
        // An empty body clears it.
        let cleared = describe(&second, Some(""), None, &[]);
        assert!(
            cleared.contains(&format!("{BLOCK_BEGIN}\n{BLOCK_END}")),
            "{cleared}"
        );
    }

    #[test]
    fn a_description_written_by_hand_gets_the_block_on_top() {
        let text = describe("Written on GitHub.\n", Some("From Calin."), None, &[]);
        assert_eq!(
            text,
            format!("{BLOCK_BEGIN}\nFrom Calin.\n{BLOCK_END}\n\nWritten on GitHub.\n")
        );
        // A block opened by an older wording of the marker is still found.
        let old = "<!-- calin:begin -->\nOld.\n<!-- calin:end -->\ntail";
        assert_eq!(
            describe(old, Some("New."), None, &[]),
            format!("{BLOCK_BEGIN}\nNew.\n{BLOCK_END}\ntail")
        );
        // Half a block is text like any other.
        let half = "<!-- calin:begin -->\nno end";
        assert!(describe(half, Some("x"), None, &[]).ends_with(half));
    }

    #[test]
    fn a_body_cannot_close_the_block_early() {
        let text = describe("", Some("sneaky <!-- calin:end --> tail"), None, &[]);
        let block = find_block(&text).unwrap();
        assert_eq!(block.after, "");
        assert!(block.inside.contains("tail"));
        let again = describe(&text, None, None, &links(&[("web", "u")]));
        assert!(again.contains("tail\n\n<!-- calin:related -->"), "{again}");
    }

    #[test]
    fn the_issue_line_sits_between_the_text_and_the_links_and_is_never_repeated() {
        let related = links(&[("web", "u2")]);
        let first = describe("", Some("Body."), Some("Closes #12"), &related);
        assert_eq!(
            first,
            format!(
                "{BLOCK_BEGIN}\nBody.\n\n{BLOCK_ISSUE}\nCloses #12\n\n{BLOCK_RELATED}\n\
                 ### Related pull requests\n\n- web: u2\n{BLOCK_END}"
            )
        );
        // A ship that keeps the text keeps one issue line, whatever it is asked again.
        let again = describe(&first, None, Some("Closes #12"), &related);
        assert_eq!(again, first);
        assert_eq!(describe(&again, None, Some("Closes #12"), &related), first);
        // Alone, and without a body.
        assert_eq!(
            describe("", None, Some("Issue: https://x/issues/1"), &[]),
            format!("{BLOCK_BEGIN}\n{BLOCK_ISSUE}\nIssue: https://x/issues/1\n{BLOCK_END}")
        );
        // A session without an issue drops a line an older block had.
        let dropped = describe(&first, None, None, &related);
        assert!(!dropped.contains("Closes"), "{dropped}");
        assert!(
            dropped.contains("Body.\n\n<!-- calin:related -->"),
            "{dropped}"
        );
    }

    #[test]
    fn the_issues_own_repo_closes_it_and_the_others_link_to_it() {
        let repo = |repo_id: &str| SessionRepo {
            id: format!("sr-{repo_id}"),
            session_id: "s".into(),
            repo_id: repo_id.into(),
            name: repo_id.into(),
            mode: RepoMode::Write,
            path: "/x".into(),
            branch: None,
            base_ref: None,
            primary: false,
        };
        let issue = IssueRef {
            repo_id: "api".into(),
            number: 12,
            title: "t".into(),
            url: "https://github.com/acme/api/issues/12".into(),
        };
        assert_eq!(
            issue_line(Some(&issue), &repo("api")).as_deref(),
            Some("Closes #12")
        );
        assert_eq!(
            issue_line(Some(&issue), &repo("web")).as_deref(),
            Some("Issue: https://github.com/acme/api/issues/12")
        );
        assert_eq!(issue_line(None, &repo("api")), None);
    }

    #[test]
    fn descriptions_compare_by_what_they_say() {
        assert!(same_text("a\r\nb\r\n", "a\nb"));
        assert!(same_text("  a\n", "a"));
        assert!(!same_text("a\nb", "a\nc"));
    }

    #[test]
    fn the_commit_message_names_the_session() {
        assert_eq!(
            commit_message("Add the thing", "01ABC"),
            "Add the thing\n\nCalin-Session: 01ABC\n"
        );
    }

    #[test]
    fn pull_requests_compare_without_the_time_they_were_seen() {
        let a = PullRequest {
            session_repo_id: "r".to_owned(),
            number: 1,
            url: "u".to_owned(),
            title: "t".to_owned(),
            state: PullRequestState::Open,
            checks: CheckState::Pending,
            review: ReviewState::None,
            updated_at: 1,
        };
        let later = PullRequest {
            updated_at: 2,
            ..a.clone()
        };
        assert!(same(&a, &later));
        let passed = PullRequest {
            checks: CheckState::Passing,
            ..later
        };
        assert!(!same(&a, &passed));
    }
}
