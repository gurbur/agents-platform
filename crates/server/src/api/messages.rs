//! `POST /v1/messages` (`IF-API-SEND`, `IF-WIRE-010`): one send for
//! agents and the owner. This handler runs steps 1–11 (format, allowlist,
//! self DM); [`message_queries::commit_send`] runs 12–18 in one
//! transaction.

use std::collections::BTreeSet;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use platform_wire::messages::{MAX_BODY_BYTES, MAX_CLIENT_KEY_BYTES};
use platform_wire::{
    AgentId, MAX_MENTIONS, MessageId, MessageResponse, RoomId, RoomIdError, SendMessageRequest,
    codes,
};
use tracing::info;
use ulid::Ulid;

use super::json::StrictJson;
use super::{ApiError, AppState, history};
use crate::auth::Principal;
use crate::clock;
use crate::store::queries::messages::{
    self as message_queries, Destination, SendAs, SendCommand, SendOutcome, SendRejection,
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/messages", post(send).get(history::list))
        .route("/v1/messages/{message_id}", get(history::get_one))
}

/// A send request that passed every check not needing the database.
struct ValidSend {
    principal: SendAs,
    sender: AgentId,
    client_key: String,
    destination: Destination,
    body: String,
    wake: bool,
    mentions: Vec<AgentId>,
}

/// The body arrives as a `Result` so an agent's malformed `sender_id` is
/// still answered as the unexpected field it is for agents (step 3).
async fn send(
    principal: Principal,
    State(state): State<AppState>,
    request: Result<StrictJson<SendMessageRequest>, ApiError>,
) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
    let request = match request {
        Ok(StrictJson(request)) => request,
        Err(err) if principal != Principal::Owner && err.names_field("sender_id") => {
            return Err(unexpected_sender_id());
        }
        Err(err) => return Err(err),
    };
    let valid = validate(&principal, &state.post_as, request)?;
    let post_as = state.post_as.to_vec();

    let outcome = state
        .store
        .transaction(move |tx| {
            // Generated on the DB thread so `created_at` follows `event_seq` order.
            let command = SendCommand {
                principal: valid.principal,
                sender: valid.sender,
                client_key: valid.client_key,
                destination: valid.destination,
                body: valid.body,
                wake: valid.wake,
                mentions: valid.mentions,
                post_as,
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
            woken,
        } => {
            if woken > 0 {
                state.delivery.notify();
            }
            // Counts only: no body, no mention values (`IF-WIRE-010`).
            info!(
                message_id = %message.message_id,
                room_id = %message.room_id,
                sender_id = %message.sender_id,
                event_seq = %message.event_seq,
                recipients,
                woken,
                wake = message.wake,
                mentions = message.mentions.len(),
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

/// Steps 3–11 of `IF-WIRE-010` (1–2 are the extractors).
fn validate(
    principal: &Principal,
    post_as: &[AgentId],
    request: SendMessageRequest,
) -> Result<ValidSend, ApiError> {
    let SendMessageRequest {
        room_id,
        reply_to,
        to,
        body,
        client_key,
        wake,
        mentions,
        sender_id,
    } = request;

    // 3 and 4: who sends.
    let sender_text = match (principal, sender_id) {
        (Principal::Agent(_), Some(_)) => return Err(unexpected_sender_id()),
        (Principal::Agent(agent_id), None) => agent_id.to_string(),
        (Principal::Owner, None) => {
            return Err(ApiError::invalid_field(
                "sender_id",
                "the owner sends with a sender_id from [owner] post_as",
            ));
        }
        (Principal::Owner, Some(sender_id)) => sender_id,
    };
    // 5
    let destinations = [&room_id, &reply_to, &to]
        .into_iter()
        .filter(|value| value.is_some())
        .count();
    if destinations != 1 {
        return Err(ApiError::unprocessable(
            codes::INVALID_DESTINATION,
            "send exactly one of room_id, reply_to and to",
        ));
    }
    // 6, 7
    check_client_key(&client_key)?;
    check_body_size(&body)?;
    // 8
    let destination = parse_destination(room_id, reply_to, to)?;
    // 9
    let mentions = parse_mentions(mentions.unwrap_or_default(), &sender_text)?;
    // 10
    let (principal, sender) = match principal {
        Principal::Agent(agent_id) => (SendAs::Agent, agent_id.clone()),
        Principal::Owner => (SendAs::Owner, allowed_sender(post_as, &sender_text)?),
    };
    // 11
    if matches!(&destination, Destination::Direct(to) if *to == sender) {
        return Err(rejection_error(SendRejection::SelfDm));
    }

    Ok(ValidSend {
        principal,
        sender,
        client_key,
        destination,
        body,
        wake: wake.unwrap_or(true),
        mentions,
    })
}

/// Step 8: the format of the one given destination.
fn parse_destination(
    room_id: Option<String>,
    reply_to: Option<String>,
    to: Option<String>,
) -> Result<Destination, ApiError> {
    if let Some(room_id) = room_id {
        let room = room_id
            .parse::<RoomId>()
            .map_err(|err: RoomIdError| ApiError::invalid_field("room_id", err.to_string()))?;
        return Ok(Destination::Room(room));
    }
    if let Some(reply_to) = parse_reply_to(reply_to)? {
        return Ok(Destination::ReplyTo(reply_to));
    }
    let to = to.unwrap_or_default();
    let recipient = to
        .parse::<AgentId>()
        .map_err(|err| ApiError::invalid_field("to", format!("`to` must be an agent_id: {err}")))?;
    Ok(Destination::Direct(recipient))
}

/// Step 9: agent ids, no duplicate, at most [`MAX_MENTIONS`], not the
/// sender. Returned in ASCII order.
fn parse_mentions(raw: Vec<String>, sender: &str) -> Result<Vec<AgentId>, ApiError> {
    let invalid = |message: String| ApiError::invalid_field("mentions", message);
    if raw.len() > MAX_MENTIONS {
        return Err(invalid(format!(
            "mentions names at most {MAX_MENTIONS} agents"
        )));
    }
    let mut mentions = BTreeSet::new();
    for text in raw {
        let agent_id = text
            .parse::<AgentId>()
            .map_err(|err| invalid(format!("mentions holds an invalid agent_id: {err}")))?;
        if agent_id.as_str() == sender {
            return Err(invalid("a sender cannot mention itself".to_owned()));
        }
        if !mentions.insert(agent_id) {
            return Err(invalid("mentions repeats an agent_id".to_owned()));
        }
    }
    Ok(mentions.into_iter().collect())
}

/// The allowlisted agent named by `sender_id`, compared verbatim; 403
/// `forbidden` naming the field otherwise, including when the list is empty.
fn allowed_sender(post_as: &[AgentId], sender_id: &str) -> Result<AgentId, ApiError> {
    post_as
        .iter()
        .find(|allowed| allowed.as_str() == sender_id)
        .cloned()
        .ok_or_else(|| ApiError::forbidden().with_detail("field", "sender_id"))
}

fn unexpected_sender_id() -> ApiError {
    ApiError::unprocessable(
        codes::UNEXPECTED_FIELD,
        "unexpected field `sender_id`: an agent sends as itself",
    )
    .with_detail("field", "sender_id")
}

/// 422 `invalid_field` unless `client_key` is 1..=128 ASCII bytes.
fn check_client_key(client_key: &str) -> Result<(), ApiError> {
    if is_valid_client_key(client_key) {
        return Ok(());
    }
    Err(ApiError::invalid_field(
        "client_key",
        format!("client_key must be 1..={MAX_CLIENT_KEY_BYTES} ASCII bytes"),
    ))
}

/// 413 `body_too_large` above 16 KiB of decoded UTF-8.
fn check_body_size(body: &str) -> Result<(), ApiError> {
    if body.len() <= MAX_BODY_BYTES {
        return Ok(());
    }
    Err(ApiError::payload_too_large(
        codes::BODY_TOO_LARGE,
        format!("body exceeds {MAX_BODY_BYTES} bytes of UTF-8"),
    ))
}

/// The optional `reply_to`, which must be a `msg-` ULID (`IF-WIRE-005`).
fn parse_reply_to(raw: Option<String>) -> Result<Option<MessageId>, ApiError> {
    raw.map(|raw| raw.parse::<MessageId>())
        .transpose()
        .map_err(|err| ApiError::invalid_field("reply_to", err.to_string()))
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

/// The error for a send that committed nothing.
fn rejection_error(rejection: SendRejection) -> ApiError {
    match rejection {
        SendRejection::IdempotencyConflict => ApiError::conflict(
            codes::IDEMPOTENCY_CONFLICT,
            "client_key is already bound to a different request",
        ),
        SendRejection::SelfDm => ApiError::unprocessable(
            codes::SELF_DM_NOT_SUPPORTED,
            "a DM needs two different agents",
        ),
        SendRejection::UnknownRecipient => {
            ApiError::not_found(codes::UNKNOWN_RECIPIENT, "no agent with this agent_id")
        }
        SendRejection::UnknownRoom => ApiError::not_found(
            codes::UNKNOWN_ROOM,
            "no such room, or a member of this DM is not a registered agent",
        ),
        SendRejection::UnknownSender => {
            ApiError::agent_not_found().with_detail("field", "sender_id")
        }
        SendRejection::NotRoomMember => ApiError::new(
            StatusCode::FORBIDDEN,
            codes::NOT_ROOM_MEMBER,
            "the sender is not a joined member of this room",
        ),
        SendRejection::RoomArchived => ApiError::conflict(
            codes::ROOM_ARCHIVED,
            "the room is archived and only readable",
        ),
        SendRejection::ReplyToNotFound => {
            ApiError::not_found(codes::REPLY_TO_NOT_FOUND, "reply_to names no message")
        }
        SendRejection::MentionNotMember(agent_id) => ApiError::unprocessable(
            codes::MENTION_NOT_MEMBER,
            "a mentioned agent is neither a joined member of the room nor a registered post_as identity",
        )
        .with_detail("field", "mentions")
        .with_detail("agent_id", agent_id.as_str()),
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
    fn only_allowlisted_senders_pass_verbatim() {
        let post_as: Vec<AgentId> = vec!["human".parse().unwrap()];
        assert_eq!(allowed_sender(&post_as, "human").unwrap().as_str(), "human");
        for refused in ["Human", " human", "agent-a", ""] {
            let err = allowed_sender(&post_as, refused).unwrap_err();
            assert_eq!(err.status(), StatusCode::FORBIDDEN, "{refused:?}");
            assert_eq!(err.code(), codes::FORBIDDEN);
            assert_eq!(err.details()["field"], "sender_id");
        }
        assert!(allowed_sender(&[], "human").is_err(), "empty = off");
    }

    #[test]
    fn mentions_come_back_sorted_and_reject_repeats_and_the_sender() {
        let sorted = parse_mentions(vec!["b".into(), "a".into()], "c").unwrap();
        assert_eq!(
            sorted,
            ["a".parse::<AgentId>().unwrap(), "b".parse().unwrap()]
        );
        for raw in [vec!["a".to_owned(), "a".to_owned()], vec!["c".to_owned()]] {
            let err = parse_mentions(raw, "c").unwrap_err();
            assert_eq!(err.details()["field"], "mentions");
        }
    }

    #[test]
    fn generated_message_ids_are_valid() {
        let id = new_message_id();
        assert!(id.as_str().starts_with("msg-"), "{id}");
    }
}
