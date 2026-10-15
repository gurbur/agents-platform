//! `GET /v1/messages` and `GET /v1/messages/{message_id}` (`IF-API-HISTORY`).
//!
//! Query order: parameter names → `room_id` (422) → room permission (403) →
//! cursor exclusivity (400) → cursor and `limit` values (422). Unknown or
//! repeated parameters are rejected rather than silently ignored.

use axum::Json;
use axum::extract::{Path, RawQuery, State};
use platform_wire::messages::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use platform_wire::{HistoryCursor, HistoryPage, Message, MessageId, RoomId, Seq, codes};
use url::form_urlencoded;

use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::store::queries::messages::{self as message_queries, HistoryDirection, HistoryQuery};

/// The recognised query parameters, still as text.
#[derive(Debug, Default)]
struct RawParams {
    room_id: Option<String>,
    before_event_seq: Option<String>,
    after_event_seq: Option<String>,
    limit: Option<String>,
    q: Option<String>,
}

impl RawParams {
    fn parse(query: Option<&str>) -> Result<Self, ApiError> {
        let mut params = RawParams::default();
        for (name, value) in form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
            let slot = match name.as_ref() {
                "room_id" => &mut params.room_id,
                "before_event_seq" => &mut params.before_event_seq,
                "after_event_seq" => &mut params.after_event_seq,
                "limit" => &mut params.limit,
                "q" => &mut params.q,
                other => {
                    return Err(ApiError::unprocessable(
                        codes::UNEXPECTED_FIELD,
                        format!("unexpected query parameter `{other}`"),
                    )
                    .with_detail("field", other));
                }
            };
            if slot.is_some() {
                return Err(ApiError::invalid_field(
                    &name,
                    format!("query parameter `{name}` is repeated"),
                ));
            }
            *slot = Some(value.into_owned());
        }
        Ok(params)
    }
}

/// `GET /v1/messages`: one page of a room, newest first by default.
pub async fn list(
    principal: Principal,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<HistoryPage>, ApiError> {
    let params = RawParams::parse(query.as_deref())?;
    let room = parse_room(params.room_id.as_deref())?;
    if !principal.can_read_room(&room) {
        return Err(ApiError::forbidden());
    }
    let direction = parse_cursor(
        params.before_event_seq.as_deref(),
        params.after_event_seq.as_deref(),
    )?;
    let limit = parse_limit(params.limit.as_deref())?;

    let history_query = HistoryQuery {
        room,
        direction,
        limit,
        contains: params.q,
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
    let message = state
        .store
        .run(move |conn| message_queries::find_message(conn, &message_id))
        .await?
        .ok_or_else(message_not_found)?;
    if !principal.can_read_room(&message.room_id) {
        return Err(message_not_found());
    }
    Ok(Json(message))
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

fn parse_seq(field: &str, raw: &str) -> Result<Seq, ApiError> {
    raw.parse()
        .map_err(|err: platform_wire::SeqError| ApiError::invalid_field(field, err.to_string()))
}

fn parse_limit(raw: Option<&str>) -> Result<u32, ApiError> {
    let Some(raw) = raw else {
        return Ok(DEFAULT_PAGE_SIZE);
    };
    let invalid = || {
        ApiError::invalid_field(
            "limit",
            format!("limit must be an integer in 1..={MAX_PAGE_SIZE}"),
        )
    };
    if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let limit: u32 = raw.parse().map_err(|_| invalid())?;
    if !(1..=MAX_PAGE_SIZE).contains(&limit) {
        return Err(invalid());
    }
    Ok(limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn limit_defaults_and_bounds() {
        assert_eq!(parse_limit(None).unwrap(), DEFAULT_PAGE_SIZE);
        assert_eq!(parse_limit(Some("1")).unwrap(), 1);
        assert_eq!(
            parse_limit(Some(&MAX_PAGE_SIZE.to_string())).unwrap(),
            MAX_PAGE_SIZE
        );
        let over = (MAX_PAGE_SIZE + 1).to_string();
        for invalid in [
            "0",
            over.as_str(),
            "",
            "-1",
            "+5",
            "1.5",
            "ten",
            "99999999999",
        ] {
            let err = parse_limit(Some(invalid)).unwrap_err();
            assert_eq!(err.code(), codes::INVALID_FIELD, "{invalid:?}");
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

    #[test]
    fn unknown_and_repeated_parameters_are_rejected() {
        let unknown = RawParams::parse(Some("room_id=all&offset=3")).unwrap_err();
        assert_eq!(unknown.code(), codes::UNEXPECTED_FIELD);
        let repeated = RawParams::parse(Some("room_id=all&room_id=all")).unwrap_err();
        assert_eq!(repeated.code(), codes::INVALID_FIELD);
        let decoded = RawParams::parse(Some("room_id=all&q=a%26b+c")).unwrap();
        assert_eq!(decoded.q.as_deref(), Some("a&b c"));
    }
}
