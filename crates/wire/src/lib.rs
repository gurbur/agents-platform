//! Wire types of agents-platform; other repositories (hub-mcp, hub-discord) depend on this crate by path (ADR-0010).
//!
//! The one definition of the HTTP wire shared by `platformd`, the
//! `platform` CLI and those consumers.
//!
//! This crate is the single validation point for identifiers and sequence
//! representations (`IF-ID-001`). It deliberately depends only on serde,
//! serde_json and thiserror.

pub mod agent_id;
pub mod agents;
pub mod avatar;
pub mod delivery;
pub mod error;
pub mod health;
pub mod identity;
pub mod inbox;
pub mod messages;
pub mod presence;
pub mod profile;
pub mod reactions;
pub mod rooms;
pub mod seq;
pub mod threads;

pub use agent_id::{AgentId, AgentIdError};
pub use agents::{
    AgentEntry, AgentList, DeliveryState, DeliverySummary, MembershipRequest, MembershipResponse,
    OwnerAgentEntry, ReceiverResponse, RegisterAgentRequest, RegisterAgentResponse,
    RotateTokensResponse, UpdateReceiverRequest, WakePolicy,
};
pub use avatar::{
    AVATAR_MAX_BYTES, Avatar, AvatarContentType, AvatarError, AvatarResponse, AvatarSummary,
    OwnerInfo, validate_avatar,
};
pub use delivery::{DeliveryRetryResponse, DeliveryStatus, WakeNotification};
pub use error::{ErrorBody, ErrorDetail, codes};
pub use health::HealthStatus;
pub use identity::{IdentityRequestError, IdentityTokenRequest, IdentityTokenResponse};
pub use inbox::{InboxEvent, InboxItem, InboxPage, ReadMarkerRequest, ReadMarkerResponse};
pub use messages::{
    HistoryCursor, HistoryPage, MAX_MENTIONS, Message, MessageId, MessageIdError, MessageResponse,
    RoomId, RoomIdError, RoomSlug, RoomSlugError, SendMessageRequest,
};
pub use presence::{
    MAX_PRESENCE_NOTE_CHARS, PRESENCE_TTL_CEILING_SECS, Presence, PresenceDeclaration,
    PresenceFieldError, PresenceNoteError, PresenceRequest, PresenceResponse, PresenceState,
    validate_presence_note,
};
pub use profile::{
    AgeBand, AgentProfile, FieldPatch, GenderForm, MAX_INTRO_CHARS, MAX_NAME_CHARS, MAX_ROLE_CHARS,
    ProfileField, ProfileFieldError, ProfileFieldProblem, ProfileResponse, ProfileUpdate,
    UnknownValue, UpdateProfileRequest, validate_name,
};
pub use reactions::{
    GlyphError, MAX_GLYPH_CHARS, MAX_REACTION_LABEL_CHARS, PutReactionKindRequest, ReactRequest,
    ReactResponse, ReactionChange, ReactionEvent, ReactionFeedPage, ReactionKind,
    ReactionKindEntry, ReactionKindError, ReactionKindFieldError, ReactionKindList,
    ReactionKindUpdate, ReactionLabelError, ReactionSummary, validate_glyph,
    validate_reaction_label,
};
pub use rooms::{
    CreateRoomRequest, MAX_ROOM_TITLE_CHARS, OWNER_CREATOR, ROOMS_SOFT_CAP, RoomKind, RoomList,
    RoomSummary, RoomTitleError, validate_room_title,
};
pub use seq::{Seq, SeqError};
pub use threads::{
    JoinedVia, ParticipationRequest, ParticipationResponse, ThreadParticipant, ThreadSummary,
    ThreadView,
};
