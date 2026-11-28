//! The one place that starts `git`.

use std::ffi::OsStr;
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};
use crate::setup::{kill, kill_group};

/// How often a time-limited command is checked for its end.
const POLL: Duration = Duration::from_millis(10);
/// After it is gone, how long its output may still trickle in.
const DRAIN: Duration = Duration::from_millis(500);

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

    /// An argument that errors and logs show as `shown`: the user's text
    /// (a commit message) is not a command line to repeat.
    pub(crate) fn arg_shown_as(mut self, arg: impl AsRef<OsStr>, shown: &str) -> Self {
        self.command.push(' ');
        self.command.push_str(shown);
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

    /// [`Self::exec`] for a command that may run hooks or wait on a remote:
    /// it gets no input at all, runs in a process group of its own, and the
    /// whole group is killed at `deadline`. `limit` is what the timeout
    /// error says the command was given.
    pub(crate) fn exec_until(mut self, deadline: Instant, limit: Duration) -> Result<Done> {
        let command = self.command.clone();
        let spawn_error = |source| Error::Io {
            context: format!("running `{command}`"),
            source,
        };
        // Nothing to feed: a hook that reads its input sees the end of it.
        self.cmd
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            self.cmd.process_group(0);
        }
        let mut child = self.cmd.spawn().map_err(spawn_error)?;
        let stdout = collect(child.stdout.take());
        let stderr = collect(child.stderr.take());
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(err) => {
                    kill(&mut child);
                    return Err(spawn_error(err));
                }
            }
            if Instant::now() >= deadline {
                kill(&mut child);
                return Err(Error::Git {
                    command,
                    stderr: format!("timed out after {}", how_long(limit)),
                });
            }
            thread::sleep(POLL);
        };
        // A hook may have left something running that holds the pipes.
        kill_group(&child);
        let stdout = stdout.recv_timeout(DRAIN).unwrap_or_default();
        let stderr = String::from_utf8_lossy(&stderr.recv_timeout(DRAIN).unwrap_or_default())
            .trim()
            .to_owned();
        if !status.success() && stderr.contains("not a git repository") {
            return Err(Error::NotARepository {
                path: self.dir,
                reason: "no git repository here".into(),
            });
        }
        Ok(Done {
            status,
            stdout,
            stderr,
            command,
        })
    }

    /// Standard output of a command that must succeed.
    pub(crate) fn run(self) -> Result<Vec<u8>> {
        Ok(self.exec()?.succeeded()?.stdout)
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

/// Keeps ssh from asking for a passphrase or a host key on the server's
/// terminal: a clone or a push nobody watches must fail instead of waiting.
const SSH_NO_PROMPT: &str = "ssh -oBatchMode=yes";

/// Makes a command that talks to a remote fail where ssh would ask something.
pub(crate) fn no_ssh_prompt(git: Git) -> Git {
    // A user who configured their own ssh command knows what it may ask.
    if std::env::var_os("GIT_SSH_COMMAND").is_some() {
        git
    } else {
        git.env("GIT_SSH_COMMAND", SSH_NO_PROMPT)
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
    /// The command, when it exited with status 0; else its error.
    pub(crate) fn succeeded(self) -> Result<Self> {
        if self.status.success() {
            Ok(self)
        } else {
            Err(self.into_error())
        }
    }

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

/// Reads a stream to its end on a thread of its own and hands over the bytes.
fn collect(stream: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (done, bytes) = mpsc::channel();
    if let Some(mut stream) = stream {
        thread::spawn(move || {
            let mut read = Vec::new();
            // What was read before an error is still what git said.
            let _ = stream.read_to_end(&mut read);
            // Nobody listens once the drain was given up.
            let _ = done.send(read);
        });
    }
    bytes
}

/// A limit as an error message says it: whole seconds, or milliseconds
/// for the short ones tests use.
fn how_long(limit: Duration) -> String {
    if limit >= Duration::from_secs(1) && limit.subsec_millis() == 0 {
        format!("{} seconds", limit.as_secs())
    } else {
        format!("{} ms", limit.as_millis())
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
