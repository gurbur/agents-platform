//! Agent presence: `PUT` and `DELETE /v1/agents/{agent_id}/presence`
//! (`IF-API-PRESENCE`, `IF-WIRE-013`, ADR-0012).
//!
//! The declaration lives in the [`crate::presence::PresenceBoard`] only;
//! the one database read is whether the agent is registered (and, for the
//! PUT response, its stored `last_seen_at`). The note is free text other
//! agents read and is never logged.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::put;
use axum::{Json, Router};
use platform_wire::{PresenceRequest, PresenceResponse, codes};
use tracing::info;

use super::agents::ManagedAgent;
use super::json::StrictJson;
use super::{ApiError, AppState};
use crate::store::queries::agents as agent_queries;

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/agents/{agent_id}/presence",
        put(declare).delete(withdraw),
    )
}

/// `PUT /v1/agents/{agent_id}/presence`: the owner or the agent itself.
/// Order is `IF-WIRE-013`: the extractors give 401, 403 (another agent's
/// path) and 404 (not a slug), `StrictJson` the body format, then the value
/// rules, the configured maximum and finally whether the agent exists.
async fn declare(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<PresenceRequest>,
) -> Result<Json<PresenceResponse>, ApiError> {
    let declaration = request
        .validate()
        .map_err(|err| ApiError::invalid_field(err.field(), err.to_string()))?;
    let limits = state.presence_config;
    let ttl = declaration
        .ttl_secs
        .map_or(limits.default_ttl, Duration::from_secs);
    if ttl > limits.max_ttl {
        return Err(ApiError::unprocessable(
            codes::TTL_TOO_LONG,
            format!("ttl_secs must be at most {}", limits.max_ttl.as_secs()),
        )
        .with_detail("field", "ttl_secs"));
    }

    let target = agent_id.clone();
    let last_seen_at = state
        .store
        .run(move |conn| agent_queries::last_seen_at(conn, &target))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;

    let presence = state.presence.set(
        &agent_id,
        declaration.state,
        declaration.note,
        ttl,
        state.clock.now(),
    );
    info!(agent_id = %agent_id, state = %presence.state, "presence set");
    Ok(Json(PresenceResponse {
        agent_id,
        presence,
        last_seen_at,
    }))
}

/// `DELETE /v1/agents/{agent_id}/presence`: the owner or the agent itself;
/// body and query unread. 204 whether or not there was a declaration.
async fn withdraw(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let target = agent_id.clone();
    let registered = state
        .store
        .run(move |conn| agent_queries::agent_exists(conn, &target))
        .await?;
    if !registered {
        return Err(ApiError::agent_not_found());
    }
    state.presence.clear(&agent_id);
    info!(agent_id = %agent_id, "presence cleared");
    Ok(StatusCode::NO_CONTENT)
}
