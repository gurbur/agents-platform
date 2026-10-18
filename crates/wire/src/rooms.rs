//! Room list and owner posting: the bodies of `GET /v1/rooms` and
//! `POST /v1/rooms/{room_id}/messages` (API_V1 "Room 목록과 owner 게시",
//! `IF-WIRE-007`).
//!
//! `sender_id` and `reply_to` arrive as plain strings: the server compares
//! `sender_id` with its allowlist verbatim and answers format errors as
//! `invalid_field` naming the field.

use serde::{Deserialize, Serialize};

use crate::agents::present_nullable;
use crate::{AgentId, RoomId, Seq};

/// Most rooms one `GET /v1/rooms` returns, `all` included (`IF-WIRE-007`).
/// A contract constant, not a tuning value.
pub const ROOMS_SOFT_CAP: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    All,
    Dm,
}

impl RoomKind {
    pub fn of(room: &RoomId) -> Self {
        match room {
            RoomId::All => RoomKind::All,
            RoomId::Dm(..) => RoomKind::Dm,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RoomKind::All => "all",
            RoomKind::Dm => "dm",
        }
    }
}

/// One room of `GET /v1/rooms`. The three `last_*` fields describe the
/// room's newest message and are `null` only for an `all` without messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub room_id: RoomId,
    pub kind: RoomKind,
    /// ASCII ascending: the two ids of a DM, the joined agents of `all`.
    pub members: Vec<AgentId>,
    #[serde(deserialize_with = "present_nullable")]
    pub last_event_seq: Option<Seq>,
    #[serde(deserialize_with = "present_nullable")]
    pub last_message_at: Option<String>,
    #[serde(deserialize_with = "present_nullable")]
    pub last_sender_id: Option<AgentId>,
    pub message_count: u64,
}

/// `GET /v1/rooms` body, newest last message first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomList {
    pub rooms: Vec<RoomSummary>,
}

/// `POST /v1/rooms/{room_id}/messages` body. A missing `reply_to` means the
/// same as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomPostRequest {
    pub sender_id: String,
    pub body: String,
    pub client_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summary_keeps_null_fields() {
        let all = RoomSummary {
            room_id: RoomId::All,
            kind: RoomKind::All,
            members: vec!["agent-a".parse().unwrap()],
            last_event_seq: None,
            last_message_at: None,
            last_sender_id: None,
            message_count: 0,
        };
        let value = serde_json::to_value(&all).unwrap();
        assert_eq!(
            value,
            json!({
                "room_id": "all",
                "kind": "all",
                "members": ["agent-a"],
                "last_event_seq": null,
                "last_message_at": null,
                "last_sender_id": null,
                "message_count": 0
            })
        );
        assert_eq!(serde_json::from_value::<RoomSummary>(value).unwrap(), all);
    }

    #[test]
    fn kind_follows_the_room() {
        let dm: RoomId = "dm:agent-a:agent-b".parse().unwrap();
        assert_eq!(RoomKind::of(&dm), RoomKind::Dm);
        assert_eq!(RoomKind::of(&RoomId::All), RoomKind::All);
        assert_eq!(serde_json::to_value(RoomKind::Dm).unwrap(), json!("dm"));
        assert_eq!(RoomKind::Dm.as_str(), "dm");
    }

    #[test]
    fn post_request_is_strict_and_defaults_reply_to() {
        let request: RoomPostRequest =
            serde_json::from_value(json!({"sender_id": "human", "body": "hi", "client_key": "k"}))
                .unwrap();
        assert_eq!(request.reply_to, None);
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"sender_id": "human", "body": "hi", "client_key": "k"}),
            "an absent reply_to is not sent"
        );
        let err = serde_json::from_value::<RoomPostRequest>(
            json!({"sender_id": "human", "body": "hi", "client_key": "k", "to": "all"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field `to`"), "{err}");
    }
}
