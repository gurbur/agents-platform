//! `StrictJson<T>`: the request-body extractor for every write handler
//! (`IF-WIRE-002`). `T` is expected to use `#[serde(deny_unknown_fields)]`.
//!
//! | failure | status | code |
//! | --- | --- | --- |
//! | `Content-Type` is not `application/json` | 415 | `unsupported_media_type` |
//! | wire body over [`MAX_WIRE_BODY_BYTES`] | 413 | `request_too_large` |
//! | body is not JSON | 400 | `invalid_json` |
//! | field outside the allowed set | 422 | `unexpected_field` + `details.field` |
//! | missing field, wrong type, invalid value | 422 | `invalid_field` + `details.field` |
//!
//! The wire limit itself is enforced by [`body_limit`] on the router so that
//! oversized bodies are cut off while buffering, before any parsing.

use axum::body::Bytes;
use axum::extract::rejection::BytesRejection;
use axum::extract::{DefaultBodyLimit, FromRequest, Request};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use platform_wire::codes;
use serde::de::DeserializeOwned;
use serde_json::error::Category;

use super::ApiError;

/// POST wire body limit (`IF-API-COMMON`: 128 KiB).
pub const MAX_WIRE_BODY_BYTES: usize = 128 * 1024;

/// Router layer that caps buffered request bodies at [`MAX_WIRE_BODY_BYTES`].
pub fn body_limit() -> DefaultBodyLimit {
    DefaultBodyLimit::max(MAX_WIRE_BODY_BYTES)
}

/// A JSON body parsed under the `IF-WIRE-002` error mapping.
#[derive(Debug, Clone)]
pub struct StrictJson<T>(pub T);

impl<S, T> FromRequest<S> for StrictJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_content_type(request.headers()) {
            return Err(ApiError::unsupported_media_type());
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(body_rejection)?;
        parse(&bytes).map(StrictJson)
    }
}

/// `application/json`, optionally with parameters such as `charset=utf-8`.
fn is_json_content_type(headers: &HeaderMap) -> bool {
    let Some(value) = headers.get(CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let essence = value.split(';').next().unwrap_or_default().trim();
    essence.eq_ignore_ascii_case("application/json")
}

fn body_rejection(rejection: BytesRejection) -> ApiError {
    if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE {
        return ApiError::payload_too_large(
            codes::REQUEST_TOO_LARGE,
            format!("request body exceeds {MAX_WIRE_BODY_BYTES} bytes"),
        );
    }
    ApiError::bad_request(codes::INVALID_JSON, "request body could not be read")
}

fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let value = serde_path_to_error::deserialize(&mut deserializer).map_err(classify)?;
    // Trailing non-whitespace after the value is a syntax error.
    deserializer.end().map_err(|_| invalid_json())?;
    Ok(value)
}

fn invalid_json() -> ApiError {
    ApiError::bad_request(codes::INVALID_JSON, "request body is not valid JSON")
}

fn classify(err: serde_path_to_error::Error<serde_json::Error>) -> ApiError {
    let inner = err.inner();
    if inner.classify() != Category::Data {
        return invalid_json();
    }
    // serde's derive phrases these two as "unknown field `x`, expected …" and
    // "missing field `x`"; neither carries the name in the path.
    let message = inner.to_string();
    if let Some(field) = backticked_after(&message, "unknown field `") {
        return ApiError::unprocessable(
            codes::UNEXPECTED_FIELD,
            format!("unexpected field `{field}`"),
        )
        .with_detail("field", field);
    }
    if let Some(field) = backticked_after(&message, "missing field `") {
        return ApiError::invalid_field(field, format!("missing field `{field}`"));
    }
    let path = err.path().to_string();
    if path == "." {
        return ApiError::unprocessable(codes::INVALID_FIELD, "request body must be a JSON object");
    }
    ApiError::invalid_field(&path, format!("invalid value for field `{path}`"))
}

/// The text between `prefix` and the next backtick, if `message` starts so.
fn backticked_after<'a>(message: &'a str, prefix: &str) -> Option<&'a str> {
    let rest = message.strip_prefix(prefix)?;
    rest.split_once('`').map(|(name, _)| name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)] // Fields exist to be deserialized.
    struct Sample {
        joined: bool,
        #[serde(default)]
        note: Option<String>,
    }

    fn detail_field(err: &ApiError) -> Option<&str> {
        err.details().get("field").and_then(|value| value.as_str())
    }

    fn parse_sample(body: &str) -> Result<Sample, ApiError> {
        parse::<Sample>(body.as_bytes())
    }

    #[test]
    fn accepts_a_valid_object() {
        let sample = parse_sample(r#"{"joined": true}"#).unwrap();
        assert!(sample.joined);
    }

    #[test]
    fn syntax_errors_are_invalid_json() {
        for body in ["", "{", "{\"joined\": tru}", "{\"joined\": true} x"] {
            let err = parse_sample(body).unwrap_err();
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{body:?}");
            assert_eq!(err.code(), codes::INVALID_JSON, "{body:?}");
        }
    }

    #[test]
    fn unknown_field_is_unexpected_field() {
        let err = parse_sample(r#"{"joined": true, "sender_id": "x"}"#).unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code(), codes::UNEXPECTED_FIELD);
        assert_eq!(detail_field(&err), Some("sender_id"));
    }

    #[test]
    fn missing_and_mistyped_fields_are_invalid_field() {
        let missing = parse_sample("{}").unwrap_err();
        assert_eq!(missing.code(), codes::INVALID_FIELD);
        assert_eq!(detail_field(&missing), Some("joined"));

        let mistyped = parse_sample(r#"{"joined": "yes"}"#).unwrap_err();
        assert_eq!(mistyped.code(), codes::INVALID_FIELD);
        assert_eq!(detail_field(&mistyped), Some("joined"));
    }

    #[test]
    fn non_object_body_is_invalid_field_without_name() {
        let err = parse_sample("[]").unwrap_err();
        assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(err.code(), codes::INVALID_FIELD);
        assert_eq!(detail_field(&err), None);
    }

    #[test]
    fn content_type_must_be_json() {
        let with = |value: &'static str| {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, HeaderValue::from_static(value));
            is_json_content_type(&headers)
        };
        assert!(with("application/json"));
        assert!(with("Application/JSON; charset=utf-8"));
        assert!(!with("text/plain"));
        assert!(!with("application/jsonx"));
        assert!(!is_json_content_type(&HeaderMap::new()));
    }
}
