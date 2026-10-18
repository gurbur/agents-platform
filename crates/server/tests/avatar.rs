//! S11: directory avatars — `PUT`, `GET` and `DELETE /v1/agents/{id}/avatar`
//! and the `avatar` summary in `GET /v1/agents` (acceptance scenario 15,
//! `IF-WIRE-008`).

mod common;

use std::time::Duration;

use common::{Api, Fixture, OWNER_TOKEN, RawReply, RunningServer};
use platform_wire::AVATAR_MAX_BYTES;
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0];

/// A PNG-looking body of `len` bytes; `seed` varies the content. The hub
/// only checks the magic, so the rest need not be a real image.
fn png(len: usize, seed: u8) -> Vec<u8> {
    image(PNG_MAGIC, len, seed)
}

fn jpeg(len: usize, seed: u8) -> Vec<u8> {
    image(JPEG_MAGIC, len, seed)
}

fn image(magic: &[u8], len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    bytes.extend((0..len.saturating_sub(magic.len())).map(|index| seed ^ (index % 251) as u8));
    bytes
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn avatar_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/avatar")
}

async fn put_avatar(
    api: &Api,
    agent_id: &str,
    token: Option<&str>,
    content_type: Option<&str>,
    bytes: Vec<u8>,
) -> RawReply {
    let headers: Vec<(&str, &str)> = content_type
        .map(|value| vec![("content-type", value)])
        .unwrap_or_default();
    api.send_bytes(
        Method::PUT,
        &avatar_path(agent_id),
        token,
        &headers,
        Some(bytes),
    )
    .await
}

async fn get_avatar(api: &Api, agent_id: &str, token: &str, headers: &[(&str, &str)]) -> RawReply {
    api.send_bytes(
        Method::GET,
        &avatar_path(agent_id),
        Some(token),
        headers,
        None,
    )
    .await
}

async fn delete_avatar(api: &Api, agent_id: &str, token: Option<&str>) -> RawReply {
    api.send_bytes(Method::DELETE, &avatar_path(agent_id), token, &[], None)
        .await
}

/// The directory item of `agent_id` as `token` sees it.
async fn directory_item(api: &Api, token: &str, agent_id: &str) -> Value {
    let reply = api.get("/v1/agents", token).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.body["items"]
        .as_array()
        .expect("items")
        .iter()
        .find(|item| item["agent_id"] == agent_id)
        .cloned()
        .unwrap_or_else(|| panic!("{agent_id} not in the directory"))
}

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    (fx, server, api)
}

/// Scenario 15: upload → listed → fetched with ETag → 304 → delete → 404.
#[tokio::test]
async fn an_agent_uploads_its_avatar_and_everyone_reads_it() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let bytes = png(4096, 7);
    let sha = sha256_hex(&bytes);

    let put = put_avatar(
        &api,
        "agent-a",
        Some(&a.api_key),
        Some("image/png"),
        bytes.clone(),
    )
    .await;
    assert_eq!(put.status, 200);
    let body = put.json();
    assert_eq!(body["agent_id"], "agent-a");
    let avatar = &body["avatar"];
    assert_eq!(avatar["content_type"], "image/png");
    assert_eq!(avatar["bytes"], bytes.len());
    assert_eq!(avatar["sha256"], sha);
    let updated_at = avatar["updated_at"]
        .as_str()
        .expect("updated_at")
        .to_owned();
    assert_eq!(
        avatar.as_object().unwrap().len(),
        4,
        "exactly four keys: {avatar}"
    );

    for token in [b.api_key.as_str(), OWNER_TOKEN] {
        let item = directory_item(&api, token, "agent-a").await;
        assert_eq!(
            item["avatar"],
            json!({"sha256": sha, "updated_at": updated_at})
        );
    }

    let etag = format!("\"{sha}\"");
    for token in [b.api_key.as_str(), OWNER_TOKEN, a.api_key.as_str()] {
        let got = get_avatar(&api, "agent-a", token, &[]).await;
        assert_eq!(got.status, 200);
        assert_eq!(got.bytes, bytes);
        assert_eq!(got.header("content-type"), Some("image/png"));
        assert_eq!(got.header("etag"), Some(etag.as_str()));
        assert_eq!(got.header("cache-control"), Some("private, max-age=300"));
        assert_eq!(got.header("x-content-type-options"), Some("nosniff"));
    }

    let weak = format!("W/{etag}");
    let listed = format!("\"other\", {etag}");
    for matching in [etag.as_str(), weak.as_str(), listed.as_str(), "*"] {
        let cached = get_avatar(&api, "agent-a", &b.api_key, &[("if-none-match", matching)]).await;
        assert_eq!(cached.status, 304, "If-None-Match: {matching}");
        assert!(cached.bytes.is_empty());
        assert_eq!(cached.header("etag"), Some(etag.as_str()));
        assert_eq!(cached.header("cache-control"), Some("private, max-age=300"));
    }
    let stale = get_avatar(
        &api,
        "agent-a",
        &b.api_key,
        &[("if-none-match", "\"stale\"")],
    )
    .await;
    assert_eq!(stale.status, 200);
    assert_eq!(stale.bytes, bytes);

    let deleted = delete_avatar(&api, "agent-a", Some(&a.api_key)).await;
    assert_eq!(deleted.status, 204);
    assert!(deleted.bytes.is_empty());
    get_avatar(&api, "agent-a", &b.api_key, &[])
        .await
        .assert_error(404, "avatar_not_found");
    assert_eq!(
        directory_item(&api, &b.api_key, "agent-a").await["avatar"],
        Value::Null
    );
    delete_avatar(&api, "agent-a", Some(&a.api_key))
        .await
        .assert_error(404, "avatar_not_found");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn every_directory_item_carries_an_avatar_key() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;

    for token in [a.api_key.as_str(), OWNER_TOKEN] {
        let reply = api.get("/v1/agents", token).await;
        for item in reply.body["items"].as_array().unwrap() {
            assert_eq!(
                item.get("avatar"),
                Some(&Value::Null),
                "no avatar is null: {item}"
            );
        }
    }

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_the_owner_or_the_agent_itself_writes_an_avatar() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;

    // Path permission comes before any body check.
    for content_type in [Some("image/png"), Some("text/plain"), None] {
        put_avatar(&api, "agent-a", Some(&b.api_key), content_type, png(64, 1))
            .await
            .assert_error(403, "forbidden");
    }
    put_avatar(
        &api,
        "Not-A-Slug",
        Some(&b.api_key),
        Some("image/png"),
        png(64, 1),
    )
    .await
    .assert_error(403, "forbidden");
    delete_avatar(&api, "agent-a", Some(&b.api_key))
        .await
        .assert_error(403, "forbidden");
    put_avatar(&api, "agent-a", None, Some("image/png"), png(64, 1))
        .await
        .assert_error(401, "unauthorized");
    delete_avatar(&api, "agent-a", None)
        .await
        .assert_error(401, "unauthorized");
    api.send_bytes(Method::GET, &avatar_path("agent-a"), None, &[], None)
        .await
        .assert_error(401, "unauthorized");

    let owner = put_avatar(
        &api,
        "agent-a",
        Some(OWNER_TOKEN),
        Some("image/png"),
        png(64, 2),
    )
    .await;
    assert_eq!(owner.status, 200);
    let mine = put_avatar(
        &api,
        "agent-a",
        Some(&a.api_key),
        Some("image/png"),
        png(64, 3),
    )
    .await;
    assert_eq!(mine.status, 200);
    let owner_delete = delete_avatar(&api, "agent-a", Some(OWNER_TOKEN)).await;
    assert_eq!(owner_delete.status, 204);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_agents_are_agent_not_found() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    for path_id in ["ghost", "Not-A-Slug"] {
        put_avatar(
            &api,
            path_id,
            Some(OWNER_TOKEN),
            Some("image/png"),
            png(64, 1),
        )
        .await
        .assert_error(404, "agent_not_found");
        get_avatar(&api, path_id, &a.api_key, &[])
            .await
            .assert_error(404, "agent_not_found");
        delete_avatar(&api, path_id, Some(OWNER_TOKEN))
            .await
            .assert_error(404, "agent_not_found");
    }
    // A registered agent without an avatar is a different 404.
    get_avatar(&api, "agent-a", OWNER_TOKEN, &[])
        .await
        .assert_error(404, "avatar_not_found");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_png_and_jpeg_with_matching_bytes_are_accepted() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let key = Some(a.api_key.as_str());

    let rejected: [(Option<&str>, Vec<u8>); 7] = [
        (None, png(64, 1)),
        (Some("image/gif"), b"GIF89a-not-allowed".to_vec()),
        (Some("image/jpg"), jpeg(64, 1)),
        (Some("application/json"), br#"{"avatar": true}"#.to_vec()),
        (Some("image/png"), jpeg(64, 1)),
        (Some("image/jpeg"), png(64, 1)),
        (Some("image/png"), Vec::new()),
    ];
    for (content_type, bytes) in rejected {
        put_avatar(&api, "agent-a", key, content_type, bytes)
            .await
            .assert_error(415, "unsupported_media_type");
    }
    get_avatar(&api, "agent-a", OWNER_TOKEN, &[])
        .await
        .assert_error(404, "avatar_not_found");

    let bytes = jpeg(2048, 9);
    let accepted = put_avatar(
        &api,
        "agent-a",
        key,
        Some("Image/JPEG; charset=binary"),
        bytes.clone(),
    )
    .await;
    assert_eq!(accepted.status, 200);
    assert_eq!(accepted.json()["avatar"]["content_type"], "image/jpeg");
    let got = get_avatar(&api, "agent-a", OWNER_TOKEN, &[]).await;
    assert_eq!(got.header("content-type"), Some("image/jpeg"));
    assert_eq!(got.bytes, bytes);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_avatar_limit_replaces_the_json_body_limits() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let key = Some(a.api_key.as_str());

    // Over the 128 KiB JSON wire limit, under the avatar limit.
    let medium = png(AVATAR_MAX_BYTES / 2, 4);
    assert_eq!(
        put_avatar(&api, "agent-a", key, Some("image/png"), medium)
            .await
            .status,
        200
    );
    let largest = png(AVATAR_MAX_BYTES, 5);
    let put = put_avatar(&api, "agent-a", key, Some("image/png"), largest.clone()).await;
    assert_eq!(put.status, 200);
    assert_eq!(put.json()["avatar"]["bytes"], AVATAR_MAX_BYTES);

    put_avatar(
        &api,
        "agent-a",
        key,
        Some("image/png"),
        png(AVATAR_MAX_BYTES + 1, 6),
    )
    .await
    .assert_error(413, "avatar_too_large");
    // A size rejection is decided before the magic.
    put_avatar(
        &api,
        "agent-a",
        key,
        Some("image/png"),
        vec![0; AVATAR_MAX_BYTES + 1],
    )
    .await
    .assert_error(413, "avatar_too_large");
    let kept = get_avatar(&api, "agent-a", OWNER_TOKEN, &[]).await;
    assert_eq!(kept.bytes, largest, "a rejected upload stores nothing");

    // The JSON endpoints keep their own limit.
    api.send_raw(
        Method::PUT,
        "/v1/agents/agent-a/profile",
        key,
        Some("application/json"),
        Some(vec![b' '; AVATAR_MAX_BYTES / 2]),
    )
    .await
    .assert_error(413, "request_too_large");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn reuploading_the_same_image_keeps_updated_at() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let key = Some(a.api_key.as_str());

    let first = put_avatar(&api, "agent-a", key, Some("image/png"), png(512, 1)).await;
    let first = first.json()["avatar"].clone();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let again = put_avatar(&api, "agent-a", key, Some("image/png"), png(512, 1)).await;
    assert_eq!(again.status, 200);
    assert_eq!(again.json()["avatar"], first, "nothing changed");

    tokio::time::sleep(Duration::from_millis(5)).await;
    let replacement = jpeg(700, 2);
    let changed = put_avatar(
        &api,
        "agent-a",
        key,
        Some("image/jpeg"),
        replacement.clone(),
    )
    .await;
    let changed = changed.json()["avatar"].clone();
    assert_eq!(changed["sha256"], sha256_hex(&replacement));
    assert_eq!(changed["content_type"], "image/jpeg");
    assert_ne!(changed["updated_at"], first["updated_at"]);
    let got = get_avatar(&api, "agent-a", OWNER_TOKEN, &[]).await;
    assert_eq!(got.bytes, replacement);
    assert_eq!(
        directory_item(&api, OWNER_TOKEN, "agent-a").await["avatar"]["sha256"],
        changed["sha256"]
    );

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn avatar_reads_take_no_query_parameters() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    put_avatar(
        &api,
        "agent-a",
        Some(&a.api_key),
        Some("image/png"),
        png(64, 1),
    )
    .await;

    api.send_bytes(
        Method::GET,
        "/v1/agents/agent-a/avatar?size=64",
        Some(OWNER_TOKEN),
        &[],
        None,
    )
    .await
    .assert_error(422, "unexpected_field");

    server.shutdown().await.unwrap();
}
