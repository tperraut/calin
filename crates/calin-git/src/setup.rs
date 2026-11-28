//! Running a shell command in a work directory: a repo's setup command in a
//! worktree that was just created, and the commands that check a change.

use std::io::{PipeReader, Read};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use crate::cmd::{INHERITED, work_dir};
use crate::error::{Error, Result};

/// How much of a setup command's output is kept: the end of it, where a
/// failing command says why.
const OUTPUT_CAP: usize = 64 * 1024;

/// How much of a check's output is kept (workflows spec, section 3).
pub const CHECK_OUTPUT_CAP: usize = 16 * 1024;

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
    let (output, ended) = run(
        dir,
        command,
        limit,
        cancel,
        (OUTPUT_CAP, true),
        "the setup command",
    )?;
    let end = match ended {
        Ended::Exited(status) if status.success() => SetupEnd::Succeeded,
        Ended::Exited(status) => SetupEnd::Failed(status.to_string()),
        Ended::TimedOut => SetupEnd::TimedOut,
        Ended::Cancelled => SetupEnd::Cancelled,
    };
    Ok(SetupRun { output, end })
}

/// A check command that ran, however it ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRun {
    /// Standard output and standard error in the order they were written,
    /// cut to its last 16 KiB.
    pub output: String,
    pub end: CheckEnd,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckEnd {
    /// The command exited: with this code, or `None` when a signal ended it.
    Exited(Option<i32>),
    /// The command was still running at the limit and was killed.
    TimedOut,
    /// `cancel` was set and the command was killed.
    Cancelled,
}

impl CheckRun {
    /// True when the command exited with status 0.
    pub fn passed(&self) -> bool {
        self.end == CheckEnd::Exited(Some(0))
    }
}

/// Runs a check: `command` with `sh -c` in `dir`, as [`run_setup`] runs a
/// setup command, but reporting the exit code and keeping only the last
/// [`CHECK_OUTPUT_CAP`] bytes of what it wrote, without a note about the cut.
pub fn run_check(
    dir: &Path,
    command: &str,
    limit: Duration,
    cancel: &AtomicBool,
) -> Result<CheckRun> {
    let (output, ended) = run(
        dir,
        command,
        limit,
        cancel,
        (CHECK_OUTPUT_CAP, false),
        "the check command",
    )?;
    let end = match ended {
        Ended::Exited(status) => CheckEnd::Exited(status.code()),
        Ended::TimedOut => CheckEnd::TimedOut,
        Ended::Cancelled => CheckEnd::Cancelled,
    };
    Ok(CheckRun { output, end })
}

/// How a command stopped running.
pub(crate) enum Ended {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
}

/// Waits for `child`, looking every `poll`. At `deadline`, or once `cancel`
/// is set, it is killed with its group; so it is when waiting fails.
pub(crate) fn wait_until(
    child: &mut Child,
    deadline: Instant,
    poll: Duration,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<Ended> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Ended::Exited(status)),
            Ok(None) => {}
            Err(err) => {
                kill(child);
                return Err(err);
            }
        }
        if cancel.is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
            kill(child);
            return Ok(Ended::Cancelled);
        }
        if Instant::now() >= deadline {
            kill(child);
            return Ok(Ended::TimedOut);
        }
        thread::sleep(poll);
    }
}

/// Makes the command the leader of a process group of its own, so that
/// [`kill_group`] reaches whatever it starts.
pub(crate) fn in_own_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = cmd;
}

/// Runs `command` and returns the end of what it wrote: its last `cap`
/// bytes, under a line saying what was cut when `noted`.
fn run(
    dir: &Path,
    command: &str,
    limit: Duration,
    cancel: &AtomicBool,
    (cap, noted): (usize, bool),
    what: &str,
) -> Result<(String, Ended)> {
    let dir = work_dir(dir)?;
    let failed = |source| Error::Io {
        context: format!("running {what} in {}", dir.display()),
        source,
    };
    let (reader, mut child) = spawn_shell(&dir, command).map_err(failed)?;
    let tail = Arc::new(Mutex::new(Tail::new(cap)));
    let drained = drain(reader, Arc::clone(&tail));
    let end = wait_until(&mut child, Instant::now() + limit, POLL, Some(cancel)).map_err(failed)?;
    // What the command left running may still hold the pipe; kill the group
    // even after a clean exit, then take what was written.
    kill_group(&child);
    let _ = drained.recv_timeout(DRAIN);
    let output = lock(&tail).text(noted);
    Ok((output, end))
}

/// Starts `command` with `sh -c` in `dir`, in a process group of its own,
/// with no input and both output streams on the one pipe it returns.
fn spawn_shell(dir: &Path, command: &str) -> std::io::Result<(PipeReader, Child)> {
    // One pipe for both streams keeps their lines in order.
    let (reader, writer) = std::io::pipe()?;
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    // The command may run git, which must see the worktree it is in.
    for name in INHERITED {
        cmd.env_remove(name);
    }
    in_own_group(&mut cmd);
    let child = cmd.spawn()?;
    // The command holds the writing ends now; ours must close for the
    // reader to see the end.
    drop(cmd);
    Ok((reader, child))
}

/// Reads the pipe into `tail` on a thread of its own, until its end. The
/// receiver hears when that came.
fn drain(mut reader: PipeReader, tail: Arc<Mutex<Tail>>) -> mpsc::Receiver<()> {
    let (finished, drained) = mpsc::channel();
    thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => lock(&tail).push(&chunk[..n]),
            }
        }
        // Nobody listens when the drain was given up.
        let _ = finished.send(());
    });
    drained
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
struct Tail {
    bytes: Vec<u8>,
    total: usize,
    /// How many bytes of the end are kept.
    cap: usize,
}

impl Tail {
    fn new(cap: usize) -> Self {
        Self {
            bytes: Vec::new(),
            total: 0,
            cap,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.total += chunk.len();
        self.bytes.extend_from_slice(chunk);
        // Twice the cap before trimming, so that a long output is not
        // shifted on every chunk.
        if self.bytes.len() > 2 * self.cap {
            let excess = self.bytes.len() - self.cap;
            self.bytes.drain(..excess);
        }
    }

    /// The kept end as text, with a first line saying what was cut when
    /// `noted` and something was.
    fn text(&self, noted: bool) -> String {
        let start = self.bytes.len().saturating_sub(self.cap);
        let text = String::from_utf8_lossy(&self.bytes[start..]);
        if self.total <= self.cap || !noted {
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

        let mut tail = Tail::new(OUTPUT_CAP);
        tail.push(b"short");
        assert_eq!(tail.text(true), "short");
    }

    #[test]
    fn the_note_says_what_was_kept_of_how_much_and_only_when_something_was_cut() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let bytes = |n: usize| format!("head -c {n} /dev/zero | tr '\\0' a");

        let ran = run(&dir, &bytes(OUTPUT_CAP));
        assert_eq!(ran.output, "a".repeat(OUTPUT_CAP));

        let ran = run(&dir, &bytes(OUTPUT_CAP + 5));
        let (note, kept) = ran.output.split_once('\n').unwrap();
        assert_eq!(
            note,
            format!(
                "[output truncated: last {OUTPUT_CAP} of {} bytes]",
                OUTPUT_CAP + 5
            )
        );
        assert_eq!(kept, "a".repeat(OUTPUT_CAP));
    }

    #[test]
    fn the_drain_says_when_the_stream_has_ended() {
        use std::io::Write as _;

        let (reader, mut writer) = std::io::pipe().unwrap();
        let tail = Arc::new(Mutex::new(Tail::new(OUTPUT_CAP)));
        let drained = drain(reader, Arc::clone(&tail));
        writer.write_all(b"all of it").unwrap();
        drop(writer);
        // Far more than the end of a closed pipe takes to arrive.
        drained.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(lock(&tail).text(true), "all of it");
    }

    #[test]
    fn a_check_reports_its_exit_code_and_the_last_16_kib_of_its_output() {
        let sandbox = Sandbox::new();
        let dir = sandbox.dir("work");
        let no = AtomicBool::new(false);
        let ran = run_check(&dir, "echo fine; pwd", LIMIT, &no).unwrap();
        assert!(ran.passed());
        assert_eq!(ran.end, CheckEnd::Exited(Some(0)));
        assert_eq!(ran.output, format!("fine\n{}\n", dir.display()));

        let ran = run_check(&dir, "echo broken >&2; exit 7", LIMIT, &no).unwrap();
        assert!(!ran.passed());
        assert_eq!(ran.end, CheckEnd::Exited(Some(7)));
        assert_eq!(ran.output, "broken\n");

        // The end of a long output, cut without a note: templates quote it.
        let long = "i=0; while [ $i -lt 9000 ]; do echo line-$i; i=$((i+1)); done; exit 1";
        let ran = run_check(&dir, long, LIMIT, &no).unwrap();
        assert_eq!(ran.end, CheckEnd::Exited(Some(1)));
        assert_eq!(ran.output.len(), CHECK_OUTPUT_CAP);
        assert!(ran.output.ends_with("line-8999\n"));

        let slow = run_check(
            &dir,
            "echo begun; sleep 60",
            Duration::from_millis(200),
            &no,
        );
        let slow = slow.unwrap();
        assert_eq!(slow.end, CheckEnd::TimedOut);
        assert_eq!(slow.output, "begun\n");
        let cancelled = run_check(&dir, "sleep 60", LIMIT, &AtomicBool::new(true)).unwrap();
        assert_eq!(cancelled.end, CheckEnd::Cancelled);
        // A signal leaves no code.
        let killed = run_check(&dir, "kill -9 $$", LIMIT, &no).unwrap();
        assert_eq!(killed.end, CheckEnd::Exited(None));
    }
}
