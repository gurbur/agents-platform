//! Reactions (`IF-API-REACTIONS`, `IF-WIRE-016`, `IF-WIRE-017`): `PUT` and
//! `DELETE /v1/messages/{message_id}/reactions/{kind}`, the change feed
//! `GET /v1/reactions`, and the kind registry `GET /v1/reaction-kinds`
//! and `PUT /v1/reaction-kinds/{slug}`.
//!
//! These handlers run the checks that need no database (authentication,
//! the path, the body, the owner's allowlist); the store runs the rest in
//! one transaction. Logs carry ids, the kind slug and `change_seq` only:
//! never a message body, a glyph or a label.

use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use platform_wire::{
    AgentId, MessageId, PutReactionKindRequest, ReactRequest, ReactResponse, ReactionFeedPage,
    ReactionKind, ReactionKindEntry, ReactionKindList, codes,
};
use serde::Deserialize;
use tracing::info;

use super::history::{message_not_found, parse_room, unknown_room, viewer_of};
use super::json::{JsonOrEmpty, StrictJson};
use super::query::QueryParams;
use super::{ApiError, AppState};
use crate::auth::Principal;
use crate::clock;
use crate::store::queries::reaction_kinds::{self as kind_queries, UpsertOutcome};
use crate::store::queries::reactions::{
    self as reaction_queries, FeedQuery, ReactCommand, ReactOutcome, ReactRejection, Reactor,
    UnreactOutcome,
};
use crate::store::queries::rooms::{self as room_queries, ReadAccess};

const FEED_PARAMS: &[&str] = &["room_id", "after_change_seq", "limit"];
const KIND_LIST_PARAMS: &[&str] = &["all"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/v1/messages/{message_id}/reactions/{kind}",
            put(react).delete(unreact),
        )
        .route("/v1/reactions", get(feed))
        .route("/v1/reaction-kinds", get(list_kinds))
        .route("/v1/reaction-kinds/{slug}", put(put_kind))
}

/// The `DELETE` body: only the owner's identity (an agent's body is not
/// read).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnreactRequest {
    #[serde(default)]
    sender_id: Option<String>,
}

/// `PUT /v1/messages/{message_id}/reactions/{kind}`: 201 for a new
/// reaction, 200 (nothing written) for one already there.
async fn react(
    principal: Principal,
    State(state): State<AppState>,
    Path((raw_message_id, raw_kind)): Path<(String, String)>,
    request: Result<JsonOrEmpty<ReactRequest>, ApiError>,
) -> Result<(StatusCode, Json<ReactResponse>), ApiError> {
    // 2, 3
    let (message_id, kind) = parse_path(&raw_message_id, &raw_kind)?;
    // 4
    let request = match request {
        Ok(JsonOrEmpty(request)) => request,
        Err(err) if principal != Principal::Owner && err.names_field("sender_id") => {
            return Err(unexpected_sender_id());
        }
        Err(err) => return Err(err),
    };
    // 5–7
    let reactor = reactor_of(principal, &state.post_as, request.sender_id)?;
    let wake = request.wake.unwrap_or(false);

    let outcome = state
        .store
        .transaction(move |tx| {
            // Taken on the DB thread so `created_at` follows `change_seq` order.
            let command = ReactCommand {
                reactor,
                message_id,
                kind,
                wake,
                created_at: clock::now_rfc3339_ms(),
            };
            reaction_queries::put(tx, &command)
        })
        .await?;

    match outcome {
        ReactOutcome::Created {
            response, woken, ..
        } => {
            if woken {
                state.delivery.notify();
            }
            info!(
                message_id = %response.message_id,
                agent_id = %response.agent_id,
                kind = %response.kind,
                change_seq = %response.change_seq,
                "reaction added"
            );
            Ok((StatusCode::CREATED, Json(response)))
        }
        ReactOutcome::Existing(response) => Ok((StatusCode::OK, Json(response))),
        ReactOutcome::Rejected(rejection) => Err(rejection_error(rejection)),
    }
}

/// `DELETE /v1/messages/{message_id}/reactions/{kind}`: 204 whether or not
/// the reaction was there. The kind's registration is not checked.
async fn unreact(
    principal: Principal,
    State(state): State<AppState>,
    Path((raw_message_id, raw_kind)): Path<(String, String)>,
    request: Result<JsonOrEmpty<UnreactRequest>, ApiError>,
) -> Result<StatusCode, ApiError> {
    let (message_id, kind) = parse_path(&raw_message_id, &raw_kind)?;
    let reactor = match principal {
        Principal::Agent(agent_id) => Reactor::Agent(agent_id),
        Principal::Owner => {
            let JsonOrEmpty(request) = request?;
            reactor_of(Principal::Owner, &state.post_as, request.sender_id)?
        }
    };

    let agent_id = reactor.agent_id().clone();
    let (removed_message, removed_kind) = (message_id.clone(), kind.clone());
    let outcome = state
        .store
        .transaction(move |tx| {
            let command = ReactCommand {
                reactor,
                message_id: removed_message,
                kind: removed_kind,
                wake: false,
                created_at: clock::now_rfc3339_ms(),
            };
            reaction_queries::remove(tx, &command)
        })
        .await?;

    match outcome {
        UnreactOutcome::Removed(change_seq) => {
            info!(
                message_id = %message_id,
                agent_id = %agent_id,
                kind = %kind,
                change_seq = %change_seq,
                "reaction removed"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        UnreactOutcome::Absent => Ok(StatusCode::NO_CONTENT),
        UnreactOutcome::Rejected(rejection) => Err(rejection_error(rejection)),
    }
}

/// `GET /v1/reactions?room_id=…[&after_change_seq=…][&limit=…]`: the room's
/// reaction changes for a caller with a membership row (or the owner).
async fn feed(
    principal: Principal,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<ReactionFeedPage>, ApiError> {
    let params = QueryParams::parse(query.as_deref(), FEED_PARAMS)?;
    let room = parse_room(params.get("room_id"))?;
    let viewer = viewer_of(principal);
    let checked_room = room.clone();
    let access = state
        .store
        .run(move |conn| room_queries::read_access(conn, &viewer, &checked_room))
        .await?;
    match access {
        ReadAccess::Allowed => {}
        ReadAccess::UnknownRoom => return Err(unknown_room()),
        // `read_access` has no thread step; a thread participant without a
        // membership row does not read the feed.
        ReadAccess::Forbidden | ReadAccess::ThreadNotFound => return Err(ApiError::forbidden()),
    }
    let feed_query = FeedQuery {
        room,
        after: params.seq("after_change_seq")?,
        limit: params.limit()?,
    };
    let page = state
        .store
        .run(move |conn| reaction_queries::feed(conn, &feed_query))
        .await?;
    Ok(Json(page))
}

/// `GET /v1/reaction-kinds[?all=true]`: the enabled kinds for everyone;
/// every kind for the owner's `all=true`.
async fn list_kinds(
    principal: Principal,
    State(state): State<AppState>,
    RawQuery(query): RawQuery,
) -> Result<Json<ReactionKindList>, ApiError> {
    let params = QueryParams::parse(query.as_deref(), KIND_LIST_PARAMS)?;
    let include_disabled = match params.get("all") {
        None | Some("false") => false,
        Some("true") => true,
        Some(_) => {
            return Err(ApiError::invalid_field(
                "all",
                "all must be `true` or `false`",
            ));
        }
    };
    if include_disabled {
        principal.require_owner()?;
    }
    let kinds = state
        .store
        .run(move |conn| kind_queries::list(conn, include_disabled))
        .await?;
    Ok(Json(ReactionKindList { kinds }))
}

/// `PUT /v1/reaction-kinds/{slug}` (owner): 201 for a new kind, 200 for an
/// existing one.
async fn put_kind(
    principal: Principal,
    State(state): State<AppState>,
    Path(raw_slug): Path<String>,
    request: Result<StrictJson<PutReactionKindRequest>, ApiError>,
) -> Result<(StatusCode, Json<ReactionKindEntry>), ApiError> {
    principal.require_owner()?;
    let slug: ReactionKind =
        raw_slug
            .parse()
            .map_err(|err: platform_wire::ReactionKindError| {
                ApiError::invalid_field("slug", err.to_string())
            })?;
    let StrictJson(request) = request?;
    let update = request
        .validate()
        .map_err(|err| ApiError::invalid_field(err.field(), err.to_string()))?;

    let outcome = state
        .store
        .transaction(move |tx| kind_queries::upsert(tx, &slug, &update, &clock::now_rfc3339_ms()))
        .await?;
    let (status, entry, change) = match outcome {
        UpsertOutcome::Created(entry) => (StatusCode::CREATED, entry, "created"),
        UpsertOutcome::Updated(entry) => (StatusCode::OK, entry, "updated"),
        UpsertOutcome::GlyphTaken => {
            return Err(ApiError::conflict(
                codes::GLYPH_TAKEN,
                "another reaction kind already uses this glyph",
            )
            .with_detail("field", "glyph"));
        }
    };
    info!(
        slug = %entry.slug,
        enabled = entry.enabled,
        change,
        "reaction kind upserted"
    );
    Ok((status, Json(entry)))
}

/// Steps 2 and 3: a malformed message id is a missing message (404), a
/// malformed kind is 422 naming `kind`.
fn parse_path(raw_message_id: &str, raw_kind: &str) -> Result<(MessageId, ReactionKind), ApiError> {
    let message_id: MessageId = raw_message_id.parse().map_err(|_| message_not_found())?;
    let kind: ReactionKind =
        raw_kind
            .parse()
            .map_err(|err: platform_wire::ReactionKindError| {
                ApiError::invalid_field("kind", err.to_string())
            })?;
    Ok((message_id, kind))
}

/// Steps 5–7: an agent reacts as itself (it may not name a `sender_id`);
/// the owner names an allowlisted identity, compared verbatim.
fn reactor_of(
    principal: Principal,
    post_as: &[AgentId],
    sender_id: Option<String>,
) -> Result<Reactor, ApiError> {
    match (principal, sender_id) {
        (Principal::Agent(_), Some(_)) => Err(unexpected_sender_id()),
        (Principal::Agent(agent_id), None) => Ok(Reactor::Agent(agent_id)),
        (Principal::Owner, None) => Err(ApiError::invalid_field(
            "sender_id",
            "the owner reacts with a sender_id from [owner] post_as",
        )),
        (Principal::Owner, Some(named)) => post_as
            .iter()
            .find(|allowed| allowed.as_str() == named)
            .cloned()
            .map(Reactor::Owner)
            .ok_or_else(|| ApiError::forbidden().with_detail("field", "sender_id")),
    }
}

fn unexpected_sender_id() -> ApiError {
    ApiError::unprocessable(
        codes::UNEXPECTED_FIELD,
        "unexpected field `sender_id`: an agent reacts as itself",
    )
    .with_detail("field", "sender_id")
}

/// The error for a reaction request that wrote nothing.
fn rejection_error(rejection: ReactRejection) -> ApiError {
    match rejection {
        ReactRejection::UnknownSender => {
            ApiError::agent_not_found().with_detail("field", "sender_id")
        }
        ReactRejection::MessageNotFound => message_not_found(),
        ReactRejection::RoomArchived => ApiError::conflict(
            codes::ROOM_ARCHIVED,
            "the room is archived and only readable",
        ),
        ReactRejection::UnknownKind => ApiError::unprocessable(
            codes::UNKNOWN_REACTION_KIND,
            "no enabled reaction kind has this slug; see GET /v1/reaction-kinds",
        )
        .with_detail("field", "kind"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reactor_is_the_agent_or_an_allowlisted_identity() {
        let post_as: Vec<AgentId> = vec!["human".parse().unwrap()];
        let agent = || Principal::Agent("agent-a".parse().unwrap());
        assert_eq!(
            reactor_of(agent(), &post_as, None).unwrap(),
            Reactor::Agent("agent-a".parse().unwrap())
        );
        let named = reactor_of(agent(), &post_as, Some("agent-a".into())).unwrap_err();
        assert_eq!(named.code(), codes::UNEXPECTED_FIELD);
        let missing = reactor_of(Principal::Owner, &post_as, None).unwrap_err();
        assert!(missing.names_field("sender_id"));
        assert_eq!(
            reactor_of(Principal::Owner, &post_as, Some("human".into())).unwrap(),
            Reactor::Owner("human".parse().unwrap())
        );
        for refused in ["Human", "agent-a", ""] {
            let err = reactor_of(Principal::Owner, &post_as, Some(refused.into())).unwrap_err();
            assert_eq!(err.status(), StatusCode::FORBIDDEN, "{refused:?}");
        }
    }

    #[test]
    fn a_malformed_path_is_a_missing_message_or_an_invalid_kind() {
        let missing = parse_path("nope", "Bad").unwrap_err();
        assert_eq!(missing.code(), codes::MESSAGE_NOT_FOUND);
        let kind = parse_path("msg-01J00000000000000000000000", "Bad").unwrap_err();
        assert!(kind.names_field("kind"));
    }
}
