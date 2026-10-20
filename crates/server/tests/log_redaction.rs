//! Credentials never reach the logs (`CLAUDE.md` 규약, `IF-AUTH-001`).
//!
//! This binary installs a process-wide subscriber at TRACE so that the DB
//! thread and every task are captured, then drives each S1 and S2 endpoint,
//! including failures that carry credentials in headers and bodies, and one
//! S4 receiver callback. Message bodies, history search text and the
//! receiver URL beyond its host must not be logged either. Identity tokens
//! and the signing seed never reach the logs (IDENTITY_ATTESTATION §11).
//! Directory profile `name`, `role` and `intro` values are free text other
//! agents read and stay out of the logs like message bodies (`IF-WIRE-006`).
//!
//! The subscriber is installed once and shared by every test in this binary;
//! each test asserts only on the secrets it created.

mod common;

use std::io;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

use common::receiver::MockReceiver;
use common::{Fixture, OWNER_TOKEN, RunningServer, eventually};
use reqwest::Method;
use serde_json::json;
use tracing_subscriber::EnvFilter;

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        let bytes = self.0.lock().expect("log buffer lock").clone();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .expect("log buffer lock")
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn install_capture() -> Captured {
    static CAPTURED: OnceLock<Captured> = OnceLock::new();
    CAPTURED
        .get_or_init(|| {
            let captured = Captured::default();
            let writer = captured.clone();
            tracing_subscriber::fmt()
                .with_env_filter(EnvFilter::new("trace"))
                .with_ansi(false)
                .with_writer(move || writer.clone())
                .init();
            captured
        })
        .clone()
}

#[tokio::test]
async fn credentials_do_not_appear_in_logs() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();

    let agent = api
        .register("agent-a", Some("http://receiver.test/notify"))
        .await;
    api.get("/v1/agents", OWNER_TOKEN).await;
    api.get("/v1/agents", &agent.api_key).await;
    api.patch(
        "/v1/agents/agent-a/receiver",
        &agent.api_key,
        json!({"receiver_url": "http://receiver.test/other"}),
    )
    .await;
    api.put(
        "/v1/agents/agent-a/membership/all",
        &agent.api_key,
        json!({"joined": false}),
    )
    .await;
    let rotated = api.rotate("agent-a", OWNER_TOKEN).await;
    api.get("/v1/agents", &agent.api_key)
        .await
        .assert_error(401, "unauthorized");
    api.get("/v1/agents", &agent.receiver_secret)
        .await
        .assert_error(401, "unauthorized");
    api.send(
        Method::POST,
        "/v1/agents",
        Some(OWNER_TOKEN),
        Some(json!({"agent_id": "agent-b", "api_key": "pak_injected_in_body"})),
    )
    .await
    .assert_error(422, "unexpected_field");
    let body_marker = "BODY-MARKER-do-not-log";
    let sender = api.register("agent-c", None).await;
    let peer = api.register("agent-d", None).await;
    for to in ["all", "agent-d"] {
        let reply = api
            .send_message(&sender.api_key, to, body_marker, &format!("key-{to}"))
            .await;
        assert_eq!(reply.status.as_u16(), 201);
    }
    api.send_message(&sender.api_key, "all", body_marker, "key-all")
        .await;
    api.send_message(&sender.api_key, "all", "BODY-MARKER-conflict", "key-all")
        .await
        .assert_error(409, "idempotency_conflict");
    api.get(
        &format!("/v1/messages?room_id=all&q={body_marker}"),
        &peer.api_key,
    )
    .await;

    // S4: a callback to a URL whose path and query must stay out of the logs.
    let mock = MockReceiver::start().await;
    let wake_url = format!("{}-PATH-MARKER?token=QUERY-MARKER", mock.url());
    let woken = api.register("agent-e", Some(&wake_url)).await;
    let reply = api
        .send_message(&sender.api_key, "agent-e", body_marker, "key-wake")
        .await;
    assert_eq!(reply.status.as_u16(), 201);
    let received = mock.wait_for(1, Duration::from_secs(10)).await;
    assert!(received[0].path_and_query.ends_with("?token=QUERY-MARKER"));
    eventually(Duration::from_secs(10), "the outcome is logged", || {
        let logs = captured.text();
        async move { logs.contains("receiver accepted the wake").then_some(()) }
    })
    .await;
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(logs.contains("message committed"), "send is logged");
    assert!(
        !logs.contains("BODY-MARKER"),
        "a message body or q was logged"
    );
    assert!(
        logs.contains("agent registered"),
        "capture is working: {logs}"
    );
    assert!(logs.contains("agent credentials rotated"));
    assert!(logs.contains("delivery worker started"));
    assert!(
        !logs.contains("PATH-MARKER") && !logs.contains("QUERY-MARKER"),
        "a receiver URL path or query was logged"
    );
    let new_key = rotated.body["agent_api_key"].as_str().unwrap();
    let new_secret = rotated.body["receiver_secret"].as_str().unwrap();
    for secret in [
        agent.api_key.as_str(),
        agent.receiver_secret.as_str(),
        new_key,
        new_secret,
        woken.api_key.as_str(),
        woken.receiver_secret.as_str(),
        OWNER_TOKEN,
    ] {
        assert!(!logs.contains(secret), "a credential was logged");
    }
    assert!(!logs.contains("pak_"), "an agent API key prefix was logged");
    assert!(
        !logs.contains("prs_"),
        "a receiver secret prefix was logged"
    );
}

#[tokio::test]
async fn profile_values_do_not_appear_in_logs() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();

    let agent = api
        .register_named("agent-p", "PROFILE-NAME-MARKER", None)
        .await;
    let path = "/v1/agents/agent-p/profile";
    let declared = api
        .put(
            path,
            &agent.api_key,
            json!({"gender_form": "none", "role": "PROFILE-ROLE-MARKER", "intro": "PROFILE-INTRO-MARKER\nline two"}),
        )
        .await;
    assert_eq!(declared.status.as_u16(), 200, "{}", declared.body);
    let renamed = api
        .put(path, OWNER_TOKEN, json!({"name": "PROFILE-NAME-MARKER-2"}))
        .await;
    assert_eq!(renamed.status.as_u16(), 200, "{}", renamed.body);
    api.put(
        path,
        &agent.api_key,
        json!({"name": "PROFILE-NAME-MARKER-3"}),
    )
    .await
    .assert_error(403, "forbidden");
    api.put(
        path,
        &agent.api_key,
        json!({"role": "PROFILE-ROLE-MARKER\tinvalid"}),
    )
    .await
    .assert_error(422, "invalid_field");
    api.post(
        "/v1/agents",
        OWNER_TOKEN,
        json!({"agent_id": "agent-q", "name": "PROFILE-NAME-MARKER\n"}),
    )
    .await;
    api.post(
        "/v1/agents",
        OWNER_TOKEN,
        json!({"agent_id": "agent-q", "name": "PROFILE-NAME-MARKER", "intro": "PROFILE-INTRO-MARKER"}),
    )
    .await
    .assert_error(422, "unexpected_field");
    api.get("/v1/agents", &agent.api_key).await;
    api.get("/v1/agents", OWNER_TOKEN).await;
    server.shutdown().await.unwrap();

    let logs = captured.text();
    let updates: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("agent profile updated") && line.contains("agent-p"))
        .collect();
    assert_eq!(updates.len(), 2, "{logs}");
    assert!(
        updates[0].contains("fields=gender_form,role,intro"),
        "{}",
        updates[0]
    );
    assert!(updates[1].contains("fields=name"), "{}", updates[1]);
    assert!(
        !logs.contains("PROFILE-"),
        "a profile name, role or intro was logged"
    );
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn tokens_and_seed_do_not_appear_in_logs() {
    let captured = install_capture();
    let fx = Fixture::new();
    let config = fx
        .load_config_with_identity("issuer = \"agents-platform\"\naudiences = [\"garage-docs\"]\n");
    let kid = config.identity.as_ref().unwrap().signing.kid().to_owned();
    let server = RunningServer::start(&config).await;
    let api = server.api();
    let agent = api.register("agent-a", None).await;

    let mut tokens = Vec::new();
    for body in [
        json!({"aud": "garage-docs"}),
        json!({"aud": "garage-docs", "ttl_secs": 5}),
    ] {
        let reply = api.post("/v1/identity/tokens", &agent.api_key, body).await;
        assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
        tokens.push(reply.body["token"].as_str().unwrap().to_owned());
    }
    api.post(
        "/v1/identity/tokens",
        OWNER_TOKEN,
        json!({"aud": "garage-docs"}),
    )
    .await
    .assert_error(403, "owner_not_allowed");
    api.post(
        "/v1/identity/tokens",
        &agent.api_key,
        json!({"aud": "nope"}),
    )
    .await
    .assert_error(422, "unknown_audience");
    // A token presented back to the hub is a failed bearer, not a log entry.
    api.get("/v1/inbox", &tokens[0])
        .await
        .assert_error(401, "unauthorized");
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        logs.contains("identity attestation enabled") && logs.contains(&kid),
        "startup logs the kid: {logs}"
    );
    let issued: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("identity token issued"))
        .collect();
    assert_eq!(issued.len(), tokens.len(), "{logs}");
    for line in &issued {
        let fields = line
            .split_once("identity token issued ")
            .map(|(_, fields)| fields)
            .unwrap_or_else(|| panic!("fields follow the message: {line}"));
        let names: Vec<&str> = fields
            .split(' ')
            .map(|field| field.split_once('=').map_or(field, |(name, _)| name))
            .collect();
        assert_eq!(names, ["sub", "jti", "aud", "exp"], "{line}");
        assert!(fields.starts_with("sub=agent-a jti="), "{line}");
        assert!(fields.contains(" aud=garage-docs exp="), "{line}");
    }

    for token in &tokens {
        assert!(!logs.contains(token.as_str()), "a token was logged");
        for part in token.split('.').skip(1) {
            assert!(!logs.contains(part), "a token segment was logged");
        }
    }
    let seed = std::fs::read(fx.path().join("identity.key")).unwrap();
    for encoded in [
        hex(&seed),
        STANDARD.encode(&seed),
        URL_SAFE_NO_PAD.encode(&seed),
    ] {
        assert!(!logs.contains(&encoded), "the seed was logged");
    }
    assert!(!logs.contains(&agent.api_key), "an API key was logged");
}

#[tokio::test]
async fn room_post_bodies_do_not_appear_in_logs() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("human", None).await;

    let body_marker = "ROOM-POST-MARKER-do-not-log";
    for (room, key) in [("dm:agent-a:agent-b", "room-key-1"), ("all", "room-key-2")] {
        let reply = api
            .post(
                "/v1/messages",
                OWNER_TOKEN,
                json!({"sender_id": "human", "room_id": room, "body": body_marker, "client_key": key}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    }
    api.post(
        "/v1/messages",
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "dm:agent-a:agent-b", "body": "ROOM-POST-MARKER-conflict", "client_key": "room-key-1"}),
    )
    .await
    .assert_error(409, "idempotency_conflict");
    api.get("/v1/rooms", OWNER_TOKEN).await;
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        !logs.contains("ROOM-POST-MARKER"),
        "a room post body was logged"
    );
    assert!(
        logs.contains("post_as"),
        "startup logs the allowlist: {logs}"
    );
}

#[tokio::test]
async fn avatar_bytes_do_not_appear_in_logs() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    let agent = api.register("agent-a", None).await;

    let marker = "AVATAR-BYTES-MARKER-do-not-log";
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend_from_slice(marker.as_bytes());
    let path = "/v1/agents/agent-a/avatar";
    for token in [agent.api_key.as_str(), OWNER_TOKEN] {
        let reply = api
            .send_bytes(
                Method::PUT,
                path,
                Some(token),
                &[("content-type", "image/png")],
                Some(bytes.clone()),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200);
    }
    api.send_bytes(Method::GET, path, Some(OWNER_TOKEN), &[], None)
        .await;
    // A rejected upload (magic mismatch) logs nothing of the body either.
    api.send_bytes(
        Method::PUT,
        path,
        Some(&agent.api_key),
        &[("content-type", "image/jpeg")],
        Some(bytes.clone()),
    )
    .await;
    api.send_bytes(Method::DELETE, path, Some(&agent.api_key), &[], None)
        .await;
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        !logs.contains("AVATAR-BYTES-MARKER"),
        "avatar bytes were logged"
    );
    assert!(!logs.contains(&agent.api_key), "an API key was logged");
}

#[tokio::test]
async fn agent_deletion_logs_no_credentials_or_bodies() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    let peer = api.register("agent-peer", None).await;
    let doomed = api
        .register(
            "agent-doomed",
            Some("http://receiver.test/DELETE-PATH-MARKER"),
        )
        .await;
    api.sent(
        &doomed.api_key,
        "agent-peer",
        "DELETE-BODY-MARKER-do-not-log",
        "d-1",
    )
    .await;
    api.sent(
        &peer.api_key,
        "agent-doomed",
        "DELETE-BODY-MARKER-reply",
        "p-1",
    )
    .await;
    let reply = api
        .send(
            Method::DELETE,
            "/v1/agents/agent-doomed",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(reply.status.as_u16(), 204, "{}", reply.body);
    api.send(Method::DELETE, "/v1/agents/human", Some(OWNER_TOKEN), None)
        .await;
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        logs.contains("agent deleted"),
        "the deletion is logged: {logs}"
    );
    for secret in [doomed.api_key.as_str(), doomed.receiver_secret.as_str()] {
        assert!(!logs.contains(secret), "a credential was logged");
    }
    assert!(!logs.contains("DELETE-BODY-MARKER"), "a body was logged");
    assert!(
        !logs.contains("DELETE-PATH-MARKER"),
        "a receiver path was logged"
    );
}

/// S13: sends, topic room management and the receiver PATCH log no body,
/// no room title and no mention values; the send line carries counts only
/// (`IF-WIRE-010`, `IF-WIRE-011`).
#[tokio::test]
async fn s13_sends_and_room_management_log_no_bodies_titles_or_mentions() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("mentioned-only", None).await;
    api.register("human", None).await;

    let title = "S13-TITLE-MARKER-do-not-log";
    let created = api
        .post(
            "/v1/rooms",
            OWNER_TOKEN,
            json!({"room_id": "room:s13-log", "title": title}),
        )
        .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    for member in ["agent-a", "mentioned-only"] {
        let reply = api
            .send(
                Method::PUT,
                &format!("/v1/rooms/room:s13-log/members/{member}"),
                Some(OWNER_TOKEN),
                None,
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }

    let body = "S13-BODY-MARKER-do-not-log";
    for request in [
        json!({"room_id": "room:s13-log", "body": body, "client_key": "s13-1", "wake": false, "mentions": ["mentioned-only"]}),
        json!({"to": "agent-b", "body": body, "client_key": "s13-2"}),
    ] {
        let reply = api.post("/v1/messages", &a.api_key, request).await;
        assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    }
    let owner_post = api
        .post(
            "/v1/messages",
            OWNER_TOKEN,
            json!({"sender_id": "human", "room_id": "dm:agent-a:agent-b", "body": body, "client_key": "s13-3"}),
        )
        .await;
    assert_eq!(owner_post.status.as_u16(), 201, "{}", owner_post.body);
    api.send(
        Method::DELETE,
        "/v1/rooms/room:s13-log/members/mentioned-only",
        Some(OWNER_TOKEN),
        None,
    )
    .await;
    api.send(
        Method::POST,
        "/v1/rooms/room:s13-log/archive",
        Some(OWNER_TOKEN),
        None,
    )
    .await;
    let patched = api
        .patch(
            "/v1/agents/agent-a/receiver",
            OWNER_TOKEN,
            json!({"wake_policy": "always"}),
        )
        .await;
    assert_eq!(patched.status.as_u16(), 200, "{}", patched.body);
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        !logs.contains("S13-BODY-MARKER"),
        "a message body was logged"
    );
    assert!(
        !logs.contains("S13-TITLE-MARKER"),
        "a room title was logged"
    );
    let committed: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("message committed") && line.contains("room:s13-log"))
        .collect();
    assert_eq!(committed.len(), 1, "{logs}");
    assert!(
        !committed[0].contains("mentioned-only"),
        "mention values are not logged: {}",
        committed[0]
    );
    for field in ["recipients=", "woken=", "wake=false", "mentions=1"] {
        assert!(committed[0].contains(field), "{field}: {}", committed[0]);
    }
}

/// S14: a presence note is free text other agents read, like a profile
/// value, and never reaches the logs; the set and clear lines carry only
/// the agent and the state, and recording `last_seen_at` logs nothing at
/// INFO.
#[tokio::test]
async fn presence_notes_and_last_seen_writes_stay_out_of_the_logs() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    let a = api.register("presence-a", None).await;
    let b = api.register("presence-b", None).await;
    let note = "S14-NOTE-MARKER-do-not-log";

    let set = api
        .put(
            "/v1/agents/presence-a/presence",
            &a.api_key,
            json!({"state": "busy", "note": note, "ttl_secs": 60}),
        )
        .await;
    assert_eq!(set.status.as_u16(), 200, "{}", set.body);
    api.put(
        "/v1/agents/presence-a/presence",
        OWNER_TOKEN,
        json!({"state": "busy", "note": format!("{note}\nsecond line")}),
    )
    .await
    .assert_error(422, "invalid_field");
    api.get("/v1/agents", &b.api_key).await;
    api.get("/v1/agents", OWNER_TOKEN).await;
    let cleared = api
        .send(
            Method::DELETE,
            "/v1/agents/presence-a/presence",
            Some(&a.api_key),
            None,
        )
        .await;
    assert_eq!(cleared.status.as_u16(), 204);
    eventually(
        Duration::from_secs(5),
        "last_seen_at is recorded",
        || async {
            let item = api.get("/v1/agents", OWNER_TOKEN).await.body["items"][0].clone();
            item["last_seen_at"].is_string().then_some(())
        },
    )
    .await;
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        !logs.contains("S14-NOTE-MARKER"),
        "a presence note was logged"
    );
    let set_lines: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("presence set") && line.contains("presence-a"))
        .collect();
    assert_eq!(set_lines.len(), 1, "{logs}");
    assert!(
        set_lines[0].contains("INFO") && set_lines[0].contains("state=busy"),
        "{}",
        set_lines[0]
    );
    assert!(
        logs.lines()
            .any(|line| line.contains("presence cleared") && line.contains("presence-a")),
        "{logs}"
    );
    let touch_info: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("INFO") && line.contains("last_seen_at"))
        .collect();
    assert!(touch_info.is_empty(), "{touch_info:?}");
}

/// S15: a thread message's commit line names its thread (ids and counts
/// only, no body or mention values), a correction logs no body, and the
/// participation line carries only the thread, the agent and `muted`.
#[tokio::test]
async fn thread_sends_and_participation_log_ids_only() {
    let captured = install_capture();
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("s15-mentioned", None).await;

    let body = "S15-BODY-MARKER-do-not-log";
    let root = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"room_id": "all", "body": body, "client_key": "s15-root"}),
        )
        .await;
    assert_eq!(root.status.as_u16(), 201, "{}", root.body);
    let root_id = root.body["message_id"].as_str().unwrap().to_owned();
    let reply = api
        .post(
            "/v1/messages",
            &b.api_key,
            json!({"reply_to": root_id, "body": body, "client_key": "s15-reply", "mentions": ["s15-mentioned"]}),
        )
        .await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    let correction = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"room_id": "all", "body": body, "client_key": "s15-fix", "corrects": root_id}),
        )
        .await;
    assert_eq!(correction.status.as_u16(), 201, "{}", correction.body);
    let muted = api
        .put(
            &format!("/v1/threads/{root_id}/participation"),
            &a.api_key,
            json!({"wake": false}),
        )
        .await;
    assert_eq!(muted.status.as_u16(), 200, "{}", muted.body);
    server.shutdown().await.unwrap();

    let logs = captured.text();
    assert!(
        !logs.contains("S15-BODY-MARKER"),
        "a message body was logged"
    );
    let reply_id = reply.body["message_id"].as_str().unwrap();
    let committed: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("message committed") && line.contains(reply_id))
        .collect();
    assert_eq!(committed.len(), 1, "{logs}");
    assert!(
        committed[0].contains(&format!("thread_id={root_id}")),
        "{}",
        committed[0]
    );
    assert!(
        !committed[0].contains("s15-mentioned"),
        "mention values are not logged: {}",
        committed[0]
    );
    let participation: Vec<&str> = logs
        .lines()
        .filter(|line| line.contains("thread participation"))
        .collect();
    assert_eq!(participation.len(), 1, "{logs}");
    for field in [
        format!("thread_id={root_id}"),
        "agent_id=agent-a".to_owned(),
        "muted=true".to_owned(),
    ] {
        assert!(
            participation[0].contains(&field),
            "{field}: {}",
            participation[0]
        );
    }
}
