//! `POST /v1/identity/tokens` (IDENTITY_ATTESTATION §4, §13): the check
//! order, every error status, and that an issued token is a JWT signed by
//! the configured key with the expected claims.

mod common;

use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use common::{Api, Fixture, OWNER_TOKEN, Reply, RunningServer};
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use platformd::config::Config;
use reqwest::Method;
use serde_json::{Value, json};

const PATH: &str = "/v1/identity/tokens";

/// Test policy values; the endpoint must apply whatever is configured.
const ISSUER: &str = "test-hub";
const DEFAULT_TTL: u64 = 120;
const MAX_TTL: u64 = 300;

fn identity_toml() -> String {
    format!(
        "issuer = \"{ISSUER}\"\n\
         audiences = [\"garage-docs\", \"other-docs\"]\n\
         default_ttl_secs = {DEFAULT_TTL}\n\
         max_ttl_secs = {MAX_TTL}\n"
    )
}

struct Enabled {
    _fx: Fixture,
    config: Config,
    server: RunningServer,
    api: Api,
}

async fn enabled() -> Enabled {
    let fx = Fixture::new();
    let config = fx.load_config_with_identity(&identity_toml());
    let server = RunningServer::start(&config).await;
    let api = server.api();
    Enabled {
        _fx: fx,
        config,
        server,
        api,
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn decode_part(part: &str) -> Value {
    let bytes = URL_SAFE_NO_PAD
        .decode(part)
        .expect("base64url without padding");
    serde_json::from_slice(&bytes).expect("JSON object")
}

/// Splits, checks the signature against `public_key`, and returns header
/// and claims.
fn verify(token: &str, public_key: &[u8; 32]) -> (Value, Value) {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "{token}");
    let signature: [u8; 64] = URL_SAFE_NO_PAD
        .decode(parts[2])
        .unwrap()
        .try_into()
        .expect("64-byte signature");
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    VerifyingKey::from_bytes(public_key)
        .unwrap()
        .verify(signing_input.as_bytes(), &Signature::from_bytes(&signature))
        .expect("signature verifies with the configured public key");
    (decode_part(parts[0]), decode_part(parts[1]))
}

async fn request(api: &Api, token: &str, body: Value) -> Reply {
    api.post(PATH, token, body).await
}

fn assert_field(reply: &Reply, code: &str, field: &str) {
    reply.assert_error(422, code);
    assert_eq!(reply.error_field(), Some(field), "{}", reply.body);
}

#[tokio::test]
async fn issues_a_signed_token_with_the_default_ttl() {
    let ctx = enabled().await;
    let agent = ctx.api.register("agent-a", None).await;

    let before = now_secs();
    let reply = request(&ctx.api, &agent.api_key, json!({"aud": "garage-docs"})).await;
    let after = now_secs();

    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    let body = reply.body.as_object().unwrap();
    let mut keys: Vec<&str> = body.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["aud", "sub", "token"], "exactly three fields");
    assert_eq!(body["sub"], "agent-a");
    assert_eq!(body["aud"], "garage-docs");

    let identity = ctx.config.identity.as_ref().unwrap();
    let token = body["token"].as_str().unwrap();
    let (header, claims) = verify(token, &identity.signing.public_key());
    assert_eq!(
        header,
        json!({"alg": "EdDSA", "typ": "JWT", "kid": identity.signing.kid()})
    );
    let mut claim_names: Vec<&str> = claims
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    claim_names.sort_unstable();
    assert_eq!(claim_names, ["aud", "exp", "iat", "iss", "jti", "sub"]);
    assert_eq!(claims["iss"], ISSUER);
    assert_eq!(claims["sub"], "agent-a");
    assert_eq!(claims["aud"], "garage-docs");
    let iat = claims["iat"].as_u64().expect("integer iat");
    let exp = claims["exp"].as_u64().expect("integer exp");
    assert!(
        (before..=after).contains(&iat),
        "{before} <= {iat} <= {after}"
    );
    assert_eq!(exp - iat, DEFAULT_TTL, "configured default applies");
    let jti = claims["jti"].as_str().unwrap();
    assert!(
        jti.parse::<ulid::Ulid>().is_ok() && jti.len() == 26,
        "{jti}"
    );
    assert_eq!(
        STANDARD
            .decode(identity.signing.public_key_base64())
            .unwrap(),
        identity.signing.public_key()
    );
    ctx.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn explicit_ttl_is_applied_and_each_call_is_a_new_token() {
    let ctx = enabled().await;
    let agent = ctx.api.register("agent-a", None).await;
    let public_key = ctx.config.identity.as_ref().unwrap().signing.public_key();

    let mut jtis = Vec::new();
    for ttl in [1, MAX_TTL] {
        let reply = request(
            &ctx.api,
            &agent.api_key,
            json!({"aud": "other-docs", "ttl_secs": ttl}),
        )
        .await;
        assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
        let (_, claims) = verify(reply.body["token"].as_str().unwrap(), &public_key);
        assert_eq!(claims["aud"], "other-docs");
        assert_eq!(
            claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
            ttl
        );
        jtis.push(claims["jti"].as_str().unwrap().to_owned());
    }
    assert_ne!(jtis[0], jtis[1]);
    ctx.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unauthenticated_calls_are_401() {
    let ctx = enabled().await;
    let agent = ctx.api.register("agent-a", None).await;
    let body = Some(json!({"aud": "garage-docs"}));
    ctx.api
        .send(Method::POST, PATH, None, body.clone())
        .await
        .assert_error(401, "unauthorized");
    for token in ["pak_not_a_key", agent.receiver_secret.as_str()] {
        ctx.api
            .send(Method::POST, PATH, Some(token), body.clone())
            .await
            .assert_error(401, "unauthorized");
    }
    ctx.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_is_403_owner_not_allowed_before_the_body() {
    let ctx = enabled().await;
    request(&ctx.api, OWNER_TOKEN, json!({"aud": "garage-docs"}))
        .await
        .assert_error(403, "owner_not_allowed");
    ctx.api
        .send_raw(
            Method::POST,
            PATH,
            Some(OWNER_TOKEN),
            Some("text/plain"),
            Some(b"x".to_vec()),
        )
        .await
        .assert_error(403, "owner_not_allowed");
    ctx.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn disabled_hub_is_503_after_auth_and_before_the_body() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    let agent = api.register("agent-a", None).await;

    api.send(
        Method::POST,
        PATH,
        None,
        Some(json!({"aud": "garage-docs"})),
    )
    .await
    .assert_error(401, "unauthorized");
    request(&api, OWNER_TOKEN, json!({"aud": "garage-docs"}))
        .await
        .assert_error(403, "owner_not_allowed");
    request(&api, &agent.api_key, json!({"aud": "garage-docs"}))
        .await
        .assert_error(503, "identity_disabled");
    request(&api, &agent.api_key, json!({"sub": "x"}))
        .await
        .assert_error(503, "identity_disabled");

    // The rest of the API is unaffected.
    let sent = api
        .send_message(&agent.api_key, "all", "still works", "k1")
        .await;
    assert_eq!(sent.status.as_u16(), 201, "{}", sent.body);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn body_errors_use_the_common_wire() {
    let ctx = enabled().await;
    let agent = ctx.api.register("agent-a", None).await;
    let key = agent.api_key.as_str();

    ctx.api
        .send_raw(Method::POST, PATH, Some(key), None, Some(b"{}".to_vec()))
        .await
        .assert_error(415, "unsupported_media_type");
    ctx.api
        .send_raw(
            Method::POST,
            PATH,
            Some(key),
            Some("application/json"),
            Some(b"{\"aud\":".to_vec()),
        )
        .await
        .assert_error(400, "invalid_json");
    let oversized = format!(
        "{{\"aud\":\"{}\"}}",
        "a".repeat(platformd::api::MAX_WIRE_BODY_BYTES)
    );
    ctx.api
        .send_raw(
            Method::POST,
            PATH,
            Some(key),
            Some("application/json"),
            Some(oversized.into_bytes()),
        )
        .await
        .assert_error(413, "request_too_large");

    let unexpected = request(
        &ctx.api,
        key,
        json!({"aud": "garage-docs", "sub": "agent-b"}),
    )
    .await;
    assert_field(&unexpected, "unexpected_field", "sub");

    for body in [
        json!({}),
        json!({"aud": ""}),
        json!({"aud": 1}),
        json!({"aud": null}),
        json!({"aud": ["garage-docs"]}),
    ] {
        let reply = request(&ctx.api, key, body.clone()).await;
        assert_field(&reply, "invalid_field", "aud");
    }
    for ttl in [json!(0), json!(-1), json!(1.5), json!("60"), json!(null)] {
        let reply = request(
            &ctx.api,
            key,
            json!({"aud": "garage-docs", "ttl_secs": ttl}),
        )
        .await;
        assert_field(&reply, "invalid_field", "ttl_secs");
    }
    ctx.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn policy_errors_come_after_format_checks() {
    let ctx = enabled().await;
    let agent = ctx.api.register("agent-a", None).await;
    let key = agent.api_key.as_str();

    // Byte-exact membership: no trimming, no case folding.
    for aud in [
        "unknown-docs",
        "Garage-docs",
        " garage-docs",
        "garage-docs ",
    ] {
        let unknown = request(&ctx.api, key, json!({"aud": aud})).await;
        assert_field(&unknown, "unknown_audience", "aud");
    }
    // aud is judged before ttl_secs.
    let both = request(
        &ctx.api,
        key,
        json!({"aud": "unknown-docs", "ttl_secs": MAX_TTL + 1}),
    )
    .await;
    assert_field(&both, "unknown_audience", "aud");
    // A format error wins over a policy error.
    let format_first = request(&ctx.api, key, json!({"aud": "unknown-docs", "ttl_secs": 0})).await;
    assert_field(&format_first, "invalid_field", "ttl_secs");

    let too_long = request(
        &ctx.api,
        key,
        json!({"aud": "garage-docs", "ttl_secs": MAX_TTL + 1}),
    )
    .await;
    assert_field(&too_long, "ttl_too_long", "ttl_secs");
    assert_eq!(
        too_long.body["error"]["details"],
        json!({"field": "ttl_secs", "max_ttl_secs": MAX_TTL})
    );
    ctx.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn token_is_not_a_hub_credential_and_rotation_stops_issuance() {
    let ctx = enabled().await;
    let agent = ctx.api.register("agent-a", None).await;
    let public_key = ctx.config.identity.as_ref().unwrap().signing.public_key();
    let reply = request(&ctx.api, &agent.api_key, json!({"aud": "garage-docs"})).await;
    let token = reply.body["token"].as_str().unwrap().to_owned();

    ctx.api
        .get("/v1/inbox", &token)
        .await
        .assert_error(401, "unauthorized");
    request(&ctx.api, &token, json!({"aud": "garage-docs"}))
        .await
        .assert_error(401, "unauthorized");

    let rotated = ctx.api.rotate("agent-a", OWNER_TOKEN).await;
    assert_eq!(rotated.status.as_u16(), 200, "{}", rotated.body);
    request(&ctx.api, &agent.api_key, json!({"aud": "garage-docs"}))
        .await
        .assert_error(401, "unauthorized");
    // The token issued before rotation still verifies.
    verify(&token, &public_key);
    let new_key = rotated.body["agent_api_key"].as_str().unwrap();
    let again = request(&ctx.api, new_key, json!({"aud": "garage-docs"})).await;
    assert_eq!(again.status.as_u16(), 201, "{}", again.body);
    ctx.server.shutdown().await.unwrap();
}
