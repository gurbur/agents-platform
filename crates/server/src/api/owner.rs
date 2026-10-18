//! `GET /v1/owner` (`IF-API-OWNER`): the `[owner] post_as` allowlist, so
//! the console knows which agent it speaks as.

use axum::extract::{RawQuery, State};
use axum::routing::get;
use axum::{Json, Router};
use platform_wire::OwnerInfo;

use super::agents::OwnerOnly;
use super::query::QueryParams;
use super::{ApiError, AppState};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/owner", get(owner_info))
}

/// Owner only; no query parameters. The list is the configuration's, in
/// order, whether or not those agents are registered.
async fn owner_info(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<OwnerInfo>, ApiError> {
    QueryParams::parse(query.as_deref(), &[])?;
    Ok(Json(OwnerInfo {
        post_as: state.post_as.to_vec(),
    }))
}
