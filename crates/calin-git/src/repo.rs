//! Looking at a clone: is it one, and where does it stand.

use std::fs;
use std::path::{Path, PathBuf};

use crate::cmd::{Git, resolve_commit, work_dir};
use crate::error::{Error, Result};

/// What Calin records about a clone when it is registered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CloneInfo {
    /// The root of the work tree, canonical.
    pub root: PathBuf,
    /// The commit `HEAD` is at. Absent in a repository without commits.
    pub head: Option<String>,
    /// The checked-out branch. Absent when `HEAD` is detached.
    pub branch: Option<String>,
    /// The branch `origin/HEAD` points to, else the checked-out branch.
    pub default_branch: Option<String>,
    /// The URL of the `origin` remote, as configured.
    pub remote_url: Option<String>,
}

/// Checks that `path` is the root of a git work tree and describes it.
///
/// A subdirectory of a work tree, a bare repository, the inside of a `.git`
/// directory and a path that is not a directory are all
/// [`Error::NotARepository`]. A linked worktree is accepted: it is a root too.
pub fn inspect_clone(path: &Path) -> Result<CloneInfo> {
    let root = work_tree_root(path)?;
    let head = resolve_commit(&root, "HEAD")?;
    let branch = checked_out_branch(&root)?;
    let remote_head = remote_head_branch(&root, "origin")?;
    let remote_url = remote_url(&root, "origin")?;
    Ok(CloneInfo {
        root,
        head,
        default_branch: remote_head.or_else(|| branch.clone()),
        branch,
        remote_url,
    })
}

/// `path`, canonical, when it is the root of a git work tree.
fn work_tree_root(path: &Path) -> Result<PathBuf> {
    let not_a_repo = |reason: String| Error::NotARepository {
        path: path.to_path_buf(),
        reason,
    };
    let root = fs::canonicalize(path).map_err(|e| not_a_repo(e.to_string()))?;
    if !root.is_dir() {
        return Err(not_a_repo("not a directory".into()));
    }

    // Parent directories are searched on purpose, to say why a subdirectory is refused.
    let facts = Git::read(&root)
        .discover_parents()
        .args([
            "rev-parse",
            "--is-bare-repository",
            "--is-inside-work-tree",
            "--show-prefix",
        ])
        .text()?;
    let mut facts = facts.splitn(3, '\n');
    let bare = facts.next() == Some("true");
    let in_work_tree = facts.next() == Some("true");
    // The path from the root of the work tree down to here: empty at the root.
    let prefix = facts.next().unwrap_or_default();
    if bare {
        return Err(not_a_repo("it is a bare repository".into()));
    }
    if !in_work_tree {
        return Err(not_a_repo("it is inside a git directory".into()));
    }
    if !prefix.is_empty() {
        return Err(not_a_repo(
            "it is a subdirectory of a work tree, not its root".into(),
        ));
    }
    Ok(root)
}

/// The branch checked out in the work tree `dir`. `None` when `HEAD` is detached.
pub(crate) fn checked_out_branch(dir: &Path) -> Result<Option<String>> {
    Ok(Git::read(dir)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .probe()?
        .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_owned)))
}

/// The branch `remote`'s `HEAD` pointed to when it was last fetched.
pub(crate) fn remote_head_branch(dir: &Path, remote: &str) -> Result<Option<String>> {
    let prefix = format!("refs/remotes/{remote}/");
    Ok(Git::read(dir)
        .args(["symbolic-ref", "--quiet", &format!("{prefix}HEAD")])
        .probe()?
        .and_then(|name| name.strip_prefix(&prefix).map(str::to_owned)))
}

/// The URL of `remote`, as configured. `None` when there is no such remote.
pub(crate) fn remote_url(dir: &Path, remote: &str) -> Result<Option<String>> {
    Git::read(dir)
        .args(["config", "--get", &format!("remote.{remote}.url")])
        .probe()
}

/// The commit `HEAD` of `dir` is at: the base for a repo edited in place.
///
/// `dir` must be the root of a work tree. A repository without commits is an
/// [`Error::Conflict`], since nothing can be measured against it yet.
pub fn head_commit(dir: &Path) -> Result<String> {
    let dir = work_dir(dir)?;
    resolve_commit(&dir, "HEAD")?
        .ok_or_else(|| Error::Conflict(format!("{} has no commits yet", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, commit_all, git, write};

    fn refused(path: &Path) -> String {
        match inspect_clone(path) {
            Err(Error::NotARepository { reason, .. }) => reason,
            other => panic!("{} should be refused, got {other:?}", path.display()),
        }
    }

    #[test]
    fn accepts_the_root_of_a_clone() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let info = inspect_clone(&repo).unwrap();
        assert_eq!(info.root, repo);
        assert_eq!(info.head, Some(git(&repo, &["rev-parse", "HEAD"])));
        assert_eq!(info.branch.as_deref(), Some("main"));
        // No `origin`, so the checked-out branch stands in.
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert_eq!(info.remote_url, None);
        assert_eq!(head_commit(&repo).unwrap(), info.head.unwrap());
    }

    #[test]
    fn refuses_what_is_not_a_work_tree_root() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "src/lib.rs", "");
        assert!(refused(&repo.join("src")).contains("subdirectory"));
        assert!(refused(&repo.join(".git")).contains("git directory"));
        assert!(refused(&repo.join("README.md")).contains("not a directory"));
        refused(&repo.join("missing"));

        let plain = sandbox.dir("plain");
        assert!(refused(&plain).contains("no git repository"));

        let bare = sandbox.dir("bare.git");
        git(&bare, &["init", "-q", "--bare"]);
        assert!(refused(&bare).contains("bare"));
    }

    #[test]
    fn default_branch_comes_from_origin_head() {
        let sandbox = Sandbox::new();
        let upstream = sandbox.repo("upstream");
        let clone = sandbox.path().join("clone");
        git(
            sandbox.path(),
            &["clone", "-q", upstream.to_str().unwrap(), "clone"],
        );
        git(&clone, &["switch", "-q", "-c", "feature"]);

        let info = inspect_clone(&clone).unwrap();
        assert_eq!(info.branch.as_deref(), Some("feature"));
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert_eq!(info.remote_url.as_deref(), upstream.to_str());

        // A remote whose HEAD was never fetched falls back to the current branch.
        git(&clone, &["remote", "set-head", "origin", "--delete"]);
        let info = inspect_clone(&clone).unwrap();
        assert_eq!(info.default_branch.as_deref(), Some("feature"));
        assert!(info.remote_url.is_some());
    }

    #[test]
    fn detached_head_has_no_branch() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        let head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", "--detach"]);
        let info = inspect_clone(&repo).unwrap();
        assert_eq!(info.head, Some(head));
        assert_eq!(info.branch, None);
        assert_eq!(info.default_branch, None);
    }

    #[test]
    fn a_repository_without_commits_has_no_head() {
        let sandbox = Sandbox::new();
        let repo = sandbox.dir("empty");
        git(&repo, &["init", "-q", "-b", "trunk"]);
        let info = inspect_clone(&repo).unwrap();
        assert_eq!(info.head, None);
        assert_eq!(info.branch.as_deref(), Some("trunk"));
        assert!(matches!(head_commit(&repo), Err(Error::Conflict(_))));
    }

    #[test]
    fn head_commit_follows_new_commits_and_needs_a_root() {
        let sandbox = Sandbox::new();
        let repo = sandbox.repo("repo");
        write(&repo, "src/a.txt", "a\n");
        let second = commit_all(&repo, "second");
        assert_eq!(head_commit(&repo).unwrap(), second);
        // A subdirectory must not answer for the repository around it.
        assert!(matches!(
            head_commit(&repo.join("src")),
            Err(Error::NotARepository { .. })
        ));
        assert!(matches!(
            head_commit(&repo.join("missing")),
            Err(Error::NotFound(_))
        ));
    }
}
