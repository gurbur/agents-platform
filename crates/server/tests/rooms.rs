//! S10: `GET /v1/rooms` and `POST /v1/rooms/{room_id}/messages` — the owner
//! lists every room and posts into any room as an agent named in
//! `[owner] post_as` (acceptance scenario 14), over HTTP with the database
//! inspected directly.

mod common;

use std::time::Duration;

use common::receiver::MockReceiver;
use common::{Api, Fixture, OWNER_TOKEN, RunningServer};
use platform_wire::messages::{MAX_BODY_BYTES, MAX_CLIENT_KEY_BYTES};
use reqwest::Method;
use serde_json::{Value, json};

/// Upper bound for a wake to arrive; only guards against a hang.
const WAIT: Duration = Duration::from_secs(20);

/// The allowlist the tests configure unless they check the default.
const POST_AS: &[&str] = &["human"];

async fn start_with(post_as: &[&str]) -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(post_as)).await;
    let api = server.api();
    (fx, server, api)
}

async fn start() -> (Fixture, RunningServer, Api) {
    start_with(POST_AS).await
}

fn post_path(room_id: &str) -> String {
    format!("/v1/rooms/{room_id}/messages")
}

fn post_body(sender_id: &str, body: &str, client_key: &str) -> Value {
    json!({"sender_id": sender_id, "body": body, "client_key": client_key})
}

fn seq(value: &Value) -> i64 {
    value
        .as_str()
        .expect("sequence string")
        .parse()
        .expect("decimal")
}

/// `agent_id`s holding an inbox row for `message_id`, in ASCII order.
async fn recipients(fx: &Fixture, message_id: &str) -> Vec<String> {
    let message_id = message_id.to_owned();
    fx.read_db(move |conn| {
        let mut statement = conn
            .prepare("SELECT agent_id FROM inbox_rows WHERE message_id = ?1 ORDER BY agent_id")?;
        let rows = statement
            .query_map([message_id], |row| row.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        Ok(rows)
    })
    .await
}

async fn message_count(fx: &Fixture) -> i64 {
    fx.read_db(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?))
        .await
}

/// `(state, target_seq)` of an agent's `notification_pending` row.
async fn pending(fx: &Fixture, agent_id: &str) -> Option<(String, i64)> {
    let agent_id = agent_id.to_owned();
    fx.read_db(move |conn| {
        let mut statement =
            conn.prepare("SELECT state, target_seq FROM notification_pending WHERE agent_id = ?1")?;
        let mut rows = statement.query_map([agent_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.next().transpose()?)
    })
    .await
}

async fn rooms(api: &Api) -> Vec<Value> {
    let reply = api.get("/v1/rooms", OWNER_TOKEN).await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    let keys: Vec<&str> = reply
        .body
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(keys, ["rooms"], "{}", reply.body);
    reply.body["rooms"].as_array().expect("rooms array").clone()
}

fn room_ids(rooms: &[Value]) -> Vec<&str> {
    rooms
        .iter()
        .map(|room| room["room_id"].as_str().unwrap())
        .collect()
}

fn room<'a>(rooms: &'a [Value], room_id: &str) -> &'a Value {
    rooms
        .iter()
        .find(|room| room["room_id"] == room_id)
        .unwrap_or_else(|| panic!("{room_id} missing from {rooms:?}"))
}

// ---- GET /v1/rooms -------------------------------------------------------------

#[tokio::test]
async fn rooms_list_is_owner_only_and_takes_no_query() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    api.send(Method::GET, "/v1/rooms", None, None)
        .await
        .assert_error(401, "unauthorized");
    api.get("/v1/rooms", &a.api_key)
        .await
        .assert_error(403, "forbidden");
    let query = api.get("/v1/rooms?limit=10", OWNER_TOKEN).await;
    query.assert_error(422, "unexpected_field");
    assert_eq!(query.error_field(), Some("limit"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_empty_hub_lists_only_all_with_null_last_fields() {
    let (_fx, server, api) = start().await;
    api.register("agent-b", None).await;
    api.register("agent-a", None).await;

    let rooms = rooms(&api).await;

    assert_eq!(
        rooms,
        [json!({
            "room_id": "all",
            "kind": "all",
            "members": ["agent-a", "agent-b"],
            "last_event_seq": null,
            "last_message_at": null,
            "last_sender_id": null,
            "message_count": 0
        })]
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn rooms_are_sorted_by_their_last_message_with_members_and_counts() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;

    api.sent(&a.api_key, "agent-b", "a to b", "k-1").await;
    // No `all` message yet: `all` sorts after every room with a message.
    let early = rooms(&api).await;
    assert_eq!(room_ids(&early), ["dm:agent-a:agent-b", "all"]);

    api.sent(&b.api_key, "agent-c", "b to c", "k-2").await;
    let broadcast = api.sent(&a.api_key, "all", "to all", "k-3").await;
    let last = api.sent(&c.api_key, "agent-b", "c to b", "k-4").await;
    let leave = api
        .put(
            "/v1/agents/agent-b/membership/all",
            &b.api_key,
            json!({"joined": false}),
        )
        .await;
    assert_eq!(leave.status.as_u16(), 200, "{}", leave.body);

    let rooms = rooms(&api).await;

    assert_eq!(
        room_ids(&rooms),
        ["dm:agent-b:agent-c", "all", "dm:agent-a:agent-b"],
        "newest last message first"
    );
    assert_eq!(
        room(&rooms, "dm:agent-b:agent-c"),
        &json!({
            "room_id": "dm:agent-b:agent-c",
            "kind": "dm",
            "members": ["agent-b", "agent-c"],
            "last_event_seq": last["event_seq"],
            "last_message_at": last["created_at"],
            "last_sender_id": "agent-c",
            "message_count": 2
        })
    );
    assert_eq!(
        room(&rooms, "all"),
        &json!({
            "room_id": "all",
            "kind": "all",
            "members": ["agent-a", "agent-c"],
            "last_event_seq": broadcast["event_seq"],
            "last_message_at": broadcast["created_at"],
            "last_sender_id": "agent-a",
            "message_count": 1
        }),
        "all lists the current members only"
    );
    let ab = room(&rooms, "dm:agent-a:agent-b");
    assert_eq!(ab["members"], json!(["agent-a", "agent-b"]));
    assert_eq!(ab["message_count"], 1);
    assert_eq!(ab["last_sender_id"], "agent-a");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn listing_rooms_does_not_touch_read_markers() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.sent(&a.api_key, "agent-b", "unread", "k-1").await;

    rooms(&api).await;

    let inbox = api.get("/v1/inbox", &b.api_key).await;
    assert_eq!(inbox.body["read_marker"], "0", "{}", inbox.body);
    assert_eq!(inbox.body["items"].as_array().unwrap().len(), 1);
    server.shutdown().await.unwrap();
}

// ---- Scenario 14: barge-in -----------------------------------------------------

#[tokio::test]
async fn barge_in_reaches_both_dm_members_and_not_the_sender() {
    let (fx, server, api) = start().await;
    let mock = MockReceiver::start().await;
    let a = api.register("agent-a", Some(&mock.url())).await;
    let b = api.register("agent-b", Some(&mock.url())).await;
    let human = api.register("human", None).await;

    let reply = api
        .post(
            &post_path("dm:agent-a:agent-b"),
            OWNER_TOKEN,
            post_body("human", "둘 다 잠깐, 배포는 내일로.", "barge-1"),
        )
        .await;

    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    let posted = &reply.body;
    let message_id = posted["message_id"].as_str().unwrap().to_owned();
    assert_eq!(posted["room_id"], "dm:agent-a:agent-b");
    assert_eq!(posted["sender_id"], "human");
    assert_eq!(posted["body"], "둘 다 잠깐, 배포는 내일로.");
    assert_eq!(posted["reply_to"], Value::Null);
    assert_eq!(posted["deduplicated"], false);

    assert_eq!(recipients(&fx, &message_id).await, ["agent-a", "agent-b"]);
    for agent in ["agent-a", "agent-b"] {
        assert_eq!(
            pending(&fx, agent).await,
            Some(("pending".to_owned(), 1)),
            "{agent} is scheduled for a wake at its new head"
        );
    }
    let wakes = mock.wait_for(2, WAIT).await;
    let mut woken: Vec<(String, String)> = wakes
        .iter()
        .map(|wake| {
            let body = wake.json();
            (
                body["agent_id"].as_str().unwrap().to_owned(),
                wake.inbox_seq(),
            )
        })
        .collect();
    woken.sort();
    woken.dedup();
    assert_eq!(
        woken,
        [
            ("agent-a".to_owned(), "1".to_owned()),
            ("agent-b".to_owned(), "1".to_owned())
        ]
    );

    let human_inbox = api.get("/v1/inbox", &human.api_key).await;
    assert_eq!(human_inbox.status.as_u16(), 200, "{}", human_inbox.body);
    assert_eq!(
        human_inbox.body["items"],
        json!([]),
        "the sender gets no row"
    );
    for agent in [&a, &b] {
        let inbox = api.get("/v1/inbox", &agent.api_key).await;
        let items = inbox.body["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{}", inbox.body);
        assert_eq!(items[0]["message_id"], message_id.as_str());
        assert_eq!(items[0]["sender_id"], "human");
        assert_eq!(items[0]["room_id"], "dm:agent-a:agent-b");
    }

    let history = api
        .get("/v1/messages?room_id=dm:agent-a:agent-b", OWNER_TOKEN)
        .await;
    assert_eq!(history.body["items"][0]["message_id"], message_id.as_str());
    let listed = rooms(&api).await;
    let dm = room(&listed, "dm:agent-a:agent-b");
    assert_eq!(dm["last_sender_id"], "human");
    assert_eq!(dm["last_event_seq"], posted["event_seq"]);
    assert_eq!(dm["members"], json!(["agent-a", "agent-b"]));

    let answer = api
        .post(
            "/v1/messages",
            &b.api_key,
            json!({"to": "agent-a", "body": "알겠습니다", "client_key": "b-1", "reply_to": message_id}),
        )
        .await;
    assert_eq!(answer.status.as_u16(), 201, "{}", answer.body);
    assert_eq!(answer.body["room_id"], "dm:agent-a:agent-b");
    assert_eq!(answer.body["reply_to"], message_id.as_str());
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn posting_into_a_dm_the_sender_belongs_to_reaches_only_the_other_member() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;
    api.register("human", None).await;

    let reply = api
        .post(
            &post_path("dm:agent-a:human"),
            OWNER_TOKEN,
            post_body("human", "hi", "k-1"),
        )
        .await;

    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    let message_id = reply.body["message_id"].as_str().unwrap();
    assert_eq!(recipients(&fx, message_id).await, ["agent-a"]);
    server.shutdown().await.unwrap();
}

// ---- Permissions ---------------------------------------------------------------

#[tokio::test]
async fn posting_is_owner_only_and_checked_before_the_body() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    let human = api.register("human", None).await;
    let path = post_path("dm:agent-a:agent-b");

    api.send(
        Method::POST,
        &path,
        None,
        Some(post_body("human", "x", "k-1")),
    )
    .await
    .assert_error(401, "unauthorized");
    let own_key = api
        .post(&path, &human.api_key, post_body("human", "x", "k-1"))
        .await;
    own_key.assert_error(403, "forbidden");
    assert_eq!(own_key.error_field(), None);
    api.send_raw(
        Method::POST,
        &path,
        Some(&human.api_key),
        Some("text/plain"),
        Some(b"not json".to_vec()),
    )
    .await
    .assert_error(403, "forbidden");

    assert_eq!(message_count(&fx).await, 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_sender_must_be_in_the_allowlist() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("human", None).await;

    for sender in ["agent-a", "Human", "owner", ""] {
        let reply = api
            .post(
                &post_path("dm:agent-a:agent-b"),
                OWNER_TOKEN,
                post_body(sender, "x", "k-1"),
            )
            .await;
        reply.assert_error(403, "forbidden");
        assert_eq!(reply.error_field(), Some("sender_id"), "{sender:?}");
    }
    assert_eq!(message_count(&fx).await, 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn posting_is_off_by_default() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    api.register("human", None).await;

    let reply = api
        .post(
            &post_path("all"),
            OWNER_TOKEN,
            post_body("human", "x", "k-1"),
        )
        .await;

    reply.assert_error(403, "forbidden");
    assert_eq!(reply.error_field(), Some("sender_id"));
    assert_eq!(message_count(&fx).await, 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_allowlisted_sender_must_be_registered() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;

    let reply = api
        .post(
            &post_path("all"),
            OWNER_TOKEN,
            post_body("human", "x", "k-1"),
        )
        .await;

    reply.assert_error(404, "agent_not_found");
    assert_eq!(reply.error_field(), Some("sender_id"));
    assert_eq!(message_count(&fx).await, 0);
    server.shutdown().await.unwrap();
}

// ---- Room checks ---------------------------------------------------------------

#[tokio::test]
async fn the_room_must_be_canonical_and_its_members_registered() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;
    api.register("human", None).await;

    for room_id in [
        "dm:human:agent-a",
        "dm:agent-a:agent-a",
        "nope",
        "dm:agent-a",
        "ALL",
    ] {
        let reply = api
            .post(
                &post_path(room_id),
                OWNER_TOKEN,
                post_body("human", "x", "k-1"),
            )
            .await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("room_id"), "{room_id}");
    }
    let unknown = api
        .post(
            &post_path("dm:agent-a:ghost"),
            OWNER_TOKEN,
            post_body("human", "x", "k-1"),
        )
        .await;
    unknown.assert_error(404, "unknown_room");
    assert_eq!(message_count(&fx).await, 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn room_format_then_allowlist_then_room_existence() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;
    api.register("human", None).await;

    let bad_room_and_sender = api
        .post(
            &post_path("nope"),
            OWNER_TOKEN,
            post_body("agent-a", "x", "k-1"),
        )
        .await;
    bad_room_and_sender.assert_error(422, "invalid_field");
    assert_eq!(bad_room_and_sender.error_field(), Some("room_id"));

    let unknown_room_and_sender = api
        .post(
            &post_path("dm:agent-a:ghost"),
            OWNER_TOKEN,
            post_body("agent-a", "x", "k-1"),
        )
        .await;
    unknown_room_and_sender.assert_error(403, "forbidden");
    assert_eq!(unknown_room_and_sender.error_field(), Some("sender_id"));

    let bad_body_and_room = api
        .post(
            &post_path("nope"),
            OWNER_TOKEN,
            json!({"sender_id": "human"}),
        )
        .await;
    bad_body_and_room.assert_error(422, "invalid_field");
    assert_ne!(
        bad_body_and_room.error_field(),
        Some("room_id"),
        "body first"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn reply_to_must_name_a_message_in_the_same_room() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("human", None).await;
    let in_all = api.sent(&a.api_key, "all", "in all", "k-all").await;
    let in_dm = api.sent(&a.api_key, "agent-b", "in dm", "k-dm").await;
    let path = post_path("dm:agent-a:agent-b");
    let with_reply = |key: &str, reply_to: &Value| json!({"sender_id": "human", "body": "x", "client_key": key, "reply_to": reply_to});

    api.post(&path, OWNER_TOKEN, with_reply("k-1", &in_all["message_id"]))
        .await
        .assert_error(422, "reply_to_room_mismatch");
    api.post(
        &path,
        OWNER_TOKEN,
        with_reply("k-2", &json!("msg-01J00000000000000000000000")),
    )
    .await
    .assert_error(404, "reply_to_not_found");
    let malformed = api
        .post(&path, OWNER_TOKEN, with_reply("k-3", &json!("not-an-id")))
        .await;
    malformed.assert_error(422, "invalid_field");
    assert_eq!(malformed.error_field(), Some("reply_to"));
    assert_eq!(message_count(&fx).await, 2, "rejections store nothing");

    let ok = api
        .post(&path, OWNER_TOKEN, with_reply("k-4", &in_dm["message_id"]))
        .await;
    assert_eq!(ok.status.as_u16(), 201, "{}", ok.body);
    assert_eq!(ok.body["reply_to"], in_dm["message_id"]);
    server.shutdown().await.unwrap();
}

// ---- Idempotency ---------------------------------------------------------------

#[tokio::test]
async fn a_repeated_post_returns_the_original_and_a_changed_one_conflicts() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("human", None).await;
    let path = post_path("dm:agent-a:agent-b");

    let first = api
        .post(&path, OWNER_TOKEN, post_body("human", "once", "k-1"))
        .await;
    assert_eq!(first.status.as_u16(), 201, "{}", first.body);
    let again = api
        .post(&path, OWNER_TOKEN, post_body("human", "once", "k-1"))
        .await;
    assert_eq!(again.status.as_u16(), 200, "{}", again.body);
    assert_eq!(again.body["deduplicated"], true);
    assert_eq!(again.body["message_id"], first.body["message_id"]);
    assert_eq!(again.body["event_seq"], first.body["event_seq"]);

    api.post(&path, OWNER_TOKEN, post_body("human", "changed", "k-1"))
        .await
        .assert_error(409, "idempotency_conflict");
    api.post(
        &post_path("all"),
        OWNER_TOKEN,
        post_body("human", "once", "k-1"),
    )
    .await
    .assert_error(409, "idempotency_conflict");
    assert_eq!(message_count(&fx).await, 1);
    let message_id = first.body["message_id"].as_str().unwrap();
    assert_eq!(recipients(&fx, message_id).await, ["agent-a", "agent-b"]);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn posts_share_the_senders_own_client_key_namespace() {
    let (fx, server, api) = start().await;
    api.register("agent-a", None).await;
    let human = api.register("human", None).await;

    let posted = api
        .post(
            &post_path("all"),
            OWNER_TOKEN,
            post_body("human", "hello", "k-all"),
        )
        .await;
    assert_eq!(posted.status.as_u16(), 201, "{}", posted.body);
    let own_send = api
        .send_message(&human.api_key, "all", "hello", "k-all")
        .await;
    assert_eq!(own_send.status.as_u16(), 200, "{}", own_send.body);
    assert_eq!(own_send.body["message_id"], posted.body["message_id"]);
    assert_eq!(own_send.body["deduplicated"], true);

    let dm_post = api
        .post(
            &post_path("dm:agent-a:human"),
            OWNER_TOKEN,
            post_body("human", "dm", "k-dm"),
        )
        .await;
    assert_eq!(dm_post.status.as_u16(), 201, "{}", dm_post.body);
    api.send_message(&human.api_key, "agent-a", "dm", "k-dm")
        .await
        .assert_error(409, "idempotency_conflict");
    assert_eq!(message_count(&fx).await, 2);
    server.shutdown().await.unwrap();
}

// ---- Posting into `all` ----------------------------------------------------------

#[tokio::test]
async fn the_owner_posts_into_all_without_the_sender_being_a_member() {
    let (fx, server, api) = start().await;
    let human = api.register("human", None).await;
    api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    let leave = api
        .put(
            "/v1/agents/human/membership/all",
            &human.api_key,
            json!({"joined": false}),
        )
        .await;
    assert_eq!(leave.status.as_u16(), 200, "{}", leave.body);
    api.send_message(&human.api_key, "all", "own", "k-own")
        .await
        .assert_error(403, "not_all_member");

    let away = api
        .post(
            &post_path("all"),
            OWNER_TOKEN,
            post_body("human", "away", "k-1"),
        )
        .await;
    assert_eq!(away.status.as_u16(), 201, "{}", away.body);
    assert_eq!(away.body["room_id"], "all");
    let away_id = away.body["message_id"].as_str().unwrap();
    assert_eq!(recipients(&fx, away_id).await, ["agent-a", "agent-b"]);

    let rejoin = api
        .put(
            "/v1/agents/human/membership/all",
            &human.api_key,
            json!({"joined": true}),
        )
        .await;
    assert_eq!(rejoin.status.as_u16(), 200, "{}", rejoin.body);
    let back = api
        .post(
            &post_path("all"),
            OWNER_TOKEN,
            post_body("human", "back", "k-2"),
        )
        .await;
    assert_eq!(back.status.as_u16(), 201, "{}", back.body);
    let back_id = back.body["message_id"].as_str().unwrap();
    assert_eq!(
        recipients(&fx, back_id).await,
        ["agent-a", "agent-b", "human"],
        "a joined sender gets its own row, like an all broadcast"
    );
    server.shutdown().await.unwrap();
}

// ---- Body shape ----------------------------------------------------------------

#[tokio::test]
async fn the_body_follows_the_send_rules() {
    let (fx, server, api) = start().await;
    api.register("human", None).await;
    let path = post_path("all");

    let unexpected = api
        .post(
            &path,
            OWNER_TOKEN,
            json!({"sender_id": "human", "body": "x", "client_key": "k", "to": "all"}),
        )
        .await;
    unexpected.assert_error(422, "unexpected_field");
    assert_eq!(unexpected.error_field(), Some("to"));

    let missing = api
        .post(&path, OWNER_TOKEN, json!({"body": "x", "client_key": "k"}))
        .await;
    missing.assert_error(422, "invalid_field");

    api.post(&path, OWNER_TOKEN, json!(["human", "x", "k"]))
        .await
        .assert_error(422, "invalid_field");

    for key in ["", "키", &"k".repeat(MAX_CLIENT_KEY_BYTES + 1)] {
        let reply = api
            .post(&path, OWNER_TOKEN, post_body("human", "x", key))
            .await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("client_key"), "{key:?}");
    }

    let large = "x".repeat(MAX_BODY_BYTES + 1);
    api.post(&path, OWNER_TOKEN, post_body("human", &large, "k"))
        .await
        .assert_error(413, "body_too_large");
    let limit = "x".repeat(MAX_BODY_BYTES);
    let at_limit = api
        .post(&path, OWNER_TOKEN, post_body("human", &limit, "k"))
        .await;
    assert_eq!(at_limit.status.as_u16(), 201, "{}", at_limit.body);

    api.send_raw(
        Method::POST,
        &path,
        Some(OWNER_TOKEN),
        Some("text/plain"),
        Some(b"{}".to_vec()),
    )
    .await
    .assert_error(415, "unsupported_media_type");
    assert_eq!(message_count(&fx).await, 1);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_post_carries_the_event_and_inbox_sequences_of_a_send() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("human", None).await;
    let before = api.sent(&a.api_key, "agent-b", "first", "k-1").await;

    let posted = api
        .post(
            &post_path("dm:agent-a:agent-b"),
            OWNER_TOKEN,
            post_body("human", "second", "k-2"),
        )
        .await;

    assert_eq!(posted.status.as_u16(), 201, "{}", posted.body);
    assert_eq!(
        seq(&posted.body["event_seq"]),
        seq(&before["event_seq"]) + 1
    );
    let heads: Vec<(String, i64)> = fx
        .read_db(|conn| {
            let mut statement =
                conn.prepare("SELECT agent_id, inbox_head FROM agents ORDER BY agent_id")?;
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await;
    assert_eq!(
        heads,
        [
            ("agent-a".to_owned(), 1),
            ("agent-b".to_owned(), 2),
            ("human".to_owned(), 0)
        ]
    );
    server.shutdown().await.unwrap();
}
