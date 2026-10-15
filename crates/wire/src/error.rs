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
    pub const NOT_ALL_MEMBER: &str = "not_all_member";
    pub const INVALID_AGENT_ID: &str = "invalid_agent_id";
    pub const AGENT_ID_TAKEN: &str = "agent_id_taken";
    pub const UNKNOWN_RECIPIENT: &str = "unknown_recipient";
    pub const SELF_DM_NOT_SUPPORTED: &str = "self_dm_not_supported";
    pub const IDEMPOTENCY_CONFLICT: &str = "idempotency_conflict";
    pub const REPLY_TO_ROOM_MISMATCH: &str = "reply_to_room_mismatch";
    pub const MUTUALLY_EXCLUSIVE_CURSORS: &str = "mutually_exclusive_cursors";
    pub const READ_MARKER_AHEAD: &str = "read_marker_ahead";
    pub const BODY_TOO_LARGE: &str = "body_too_large";
    pub const REQUEST_TOO_LARGE: &str = "request_too_large";
    pub const INVALID_RECEIVER_URL: &str = "invalid_receiver_url";
    pub const UNEXPECTED_FIELD: &str = "unexpected_field";
    pub const INVALID_FIELD: &str = "invalid_field";
    pub const INVALID_JSON: &str = "invalid_json";
    pub const UNSUPPORTED_MEDIA_TYPE: &str = "unsupported_media_type";
    /// 503 from `GET /healthz` when the database round trip fails. Not listed
    /// in API_V1; chosen in S0 for the `IF-OPS-001` failure path.
    pub const UNAVAILABLE: &str = "unavailable";
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
