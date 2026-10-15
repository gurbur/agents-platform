//! `GET /v1/inbox`, `GET /v1/agents/{agent_id}/inbox` (`IF-API-INBOX`) and
//! `PUT /v1/inbox/read-marker` (`IF-API-READMARKER`).
//!
//! Query order (`IF-WIRE-005`): parameter names (422) → permission (403) →
//! `after_seq`/`until_seq`/`limit` values (422) → the target agent (404,
//! owner path only). The GET paths never write; only the PUT moves the
//! marker, and a marker at the head makes the delivery state idle in the
//! same transaction.

use axum::extract::{FromRequestParts, RawQuery, State};
use axum::http::request::Parts;
use axum::routing::{get, put};
use axum::{Json, Router};
use platform_wire::{AgentId, InboxPage, ReadMarkerRequest, ReadMarkerResponse, Seq, codes};
use tracing::info;

use super::agents::AgentPath;
use super::json::StrictJson;
use super::query::QueryParams;
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::store::StoreError;
use crate::store::queries::inbox::{self as inbox_queries, InboxQuery, ReadMarkerOutcome};

const PARAMS: &[&str] = &["after_seq", "until_seq", "limit"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/inbox", get(own_inbox))
        .route("/v1/inbox/read-marker", put(update_read_marker))
        .route("/v1/agents/{agent_id}/inbox", get(agent_inbox))
}

/// Extractor: the calling agent; the owner is 403 before the body is read.
struct AgentOnly(AgentId);

impl FromRequestParts<AppState> for AgentOnly {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, ApiError> {
        let principal = Principal::from_request_parts(parts, state).await?;
        principal.require_agent().map(AgentOnly)
    }
}

/// The page bounds of a request, validated but not yet defaulted.
struct PageParams {
    after_seq: Option<Seq>,
    until_seq: Option<Seq>,
    limit: u32,
}

impl PageParams {
    fn parse(params: &QueryParams) -> Result<Self, ApiError> {
        Ok(Self {
            after_seq: params.seq("after_seq")?,
            until_seq: params.seq("until_seq")?,
            limit: params.limit()?,
        })
    }

    fn for_agent(self, agent_id: AgentId) -> InboxQuery {
        InboxQuery {
            agent_id,
            after_seq: self.after_seq,
            until_seq: self.until_seq,
            limit: self.limit,
        }
    }
}

/// `GET /v1/inbox`: the calling agent's own inbox.
async fn own_inbox(
    principal: Principal,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<InboxPage>, ApiError> {
    let params = QueryParams::parse(query.as_deref(), PARAMS)?;
    let agent_id = principal.require_agent()?;
    let page_params = PageParams::parse(&params)?;

    let page = read_page(&state, page_params.for_agent(agent_id))
        .await?
        .ok_or_else(authenticated_agent_missing)?;
    Ok(Json(page))
}

/// `GET /v1/agents/{agent_id}/inbox`: owner only, the same page without any
/// side effect. Agents use `GET /v1/inbox`, even for themselves.
async fn agent_inbox(
    principal: Principal,
    State(state): State<AppState>,
    AgentPath(raw_agent_id): AgentPath,
    RawQuery(query): RawQuery,
) -> Result<Json<InboxPage>, ApiError> {
    let params = QueryParams::parse(query.as_deref(), PARAMS)?;
    principal.require_owner()?;
    let page_params = PageParams::parse(&params)?;
    let agent_id: AgentId = raw_agent_id
        .parse()
        .map_err(|_| ApiError::agent_not_found())?;

    let page = read_page(&state, page_params.for_agent(agent_id))
        .await?
        .ok_or_else(ApiError::agent_not_found)?;
    Ok(Json(page))
}

async fn read_page(state: &AppState, query: InboxQuery) -> Result<Option<InboxPage>, ApiError> {
    let agent_id = query.agent_id.clone();
    let slice = state
        .store
        .run(move |conn| inbox_queries::page(conn, &query))
        .await?;
    let Some(slice) = slice else {
        return Ok(None);
    };
    let next_after_seq = slice
        .items
        .last()
        .map_or(slice.after_seq, |item| item.inbox_seq);
    Ok(Some(InboxPage {
        agent_id,
        read_marker: slice.read_marker,
        snapshot_head_seq: slice.snapshot_head_seq,
        items: slice.items,
        next_after_seq,
        has_more: slice.has_more,
    }))
}

/// `PUT /v1/inbox/read-marker`: raises the caller's own marker. A value at
/// or below the marker is a 200 no-op; beyond the head is 409.
async fn update_read_marker(
    AgentOnly(agent_id): AgentOnly,
    State(state): State<AppState>,
    StrictJson(request): StrictJson<ReadMarkerRequest>,
) -> Result<Json<ReadMarkerResponse>, ApiError> {
    let target = agent_id.clone();
    let outcome = state
        .store
        .transaction(move |tx| inbox_queries::advance_read_marker(tx, &target, request.through_seq))
        .await?
        .ok_or_else(authenticated_agent_missing)?;

    let read_marker = match outcome {
        ReadMarkerOutcome::Advanced(read_marker) => {
            info!(agent_id = %agent_id, read_marker = %read_marker, "read marker advanced");
            read_marker
        }
        ReadMarkerOutcome::Unchanged(read_marker) => read_marker,
        ReadMarkerOutcome::Ahead => {
            return Err(ApiError::conflict(
                codes::READ_MARKER_AHEAD,
                "through_seq is beyond the inbox head",
            ));
        }
    };
    Ok(Json(ReadMarkerResponse {
        agent_id,
        read_marker,
    }))
}

/// An authenticated agent always has its `agents` and `read_state` rows:
/// v1 has no agent deletion. Reaching this is a 500.
fn authenticated_agent_missing() -> ApiError {
    StoreError::Invariant("authenticated agent has no inbox cursor".to_owned()).into()
}
