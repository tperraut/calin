//! Workflows: the file format, its validation, templates and the engine
//! (`docs/specs/2026-11-27-workflows.md`, sections 2 to 5).
//!
//! A [`Workflow`] is a graph of typed [`Node`]s joined by [`Edge`]s from a node's port to a
//! node. [`parse`] reads a YAML file and checks every rule, returning all [`Problems`] at once;
//! [`quick`] is the built-in workflow. The engine is two pure functions: [`start`] and
//! [`advance`] take and return an owned, serializable [`RunState`] plus [`Action`]s for the
//! orchestrator, which answers each action with a [`NodeResult`]. Nothing here does I/O.
//!
//! ```
//! use calin_workflow::{Action, Context, NodeResult, advance, parse, start};
//!
//! let yaml = r#"
//! version: 1
//! name: Implement then test
//! nodes:
//!   - { id: start, type: start }
//!   - { id: implement, type: agent, prompt: "{{task}}" }
//!   - { id: tests, type: check, command: "cargo test" }
//!   - { id: ok, type: end }
//! edges:
//!   - { from: start, to: implement }
//!   - { from: implement.done, to: tests }
//!   - { from: tests.pass, to: ok }
//! "#;
//! let workflow = parse("implement", yaml).expect("valid");
//! let context = Context { task: "Add a flag".into(), ..Context::default() };
//!
//! let (state, actions) = start(workflow, context);
//! assert!(matches!(&actions[0], Action::RunAgent { prompt, .. } if prompt == "Add a flag"));
//!
//! let done = NodeResult::new("implement", "done").with("summary", "Added it.");
//! let (state, actions) = advance(&state, done).expect("implement is active");
//! assert_eq!(actions[0].node(), Some("tests"));
//!
//! let (state, actions) = advance(&state, NodeResult::new("tests", "pass")).expect("active");
//! assert!(matches!(&actions[0], Action::Finish(f) if f.status.as_str() == "success"));
//! assert!(state.finished().is_some());
//! ```
//!
//! # Readings of the spec
//!
//! Where the spec is silent or ambiguous this crate takes the stricter reading:
//!
//! - Every cycle must leave a `loop` node through `repeat`. A cycle that passes a loop only
//!   through `exhausted` is unbounded (that port can be taken any number of times), so it is
//!   rejected even though it "passes through a loop node".
//! - No edge may lead into the `start` node.
//! - `name` is required and non-empty; `edges` and `nodes` are required lists.
//! - Required text fields and templates must not be blank; optional ones (`title`, `runner`,
//!   `model`, `repo`, `command`) must not be blank when present.
//! - `options`, a branch's `repos` and `verdicts` must not be empty lists when present, and
//!   their entries must be non-empty and unique.
//! - A `parallel` branch id follows the node id rule.
//! - `timeout_seconds` is an integer from 1 to 86,400; `budget_usd` a positive number.
//! - Failure ports are decided by name alone, so a verdict called `reject` or `fail` ends the
//!   run as failed when it is taken unwired.
//! - Only YAML 1.2 booleans (`true`/`false`) are booleans; `yes`/`no` are strings. Scalars must
//!   have the field's type: `prompt: 42` is an error, not the text "42".
//! - Whitespace is ignored only just inside the braces: `{{ task }}` is fine, `{{nodes. a.b}}`
//!   is malformed.
//! - A node's outputs are replaced as a whole by each run, so a value missing from the newest
//!   run renders empty rather than showing an older run's.
//! - A verdict line is the last line of the form `VERDICT: <rest>`; when `<rest>` is not one of
//!   the node's verdicts the answer has none, even if an earlier line named a valid one.
//! - The workflow id (file name) follows the node id rule, and `quick` is reserved.

mod engine;
mod model;
mod parse;
mod problem;
mod raw;
mod template;
mod validate;
mod verdict;

pub use engine::{
    Action, CheckCommand, EngineError, Finish, FinishReason, MAX_STEPS, NodeResult, NodeSnapshot,
    NodeState, ParallelBranch, RunSnapshot, RunState, TakenEdge, advance, start,
};
pub use model::{
    AgentNode, Branch, CheckNode, Edge, EndStatus, FAILURE_PORTS, FORMAT_VERSION, Graph, GraphNode,
    MAX_ID_LEN, Node, NodeKind, NodeType, Port, Workflow, is_failure_port, quick,
};
pub use parse::{DEFAULT_TIMEOUT_SECONDS, parse};
pub use problem::{Problem, Problems, Subject};
pub use template::{Context, Outputs, Segment, Template, TemplateError};
pub use validate::{MAX_LOOP, MAX_TIMEOUT_SECONDS, PARALLEL_BRANCHES, is_valid_id};
pub use verdict::{FAILED_PORT, VERDICT_KEYWORD, Verdict, extract_verdict, verdict_instruction};

#[cfg(test)]
mod tests;
