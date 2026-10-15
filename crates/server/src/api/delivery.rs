//! `GET /v1/agents/{agent_id}/delivery` and
//! `POST /v1/agents/{agent_id}/delivery/retry` (`IF-API-DELIVERY`).
//!
//! Both are for the owner or the agent itself: another agent is 403 and an
//! unknown agent is 404 `agent_not_found` (`IF-WIRE-003`, `IF-WIRE-004`).
//! The retry body is not read, whatever its `Content-Type`.

use std::time::SystemTime;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use platform_wire::{DeliveryRetryResponse, DeliveryStatus};
use tracing::info;

use super::agents::ManagedAgent;
use super::{ApiError, AppState};
use crate::store::queries::delivery as delivery_queries;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/agents/{agent_id}/delivery", get(status))
        .route("/v1/agents/{agent_id}/delivery/retry", post(retry))
}

async fn status(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
) -> Result<Json<DeliveryStatus>, ApiError> {
    let status = state
        .store
        .run(move |conn| delivery_queries::status(conn, &agent_id))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;
    Ok(Json(status))
}

/// Releases a blocked route and schedules it now; a pending one keeps its
/// due time and only coalesces into the head (`IF-DLV-001`).
async fn retry(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
) -> Result<Json<DeliveryRetryResponse>, ApiError> {
    let target = agent_id.clone();
    let response = state
        .store
        .transaction(move |tx| delivery_queries::retry(tx, &target, SystemTime::now()))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;
    state.delivery.notify();
    info!(agent_id = %agent_id, state = ?response.state, "manual delivery retry requested");
    Ok(Json(response))
}
