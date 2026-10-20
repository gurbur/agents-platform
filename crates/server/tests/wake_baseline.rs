//! S13 delivery baseline (`IF-DLV-001` "wake 집합과 기준선", ADR-0011):
//! `target_seq` is the last woken `inbox_seq`, "unacknowledged" means
//! `read_marker < target_seq`, quiet rows only move the head, and the wake
//! body stays `{v, agent_id, inbox_seq}` with `inbox_seq` the head at the
//! start of the attempt. Also `PATCH …/receiver` with `wake_policy`.
//!
//! Timing follows `tests/delivery.rs`: short injected `[delivery]` values
//! and bounded polls; "nothing happened" is checked after a quiet period
//! longer than any scheduled retry.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::receiver::{MockReceiver, MockResponse, Received};
use common::{Api, Fixture, OWNER_TOKEN, Registered, Reply, RunningServer, eventually};
use reqwest::Method;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);
/// Longer than `backoff_max` plus scheduling slack, and than the redrive.
const QUIET: Duration = Duration::from_millis(3500);
/// How long a scripted receiver holds a response.
const HOLD: Duration = Duration::from_millis(700);

const DELIVERY: &str = "connect_timeout_ms = 1000\nrequest_timeout_ms = 3000\n\
                        backoff_min_secs = 1\nbackoff_max_secs = 2\nredrive_after_secs = 1\n";

struct Harness {
    _fx: Fixture,
    server: RunningServer,
    api: Api,
}

static KEYS: AtomicUsize = AtomicUsize::new(0);

async fn start() -> Harness {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config_with_delivery(DELIVERY)).await;
    let api = server.api();
    Harness {
        _fx: fx,
        server,
        api,
    }
}

impl Harness {
    async fn dm(&self, sender: &Registered, to: &str, wake: bool) -> Value {
        let key = format!("k-{}", KEYS.fetch_add(1, Ordering::Relaxed));
        let reply = self
            .api
            .post(
                "/v1/messages",
                &sender.api_key,
                json!({"to": to, "body": "x", "client_key": key, "wake": wake}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 201, "{}", reply.body);
        reply.body
    }

    async fn status(&self, agent_id: &str) -> Value {
        let reply = self
            .api
            .get(&format!("/v1/agents/{agent_id}/delivery"), OWNER_TOKEN)
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
        reply.body
    }

    async fn wait_status(&self, agent_id: &str, what: &str, accept: impl Fn(&Value) -> bool) {
        eventually(WAIT, what, || async {
            let status = self.status(agent_id).await;
            accept(&status).then_some(())
        })
        .await;
    }

    async fn retry(&self, agent_id: &str) -> Reply {
        self.api
            .send(
                Method::POST,
                &format!("/v1/agents/{agent_id}/delivery/retry"),
                Some(OWNER_TOKEN),
                None,
            )
            .await
    }

    async fn receiver(&self, agent_id: &str, body: Value) -> Reply {
        self.api
            .patch(
                &format!("/v1/agents/{agent_id}/receiver"),
                OWNER_TOKEN,
                body,
            )
            .await
    }

    async fn put_marker(&self, agent: &Registered, through_seq: &str) {
        let reply = self
            .api
            .put(
                "/v1/inbox/read-marker",
                &agent.api_key,
                json!({"through_seq": through_seq}),
            )
            .await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }

    async fn shutdown(self) {
        self.server.shutdown().await.unwrap();
    }
}

fn seqs(received: &[Received]) -> Vec<String> {
    received.iter().map(Received::inbox_seq).collect()
}

#[tokio::test]
async fn quiet_rows_alone_stay_idle_without_posts_redrive_or_retry() {
    let h = start().await;
    let mock = MockReceiver::start().await;
    let a = h.api.register("agent-a", None).await;
    h.api.register("agent-b", Some(&mock.url())).await;

    h.dm(&a, "agent-b", false).await;
    h.dm(&a, "agent-b", false).await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(mock.count(), 0, "a quiet row wakes no one");

    let status = h.status("agent-b").await;
    assert_eq!(status["state"], "idle");
    assert_eq!(status["inbox_head_seq"], "2");
    assert_eq!(status["read_marker"], "0");
    assert_eq!(status["pending_seq"], Value::Null);
    let retried = h.retry("agent-b").await;
    assert_eq!(retried.status.as_u16(), 200, "{}", retried.body);
    assert_eq!(retried.body["state"], "idle");
    assert_eq!(retried.body["pending_seq"], Value::Null);
    tokio::time::sleep(QUIET).await;
    assert_eq!(mock.count(), 0, "no redrive and no retry for quiet rows");
    h.shutdown().await;
}

#[tokio::test]
async fn the_post_carries_the_head_and_the_marker_need_only_reach_the_wake_row() {
    let h = start().await;
    let mock = MockReceiver::start().await;
    let a = h.api.register("agent-a", None).await;
    let b = h.api.register("agent-b", None).await;

    h.dm(&a, "agent-b", true).await;
    h.dm(&a, "agent-b", false).await;
    let reply = h
        .receiver("agent-b", json!({"receiver_url": mock.url()}))
        .await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    assert_eq!(reply.body["state"], "pending", "a wake is owed");

    let received = mock.wait_for(1, WAIT).await;
    assert_eq!(
        received[0].json(),
        json!({"v": 1, "agent_id": "agent-b", "inbox_seq": "2"}),
        "three keys; inbox_seq is the head, the quiet row included"
    );
    h.wait_status("agent-b", "the 202 is recorded", |s| {
        s["last_accepted_seq"] == "2"
    })
    .await;
    let status = h.status("agent-b").await;
    assert_eq!(status["state"], "pending");
    assert_eq!(status["pending_seq"], "1", "the last woken row");

    h.put_marker(&b, "1").await;
    let status = h.status("agent-b").await;
    assert_eq!(status["state"], "idle", "the quiet row 2 owes nothing");
    assert_eq!(status["inbox_head_seq"], "2");
    tokio::time::sleep(QUIET).await;
    assert_eq!(mock.count(), 1, "no redrive once the wake is acknowledged");
    h.shutdown().await;
}

#[tokio::test]
async fn setting_the_receiver_again_is_pending_only_with_an_owed_wake() {
    let h = start().await;
    let mock = MockReceiver::start().await;
    let a = h.api.register("agent-a", None).await;
    h.api.register("agent-c", None).await;

    h.dm(&a, "agent-c", false).await;
    let reply = h
        .receiver("agent-c", json!({"receiver_url": mock.url()}))
        .await;
    assert_eq!(reply.body["state"], "idle", "{}", reply.body);
    tokio::time::sleep(QUIET).await;
    assert_eq!(mock.count(), 0);
    h.shutdown().await;
}

#[tokio::test]
async fn quiet_rows_in_flight_do_not_repost_but_wake_rows_do() {
    let h = start().await;
    let mock = MockReceiver::start().await;
    mock.push(MockResponse::accepted().after(HOLD));
    mock.push(MockResponse::accepted().after(HOLD));
    let a = h.api.register("agent-a", None).await;
    let b = h.api.register("agent-b", Some(&mock.url())).await;

    h.dm(&a, "agent-b", true).await;
    mock.wait_for(1, WAIT).await;
    h.dm(&a, "agent-b", false).await;
    h.wait_status("agent-b", "1 accepted", |s| s["last_accepted_seq"] == "1")
        .await;
    h.put_marker(&b, "1").await;
    tokio::time::sleep(QUIET).await;
    assert_eq!(seqs(&mock.received()), ["1"], "the quiet row is not posted");

    h.dm(&a, "agent-b", true).await;
    mock.wait_for(2, WAIT).await;
    h.dm(&a, "agent-b", true).await;
    let received = mock.wait_for(3, WAIT).await;
    assert_eq!(
        seqs(&received),
        ["1", "3", "4"],
        "a wake row during the flight is posted right after the 202"
    );
    h.shutdown().await;
}

#[tokio::test]
async fn wake_policy_applies_to_later_events_only() {
    let h = start().await;
    let mock = MockReceiver::start().await;
    let a = h.api.register("agent-a", None).await;
    h.api.register("agent-c", Some(&mock.url())).await;

    h.dm(&a, "agent-c", false).await;
    let before = h.status("agent-c").await;
    let reply = h
        .receiver("agent-c", json!({"wake_policy": "always"}))
        .await;
    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    assert_eq!(reply.body["wake_policy"], "always");
    assert_eq!(reply.body["route_version"], before["route_version"]);
    assert_eq!(reply.body["state"], "idle", "nothing is backdated");
    tokio::time::sleep(QUIET).await;
    assert_eq!(mock.count(), 0);

    h.dm(&a, "agent-c", false).await;
    let received = mock.wait_for(1, WAIT).await;
    assert_eq!(
        received[0].json(),
        json!({"v": 1, "agent_id": "agent-c", "inbox_seq": "2"})
    );
    h.shutdown().await;
}

// ---- PATCH /v1/agents/{id}/receiver ------------------------------------------------

#[tokio::test]
async fn the_receiver_patch_takes_url_wake_policy_or_both() {
    let h = start().await;
    let a = h.api.register("agent-a", None).await;

    let policy = h
        .receiver("agent-a", json!({"wake_policy": "always"}))
        .await;
    assert_eq!(policy.status.as_u16(), 200, "{}", policy.body);
    let keys = h
        .api
        .keys_of(
            Method::PATCH,
            "/v1/agents/agent-a/receiver",
            OWNER_TOKEN,
            Some(json!({"wake_policy": "always"})),
        )
        .await;
    assert_eq!(
        keys,
        [
            "agent_id",
            "receiver_url",
            "route_version",
            "wake_policy",
            "state"
        ]
    );
    assert_eq!(policy.body["route_version"], 1);
    assert_eq!(policy.body["receiver_url"], Value::Null);

    let url = h
        .receiver("agent-a", json!({"receiver_url": "http://127.0.0.1:9/n"}))
        .await;
    assert_eq!(url.body["route_version"], 2);
    assert_eq!(url.body["wake_policy"], "always", "kept when omitted");
    let both = h
        .api
        .patch(
            "/v1/agents/agent-a/receiver",
            &a.api_key,
            json!({"receiver_url": null, "wake_policy": "sender"}),
        )
        .await;
    assert_eq!(both.status.as_u16(), 200, "{}", both.body);
    assert_eq!(both.body["route_version"], 3);
    assert_eq!(both.body["wake_policy"], "sender");
    assert_eq!(both.body["state"], "no_receiver");

    h.receiver("agent-a", json!({}))
        .await
        .assert_error(422, "invalid_field");
    h.receiver("agent-a", json!({"wake_policy": null}))
        .await
        .assert_error(422, "invalid_field");
    for value in [json!("Always"), json!("never"), json!(1)] {
        let reply = h.receiver("agent-a", json!({"wake_policy": value})).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some("wake_policy"), "{value}");
    }
    h.shutdown().await;
}

#[tokio::test]
async fn wake_policy_shows_in_the_owner_directory_and_delivery_only() {
    let h = start().await;
    let a = h.api.register("agent-a", None).await;
    h.receiver("agent-a", json!({"wake_policy": "always"}))
        .await;

    let owner = h.api.get("/v1/agents", OWNER_TOKEN).await;
    let item = &owner.body["items"][0];
    assert_eq!(item["wake_policy"], "always");
    let raw = h
        .api
        .send_bytes(Method::GET, "/v1/agents", Some(OWNER_TOKEN), &[], None)
        .await
        .text();
    let item_text = &raw[raw.find("[{").unwrap() + 1..];
    let keys = common::ordered_keys(&item_text[..item_text.find("}]").unwrap() + 1]);
    let url_at = keys.iter().position(|key| key == "receiver_url").unwrap();
    assert_eq!(keys[url_at + 1], "wake_policy", "right after receiver_url");

    let agent = h.api.get("/v1/agents", &a.api_key).await;
    assert!(agent.body["items"][0].get("wake_policy").is_none());
    let status = h.status("agent-a").await;
    assert_eq!(status["wake_policy"], "always");
    h.shutdown().await;
}
