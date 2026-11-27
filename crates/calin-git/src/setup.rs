//! Running a repo's setup command in a worktree that was just created.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::cmd::{INHERITED, work_dir};
use crate::error::{Error, Result};

/// How much of the command's output is kept: the end of it, where a failing
/// command says why.
const OUTPUT_CAP: usize = 64 * 1024;

/// How often the command is checked for its end, the limit and a cancellation.
const POLL: Duration = Duration::from_millis(20);

/// After the command is gone, how long its output may still trickle in.
/// Something it left running in the background can hold the pipe open forever.
const DRAIN: Duration = Duration::from_millis(500);

/// A setup command that ran, however it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetupRun {
    /// Standard output and standard error in the order they were written,
    /// cut to its last 64 KiB with a note when it was longer.
    pub output: String,
    pub end: SetupEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupEnd {
    Succeeded,
    /// The command exited with a failure; this is how, for example
    /// `exit status: 1`.
    Failed(String),
    /// The command was still running at the limit and was killed.
    TimedOut,
    /// `cancel` was set and the command was killed.
    Cancelled,
}

/// Runs `command` with `sh -c` in `dir` and waits for it, for at most `limit`.
///
/// The command gets no input. It runs in a process group of its own, and
/// the whole group is killed when the command ends, at the limit, or once
/// `cancel` is set: nothing it started lives on.
///
/// An error means the command could not be run at all; a command that ran
/// and failed is a [`SetupRun`].
pub fn run_setup(
    dir: &Path,
    command: &str,
    limit: Duration,
    cancel: &AtomicBool,
) -> Result<SetupRun> {
    let dir = work_dir(dir)?;
    let failed = |source| Error::Io {
        context: format!("running the setup command in {}", dir.display()),
        source,
    };
    // One pipe for both streams keeps their lines in order.
    let (mut reader, writer) = std::io::pipe().map_err(failed)?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(writer.try_clone().map_err(failed)?)
        .stderr(writer);
    // The command may run git, which must see the worktree it is in.
    for name in INHERITED {
        cmd.env_remove(name);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(failed)?;
    // The command holds the writing ends now; ours must close for the
    // reader to see the end.
    drop(cmd);

    let tail = Arc::new(Mutex::new(Tail::default()));
    let (finished, drained) = mpsc::channel();
    let collector = Arc::clone(&tail);
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => lock(&collector).push(&chunk[..n]),
            }
        }
        // Nobody listens when the drain was given up.
        let _ = finished.send(());
    });

    let started = Instant::now();
    let end = loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break SetupEnd::Succeeded,
            Ok(Some(status)) => break SetupEnd::Failed(status.to_string()),
            Ok(None) => {}
            Err(err) => {
                kill(&mut child);
                return Err(failed(err));
            }
        }
        if cancel.load(Ordering::Relaxed) {
            kill(&mut child);
            break SetupEnd::Cancelled;
        }
        if started.elapsed() >= limit {
            kill(&mut child);
            break SetupEnd::TimedOut;
        }
        thread::sleep(POLL);
    };
    // What the command left running may still hold the pipe; kill the group
    // even after a clean exit, then take what was written.
    kill_group(&child);
    let _ = drained.recv_timeout(DRAIN);
    let output = lock(&tail).text();
    Ok(SetupRun { output, end })
}

fn lock(tail: &Mutex<Tail>) -> std::sync::MutexGuard<'_, Tail> {
    // Every critical section is one append or one copy.
    tail.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Kills the command and everything in its group, and reaps it.
pub(crate) fn kill(child: &mut Child) {
    kill_group(child);
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) fn kill_group(child: &Child) {
    #[cfg(unix)]
    {
        let leader = i32::try_from(child.id())
            .ok()
            .and_then(rustix::process::Pid::from_raw);
        if let Some(leader) = leader {
            // Fails when the group is already empty, which is the goal.
            let _ = rustix::process::kill_process_group(leader, rustix::process::Signal::KILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

/// The end of a stream of bytes, and how long the stream was.
#[derive(Default)]
struct Tail {
    bytes: Vec<u8>,
    total: usize,
}

impl Tail {
    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        self.bytes.extend_from_slice(chunk);
        // Twice the cap before trimming, so that a long output is not
        // shifted on every chunk.
        if self.bytes.len() > 2 * OUTPUT_CAP {
            let excess = self.bytes.len() - OUTPUT_CAP;
            self.bytes.drain(..excess);
        }
    }

    fn text(&self) -> String {
        let start = self.bytes.len().saturating_sub(OUTPUT_CAP);
        let text = String::from_utf8_lossy(&self.bytes[start..]);
        if self.total <= OUTPUT_CAP {
            return text.into_owned();
        }
        format!(
            "[output truncated: last {} of {} bytes]\n{text}",
            self.bytes.len() - start,
            self.total
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::Sandbox;

    const LIMIT: Duration = Duration::from_secs(30);

    fn run(dir: &Path, command: &str) -> SetupRun {
        run_setup(dir, command, LIMIT, &AtomicBool::new(false)).unwrap()
    }

    #[test]
    fn a_command_runs_in_the_directory_and_both_streams_are_kept_in_order() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let ran = run(
            &dir,
            "echo one; echo two >&2; echo three; pwd; touch made.txt",
        );
        assert_eq!(ran.end, SetupEnd::Succeeded);
        assert_eq!(ran.output, format!("one\ntwo\nthree\n{}\n", dir.display()));
        assert!(dir.join("made.txt").is_file());
        // No input: a command that reads sees the end at once.
        assert_eq!(run(&dir, "cat; echo done").output, "done\n");
    }

    #[test]
    fn a_failing_command_reports_its_status_and_what_it_said() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let ran = run(&dir, "echo broken >&2; exit 3");
        assert_eq!(ran.end, SetupEnd::Failed("exit status: 3".to_owned()));
        assert_eq!(ran.output, "broken\n");
        // A command the shell cannot even parse fails like any other.
        let ran = run(&dir, "if then");
        assert!(matches!(ran.end, SetupEnd::Failed(_)), "{ran:?}");
        assert!(!ran.output.is_empty());

        let missing = run_setup(&dir.join("gone"), "true", LIMIT, &AtomicBool::new(false));
        assert!(matches!(missing, Err(Error::NotFound(_))), "{missing:?}");
    }

    #[test]
    fn a_command_over_the_limit_is_killed_with_what_it_started() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let started = Instant::now();
        let ran = run_setup(
            &dir,
            "echo begun; sleep 60 & sleep 60; echo never",
            Duration::from_millis(200),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(ran.end, SetupEnd::TimedOut);
        assert_eq!(ran.output, "begun\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_cancelled_command_is_killed() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let cancel = AtomicBool::new(true);
        let started = Instant::now();
        let ran = run_setup(&dir, "sleep 60", LIMIT, &cancel).unwrap();
        assert_eq!(ran.end, SetupEnd::Cancelled);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn a_background_job_that_outlives_the_command_does_not_hold_it_up() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let started = Instant::now();
        let ran = run(&dir, "sleep 60 & echo started");
        assert_eq!(ran.end, SetupEnd::Succeeded);
        assert_eq!(ran.output, "started\n");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn long_output_keeps_its_end() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let ran = run(
            &dir,
            "i=0; while [ $i -lt 30000 ]; do echo line-$i; i=$((i+1)); done",
        );
        assert_eq!(ran.end, SetupEnd::Succeeded);
        let (note, kept) = ran.output.split_once('\n').unwrap();
        assert!(
            note.starts_with("[output truncated: last 65536 of "),
            "{note}"
        );
        assert_eq!(kept.len(), OUTPUT_CAP);
        assert!(kept.ends_with("line-29999\n"));

        let mut tail = Tail::default();
        tail.push(b"short");
        assert_eq!(tail.text(), "short");
    }
}
