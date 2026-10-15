//! Delivery diagnostics, manual retry (API_V1 "Delivery 진단과 receiver
//! 알림") and the receiver wake body (RECEIVER_WAKE_CONVENTION §6,
//! `IF-RCV-BODY`).

use serde::{Deserialize, Serialize};

use crate::{AgentId, DeliveryState, Seq};

/// The only wake body version defined by v1.
pub const WAKE_VERSION: u32 = 1;

/// Upper bound of a wake body in UTF-8 bytes (`IF-RCV-BODY`).
pub const MAX_WAKE_BODY_BYTES: usize = 1024;

/// `GET /v1/agents/{agent_id}/delivery` 200 body. Secrets and the receiver
/// URL itself are never part of it; `receiver_url_present` says whether one
/// is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryStatus {
    pub read_marker: Seq,
    pub inbox_head_seq: Seq,
    /// The head while `read_marker < inbox_head_seq`, else `null`.
    pub pending_seq: Option<Seq>,
    /// The `inbox_seq` of the last POST answered with a valid 202. Not a
    /// read marker and not necessarily the head.
    pub last_accepted_seq: Option<Seq>,
    pub route_version: i64,
    pub receiver_url_present: bool,
    pub state: DeliveryState,
    pub last_attempt_at: Option<String>,
    /// A code-level summary such as `http_503`, `timeout` or
    /// `protocol_mismatch`.
    pub last_error: Option<String>,
}

/// `POST /v1/agents/{agent_id}/delivery/retry` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRetryResponse {
    pub state: DeliveryState,
    pub route_version: i64,
    pub pending_seq: Option<Seq>,
    pub last_accepted_seq: Option<Seq>,
}

/// The callback body `{"v":1,"agent_id":"..","inbox_seq":".."}`. Exactly
/// these three members; `inbox_seq` is a decimal string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WakeNotification {
    pub v: u32,
    pub agent_id: AgentId,
    pub inbox_seq: Seq,
}

impl WakeNotification {
    pub fn new(agent_id: AgentId, inbox_seq: Seq) -> Self {
        Self {
            v: WAKE_VERSION,
            agent_id,
            inbox_seq,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wake_body_has_exactly_three_members_with_a_string_seq() {
        let body = WakeNotification::new("agent-a".parse().unwrap(), Seq::new(42).unwrap());
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({"v": 1, "agent_id": "agent-a", "inbox_seq": "42"})
        );
    }

    #[test]
    fn longest_wake_body_fits_the_limit() {
        let agent_id: AgentId = format!("a{}", "z".repeat(63)).parse().unwrap();
        let body = WakeNotification::new(agent_id, Seq::new(i64::MAX).unwrap());
        let bytes = serde_json::to_vec(&body).unwrap();
        assert!(bytes.len() <= MAX_WAKE_BODY_BYTES, "{} bytes", bytes.len());
    }

    #[test]
    fn wake_body_rejects_extra_members_and_numeric_seq() {
        let extra = json!({"v": 1, "agent_id": "agent-a", "inbox_seq": "1", "body": "x"});
        assert!(serde_json::from_value::<WakeNotification>(extra).is_err());
        let numeric = json!({"v": 1, "agent_id": "agent-a", "inbox_seq": 1});
        assert!(serde_json::from_value::<WakeNotification>(numeric).is_err());
    }

    #[test]
    fn status_serializes_absent_values_as_null() {
        let status = DeliveryStatus {
            read_marker: Seq::ZERO,
            inbox_head_seq: Seq::ZERO,
            pending_seq: None,
            last_accepted_seq: None,
            route_version: 1,
            receiver_url_present: false,
            state: DeliveryState::NoReceiver,
            last_attempt_at: None,
            last_error: None,
        };
        assert_eq!(
            serde_json::to_value(&status).unwrap(),
            json!({
                "read_marker": "0",
                "inbox_head_seq": "0",
                "pending_seq": null,
                "last_accepted_seq": null,
                "route_version": 1,
                "receiver_url_present": false,
                "state": "no_receiver",
                "last_attempt_at": null,
                "last_error": null
            })
        );
    }
}
