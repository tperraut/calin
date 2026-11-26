//! Wire types shared by the Calin server and the web app.
//!
//! This crate is the single source of truth for everything that crosses the
//! network: REST bodies, WebSocket frames and session events. The TypeScript
//! twins in `apps/web/src/gen/` are generated from these definitions by
//! `make gen` and must never be edited by hand.
//!
//! Rules:
//! - Every type derives `Serialize`, `Deserialize` and `TS`.
//! - Enums are tagged (`type`, `kind` or `op`) and use `snake_case` names.
//! - A new field on an existing type is `Option` or has a serde default, so
//!   older event logs keep parsing.

mod api;
mod event;
mod model;
mod permission;
mod repo;
mod terminal;
mod ws;

pub use api::*;
pub use event::*;
pub use model::*;
pub use permission::*;
pub use repo::*;
pub use terminal::*;
pub use ws::*;

/// Topic carrying [`ServerMsg::SessionUpserted`] for every session.
pub const TOPIC_SESSIONS: &str = "sessions";

/// Topic carrying the events of one session.
pub fn session_topic(session_id: &str) -> String {
    format!("session:{session_id}")
}

/// Session id of a `session:<id>` topic, if it is one.
pub fn parse_session_topic(topic: &str) -> Option<&str> {
    topic.strip_prefix("session:").filter(|id| !id.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_topics_round_trip() {
        let topic = session_topic("abc");
        assert_eq!(topic, "session:abc");
        assert_eq!(parse_session_topic(&topic), Some("abc"));
        assert_eq!(parse_session_topic(TOPIC_SESSIONS), None);
        assert_eq!(parse_session_topic("session:"), None);
    }

    #[test]
    fn event_body_is_tagged_snake_case() {
        let body = EventBody::ItemDelta {
            item_id: "i1".into(),
            delta: ItemDelta::Text { text: "hi".into() },
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["type"], "item_delta");
        assert_eq!(json["delta"]["kind"], "text");
        let back: EventBody = serde_json::from_value(json).unwrap();
        assert_eq!(back, body);
    }

    #[test]
    fn client_messages_parse_from_the_documented_shape() {
        let msg: ClientMsg =
            serde_json::from_str(r#"{"op":"subscribe","topic":"session:s1","after_seq":4}"#)
                .unwrap();
        assert_eq!(
            msg,
            ClientMsg::Subscribe {
                topic: "session:s1".into(),
                after_seq: Some(4)
            }
        );
        let msg: ClientMsg =
            serde_json::from_str(r#"{"op":"subscribe","topic":"sessions"}"#).unwrap();
        assert_eq!(
            msg,
            ClientMsg::Subscribe {
                topic: "sessions".into(),
                after_seq: None
            }
        );
    }
}
