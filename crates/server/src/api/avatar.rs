//! Directory avatars (`IF-API-AVATAR`): `PUT`, `GET` and `DELETE
//! /v1/agents/{agent_id}/avatar`.
//!
//! The only endpoint whose body is not JSON: [`AvatarBody`] replaces
//! `StrictJson`, and the route carries its own body limit
//! ([`AVATAR_MAX_BYTES`]) in place of the router's 128 KiB. Decision order
//! is `IF-WIRE-008`; the bytes never reach a log.

use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, FromRequest, RawQuery, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, ETAG, IF_NONE_MATCH, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::put;
use axum::{Json, Router};
use platform_wire::{
    AVATAR_MAX_BYTES, AgentId, AvatarContentType, AvatarError, AvatarResponse, codes,
    validate_avatar,
};
use tracing::info;

use super::agents::{AgentPath, ManagedAgent};
use super::query::QueryParams;
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::auth::credential::sha256_hex;
use crate::clock;
use crate::store::queries::avatars::{self as avatar_queries, AvatarLookup, NewAvatar};

/// How long a client may reuse a fetched avatar without revalidating.
const CACHE_CONTROL_VALUE: &str = "private, max-age=300";

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/v1/agents/{agent_id}/avatar",
        put(put_avatar)
            // Inner layer: overrides the router-wide JSON limit for PUT only.
            .layer(DefaultBodyLimit::max(AVATAR_MAX_BYTES))
            .get(get_avatar)
            .delete(delete_avatar),
    )
}

/// A request body that is an acceptable avatar: PNG or JPEG by
/// `Content-Type` (415), at most [`AVATAR_MAX_BYTES`] (413) and starting
/// with the declared format's magic (415).
struct AvatarBody {
    content_type: AvatarContentType,
    bytes: Bytes,
}

impl<S: Send + Sync> FromRequest<S> for AvatarBody {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let declared = request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if declared
            .as_deref()
            .and_then(AvatarContentType::from_header)
            .is_none()
        {
            return Err(avatar_error(AvatarError::UnsupportedType));
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|rejection| {
                if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    too_large()
                } else {
                    ApiError::bad_request(codes::INVALID_JSON, "request body could not be read")
                }
            })?;
        let content_type = validate_avatar(declared.as_deref(), &bytes).map_err(avatar_error)?;
        Ok(AvatarBody {
            content_type,
            bytes,
        })
    }
}

fn avatar_error(err: AvatarError) -> ApiError {
    match err {
        AvatarError::TooLarge { .. } => too_large(),
        AvatarError::UnsupportedType | AvatarError::MagicMismatch(_) => ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            codes::UNSUPPORTED_MEDIA_TYPE,
            err.to_string(),
        ),
    }
}

fn too_large() -> ApiError {
    ApiError::payload_too_large(
        codes::AVATAR_TOO_LARGE,
        format!("an avatar is at most {AVATAR_MAX_BYTES} bytes"),
    )
}

fn avatar_not_found() -> ApiError {
    ApiError::not_found(codes::AVATAR_NOT_FOUND, "this agent has no avatar")
}

/// `PUT /v1/agents/{agent_id}/avatar`: owner or the agent itself.
async fn put_avatar(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
    body: AvatarBody,
) -> Result<Json<AvatarResponse>, ApiError> {
    let image = NewAvatar {
        content_type: body.content_type,
        sha256: sha256_hex(&body.bytes),
        bytes: body.bytes.to_vec(),
    };
    let target = agent_id.clone();
    let avatar = state
        .store
        .transaction(move |tx| {
            avatar_queries::put_avatar(tx, &target, &image, &clock::now_rfc3339_ms())
        })
        .await?
        .ok_or_else(ApiError::agent_not_found)?;

    info!(
        agent_id = %agent_id,
        content_type = %avatar.content_type,
        bytes = avatar.bytes,
        sha256 = %avatar.sha256,
        "agent avatar stored"
    );
    Ok(Json(AvatarResponse { agent_id, avatar }))
}

/// `GET /v1/agents/{agent_id}/avatar`: any principal, like the directory.
async fn get_avatar(
    _caller: Principal,
    AgentPath(raw_agent_id): AgentPath,
    RawQuery(query): RawQuery,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let agent_id: AgentId = raw_agent_id
        .parse()
        .map_err(|_| ApiError::agent_not_found())?;
    QueryParams::parse(query.as_deref(), &[])?;

    let lookup = state
        .store
        .run(move |conn| avatar_queries::get_avatar(conn, &agent_id))
        .await?;
    let stored = match lookup {
        AvatarLookup::NoAgent => return Err(ApiError::agent_not_found()),
        AvatarLookup::NoAvatar => return Err(avatar_not_found()),
        AvatarLookup::Found(stored) => stored,
    };

    let etag = entity_tag(&stored.avatar.sha256)?;
    if if_none_match(&headers, &stored.avatar.sha256) {
        let cached = [
            (ETAG, etag),
            (CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE)),
        ];
        return Ok((StatusCode::NOT_MODIFIED, cached).into_response());
    }
    let headers = [
        (
            CONTENT_TYPE,
            HeaderValue::from_static(stored.avatar.content_type.as_str()),
        ),
        (CONTENT_LENGTH, HeaderValue::from(stored.bytes.len())),
        (ETAG, etag),
        (CACHE_CONTROL, HeaderValue::from_static(CACHE_CONTROL_VALUE)),
        (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
    ];
    Ok((headers, Body::from(stored.bytes)).into_response())
}

/// `"<sha256>"`. The hash is hex, so it is always a valid header value.
fn entity_tag(sha256: &str) -> Result<HeaderValue, ApiError> {
    HeaderValue::from_str(&format!("\"{sha256}\"")).map_err(|_| {
        tracing::error!("stored avatar sha256 is not a valid header value");
        ApiError::internal()
    })
}

/// RFC 9110 §13.1.2 with weak comparison: `*`, or any listed entity tag
/// (`"x"` or `W/"x"`) equal to the current one.
fn if_none_match(headers: &HeaderMap, sha256: &str) -> bool {
    let current = format!("\"{sha256}\"");
    headers
        .get_all(IF_NONE_MATCH)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == current)
}

/// `DELETE /v1/agents/{agent_id}/avatar`: owner or the agent itself; the
/// body is not read.
async fn delete_avatar(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let target = agent_id.clone();
    let lookup = state
        .store
        .transaction(move |tx| avatar_queries::delete_avatar(tx, &target))
        .await?;
    let removed = match lookup {
        AvatarLookup::NoAgent => return Err(ApiError::agent_not_found()),
        AvatarLookup::NoAvatar => return Err(avatar_not_found()),
        AvatarLookup::Found(removed) => removed,
    };
    info!(
        agent_id = %agent_id,
        content_type = %removed.content_type,
        bytes = removed.bytes,
        sha256 = %removed.sha256,
        "agent avatar removed"
    );
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_if_none_match(values: &[&'static str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(IF_NONE_MATCH, HeaderValue::from_static(value));
        }
        headers
    }

    #[test]
    fn if_none_match_uses_weak_comparison() {
        for values in [
            &["\"abc\""][..],
            &["W/\"abc\""],
            &["\"x\", \"abc\""],
            &["\"x\"", "\"abc\""],
            &["*"],
        ] {
            assert!(
                if_none_match(&with_if_none_match(values), "abc"),
                "{values:?}"
            );
        }
        for values in [&[][..], &["\"abd\""], &["abc"], &["\"ab\"c"]] {
            assert!(
                !if_none_match(&with_if_none_match(values), "abc"),
                "{values:?}"
            );
        }
    }
}
