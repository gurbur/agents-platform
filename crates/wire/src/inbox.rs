//! Bodies of `GET /v1/inbox`, `GET /v1/agents/{agent_id}/inbox` and
//! `PUT /v1/inbox/read-marker` (API_V1 "Inbox 조회와 read marker";
//! PLATFORM_MESSAGING_SPEC "Inbox pagination snapshot").
//!
//! Page sizes are shared with history: [`crate::messages::DEFAULT_PAGE_SIZE`]
//! and [`crate::messages::MAX_PAGE_SIZE`].

use serde::{Deserialize, Serialize};

use crate::{AgentId, MessageId, RoomId, Seq};

/// One inbox row joined with its message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub inbox_seq: Seq,
    pub message_id: MessageId,
    pub event_seq: Seq,
    pub room_id: RoomId,
    pub sender_id: AgentId,
    pub body: String,
    pub created_at: String,
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
            items: vec![InboxItem {
                inbox_seq: seq(42),
                message_id: "msg-01J00000000000000000000000".parse().unwrap(),
                event_seq: seq(42),
                room_id: RoomId::All,
                sender_id: "agent-a".parse().unwrap(),
                body: "hi".into(),
                created_at: "2026-10-14T10:00:00.000Z".into(),
            }],
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
                    "message_id": "msg-01J00000000000000000000000",
                    "event_seq": "42",
                    "room_id": "all",
                    "sender_id": "agent-a",
                    "body": "hi",
                    "created_at": "2026-10-14T10:00:00.000Z"
                }],
                "next_after_seq": "42",
                "has_more": true
            })
        );
        assert_eq!(serde_json::from_value::<InboxPage>(value).unwrap(), page);
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
