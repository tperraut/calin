//! Delivering a session's work: committing what is uncommitted and pushing
//! the session's branch. Nothing here amends, forces or touches another branch.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::cmd::{Done, Git, no_ssh_prompt, require_commit, resolve_commit, work_dir};
use crate::error::{Error, Result};
use crate::worktree::main_branch;

/// Makes git refuse to invent an identity from the user and host names: a
/// commit signed `calin@vps.(none)` is worse than an error that says why.
const CONFIGURED_IDENTITY_ONLY: [&str; 2] = ["-c", "user.useConfigOnly=true"];

/// How [`commit_all`] ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The new commit.
    Committed(String),
    /// Nothing differed from `HEAD` once everything was staged.
    NothingToCommit,
}

/// Stages everything in `worktree` that is not ignored and commits it on the
/// branch that is checked out, with `message` as it is.
///
/// The commit is made with the machine's git identity and runs the repo's
/// hooks, like a commit made by hand. It is never an amend.
///
/// Every git command it runs gets no input and nothing to ask on, and all
/// of them together get `limit`: at the limit the one running is killed with
/// whatever it started (a hook included), and the error says so.
///
/// [`Error::Conflict`] when git has no name or email to commit with; the
/// changes are staged then, and nothing else happened. [`Error::Git`] with
/// git's own words when the commit fails, for example in a hook, or when it
/// timed out.
pub fn commit_all(worktree: &Path, message: &str, limit: Duration) -> Result<CommitOutcome> {
    let dir = work_dir(worktree)?;
    if message.trim().is_empty() {
        return Err(Error::Conflict("a commit needs a message".to_owned()));
    }
    let deadline = Instant::now() + limit;
    let run = |git: Git| git.exec_until(deadline, limit);
    if !stage_all(&dir, &run)? {
        return Ok(CommitOutcome::NothingToCommit);
    }
    commit_staged(&dir, message, &run).map(CommitOutcome::Committed)
}

/// Stages everything that is not ignored. True when the index then differs
/// from `HEAD`.
fn stage_all(dir: &Path, run: &impl Fn(Git) -> Result<Done>) -> Result<bool> {
    run(Git::write(dir).args(["add", "--all"]))?.succeeded()?;
    // Status 1 says the index differs from `HEAD`.
    let staged = run(Git::read(dir).args(["diff", "--cached", "--quiet"]))?;
    match staged.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(staged.into_error()),
    }
}

/// Commits the index with git's configured identity; the new commit's id.
fn commit_staged(dir: &Path, message: &str, run: &impl Fn(Git) -> Result<Done>) -> Result<String> {
    for who in ["GIT_AUTHOR_IDENT", "GIT_COMMITTER_IDENT"] {
        let known = run(Git::read(dir)
            .args(CONFIGURED_IDENTITY_ONLY)
            .args(["var", who]))?;
        if !known.status.success() {
            return Err(Error::Conflict(
                "git has no identity to commit with: set one with `git config --global \
                 user.name \"Your Name\"` and `git config --global user.email you@example.com`"
                    .to_owned(),
            ));
        }
    }
    // One argument, whatever the message holds; stdin stays closed so that
    // a hook reading it ends at once instead of waiting.
    run(Git::write(dir)
        .args(CONFIGURED_IDENTITY_ONLY)
        .args(["commit", "--quiet"])
        .arg_shown_as(format!("--message={message}"), "--message=<message>"))?
    .succeeded()?;
    resolve_commit(dir, "HEAD")?
        .ok_or_else(|| Error::NotFound("the commit that was just made".to_owned()))
}

/// Pushes `branch` to the branch of the same name on `remote` and makes that
/// its upstream. Never forced: a remote branch with commits the local one
/// lacks rejects the push, which is an [`Error::Git`] with git's message.
/// Nothing is ever asked on a terminal, the push gets no input, and it is
/// killed with whatever it started (a hook, ssh) once it has run for
/// `limit`: an [`Error::Git`] that says it timed out.
///
/// Refused before anything runs, with [`Error::Conflict`], when `branch` is
/// the repo's default branch ([`default_branch`]): a session delivers through
/// a pull request, never by pushing to the branch it would be merged into.
/// [`Error::NotFound`] when there is no such remote or no such branch.
pub fn push_branch(worktree: &Path, remote: &str, branch: &str, limit: Duration) -> Result<()> {
    let dir = work_dir(worktree)?;
    if !has_remote(&dir, remote)? {
        return Err(Error::NotFound(format!("remote {remote}")));
    }
    let full_ref = local_ref(branch)?;
    if default_branch(&dir, remote)?.as_deref() == Some(branch) {
        return Err(Error::Conflict(format!(
            "{branch} is the default branch of this repository and is never pushed to"
        )));
    }
    require_commit(&dir, &full_ref)?;
    // Both sides spelled out: a refspec that cannot start with `+` (which
    // would force) and cannot be read as an option or a tag.
    no_ssh_prompt(Git::write(&dir))
        .args(["push", "--set-upstream", remote])
        .arg(format!("{full_ref}:{full_ref}"))
        .exec_until(Instant::now() + limit, limit)?
        .succeeded()
        .map(drop)
}

/// True when `dir` has a remote called `remote`.
pub fn has_remote(dir: &Path, remote: &str) -> Result<bool> {
    let dir = work_dir(dir)?;
    // A name that git would read as an option or as a config section of its own.
    if remote.is_empty() || remote.starts_with('-') || remote.contains(['.', '\n', ' ']) {
        return Ok(false);
    }
    let url = Git::read(&dir)
        .args(["config", "--get", &format!("remote.{remote}.url")])
        .probe()?;
    Ok(url.is_some())
}

/// The branch pull requests of this repository go to: the one `remote`'s
/// `HEAD` pointed to when it was last fetched, else the branch checked out
/// in the clone itself (not in the worktree asking). `None` when neither is
/// known.
pub fn default_branch(dir: &Path, remote: &str) -> Result<Option<String>> {
    let dir = work_dir(dir)?;
    let prefix = format!("refs/remotes/{remote}/");
    let remote_head = Git::read(&dir)
        .args(["symbolic-ref", "--quiet", &format!("{prefix}HEAD")])
        .probe()?
        .and_then(|name| name.strip_prefix(&prefix).map(str::to_owned));
    match remote_head {
        Some(branch) => Ok(Some(branch)),
        None => main_branch(&dir),
    }
}

/// The branch checked out in `dir`. `None` when `HEAD` is detached.
pub fn current_branch(dir: &Path) -> Result<Option<String>> {
    let dir = work_dir(dir)?;
    Ok(Git::read(&dir)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .probe()?
        .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_owned)))
}

/// How many commits `branch` has that `base` does not.
pub fn commits_ahead(dir: &Path, base: &str, branch: &str) -> Result<u32> {
    let dir = work_dir(dir)?;
    let base = require_commit(&dir, base)?;
    let tip = require_commit(&dir, &local_ref(branch)?)?;
    count(&dir, &format!("{base}..{tip}"))
}

/// True when `branch` has commits that the branch of the same name on
/// `remote` did not have when it was last pushed to or fetched; also when
/// that branch is not known to exist there at all.
pub fn has_unpushed_commits(dir: &Path, remote: &str, branch: &str) -> Result<bool> {
    let dir = work_dir(dir)?;
    let tip = require_commit(&dir, &local_ref(branch)?)?;
    let tracking = format!("refs/remotes/{remote}/{branch}");
    match resolve_commit(&dir, &tracking)? {
        Some(pushed) => Ok(count(&dir, &format!("{pushed}..{tip}"))? > 0),
        None => Ok(true),
    }
}

/// True when the work tree holds anything a commit would record: staged or
/// unstaged changes, or untracked files that are not ignored.
pub fn is_dirty(dir: &Path) -> Result<bool> {
    let dir = work_dir(dir)?;
    let status = Git::read(&dir)
        .args(["status", "--porcelain", "-z", "--untracked-files=normal"])
        .run()?;
    Ok(!status.is_empty())
}

fn count(dir: &Path, range: &str) -> Result<u32> {
    let text = Git::read(dir).args(["rev-list", "--count", range]).text()?;
    text.trim().parse().map_err(|_| Error::Git {
        command: format!("git rev-list --count {range}"),
        stderr: format!("unexpected output {text:?}"),
    })
}

/// `refs/heads/<branch>`, for a name that can be one.
fn local_ref(branch: &str) -> Result<String> {
    if branch.is_empty() || branch.starts_with(['-', '+']) || branch.contains(['\n', ':', ' ']) {
        return Err(Error::Git {
            command: "git push".to_owned(),
            stderr: format!("{branch:?} is not a valid branch name"),
        });
    }
    Ok(format!("refs/heads/{branch}"))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::testing::{
        Sandbox, commit_all as commit, git, git_ok, without_identity, write, write_executable,
    };
    use crate::worktree::create_worktree;

    const BRANCH: &str = "calin/feature-abc123";
    /// Long enough for any git command a test runs that is meant to finish.
    const LIMIT: Duration = Duration::from_secs(30);
    /// What a test gives a command that is meant to hang.
    const SHORT: Duration = Duration::from_millis(400);

    /// A bare repository, a clone of it, and a worktree of the clone on [`BRANCH`].
    struct Delivery {
        sandbox: Sandbox,
        remote: PathBuf,
        clone: PathBuf,
        worktree: PathBuf,
    }

    fn delivery() -> Delivery {
        let sandbox = Sandbox::new();
        let seed = sandbox.repo("seed");
        let remote = sandbox.path().join("remote.git");
        git(
            sandbox.path(),
            &[
                "clone",
                "-q",
                "--bare",
                seed.to_str().unwrap(),
                "remote.git",
            ],
        );
        let clone = sandbox.path().join("clone");
        git(
            sandbox.path(),
            &["clone", "-q", remote.to_str().unwrap(), "clone"],
        );
        let worktree = sandbox.path().join("session/clone");
        create_worktree(&clone, &worktree, BRANCH, None).unwrap();
        Delivery {
            sandbox,
            remote,
            clone,
            worktree,
        }
    }

    fn remote_tip(remote: &Path, branch: &str) -> Option<String> {
        let full = format!("refs/heads/{branch}");
        git_ok(remote, &["rev-parse", "--verify", "--quiet", &full])
            .then(|| git(remote, &["rev-parse", &full]))
    }

    #[test]
    fn dirty_work_is_committed_on_the_checked_out_branch() {
        let d = delivery();
        let base = git(&d.worktree, &["rev-parse", "HEAD"]);
        assert!(!is_dirty(&d.worktree).unwrap());
        write(&d.worktree, "src/new.rs", "fn new() {}\n");
        write(&d.worktree, "README.md", "# changed\n");
        git(&d.worktree, &["add", "README.md"]);
        assert!(is_dirty(&d.worktree).unwrap());

        let message = "Add the thing\n\nCalin-Session: 01ABC\n";
        let CommitOutcome::Committed(commit) = commit_all(&d.worktree, message, LIMIT).unwrap()
        else {
            panic!("expected a commit");
        };
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD"]), commit);
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD^"]), base);
        assert_eq!(
            git(&d.worktree, &["log", "-1", "--format=%B"]),
            message.trim()
        );
        assert_eq!(
            git(&d.worktree, &["log", "-1", "--format=%an <%ae>"]),
            "Calin Test <test@calin.invalid>"
        );
        let files = git(&d.worktree, &["show", "--name-only", "--format=", "HEAD"]);
        assert_eq!(files, "README.md\nsrc/new.rs");
        assert!(!is_dirty(&d.worktree).unwrap());
        assert_eq!(commits_ahead(&d.worktree, &base, BRANCH).unwrap(), 1);
        assert_eq!(
            current_branch(&d.worktree).unwrap().as_deref(),
            Some(BRANCH)
        );
        // The clone's own branch did not move.
        assert_eq!(git(&d.clone, &["rev-parse", "main"]), base);
    }

    #[test]
    fn a_clean_tree_and_one_with_only_ignored_files_have_nothing_to_commit() {
        let d = delivery();
        let head = git(&d.worktree, &["rev-parse", "HEAD"]);
        assert_eq!(
            commit_all(&d.worktree, "nothing", LIMIT).unwrap(),
            CommitOutcome::NothingToCommit
        );

        write(&d.worktree, ".gitignore", "target/\n*.log\n");
        commit(&d.worktree, "ignore build output");
        let head_after = git(&d.worktree, &["rev-parse", "HEAD"]);
        assert_ne!(head, head_after);
        write(&d.worktree, "target/debug/app", "binary");
        write(&d.worktree, "run.log", "noise\n");
        assert!(!is_dirty(&d.worktree).unwrap());
        assert_eq!(
            commit_all(&d.worktree, "nothing", LIMIT).unwrap(),
            CommitOutcome::NothingToCommit
        );
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD"]), head_after);
        assert!(d.worktree.join("run.log").is_file());

        // A blank message is refused before anything is staged.
        write(&d.worktree, "a.txt", "a\n");
        assert!(matches!(
            commit_all(&d.worktree, " \n", LIMIT),
            Err(Error::Conflict(_))
        ));
        assert_eq!(git(&d.worktree, &["diff", "--cached", "--name-only"]), "");
    }

    #[test]
    fn a_file_git_cannot_read_fails_the_commit_with_gits_words() {
        use std::os::unix::fs::PermissionsExt as _;

        let d = delivery();
        let head = git(&d.worktree, &["rev-parse", "HEAD"]);
        write(&d.worktree, "secret.txt", "s\n");
        let secret = d.worktree.join("secret.txt");
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o000)).unwrap();
        let committed = commit_all(&d.worktree, "add the secret", LIMIT);
        std::fs::set_permissions(&secret, std::fs::Permissions::from_mode(0o644)).unwrap();

        match committed {
            Err(Error::Git { command, stderr }) => {
                assert_eq!(command, "git add --all");
                assert!(stderr.contains("Permission denied"), "{stderr}");
            }
            other => panic!("expected git's error, got {other:?}"),
        }
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD"]), head);
    }

    #[test]
    fn without_an_identity_the_commit_is_refused_and_says_how_to_set_one() {
        let d = delivery();
        let head = git(&d.worktree, &["rev-parse", "HEAD"]);
        write(&d.worktree, "a.txt", "a\n");
        let err = without_identity(|| commit_all(&d.worktree, "no author", LIMIT)).unwrap_err();
        let Error::Conflict(message) = &err else {
            panic!("expected a conflict, got {err:?}");
        };
        assert!(message.contains("user.name"), "{message}");
        assert!(message.contains("user.email"), "{message}");
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD"]), head);
        // Nothing to commit needs no identity.
        git(&d.worktree, &["reset", "-q"]);
        std::fs::remove_file(d.worktree.join("a.txt")).unwrap();
        assert_eq!(
            without_identity(|| commit_all(&d.worktree, "clean", LIMIT)).unwrap(),
            CommitOutcome::NothingToCommit
        );
    }

    #[test]
    fn a_branch_is_pushed_to_the_remote_and_tracks_it() {
        let d = delivery();
        let base = git(&d.worktree, &["rev-parse", "HEAD"]);
        assert!(has_remote(&d.worktree, "origin").unwrap());
        assert!(has_unpushed_commits(&d.worktree, "origin", BRANCH).unwrap());
        write(&d.worktree, "a.txt", "a\n");
        let first = commit(&d.worktree, "first");

        push_branch(&d.worktree, "origin", BRANCH, LIMIT).unwrap();
        assert_eq!(remote_tip(&d.remote, BRANCH), Some(first.clone()));
        assert_eq!(remote_tip(&d.remote, "main"), Some(base));
        assert_eq!(
            git(&d.worktree, &["rev-parse", "--abbrev-ref", "@{upstream}"]),
            format!("origin/{BRANCH}")
        );
        assert!(!has_unpushed_commits(&d.worktree, "origin", BRANCH).unwrap());

        // A second push carries the next commit; one with nothing new is fine.
        write(&d.worktree, "b.txt", "b\n");
        let second = commit(&d.worktree, "second");
        assert!(has_unpushed_commits(&d.worktree, "origin", BRANCH).unwrap());
        push_branch(&d.worktree, "origin", BRANCH, LIMIT).unwrap();
        push_branch(&d.worktree, "origin", BRANCH, LIMIT).unwrap();
        assert_eq!(remote_tip(&d.remote, BRANCH), Some(second));

        // A file URL works like a path.
        let url = format!("file://{}", d.remote.display());
        git(&d.worktree, &["remote", "add", "mirror", &url]);
        push_branch(&d.worktree, "mirror", BRANCH, LIMIT).unwrap();
    }

    #[test]
    fn the_default_branch_is_never_pushed() {
        let d = delivery();
        assert_eq!(
            default_branch(&d.worktree, "origin").unwrap().as_deref(),
            Some("main")
        );
        write(&d.clone, "a.txt", "a\n");
        commit(&d.clone, "on main");
        let before = remote_tip(&d.remote, "main");
        let err = push_branch(&d.clone, "origin", "main", LIMIT).unwrap_err();
        assert!(
            matches!(&err, Error::Conflict(m) if m.contains("default branch")),
            "{err:?}"
        );
        assert_eq!(remote_tip(&d.remote, "main"), before);

        // Without the remote's `HEAD`, the clone's own branch is the default,
        // seen from a worktree too.
        git(&d.clone, &["remote", "set-head", "origin", "--delete"]);
        assert_eq!(
            default_branch(&d.worktree, "origin").unwrap().as_deref(),
            Some("main")
        );
        assert!(matches!(
            push_branch(&d.worktree, "origin", "main", LIMIT),
            Err(Error::Conflict(_))
        ));
        assert_eq!(remote_tip(&d.remote, "main"), before);
        // The session's branch still goes through.
        push_branch(&d.worktree, "origin", BRANCH, LIMIT).unwrap();
        assert!(remote_tip(&d.remote, BRANCH).is_some());
    }

    #[test]
    fn a_push_the_remote_rejects_is_an_error_and_changes_nothing_there() {
        let d = delivery();
        write(&d.worktree, "a.txt", "a\n");
        commit(&d.worktree, "first");
        push_branch(&d.worktree, "origin", BRANCH, LIMIT).unwrap();

        // Someone else pushes to the same branch.
        let other = d.sandbox.path().join("other");
        git(
            d.sandbox.path(),
            &["clone", "-q", d.remote.to_str().unwrap(), "other"],
        );
        git(&other, &["switch", "-q", BRANCH]);
        write(&other, "theirs.txt", "theirs\n");
        let theirs = commit(&other, "theirs");
        git(&other, &["push", "-q", "origin", BRANCH]);

        write(&d.worktree, "b.txt", "b\n");
        let ours = commit(&d.worktree, "ours");
        let err = push_branch(&d.worktree, "origin", BRANCH, LIMIT).unwrap_err();
        let Error::Git { stderr, .. } = &err else {
            panic!("expected git's error, got {err:?}");
        };
        assert!(stderr.contains("rejected"), "{stderr}");
        assert_eq!(remote_tip(&d.remote, BRANCH), Some(theirs));
        // Nothing was rewritten locally either.
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD"]), ours);
    }

    #[test]
    fn a_missing_remote_or_branch_is_not_found() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        git(&repo, &["switch", "-q", "-c", BRANCH]);
        assert!(!has_remote(&repo, "origin").unwrap());
        assert!(!has_remote(&repo, "--upload-pack=x").unwrap());
        assert!(matches!(
            push_branch(&repo, "origin", BRANCH, LIMIT),
            Err(Error::NotFound(what)) if what.contains("origin")
        ));

        let d = delivery();
        assert!(matches!(
            push_branch(&d.worktree, "origin", "calin/never-made", LIMIT),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            push_branch(&d.worktree, "upstream", BRANCH, LIMIT),
            Err(Error::NotFound(_))
        ));
        for odd in ["", "-f", "+calin/x", "a:b"] {
            assert!(
                matches!(
                    push_branch(&d.worktree, "origin", odd, LIMIT),
                    Err(Error::Git { .. })
                ),
                "{odd:?}"
            );
        }
        assert_eq!(remote_tip(&d.remote, BRANCH), None);
        // A detached worktree has no branch to speak of.
        git(&d.worktree, &["checkout", "-q", "--detach"]);
        assert_eq!(current_branch(&d.worktree).unwrap(), None);
    }

    /// True once process `pid` is gone, or is a zombie nobody waits for
    /// any more; looks for up to two seconds.
    #[cfg(target_os = "linux")]
    fn gone(pid: &str) -> bool {
        let until = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => return true,
                // The state follows the command name, which ends with `)`.
                Ok(stat)
                    if stat
                        .rsplit(')')
                        .next()
                        .is_some_and(|rest| rest.trim_start().starts_with('Z')) =>
                {
                    return true;
                }
                Ok(_) => {}
            }
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A hook of the clone, which its worktrees share.
    fn hook(d: &Delivery, name: &str, script: &str) {
        let hooks = d.clone.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        write_executable(&hooks.join(name), script);
    }

    #[test]
    fn a_hook_that_reads_its_input_finds_none_and_does_not_hold_the_commit() {
        let d = delivery();
        // `cat` ends at once on a closed input; the commit goes through.
        hook(&d, "pre-commit", "#!/bin/sh\ncat\n");
        write(&d.worktree, "a.txt", "a\n");
        let started = Instant::now();
        let outcome = commit_all(&d.worktree, "with cat", LIMIT).unwrap();
        assert!(
            matches!(outcome, CommitOutcome::Committed(_)),
            "{outcome:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(5));

        // One that needs an answer gets none, and fails at once.
        hook(
            &d,
            "pre-commit",
            "#!/bin/sh\nread answer || { echo nobody answered >&2; exit 1; }\n",
        );
        write(&d.worktree, "b.txt", "b\n");
        let started = Instant::now();
        let err = commit_all(&d.worktree, "asks", LIMIT).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        let Error::Git { command, stderr } = &err else {
            panic!("expected git's error, got {err:?}");
        };
        assert!(stderr.contains("nobody answered"), "{stderr}");
        // The message is the user's text: errors do not repeat it.
        assert_eq!(
            command,
            "git -c user.useConfigOnly=true commit --quiet --message=<message>"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_commit_whose_hook_hangs_is_killed_with_the_hook_at_the_limit() {
        let d = delivery();
        let head = git(&d.worktree, &["rev-parse", "HEAD"]);
        let pid_file = d.sandbox.path().join("hook.pid");
        let script = format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
            pid_file.display()
        );
        hook(&d, "pre-commit", &script);
        write(&d.worktree, "a.txt", "a\n");
        let started = Instant::now();
        let err = commit_all(&d.worktree, "hangs", SHORT).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let Error::Git { command, stderr } = &err else {
            panic!("expected a timeout, got {err:?}");
        };
        assert!(command.contains("commit"), "{command}");
        assert_eq!(stderr, "timed out after 400 ms");
        assert!(err.to_string().contains("commit"), "{err}");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(gone(pid.trim()), "the hook {} still runs", pid.trim());
        assert_eq!(git(&d.worktree, &["rev-parse", "HEAD"]), head);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_push_to_a_remote_that_never_answers_is_killed_at_the_limit() {
        let d = delivery();
        write(&d.worktree, "a.txt", "a\n");
        commit(&d.worktree, "first");
        // The local transport runs this in place of `git-receive-pack`.
        let pid_file = d.sandbox.path().join("remote.pid");
        let silent = d.sandbox.path().join("silent-receive-pack");
        let script = format!(
            "#!/bin/sh\necho $$ > '{}'\nexec sleep 30\n",
            pid_file.display()
        );
        write_executable(&silent, &script);
        git(
            &d.clone,
            &[
                "config",
                "remote.origin.receivepack",
                silent.to_str().unwrap(),
            ],
        );

        let started = Instant::now();
        let err = push_branch(&d.worktree, "origin", BRANCH, SHORT).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
        let Error::Git { command, stderr } = &err else {
            panic!("expected a timeout, got {err:?}");
        };
        assert!(command.starts_with("git push"), "{command}");
        assert_eq!(stderr, "timed out after 400 ms");
        let pid = std::fs::read_to_string(&pid_file).unwrap();
        assert!(
            gone(pid.trim()),
            "the remote side {} still runs",
            pid.trim()
        );
        assert_eq!(remote_tip(&d.remote, BRANCH), None);
    }
}
