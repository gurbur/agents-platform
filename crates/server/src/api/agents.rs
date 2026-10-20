//! Agent registration, directory and deletion, directory profile, receiver
//! route, `all` membership and token rotation (`IF-API-AGENTS`,
//! `IF-API-PROFILE`, `IF-API-RECEIVER`, `IF-API-MEMBERSHIP`, `IF-API-ROTATE`).
//!
//! Authentication and permission run in `FromRequestParts` extractors, so a
//! caller without permission gets 401/403 before the body is examined.

use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, patch, post, put};
use axum::{Json, Router};
use platform_wire::{
    AgentEntry, AgentId, AgentList, DeliverySummary, MembershipRequest, MembershipResponse,
    OwnerAgentEntry, ProfileFieldError, ProfileResponse, ReceiverResponse, RegisterAgentRequest,
    RegisterAgentResponse, RotateTokensResponse, Seq, UpdateProfileRequest, UpdateReceiverRequest,
    WakePolicy, codes, validate_name,
};
use tracing::info;

use super::json::StrictJson;
use super::{ApiError, AppState, receiver_url};
use crate::auth::Principal;
use crate::auth::credential::IssuedCredentials;
use crate::clock;
use crate::store::StoreError;
use crate::store::queries::agents::{self as agent_queries, AgentRecord, NewAgent, Registration};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/agents", post(register).get(list))
        .route("/v1/agents/{agent_id}", delete(delete_agent))
        .route("/v1/agents/{agent_id}/profile", put(update_profile))
        .route("/v1/agents/{agent_id}/receiver", patch(update_receiver))
        .route(
            "/v1/agents/{agent_id}/membership/all",
            put(update_all_membership),
        )
        .route("/v1/agents/{agent_id}/tokens/rotate", post(rotate_tokens))
}

/// Extractor: the caller is the owner (else 401/403).
pub(super) struct OwnerOnly;

impl FromRequestParts<AppState> for OwnerOnly {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        Principal::from_request_parts(parts, state)
            .await?
            .require_owner()?;
        Ok(OwnerOnly)
    }
}

/// Extractor: the `{agent_id}` path target, which the caller (the owner or
/// that agent) may manage. An unparsable slug can never name an agent, so
/// for the owner it is 404 `agent_not_found`; an agent already got 403.
pub(super) struct ManagedAgent(pub(super) AgentId);

impl FromRequestParts<AppState> for ManagedAgent {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let ManagedBy { agent_id, .. } = ManagedBy::from_request_parts(parts, state).await?;
        Ok(ManagedAgent(agent_id))
    }
}

/// Extractor: like [`ManagedAgent`], keeping the caller for handlers whose
/// body permissions depend on it (the profile `name`).
struct ManagedBy {
    principal: Principal,
    agent_id: AgentId,
}

impl FromRequestParts<AppState> for ManagedBy {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let principal = Principal::from_request_parts(parts, state).await?;
        let AgentPath(raw) = AgentPath::from_request_parts(parts, state).await?;
        principal.require_owner_or_self(&raw)?;
        let agent_id = raw.parse().map_err(|_| ApiError::agent_not_found())?;
        Ok(ManagedBy {
            principal,
            agent_id,
        })
    }
}

/// Extractor: the raw `{agent_id}` path segment, before slug validation.
pub(super) struct AgentPath(pub(super) String);

impl FromRequestParts<AppState> for AgentPath {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let Path(raw) = Path::<String>::from_request_parts(parts, state)
            .await
            .map_err(|_| ApiError::not_found(codes::NOT_FOUND, "no such path"))?;
        Ok(AgentPath(raw))
    }
}

/// `POST /v1/agents`: owner only. 201 with both one-time credentials.
async fn register(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<RegisterAgentRequest>,
) -> Result<(StatusCode, Json<RegisterAgentResponse>), ApiError> {
    let agent_id = request.agent_id.parse::<AgentId>().map_err(|err| {
        ApiError::unprocessable(codes::INVALID_AGENT_ID, err.to_string())
            .with_detail("field", "agent_id")
    })?;
    let name = validate_name(&request.name).map_err(profile_field_error)?;
    let receiver_url = request
        .receiver_url
        .as_deref()
        .map(receiver_url::normalize)
        .transpose()?;

    let credentials = IssuedCredentials::generate();
    let new_agent = NewAgent {
        agent_id,
        name,
        receiver_url,
        api_key_hash: credentials.api_key_hash.clone(),
        receiver_secret: credentials.receiver_secret.clone(),
        created_at: clock::now_rfc3339_ms(),
    };
    let registration = state
        .store
        .transaction(move |tx| agent_queries::register(tx, &new_agent))
        .await?;
    let Registration::Created(agent) = registration else {
        return Err(ApiError::conflict(
            codes::AGENT_ID_TAKEN,
            "agent_id is already registered",
        ));
    };

    info!(agent_id = %agent.agent_id, "agent registered");
    let body = RegisterAgentResponse {
        agent_id: agent.agent_id,
        name: Some(agent.name),
        joined_all: agent.joined_all,
        receiver_url: agent.receiver_url,
        route_version: agent.route_version,
        profile: agent.profile,
        agent_api_key: credentials.agent_api_key,
        receiver_secret: credentials.receiver_secret,
    };
    Ok((StatusCode::CREATED, Json(body)))
}

/// `DELETE /v1/agents/{agent_id}`: owner only, body and query unread.
/// Order is `IF-WIRE-009`: 401/403 from the extractor, 404 for a path that
/// is not a slug, 409 for an id in `[owner] post_as`, then the store's 404
/// or the cascade (ADR-0009: the agent's state goes, its messages stay).
async fn delete_agent(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    AgentPath(raw_agent_id): AgentPath,
) -> Result<StatusCode, ApiError> {
    let agent_id: AgentId = raw_agent_id
        .parse()
        .map_err(|_| ApiError::agent_not_found())?;
    if state.post_as.contains(&agent_id) {
        return Err(ApiError::conflict(
            codes::AGENT_PROTECTED,
            "this agent_id is in [owner] post_as and cannot be deleted; remove it from the configuration first",
        ));
    }

    let target = agent_id.clone();
    let deleted = state
        .store
        .transaction(move |tx| agent_queries::delete(tx, &target))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;

    info!(agent_id = %agent_id, inbox_rows = deleted.inbox_rows, "agent deleted");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/agents`: both views carry `name` and `profile` so agents can
/// address each other; the owner view adds route and delivery fields
/// (`IF-WIRE-001`).
async fn list(principal: Principal, State(state): State<AppState>) -> Result<Response, ApiError> {
    let records = state.store.run(|conn| agent_queries::list(conn)).await?;
    let response = match principal {
        Principal::Owner => Json(AgentList {
            items: records.into_iter().map(owner_entry).collect(),
        })
        .into_response(),
        Principal::Agent(_) => Json(AgentList {
            items: records.into_iter().map(agent_entry).collect(),
        })
        .into_response(),
    };
    Ok(response)
}

fn owner_entry(record: AgentRecord) -> OwnerAgentEntry {
    OwnerAgentEntry {
        agent_id: record.agent_id,
        name: record.name,
        joined_all: record.joined_all,
        profile: record.profile,
        receiver_url: record.receiver_url,
        wake_policy: record.wake_policy,
        route_version: record.route_version,
        created_at: record.created_at,
        delivery: DeliverySummary {
            state: record.delivery_state,
        },
        avatar: record.avatar,
    }
}

fn agent_entry(record: AgentRecord) -> AgentEntry {
    AgentEntry {
        agent_id: record.agent_id,
        name: record.name,
        joined_all: record.joined_all,
        profile: record.profile,
        avatar: record.avatar,
    }
}

/// 422 `invalid_field` naming the profile field. The message never echoes
/// the value.
fn profile_field_error(err: ProfileFieldError) -> ApiError {
    ApiError::invalid_field(err.field(), err.to_string())
}

/// `PUT /v1/agents/{agent_id}/profile`: owner or the agent itself; `name`
/// is owner only. Checks run in `IF-WIRE-006` order: the extractors give
/// 401, 403 (another agent's path) and 404 (not a slug), `StrictJson` the
/// body format, then the `name` permission, the value rules and finally
/// whether the agent exists.
async fn update_profile(
    target: ManagedBy,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, ApiError> {
    if request.has_name() {
        target.principal.require_owner()?;
    }
    let update = request.validate().map_err(profile_field_error)?;

    let agent_id = target.agent_id;
    let target_id = agent_id.clone();
    let change = state
        .store
        .transaction(move |tx| {
            agent_queries::update_profile(tx, &target_id, &update, &clock::now_rfc3339_ms())
        })
        .await?
        .ok_or_else(ApiError::agent_not_found)?;

    if !change.changed.is_empty() {
        // Field names only: the values are free text other agents read.
        let fields: Vec<&str> = change.changed.iter().map(|field| field.as_str()).collect();
        info!(agent_id = %agent_id, fields = %fields.join(","), "agent profile updated");
    }
    Ok(Json(ProfileResponse {
        agent_id,
        name: change.record.name,
        profile: change.record.profile,
    }))
}

/// `PATCH /v1/agents/{agent_id}/receiver`: owner or the agent itself.
/// `receiver_url`, `wake_policy` or both; an omitted key keeps its value
/// and `{}` is 422 (`IF-WIRE-004`). Only a URL change moves the route.
async fn update_receiver(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<UpdateReceiverRequest>,
) -> Result<Json<ReceiverResponse>, ApiError> {
    let UpdateReceiverRequest {
        receiver_url,
        wake_policy,
    } = request;
    if receiver_url.is_keep() && wake_policy.is_none() {
        return Err(ApiError::unprocessable(
            codes::INVALID_FIELD,
            "send receiver_url, wake_policy or both",
        ));
    }
    let receiver_url = receiver_url.try_map(|url| receiver_url::normalize(&url))?;
    let wake_policy = wake_policy
        .map(|raw| {
            raw.parse::<WakePolicy>().map_err(|_| {
                ApiError::invalid_field("wake_policy", "wake_policy must be sender or always")
            })
        })
        .transpose()?;

    let target = agent_id.clone();
    let route = state
        .store
        .transaction(move |tx| {
            agent_queries::update_receiver(tx, &target, receiver_url, wake_policy)
        })
        .await?
        .ok_or_else(ApiError::agent_not_found)?;
    if route.route_changed {
        // A new route may have scheduled a notification.
        state.delivery.notify();
    }
    if let Some(wake_policy) = wake_policy {
        info!(agent_id = %agent_id, wake_policy = %wake_policy, "receiver wake_policy set");
    }

    Ok(Json(ReceiverResponse {
        agent_id,
        receiver_url: route.receiver_url,
        route_version: route.route_version,
        wake_policy: route.wake_policy,
        state: route.delivery_state,
    }))
}

/// `PUT /v1/agents/{agent_id}/membership/all`: owner or the agent itself.
async fn update_all_membership(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<MembershipRequest>,
) -> Result<Json<MembershipResponse>, ApiError> {
    let target = agent_id.clone();
    let membership = state
        .store
        .transaction(move |tx| agent_queries::set_all_membership(tx, &target, request.joined))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;

    let effective_event_seq = Seq::new(membership.effective_event_seq).map_err(|_| {
        StoreError::Invariant("memberships.effective_event_seq is negative".to_owned())
    })?;
    Ok(Json(MembershipResponse {
        agent_id,
        joined_all: membership.joined,
        membership_epoch: membership.epoch,
        effective_event_seq,
    }))
}

/// `POST /v1/agents/{agent_id}/tokens/rotate`: owner only. Both credentials
/// are replaced at once; the old API key stops authenticating immediately.
async fn rotate_tokens(
    _owner: OwnerOnly,
    State(state): State<AppState>,
    AgentPath(raw_agent_id): AgentPath,
) -> Result<Json<RotateTokensResponse>, ApiError> {
    let agent_id: AgentId = raw_agent_id
        .parse()
        .map_err(|_| ApiError::agent_not_found())?;

    let credentials = IssuedCredentials::generate();
    let target = agent_id.clone();
    let api_key_hash = credentials.api_key_hash.clone();
    let receiver_secret = credentials.receiver_secret.clone();
    let route_version = state
        .store
        .transaction(move |tx| {
            agent_queries::rotate_credentials(tx, &target, &api_key_hash, &receiver_secret)
        })
        .await?
        .ok_or_else(ApiError::agent_not_found)?;
    state.delivery.notify();

    info!(agent_id = %agent_id, route_version, "agent credentials rotated");
    Ok(Json(RotateTokensResponse {
        agent_id,
        route_version,
        agent_api_key: credentials.agent_api_key,
        receiver_secret: credentials.receiver_secret,
        previous_keys_revoked: true,
    }))
}
