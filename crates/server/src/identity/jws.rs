//! Compact JWS (RFC 7515 §7.1) with the `EdDSA` algorithm (RFC 8037), and
//! the identity token's fixed header and claims (IDENTITY_ATTESTATION §5).
//!
//! The hub only signs: it never parses an untrusted JWS, so this module is
//! an encoder and nothing else.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signer as _, SigningKey};
use serde::Serialize;

/// The only `alg` the hub emits.
pub const ALG_EDDSA: &str = "EdDSA";
/// The only `typ` the hub emits.
pub const TYP_JWT: &str = "JWT";

/// Protected header `{"alg":"EdDSA","typ":"JWT","kid":…}`. Field order is
/// the serialization order.
#[derive(Debug, Serialize)]
struct Header<'a> {
    alg: &'static str,
    typ: &'static str,
    kid: &'a str,
}

/// Claims `{"iss","sub","aud","iat","exp","jti"}`; `iat` and `exp` are
/// whole seconds since the Unix epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Claims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub iat: u64,
    pub exp: u64,
    pub jti: String,
}

/// `base64url(header) "." base64url(payload) "." base64url(signature)`, where
/// the signature is Ed25519 over the ASCII text before the second dot. The
/// header and payload bytes are encoded exactly as given.
pub fn compact_jws(header_json: &[u8], payload: &[u8], key: &SigningKey) -> String {
    let mut token = URL_SAFE_NO_PAD.encode(header_json);
    token.push('.');
    URL_SAFE_NO_PAD.encode_string(payload, &mut token);
    let signature = key.sign(token.as_bytes());
    token.push('.');
    URL_SAFE_NO_PAD.encode_string(signature.to_bytes(), &mut token);
    token
}

/// The identity JWT for `claims`, signed by `key` and labelled with `kid`.
pub fn encode_jwt(claims: &Claims, kid: &str, key: &SigningKey) -> String {
    let header = Header {
        alg: ALG_EDDSA,
        typ: TYP_JWT,
        kid,
    };
    // Serializing these structs (strings and integers only) into a `Vec`
    // cannot fail.
    let header_json = serde_json::to_vec(&header).expect("header serializes");
    let claims_json = serde_json::to_vec(claims).expect("claims serialize");
    compact_jws(&header_json, &claims_json, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};

    // RFC 8037 Appendix A.1 key, A.4 input and signature.
    const RFC_D: &str = "nWGxne_9WmC6hEr0kuwsxERJxWl7MmkZcDusAxyuf2A";
    const RFC_X: &str = "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo";
    const RFC_HEADER: &str = r#"{"alg":"EdDSA"}"#;
    const RFC_PAYLOAD: &str = "Example of Ed25519 signing";
    const RFC_JWS: &str = "eyJhbGciOiJFZERTQSJ9.RXhhbXBsZSBvZiBFZDI1NTE5IHNpZ25pbmc.\
        hgyY0il_MGCjP0JzlnLWG1PPOt7-09PGcvMg3AIbQR6dWbhijcNR4ki4iylGjg5BhVsPt9g7sVvpAr_MuM0KAg";

    fn decode32(text: &str) -> [u8; 32] {
        URL_SAFE_NO_PAD
            .decode(text)
            .unwrap()
            .try_into()
            .expect("32 bytes")
    }

    fn rfc_key() -> SigningKey {
        SigningKey::from_bytes(&decode32(RFC_D))
    }

    fn b64_json(part: &str) -> serde_json::Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    #[test]
    fn rfc8037_a4_signature_is_reproduced_exactly() {
        let key = rfc_key();
        assert_eq!(
            key.verifying_key().to_bytes(),
            decode32(RFC_X),
            "A.2 public key"
        );
        let jws = compact_jws(RFC_HEADER.as_bytes(), RFC_PAYLOAD.as_bytes(), &key);
        assert_eq!(jws, RFC_JWS);
    }

    #[test]
    fn identity_jwt_has_the_fixed_header_and_verifies() {
        let key = rfc_key();
        let claims = Claims {
            iss: "agents-platform".into(),
            sub: "agent-a".into(),
            aud: "garage-docs".into(),
            iat: 1_792_129_200,
            exp: 1_792_129_800,
            jti: "01JAB3C4D5E6F7G8H9J0KMNPQR".into(),
        };
        let token = encode_jwt(&claims, "3f9a1c2e", &key);

        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|part| !part.contains('=')), "no padding");
        let header_bytes = URL_SAFE_NO_PAD.decode(parts[0]).unwrap();
        assert_eq!(
            std::str::from_utf8(&header_bytes).unwrap(),
            r#"{"alg":"EdDSA","typ":"JWT","kid":"3f9a1c2e"}"#
        );
        let claims_bytes = URL_SAFE_NO_PAD.decode(parts[1]).unwrap();
        assert_eq!(
            std::str::from_utf8(&claims_bytes).unwrap(),
            r#"{"iss":"agents-platform","sub":"agent-a","aud":"garage-docs","iat":1792129200,"exp":1792129800,"jti":"01JAB3C4D5E6F7G8H9J0KMNPQR"}"#
        );
        assert_eq!(b64_json(parts[1])["exp"], 1_792_129_800);

        let verifying = VerifyingKey::from_bytes(&decode32(RFC_X)).unwrap();
        let signature: [u8; 64] = URL_SAFE_NO_PAD
            .decode(parts[2])
            .unwrap()
            .try_into()
            .unwrap();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        verifying
            .verify(signing_input.as_bytes(), &Signature::from_bytes(&signature))
            .expect("signature verifies with the public key");
        let tampered = format!("{}.{}x", parts[0], parts[1]);
        assert!(
            verifying
                .verify(tampered.as_bytes(), &Signature::from_bytes(&signature))
                .is_err()
        );
    }

    #[test]
    fn encoding_is_deterministic_for_fixed_inputs() {
        let key = rfc_key();
        let claims = Claims {
            iss: "i".into(),
            sub: "s".into(),
            aud: "a".into(),
            iat: 1,
            exp: 2,
            jti: "j".into(),
        };
        assert_eq!(
            encode_jwt(&claims, "k", &key),
            encode_jwt(&claims, "k", &key)
        );
    }
}
