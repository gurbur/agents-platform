//! S13 routing (`IF-WIRE-010` R and W, PLATFORM_MESSAGING_SPEC "방
//! routing", ADR-0010/0011): which room a send lands in, who gets an inbox
//! row (R: every joined member, the sender included) and who is woken
//! (W: R minus the sender, by `wake`, `mentions` or `wake_policy=always`).
//!
//! Without receivers a woken agent shows as a `pending` notification row
//! whose `target_seq` is its new `inbox_seq`; an agent that only got a quiet
//! row stays `idle`.

mod common;

use std::collections::BTreeSet;

use common::{Api, Fixture, OWNER_TOKEN, Registered, Reply, RunningServer};
use serde_json::{Value, json};

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human", "ghost"])).await;
    let api = server.api();
    (fx, server, api)
}

async fn send(api: &Api, token: &str, body: Value) -> Reply {
    api.post("/v1/messages", token, body).await
}

async fn sent(api: &Api, token: &str, body: Value) -> Value {
    let reply = send(api, token, body).await;
    assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
    reply.body
}

/// Agents with an inbox row for `message_id`, with that row's `kind` and
/// `wake`, in ASCII order.
async fn rows_for(fx: &Fixture, message_id: &str) -> Vec<(String, String, i64)> {
    let message_id = message_id.to_owned();
    fx.read_db(move |conn| {
        let mut statement = conn.prepare(
            "SELECT agent_id, kind, wake FROM inbox_rows WHERE message_id = ?1 ORDER BY agent_id",
        )?;
        let rows = statement
            .query_map([message_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

fn recipients(rows: &[(String, String, i64)]) -> Vec<&str> {
    rows.iter().map(|(agent, _, _)| agent.as_str()).collect()
}

/// `(state, target_seq)` of every notification row.
async fn notifications(fx: &Fixture) -> Vec<(String, String, i64)> {
    fx.read_db(|conn| {
        let mut statement = conn.prepare(
            "SELECT agent_id, state, target_seq FROM notification_pending ORDER BY agent_id",
        )?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

/// Agents whose notification row is `pending`.
async fn woken(fx: &Fixture) -> BTreeSet<String> {
    notifications(fx)
        .await
        .into_iter()
        .filter(|(_, state, _)| state == "pending")
        .map(|(agent, _, _)| agent)
        .collect()
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

async fn room_rows(fx: &Fixture, room_id: &str) -> (Vec<(String, String)>, Vec<(String, i64)>) {
    let room_id = room_id.to_owned();
    fx.read_db(move |conn| {
        let mut rooms = conn.prepare("SELECT kind, created_by FROM rooms WHERE room_id = ?1")?;
        let room = rooms
            .query_map([&room_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut members = conn.prepare(
            "SELECT agent_id, joined FROM memberships WHERE room_id = ?1 ORDER BY agent_id",
        )?;
        let members = members
            .query_map([&room_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((room, members))
    })
    .await
}

// ---- where a send lands --------------------------------------------------------------

#[tokio::test]
async fn all_reaches_every_joined_member_and_the_sender_gets_a_quiet_own_row() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    api.put(
        "/v1/agents/agent-c/membership/all",
        &c.api_key,
        json!({"joined": false}),
    )
    .await;

    let message = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "hi", "client_key": "k"}),
    )
    .await;
    let rows = rows_for(&fx, message["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-a", "agent-b"]);
    assert!(
        rows.iter()
            .all(|(_, kind, wake)| kind == "message" && *wake == 1)
    );
    assert_eq!(
        woken(&fx).await,
        set(&["agent-b"]),
        "the sender is not woken"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn to_opens_the_dm_room_once_and_the_sender_gets_its_own_row() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;

    let first = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "hi", "client_key": "k1"}),
    )
    .await;
    assert_eq!(first["room_id"], "dm:agent-a:agent-b");
    let (room, members) = room_rows(&fx, "dm:agent-a:agent-b").await;
    assert_eq!(room, [("dm".to_owned(), "agent-a".to_owned())]);
    assert_eq!(
        members,
        [("agent-a".to_owned(), 1), ("agent-b".to_owned(), 1)]
    );
    let rows = rows_for(&fx, first["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-a", "agent-b"]);
    assert_eq!(woken(&fx).await, set(&["agent-b"]));

    let by_room_id = sent(
        &api,
        &b.api_key,
        json!({"room_id": "dm:agent-a:agent-b", "body": "back", "client_key": "k2"}),
    )
    .await;
    assert_eq!(by_room_id["room_id"], "dm:agent-a:agent-b");
    let (room_again, members_again) = room_rows(&fx, "dm:agent-a:agent-b").await;
    assert_eq!(
        (room_again, members_again),
        (room, members),
        "not re-created"
    );
    let rooms: i64 = fx
        .read_db(|conn| Ok(conn.query_row("SELECT COUNT(*) FROM rooms", [], |row| row.get(0))?))
        .await;
    assert_eq!(rooms, 2, "all and the one DM");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_topic_room_reaches_its_joined_members_the_sender_included() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    api.register("agent-c", None).await;
    create_topic(&api, "room:launch", &["agent-a", "agent-b"]).await;

    let message = sent(
        &api,
        &a.api_key,
        json!({"room_id": "room:launch", "body": "hi", "client_key": "k"}),
    )
    .await;
    assert_eq!(message["room_kind"], "topic");
    let rows = rows_for(&fx, message["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-a", "agent-b"]);
    assert_eq!(woken(&fx).await, set(&["agent-b"]));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn reply_to_alone_lands_in_the_room_of_the_original() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("human", None).await;
    create_topic(&api, "room:launch", &["agent-a", "agent-b"]).await;

    for (room, key) in [
        ("all", "all"),
        ("dm:agent-a:agent-b", "dm"),
        ("room:launch", "topic"),
    ] {
        let original = sent(
            &api,
            &a.api_key,
            json!({"room_id": room, "body": "q", "client_key": key}),
        )
        .await;
        let reply = sent(
            &api,
            &b.api_key,
            json!({"reply_to": original["message_id"], "body": "a", "client_key": format!("re-{key}")}),
        )
        .await;
        assert_eq!(reply["room_id"], room);
        assert_eq!(reply["reply_to"], original["message_id"]);
    }

    // The owner barges into a's and b's DM as human; a answers in place.
    // S15: the answer opens human's thread, so human (its root author)
    // gets it without a mention (scenario 27, corrected scenario 18).
    let barge = sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "dm:agent-a:agent-b", "body": "잠깐", "client_key": "barge"}),
    )
    .await;
    let answer = sent(
        &api,
        &a.api_key,
        json!({"reply_to": barge["message_id"], "body": "네", "client_key": "answer"}),
    )
    .await;
    assert_eq!(answer["room_id"], "dm:agent-a:agent-b");
    let rows = rows_for(&fx, answer["message_id"].as_str().unwrap()).await;
    assert_eq!(
        recipients(&rows),
        ["agent-a", "agent-b", "human"],
        "human is the thread's root participant"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_posts_need_no_membership_of_the_sender() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let human = api.register("human", None).await;
    api.put(
        "/v1/agents/human/membership/all",
        &human.api_key,
        json!({"joined": false}),
    )
    .await;
    create_topic(&api, "room:launch", &["agent-a"]).await;

    for (room, key) in [("all", "p1"), ("room:launch", "p2")] {
        let post = sent(
            &api,
            OWNER_TOKEN,
            json!({"sender_id": "human", "room_id": room, "body": "x", "client_key": key}),
        )
        .await;
        assert_eq!(post["sender_id"], "human");
        let rows = rows_for(&fx, post["message_id"].as_str().unwrap()).await;
        assert!(
            !recipients(&rows).contains(&"human"),
            "a non-member sender gets no row: {rows:?}"
        );
    }

    // A DM the owner opens records the sender as its creator.
    let opened = sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "dm:agent-a:agent-b", "body": "x", "client_key": "p3"}),
    )
    .await;
    let (room, members) = room_rows(&fx, "dm:agent-a:agent-b").await;
    assert_eq!(room, [("dm".to_owned(), "human".to_owned())]);
    assert_eq!(
        members,
        [("agent-a".to_owned(), 1), ("agent-b".to_owned(), 1)]
    );
    let rows = rows_for(&fx, opened["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-a", "agent-b"]);

    // As a member of its own DM the posting identity does get its row.
    let own_dm = sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "to": "agent-b", "body": "x", "client_key": "p4"}),
    )
    .await;
    assert_eq!(own_dm["room_id"], "dm:agent-b:human");
    let rows = rows_for(&fx, own_dm["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-b", "human"]);
    drop((a, b));
    server.shutdown().await.unwrap();
}

// ---- who is woken -------------------------------------------------------------------

#[tokio::test]
async fn wake_false_writes_rows_for_everyone_and_wakes_no_one() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let before = notifications(&fx).await;

    let quiet = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "ok", "client_key": "k", "wake": false}),
    )
    .await;
    assert_eq!(quiet["wake"], false);
    let rows = rows_for(&fx, quiet["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-a", "agent-b"]);
    assert!(rows.iter().all(|(_, _, wake)| *wake == 0), "{rows:?}");
    assert_eq!(notifications(&fx).await, before, "no delivery transition");
    let inbox = api.get("/v1/inbox", &b.api_key).await;
    assert_eq!(
        inbox.body["snapshot_head_seq"], "1",
        "the inbox head still moves"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn mentions_wake_only_the_named_agents_even_when_quiet() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    create_topic(&api, "room:launch", &["agent-a", "agent-b", "agent-c"]).await;

    let message = sent(
        &api,
        &a.api_key,
        json!({"room_id": "room:launch", "body": "b?", "client_key": "k", "wake": false, "mentions": ["agent-b"]}),
    )
    .await;
    assert_eq!(woken(&fx).await, set(&["agent-b"]));
    for agent in [&b, &c] {
        let inbox = api.get("/v1/inbox", &agent.api_key).await;
        let item = &inbox.body["items"][0];
        assert_eq!(item["message_id"], message["message_id"]);
        assert_eq!(item["wake"], false, "the sender's value, even for b");
        assert_eq!(item["mentions"], json!(["agent-b"]));
    }

    ack_all(&api, &[&b]).await;
    sent(
        &api,
        &a.api_key,
        json!({"room_id": "room:launch", "body": "all", "client_key": "k2", "mentions": ["agent-c"]}),
    )
    .await;
    assert_eq!(
        woken(&fx).await,
        set(&["agent-b", "agent-c"]),
        "wake=true wakes every member"
    );
    server.shutdown().await.unwrap();
}

/// `always` wakes for every inbox row, the receiver's own sends included
/// (coordinator decision 2026-10-20); a `sender` policy never wakes for
/// its own send.
#[tokio::test]
async fn wake_policy_always_wakes_for_quiet_events_and_for_its_own_sends() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    drop(c);
    for agent in ["agent-a", "agent-c"] {
        let reply = api
            .patch(
                &format!("/v1/agents/{agent}/receiver"),
                OWNER_TOKEN,
                json!({"wake_policy": "always"}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }

    sent(
        &api,
        &b.api_key,
        json!({"room_id": "all", "body": "fyi", "client_key": "k", "wake": false}),
    )
    .await;
    assert_eq!(
        woken(&fx).await,
        set(&["agent-a", "agent-c"]),
        "quiet: only the always receivers, b (sender policy) is not woken by itself"
    );
    ack_all(&api, &[&a]).await;
    sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "fyi", "client_key": "k", "wake": false}),
    )
    .await;
    assert!(
        woken(&fx).await.contains("agent-a"),
        "an always receiver is woken by its own send"
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_mentioned_post_as_identity_gets_the_event_without_room_rights() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    api.register("agent-b", None).await;
    let human = api.register("human", None).await;
    sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "hi", "client_key": "k0"}),
    )
    .await;

    let answer = sent(
        &api,
        &a.api_key,
        json!({"room_id": "dm:agent-a:agent-b", "body": "for you", "client_key": "k1", "wake": false, "mentions": ["human"]}),
    )
    .await;
    let rows = rows_for(&fx, answer["message_id"].as_str().unwrap()).await;
    assert_eq!(recipients(&rows), ["agent-a", "agent-b", "human"]);
    assert!(woken(&fx).await.contains("human"));
    let inbox = api.get("/v1/inbox", &human.api_key).await;
    assert_eq!(inbox.body["items"][0]["message_id"], answer["message_id"]);

    api.get("/v1/messages?room_id=dm:agent-a:agent-b", &human.api_key)
        .await
        .assert_error(403, "forbidden");
    send(
        &api,
        &human.api_key,
        json!({"room_id": "dm:agent-a:agent-b", "body": "x", "client_key": "k2"}),
    )
    .await
    .assert_error(403, "not_room_member");
    send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "x", "client_key": "k3", "mentions": ["ghost"]}),
    )
    .await
    .assert_error(422, "mention_not_member");
    server.shutdown().await.unwrap();
}

/// `woke` (inbox only) is the sender's rule for this recipient:
/// `recipient != sender && (wake || mentioned)`. A receiver's own
/// `wake_policy = always` wakes it without making `woke` true.
#[tokio::test]
async fn inbox_items_say_whether_the_senders_rule_woke_the_recipient() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    let reply = api
        .patch(
            "/v1/agents/agent-c/receiver",
            OWNER_TOKEN,
            json!({"wake_policy": "always"}),
        )
        .await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);

    /// `woke` of each agent's inbox item for `message`, in the given order.
    async fn woke_of(api: &Api, agents: [&Registered; 3], message: &Value) -> Vec<Value> {
        let mut values = Vec::new();
        for agent in agents {
            let page = api
                .get("/v1/inbox?after_seq=0&limit=100", &agent.api_key)
                .await;
            let item = page.body["items"]
                .as_array()
                .unwrap()
                .iter()
                .find(|item| item["message_id"] == message["message_id"])
                .cloned()
                .unwrap_or_else(|| panic!("no item for {message} in {}", page.body));
            values.push(item["woke"].clone());
        }
        values
    }
    let agents = [&a, &b, &c];

    let quiet = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "fyi", "client_key": "k1", "wake": false}),
    )
    .await;
    assert_eq!(
        woke_of(&api, agents, &quiet).await,
        [json!(false), json!(false), json!(false)],
        "quiet: nobody by the sender's rule"
    );
    assert_eq!(
        woken(&fx).await,
        set(&["agent-c"]),
        "always still wakes c, with woke=false"
    );

    let mention = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "b?", "client_key": "k2", "wake": false, "mentions": ["agent-b"]}),
    )
    .await;
    assert_eq!(
        woke_of(&api, agents, &mention).await,
        [json!(false), json!(true), json!(false)]
    );

    let loud = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "all", "client_key": "k3"}),
    )
    .await;
    assert_eq!(
        woke_of(&api, agents, &loud).await,
        [json!(false), json!(true), json!(true)],
        "default wake: every recipient but the sender"
    );
    server.shutdown().await.unwrap();
}

// ---- item shapes --------------------------------------------------------------------

#[tokio::test]
async fn inbox_and_history_items_carry_the_event_fields() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let original = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "q", "client_key": "k1"}),
    )
    .await;
    sent(
        &api,
        &b.api_key,
        json!({"reply_to": original["message_id"], "body": "a", "client_key": "k2", "wake": false, "mentions": ["agent-a"]}),
    )
    .await;

    let inbox = api.get("/v1/inbox", &a.api_key).await;
    let items = inbox.body["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "a's own DM and b's answer: {}", inbox.body);
    let keys = api
        .keys_of(reqwest::Method::GET, "/v1/inbox", &a.api_key, None)
        .await;
    assert_eq!(
        keys,
        [
            "agent_id",
            "read_marker",
            "snapshot_head_seq",
            "items",
            "next_after_seq",
            "has_more"
        ]
    );
    let raw = api
        .send_bytes(
            reqwest::Method::GET,
            "/v1/inbox",
            Some(&a.api_key),
            &[],
            None,
        )
        .await
        .text();
    let items_text = &raw[raw.find("\"items\":[").unwrap() + "\"items\":[".len()..];
    // S15: the first item is a root with a summary object; cut after the
    // item's own `created_at`.
    let created = items_text.find("\"created_at\"").unwrap();
    let first_item = &items_text[..=created + items_text[created..].find('}').unwrap()];
    assert_eq!(
        common::ordered_keys(first_item),
        [
            "inbox_seq",
            "kind",
            "woke",
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
        ]
    );
    assert_eq!(items[0]["sender_id"], "agent-a", "the sender's own row");
    assert_eq!(items[0]["kind"], "message");
    assert_eq!(items[0]["reply_to"], Value::Null);
    assert_eq!(items[0]["mentions"], json!([]));
    assert_eq!(items[1]["reply_to"], original["message_id"]);
    assert_eq!(items[1]["room_kind"], "dm");
    assert_eq!(items[1]["wake"], false);
    assert_eq!(items[1]["mentions"], json!(["agent-a"]));

    let history = api
        .get(
            "/v1/messages?room_id=dm:agent-a:agent-b&after_event_seq=0",
            &a.api_key,
        )
        .await;
    let first = &history.body["items"][0];
    assert_eq!(first.as_object().unwrap().len(), 14, "S16: fourteen keys");
    let single = api
        .get(
            &format!("/v1/messages/{}", original["message_id"].as_str().unwrap()),
            &b.api_key,
        )
        .await;
    assert_eq!(&single.body, first);
    server.shutdown().await.unwrap();
}
