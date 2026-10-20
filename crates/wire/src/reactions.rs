//! Reactions (S16, ADR-0014; API_V1 "반응", `IF-WIRE-016`, `IF-WIRE-017`):
//! the kind slug, the registry bodies, `PUT …/reactions/{kind}` and its
//! response, the per-message summary, the change feed and the `reaction`
//! inbox event.
//!
//! The hub knows no fixed set of kinds: [`ReactionKind`] checks only the
//! slug format, and the registry (`GET /v1/reaction-kinds`) says which
//! kinds exist. Glyphs and labels are owner-written text.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::{AgentId, MessageId, RoomId, RoomKind, Seq};

/// Most Unicode scalar values of a glyph (a variation selector counts as
/// one). A contract constant, not a tuning value.
pub const MAX_GLYPH_CHARS: usize = 4;

/// Most Unicode scalar values of a kind label after trimming. A contract
/// constant, not a tuning value.
pub const MAX_REACTION_LABEL_CHARS: usize = 32;

/// Longest slug in bytes (`^[a-z][a-z0-9_]{1,31}$`).
const MAX_SLUG_LEN: usize = 32;
const MIN_SLUG_LEN: usize = 2;

// ---- ReactionKind ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("a reaction kind matches ^[a-z][a-z0-9_]{{1,31}}$")]
pub struct ReactionKindError;

/// A reaction kind slug, `^[a-z][a-z0-9_]{1,31}$` (`IF-WIRE-017`): the one
/// format check. Whether the kind exists or is enabled is the registry's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ReactionKind(String);

impl ReactionKind {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn validate(candidate: &str) -> Result<(), ReactionKindError> {
        let bytes = candidate.as_bytes();
        let Some((first, rest)) = bytes.split_first() else {
            return Err(ReactionKindError);
        };
        let rest_ok = rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_');
        let length_ok = (MIN_SLUG_LEN..=MAX_SLUG_LEN).contains(&bytes.len());
        if !first.is_ascii_lowercase() || !rest_ok || !length_ok {
            return Err(ReactionKindError);
        }
        Ok(())
    }
}

impl FromStr for ReactionKind {
    type Err = ReactionKindError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::validate(s)?;
        Ok(Self(s.to_owned()))
    }
}

impl TryFrom<String> for ReactionKind {
    type Error = ReactionKindError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::validate(&value)?;
        Ok(Self(value))
    }
}

impl From<ReactionKind> for String {
    fn from(kind: ReactionKind) -> Self {
        kind.0
    }
}

impl fmt::Display for ReactionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

// ---- glyph and label ---------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GlyphError {
    #[error("glyph must not be empty")]
    Empty,
    #[error("glyph must be at most {MAX_GLYPH_CHARS} characters")]
    TooLong,
    #[error("glyph must not contain whitespace")]
    Whitespace,
    #[error("glyph must not contain control characters")]
    ControlCharacter,
}

/// A glyph is 1..=[`MAX_GLYPH_CHARS`] scalar values with no whitespace
/// and no control character (`IF-WIRE-017`). It is not trimmed: a space is
/// a rejection, not something to remove.
pub fn validate_glyph(raw: &str) -> Result<(), GlyphError> {
    if raw.is_empty() {
        return Err(GlyphError::Empty);
    }
    if raw.chars().count() > MAX_GLYPH_CHARS {
        return Err(GlyphError::TooLong);
    }
    if raw.chars().any(char::is_whitespace) {
        return Err(GlyphError::Whitespace);
    }
    if raw.chars().any(char::is_control) {
        return Err(GlyphError::ControlCharacter);
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReactionLabelError {
    #[error("label must not be empty after trimming")]
    Empty,
    #[error("label must be at most {MAX_REACTION_LABEL_CHARS} characters after trimming")]
    TooLong,
    #[error("label must be one line without control characters")]
    ControlCharacter,
}

/// Validates a kind label the way a directory `name` is validated
/// (`IF-WIRE-006` method): `str::trim`, 1..=[`MAX_REACTION_LABEL_CHARS`]
/// scalar values, no Unicode `Cc`. Returns the trimmed label.
pub fn validate_reaction_label(raw: &str) -> Result<String, ReactionLabelError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(ReactionLabelError::Empty);
    }
    if trimmed.chars().count() > MAX_REACTION_LABEL_CHARS {
        return Err(ReactionLabelError::TooLong);
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ReactionLabelError::ControlCharacter);
    }
    Ok(trimmed.to_owned())
}

// ---- registry ------------------------------------------------------------------------

/// One registered kind (`GET /v1/reaction-kinds` item, `PUT
/// /v1/reaction-kinds/{slug}` response).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionKindEntry {
    pub slug: ReactionKind,
    pub glyph: String,
    pub label: String,
    pub sort: i64,
    pub enabled: bool,
}

/// `GET /v1/reaction-kinds` body, in `sort` then `slug` order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionKindList {
    pub kinds: Vec<ReactionKindEntry>,
}

/// `PUT /v1/reaction-kinds/{slug}` body. `glyph` and `label` are always
/// given; an omitted `sort` or `enabled` takes the default for a new kind
/// and keeps the stored value of an existing one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutReactionKindRequest {
    pub glyph: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// A request that passed [`PutReactionKindRequest::validate`]: the glyph as
/// given, the trimmed label, a `sort` of at least 0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionKindUpdate {
    pub glyph: String,
    pub label: String,
    pub sort: Option<i64>,
    pub enabled: Option<bool>,
}

impl PutReactionKindRequest {
    /// Value rules in `IF-WIRE-017` order: `glyph`, `label`, `sort`; the
    /// first violation wins.
    pub fn validate(self) -> Result<ReactionKindUpdate, ReactionKindFieldError> {
        validate_glyph(&self.glyph).map_err(ReactionKindFieldError::Glyph)?;
        let label = validate_reaction_label(&self.label).map_err(ReactionKindFieldError::Label)?;
        if self.sort.is_some_and(|sort| sort < 0) {
            return Err(ReactionKindFieldError::NegativeSort);
        }
        Ok(ReactionKindUpdate {
            glyph: self.glyph,
            label,
            sort: self.sort,
            enabled: self.enabled,
        })
    }
}

/// Why a kind request field was rejected, for 422 `invalid_field` with
/// `details.field`. Messages never repeat the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReactionKindFieldError {
    #[error("{0}")]
    Glyph(GlyphError),
    #[error("{0}")]
    Label(ReactionLabelError),
    #[error("sort must be at least 0")]
    NegativeSort,
}

impl ReactionKindFieldError {
    /// The request field the error names, for `details.field`.
    pub fn field(&self) -> &'static str {
        match self {
            ReactionKindFieldError::Glyph(_) => "glyph",
            ReactionKindFieldError::Label(_) => "label",
            ReactionKindFieldError::NegativeSort => "sort",
        }
    }
}

// ---- reacting ----------------------------------------------------------------------------

/// `PUT /v1/messages/{message_id}/reactions/{kind}` body; an empty body
/// means the same as `{}`. `wake` absent is `false`. `sender_id` is the
/// owner's allowlisted identity (an agent sending it gets
/// `unexpected_field`); `DELETE` takes only `sender_id`, from the owner.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReactRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender_id: Option<String>,
}

/// The current reactions of one kind on a message (`IF-WIRE-012`):
/// `agents` in ASCII order, `count` its length.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionSummary {
    pub kind: ReactionKind,
    pub count: u64,
    pub agents: Vec<AgentId>,
}

/// `PUT …/reactions/{kind}` 201/200 body: the reaction's `change_seq` and
/// `created_at` (the stored ones for a 200) and the message's summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactResponse {
    pub message_id: MessageId,
    pub agent_id: AgentId,
    pub kind: ReactionKind,
    pub change_seq: Seq,
    pub created_at: String,
    pub reactions: Vec<ReactionSummary>,
}

/// One addition or removal in `GET /v1/reactions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionChange {
    pub change_seq: Seq,
    pub message_id: MessageId,
    pub agent_id: AgentId,
    pub kind: ReactionKind,
    pub removed: bool,
    pub created_at: String,
}

/// `GET /v1/reactions` body. `next_after_change_seq` is the last item's
/// `change_seq`, the request's `after_change_seq` for an empty page, or the
/// room's current cursor when `after_change_seq` was omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionFeedPage {
    pub items: Vec<ReactionChange>,
    pub next_after_change_seq: Seq,
    pub has_more: bool,
}

/// The fields of a `kind = "reaction"` inbox event after `inbox_seq`,
/// `kind` and `woke` (`IF-WIRE-012`): someone reacted to the recipient's
/// message `message_id`. `sender_id` is who reacted, `wake` its choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionEvent {
    pub change_seq: Seq,
    pub message_id: MessageId,
    pub room_id: RoomId,
    pub room_kind: RoomKind,
    pub thread_id: Option<MessageId>,
    pub sender_id: AgentId,
    pub reaction: ReactionKind,
    pub wake: bool,
    pub created_at: String,
}
