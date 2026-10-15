//! `agent_id`: `^[a-z][a-z0-9_-]{0,63}$`, reserved words rejected (`IF-ID-001`).

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Maximum length of an agent id in bytes (all allowed characters are ASCII).
pub const AGENT_ID_MAX_LEN: usize = 64;

/// Identifiers that name rooms or principals and can never be agent ids.
pub const RESERVED_AGENT_IDS: &[&str] = &["all", "owner"];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentIdError {
    #[error("agent_id must not be empty")]
    Empty,
    #[error("agent_id must be at most {AGENT_ID_MAX_LEN} characters")]
    TooLong,
    #[error("agent_id must start with a lowercase ASCII letter")]
    InvalidStart,
    #[error("agent_id may only contain lowercase ASCII letters, digits, '_' and '-'")]
    InvalidCharacter,
    #[error("agent_id '{0}' is reserved")]
    Reserved(String),
}

/// A validated agent identifier. The only way to obtain one is through
/// validation, so holding an `AgentId` proves the format is correct.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

impl AgentId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(candidate: &str) -> Result<(), AgentIdError> {
        let mut chars = candidate.chars();
        let Some(first) = chars.next() else {
            return Err(AgentIdError::Empty);
        };
        if candidate.len() > AGENT_ID_MAX_LEN {
            return Err(AgentIdError::TooLong);
        }
        if !first.is_ascii_lowercase() {
            return Err(AgentIdError::InvalidStart);
        }
        let allowed =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-';
        if !chars.all(allowed) {
            return Err(AgentIdError::InvalidCharacter);
        }
        if RESERVED_AGENT_IDS.contains(&candidate) {
            return Err(AgentIdError::Reserved(candidate.to_owned()));
        }
        Ok(())
    }
}

impl FromStr for AgentId {
    type Err = AgentIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::validate(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for AgentId {
    type Error = AgentIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

impl AsRef<str> for AgentId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_valid_ids() {
        for valid in ["a", "agent-a", "agent_b", "x9", "a-1_b-2"] {
            let id: AgentId = valid.parse().unwrap_or_else(|e| panic!("{valid}: {e}"));
            assert_eq!(id.as_str(), valid);
            assert_eq!(id.to_string(), valid);
        }
    }

    #[test]
    fn accepts_exactly_max_length() {
        let id = "a".repeat(AGENT_ID_MAX_LEN);
        assert!(id.parse::<AgentId>().is_ok());
    }

    #[test]
    fn rejects_one_over_max_length() {
        let id = "a".repeat(AGENT_ID_MAX_LEN + 1);
        assert_eq!(id.parse::<AgentId>(), Err(AgentIdError::TooLong));
    }

    #[test]
    fn rejects_reserved_words() {
        for reserved in RESERVED_AGENT_IDS {
            assert!(matches!(
                reserved.parse::<AgentId>(),
                Err(AgentIdError::Reserved(_))
            ));
        }
    }

    #[test]
    fn rejects_uppercase() {
        assert_eq!("Agent".parse::<AgentId>(), Err(AgentIdError::InvalidStart));
        assert_eq!(
            "agentA".parse::<AgentId>(),
            Err(AgentIdError::InvalidCharacter)
        );
    }

    #[test]
    fn rejects_leading_digit_and_symbols() {
        assert_eq!("1agent".parse::<AgentId>(), Err(AgentIdError::InvalidStart));
        assert_eq!("-agent".parse::<AgentId>(), Err(AgentIdError::InvalidStart));
        assert_eq!("_agent".parse::<AgentId>(), Err(AgentIdError::InvalidStart));
    }

    #[test]
    fn rejects_empty_and_foreign_characters() {
        assert_eq!("".parse::<AgentId>(), Err(AgentIdError::Empty));
        for invalid in ["agent a", "agent.a", "agent/a", "agént"] {
            assert_eq!(
                invalid.parse::<AgentId>(),
                Err(AgentIdError::InvalidCharacter),
                "{invalid}"
            );
        }
    }

    #[test]
    fn serde_validates_on_deserialize() {
        let id: AgentId = serde_json::from_str("\"agent-a\"").unwrap();
        assert_eq!(serde_json::to_string(&id).unwrap(), "\"agent-a\"");
        assert!(serde_json::from_str::<AgentId>("\"owner\"").is_err());
        assert!(serde_json::from_str::<AgentId>("\"Agent\"").is_err());
        assert!(serde_json::from_str::<AgentId>("42").is_err());
    }
}
