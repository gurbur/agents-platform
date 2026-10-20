//! Rooms (`IF-API-ROOMS`, `IF-WIRE-011`): `GET /v1/rooms` for both
//! principals, and the owner's topic room management — `POST /v1/rooms`,
//! `PUT·DELETE /v1/rooms/{room_id}/members/{agent_id}` and
//! `POST /v1/rooms/{room_id}/archive`. Owner posting is `POST /v1/messages`
//! (S13); `POST /v1/rooms/{room_id}/messages` no longer exists.
//!
//! Order: authentication → owner only (a parts extractor, before any body)
//! → path and body format (422) → the store's room, archive and agent
//! checks. Logs carry the room, the agent and the action, never a title.

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use platform_wire::{
    AgentId, CreateRoomRequest, ROOMS_SOFT_CAP, RoomId, RoomKind, RoomList, RoomSummary, codes,
    validate_room_title,
};
use tracing::info;

use super::agents::OwnerOnly;
use super::json::StrictJson;
use super::query::QueryParams;
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::clock;
use crate::store::queries::rooms::{self as room_queries, RoomRejection, Viewer};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/rooms", get(list).post(create))
        .route(
            "/v1/rooms/{room_id}/members/{agent_id}",
            put(invite).delete(remove),
        )
        .route("/v1/rooms/{room_id}/archive", post(archive))
}

/// `GET /v1/rooms`: every room for the owner, the rooms an agent has a
/// membership row of (a left `all` included) for an agent.
async fn list(
    principal: Principal,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<RoomList>, ApiError> {
    QueryParams::parse(query.as_deref(), &[])?;
    let viewer = match principal {
        Principal::Owner => Viewer::Owner,
        Principal::Agent(agent_id) => Viewer::Agent(agent_id),
    };
    let rooms = state
        .store
        .run(move |conn| room_queries::list(conn, &viewer, ROOMS_SOFT_CAP))
        .await?;
    Ok(Json(RoomList { rooms }))
}

/// `POST /v1/rooms`: a topic room with no members yet.
async fn create(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<CreateRoomRequest>,
) -> Result<(StatusCode, Json<RoomSummary>), ApiError> {
    let room = parse_topic(&request.room_id)?;
    let title = request
        .title
        .as_deref()
        .map(validate_room_title)
        .transpose()
        .map_err(|err| ApiError::invalid_field("title", err.to_string()))?;

    let target = room.clone();
    let created = state
        .store
        .transaction(move |tx| {
            room_queries::create_topic(tx, &target, title.as_deref(), &clock::now_rfc3339_ms())
        })
        .await?
        .map_err(rejection_error)?;
    info!(room_id = %room, "topic room created");
    Ok((StatusCode::CREATED, Json(created)))
}

/// `PUT /v1/rooms/{room_id}/members/{agent_id}`: body and query unread.
async fn invite(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    Path((raw_room_id, raw_agent_id)): Path<(String, String)>,
) -> Result<Json<RoomSummary>, ApiError> {
    let (room, agent) = parse_member_path(&raw_room_id, &raw_agent_id)?;
    let (target_room, target_agent) = (room.clone(), agent.clone());
    let summary = state
        .store
        .transaction(move |tx| room_queries::invite(tx, &target_room, &target_agent))
        .await?
        .map_err(rejection_error)?;
    info!(room_id = %room, agent_id = %agent, "room member invited");
    Ok(Json(summary))
}

/// `DELETE /v1/rooms/{room_id}/members/{agent_id}`: 204 whether or not the
/// agent was a member.
async fn remove(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    Path((raw_room_id, raw_agent_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (room, agent) = parse_member_path(&raw_room_id, &raw_agent_id)?;
    let (target_room, target_agent) = (room.clone(), agent.clone());
    state
        .store
        .transaction(move |tx| room_queries::remove(tx, &target_room, &target_agent))
        .await?
        .map_err(rejection_error)?;
    info!(room_id = %room, agent_id = %agent, "room member removed");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/rooms/{room_id}/archive`: body and query unread; a second
/// call keeps the first `archived_at`.
async fn archive(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    Path(raw_room_id): Path<String>,
) -> Result<Json<RoomSummary>, ApiError> {
    let room = parse_topic(&raw_room_id)?;
    let target = room.clone();
    let summary = state
        .store
        .transaction(move |tx| room_queries::archive(tx, &target, &clock::now_rfc3339_ms()))
        .await?
        .map_err(rejection_error)?;
    info!(room_id = %room, "topic room archived");
    Ok(Json(summary))
}

/// 422 `invalid_field` (`room_id`) unless `raw` is a `room:<slug>` id.
fn parse_topic(raw: &str) -> Result<RoomId, ApiError> {
    let room: RoomId = raw.parse().map_err(|err: platform_wire::RoomIdError| {
        ApiError::invalid_field("room_id", err.to_string())
    })?;
    if room.kind() != RoomKind::Topic {
        return Err(ApiError::invalid_field(
            "room_id",
            "only topic rooms (`room:<slug>`) are managed here",
        ));
    }
    Ok(room)
}

/// The room format first (422), then the agent path (404 for a non-slug).
fn parse_member_path(raw_room_id: &str, raw_agent_id: &str) -> Result<(RoomId, AgentId), ApiError> {
    let room = parse_topic(raw_room_id)?;
    let agent = raw_agent_id
        .parse()
        .map_err(|_| ApiError::agent_not_found())?;
    Ok((room, agent))
}

fn rejection_error(rejection: RoomRejection) -> ApiError {
    match rejection {
        RoomRejection::UnknownRoom => ApiError::not_found(codes::UNKNOWN_ROOM, "no such room"),
        RoomRejection::NotTopic => ApiError::invalid_field(
            "room_id",
            "only topic rooms (`room:<slug>`) are managed here",
        ),
        RoomRejection::RoomArchived => ApiError::conflict(
            codes::ROOM_ARCHIVED,
            "the room is archived and only readable",
        ),
        RoomRejection::UnknownAgent => ApiError::agent_not_found(),
        RoomRejection::RoomIdTaken => {
            ApiError::conflict(codes::ROOM_ID_TAKEN, "a room with this room_id exists")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_topic_ids_are_managed() {
        assert!(parse_topic("room:launch").is_ok());
        for raw in ["all", "dm:agent-a:agent-b", "room:Launch", "launch"] {
            let err = parse_topic(raw).unwrap_err();
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY, "{raw}");
            assert!(err.names_field("room_id"));
        }
    }
}
