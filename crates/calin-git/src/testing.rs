//! Test support: throwaway repositories and a git that ignores this machine.

use std::cell::Cell;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;

thread_local! {
    /// Set while a test wants the library's git to find no identity at all.
    static NO_IDENTITY: Cell<bool> = const { Cell::new(false) };
}

/// Runs `work` with a git that has no name and no email to commit with, as
/// on a machine where nobody configured one. The library's calls are
/// blocking, so they happen on this thread.
pub(crate) fn without_identity<T>(work: impl FnOnce() -> T) -> T {
    NO_IDENTITY.set(true);
    let result = work();
    NO_IDENTITY.set(false);
    result
}

/// Cuts a git command off from the user's and the system's configuration and
/// fixes the identity and the clock. Applied to the commands the tests run
/// and, in test builds, to the ones the library runs.
pub(crate) fn isolate(cmd: &mut Command) {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    if NO_IDENTITY.get() {
        for name in [
            "GIT_AUTHOR_NAME",
            "GIT_AUTHOR_EMAIL",
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "EMAIL",
        ] {
            cmd.env_remove(name);
        }
        return;
    }
    cmd.env("GIT_AUTHOR_NAME", "Calin Test")
        .env("GIT_AUTHOR_EMAIL", "test@calin.invalid")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "Calin Test")
        .env("GIT_COMMITTER_EMAIL", "test@calin.invalid")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
}

/// Runs git in `dir`, panics when it fails, returns trimmed standard output.
pub(crate) fn git(dir: &Path, args: &[&str]) -> String {
    let output = try_git(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// Runs git in `dir` and reports whether it succeeded.
pub(crate) fn git_ok(dir: &Path, args: &[&str]) -> bool {
    try_git(dir, args).status.success()
}

fn try_git(dir: &Path, args: &[&str]) -> std::process::Output {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(["-c", "core.quotepath=off"])
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0");
    for name in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        cmd.env_remove(name);
    }
    isolate(&mut cmd);
    cmd.output().expect("git should start")
}

/// Writes `content` to `rel` under `dir`, creating parent directories.
pub(crate) fn write(dir: &Path, rel: &str, content: impl AsRef<[u8]>) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().expect("a file has a parent")).unwrap();
    fs::write(path, content).unwrap();
}

/// Stages everything and commits it. Returns the new commit.
pub(crate) fn commit_all(dir: &Path, message: &str) -> String {
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

/// Writes an executable without this process ever holding it open for
/// writing. Otherwise a test that forks at the wrong moment passes the open
/// file on to its child, and running the script fails with "text file busy".
pub(crate) fn write_executable(path: &Path, content: &str) {
    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$0" && chmod 755 "$0""#])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = writer.stdin.take().unwrap();
    stdin.write_all(content.as_bytes()).unwrap();
    drop(stdin);
    assert!(writer.wait().unwrap().success());
}

/// A temporary directory that holds the repositories of one test.
pub(crate) struct Sandbox {
    // Held for its destructor, which deletes the directory.
    _tmp: TempDir,
    root: PathBuf,
}

impl Sandbox {
    pub(crate) fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical, so paths compare equal to the ones the library returns.
        let root = fs::canonicalize(tmp.path()).unwrap();
        Self { _tmp: tmp, root }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.root
    }

    /// A new empty directory.
    pub(crate) fn dir(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    /// A repository on branch `main` with one commit that adds `README.md`.
    pub(crate) fn repo(&self, name: &str) -> PathBuf {
        let path = self.dir(name);
        git(&path, &["init", "-q", "-b", "main"]);
        write(&path, "README.md", "# readme\n");
        commit_all(&path, "initial");
        path
    }
}
