//! Body of `POST /v1/identity/tokens` (IDENTITY_ATTESTATION §4).
//!
//! `aud` arrives as a plain string and `ttl_secs` as an optional integer so
//! that [`IdentityTokenRequest::validate`] can answer format errors as
//! `invalid_field` naming the field. Whether `aud` is allowed (byte for byte,
//! no trimming or case folding) and whether `ttl_secs` fits the configured
//! maximum are server decisions.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::AgentId;

/// Wire upper bound on `exp - iat`, agreed with token consumers
/// (`IF-IDT-JWT`, IDENTITY_ATTESTATION §5.2). A contract constant, not a
/// tuning value: configuration can only lower it.
pub const MAX_TOKEN_LIFETIME_SECS: u64 = 900;

/// Clock skew consumers allow when checking `iat` and `exp` (`IF-IDT-VERIFY`,
/// IDENTITY_ATTESTATION §6). The hub itself does not verify tokens.
pub const CLOCK_SKEW_SECS: u64 = 60;

/// Placeholder printed instead of the token by `Debug`.
const REDACTED: &str = "<redacted>";

/// A request field that failed validation, with the reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdentityRequestError {
    #[error("aud must be a non-empty string")]
    EmptyAudience,
    #[error("ttl_secs must be at least 1")]
    TtlTooSmall,
}

impl IdentityRequestError {
    /// The request field the error names, for `details.field`.
    pub fn field(&self) -> &'static str {
        match self {
            IdentityRequestError::EmptyAudience => "aud",
            IdentityRequestError::TtlTooSmall => "ttl_secs",
        }
    }
}

/// `POST /v1/identity/tokens` body. A missing `ttl_secs` means the server
/// default; `null` is not an integer and is rejected like any other type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityTokenRequest {
    pub aud: String,
    #[serde(
        default,
        deserialize_with = "present_integer",
        skip_serializing_if = "Option::is_none"
    )]
    pub ttl_secs: Option<u64>,
}

impl IdentityTokenRequest {
    /// Format checks that need no configuration: `aud` is not empty and
    /// `ttl_secs`, when present, is at least 1.
    pub fn validate(&self) -> Result<(), IdentityRequestError> {
        if self.aud.is_empty() {
            return Err(IdentityRequestError::EmptyAudience);
        }
        if self.ttl_secs == Some(0) {
            return Err(IdentityRequestError::TtlTooSmall);
        }
        Ok(())
    }
}

/// `POST /v1/identity/tokens` 201 body. `sub` and `aud` equal the token's
/// claims. The token is shown once; `Debug` never prints it.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityTokenResponse {
    pub token: String,
    pub sub: AgentId,
    pub aud: String,
}

impl fmt::Debug for IdentityTokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdentityTokenResponse")
            .field("token", &REDACTED)
            .field("sub", &self.sub)
            .field("aud", &self.aud)
            .finish()
    }
}

/// Deserializes a present value as `Some(u64)`, so an explicit `null` is a
/// type error instead of silently meaning "absent".
fn present_integer<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
where
    D: Deserializer<'de>,
{
    u64::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request(value: serde_json::Value) -> Result<IdentityTokenRequest, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn ttl_is_optional() {
        let parsed = request(json!({"aud": "garage-docs"})).unwrap();
        assert_eq!(parsed.ttl_secs, None);
        assert_eq!(parsed.validate(), Ok(()));
        let parsed = request(json!({"aud": "garage-docs", "ttl_secs": 1})).unwrap();
        assert_eq!(parsed.ttl_secs, Some(1));
        assert_eq!(parsed.validate(), Ok(()));
    }

    #[test]
    fn aud_is_required() {
        let err = request(json!({"ttl_secs": 5})).unwrap_err();
        assert!(err.to_string().contains("missing field `aud`"), "{err}");
    }

    #[test]
    fn rejects_unknown_fields_including_sub() {
        let err = request(json!({"aud": "x", "sub": "agent-b"})).unwrap_err();
        assert!(err.to_string().contains("unknown field `sub`"), "{err}");
    }

    #[test]
    fn ttl_must_be_a_non_negative_integer() {
        for ttl in [
            json!(null),
            json!(-1),
            json!(1.5),
            json!("600"),
            json!(true),
        ] {
            assert!(
                request(json!({"aud": "x", "ttl_secs": ttl})).is_err(),
                "{ttl}"
            );
        }
    }

    #[test]
    fn zero_ttl_is_rejected_on_validate() {
        let parsed = request(json!({"aud": "x", "ttl_secs": 0})).unwrap();
        let err = parsed.validate().unwrap_err();
        assert_eq!(err, IdentityRequestError::TtlTooSmall);
        assert_eq!(err.field(), "ttl_secs");
    }

    #[test]
    fn aud_must_not_be_empty_but_is_otherwise_free_form() {
        let empty = request(json!({"aud": ""})).unwrap();
        let err = empty.validate().unwrap_err();
        assert_eq!(err, IdentityRequestError::EmptyAudience);
        assert_eq!(err.field(), "aud");
        // Allow-list membership is the server's byte-exact decision.
        for aud in [" ", "Garage-docs", "garage-docs ", "x.y"] {
            assert_eq!(request(json!({"aud": aud})).unwrap().validate(), Ok(()));
        }
    }

    #[test]
    fn omitted_ttl_is_not_serialized() {
        let body = IdentityTokenRequest {
            aud: "garage-docs".into(),
            ttl_secs: None,
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({"aud": "garage-docs"})
        );
    }

    #[test]
    fn response_has_exactly_three_fields_and_redacts_debug() {
        let response = IdentityTokenResponse {
            token: "header.claims.signature".into(),
            sub: "agent-a".parse().unwrap(),
            aud: "garage-docs".into(),
        };
        assert_eq!(
            serde_json::to_value(&response).unwrap(),
            json!({"token": "header.claims.signature", "sub": "agent-a", "aud": "garage-docs"})
        );
        assert!(!format!("{response:?}").contains("signature"));
    }
}
