//! The runner interface and its implementations.
//!
//! A runner starts an agent for a session and speaks in `calin_proto::EventBody`
//! values plus a few lifecycle signals. It never touches the store and never
//! assigns event ids, sequence numbers or timestamps: that is the
//! orchestrator's job (`calin-core`).

mod claude;
mod mock;
mod prose;
mod registry;
mod runner;

pub use claude::{CLAUDE_RUNNER_ID, ClaudeConfig, ClaudeRunner};
pub use mock::{MOCK_RUNNER_ID, MockConfig, MockRunner};
pub use registry::{RegistryConfig, RunnerRegistry};
pub use runner::{
    AgentHandle, AgentIo, DirGrant, Exit, PermissionAsk, Runner, RunnerError, RunnerInput,
    RunnerOutput, StartParams, SubagentStart, ToolAccess,
};
