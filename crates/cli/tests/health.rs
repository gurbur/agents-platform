//! Runs the real `platform` binary against an in-process platformd.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use common::{BIN, Platformd, closed_port_addr, platform, stderr, stdout};

#[tokio::test(flavor = "multi_thread")]
async fn health_prints_status_and_exits_0() {
    let server = Platformd::start().await;

    let output = platform(&["--url", &server.url, "--token", "x", "health"]).await;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stdout(&output).starts_with("ok "), "{}", stdout(&output));
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn health_json_prints_the_raw_body() {
    let server = Platformd::start().await;

    let output = platform(&["health", "--json", "--url", &server.url, "--token", "x"]).await;

    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let body: serde_json::Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["schema_version"].is_u64());
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn token_file_and_environment_are_accepted() {
    let server = Platformd::start().await;
    let dir = tempfile::tempdir().unwrap();
    let token_file = dir.path().join("t.token");
    std::fs::write(&token_file, "x\n").unwrap();

    let from_file = platform(&[
        "--url",
        &server.url,
        "--token-file",
        token_file.to_str().unwrap(),
        "health",
    ])
    .await;
    assert_eq!(from_file.status.code(), Some(0), "{}", stderr(&from_file));

    let url = server.url.clone();
    let from_env = tokio::task::spawn_blocking(move || {
        Command::new(BIN)
            .arg("health")
            .env("PLATFORM_URL", url)
            .env("PLATFORM_TOKEN", "x")
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(from_env.status.code(), Some(0), "{}", stderr(&from_env));
    server.stop().await;
}

#[tokio::test]
async fn no_server_exits_3() {
    let url = format!("http://{}", closed_port_addr());

    let output = platform(&["--url", &url, "--token", "x", "health"]).await;

    assert_eq!(output.status.code(), Some(3), "{}", stderr(&output));
}

#[tokio::test]
async fn missing_url_or_token_exits_2() {
    let no_url = platform(&["--token", "x", "health"]).await;
    assert_eq!(no_url.status.code(), Some(2), "{}", stderr(&no_url));

    let no_token = platform(&["--url", "http://127.0.0.1:1", "health"]).await;
    assert_eq!(no_token.status.code(), Some(2), "{}", stderr(&no_token));

    let missing_file = platform(&[
        "--url",
        "http://127.0.0.1:1",
        "--token-file",
        Path::new("/nonexistent/token").to_str().unwrap(),
        "health",
    ])
    .await;
    assert_eq!(
        missing_file.status.code(),
        Some(2),
        "{}",
        stderr(&missing_file)
    );

    let bad_url = platform(&["--url", "not a url", "--token", "x", "health"]).await;
    assert_eq!(bad_url.status.code(), Some(2), "{}", stderr(&bad_url));

    let unknown_flag = platform(&["--bogus", "health"]).await;
    assert_eq!(unknown_flag.status.code(), Some(2));
}

#[tokio::test]
async fn server_error_body_exits_1() {
    let body = r#"{"error":{"code":"unavailable","message":"database is not available","request_id":"req-01J00000000000000000000000","details":{}}}"#;
    let addr = serve_one_canned_response(503, body);

    let output = platform(&["--url", &format!("http://{addr}"), "--token", "x", "health"]).await;

    assert_eq!(output.status.code(), Some(1), "{}", stderr(&output));
    assert_eq!(
        stderr(&output),
        "platform: unavailable: database is not available (request req-01J00000000000000000000000)\n"
    );
}

/// A one-shot HTTP/1.1 responder for exercising the CLI's error path.
fn serve_one_canned_response(status: u16, body: &'static str) -> std::net::SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let response = format!(
            "HTTP/1.1 {status} Service Unavailable\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).unwrap();
    });
    addr
}
