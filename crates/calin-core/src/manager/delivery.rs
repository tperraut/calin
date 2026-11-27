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
    let plan = Arc::new(plan);
    let mut delivered: Vec<Delivered> = Vec::new();
    let mut failed = false;
    for (index, (repo, known)) in plan.repos.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // `live` and `read` repos are the user's own checkouts: never
        // committed to, pushed or shipped.
        if repo.mode != RepoMode::Write {
            continue;
        }
        let work = {
            let (plan, repo) = (Arc::clone(&plan), repo.clone());
            tokio::task::spawn_blocking(move || deliver(&plan, &repo))
        };
        let pushed = match work.await {
            Ok(Ok(Some(pushed))) => pushed,
            Ok(Ok(None)) => continue,
            Ok(Err(failure)) => {
                failed = true;
                if report(&results, &repo.name, &failure).await.is_err() {
                    return;
                }
                continue;
            }
            Err(panicked) => {
                failed = true;
                let failure = Failure {
                    step: "delivery",
                    error: panicked.to_string(),
                };
                if report(&results, &repo.name, &failure).await.is_err() {
                    return;
                }
                continue;
            }
        };
        let (pull_request, body) = match pushed.info {
            Some(info) => {
                let body = info.body.clone();
                (on_github(&repo.id, info), Some(body))
            }
            None => match made_up(&plan, repo, known.as_ref()).await {
                Ok(pull_request) => (pull_request, None),
                Err(error) => {
                    failed = true;
                    let failure = Failure {
                        step: "pull request",
                        error: error.to_string(),
                    };
                    if report(&results, &repo.name, &failure).await.is_err() {
                        return;
                    }
                    continue;
                }
            },
        };
        let on_record = known
            .as_ref()
            .filter(|known| known.number == pull_request.number && known.url == pull_request.url);
        let new = on_record.is_none();
        let changed =
            on_record.is_some_and(|known| pushed.new_commits || !same(known, &pull_request));
        if new
            && results
                .send(Shipped::Opened(pull_request.clone()))
                .await
                .is_err()
        {
            return;
        }
        delivered.push(Delivered {
            index,
            repo: repo.clone(),
            pull_request,
            body,
            new,
            changed,
        });
    }

    if delivered.is_empty() {
        if !failed {
            let nothing = "nothing to ship: no repository attached for writing differs from \
                           its base";
            let _ = results.send(Shipped::Failed(nothing.to_owned())).await;
        }
        return;
    }

    // Every repo was attempted: only now are all the siblings known.
    let any_new = delivered.iter().any(|d| d.new);
    for d in &delivered {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let repo = &d.repo;
        let related = related(&plan, &delivered, d.index);
        let described = match &d.body {
            // Nothing is written anywhere in dry run; the events are the
            // ones a real ship gives when a sibling appears.
            None => any_new && !related.is_empty(),
            Some(current) => {
                let issue = issue_line(plan.issue.as_ref(), repo);
                let wanted = describe(current, plan.body.as_deref(), issue.as_deref(), &related);
                if same_text(&wanted, current) {
                    false
                } else {
                    let (github, dir) = (plan.github.clone(), repo.path.clone());
                    let number = d.pull_request.number;
                    let edited = tokio::task::spawn_blocking(move || {
                        github.edit_body(Path::new(&dir), number, &wanted)
                    })
                    .await;
                    let error = match edited {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(error.to_string()),
                        Err(panicked) => Some(panicked.to_string()),
                    };
                    let rewritten = error.is_none();
                    if let Some(error) = error {
                        let failure = Failure {
                            step: "description",
                            error,
                        };
                        if report(&results, &repo.name, &failure).await.is_err() {
                            return;
                        }
                    }
                    rewritten
                }
            }
        };
        let step = if described || d.changed {
            Shipped::Updated(PullRequest {
                updated_at: now_ms(),
                ..d.pull_request.clone()
            })
        } else if d.new {
            // Already announced as opened, and nothing happened to it since.
            continue;
        } else {
            Shipped::Kept
        };
        if results.send(step).await.is_err() {
            return;
        }
    }
}

async fn report(
    results: &mpsc::Sender<Shipped>,
    repo: &str,
    failure: &Failure,
) -> Result<(), mpsc::error::SendError<Shipped>> {
    let message = format!(
        "could not ship {repo}: {} failed: {}",
        failure.step, failure.error
    );
    results.send(Shipped::Failed(message)).await
}

/// Blocking. Commits what is uncommitted, pushes the session's branch and
/// finds or opens its pull request. `None` when the repo has nothing to
/// deliver. In dry run it stops after the commit.
fn deliver(plan: &ShipPlan, repo: &SessionRepo) -> Result<Option<Pushed>, Failure> {
    let at = |step: &'static str| {
        move |error: calin_git::Error| Failure {
            step,
            error: error.to_string(),
        }
    };
    // A `write` repo always has both; without them there is nothing to measure.
    let (Some(branch), Some(base)) = (&repo.branch, &repo.base_ref) else {
        return Ok(None);
    };
    let dir = Path::new(&repo.path);
    // Changed files, or commits whose changes cancel out: either way the
    // branch is not its base any more.
    let differs = !calin_git::changes(dir, base)
        .map_err(at("changes"))?
        .is_empty()
        || calin_git::commits_ahead(dir, base, branch).map_err(at("changes"))? > 0;
    if !differs {
        return Ok(None);
    }

    let mut committed = false;
    if calin_git::is_dirty(dir).map_err(at("commit"))? {
        // The commit lands on whatever is checked out, and what is pushed
        // is the session's branch: they have to be the same.
        let current = calin_git::current_branch(dir).map_err(at("commit"))?;
        if current.as_deref() != Some(branch.as_str()) {
            return Err(Failure {
                step: "commit",
                error: format!(
                    "the worktree is on {}, not on the session's branch {branch}",
                    current.as_deref().unwrap_or("a detached HEAD")
                ),
            });
        }
        let message = commit_message(&plan.title, &plan.session_id);
        let outcome =
            calin_git::commit_all(dir, &message, plan.commit_timeout).map_err(at("commit"))?;
        committed = matches!(outcome, CommitOutcome::Committed(_));
    }
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

    if !calin_git::has_remote(dir, REMOTE).map_err(at("push"))? {
        return Err(Failure {
            step: "push",
            error: format!("the repository has no `{REMOTE}` remote to push to"),
        });
    }
    let Some(default_branch) = calin_git::default_branch(dir, REMOTE).map_err(at("push"))? else {
        return Err(Failure {
            step: "push",
            error: "the repository's default branch is not known".to_owned(),
        });
    };
    let new_commits = calin_git::has_unpushed_commits(dir, REMOTE, branch).map_err(at("push"))?;
    calin_git::push_branch(dir, REMOTE, branch, plan.push_timeout).map_err(at("push"))?;

    let open = plan
        .github
        .find_open_pull_request(dir, branch)
        .map_err(at("pull request"))?;
    let info = match open {
        Some(info) => info,
        None => {
            // The links come later, once every repo has its pull request.
            let issue = issue_line(plan.issue.as_ref(), repo);
            let body = describe("", plan.body.as_deref(), issue.as_deref(), &[]);
            plan.github
                .create_pull_request(dir, &default_branch, branch, &plan.title, &body, plan.draft)
                .map_err(at("pull request"))?
        }
    };
    Ok(Some(Pushed {
        info: Some(info),
        new_commits,
    }))
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
    let text = match (body, &found) {
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
    };
    let mut block = format!("{BLOCK_BEGIN}\n");
    if !text.is_empty() {
        block.push_str(&text);
        block.push('\n');
    }
    if let Some(issue) = issue {
        if !text.is_empty() {
            block.push('\n');
        }
        block.push_str(&format!("{BLOCK_ISSUE}\n{issue}\n"));
    }
    if !related.is_empty() {
        if !text.is_empty() || issue.is_some() {
            block.push('\n');
        }
        block.push_str(&format!("{BLOCK_RELATED}\n{RELATED_HEADING}\n\n"));
        for (name, url) in related {
            block.push_str(&format!("- {name}: {url}\n"));
        }
    }
    block.push_str(BLOCK_END);
    match found {
        Some(found) => format!("{}{block}{}", found.before, found.after),
        None if current.trim().is_empty() => block,
        None => format!("{block}\n\n{current}"),
    }
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
