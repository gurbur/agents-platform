//! Identity attestation (IDENTITY_ATTESTATION, ADR-0005): the hub signs
//! short-lived Ed25519 JWTs that attest an agent's identity to third
//! parties, which verify them offline with the hub's public key.
//!
//! The signing key is a raw 32-byte Ed25519 seed read once at startup. The
//! seed never leaves this module: `Debug` prints only the `kid`, and nothing
//! here logs. Tokens are not stored and are never logged.

pub mod jws;
pub mod keygen;

use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::SigningKey;
use platform_wire::AgentId;
use sha2::{Digest, Sha256};
use ulid::Ulid;

pub use jws::Claims;

/// Size of the signing key file: the raw Ed25519 seed, nothing else.
pub const SEED_LEN: usize = 32;

/// Bytes of SHA-256(public key) that make up the `kid` (8 hex characters).
const KID_DIGEST_BYTES: usize = 4;

/// An Ed25519 signing key and its `kid`. `Debug` never prints the seed.
#[derive(Clone)]
pub struct SigningIdentity {
    key: SigningKey,
    kid: String,
}

impl SigningIdentity {
    pub fn from_seed(seed: &[u8; SEED_LEN]) -> Self {
        let key = SigningKey::from_bytes(seed);
        let kid = kid_for(&key.verifying_key().to_bytes());
        Self { key, kid }
    }

    /// Lower-case hex of the first 4 bytes of SHA-256(raw public key).
    pub fn kid(&self) -> &str {
        &self.kid
    }

    /// The raw 32-byte public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// The public key in standard, padded base64 (44 characters), the form
    /// operators paste into consumer configuration.
    pub fn public_key_base64(&self) -> String {
        STANDARD.encode(self.public_key())
    }

    fn signing_key(&self) -> &SigningKey {
        &self.key
    }
}

impl fmt::Debug for SigningIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SigningIdentity")
            .field("kid", &self.kid)
            .field("seed", &"<redacted>")
            .finish()
    }
}

/// The `kid` of a raw public key: SHA-256, first 4 bytes, lower-case hex.
pub fn kid_for(public_key: &[u8; 32]) -> String {
    let digest = Sha256::digest(public_key);
    digest[..KID_DIGEST_BYTES].iter().fold(
        String::with_capacity(KID_DIGEST_BYTES * 2),
        |mut hex, byte| {
            // Writing into a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        },
    )
}

#[derive(Debug, thiserror::Error)]
pub enum KeyFileError {
    #[error("cannot read identity signing key file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "identity signing key file {path} is {actual} bytes; exactly {SEED_LEN} raw seed bytes are required"
    )]
    Length { path: PathBuf, actual: usize },
}

/// Reads the raw 32-byte seed at `path`. Any other length is refused.
pub fn load_signing_key(path: &Path) -> Result<SigningIdentity, KeyFileError> {
    let mut bytes = std::fs::read(path).map_err(|source| KeyFileError::Read {
        path: path.to_owned(),
        source,
    })?;
    let seed: Result<[u8; SEED_LEN], _> = bytes.as_slice().try_into();
    let actual = bytes.len();
    // Best effort: the heap copy of the seed is not kept around.
    bytes.fill(0);
    let seed = seed.map_err(|_| KeyFileError::Length {
        path: path.to_owned(),
        actual,
    })?;
    Ok(SigningIdentity::from_seed(&seed))
}

/// Issuance policy from `[identity]`, validated at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuePolicy {
    pub issuer: String,
    pub audiences: Vec<String>,
    pub default_ttl_secs: u64,
    pub max_ttl_secs: u64,
}

/// Why a well-formed request is refused by policy.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IssueError {
    #[error("aud is not an audience this hub issues tokens for")]
    UnknownAudience,
    #[error("ttl_secs exceeds the maximum of {max_ttl_secs} seconds")]
    TtlTooLong { max_ttl_secs: u64 },
}

/// A freshly signed token and the claims inside it.
#[derive(Clone)]
pub struct IssuedToken {
    pub token: String,
    pub claims: Claims,
}

impl fmt::Debug for IssuedToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedToken")
            .field("token", &"<redacted>")
            .field("claims", &self.claims)
            .finish()
    }
}

/// Signs identity tokens under one key and one policy.
#[derive(Debug, Clone)]
pub struct Issuer {
    signing: SigningIdentity,
    policy: IssuePolicy,
}

impl Issuer {
    pub fn new(signing: SigningIdentity, policy: IssuePolicy) -> Self {
        Self { signing, policy }
    }

    pub fn kid(&self) -> &str {
        self.signing.kid()
    }

    pub fn policy(&self) -> &IssuePolicy {
        &self.policy
    }

    /// Signs a token for `sub` and `aud`, valid `ttl_secs` (or the default)
    /// from `now` truncated to whole seconds. `aud` must be allowed byte for
    /// byte; `ttl_secs` must not exceed the maximum. A pure function of its
    /// inputs, so tests can fix the key, the time and the `jti`.
    pub fn issue(
        &self,
        sub: &AgentId,
        aud: &str,
        ttl_secs: Option<u64>,
        now: SystemTime,
        jti: Ulid,
    ) -> Result<IssuedToken, IssueError> {
        if !self.policy.audiences.iter().any(|allowed| allowed == aud) {
            return Err(IssueError::UnknownAudience);
        }
        let ttl = ttl_secs.unwrap_or(self.policy.default_ttl_secs);
        if ttl > self.policy.max_ttl_secs {
            return Err(IssueError::TtlTooLong {
                max_ttl_secs: self.policy.max_ttl_secs,
            });
        }
        // A host clock before 1970 is a deployment fault; clamp like `clock`.
        let iat = now
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0);
        let claims = Claims {
            iss: self.policy.issuer.clone(),
            sub: sub.to_string(),
            aud: aud.to_owned(),
            iat,
            // `ttl` is at most `max_ttl_secs` (≤ 900), so this cannot overflow
            // for any clock this side of the year 584 billion.
            exp: iat.saturating_add(ttl),
            jti: jti.to_string(),
        };
        let token = jws::encode_jwt(&claims, self.signing.kid(), self.signing.signing_key());
        Ok(IssuedToken { token, claims })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

    const SEED: [u8; SEED_LEN] = [7; SEED_LEN];

    fn policy() -> IssuePolicy {
        IssuePolicy {
            issuer: "test-issuer".into(),
            audiences: vec!["garage-docs".into(), "other".into()],
            default_ttl_secs: 120,
            max_ttl_secs: 300,
        }
    }

    fn issuer() -> Issuer {
        Issuer::new(SigningIdentity::from_seed(&SEED), policy())
    }

    fn agent() -> AgentId {
        "agent-a".parse().unwrap()
    }

    fn at(secs: u64, millis: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs) + Duration::from_millis(millis)
    }

    #[test]
    fn kid_is_the_sha256_prefix_of_the_public_key() {
        let identity = SigningIdentity::from_seed(&SEED);
        let digest = Sha256::digest(identity.public_key());
        let expected: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(identity.kid(), expected);
        assert_eq!(identity.kid().len(), 8);
        assert_eq!(identity.public_key_base64().len(), 44);
        assert!(identity.public_key_base64().ends_with('='));
    }

    #[test]
    fn issues_claims_from_policy_with_truncated_seconds() {
        let jti = Ulid::from_parts(1, 2);
        let issued = issuer()
            .issue(&agent(), "garage-docs", None, at(1_792_129_200, 999), jti)
            .unwrap();
        let claims = &issued.claims;
        assert_eq!(claims.iss, "test-issuer");
        assert_eq!(claims.sub, "agent-a");
        assert_eq!(claims.aud, "garage-docs");
        assert_eq!(claims.iat, 1_792_129_200, "milliseconds are dropped");
        assert_eq!(claims.exp - claims.iat, policy().default_ttl_secs);
        assert_eq!(claims.jti, jti.to_string());
        assert_eq!(claims.jti.len(), 26);
    }

    #[test]
    fn explicit_ttl_up_to_the_maximum_is_applied() {
        let max = policy().max_ttl_secs;
        let issued = issuer()
            .issue(&agent(), "other", Some(max), at(10, 0), Ulid::nil())
            .unwrap();
        assert_eq!(issued.claims.exp - issued.claims.iat, max);
    }

    #[test]
    fn refuses_unknown_audience_byte_for_byte() {
        for aud in ["unknown", "Garage-docs", "garage-docs ", ""] {
            assert_eq!(
                issuer()
                    .issue(&agent(), aud, None, at(10, 0), Ulid::nil())
                    .unwrap_err(),
                IssueError::UnknownAudience,
                "{aud:?}"
            );
        }
    }

    #[test]
    fn refuses_ttl_over_the_maximum() {
        let max = policy().max_ttl_secs;
        assert_eq!(
            issuer()
                .issue(
                    &agent(),
                    "garage-docs",
                    Some(max + 1),
                    at(10, 0),
                    Ulid::nil()
                )
                .unwrap_err(),
            IssueError::TtlTooLong { max_ttl_secs: max }
        );
    }

    #[test]
    fn token_verifies_with_the_public_key() {
        let issuer = issuer();
        let issued = issuer
            .issue(&agent(), "garage-docs", None, at(10, 0), Ulid::nil())
            .unwrap();
        let (signing_input, signature) = issued.token.rsplit_once('.').unwrap();
        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(signature)
            .unwrap()
            .try_into()
            .unwrap();
        let public = SigningIdentity::from_seed(&SEED).public_key();
        VerifyingKey::from_bytes(&public)
            .unwrap()
            .verify(signing_input.as_bytes(), &Signature::from_bytes(&signature))
            .unwrap();
        assert!(!format!("{issued:?}").contains(&issued.token));
    }

    #[test]
    fn loads_exactly_32_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.key");
        std::fs::write(&good, SEED).unwrap();
        assert_eq!(
            load_signing_key(&good).unwrap().kid(),
            SigningIdentity::from_seed(&SEED).kid()
        );
        for len in [0, 31, 33, 44] {
            let path = dir.path().join(format!("{len}.key"));
            std::fs::write(&path, vec![1u8; len]).unwrap();
            assert!(
                matches!(load_signing_key(&path), Err(KeyFileError::Length { actual, .. }) if actual == len),
                "{len}"
            );
        }
        assert!(matches!(
            load_signing_key(&dir.path().join("absent.key")),
            Err(KeyFileError::Read { .. })
        ));
    }

    // Interop fixture (S8 DoD): a token from a fixed key, time and jti. The
    // same values are fed to an external `golang-jwt/jwt/v5` verifier, so
    // any change to the encoding shows up here first. The key is RFC 8037
    // Appendix A.1 `d`, whose public key is A.1 `x`.
    const FIXTURE_SEED_B64URL: &str = "nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A";
    const FIXTURE_IAT: u64 = 1_792_129_200;
    const FIXTURE_TTL: u64 = 600;
    const FIXTURE_JTI: &str = "01JAB3C4D5E6F7G8H9J0KMNPQR";
    const FIXTURE_KID: &str = "21fe31df";
    const FIXTURE_PUBLIC_KEY: &str = "11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=";
    const FIXTURE_TOKEN: &str = "eyJhbGciOiJFZERTQSIsInR5cCI6IkpXVCIsImtpZCI6IjIxZmUzMWRmIn0.\
        eyJpc3MiOiJhZ2VudHMtcGxhdGZvcm0iLCJzdWIiOiJhZ2VudC1hIiwiYXVkIjoiZ2FyYWdlLWRvY3MiLCJpYXQiOjE3OTIxMjkyMDAsImV4cCI6MTc5MjEyOTgwMCwianRpIjoiMDFKQUIzQzRENUU2RjdHOEg5SjBLTU5QUVIifQ.\
        Ms4ITDC8GtMiNoVgY-0V39Tyi_Sy0DkCJG7CkRg5JCfHiNECllpqLFRDKCktK1mwFfO-4nIPSskZNhXKZ1eKCA";

    #[test]
    fn interop_fixture_is_stable() {
        let seed: [u8; SEED_LEN] = URL_SAFE_NO_PAD
            .decode(FIXTURE_SEED_B64URL)
            .unwrap()
            .try_into()
            .unwrap();
        let signing = SigningIdentity::from_seed(&seed);
        let issuer = Issuer::new(
            signing.clone(),
            IssuePolicy {
                issuer: "agents-platform".into(),
                audiences: vec!["garage-docs".into()],
                default_ttl_secs: FIXTURE_TTL,
                max_ttl_secs: FIXTURE_TTL,
            },
        );
        let issued = issuer
            .issue(
                &agent(),
                "garage-docs",
                None,
                at(FIXTURE_IAT, 0),
                FIXTURE_JTI.parse().unwrap(),
            )
            .unwrap();
        assert_eq!(signing.kid(), FIXTURE_KID);
        assert_eq!(signing.public_key_base64(), FIXTURE_PUBLIC_KEY);
        assert_eq!(issued.token, FIXTURE_TOKEN);
    }

    #[test]
    fn debug_never_prints_the_seed() {
        let identity = SigningIdentity::from_seed(&SEED);
        let debug = format!("{identity:?}");
        assert!(debug.contains(identity.kid()));
        assert!(!debug.contains("7, 7"), "{debug}");
    }
}
