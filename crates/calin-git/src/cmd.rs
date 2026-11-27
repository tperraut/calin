//! The one place that starts `git`.

use std::ffi::OsStr;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;

use crate::error::{Error, Result};

/// Set by git for hooks and `rebase -x`. Inherited, they would point every
/// command at the repository the server was started from.
pub(crate) const INHERITED: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
    "GIT_OBJECT_DIRECTORY",
    "GIT_NAMESPACE",
    "GIT_PREFIX",
];

/// Canonical form of a directory git is about to run in.
pub(crate) fn work_dir(dir: &Path) -> Result<PathBuf> {
    let real = fs::canonicalize(dir).map_err(|e| Error::io(dir, e))?;
    if real.is_dir() {
        Ok(real)
    } else {
        Err(Error::NotARepository {
            path: real,
            reason: "not a directory".into(),
        })
    }
}

/// One `git` invocation, run in `dir`.
pub(crate) struct Git {
    cmd: Command,
    dir: PathBuf,
    command: String,
    stdin: Option<Vec<u8>>,
}

impl Git {
    /// A command that only reads. It takes no optional locks, so it never
    /// makes an agent's own git fail on `index.lock` in the same work tree.
    pub(crate) fn read(dir: &Path) -> Self {
        let mut git = Self::new(dir);
        git.cmd.env("GIT_OPTIONAL_LOCKS", "0");
        git
    }

    /// A command that changes the repository.
    pub(crate) fn write(dir: &Path) -> Self {
        Self::new(dir)
    }

    /// `dir` must be canonical (see [`work_dir`]).
    fn new(dir: &Path) -> Self {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .args(["--no-pager", "-c", "core.quotepath=off"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .stdin(Stdio::null());
        for name in INHERITED {
            cmd.env_remove(name);
        }
        // Without a ceiling, a directory that stopped being a repository would
        // silently answer for whatever repository happens to contain it.
        if let Some(parent) = dir.parent() {
            cmd.env("GIT_CEILING_DIRECTORIES", parent);
        }
        #[cfg(test)]
        crate::testing::isolate(&mut cmd);
        Self {
            cmd,
            dir: dir.to_path_buf(),
            command: "git".into(),
            stdin: None,
        }
    }

    /// Lets git look for the repository in parent directories too, so a
    /// subdirectory of a work tree can be told apart from a non-repository.
    pub(crate) fn discover_parents(mut self) -> Self {
        self.cmd.env_remove("GIT_CEILING_DIRECTORIES");
        self
    }

    /// Makes git take path arguments as they are. Without this a name such as
    /// `*.txt` is a glob and `:(top)x` is pathspec magic. Not global because
    /// some commands (`check-ignore`) refuse to run with it.
    pub(crate) fn literal_pathspecs(mut self) -> Self {
        self.cmd.env("GIT_LITERAL_PATHSPECS", "1");
        self
    }

    pub(crate) fn env(mut self, name: &str, value: &str) -> Self {
        self.cmd.env(name, value);
        self
    }

    pub(crate) fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.command.push(' ');
        self.command.push_str(&arg.as_ref().to_string_lossy());
        self.cmd.arg(arg);
        self
    }

    pub(crate) fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        for arg in args {
            self = self.arg(arg);
        }
        self
    }

    pub(crate) fn stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Runs the command and reports how it ended, whatever the exit status.
    /// Only "this is not a repository" is turned into an error here.
    pub(crate) fn exec(mut self) -> Result<Done> {
        let command = self.command;
        let spawn_error = |source| Error::Io {
            context: format!("running `{command}`"),
            source,
        };
        let output = match self.stdin {
            None => self.cmd.output().map_err(spawn_error)?,
            Some(bytes) => {
                self.cmd
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped());
                let mut child = self.cmd.spawn().map_err(spawn_error)?;
                // Feed stdin from another thread: git may fill the stdout pipe
                // before it has read everything, and would then block forever.
                let feeder = child.stdin.take().map(|mut pipe| {
                    thread::spawn(move || {
                        // A closed pipe means git stopped reading; its exit status tells why.
                        let _ = pipe.write_all(&bytes);
                    })
                });
                let output = child.wait_with_output().map_err(spawn_error)?;
                if let Some(feeder) = feeder {
                    // The thread only writes to a pipe and cannot panic.
                    let _ = feeder.join();
                }
                output
            }
        };
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !output.status.success() && stderr.contains("not a git repository") {
            return Err(Error::NotARepository {
                path: self.dir,
                reason: "no git repository here".into(),
            });
        }
        Ok(Done {
            status: output.status,
            stdout: output.stdout,
            stderr,
            command,
        })
    }

    /// Standard output of a command that must succeed.
    pub(crate) fn run(self) -> Result<Vec<u8>> {
        let done = self.exec()?;
        if done.status.success() {
            Ok(done.stdout)
        } else {
            Err(done.into_error())
        }
    }

    /// Standard output as text, without the trailing newline.
    pub(crate) fn text(self) -> Result<String> {
        self.run().map(|stdout| text(&stdout))
    }

    /// For commands that answer "no" with exit status 1: `None` in that case.
    pub(crate) fn probe(self) -> Result<Option<String>> {
        let done = self.exec()?;
        match done.status.code() {
            Some(0) => Ok(Some(text(&done.stdout))),
            Some(1) => Ok(None),
            _ => Err(done.into_error()),
        }
    }
}

/// A finished command.
pub(crate) struct Done {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: Vec<u8>,
    /// Trimmed.
    pub(crate) stderr: String,
    command: String,
}

impl Done {
    pub(crate) fn into_error(self) -> Error {
        Error::Git {
            command: self.command,
            stderr: if self.stderr.is_empty() {
                self.status.to_string()
            } else {
                self.stderr
            },
        }
    }
}

fn text(stdout: &[u8]) -> String {
    let text = String::from_utf8_lossy(stdout);
    text.strip_suffix('\n').unwrap_or(&text).to_owned()
}

/// Resolves a revision to a commit id. `None` when it names no commit, which
/// includes `HEAD` in a repository without commits.
pub(crate) fn resolve_commit(dir: &Path, rev: &str) -> Result<Option<String>> {
    Git::read(dir)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{rev}^{{commit}}"))
        .probe()
}

/// The commit `base` names, or [`Error::NotFound`].
pub(crate) fn require_commit(dir: &Path, rev: &str) -> Result<String> {
    resolve_commit(dir, rev)?.ok_or_else(|| Error::NotFound(format!("commit {rev}")))
}
