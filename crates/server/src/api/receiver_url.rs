//! `receiver_url` validation: an absolute `http`/`https` URL with a host,
//! without userinfo or fragment, at most [`MAX_RECEIVER_URL_LEN`] bytes.
//! Anything else is 422 `invalid_receiver_url`.

use platform_wire::codes;
use url::Url;

use super::ApiError;

pub const MAX_RECEIVER_URL_LEN: usize = 2048;

const FIELD: &str = "receiver_url";

/// Validates `raw` and returns its normalized serialization (lower-case
/// scheme and host, explicit path), which is what gets stored and compared.
pub fn normalize(raw: &str) -> Result<String, ApiError> {
    if raw.len() > MAX_RECEIVER_URL_LEN {
        return Err(too_long());
    }
    let url = Url::parse(raw).map_err(|_| invalid("receiver_url is not an absolute URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid("receiver_url scheme must be http or https"));
    }
    if url.host().is_none() {
        return Err(invalid("receiver_url must have a host"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid("receiver_url must not contain userinfo"));
    }
    if url.fragment().is_some() {
        return Err(invalid("receiver_url must not contain a fragment"));
    }
    let normalized = String::from(url);
    // Normalization can percent-encode and grow the string.
    if normalized.len() > MAX_RECEIVER_URL_LEN {
        return Err(too_long());
    }
    Ok(normalized)
}

fn invalid(message: &str) -> ApiError {
    ApiError::unprocessable(codes::INVALID_RECEIVER_URL, message).with_detail("field", FIELD)
}

fn too_long() -> ApiError {
    invalid(&format!(
        "receiver_url must be at most {MAX_RECEIVER_URL_LEN} bytes"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https_with_host() {
        for raw in [
            "http://100.64.0.10:8080/notify",
            "https://receiver.example/notify?agent=a",
            "http://[::1]:9000/",
        ] {
            assert!(normalize(raw).is_ok(), "{raw}");
        }
    }

    #[test]
    fn normalizes_case_and_empty_path() {
        assert_eq!(
            normalize("HTTP://Receiver.Example").unwrap(),
            "http://receiver.example/"
        );
    }

    #[test]
    fn rejects_other_schemes_userinfo_fragment_and_relative() {
        for raw in [
            "ftp://receiver.example/notify",
            "file:///etc/passwd",
            "mailto:someone@example.com",
            "http://user:pass@receiver.example/",
            "http://user@receiver.example/",
            "http://receiver.example/notify#frag",
            "http://receiver.example/notify#",
            "/notify",
            "",
            "not a url",
        ] {
            let err = normalize(raw).unwrap_err();
            assert_eq!(err.code(), codes::INVALID_RECEIVER_URL, "{raw}");
        }
    }

    #[test]
    fn enforces_length_limit() {
        let base = "http://receiver.example/";
        let at_limit = format!("{base}{}", "a".repeat(MAX_RECEIVER_URL_LEN - base.len()));
        assert!(normalize(&at_limit).is_ok());
        let over = format!("{at_limit}a");
        assert_eq!(
            normalize(&over).unwrap_err().code(),
            codes::INVALID_RECEIVER_URL
        );
    }
}
