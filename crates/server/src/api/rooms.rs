//! `GET /v1/rooms` and `POST /v1/rooms/{room_id}/messages` (`IF-API-ROOMS`):
//! the owner lists every room and posts into any room as an agent named in
//! `[owner] post_as`.
//!
//! Post order (`IF-WIRE-007`): authentication → owner only (a parts
//! extractor, so before the body) → `StrictJson` → `client_key` → decoded
//! `body` size → `reply_to` format → `room_id` format → allowlist →
//! transaction (sender registered → idempotency → room members →
//! `reply_to`).

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use platform_wire::{
    AgentId, MessageResponse, ROOMS_SOFT_CAP, RoomId, RoomIdError, RoomList, RoomPostRequest,
};
use tracing::info;

use super::agents::OwnerOnly;
use super::json::StrictJson;
use super::messages::{
    check_body_size, check_client_key, new_message_id, parse_reply_to, rejection_error,
};
use super::query::QueryParams;
use super::{ApiError, AppState};
use crate::clock;
use crate::store::queries::messages::{self as message_queries, RoomPostCommand, SendOutcome};
use crate::store::queries::rooms as room_queries;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/rooms", get(list))
        .route("/v1/rooms/{room_id}/messages", post(post_message))
}

/// `GET /v1/rooms`: every room with a message plus `all`, newest first.
async fn list(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<RoomList>, ApiError> {
    QueryParams::parse(query.as_deref(), &[])?;
    let rooms = state
        .store
        .run(|conn| room_queries::list_rooms(conn, ROOMS_SOFT_CAP))
        .await?;
    Ok(Json(RoomList { rooms }))
}

/// `POST /v1/rooms/{room_id}/messages`: one durable commit with the same
/// writes as `POST /v1/messages`.
async fn post_message(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    Path(raw_room_id): Path<String>,
    StrictJson(request): StrictJson<RoomPostRequest>,
) -> Result<(StatusCode, Json<MessageResponse>), ApiError> {
    let RoomPostRequest {
        sender_id,
        body,
        client_key,
        reply_to,
    } = request;
    check_client_key(&client_key)?;
    check_body_size(&body)?;
    let reply_to = parse_reply_to(reply_to)?;
    let room: RoomId = raw_room_id
        .parse()
        .map_err(|err: RoomIdError| ApiError::invalid_field("room_id", err.to_string()))?;
    let sender = allowed_sender(&state.post_as, &sender_id)?;

    let outcome = state
        .store
        .transaction(move |tx| {
            // Generated on the DB thread so `created_at` follows `event_seq` order.
            let command = RoomPostCommand {
                sender,
                client_key,
                room,
                body,
                reply_to,
                message_id: new_message_id(),
                created_at: clock::now_rfc3339_ms(),
            };
            message_queries::commit_room_post(tx, &command)
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
                "owner post committed"
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

/// The allowlisted agent named by `sender_id`, compared verbatim; 403
/// `forbidden` naming the field otherwise, including when the list is empty.
fn allowed_sender(post_as: &[AgentId], sender_id: &str) -> Result<AgentId, ApiError> {
    post_as
        .iter()
        .find(|allowed| allowed.as_str() == sender_id)
        .cloned()
        .ok_or_else(|| ApiError::forbidden().with_detail("field", "sender_id"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_wire::codes;

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
}
