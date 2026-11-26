use calin_proto::EventBody;
use tokio::sync::mpsc;

/// Outputs buffered between an agent and the orchestrator. Small on purpose:
/// a full buffer is the backpressure that keeps an agent from outrunning the log.
const OUTPUT_BUFFER: usize = 64;

/// What a runner needs to start an agent.
#[derive(Clone, Debug)]
pub struct StartParams {
    pub session_id: String,
    /// Assigned by the orchestrator; stable across restarts of the same agent.
    pub agent_id: String,
    /// The user text of the first turn.
    pub prompt: String,
    /// True when this agent ran before in the session and is being started
    /// again, so the runner should continue its conversation.
    pub resume: bool,
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
    /// Starts another turn. The orchestrator sends it only between turns.
    UserMessage(String),
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
/// The first turn starts with `StartParams::prompt`, each later one with a
/// `RunnerInput::UserMessage`. A turn produces any number of `Event`s and then
/// exactly one `TurnEnded`, unless the agent exits first.
pub trait Runner: Send + Sync + 'static {
    /// The id users select the runner by, for example `mock` or `claude`.
    fn id(&self) -> &str;

    /// The model the agents of this runner use, when it is known up front.
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

    /// Asks the agent to end its turn and exit. `Exited` follows.
    pub fn stop(&self) -> Result<(), RunnerError> {
        self.send(RunnerInput::Stop)
    }

    fn send(&self, input: RunnerInput) -> Result<(), RunnerError> {
        self.inputs.send(input).map_err(|_| RunnerError::Gone)
    }
}
