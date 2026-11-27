//! Test support: throwaway git repositories, made with a git that ignores
//! this machine's configuration.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs git in `dir`, panics when it fails, returns trimmed standard output.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Calin Test")
        .env("GIT_AUTHOR_EMAIL", "test@calin.invalid")
        .env("GIT_COMMITTER_NAME", "Calin Test")
        .env("GIT_COMMITTER_EMAIL", "test@calin.invalid")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .expect("git should start");
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Writes `content` to `rel` under `dir`, creating parent directories.
pub(crate) fn write_file(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    std::fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap();
    std::fs::write(path, content).unwrap();
}

/// A repository `parent/name` on branch `main` with one commit that adds
/// `README.md`. Returns its canonical path, as the library reports paths.
pub(crate) fn git_repo(parent: &Path, name: &str) -> PathBuf {
    let path = parent.join(name);
    std::fs::create_dir_all(&path).unwrap();
    git(&path, &["init", "-q", "-b", "main"]);
    write_file(&path, "README.md", "# readme\n");
    git(&path, &["add", "-A"]);
    git(&path, &["commit", "-q", "-m", "initial"]);
    std::fs::canonicalize(path).unwrap()
}
