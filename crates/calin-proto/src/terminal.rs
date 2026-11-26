//! The terminal socket: `GET /api/v1/sessions/{id}/terminals/ws`.
//!
//! Query parameters: `session_repo_id` (optional; the directory the shell
//! starts in, the session's working directory when absent), `cols`, `rows`.
//!
//! One socket is one shell. Binary frames carry raw terminal bytes in both
//! directions. Text frames are JSON: [`TermClientMsg`] from the client,
//! [`TermServerMsg`] from the server. Closing the socket kills the shell.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export)]
pub enum TermClientMsg {
    Resize { cols: u16, rows: u16 },
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export)]
pub enum TermServerMsg {
    /// The shell exited. The server closes the socket after this frame.
    Exit { code: Option<i32> },
    /// The shell could not be started. The server closes the socket after this frame.
    Error { message: String },
}
