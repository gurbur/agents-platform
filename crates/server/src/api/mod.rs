//! HTTP router. Handlers validate input and delegate to the store; they never
//! contain SQL.

mod error;
mod request_id;

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router, middleware};
use platform_wire::HealthStatus;
use tracing::warn;

use crate::store::{Store, migrations};

pub use error::ApiError;
pub use request_id::{RequestId, X_REQUEST_ID};

#[derive(Clone)]
pub struct AppState {
    store: Store,
}

impl AppState {
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .with_state(state)
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
