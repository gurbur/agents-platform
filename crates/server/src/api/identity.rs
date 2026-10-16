//! `POST /v1/identity/tokens` (IDENTITY_ATTESTATION §4): an agent receives a
//! short-lived Ed25519-signed JWT whose `sub` is its own agent_id.
//!
//! Order: authentication (401) → owner refused (403 `owner_not_allowed`) →
//! identity configured (503 `identity_disabled`) → `StrictJson` (415/413/400/
//! 422) → `aud`/`ttl_secs` format (422 `invalid_field`) → `aud` allowed (422
//! `unknown_audience`) → `ttl_secs` within the maximum (422 `ttl_too_long`)
//! → 201. The first three run as parts extractors, before the body is read.
//! No database command beyond the authentication lookup.

use std::sync::Arc;
use std::time::SystemTime;

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::routing::post;
use axum::{Json, Router};
use platform_wire::{AgentId, IdentityTokenRequest, IdentityTokenResponse, codes};
use tracing::info;
use ulid::Ulid;

use super::json::StrictJson;
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::identity::{IssueError, Issuer};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/identity/tokens", post(issue_token))
}

/// Extractor: the calling agent. The owner has no agent identity to attest
/// (403 `owner_not_allowed`).
struct AttestedAgent(AgentId);

impl FromRequestParts<AppState> for AttestedAgent {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        match Principal::from_request_parts(parts, state).await? {
            Principal::Agent(agent_id) => Ok(AttestedAgent(agent_id)),
            Principal::Owner => Err(ApiError::new(
                StatusCode::FORBIDDEN,
                codes::OWNER_NOT_ALLOWED,
                "the owner cannot obtain an identity token; use an agent API key",
            )),
        }
    }
}

/// Extractor: the configured issuer; 503 `identity_disabled` without an
/// `[identity]` table. Runs after authentication so unauthenticated callers
/// learn nothing about the configuration.
struct EnabledIssuer(Arc<Issuer>);

impl FromRequestParts<AppState> for EnabledIssuer {
    type Rejection = ApiError;

    async fn from_request_parts(_parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        state.identity.clone().map(EnabledIssuer).ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                codes::IDENTITY_DISABLED,
                "identity attestation is not configured on this hub",
            )
        })
    }
}

async fn issue_token(
    AttestedAgent(agent_id): AttestedAgent,
    EnabledIssuer(issuer): EnabledIssuer,
    StrictJson(request): StrictJson<IdentityTokenRequest>,
) -> Result<(StatusCode, Json<IdentityTokenResponse>), ApiError> {
    request
        .validate()
        .map_err(|err| ApiError::invalid_field(err.field(), err.to_string()))?;

    let issued = issuer
        .issue(
            &agent_id,
            &request.aud,
            request.ttl_secs,
            SystemTime::now(),
            Ulid::generate(),
        )
        .map_err(issue_error)?;

    let claims = &issued.claims;
    // The token itself is never logged (IDENTITY_ATTESTATION §11).
    info!(
        sub = %claims.sub,
        jti = %claims.jti,
        aud = %claims.aud,
        exp = claims.exp,
        "identity token issued"
    );
    let body = IdentityTokenResponse {
        token: issued.token,
        sub: agent_id,
        aud: request.aud,
    };
    Ok((StatusCode::CREATED, Json(body)))
}

fn issue_error(err: IssueError) -> ApiError {
    match err {
        IssueError::UnknownAudience => {
            ApiError::unprocessable(codes::UNKNOWN_AUDIENCE, err.to_string())
                .with_detail("field", "aud")
        }
        IssueError::TtlTooLong { max_ttl_secs } => {
            ApiError::unprocessable(codes::TTL_TOO_LONG, err.to_string())
                .with_detail("field", "ttl_secs")
                .with_detail("max_ttl_secs", max_ttl_secs)
        }
    }
}
