//! `GET /v1/messages` and `GET /v1/messages/{message_id}` (`IF-API-HISTORY`).
//!
//! Query order (`IF-WIRE-005` S15): parameter names → `room_id` (422) →
//! `thread_id` format, `top_level` value, both given (422) → room
//! permission (the store's: owner 404 `unknown_room`, agent 403
//! `forbidden` unless it participates in the asked thread) → the thread
//! (404 `thread_not_found`) → cursor exclusivity (400) → cursor and `limit`
//! values (422). Unknown or repeated parameters are rejected rather than
//! silently ignored.

use axum::Json;
use axum::extract::{Path, RawQuery, State};
use platform_wire::{HistoryCursor, HistoryPage, Message, MessageId, RoomId, codes};

use super::query::{QueryParams, parse_seq};
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::store::queries::messages::{
    self as message_queries, HistoryDirection, HistoryQuery, ThreadFilter,
};
use crate::store::queries::reactions as reaction_queries;
use crate::store::queries::rooms::{self as room_queries, ReadAccess, Viewer};
use crate::store::queries::threads as thread_queries;

const PARAMS: &[&str] = &[
    "room_id",
    "thread_id",
    "top_level",
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
    let filter = parse_filter(params.get("thread_id"), params.get("top_level"))?;
    let viewer = viewer_of(principal);
    let (checked_room, checked_thread) = (room.clone(), thread_of(&filter).cloned());
    let access = state
        .store
        .run(move |conn| {
            room_queries::history_access(conn, &viewer, &checked_room, checked_thread.as_ref())
        })
        .await?;
    match access {
        ReadAccess::Allowed => {}
        ReadAccess::UnknownRoom => return Err(unknown_room()),
        ReadAccess::Forbidden => return Err(ApiError::forbidden()),
        ReadAccess::ThreadNotFound => return Err(thread_not_found()),
    }
    let direction = parse_cursor(
        params.get("before_event_seq"),
        params.get("after_event_seq"),
    )?;
    let limit = params.limit()?;

    let history_query = HistoryQuery {
        room,
        filter,
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

/// `GET /v1/messages/{message_id}`: a message outside the caller's rooms
/// and threads is indistinguishable from a missing one (404
/// `message_not_found`). A root carries its thread summary; every message
/// its reactions.
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
            let Some(mut message) = message_queries::find_message(conn, &message_id)? else {
                return Ok(None);
            };
            if !room_queries::can_read_message(conn, &viewer, &message)? {
                return Ok(None);
            }
            thread_queries::attach_summaries(conn, [&mut message])?;
            reaction_queries::attach_summaries(conn, [&mut message])?;
            Ok(Some(message))
        })
        .await?
        .ok_or_else(message_not_found)?;
    Ok(Json(message))
}

pub(super) fn viewer_of(principal: Principal) -> Viewer {
    match principal {
        Principal::Owner => Viewer::Owner,
        Principal::Agent(agent_id) => Viewer::Agent(agent_id),
    }
}

pub(super) fn unknown_room() -> ApiError {
    ApiError::not_found(codes::UNKNOWN_ROOM, "no such room")
}

pub(super) fn thread_not_found() -> ApiError {
    ApiError::not_found(
        codes::THREAD_NOT_FOUND,
        "no such thread, or it is not readable",
    )
}

pub(super) fn message_not_found() -> ApiError {
    ApiError::not_found(codes::MESSAGE_NOT_FOUND, "no such message")
}

pub(super) fn parse_room(raw: Option<&str>) -> Result<RoomId, ApiError> {
    let Some(raw) = raw else {
        return Err(ApiError::invalid_field("room_id", "room_id is required"));
    };
    raw.parse().map_err(|err: platform_wire::RoomIdError| {
        ApiError::invalid_field("room_id", err.to_string())
    })
}

/// `thread_id` (a `msg-` ULID) and `top_level` (`true`·`false`, `false`
/// meaning the default) exclude each other: both given is 422 whatever
/// their values.
fn parse_filter(
    thread_id: Option<&str>,
    top_level: Option<&str>,
) -> Result<ThreadFilter, ApiError> {
    let thread = thread_id
        .map(|raw| raw.parse::<MessageId>())
        .transpose()
        .map_err(|err| ApiError::invalid_field("thread_id", err.to_string()))?;
    let top = match top_level {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => {
            return Err(ApiError::invalid_field(
                "top_level",
                "top_level must be `true` or `false`",
            ));
        }
    };
    match (thread, top_level) {
        (Some(_), Some(_)) => Err(ApiError::invalid_field(
            "top_level",
            "thread_id and top_level cannot be combined",
        )),
        (Some(root), None) => Ok(ThreadFilter::Thread(root)),
        (None, _) if top => Ok(ThreadFilter::TopLevel),
        (None, _) => Ok(ThreadFilter::All),
    }
}

fn thread_of(filter: &ThreadFilter) -> Option<&MessageId> {
    match filter {
        ThreadFilter::Thread(root) => Some(root),
        ThreadFilter::All | ThreadFilter::TopLevel => None,
    }
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
    fn thread_filters_exclude_each_other() {
        let root = "msg-01J00000000000000000000000";
        assert_eq!(parse_filter(None, None).unwrap(), ThreadFilter::All);
        assert_eq!(
            parse_filter(None, Some("false")).unwrap(),
            ThreadFilter::All
        );
        assert_eq!(
            parse_filter(None, Some("true")).unwrap(),
            ThreadFilter::TopLevel
        );
        assert_eq!(
            parse_filter(Some(root), None).unwrap(),
            ThreadFilter::Thread(root.parse().unwrap())
        );
        for (thread, top) in [
            (Some("nope"), None),
            (None, Some("1")),
            (None, Some("")),
            (Some(root), Some("false")),
            (Some(root), Some("true")),
        ] {
            let err = parse_filter(thread, top).unwrap_err();
            assert_eq!(err.code(), codes::INVALID_FIELD, "{thread:?} {top:?}");
        }
    }

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
