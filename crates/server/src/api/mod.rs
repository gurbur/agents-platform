//! HTTP router. Handlers validate input and delegate to the store; they never
//! contain SQL.

mod agents;
mod avatar;
mod delivery;
mod error;
mod history;
mod identity;
mod inbox;
mod json;
mod messages;
mod owner;
mod presence;
mod query;
mod reactions;
mod receiver_url;
mod request_id;
mod rooms;
mod threads;
mod ui;

use std::sync::Arc;

use axum::extract::{FromRef, State};
use axum::routing::get;
use axum::{Json, Router, middleware};
use platform_wire::{AgentId, HealthStatus, codes};
use tracing::warn;

use crate::auth::Authenticator;
use crate::clock::Clock;
use crate::config::{OwnerToken, PresenceConfig};
use crate::delivery::DeliverySignal;
use crate::identity::Issuer;
use crate::presence::PresenceBoard;
use crate::store::{Store, migrations};

pub use error::ApiError;
pub use json::{MAX_WIRE_BODY_BYTES, StrictJson};
pub use receiver_url::MAX_RECEIVER_URL_LEN;
pub use request_id::{RequestId, X_REQUEST_ID};

#[derive(Clone)]
pub struct AppState {
    store: Store,
    authenticator: Authenticator,
    delivery: DeliverySignal,
    /// `None` when identity attestation is disabled.
    identity: Option<Arc<Issuer>>,
    /// `[owner] post_as`: the agents the owner may post as (`IF-CFG-001`).
    post_as: Arc<[AgentId]>,
    /// Declared presence and the `last_seen_at` throttle (memory only).
    presence: PresenceBoard,
    /// `[presence]` TTL bounds (`IF-CFG-001`).
    presence_config: PresenceConfig,
    /// The time source of presence.
    clock: Clock,
}

impl AppState {
    pub fn new(
        store: Store,
        owner_token: OwnerToken,
        delivery: DeliverySignal,
        identity: Option<Issuer>,
        post_as: Vec<AgentId>,
        presence_config: PresenceConfig,
        clock: Clock,
    ) -> Self {
        let presence = PresenceBoard::new();
        let authenticator = Authenticator::new(
            owner_token,
            store.clone(),
            presence.clone(),
            clock.clone(),
            presence_config.last_seen_interval,
        );
        Self {
            store,
            authenticator,
            delivery,
            identity: identity.map(Arc::new),
            post_as: post_as.into(),
            presence,
            presence_config,
            clock,
        }
    }
}

impl FromRef<AppState> for Authenticator {
    fn from_ref(state: &AppState) -> Self {
        state.authenticator.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .merge(agents::routes())
        .merge(avatar::routes())
        .merge(presence::routes())
        .merge(delivery::routes())
        .merge(messages::routes())
        .merge(inbox::routes())
        .merge(rooms::routes())
        .merge(threads::routes())
        .merge(reactions::routes())
        .merge(owner::routes())
        .merge(identity::routes())
        .merge(ui::routes())
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .with_state(state)
        .layer(json::body_limit())
        // Outermost layer so every response, including 404s, carries the id.
        .layer(middleware::from_fn(request_id::assign))
}

/// `GET /healthz` (`IF-OPS-001`): a round trip through the DB thread that
/// confirms the latest migration is applied.
async fn healthz(State(state): State<AppState>) -> Result<Json<HealthStatus>, ApiError> {
    let version = state
        .store
        .run(|conn| migrations::current_version(conn))
        .await
        .map_err(|err| {
            warn!(error = %err, "health check could not reach the database");
            ApiError::unavailable("database is not available")
        })?;
    if version != migrations::LATEST_SCHEMA_VERSION {
        return Err(ApiError::unavailable("database schema is not current"));
    }
    Ok(Json(HealthStatus::ok(version)))
}

async fn not_found() -> ApiError {
    ApiError::not_found(codes::NOT_FOUND, "no such path")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::METHOD_NOT_ALLOWED,
        codes::METHOD_NOT_ALLOWED,
        "method not allowed for this path",
    )
}
