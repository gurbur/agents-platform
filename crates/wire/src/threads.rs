//! Threads (S15, ADR-0013; API_V1 "스레드", `IF-WIRE-014`): the summary a
//! root carries, `GET /v1/threads/{root_id}` and
//! `PUT /v1/threads/{root_id}/participation`.
//!
//! A reply is always in a thread: the root is the first message, whose own
//! `thread_id` is `null`. Participants are registered by the hub in the
//! send transaction; a participant can only mute or unmute itself.

use serde::{Deserialize, Serialize};

use crate::{AgentId, MessageId, RoomId, RoomKind, Seq};

/// What a root with replies carries in `thread_summary` (`IF-WIRE-012`),
/// counted when read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadSummary {
    pub reply_count: u64,
    pub last_event_seq: Seq,
}

/// How an agent became a participant (`IF-WIRE-014`). The first row an
/// agent gets in a thread stays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JoinedVia {
    /// Wrote the root.
    Root,
    /// Wrote the first reply.
    Opener,
    /// Wrote in the thread later.
    Poster,
    /// Was mentioned by a thread message.
    Mentioned,
    /// An owner post as an allowlisted identity that is no joined member.
    Barged,
}

impl JoinedVia {
    pub fn as_str(self) -> &'static str {
        match self {
            JoinedVia::Root => "root",
            JoinedVia::Opener => "opener",
            JoinedVia::Poster => "poster",
            JoinedVia::Mentioned => "mentioned",
            JoinedVia::Barged => "barged",
        }
    }

    /// The stored `thread_participants.joined_via` text back as a value.
    pub fn parse(stored: &str) -> Option<Self> {
        match stored {
            "root" => Some(JoinedVia::Root),
            "opener" => Some(JoinedVia::Opener),
            "poster" => Some(JoinedVia::Poster),
            "mentioned" => Some(JoinedVia::Mentioned),
            "barged" => Some(JoinedVia::Barged),
            _ => None,
        }
    }
}

/// One participant of a thread view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadParticipant {
    pub agent_id: AgentId,
    pub joined_via: JoinedVia,
    /// The participant turned the thread's wakes off; mentions still wake.
    pub muted: bool,
    pub joined_at: String,
}

/// `GET /v1/threads/{root_id}` body. Participants are in `joined_at`
/// order, then `agent_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThreadView {
    pub thread_id: MessageId,
    pub room_id: RoomId,
    pub room_kind: RoomKind,
    pub root_sender_id: AgentId,
    pub participants: Vec<ThreadParticipant>,
    pub reply_count: u64,
    pub last_event_seq: Seq,
    pub last_message_at: String,
}

/// `PUT /v1/threads/{root_id}/participation` body. An agent mutes itself;
/// the owner names an allowlisted `agent_id`, which an agent may not send
/// (the server answers `unexpected_field`). Kept as text so a bad id is
/// the server's 403, not a parse error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParticipationRequest {
    pub wake: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
}

/// `PUT /v1/threads/{root_id}/participation` 200 body: the participant of
/// that thread after the change (`muted = !wake`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParticipationResponse {
    pub thread_id: MessageId,
    #[serde(flatten)]
    pub participant: ThreadParticipant,
}
