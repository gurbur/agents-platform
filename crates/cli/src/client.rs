//! HTTP client for the platformd API. Independent of the server's delivery
//! client (`IF-CLI-001`).

use std::fmt;
use std::time::Duration;

use platform_wire::{ErrorBody, HealthStatus};
use reqwest::{Method, StatusCode, Url};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::CliError;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Bearer credential. `Debug` never prints it.
pub struct BearerToken(String);

impl BearerToken {
    pub fn new(raw: String) -> Result<Self, CliError> {
        let token = raw.trim();
        if token.is_empty() {
            return Err(CliError::Usage("token is empty".into()));
        }
        Ok(Self(token.to_owned()))
    }
}

impl fmt::Debug for BearerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BearerToken(<redacted>)")
    }
}

pub struct ApiClient {
    base: String,
    token: BearerToken,
    http: reqwest::Client,
}

/// Response header carrying the server's request id (`IF-API-COMMON`).
const REQUEST_ID_HEADER: &str = "x-request-id";

/// A decoded response together with its raw body for `--json`.
pub struct Decoded<T> {
    pub value: T,
    pub raw: Vec<u8>,
    /// The success status, e.g. 201 for a new reaction and 200 for one
    /// already there.
    pub status: StatusCode,
    /// The `x-request-id` header, empty when absent.
    pub request_id: String,
}

/// Query parameters in request order. Absent options are simply not pushed.
pub type Query = Vec<(&'static str, String)>;

impl ApiClient {
    pub fn new(base_url: &str, token: BearerToken) -> Result<Self, CliError> {
        let base = normalize_base(base_url)?;
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|err| CliError::Usage(format!("cannot build HTTP client: {err}")))?;
        Ok(Self { base, token, http })
    }

    /// `GET /healthz` (unauthenticated).
    pub async fn health(&self) -> Result<Decoded<HealthStatus>, CliError> {
        let url = self.url("/healthz", &Query::new())?;
        execute(self.http.get(url)).await
    }

    /// Authenticated `GET` with query parameters.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &Query,
    ) -> Result<Decoded<T>, CliError> {
        let url = self.url(path, query)?;
        execute(self.authorized(self.http.get(url))).await
    }

    /// Authenticated request with a JSON body (`Content-Type: application/json`).
    pub async fn send_json<T: DeserializeOwned, B: Serialize>(
        &self,
        method: Method,
        path: &str,
        body: &B,
    ) -> Result<Decoded<T>, CliError> {
        let url = self.url(path, &Query::new())?;
        execute(self.authorized(self.http.request(method, url)).json(body)).await
    }

    /// Authenticated `POST` without a body (rotation, delivery retry).
    pub async fn post_empty<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Decoded<T>, CliError> {
        self.send_empty(Method::POST, path).await
    }

    /// Authenticated request without a body (room invite and archive).
    pub async fn send_empty<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
    ) -> Result<Decoded<T>, CliError> {
        let url = self.url(path, &Query::new())?;
        execute(self.authorized(self.http.request(method, url))).await
    }

    /// Authenticated `PUT` with a raw (non-JSON) body, e.g. an avatar image.
    pub async fn put_bytes<T: DeserializeOwned>(
        &self,
        path: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Decoded<T>, CliError> {
        let url = self.url(path, &Query::new())?;
        let request = self
            .authorized(self.http.put(url))
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
        execute(request).await
    }

    /// Authenticated `GET` of a non-JSON body, kept as bytes with the
    /// response headers.
    pub async fn get_bytes(&self, path: &str) -> Result<RawResponse, CliError> {
        let url = self.url(path, &Query::new())?;
        execute_raw(self.authorized(self.http.get(url))).await
    }

    /// Authenticated `DELETE` without a body; a success has no body either.
    pub async fn delete(&self, path: &str) -> Result<RawResponse, CliError> {
        let url = self.url(path, &Query::new())?;
        execute_raw(self.authorized(self.http.delete(url))).await
    }

    /// Authenticated `DELETE` with a JSON body (the owner's identity of an
    /// `unreact`); a success has no body.
    pub async fn delete_json<B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<RawResponse, CliError> {
        let url = self.url(path, &Query::new())?;
        execute_raw(self.authorized(self.http.delete(url)).json(body)).await
    }

    /// Attaches the bearer credential to an authenticated `/v1` request.
    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token.0)
    }

    fn url(&self, path: &str, query: &Query) -> Result<Url, CliError> {
        let mut url = Url::parse(&format!("{}{path}", self.base))
            .map_err(|err| CliError::Usage(format!("invalid URL: {err}")))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        Ok(url)
    }
}

/// Validates the base URL and strips trailing slashes so paths can be appended.
fn normalize_base(base_url: &str) -> Result<String, CliError> {
    let trimmed = base_url.trim().trim_end_matches('/');
    let parsed = Url::parse(trimmed)
        .map_err(|err| CliError::Usage(format!("invalid --url '{trimmed}': {err}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(CliError::Usage(format!(
            "invalid --url '{trimmed}': scheme must be http or https"
        )));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(CliError::Usage(format!(
            "invalid --url '{trimmed}': must not contain a query or fragment"
        )));
    }
    Ok(trimmed.to_owned())
}

/// A successful response whose body is not JSON.
pub struct RawResponse {
    pub status: StatusCode,
    pub headers: reqwest::header::HeaderMap,
    pub body: Vec<u8>,
}

impl RawResponse {
    /// A header as text, empty when absent or not visible ASCII.
    pub fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
    }
}

async fn execute<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
) -> Result<Decoded<T>, CliError> {
    let RawResponse {
        status,
        headers,
        body: raw,
    } = execute_raw(request).await?;
    let request_id = headers
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let value = serde_json::from_slice(&raw).map_err(|err| CliError::UnexpectedResponse {
        status,
        detail: format!("cannot decode body: {err}"),
    })?;
    Ok(Decoded {
        value,
        raw,
        status,
        request_id,
    })
}

/// Sends `request`; a non-2xx status becomes the server's error.
async fn execute_raw(request: reqwest::RequestBuilder) -> Result<RawResponse, CliError> {
    let response = request.send().await.map_err(CliError::Network)?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(CliError::Network)?.to_vec();
    if !status.is_success() {
        return Err(server_error(status, &body));
    }
    Ok(RawResponse {
        status,
        headers,
        body,
    })
}

fn server_error(status: StatusCode, raw: &[u8]) -> CliError {
    match serde_json::from_slice::<ErrorBody>(raw) {
        Ok(body) => CliError::Server {
            status,
            code: body.error.code,
            message: body.error.message,
            request_id: body.error.request_id,
        },
        Err(_) => CliError::UnexpectedResponse {
            status,
            detail: "response has no error body".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_trailing_slashes_are_removed() {
        assert_eq!(
            normalize_base("http://127.0.0.1:7340/").unwrap(),
            "http://127.0.0.1:7340"
        );
        assert_eq!(
            normalize_base("https://host/prefix//").unwrap(),
            "https://host/prefix"
        );
    }

    #[test]
    fn base_url_must_be_http() {
        for invalid in [
            "ftp://host",
            "not a url",
            "http://host/?q=1",
            "127.0.0.1:7340",
        ] {
            assert!(normalize_base(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn error_body_maps_to_server_error() {
        let raw = r#"{"error":{"code":"unavailable","message":"down","request_id":"req-1","details":{}}}"#;
        let err = server_error(StatusCode::SERVICE_UNAVAILABLE, raw.as_bytes());
        assert!(matches!(err, CliError::Server { ref code, .. } if code == "unavailable"));
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn query_is_form_encoded_and_absent_when_empty() {
        let client = ApiClient::new("http://host/", BearerToken::new("t".into()).unwrap()).unwrap();
        let plain = client.url("/v1/inbox", &Query::new()).unwrap();
        assert_eq!(plain.as_str(), "http://host/v1/inbox");
        let query = vec![("room_id", "dm:a:b".to_owned()), ("q", "a b&c+".to_owned())];
        let url = client.url("/v1/messages", &query).unwrap();
        assert_eq!(url.query(), Some("room_id=dm%3Aa%3Ab&q=a+b%26c%2B"));
    }

    #[test]
    fn token_debug_is_redacted() {
        let token = BearerToken::new("secret-value".into()).unwrap();
        assert!(!format!("{token:?}").contains("secret-value"));
    }
}
