//! Cloning a repository from its URL.

use std::fs;
use std::path::Path;

use crate::cmd::{Git, no_ssh_prompt, work_dir};
use crate::error::{Error, Result};
use crate::path::absolutize;
use crate::repo::{CloneInfo, inspect_clone};

/// Clones `url` into `dest`, which must be absolute and must not exist yet.
/// Missing parent directories are created.
///
/// Nothing is ever asked on a terminal: a remote that wants a password is a
/// failure. When the clone fails, whatever it left at `dest` is removed.
///
/// [`Error::Conflict`] when something already is at `dest`, which is then
/// left alone. [`Error::Git`] with git's own words when the clone fails.
pub fn clone_repo(url: &str, dest: &Path) -> Result<CloneInfo> {
    let dest = absolutize(dest)?;
    if fs::symlink_metadata(&dest).is_ok() {
        return Err(Error::Conflict(format!(
            "{} already exists",
            dest.display()
        )));
    }
    let Some(parent) = dest.parent() else {
        return Err(Error::Conflict(format!(
            "{} cannot hold a clone",
            dest.display()
        )));
    };
    fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
    let parent = work_dir(parent)?;

    // `--`: a URL that starts with a hyphen is still a URL, not an option.
    let cloned = no_ssh_prompt(Git::write(&parent))
        .args(["clone", "--quiet", "--"])
        .arg(url)
        .arg(&dest)
        .run();
    match cloned.and_then(|_| inspect_clone(&dest)) {
        Ok(info) => Ok(info),
        Err(err) => {
            // Best effort: the failure to report is the clone's.
            let _ = fs::remove_dir_all(&dest);
            Err(match err {
                // git's words for a local path that is no repository.
                Error::NotARepository { reason, .. } => Error::Git {
                    command: format!("git clone {url}"),
                    stderr: reason,
                },
                other => other,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{Sandbox, git};

    #[test]
    fn clones_from_a_local_path_and_from_a_file_url() {
        let sandbox = Sandbox::new();
        let upstream = sandbox.repo("upstream");
        let head = git(&upstream, &["rev-parse", "HEAD"]);

        let dest = sandbox.path().join("clones/deep/api");
        let info = clone_repo(upstream.to_str().unwrap(), &dest).unwrap();
        assert_eq!(info.root, dest);
        assert_eq!(info.head.as_deref(), Some(head.as_str()));
        assert_eq!(info.default_branch.as_deref(), Some("main"));
        assert_eq!(info.remote_url.as_deref(), upstream.to_str());
        assert!(dest.join("README.md").is_file());

        let url = format!("file://{}", upstream.display());
        let second = sandbox.path().join("clones/web");
        let info = clone_repo(&url, &second).unwrap();
        assert_eq!(info.remote_url.as_deref(), Some(url.as_str()));
    }

    #[test]
    fn a_failed_clone_says_why_and_leaves_nothing_behind() {
        let sandbox = Sandbox::new();
        let missing = sandbox.path().join("no-such-repo");
        let dest = sandbox.path().join("clones/api");
        let err = clone_repo(missing.to_str().unwrap(), &dest).unwrap_err();
        let Error::Git { stderr, .. } = &err else {
            panic!("expected git's error, got {err:?}");
        };
        assert!(stderr.contains("does not exist"), "{stderr}");
        assert!(!dest.exists());
        // The parent was created and stays: it may hold other clones.
        assert!(dest.parent().unwrap().is_dir());

        // A directory that is not a repository fails the same way.
        let plain = sandbox.dir("plain");
        let err = clone_repo(plain.to_str().unwrap(), &dest).unwrap_err();
        assert!(matches!(err, Error::Git { .. }), "{err:?}");
        assert!(!dest.exists());

        // A URL that looks like an option is not one.
        let err = clone_repo("--upload-pack=touch pwned", &dest).unwrap_err();
        assert!(matches!(err, Error::Git { .. }), "{err:?}");
        assert!(!sandbox.path().join("clones/pwned").exists());
    }

    #[test]
    fn what_is_already_there_is_left_alone() {
        let sandbox = Sandbox::new();
        let upstream = sandbox.repo("upstream");
        let dest = sandbox.dir("taken");
        std::fs::write(dest.join("mine.txt"), "keep").unwrap();
        let err = clone_repo(upstream.to_str().unwrap(), &dest).unwrap_err();
        assert!(matches!(err, Error::Conflict(_)), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(dest.join("mine.txt")).unwrap(),
            "keep"
        );

        let relative = clone_repo(upstream.to_str().unwrap(), Path::new("relative/dest"));
        assert!(relative.is_err());
    }
}
