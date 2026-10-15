//! Bearer authentication and permission checks (`IF-AUTH-001`).
//!
//! Every `/v1/*` handler takes a [`Principal`]. The extractor compares the
//! bearer token with the owner token in constant time, then looks up the
//! SHA-256 of the token among agent API keys. Anything else, including a
//! receiver secret, is 401. Token text never reaches logs or error bodies.

pub mod credential;

use axum::extract::{FromRef, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use platform_wire::AgentId;
use subtle::ConstantTimeEq;

use crate::api::ApiError;
use crate::config::OwnerToken;
use crate::store::{Store, StoreError, queries};

const BEARER_SCHEME: &str = "bearer";

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Principal {
    Owner,
    Agent(AgentId),
}

impl Principal {
    /// 403 unless the caller is the owner.
    pub fn require_owner(&self) -> Result<(), ApiError> {
        match self {
            Principal::Owner => Ok(()),
            Principal::Agent(_) => Err(ApiError::forbidden()),
        }
    }

    /// 403 unless the caller is the owner or the agent named `agent_id`.
    /// Takes the raw path segment so that it can run before slug validation.
    pub fn require_owner_or_self(&self, agent_id: &str) -> Result<(), ApiError> {
        match self {
            Principal::Owner => Ok(()),
            Principal::Agent(own) if own.as_str() == agent_id => Ok(()),
            Principal::Agent(_) => Err(ApiError::forbidden()),
        }
    }
}

/// Resolves bearer tokens to principals.
#[derive(Clone)]
pub struct Authenticator {
    owner_token: OwnerToken,
    store: Store,
}

impl Authenticator {
    pub fn new(owner_token: OwnerToken, store: Store) -> Self {
        Self { owner_token, store }
    }

    /// `Ok(None)` when the token matches neither the owner nor any agent.
    pub async fn authenticate(&self, token: &str) -> Result<Option<Principal>, StoreError> {
        let is_owner: bool = token
            .as_bytes()
            .ct_eq(self.owner_token.expose().as_bytes())
            .into();
        if is_owner {
            return Ok(Some(Principal::Owner));
        }
        let hash = credential::hash_api_key(token);
        let agent = self
            .store
            .run(move |conn| queries::agents::find_by_api_key_hash(conn, &hash))
            .await?;
        Ok(agent.map(Principal::Agent))
    }
}

impl<S> FromRequestParts<S> for Principal
where
    S: Send + Sync,
    Authenticator: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Some(token) = bearer_token(&parts.headers) else {
            return Err(ApiError::unauthorized());
        };
        let authenticator = Authenticator::from_ref(state);
        authenticator
            .authenticate(token)
            .await?
            .ok_or_else(ApiError::unauthorized)
    }
}

/// The token of an `Authorization: Bearer <token>` header. The scheme is
/// case-insensitive (RFC 9110 §11.1).
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case(BEARER_SCHEME) {
        return None;
    }
    let token = token.trim_matches(' ');
    if token.is_empty() {
        return None;
    }
    Some(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers_with(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn parses_bearer_tokens() {
        assert_eq!(bearer_token(&headers_with("Bearer abc")), Some("abc"));
        assert_eq!(bearer_token(&headers_with("bearer  abc ")), Some("abc"));
    }

    #[test]
    fn rejects_other_schemes_and_empty_tokens() {
        assert_eq!(bearer_token(&HeaderMap::new()), None);
        assert_eq!(bearer_token(&headers_with("Basic abc")), None);
        assert_eq!(bearer_token(&headers_with("Bearer ")), None);
        assert_eq!(bearer_token(&headers_with("Bearer")), None);
    }

    #[test]
    fn owner_passes_every_check() {
        assert!(Principal::Owner.require_owner().is_ok());
        assert!(Principal::Owner.require_owner_or_self("anyone").is_ok());
    }

    #[test]
    fn agent_passes_only_for_itself() {
        let agent = Principal::Agent("agent-a".parse().unwrap());
        assert!(agent.require_owner().is_err());
        assert!(agent.require_owner_or_self("agent-a").is_ok());
        assert!(agent.require_owner_or_self("agent-b").is_err());
    }
}
