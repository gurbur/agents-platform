//! `GET /v1/messages` and `GET /v1/messages/{message_id}` (`IF-API-HISTORY`).
//!
//! Query order: parameter names → `room_id` (422) → room permission (the
//! store's: owner 404 `unknown_room`, agent 403 `forbidden`) → cursor
//! exclusivity (400) → cursor and `limit` values (422). Unknown or repeated
//! parameters are rejected rather than silently ignored.

use axum::Json;
use axum::extract::{Path, RawQuery, State};
use platform_wire::{HistoryCursor, HistoryPage, Message, MessageId, RoomId, codes};

use super::query::{QueryParams, parse_seq};
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::store::queries::messages::{self as message_queries, HistoryDirection, HistoryQuery};
use crate::store::queries::rooms::{self as room_queries, ReadAccess, Viewer};

const PARAMS: &[&str] = &[
    "room_id",
    "before_event_seq",
    "after_event_seq",
    "limit",
    "q",
];

/// `GET /v1/messages`: one page of a room, newest first by default.
pub async fn list(
    principal: Principal,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<HistoryPage>, ApiError> {
    let mut params = QueryParams::parse(query.as_deref(), PARAMS)?;
    let room = parse_room(params.get("room_id"))?;
    let viewer = viewer_of(principal);
    let checked = room.clone();
    let access = state
        .store
        .run(move |conn| room_queries::read_access(conn, &viewer, &checked))
        .await?;
    match access {
        ReadAccess::Allowed => {}
        ReadAccess::UnknownRoom => return Err(unknown_room()),
        ReadAccess::Forbidden => return Err(ApiError::forbidden()),
    }
    let direction = parse_cursor(
        params.get("before_event_seq"),
        params.get("after_event_seq"),
    )?;
    let limit = params.limit()?;

    let history_query = HistoryQuery {
        room,
        direction,
        limit,
        contains: params.take("q"),
    };
    let slice = state
        .store
        .run(move |conn| message_queries::history(conn, &history_query))
        .await?;

    let last_seq = slice.items.last().map(|message| message.event_seq);
    let next = match direction {
        HistoryDirection::Before(_) => HistoryCursor::Before {
            next_before_event_seq: last_seq,
        },
        HistoryDirection::After(_) => HistoryCursor::After {
            next_after_event_seq: last_seq,
        },
    };
    Ok(Json(HistoryPage {
        items: slice.items,
        next,
        has_more: slice.has_more,
    }))
}

/// `GET /v1/messages/{message_id}`: a message outside the caller's rooms is
/// indistinguishable from a missing one (404 `message_not_found`).
pub async fn get_one(
    principal: Principal,
    State(state): State<AppState>,
    Path(raw_message_id): Path<String>,
) -> Result<Json<Message>, ApiError> {
    let message_id: MessageId = raw_message_id.parse().map_err(|_| message_not_found())?;
    let viewer = viewer_of(principal);
    let message = state
        .store
        .run(move |conn| {
            let Some(message) = message_queries::find_message(conn, &message_id)? else {
                return Ok(None);
            };
            let access = room_queries::read_access(conn, &viewer, &message.room_id)?;
            Ok((access == ReadAccess::Allowed).then_some(message))
        })
        .await?
        .ok_or_else(message_not_found)?;
    Ok(Json(message))
}

fn viewer_of(principal: Principal) -> Viewer {
    match principal {
        Principal::Owner => Viewer::Owner,
        Principal::Agent(agent_id) => Viewer::Agent(agent_id),
    }
}

fn unknown_room() -> ApiError {
    ApiError::not_found(codes::UNKNOWN_ROOM, "no such room")
}

fn message_not_found() -> ApiError {
    ApiError::not_found(codes::MESSAGE_NOT_FOUND, "no such message")
}

fn parse_room(raw: Option<&str>) -> Result<RoomId, ApiError> {
    let Some(raw) = raw else {
        return Err(ApiError::invalid_field("room_id", "room_id is required"));
    };
    raw.parse().map_err(|err: platform_wire::RoomIdError| {
        ApiError::invalid_field("room_id", err.to_string())
    })
}

fn parse_cursor(before: Option<&str>, after: Option<&str>) -> Result<HistoryDirection, ApiError> {
    match (before, after) {
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            codes::MUTUALLY_EXCLUSIVE_CURSORS,
            "before_event_seq and after_event_seq cannot be combined",
        )),
        (None, Some(after)) => Ok(HistoryDirection::After(parse_seq(
            "after_event_seq",
            after,
        )?)),
        (Some(before), None) => Ok(HistoryDirection::Before(Some(parse_seq(
            "before_event_seq",
            before,
        )?))),
        (None, None) => Ok(HistoryDirection::Before(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn cursors_are_exclusive_and_validated() {
        let both = parse_cursor(Some("1"), Some("2")).unwrap_err();
        assert_eq!(both.status(), StatusCode::BAD_REQUEST);
        assert_eq!(both.code(), codes::MUTUALLY_EXCLUSIVE_CURSORS);
        assert!(parse_cursor(Some("007"), None).is_err());
        assert_eq!(
            parse_cursor(None, None).unwrap(),
            HistoryDirection::Before(None)
        );
    }
}
