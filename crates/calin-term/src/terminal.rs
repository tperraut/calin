use std::fmt;
use std::io::{self, ErrorKind, PipeReader, PipeWriter, Read, Write};
use std::process::Child;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc as std_mpsc};
use std::thread;

use pty_process::Size;
use pty_process::blocking::{Command, Pts, Pty};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use rustix::io::Errno;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process_group, waitid};
use tokio::sync::{mpsc, watch};

use crate::{TermError, TerminalConfig, session};

/// Output chunks buffered for a slow consumer before the reader thread stops
/// reading, which in turn blocks the program on its next write.
const OUTPUT_QUEUE: usize = 64;
/// Input chunks buffered before [`Terminal::write`] waits.
const INPUT_QUEUE: usize = 64;
const READ_BUFFER: usize = 8 * 1024;
const MAX_DIMENSION: u16 = 1000;

/// After the program exited, output normally ends when the last process holding
/// the tty is gone. A process that left the session can hold it forever, so the
/// stream also ends once it has been quiet for this long...
const QUIET_AFTER_EXIT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 200_000_000,
};
/// ...or after this much more output, which is beyond what the kernel can have
/// buffered from the program itself.
const MAX_OUTPUT_AFTER_EXIT: usize = 1024 * 1024;

/// Numbers the threads of each terminal, to tell them apart in a debugger.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Chunks of bytes as the program writes them. `recv()` returns `None` once the
/// program has exited and all its output was delivered.
///
/// The queue is bounded: not receiving blocks the program, and holding the
/// receiver without draining it keeps the terminal's reader thread parked, so
/// drop it when done.
pub type Output = mpsc::Receiver<Vec<u8>>;

/// A program running on a pseudo-terminal, as the leader of its own session.
///
/// Dropping it kills the session, like [`Terminal::kill`].
pub struct Terminal {
    shared: Arc<Shared>,
    /// `None` until the program exits, then its exit code if it has one.
    exit: watch::Receiver<Option<Option<i32>>>,
    pid: u32,
    id: u64,
}

struct Shared {
    /// `Some` until the watcher thread sees the program exit. The watcher
    /// empties it before reaping, so whoever finds it `Some` under the lock
    /// knows the leader's pid has not been recycled.
    live: Mutex<Option<Live>>,
}

struct Live {
    pty: Arc<Pty>,
    input: mpsc::Sender<Vec<u8>>,
    leader: Pid,
}

impl Shared {
    fn live(&self) -> MutexGuard<'_, Option<Live>> {
        // Nothing panics while holding the lock, and the state is a plain
        // `Option` either way.
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Terminal {
    /// Starts the configured program on a new pty.
    ///
    /// This forks and executes, like `std::process::Command::spawn`; it does
    /// not wait for the program. It needs no tokio runtime.
    pub fn spawn(config: &TerminalConfig) -> Result<(Self, Output), TermError> {
        if !config.cwd.is_dir() {
            return Err(TermError::Spawn(format!(
                "{} is not a directory",
                config.cwd.display()
            )));
        }
        let (pty, pts) = open_pty(config)?;
        // The threads start before the program so that a failure to start one
        // leaves no process to clean up. If anything below fails, dropping the
        // locals ends them: the tty closes, and so do the channels.
        let ends = start_threads(&pty)?;

        let (program, args) = config.command_line();
        let child = Command::new(&program)
            .args(&args)
            .current_dir(&config.cwd)
            .envs(config.env.iter().map(|(key, value)| (key, value)))
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor")
            .spawn(pts)
            .map_err(|e| TermError::Spawn(format!("{}: {e}", program.display())))?;

        let pid = child.id();
        let Some(leader) = i32::try_from(pid).ok().and_then(Pid::from_raw) else {
            return Err(abandon(child, "the program has no valid pid"));
        };
        let shared = Arc::new(Shared {
            live: Mutex::new(Some(Live {
                pty,
                input: ends.input,
                leader,
            })),
        });
        if let Err(std_mpsc::SendError(start)) = ends.start.send(Start {
            child,
            leader,
            shared: shared.clone(),
        }) {
            return Err(abandon(start.child, "the exit watcher thread is gone"));
        }

        let terminal = Self {
            shared,
            exit: ends.exit,
            pid,
            id: ends.id,
        };
        Ok((terminal, ends.output))
    }

    /// Process id of the program, which is also its session and group id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Queues bytes for the program's input, waiting while the queue is full.
    ///
    /// `Ok` means queued, not read by the program. Fails with
    /// [`TermError::Closed`] once the program has exited.
    pub async fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<(), TermError> {
        let input = match self.shared.live().as_ref() {
            Some(live) => live.input.clone(),
            None => return Err(TermError::Closed),
        };
        input
            .send(bytes.into())
            .await
            .map_err(|_| TermError::Closed)
    }

    /// Tells the program its window changed. Each dimension is clamped to
    /// `1..=1000`.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), TermError> {
        match self.shared.live().as_ref() {
            Some(live) => live
                .pty
                .resize(clamp_size(cols, rows))
                .map_err(|e| TermError::Io(io::Error::other(e.to_string()))),
            None => Err(TermError::Closed),
        }
    }

    /// Waits for the program to exit and returns its exit code, or `None` if a
    /// signal killed it. Can be called any number of times, from any task.
    ///
    /// When this returns, every process left in the program's session has been
    /// killed too.
    pub async fn wait(&self) -> Option<i32> {
        let mut exit = self.exit.clone();
        match exit.wait_for(Option::is_some).await {
            Ok(code) => code.flatten(),
            // The watcher thread publishes before it ends, so a closed channel
            // means it panicked: there is no code to report.
            Err(_) => None,
        }
    }

    /// Kills the program and everything else in its session, without waiting:
    /// follow with [`Terminal::wait`] to know when it is done. Does nothing
    /// once the program has exited.
    ///
    /// A process that moved to another session (`setsid`) is out of reach.
    pub fn kill(&self) {
        if let Some(live) = self.shared.live().as_ref() {
            // The leader's death wakes the watcher thread, which kills the rest
            // of the session off the caller's thread. Failure would mean the
            // program changed owner, which leaves nothing to try.
            let _ = kill_process_group(live.leader, Signal::KILL);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.kill();
    }
}

impl fmt::Debug for Terminal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Terminal")
            .field("id", &self.id)
            .field("pid", &self.pid)
            .finish_non_exhaustive()
    }
}

/// A new pty of the configured size, and the program's end of it.
fn open_pty(config: &TerminalConfig) -> Result<(Arc<Pty>, Pts), TermError> {
    let (pty, pts) = pty_process::blocking::open().map_err(spawn_error("open a pty"))?;
    pty.resize(clamp_size(config.cols, config.rows))
        .map_err(spawn_error("size the pty"))?;
    // The threads wait with poll(2) so that the exit of the program can
    // interrupt them; a blocking read or write could not be.
    let flags = fcntl_getfl(&pty).map_err(spawn_error("configure the pty"))?;
    fcntl_setfl(&pty, flags | OFlags::NONBLOCK).map_err(spawn_error("configure the pty"))?;
    Ok((Arc::new(pty), pts))
}

/// What [`Terminal::spawn`] keeps of the threads it started: their number
/// and the ends of their channels.
struct Ends {
    id: u64,
    output: Output,
    input: mpsc::Sender<Vec<u8>>,
    exit: watch::Receiver<Option<Option<i32>>>,
    start: std_mpsc::Sender<Start>,
}

/// Starts the output, input and exit watcher threads of a terminal on `pty`.
fn start_threads(pty: &Arc<Pty>) -> Result<Ends, TermError> {
    // Closing the write end wakes every thread polling the read end.
    let (exited, on_exit) = io::pipe().map_err(spawn_error("open a pipe"))?;
    let exited = Arc::new(exited);

    let (output_tx, output_rx) = mpsc::channel(OUTPUT_QUEUE);
    let (input_tx, mut input_rx) = mpsc::channel(INPUT_QUEUE);
    let (exit_tx, exit_rx) = watch::channel(None);
    let (start_tx, start_rx) = std_mpsc::channel();

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    spawn_thread(format!("pty-out-{id}"), {
        let (pty, exited) = (pty.clone(), exited.clone());
        move || read_output(&pty, &exited, &output_tx)
    })?;
    spawn_thread(format!("pty-in-{id}"), {
        let pty = pty.clone();
        move || write_input(&pty, &exited, &mut input_rx)
    })?;
    spawn_thread(format!("pty-wait-{id}"), move || {
        watch_exit(&start_rx, on_exit, &exit_tx);
    })?;
    Ok(Ends {
        id,
        output: output_rx,
        input: input_tx,
        exit: exit_rx,
        start: start_tx,
    })
}

fn clamp_size(cols: u16, rows: u16) -> Size {
    Size::new(rows.clamp(1, MAX_DIMENSION), cols.clamp(1, MAX_DIMENSION))
}

fn spawn_error<E: fmt::Display>(action: &'static str) -> impl Fn(E) -> TermError {
    move |e| TermError::Spawn(format!("could not {action}: {e}"))
}

fn spawn_thread(name: String, body: impl FnOnce() + Send + 'static) -> Result<(), TermError> {
    thread::Builder::new()
        .name(name)
        .spawn(body)
        .map(drop)
        .map_err(spawn_error("start a thread"))
}

/// Ends a program that was started but cannot be handed to its watcher.
fn abandon(mut child: Child, reason: &str) -> TermError {
    let _ = child.kill();
    let _ = child.wait();
    TermError::Spawn(reason.to_owned())
}

struct Start {
    child: Child,
    leader: Pid,
    shared: Arc<Shared>,
}

/// Body of the watcher thread: ends the terminal when the program exits.
fn watch_exit(
    start: &std_mpsc::Receiver<Start>,
    on_exit: PipeWriter,
    exit: &watch::Sender<Option<Option<i32>>>,
) {
    // No message: the program never started.
    let Ok(Start {
        mut child,
        leader,
        shared,
    }) = start.recv()
    else {
        return;
    };
    // Wait without reaping: the zombie keeps the pid, and with it the session
    // id, reserved while the session is being killed.
    let unreaped = loop {
        match waitid(
            WaitId::Pid(leader),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Err(Errno::INTR) => {}
            result => break result.is_ok(),
        }
    };
    // From here on `kill`, `write` and `resize` see a closed terminal. This
    // also drops the input sender, which ends the input thread.
    drop(shared.live().take());
    // If the wait failed the pid may already be someone else's: leave it alone.
    if unreaped {
        session::kill_all(leader);
    }
    let code = child.wait().ok().and_then(|status| status.code());
    exit.send_replace(Some(code));
    drop(on_exit);
}

struct Ready {
    pty: bool,
    exited: bool,
}

/// Waits until the pty is ready for `interest`, the program exits (when
/// `exited` is given) or `timeout` passes.
fn wait_ready(
    pty: &Pty,
    interest: PollFlags,
    exited: Option<&PipeReader>,
    timeout: Option<&Timespec>,
) -> io::Result<Ready> {
    let pty = PollFd::new(pty, interest);
    let (mut one, mut two);
    let fds: &mut [PollFd<'_>] = match exited {
        Some(exited) => {
            two = [pty, PollFd::new(exited, PollFlags::IN)];
            &mut two
        }
        None => {
            one = [pty];
            &mut one
        }
    };
    loop {
        match poll(fds, timeout) {
            Err(Errno::INTR) => {}
            Err(e) => return Err(e.into()),
            // Errors and hang-ups count as ready: the next read or write
            // reports them.
            Ok(_) => {
                return Ok(Ready {
                    pty: !fds[0].revents().is_empty(),
                    exited: fds.get(1).is_some_and(|fd| !fd.revents().is_empty()),
                });
            }
        }
    }
}

/// Body of the output thread: pty to queue, until the output ends.
fn read_output(pty: &Pty, exited: &PipeReader, output: &mpsc::Sender<Vec<u8>>) {
    let mut reader = pty;
    let mut buffer = vec![0; READ_BUFFER];
    let mut drain = Drain::default();
    while drain.wait_for_output(pty, exited) {
        match reader.read(&mut buffer) {
            Ok(0) => return,
            Ok(n) => {
                // Blocks while the queue is full; fails once the receiver is dropped.
                if output.blocking_send(buffer[..n].to_vec()).is_err() || drain.read_too_much(n) {
                    return;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
            // EIO: every process holding the tty is gone and nothing is left to read.
            Err(_) => return,
        }
    }
}

/// Where the output thread is: before the program's exit, or after it with
/// how much it has read since.
#[derive(Default)]
struct Drain {
    has_exited: bool,
    read_after_exit: usize,
}

impl Drain {
    /// Waits until the pty has output. False when the output is over: it was
    /// quiet for too long after the exit, or waiting failed.
    fn wait_for_output(&mut self, pty: &Pty, exited: &PipeReader) -> bool {
        loop {
            let ready = if self.has_exited {
                wait_ready(pty, PollFlags::IN, None, Some(&QUIET_AFTER_EXIT))
            } else {
                wait_ready(pty, PollFlags::IN, Some(exited), None)
            };
            let Ok(ready) = ready else { return false };
            if ready.pty {
                self.has_exited |= ready.exited;
                return true;
            }
            if self.has_exited {
                return false;
            }
            self.has_exited = ready.exited;
        }
    }

    /// Counts `n` bytes read. True once more came after the exit than the
    /// program itself can have left in the kernel's buffer.
    fn read_too_much(&mut self, n: usize) -> bool {
        if !self.has_exited {
            return false;
        }
        self.read_after_exit += n;
        self.read_after_exit > MAX_OUTPUT_AFTER_EXIT
    }
}

/// Body of the input thread: queue to pty, until the terminal is closed.
fn write_input(pty: &Pty, exited: &PipeReader, input: &mut mpsc::Receiver<Vec<u8>>) {
    while let Some(bytes) = input.blocking_recv() {
        if !write_all(pty, exited, &bytes) {
            return;
        }
    }
}

/// Writes all of `bytes` to the pty. False when the input is over: the
/// program exited or the pty failed.
fn write_all(pty: &Pty, exited: &PipeReader, bytes: &[u8]) -> bool {
    let mut writer = pty;
    let mut rest = bytes;
    while !rest.is_empty() {
        match writer.write(rest) {
            Ok(0) => return false,
            Ok(n) => rest = &rest[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            // The program is not reading its input: wait for room, unless
            // it exits first.
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                if !matches!(wait_ready(pty, PollFlags::OUT, Some(exited), None), Ok(ready) if !ready.exited)
                {
                    return false;
                }
            }
            Err(_) => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests;
