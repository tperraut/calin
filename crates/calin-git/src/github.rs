//! GitHub through the `gh` CLI: whether it can be used at all, and the pull
//! request of a branch. `gh` brings the user's login, so Calin holds no token.
//!
//! Every invocation runs without prompts, with a time limit, and with its
//! standard error kept for the error message.

use std::io::{self, Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use calin_proto::{CheckState, PullRequestState, ReviewState};
use serde::Deserialize;

use crate::cmd::{INHERITED, work_dir};
use crate::error::{Error, Result};
use crate::setup::{kill, kill_group};

mod issues;

pub use issues::{IssueQuery, PullRequestItem, on_github};

const DEFAULT_PROGRAM: &str = "gh";
/// Generous for one API round trip, short enough that a hung `gh` does not
/// hold a delivery up for good.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
/// Asking who is signed in is one small request, and health waits for it.
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// How often a running `gh` is checked for its end.
const POLL: Duration = Duration::from_millis(5);
/// After `gh` is gone, how long its output may still trickle in.
const DRAIN: Duration = Duration::from_millis(500);

/// What is asked of `gh` about a pull request, in its `--json` vocabulary.
const FIELDS: &str = "number,url,title,state,isDraft,statusCheckRollup,reviewDecision,body,\
     headRefName,isCrossRepository";
/// More open pull requests than one can share a head branch name (forks);
/// this many are looked at to find the repository's own.
const LIST_LIMIT: &str = "--limit=20";

/// How to run `gh`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHub {
    program: PathBuf,
    timeout: Duration,
}

impl Default for GitHub {
    /// `gh` on `PATH`.
    fn default() -> Self {
        Self::new(DEFAULT_PROGRAM)
    }
}

/// A pull request as GitHub describes it, in Calin's terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestInfo {
    pub number: u32,
    pub url: String,
    pub title: String,
    pub state: PullRequestState,
    pub checks: CheckState,
    pub review: ReviewState,
    /// The description, as it is on GitHub.
    pub body: String,
}

impl GitHub {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// How long one invocation may take before it is killed.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// `Ok` when pull requests can be opened from this machine, else a short
    /// reason for a person: `gh` is missing, or nobody is signed in.
    pub fn status(&self) -> std::result::Result<(), String> {
        let program = self.program.display();
        let quick = Self {
            program: self.program.clone(),
            timeout: self.timeout.min(STATUS_TIMEOUT),
        };
        // `--active`: an old login that went stale next to the one in use
        // must not count against it.
        let mut answer = quick.run(None, &["auth", "status", "--active"], None);
        if matches!(&answer, Ok(ran) if ran.stderr.contains("unknown flag")) {
            // A `gh` from before accounts could be switched.
            answer = quick.run(None, &["auth", "status"], None);
        }
        match answer {
            Ok(ran) if ran.status.success() => Ok(()),
            Ok(_) => Err(format!(
                "the GitHub CLI is not signed in: run `{program} auth login` on this machine"
            )),
            Err(Failure::Start(err)) if err.kind() == io::ErrorKind::NotFound => Err(format!(
                "the GitHub CLI (`{program}`) was not found on this machine"
            )),
            Err(Failure::Start(err) | Failure::Wait(err)) => {
                Err(format!("the GitHub CLI (`{program}`) cannot be run: {err}"))
            }
            Err(Failure::TimedOut) => Err(format!(
                "`{program} auth status` did not answer within {} seconds",
                quick.timeout.as_secs()
            )),
        }
    }

    /// The open pull request of this repository whose head is `branch`, if
    /// there is one. One opened from a fork's branch of the same name is not it.
    pub fn find_open_pull_request(
        &self,
        dir: &Path,
        branch: &str,
    ) -> Result<Option<PullRequestInfo>> {
        let head = format!("--head={branch}");
        let json = format!("--json={FIELDS}");
        let args = ["pr", "list", &head, "--state=open", LIST_LIMIT, &json];
        let ran = self.gh(dir, "pr list", &args, None)?;
        let listed: Vec<RawPullRequest> = parse("pr list", &ran.stdout)?;
        Ok(listed
            .into_iter()
            .find(|pr| pr.head_ref_name == branch && !pr.is_cross_repository)
            .map(PullRequestInfo::from))
    }

    /// Opens a pull request from `head` into `base`, both branches of the
    /// repository `dir` is a clone of, and returns it as GitHub then shows it.
    /// The head branch must already be pushed.
    pub fn create_pull_request(
        &self,
        dir: &Path,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<PullRequestInfo> {
        // `--flag=value`: a title that starts with a hyphen stays a title.
        let (base, head) = (format!("--base={base}"), format!("--head={head}"));
        let title = format!("--title={title}");
        let mut args = vec!["pr", "create", &base, &head, &title, "--body-file=-"];
        if draft {
            args.push("--draft");
        }
        let ran = self.gh(dir, "pr create", &args, Some(body))?;
        // All `gh` prints on success is the new pull request's URL.
        let url = ran.stdout.lines().map(str::trim).rfind(|l| !l.is_empty());
        let Some(url) = url.filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        else {
            return Err(unexpected(
                "pr create",
                format!("expected the pull request's URL, got {:?}", ran.stdout),
            ));
        };
        self.view_reference(dir, url)
    }

    /// Replaces the description of pull request `number`.
    pub fn edit_body(&self, dir: &Path, number: u32, body: &str) -> Result<()> {
        let number = number.to_string();
        let args = ["pr", "edit", &number, "--body-file=-"];
        self.gh(dir, "pr edit", &args, Some(body))?;
        Ok(())
    }

    /// Pull request `number` of the repository `dir` is a clone of.
    pub fn view(&self, dir: &Path, number: u32) -> Result<PullRequestInfo> {
        self.view_reference(dir, &number.to_string())
    }

    /// `reference` is a number or a URL.
    fn view_reference(&self, dir: &Path, reference: &str) -> Result<PullRequestInfo> {
        let json = format!("--json={FIELDS}");
        let ran = self.gh(dir, "pr view", &["pr", "view", reference, &json], None)?;
        let raw: RawPullRequest = parse("pr view", &ran.stdout)?;
        Ok(raw.into())
    }

    /// Runs `gh` in `dir` for a command that must succeed. `what` names the
    /// command in errors, without the arguments: they can hold a whole
    /// description.
    fn gh(&self, dir: &Path, what: &str, args: &[&str], stdin: Option<&str>) -> Result<Ran> {
        let dir = work_dir(dir)?;
        let command = format!("{} {what}", self.program.display());
        match self.run(Some(&dir), args, stdin) {
            Ok(ran) if ran.status.success() => Ok(ran),
            Ok(ran) => Err(Error::Git {
                command,
                stderr: if ran.stderr.is_empty() {
                    ran.status.to_string()
                } else {
                    ran.stderr
                },
            }),
            Err(Failure::TimedOut) => Err(Error::Git {
                command,
                stderr: format!("no answer within {} seconds", self.timeout.as_secs()),
            }),
            Err(Failure::Start(source) | Failure::Wait(source)) => Err(Error::Io {
                context: format!("running `{command}`"),
                source,
            }),
        }
    }

    /// One invocation, however it ends. It gets no terminal to ask on, and
    /// is killed with whatever it started when it takes too long.
    fn run(
        &self,
        dir: Option<&Path>,
        args: &[&str],
        stdin: Option<&str>,
    ) -> std::result::Result<Ran, Failure> {
        let mut cmd = Command::new(&self.program);
        cmd.args(args)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            // `gh` runs git to find the repository, which must be `dir`'s.
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for name in INHERITED {
            cmd.env_remove(name);
        }
        if let Some(dir) = dir {
            cmd.current_dir(dir);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().map_err(Failure::Start)?;

        if let (Some(mut pipe), Some(text)) = (child.stdin.take(), stdin) {
            let bytes = text.as_bytes().to_vec();
            // From another thread: `gh` may not read before it has written.
            thread::spawn(move || {
                // A closed pipe means `gh` stopped reading; its status tells why.
                let _ = pipe.write_all(&bytes);
            });
        }
        let stdout = collect(child.stdout.take());
        let stderr = collect(child.stderr.take());

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(err) => {
                    kill(&mut child);
                    return Err(Failure::Wait(err));
                }
            }
            if Instant::now() >= deadline {
                kill(&mut child);
                return Err(Failure::TimedOut);
            }
            thread::sleep(POLL);
        };
        // Something it left running would hold the pipes open.
        kill_group(&child);
        Ok(Ran {
            status,
            stdout: stdout.recv_timeout(DRAIN).unwrap_or_default(),
            stderr: stderr
                .recv_timeout(DRAIN)
                .unwrap_or_default()
                .trim()
                .to_owned(),
        })
    }
}

/// Reads a stream to its end on a thread of its own and hands over the text.
fn collect(stream: Option<impl Read + Send + 'static>) -> mpsc::Receiver<String> {
    let (done, text) = mpsc::channel();
    if let Some(mut stream) = stream {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            // What was read before an error is still what `gh` said.
            let _ = stream.read_to_end(&mut bytes);
            // Nobody listens once the drain was given up.
            let _ = done.send(String::from_utf8_lossy(&bytes).into_owned());
        });
    }
    text
}

/// A finished invocation.
struct Ran {
    status: ExitStatus,
    stdout: String,
    /// Trimmed.
    stderr: String,
}

enum Failure {
    /// `gh` could not be started.
    Start(io::Error),
    Wait(io::Error),
    TimedOut,
}

fn unexpected(what: &str, detail: String) -> Error {
    Error::Git {
        command: format!("gh {what}"),
        stderr: detail,
    }
}

fn parse<T: serde::de::DeserializeOwned>(what: &str, stdout: &str) -> Result<T> {
    serde_json::from_str(stdout)
        .map_err(|err| unexpected(what, format!("its output is not what was asked for: {err}")))
}

/// A pull request in `gh`'s JSON. Every field asked for must be there;
/// fields `gh` adds are ignored.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPullRequest {
    number: u32,
    url: String,
    title: String,
    state: RawState,
    is_draft: bool,
    /// Null or absent when the head commit has no checks.
    #[serde(default)]
    status_check_rollup: Option<Vec<RawCheck>>,
    /// Empty or null when no review decides anything.
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    body: Option<String>,
    head_ref_name: String,
    is_cross_repository: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum RawState {
    Open,
    Closed,
    Merged,
}

/// One entry of the rollup: a check run (`status`, then `conclusion` once it
/// is completed) or a commit status (`state` only).
#[derive(Deserialize)]
struct RawCheck {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

impl From<RawPullRequest> for PullRequestInfo {
    fn from(raw: RawPullRequest) -> Self {
        let state = match (raw.state, raw.is_draft) {
            (RawState::Merged, _) => PullRequestState::Merged,
            (RawState::Closed, _) => PullRequestState::Closed,
            (RawState::Open, true) => PullRequestState::Draft,
            (RawState::Open, false) => PullRequestState::Open,
        };
        let review = match raw.review_decision.as_deref() {
            Some("APPROVED") => ReviewState::Approved,
            Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
            _ => ReviewState::None,
        };
        Self {
            number: raw.number,
            url: raw.url,
            title: raw.title,
            state,
            checks: check_state(&raw.status_check_rollup.unwrap_or_default()),
            review,
            body: raw.body.unwrap_or_default(),
        }
    }
}

/// The combined result, sorted the way `gh pr checks` sorts: one failure
/// makes the whole failing, else one check still to finish makes it pending.
fn check_state(checks: &[RawCheck]) -> CheckState {
    let mut combined = CheckState::None;
    for check in checks {
        let word = |value: &Option<String>| value.clone().filter(|word| !word.is_empty());
        let outcome = match (word(&check.state), word(&check.status)) {
            (Some(state), _) => state,
            (None, Some(status)) if status == "COMPLETED" => {
                word(&check.conclusion).unwrap_or_default()
            }
            (None, status) => status.unwrap_or_default(),
        };
        match outcome.as_str() {
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED"
            | "STARTUP_FAILURE" => return CheckState::Failing,
            "SUCCESS" | "NEUTRAL" | "SKIPPED" => {
                if combined == CheckState::None {
                    combined = CheckState::Passing;
                }
            }
            // Queued, in progress, pending, waiting, expected, and whatever
            // GitHub adds next: not known to be over.
            _ => combined = CheckState::Pending,
        }
    }
    combined
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::testing::{Sandbox, write_executable};

    /// Logs its arguments, its standard input and what it was started with,
    /// then answers with what the test left for the command next to it.
    const FAKE_GH: &str = r#"#!/bin/sh
dir=$(dirname "$0")
key="$1-$2"
for arg in "$@"; do printf '%s\n' "$arg"; done >> "$dir/args-$key"
printf '%s\n' "$key" >> "$dir/log"
printf '%s|%s|%s\n' "$GH_PROMPT_DISABLED" "$GH_NO_UPDATE_NOTIFIER" "$(pwd)" > "$dir/env-$key"
case "$*" in *--body-file=-*) cat > "$dir/stdin-$key" ;; esac
if [ -f "$dir/sleep-$key" ]; then sleep "$(cat "$dir/sleep-$key")"; fi
if [ -f "$dir/err-$key" ]; then cat "$dir/err-$key" >&2; fi
if [ -f "$dir/out-$key" ]; then cat "$dir/out-$key"; fi
if [ -f "$dir/code-$key" ]; then exit "$(cat "$dir/code-$key")"; fi
exit 0
"#;

    pub(super) struct Fake {
        pub(super) sandbox: Sandbox,
        pub(super) bin: PathBuf,
        /// Where `gh` is run: any directory will do for a fake.
        pub(super) work: PathBuf,
    }

    impl Fake {
        pub(super) fn new() -> Self {
            let sandbox = Sandbox::new();
            let bin = sandbox.dir("bin").join("gh");
            write_executable(&bin, FAKE_GH);
            let work = sandbox.dir("work");
            Self { sandbox, bin, work }
        }

        pub(super) fn github(&self) -> GitHub {
            GitHub::new(&self.bin).with_timeout(Duration::from_secs(30))
        }

        pub(super) fn set(&self, file: &str, content: impl AsRef<[u8]>) {
            std::fs::write(self.bin.with_file_name(file), content).unwrap();
        }

        pub(super) fn read(&self, file: &str) -> String {
            std::fs::read_to_string(self.bin.with_file_name(file)).unwrap_or_default()
        }

        pub(super) fn args(&self, key: &str) -> Vec<String> {
            let args = self.read(&format!("args-{key}"));
            args.lines().map(str::to_owned).collect()
        }

        pub(super) fn log(&self) -> Vec<String> {
            self.read("log").lines().map(str::to_owned).collect()
        }
    }

    fn pull_request(overrides: Value) -> Value {
        let mut pr = json!({
            "number": 12,
            "url": "https://github.com/acme/api/pull/12",
            "title": "Add the thing",
            "state": "OPEN",
            "isDraft": true,
            "statusCheckRollup": [],
            "reviewDecision": "",
            "body": "What it does.",
            "headRefName": "calin/add-the-thing-abc123",
            "isCrossRepository": false,
            "somethingNew": {"ignored": true},
        });
        for (key, value) in overrides.as_object().unwrap() {
            pr[key] = value.clone();
        }
        pr
    }

    fn info(overrides: Value) -> PullRequestInfo {
        let raw: RawPullRequest = serde_json::from_value(pull_request(overrides)).unwrap();
        raw.into()
    }

    #[test]
    fn status_tells_a_usable_gh_from_a_missing_one_and_from_no_login() {
        let fake = Fake::new();
        assert_eq!(fake.github().status(), Ok(()));
        assert_eq!(fake.args("auth-status"), ["auth", "status", "--active"]);
        assert_eq!(fake.read("env-auth-status").split('|').next(), Some("1"));

        fake.set("code-auth-status", "1");
        fake.set(
            "err-auth-status",
            "You are not logged into any GitHub hosts.",
        );
        let reason = fake.github().status().unwrap_err();
        assert!(reason.contains("not signed in"), "{reason}");
        assert!(reason.contains("auth login"), "{reason}");

        let missing = GitHub::new(fake.sandbox.path().join("nope/gh"));
        let reason = missing.status().unwrap_err();
        assert!(reason.contains("was not found"), "{reason}");

        // Present but not something that runs.
        let plain = fake.sandbox.path().join("plain");
        std::fs::write(&plain, "not a program").unwrap();
        let reason = GitHub::new(&plain).status().unwrap_err();
        assert!(reason.contains("cannot be run"), "{reason}");

        fake.set("code-auth-status", "0");
        fake.set("sleep-auth-status", "30");
        let slow = fake.github().with_timeout(Duration::from_millis(200));
        let reason = slow.status().unwrap_err();
        assert!(reason.contains("did not answer"), "{reason}");
    }

    #[test]
    fn an_old_gh_without_the_active_flag_is_asked_again_without_it() {
        let sandbox = Sandbox::new();
        let bin = sandbox.dir("bin").join("gh");
        write_executable(
            &bin,
            "#!/bin/sh\ncase \"$*\" in *--active*) echo 'unknown flag: --active' >&2; exit 1;; esac\n",
        );
        assert_eq!(GitHub::new(&bin).status(), Ok(()));
    }

    #[test]
    fn the_open_pull_request_of_a_branch_is_found_among_what_gh_lists() {
        let fake = Fake::new();
        let branch = "calin/add-the-thing-abc123";
        let listed = json!([
            pull_request(json!({"number": 7, "isCrossRepository": true})),
            pull_request(json!({"number": 8, "headRefName": "calin/add-the-thing-abc123-2"})),
            pull_request(json!({"reviewDecision": "APPROVED", "isDraft": false})),
        ]);
        fake.set("out-pr-list", listed.to_string());
        let found = fake
            .github()
            .find_open_pull_request(&fake.work, branch)
            .unwrap()
            .unwrap();
        assert_eq!(
            found,
            PullRequestInfo {
                number: 12,
                url: "https://github.com/acme/api/pull/12".to_owned(),
                title: "Add the thing".to_owned(),
                state: PullRequestState::Open,
                checks: CheckState::None,
                review: ReviewState::Approved,
                body: "What it does.".to_owned(),
            }
        );
        assert_eq!(
            fake.args("pr-list"),
            [
                "pr",
                "list",
                "--head=calin/add-the-thing-abc123",
                "--state=open",
                "--limit=20",
                &format!("--json={FIELDS}"),
            ]
        );
        // No prompts, no update notice, and run in the repository.
        assert_eq!(
            fake.read("env-pr-list").trim(),
            format!("1|1|{}", fake.work.display())
        );

        fake.set("out-pr-list", "[]");
        let none = fake.github().find_open_pull_request(&fake.work, branch);
        assert_eq!(none.unwrap(), None);
    }

    #[test]
    fn a_pull_request_is_created_with_its_body_on_stdin_and_then_looked_up() {
        let fake = Fake::new();
        let url = "https://github.com/acme/api/pull/12";
        fake.set(
            "out-pr-create",
            format!("Creating pull request...\n{url}\n"),
        );
        fake.set("out-pr-view", pull_request(json!({})).to_string());
        let body = "First line.\n\n- \"quoted\" and `code`\n";
        let created = fake
            .github()
            .create_pull_request(
                &fake.work,
                "main",
                "calin/x",
                "--fix: the thing",
                body,
                true,
            )
            .unwrap();
        assert_eq!(created.number, 12);
        assert_eq!(created.state, PullRequestState::Draft);
        assert_eq!(
            fake.args("pr-create"),
            [
                "pr",
                "create",
                "--base=main",
                "--head=calin/x",
                "--title=--fix: the thing",
                "--body-file=-",
                "--draft",
            ]
        );
        assert_eq!(fake.read("stdin-pr-create"), body);
        assert_eq!(
            fake.args("pr-view"),
            ["pr", "view", url, &format!("--json={FIELDS}")]
        );
        assert_eq!(fake.log(), ["pr-create", "pr-view"]);

        // Not a draft: no flag.
        std::fs::remove_file(fake.bin.with_file_name("args-pr-create")).unwrap();
        let github = fake.github();
        github
            .create_pull_request(&fake.work, "main", "calin/x", "t", "b", false)
            .unwrap();
        assert!(!fake.args("pr-create").contains(&"--draft".to_owned()));

        // Anything but a URL is not a created pull request.
        fake.set("out-pr-create", "done\n");
        let err = github
            .create_pull_request(&fake.work, "main", "calin/x", "t", "b", true)
            .unwrap_err();
        assert!(
            matches!(&err, Error::Git { stderr, .. } if stderr.contains("URL")),
            "{err:?}"
        );
    }

    #[test]
    fn a_description_is_replaced_and_a_pull_request_viewed_by_number() {
        let fake = Fake::new();
        fake.github()
            .edit_body(&fake.work, 12, "new\nbody")
            .unwrap();
        assert_eq!(fake.args("pr-edit"), ["pr", "edit", "12", "--body-file=-"]);
        assert_eq!(fake.read("stdin-pr-edit"), "new\nbody");

        let merged = json!({"state": "MERGED", "isDraft": false, "body": null});
        fake.set("out-pr-view", pull_request(merged).to_string());
        let viewed = fake.github().view(&fake.work, 12).unwrap();
        assert_eq!(viewed.state, PullRequestState::Merged);
        assert_eq!(viewed.body, "");
        assert_eq!(
            fake.args("pr-view"),
            ["pr", "view", "12", &format!("--json={FIELDS}")]
        );
    }

    #[test]
    fn a_failing_gh_is_an_error_with_what_it_said() {
        let fake = Fake::new();
        let github = fake.github();
        fake.set("code-pr-view", "1");
        fake.set(
            "err-pr-view",
            "GraphQL: Could not resolve to a PullRequest\n",
        );
        let err = github.view(&fake.work, 99).unwrap_err();
        let Error::Git { command, stderr } = &err else {
            panic!("expected the command's error, got {err:?}");
        };
        assert!(command.ends_with("gh pr view"), "{command}");
        assert_eq!(stderr, "GraphQL: Could not resolve to a PullRequest");

        // Silent failures still say how they ended.
        fake.set("code-pr-edit", "4");
        let err = github.edit_body(&fake.work, 1, "b").unwrap_err();
        assert!(
            matches!(&err, Error::Git { stderr, .. } if stderr.contains('4')),
            "{err:?}"
        );
        fake.set("code-pr-list", "1");
        assert!(github.find_open_pull_request(&fake.work, "b").is_err());
        fake.set("code-pr-create", "1");
        fake.set(
            "err-pr-create",
            "pull request create failed: No commits between main and x",
        );
        let viewed = fake.log().iter().filter(|key| *key == "pr-view").count();
        let err = github
            .create_pull_request(&fake.work, "main", "x", "t", "b", true)
            .unwrap_err();
        assert!(err.to_string().contains("No commits between"), "{err}");
        // Nothing was looked up after the failed creation.
        let now = fake.log().iter().filter(|key| *key == "pr-view").count();
        assert_eq!(now, viewed);

        let missing = GitHub::new(fake.sandbox.path().join("nope/gh"));
        assert!(matches!(missing.view(&fake.work, 1), Err(Error::Io { .. })));
        assert!(matches!(
            github.view(&fake.work.join("gone"), 1),
            Err(Error::NotFound(_))
        ));
    }

    #[test]
    fn output_that_is_not_the_json_asked_for_is_an_error() {
        let fake = Fake::new();
        let github = fake.github();
        let mut missing_state = pull_request(json!({}));
        missing_state.as_object_mut().unwrap().remove("state");
        let cases = [
            String::new(),
            "not json".to_owned(),
            "{\"number\": 12".to_owned(),
            "[]".to_owned(),
            missing_state.to_string(),
            pull_request(json!({"state": "LOCKED"})).to_string(),
            pull_request(json!({"number": "12"})).to_string(),
            pull_request(json!({"number": -1})).to_string(),
            pull_request(json!({"isDraft": "no"})).to_string(),
            pull_request(json!({"statusCheckRollup": {"state": "SUCCESS"}})).to_string(),
        ];
        for output in cases {
            fake.set("out-pr-view", &output);
            let err = github.view(&fake.work, 12).unwrap_err();
            assert!(
                matches!(&err, Error::Git { stderr, .. } if stderr.contains("not what was asked")),
                "{output:?} gave {err:?}"
            );
        }
        fake.set("out-pr-list", pull_request(json!({})).to_string());
        assert!(github.find_open_pull_request(&fake.work, "b").is_err());
    }

    #[test]
    fn a_gh_that_hangs_is_killed_at_the_limit() {
        let fake = Fake::new();
        fake.set("sleep-pr-view", "30");
        let github = fake.github().with_timeout(Duration::from_millis(200));
        let started = Instant::now();
        let err = github.view(&fake.work, 12).unwrap_err();
        assert!(
            matches!(&err, Error::Git { stderr, .. } if stderr.contains("no answer within")),
            "{err:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn state_comes_from_the_state_and_the_draft_flag() {
        let state =
            |state: &str, draft: bool| info(json!({"state": state, "isDraft": draft})).state;
        assert_eq!(state("OPEN", true), PullRequestState::Draft);
        assert_eq!(state("OPEN", false), PullRequestState::Open);
        assert_eq!(state("MERGED", false), PullRequestState::Merged);
        assert_eq!(state("MERGED", true), PullRequestState::Merged);
        assert_eq!(state("CLOSED", false), PullRequestState::Closed);
        // A draft that was closed is closed.
        assert_eq!(state("CLOSED", true), PullRequestState::Closed);
    }

    #[test]
    fn review_comes_from_the_review_decision() {
        let review = |decision: Value| info(json!({"reviewDecision": decision})).review;
        assert_eq!(review(json!("APPROVED")), ReviewState::Approved);
        assert_eq!(
            review(json!("CHANGES_REQUESTED")),
            ReviewState::ChangesRequested
        );
        assert_eq!(review(json!("REVIEW_REQUIRED")), ReviewState::None);
        assert_eq!(review(json!("")), ReviewState::None);
        assert_eq!(review(Value::Null), ReviewState::None);
        let mut absent = pull_request(json!({}));
        absent.as_object_mut().unwrap().remove("reviewDecision");
        let raw: RawPullRequest = serde_json::from_value(absent).unwrap();
        assert_eq!(PullRequestInfo::from(raw).review, ReviewState::None);
    }

    #[test]
    fn checks_come_from_the_status_check_rollup() {
        let run = |status: &str, conclusion: &str| json!({"__typename": "CheckRun", "name": "ci", "status": status, "conclusion": conclusion});
        let status =
            |state: &str| json!({"__typename": "StatusContext", "context": "ci", "state": state});
        let checks = |rollup: Value| info(json!({"statusCheckRollup": rollup})).checks;
        let passed = || run("COMPLETED", "SUCCESS");

        // No checks at all.
        assert_eq!(checks(json!([])), CheckState::None);
        assert_eq!(checks(Value::Null), CheckState::None);
        let mut absent = pull_request(json!({}));
        absent.as_object_mut().unwrap().remove("statusCheckRollup");
        let raw: RawPullRequest = serde_json::from_value(absent).unwrap();
        assert_eq!(PullRequestInfo::from(raw).checks, CheckState::None);

        // Everything over and nothing failed.
        for conclusion in ["SUCCESS", "NEUTRAL", "SKIPPED"] {
            let rollup = json!([run("COMPLETED", conclusion), passed()]);
            assert_eq!(checks(rollup), CheckState::Passing, "{conclusion}");
        }
        assert_eq!(checks(json!([status("SUCCESS")])), CheckState::Passing);

        // Something is still to finish.
        for waiting in ["QUEUED", "IN_PROGRESS", "PENDING", "WAITING", "REQUESTED"] {
            let rollup = json!([passed(), run(waiting, "")]);
            assert_eq!(checks(rollup), CheckState::Pending, "{waiting}");
            let rollup = json!([run(waiting, ""), passed()]);
            assert_eq!(checks(rollup), CheckState::Pending, "{waiting}");
        }
        for waiting in ["PENDING", "EXPECTED"] {
            let rollup = json!([status(waiting), passed()]);
            assert_eq!(checks(rollup), CheckState::Pending, "{waiting}");
        }
        // A conclusion nobody knows is not a pass.
        let rollup = json!([run("COMPLETED", "SOMETHING_NEW")]);
        assert_eq!(checks(rollup), CheckState::Pending);
        assert_eq!(checks(json!([{"name": "bare"}])), CheckState::Pending);

        // One failure decides, whatever else is running or passed.
        for conclusion in [
            "FAILURE",
            "CANCELLED",
            "TIMED_OUT",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
        ] {
            let failed = run("COMPLETED", conclusion);
            let rollup = json!([passed(), run("IN_PROGRESS", ""), failed, passed()]);
            assert_eq!(checks(rollup), CheckState::Failing, "{conclusion}");
            let rollup = json!([failed, run("QUEUED", "")]);
            assert_eq!(checks(rollup), CheckState::Failing, "{conclusion}");
        }
        for state in ["FAILURE", "ERROR"] {
            let rollup = json!([passed(), status("PENDING"), status(state)]);
            assert_eq!(checks(rollup), CheckState::Failing, "{state}");
        }
    }
}
