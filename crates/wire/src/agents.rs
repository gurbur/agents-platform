//! Request and response bodies for agent registration, directory, receiver
//! route and wake policy, `all` membership and token rotation (API_V1
//! "Agent 등록과 directory", "Receiver 경로와 membership", "Token rotation";
//! `IF-WIRE-001`, `IF-WIRE-004`).
//!
//! Request types reject unknown fields. `agent_id`, `name` and
//! `receiver_url` arrive as plain strings so the server can answer format
//! errors with their dedicated codes (`invalid_agent_id`, `invalid_field`,
//! `invalid_receiver_url`) instead of a generic deserialization error.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::profile::present_patch;
use crate::{AgentId, AgentProfile, AvatarSummary, FieldPatch, Seq, UnknownValue};

/// Placeholder printed instead of a credential by `Debug` impls.
const REDACTED: &str = "<redacted>";

/// `POST /v1/agents` body. `name` is required and checked with
/// [`crate::validate_name`]; a missing `receiver_url` means the same as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisterAgentRequest {
    pub agent_id: String,
    pub name: String,
    #[serde(default)]
    pub receiver_url: Option<String>,
}

/// `POST /v1/agents` 201 body. The only place, besides rotation, where the
/// two credentials appear in plain text. `name` always has a value here;
/// the type matches the directory, where pre-S9 agents have none.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterAgentResponse {
    pub agent_id: AgentId,
    pub name: Option<String>,
    pub joined_all: bool,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub profile: AgentProfile,
    pub agent_api_key: String,
    pub receiver_secret: String,
}

impl fmt::Debug for RegisterAgentResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterAgentResponse")
            .field("agent_id", &self.agent_id)
            .field("name", &self.name)
            .field("joined_all", &self.joined_all)
            .field("receiver_url", &self.receiver_url)
            .field("route_version", &self.route_version)
            .field("profile", &self.profile)
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

/// Directory entry visible to agents: who the other agents are and what
/// they declared about themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentEntry {
    pub agent_id: AgentId,
    pub name: Option<String>,
    pub joined_all: bool,
    pub profile: AgentProfile,
    /// `null` when the agent has no avatar (S11). Absent from older hubs.
    #[serde(default)]
    pub avatar: Option<AvatarSummary>,
}

/// Directory entry visible to the owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerAgentEntry {
    pub agent_id: AgentId,
    pub name: Option<String>,
    pub joined_all: bool,
    pub profile: AgentProfile,
    pub receiver_url: Option<String>,
    /// The stored receiver setting, also without a receiver (S13).
    pub wake_policy: WakePolicy,
    pub route_version: i64,
    pub created_at: String,
    pub delivery: DeliverySummary,
    /// `null` when the agent has no avatar (S11). Absent from older hubs.
    #[serde(default)]
    pub avatar: Option<AvatarSummary>,
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

/// Which events wake an agent's receiver (`IF-RCV-WAKESET`): `sender`
/// (the default) follows the sender's `wake` and `mentions`; `always` also
/// wakes for quiet events. The sender is never woken by its own event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakePolicy {
    #[default]
    Sender,
    Always,
}

impl WakePolicy {
    pub const ALL: [WakePolicy; 2] = [WakePolicy::Sender, WakePolicy::Always];

    pub fn as_str(self) -> &'static str {
        match self {
            WakePolicy::Sender => "sender",
            WakePolicy::Always => "always",
        }
    }
}

impl FromStr for WakePolicy {
    type Err = UnknownValue;

    /// Exact spelling only, like the profile enums.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|policy| policy.as_str() == s)
            .ok_or(UnknownValue)
    }
}

impl fmt::Display for WakePolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// `PATCH /v1/agents/{agent_id}/receiver` body: `receiver_url` (`null`
/// disconnects the route) and `wake_policy`, at least one of them; an
/// omitted key keeps the stored value (`IF-WIRE-004`). `wake_policy`
/// arrives as a string so a wrong spelling is `invalid_field`; like any
/// optional field, `null` means the same as an omitted key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateReceiverRequest {
    #[serde(
        default,
        deserialize_with = "present_patch",
        skip_serializing_if = "FieldPatch::is_keep"
    )]
    pub receiver_url: FieldPatch<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_policy: Option<String>,
}

/// `PATCH /v1/agents/{agent_id}/receiver` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiverResponse {
    pub agent_id: AgentId,
    pub receiver_url: Option<String>,
    pub route_version: i64,
    pub wake_policy: WakePolicy,
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
pub(crate) fn present_nullable<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
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
            serde_json::from_value(json!({"agent_id": "agent-a", "name": "A"})).unwrap();
        assert_eq!(request.receiver_url, None);
    }

    #[test]
    fn register_request_requires_a_string_name() {
        let err = serde_json::from_value::<RegisterAgentRequest>(json!({"agent_id": "agent-a"}))
            .unwrap_err();
        assert!(err.to_string().contains("missing field `name`"), "{err}");
        for name in [json!(null), json!(7)] {
            let err = serde_json::from_value::<RegisterAgentRequest>(
                json!({"agent_id": "agent-a", "name": name}),
            )
            .unwrap_err();
            assert!(err.to_string().contains("invalid type"), "{err}");
        }
    }

    #[test]
    fn register_request_rejects_profile_fields() {
        let err = serde_json::from_value::<RegisterAgentRequest>(
            json!({"agent_id": "agent-a", "name": "A", "role": "ops"}),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown field `role`"), "{err}");
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
    fn receiver_request_keeps_omitted_keys_and_serializes_only_given_ones() {
        let null: UpdateReceiverRequest =
            serde_json::from_value(json!({"receiver_url": null})).unwrap();
        assert_eq!(null.receiver_url, FieldPatch::Clear);
        let empty: UpdateReceiverRequest = serde_json::from_value(json!({})).unwrap();
        assert_eq!(empty, UpdateReceiverRequest::default());

        let request = UpdateReceiverRequest {
            receiver_url: FieldPatch::Set("http://r/n".into()),
            wake_policy: Some("always".into()),
        };
        assert_eq!(
            serde_json::to_value(&request).unwrap(),
            json!({"receiver_url": "http://r/n", "wake_policy": "always"})
        );
        let unset = UpdateReceiverRequest {
            receiver_url: FieldPatch::Clear,
            wake_policy: None,
        };
        assert_eq!(
            serde_json::to_value(&unset).unwrap(),
            json!({"receiver_url": null})
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
            name: Some("A".into()),
            joined_all: true,
            receiver_url: None,
            route_version: 1,
            profile: AgentProfile::default(),
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
