//! S2: `POST /v1/messages` — durable commit, fanout, DM, idempotency,
//! validation order and the two sequences, over HTTP with the database
//! inspected directly.

mod common;

use std::collections::BTreeMap;
use std::time::Duration;

use common::{Api, Fixture, OWNER_TOKEN, RunningServer};
use platform_wire::messages::{MAX_BODY_BYTES, MAX_CLIENT_KEY_BYTES};
use platformd::api::MAX_WIRE_BODY_BYTES;
use reqwest::Method;
use serde_json::{Value, json};

/// Guards against a hang only; the signal is expected immediately.
const SIGNAL_WAIT: Duration = Duration::from_secs(5);

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    (fx, server, api)
}

fn membership_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/membership/all")
}

fn seq(value: &Value) -> i64 {
    value
        .as_str()
        .expect("sequence string")
        .parse()
        .expect("decimal")
}

/// `(agent_id, inbox_seq, message_id)` for every inbox row, in key order.
async fn inbox_rows(fx: &Fixture) -> Vec<(String, i64, String)> {
    fx.read_db(|conn| {
        let mut statement = conn.prepare(
            "SELECT agent_id, inbox_seq, message_id FROM inbox_rows ORDER BY agent_id, inbox_seq",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

async fn message_count(fx: &Fixture) -> i64 {
    fx.read_db(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?))
        .await
}

fn recipients_of<'a>(rows: &'a [(String, i64, String)], message_id: &str) -> Vec<&'a str> {
    rows.iter()
        .filter(|(_, _, id)| id == message_id)
        .map(|(agent, _, _)| agent.as_str())
        .collect()
}

// ---- Scenario 1: all fanout ---------------------------------------------------

#[tokio::test]
async fn all_fanout_stores_one_message_and_an_inbox_row_per_member() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("agent-c", None).await;

    let reply = api
        .send_message(&a.api_key, "all", "배포 결과를 확인해 주세요.", "k-1")
        .await;

    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    let message = &reply.body;
    let message_id = message["message_id"].as_str().unwrap();
    assert!(message_id.starts_with("msg-"), "{message}");
    assert_eq!(message["room_id"], "all");
    assert_eq!(message["sender_id"], "agent-a");
    assert_eq!(message["body"], "배포 결과를 확인해 주세요.");
    assert_eq!(message["reply_to"], Value::Null);
    assert_eq!(message["deduplicated"], false);
    assert_eq!(seq(&message["event_seq"]), 1);
    assert!(message["created_at"].as_str().unwrap().ends_with('Z'));

    assert_eq!(message_count(&fx).await, 1);
    let rows = inbox_rows(&fx).await;
    assert_eq!(
        recipients_of(&rows, message_id),
        ["agent-a", "agent-b", "agent-c"],
        "every joined agent, the sender included"
    );
    assert!(rows.iter().all(|(_, inbox_seq, _)| *inbox_seq == 1));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_commit_raises_the_delivery_signal() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    api.sent(&a.api_key, "all", "wake", "k-1").await;

    tokio::time::timeout(SIGNAL_WAIT, server.delivery.notified())
        .await
        .expect("the 201 commit notifies the delivery signal");
    server.shutdown().await.unwrap();
}

// ---- Scenario 2: DM ------------------------------------------------------------

#[tokio::test]
async fn dm_goes_to_the_recipient_inbox_only_in_a_canonical_room() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("agent-c", None).await;

    let from_b = api.sent(&b.api_key, "agent-a", "b to a", "k-1").await;
    let from_a = api.sent(&a.api_key, "agent-b", "a to b", "k-1").await;

    assert_eq!(from_b["room_id"], "dm:agent-a:agent-b");
    assert_eq!(
        from_a["room_id"], "dm:agent-a:agent-b",
        "both directions share the room"
    );
    let rows = inbox_rows(&fx).await;
    assert_eq!(
        recipients_of(&rows, from_b["message_id"].as_str().unwrap()),
        ["agent-a"]
    );
    assert_eq!(
        recipients_of(&rows, from_a["message_id"].as_str().unwrap()),
        ["agent-b"]
    );
    assert_eq!(rows.len(), 2, "no row for the sender or bystanders");

    let history = api
        .get("/v1/messages?room_id=dm:agent-a:agent-b", &a.api_key)
        .await;
    assert_eq!(history.status.as_u16(), 200, "{}", history.body);
    assert_eq!(history.body["items"][0]["message_id"], from_a["message_id"]);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unknown_recipient_is_404_and_self_dm_is_422_without_writes() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    api.send_message(&a.api_key, "agent-zed", "hello?", "k-1")
        .await
        .assert_error(404, "unknown_recipient");
    api.send_message(&a.api_key, "agent-a", "me", "k-2")
        .await
        .assert_error(422, "self_dm_not_supported");

    assert_eq!(message_count(&fx).await, 0);
    assert!(inbox_rows(&fx).await.is_empty());
    // A rejected key stays unbound: it can still commit a different request.
    api.sent(&a.api_key, "all", "now valid", "k-1").await;
    server.shutdown().await.unwrap();
}

// ---- Scenario 4: idempotency ------------------------------------------------------

#[tokio::test]
async fn repeated_client_key_returns_the_original_or_conflicts() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let original = api.sent(&a.api_key, "all", "hello", "k-1").await;

    let repeat = api.send_message(&a.api_key, "all", "hello", "k-1").await;
    assert_eq!(repeat.status.as_u16(), 200, "{}", repeat.body);
    assert_eq!(repeat.body["deduplicated"], true);
    let mut expected = original.clone();
    expected["deduplicated"] = json!(true);
    assert_eq!(repeat.body, expected, "the original message, unchanged");

    let explicit_null = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"to": "all", "body": "hello", "client_key": "k-1", "reply_to": null}),
        )
        .await;
    assert_eq!(
        explicit_null.status.as_u16(),
        200,
        "missing and null reply_to are equal"
    );

    for (to, body) in [("all", "hello "), ("all", "Hello"), ("agent-b", "hello")] {
        api.send_message(&a.api_key, to, body, "k-1")
            .await
            .assert_error(409, "idempotency_conflict");
    }
    let with_reply = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"to": "all", "body": "hello", "client_key": "k-1",
                   "reply_to": original["message_id"]}),
        )
        .await;
    with_reply.assert_error(409, "idempotency_conflict");

    // Keys are per sender; a new key is a new message.
    let other_sender = api.send_message(&b.api_key, "all", "hello", "k-1").await;
    assert_eq!(other_sender.status.as_u16(), 201);
    let new_key = api.send_message(&a.api_key, "all", "hello", "k-2").await;
    assert_eq!(new_key.status.as_u16(), 201);
    assert_ne!(new_key.body["message_id"], original["message_id"]);

    assert_eq!(message_count(&fx).await, 3);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn retry_after_leaving_all_still_returns_the_original() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let original = api.sent(&a.api_key, "all", "before leaving", "k-1").await;
    api.put(
        &membership_path("agent-a"),
        &a.api_key,
        json!({"joined": false}),
    )
    .await;

    let retry = api
        .send_message(&a.api_key, "all", "before leaving", "k-1")
        .await;
    assert_eq!(retry.status.as_u16(), 200, "{}", retry.body);
    assert_eq!(retry.body["message_id"], original["message_id"]);

    api.send_message(&a.api_key, "all", "after leaving", "k-2")
        .await
        .assert_error(403, "not_all_member");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_conflicting_key_is_checked_before_membership() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.sent(&a.api_key, "all", "first", "k-1").await;
    api.put(
        &membership_path("agent-a"),
        &a.api_key,
        json!({"joined": false}),
    )
    .await;

    api.send_message(&a.api_key, "all", "different", "k-1")
        .await
        .assert_error(409, "idempotency_conflict");
    server.shutdown().await.unwrap();
}

// ---- Scenario 11: leave and rejoin ---------------------------------------------------

#[tokio::test]
async fn broadcasts_during_leave_are_not_backfilled_after_rejoin() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;

    let before = api.sent(&a.api_key, "all", "before leave", "k-1").await;
    api.put(
        &membership_path("agent-b"),
        &b.api_key,
        json!({"joined": false}),
    )
    .await;
    let during = api.sent(&a.api_key, "all", "during leave", "k-2").await;
    api.put(
        &membership_path("agent-b"),
        &b.api_key,
        json!({"joined": true}),
    )
    .await;
    let after = api.sent(&a.api_key, "all", "after rejoin", "k-3").await;

    let rows = inbox_rows(&fx).await;
    let b_rows: Vec<(i64, &str)> = rows
        .iter()
        .filter(|(agent, _, _)| agent == "agent-b")
        .map(|(_, inbox_seq, id)| (*inbox_seq, id.as_str()))
        .collect();
    assert_eq!(
        b_rows,
        [
            (1, before["message_id"].as_str().unwrap()),
            (2, after["message_id"].as_str().unwrap())
        ],
        "the row from before leaving is kept and inbox_seq stays contiguous"
    );
    assert_eq!(
        recipients_of(&rows, during["message_id"].as_str().unwrap()),
        ["agent-a"]
    );

    let history = api.get("/v1/messages?room_id=all", &b.api_key).await;
    let ids: Vec<&Value> = history.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| &item["message_id"])
        .collect();
    assert!(
        ids.contains(&&during["message_id"]),
        "history still shows it"
    );
    server.shutdown().await.unwrap();
}

// ---- Sequences ---------------------------------------------------------------------

#[tokio::test]
async fn event_and_inbox_sequences_have_no_holes_under_concurrent_sends() {
    let (fx, server, api) = start().await;
    let agents = ["agent-a", "agent-b", "agent-c"];
    let mut keys = Vec::new();
    for agent in agents {
        keys.push(api.register(agent, None).await.api_key);
    }

    let mut tasks = Vec::new();
    for round in 0..15 {
        for (index, key) in keys.iter().enumerate() {
            let api = server.api();
            let key = key.clone();
            let to = if round % 3 == 0 {
                agents[(index + 1) % agents.len()].to_owned()
            } else {
                "all".to_owned()
            };
            tasks.push(tokio::spawn(async move {
                // Rejections in between must not consume sequence numbers.
                api.send_message(&key, "agent-zed", "lost", &format!("bad-{round}"))
                    .await
                    .assert_error(404, "unknown_recipient");
                api.sent(&key, &to, &format!("round {round}"), &format!("k-{round}"))
                    .await
            }));
        }
    }
    let mut committed = 0;
    for task in tasks {
        task.await.expect("send task");
        committed += 1;
    }

    let (event_seqs, heads) = fx
        .read_db(|conn| {
            let mut statement =
                conn.prepare("SELECT event_seq FROM messages ORDER BY event_seq")?;
            let seqs = statement
                .query_map([], |row| row.get::<_, i64>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let mut statement = conn.prepare("SELECT agent_id, inbox_head FROM agents")?;
            let heads = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })?
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            let counter: i64 = conn.query_row(
                "SELECT value FROM sequences WHERE name = 'event_seq'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(counter, seqs.len() as i64);
            Ok((seqs, heads))
        })
        .await;
    assert_eq!(event_seqs, (1..=committed).collect::<Vec<i64>>());

    let rows = inbox_rows(&fx).await;
    for agent in agents {
        let seqs: Vec<i64> = rows
            .iter()
            .filter(|(owner, _, _)| owner == agent)
            .map(|(_, inbox_seq, _)| *inbox_seq)
            .collect();
        let head = heads[agent];
        assert_eq!(seqs, (1..=head).collect::<Vec<i64>>(), "{agent}");
    }
    server.shutdown().await.unwrap();
}

// ---- reply_to --------------------------------------------------------------------------

#[tokio::test]
async fn reply_to_must_exist_in_the_same_room() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let in_all = api.sent(&a.api_key, "all", "question", "k-1").await;
    let in_dm = api.sent(&a.api_key, "agent-b", "private", "k-2").await;

    let send_reply = |to: &'static str, reply_to: Value, key: &'static str| {
        api.post(
            "/v1/messages",
            &b.api_key,
            json!({"to": to, "body": "answer", "client_key": key, "reply_to": reply_to}),
        )
    };

    let reply = send_reply("all", in_all["message_id"].clone(), "r-1").await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    assert_eq!(reply.body["reply_to"], in_all["message_id"]);

    let dm_reply = send_reply("agent-a", in_dm["message_id"].clone(), "r-2").await;
    assert_eq!(
        dm_reply.status.as_u16(),
        201,
        "the reverse DM is the same room"
    );

    send_reply("all", in_dm["message_id"].clone(), "r-3")
        .await
        .assert_error(422, "reply_to_room_mismatch");
    send_reply("all", json!("msg-01J00000000000000000000000"), "r-4")
        .await
        .assert_error(404, "reply_to_not_found");
    let malformed = send_reply("all", json!("not-a-message-id"), "r-5").await;
    malformed.assert_error(422, "invalid_field");
    assert_eq!(malformed.error_field(), Some("reply_to"));
    server.shutdown().await.unwrap();
}

// ---- Validation order and limits ----------------------------------------------------------

#[tokio::test]
async fn only_agents_may_send() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;

    api.send_message(OWNER_TOKEN, "all", "owner", "k-1")
        .await
        .assert_error(403, "forbidden");
    // Authentication runs before the body is examined.
    api.send_raw(
        Method::POST,
        "/v1/messages",
        Some(OWNER_TOKEN),
        Some("text/plain"),
        Some(b"{".to_vec()),
    )
    .await
    .assert_error(403, "forbidden");
    api.send_message("pak_unknown", "all", "who", "k-1")
        .await
        .assert_error(401, "unauthorized");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn request_shape_errors_use_the_wire_codes() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    let forged = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"to": "all", "body": "x", "client_key": "k", "sender_id": "agent-b"}),
        )
        .await;
    forged.assert_error(422, "unexpected_field");
    assert_eq!(forged.error_field(), Some("sender_id"));

    let missing_key = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"to": "all", "body": "x"}),
        )
        .await;
    missing_key.assert_error(422, "invalid_field");
    assert_eq!(missing_key.error_field(), Some("client_key"));

    let too_long = "k".repeat(MAX_CLIENT_KEY_BYTES + 1);
    for client_key in ["", too_long.as_str(), "키"] {
        let reply = api.send_message(&a.api_key, "all", "x", client_key).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("client_key"), "{client_key:?}");
    }
    let longest = "k".repeat(MAX_CLIENT_KEY_BYTES);
    api.sent(&a.api_key, "all", "x", &longest).await;

    for to in ["", "owner", "ALL", "dm:agent-a:agent-b"] {
        let reply = api.send_message(&a.api_key, to, "x", "k-to").await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("to"), "{to:?}");
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn body_limit_counts_utf8_bytes_and_wire_limit_applies_first() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    let exact = "a".repeat(MAX_BODY_BYTES);
    api.sent(&a.api_key, "all", &exact, "k-exact").await;

    // Three bytes per character: under the limit in characters, over in bytes.
    let multibyte = "가".repeat(MAX_BODY_BYTES / 3 + 1);
    assert!(multibyte.chars().count() < MAX_BODY_BYTES);
    api.send_message(&a.api_key, "all", &multibyte, "k-wide")
        .await
        .assert_error(413, "body_too_large");

    // client_key is checked before body size, body size before `to`.
    let over = "a".repeat(MAX_BODY_BYTES + 1);
    let reply = api.send_message(&a.api_key, "all", &over, "").await;
    reply.assert_error(422, "invalid_field");
    assert_eq!(reply.error_field(), Some("client_key"));
    api.send_message(&a.api_key, "owner", &over, "k-over")
        .await
        .assert_error(413, "body_too_large");

    let padding = "a".repeat(MAX_WIRE_BODY_BYTES);
    let wire = format!(r#"{{"to":"all","body":"{padding}","client_key":"k-wire"}}"#);
    api.send_raw(
        Method::POST,
        "/v1/messages",
        Some(&a.api_key),
        Some("application/json"),
        Some(wire.into_bytes()),
    )
    .await
    .assert_error(413, "request_too_large");
    server.shutdown().await.unwrap();
}
