//! S16 reactions (`IF-WIRE-016`, `IF-WIRE-012`, `IF-DLV-001` S16,
//! ADR-0014, API_V1 "반응"): `PUT`/`DELETE
//! /v1/messages/{id}/reactions/{kind}` in the `IF-WIRE-016` order, their
//! idempotence on the current-state table and the append-only event log,
//! the author's quiet `reaction` inbox event and its wake set, the
//! `reactions` summary on every message, and the change feed
//! `GET /v1/reactions`.
//!
//! Without receivers a woken agent shows as a `pending` notification row.

mod common;

use std::collections::BTreeSet;

use common::{Api, Fixture, OWNER_TOKEN, Registered, Reply, RunningServer, ordered_keys};
use reqwest::Method;
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};

const MISSING_ID: &str = "msg-01J00000000000000000000000";

struct Hub {
    fx: Fixture,
    server: RunningServer,
    api: Api,
}

async fn start_with(post_as: &[&str]) -> Hub {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(post_as)).await;
    let api = server.api();
    Hub { fx, server, api }
}

async fn start() -> Hub {
    start_with(&["human"]).await
}

fn reaction_path(message_id: &str, kind: &str) -> String {
    format!("/v1/messages/{message_id}/reactions/{kind}")
}

/// `PUT …/reactions/{kind}` with a JSON body, or with no body at all.
async fn react(api: &Api, token: &str, message_id: &str, kind: &str, body: Option<Value>) -> Reply {
    api.send(
        Method::PUT,
        &reaction_path(message_id, kind),
        Some(token),
        body,
    )
    .await
}

async fn reacted(api: &Api, token: &str, message_id: &str, kind: &str) -> Value {
    let reply = react(api, token, message_id, kind, None).await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    reply.body
}

async fn unreact(
    api: &Api,
    token: &str,
    message_id: &str,
    kind: &str,
    body: Option<Value>,
) -> Reply {
    api.send(
        Method::DELETE,
        &reaction_path(message_id, kind),
        Some(token),
        body,
    )
    .await
}

async fn post(api: &Api, token: &str, body: Value) -> Value {
    let reply = api.post("/v1/messages", token, body).await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    reply.body
}

async fn post_all(api: &Api, agent: &Registered, body: &str) -> String {
    let key = format!("k-{}", ulid::Ulid::generate());
    let sent = post(
        api,
        &agent.api_key,
        json!({"room_id": "all", "body": body, "client_key": key}),
    )
    .await;
    sent["message_id"].as_str().unwrap().to_owned()
}

fn id(message: &Value) -> &str {
    message["message_id"].as_str().unwrap()
}

/// `reaction_events` rows `(change_seq, message_id, agent_id, kind,
/// removed, wake, created_at)` in order.
async fn events(fx: &Fixture) -> Vec<Vec<SqlValue>> {
    select(
        fx,
        "SELECT change_seq, message_id, agent_id, kind, removed, wake, created_at
         FROM reaction_events ORDER BY change_seq",
    )
    .await
}

async fn select(fx: &Fixture, sql: &'static str) -> Vec<Vec<SqlValue>> {
    fx.read_db(move |conn| {
        let mut statement = conn.prepare(sql)?;
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..columns)
                    .map(|index| row.get::<_, SqlValue>(index))
                    .collect()
            })?
            .collect::<Result<Vec<Vec<SqlValue>>, _>>()?;
        Ok(rows)
    })
    .await
}

/// `agents.inbox_head` of every agent, in ASCII order.
async fn heads(fx: &Fixture) -> Vec<(String, i64)> {
    fx.read_db(|conn| {
        let mut statement =
            conn.prepare("SELECT agent_id, inbox_head FROM agents ORDER BY agent_id")?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Agents whose notification row is `pending` (woken and not yet read).
async fn woken(fx: &Fixture) -> BTreeSet<String> {
    fx.read_db(|conn| {
        let mut statement = conn.prepare(
            "SELECT agent_id FROM notification_pending WHERE state = 'pending' ORDER BY agent_id",
        )?;
        let rows = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<BTreeSet<String>, _>>()?;
        Ok(rows)
    })
    .await
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

/// Reads every inbox row up to the head so each agent is idle again.
async fn ack_all(api: &Api, agents: &[&Registered]) {
    for agent in agents {
        let page = api.get("/v1/inbox?limit=100", &agent.api_key).await;
        let head = page.body["snapshot_head_seq"].as_str().unwrap().to_owned();
        let reply = api
            .put(
                "/v1/inbox/read-marker",
                &agent.api_key,
                json!({"through_seq": head}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }
}

/// The last item of an agent's inbox from the marker.
async fn last_inbox_item(api: &Api, agent: &Registered) -> Value {
    let page = api.get("/v1/inbox?limit=100", &agent.api_key).await;
    assert_eq!(page.status.as_u16(), 200, "{}", page.body);
    page.body["items"]
        .as_array()
        .unwrap()
        .last()
        .cloned()
        .unwrap()
}

async fn message(api: &Api, token: &str, message_id: &str) -> Value {
    let reply = api.get(&format!("/v1/messages/{message_id}"), token).await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    reply.body
}

/// `(kind, count, agents)` of a message's `reactions`.
fn summary(message: &Value) -> Vec<(String, u64, Vec<String>)> {
    message["reactions"]
        .as_array()
        .unwrap_or_else(|| panic!("no reactions in {message}"))
        .iter()
        .map(|entry| {
            (
                entry["kind"].as_str().unwrap().to_owned(),
                entry["count"].as_u64().unwrap(),
                entry["agents"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|agent| agent.as_str().unwrap().to_owned())
                    .collect(),
            )
        })
        .collect()
}

fn expected(entries: &[(&str, &[&str])]) -> Vec<(String, u64, Vec<String>)> {
    entries
        .iter()
        .map(|(kind, agents)| {
            (
                (*kind).to_owned(),
                agents.len() as u64,
                agents.iter().map(|agent| (*agent).to_owned()).collect(),
            )
        })
        .collect()
}

fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
}

// ---- PUT and DELETE: idempotence and state --------------------------------------------

#[tokio::test]
async fn a_new_reaction_is_201_a_repeat_200_and_removal_204_both_times() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let target = post_all(api, &a, "hello").await;

    let created = react(api, &b.api_key, &target, "thumbsup", None).await;
    assert_eq!(created.status.as_u16(), 201, "{}", created.body);
    let keys = api
        .keys_of(
            Method::PUT,
            &reaction_path(&target, "thumbsup"),
            &b.api_key,
            Some(json!({})),
        )
        .await;
    assert_eq!(
        keys,
        [
            "message_id",
            "agent_id",
            "kind",
            "change_seq",
            "created_at",
            "reactions"
        ]
    );
    assert_eq!(created.body["message_id"], target.as_str());
    assert_eq!(created.body["agent_id"], "agent-b");
    assert_eq!(created.body["kind"], "thumbsup");
    assert_eq!(created.body["change_seq"], "1");
    assert_eq!(
        created.body["reactions"],
        json!([{"kind": "thumbsup", "count": 1, "agents": ["agent-b"]}])
    );

    // Again, even with a different wake: 200, the stored values, no write.
    let again = react(
        api,
        &b.api_key,
        &target,
        "thumbsup",
        Some(json!({"wake": true})),
    )
    .await;
    assert_eq!(again.status.as_u16(), 200, "{}", again.body);
    assert_eq!(again.body, created.body);
    let first_event = vec![
        int(1),
        text(&target),
        text("agent-b"),
        text("thumbsup"),
        int(0),
        int(0),
        text(created.body["created_at"].as_str().unwrap()),
    ];
    assert_eq!(events(&hub.fx).await, std::slice::from_ref(&first_event));
    assert_eq!(
        summary(&message(api, &a.api_key, &target).await),
        expected(&[("thumbsup", &["agent-b"])])
    );

    // Removal: 204 without a body, a tombstone and a new change_seq.
    let removed = unreact(api, &b.api_key, &target, "thumbsup", None).await;
    assert_eq!(removed.status.as_u16(), 204);
    assert_eq!(removed.body, Value::Null);
    let state = select(
        &hub.fx,
        "SELECT agent_id, kind, removed, change_seq FROM reactions",
    )
    .await;
    assert_eq!(
        state,
        [vec![text("agent-b"), text("thumbsup"), int(1), int(2)]]
    );
    assert_eq!(events(&hub.fx).await.len(), 2);
    assert_eq!(
        message(api, &a.api_key, &target).await["reactions"],
        json!([])
    );
    let twice = unreact(api, &b.api_key, &target, "thumbsup", None).await;
    assert_eq!(twice.status.as_u16(), 204);
    assert_eq!(events(&hub.fx).await.len(), 2, "nothing written");
    let never = unreact(api, &b.api_key, &target, "eyes", None).await;
    assert_eq!(never.status.as_u16(), 204, "never there is 204 too");

    // Re-adding is new: 201 with a new change_seq; the old event stays.
    let readded = reacted(api, &b.api_key, &target, "thumbsup").await;
    assert_eq!(readded["change_seq"], "3");
    let log = events(&hub.fx).await;
    assert_eq!(log.len(), 3);
    assert_eq!(log[0], first_event, "the first event is unchanged");
    assert_eq!(log[1][4], int(1), "the removal event");
    assert_eq!(log[2][4], int(0));
    let state = select(&hub.fx, "SELECT removed, change_seq FROM reactions").await;
    assert_eq!(state, [vec![int(0), int(3)]]);
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn put_checks_run_in_the_documented_order() {
    let hub = start_with(&["human", "ghost"]).await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("human", None).await;
    let target = post_all(api, &a, "hello").await;
    let path = reaction_path(&target, "thumbsup");

    // 1
    api.send(Method::PUT, &path, None, None)
        .await
        .assert_error(401, "unauthorized");
    // 2 before 3: a malformed id is a missing message whatever the kind.
    react(api, &b.api_key, "nope", "Bad", None)
        .await
        .assert_error(404, "message_not_found");
    // 3 before the message's existence.
    let kind = react(api, &b.api_key, MISSING_ID, "Bad", None).await;
    kind.assert_error(422, "invalid_field");
    assert_eq!(kind.error_field(), Some("kind"));
    for slug in ["a", "1ab", "ab-c", &"a".repeat(33)] {
        let reply = react(api, &b.api_key, &target, slug, None).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("kind"), "{slug}");
    }
    // 4: a body, when there is one, is StrictJson.
    api.send_raw(
        Method::PUT,
        &path,
        Some(&b.api_key),
        Some("text/plain"),
        Some(b"{}".to_vec()),
    )
    .await
    .assert_error(415, "unsupported_media_type");
    api.send_raw(
        Method::PUT,
        &path,
        Some(&b.api_key),
        Some("application/json"),
        Some(b"{".to_vec()),
    )
    .await
    .assert_error(400, "invalid_json");
    react(api, &b.api_key, &target, "thumbsup", Some(json!([true])))
        .await
        .assert_error(422, "invalid_field");
    // 5
    let extra = react(
        api,
        &b.api_key,
        &target,
        "thumbsup",
        Some(json!({"emoji": "x"})),
    )
    .await;
    extra.assert_error(422, "unexpected_field");
    assert_eq!(extra.error_field(), Some("emoji"));
    for sender in [json!("human"), json!(5)] {
        let reply = react(
            api,
            &b.api_key,
            &target,
            "nosuchkind",
            Some(json!({"sender_id": sender})),
        )
        .await;
        reply.assert_error(422, "unexpected_field");
        assert_eq!(reply.error_field(), Some("sender_id"));
    }
    // 6
    let wake = react(
        api,
        &b.api_key,
        &target,
        "thumbsup",
        Some(json!({"wake": "yes"})),
    )
    .await;
    wake.assert_error(422, "invalid_field");
    assert_eq!(wake.error_field(), Some("wake"));
    for body in [None, Some(json!({})), Some(json!({"sender_id": 5}))] {
        let reply = react(api, OWNER_TOKEN, &target, "thumbsup", body.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("sender_id"), "{body:?}");
    }
    // 7 before the message's existence.
    let outside = react(
        api,
        OWNER_TOKEN,
        MISSING_ID,
        "thumbsup",
        Some(json!({"sender_id": "agent-a"})),
    )
    .await;
    outside.assert_error(403, "forbidden");
    assert_eq!(outside.error_field(), Some("sender_id"));
    // 8 before the message's existence.
    let ghost = react(
        api,
        OWNER_TOKEN,
        MISSING_ID,
        "thumbsup",
        Some(json!({"sender_id": "ghost"})),
    )
    .await;
    ghost.assert_error(404, "agent_not_found");
    assert_eq!(ghost.error_field(), Some("sender_id"));
    // 9 before the kind.
    react(api, &b.api_key, MISSING_ID, "nosuchkind", None)
        .await
        .assert_error(404, "message_not_found");
    // 11
    react(api, &b.api_key, &target, "nosuchkind", None)
        .await
        .assert_error(422, "unknown_reaction_kind");
    assert!(events(&hub.fx).await.is_empty(), "nothing was written");

    // 13: an empty body, an empty object and the owner's identity all work.
    let empty = api
        .send_raw(Method::PUT, &path, Some(&b.api_key), None, None)
        .await;
    assert_eq!(empty.status.as_u16(), 201, "{}", empty.body);
    let owner = react(
        api,
        OWNER_TOKEN,
        &target,
        "thumbsup",
        Some(json!({"sender_id": "human"})),
    )
    .await;
    assert_eq!(owner.status.as_u16(), 201, "{}", owner.body);
    assert_eq!(owner.body["agent_id"], "human");
    assert_eq!(
        owner.body["reactions"],
        json!([{"kind": "thumbsup", "count": 2, "agents": ["agent-b", "human"]}])
    );
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn delete_checks_the_same_steps_but_not_the_kind() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("human", None).await;
    let target = post_all(api, &a, "hello").await;

    api.send(Method::DELETE, &reaction_path(&target, "eyes"), None, None)
        .await
        .assert_error(401, "unauthorized");
    unreact(api, &b.api_key, "nope", "eyes", None)
        .await
        .assert_error(404, "message_not_found");
    let bad = unreact(api, &b.api_key, &target, "Eyes", None).await;
    bad.assert_error(422, "invalid_field");
    assert_eq!(bad.error_field(), Some("kind"));
    unreact(api, &b.api_key, MISSING_ID, "eyes", None)
        .await
        .assert_error(404, "message_not_found");
    // The kind's existence is not checked.
    let unknown = unreact(api, &b.api_key, &target, "nosuchkind", None).await;
    assert_eq!(unknown.status.as_u16(), 204);
    // An agent's body is not read at all.
    let ignored = api
        .send_raw(
            Method::DELETE,
            &reaction_path(&target, "eyes"),
            Some(&b.api_key),
            Some("text/plain"),
            Some(b"not json".to_vec()),
        )
        .await;
    assert_eq!(ignored.status.as_u16(), 204);

    // The owner names its identity in the body.
    for body in [None, Some(json!({}))] {
        let reply = unreact(api, OWNER_TOKEN, &target, "eyes", body).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("sender_id"));
    }
    let extra = unreact(
        api,
        OWNER_TOKEN,
        &target,
        "eyes",
        Some(json!({"sender_id": "human", "wake": true})),
    )
    .await;
    extra.assert_error(422, "unexpected_field");
    unreact(
        api,
        OWNER_TOKEN,
        &target,
        "eyes",
        Some(json!({"sender_id": "agent-a"})),
    )
    .await
    .assert_error(403, "forbidden");
    react(
        api,
        OWNER_TOKEN,
        &target,
        "eyes",
        Some(json!({"sender_id": "human"})),
    )
    .await;
    let removed = unreact(
        api,
        OWNER_TOKEN,
        &target,
        "eyes",
        Some(json!({"sender_id": "human"})),
    )
    .await;
    assert_eq!(removed.status.as_u16(), 204);
    let log = events(&hub.fx).await;
    assert_eq!(log.len(), 2);
    assert_eq!(
        log[1][2..6],
        [text("human"), text("eyes"), int(1), int(0)],
        "removed by the owner's identity, wake 0"
    );
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_archived_room_refuses_reactions_before_the_kind() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let room = api
        .post("/v1/rooms", OWNER_TOKEN, json!({"room_id": "room:old"}))
        .await;
    assert_eq!(room.status.as_u16(), 201, "{}", room.body);
    for agent in ["agent-a", "agent-b"] {
        let invite = api
            .put(
                &format!("/v1/rooms/room:old/members/{agent}"),
                OWNER_TOKEN,
                json!({}),
            )
            .await;
        assert_eq!(invite.status.as_u16(), 200, "{}", invite.body);
    }
    let sent = post(
        api,
        &a.api_key,
        json!({"room_id": "room:old", "body": "x", "client_key": "k"}),
    )
    .await;
    let target = id(&sent).to_owned();
    reacted(api, &b.api_key, &target, "eyes").await;
    let archive = api
        .send(
            Method::POST,
            "/v1/rooms/room:old/archive",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(archive.status.as_u16(), 200, "{}", archive.body);

    for kind in ["thumbsup", "nosuchkind"] {
        react(api, &b.api_key, &target, kind, None)
            .await
            .assert_error(409, "room_archived");
    }
    unreact(api, &b.api_key, &target, "eyes", None)
        .await
        .assert_error(409, "room_archived");
    // The reaction stays readable.
    assert_eq!(
        summary(&message(api, &a.api_key, &target).await),
        expected(&[("eyes", &["agent-b"])])
    );
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_an_agent_that_reads_the_message_may_react() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    let human = api.register("human", None).await;

    // Another pair's DM: hidden like a missing message.
    let dm = post(
        api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "dm"}),
    )
    .await;
    let dm_id = id(&dm).to_owned();
    react(api, &c.api_key, &dm_id, "thumbsup", None)
        .await
        .assert_error(404, "message_not_found");
    unreact(api, &c.api_key, &dm_id, "thumbsup", None)
        .await
        .assert_error(404, "message_not_found");
    reacted(api, &b.api_key, &dm_id, "thumbsup").await;
    // The owner always reads, whatever its identity's memberships.
    let owner = react(
        api,
        OWNER_TOKEN,
        &dm_id,
        "eyes",
        Some(json!({"sender_id": "human"})),
    )
    .await;
    assert_eq!(owner.status.as_u16(), 201, "{}", owner.body);

    // A topic room without human; human joins a thread there by barging in.
    let room = api
        .post("/v1/rooms", OWNER_TOKEN, json!({"room_id": "room:t"}))
        .await;
    assert_eq!(room.status.as_u16(), 201, "{}", room.body);
    for agent in ["agent-a", "agent-b"] {
        api.put(
            &format!("/v1/rooms/room:t/members/{agent}"),
            OWNER_TOKEN,
            json!({}),
        )
        .await;
    }
    let root = post(
        api,
        &a.api_key,
        json!({"room_id": "room:t", "body": "root", "client_key": "root"}),
    )
    .await;
    let other = post(
        api,
        &a.api_key,
        json!({"room_id": "room:t", "body": "other", "client_key": "other"}),
    )
    .await;
    let barge = post(
        api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "reply_to": id(&root), "body": "hi", "client_key": "barge"}),
    )
    .await;
    // A thread participant reads (and reacts to) the root and the replies.
    for target in [id(&root), id(&barge)] {
        reacted(api, &human.api_key, target, "eyes").await;
    }
    // Not the room's other messages.
    react(api, &human.api_key, id(&other), "eyes", None)
        .await
        .assert_error(404, "message_not_found");
    react(api, &c.api_key, id(&root), "eyes", None)
        .await
        .assert_error(404, "message_not_found");
    hub.server.shutdown().await.unwrap();
}

// ---- the author's inbox event and its wake ----------------------------------------------

const REACTION_EVENT_KEYS: [&str; 12] = [
    "inbox_seq",
    "kind",
    "woke",
    "change_seq",
    "message_id",
    "room_id",
    "room_kind",
    "thread_id",
    "sender_id",
    "reaction",
    "wake",
    "created_at",
];

#[tokio::test]
async fn the_author_alone_gets_one_quiet_reaction_event() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    let target = post_all(api, &a, "hello").await;
    ack_all(api, &[&a, &b, &c]).await;
    let before = heads(&hub.fx).await;

    let added = reacted(api, &b.api_key, &target, "thumbsup").await;
    let after = heads(&hub.fx).await;
    let head =
        |rows: &[(String, i64)], agent: &str| rows.iter().find(|(id, _)| id == agent).unwrap().1;
    assert_eq!(head(&after, "agent-a"), head(&before, "agent-a") + 1);
    assert_eq!(
        head(&after, "agent-b"),
        head(&before, "agent-b"),
        "not the reactor"
    );
    assert_eq!(
        head(&after, "agent-c"),
        head(&before, "agent-c"),
        "nor others"
    );
    assert!(woken(&hub.fx).await.is_empty(), "quiet by default");

    let page = api.get("/v1/inbox", &a.api_key).await;
    assert_eq!(
        page.body["read_marker"],
        head(&before, "agent-a").to_string()
    );
    let items = page.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{}", page.body);
    let event = &items[0];
    assert_eq!(
        *event,
        json!({
            "inbox_seq": head(&after, "agent-a").to_string(),
            "kind": "reaction",
            "woke": false,
            "change_seq": added["change_seq"],
            "message_id": target,
            "room_id": "all",
            "room_kind": "all",
            "thread_id": null,
            "sender_id": "agent-b",
            "reaction": "thumbsup",
            "wake": false,
            "created_at": added["created_at"],
        })
    );
    let raw = api
        .send_bytes(Method::GET, "/v1/inbox", Some(&a.api_key), &[], None)
        .await;
    let text = raw.text();
    let items_at = text.find("\"items\":[").unwrap() + "\"items\":[".len();
    let end = items_at + text[items_at..].find('}').unwrap() + 1;
    assert_eq!(ordered_keys(&text[items_at..end]), REACTION_EVENT_KEYS);

    // `wake: true` wakes the author; the event says so.
    let waking = react(
        api,
        &b.api_key,
        &target,
        "eyes",
        Some(json!({"wake": true})),
    )
    .await;
    assert_eq!(waking.status.as_u16(), 201, "{}", waking.body);
    assert_eq!(woken(&hub.fx).await, set(&["agent-a"]));
    let event = last_inbox_item(api, &a).await;
    assert_eq!(
        (event["woke"].clone(), event["wake"].clone()),
        (json!(true), json!(true))
    );
    assert_eq!(event["reaction"], "eyes");
    ack_all(api, &[&a]).await;

    // Reacting to one's own message, removal and a repeat make no event.
    let before = heads(&hub.fx).await;
    reacted(api, &a.api_key, &target, "ack").await;
    unreact(api, &b.api_key, &target, "thumbsup", None).await;
    react(
        api,
        &b.api_key,
        &target,
        "eyes",
        Some(json!({"wake": true})),
    )
    .await;
    assert_eq!(heads(&hub.fx).await, before);
    assert!(woken(&hub.fx).await.is_empty());

    // Snapshot paging walks reaction events like any other row.
    let first = api.get("/v1/inbox?after_seq=0&limit=1", &a.api_key).await;
    let until = first.body["snapshot_head_seq"].as_str().unwrap().to_owned();
    let mut after_seq = "0".to_owned();
    let mut kinds = Vec::new();
    loop {
        let page = api
            .get(
                &format!("/v1/inbox?after_seq={after_seq}&until_seq={until}&limit=1"),
                &a.api_key,
            )
            .await;
        for item in page.body["items"].as_array().unwrap() {
            kinds.push(item["kind"].as_str().unwrap().to_owned());
        }
        after_seq = page.body["next_after_seq"].as_str().unwrap().to_owned();
        if page.body["has_more"] == false {
            break;
        }
    }
    assert_eq!(kinds, ["message", "reaction", "reaction"]);
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_always_author_wakes_and_the_reactor_never_does() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let target = post_all(api, &a, "hello").await;
    ack_all(api, &[&a, &b]).await;
    for agent in ["agent-a", "agent-b"] {
        let reply = api
            .patch(
                &format!("/v1/agents/{agent}/receiver"),
                OWNER_TOKEN,
                json!({"wake_policy": "always"}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }

    reacted(api, &b.api_key, &target, "heart").await;
    assert_eq!(
        woken(&hub.fx).await,
        set(&["agent-a"]),
        "always, not the reactor"
    );
    let event = last_inbox_item(api, &a).await;
    assert_eq!(event["woke"], false, "woke is the request's wake");
    ack_all(api, &[&a]).await;

    // Its own reaction makes no row, so it does not wake even under always.
    reacted(api, &a.api_key, &target, "heart").await;
    assert!(woken(&hub.fx).await.is_empty());
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_departed_author_still_hears_and_a_deleted_one_does_not() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let d = api.register("agent-d", None).await;
    let from_a = post_all(api, &a, "a").await;
    let from_d = post_all(api, &d, "d").await;

    let left = api
        .put(
            "/v1/agents/agent-a/membership/all",
            &a.api_key,
            json!({"joined": false}),
        )
        .await;
    assert_eq!(left.status.as_u16(), 200, "{}", left.body);
    reacted(api, &b.api_key, &from_a, "thumbsup").await;
    assert_eq!(last_inbox_item(api, &a).await["kind"], "reaction");

    let deleted = api
        .send(
            Method::DELETE,
            "/v1/agents/agent-d",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    let reply = reacted(api, &b.api_key, &from_d, "thumbsup").await;
    assert_eq!(reply["reactions"][0]["agents"], json!(["agent-b"]));
    let rows = select(
        &hub.fx,
        "SELECT COUNT(*) FROM inbox_rows WHERE kind = 'reaction'",
    )
    .await;
    assert_eq!(rows, [vec![int(1)]], "only agent-a's event exists");
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_deleted_reactor_stays_in_the_summary() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let target = post_all(api, &a, "a").await;
    reacted(api, &b.api_key, &target, "eyes").await;
    let deleted = api
        .send(
            Method::DELETE,
            "/v1/agents/agent-b",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    assert_eq!(
        summary(&message(api, &a.api_key, &target).await),
        expected(&[("eyes", &["agent-b"])])
    );
    // a's reaction inbox row stays (only the deleted agent's rows go).
    assert_eq!(last_inbox_item(api, &a).await["sender_id"], "agent-b");
    hub.server.shutdown().await.unwrap();
}

// ---- summaries ----------------------------------------------------------------------------

const MESSAGE_KEYS: [&str; 14] = [
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
    "created_at",
];

#[tokio::test]
async fn every_message_carries_its_summary_in_registry_order() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    api.register("human", None).await;

    let sent = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"room_id": "all", "body": "x", "client_key": "x"}),
        )
        .await;
    assert_eq!(sent.body["reactions"], json!([]), "the send response");
    let target = id(&sent.body).to_owned();
    let kind = |slug: &str, glyph: &str, sort: i64| {
        let slug = slug.to_owned();
        let body = json!({"glyph": glyph, "label": slug, "sort": sort});
        async move {
            let reply = api
                .put(&format!("/v1/reaction-kinds/{slug}"), OWNER_TOKEN, body)
                .await;
            assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
        }
    };
    kind("party", "🎉", 15).await;
    kind("aaa_tie", "🅰️", 10).await;

    reacted(api, &c.api_key, &target, "claim").await;
    reacted(api, &c.api_key, &target, "ack").await;
    reacted(api, &b.api_key, &target, "ack").await;
    reacted(api, &b.api_key, &target, "party").await;
    reacted(api, &b.api_key, &target, "aaa_tie").await;
    reacted(api, &b.api_key, &target, "eyes").await;
    react(
        api,
        OWNER_TOKEN,
        &target,
        "heart",
        Some(json!({"sender_id": "human"})),
    )
    .await;
    unreact(api, &b.api_key, &target, "eyes", None).await;
    // A disabled kind's reactions stay.
    let off = api
        .put(
            "/v1/reaction-kinds/heart",
            OWNER_TOKEN,
            json!({"glyph": "❤️", "label": "고마워", "enabled": false}),
        )
        .await;
    assert_eq!(off.status.as_u16(), 200, "{}", off.body);

    let want = expected(&[
        ("aaa_tie", &["agent-b"]),
        ("ack", &["agent-b", "agent-c"]),
        ("party", &["agent-b"]),
        ("heart", &["human"]),
        ("claim", &["agent-c"]),
    ]);
    // Single, history, and an inbox message event.
    let single = message(api, &b.api_key, &target).await;
    assert_eq!(summary(&single), want);
    let history = api.get("/v1/messages?room_id=all", &b.api_key).await;
    let item = history.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["message_id"] == target.as_str())
        .unwrap();
    assert_eq!(summary(item), want);
    let page = api.get("/v1/inbox?after_seq=0", &c.api_key).await;
    let event = page.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["kind"] == "message" && item["message_id"] == target.as_str())
        .unwrap();
    assert_eq!(summary(event), want);

    // The message has fourteen keys, an inbox message event seventeen.
    let keys = api
        .keys_of(
            Method::GET,
            &format!("/v1/messages/{target}"),
            &b.api_key,
            None,
        )
        .await;
    assert_eq!(keys, MESSAGE_KEYS);
    let raw = api
        .send_bytes(
            Method::GET,
            "/v1/inbox?after_seq=0&limit=1",
            Some(&c.api_key),
            &[],
            None,
        )
        .await;
    let text = raw.text();
    let at = text.find("\"items\":[").unwrap() + "\"items\":[".len();
    let end = text.rfind("],\"next_after_seq\"").unwrap();
    let event_keys = ordered_keys(&text[at..end]);
    assert_eq!(event_keys.len(), 17);
    assert_eq!(event_keys[..3], ["inbox_seq", "kind", "woke"]);
    assert_eq!(event_keys[3..], MESSAGE_KEYS);
    // A repeated send still answers with an empty summary.
    let replay = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"room_id": "all", "body": "x", "client_key": "x"}),
        )
        .await;
    assert_eq!(replay.status.as_u16(), 200);
    assert_eq!(replay.body["reactions"], json!([]));
    hub.server.shutdown().await.unwrap();
}

// ---- the change feed ------------------------------------------------------------------------

#[tokio::test]
async fn the_change_feed_follows_one_room_in_change_order() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;

    let empty = api.get("/v1/reactions?room_id=all", &c.api_key).await;
    assert_eq!(
        empty.body,
        json!({"items": [], "next_after_change_seq": "0", "has_more": false})
    );
    let target = post_all(api, &a, "x").await;
    let dm = post(
        api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "dm"}),
    )
    .await;
    let added = reacted(api, &b.api_key, &target, "thumbsup").await;
    reacted(api, &b.api_key, id(&dm), "eyes").await;
    unreact(api, &b.api_key, &target, "thumbsup", None).await;

    let feed = api
        .get("/v1/reactions?room_id=all&after_change_seq=0", &c.api_key)
        .await;
    assert_eq!(feed.status.as_u16(), 200, "{}", feed.body);
    let items = feed.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "the DM's change is another room's");
    assert_eq!(
        items[0],
        json!({"change_seq": "1", "message_id": target, "agent_id": "agent-b",
               "kind": "thumbsup", "removed": false, "created_at": added["created_at"]})
    );
    assert_eq!(items[1]["change_seq"], "3");
    assert_eq!(items[1]["removed"], true);
    assert_eq!(feed.body["next_after_change_seq"], "3");
    assert_eq!(feed.body["has_more"], false);
    let raw = api
        .send_bytes(
            Method::GET,
            "/v1/reactions?room_id=all&after_change_seq=0",
            Some(&c.api_key),
            &[],
            None,
        )
        .await;
    assert_eq!(
        ordered_keys(&raw.text()),
        ["items", "next_after_change_seq", "has_more"]
    );
    let text = raw.text();
    let at = text.find("[").unwrap() + 1;
    let end = at + text[at..].find('}').unwrap() + 1;
    assert_eq!(
        ordered_keys(&text[at..end]),
        [
            "change_seq",
            "message_id",
            "agent_id",
            "kind",
            "removed",
            "created_at"
        ]
    );

    // limit and the cursor.
    let first = api
        .get(
            "/v1/reactions?room_id=all&after_change_seq=0&limit=1",
            OWNER_TOKEN,
        )
        .await;
    assert_eq!(first.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(first.body["next_after_change_seq"], "1");
    assert_eq!(first.body["has_more"], true);
    let second = api
        .get(
            "/v1/reactions?room_id=all&after_change_seq=1&limit=1",
            OWNER_TOKEN,
        )
        .await;
    assert_eq!(second.body["items"][0]["change_seq"], "3");
    assert_eq!(second.body["has_more"], false);
    let past = api
        .get("/v1/reactions?room_id=all&after_change_seq=3", OWNER_TOKEN)
        .await;
    assert_eq!(
        past.body,
        json!({"items": [], "next_after_change_seq": "3", "has_more": false})
    );
    // Without after_change_seq: no items, the room's cursor.
    let cursor = api.get("/v1/reactions?room_id=all", &c.api_key).await;
    assert_eq!(
        cursor.body,
        json!({"items": [], "next_after_change_seq": "3", "has_more": false})
    );
    let dm_cursor = api
        .get("/v1/reactions?room_id=dm:agent-a:agent-b", &b.api_key)
        .await;
    assert_eq!(dm_cursor.body["next_after_change_seq"], "2");
    hub.server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_change_feed_checks_run_in_the_documented_order() {
    let hub = start().await;
    let api = &hub.api;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    let human = api.register("human", None).await;
    post(
        api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "dm"}),
    )
    .await;

    api.send(Method::GET, "/v1/reactions?room_id=all", None, None)
        .await
        .assert_error(401, "unauthorized");
    let unknown = api
        .get("/v1/reactions?room_id=all&offset=1", &c.api_key)
        .await;
    unknown.assert_error(422, "unexpected_field");
    let repeated = api
        .get("/v1/reactions?room_id=all&room_id=all", &c.api_key)
        .await;
    repeated.assert_error(422, "invalid_field");
    for query in ["", "?room_id=nope", "?room_id=dm:b:a"] {
        let reply = api.get(&format!("/v1/reactions{query}"), &c.api_key).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("room_id"), "{query}");
    }
    // Permission before the values.
    api.get(
        "/v1/reactions?room_id=dm:agent-a:agent-b&after_change_seq=01",
        &c.api_key,
    )
    .await
    .assert_error(403, "forbidden");
    api.get("/v1/reactions?room_id=room:nope", &c.api_key)
        .await
        .assert_error(403, "forbidden");
    api.get("/v1/reactions?room_id=room:nope", OWNER_TOKEN)
        .await
        .assert_error(404, "unknown_room");
    for (query, field) in [
        ("after_change_seq=01", "after_change_seq"),
        ("after_change_seq=-1", "after_change_seq"),
        ("limit=0", "limit"),
        ("limit=101", "limit"),
    ] {
        let reply = api
            .get(&format!("/v1/reactions?room_id=all&{query}"), &c.api_key)
            .await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{query}");
    }

    // A thread participant without a membership does not read the feed.
    let room = api
        .post("/v1/rooms", OWNER_TOKEN, json!({"room_id": "room:t"}))
        .await;
    assert_eq!(room.status.as_u16(), 201);
    api.put("/v1/rooms/room:t/members/agent-a", OWNER_TOKEN, json!({}))
        .await;
    let root = post(
        api,
        &a.api_key,
        json!({"room_id": "room:t", "body": "root", "client_key": "t-root"}),
    )
    .await;
    post(
        api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "reply_to": id(&root), "body": "hi", "client_key": "t-barge"}),
    )
    .await;
    api.get("/v1/reactions?room_id=room:t", &human.api_key)
        .await
        .assert_error(403, "forbidden");
    // A member that left `all` keeps its membership row and reads it.
    api.put(
        "/v1/agents/agent-c/membership/all",
        &c.api_key,
        json!({"joined": false}),
    )
    .await;
    let reply = api.get("/v1/reactions?room_id=all", &c.api_key).await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    hub.server.shutdown().await.unwrap();
}
