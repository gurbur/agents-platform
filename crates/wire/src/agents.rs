//! Request and response bodies for agent registration, directory, receiver
//! route, `all` membership and token rotation (API_V1 "Agent 등록과
//! directory", "Receiver 경로와 membership", "Token rotation"; `IF-WIRE-001`).
//!
//! Request types reject unknown fields. `agent_id` and `receiver_url` arrive
//! as plain strings so the server can answer format errors with their
//! dedicated codes (`invalid_agent_id`, `invalid_receiver_url`) instead of a
//! generic deserialization error.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::{AgentId, Seq};

/// Placeholder printed instead of a credential by `Debug` impls.
const REDACTED: &str = "<redacted>";

/// `POST /v1/agents` body. A missing `receiver_url` means the same as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterAgentRequest {
    pub agent_id: String,
    #[serde(default)]
    pub receiver_url: Option<String>,
}

/// `POST /v1/agents` 201 body. The only place, besides rotation, where the
/// two credentials appear in plain text.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAgentResponse {
    pub agent_id: AgentId,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub agent_api_key: String,
    pub receiver_secret: String,
}

impl fmt::Debug for RegisterAgentResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterAgentResponse")
            .field("agent_id", &self.agent_id)
            .field("joined_all", &self.joined_all)
            .field("receiver_url", &self.receiver_url)
            .field("route_version", &self.route_version)
            .field("agent_api_key", &REDACTED)
            .field("receiver_secret", &REDACTED)
            .finish()
    }
}

/// `GET /v1/agents` body. `T` is [`AgentEntry`] for an agent caller and
/// [`OwnerAgentEntry`] for the owner (`IF-WIRE-001`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentList<T> {
    pub items: Vec<T>,
}

/// Directory entry visible to agents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    pub agent_id: AgentId,
    pub joined_all: bool,
}

/// Directory entry visible to the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerAgentEntry {
    pub agent_id: AgentId,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub created_at: String,
    pub delivery: DeliverySummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverySummary {
    pub state: DeliveryState,
}

/// Displayed delivery state. `no_receiver` is derived from a null
/// `receiver_url`; the other three are stored (`IF-DLV-001`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Idle,
    NoReceiver,
    Pending,
    Blocked,
}

/// `PATCH /v1/agents/{agent_id}/receiver` body. `receiver_url` must be
/// present; `null` disconnects the route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReceiverRequest {
    #[serde(deserialize_with = "present_nullable")]
    pub receiver_url: Option<String>,
}

/// `PATCH /v1/agents/{agent_id}/receiver` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverResponse {
    pub agent_id: AgentId,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub state: DeliveryState,
}

/// `PUT /v1/agents/{agent_id}/membership/all` body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipRequest {
    pub joined: bool,
}

/// `PUT /v1/agents/{agent_id}/membership/all` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MembershipResponse {
    pub agent_id: AgentId,
    pub joined_all: bool,
    pub membership_epoch: i64,
    pub effective_event_seq: Seq,
}

/// `POST /v1/agents/{agent_id}/tokens/rotate` 200 body.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RotateTokensResponse {
    pub agent_id: AgentId,
    pub route_version: i64,
    pub agent_api_key: String,
    pub receiver_secret: String,
    pub previous_keys_revoked: bool,
}

impl fmt::Debug for RotateTokensResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RotateTokensResponse")
            .field("agent_id", &self.agent_id)
            .field("route_version", &self.route_version)
            .field("agent_api_key", &REDACTED)
            .field("receiver_secret", &REDACTED)
            .field("previous_keys_revoked", &self.previous_keys_revoked)
            .finish()
    }
}

/// Deserializes `Option<T>` through `deserialize_with`, which makes serde
/// report a missing key as `missing field` instead of defaulting to `None`.
fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn register_request_defaults_receiver_url_to_none() {
        let request: RegisterAgentRequest =
            serde_json::from_value(json!({"agent_id": "agent-a"})).unwrap();
        assert_eq!(request.receiver_url, None);
    }

    #[test]
    fn register_request_rejects_unknown_fields() {
        let err = serde_json::from_value::<RegisterAgentRequest>(
            json!({"agent_id": "agent-a", "sender_id": "x"}),
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("unknown field `sender_id`"),
            "{err}"
        );
    }

    #[test]
    fn receiver_request_requires_the_key_but_accepts_null() {
        let null: UpdateReceiverRequest =
            serde_json::from_value(json!({"receiver_url": null})).unwrap();
        assert_eq!(null.receiver_url, None);

        let err = serde_json::from_value::<UpdateReceiverRequest>(json!({})).unwrap_err();
        assert!(
            err.to_string().contains("missing field `receiver_url`"),
            "{err}"
        );
    }

    #[test]
    fn delivery_state_uses_snake_case() {
        assert_eq!(
            serde_json::to_value(DeliveryState::NoReceiver).unwrap(),
            json!("no_receiver")
        );
    }

    #[test]
    fn credential_responses_redact_debug_output() {
        let registered = RegisterAgentResponse {
            agent_id: "agent-a".parse().unwrap(),
            joined_all: true,
            receiver_url: None,
            route_version: 1,
            agent_api_key: "pak_secret".into(),
            receiver_secret: "prs_secret".into(),
        };
        let rotated = RotateTokensResponse {
            agent_id: "agent-a".parse().unwrap(),
            route_version: 2,
            agent_api_key: "pak_secret".into(),
            receiver_secret: "prs_secret".into(),
            previous_keys_revoked: true,
        };
        for debug in [format!("{registered:?}"), format!("{rotated:?}")] {
            assert!(!debug.contains("secret\""), "{debug}");
            assert!(
                !debug.contains("pak_") && !debug.contains("prs_"),
                "{debug}"
            );
        }
    }
}
