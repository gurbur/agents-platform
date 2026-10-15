//! Error responses in the `IF-API-COMMON` body shape
//! `{"error":{"code","message","request_id","details"}}`.

use axum::Json;
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use platform_wire::{ErrorBody, ErrorDetail, codes};
use serde_json::{Map, Value};
use tracing::error;

use super::RequestId;
use crate::store::StoreError;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Map<String, Value>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            details: Map::new(),
        }
    }

    /// Adds one `details` entry, e.g. `field` for `IF-WIRE-002` errors.
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn details(&self) -> &Map<String, Value> {
        &self.details
    }

    /// 401: no credential, an unknown credential, or a receiver secret.
    pub fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            codes::UNAUTHORIZED,
            "a valid owner token or agent API key is required",
        )
    }

    /// 403: authenticated, but not allowed to act on this resource.
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            codes::FORBIDDEN,
            "this credential is not allowed to perform the request",
        )
    }

    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    pub fn conflict(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, code, message)
    }

    pub fn payload_too_large(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, code, message)
    }

    pub fn unsupported_media_type() -> Self {
        Self::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            codes::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        )
    }

    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNPROCESSABLE_ENTITY, code, message)
    }

    /// 422 `invalid_field` naming the offending field.
    pub fn invalid_field(field: &str, message: impl Into<String>) -> Self {
        Self::unprocessable(codes::INVALID_FIELD, message).with_detail("field", field)
    }

    pub fn agent_not_found() -> Self {
        Self::not_found(codes::AGENT_NOT_FOUND, "no agent with this agent_id")
    }

    /// 500 with a generic message; the cause is logged, never returned.
    pub fn internal() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            codes::INTERNAL,
            "internal server error",
        )
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, codes::UNAVAILABLE, message)
    }
}

impl From<StoreError> for ApiError {
    fn from(err: StoreError) -> Self {
        error!(error = %err, "database operation failed");
        Self::internal()
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Responses are produced inside the request-id scope; the fallback only
        // matters if a handler is ever mounted outside the middleware.
        let request_id = RequestId::current()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.code.to_owned(),
                message: self.message,
                request_id,
                details: self.details,
            },
        };
        let mut response = (self.status, Json(body)).into_response();
        if self.status == StatusCode::UNAUTHORIZED {
            // RFC 6750 §3: a 401 names the expected scheme.
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
