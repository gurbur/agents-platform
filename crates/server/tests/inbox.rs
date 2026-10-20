//! S3: `GET /v1/inbox`, `GET /v1/agents/{agent_id}/inbox` (snapshot
//! pagination, permissions, no side effects) and `PUT /v1/inbox/read-marker`.

mod common;

use std::collections::BTreeSet;
use std::time::Duration;

use common::{Api, Fixture, OWNER_TOKEN, RunningServer};
use platform_wire::messages::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use reqwest::Method;
use serde_json::{Value, json};

const READ_MARKER: &str = "/v1/inbox/read-marker";

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    (fx, server, api)
}

fn seq(value: &Value) -> i64 {
    value
        .as_str()
        .expect("sequence string")
        .parse()
        .expect("decimal")
}

fn item_seqs(page: &Value) -> Vec<i64> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| seq(&item["inbox_seq"]))
        .collect()
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect()
}

async fn own_page(api: &Api, token: &str, query: &str) -> Value {
    let reply = api.get(&format!("/v1/inbox?{query}"), token).await;
    assert_eq!(reply.status.as_u16(), 200, "{query}: {}", reply.body);
    reply.body
}

async fn owner_page(api: &Api, agent_id: &str, query: &str) -> Value {
    let reply = api
        .get(&format!("/v1/agents/{agent_id}/inbox?{query}"), OWNER_TOKEN)
        .await;
    assert_eq!(reply.status.as_u16(), 200, "{query}: {}", reply.body);
    reply.body
}

async fn put_marker(api: &Api, token: &str, through_seq: &str) -> common::Reply {
    api.put(READ_MARKER, token, json!({"through_seq": through_seq}))
        .await
}

/// Sends `count` broadcasts from `token`, numbering keys from `first`.
async fn broadcast(api: &Api, token: &str, first: usize, count: usize) {
    for index in first..first + count {
        api.sent(
            token,
            "all",
            &format!("all {index}"),
            &format!("all-{index}"),
        )
        .await;
    }
}

async fn read_marker_in_db(fx: &Fixture, agent_id: &'static str) -> i64 {
    fx.read_db(move |conn| {
        Ok(conn.query_row(
            "SELECT read_marker FROM read_state WHERE agent_id = ?1",
            [agent_id],
            |row| row.get(0),
        )?)
    })
    .await
}

/// Every `notification_pending` column, to show what a marker change does to it.
async fn notification_rows(fx: &Fixture) -> Vec<Vec<Option<String>>> {
    fx.read_db(|conn| {
        let mut statement = conn.prepare(
            "SELECT agent_id, route_version, state, target_seq, last_accepted_seq, attempt,
                    next_due_at, redrive_due_at, last_attempt_at, last_error
             FROM notification_pending ORDER BY agent_id",
        )?;
        let rows = statement.query_map([], |row| {
            (0..10)
                .map(|index| {
                    row.get::<_, rusqlite::types::Value>(index)
                        .map(|value| match value {
                            rusqlite::types::Value::Null => None,
                            other => Some(format!("{other:?}")),
                        })
                })
                .collect::<rusqlite::Result<Vec<_>>>()
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    })
    .await
}

/// A second connection kept open across requests: `PRAGMA data_version`
/// changes only when another connection commits (SQLite docs).
struct WriteProbe(rusqlite::Connection);

impl WriteProbe {
    fn open(fx: &Fixture) -> Self {
        Self(rusqlite::Connection::open(fx.db_path()).expect("open probe connection"))
    }

    fn data_version(&self) -> i64 {
        self.0
            .query_row("PRAGMA data_version", [], |row| row.get(0))
            .expect("data_version")
    }
}

#[tokio::test]
async fn snapshot_head_bounds_every_page_of_a_traversal() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    let b = api.register("agent-b", None).await.api_key;
    // Both are `all` members, so each broadcast is one row in b's inbox.
    broadcast(&api, &a, 1, 44).await;
    assert_eq!(put_marker(&api, &b, "41").await.status.as_u16(), 200);

    // API_V1 example: head 44, `after_seq=41&limit=1` → 42.
    let first = own_page(&api, &b, "after_seq=41&limit=1").await;
    assert_eq!(item_seqs(&first), vec![42]);
    assert_eq!(first["agent_id"], "agent-b");
    assert_eq!(first["read_marker"], "41");
    assert_eq!(first["snapshot_head_seq"], "44");
    assert_eq!(first["next_after_seq"], "42");
    assert_eq!(first["has_more"], true);
    assert_eq!(
        keys(&first),
        BTreeSet::from([
            "agent_id",
            "read_marker",
            "snapshot_head_seq",
            "items",
            "next_after_seq",
            "has_more"
        ])
    );
    let item = &first["items"][0];
    assert_eq!(
        keys(item),
        BTreeSet::from([
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
            "wake",
            "mentions",
            "created_at"
        ])
    );
    assert_eq!(item["room_id"], "all");
    assert_eq!(item["sender_id"], "agent-a");
    assert_eq!(item["body"], "all 42");
    let message = api
        .get(
            &format!("/v1/messages/{}", item["message_id"].as_str().unwrap()),
            &b,
        )
        .await;
    assert_eq!(message.body["event_seq"], item["event_seq"]);
    assert_eq!(message.body["created_at"], item["created_at"]);

    // Without `after_seq` the marker is the lower bound.
    assert_eq!(own_page(&api, &b, "limit=1").await, first);

    // 45 arrives mid-traversal and stays out of this snapshot.
    broadcast(&api, &a, 45, 1).await;
    let snapshot = first["snapshot_head_seq"].as_str().unwrap();
    let mut after = first["next_after_seq"].as_str().unwrap().to_owned();
    let mut seen = item_seqs(&first);
    loop {
        let query = format!("after_seq={after}&until_seq={snapshot}&limit=1");
        let page = own_page(&api, &b, &query).await;
        assert_eq!(
            own_page(&api, &b, &query).await,
            page,
            "a retried page is identical"
        );
        assert_eq!(page["snapshot_head_seq"], snapshot);
        seen.extend(item_seqs(&page));
        after = page["next_after_seq"].as_str().unwrap().to_owned();
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(seen, vec![42, 43, 44], "no gap, no duplicate, no 45");
    assert_eq!(after, "44");

    // An exhausted snapshot keeps `after_seq` as the cursor.
    let empty = own_page(&api, &b, "after_seq=44&until_seq=44").await;
    assert_eq!(item_seqs(&empty), Vec::<i64>::new());
    assert_eq!(empty["next_after_seq"], "44");
    assert_eq!(empty["has_more"], false);

    // The next snapshot picks up 45.
    let next = own_page(&api, &b, "after_seq=44").await;
    assert_eq!(item_seqs(&next), vec![45]);
    assert_eq!(next["snapshot_head_seq"], "45");
    assert_eq!(next["has_more"], false);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn limit_defaults_and_is_bounded() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    broadcast(&api, &a, 0, DEFAULT_PAGE_SIZE as usize + 1).await;

    let default = own_page(&api, &a, "").await;
    assert_eq!(item_seqs(&default).len(), DEFAULT_PAGE_SIZE as usize);
    assert_eq!(default["has_more"], true);
    let max = own_page(&api, &a, &format!("limit={MAX_PAGE_SIZE}")).await;
    assert_eq!(item_seqs(&max).len(), DEFAULT_PAGE_SIZE as usize + 1);
    assert_eq!(max["has_more"], false);

    for limit in [
        "0".to_owned(),
        (MAX_PAGE_SIZE + 1).to_string(),
        "+5".to_owned(),
        "x".to_owned(),
    ] {
        for path in ["/v1/inbox", "/v1/agents/agent-a/inbox"] {
            let token = if path == "/v1/inbox" { &a } else { OWNER_TOKEN };
            let reply = api.get(&format!("{path}?limit={limit}"), token).await;
            reply.assert_error(422, "invalid_field");
            assert_eq!(reply.error_field(), Some("limit"), "{path} {limit}");
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn query_errors_follow_the_strict_order() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;

    for (query, code, field) in [
        ("after_seq=007", "invalid_field", "after_seq"),
        ("after_seq=-1", "invalid_field", "after_seq"),
        (
            "until_seq=9223372036854775808",
            "invalid_field",
            "until_seq",
        ),
        ("until_seq=", "invalid_field", "until_seq"),
        ("offset=3", "unexpected_field", "offset"),
        ("limit=1&limit=2", "invalid_field", "limit"),
    ] {
        let own = api.get(&format!("/v1/inbox?{query}"), &a).await;
        own.assert_error(422, code);
        assert_eq!(own.error_field(), Some(field), "{query}");
        let owner = api
            .get(&format!("/v1/agents/agent-a/inbox?{query}"), OWNER_TOKEN)
            .await;
        owner.assert_error(422, code);
        assert_eq!(owner.error_field(), Some(field), "{query}");
    }

    // Names → permission → values.
    api.get("/v1/inbox?offset=3", OWNER_TOKEN)
        .await
        .assert_error(422, "unexpected_field");
    api.get("/v1/inbox?after_seq=007", OWNER_TOKEN)
        .await
        .assert_error(403, "forbidden");
    api.get("/v1/agents/agent-a/inbox?after_seq=007", &a)
        .await
        .assert_error(403, "forbidden");
    // Values come before the agent lookup on the owner path.
    api.get("/v1/agents/nobody/inbox?after_seq=007", OWNER_TOKEN)
        .await
        .assert_error(422, "invalid_field");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_marker_moves_only_up_through_put() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    let b = api.register("agent-b", None).await.api_key;
    broadcast(&api, &a, 1, 3).await;
    let notifications = notification_rows(&fx).await;

    for _ in 0..2 {
        let page = own_page(&api, &b, "").await;
        assert_eq!(page["read_marker"], "0");
        assert_eq!(item_seqs(&page), vec![1, 2, 3]);
    }
    assert_eq!(read_marker_in_db(&fx, "agent-b").await, 0, "GET is no ack");

    let advanced = put_marker(&api, &b, "2").await;
    assert_eq!(advanced.status.as_u16(), 200, "{}", advanced.body);
    assert_eq!(
        advanced.body,
        json!({"agent_id": "agent-b", "read_marker": "2"})
    );

    put_marker(&api, &b, "4")
        .await
        .assert_error(409, "read_marker_ahead");
    for (through_seq, why) in [("1", "lower is a max no-op"), ("2", "same is idempotent")] {
        let reply = put_marker(&api, &b, through_seq).await;
        assert_eq!(reply.status.as_u16(), 200, "{why}: {}", reply.body);
        assert_eq!(reply.body["read_marker"], "2", "{why}");
    }
    assert_eq!(put_marker(&api, &b, "0").await.body["read_marker"], "2");
    assert_eq!(read_marker_in_db(&fx, "agent-b").await, 2);

    assert_eq!(
        notification_rows(&fx).await,
        notifications,
        "GETs and a marker below the head leave delivery state alone"
    );

    let rest = own_page(&api, &b, "").await;
    assert_eq!(rest["read_marker"], "2");
    assert_eq!(
        item_seqs(&rest),
        vec![3],
        "the marker is the default after_seq"
    );

    assert_eq!(put_marker(&api, &b, "3").await.body["read_marker"], "3");
    let caught_up = own_page(&api, &b, "").await;
    assert_eq!(item_seqs(&caught_up), Vec::<i64>::new());
    assert_eq!(caught_up["next_after_seq"], "3");
    assert_eq!(caught_up["snapshot_head_seq"], "3");
    assert_eq!(caught_up["has_more"], false);

    // History stays whole and a's marker is its own.
    let history = api.get("/v1/messages?room_id=all", &b).await;
    assert_eq!(history.body["items"].as_array().unwrap().len(), 3);
    assert_eq!(read_marker_in_db(&fx, "agent-a").await, 0);
    // At the head the same transaction makes b idle (IF-DLV-001); a is untouched.
    let after = notification_rows(&fx).await;
    assert_eq!(after[0], notifications[0], "agent-a's row");
    assert_eq!(after[1][2].as_deref(), Some("Text(\"idle\")"));
    assert_eq!(after[1][6], None, "next_due_at");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_marker_body_and_principal_errors() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    broadcast(&api, &a, 1, 1).await;

    put_marker(&api, OWNER_TOKEN, "1")
        .await
        .assert_error(403, "forbidden");
    let unauthenticated = api
        .send(
            Method::PUT,
            READ_MARKER,
            None,
            Some(json!({"through_seq": "1"})),
        )
        .await;
    unauthenticated.assert_error(401, "unauthorized");

    for (body, code, field) in [
        (json!({"through_seq": 1}), "invalid_field", "through_seq"),
        (json!({"through_seq": "01"}), "invalid_field", "through_seq"),
        (json!({}), "invalid_field", "through_seq"),
        (
            json!({"through_seq": "1", "agent_id": "agent-a"}),
            "unexpected_field",
            "agent_id",
        ),
    ] {
        let reply = api.put(READ_MARKER, &a, body.clone()).await;
        reply.assert_error(422, code);
        assert_eq!(reply.error_field(), Some(field), "{body}");
    }
    api.send_raw(
        Method::PUT,
        READ_MARKER,
        Some(&a),
        Some("text/plain"),
        Some(br#"{"through_seq":"1"}"#.to_vec()),
    )
    .await
    .assert_error(415, "unsupported_media_type");
    api.send_raw(
        Method::PUT,
        READ_MARKER,
        Some(&a),
        Some("application/json"),
        Some(b"{".to_vec()),
    )
    .await
    .assert_error(400, "invalid_json");

    assert_eq!(read_marker_in_db(&fx, "agent-a").await, 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn inboxes_are_private_and_the_owner_reads_without_side_effects() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    let b = api.register("agent-b", None).await.api_key;
    let c = api.register("agent-c", None).await.api_key;
    api.sent(&a, "all", "to everyone", "k-all").await;
    api.sent(&a, "agent-b", "to b", "k-b").await;
    api.sent(&a, "agent-c", "to c", "k-c").await;
    assert_eq!(put_marker(&api, &b, "1").await.status.as_u16(), 200);

    for path in [
        "/v1/agents/agent-a/inbox",
        "/v1/agents/agent-b/inbox",
        "/v1/agents/nobody/inbox",
    ] {
        api.get(path, &b).await.assert_error(403, "forbidden");
    }
    api.get("/v1/inbox", OWNER_TOKEN)
        .await
        .assert_error(403, "forbidden");
    api.send(Method::GET, "/v1/inbox", None, None)
        .await
        .assert_error(401, "unauthorized");
    for missing in ["nobody", "Not-A-Slug", "all"] {
        api.get(&format!("/v1/agents/{missing}/inbox"), OWNER_TOKEN)
            .await
            .assert_error(404, "agent_not_found");
    }

    let own = own_page(&api, &b, "after_seq=0").await;
    let rooms: Vec<&str> = own["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["room_id"].as_str().unwrap())
        .collect();
    assert_eq!(rooms, vec!["all", "dm:agent-a:agent-b"], "no agent-c DM");
    assert_eq!(owner_page(&api, "agent-b", "after_seq=0").await, own);
    let owner_default = owner_page(&api, "agent-b", "").await;
    assert_eq!(owner_default["read_marker"], "1");
    assert_eq!(item_seqs(&owner_default), vec![2]);
    let c_page = owner_page(&api, "agent-c", "").await;
    assert_eq!(c_page["agent_id"], "agent-c");
    assert_eq!(c_page["items"][1]["body"], "to c");
    assert_eq!(own_page(&api, &c, "").await, c_page);

    assert_eq!(
        read_marker_in_db(&fx, "agent-b").await,
        1,
        "owner read is no ack"
    );
    assert_eq!(read_marker_in_db(&fx, "agent-c").await, 0);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn get_paths_commit_nothing() {
    let (fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    let b = api.register("agent-b", None).await.api_key;
    broadcast(&api, &a, 1, 3).await;
    // S14: the first request with each key records `last_seen_at`, a commit
    // off the request path (IF-WIRE-013). Let those land first; within the
    // throttle interval the GETs below record nothing more.
    own_page(&api, &b, "").await;
    common::eventually(
        Duration::from_secs(5),
        "last_seen_at of both keys",
        || async {
            let directory = api.get("/v1/agents", OWNER_TOKEN).await.body;
            let items = directory["items"].as_array()?;
            items
                .iter()
                .all(|item| item["last_seen_at"].is_string())
                .then_some(())
        },
    )
    .await;
    let probe = WriteProbe::open(&fx);
    let before = probe.data_version();

    own_page(&api, &b, "").await;
    own_page(&api, &b, "after_seq=1&until_seq=2&limit=1").await;
    own_page(&api, &b, "after_seq=9&until_seq=99").await;
    owner_page(&api, "agent-b", "").await;
    owner_page(&api, "agent-a", "limit=1").await;
    api.get("/v1/inbox?after_seq=007", &b).await;
    api.get("/v1/agents/nobody/inbox", OWNER_TOKEN).await;
    assert_eq!(probe.data_version(), before, "inbox GETs commit nothing");

    // The probe does see commits: the PUT is one.
    assert_eq!(put_marker(&api, &b, "3").await.status.as_u16(), 200);
    assert_ne!(probe.data_version(), before);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_agent_without_a_receiver_still_has_its_inbox() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await.api_key;
    let b = api.register("agent-b", None).await;
    assert!(b.body["receiver_url"].is_null());
    api.sent(&a, "all", "while offline", "k-1").await;
    api.sent(&a, "agent-b", "dm while offline", "k-2").await;

    let offline = owner_page(&api, "agent-b", "").await;
    assert_eq!(item_seqs(&offline), vec![1, 2]);

    // Reconnecting does not consume anything.
    let reconnected = api
        .patch(
            "/v1/agents/agent-b/receiver",
            &b.api_key,
            json!({"receiver_url": "http://127.0.0.1:9/hook"}),
        )
        .await;
    assert_eq!(reconnected.status.as_u16(), 200, "{}", reconnected.body);
    let page = own_page(&api, &b.api_key, "").await;
    assert_eq!(page, offline);
    assert_eq!(page["read_marker"], "0");
    server.shutdown().await.unwrap();
}
