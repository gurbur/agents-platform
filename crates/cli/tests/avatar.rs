//! S11 CLI: `profile avatar set|get|clear` through the real binary against
//! an in-process platformd (`IF-CLI-001` S11 additions).

mod common;

use common::{
    OWNER_TOKEN, Platformd, assert_server_error, closed_port_addr, json, ok, platform, stderr,
    stdout, value,
};
use platform_wire::AVATAR_MAX_BYTES;

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];

fn image(magic: &[u8], len: usize) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.resize(len, 0x5A);
    bytes
}

fn write(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> String {
    let path = dir.path().join(name);
    std::fs::write(&path, bytes).unwrap();
    path.display().to_string()
}

fn assert_usage_error(output: &std::process::Output) {
    assert_eq!(output.status.code(), Some(2), "stderr: {}", stderr(output));
    assert!(stdout(output).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn set_get_and_clear_round_trip() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let key_b = server.register("agent-b").await;
    let dir = tempfile::tempdir().unwrap();
    let png = image(PNG_MAGIC, 300);
    let file = write(&dir, "face.bin", &png);

    let set = ok(&server
        .cli(&key_a, &["profile", "avatar", "set", "agent-a", &file])
        .await);
    assert_eq!(value(&set, "agent_id"), "agent-a");
    assert_eq!(value(&set, "content_type"), "image/png");
    assert_eq!(value(&set, "bytes"), "300");
    let sha = value(&set, "sha256");
    assert_eq!(sha.len(), 64);
    assert!(!value(&set, "updated_at").is_empty());

    let out = dir.path().join("copy.png");
    let out_text = out.display().to_string();
    let got = ok(&server
        .cli(
            &key_b,
            &["profile", "avatar", "get", "agent-a", "--out", &out_text],
        )
        .await);
    assert_eq!(std::fs::read(&out).unwrap(), png);
    assert_eq!(value(&got, "content_type"), "image/png");
    assert_eq!(value(&got, "sha256"), sha);
    assert_eq!(value(&got, "bytes"), "300");
    assert_eq!(value(&got, "out"), out_text);

    // An existing --out is refused before any request.
    let again = server
        .cli(
            &key_b,
            &["profile", "avatar", "get", "agent-a", "--out", &out_text],
        )
        .await;
    assert_usage_error(&again);

    let json_out = dir.path().join("copy2.png").display().to_string();
    let as_json = json(
        &server
            .cli(
                OWNER_TOKEN,
                &[
                    "--json", "profile", "avatar", "get", "agent-a", "--out", &json_out,
                ],
            )
            .await,
    );
    assert_eq!(as_json["sha256"], sha);
    assert_eq!(as_json["bytes"], 300);
    assert_eq!(as_json["content_type"], "image/png");

    let cleared = ok(&server
        .cli(&key_a, &["profile", "avatar", "clear", "agent-a"])
        .await);
    assert_eq!(cleared, "avatar cleared: agent-a\n");
    let missing_out = dir.path().join("none.png");
    let missing = server
        .cli(
            &key_b,
            &[
                "profile",
                "avatar",
                "get",
                "agent-a",
                "--out",
                &missing_out.display().to_string(),
            ],
        )
        .await;
    assert_server_error(&missing, "avatar_not_found");
    assert!(!missing_out.exists(), "no file for a failed download");
    let cleared_again = server
        .cli(&key_a, &["--json", "profile", "avatar", "clear", "agent-a"])
        .await;
    assert_server_error(&cleared_again, "avatar_not_found");

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn jpeg_is_detected_and_json_is_the_server_body() {
    let server = Platformd::start().await;
    let key_a = server.register("agent-a").await;
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "face.png", &image(JPEG_MAGIC, 64));

    let body = json(
        &server
            .cli(
                &key_a,
                &["--json", "profile", "avatar", "set", "agent-a", &file],
            )
            .await,
    );
    assert_eq!(body["agent_id"], "agent-a");
    assert_eq!(body["avatar"]["content_type"], "image/jpeg");
    assert_eq!(body["avatar"]["bytes"], 64);

    let cleared = json(
        &server
            .cli(&key_a, &["--json", "profile", "avatar", "clear", "agent-a"])
            .await,
    );
    assert_eq!(
        cleared,
        serde_json::json!({"agent_id": "agent-a", "avatar": null})
    );

    server.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn another_agents_avatar_is_forbidden() {
    let server = Platformd::start().await;
    server.register("agent-a").await;
    let key_b = server.register("agent-b").await;
    let dir = tempfile::tempdir().unwrap();
    let file = write(&dir, "face.png", &image(PNG_MAGIC, 64));

    let set = server
        .cli(&key_b, &["profile", "avatar", "set", "agent-a", &file])
        .await;
    assert_server_error(&set, "forbidden");
    let clear = server
        .cli(&key_b, &["profile", "avatar", "clear", "agent-a"])
        .await;
    assert_server_error(&clear, "forbidden");

    server.stop().await;
}

/// Runs against a port nothing listens on: exit 2 proves the CLI refused
/// before sending anything (a request would exit 3).
#[tokio::test(flavor = "multi_thread")]
async fn files_that_cannot_be_avatars_are_refused_locally() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://{}", closed_port_addr());
    let gif = write(&dir, "face.gif", b"GIF89a....");
    let empty = write(&dir, "empty.png", b"");
    let huge = write(&dir, "huge.png", &image(PNG_MAGIC, AVATAR_MAX_BYTES + 1));
    let missing = dir.path().join("missing.png").display().to_string();
    for file in [&gif, &empty, &huge, &missing] {
        let output = platform(&[
            "--url", &url, "--token", "t", "profile", "avatar", "set", "agent-a", file,
        ])
        .await;
        assert_usage_error(&output);
    }
}
