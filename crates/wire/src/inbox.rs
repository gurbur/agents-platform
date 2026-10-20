//! Bodies of `GET /v1/inbox`, `GET /v1/agents/{agent_id}/inbox` and
//! `PUT /v1/inbox/read-marker` (API_V1 "Inbox 조회와 read marker";
//! PLATFORM_MESSAGING_SPEC "Inbox pagination snapshot"; `IF-WIRE-012`).
//!
//! Page sizes are shared with history: [`crate::messages::DEFAULT_PAGE_SIZE`]
//! and [`crate::messages::MAX_PAGE_SIZE`].

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

use crate::{AgentId, Message, Seq};

/// The `kind` of a message event.
pub const MESSAGE_KIND: &str = "message";

/// What an inbox row records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxEvent {
    /// `kind = "message"`: the message's fields sit flat next to
    /// `inbox_seq` and `kind`.
    Message(Message),
    /// A `kind` this crate does not know (a later slice added it). Readers
    /// skip it and still move the read marker over it.
    Unknown { kind: String },
}

/// One inbox event (`IF-WIRE-012`): `inbox_seq` and `kind` always, the
/// rest decided by `kind`. Reading an unknown `kind` is not an error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    pub inbox_seq: Seq,
    pub event: InboxEvent,
}

impl InboxItem {
    pub fn message(inbox_seq: Seq, message: Message) -> Self {
        Self {
            inbox_seq,
            event: InboxEvent::Message(message),
        }
    }

    pub fn kind(&self) -> &str {
        match &self.event {
            InboxEvent::Message(_) => MESSAGE_KIND,
            InboxEvent::Unknown { kind } => kind,
        }
    }

    pub fn as_message(&self) -> Option<&Message> {
        match &self.event {
            InboxEvent::Message(message) => Some(message),
            InboxEvent::Unknown { .. } => None,
        }
    }
}

/// The serialized shape of a message event: `inbox_seq`, `kind`, then the
/// ten message keys.
#[derive(Serialize)]
struct MessageEvent<'a> {
    inbox_seq: Seq,
    kind: &'static str,
    #[serde(flatten)]
    message: &'a Message,
}

#[derive(Serialize)]
struct UnknownEvent<'a> {
    inbox_seq: Seq,
    kind: &'a str,
}

impl Serialize for InboxItem {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.event {
            InboxEvent::Message(message) => MessageEvent {
                inbox_seq: self.inbox_seq,
                kind: MESSAGE_KIND,
                message,
            }
            .serialize(serializer),
            InboxEvent::Unknown { kind } => UnknownEvent {
                inbox_seq: self.inbox_seq,
                kind,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for InboxItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut fields = Map::<String, Value>::deserialize(deserializer)?;
        let inbox_seq: Seq = take(&mut fields, "inbox_seq")?;
        let kind: String = take(&mut fields, "kind")?;
        if kind != MESSAGE_KIND {
            return Ok(Self {
                inbox_seq,
                event: InboxEvent::Unknown { kind },
            });
        }
        let message = Message::deserialize(Value::Object(fields)).map_err(D::Error::custom)?;
        Ok(Self::message(inbox_seq, message))
    }
}

/// Removes and decodes the required member `name`.
fn take<T: serde::de::DeserializeOwned, E: serde::de::Error>(
    fields: &mut Map<String, Value>,
    name: &'static str,
) -> Result<T, E> {
    let value = fields.remove(name).ok_or_else(|| E::missing_field(name))?;
    serde_json::from_value(value).map_err(E::custom)
}

/// One page of an inbox under a fixed upper bound `snapshot_head_seq`.
///
/// `next_after_seq` is the `inbox_seq` of the last returned item, or the
/// request's `after_seq` when the page is empty; it never jumps to the head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPage {
    pub agent_id: AgentId,
    pub read_marker: Seq,
    pub snapshot_head_seq: Seq,
    pub items: Vec<InboxItem>,
    pub next_after_seq: Seq,
    pub has_more: bool,
}

/// `PUT /v1/inbox/read-marker` body: the confirmed contiguous prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadMarkerRequest {
    pub through_seq: Seq,
}

/// `PUT /v1/inbox/read-marker` 200 body, for an advance and a no-op alike.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadMarkerResponse {
    pub agent_id: AgentId,
    pub read_marker: Seq,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RoomId, RoomKind};
    use serde_json::json;

    fn seq(value: i64) -> Seq {
        Seq::new(value).unwrap()
    }

    #[test]
    fn inbox_page_matches_the_api_example_shape() {
        let page = InboxPage {
            agent_id: "agent-b".parse().unwrap(),
            read_marker: seq(41),
            snapshot_head_seq: seq(44),
            items: vec![InboxItem::message(
                seq(42),
                Message {
                    message_id: "msg-01J00000000000000000000000".parse().unwrap(),
                    event_seq: seq(42),
                    room_id: RoomId::All,
                    room_kind: RoomKind::All,
                    sender_id: "agent-a".parse().unwrap(),
                    body: "hi".into(),
                    reply_to: None,
                    wake: true,
                    mentions: vec![],
                    created_at: "2026-10-14T10:00:00.000Z".into(),
                },
            )],
            next_after_seq: seq(42),
            has_more: true,
        };
        let value = serde_json::to_value(&page).unwrap();
        assert_eq!(
            value,
            json!({
                "agent_id": "agent-b",
                "read_marker": "41",
                "snapshot_head_seq": "44",
                "items": [{
                    "inbox_seq": "42",
                    "kind": "message",
                    "message_id": "msg-01J00000000000000000000000",
                    "event_seq": "42",
                    "room_id": "all",
                    "room_kind": "all",
                    "sender_id": "agent-a",
                    "body": "hi",
                    "reply_to": null,
                    "wake": true,
                    "mentions": [],
                    "created_at": "2026-10-14T10:00:00.000Z"
                }],
                "next_after_seq": "42",
                "has_more": true
            })
        );
        assert_eq!(serde_json::from_value::<InboxPage>(value).unwrap(), page);
    }

    #[test]
    fn an_unknown_event_serializes_its_sequence_and_kind() {
        let item = InboxItem {
            inbox_seq: seq(3),
            event: InboxEvent::Unknown {
                kind: "reaction".into(),
            },
        };
        let value = serde_json::to_value(&item).unwrap();
        assert_eq!(value, json!({"inbox_seq": "3", "kind": "reaction"}));
        assert_eq!(serde_json::from_value::<InboxItem>(value).unwrap(), item);
    }

    #[test]
    fn read_marker_request_takes_only_a_string_through_seq() {
        let request: ReadMarkerRequest =
            serde_json::from_value(json!({"through_seq": "42"})).unwrap();
        assert_eq!(request.through_seq, seq(42));
        for invalid in [
            json!({"through_seq": 42}),
            json!({"through_seq": "042"}),
            json!({}),
            json!({"through_seq": "1", "agent_id": "agent-b"}),
        ] {
            assert!(
                serde_json::from_value::<ReadMarkerRequest>(invalid.clone()).is_err(),
                "{invalid}"
            );
        }
    }
}
