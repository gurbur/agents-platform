//! Credential generation and hashing (`IF-AUTH-001`).
//!
//! Both credentials are a prefix plus 32 CSPRNG bytes in unpadded base64url.
//! Only the SHA-256 hex digest of the API key is stored; the receiver secret
//! is stored as is because outbound callbacks present it.

use std::fmt;
use std::fmt::Write as _;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::Rng as _;
use sha2::{Digest, Sha256};

pub const API_KEY_PREFIX: &str = "pak_";
pub const RECEIVER_SECRET_PREFIX: &str = "prs_";

/// Random bytes behind each credential.
const SECRET_BYTES: usize = 32;

/// A freshly issued credential pair. The plain-text values are returned to
/// the caller exactly once; `Debug` never prints them.
pub struct IssuedCredentials {
    pub agent_api_key: String,
    pub api_key_hash: String,
    pub receiver_secret: String,
}

impl IssuedCredentials {
    pub fn generate() -> Self {
        let agent_api_key = random_token(API_KEY_PREFIX);
        let api_key_hash = hash_api_key(&agent_api_key);
        Self {
            agent_api_key,
            api_key_hash,
            receiver_secret: random_token(RECEIVER_SECRET_PREFIX),
        }
    }
}

impl fmt::Debug for IssuedCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("IssuedCredentials(<redacted>)")
    }
}

/// Lower-case hex SHA-256 of the presented bearer string.
pub fn hash_api_key(token: &str) -> String {
    sha256_hex(token.as_bytes())
}

/// Lower-case hex SHA-256 of `bytes` (the API key hash, the avatar `ETag`).
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest
        .iter()
        .fold(String::with_capacity(digest.len() * 2), |mut hex, byte| {
            // Writing into a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn random_token(prefix: &str) -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    // `ThreadRng` is a CSPRNG seeded from the OS (`rand` 0.10, `thread_rng`).
    rand::rng().fill_bytes(&mut bytes);
    format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unpadded base64url of 32 bytes.
    const ENCODED_LEN: usize = 43;

    fn assert_token_shape(token: &str, prefix: &str) {
        let encoded = token
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("prefix {prefix}"));
        assert_eq!(encoded.len(), ENCODED_LEN);
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(URL_SAFE_NO_PAD.decode(encoded).unwrap().len(), SECRET_BYTES);
    }

    #[test]
    fn generated_credentials_have_prefixes_and_length() {
        let issued = IssuedCredentials::generate();
        assert_token_shape(&issued.agent_api_key, API_KEY_PREFIX);
        assert_token_shape(&issued.receiver_secret, RECEIVER_SECRET_PREFIX);
        assert_eq!(issued.api_key_hash, hash_api_key(&issued.agent_api_key));
    }

    #[test]
    fn generated_credentials_differ() {
        let first = IssuedCredentials::generate();
        let second = IssuedCredentials::generate();
        assert_ne!(first.agent_api_key, second.agent_api_key);
        assert_ne!(first.receiver_secret, second.receiver_secret);
    }

    #[test]
    fn hash_is_sha256_hex() {
        // `printf abc | shasum -a 256`
        assert_eq!(
            hash_api_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn debug_redacts_plain_text() {
        let issued = IssuedCredentials::generate();
        let debug = format!("{issued:?}");
        assert!(!debug.contains(&issued.agent_api_key));
        assert!(!debug.contains(&issued.receiver_secret));
    }
}
