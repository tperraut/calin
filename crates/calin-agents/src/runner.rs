use std::path::PathBuf;

use calin_proto::{EventBody, PermissionDecision, PermissionMode, PermissionRequest};
use tokio::sync::mpsc;

/// Outputs buffered between an agent and the orchestrator. Small on purpose:
/// a full buffer is the backpressure that keeps an agent from outrunning the log.
const OUTPUT_BUFFER: usize = 64;

/// What a runner needs to start an agent.
#[derive(Clone, Debug, Default)]
pub struct StartParams {
    pub session_id: String,
    /// Assigned by the orchestrator; stable across restarts of the same agent.
    pub agent_id: String,
    /// The user text of the first turn.
    pub prompt: String,
    /// The directory the agent works in. Must exist.
    pub cwd: PathBuf,
    /// Further directories the agent may read and write, for example the
    /// session's other worktrees.
    pub add_dirs: Vec<PathBuf>,
    /// Decides which tool calls the agent asks about at all. The runner never
    /// answers a request itself: see [`RunnerOutput::PermissionRequest`].
    pub permission_mode: PermissionMode,
    /// The model to use; `None` leaves the choice to the runner.
    pub model: Option<String>,
    /// The conversation to continue: the id a previous start of this agent
    /// announced with [`RunnerOutput::NativeSession`]. `None` starts a new one.
    pub resume: Option<String>,
    /// The session's newest sequence number at start. It differs for every
    /// start within a session, so a runner that mints its own item ids can
    /// use it as a namespace.
    pub start_seq: u64,
}

/// What an agent tells the orchestrator.
#[derive(Clone, Debug, PartialEq)]
pub enum RunnerOutput {
    /// Something to append to the session log, attributed to the agent.
    Event(EventBody),
    /// The agent wants to use a tool and waits for
    /// [`RunnerInput::PermissionDecision`] with the request's id. The runner
    /// forwards every request it receives and applies no policy of its own.
    PermissionRequest(PermissionRequest),
    /// The runner's own conversation id for this agent, and the model in use
    /// when the runner knows it. Sent when first known and again whenever
    /// either changes; store the id and pass it as [`StartParams::resume`].
    NativeSession { id: String, model: Option<String> },
    /// The agent answered the current user message and waits for the next one.
    TurnEnded,
    /// The agent is gone. Always the last output.
    Exited(Exit),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The agent finished or honored a stop request.
    Ok,
    /// The agent died; the message is shown to the user.
    Error(String),
}

/// What the orchestrator tells an agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunnerInput {
    /// Starts another turn. A message sent during a turn waits for its end.
    UserMessage(String),
    /// Answers the [`RunnerOutput::PermissionRequest`] with this id. Ignored
    /// when no such request is waiting.
    PermissionDecision {
        request_id: String,
        decision: PermissionDecision,
        /// Shown to the agent with a denial, so it can adapt.
        message: Option<String>,
    },
    /// End the current turn but stay alive. Ignored between turns.
    Interrupt,
    /// End the current turn and exit.
    Stop,
}

#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("the agent could not be started: {0}")]
    Start(String),
    #[error("the agent is no longer running")]
    Gone,
}

/// One way of running an agent: a scripted mock, a child process, a protocol client.
///
/// # Turns
///
/// The first turn starts with [`StartParams::prompt`], each later one with a
/// [`RunnerInput::UserMessage`]. A turn produces any number of `Event`,
/// `PermissionRequest` and `NativeSession` outputs and then exactly one
/// `TurnEnded`, unless the agent exits first. `Exited` is always the last
/// output, in a turn or between turns.
///
/// Between turns an agent is silent, with one exception: work it left running
/// in the background (a subagent, a watched command) may report `Event`s
/// whenever it has some. They never come with a `TurnEnded` of their own.
///
/// # Items
///
/// Every item a runner starts, it completes before the `TurnEnded` or
/// `Exited` that follows. A tool call cut short by an interrupt, a stop or the
/// death of the agent completes as `failed`, unfinished text as `completed`
/// with what was written so far. Item ids are unique within the session.
///
/// # Permission requests
///
/// A request blocks its tool call, not the stream: other outputs may follow
/// while it waits. Allowed, the call runs. Denied, its item completes as
/// `declined` and the turn goes on. A request lives no longer than its turn:
/// `TurnEnded` and `Exited` void every request still unanswered, and a
/// decision that arrives afterwards is ignored.
///
/// # Interrupt and stop
///
/// `Interrupt` ends the current turn early. The turn still ends with
/// `TurnEnded`, the agent keeps its conversation and takes the next message.
/// `Stop` ends the current turn and the agent: events of the dying turn and
/// even its `TurnEnded` may still arrive, then `Exited(Exit::Ok)`. Both void
/// the pending permission requests as described above. Dropping the
/// [`AgentHandle`] ends the agent at once and without further outputs.
pub trait Runner: Send + Sync + 'static {
    /// The id users select the runner by, for example `mock` or `claude`.
    fn id(&self) -> &str;

    /// The model the agents of this runner use, when it is known up front.
    /// [`RunnerOutput::NativeSession`] reports the one actually in use.
    fn model(&self) -> Option<String> {
        None
    }

    /// Starts an agent. Must be called inside a tokio runtime: implementations
    /// spawn the task (and the child process, if any) that feeds the handle.
    /// Slow setup belongs in that task, not here.
    fn start(&self, params: StartParams) -> Result<AgentHandle, RunnerError>;
}

/// The orchestrator's end of a running agent. Dropping it ends the agent.
#[derive(Debug)]
pub struct AgentHandle {
    outputs: mpsc::Receiver<RunnerOutput>,
    inputs: mpsc::UnboundedSender<RunnerInput>,
}

/// The runner's end of a running agent.
#[derive(Debug)]
pub struct AgentIo {
    /// Closed when the orchestrator dropped the handle: stop working.
    pub outputs: mpsc::Sender<RunnerOutput>,
    /// Yields `None` when the orchestrator dropped the handle.
    pub inputs: mpsc::UnboundedReceiver<RunnerInput>,
}

impl AgentHandle {
    /// A connected handle and runner end.
    pub fn channel() -> (AgentHandle, AgentIo) {
        let (out_tx, out_rx) = mpsc::channel(OUTPUT_BUFFER);
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        (
            AgentHandle {
                outputs: out_rx,
                inputs: in_tx,
            },
            AgentIo {
                outputs: out_tx,
                inputs: in_rx,
            },
        )
    }

    /// The next output, or `None` when the agent ended without saying so.
    pub async fn next_output(&mut self) -> Option<RunnerOutput> {
        self.outputs.recv().await
    }

    pub fn send_message(&self, text: impl Into<String>) -> Result<(), RunnerError> {
        self.send(RunnerInput::UserMessage(text.into()))
    }

    /// Answers a permission request. `message` goes to the agent with a denial.
    pub fn resolve_permission(
        &self,
        request_id: impl Into<String>,
        decision: PermissionDecision,
        message: Option<String>,
    ) -> Result<(), RunnerError> {
        self.send(RunnerInput::PermissionDecision {
            request_id: request_id.into(),
            decision,
            message,
        })
    }

    /// Asks the agent to end its turn and wait for the next message.
    /// `TurnEnded` follows.
    pub fn interrupt(&self) -> Result<(), RunnerError> {
        self.send(RunnerInput::Interrupt)
    }

    /// Asks the agent to end its turn and exit. `Exited` follows.
    pub fn stop(&self) -> Result<(), RunnerError> {
        self.send(RunnerInput::Stop)
    }

    fn send(&self, input: RunnerInput) -> Result<(), RunnerError> {
        self.inputs.send(input).map_err(|_| RunnerError::Gone)
    }
}
