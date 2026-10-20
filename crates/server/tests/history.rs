//! S2: `GET /v1/messages` (cursor directions, `has_more`, `q`, permissions)
//! and `GET /v1/messages/{message_id}`.

mod common;

use std::collections::BTreeSet;

use common::{Api, Fixture, OWNER_TOKEN, RunningServer};
use platform_wire::messages::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use serde_json::{Value, json};

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
        .map(|item| seq(&item["event_seq"]))
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

/// Registers agent-a, agent-b, agent-c and returns their API keys.
async fn three_agents(api: &Api) -> (String, String, String) {
    let a = api.register("agent-a", None).await;
    let b = api.register("agent-b", None).await;
    let c = api.register("agent-c", None).await;
    (a.api_key, b.api_key, c.api_key)
}

async fn page(api: &Api, token: &str, query: &str) -> Value {
    let reply = api.get(&format!("/v1/messages?{query}"), token).await;
    assert_eq!(reply.status.as_u16(), 200, "{query}: {}", reply.body);
    reply.body
}

/// Five `all` messages interleaved with DMs; returns the `all` event_seqs.
async fn seed_all_room(api: &Api, a: &str, b: &str) -> Vec<i64> {
    let mut all_seqs = Vec::new();
    for index in 0..5 {
        let sent = api
            .sent(a, "all", &format!("all {index}"), &format!("all-{index}"))
            .await;
        all_seqs.push(seq(&sent["event_seq"]));
        api.sent(b, "agent-a", &format!("dm {index}"), &format!("dm-{index}"))
            .await;
    }
    all_seqs
}

#[tokio::test]
async fn default_and_before_pages_are_newest_first_and_exclusive() {
    let (_fx, server, api) = start().await;
    let (a, b, _) = three_agents(&api).await;
    let all_seqs = seed_all_room(&api, &a, &b).await;
    let newest_first: Vec<i64> = all_seqs.iter().rev().copied().collect();

    let first = page(&api, &a, "room_id=all&limit=2").await;
    assert_eq!(
        keys(&first),
        ["has_more", "items", "next_before_event_seq"].into()
    );
    assert_eq!(item_seqs(&first), newest_first[..2]);
    assert_eq!(first["has_more"], true);
    assert_eq!(seq(&first["next_before_event_seq"]), newest_first[1]);

    let cursor = first["next_before_event_seq"].as_str().unwrap();
    let second = page(
        &api,
        &a,
        &format!("room_id=all&limit=2&before_event_seq={cursor}"),
    )
    .await;
    assert_eq!(item_seqs(&second), newest_first[2..4]);
    assert_eq!(second["has_more"], true);

    let cursor = second["next_before_event_seq"].as_str().unwrap();
    let last = page(
        &api,
        &a,
        &format!("room_id=all&limit=2&before_event_seq={cursor}"),
    )
    .await;
    assert_eq!(item_seqs(&last), newest_first[4..]);
    assert_eq!(last["has_more"], false, "exactly the remaining item");

    let cursor = last["next_before_event_seq"].as_str().unwrap();
    let empty = page(&api, &a, &format!("room_id=all&before_event_seq={cursor}")).await;
    assert_eq!(
        empty,
        json!({"items": [], "next_before_event_seq": null, "has_more": false})
    );

    let full = page(&api, &a, "room_id=all").await;
    assert_eq!(item_seqs(&full), newest_first);
    assert_eq!(full["has_more"], false);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn after_pages_are_oldest_first_and_exclusive() {
    let (_fx, server, api) = start().await;
    let (a, b, _) = three_agents(&api).await;
    let all_seqs = seed_all_room(&api, &a, &b).await;

    let first = page(&api, &a, "room_id=all&after_event_seq=0&limit=3").await;
    assert_eq!(
        keys(&first),
        ["has_more", "items", "next_after_event_seq"].into()
    );
    assert_eq!(item_seqs(&first), all_seqs[..3]);
    assert_eq!(first["has_more"], true);

    let cursor = first["next_after_event_seq"].as_str().unwrap();
    let second = page(
        &api,
        &a,
        &format!("room_id=all&after_event_seq={cursor}&limit=3"),
    )
    .await;
    assert_eq!(item_seqs(&second), all_seqs[3..]);
    assert_eq!(second["has_more"], false);

    let exclusive = page(
        &api,
        &a,
        &format!("room_id=all&after_event_seq={}", all_seqs[4]),
    )
    .await;
    assert_eq!(
        exclusive,
        json!({"items": [], "next_after_event_seq": null, "has_more": false})
    );
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn limit_defaults_and_is_bounded() {
    let (_fx, server, api) = start().await;
    let (a, _, _) = three_agents(&api).await;
    for index in 0..=DEFAULT_PAGE_SIZE {
        api.sent(&a, "all", "m", &format!("k-{index}")).await;
    }

    let default = page(&api, &a, "room_id=all").await;
    assert_eq!(item_seqs(&default).len(), DEFAULT_PAGE_SIZE as usize);
    assert_eq!(default["has_more"], true);
    let max = page(&api, &a, &format!("room_id=all&limit={MAX_PAGE_SIZE}")).await;
    assert_eq!(item_seqs(&max).len(), DEFAULT_PAGE_SIZE as usize + 1);
    assert_eq!(max["has_more"], false);

    for limit in [
        "0".to_owned(),
        (MAX_PAGE_SIZE + 1).to_string(),
        "x".to_owned(),
    ] {
        let reply = api
            .get(&format!("/v1/messages?room_id=all&limit={limit}"), &a)
            .await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("limit"));
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn cursor_and_room_errors() {
    let (_fx, server, api) = start().await;
    let (a, _, _) = three_agents(&api).await;

    api.get(
        "/v1/messages?room_id=all&before_event_seq=5&after_event_seq=1",
        &a,
    )
    .await
    .assert_error(400, "mutually_exclusive_cursors");

    let bad_seq = api
        .get("/v1/messages?room_id=all&after_event_seq=007", &a)
        .await;
    bad_seq.assert_error(422, "invalid_field");
    assert_eq!(bad_seq.error_field(), Some("after_event_seq"));

    for room in [
        "",
        "room_id=",
        "room_id=dm:agent-b:agent-a",
        "room_id=dm:agent-a:agent-a",
        "room_id=dm:agent-a",
        "room_id=general",
    ] {
        let reply = api.get(&format!("/v1/messages?{room}"), &a).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("room_id"), "{room:?}");
    }

    let unknown = api.get("/v1/messages?room_id=all&offset=10", &a).await;
    unknown.assert_error(422, "unexpected_field");
    assert_eq!(unknown.error_field(), Some("offset"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn q_is_a_literal_substring_filter_with_paging() {
    let (_fx, server, api) = start().await;
    let (a, _, _) = three_agents(&api).await;
    let mut matching = Vec::new();
    for (index, body) in [
        "deploy ok",
        "100% done",
        "unrelated",
        "deploy failed",
        "a_b",
        "aXb",
        "DEPLOY upper",
        "redeploy",
    ]
    .into_iter()
    .enumerate()
    {
        let sent = api.sent(&a, "all", body, &format!("k-{index}")).await;
        if body.contains("deploy") {
            matching.push(seq(&sent["event_seq"]));
        }
    }

    let found = page(&api, &a, "room_id=all&q=deploy").await;
    assert_eq!(
        item_seqs(&found),
        matching.iter().rev().copied().collect::<Vec<_>>()
    );

    let first = page(&api, &a, "room_id=all&q=deploy&after_event_seq=0&limit=2").await;
    assert_eq!(item_seqs(&first), matching[..2]);
    assert_eq!(first["has_more"], true);
    let cursor = first["next_after_event_seq"].as_str().unwrap();
    let rest = page(
        &api,
        &a,
        &format!("room_id=all&q=deploy&after_event_seq={cursor}&limit=2"),
    )
    .await;
    assert_eq!(item_seqs(&rest), matching[2..]);
    assert_eq!(rest["has_more"], false);

    // LIKE wildcards are ordinary characters.
    let percent = page(&api, &a, "room_id=all&q=%25").await;
    assert_eq!(percent["items"][0]["body"], "100% done");
    assert_eq!(item_seqs(&percent).len(), 1);
    let underscore = page(&api, &a, "room_id=all&q=a_b").await;
    assert_eq!(item_seqs(&underscore).len(), 1, "`_` does not match `X`");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn agents_read_all_and_their_own_dms_and_the_owner_reads_everything() {
    let (_fx, server, api) = start().await;
    let (a, b, c) = three_agents(&api).await;
    let dm_ac = api.sent(&a, "agent-c", "for c", "k-1").await;
    api.sent(&a, "all", "for everyone", "k-2").await;

    api.get("/v1/messages?room_id=dm:agent-a:agent-c", &b)
        .await
        .assert_error(403, "forbidden");
    for token in [&a, &c, &OWNER_TOKEN.to_owned()] {
        let dm = page(&api, token, "room_id=dm:agent-a:agent-c").await;
        assert_eq!(dm["items"][0]["message_id"], dm_ac["message_id"]);
    }
    // S13: a DM nobody wrote to has no room yet (IF-WIRE-005).
    api.get("/v1/messages?room_id=dm:agent-b:agent-c", OWNER_TOKEN)
        .await
        .assert_error(404, "unknown_room");

    // Leaving `all` does not hide its history.
    api.put(
        "/v1/agents/agent-b/membership/all",
        &b,
        json!({"joined": false}),
    )
    .await;
    let all = page(&api, &b, "room_id=all").await;
    assert_eq!(all["items"][0]["body"], "for everyone");

    api.get("/v1/messages?room_id=all", "pak_unknown")
        .await
        .assert_error(401, "unauthorized");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn single_message_lookup_hides_what_the_caller_cannot_read() {
    let (_fx, server, api) = start().await;
    let (a, b, c) = three_agents(&api).await;
    let dm = api.sent(&a, "agent-c", "for c", "k-1").await;
    let id = dm["message_id"].as_str().unwrap();
    let path = format!("/v1/messages/{id}");

    let mut expected = dm.clone();
    expected.as_object_mut().unwrap().remove("deduplicated");
    for token in [a.as_str(), c.as_str(), OWNER_TOKEN] {
        let reply = api.get(&path, token).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
        assert_eq!(
            reply.body, expected,
            "a single object without paging fields"
        );
    }

    api.get(&path, &b)
        .await
        .assert_error(404, "message_not_found");
    api.get("/v1/messages/msg-01J00000000000000000000000", OWNER_TOKEN)
        .await
        .assert_error(404, "message_not_found");
    api.get("/v1/messages/not-an-id", OWNER_TOKEN)
        .await
        .assert_error(404, "message_not_found");
    server.shutdown().await.unwrap();
}
