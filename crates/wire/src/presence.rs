//! Agent presence (API_V1 "Agent presence", `IF-WIRE-013`, ADR-0012): the
//! state an agent declares with a TTL and the directory's derived view.
//!
//! `state` arrives as a plain string so that a wrong value, `"offline"`
//! included, is `invalid_field` from [`PresenceRequest::validate`] rather
//! than a deserialization error. Whether `ttl_secs` fits the configured
//! maximum (`ttl_too_long`) is a server decision.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AgentId, UnknownValue};

/// Most scalar values of a presence `note` after trimming. A contract
/// constant, not a tuning value.
pub const MAX_PRESENCE_NOTE_CHARS: usize = 80;

/// Upper bound of the configured `max_ttl_secs` (`IF-CFG-001` `[presence]`).
/// A contract constant, not a tuning value: configuration can only lower it.
pub const PRESENCE_TTL_CEILING_SECS: u64 = 86_400;

/// The displayed state. `online` and `busy` are declared; `offline` is
/// derived (no entry, or an expired one) and never accepted in a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceState {
    Online,
    Busy,
    Offline,
}

impl PresenceState {
    pub const ALL: [PresenceState; 3] = [
        PresenceState::Online,
        PresenceState::Busy,
        PresenceState::Offline,
    ];

    /// The states a `PUT …/presence` may declare.
    pub const DECLARABLE: [PresenceState; 2] = [PresenceState::Online, PresenceState::Busy];

    pub fn as_str(self) -> &'static str {
        match self {
            PresenceState::Online => "online",
            PresenceState::Busy => "busy",
            PresenceState::Offline => "offline",
        }
    }

    /// The declarable state spelled exactly `raw`.
    pub fn declared(raw: &str) -> Result<Self, UnknownValue> {
        Self::DECLARABLE
            .into_iter()
            .find(|state| state.as_str() == raw)
            .ok_or(UnknownValue)
    }
}

impl FromStr for PresenceState {
    type Err = UnknownValue;

    /// Exact spelling only, like the profile enums.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or(UnknownValue)
    }
}

impl fmt::Display for PresenceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An agent's presence as the directory shows it. All four keys are always
/// serialized; `offline` has `null` in the other three.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Presence {
    pub state: PresenceState,
    pub note: Option<String>,
    pub updated_at: Option<String>,
    pub expires_at: Option<String>,
}

impl Presence {
    /// No declaration, or an expired one.
    pub fn offline() -> Self {
        Self {
            state: PresenceState::Offline,
            note: None,
            updated_at: None,
            expires_at: None,
        }
    }
}

/// `offline`, also for a directory item from a hub older than S14.
impl Default for Presence {
    fn default() -> Self {
        Self::offline()
    }
}

/// `PUT /v1/agents/{agent_id}/presence` body. An omitted `note` or
/// `ttl_secs` means the same as `null`; `ttl_secs` must be a JSON integer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceRequest {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_secs: Option<i64>,
}

/// A request that passed [`PresenceRequest::validate`]: a declarable
/// state, the trimmed note and a TTL of at least one second (`None` = the
/// server default).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresenceDeclaration {
    pub state: PresenceState,
    pub note: Option<String>,
    pub ttl_secs: Option<u64>,
}

impl PresenceRequest {
    /// Value rules in `IF-WIRE-013` order: `state`, `note`, `ttl_secs`; the
    /// first violation wins.
    pub fn validate(self) -> Result<PresenceDeclaration, PresenceFieldError> {
        let state = PresenceState::declared(&self.state).map_err(|_| PresenceFieldError::State)?;
        let note = self
            .note
            .as_deref()
            .map(validate_presence_note)
            .transpose()
            .map_err(PresenceFieldError::Note)?;
        let ttl_secs = match self.ttl_secs {
            None => None,
            Some(ttl) if ttl >= 1 => Some(ttl.unsigned_abs()),
            Some(_) => return Err(PresenceFieldError::TtlTooSmall),
        };
        Ok(PresenceDeclaration {
            state,
            note,
            ttl_secs,
        })
    }
}

/// Why a presence request field was rejected, for 422 `invalid_field` with
/// `details.field`. Messages never repeat the value: a note is free text,
/// kept out of logs like a message body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PresenceFieldError {
    #[error("state must be online or busy; withdraw a declaration with DELETE")]
    State,
    #[error("{0}")]
    Note(PresenceNoteError),
    #[error("ttl_secs must be at least 1")]
    TtlTooSmall,
}

impl PresenceFieldError {
    /// The request field the error names, for `details.field`.
    pub fn field(&self) -> &'static str {
        match self {
            PresenceFieldError::State => "state",
            PresenceFieldError::Note(_) => "note",
            PresenceFieldError::TtlTooSmall => "ttl_secs",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PresenceNoteError {
    #[error("note must not be empty after trimming; omit it for no note")]
    Empty,
    #[error("note must be at most {MAX_PRESENCE_NOTE_CHARS} characters after trimming")]
    TooLong,
    #[error("note must be one line without control characters")]
    ControlCharacter,
}

/// Validates a presence note the way `name` is validated (`IF-WIRE-006`
/// method): `str::trim`, 1..=[`MAX_PRESENCE_NOTE_CHARS`] scalar values, no
/// Unicode `Cc` character (line feeds included). Returns the trimmed note.
pub fn validate_presence_note(raw: &str) -> Result<String, PresenceNoteError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(PresenceNoteError::Empty);
    }
    if trimmed.chars().count() > MAX_PRESENCE_NOTE_CHARS {
        return Err(PresenceNoteError::TooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(PresenceNoteError::ControlCharacter);
    }
    Ok(trimmed.to_owned())
}

/// `PUT /v1/agents/{agent_id}/presence` 200 body. `last_seen_at` is the
/// stored value when the request was answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceResponse {
    pub agent_id: AgentId,
    pub presence: Presence,
    pub last_seen_at: Option<String>,
}
