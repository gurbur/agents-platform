//! Agent registration and directory, receiver route, `all` membership and
//! token rotation (`IF-API-AGENTS`, `IF-API-RECEIVER`, `IF-API-MEMBERSHIP`,
//! `IF-API-ROTATE`).
//!
//! Authentication and permission run in `FromRequestParts` extractors, so a
//! caller without permission gets 401/403 before the body is examined.

use axum::extract::{FromRequestParts, Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{patch, post, put};
use axum::{Json, Router};
use platform_wire::{
    AgentEntry, AgentId, AgentList, DeliverySummary, MembershipRequest, MembershipResponse,
    OwnerAgentEntry, ReceiverResponse, RegisterAgentRequest, RegisterAgentResponse,
    RotateTokensResponse, Seq, UpdateReceiverRequest, codes,
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
        .route("/v1/agents/{agent_id}/receiver", patch(update_receiver))
        .route(
            "/v1/agents/{agent_id}/membership/all",
            put(update_all_membership),
        )
        .route("/v1/agents/{agent_id}/tokens/rotate", post(rotate_tokens))
}

/// Extractor: the caller is the owner (else 401/403).
struct OwnerOnly;

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
struct ManagedAgent(AgentId);

impl FromRequestParts<AppState> for ManagedAgent {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let principal = Principal::from_request_parts(parts, state).await?;
        let AgentPath(raw) = AgentPath::from_request_parts(parts, state).await?;
        principal.require_owner_or_self(&raw)?;
        let agent_id = raw.parse().map_err(|_| ApiError::agent_not_found())?;
        Ok(ManagedAgent(agent_id))
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
    let receiver_url = request
        .receiver_url
        .as_deref()
        .map(receiver_url::normalize)
        .transpose()?;

    let credentials = IssuedCredentials::generate();
    let new_agent = NewAgent {
        agent_id,
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
        joined_all: agent.joined_all,
        receiver_url: agent.receiver_url,
        route_version: agent.route_version,
        agent_api_key: credentials.agent_api_key,
        receiver_secret: credentials.receiver_secret,
    };
    Ok((StatusCode::CREATED, Json(body)))
}

/// `GET /v1/agents`: the agent view lists ids and membership only; the owner
/// view adds route and delivery fields (`IF-WIRE-001`).
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
        joined_all: record.joined_all,
        receiver_url: record.receiver_url,
        route_version: record.route_version,
        created_at: record.created_at,
        delivery: DeliverySummary {
            state: record.delivery_state,
        },
    }
}

fn agent_entry(record: AgentRecord) -> AgentEntry {
    AgentEntry {
        agent_id: record.agent_id,
        joined_all: record.joined_all,
    }
}

/// `PATCH /v1/agents/{agent_id}/receiver`: owner or the agent itself.
async fn update_receiver(
    ManagedAgent(agent_id): ManagedAgent,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<UpdateReceiverRequest>,
) -> Result<Json<ReceiverResponse>, ApiError> {
    let receiver_url = request
        .receiver_url
        .as_deref()
        .map(receiver_url::normalize)
        .transpose()?;

    let target = agent_id.clone();
    let route = state
        .store
        .transaction(move |tx| agent_queries::update_receiver(tx, &target, receiver_url))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;

    Ok(Json(ReceiverResponse {
        agent_id,
        receiver_url: route.receiver_url,
        route_version: route.route_version,
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

    info!(agent_id = %agent_id, route_version, "agent credentials rotated");
    Ok(Json(RotateTokensResponse {
        agent_id,
        route_version,
        agent_api_key: credentials.agent_api_key,
        receiver_secret: credentials.receiver_secret,
        previous_keys_revoked: true,
    }))
}
