//! S15 threads (`IF-WIRE-014`, ADR-0013, API_V1 "스레드"): a reply opens
//! or joins the thread of its target, participants are registered in the
//! send transaction before R and W are counted, a thread message reaches
//! every joined member's inbox but wakes only unmuted participants (plus
//! mentions and `wake_policy = always`), participants read their thread
//! without room membership, and the two thread endpoints answer in the
//! `IF-WIRE-014` order.
//!
//! Without receivers a woken agent shows as a `pending` notification row;
//! [`ack_all`] makes everyone idle again between sends.

mod common;

use std::collections::BTreeSet;

use common::{Api, Fixture, OWNER_TOKEN, Registered, Reply, RunningServer};
use reqwest::Method;
use serde_json::{Value, json};

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_owner(&["human"])).await;
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

fn id(message: &Value) -> &str {
    message["message_id"].as_str().unwrap()
}

/// `(agent_id, woke)` of every inbox row for `message_id`, ASCII order.
async fn rows_for(fx: &Fixture, message_id: &str) -> Vec<(String, bool)> {
    let message_id = message_id.to_owned();
    fx.read_db(move |conn| {
        let mut statement = conn.prepare(
            "SELECT agent_id, woke FROM inbox_rows WHERE message_id = ?1 ORDER BY agent_id",
        )?;
        let rows = statement
            .query_map([message_id], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    })
    .await
}

fn rows(expected: &[(&str, bool)]) -> Vec<(String, bool)> {
    expected
        .iter()
        .map(|(agent, woke)| ((*agent).to_owned(), *woke))
        .collect()
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

async fn thread(api: &Api, token: &str, root: &str) -> Reply {
    api.get(&format!("/v1/threads/{root}"), token).await
}

/// `(agent_id, joined_via, muted)` of a thread's participants as listed.
async fn participants(api: &Api, root: &str) -> Vec<(String, String, bool)> {
    let reply = thread(api, OWNER_TOKEN, root).await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    reply.body["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["agent_id"].as_str().unwrap().to_owned(),
                p["joined_via"].as_str().unwrap().to_owned(),
                p["muted"].as_bool().unwrap(),
            )
        })
        .collect()
}

fn listed(expected: &[(&str, &str, bool)]) -> Vec<(String, String, bool)> {
    expected
        .iter()
        .map(|(agent, via, muted)| ((*agent).to_owned(), (*via).to_owned(), *muted))
        .collect()
}

async fn participation(api: &Api, token: &str, root: &str, body: Value) -> Reply {
    api.put(&format!("/v1/threads/{root}/participation"), token, body)
        .await
}

const MISSING_ID: &str = "msg-01J00000000000000000000000";

// ---- resolution ----------------------------------------------------------------------

#[tokio::test]
async fn a_reply_opens_the_thread_of_a_root_and_a_reply_in_it_stays_there() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    assert_eq!(root["thread_id"], Value::Null);
    ack_all(&api, &[&a, &b, &c]).await;

    let opener = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "first", "client_key": "o"}),
    )
    .await;
    assert_eq!(opener["thread_id"], root["message_id"]);
    assert_eq!(opener["reply_to"], root["message_id"]);
    assert_eq!(opener["room_id"], "all");
    assert_eq!(opener["thread_summary"], Value::Null, "send: always null");
    // Scenario 25: the root author wakes, the other member only gets a row.
    assert_eq!(
        rows_for(&fx, id(&opener)).await,
        rows(&[("agent-a", true), ("agent-b", false), ("agent-c", false)])
    );
    assert_eq!(woken(&fx).await, set(&["agent-a"]));
    assert_eq!(
        participants(&api, id(&root)).await,
        listed(&[("agent-a", "root", false), ("agent-b", "opener", false)])
    );

    // A reply to the reply: same thread, `reply_to` is the quoted message.
    let nested = sent(
        &api,
        &c.api_key,
        json!({"reply_to": id(&opener), "body": "second", "client_key": "n"}),
    )
    .await;
    assert_eq!(nested["thread_id"], root["message_id"], "no nesting");
    assert_eq!(nested["reply_to"], opener["message_id"]);

    // Messages sent with `room_id` or `to` are roots.
    let dm = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "d"}),
    )
    .await;
    assert_eq!(dm["thread_id"], Value::Null);
    let dm_reply = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&dm), "body": "dm re", "client_key": "dr"}),
    )
    .await;
    assert_eq!(dm_reply["thread_id"], dm["message_id"], "same rule in a DM");
    assert_eq!(dm_reply["room_id"], "dm:agent-a:agent-b");

    // The stored root keeps `thread_id` null.
    let single = api
        .get(&format!("/v1/messages/{}", id(&root)), &a.api_key)
        .await;
    assert_eq!(single.body["thread_id"], Value::Null);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn the_send_body_has_no_thread_id() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    let reply = send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "thread_id": id(&root), "body": "x", "client_key": "t"}),
    )
    .await;
    reply.assert_error(422, "unexpected_field");
    assert_eq!(reply.error_field(), Some("thread_id"));
    server.shutdown().await.unwrap();
}

// ---- participants ---------------------------------------------------------------------

#[tokio::test]
async fn participants_join_in_order_and_keep_their_first_row() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    api.register("agent-d", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    // The root author opening its own thread is one `root` row.
    sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&root), "body": "self", "client_key": "s"}),
    )
    .await;
    assert_eq!(
        participants(&api, id(&root)).await,
        listed(&[("agent-a", "root", false)])
    );

    sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "b", "client_key": "b", "mentions": ["agent-c"]}),
    )
    .await;
    let after_mention = thread(&api, OWNER_TOKEN, id(&root)).await.body;
    // A muted participant stays muted when it writes again.
    let muted = participation(&api, &c.api_key, id(&root), json!({"wake": false})).await;
    assert_eq!(muted.status.as_u16(), 200, "{}", muted.body);
    sent(
        &api,
        &c.api_key,
        json!({"reply_to": id(&root), "body": "c", "client_key": "c", "mentions": ["agent-d"]}),
    )
    .await;
    assert_eq!(
        participants(&api, id(&root)).await,
        listed(&[
            ("agent-a", "root", false),
            ("agent-b", "poster", false),
            ("agent-c", "mentioned", true),
            ("agent-d", "mentioned", false),
        ])
    );
    let view = thread(&api, OWNER_TOKEN, id(&root)).await.body;
    let joined_at = |body: &Value, agent: &str| {
        body["participants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["agent_id"] == agent)
            .map(|p| p["joined_at"].clone())
            .unwrap()
    };
    assert_eq!(
        joined_at(&view, "agent-c"),
        joined_at(&after_mention, "agent-c"),
        "joined_at is kept"
    );
    // joined_at ascending, then agent_id.
    let order: Vec<(String, String)> = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["joined_at"].as_str().unwrap().to_owned(),
                p["agent_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_owner_post_by_a_non_member_is_barged_and_a_deleted_author_is_not_registered() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    api.register("human", None).await;
    let z = api.register("agent-z", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm root", "client_key": "r"}),
    )
    .await;
    sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "b", "client_key": "b"}),
    )
    .await;
    sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "reply_to": id(&root), "body": "barge", "client_key": "h"}),
    )
    .await;
    assert_eq!(
        participants(&api, id(&root)).await,
        listed(&[
            ("agent-a", "root", false),
            ("agent-b", "opener", false),
            ("human", "barged", false),
        ])
    );

    // An owner post by a member of the room is a plain poster.
    let all_root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "all root", "client_key": "ar"}),
    )
    .await;
    let z_root = sent(
        &api,
        &z.api_key,
        json!({"room_id": "all", "body": "z root", "client_key": "zr"}),
    )
    .await;
    sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&all_root), "body": "b", "client_key": "ab"}),
    )
    .await;
    sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "reply_to": id(&all_root), "body": "h", "client_key": "ah"}),
    )
    .await;
    assert_eq!(
        participants(&api, id(&all_root)).await,
        listed(&[
            ("agent-a", "root", false),
            ("agent-b", "opener", false),
            ("human", "poster", false),
        ])
    );

    // A deleted root author is not registered when its thread opens.
    let deleted = api
        .send(
            Method::DELETE,
            "/v1/agents/agent-z",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.body);
    sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&z_root), "body": "late", "client_key": "zl"}),
    )
    .await;
    assert_eq!(
        participants(&api, id(&z_root)).await,
        listed(&[("agent-b", "opener", false)])
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleting_a_participant_removes_its_rows_and_re_registering_does_not_restore_them() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    let reply = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "b", "client_key": "b"}),
    )
    .await;
    let deleted = api
        .send(
            Method::DELETE,
            "/v1/agents/agent-b",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.body);
    let left: i64 = fx
        .read_db(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM thread_participants WHERE agent_id = 'agent-b'",
                [],
                |row| row.get(0),
            )?)
        })
        .await;
    assert_eq!(left, 0);
    assert_eq!(
        participants(&api, id(&root)).await,
        listed(&[("agent-a", "root", false)])
    );
    // The message stays; the re-registered agent is no participant.
    let again = api.register("agent-b", None).await;
    let single = api
        .get(&format!("/v1/messages/{}", id(&reply)), &a.api_key)
        .await;
    assert_eq!(single.status.as_u16(), 200, "{}", single.body);
    participation(&api, &again.api_key, id(&root), json!({"wake": false}))
        .await
        .assert_error(404, "thread_not_found");
    assert_eq!(
        participants(&api, id(&root)).await,
        listed(&[("agent-a", "root", false)])
    );
    server.shutdown().await.unwrap();
}

// ---- R, W and woke --------------------------------------------------------------------

#[tokio::test]
async fn thread_messages_wake_unmuted_participants_and_mentions_pierce_a_mute() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    let d = api.register("agent-d", None).await;
    let everyone = [&a, &b, &c, &d];

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "open", "client_key": "o"}),
    )
    .await;
    ack_all(&api, &everyone).await;

    // Scenario 26: a mention makes c a participant and wakes it.
    let mention = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&root), "body": "c?", "client_key": "m", "mentions": ["agent-c"]}),
    )
    .await;
    assert_eq!(
        rows_for(&fx, id(&mention)).await,
        rows(&[
            ("agent-a", false),
            ("agent-b", true),
            ("agent-c", true),
            ("agent-d", false)
        ])
    );
    assert_eq!(woken(&fx).await, set(&["agent-b", "agent-c"]));
    ack_all(&api, &everyone).await;

    // Muted: the row stays, the wake does not.
    let muted = participation(&api, &c.api_key, id(&root), json!({"wake": false})).await;
    assert_eq!(muted.status.as_u16(), 200, "{}", muted.body);
    assert_eq!(muted.body["muted"], true);
    let quiet_for_c = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "more", "client_key": "q"}),
    )
    .await;
    assert_eq!(
        rows_for(&fx, id(&quiet_for_c)).await,
        rows(&[
            ("agent-a", true),
            ("agent-b", false),
            ("agent-c", false),
            ("agent-d", false)
        ])
    );
    assert_eq!(woken(&fx).await, set(&["agent-a"]));
    ack_all(&api, &everyone).await;

    // A mention pierces the mute.
    let named = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "c!", "client_key": "n", "mentions": ["agent-c"]}),
    )
    .await;
    assert_eq!(
        rows_for(&fx, id(&named)).await,
        rows(&[
            ("agent-a", true),
            ("agent-b", false),
            ("agent-c", true),
            ("agent-d", false)
        ])
    );
    assert_eq!(woken(&fx).await, set(&["agent-a", "agent-c"]));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_quiet_thread_message_wakes_only_mentions_and_always_receivers() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    let d = api.register("agent-d", None).await;
    let everyone = [&a, &b, &c, &d];
    for agent in ["agent-d", "agent-b"] {
        let patched = api
            .patch(
                &format!("/v1/agents/{agent}/receiver"),
                OWNER_TOKEN,
                json!({"wake_policy": "always"}),
            )
            .await;
        assert_eq!(patched.status.as_u16(), 200, "{}", patched.body);
    }

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    sent(
        &api,
        &c.api_key,
        json!({"reply_to": id(&root), "body": "open", "client_key": "o"}),
    )
    .await;
    ack_all(&api, &everyone).await;

    // Scenario 29: participants a and c are not woken by a quiet message;
    // the mentioned c and the `always` members b (the sender) and d are.
    let quiet = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "q", "client_key": "q", "wake": false, "mentions": ["agent-c"]}),
    )
    .await;
    assert_eq!(
        rows_for(&fx, id(&quiet)).await,
        rows(&[
            ("agent-a", false),
            ("agent-b", false),
            ("agent-c", true),
            ("agent-d", false)
        ])
    );
    assert_eq!(woken(&fx).await, set(&["agent-b", "agent-c", "agent-d"]));
    ack_all(&api, &everyone).await;

    // A loud thread message: the always non-participant d wakes, its row
    // says woke=false.
    let loud = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&root), "body": "loud", "client_key": "l"}),
    )
    .await;
    assert_eq!(
        rows_for(&fx, id(&loud)).await,
        rows(&[
            ("agent-a", false),
            ("agent-b", true),
            ("agent-c", true),
            ("agent-d", false)
        ])
    );
    assert_eq!(woken(&fx).await, set(&["agent-b", "agent-c", "agent-d"]));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_barged_human_is_a_root_participant_and_reads_only_its_thread() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let human = api.register("human", None).await;

    // Scenario 27.
    let barge = sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "room_id": "dm:agent-a:agent-b", "body": "끼어들기", "client_key": "h"}),
    )
    .await;
    ack_all(&api, &[&a, &b, &human]).await;
    let answer = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&barge), "body": "네", "client_key": "a"}),
    )
    .await;
    assert_eq!(answer["thread_id"], barge["message_id"]);
    assert_eq!(
        rows_for(&fx, id(&answer)).await,
        rows(&[("agent-a", false), ("agent-b", false), ("human", true)]),
        "b is a member, not a participant: a row only"
    );
    assert_eq!(woken(&fx).await, set(&["human"]));
    assert_eq!(
        participants(&api, id(&barge)).await,
        listed(&[("agent-a", "opener", false), ("human", "root", false)])
    );

    let room = "dm:agent-a:agent-b";
    let in_thread = api
        .get(
            &format!("/v1/messages?room_id={room}&thread_id={}", id(&barge)),
            &human.api_key,
        )
        .await;
    assert_eq!(in_thread.status.as_u16(), 200, "{}", in_thread.body);
    let ids: Vec<&str> = in_thread.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["message_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [id(&answer), id(&barge)],
        "newest first, root included"
    );
    for message in [&barge, &answer] {
        let single = api
            .get(&format!("/v1/messages/{}", id(message)), &human.api_key)
            .await;
        assert_eq!(single.status.as_u16(), 200, "{}", single.body);
    }
    let view = thread(&api, &human.api_key, id(&barge)).await;
    assert_eq!(view.status.as_u16(), 200, "{}", view.body);

    // The whole room stays closed; writing needs membership.
    api.get(&format!("/v1/messages?room_id={room}"), &human.api_key)
        .await
        .assert_error(403, "forbidden");
    send(
        &api,
        &human.api_key,
        json!({"reply_to": id(&answer), "body": "x", "client_key": "hx"}),
    )
    .await
    .assert_error(403, "not_room_member");

    // Another thread of the same room is not readable.
    let other_root = sent(
        &api,
        &a.api_key,
        json!({"room_id": room, "body": "other", "client_key": "o"}),
    )
    .await;
    let other_reply = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&other_root), "body": "re", "client_key": "or"}),
    )
    .await;
    api.get(
        &format!("/v1/messages?room_id={room}&thread_id={}", id(&other_root)),
        &human.api_key,
    )
    .await
    .assert_error(403, "forbidden");
    for message in [&other_root, &other_reply] {
        api.get(&format!("/v1/messages/{}", id(message)), &human.api_key)
            .await
            .assert_error(404, "message_not_found");
    }
    thread(&api, &human.api_key, id(&other_root))
        .await
        .assert_error(404, "thread_not_found");
    server.shutdown().await.unwrap();
}

// ---- GET /v1/threads/{root_id} ---------------------------------------------------------

#[tokio::test]
async fn the_thread_view_has_its_shape_and_hides_what_the_caller_cannot_read() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "root", "client_key": "r"}),
    )
    .await;
    let first = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "1", "client_key": "1"}),
    )
    .await;
    let last = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&first), "body": "2", "client_key": "2"}),
    )
    .await;

    let path = format!("/v1/threads/{}", id(&root));
    let keys = api.keys_of(Method::GET, &path, &a.api_key, None).await;
    assert_eq!(
        keys,
        [
            "thread_id",
            "room_id",
            "room_kind",
            "root_sender_id",
            "participants",
            "reply_count",
            "last_event_seq",
            "last_message_at"
        ]
    );
    let view = thread(&api, &b.api_key, id(&root)).await.body;
    assert_eq!(view["thread_id"], root["message_id"]);
    assert_eq!(view["room_id"], "dm:agent-a:agent-b");
    assert_eq!(view["room_kind"], "dm");
    assert_eq!(view["root_sender_id"], "agent-a");
    assert_eq!(view["reply_count"], 2);
    assert_eq!(view["last_event_seq"], last["event_seq"]);
    assert_eq!(view["last_message_at"], last["created_at"]);
    let participant_keys: Vec<&String> = view["participants"][0]
        .as_object()
        .unwrap()
        .keys()
        .collect();
    assert_eq!(
        participant_keys,
        ["agent_id", "joined_at", "joined_via", "muted"]
    );
    assert_eq!(
        thread(&api, OWNER_TOKEN, id(&root)).await.status.as_u16(),
        200
    );

    // c reads neither the DM nor the thread.
    thread(&api, &c.api_key, id(&root))
        .await
        .assert_error(404, "thread_not_found");
    // Not a thread: an id inside one, a root without replies, a missing or
    // malformed id.
    let lone = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "lone", "client_key": "l"}),
    )
    .await;
    for target in [id(&first), id(&lone), MISSING_ID, "nope"] {
        thread(&api, OWNER_TOKEN, target)
            .await
            .assert_error(404, "thread_not_found");
    }
    // Queries are refused, after the path.
    let query = api.get(&format!("{path}?limit=1"), &a.api_key).await;
    query.assert_error(422, "unexpected_field");
    api.get("/v1/threads/nope?limit=1", &a.api_key)
        .await
        .assert_error(404, "thread_not_found");
    api.send(Method::GET, &path, None, None)
        .await
        .assert_error(401, "unauthorized");
    server.shutdown().await.unwrap();
}

// ---- PUT /v1/threads/{root_id}/participation -------------------------------------------

#[tokio::test]
async fn participation_answers_in_the_contract_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    api.register("human", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    let reply = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "re", "client_key": "b", "mentions": []}),
    )
    .await;
    sent(
        &api,
        OWNER_TOKEN,
        json!({"sender_id": "human", "reply_to": id(&root), "body": "h", "client_key": "h"}),
    )
    .await;
    let root_id = id(&root);
    let path = format!("/v1/threads/{root_id}/participation");

    // 401 before anything.
    api.send(Method::PUT, &path, None, Some(json!({"wake": false})))
        .await
        .assert_error(401, "unauthorized");
    // The path format before the body.
    let bad_path = api
        .send_raw(
            Method::PUT,
            "/v1/threads/nope/participation",
            Some(&a.api_key),
            Some("text/plain"),
            Some(b"x".to_vec()),
        )
        .await;
    bad_path.assert_error(404, "thread_not_found");
    let text = api
        .send_raw(
            Method::PUT,
            &path,
            Some(&a.api_key),
            Some("text/plain"),
            Some(b"x".to_vec()),
        )
        .await;
    text.assert_error(415, "unsupported_media_type");
    // An agent names itself by its key only.
    let unexpected = participation(
        &api,
        &a.api_key,
        root_id,
        json!({"wake": false, "agent_id": "agent-a"}),
    )
    .await;
    unexpected.assert_error(422, "unexpected_field");
    assert_eq!(unexpected.error_field(), Some("agent_id"));
    participation(&api, &a.api_key, root_id, json!({"wake": false, "x": 1}))
        .await
        .assert_error(422, "unexpected_field");
    for body in [json!({}), json!({"wake": "no"}), json!({"wake": null})] {
        let invalid = participation(&api, &a.api_key, root_id, body).await;
        invalid.assert_error(422, "invalid_field");
        assert_eq!(invalid.error_field(), Some("wake"));
    }
    // The owner names a post_as identity.
    let missing = participation(&api, OWNER_TOKEN, root_id, json!({"wake": false})).await;
    missing.assert_error(422, "invalid_field");
    assert_eq!(missing.error_field(), Some("agent_id"));
    let outside = participation(
        &api,
        OWNER_TOKEN,
        root_id,
        json!({"wake": false, "agent_id": "agent-a"}),
    )
    .await;
    outside.assert_error(403, "forbidden");
    assert_eq!(outside.error_field(), Some("agent_id"));

    // Not a thread, or not a participant of it.
    let lone = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "lone", "client_key": "l"}),
    )
    .await;
    for target in [id(&reply), id(&lone), MISSING_ID] {
        participation(&api, &a.api_key, target, json!({"wake": false}))
            .await
            .assert_error(404, "thread_not_found");
    }
    participation(&api, &c.api_key, root_id, json!({"wake": false}))
        .await
        .assert_error(404, "thread_not_found");

    // A participant mutes, repeats (no-op 200) and unmutes.
    let keys = api
        .keys_of(Method::PUT, &path, &a.api_key, Some(json!({"wake": false})))
        .await;
    assert_eq!(
        keys,
        ["thread_id", "agent_id", "joined_via", "muted", "joined_at"]
    );
    let again = participation(&api, &a.api_key, root_id, json!({"wake": false})).await;
    assert_eq!(again.status.as_u16(), 200, "{}", again.body);
    assert_eq!(again.body["thread_id"], root["message_id"]);
    assert_eq!(again.body["agent_id"], "agent-a");
    assert_eq!(again.body["joined_via"], "root");
    assert_eq!(again.body["muted"], true);
    let unmuted = participation(&api, &a.api_key, root_id, json!({"wake": true})).await;
    assert_eq!(unmuted.body["muted"], false);

    // The owner mutes its post_as identity.
    let owner = participation(
        &api,
        OWNER_TOKEN,
        root_id,
        json!({"wake": false, "agent_id": "human"}),
    )
    .await;
    assert_eq!(owner.status.as_u16(), 200, "{}", owner.body);
    assert_eq!(owner.body["agent_id"], "human");
    assert_eq!(owner.body["joined_via"], "poster");
    assert_eq!(owner.body["muted"], true);
    assert_eq!(
        participants(&api, root_id).await,
        listed(&[
            ("agent-a", "root", false),
            ("agent-b", "opener", false),
            ("human", "poster", true),
        ])
    );
    server.shutdown().await.unwrap();
}

// ---- history filters ------------------------------------------------------------------

#[tokio::test]
async fn history_filters_threads_and_top_level_and_summarises_roots() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root needle", "client_key": "r"}),
    )
    .await;
    let lone = sent(
        &api,
        &b.api_key,
        json!({"room_id": "all", "body": "lone", "client_key": "l"}),
    )
    .await;
    let first = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "first needle", "client_key": "1"}),
    )
    .await;
    let second = sent(
        &api,
        &a.api_key,
        json!({"reply_to": id(&first), "body": "second", "client_key": "2"}),
    )
    .await;
    let ids = |reply: &Reply| -> Vec<String> {
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
        reply.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["message_id"].as_str().unwrap().to_owned())
            .collect()
    };
    let owned = |messages: &[&Value]| -> Vec<String> {
        messages.iter().map(|m| id(m).to_owned()).collect()
    };

    // Default: everything, newest first.
    let all = api.get("/v1/messages?room_id=all", &a.api_key).await;
    assert_eq!(ids(&all), owned(&[&second, &first, &lone, &root]));
    let summary = &all.body["items"][3]["thread_summary"];
    assert_eq!(
        summary,
        &json!({"reply_count": 2, "last_event_seq": second["event_seq"]})
    );
    for index in 0..3 {
        assert_eq!(all.body["items"][index]["thread_summary"], Value::Null);
    }

    // Scenario 28.
    let top = api
        .get("/v1/messages?room_id=all&top_level=true", &a.api_key)
        .await;
    assert_eq!(ids(&top), owned(&[&lone, &root]));
    assert_eq!(top.body["items"][1]["thread_summary"]["reply_count"], 2);
    let not_top = api
        .get("/v1/messages?room_id=all&top_level=false", &a.api_key)
        .await;
    assert_eq!(ids(&not_top), ids(&all), "false is the default");
    let only = api
        .get(
            &format!("/v1/messages?room_id=all&thread_id={}", id(&root)),
            &a.api_key,
        )
        .await;
    assert_eq!(ids(&only), owned(&[&second, &first, &root]));
    // Cursors and `q` combine with the filters.
    let after = api
        .get(
            &format!(
                "/v1/messages?room_id=all&thread_id={}&after_event_seq={}&limit=1",
                id(&root),
                root["event_seq"].as_str().unwrap()
            ),
            &a.api_key,
        )
        .await;
    assert_eq!(ids(&after), owned(&[&first]));
    assert_eq!(after.body["has_more"], true);
    let searched = api
        .get(
            &format!("/v1/messages?room_id=all&thread_id={}&q=needle", id(&root)),
            &a.api_key,
        )
        .await;
    assert_eq!(ids(&searched), owned(&[&first, &root]));
    let top_searched = api
        .get(
            "/v1/messages?room_id=all&top_level=true&q=needle",
            &a.api_key,
        )
        .await;
    assert_eq!(ids(&top_searched), owned(&[&root]));

    // The summary also rides on single messages and inbox items.
    let single = api
        .get(&format!("/v1/messages/{}", id(&root)), &b.api_key)
        .await;
    assert_eq!(single.body["thread_summary"]["reply_count"], 2);
    let inbox = api.get("/v1/inbox?limit=100", &b.api_key).await;
    let root_item = inbox.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["message_id"] == root["message_id"])
        .unwrap()
        .clone();
    assert_eq!(
        root_item["thread_summary"],
        json!({"reply_count": 2, "last_event_seq": second["event_seq"]})
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn history_checks_the_thread_query_in_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;

    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    let reply = sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&root), "body": "re", "client_key": "b"}),
    )
    .await;
    let lone = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "lone", "client_key": "l"}),
    )
    .await;
    let dm_root = sent(
        &api,
        &a.api_key,
        json!({"to": "agent-b", "body": "dm", "client_key": "d"}),
    )
    .await;
    sent(
        &api,
        &b.api_key,
        json!({"reply_to": id(&dm_root), "body": "dm re", "client_key": "dr"}),
    )
    .await;
    let get = |query: String, token: String| {
        let api = &api;
        async move { api.get(&format!("/v1/messages?{query}"), &token).await }
    };
    let root_id = id(&root);

    // Names, then room_id, then the thread query values.
    get("room_id=all&thread=x".to_owned(), a.api_key.clone())
        .await
        .assert_error(422, "unexpected_field");
    let bad_room = get("room_id=lobby&thread_id=nope".to_owned(), a.api_key.clone()).await;
    bad_room.assert_error(422, "invalid_field");
    assert_eq!(bad_room.error_field(), Some("room_id"));
    for (query, field) in [
        ("room_id=all&thread_id=nope".to_owned(), "thread_id"),
        ("room_id=all&top_level=yes".to_owned(), "top_level"),
        ("room_id=all&top_level=TRUE".to_owned(), "top_level"),
        (
            format!("room_id=all&thread_id={root_id}&top_level=true"),
            "top_level",
        ),
    ] {
        let reply = get(query.clone(), a.api_key.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{query}");
    }
    // The format comes before permission: c is no DM member.
    let dm = "dm:agent-a:agent-b";
    let before_permission = get(format!("room_id={dm}&thread_id=nope"), c.api_key.clone()).await;
    before_permission.assert_error(422, "invalid_field");
    // Permission before the thread: a missing thread in an unreadable room.
    get(
        format!("room_id={dm}&thread_id={MISSING_ID}"),
        c.api_key.clone(),
    )
    .await
    .assert_error(403, "forbidden");
    get(
        format!("room_id=room:nowhere&thread_id={MISSING_ID}"),
        OWNER_TOKEN.to_owned(),
    )
    .await
    .assert_error(404, "unknown_room");
    // Not a thread of this room.
    for target in [MISSING_ID, id(&lone), id(&reply), id(&dm_root)] {
        get(format!("room_id=all&thread_id={target}"), a.api_key.clone())
            .await
            .assert_error(404, "thread_not_found");
    }
    // The thread before the cursors.
    get(
        format!("room_id=all&thread_id={MISSING_ID}&before_event_seq=1&after_event_seq=1"),
        a.api_key.clone(),
    )
    .await
    .assert_error(404, "thread_not_found");
    get(
        format!("room_id=all&thread_id={root_id}&before_event_seq=1&after_event_seq=1"),
        a.api_key.clone(),
    )
    .await
    .assert_error(400, "mutually_exclusive_cursors");
    get(
        format!("room_id=all&thread_id={root_id}&limit=0"),
        a.api_key.clone(),
    )
    .await
    .assert_error(422, "invalid_field");
    server.shutdown().await.unwrap();
}

// ---- item shapes ----------------------------------------------------------------------

const MESSAGE_KEYS: [&str; 13] = [
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
    "created_at",
];

#[tokio::test]
async fn every_message_carries_the_thirteen_keys_in_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let root = sent(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    let reply_body = json!({"reply_to": id(&root), "body": "re", "client_key": "b"});
    let sent_keys = api
        .keys_of(
            Method::POST,
            "/v1/messages",
            &b.api_key,
            Some(reply_body.clone()),
        )
        .await;
    let mut with_dedup: Vec<&str> = MESSAGE_KEYS.to_vec();
    with_dedup.push("deduplicated");
    assert_eq!(sent_keys, with_dedup);
    // The replayed root answers with a null summary although it has replies.
    let replayed = send(
        &api,
        &a.api_key,
        json!({"room_id": "all", "body": "root", "client_key": "r"}),
    )
    .await;
    assert_eq!(replayed.status.as_u16(), 200, "{}", replayed.body);
    assert_eq!(replayed.body["thread_summary"], Value::Null);
    assert_eq!(replayed.body["deduplicated"], true);

    let single_keys = api
        .keys_of(
            Method::GET,
            &format!("/v1/messages/{}", id(&root)),
            &a.api_key,
            None,
        )
        .await;
    assert_eq!(single_keys, MESSAGE_KEYS);

    let raw = api
        .send_bytes(
            Method::GET,
            "/v1/messages?room_id=all&limit=1",
            Some(&a.api_key),
            &[],
            None,
        )
        .await
        .text();
    let items_text = &raw[raw.find("\"items\":[").unwrap() + "\"items\":[".len()..];
    let first_item = &items_text[..=items_text.find('}').unwrap()];
    assert_eq!(common::ordered_keys(first_item), MESSAGE_KEYS);

    let inbox = api
        .send_bytes(Method::GET, "/v1/inbox", Some(&a.api_key), &[], None)
        .await
        .text();
    let items_text = &inbox[inbox.find("\"items\":[").unwrap() + "\"items\":[".len()..];
    // The first item is a's root, whose summary is an object: cut after it.
    let end = items_text.find("\"created_at\"").unwrap();
    let close = end + items_text[end..].find('}').unwrap();
    let first_event = &items_text[..=close];
    let mut event_keys = vec!["inbox_seq", "kind", "woke"];
    event_keys.extend(MESSAGE_KEYS);
    assert_eq!(common::ordered_keys(first_event), event_keys);
    server.shutdown().await.unwrap();
}
