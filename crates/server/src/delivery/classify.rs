//! What one callback attempt means for the route (RECEIVER_WAKE_CONVENTION
//! §7, §9, §10; `IF-RCV-RESPONSE`, `IF-RCV-RETRY`).
//!
//! | result | outcome | `last_error` |
//! | --- | --- | --- |
//! | 202 + exactly one `Content-Length: 0` + empty body | accepted | — |
//! | any other 202 (body, chunked, no `Content-Length`) | blocked | `protocol_mismatch` |
//! | 5xx, 429 | transient (+ `Retry-After`) | `http_NNN` |
//! | any other status (3xx, other 2xx, other 4xx) | blocked | `http_NNN` |
//! | connect failure | transient | `connect` |
//! | connect or whole-request timeout | transient | `timeout` |
//! | other transport failure | transient | `network` |

use std::time::{Duration, SystemTime};

use reqwest::header::{CONTENT_LENGTH, HeaderMap, RETRY_AFTER};

pub const PROTOCOL_MISMATCH: &str = "protocol_mismatch";
pub const CONNECT: &str = "connect";
pub const TIMEOUT: &str = "timeout";
pub const NETWORK: &str = "network";
/// A failure on our side (the body could not be built). Never expected.
pub const INTERNAL: &str = "internal";

/// HTTP 202, the only success status.
pub const ACCEPTED: u16 = 202;
const TOO_MANY_REQUESTS: u16 = 429;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptOutcome {
    /// The receiver accepted the wake for the posted `inbox_seq`.
    Accepted,
    /// Retry later with backoff, or after `retry_after` when the receiver
    /// sent a valid `Retry-After`.
    Transient {
        error: String,
        retry_after: Option<Duration>,
    },
    /// Configuration failure: stop until the route changes or a manual retry.
    Blocked { error: String },
}

impl AttemptOutcome {
    /// The `last_error` code, `None` for an accepted attempt.
    pub fn error_code(&self) -> Option<&str> {
        match self {
            AttemptOutcome::Accepted => None,
            AttemptOutcome::Transient { error, .. } | AttemptOutcome::Blocked { error } => {
                Some(error)
            }
        }
    }
}

/// Outcome of a response whose status is not 202.
pub fn non_accepted_status(status: u16, headers: &HeaderMap, now: SystemTime) -> AttemptOutcome {
    let error = format!("http_{status}");
    if status == TOO_MANY_REQUESTS || (500..=599).contains(&status) {
        return AttemptOutcome::Transient {
            error,
            retry_after: retry_after(headers, now),
        };
    }
    AttemptOutcome::Blocked { error }
}

/// Outcome of a 202 once its headers and (bounded) body have been read.
pub fn accepted_status(headers: &HeaderMap, body_len: usize) -> AttemptOutcome {
    if content_length_is_zero(headers) && body_len == 0 {
        return AttemptOutcome::Accepted;
    }
    protocol_mismatch()
}

pub fn protocol_mismatch() -> AttemptOutcome {
    AttemptOutcome::Blocked {
        error: PROTOCOL_MISMATCH.to_owned(),
    }
}

/// Exactly one `Content-Length` header whose value is `0`.
pub fn content_length_is_zero(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(CONTENT_LENGTH).iter();
    let (Some(only), None) = (values.next(), values.next()) else {
        return false;
    };
    only.as_bytes() == b"0"
}

/// A transport failure: no usable response arrived.
pub fn transport(err: &reqwest::Error) -> AttemptOutcome {
    if err.is_builder() {
        return AttemptOutcome::Blocked {
            error: INTERNAL.to_owned(),
        };
    }
    let error = if err.is_timeout() {
        TIMEOUT
    } else if err.is_connect() {
        CONNECT
    } else {
        NETWORK
    };
    AttemptOutcome::Transient {
        error: error.to_owned(),
        retry_after: None,
    }
}

/// `Retry-After` as delay-seconds or an HTTP-date (RFC 9110 §10.2.3). A
/// date in the past is a zero delay; anything unparsable is ignored.
fn retry_after(headers: &HeaderMap, now: SystemTime) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    parse_retry_after(value, now)
}

pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        // All digits: only overflow can fail, and such a delay is capped anyway.
        let seconds = value.parse::<u64>().unwrap_or(u64::MAX);
        return Some(Duration::from_secs(seconds));
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_static(value));
        }
        map
    }

    fn now() -> SystemTime {
        // 2015-10-21T07:28:00Z, the RFC 9110 example date.
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_445_412_480)
    }

    #[test]
    fn only_an_empty_202_with_content_length_zero_is_accepted() {
        assert_eq!(
            accepted_status(&headers(&[("content-length", "0")]), 0),
            AttemptOutcome::Accepted
        );
        for (pairs, body_len) in [
            (&[("content-length", "1")][..], 1),
            (&[][..], 0),
            (&[("transfer-encoding", "chunked")][..], 0),
            (&[("content-length", "00")][..], 0),
            (&[("content-length", "0"), ("content-length", "0")][..], 0),
            (&[("content-length", "0")][..], 3),
        ] {
            assert_eq!(
                accepted_status(&headers(pairs), body_len),
                protocol_mismatch(),
                "{pairs:?} body {body_len}"
            );
        }
    }

    #[test]
    fn server_errors_and_429_are_transient() {
        for status in [500, 502, 503, 599, 429] {
            let outcome = non_accepted_status(status, &HeaderMap::new(), now());
            assert_eq!(
                outcome,
                AttemptOutcome::Transient {
                    error: format!("http_{status}"),
                    retry_after: None
                }
            );
        }
    }

    #[test]
    fn other_statuses_block_the_route() {
        for status in [
            200, 201, 204, 301, 302, 307, 400, 401, 403, 404, 405, 413, 415, 422,
        ] {
            assert_eq!(
                non_accepted_status(status, &HeaderMap::new(), now()),
                AttemptOutcome::Blocked {
                    error: format!("http_{status}")
                },
                "{status}"
            );
        }
    }

    #[test]
    fn retry_after_accepts_seconds_and_http_dates() {
        let at = |value| parse_retry_after(value, now());
        assert_eq!(at("120"), Some(Duration::from_secs(120)));
        assert_eq!(at("0"), Some(Duration::ZERO));
        assert_eq!(
            at("Wed, 21 Oct 2015 07:28:30 GMT"),
            Some(Duration::from_secs(30)),
            "IMF-fixdate"
        );
        assert_eq!(
            at("Wednesday, 21-Oct-15 07:29:00 GMT"),
            Some(Duration::from_secs(60)),
            "obsolete RFC 850 form"
        );
        assert_eq!(
            at("Wed Oct 21 07:28:10 2015"),
            Some(Duration::from_secs(10)),
            "asctime form"
        );
        assert_eq!(
            at("Wed, 21 Oct 2015 07:00:00 GMT"),
            Some(Duration::ZERO),
            "a past date"
        );
        for invalid in ["", "-1", "1.5", "soon", "+5"] {
            assert_eq!(at(invalid), None, "{invalid:?}");
        }
        assert_eq!(
            at("99999999999999999999999"),
            Some(Duration::from_secs(u64::MAX)),
            "overflow saturates; the caller caps it"
        );
    }

    #[test]
    fn retry_after_header_is_read_for_transient_statuses_only() {
        let with = headers(&[("retry-after", "7")]);
        assert_eq!(
            non_accepted_status(503, &with, now()),
            AttemptOutcome::Transient {
                error: "http_503".into(),
                retry_after: Some(Duration::from_secs(7))
            }
        );
        assert_eq!(
            non_accepted_status(404, &with, now()),
            AttemptOutcome::Blocked {
                error: "http_404".into()
            }
        );
    }
}
