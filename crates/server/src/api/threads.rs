//! Threads (`IF-API-THREADS`, `IF-WIRE-014`): `GET /v1/threads/{root_id}`
//! and `PUT /v1/threads/{root_id}/participation`.
//!
//! Order: authentication → the path's format (404 `thread_not_found`, so
//! a malformed id looks like a missing thread) → query or body → the
//! store's thread and participant checks. A thread the caller cannot read
//! is indistinguishable from a missing one. Logs carry ids and `muted`.

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use platform_wire::{
    AgentId, MessageId, ParticipationRequest, ParticipationResponse, ThreadView, codes,
};
use tracing::info;

use super::history::{thread_not_found, viewer_of};
use super::json::StrictJson;
use super::query::QueryParams;
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::store::queries::rooms as room_queries;
use crate::store::queries::threads as thread_queries;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/threads/{root_id}", get(show))
        .route("/v1/threads/{root_id}/participation", put(participate))
}

/// `GET /v1/threads/{root_id}`: the participants and counts of an open
/// thread the caller reads (its room, or as a participant; the owner
/// always).
async fn show(
    principal: Principal,
    State(state): State<AppState>,
    Path(raw_root_id): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<ThreadView>, ApiError> {
    let root = parse_root(&raw_root_id)?;
    QueryParams::parse(query.as_deref(), &[])?;
    let viewer = viewer_of(principal);
    let view = state
        .store
        .run(move |conn| {
            let Some(room) = thread_queries::open_root_room(conn, &root)? else {
                return Ok(None);
            };
            if !room_queries::can_read_thread(conn, &viewer, &room, &root)? {
                return Ok(None);
            }
            thread_queries::view(conn, &root)
        })
        .await?
        .ok_or_else(thread_not_found)?;
    Ok(Json(view))
}

/// `PUT /v1/threads/{root_id}/participation`: a participant turns the
/// thread's wakes off (`wake: false`) or on. The owner names one of its
/// `post_as` identities. Repeating the current value writes nothing.
async fn participate(
    principal: Principal,
    State(state): State<AppState>,
    Path(raw_root_id): Path<String>,
    request: Result<StrictJson<ParticipationRequest>, ApiError>,
) -> Result<Json<ParticipationResponse>, ApiError> {
    let root = parse_root(&raw_root_id)?;
    let request = match request {
        Ok(StrictJson(request)) => request,
        Err(err) if principal != Principal::Owner && err.names_field("agent_id") => {
            return Err(unexpected_agent_id());
        }
        Err(err) => return Err(err),
    };
    let agent = participant_of(&principal, &state.post_as, request.agent_id)?;
    let muted = !request.wake;

    let (target_root, target_agent) = (root.clone(), agent.clone());
    let participant = state
        .store
        .transaction(move |tx| thread_queries::set_muted(tx, &target_root, &target_agent, muted))
        .await?
        .ok_or_else(thread_not_found)?;
    info!(thread_id = %root, agent_id = %agent, muted, "thread participation");
    Ok(Json(ParticipationResponse {
        thread_id: root,
        participant,
    }))
}

/// 404 `thread_not_found` unless the path is a `msg-` ULID.
fn parse_root(raw: &str) -> Result<MessageId, ApiError> {
    raw.parse().map_err(|_| thread_not_found())
}

/// Whose participation changes: an agent's own (it may not name one), or
/// the owner's allowlisted `agent_id`, compared verbatim (403 otherwise).
fn participant_of(
    principal: &Principal,
    post_as: &[AgentId],
    agent_id: Option<String>,
) -> Result<AgentId, ApiError> {
    match (principal, agent_id) {
        (Principal::Agent(_), Some(_)) => Err(unexpected_agent_id()),
        (Principal::Agent(own), None) => Ok(own.clone()),
        (Principal::Owner, None) => Err(ApiError::invalid_field(
            "agent_id",
            "the owner names an agent_id from [owner] post_as",
        )),
        (Principal::Owner, Some(named)) => post_as
            .iter()
            .find(|allowed| allowed.as_str() == named)
            .cloned()
            .ok_or_else(|| ApiError::forbidden().with_detail("field", "agent_id")),
    }
}

fn unexpected_agent_id() -> ApiError {
    ApiError::unprocessable(
        codes::UNEXPECTED_FIELD,
        "unexpected field `agent_id`: an agent changes its own participation",
    )
    .with_detail("field", "agent_id")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    #[test]
    fn the_participant_is_the_agent_or_an_allowlisted_identity() {
        let post_as: Vec<AgentId> = vec!["human".parse().unwrap()];
        let agent = Principal::Agent("agent-a".parse().unwrap());
        assert_eq!(
            participant_of(&agent, &post_as, None).unwrap().as_str(),
            "agent-a"
        );
        let named = participant_of(&agent, &post_as, Some("agent-a".into())).unwrap_err();
        assert_eq!(named.code(), codes::UNEXPECTED_FIELD);
        let missing = participant_of(&Principal::Owner, &post_as, None).unwrap_err();
        assert_eq!(missing.code(), codes::INVALID_FIELD);
        assert!(missing.names_field("agent_id"));
        assert_eq!(
            participant_of(&Principal::Owner, &post_as, Some("human".into()))
                .unwrap()
                .as_str(),
            "human"
        );
        for refused in ["Human", "agent-a", ""] {
            let err =
                participant_of(&Principal::Owner, &post_as, Some(refused.into())).unwrap_err();
            assert_eq!(err.status(), StatusCode::FORBIDDEN, "{refused:?}");
        }
    }

    #[test]
    fn a_malformed_root_is_a_missing_thread() {
        let err = parse_root("nope").unwrap_err();
        assert_eq!(err.status(), StatusCode::NOT_FOUND);
        assert_eq!(err.code(), codes::THREAD_NOT_FOUND);
    }
}
