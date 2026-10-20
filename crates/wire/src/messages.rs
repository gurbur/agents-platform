//! Message identifiers, rooms and the bodies of `POST /v1/messages`,
//! `GET /v1/messages` and `GET /v1/messages/{message_id}` (API_V1 "Message
//! 송신과 durable commit", "History 조회"; `IF-ID-001`, `IF-WIRE-003`).
//!
//! The send body keeps its destinations (`room_id`, `reply_to`, `to`),
//! `sender_id` and `mentions` as plain strings so the server can answer
//! format errors as `invalid_field` naming the field, in the `IF-WIRE-010`
//! order; [`RoomId`], [`MessageId`] and [`AgentId`] are the validation
//! points.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::agents::present_nullable;
use crate::{AgentId, AgentIdError, RoomKind, Seq};

/// Maximum decoded UTF-8 length of a message `body` in bytes (`IF-API-COMMON`: 16 KiB).
pub const MAX_BODY_BYTES: usize = 16 * 1024;

/// Maximum length of a `client_key` in bytes (`IF-API-COMMON`: ASCII 1..=128).
pub const MAX_CLIENT_KEY_BYTES: usize = 128;

/// History and inbox page size when `limit` is absent (`IF-API-COMMON`).
pub const DEFAULT_PAGE_SIZE: u32 = 50;

/// Largest accepted history and inbox `limit` (`IF-API-COMMON`).
pub const MAX_PAGE_SIZE: u32 = 100;

/// Most agent ids one send may name in `mentions` (`IF-WIRE-010`). A
/// contract constant, not a tuning value.
pub const MAX_MENTIONS: usize = 16;

/// The only public room.
pub const ALL_ROOM_ID: &str = "all";

const MESSAGE_ID_PREFIX: &str = "msg-";
const DM_PREFIX: &str = "dm:";
const TOPIC_PREFIX: &str = "room:";
/// Longest topic room slug in bytes (`IF-ID-001`).
const MAX_SLUG_LEN: usize = 64;
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

// ---- RoomSlug ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a topic room slug matches ^[a-z0-9][a-z0-9_-]{{0,63}}$")]
pub struct RoomSlugError;

/// The name of a topic room after `room:`: `^[a-z0-9][a-z0-9_-]{0,63}$`
/// (`IF-ID-001`). Unlike an `agent_id` it may start with a digit and has no
/// reserved words.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RoomSlug(String);

impl RoomSlug {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(candidate: &str) -> Result<(), RoomSlugError> {
        let bytes = candidate.as_bytes();
        let Some((first, rest)) = bytes.split_first() else {
            return Err(RoomSlugError);
        };
        let starts_well = first.is_ascii_lowercase() || first.is_ascii_digit();
        let rest_ok = rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-');
        if !starts_well || !rest_ok || bytes.len() > MAX_SLUG_LEN {
            return Err(RoomSlugError);
        }
        Ok(())
    }
}

impl FromStr for RoomSlug {
    type Err = RoomSlugError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::validate(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for RoomSlug {
    type Error = RoomSlugError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<RoomSlug> for String {
    fn from(slug: RoomSlug) -> Self {
        slug.0
    }
}

impl fmt::Display for RoomSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---- RoomId --------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoomIdError {
    #[error("room_id must be `all`, `dm:<agent_id>:<agent_id>` or `room:<slug>`")]
    InvalidFormat,
    #[error("room_id names an invalid agent_id: {0}")]
    InvalidAgentId(#[from] AgentIdError),
    #[error("a DM room_id lists two different agent_ids in ascending ASCII order")]
    NotCanonical,
    #[error("room_id names an invalid topic room: {0}")]
    InvalidSlug(#[from] RoomSlugError),
}

/// `all`, the canonical DM room `dm:<a>:<b>` with `a < b` in ASCII order,
/// or a topic room `room:<slug>` (`IF-ID-001`). A `Dm` value is only
/// constructed through [`RoomId::dm`] or parsing, so its two ids are always
/// distinct and sorted. An `agent_id` never contains `:`, so the three forms
/// never overlap.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum RoomId {
    All,
    Dm(AgentId, AgentId),
    Topic(RoomSlug),
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

    pub fn kind(&self) -> RoomKind {
        match self {
            RoomId::All => RoomKind::All,
            RoomId::Dm(..) => RoomKind::Dm,
            RoomId::Topic(_) => RoomKind::Topic,
        }
    }

    /// The two members a DM room id names, in ASCII order.
    pub fn dm_members(&self) -> Option<(&AgentId, &AgentId)> {
        match self {
            RoomId::Dm(low, high) => Some((low, high)),
            RoomId::All | RoomId::Topic(_) => None,
        }
    }
}

impl FromStr for RoomId {
    type Err = RoomIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == ALL_ROOM_ID {
            return Ok(RoomId::All);
        }
        if let Some(slug) = s.strip_prefix(TOPIC_PREFIX) {
            return Ok(RoomId::Topic(slug.parse()?));
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
            RoomId::Topic(slug) => write!(f, "{TOPIC_PREFIX}{slug}"),
        }
    }
}

// ---- Bodies ----------------------------------------------------------------------

/// `POST /v1/messages` body, for agents and the owner alike
/// (`IF-WIRE-010`). Exactly one of `room_id`, `reply_to` and `to` is meant
/// to be non-null; the server checks it. A missing key means the same as
/// `null`; serializing leaves out every `None` key. `sender_id` is the
/// owner's posting identity; an agent sending it gets `unexpected_field`
/// from the server.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendMessageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    pub body: String,
    pub client_key: String,
    /// Absent means `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mentions: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
}

/// A stored message, as history, single-message lookups and the send
/// response carry it (`IF-WIRE-012`): always these ten keys in this order.
/// `wake` is the sender's choice; `mentions` is in ASCII order, `[]` when
/// nobody was named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub message_id: MessageId,
    pub event_seq: Seq,
    pub room_id: RoomId,
    pub room_kind: RoomKind,
    pub sender_id: AgentId,
    pub body: String,
    pub reply_to: Option<MessageId>,
    pub wake: bool,
    pub mentions: Vec<AgentId>,
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
        for invalid in [
            "",
            "All",
            "dm:",
            "dm:agent-a",
            "dm:agent-a:",
            "room:a:b",
            "room:",
        ] {
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
    fn send_request_omits_absent_keys_when_serialized() {
        let request = SendMessageRequest {
            room_id: Some("all".into()),
            body: "hi".into(),
            client_key: "k".into(),
            ..SendMessageRequest::default()
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"room_id": "all", "body": "hi", "client_key": "k"})
        );
        let quiet = SendMessageRequest {
            wake: Some(false),
            mentions: Some(vec!["agent-b".into()]),
            ..request
        };
        let value = serde_json::to_value(&quiet).unwrap();
        assert_eq!(value["wake"], false);
        assert_eq!(value["mentions"], json!(["agent-b"]));
    }

    fn sample_message() -> Message {
        Message {
            message_id: SAMPLE_ID.parse().unwrap(),
            event_seq: Seq::new(42).unwrap(),
            room_id: RoomId::All,
            room_kind: RoomKind::All,
            sender_id: agent("agent-a"),
            body: "hi".into(),
            reply_to: None,
            wake: true,
            mentions: vec![],
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
                "room_kind": "all",
                "sender_id": "agent-a",
                "body": "hi",
                "reply_to": null,
                "wake": true,
                "mentions": [],
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
