//! Runs the CLI as a child process and connects its pipes to an [`AgentHandle`].
//! What the lines mean is the [`Mapper`]'s business; this file only moves them.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::PathBuf;
use std::pin::Pin;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Sleep, sleep, timeout};

use super::mapper::Mapper;
use crate::runner::{AgentHandle, AgentIo, Exit, RunnerError, RunnerInput, RunnerOutput};

/// Lines read ahead of the mapper. Small: a full buffer stops the reader,
/// then the pipe fills and the CLI waits for the orchestrator to catch up.
const LINE_BUFFER: usize = 16;

/// How much of stderr is kept for the message of a failed agent.
const STDERR_TAIL: usize = 4 * 1024;

/// Time between asking a process group to terminate and killing it.
const TERM_GRACE: Duration = Duration::from_millis(500);

/// After the process is gone, how long its pipes may stay quiet before they
/// are given up: a stray grandchild can hold them open forever.
const DRAIN: Duration = Duration::from_millis(250);

pub(crate) struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    /// Time a stopped agent has to exit by itself.
    pub stop_grace: Duration,
}

/// Starts the CLI and the task that drives it. The first turn starts with `prompt`.
pub(crate) fn spawn(
    launch: Launch,
    mapper: Mapper,
    prompt: &str,
) -> Result<AgentHandle, RunnerError> {
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .current_dir(&launch.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A backstop for a driver task that is dropped with its runtime.
        .kill_on_drop(true);
    // Its own group, so that the tools it spawned can be killed with it.
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|err| RunnerError::Start(format!("{}: {err}", launch.program.display())))?;
    let group = ProcessGroup::of(&child);
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        // Cannot happen: all three were requested as pipes above.
        return Err(RunnerError::Start("the CLI started without pipes".into()));
    };

    let (lines_tx, lines) = mpsc::channel(LINE_BUFFER);
    tokio::spawn(read_lines(stdout, lines_tx));
    let stderr_tail = Arc::new(Mutex::new(Vec::new()));
    let stderr_reader = tokio::spawn(read_tail(stderr, Arc::clone(&stderr_tail)));

    let (handle, io) = AgentHandle::channel();
    let driver = Driver {
        io,
        child,
        group,
        stdin: Some(stdin),
        lines,
        stderr_tail,
        stderr_reader,
        mapper,
        in_turn: false,
        queued: VecDeque::new(),
        stopping: false,
        stop_grace: launch.stop_grace,
        escalation: None,
    };
    tokio::spawn(driver.run(prompt.to_owned()));
    Ok(handle)
}

struct Driver {
    io: AgentIo,
    child: Child,
    group: ProcessGroup,
    /// `None` once closed, which is how the CLI is told to finish.
    stdin: Option<ChildStdin>,
    lines: mpsc::Receiver<Vec<u8>>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    stderr_reader: JoinHandle<()>,
    mapper: Mapper,
    /// True from a user message written to the CLI until its `TurnEnded`.
    in_turn: bool,
    /// Messages that arrived mid-turn. The CLI would fold them into the
    /// running turn; each one gets a turn of its own instead.
    queued: VecDeque<String>,
    /// True once the orchestrator asked for a stop: the exit is then expected.
    stopping: bool,
    stop_grace: Duration,
    /// Armed when the process has to go: first asked to terminate, then killed.
    escalation: Option<(Pin<Box<Sleep>>, Step)>,
}

/// What happens when the escalation timer fires.
#[derive(Clone, Copy)]
enum Step {
    Terminate,
    Kill,
}

/// The driver's loop ended because:
enum End {
    /// the process exited, or its status could not be read;
    Exited(Option<ExitStatus>),
    /// the orchestrator dropped the handle.
    Detached,
}

impl Driver {
    async fn run(mut self, prompt: String) {
        self.begin_turn(&prompt).await;
        let status = match self.pump().await {
            End::Exited(status) => status,
            End::Detached => return self.abandon().await,
        };
        // The group outlives its leader when a tool left something running.
        self.group.kill();
        if !self.flush().await {
            return;
        }
        let exit = self.exit(status).await;
        let _ = self.io.outputs.send(RunnerOutput::Exited(exit)).await;
    }

    /// Forwards the lines the CLI wrote just before it went, then completes
    /// what the mapper still has open. False when nobody listens any more.
    async fn flush(&mut self) -> bool {
        while let Ok(Some(line)) = timeout(DRAIN, self.lines.recv()).await {
            if !self.forward(&line).await {
                return false;
            }
        }
        for output in self.mapper.finish() {
            if self.io.outputs.send(output).await.is_err() {
                return false;
            }
        }
        true
    }

    /// How the run ended: fine when it was stopped, or exited cleanly
    /// between turns; otherwise an error that quotes stderr.
    async fn exit(&mut self, status: Option<ExitStatus>) -> Exit {
        if self.stopping || (!self.in_turn && status.is_some_and(|s| s.success())) {
            return Exit::Ok;
        }
        let _ = timeout(DRAIN, &mut self.stderr_reader).await;
        Exit::Error(self.failure(status))
    }

    /// Moves lines and inputs until the process is gone or nobody listens.
    async fn pump(&mut self) -> End {
        let mut stdout_open = true;
        loop {
            tokio::select! {
                status = self.child.wait() => return End::Exited(status.ok()),
                line = self.lines.recv(), if stdout_open => match line {
                    Some(line) => {
                        if !self.forward(&line).await {
                            return End::Detached;
                        }
                    }
                    None => {
                        stdout_open = false;
                        // A CLI that closed its output is finished or broken:
                        // either way it does not get to linger.
                        self.escalate_after(self.stop_grace);
                    }
                },
                input = self.io.inputs.recv(), if !self.stopping => match input {
                    Some(input) => self.input(input).await,
                    None => return End::Detached,
                },
                step = expired(&mut self.escalation) => match step {
                    Step::Terminate => {
                        self.group.terminate();
                        self.escalation = Some((Box::pin(sleep(TERM_GRACE)), Step::Kill));
                    }
                    Step::Kill => self.group.kill(),
                },
            }
        }
    }

    async fn input(&mut self, input: RunnerInput) {
        match input {
            RunnerInput::UserMessage(text) if self.in_turn => self.queued.push_back(text),
            RunnerInput::UserMessage(text) => self.begin_turn(&text).await,
            RunnerInput::PermissionDecision {
                request_id,
                decision,
                message,
                grants,
            } => {
                let response = self.mapper.permission_response_granting(
                    &request_id,
                    decision,
                    message,
                    &grants,
                );
                match response {
                    Some(response) => self.write(&response).await,
                    None => {
                        tracing::debug!(%request_id, "decision for a request that is not waiting")
                    }
                }
            }
            RunnerInput::Interrupt => self.interrupt().await,
            RunnerInput::Stop => {
                self.stopping = true;
                // Ending the turn first lets the CLI stop its tools and save
                // the conversation, so that it can be resumed.
                self.interrupt().await;
                self.stdin = None;
                self.escalate_after(self.stop_grace);
            }
        }
    }

    async fn begin_turn(&mut self, text: &str) {
        self.in_turn = true;
        self.write(&Mapper::user_message(text)).await;
    }

    async fn interrupt(&mut self) {
        if self.in_turn {
            let request = self.mapper.interrupt_request();
            self.write(&request).await;
        }
    }

    /// Writes one line to the CLI. A CLI that no longer reads is on its way
    /// out, and its exit is what gets reported.
    async fn write(&mut self, message: &Value) {
        let Some(stdin) = &mut self.stdin else {
            return;
        };
        let mut line = message.to_string().into_bytes();
        line.push(b'\n');
        let written = async {
            stdin.write_all(&line).await?;
            stdin.flush().await
        };
        if let Err(err) = written.await {
            tracing::debug!(%err, "could not write to claude");
            self.stdin = None;
        }
    }

    /// Maps one stdout line and passes on what it yields. False when nobody listens.
    async fn forward(&mut self, line: &[u8]) -> bool {
        for output in self.mapper.line(&String::from_utf8_lossy(line)) {
            let turn_ended = output == RunnerOutput::TurnEnded;
            // The CLI can take a turn nobody asked for, when a background
            // task of its own reports back. Its events are passed on; its
            // end is not, because no user message is waiting for one.
            if turn_ended && !self.in_turn {
                continue;
            }
            if self.io.outputs.send(output).await.is_err() {
                return false;
            }
            if turn_ended {
                self.in_turn = false;
                if !self.stopping
                    && let Some(text) = self.queued.pop_front()
                {
                    self.begin_turn(&text).await;
                }
            }
        }
        true
    }

    fn escalate_after(&mut self, grace: Duration) {
        if self.escalation.is_none() {
            self.escalation = Some((Box::pin(sleep(grace)), Step::Terminate));
        }
    }

    /// Ends the process for a handle that was dropped.
    async fn abandon(mut self) {
        self.group.kill();
        let _ = self.child.start_kill();
        let _ = self.child.wait().await;
    }

    /// Why the agent died, for the user: what the CLI said last, else how it ended.
    fn failure(&self, status: Option<ExitStatus>) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let said = String::from_utf8_lossy(&tail);
        let said = said.trim();
        if !said.is_empty() {
            return said.to_owned();
        }
        match status {
            Some(status) => format!("claude ended unexpectedly ({status})"),
            None => "claude ended unexpectedly".to_owned(),
        }
    }
}

/// Resolves with the step to take when the escalation timer fires; never, when none is armed.
async fn expired(escalation: &mut Option<(Pin<Box<Sleep>>, Step)>) -> Step {
    match escalation {
        Some((timer, step)) => {
            timer.await;
            let step = *step;
            *escalation = None;
            step
        }
        None => std::future::pending().await,
    }
}

async fn read_lines(stdout: ChildStdout, lines: mpsc::Sender<Vec<u8>>) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = Vec::new();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                if lines.send(line).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Reads stderr to its end, so the CLI never blocks on it, and keeps the last of it.
async fn read_tail(mut stderr: ChildStderr, tail: Arc<Mutex<Vec<u8>>>) {
    let mut chunk = [0u8; 1024];
    loop {
        match stderr.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => {
                let mut tail = tail.lock().unwrap_or_else(PoisonError::into_inner);
                tail.extend_from_slice(&chunk[..n]);
                let excess = tail.len().saturating_sub(STDERR_TAIL);
                tail.drain(..excess);
            }
        }
    }
}

/// The process group of the CLI. Killed when dropped, so that no path out of
/// the driver, a panic included, leaves a tool of the agent running.
struct ProcessGroup {
    #[cfg(unix)]
    leader: Option<rustix::process::Pid>,
}

impl ProcessGroup {
    #[cfg(unix)]
    fn of(child: &Child) -> Self {
        let leader = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw);
        Self { leader }
    }

    #[cfg(not(unix))]
    fn of(_child: &Child) -> Self {
        Self {}
    }

    /// Asks every process of the group to exit.
    fn terminate(&self) {
        #[cfg(unix)]
        self.signal(rustix::process::Signal::TERM);
    }

    fn kill(&self) {
        #[cfg(unix)]
        self.signal(rustix::process::Signal::KILL);
    }

    #[cfg(unix)]
    fn signal(&self, signal: rustix::process::Signal) {
        if let Some(leader) = self.leader {
            // Fails when the group is already empty, which is the goal.
            let _ = rustix::process::kill_process_group(leader, signal);
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

#[cfg(all(test, unix))]
mod tests;
