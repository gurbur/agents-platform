//! Error body `{"error":{"code","message","request_id","details"}}` (`IF-API-COMMON`).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    pub request_id: String,
    #[serde(default)]
    pub details: Map<String, Value>,
}

impl ErrorBody {
    pub fn new(
        code: impl Into<String>,
        message: impl Into<String>,
        request_id: impl Into<String>,
    ) -> Self {
        Self {
            error: ErrorDetail {
                code: code.into(),
                message: message.into(),
                request_id: request_id.into(),
                details: Map::new(),
            },
        }
    }

    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.error.details.insert(key.into(), value.into());
        self
    }
}

/// Error `code` strings. Sources: API_V1 "오류와 권한 요약" and `IF-WIRE-002`.
pub mod codes {
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const FORBIDDEN: &str = "forbidden";
    /// No joined membership of the room to write to; for an agent also a
    /// room that does not exist (S13, `IF-WIRE-003`).
    pub const NOT_ROOM_MEMBER: &str = "not_room_member";
    pub const INVALID_AGENT_ID: &str = "invalid_agent_id";
    pub const AGENT_ID_TAKEN: &str = "agent_id_taken";
    pub const UNKNOWN_RECIPIENT: &str = "unknown_recipient";
    pub const SELF_DM_NOT_SUPPORTED: &str = "self_dm_not_supported";
    pub const IDEMPOTENCY_CONFLICT: &str = "idempotency_conflict";
    pub const MUTUALLY_EXCLUSIVE_CURSORS: &str = "mutually_exclusive_cursors";
    pub const READ_MARKER_AHEAD: &str = "read_marker_ahead";
    pub const BODY_TOO_LARGE: &str = "body_too_large";
    pub const REQUEST_TOO_LARGE: &str = "request_too_large";
    pub const INVALID_RECEIVER_URL: &str = "invalid_receiver_url";
    pub const UNEXPECTED_FIELD: &str = "unexpected_field";
    pub const INVALID_FIELD: &str = "invalid_field";
    pub const INVALID_JSON: &str = "invalid_json";
    pub const UNSUPPORTED_MEDIA_TYPE: &str = "unsupported_media_type";
    /// Supplementary codes from `IF-WIRE-003`.
    pub const AGENT_NOT_FOUND: &str = "agent_not_found";
    pub const NOT_FOUND: &str = "not_found";
    pub const METHOD_NOT_ALLOWED: &str = "method_not_allowed";
    pub const INTERNAL: &str = "internal";
    pub const REPLY_TO_NOT_FOUND: &str = "reply_to_not_found";
    pub const MESSAGE_NOT_FOUND: &str = "message_not_found";
    /// No such room: the owner's history, send and room management, or a
    /// DM whose member is not registered (S13, `IF-WIRE-003`).
    pub const UNKNOWN_ROOM: &str = "unknown_room";
    /// S13 room and send codes (`IF-WIRE-003`, `IF-WIRE-010`, `IF-WIRE-011`).
    pub const ROOM_ARCHIVED: &str = "room_archived";
    pub const ROOM_ID_TAKEN: &str = "room_id_taken";
    pub const INVALID_DESTINATION: &str = "invalid_destination";
    pub const MENTION_NOT_MEMBER: &str = "mention_not_member";
    /// S15 thread and correction codes (`IF-WIRE-003`, `IF-WIRE-014`,
    /// `IF-WIRE-015`). `thread_not_found` also hides a thread the caller
    /// cannot read.
    pub const THREAD_NOT_FOUND: &str = "thread_not_found";
    pub const CORRECTS_NOT_FOUND: &str = "corrects_not_found";
    pub const CORRECTS_ROOM_MISMATCH: &str = "corrects_room_mismatch";
    pub const CORRECTS_NOT_OWN: &str = "corrects_not_own";
    /// S16 reaction codes (`IF-WIRE-003`, `IF-WIRE-016`, `IF-WIRE-017`):
    /// a well-formed kind the registry lacks or has disabled, and a glyph
    /// another kind already uses.
    pub const UNKNOWN_REACTION_KIND: &str = "unknown_reaction_kind";
    pub const GLYPH_TAKEN: &str = "glyph_taken";
    /// Directory avatar codes (API_V1 "Directory avatar", `IF-WIRE-008`).
    pub const AVATAR_TOO_LARGE: &str = "avatar_too_large";
    pub const AVATAR_NOT_FOUND: &str = "avatar_not_found";
    /// `DELETE /v1/agents/{agent_id}` of an id in `[owner] post_as`
    /// (API_V1 "DELETE /v1/agents/{agent_id}", `IF-WIRE-009`).
    pub const AGENT_PROTECTED: &str = "agent_protected";
    /// 503 from `GET /healthz` when the database round trip fails. Not listed
    /// in API_V1; chosen in S0 for the `IF-OPS-001` failure path.
    pub const UNAVAILABLE: &str = "unavailable";
    /// Identity attestation codes (IDENTITY_ATTESTATION §4.4).
    pub const OWNER_NOT_ALLOWED: &str = "owner_not_allowed";
    pub const IDENTITY_DISABLED: &str = "identity_disabled";
    pub const UNKNOWN_AUDIENCE: &str = "unknown_audience";
    pub const TTL_TOO_LONG: &str = "ttl_too_long";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_wire_shape() {
        let body = ErrorBody::new(
            codes::IDEMPOTENCY_CONFLICT,
            "client_key is already bound to a different request",
            "req-01J00000000000000000000000",
        );
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "error": {
                    "code": "idempotency_conflict",
                    "message": "client_key is already bound to a different request",
                    "request_id": "req-01J00000000000000000000000",
                    "details": {}
                }
            })
        );
        let back: ErrorBody = serde_json::from_value(json).unwrap();
        assert_eq!(back, body);
    }

    #[test]
    fn carries_details() {
        let body = ErrorBody::new(codes::UNEXPECTED_FIELD, "unexpected field", "req-x")
            .with_detail("field", "extra");
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["error"]["details"]["field"], "extra");
    }
}
