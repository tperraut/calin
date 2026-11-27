//! The terminal socket at `/api/v1/sessions/{id}/terminals/ws`: one socket,
//! one shell. The protocol is documented in `calin-proto/src/terminal.rs`.
//!
//! Two tasks per socket, so neither direction can hold up the other: one
//! reads client frames into the terminal, the other drains the terminal's
//! output into the socket.

use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Extension;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use calin_proto::{TermClientMsg, TermServerMsg};
use calin_term::{Terminal, TerminalConfig};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::AppState;
use crate::api::{ApiFailure, ApiQuery};
use crate::auth::{Caller, close_with_credential};

/// Shells one session may have open at a time. Each one is a process tree
/// and three threads, and nothing else bounds what a client can open.
pub(crate) const MAX_TERMINALS_PER_SESSION: usize = 8;
/// A client that accepts nothing for this long is gone.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// How long to wait for a killed shell to be gone before giving its slot back.
const KILL_TIMEOUT: Duration = Duration::from_secs(5);
/// Room for a large paste in one frame.
const MAX_CLIENT_FRAME: usize = 1024 * 1024;
/// Tells a program in the terminal which session it belongs to.
const ENV_SESSION_ID: &str = "CALIN_SESSION_ID";

/// The program terminals run instead of the user's shell.
#[derive(Clone, Debug)]
pub(crate) struct TerminalCommand {
    pub(crate) program: OsString,
    pub(crate) args: Vec<OsString>,
}

/// How many terminals each session has open.
#[derive(Clone, Default)]
pub(crate) struct Terminals {
    open: Arc<Mutex<HashMap<String, usize>>>,
}

/// One open terminal's place in its session's count, given back on drop.
struct Slot {
    terminals: Terminals,
    session_id: String,
}

impl Terminals {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, usize>> {
        // Every critical section is one read or one update of a counter.
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `None` when the session already has as many terminals as it may.
    fn acquire(&self, session_id: &str) -> Option<Slot> {
        let mut open = self.lock();
        let count = open.entry(session_id.to_owned()).or_insert(0);
        if *count >= MAX_TERMINALS_PER_SESSION {
            return None;
        }
        *count += 1;
        Some(Slot {
            terminals: self.clone(),
            session_id: session_id.to_owned(),
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = self.terminals.lock();
        if let Some(count) = open.get_mut(&self.session_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                open.remove(&self.session_id);
            }
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct TerminalQuery {
    /// The repo whose root the shell starts in; the session's working
    /// directory when absent.
    session_repo_id: Option<String>,
    cols: Option<u16>,
    rows: Option<u16>,
}

pub(crate) async fn upgrade(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    Path(session_id): Path<String>,
    ApiQuery(query): ApiQuery<TerminalQuery>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    // Before the upgrade: an unknown session or repo is an ordinary error
    // response, which a WebSocket client sees as a failed handshake.
    let repo = query.session_repo_id.as_deref().filter(|id| !id.is_empty());
    let dir = match state.manager.terminal_dir(&session_id, repo).await {
        Ok(dir) => dir,
        Err(err) => return ApiFailure::from(err).into_response(),
    };
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => {
            return ApiFailure::new(StatusCode::BAD_REQUEST, rejection.body_text()).into_response();
        }
    };
    let mut config = TerminalConfig::new(dir).env(ENV_SESSION_ID, &session_id);
    // The library's defaults stand in for a size the client did not give.
    let (cols, rows) = (
        query.cols.unwrap_or(config.cols),
        query.rows.unwrap_or(config.rows),
    );
    config = config.size(cols, rows);
    if let Some(command) = state.terminal_command.as_deref() {
        config = config.command(&command.program, &command.args);
    }
    // Counted here rather than on the socket's own task, so two handshakes
    // racing for the last slot cannot both get it. A socket that never
    // finishes its upgrade drops the slot with the callback.
    let slot = state.terminals.acquire(&session_id);
    upgrade
        .max_message_size(MAX_CLIENT_FRAME)
        .on_upgrade(move |socket| serve(socket, state, caller, session_id, config, slot))
}

async fn serve(
    socket: WebSocket,
    state: AppState,
    caller: Caller,
    session_id: String,
    config: TerminalConfig,
    slot: Option<Slot>,
) {
    let (mut sink, frames) = socket.split();
    // The refusal is a frame, not a failed handshake: the client asked for
    // something that exists and may ask again once a terminal is closed.
    let Some(slot) = slot else {
        let message = format!(
            "this session already has {MAX_TERMINALS_PER_SESSION} terminals open; close one first"
        );
        refuse(sink, message).await;
        return;
    };
    // Starting the program forks, which is not for a runtime thread.
    let spawned = tokio::task::spawn_blocking(move || Terminal::spawn(&config)).await;
    let (terminal, mut output) = match spawned {
        Ok(Ok(started)) => started,
        Ok(Err(err)) => return refuse(sink, err.to_string()).await,
        Err(err) => {
            tracing::error!(%err, "starting a terminal panicked");
            return refuse(sink, "the terminal could not be started".to_owned()).await;
        }
    };
    let terminal = Arc::new(terminal);
    // Cancelled when the server shuts down, the client is gone, or the
    // sign-in the socket was opened with has ended.
    let closing = state.shutdown.child_token();
    close_with_credential(&state, &caller, &closing);
    let reader = tokio::spawn(read(frames, Arc::clone(&terminal), closing.clone()));

    let exited = loop {
        let chunk = tokio::select! {
            () = closing.cancelled() => break false,
            chunk = output.recv() => chunk,
        };
        // The output ends once the program has exited and all of it was read.
        let Some(bytes) = chunk else {
            break true;
        };
        if !send(&mut sink, Message::Binary(bytes.into())).await {
            break false;
        }
    };
    if exited {
        let code = terminal.wait().await;
        send_json(&mut sink, &TermServerMsg::Exit { code }).await;
    }
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, sink.close()).await;
    closing.cancel();
    reader.abort();
    let _ = reader.await;

    // Closing the socket kills the shell and everything it left running.
    terminal.kill();
    drop(output);
    if tokio::time::timeout(KILL_TIMEOUT, terminal.wait())
        .await
        .is_err()
    {
        tracing::warn!(session = %session_id, pid = terminal.pid(), "a terminal did not end when killed");
    }
    drop(slot);
}

/// Client frames into the terminal, until the client goes away.
async fn read(
    mut frames: SplitStream<WebSocket>,
    terminal: Arc<Terminal>,
    closing: CancellationToken,
) {
    loop {
        let frame = tokio::select! {
            () = closing.cancelled() => return,
            frame = frames.next() => frame,
        };
        match frame {
            Some(Ok(Message::Binary(bytes))) => {
                // Waits while the program does not read its input. A program
                // that has exited takes none; the other task reports the exit.
                tokio::select! {
                    () = closing.cancelled() => return,
                    _ = terminal.write(bytes.to_vec()) => {}
                }
            }
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<TermClientMsg>(&text) {
                Ok(TermClientMsg::Resize { cols, rows }) => {
                    if let Err(err) = terminal.resize(cols, rows) {
                        tracing::debug!(%err, "could not resize a terminal");
                    }
                }
                Err(err) => tracing::debug!(%err, "ignoring an unreadable terminal frame"),
            },
            // The protocol library answers pings itself.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {}
            Some(Ok(Message::Close(_)) | Err(_)) | None => break,
        }
    }
    closing.cancel();
}

/// Says why there is no shell, then closes the socket.
async fn refuse(mut sink: SplitSink<WebSocket, Message>, message: String) {
    send_json(&mut sink, &TermServerMsg::Error { message }).await;
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, sink.close()).await;
}

async fn send_json(sink: &mut SplitSink<WebSocket, Message>, message: &TermServerMsg) -> bool {
    match serde_json::to_string(message) {
        Ok(text) => send(sink, Message::Text(text.into())).await,
        Err(err) => {
            tracing::error!(%err, "could not encode a terminal frame");
            false
        }
    }
}

/// False when the client is gone or does not read.
async fn send(sink: &mut SplitSink<WebSocket, Message>, message: Message) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, sink.send(message)).await,
        Ok(Ok(()))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_has_a_bounded_number_of_slots_and_gets_them_back() {
        let terminals = Terminals::default();
        let mut slots: Vec<Slot> = (0..MAX_TERMINALS_PER_SESSION)
            .map(|_| terminals.acquire("s1").expect("a free slot"))
            .collect();
        assert!(terminals.acquire("s1").is_none());
        // Another session has slots of its own.
        let other = terminals.acquire("s2").expect("a free slot");
        slots.pop();
        slots.push(
            terminals
                .acquire("s1")
                .expect("the slot that was given back"),
        );
        assert!(terminals.acquire("s1").is_none());
        drop((slots, other));
        // Nothing is kept for a session without terminals.
        assert!(terminals.lock().is_empty());
    }
}
