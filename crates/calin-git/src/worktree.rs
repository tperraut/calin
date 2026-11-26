//! Worktrees of a clone: one per session, on the session's branch.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::cmd::{Git, require_commit, resolve_commit, work_dir};
use crate::error::{Error, Result};
use crate::path::absolutize;

/// How [`create_worktree`] got to a worktree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorktreeOutcome {
    /// A new branch and a new worktree.
    Created,
    /// The branch existed and no worktree used it: a new worktree on that branch.
    Resumed,
    /// The path already was a worktree on that branch; nothing was changed.
    AlreadyPresent,
}

/// A worktree ready for a session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// The root of the worktree, canonical.
    pub path: PathBuf,
    pub branch: String,
    /// The commit the session's changes are measured against. For a new branch
    /// this is the start point. For an existing branch it is the merge-base of
    /// the branch and the start point (the start point itself when they share
    /// no history).
    pub base_ref: String,
    pub outcome: WorktreeOutcome,
}

/// Makes `path` a worktree of `clone` on `branch`.
///
/// - A new branch is created at `start_point` (any revision; the clone's
///   current `HEAD` when absent).
/// - A branch that already exists and is not checked out anywhere is reused as
///   it is; this is how a session resumes after its worktree was removed. The
///   branch is never moved or reset.
/// - Calling this again for a path that already is the worktree of that branch
///   changes nothing and succeeds.
///
/// `path` must be absolute. Missing parent directories are created.
///
/// [`Error::Conflict`] when the branch is checked out elsewhere (the clone
/// itself included), when `path` is a worktree of another branch or a
/// non-empty directory, or when the clone has no commits. [`Error::NotFound`]
/// when `start_point` names no commit.
///
/// Registrations of worktrees whose directory was deleted by hand are pruned
/// when they hold `path` or `branch`; their branches stay.
pub fn create_worktree(
    clone: &Path,
    path: &Path,
    branch: &str,
    start_point: Option<&str>,
) -> Result<Worktree> {
    let clone = work_dir(clone)?;
    let target = absolutize(path)?;
    let start = match start_point {
        Some(rev) => require_commit(&clone, rev)?,
        None => resolve_commit(&clone, "HEAD")?
            .ok_or_else(|| Error::Conflict(format!("{} has no commits yet", clone.display())))?,
    };
    let full_ref = format!("refs/heads/{branch}");
    check_branch_name(&clone, branch, &full_ref)?;

    let mut worktrees = list(&clone)?;
    let stale =
        |w: &Registered| w.prunable && (w.path == target || w.branch.as_deref() == Some(&full_ref));
    if worktrees.iter().any(stale) {
        // A deleted directory still holds its path and its branch until pruned.
        prune(&clone)?;
        worktrees = list(&clone)?;
    }

    if let Some(at_path) = worktrees.iter().find(|w| w.path == target) {
        if at_path.main {
            return Err(Error::Conflict(format!(
                "{} is the clone itself, not a worktree of it",
                target.display()
            )));
        }
        if at_path.branch.as_deref() != Some(&full_ref) {
            return Err(Error::Conflict(format!(
                "{} is already a worktree, on {}",
                target.display(),
                at_path.branch.as_deref().unwrap_or("a detached HEAD")
            )));
        }
        return Ok(Worktree {
            base_ref: merge_base(&clone, &full_ref, &start)?,
            path: target,
            branch: branch.to_owned(),
            outcome: WorktreeOutcome::AlreadyPresent,
        });
    }
    if let Some(user) = worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some(&full_ref))
    {
        return Err(Error::Conflict(format!(
            "branch {branch} is checked out at {}",
            user.path.display()
        )));
    }
    if is_occupied(&target)? {
        return Err(Error::Conflict(format!(
            "{} already exists and is not an empty directory",
            target.display()
        )));
    }

    let add = Git::write(&clone).args(["worktree", "add"]);
    let (add, base_ref, outcome) = if resolve_commit(&clone, &full_ref)?.is_some() {
        // The short name, not the full ref: git only attaches HEAD to a name
        // it finds under refs/heads.
        (
            add.arg(&target).arg(branch),
            merge_base(&clone, &full_ref, &start)?,
            WorktreeOutcome::Resumed,
        )
    } else {
        // Starting from a commit id rather than a branch name also keeps git
        // from setting an upstream on the session's branch.
        (
            add.args(["-b", branch]).arg(&target).arg(&start),
            start,
            WorktreeOutcome::Created,
        )
    };
    add.run()?;

    Ok(Worktree {
        path: fs::canonicalize(&target).map_err(|e| Error::io(&target, e))?,
        branch: branch.to_owned(),
        base_ref,
        outcome,
    })
}

/// Removes the worktree at `path` from `clone`: its directory, with whatever
/// uncommitted work is in it, and its registration. The branch stays.
///
/// A worktree that is already gone is not an error. [`Error::Conflict`] when
/// `path` is the clone itself, or a directory that is not a worktree of this
/// clone: nothing is deleted then.
pub fn remove_worktree(clone: &Path, path: &Path) -> Result<()> {
    let clone = work_dir(clone)?;
    let target = absolutize(path)?;
    let worktrees = list(&clone)?;
    let Some(registered) = worktrees.iter().find(|w| w.path == target) else {
        return match fs::symlink_metadata(&target) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(Error::io(&target, e)),
            Ok(_) => Err(Error::Conflict(format!(
                "{} exists but is not a worktree of {}",
                target.display(),
                clone.display()
            ))),
        };
    };
    if registered.main {
        return Err(Error::Conflict(format!(
            "{} is the clone itself, not a worktree of it",
            target.display()
        )));
    }
    // When only the registration is left, pruning is all there is to do.
    if !registered.prunable {
        Git::write(&clone)
            .args(["worktree", "remove", "--force"])
            .arg(&target)
            .run()?;
    }
    prune(&clone)
}

fn prune(clone: &Path) -> Result<()> {
    Git::write(clone)
        .args(["worktree", "prune"])
        .run()
        .map(drop)
}

/// The best common ancestor, or `fallback` when the histories are unrelated.
fn merge_base(clone: &Path, branch_ref: &str, start: &str) -> Result<String> {
    let found = Git::read(clone)
        .args(["merge-base", branch_ref, start])
        .probe()?;
    Ok(found.unwrap_or_else(|| start.to_owned()))
}

fn check_branch_name(clone: &Path, branch: &str, full_ref: &str) -> Result<()> {
    let check = Git::read(clone)
        .args(["check-ref-format", full_ref])
        .exec()?;
    // A leading hyphen is a valid ref but would be read as an option.
    if check.status.success() && !branch.starts_with('-') {
        Ok(())
    } else {
        Err(Error::Git {
            command: format!("git check-ref-format {full_ref}"),
            stderr: format!("{branch:?} is not a valid branch name"),
        })
    }
}

/// True when something other than an empty directory is at `path`.
fn is_occupied(path: &Path) -> Result<bool> {
    match fs::read_dir(path) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) if e.kind() == ErrorKind::NotADirectory => Ok(true),
        Err(e) => Err(Error::io(path, e)),
    }
}

/// One entry of `git worktree list`.
struct Registered {
    /// Canonical when the directory exists.
    path: PathBuf,
    /// The full ref, `refs/heads/...`. Absent when detached.
    branch: Option<String>,
    /// The clone's own work tree, which git lists first.
    main: bool,
    /// The directory is gone and only the registration is left.
    prunable: bool,
}

fn list(clone: &Path) -> Result<Vec<Registered>> {
    let out = Git::read(clone)
        .args(["worktree", "list", "--porcelain", "-z"])
        .run()?;
    let mut worktrees: Vec<Registered> = Vec::new();
    // Attributes are NUL-terminated and a record starts at each `worktree` line.
    for field in out.split(|b| *b == 0) {
        let field = String::from_utf8_lossy(field);
        let (key, value) = field.split_once(' ').unwrap_or((&field, ""));
        if key == "worktree" {
            let path = PathBuf::from(value);
            worktrees.push(Registered {
                path: fs::canonicalize(&path).unwrap_or(path),
                branch: None,
                main: worktrees.is_empty(),
                prunable: false,
            });
        } else if let Some(current) = worktrees.last_mut() {
            match key {
                "branch" => current.branch = Some(value.to_owned()),
                "prunable" => current.prunable = true,
                _ => {}
            }
        }
    }
    Ok(worktrees)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, commit_all, git, git_ok, write};

    fn branch_exists(repo: &Path, branch: &str) -> bool {
        git_ok(
            repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
    }

    #[test]
    fn creates_a_worktree_on_a_new_branch() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let path = sandbox.path().join("sessions/s1/repo");

        let wt = create_worktree(&repo, &path, "calin/first-abc123", None).unwrap();
        assert_eq!(wt.path, path);
        assert_eq!(wt.branch, "calin/first-abc123");
        assert_eq!(wt.base_ref, head);
        assert_eq!(wt.outcome, WorktreeOutcome::Created);

        assert_eq!(
            git(&path, &["symbolic-ref", "--short", "HEAD"]),
            "calin/first-abc123"
        );
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), head);
        assert!(path.join("README.md").is_file());
        // The session's branch must not track anything by accident.
        assert!(!git_ok(
            &path,
            &["rev-parse", "--verify", "--quiet", "@{upstream}"]
        ));
        // The clone itself is untouched.
        assert_eq!(git(&repo, &["symbolic-ref", "--short", "HEAD"]), "main");
        assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    }

    #[test]
    fn starts_from_an_explicit_start_point() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let first = git(&repo, &["rev-parse", "HEAD"]);
        write(&repo, "later.txt", "x\n");
        commit_all(&repo, "later");
        git(&repo, &["tag", "v1", &first]);

        let path = sandbox.path().join("wt");
        let wt = create_worktree(&repo, &path, "calin/old", Some("v1")).unwrap();
        assert_eq!(wt.base_ref, first);
        assert!(!path.join("later.txt").exists());

        assert!(matches!(
            create_worktree(&repo, &sandbox.path().join("wt2"), "calin/x", Some("nope")),
            Err(Error::NotFound(_))
        ));
        assert!(matches!(
            create_worktree(
                &repo,
                &sandbox.path().join("wt2"),
                "calin/x",
                Some("--force")
            ),
            Err(Error::NotFound(_))
        ));
        assert!(!branch_exists(&repo, "calin/x"));
    }

    #[test]
    fn creating_twice_is_idempotent() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let path = sandbox.path().join("wt");
        let first = create_worktree(&repo, &path, "calin/s", None).unwrap();
        write(&path, "wip.txt", "uncommitted\n");

        let again = create_worktree(&repo, &path, "calin/s", None).unwrap();
        assert_eq!(again.outcome, WorktreeOutcome::AlreadyPresent);
        assert_eq!(again.path, first.path);
        assert_eq!(again.base_ref, first.base_ref);
        assert_eq!(
            fs::read_to_string(path.join("wip.txt")).unwrap(),
            "uncommitted\n"
        );

        // The same path on another branch is a different request.
        assert!(matches!(
            create_worktree(&repo, &path, "calin/other", None),
            Err(Error::Conflict(_))
        ));
        assert!(!branch_exists(&repo, "calin/other"));
    }

    #[test]
    fn remove_keeps_the_branch_and_resume_reattaches_it() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let path = sandbox.path().join("wt");
        let created = create_worktree(&repo, &path, "calin/s", None).unwrap();
        write(&path, "work.txt", "done\n");
        let work = commit_all(&path, "session work");
        write(&path, "scratch.txt", "never committed\n");

        remove_worktree(&repo, &path).unwrap();
        assert!(!path.exists());
        assert!(branch_exists(&repo, "calin/s"));
        assert_eq!(git(&repo, &["rev-parse", "calin/s"]), work);
        // Removing again, or removing something that never existed, is fine.
        remove_worktree(&repo, &path).unwrap();
        remove_worktree(&repo, &sandbox.path().join("never")).unwrap();

        // Meanwhile the clone moves on; the base stays where the branch forked.
        write(&repo, "main.txt", "m\n");
        commit_all(&repo, "main moves");

        let resumed = create_worktree(&repo, &path, "calin/s", None).unwrap();
        assert_eq!(resumed.outcome, WorktreeOutcome::Resumed);
        assert_eq!(resumed.base_ref, created.base_ref);
        assert_eq!(git(&path, &["rev-parse", "HEAD"]), work);
        assert_eq!(git(&path, &["symbolic-ref", "--short", "HEAD"]), "calin/s");
        assert!(path.join("work.txt").is_file());
        assert!(!path.join("scratch.txt").exists());
    }

    #[test]
    fn resumes_after_the_directory_was_deleted_by_hand() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let path = sandbox.path().join("wt");
        create_worktree(&repo, &path, "calin/s", None).unwrap();
        fs::remove_dir_all(&path).unwrap();

        // Git still has the registration; at the same path or another one.
        let elsewhere = sandbox.path().join("wt-2");
        let resumed = create_worktree(&repo, &elsewhere, "calin/s", None).unwrap();
        assert_eq!(resumed.outcome, WorktreeOutcome::Resumed);

        fs::remove_dir_all(&elsewhere).unwrap();
        let resumed = create_worktree(&repo, &elsewhere, "calin/s", None).unwrap();
        assert_eq!(resumed.outcome, WorktreeOutcome::Resumed);

        // And removing a worktree whose directory is gone clears the registration.
        fs::remove_dir_all(&elsewhere).unwrap();
        remove_worktree(&repo, &elsewhere).unwrap();
        assert!(!git(&repo, &["worktree", "list"]).contains("wt-2"));
        assert!(branch_exists(&repo, "calin/s"));
    }

    #[test]
    fn two_worktrees_of_one_clone_coexist() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let a = sandbox.path().join("a");
        let b = sandbox.path().join("b");
        create_worktree(&repo, &a, "calin/a", None).unwrap();
        create_worktree(&repo, &b, "calin/b", None).unwrap();

        write(&a, "a.txt", "a\n");
        commit_all(&a, "in a");
        write(&b, "b.txt", "b\n");
        assert!(!b.join("a.txt").exists());
        assert_eq!(git(&b, &["symbolic-ref", "--short", "HEAD"]), "calin/b");

        // A branch in use cannot be given to a second worktree.
        assert!(matches!(
            create_worktree(&repo, &sandbox.path().join("c"), "calin/a", None),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            create_worktree(&repo, &sandbox.path().join("c"), "main", None),
            Err(Error::Conflict(_))
        ));

        remove_worktree(&repo, &a).unwrap();
        assert!(!a.exists());
        assert!(b.join("b.txt").is_file());
        assert!(branch_exists(&repo, "calin/a"));
    }

    #[test]
    fn refuses_to_take_over_or_delete_what_is_not_a_worktree() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let occupied = sandbox.dir("occupied");
        write(&occupied, "keep.txt", "keep\n");

        assert!(matches!(
            create_worktree(&repo, &occupied, "calin/s", None),
            Err(Error::Conflict(_))
        ));
        assert!(!branch_exists(&repo, "calin/s"));
        assert!(matches!(
            remove_worktree(&repo, &occupied),
            Err(Error::Conflict(_))
        ));
        assert!(occupied.join("keep.txt").is_file());

        // The clone is never its own worktree, and is never removed.
        assert!(matches!(
            create_worktree(&repo, &repo, "main", None),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            remove_worktree(&repo, &repo),
            Err(Error::Conflict(_))
        ));
        assert!(repo.join("README.md").is_file());

        // An empty directory is free to use.
        let empty = sandbox.dir("empty");
        create_worktree(&repo, &empty, "calin/s", None).unwrap();
    }

    #[test]
    fn rejects_bad_input() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let path = sandbox.path().join("wt");
        for branch in ["", "a..b", "-x", "with space", "x.lock"] {
            assert!(
                matches!(
                    create_worktree(&repo, &path, branch, None),
                    Err(Error::Git { .. })
                ),
                "{branch:?}"
            );
        }
        assert!(matches!(
            create_worktree(&repo, Path::new("relative/wt"), "calin/s", None),
            Err(Error::InvalidPath { .. })
        ));
        assert!(!path.exists());

        let empty = sandbox.dir("empty");
        git(&empty, &["init", "-q"]);
        assert!(matches!(
            create_worktree(&empty, &path, "calin/s", None),
            Err(Error::Conflict(_))
        ));
        assert!(matches!(
            create_worktree(&sandbox.dir("plain"), &path, "calin/s", None),
            Err(Error::NotARepository { .. })
        ));
    }
}
