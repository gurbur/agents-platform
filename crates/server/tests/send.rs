//! S13 `POST /v1/messages` (`IF-WIRE-010`): one send contract for agents
//! and the owner, the judgement order 1–18 one step at a time, and
//! idempotency over the resolved room, `wake` and `mentions`.
//!
//! Every rejection is followed by a check that nothing was written.

mod common;

use common::{Api, Fixture, OWNER_TOKEN, Registered, Reply, RunningServer};
use platform_wire::MAX_MENTIONS;
use platform_wire::messages::MAX_BODY_BYTES;
use serde_json::{Value, json};

/// A hub with `[owner] post_as = ["human", "ghost"]`; `ghost` is never
/// registered.
async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human", "ghost"])).await;
    let api = server.api();
    (fx, server, api)
}

struct Agents {
    a: Registered,
    b: Registered,
    c: Registered,
    human: Registered,
}

async fn agents(api: &Api) -> Agents {
    Agents {
        a: api.register("agent-a", None).await,
        b: api.register("agent-b", None).await,
        c: api.register("agent-c", None).await,
        human: api.register("human", None).await,
    }
}

async fn send(api: &Api, token: &str, body: Value) -> Reply {
    api.post("/v1/messages", token, body).await
}

async fn sent(api: &Api, token: &str, body: Value) -> Value {
    let reply = send(api, token, body).await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    reply.body
}

/// `(messages, event_seq, inbox_rows, rooms, memberships, idempotency)`.
async fn counts(fx: &Fixture) -> [i64; 6] {
    fx.read_db(|conn| {
        let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
        Ok([
            count("SELECT COUNT(*) FROM messages")?,
            count("SELECT value FROM sequences WHERE name = 'event_seq'")?,
            count("SELECT COUNT(*) FROM inbox_rows")?,
            count("SELECT COUNT(*) FROM rooms")?,
            count("SELECT COUNT(*) FROM memberships")?,
            count("SELECT COUNT(*) FROM idempotency")?,
        ])
    })
    .await
}

async fn create_topic(api: &Api, room_id: &str, members: &[&str]) {
    let reply = api
        .post("/v1/rooms", OWNER_TOKEN, json!({"room_id": room_id}))
        .await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    for member in members {
        let reply = api
            .send(
                reqwest::Method::PUT,
                &format!("/v1/rooms/{room_id}/members/{member}"),
                Some(OWNER_TOKEN),
                None,
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }
}

async fn archive(api: &Api, room_id: &str) {
    let reply = api
        .send(
            reqwest::Method::POST,
            &format!("/v1/rooms/{room_id}/archive"),
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
}

// ---- handler steps 1–11 ----------------------------------------------------------

#[tokio::test]
async fn steps_1_to_3_authentication_body_format_and_the_agents_sender_id() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let before = counts(&fx).await;

    let anonymous = api
        .send(
            reqwest::Method::POST,
            "/v1/messages",
            None,
            Some(json!({"room_id": "all", "body": "x", "client_key": "k"})),
        )
        .await;
    anonymous.assert_error(401, "unauthorized");

    let text = api
        .send_raw(
            reqwest::Method::POST,
            "/v1/messages",
            Some(&ag.a.api_key),
            Some("text/plain"),
            Some(b"{}".to_vec()),
        )
        .await;
    text.assert_error(415, "unsupported_media_type");
    let not_json = api
        .send_raw(
            reqwest::Method::POST,
            "/v1/messages",
            Some(&ag.a.api_key),
            Some("application/json"),
            Some(b"{".to_vec()),
        )
        .await;
    not_json.assert_error(400, "invalid_json");

    let unknown = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k", "thread_id": "t"}),
    )
    .await;
    unknown.assert_error(422, "unexpected_field");
    assert_eq!(unknown.error_field(), Some("thread_id"));

    let forged = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k", "sender_id": "agent-a"}),
    )
    .await;
    forged.assert_error(422, "unexpected_field");
    assert_eq!(forged.error_field(), Some("sender_id"));

    assert_eq!(counts(&fx).await, before, "nothing was written");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_4_missing_fields_and_types() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let before = counts(&fx).await;

    let owner_without_sender = send(
        &api,
        OWNER_TOKEN,
        json!({"room_id": "all", "body": "x", "client_key": "k"}),
    )
    .await;
    owner_without_sender.assert_error(422, "invalid_field");
    assert_eq!(owner_without_sender.error_field(), Some("sender_id"));

    for (body, field) in [
        (json!({"room_id": "all", "client_key": "k"}), "body"),
        (json!({"room_id": "all", "body": "x"}), "client_key"),
        (
            json!({"room_id": "all", "body": "x", "client_key": "k", "wake": "no"}),
            "wake",
        ),
        (
            json!({"room_id": "all", "body": "x", "client_key": "k", "mentions": "agent-b"}),
            "mentions",
        ),
        (
            json!({"room_id": "all", "body": "x", "client_key": "k", "mentions": [7]}),
            "mentions",
        ),
        (
            json!({"room_id": 7, "body": "x", "client_key": "k"}),
            "room_id",
        ),
        (json!({"to": true, "body": "x", "client_key": "k"}), "to"),
    ] {
        let reply = send(&api, &ag.a.api_key, body.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{body}");
    }
    assert_eq!(counts(&fx).await, before, "nothing was written");

    // A null optional field means the same as an omitted one.
    let nulls = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "to": null, "reply_to": null, "body": "x", "client_key": "nulls",
               "wake": null, "mentions": null, "sender_id": null}),
    )
    .await;
    assert_eq!(nulls.status.as_u16(), 201, "{}", nulls.body);
    assert_eq!(nulls.body["wake"], true);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_5_exactly_one_destination() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let first = sent(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "seed"}),
    )
    .await;
    let reply_to = first["message_id"].as_str().unwrap();
    let before = counts(&fx).await;

    for body in [
        json!({"body": "x", "client_key": "k"}),
        json!({"room_id": null, "to": null, "reply_to": null, "body": "x", "client_key": "k"}),
        json!({"room_id": "all", "to": "agent-b", "body": "x", "client_key": "k"}),
        json!({"to": "agent-b", "reply_to": reply_to, "body": "x", "client_key": "k"}),
        json!({"room_id": "all", "reply_to": reply_to, "body": "x", "client_key": "k"}),
        json!({"room_id": "all", "to": "agent-b", "reply_to": reply_to, "body": "x", "client_key": "k"}),
        // Checked before client_key (step 6).
        json!({"body": "x", "client_key": ""}),
    ] {
        let reply = send(&api, &ag.a.api_key, body.clone()).await;
        reply.assert_error(422, "invalid_destination");
    }
    let owner = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "to": "agent-b", "reply_to": reply_to, "body": "x", "client_key": "k"}),
    )
    .await;
    owner.assert_error(422, "invalid_destination");
    assert_eq!(counts(&fx).await, before, "nothing was written");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn steps_6_to_8_client_key_body_size_and_destination_formats() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let before = counts(&fx).await;

    let key = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": ""}),
    )
    .await;
    key.assert_error(422, "invalid_field");
    assert_eq!(key.error_field(), Some("client_key"));

    // The body is checked before the destination format (step 7 < 8).
    let large = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "room:A", "body": "x".repeat(MAX_BODY_BYTES + 1), "client_key": "k"}),
    )
    .await;
    large.assert_error(413, "body_too_large");

    for (body, field) in [
        (
            json!({"room_id": "room:Launch", "body": "x", "client_key": "k"}),
            "room_id",
        ),
        (
            json!({"room_id": "room:", "body": "x", "client_key": "k"}),
            "room_id",
        ),
        (
            json!({"room_id": "room:-x", "body": "x", "client_key": "k"}),
            "room_id",
        ),
        (
            json!({"room_id": "dm:agent-b:agent-a", "body": "x", "client_key": "k"}),
            "room_id",
        ),
        (json!({"to": "all", "body": "x", "client_key": "k"}), "to"),
        (
            json!({"to": "Agent-B", "body": "x", "client_key": "k"}),
            "to",
        ),
        (
            json!({"reply_to": "nope", "body": "x", "client_key": "k"}),
            "reply_to",
        ),
    ] {
        let reply = send(&api, &ag.a.api_key, body.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{body}");
    }
    assert_eq!(counts(&fx).await, before, "nothing was written");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_9_mentions_format_duplicates_and_self() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let before = counts(&fx).await;

    for mentions in [
        json!(["Agent-B"]),
        json!(["all"]),
        json!(["agent-b", "agent-b"]),
        json!(["agent-a"]),
    ] {
        let reply = send(
            &api,
            &ag.a.api_key,
            json!({"room_id": "all", "body": "x", "client_key": "k", "mentions": mentions}),
        )
        .await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("mentions"), "{mentions}");
    }
    // The owner's sender is the `sender_id` it posts as.
    let owner_self = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "all", "body": "x", "client_key": "k", "mentions": ["human"]}),
    )
    .await;
    owner_self.assert_error(422, "invalid_field");
    assert_eq!(owner_self.error_field(), Some("mentions"));
    assert_eq!(counts(&fx).await, before, "nothing was written");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_9_mentions_are_capped_by_the_contract_constant() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let mentions: Vec<String> = (0..=MAX_MENTIONS).map(|n| format!("m-{n}")).collect();
    let over = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k", "mentions": mentions}),
    )
    .await;
    over.assert_error(422, "invalid_field");
    assert_eq!(over.error_field(), Some("mentions"));
    let before = counts(&fx).await;

    // At the cap the format passes and membership decides (step 17).
    let at_cap: Vec<String> = (0..MAX_MENTIONS).map(|n| format!("m-{n}")).collect();
    let reply = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k", "mentions": at_cap}),
    )
    .await;
    reply.assert_error(422, "mention_not_member");
    assert_eq!(counts(&fx).await, before);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn steps_10_to_12_allowlist_self_dm_and_the_owners_sender() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let before = counts(&fx).await;

    let outside = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "agent-a", "room_id": "all", "body": "x", "client_key": "k"}),
    )
    .await;
    outside.assert_error(403, "forbidden");
    assert_eq!(outside.error_field(), Some("sender_id"));

    let self_dm = send(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-a", "body": "x", "client_key": "k"}),
    )
    .await;
    self_dm.assert_error(422, "self_dm_not_supported");
    let owner_self_dm = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "to": "human", "body": "x", "client_key": "k"}),
    )
    .await;
    owner_self_dm.assert_error(422, "self_dm_not_supported");

    let unregistered = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "ghost", "room_id": "all", "body": "x", "client_key": "k"}),
    )
    .await;
    unregistered.assert_error(404, "agent_not_found");
    assert_eq!(unregistered.error_field(), Some("sender_id"));

    assert_eq!(counts(&fx).await, before, "nothing was written");
    drop(ag);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn steps_13_to_15_reply_to_idempotency_then_room_permission() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let missing = send(
        &api,
        &ag.a.api_key,
        json!({"reply_to": "msg-01J00000000000000000000000", "body": "x", "client_key": "k"}),
    )
    .await;
    missing.assert_error(404, "reply_to_not_found");

    // A key bound to another request conflicts before the room is judged.
    sent(
        &api,
        &ag.c.api_key,
        json!({"room_id": "all", "body": "first", "client_key": "bound"}),
    )
    .await;
    let before = counts(&fx).await;
    let conflict = send(
        &api,
        &ag.c.api_key,
        json!({"room_id": "dm:agent-a:agent-b", "body": "x", "client_key": "bound"}),
    )
    .await;
    conflict.assert_error(409, "idempotency_conflict");

    let unknown = send(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-zz", "body": "x", "client_key": "k"}),
    )
    .await;
    unknown.assert_error(404, "unknown_recipient");
    assert_eq!(counts(&fx).await, before, "nothing was written");
    drop(ag);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_15_not_room_member_for_agents() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    create_topic(&api, "room:launch", &["agent-a"]).await;
    sent(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "dm"}),
    )
    .await;
    let left = api
        .put(
            "/v1/agents/agent-c/membership/all",
            &ag.c.api_key,
            json!({"joined": false}),
        )
        .await;
    assert_eq!(left.status.as_u16(), 200);
    let before = counts(&fx).await;

    for (token, room) in [
        (&ag.c.api_key, "all"),
        (&ag.c.api_key, "dm:agent-a:agent-b"),
        (&ag.c.api_key, "room:missing"),
        (&ag.b.api_key, "room:launch"),
    ] {
        let reply = send(
            &api,
            token,
            json!({"room_id": room, "body": "x", "client_key": format!("k-{room}")}),
        )
        .await;
        reply.assert_error(403, "not_room_member");
    }
    // `reply_to` resolves the room first, then the same permission applies.
    let dm = api
        .get("/v1/messages?room_id=dm:agent-a:agent-b", OWNER_TOKEN)
        .await;
    let dm_id = dm.body["items"][0]["message_id"].as_str().unwrap();
    let reply = send(
        &api,
        &ag.c.api_key,
        json!({"reply_to": dm_id, "body": "x", "client_key": "reply"}),
    )
    .await;
    reply.assert_error(403, "not_room_member");
    assert_eq!(counts(&fx).await, before, "nothing was written");
    drop(ag);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_15_unknown_room() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    api.register("agent-z", None).await;
    sent(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-z", "body": "before", "client_key": "z"}),
    )
    .await;
    let deleted = api
        .send(
            reqwest::Method::DELETE,
            "/v1/agents/agent-z",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    let before = counts(&fx).await;

    let owner_missing_topic = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "room:missing", "body": "x", "client_key": "k1"}),
    )
    .await;
    owner_missing_topic.assert_error(404, "unknown_room");
    for (token, sender) in [(OWNER_TOKEN, Some("human")), (ag.a.api_key.as_str(), None)] {
        let mut body = json!({"room_id": "dm:agent-a:agent-z", "body": "x", "client_key": "k2"});
        if let Some(sender) = sender {
            body["sender_id"] = json!(sender);
        }
        send(&api, token, body)
            .await
            .assert_error(404, "unknown_room");
    }
    let owner_unregistered_dm = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "dm:agent-a:agent-yy", "body": "x", "client_key": "k3"}),
    )
    .await;
    owner_unregistered_dm.assert_error(404, "unknown_room");
    let old = api
        .get("/v1/messages?room_id=dm:agent-a:agent-z", OWNER_TOKEN)
        .await;
    let old_id = old.body["items"][0]["message_id"].as_str().unwrap();
    send(
        &api,
        &ag.a.api_key,
        json!({"reply_to": old_id, "body": "x", "client_key": "k4"}),
    )
    .await
    .assert_error(404, "unknown_room");
    assert_eq!(counts(&fx).await, before, "nothing was written");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn steps_16_and_17_archived_rooms_and_mentions_outside_the_room() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    create_topic(&api, "room:old", &["agent-a", "agent-b"]).await;
    let first = sent(
        &api,
        &ag.a.api_key,
        json!({"room_id": "room:old", "body": "x", "client_key": "seed"}),
    )
    .await;
    archive(&api, "room:old").await;
    let before = counts(&fx).await;

    send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "room:old", "body": "x", "client_key": "k1"}),
    )
    .await
    .assert_error(409, "room_archived");
    send(
        &api,
        &ag.b.api_key,
        json!({"reply_to": first["message_id"], "body": "x", "client_key": "k2"}),
    )
    .await
    .assert_error(409, "room_archived");
    send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "room:old", "body": "x", "client_key": "k3"}),
    )
    .await
    .assert_error(409, "room_archived");

    let outside = send(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-b", "body": "x", "client_key": "k4", "mentions": ["agent-c"]}),
    )
    .await;
    outside.assert_error(422, "mention_not_member");
    assert_eq!(outside.error_field(), Some("mentions"));
    assert_eq!(outside.body["error"]["details"]["agent_id"], "agent-c");
    let unregistered_post_as = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k5", "mentions": ["ghost"]}),
    )
    .await;
    unregistered_post_as.assert_error(422, "mention_not_member");
    assert_eq!(
        unregistered_post_as.body["error"]["details"]["agent_id"],
        "ghost"
    );
    assert_eq!(counts(&fx).await, before, "nothing was written");
    drop(ag);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn step_18_responds_with_the_new_message_shape() {
    let (_fx, server, api) = start().await;
    let ag = agents(&api).await;
    let reply = send(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-b", "body": "hi", "client_key": "k", "wake": false, "mentions": ["agent-b"]}),
    )
    .await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    let keys = api
        .keys_of(
            reqwest::Method::GET,
            &format!(
                "/v1/messages/{}",
                reply.body["message_id"].as_str().unwrap()
            ),
            &ag.a.api_key,
            None,
        )
        .await;
    assert_eq!(
        keys,
        [
            "message_id",
            "event_seq",
            "room_id",
            "room_kind",
            "sender_id",
            "body",
            "reply_to",
            "thread_id",
            "corrects",
            "wake",
            "mentions",
            "thread_summary",
            "reactions",
            "created_at"
        ],
        "S15 adds thread_id, corrects and thread_summary, S16 reactions (IF-WIRE-012)"
    );
    let sent_keys = api
        .keys_of(
            reqwest::Method::POST,
            "/v1/messages",
            &ag.a.api_key,
            Some(json!({"to": "agent-b", "body": "hi", "client_key": "k", "wake": false, "mentions": ["agent-b"]})),
        )
        .await;
    assert_eq!(sent_keys.last().map(String::as_str), Some("deduplicated"));
    assert_eq!(sent_keys[..14], keys[..]);
    assert_eq!(reply.body["room_id"], "dm:agent-a:agent-b");
    assert_eq!(reply.body["room_kind"], "dm");
    assert_eq!(reply.body["wake"], false);
    assert_eq!(reply.body["mentions"], json!(["agent-b"]));
    assert_eq!(reply.body["deduplicated"], false);

    let plain = sent(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "hi", "client_key": "k2", "mentions": []}),
    )
    .await;
    assert_eq!(plain["wake"], true, "wake defaults to true");
    assert_eq!(
        plain["mentions"],
        json!([]),
        "an empty list is kept, not omitted"
    );
    assert_eq!(plain["room_kind"], "all");
    drop(ag);
    server.shutdown().await.unwrap();
}

// ---- idempotency -------------------------------------------------------------------

#[tokio::test]
async fn idempotency_compares_the_resolved_room_wake_and_the_mention_set() {
    let (fx, server, api) = start().await;
    let ag = agents(&api).await;
    let original = sent(
        &api,
        &ag.a.api_key,
        json!({"to": "agent-b", "body": "hi", "client_key": "k", "mentions": ["agent-b"]}),
    )
    .await;

    let same_room = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "dm:agent-a:agent-b", "body": "hi", "client_key": "k", "mentions": ["agent-b"], "wake": true}),
    )
    .await;
    assert_eq!(same_room.status.as_u16(), 200, "{}", same_room.body);
    assert_eq!(same_room.body["message_id"], original["message_id"]);
    assert_eq!(same_room.body["deduplicated"], true);

    let before = counts(&fx).await;
    for changed in [
        json!({"to": "agent-b", "body": "hi", "client_key": "k", "mentions": ["agent-b"], "wake": false}),
        json!({"to": "agent-b", "body": "hi", "client_key": "k"}),
        json!({"to": "agent-c", "body": "hi", "client_key": "k", "mentions": ["agent-c"]}),
    ] {
        send(&api, &ag.a.api_key, changed)
            .await
            .assert_error(409, "idempotency_conflict");
    }

    let two = sent(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "set", "mentions": ["agent-c", "agent-b"]}),
    )
    .await;
    assert_eq!(
        two["mentions"],
        json!(["agent-b", "agent-c"]),
        "ASCII order"
    );
    let reordered = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "set", "mentions": ["agent-b", "agent-c"]}),
    )
    .await;
    assert_eq!(reordered.status.as_u16(), 200, "{}", reordered.body);
    assert_eq!(reordered.body["message_id"], two["message_id"]);
    assert_eq!(counts(&fx).await[0], before[0] + 1);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_retry_after_leaving_all_returns_the_original() {
    let (_fx, server, api) = start().await;
    let ag = agents(&api).await;
    let original = sent(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k"}),
    )
    .await;
    api.put(
        "/v1/agents/agent-a/membership/all",
        &ag.a.api_key,
        json!({"joined": false}),
    )
    .await;
    let retried = send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k"}),
    )
    .await;
    assert_eq!(retried.status.as_u16(), 200, "{}", retried.body);
    assert_eq!(retried.body["message_id"], original["message_id"]);
    send(
        &api,
        &ag.a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "new"}),
    )
    .await
    .assert_error(403, "not_room_member");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_posts_share_the_senders_own_namespace() {
    let (_fx, server, api) = start().await;
    let ag = agents(&api).await;
    let own = sent(
        &api,
        &ag.human.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "shared"}),
    )
    .await;
    let as_owner = send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "all", "body": "x", "client_key": "shared"}),
    )
    .await;
    assert_eq!(as_owner.status.as_u16(), 200, "{}", as_owner.body);
    assert_eq!(as_owner.body["message_id"], own["message_id"]);
    send(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "all", "body": "other", "client_key": "shared"}),
    )
    .await
    .assert_error(409, "idempotency_conflict");
    server.shutdown().await.unwrap();
}
