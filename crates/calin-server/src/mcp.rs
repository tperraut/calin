//! The tool endpoint agents call: the Model Context Protocol over HTTP, as
//! far as a handful of tools need it. One JSON-RPC 2.0 message per POST, one JSON
//! response, no session and no stream (the semantics in
//! `calin-proto/src/api.rs`). What the tools do is `calin-core`'s business.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::header::HOST;
use axum::http::request::Parts;
use axum::http::{Extensions, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use calin_core::{AgentRef, AgentRepo, NoteOutcome};
use calin_proto::RepoMode;
use serde_json::{Value, json};

use crate::AppState;
use crate::api::ApiFailure;
use crate::auth::peer_ip;
use crate::guard::is_loopback_authority;

/// The protocol revisions this endpoint speaks, newest first. Nothing it
/// uses differs between them.
const VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const LIST_REPOS: &str = "list_repos";
const ATTACH_REPO: &str = "attach_repo";
const DISPATCH: &str = "dispatch";
const SEND_MESSAGE: &str = "send_message";
const REPORT: &str = "report";
const ASK_USER: &str = "ask_user";

// JSON-RPC 2.0 error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Told to the agent's client when it connects.
const INSTRUCTIONS: &str = "Calin's own tools for the agent of one session. Use attach_repo to \
    work in a repository that is not attached to the session yet, and list_repos to see which \
    repositories exist. Use dispatch to hand an independent piece of work to a child session, \
    report (in a child session) to hand the result back, send_message for a note to another \
    agent or to the user, and ask_user when only the user can decide. Messages that start with \
    [Calin] are deliveries from Calin (a report, a note, an answer), not from the user.";

/// What a reverse proxy adds to a request it passes on. One of them on a
/// request means it did not start on this machine.
const FORWARDING_HEADERS: [&str; 5] = [
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-real-ip",
];

/// Whether a request came straight from this machine, where the agents run
/// (remote-access spec, section 1): a loopback peer and no forwarding
/// header. Agents are given a loopback URL, so the Host is loopback too; a
/// proxy that passes the public name on fails that as well.
fn came_straight_from_loopback(extensions: &Extensions, headers: &HeaderMap) -> bool {
    let loopback_peer = peer_ip(extensions).is_some_and(|peer| peer.to_canonical().is_loopback());
    let loopback_host = headers
        .get(HOST)
        .and_then(|host| host.to_str().ok())
        .is_some_and(is_loopback_authority);
    loopback_peer
        && loopback_host
        && FORWARDING_HEADERS
            .iter()
            .all(|name| !headers.contains_key(*name))
}

pub(crate) async fn call(
    State(state): State<AppState>,
    Path(token): Path<String>,
    request: Parts,
    body: Bytes,
) -> Response {
    // The token is the whole credential: without a running agent behind it
    // there is nothing here. Neither is there for a request from elsewhere,
    // which gets the very same answer.
    let agent = Some(())
        .filter(|()| came_straight_from_loopback(&request.extensions, &request.headers))
        .and_then(|()| state.manager.agent_for_token(&token));
    let Some(agent) = agent else {
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
        {
            "name": DISPATCH,
            "description": "Start a child session: another agent that works on its own, with \
                a fresh context, on its own branch of each repository it is given (the \
                branch starts at your current commit there). Use it for an independent \
                piece of work that can proceed in parallel with yours, or that needs a fresh \
                context. The child knows nothing of this conversation, so the brief must \
                stand on its own. It returns the child's id at once; the child's report is \
                delivered to you later as a message that starts your next turn, so after \
                dispatching finish what you can do yourself and end your turn instead of \
                waiting or polling. At most 4 children run at a time. Uncommitted changes \
                in your worktree are not given to the child: commit first if it needs them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": {
                        "type": "string",
                        "description": "A short name for the child session, on one line. \
                            It also names the child's branch.",
                    },
                    "brief": {
                        "type": "string",
                        "description": "Everything the child needs to do the work without \
                            asking: the goal, the files or areas involved, constraints, \
                            what done looks like, and what to report back.",
                    },
                    "repos": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Names of the repositories to give the child: a \
                            subset of the repositories attached to your session in write \
                            mode. All of those when omitted.",
                    },
                },
                "required": ["title", "brief"],
                "additionalProperties": false,
            },
        },
        {
            "name": SEND_MESSAGE,
            "description": "Send a note to another participant: `parent` (the agent that \
                dispatched this session, when it is a child session), the id of a child \
                session you dispatched, or `user`. A note to an agent is delivered as its \
                next input and may start a turn there. A note to the user only shows in the \
                transcript and does not wait for an answer: use ask_user for a question. A \
                child's final result goes through report, not through this.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "`parent`, `user`, or the id of a child session as \
                            dispatch returned it.",
                    },
                    "text": { "type": "string", "description": "The note." },
                },
                "required": ["to", "text"],
                "additionalProperties": false,
            },
        },
        {
            "name": REPORT,
            "description": "In a child session only: hand your result to the agent that \
                dispatched you. Call it once when your work is done or you cannot go on, \
                with what you did, what you verified and what is left open, then end your \
                turn. Commit your work first: the report is delivered with your branch \
                names, and only commits can be merged from them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": {
                        "type": "string",
                        "description": "The result, written for an agent that has not \
                            seen your work.",
                    },
                },
                "required": ["summary"],
                "additionalProperties": false,
            },
        },
        {
            "name": ASK_USER,
            "description": "Ask the user a question when you are blocked on a decision only \
                they can make: a choice between real alternatives, something only they \
                know, or their consent to something that cannot be undone. The session \
                waits for them. The tool returns at once and you must then end your turn: \
                the answer arrives as your next message. One question at a time. Do not \
                use it for what you can find out or decide yourself.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question, with what the user needs to know to \
                            answer it.",
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Suggested answers, when there are natural choices. \
                            The user may still answer in their own words.",
                    },
                },
                "required": ["question"],
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
        DISPATCH => dispatch(state, agent, arguments).await,
        SEND_MESSAGE => send_message(state, agent, arguments).await,
        REPORT => report(state, agent, arguments).await,
        ASK_USER => ask_user(state, agent, arguments).await,
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

/// A required text argument: `Err` with what to tell the model when it is
/// absent, not a string or blank.
fn text_argument<'a>(arguments: &'a Value, tool: &str, key: &str) -> Result<&'a str, String> {
    match arguments.get(key).and_then(Value::as_str).map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text),
        _ => Err(format!("{tool} needs `{key}`: a string that is not blank.")),
    }
}

/// An optional list of strings. `Err` when the argument is anything else.
fn list_argument(arguments: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    let wrong = || format!("`{key}` must be a list of strings.");
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().map(str::to_owned).ok_or_else(wrong))
            .collect::<Result<Vec<String>, String>>()
            .map(Some),
        Some(_) => Err(wrong()),
    }
}

fn status_name(status: calin_proto::SessionStatus) -> String {
    format!("{status:?}").to_lowercase()
}

async fn dispatch(state: &AppState, agent: &AgentRef, arguments: &Value) -> Result<String, String> {
    let title = text_argument(arguments, DISPATCH, "title")?;
    let brief = text_argument(arguments, DISPATCH, "brief")?;
    let repos = list_argument(arguments, "repos")?;
    let dispatched = state.manager.agent_dispatch(agent, title, brief, repos);
    let dispatched = dispatched.await.map_err(failure)?;
    let mut text = format!(
        "Dispatched child session \"{}\" (id {}).\n",
        dispatched.session.title, dispatched.session.id
    );
    if dispatched.repos.is_empty() {
        text.push_str("It was given no repository: it works in an empty folder of its own.\n");
    } else {
        let starts: Vec<String> = dispatched
            .repos
            .iter()
            .map(|repo| format!("{} (commit {})", repo.name, short(&repo.commit)))
            .collect();
        text.push_str(&format!(
            "It works on a branch of its own that starts at your current commit in: {}.\n",
            starts.join(", ")
        ));
    }
    let dirty: Vec<&str> = dispatched
        .repos
        .iter()
        .filter(|repo| repo.dirty)
        .map(|repo| repo.name.as_str())
        .collect();
    if !dirty.is_empty() {
        text.push_str(&format!(
            "Note: your worktree of {} has uncommitted changes. The child does not have \
             them, because it started from your last commit. If it needs them, commit them \
             and tell the child with send_message to merge your branch; next time, commit \
             before you dispatch.\n",
            dirty.join(", ")
        ));
    }
    text.push_str(
        "Its report will be delivered to you as a message when it finishes. Do not wait or \
         poll for it: finish what you can do yourself, then end your turn.",
    );
    Ok(text)
}

/// A commit id, as short as people write them.
fn short(commit: &str) -> &str {
    commit.get(..10).unwrap_or(commit)
}

async fn send_message(
    state: &AppState,
    agent: &AgentRef,
    arguments: &Value,
) -> Result<String, String> {
    let to = text_argument(arguments, SEND_MESSAGE, "to")?;
    let text = text_argument(arguments, SEND_MESSAGE, "text")?;
    let sent = state.manager.agent_send_message(agent, to, text).await;
    Ok(match sent.map_err(failure)? {
        NoteOutcome::Shown => "Noted for the user. It shows in the transcript; the user is not \
            asked to answer. Use ask_user when you need an answer."
            .to_owned(),
        NoteOutcome::Delivered => {
            "Sent. It is delivered to that agent as its next input.".to_owned()
        }
        NoteOutcome::Unread(status) => format!(
            "Recorded, but that session is {}: nobody will act on the note.",
            status_name(status)
        ),
    })
}

async fn report(state: &AppState, agent: &AgentRef, arguments: &Value) -> Result<String, String> {
    let summary = text_argument(arguments, REPORT, "summary")?;
    let kept = state.manager.agent_report(agent, summary).await;
    kept.map_err(failure)?;
    Ok(
        "Recorded. It is delivered to the agent that dispatched you, with your branch names, \
         when this turn ends. End your turn now."
            .to_owned(),
    )
}

async fn ask_user(state: &AppState, agent: &AgentRef, arguments: &Value) -> Result<String, String> {
    let question = text_argument(arguments, ASK_USER, "question")?;
    let options = list_argument(arguments, "options")?.unwrap_or_default();
    let asked = state.manager.agent_ask_user(agent, question, options).await;
    let id = asked.map_err(failure)?;
    Ok(format!(
        "The question was put to the user (question {id}). End your turn now: the answer \
         arrives as your next message."
    ))
}
