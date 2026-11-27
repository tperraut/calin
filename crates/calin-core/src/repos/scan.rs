//! Finding the clones on this machine: a bounded walk of the scan roots.

use std::path::{Path, PathBuf};

use super::url;

/// Levels below a scan root that are looked at: `root/a/b/c` is found,
/// anything deeper is not. Deep enough for `~/code/<host>/<owner>/<repo>`.
pub(crate) const MAX_DEPTH: usize = 3;

/// Directories that hold dependencies or build output, never a clone the
/// user works in.
const SKIPPED: [&str; 2] = ["node_modules", "target"];

/// A clone found under a scan root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Found {
    /// The root of the work tree, canonical.
    pub path: String,
    /// The folder's name.
    pub name: String,
    /// True when it sits directly under a scan root.
    pub direct: bool,
    pub default_branch: Option<String>,
    pub remote_url: Option<String>,
    /// [`url::key`] of the remote, which is what a declared URL is matched to.
    pub key: Option<String>,
}

/// Blocking. Every clone under `roots`, in the order of the roots and by
/// path within one. A root that does not exist holds nothing.
pub(crate) fn scan(roots: &[PathBuf]) -> Vec<Found> {
    let mut found: Vec<Found> = Vec::new();
    for root in roots {
        let mut here = Vec::new();
        visit(root, 0, &mut here);
        // Roots can overlap; a clone is listed where it was first found.
        here.retain(|clone: &Found| !found.iter().any(|known| known.path == clone.path));
        found.extend(here);
    }
    found
}

fn visit(dir: &Path, depth: usize, found: &mut Vec<Found>) {
    // `.git` is a directory in a clone and a file in a linked worktree.
    if std::fs::symlink_metadata(dir.join(".git")).is_ok() {
        // A work tree is never searched for more: what is inside it belongs
        // to it (submodules, vendored clones, worktrees of a session).
        if let Some(clone) = describe(dir, depth) {
            found.push(clone);
        }
        return;
    }
    if depth == MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            // The type of the entry itself: a symlink is not followed, so a
            // link cannot lead the walk out of the root or around in circles.
            entry.file_type().is_ok_and(|kind| kind.is_dir())
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with('.') && !SKIPPED.contains(&name))
        })
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        visit(&child, depth + 1, found);
    }
}

fn describe(dir: &Path, depth: usize) -> Option<Found> {
    // A `.git` that git itself does not accept is not a clone.
    let clone = calin_git::inspect_clone(dir).ok()?;
    let path = clone.root.to_str()?.to_owned();
    let name = clone.root.file_name()?.to_str()?.to_owned();
    Some(Found {
        path,
        name,
        direct: depth == 1,
        default_branch: clone.default_branch,
        key: clone.remote_url.as_deref().map(url::key),
        remote_url: clone.remote_url,
    })
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::testing::{git, git_repo};

    fn names(found: &[Found]) -> Vec<&str> {
        found.iter().map(|clone| clone.name.as_str()).collect()
    }

    #[test]
    fn clones_are_found_down_to_the_depth_bound_and_no_further() {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let one = git_repo(&root, "one");
        git_repo(&root.join("a"), "two");
        git_repo(&root.join("a/b"), "three");
        git_repo(&root.join("a/b/c"), "four");
        git(
            &one,
            &["remote", "add", "origin", "git@github.com:you/One.git"],
        );

        let found = scan(std::slice::from_ref(&root));
        assert_eq!(names(&found), ["three", "two", "one"]);
        let one = found.iter().find(|clone| clone.name == "one").unwrap();
        assert_eq!(
            *one,
            Found {
                path: root.join("one").to_str().unwrap().to_owned(),
                name: "one".to_owned(),
                direct: true,
                default_branch: Some("main".to_owned()),
                remote_url: Some("git@github.com:you/One.git".to_owned()),
                key: Some("github.com/you/One".to_owned()),
            }
        );
        let two = found.iter().find(|clone| clone.name == "two").unwrap();
        assert!(!two.direct);
        assert_eq!(two.key, None);

        // A root that is itself a clone is that clone, and nothing below it.
        let found = scan(&[root.join("one")]);
        assert_eq!(names(&found), ["one"]);
        assert!(!found[0].direct);
        assert!(scan(&[root.join("missing")]).is_empty());
        assert!(scan(&[]).is_empty());
    }

    #[test]
    fn hidden_dependency_and_build_directories_are_skipped() {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        git_repo(&root, "kept");
        for skipped in [".hidden", "node_modules", "target"] {
            git_repo(&root.join(skipped), "inside");
        }
        // Skipped by name at every level, not only at the top.
        git_repo(&root.join("group/node_modules"), "dep");
        git_repo(&root.join("group"), "target");
        assert_eq!(names(&scan(&[root])), ["kept"]);
    }

    #[test]
    fn nothing_inside_a_work_tree_is_searched() {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let outer = git_repo(&root, "outer");
        git_repo(&outer, "vendored");
        git_repo(&outer.join("deep"), "nested");
        // A linked worktree has a `.git` file; it is a work tree all the same.
        git(
            &outer,
            &["worktree", "add", "-q", "../linked", "-b", "side"],
        );
        git_repo(&root.join("linked"), "below");
        assert_eq!(names(&scan(&[root])), ["linked", "outer"]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed() {
        let dir = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let real = git_repo(&root, "real");
        let outside = git_repo(elsewhere.path(), "outside");
        // To a clone outside the root, to one inside it, and back up to the root.
        std::os::unix::fs::symlink(&outside, root.join("link-out")).unwrap();
        std::os::unix::fs::symlink(&real, root.join("link-in")).unwrap();
        std::fs::create_dir(root.join("group")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("group/loop")).unwrap();
        assert_eq!(names(&scan(&[root])), ["real"]);
    }

    #[test]
    fn overlapping_roots_list_a_clone_once_where_it_was_first_found() {
        let dir = TempDir::new().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        git_repo(&root.join("group"), "api");
        let found = scan(&[root.join("group"), root.clone()]);
        assert_eq!(names(&found), ["api"]);
        assert!(found[0].direct);
        let found = scan(&[root.clone(), root.join("group")]);
        assert_eq!(names(&found), ["api"]);
        assert!(!found[0].direct);
    }
}
