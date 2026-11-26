use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{Event, Session};

/// A frame sent by the client on `/api/v1/ws`. One JSON object per text frame.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export)]
pub enum ClientMsg {
    /// Start receiving a topic.
    ///
    /// For `session:<id>`, the server first replays every event with
    /// `seq > after_seq` (all of them when `after_seq` is absent), then sends
    /// `subscribed`, then live events. For `sessions`, the server sends one
    /// `session_upserted` per existing session, then `subscribed`, then updates.
    Subscribe {
        topic: String,
        #[serde(default)]
        #[ts(optional, type = "number")]
        after_seq: Option<u64>,
    },
    Unsubscribe {
        topic: String,
    },
    Ping,
}

/// A frame sent by the server on `/api/v1/ws`.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
#[ts(export)]
pub enum ServerMsg {
    /// Replay for `topic` is complete; everything after this frame is live.
    Subscribed {
        topic: String,
        /// Newest sequence number at the time of the subscription. 0 for `sessions`.
        #[ts(type = "number")]
        head_seq: u64,
    },
    /// An event of a session, on `session:<id>`. Delivered in `seq` order, never twice.
    Event {
        topic: String,
        event: Event,
    },
    /// A session was created or changed, on `sessions`. Sent on every status
    /// change, and at most about once a second while events are arriving.
    SessionUpserted {
        session: Session,
    },
    Pong,
    /// The last client frame could not be handled. The connection stays open.
    Error {
        message: String,
        /// The topic of the refused `subscribe` or `unsubscribe`, when there is one.
        #[serde(default)]
        #[ts(optional)]
        topic: Option<String>,
    },
}
