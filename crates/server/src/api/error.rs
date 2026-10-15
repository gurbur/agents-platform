//! Error responses in the `IF-API-COMMON` body shape.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use platform_wire::{ErrorBody, codes};

use super::RequestId;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, codes::UNAVAILABLE, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Responses are produced inside the request-id scope; the fallback only
        // matters if a handler is ever mounted outside the middleware.
        let request_id = RequestId::current()
            .map(|id| id.to_string())
            .unwrap_or_default();
        let body = ErrorBody::new(self.code, self.message, request_id);
        (self.status, Json(body)).into_response()
    }
}
