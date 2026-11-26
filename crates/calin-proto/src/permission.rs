use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::ToolKind;

/// An agent's request to use a tool, waiting for a decision.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct PermissionRequest {
    /// Unique within the session.
    pub id: String,
    pub tool_name: String,
    pub tool_kind: ToolKind,
    /// One line describing the call, for example the command or the file path.
    pub title: String,
    pub input: serde_json::Value,
    /// The tool item this request belongs to, when the runner knows it.
    pub item_id: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum PermissionDecision {
    Allow,
    Deny,
}

/// Who answered a permission request.
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[ts(export)]
pub enum DecisionSource {
    User,
    /// The session's permission mode answered without asking.
    Policy,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct ResolvePermissionRequest {
    pub decision: PermissionDecision,
    /// Shown to the agent with a denial, so it can adapt.
    #[serde(default)]
    #[ts(optional)]
    pub message: Option<String>,
}
