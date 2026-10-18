//! S12: `DELETE /v1/agents/{agent_id}` (`IF-WIRE-009`, ADR-0009). The
//! owner removes an agent's registration and state in one transaction; the
//! messages it sent and the DM rooms it was in stay. A deleted id registers
//! again as a brand-new agent. Ids in `[owner] post_as` are protected.

mod common;

use common::{Api, Fixture, OWNER_TOKEN, Registered, RunningServer};
use reqwest::Method;
use serde_json::{Value, json};

const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0];

/// Every table holding a row keyed to an agent, with its key column.
const AGENT_TABLES: [(&str, &str); 7] = [
    ("agents", "agent_id"),
    ("memberships", "agent_id"),
    ("inbox_rows", "agent_id"),
    ("read_state", "agent_id"),
    ("notification_pending", "agent_id"),
    ("agent_avatars", "agent_id"),
    ("idempotency", "sender_id"),
];

fn agent_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}")
}

async fn delete(api: &Api, agent_id: &str, token: Option<&str>) -> common::Reply {
    api.send(Method::DELETE, &agent_path(agent_id), token, None)
        .await
}

async fn start_with_human() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
    let api = server.api();
    (fx, server, api)
}

fn ids(list: &Value, key: &str, field: &str) -> Vec<String> {
    list[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap().to_owned())
        .collect()
}

/// Rows per table keyed to `agent_id`.
async fn rows_for(fx: &Fixture, agent_id: &str) -> Vec<(&'static str, i64)> {
    let agent_id = agent_id.to_owned();
    fx.read_db(move |conn| {
        let mut counts = Vec::new();
        for (table, column) in AGENT_TABLES {
            let count: i64 = conn.query_row(
                &format!("SELECT COUNT(*) FROM {table} WHERE {column} = ?1"),
                [&agent_id],
                |row| row.get(0),
            )?;
            counts.push((table, count));
        }
        Ok(counts)
    })
    .await
}

async fn message_count(fx: &Fixture) -> i64 {
    fx.read_db(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM messages", [], |row| row.get(0))?))
        .await
}

#[tokio::test]
async fn only_the_owner_deletes_and_refusals_follow_the_decision_order() {
    let (_fx, server, api) = start_with_human().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("human", None).await;

    delete(&api, "agent-a", None)
        .await
        .assert_error(401, "unauthorized");
    for token in [&a.api_key, &b.api_key] {
        delete(&api, "agent-a", Some(token))
            .await
            .assert_error(403, "forbidden");
    }
    // An agent is refused before the path is looked at.
    delete(&api, "Not_A_Slug", Some(&a.api_key))
        .await
        .assert_error(403, "forbidden");
    for path in ["Not_A_Slug", "all", "owner"] {
        delete(&api, path, Some(OWNER_TOKEN))
            .await
            .assert_error(404, "agent_not_found");
    }
    delete(&api, "nobody", Some(OWNER_TOKEN))
        .await
        .assert_error(404, "agent_not_found");

    // Protected by `[owner] post_as`, registered or not.
    delete(&api, "human", Some(OWNER_TOKEN))
        .await
        .assert_error(409, "agent_protected");
    let fx2 = Fixture::new();
    let server2 = RunningServer::start(&fx2.load_config_with_owner(&["ghost"])).await;
    delete(&server2.api(), "ghost", Some(OWNER_TOKEN))
        .await
        .assert_error(409, "agent_protected");
    server2.shutdown().await.unwrap();

    let deleted = delete(&api, "agent-a", Some(OWNER_TOKEN)).await;
    assert_eq!(deleted.status.as_u16(), 204);
    assert_eq!(deleted.body, Value::Null, "204 has no body");
    delete(&api, "agent-a", Some(OWNER_TOKEN))
        .await
        .assert_error(404, "agent_not_found");
    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    assert_eq!(
        ids(&directory.body, "items", "agent_id"),
        ["agent-b", "human"]
    );

    server.shutdown().await.unwrap();
}

/// The agent's key, inbox, delivery, membership, profile, avatar and
/// idempotency keys are gone; every message and room stays.
#[tokio::test]
async fn deletion_removes_the_agents_state_and_keeps_history() {
    let (fx, server, api) = start_with_human().await;
    let a = api.register("agent-a", None).await;
    let z = api.register("agent-z", None).await;
    api.register("human", None).await;
    api.put(
        "/v1/agents/agent-z/profile",
        &z.api_key,
        json!({"role": "retiring"}),
    )
    .await;
    let avatar = api
        .send_bytes(
            Method::PUT,
            "/v1/agents/agent-z/avatar",
            Some(&z.api_key),
            &[("content-type", "image/png")],
            Some(PNG.to_vec()),
        )
        .await;
    assert_eq!(avatar.status.as_u16(), 200);

    let from_z = api.sent(&z.api_key, "agent-a", "z to a", "z-1").await;
    api.sent(&a.api_key, "agent-z", "a to z", "a-1").await;
    api.sent(&a.api_key, "all", "a to all", "a-2").await;
    let z_all = api.sent(&z.api_key, "all", "z to all", "z-2").await;
    let messages_before = message_count(&fx).await;
    assert!(
        rows_for(&fx, "agent-z")
            .await
            .iter()
            .all(|(_, count)| *count > 0),
        "every table has a row for agent-z before"
    );

    let deleted = delete(&api, "agent-z", Some(OWNER_TOKEN)).await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.body);

    // State: the key no longer authenticates and the owner's paths are 404.
    api.get("/v1/inbox", &z.api_key)
        .await
        .assert_error(401, "unauthorized");
    for suffix in ["/inbox", "/delivery", "/avatar"] {
        api.get(&format!("/v1/agents/agent-z{suffix}"), OWNER_TOKEN)
            .await
            .assert_error(404, "agent_not_found");
    }
    api.put(
        "/v1/agents/agent-z/profile",
        OWNER_TOKEN,
        json!({"role": "back"}),
    )
    .await
    .assert_error(404, "agent_not_found");
    let directory = api.get("/v1/agents", &a.api_key).await;
    assert_eq!(
        ids(&directory.body, "items", "agent_id"),
        ["agent-a", "human"]
    );
    for (table, count) in rows_for(&fx, "agent-z").await {
        assert_eq!(count, 0, "{table} still has a row for agent-z");
    }

    // History: every message stays, the DM room is still listed.
    assert_eq!(message_count(&fx).await, messages_before);
    let rooms = api.get("/v1/rooms", OWNER_TOKEN).await;
    let dm = rooms.body["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|room| room["room_id"] == "dm:agent-a:agent-z")
        .expect("the DM room is still listed")
        .clone();
    assert_eq!(dm["members"], json!(["agent-a", "agent-z"]));
    assert_eq!(dm["message_count"], 2);
    let all = rooms.body["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|room| room["room_id"] == "all")
        .unwrap()
        .clone();
    assert_eq!(all["members"], json!(["agent-a", "human"]));
    for token in [a.api_key.as_str(), OWNER_TOKEN] {
        let history = api
            .get("/v1/messages?room_id=dm:agent-a:agent-z", token)
            .await;
        assert_eq!(history.status.as_u16(), 200, "{}", history.body);
        let senders = ids(&history.body, "items", "sender_id");
        assert_eq!(senders, ["agent-a", "agent-z"], "newest first");
    }
    let single = api
        .get(
            &format!("/v1/messages/{}", from_z["message_id"].as_str().unwrap()),
            &a.api_key,
        )
        .await;
    assert_eq!(single.status.as_u16(), 200);
    assert_eq!(single.body["sender_id"], "agent-z");

    // A deleted agent is an unknown recipient; replies to its messages work.
    api.send_message(&a.api_key, "agent-z", "anyone?", "a-3")
        .await
        .assert_error(404, "unknown_recipient");
    api.post(
        "/v1/rooms/dm:agent-a:agent-z/messages",
        OWNER_TOKEN,
        json!({"sender_id": "human", "body": "hi", "client_key": "h-1"}),
    )
    .await
    .assert_error(404, "unknown_room");
    let reply = api
        .post(
            "/v1/messages",
            &a.api_key,
            json!({"to": "all", "body": "re", "client_key": "a-4", "reply_to": z_all["message_id"]}),
        )
        .await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_deleted_id_registers_again_as_a_new_agent() {
    let (_fx, server, api) = start_with_human().await;
    let a = api.register("agent-a", None).await;
    let old = api.register("agent-z", None).await;
    api.sent(&old.api_key, "agent-a", "old hello", "k1").await;
    api.sent(&a.api_key, "agent-z", "old reply", "a-1").await;
    api.sent(&a.api_key, "agent-z", "old reply 2", "a-2").await;
    assert_eq!(delete(&api, "agent-z", Some(OWNER_TOKEN)).await.status, 204);

    let new: Registered = api.register("agent-z", None).await;
    assert_ne!(new.api_key, old.api_key);
    assert_eq!(new.body["route_version"], 1);
    assert_eq!(new.body["profile"]["role"], Value::Null);
    api.get("/v1/inbox", &old.api_key)
        .await
        .assert_error(401, "unauthorized");

    let inbox = api.get("/v1/inbox", &new.api_key).await;
    assert_eq!(inbox.status.as_u16(), 200, "{}", inbox.body);
    assert_eq!(inbox.body["snapshot_head_seq"], "0");
    assert_eq!(inbox.body["read_marker"], "0");
    assert_eq!(inbox.body["items"], json!([]));

    // The old client_key is free again: a new message, not a replay.
    let fresh = api.sent(&new.api_key, "agent-a", "old hello", "k1").await;
    assert_eq!(fresh["deduplicated"], false);

    api.sent(&a.api_key, "agent-z", "new hello", "a-3").await;
    let inbox = api.get("/v1/inbox", &new.api_key).await;
    assert_eq!(inbox.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(inbox.body["items"][0]["inbox_seq"], "1");
    assert_eq!(inbox.body["items"][0]["body"], "new hello");

    // The old DM history is visible to the new member of that room.
    let history = api
        .get("/v1/messages?room_id=dm:agent-a:agent-z", &new.api_key)
        .await;
    assert_eq!(history.status.as_u16(), 200, "{}", history.body);
    assert_eq!(history.body["items"].as_array().unwrap().len(), 5);

    server.shutdown().await.unwrap();
}
