//! The receiver callback client (`IF-RCV-BODY`, TECH "API와 receiver 계약
//! 유지"): HTTP/1.1 only, no redirects, no compression, explicit
//! `Content-Length`, connect and whole-request timeouts from `[delivery]`.
//! It is separate from any other HTTP client.

use std::time::{Duration, SystemTime};

use platform_wire::WakeNotification;
use platform_wire::delivery::MAX_WAKE_BODY_BYTES;
use reqwest::header::CONTENT_TYPE;
use reqwest::redirect::Policy;
use tracing::error;

use super::classify::{self, ACCEPTED, AttemptOutcome};
use crate::config::DeliveryConfig;
use crate::store::queries::delivery::Attempt;

/// How much of a 202 body is read to decide that it is not empty.
const MAX_RESPONSE_BODY_PROBE: usize = 4 * 1024;

const USER_AGENT: &str = concat!("platformd/", env!("CARGO_PKG_VERSION"));

pub struct CallbackClient {
    http: reqwest::Client,
}

impl CallbackClient {
    pub fn new(config: &DeliveryConfig) -> Result<Self, reqwest::Error> {
        let http = reqwest::Client::builder()
            .http1_only()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_millis(config.connect_timeout_ms))
            .timeout(Duration::from_millis(config.request_timeout_ms))
            // One short request per attempt; a pooled connection the receiver
            // has since closed would only turn into a spurious failure.
            .pool_max_idle_per_host(0)
            .user_agent(USER_AGENT)
            .build()?;
        Ok(Self { http })
    }

    /// POSTs the wake body for `attempt` and classifies the result. Never
    /// follows a redirect; reads at most [`MAX_RESPONSE_BODY_PROBE`] bytes of
    /// a 202 body.
    pub async fn notify(&self, attempt: &Attempt) -> AttemptOutcome {
        let wake = WakeNotification::new(attempt.agent_id.clone(), attempt.posted.posted_seq);
        let body = match serde_json::to_vec(&wake) {
            Ok(body) if body.len() <= MAX_WAKE_BODY_BYTES => body,
            Ok(body) => {
                error!(agent_id = %attempt.agent_id, bytes = body.len(), "wake body exceeds the v1 limit");
                return internal_failure();
            }
            Err(err) => {
                error!(agent_id = %attempt.agent_id, error = %err, "cannot serialize the wake body");
                return internal_failure();
            }
        };

        let sent = self
            .http
            .post(&attempt.receiver_url)
            .bearer_auth(&attempt.receiver_secret)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        let mut response = match sent {
            Ok(response) => response,
            Err(err) => return classify::transport(&err),
        };

        let status = response.status().as_u16();
        if status != ACCEPTED {
            return classify::non_accepted_status(status, response.headers(), SystemTime::now());
        }
        if !classify::content_length_is_zero(response.headers()) {
            return classify::protocol_mismatch();
        }
        let mut body_len = 0;
        while body_len <= MAX_RESPONSE_BODY_PROBE {
            match response.chunk().await {
                Ok(Some(chunk)) => body_len += chunk.len(),
                Ok(None) => break,
                Err(err) => return classify::transport(&err),
            }
        }
        classify::accepted_status(response.headers(), body_len)
    }
}

fn internal_failure() -> AttemptOutcome {
    AttemptOutcome::Blocked {
        error: classify::INTERNAL.to_owned(),
    }
}

/// The receiver host for logs: never the path, query or userinfo.
pub fn log_host(receiver_url: &str) -> String {
    url::Url::parse(receiver_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| "<invalid>".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_host_drops_everything_but_the_host() {
        assert_eq!(
            log_host("https://receiver.test:8443/wake?token=abc"),
            "receiver.test"
        );
        assert_eq!(log_host("http://[::1]:9000/"), "[::1]");
        assert_eq!(log_host("not a url"), "<invalid>");
    }

    #[test]
    fn builds_with_the_default_config() {
        assert!(CallbackClient::new(&DeliveryConfig::default()).is_ok());
    }
}
