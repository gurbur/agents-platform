//! `POST /v1/messages` (`IF-API-SEND`): validation in the S2 order, then one
//! durable commit through [`message_queries::commit_send`].
//!
//! Order: authentication (agents only; the owner is 403) → `StrictJson` →
//! `client_key` → decoded `body` size → `to` format → `reply_to` format →
//! transaction.

use axum::extract::{FromRequestParts, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::{get, post};
use axum::{Json, Router};
use platform_wire::messages::{MAX_BODY_BYTES, MAX_CLIENT_KEY_BYTES};
use platform_wire::{AgentId, MessageId, MessageResponse, Recipient, SendMessageRequest, codes};
use tracing::info;
use ulid::Ulid;

use super::json::StrictJson;
use super::{ApiError, AppState, history};
use crate::auth::Principal;
use crate::clock;
use crate::store::queries::messages::{
    self as message_queries, SendCommand, SendOutcome, SendRejection,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/messages", post(send).get(history::list))
        .route("/v1/messages/{message_id}", get(history::get_one))
}

/// Extractor: the calling agent. The owner cannot send (403 `forbidden`,
/// `IF-WIRE-003`); as a parts extractor this runs before the body is read.
struct SendingAgent(AgentId);

impl FromRequestParts<AppState> for SendingAgent {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match Principal::from_request_parts(parts, state).await? {
            Principal::Agent(agent_id) => Ok(SendingAgent(agent_id)),
            Principal::Owner => Err(ApiError::forbidden()),
        }
    }
}

/// A send request that passed every check not needing the database.
struct ValidSend {
    sender: AgentId,
    client_key: String,
    to: Recipient,
    body: String,
    reply_to: Option<MessageId>,
}

async fn send(
    SendingAgent(sender): SendingAgent,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<SendMessageRequest>,
) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
    let valid = validate(sender, request)?;

    let outcome = state
        .store
        .transaction(move |tx| {
            // Generated on the DB thread so `created_at` follows `event_seq` order.
            let command = SendCommand {
                sender: valid.sender,
                client_key: valid.client_key,
                to: valid.to,
                body: valid.body,
                reply_to: valid.reply_to,
                message_id: new_message_id(),
                created_at: clock::now_rfc3339_ms(),
            };
            message_queries::commit_send(tx, &command)
        })
        .await?;

    match outcome {
        SendOutcome::Created {
            message,
            recipients,
        } => {
            state.delivery.notify();
            info!(
                message_id = %message.message_id,
                room_id = %message.room_id,
                sender_id = %message.sender_id,
                event_seq = %message.event_seq,
                recipients,
                "message committed"
            );
            let body = MessageResponse {
                message,
                deduplicated: false,
            };
            Ok((StatusCode::CREATED, Json(body)))
        }
        SendOutcome::Deduplicated(message) => {
            info!(
                message_id = %message.message_id,
                sender_id = %message.sender_id,
                "repeated client_key returned the original message"
            );
            let body = MessageResponse {
                message,
                deduplicated: true,
            };
            Ok((StatusCode::OK, Json(body)))
        }
        SendOutcome::Rejected(rejection) => Err(rejection_error(rejection)),
    }
}

fn validate(sender: AgentId, request: SendMessageRequest) -> Result<ValidSend, ApiError> {
    let SendMessageRequest {
        to,
        body,
        client_key,
        reply_to,
    } = request;

    if !is_valid_client_key(&client_key) {
        return Err(ApiError::invalid_field(
            "client_key",
            format!("client_key must be 1..={MAX_CLIENT_KEY_BYTES} ASCII bytes"),
        ));
    }
    if body.len() > MAX_BODY_BYTES {
        return Err(ApiError::payload_too_large(
            codes::BODY_TOO_LARGE,
            format!("body exceeds {MAX_BODY_BYTES} bytes of UTF-8"),
        ));
    }
    let to = to
        .parse::<Recipient>()
        .map_err(|err| ApiError::invalid_field("to", err.to_string()))?;
    let reply_to = reply_to
        .map(|raw| raw.parse::<MessageId>())
        .transpose()
        .map_err(|err| ApiError::invalid_field("reply_to", err.to_string()))?;

    Ok(ValidSend {
        sender,
        client_key,
        to,
        body,
        reply_to,
    })
}

fn is_valid_client_key(client_key: &str) -> bool {
    (1..=MAX_CLIENT_KEY_BYTES).contains(&client_key.len()) && client_key.is_ascii()
}

fn new_message_id() -> MessageId {
    // Invariant: a ULID always prints as 26 upper-case Crockford digits
    // starting with 0..=7, which is exactly what `MessageId` accepts.
    MessageId::from_ulid_text(&Ulid::generate().to_string())
        .expect("a generated ULID is a valid message_id")
}

fn rejection_error(rejection: SendRejection) -> ApiError {
    match rejection {
        SendRejection::IdempotencyConflict => ApiError::conflict(
            codes::IDEMPOTENCY_CONFLICT,
            "client_key is already bound to a different request",
        ),
        SendRejection::NotAllMember => ApiError::new(
            StatusCode::FORBIDDEN,
            codes::NOT_ALL_MEMBER,
            "the sender has left `all`",
        ),
        SendRejection::SelfDm => ApiError::unprocessable(
            codes::SELF_DM_NOT_SUPPORTED,
            "a DM needs two different agents",
        ),
        SendRejection::UnknownRecipient => {
            ApiError::not_found(codes::UNKNOWN_RECIPIENT, "no agent with this agent_id")
        }
        SendRejection::ReplyToNotFound => {
            ApiError::not_found(codes::REPLY_TO_NOT_FOUND, "reply_to names no message")
        }
        SendRejection::ReplyToRoomMismatch => ApiError::unprocessable(
            codes::REPLY_TO_ROOM_MISMATCH,
            "reply_to names a message in another room",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_key_is_ascii_within_bounds() {
        assert!(is_valid_client_key("k"));
        assert!(is_valid_client_key(&"k".repeat(MAX_CLIENT_KEY_BYTES)));
        assert!(!is_valid_client_key(""));
        assert!(!is_valid_client_key(&"k".repeat(MAX_CLIENT_KEY_BYTES + 1)));
        assert!(!is_valid_client_key("키"));
    }

    #[test]
    fn generated_message_ids_are_valid() {
        let id = new_message_id();
        assert!(id.as_str().starts_with("msg-"), "{id}");
    }
}
