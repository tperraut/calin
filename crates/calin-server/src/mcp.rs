//! The tool endpoint agents call: the Model Context Protocol over HTTP, as
//! far as two tools need it. One JSON-RPC 2.0 message per POST, one JSON
//! response, no session and no stream (the semantics in
//! `calin-proto/src/api.rs`). What the tools do is `calin-core`'s business.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use calin_core::{AgentRef, AgentRepo};
use calin_proto::RepoMode;
use serde_json::{Value, json};

use crate::AppState;
use crate::api::ApiFailure;

/// The protocol revisions this endpoint speaks, newest first. Nothing it
/// uses differs between them.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const LIST_REPOS: &str = "list_repos";
const ATTACH_REPO: &str = "attach_repo";

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Told to the agent's client when it connects.
const INSTRUCTIONS: &str = "Calin's own tools for the agent of one session. Use attach_repo to \
    work in a repository that is not attached to the session yet, and list_repos to see which \
    repositories exist.";

pub(crate) async fn call(
    State(state): State<AppState>,
    Path(token): Path<String>,
    body: Bytes,
) -> Response {
    // The token is the whole credential: without a running agent behind it
    // there is nothing here.
    let Some(agent) = state.manager.agent_for_token(&token) else {
        return ApiFailure::new(StatusCode::NOT_FOUND, "unknown tool endpoint").into_response();
    };
    let message: Value = match serde_json::from_slice(&body) {
        Ok(message) => message,
        Err(_) => return invalid(PARSE_ERROR, "the body is not JSON"),
    };
    // Batches left the protocol in 2025-06-18; nothing sends them here.
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        if message.is_object() && message.get("id").is_some() {
            // A response to a request this server never makes.
            return StatusCode::ACCEPTED.into_response();
        }
        return invalid(INVALID_REQUEST, "expected one JSON-RPC message");
    };
    let Some(id) = message.get("id").filter(|id| !id.is_null()) else {
        // A notification: acknowledged, never answered.
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").unwrap_or(&Value::Null);
    let outcome = match method {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(&state, &agent, params).await,
        _ => Err((METHOD_NOT_FOUND, format!("method not found: {method}"))),
    };
    let reply = match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    Json(reply).into_response()
}

/// A message that cannot be answered by id: the transport says so.
fn invalid(code: i64, message: &str) -> Response {
    let reply = json!({
        "jsonrpc": "2.0", "id": null, "error": { "code": code, "message": message },
    });
    (StatusCode::BAD_REQUEST, Json(reply)).into_response()
}

fn initialize(params: &Value) -> Value {
    let wanted = params.get("protocolVersion").and_then(Value::as_str);
    // The client's revision when this endpoint speaks it, else the newest it does.
    let version = wanted
        .filter(|wanted| VERSIONS.contains(wanted))
        .unwrap_or(VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "calin", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

/// The tool definitions. The descriptions are written for a model: they say
/// when to call the tool, not how it is built.
fn tools() -> Value {
    json!([
        {
            "name": LIST_REPOS,
            "description": "List every git repository Calin knows on this machine: its name, \
                whether it is already attached to your session (and at which path), and \
                whether it can be attached. Call this when you need a repository and are not \
                sure of its name.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            },
        },
        {
            "name": ATTACH_REPO,
            "description": "Attach a repository to your session so that you can work in it, \
                and get the absolute path to work in. Call this whenever the task needs a \
                repository that is not attached yet, instead of searching the disk for a \
                clone or cloning it yourself. It can take a while when the repository has \
                a setup command.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The repository's name, as list_repos shows it.",
                    },
                    "mode": {
                        "type": "string",
                        "enum": ["write", "read"],
                        "description": "write (the default): a private git worktree on this \
                            session's branch, which you may edit. read: the user's own \
                            checkout, for reading only.",
                    },
                },
                "required": ["name"],
                "additionalProperties": false,
            },
        },
    ])
}

/// Runs a tool. A tool that cannot do what was asked still answers, with
/// `isError` and words the model can act on; only a call that names no tool
/// of this server is a protocol error.
async fn call_tool(
    state: &AppState,
    agent: &AgentRef,
    params: &Value,
) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    let outcome = match name {
        LIST_REPOS => list_repos(state, agent).await,
        ATTACH_REPO => attach_repo(state, agent, arguments).await,
        _ => return Err((INVALID_PARAMS, format!("unknown tool: {name}"))),
    };
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

/// What went wrong, for the model. A refusal says why in its own words;
/// anything else is the server's fault and is only logged.
fn failure(err: calin_core::Error) -> String {
    if err.is_refusal() {
        return err.to_string();
    }
    tracing::error!(%err, "a tool call failed");
    "Calin could not do this because of an internal error. Tell the user.".to_owned()
}

fn mode_name(mode: RepoMode) -> &'static str {
    match mode {
        RepoMode::Write => "write",
        RepoMode::Read => "read",
        RepoMode::Live => "live",
    }
}

async fn list_repos(state: &AppState, agent: &AgentRef) -> Result<String, String> {
    let repos = state.manager.agent_repos(agent).await.map_err(failure)?;
    if repos.is_empty() {
        return Ok("Calin knows no repository on this machine yet.".to_owned());
    }
    let mut text = String::from("Repositories Calin knows:\n");
    for AgentRepo { repo, attached } in repos {
        let state = match attached {
            Some(attached) => {
                let primary = if attached.primary { ", primary" } else { "" };
                let mode = mode_name(attached.mode);
                format!(
                    "attached to this session ({mode}{primary}) at {}",
                    attached.path
                )
            }
            None if repo.available => "not attached; call attach_repo to work in it".to_owned(),
            None if repo.cloning => "being cloned; not ready to attach yet".to_owned(),
            None => "not cloned on this machine; the user has to clone it in Calin \
                     before it can be attached"
                .to_owned(),
        };
        text.push_str(&format!("- {}: {state}\n", repo.name));
    }
    Ok(text)
}

async fn attach_repo(
    state: &AppState,
    agent: &AgentRef,
    arguments: &Value,
) -> Result<String, String> {
    let name = arguments.get("name").and_then(Value::as_str).map(str::trim);
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return Err("attach_repo needs the repository's name in `name`.".to_owned());
    };
    let mode = match arguments.get("mode") {
        None | Some(Value::Null) => None,
        Some(Value::String(mode)) if mode == "write" => Some(RepoMode::Write),
        Some(Value::String(mode)) if mode == "read" => Some(RepoMode::Read),
        Some(other) => {
            return Err(format!(
                "`mode` must be \"write\" or \"read\", not {other}."
            ));
        }
    };
    let attached = state.manager.agent_attach_repo(agent, name, mode).await;
    let repo = attached.map_err(|err| match err {
        calin_core::Error::Missing(_) => {
            format!("No repository is named {name}. Call list_repos to see the names.")
        }
        other => failure(other),
    })?;
    Ok(match repo.mode {
        RepoMode::Read => format!(
            "Attached {} read-only. Read it at: {}\nEdits there are refused.",
            repo.name, repo.path
        ),
        _ => format!(
            "Attached {} in {} mode. Work in it at: {}\nIt is a git worktree on this \
             session's branch{}.",
            repo.name,
            mode_name(repo.mode),
            repo.path,
            repo.branch
                .map(|branch| format!(" ({branch})"))
                .unwrap_or_default()
        ),
    })
}
