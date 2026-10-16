//! `platform identity token` through the real binary (IDENTITY_ATTESTATION
//! §10, §13 scenario 9): stdout is the token line only; errors go to stderr
//! with the `IF-CLI-001` exit codes.

mod common;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{OWNER_TOKEN, Platformd, assert_server_error, json, ok, stderr, stdout};

const IDENTITY: &str = "issuer = \"agents-platform\"\naudiences = [\"garage-docs\"]\n\
    default_ttl_secs = 60\nmax_ttl_secs = 120\n";

fn decode_claims(token: &str) -> serde_json::Value {
    let part = token.split('.').nth(1).expect("claims segment");
    serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn prints_only_the_token_line() {
    let server = Platformd::start_with_identity(IDENTITY).await;
    let key = server.register("agent-a").await;

    let output = server
        .cli(&key, &["identity", "token", "--aud", "garage-docs"])
        .await;

    let out = ok(&output);
    assert!(stderr(&output).is_empty(), "{}", stderr(&output));
    let token = out.strip_suffix('\n').expect("one trailing newline");
    assert!(!token.contains('\n'), "{out:?}");
    assert_eq!(token.split('.').count(), 3, "{token}");
    let claims = decode_claims(token);
    assert_eq!(claims["sub"], "agent-a");
    assert_eq!(claims["aud"], "garage-docs");

    let with_ttl = ok(&server
        .cli(
            &key,
            &["identity", "token", "--aud", "garage-docs", "--ttl", "7"],
        )
        .await);
    let claims = decode_claims(with_ttl.trim_end());
    assert_eq!(
        claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
        7
    );
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn json_prints_the_raw_response() {
    let server = Platformd::start_with_identity(IDENTITY).await;
    let key = server.register("agent-a").await;

    let body = json(
        &server
            .cli(
                &key,
                &["--json", "identity", "token", "--aud", "garage-docs"],
            )
            .await,
    );

    let object = body.as_object().unwrap();
    assert_eq!(object.len(), 3, "{body}");
    assert_eq!(body["sub"], "agent-a");
    assert_eq!(body["aud"], "garage-docs");
    assert_eq!(body["token"].as_str().unwrap().split('.').count(), 3);
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn server_errors_go_to_stderr_with_exit_1() {
    let server = Platformd::start_with_identity(IDENTITY).await;
    let key = server.register("agent-a").await;

    let cases = [
        (
            OWNER_TOKEN,
            vec!["--aud", "garage-docs"],
            "owner_not_allowed",
        ),
        (key.as_str(), vec!["--aud", "unknown"], "unknown_audience"),
        (
            key.as_str(),
            vec!["--aud", "garage-docs", "--ttl", "121"],
            "ttl_too_long",
        ),
        (
            key.as_str(),
            vec!["--aud", "Garage-docs"],
            "unknown_audience",
        ),
    ];
    for (token, extra, code) in cases {
        let mut args = vec!["identity", "token"];
        args.extend(extra);
        let output = server.cli(token, &args).await;
        assert_server_error(&output, code);
        assert!(stdout(&output).is_empty(), "{}", stdout(&output));
    }
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabled_hub_and_usage_errors() {
    let server = Platformd::start().await;
    let key = server.register("agent-a").await;

    let disabled = server
        .cli(&key, &["identity", "token", "--aud", "garage-docs"])
        .await;
    assert_server_error(&disabled, "identity_disabled");
    assert!(stdout(&disabled).is_empty());

    for args in [
        vec!["identity", "token"],
        vec!["identity", "token", "--aud", "garage-docs", "--ttl", "-1"],
        vec!["identity", "token", "--aud", "garage-docs", "--ttl", "x"],
        vec!["identity", "token", "--aud", "garage-docs", "--ttl", "0"],
        vec!["identity", "token", "--aud", ""],
    ] {
        let output = server.cli(&key, &args).await;
        assert_eq!(
            output.status.code(),
            Some(2),
            "{args:?}: {}",
            stderr(&output)
        );
        assert!(stdout(&output).is_empty());
    }
    server.stop().await;
}
