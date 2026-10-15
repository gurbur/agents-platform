//! Message identifiers, rooms and the bodies of `POST /v1/messages`,
//! `GET /v1/messages` and `GET /v1/messages/{message_id}` (API_V1 "Message
//! 송신과 durable commit", "History 조회"; `IF-ID-001`, `IF-WIRE-003`).
//!
//! `to` and `reply_to` arrive as plain strings so the server can answer
//! format errors as `invalid_field` naming the field; [`Recipient`] and
//! [`MessageId`] are the validation points.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agents::present_nullable;
use crate::{AgentId, AgentIdError, Seq};

/// Maximum decoded UTF-8 length of a message `body` in bytes (`IF-API-COMMON`: 16 KiB).
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Maximum length of a `client_key` in bytes (`IF-API-COMMON`: ASCII 1..=128).
pub const MAX_CLIENT_KEY_BYTES: usize = 128;

/// History and inbox page size when `limit` is absent (`IF-API-COMMON`).
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Largest accepted history and inbox `limit` (`IF-API-COMMON`).
pub const MAX_PAGE_SIZE: u32 = 100;

/// The only public room.
pub const ALL_ROOM_ID: &str = "all";

const MESSAGE_ID_PREFIX: &str = "msg-";
const DM_PREFIX: &str = "dm:";
const ULID_LEN: usize = 26;
const CROCKFORD_BASE32: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

// ---- MessageId ---------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("message_id must be `msg-` followed by a 26-character upper-case ULID")]
pub struct MessageIdError;

/// `msg-` + ULID (26 upper-case Crockford base32 characters, `IF-ID-001`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MessageId(String);

impl MessageId {
    /// Builds an id from the canonical text of a ULID, as printed by the
    /// `ulid` crate. Fails when `ulid` is not 26 upper-case Crockford digits.
    pub fn from_ulid_text(ulid: &str) -> Result<Self, MessageIdError> {
        format!("{MESSAGE_ID_PREFIX}{ulid}").parse()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(candidate: &str) -> Result<(), MessageIdError> {
        let ulid = candidate
            .strip_prefix(MESSAGE_ID_PREFIX)
            .ok_or(MessageIdError)?
            .as_bytes();
        if ulid.len() != ULID_LEN || !ulid.iter().all(|c| CROCKFORD_BASE32.contains(c)) {
            return Err(MessageIdError);
        }
        // 26 base32 digits hold 130 bits; a 128-bit ULID starts with 0..=7.
        if ulid[0] > b'7' {
            return Err(MessageIdError);
        }
        Ok(())
    }
}

impl FromStr for MessageId {
    type Err = MessageIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::validate(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for MessageId {
    type Error = MessageIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<MessageId> for String {
    fn from(id: MessageId) -> Self {
        id.0
    }
}

impl fmt::Display for MessageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---- RoomId --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomIdError {
    #[error("room_id must be `all` or `dm:<agent_id>:<agent_id>`")]
    InvalidFormat,
    #[error("room_id names an invalid agent_id: {0}")]
    InvalidAgentId(#[from] AgentIdError),
    #[error("a DM room_id lists two different agent_ids in ascending ASCII order")]
    NotCanonical,
}

/// `all`, or the canonical DM room `dm:<a>:<b>` with `a < b` in ASCII order.
/// A `Dm` value is only constructed through [`RoomId::dm`] or parsing, so
/// its two ids are always distinct and sorted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum RoomId {
    All,
    Dm(AgentId, AgentId),
}

impl RoomId {
    /// The DM room between two agents, in either order. `None` for a self DM.
    pub fn dm(one: AgentId, other: AgentId) -> Option<Self> {
        match one.cmp(&other) {
            std::cmp::Ordering::Less => Some(RoomId::Dm(one, other)),
            std::cmp::Ordering::Greater => Some(RoomId::Dm(other, one)),
            std::cmp::Ordering::Equal => None,
        }
    }

    /// Whether `agent` may read this room's history: everyone reads `all`,
    /// only the two participants read a DM.
    pub fn is_readable_by(&self, agent: &AgentId) -> bool {
        match self {
            RoomId::All => true,
            RoomId::Dm(low, high) => low == agent || high == agent,
        }
    }
}

impl FromStr for RoomId {
    type Err = RoomIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == ALL_ROOM_ID {
            return Ok(RoomId::All);
        }
        let pair = s
            .strip_prefix(DM_PREFIX)
            .ok_or(RoomIdError::InvalidFormat)?;
        // An agent_id never contains ':', so a single split is unambiguous.
        let (low, high) = pair.split_once(':').ok_or(RoomIdError::InvalidFormat)?;
        let low: AgentId = low.parse()?;
        let high: AgentId = high.parse()?;
        if low >= high {
            return Err(RoomIdError::NotCanonical);
        }
        Ok(RoomId::Dm(low, high))
    }
}

impl TryFrom<String> for RoomId {
    type Error = RoomIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<RoomId> for String {
    fn from(room: RoomId) -> Self {
        room.to_string()
    }
}

impl fmt::Display for RoomId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoomId::All => f.write_str(ALL_ROOM_ID),
            RoomId::Dm(low, high) => write!(f, "{DM_PREFIX}{low}:{high}"),
        }
    }
}

// ---- Recipient -----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`to` must be `all` or an agent_id")]
pub struct RecipientError;

/// The `to` of `POST /v1/messages`: `all` or one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recipient {
    All,
    Agent(AgentId),
}

impl FromStr for Recipient {
    type Err = RecipientError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == ALL_ROOM_ID {
            return Ok(Recipient::All);
        }
        s.parse().map(Recipient::Agent).map_err(|_| RecipientError)
    }
}

impl fmt::Display for Recipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Recipient::All => f.write_str(ALL_ROOM_ID),
            Recipient::Agent(agent) => f.write_str(agent.as_str()),
        }
    }
}

// ---- Bodies ----------------------------------------------------------------------

/// `POST /v1/messages` body. `sender_id` is taken from the API key, so it is
/// an unexpected field here. A missing `reply_to` means the same as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageRequest {
    pub to: String,
    pub body: String,
    pub client_key: String,
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// A stored message, as history and single-message lookups return it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub message_id: MessageId,
    pub event_seq: Seq,
    pub room_id: RoomId,
    pub sender_id: AgentId,
    pub body: String,
    pub reply_to: Option<MessageId>,
    pub created_at: String,
}

/// `POST /v1/messages` 201/200 body: the message plus whether it is the
/// original returned for a repeated `client_key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageResponse {
    #[serde(flatten)]
    pub message: Message,
    pub deduplicated: bool,
}

/// `GET /v1/messages` body. Exactly one cursor field is present, matching the
/// direction of the request; it is `null` when the page is empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPage {
    pub items: Vec<Message>,
    #[serde(flatten)]
    pub next: HistoryCursor,
    pub has_more: bool,
}

/// The direction-specific cursor of a [`HistoryPage`]: the `event_seq` of
/// the last returned item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HistoryCursor {
    /// Default and `before_event_seq` pages (newest first).
    Before {
        #[serde(deserialize_with = "present_nullable")]
        next_before_event_seq: Option<Seq>,
    },
    /// `after_event_seq` pages (oldest first).
    After {
        #[serde(deserialize_with = "present_nullable")]
        next_after_event_seq: Option<Seq>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(id: &str) -> AgentId {
        id.parse().unwrap()
    }

    const SAMPLE_ID: &str = "msg-01J00000000000000000000000";

    #[test]
    fn message_id_accepts_canonical_ulids() {
        let id: MessageId = SAMPLE_ID.parse().unwrap();
        assert_eq!(id.as_str(), SAMPLE_ID);
        assert_eq!(
            MessageId::from_ulid_text("7ZZZZZZZZZZZZZZZZZZZZZZZZZ")
                .unwrap()
                .as_str(),
            "msg-7ZZZZZZZZZZZZZZZZZZZZZZZZZ"
        );
    }

    #[test]
    fn message_id_rejects_other_shapes() {
        for invalid in [
            "",
            "msg-",
            "01J00000000000000000000000",
            "req-01J00000000000000000000000",
            "msg-01j00000000000000000000000",
            "msg-01J0000000000000000000000",
            "msg-01J000000000000000000000000",
            "msg-01J0000000000000000000000I",
            "msg-81J00000000000000000000000",
        ] {
            assert_eq!(
                invalid.parse::<MessageId>(),
                Err(MessageIdError),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn dm_room_is_sorted_and_rejects_self() {
        let forward = RoomId::dm(agent("agent-a"), agent("agent-b")).unwrap();
        let backward = RoomId::dm(agent("agent-b"), agent("agent-a")).unwrap();
        assert_eq!(forward, backward);
        assert_eq!(forward.to_string(), "dm:agent-a:agent-b");
        assert_eq!(RoomId::dm(agent("agent-a"), agent("agent-a")), None);
    }

    #[test]
    fn room_id_parses_canonical_forms_only() {
        assert_eq!("all".parse::<RoomId>(), Ok(RoomId::All));
        assert_eq!(
            "dm:agent-a:agent-b".parse::<RoomId>(),
            Ok(RoomId::Dm(agent("agent-a"), agent("agent-b")))
        );
        assert_eq!(
            "dm:agent-b:agent-a".parse::<RoomId>(),
            Err(RoomIdError::NotCanonical)
        );
        assert_eq!(
            "dm:agent-a:agent-a".parse::<RoomId>(),
            Err(RoomIdError::NotCanonical)
        );
        for invalid in ["", "All", "dm:", "dm:agent-a", "dm:agent-a:", "room:a:b"] {
            assert!(invalid.parse::<RoomId>().is_err(), "{invalid:?}");
        }
        for bad_slug in ["dm:Agent:b", "dm:a:all", "dm:a:b:c", "dm:owner:zed"] {
            assert!(
                matches!(
                    bad_slug.parse::<RoomId>(),
                    Err(RoomIdError::InvalidAgentId(_))
                ),
                "{bad_slug:?}"
            );
        }
    }

    #[test]
    fn room_readability() {
        let dm = RoomId::dm(agent("agent-a"), agent("agent-b")).unwrap();
        assert!(dm.is_readable_by(&agent("agent-a")));
        assert!(dm.is_readable_by(&agent("agent-b")));
        assert!(!dm.is_readable_by(&agent("agent-c")));
        assert!(RoomId::All.is_readable_by(&agent("agent-c")));
    }

    #[test]
    fn recipient_is_all_or_an_agent() {
        assert_eq!("all".parse::<Recipient>(), Ok(Recipient::All));
        assert_eq!(
            "agent-b".parse::<Recipient>(),
            Ok(Recipient::Agent(agent("agent-b")))
        );
        for invalid in ["", "owner", "ALL", "dm:agent-a:agent-b", "Agent"] {
            assert_eq!(
                invalid.parse::<Recipient>(),
                Err(RecipientError),
                "{invalid:?}"
            );
        }
    }

    #[test]
    fn send_request_defaults_reply_to_and_rejects_sender_id() {
        let request: SendMessageRequest =
            serde_json::from_value(json!({"to": "all", "body": "hi", "client_key": "k"})).unwrap();
        assert_eq!(request.reply_to, None);

        let err = serde_json::from_value::<SendMessageRequest>(
            json!({"to": "all", "body": "hi", "client_key": "k", "sender_id": "agent-a"}),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown field `sender_id`"),
            "{err}"
        );
    }

    fn sample_message() -> Message {
        Message {
            message_id: SAMPLE_ID.parse().unwrap(),
            event_seq: Seq::new(42).unwrap(),
            room_id: RoomId::All,
            sender_id: agent("agent-a"),
            body: "hi".into(),
            reply_to: None,
            created_at: "2026-10-14T10:00:00.000Z".into(),
        }
    }

    #[test]
    fn message_response_is_flat() {
        let response = MessageResponse {
            message: sample_message(),
            deduplicated: true,
        };
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(
            value,
            json!({
                "message_id": SAMPLE_ID,
                "event_seq": "42",
                "room_id": "all",
                "sender_id": "agent-a",
                "body": "hi",
                "reply_to": null,
                "created_at": "2026-10-14T10:00:00.000Z",
                "deduplicated": true
            })
        );
        assert_eq!(
            serde_json::from_value::<MessageResponse>(value).unwrap(),
            response
        );
    }

    #[test]
    fn history_page_carries_one_direction_cursor() {
        let before = HistoryPage {
            items: vec![sample_message()],
            next: HistoryCursor::Before {
                next_before_event_seq: Some(Seq::new(42).unwrap()),
            },
            has_more: false,
        };
        let value = serde_json::to_value(&before).unwrap();
        assert_eq!(value["next_before_event_seq"], "42");
        assert!(value.get("next_after_event_seq").is_none());
        assert_eq!(
            serde_json::from_value::<HistoryPage>(value).unwrap(),
            before
        );

        let empty_after = HistoryPage {
            items: vec![],
            next: HistoryCursor::After {
                next_after_event_seq: None,
            },
            has_more: false,
        };
        let value = serde_json::to_value(&empty_after).unwrap();
        assert_eq!(
            value,
            json!({"items": [], "next_after_event_seq": null, "has_more": false})
        );
        assert_eq!(
            serde_json::from_value::<HistoryPage>(value).unwrap(),
            empty_after
        );
    }
}
