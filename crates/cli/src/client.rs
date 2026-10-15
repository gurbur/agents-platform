//! HTTP client for the platformd API. Independent of the server's delivery
//! client (`IF-CLI-001`).

use std::fmt;
use std::time::Duration;

use platform_wire::{ErrorBody, HealthStatus};
use reqwest::{StatusCode, Url};
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

/// A decoded response together with its raw body for `--json`.
pub struct Decoded<T> {
    pub value: T,
    pub raw: String,
}

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
        let url = self.url("/healthz")?;
        let response = self.http.get(url).send().await.map_err(CliError::Network)?;
        decode(response).await
    }

    /// Attaches the bearer credential to an authenticated `/v1` request.
    #[expect(
        dead_code,
        reason = "S0 only has the unauthenticated health command; /v1 commands arrive in S1+"
    )]
    fn authorized(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.bearer_auth(&self.token.0)
    }

    fn url(&self, path: &str) -> Result<Url, CliError> {
        Url::parse(&format!("{}{path}", self.base))
            .map_err(|err| CliError::Usage(format!("invalid URL: {err}")))
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

async fn decode<T: DeserializeOwned>(response: reqwest::Response) -> Result<Decoded<T>, CliError> {
    let status = response.status();
    let raw = response.text().await.map_err(CliError::Network)?;
    if !status.is_success() {
        return Err(server_error(status, &raw));
    }
    let value = serde_json::from_str(&raw).map_err(|err| CliError::UnexpectedResponse {
        status,
        detail: format!("cannot decode body: {err}"),
    })?;
    Ok(Decoded { value, raw })
}

fn server_error(status: StatusCode, raw: &str) -> CliError {
    match serde_json::from_str::<ErrorBody>(raw) {
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
        let err = server_error(StatusCode::SERVICE_UNAVAILABLE, raw);
        assert!(matches!(err, CliError::Server { ref code, .. } if code == "unavailable"));
        assert_eq!(err.exit_code(), 1);
    }

    #[test]
    fn token_debug_is_redacted() {
        let token = BearerToken::new("secret-value".into()).unwrap();
        assert!(!format!("{token:?}").contains("secret-value"));
    }
}
