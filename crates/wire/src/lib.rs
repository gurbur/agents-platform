//! Wire types shared by `platformd` and the `platform` CLI.
//!
//! This crate is the single validation point for identifiers and sequence
//! representations (`IF-ID-001`). It deliberately depends only on serde,
//! serde_json and thiserror.

pub mod agent_id;
pub mod agents;
pub mod delivery;
pub mod error;
pub mod health;
pub mod identity;
pub mod inbox;
pub mod messages;
pub mod profile;
pub mod seq;

pub use agent_id::{AgentId, AgentIdError};
pub use agents::{
    AgentEntry, AgentList, DeliveryState, DeliverySummary, MembershipRequest, MembershipResponse,
    OwnerAgentEntry, ReceiverResponse, RegisterAgentRequest, RegisterAgentResponse,
    RotateTokensResponse, UpdateReceiverRequest,
};
pub use delivery::{DeliveryRetryResponse, DeliveryStatus, WakeNotification};
pub use error::{ErrorBody, ErrorDetail, codes};
pub use health::HealthStatus;
pub use identity::{IdentityRequestError, IdentityTokenRequest, IdentityTokenResponse};
pub use inbox::{InboxItem, InboxPage, ReadMarkerRequest, ReadMarkerResponse};
pub use messages::{
    HistoryCursor, HistoryPage, Message, MessageId, MessageIdError, MessageResponse, Recipient,
    RecipientError, RoomId, RoomIdError, SendMessageRequest,
};
pub use profile::{
    AgeBand, AgentProfile, FieldPatch, GenderForm, MAX_INTRO_CHARS, MAX_NAME_CHARS, MAX_ROLE_CHARS,
    ProfileField, ProfileFieldError, ProfileFieldProblem, ProfileResponse, ProfileUpdate,
    UnknownValue, UpdateProfileRequest, validate_name,
};
pub use seq::{Seq, SeqError};
