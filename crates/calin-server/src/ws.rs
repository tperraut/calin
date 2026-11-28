//! The WebSocket at `/api/v1/ws`. The protocol is documented in
//! `calin-proto/src/ws.rs`.
//!
//! One task per subscription copies from the store into a bounded queue, and
//! one writer task drains the queue into the socket. The store never waits
//! for a client: a client that cannot keep up overflows its live buffer in
//! the store, is disconnected, and replays from its last sequence number.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use calin_core::{Live, Store};
use calin_proto::{ClientMsg, ServerMsg, TOPIC_SESSIONS, parse_session_topic};
use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::AppState;
use crate::api::ApiFailure;
use crate::auth::{Caller, close_with_credential};
use crate::push::{ViewGuard, Viewers};

/// Frames queued for one client. Replay waits when it is full.
const OUTBOUND_BUFFER: usize = 256;
/// A client that accepts nothing for this long is gone.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
/// Client frames are small JSON objects.
const MAX_CLIENT_FRAME: usize = 64 * 1024;
const MAX_TOPICS: usize = 256;

pub(crate) async fn upgrade(
    State(state): State<AppState>,
    Extension(caller): Extension<Caller>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    match upgrade {
        Ok(upgrade) => upgrade
            .max_message_size(MAX_CLIENT_FRAME)
            .on_upgrade(move |socket| serve(socket, state, caller)),
        Err(rejection) => {
            ApiFailure::new(StatusCode::BAD_REQUEST, rejection.body_text()).into_response()
        }
    }
}

async fn serve(socket: WebSocket, state: AppState, caller: Caller) {
    let (sink, mut frames) = socket.split();
    let (out, outbound) = mpsc::channel(OUTBOUND_BUFFER);
    // Cancelled when the server shuts down or this connection has to end,
    // which includes the end of the sign-in it was opened with.
    let closing = state.shutdown.child_token();
    close_with_credential(&state, &caller, &closing);
    let writer = tokio::spawn(write(sink, outbound, closing.clone()));
    let mut connection = Connection::new(&state, &caller, out, closing.clone());

    loop {
        let frame = tokio::select! {
            () = closing.cancelled() => break,
            frame = frames.next() => frame,
        };
        let ControlFlow::Continue(reply) = connection.answer(frame) else {
            break;
        };
        // A full queue here means the client sends but does not read.
        if let Some(reply) = reply
            && connection.out.try_send(reply).is_err()
        {
            break;
        }
    }

    closing.cancel();
    for (_, task) in connection.topics.drain() {
        task.abort();
    }
    let _ = writer.await;
}

/// An error frame. `topic` names the subscription the refused frame was
/// about, so a client with several pending knows which one failed.
fn error(message: String, topic: Option<&str>) -> ServerMsg {
    ServerMsg::Error {
        message,
        topic: topic.map(str::to_owned),
    }
}

fn is_topic(topic: &str) -> bool {
    topic == TOPIC_SESSIONS || parse_session_topic(topic).is_some()
}

/// Drains the queue into the socket until the connection closes.
async fn write(
    mut sink: SplitSink<WebSocket, Message>,
    mut outbound: mpsc::Receiver<ServerMsg>,
    closing: CancellationToken,
) {
    loop {
        let message = tokio::select! {
            () = closing.cancelled() => break,
            message = outbound.recv() => message,
        };
        let Some(message) = message else {
            break;
        };
        let text = match serde_json::to_string(&message) {
            Ok(text) => text,
            Err(err) => {
                tracing::error!(%err, "could not encode a server frame");
                break;
            }
        };
        let sent = tokio::select! {
            () = closing.cancelled() => break,
            sent = tokio::time::timeout(SEND_TIMEOUT, sink.send(Message::Text(text.into()))) => sent,
        };
        if !matches!(sent, Ok(Ok(()))) {
            break;
        }
    }
    // Wakes the reader and the forwarders if the writer is the one giving up.
    closing.cancel();
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, sink.close()).await;
}

struct Connection {
    store: Store,
    out: mpsc::Sender<ServerMsg>,
    closing: CancellationToken,
    /// The forwarder of each subscribed topic.
    topics: HashMap<String, JoinHandle<()>>,
    /// Aborted forwarders that may not have stopped yet. A new subscription
    /// to the same topic waits for them so frames cannot interleave.
    retired: HashMap<String, JoinHandle<()>>,
    /// Who has which session open, for the push notifications it spares.
    viewers: Viewers,
    /// The signed-in identity; `None` in local mode, where nobody is notified.
    identity: Option<String>,
    /// One per subscribed session topic; dropped with the subscription or
    /// the connection.
    views: HashMap<String, ViewGuard>,
}

impl Connection {
    fn new(
        state: &AppState,
        caller: &Caller,
        out: mpsc::Sender<ServerMsg>,
        closing: CancellationToken,
    ) -> Self {
        let identity = match caller {
            Caller::SignedIn(who) => Some(who.identity.to_string()),
            Caller::Local | Caller::Anonymous => None,
        };
        Self {
            store: state.manager.store().clone(),
            out,
            closing,
            topics: HashMap::new(),
            retired: HashMap::new(),
            viewers: state.viewers.clone(),
            identity,
            views: HashMap::new(),
        }
    }

    /// Reads one frame off the socket: `Break` when the connection ends,
    /// else what to answer right away, if anything.
    fn answer(
        &mut self,
        frame: Option<Result<Message, axum::Error>>,
    ) -> ControlFlow<(), Option<ServerMsg>> {
        let reply = match frame {
            Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMsg>(&text) {
                Ok(message) => self.handle(message),
                Err(err) => Some(error(format!("unreadable frame: {err}"), None)),
            },
            Some(Ok(Message::Binary(_))) => {
                Some(error("binary frames are not supported".to_owned(), None))
            }
            // The protocol library answers pings itself.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => None,
            Some(Ok(Message::Close(_)) | Err(_)) | None => return ControlFlow::Break(()),
        };
        ControlFlow::Continue(reply)
    }

    /// Handles a client frame. Returns what to answer right away, if anything.
    fn handle(&mut self, message: ClientMsg) -> Option<ServerMsg> {
        match message {
            ClientMsg::Ping => Some(ServerMsg::Pong),
            ClientMsg::Unsubscribe { topic } => {
                if !is_topic(&topic) {
                    return Some(error(format!("unknown topic {topic}"), Some(&topic)));
                }
                self.views.remove(&topic);
                // Leaving a topic that was never joined is not an error.
                if let Some(task) = self.topics.remove(&topic) {
                    task.abort();
                    self.retired.retain(|_, retired| !retired.is_finished());
                    self.retired.insert(topic, task);
                }
                None
            }
            ClientMsg::Subscribe { topic, after_seq } => self.subscribe(topic, after_seq),
        }
    }

    fn subscribe(&mut self, topic: String, after_seq: Option<u64>) -> Option<ServerMsg> {
        let source = if topic == TOPIC_SESSIONS {
            Source::Sessions
        } else if let Some(session_id) = parse_session_topic(&topic) {
            Source::Session {
                session_id: session_id.to_owned(),
                after_seq: after_seq.unwrap_or(0),
            }
        } else {
            return Some(error(format!("unknown topic {topic}"), Some(&topic)));
        };
        // Subscribing to a topic again restarts it from the new position.
        let previous = self
            .topics
            .remove(&topic)
            .or_else(|| self.retired.remove(&topic));
        if self.topics.len() >= MAX_TOPICS {
            self.views.remove(&topic);
            let message = format!("too many subscriptions on one socket (limit {MAX_TOPICS})");
            return Some(error(message, Some(&topic)));
        }
        if let (Source::Session { session_id, .. }, Some(identity)) = (&source, &self.identity) {
            let view = self.viewers.watch(identity, session_id);
            self.views.insert(topic.clone(), view);
        }
        let forwarder = Forwarder {
            store: self.store.clone(),
            topic: topic.clone(),
            out: self.out.clone(),
            closing: self.closing.clone(),
        };
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                // Its last frame must be out before the replay starts.
                previous.abort();
                let _ = previous.await;
            }
            // A send only fails when the connection is closing.
            let _ = forwarder.run(source).await;
        });
        self.topics.insert(topic, task);
        None
    }
}

enum Source {
    Sessions,
    Session { session_id: String, after_seq: u64 },
}

/// The receiving side of the connection is gone.
struct Gone;

struct Forwarder {
    store: Store,
    topic: String,
    out: mpsc::Sender<ServerMsg>,
    closing: CancellationToken,
}

impl Forwarder {
    async fn run(self, source: Source) -> Result<(), Gone> {
        match source {
            Source::Sessions => self.sessions().await,
            Source::Session {
                session_id,
                after_seq,
            } => self.session(&session_id, after_seq).await,
        }
    }

    async fn session(&self, session_id: &str, after_seq: u64) -> Result<(), Gone> {
        let subscription = match self.store.subscribe(session_id, after_seq).await {
            Ok(subscription) => subscription,
            Err(err) => return self.refuse(err).await,
        };
        let event = |event| ServerMsg::Event {
            topic: self.topic.clone(),
            event,
        };
        let backlog = subscription.backlog.into_iter().map(event);
        self.follow(backlog, subscription.head_seq, subscription.live, |live| {
            event((*live).clone())
        })
        .await
    }

    async fn sessions(&self) -> Result<(), Gone> {
        let (sessions, updates) = match self.store.subscribe_sessions().await {
            Ok(subscribed) => subscribed,
            Err(err) => return self.refuse(err).await,
        };
        let upserted = |session| ServerMsg::SessionUpserted { session };
        self.follow(sessions.into_iter().map(upserted), 0, updates, upserted)
            .await
    }

    /// Sends the backlog, then `subscribed` at `head_seq`, then each live
    /// item until the client falls behind or goes away.
    async fn follow<T: Clone>(
        &self,
        backlog: impl IntoIterator<Item = ServerMsg>,
        head_seq: u64,
        mut live: Live<T>,
        frame: impl Fn(T) -> ServerMsg,
    ) -> Result<(), Gone> {
        for message in backlog {
            self.send(message).await?;
        }
        self.send(ServerMsg::Subscribed {
            topic: self.topic.clone(),
            head_seq,
        })
        .await?;
        loop {
            match live.recv().await {
                Ok(item) => self.send(frame(item)).await?,
                Err(end) => return self.disconnect(format!("{end:?}")),
            }
        }
    }

    async fn send(&self, message: ServerMsg) -> Result<(), Gone> {
        self.out.send(message).await.map_err(|_| Gone)
    }

    /// The subscription could not start; the socket stays open.
    async fn refuse(&self, err: calin_core::Error) -> Result<(), Gone> {
        let message = match err {
            calin_core::Error::NotFound(session_id) => format!("unknown session {session_id}"),
            err => {
                tracing::error!(topic = %self.topic, %err, "could not subscribe");
                format!("could not subscribe to {}", self.topic)
            }
        };
        self.send(error(message, Some(&self.topic))).await
    }

    /// The client missed live updates. Closing the socket makes it reconnect
    /// and replay, which is the only way to stay gapless.
    fn disconnect(&self, reason: String) -> Result<(), Gone> {
        tracing::debug!(topic = %self.topic, reason, "closing a websocket that fell behind");
        self.closing.cancel();
        Err(Gone)
    }
}
