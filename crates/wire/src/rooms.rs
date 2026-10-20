//! Rooms: the body of `GET /v1/rooms` and topic room creation
//! (API_V1 "Room과 owner 게시", `IF-WIRE-011`).
//!
//! `room_id` and `title` of a creation arrive as plain strings so the
//! server can answer format errors as `invalid_field` naming the field;
//! [`crate::RoomId`] and [`validate_room_title`] are the validation points.

use serde::{Deserialize, Serialize};

use crate::agents::present_nullable;
use crate::{AgentId, RoomId, Seq};

/// Most rooms one `GET /v1/rooms` returns, `all` included (`IF-WIRE-011`).
/// A contract constant, not a tuning value.
pub const ROOMS_SOFT_CAP: usize = 1000;

/// Most scalar values of a topic room `title` after trimming
/// (`IF-WIRE-011`). A contract constant, not a tuning value.
pub const MAX_ROOM_TITLE_CHARS: usize = 64;

/// `created_by` of `all` and of every topic room.
pub const OWNER_CREATOR: &str = "owner";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomKind {
    All,
    Dm,
    Topic,
}

impl RoomKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RoomKind::All => "all",
            RoomKind::Dm => "dm",
            RoomKind::Topic => "topic",
        }
    }

    /// The stored `rooms.kind` text back as a kind.
    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "all" => Some(RoomKind::All),
            "dm" => Some(RoomKind::Dm),
            "topic" => Some(RoomKind::Topic),
            _ => None,
        }
    }
}

/// One room of `GET /v1/rooms` and the body of every topic room endpoint:
/// always these eleven keys, `null` never omitted (`IF-WIRE-011`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomSummary {
    pub room_id: RoomId,
    pub kind: RoomKind,
    /// A topic room's display name; `null` for `all` and DMs.
    #[serde(deserialize_with = "present_nullable")]
    pub title: Option<String>,
    /// ASCII ascending: the two ids of a DM (a deleted agent's included),
    /// the joined agents of `all` and of a topic room.
    pub members: Vec<AgentId>,
    /// [`OWNER_CREATOR`], or the sender of a DM's first message.
    pub created_by: String,
    pub created_at: String,
    #[serde(deserialize_with = "present_nullable")]
    pub archived_at: Option<String>,
    #[serde(deserialize_with = "present_nullable")]
    pub last_event_seq: Option<Seq>,
    #[serde(deserialize_with = "present_nullable")]
    pub last_message_at: Option<String>,
    #[serde(deserialize_with = "present_nullable")]
    pub last_sender_id: Option<AgentId>,
    pub message_count: u64,
}

/// `GET /v1/rooms` body: rooms with messages newest first, then the rest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomList {
    pub rooms: Vec<RoomSummary>,
}

/// `POST /v1/rooms` body. A missing `title` means the same as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRoomRequest {
    pub room_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Why a room title was rejected. Messages never repeat the value: a title
/// is free text, kept out of logs like a message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RoomTitleError {
    #[error("title must not be empty after trimming; send null for no title")]
    Empty,
    #[error("title must be at most {MAX_ROOM_TITLE_CHARS} characters after trimming")]
    TooLong,
    #[error("title must not contain control characters")]
    ControlCharacter,
}

/// Validates a topic room title the way `name` is validated (`IF-WIRE-006`
/// method): `str::trim`, 1..=[`MAX_ROOM_TITLE_CHARS`] scalar values, no
/// Unicode `Cc` character. Returns the trimmed title.
pub fn validate_room_title(raw: &str) -> Result<String, RoomTitleError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(RoomTitleError::Empty);
    }
    if trimmed.chars().count() > MAX_ROOM_TITLE_CHARS {
        return Err(RoomTitleError::TooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(RoomTitleError::ControlCharacter);
    }
    Ok(trimmed.to_owned())
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
            title: None,
            members: vec!["agent-a".parse().unwrap()],
            created_by: OWNER_CREATOR.into(),
            created_at: "2026-10-20T00:00:00.000Z".into(),
            archived_at: None,
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
                "title": null,
                "members": ["agent-a"],
                "created_by": "owner",
                "created_at": "2026-10-20T00:00:00.000Z",
                "archived_at": null,
                "last_event_seq": null,
                "last_message_at": null,
                "last_sender_id": null,
                "message_count": 0
            })
        );
        assert_eq!(serde_json::from_value::<RoomSummary>(value).unwrap(), all);
    }

    #[test]
    fn kind_follows_the_room_and_round_trips_as_stored_text() {
        let dm: RoomId = "dm:agent-a:agent-b".parse().unwrap();
        assert_eq!(dm.kind(), RoomKind::Dm);
        assert_eq!(RoomId::All.kind(), RoomKind::All);
        for kind in [RoomKind::All, RoomKind::Dm, RoomKind::Topic] {
            assert_eq!(RoomKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(RoomKind::parse("group"), None);
    }

    #[test]
    fn title_rules_follow_the_name_rules() {
        assert_eq!(validate_room_title(" a b "), Ok("a b".to_owned()));
        assert_eq!(validate_room_title(" \t"), Err(RoomTitleError::Empty));
        assert_eq!(
            validate_room_title(&"x".repeat(MAX_ROOM_TITLE_CHARS + 1)),
            Err(RoomTitleError::TooLong)
        );
        assert_eq!(
            validate_room_title("a\u{7}b"),
            Err(RoomTitleError::ControlCharacter)
        );
    }

    #[test]
    fn create_request_omits_an_absent_title() {
        let request = CreateRoomRequest {
            room_id: "room:a".into(),
            title: None,
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"room_id": "room:a"})
        );
    }
}
