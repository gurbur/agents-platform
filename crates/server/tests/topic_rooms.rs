//! S13 rooms (`IF-WIRE-011`, ADR-0010): topic room management by the
//! owner, `GET /v1/rooms` for both principals, history permission from
//! `memberships`, and DM membership restored on re-registration.

mod common;

use common::{Api, Fixture, OWNER_TOKEN, Reply, RunningServer};
use platform_wire::MAX_ROOM_TITLE_CHARS;
use reqwest::Method;
use serde_json::{Value, json};

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    (fx, server, api)
}

async fn create(api: &Api, token: &str, body: Value) -> Reply {
    api.post("/v1/rooms", token, body).await
}

fn member_path(room_id: &str, agent_id: &str) -> String {
    format!("/v1/rooms/{room_id}/members/{agent_id}")
}

async fn invite(api: &Api, token: &str, room_id: &str, agent_id: &str) -> Reply {
    api.send(
        Method::PUT,
        &member_path(room_id, agent_id),
        Some(token),
        None,
    )
    .await
}

async fn remove(api: &Api, token: &str, room_id: &str, agent_id: &str) -> Reply {
    api.send(
        Method::DELETE,
        &member_path(room_id, agent_id),
        Some(token),
        None,
    )
    .await
}

async fn archive(api: &Api, token: &str, room_id: &str) -> Reply {
    api.send(
        Method::POST,
        &format!("/v1/rooms/{room_id}/archive"),
        Some(token),
        None,
    )
    .await
}

async fn send(api: &Api, token: &str, body: Value) -> Reply {
    api.post("/v1/messages", token, body).await
}

async fn sent(api: &Api, token: &str, body: Value) -> Value {
    let reply = send(api, token, body).await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    reply.body
}

async fn history(api: &Api, token: &str, room_id: &str) -> Reply {
    api.get(&format!("/v1/messages?room_id={room_id}"), token)
        .await
}

async fn rooms(api: &Api, token: &str) -> Vec<Value> {
    let reply = api.get("/v1/rooms", token).await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    reply.body["rooms"].as_array().unwrap().clone()
}

fn ids(rooms: &[Value]) -> Vec<&str> {
    rooms
        .iter()
        .map(|room| room["room_id"].as_str().unwrap())
        .collect()
}

const SUMMARY_KEYS: [&str; 11] = [
    "room_id",
    "kind",
    "title",
    "members",
    "created_by",
    "created_at",
    "archived_at",
    "last_event_seq",
    "last_message_at",
    "last_sender_id",
    "message_count",
];

// ---- POST /v1/rooms -------------------------------------------------------------------

#[tokio::test]
async fn the_owner_creates_a_topic_room() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;

    let created = create(
        &api,
        OWNER_TOKEN,
        json!({"room_id": "room:launch", "title": "  출시 준비 "}),
    )
    .await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    let listed = api
        .keys_of(Method::GET, "/v1/rooms", OWNER_TOKEN, None)
        .await;
    assert_eq!(listed, ["rooms"]);
    let archived_keys = api
        .keys_of(
            Method::POST,
            "/v1/rooms/room:launch/archive",
            OWNER_TOKEN,
            None,
        )
        .await;
    assert_eq!(archived_keys, SUMMARY_KEYS);
    assert_eq!(created.body["room_id"], "room:launch");
    assert_eq!(created.body["kind"], "topic");
    assert_eq!(created.body["title"], "출시 준비", "trimmed");
    assert_eq!(created.body["members"], json!([]));
    assert_eq!(created.body["created_by"], "owner");
    assert!(created.body["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(created.body["archived_at"], Value::Null);
    assert_eq!(created.body["last_event_seq"], Value::Null);
    assert_eq!(created.body["message_count"], 0);

    let untitled = create(
        &api,
        OWNER_TOKEN,
        json!({"room_id": "room:x_1", "title": null}),
    )
    .await;
    assert_eq!(untitled.status.as_u16(), 201, "{}", untitled.body);
    assert_eq!(untitled.body["title"], Value::Null);

    create(&api, OWNER_TOKEN, json!({"room_id": "room:launch"}))
        .await
        .assert_error(409, "room_id_taken");
    create(&api, &a.api_key, json!({"room_id": "room:other"}))
        .await
        .assert_error(403, "forbidden");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn room_creation_validates_in_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    // 403 comes before the body is read.
    let agent = api
        .send_raw(
            Method::POST,
            "/v1/rooms",
            Some(&a.api_key),
            Some("text/plain"),
            Some(b"x".to_vec()),
        )
        .await;
    agent.assert_error(403, "forbidden");

    create(
        &api,
        OWNER_TOKEN,
        json!({"room_id": "room:a", "members": []}),
    )
    .await
    .assert_error(422, "unexpected_field");
    let missing = create(&api, OWNER_TOKEN, json!({"title": "x"})).await;
    missing.assert_error(422, "invalid_field");
    assert_eq!(missing.error_field(), Some("room_id"));

    for room_id in [
        "all",
        "dm:agent-a:agent-b",
        "room:Launch",
        "room:",
        "room:-x",
        "launch",
    ] {
        let reply = create(&api, OWNER_TOKEN, json!({"room_id": room_id, "title": ""})).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("room_id"), "{room_id}");
    }
    let too_long = "가".repeat(MAX_ROOM_TITLE_CHARS + 1);
    for title in [
        json!(""),
        json!("   "),
        json!("a\tb"),
        json!(too_long),
        json!(5),
    ] {
        let reply = create(
            &api,
            OWNER_TOKEN,
            json!({"room_id": "room:t", "title": title}),
        )
        .await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("title"), "{title}");
    }
    let at_limit = "가".repeat(MAX_ROOM_TITLE_CHARS);
    let reply = create(
        &api,
        OWNER_TOKEN,
        json!({"room_id": "room:t", "title": at_limit}),
    )
    .await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    server.shutdown().await.unwrap();
}

// ---- members and archive ----------------------------------------------------------------

#[tokio::test]
async fn invite_remove_and_archive_follow_the_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    create(&api, OWNER_TOKEN, json!({"room_id": "room:launch"})).await;

    let invited = invite(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    assert_eq!(invited.status.as_u16(), 200, "{}", invited.body);
    assert_eq!(invited.body.as_object().unwrap().len(), SUMMARY_KEYS.len());
    assert_eq!(invited.body["members"], json!(["agent-a"]));
    let again = invite(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    assert_eq!(again.status.as_u16(), 200, "a no-op");
    assert_eq!(again.body, invited.body);

    invite(&api, &a.api_key, "room:launch", "agent-a")
        .await
        .assert_error(403, "forbidden");
    for room in ["all", "dm:agent-a:agent-b", "room:Bad"] {
        let reply = invite(&api, OWNER_TOKEN, room, "agent-a").await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("room_id"));
    }
    invite(&api, OWNER_TOKEN, "room:launch", "Not-A-Slug")
        .await
        .assert_error(404, "agent_not_found");
    invite(&api, OWNER_TOKEN, "room:missing", "agent-a")
        .await
        .assert_error(404, "unknown_room");
    invite(&api, OWNER_TOKEN, "room:launch", "agent-zz")
        .await
        .assert_error(404, "agent_not_found");

    let removed = remove(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    assert_eq!(removed.status.as_u16(), 204);
    assert_eq!(removed.body, Value::Null);
    let no_op = remove(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    assert_eq!(no_op.status.as_u16(), 204);
    remove(&api, &a.api_key, "room:launch", "agent-a")
        .await
        .assert_error(403, "forbidden");
    remove(&api, OWNER_TOKEN, "all", "agent-a")
        .await
        .assert_error(422, "invalid_field");

    archive(&api, &a.api_key, "room:launch")
        .await
        .assert_error(403, "forbidden");
    archive(&api, OWNER_TOKEN, "all")
        .await
        .assert_error(422, "invalid_field");
    archive(&api, OWNER_TOKEN, "room:missing")
        .await
        .assert_error(404, "unknown_room");
    let archived = archive(&api, OWNER_TOKEN, "room:launch").await;
    assert_eq!(archived.status.as_u16(), 200, "{}", archived.body);
    let archived_at = archived.body["archived_at"].as_str().unwrap().to_owned();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    let twice = archive(&api, OWNER_TOKEN, "room:launch").await;
    assert_eq!(twice.status.as_u16(), 200);
    assert_eq!(twice.body["archived_at"], archived_at, "kept");

    invite(&api, OWNER_TOKEN, "room:launch", "agent-a")
        .await
        .assert_error(409, "room_archived");
    remove(&api, OWNER_TOKEN, "room:launch", "agent-a")
        .await
        .assert_error(409, "room_archived");
    server.shutdown().await.unwrap();
}

// ---- reading -------------------------------------------------------------------------------

#[tokio::test]
async fn topic_history_follows_the_membership_row() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let c = api.register("agent-c", None).await;
    create(&api, OWNER_TOKEN, json!({"room_id": "room:launch"})).await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    let early = sent(
        &api,
        &a.api_key,
        json!({"room_id": "room:launch", "body": "early", "client_key": "k"}),
    )
    .await;

    history(&api, &c.api_key, "room:launch")
        .await
        .assert_error(403, "forbidden");
    let single = format!("/v1/messages/{}", early["message_id"].as_str().unwrap());
    api.get(&single, &c.api_key)
        .await
        .assert_error(404, "message_not_found");

    invite(&api, OWNER_TOKEN, "room:launch", "agent-c").await;
    let read = history(&api, &c.api_key, "room:launch").await;
    assert_eq!(read.status.as_u16(), 200, "{}", read.body);
    assert_eq!(read.body["items"][0]["message_id"], early["message_id"]);
    let inbox = api.get("/v1/inbox", &c.api_key).await;
    assert_eq!(inbox.body["items"], json!([]), "no backfill into the inbox");

    remove(&api, OWNER_TOKEN, "room:launch", "agent-c").await;
    history(&api, &c.api_key, "room:launch")
        .await
        .assert_error(403, "forbidden");
    send(
        &api,
        &c.api_key,
        json!({"room_id": "room:launch", "body": "x", "client_key": "c"}),
    )
    .await
    .assert_error(403, "not_room_member");

    archive(&api, OWNER_TOKEN, "room:launch").await;
    let archived = history(&api, &a.api_key, "room:launch").await;
    assert_eq!(
        archived.status.as_u16(),
        200,
        "an archived room is readable"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn all_and_dm_history_and_rooms_that_do_not_exist() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    api.put(
        "/v1/agents/agent-c/membership/all",
        &c.api_key,
        json!({"joined": false}),
    )
    .await;
    let left = history(&api, &c.api_key, "all").await;
    assert_eq!(left.status.as_u16(), 200, "leaving all keeps reading");

    // Before the first send the DM room does not exist.
    history(&api, OWNER_TOKEN, "dm:agent-a:agent-b")
        .await
        .assert_error(404, "unknown_room");
    history(&api, &a.api_key, "dm:agent-a:agent-b")
        .await
        .assert_error(403, "forbidden");
    history(&api, OWNER_TOKEN, "room:missing")
        .await
        .assert_error(404, "unknown_room");
    history(&api, &a.api_key, "room:missing")
        .await
        .assert_error(403, "forbidden");

    sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "x", "client_key": "k"}),
    )
    .await;
    for token in [&a.api_key, &b.api_key] {
        let reply = history(&api, token, "dm:agent-a:agent-b").await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }
    history(&api, &c.api_key, "dm:agent-a:agent-b")
        .await
        .assert_error(403, "forbidden");
    assert_eq!(
        history(&api, OWNER_TOKEN, "dm:agent-a:agent-b")
            .await
            .status
            .as_u16(),
        200
    );
    // The permission step still comes before cursor checks.
    api.get(
        "/v1/messages?room_id=room:missing&before_event_seq=1&after_event_seq=1",
        &a.api_key,
    )
    .await
    .assert_error(403, "forbidden");
    server.shutdown().await.unwrap();
}

// ---- GET /v1/rooms --------------------------------------------------------------------------

#[tokio::test]
async fn agents_list_their_own_rooms_and_the_owner_lists_every_room() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    create(
        &api,
        OWNER_TOKEN,
        json!({"room_id": "room:quiet", "title": "조용"}),
    )
    .await;
    create(&api, OWNER_TOKEN, json!({"room_id": "room:launch"})).await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-c").await;
    sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "x", "client_key": "k1"}),
    )
    .await;
    sent(
        &api,
        &a.api_key,
        json!({"room_id": "room:launch", "body": "x", "client_key": "k2"}),
    )
    .await;
    api.put(
        "/v1/agents/agent-c/membership/all",
        &c.api_key,
        json!({"joined": false}),
    )
    .await;

    let owner = rooms(&api, OWNER_TOKEN).await;
    assert_eq!(
        ids(&owner),
        ["room:launch", "dm:agent-a:agent-b", "room:quiet", "all"],
        "rooms with messages by last_event_seq, then the rest by created_at"
    );
    for room in &owner {
        assert_eq!(room.as_object().unwrap().len(), SUMMARY_KEYS.len());
    }
    assert_eq!(owner[0]["members"], json!(["agent-a", "agent-c"]));
    assert_eq!(owner[1]["members"], json!(["agent-a", "agent-b"]));
    assert_eq!(owner[1]["created_by"], "agent-a");
    assert_eq!(owner[2]["title"], "조용");
    assert_eq!(owner[2]["last_event_seq"], Value::Null);
    assert_eq!(owner[2]["message_count"], 0);
    assert_eq!(owner[3]["members"], json!(["agent-a", "agent-b"]));
    assert_eq!(owner[3]["created_by"], "owner");

    let mine = rooms(&api, &c.api_key).await;
    assert_eq!(
        ids(&mine),
        ["room:launch", "all"],
        "a left all is still listed; no foreign DM, no uninvited topic"
    );
    api.get("/v1/rooms?x=1", &a.api_key)
        .await
        .assert_error(422, "unexpected_field");

    remove(&api, OWNER_TOKEN, "room:launch", "agent-c").await;
    assert_eq!(ids(&rooms(&api, &c.api_key).await), ["all"]);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_deleted_member_stays_in_dm_members_and_leaves_topic_members() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let z = api.register("agent-z", None).await;
    create(&api, OWNER_TOKEN, json!({"room_id": "room:launch"})).await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-z").await;
    sent(
        &api,
        &z.api_key,
        json!({"to": "agent-a", "body": "x", "client_key": "k"}),
    )
    .await;
    api.send(
        Method::DELETE,
        "/v1/agents/agent-z",
        Some(OWNER_TOKEN),
        None,
    )
    .await;

    let owner = rooms(&api, OWNER_TOKEN).await;
    let dm = owner
        .iter()
        .find(|room| room["room_id"] == "dm:agent-a:agent-z")
        .unwrap();
    assert_eq!(dm["members"], json!(["agent-a", "agent-z"]));
    let topic = owner
        .iter()
        .find(|room| room["room_id"] == "room:launch")
        .unwrap();
    assert_eq!(topic["members"], json!(["agent-a"]));
    drop(a);
    server.shutdown().await.unwrap();
}

// ---- re-registration ----------------------------------------------------------------------

#[tokio::test]
async fn a_re_registered_agent_gets_its_dms_back_but_not_its_topics() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let z = api.register("agent-z", None).await;
    create(&api, OWNER_TOKEN, json!({"room_id": "room:launch"})).await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-a").await;
    invite(&api, OWNER_TOKEN, "room:launch", "agent-z").await;
    let old = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-z", "body": "old", "client_key": "k"}),
    )
    .await;
    api.send(
        Method::DELETE,
        "/v1/agents/agent-z",
        Some(OWNER_TOKEN),
        None,
    )
    .await;

    let z = {
        drop(z);
        api.register("agent-z", None).await
    };
    let read = history(&api, &z.api_key, "dm:agent-a:agent-z").await;
    assert_eq!(read.status.as_u16(), 200, "{}", read.body);
    assert_eq!(read.body["items"][0]["message_id"], old["message_id"]);
    sent(
        &api,
        &z.api_key,
        json!({"room_id": "dm:agent-a:agent-z", "body": "back", "client_key": "k"}),
    )
    .await;
    history(&api, &z.api_key, "room:launch")
        .await
        .assert_error(403, "forbidden");
    assert_eq!(
        ids(&rooms(&api, &z.api_key).await),
        ["dm:agent-a:agent-z", "all"]
    );
    server.shutdown().await.unwrap();
}
