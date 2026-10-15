//! `x-request-id` on every response. The same id is exposed through a task
//! local so error bodies can carry it as `request_id` (`IF-ID-001`).

use std::fmt;

use axum::extract::Request;
use axum::http::HeaderValue;
use axum::http::header::HeaderName;
use axum::middleware::Next;
use axum::response::Response;
use tracing::info;
use ulid::Ulid;

pub const X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

const PREFIX: &str = "req-";

tokio::task_local! {
    static CURRENT: RequestId;
}

/// `req-` + ULID (26 upper-case Crockford base32 characters).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    pub fn generate() -> Self {
        Self(format!("{PREFIX}{}", Ulid::generate()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The id of the request being handled on this task, if any.
    pub fn current() -> Option<RequestId> {
        CURRENT.try_with(Clone::clone).ok()
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Middleware: assigns an id, runs the request inside its scope, stamps the
/// response header and logs one line. Only method, path and status are logged;
/// never headers, query strings or bodies.
pub async fn assign(mut request: Request, next: Next) -> Response {
    let id = RequestId::generate();
    request.extensions_mut().insert(id.clone());
    let method = request.method().clone();
    let path = request.uri().path().to_owned();

    let mut response = CURRENT.scope(id.clone(), next.run(request)).await;

    // Invariant: `req-` + Crockford base32 is visible ASCII, a valid header value.
    let value = HeaderValue::from_str(id.as_str()).expect("request id is visible ASCII");
    response.headers_mut().insert(X_REQUEST_ID, value);
    info!(
        request_id = %id,
        %method,
        path,
        status = response.status().as_u16(),
        "request"
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_have_prefix_and_ulid() {
        let id = RequestId::generate();
        let ulid = id.as_str().strip_prefix(PREFIX).unwrap();
        assert_eq!(ulid.len(), 26);
        assert!(ulid.parse::<Ulid>().is_ok());
        assert_eq!(ulid, ulid.to_ascii_uppercase());
        assert_ne!(id, RequestId::generate());
    }
}
