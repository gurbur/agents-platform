//! S4: the receiver wake worker, the callback wire, the `IF-DLV-001`
//! transitions and `GET/POST /v1/agents/{id}/delivery[/retry]`, against a
//! mock receiver.
//!
//! Timing: every test injects short `[delivery]` values through
//! [`Tuning`] and asserts only "within the configured range", with
//! [`EARLY`] for millisecond truncation of stored deadlines and [`SLACK`] for
//! scheduling. Waits are bounded polls, never fixed expectations of speed.

mod common;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime};

use axum::http::Version;
use common::receiver::{MockBody, MockReceiver, MockResponse, Received};
use common::{Api, Fixture, OWNER_TOKEN, Registered, Reply, RunningServer, eventually};
use platform_wire::delivery::MAX_WAKE_BODY_BYTES;
use platformd::clock;
use platformd::config::Config;
use reqwest::Method;
use serde_json::{Value, json};

/// Tolerance for deadlines stored with millisecond precision.
const EARLY: Duration = Duration::from_millis(20);
/// Scheduling tolerance on top of a configured delay.
const SLACK: Duration = Duration::from_millis(1500);
/// Upper bound for any single wait; only guards against a hang.
const WAIT: Duration = Duration::from_secs(20);

/// `[delivery]` values injected into the server under test.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    connect_timeout: Duration,
    request_timeout: Duration,
    backoff_min: Duration,
    backoff_max: Duration,
    redrive_after: Duration,
}

impl Tuning {
    /// Short enough for retries to happen within a test. Redrive is far
    /// away unless a test shortens it.
    fn fast() -> Self {
        Self {
            connect_timeout: Duration::from_millis(1000),
            request_timeout: Duration::from_millis(2000),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(2),
            redrive_after: Duration::from_secs(60),
        }
    }

    fn toml(&self) -> String {
        format!(
            "connect_timeout_ms = {}\nrequest_timeout_ms = {}\nbackoff_min_secs = {}\n\
             backoff_max_secs = {}\nredrive_after_secs = {}\n",
            self.connect_timeout.as_millis(),
            self.request_timeout.as_millis(),
            self.backoff_min.as_secs(),
            self.backoff_max.as_secs(),
            self.redrive_after.as_secs(),
        )
    }

    /// Long enough for any scheduled retry to have happened.
    fn quiet_period(&self) -> Duration {
        self.backoff_max + SLACK
    }
}

/// A `notification_pending` row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    route_version: i64,
    state: String,
    target_seq: i64,
    last_accepted_seq: Option<i64>,
    attempt: i64,
    next_due_at: Option<String>,
    redrive_due_at: Option<String>,
    last_attempt_at: Option<String>,
    last_error: Option<String>,
}

struct Harness {
    fx: Fixture,
    config: Config,
    server: RunningServer,
    api: Api,
    tuning: Tuning,
}

static CLIENT_KEYS: AtomicUsize = AtomicUsize::new(0);

async fn start(tuning: Tuning) -> Harness {
    let fx = Fixture::new();
    let config = fx.load_config_with_delivery(&tuning.toml());
    let server = RunningServer::start(&config).await;
    let api = server.api();
    Harness {
        fx,
        config,
        server,
        api,
        tuning,
    }
}

fn delivery_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/delivery")
}

fn retry_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/delivery/retry")
}

fn keys(value: &Value) -> BTreeSet<&str> {
    value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect()
}

fn set<'a>(names: &[&'a str]) -> BTreeSet<&'a str> {
    names.iter().copied().collect()
}

fn deadline(row: &Row) -> SystemTime {
    let text = row.next_due_at.as_deref().expect("next_due_at is set");
    clock::parse_rfc3339_ms(text).expect("stored timestamp")
}

fn gap(earlier: &Received, later: &Received) -> Duration {
    later.at.duration_since(earlier.at)
}

fn assert_within(actual: Duration, low: Duration, high: Duration, what: &str) {
    assert!(
        actual + EARLY >= low && actual <= high,
        "{what}: {actual:?} not in [{low:?}, {high:?}]"
    );
}

impl Harness {
    async fn register(&self, agent_id: &str, receiver_url: Option<&str>) -> Registered {
        self.api.register(agent_id, receiver_url).await
    }

    /// A DM from `sender` to `to`, asserting 201.
    async fn dm(&self, sender: &Registered, to: &str) {
        let key = format!("k-{}", CLIENT_KEYS.fetch_add(1, Ordering::Relaxed));
        self.api.sent(&sender.api_key, to, "hello", &key).await;
    }

    async fn status(&self, agent_id: &str) -> Value {
        let reply = self.api.get(&delivery_path(agent_id), OWNER_TOKEN).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
        reply.body
    }

    async fn retry(&self, agent_id: &str, token: &str) -> Reply {
        self.api
            .send(Method::POST, &retry_path(agent_id), Some(token), None)
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

    /// Polls `GET delivery` until `accept` holds.
    async fn wait_status(
        &self,
        agent_id: &str,
        what: &str,
        accept: impl Fn(&Value) -> bool,
    ) -> Value {
        eventually(WAIT, what, || async {
            let status = self.status(agent_id).await;
            accept(&status).then_some(status)
        })
        .await
    }

    async fn row(&self, agent_id: &str) -> Row {
        let agent_id = agent_id.to_owned();
        self.fx
            .read_db(move |conn| {
                Ok(conn.query_row(
                    "SELECT route_version, state, target_seq, last_accepted_seq, attempt,
                            next_due_at, redrive_due_at, last_attempt_at, last_error
                     FROM notification_pending WHERE agent_id = ?1",
                    [agent_id],
                    |row| {
                        Ok(Row {
                            route_version: row.get(0)?,
                            state: row.get(1)?,
                            target_seq: row.get(2)?,
                            last_accepted_seq: row.get(3)?,
                            attempt: row.get(4)?,
                            next_due_at: row.get(5)?,
                            redrive_due_at: row.get(6)?,
                            last_attempt_at: row.get(7)?,
                            last_error: row.get(8)?,
                        })
                    },
                )?)
            })
            .await
    }

    async fn wait_row(&self, agent_id: &str, what: &str, accept: impl Fn(&Row) -> bool) -> Row {
        eventually(WAIT, what, || async {
            let row = self.row(agent_id).await;
            accept(&row).then_some(row)
        })
        .await
    }

    async fn shutdown(self) {
        self.server.shutdown().await.expect("clean shutdown");
    }
}

// ---- Wire: body, headers, 202 -------------------------------------------------

#[tokio::test]
async fn wake_post_carries_three_fields_and_the_receiver_secret() {
    let h = start(Tuning::fast()).await;
    let mock = MockReceiver::start().await;
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let received = mock.wait_for(1, WAIT).await;
    let wake = &received[0];

    assert_eq!(wake.method, "POST");
    assert_eq!(wake.path_and_query, "/wake");
    assert_eq!(wake.version, Version::HTTP_11);
    let expected_auth = format!("Bearer {}", b.receiver_secret);
    assert_eq!(wake.authorization(), Some(expected_auth.as_str()));
    assert!(
        !expected_auth.contains(&b.api_key),
        "never the agent API key"
    );
    assert_eq!(wake.header("content-type"), Some("application/json"));
    let length = wake.body.len().to_string();
    assert_eq!(wake.header("content-length"), Some(length.as_str()));
    for absent in ["transfer-encoding", "content-encoding", "accept-encoding"] {
        assert_eq!(wake.header(absent), None, "{absent}");
    }
    assert!(
        wake.header("user-agent")
            .is_some_and(|agent| agent.starts_with("platformd/")),
        "{:?}",
        wake.header("user-agent")
    );
    assert!(wake.body.len() <= MAX_WAKE_BODY_BYTES);
    assert_eq!(
        wake.json(),
        json!({"v": 1, "agent_id": "agent-b", "inbox_seq": "1"}),
        "exactly three members, inbox_seq as a string"
    );

    let status = h
        .wait_status("agent-b", "the 202 is recorded", |s| {
            s["last_accepted_seq"] == "1"
        })
        .await;
    assert_eq!(
        keys(&status),
        set(&[
            "read_marker",
            "inbox_head_seq",
            "pending_seq",
            "last_accepted_seq",
            "route_version",
            "receiver_url_present",
            "wake_policy",
            "state",
            "last_attempt_at",
            "last_error"
        ])
    );
    assert_eq!(status["state"], "pending", "a 202 is not a read");
    assert_eq!(status["pending_seq"], "1");
    assert_eq!(status["read_marker"], "0");
    assert_eq!(status["inbox_head_seq"], "1");
    assert_eq!(status["receiver_url_present"], true);
    assert_eq!(status["route_version"], 1);
    assert_eq!(status["last_error"], Value::Null);
    assert!(status["last_attempt_at"].is_string(), "{status}");

    let inbox = h.api.get("/v1/inbox", &b.api_key).await;
    assert_eq!(inbox.body["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        inbox.body["read_marker"], "0",
        "202 does not move the marker"
    );

    h.put_marker(&b, "1").await;
    let idle = h.status("agent-b").await;
    assert_eq!(idle["state"], "idle");
    assert_eq!(idle["pending_seq"], Value::Null);
    assert_eq!(idle["last_accepted_seq"], "1");
    let row = h.row("agent-b").await;
    assert_eq!((row.next_due_at, row.redrive_due_at), (None, None));
    h.shutdown().await;
}

// ---- API: permissions and the retry body --------------------------------------

#[tokio::test]
async fn delivery_endpoints_check_permissions_and_ignore_the_retry_body() {
    let h = start(Tuning::fast()).await;
    let a = h.register("agent-a", None).await;
    let b = h.register("agent-b", None).await;

    for (method, path) in [
        (Method::GET, delivery_path("agent-a")),
        (Method::POST, retry_path("agent-a")),
    ] {
        for token in [OWNER_TOKEN, a.api_key.as_str()] {
            let reply = h.api.send(method.clone(), &path, Some(token), None).await;
            assert_eq!(
                reply.status.as_u16(),
                200,
                "{method} {path}: {}",
                reply.body
            );
            let text = reply.body.to_string();
            assert!(!text.contains("prs_") && !text.contains("pak_"), "{text}");
        }
        h.api
            .send(method.clone(), &path, Some(&b.api_key), None)
            .await
            .assert_error(403, "forbidden");
        h.api
            .send(method.clone(), &path, None, None)
            .await
            .assert_error(401, "unauthorized");
        h.api
            .send(method.clone(), &path, Some(&a.receiver_secret), None)
            .await
            .assert_error(401, "unauthorized");
    }
    for path in [delivery_path("nobody"), delivery_path("Not-A-Slug")] {
        h.api
            .get(&path, OWNER_TOKEN)
            .await
            .assert_error(404, "agent_not_found");
    }
    h.retry("nobody", OWNER_TOKEN)
        .await
        .assert_error(404, "agent_not_found");
    h.retry("nobody", &b.api_key)
        .await
        .assert_error(403, "forbidden");

    let garbage = h
        .api
        .send_raw(
            Method::POST,
            &retry_path("agent-a"),
            Some(&a.api_key),
            Some("text/plain"),
            Some(b"not json at all".to_vec()),
        )
        .await;
    assert_eq!(garbage.status.as_u16(), 200, "{}", garbage.body);
    assert_eq!(
        keys(&garbage.body),
        set(&["state", "route_version", "pending_seq", "last_accepted_seq"])
    );
    h.shutdown().await;
}

#[tokio::test]
async fn retry_reports_no_receiver_and_idle_without_scheduling() {
    let h = start(Tuning::fast()).await;
    let mock = MockReceiver::start().await;
    let sender = h.register("sender", None).await;
    let a = h.register("agent-a", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-a").await;
    let none = h.retry("agent-a", &a.api_key).await;
    assert_eq!(
        none.body,
        json!({"state": "no_receiver", "route_version": 1, "pending_seq": "1", "last_accepted_seq": null})
    );

    let nothing_unread = h.retry("agent-b", &b.api_key).await;
    assert_eq!(nothing_unread.body["state"], "idle");

    h.dm(&sender, "agent-b").await;
    h.wait_status("agent-b", "accepted", |s| s["last_accepted_seq"] == "1")
        .await;
    h.put_marker(&b, "1").await;
    let caught_up = h.retry("agent-b", OWNER_TOKEN).await;
    assert_eq!(
        caught_up.body,
        json!({"state": "idle", "route_version": 1, "pending_seq": null, "last_accepted_seq": "1"})
    );
    assert_eq!(h.row("agent-b").await.state, "idle");
    tokio::time::sleep(h.tuning.quiet_period()).await;
    assert_eq!(mock.count(), 1, "idle schedules nothing");
    h.shutdown().await;
}

// ---- Scenario 5: no receiver, then a new route ---------------------------------

#[tokio::test]
async fn null_receiver_keeps_the_inbox_and_resumes_on_the_new_route() {
    let h = start(Tuning::fast()).await;
    let mock = MockReceiver::start().await;
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", None).await;

    h.dm(&sender, "agent-b").await;
    h.dm(&sender, "agent-b").await;
    let status = h.status("agent-b").await;
    assert_eq!(status["state"], "no_receiver");
    assert_eq!(status["receiver_url_present"], false);
    assert_eq!(status["pending_seq"], "2");
    assert_eq!(
        h.retry("agent-b", &b.api_key).await.body["state"],
        "no_receiver"
    );
    assert_eq!(
        h.row("agent-b").await.last_attempt_at,
        None,
        "nothing attempted"
    );

    let patched = h
        .api
        .patch(
            "/v1/agents/agent-b/receiver",
            &b.api_key,
            json!({"receiver_url": mock.url()}),
        )
        .await;
    assert_eq!(patched.status.as_u16(), 200, "{}", patched.body);
    assert_eq!(patched.body["route_version"], 2);
    assert_eq!(patched.body["state"], "pending");

    let received = mock.wait_for(1, WAIT).await;
    assert_eq!(received[0].inbox_seq(), "2", "only the latest head");
    let status = h
        .wait_status("agent-b", "accepted on the new route", |s| {
            s["last_accepted_seq"] == "2"
        })
        .await;
    assert_eq!(status["route_version"], 2);
    assert_eq!(h.row("agent-b").await.route_version, 2);
    assert_eq!(mock.count(), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn clearing_the_receiver_stops_attempts_but_keeps_the_state() {
    let h = start(Tuning::fast()).await;
    let mock = MockReceiver::with_fallback(MockResponse::status(503)).await;
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    h.wait_row("agent-b", "first failure", |r| {
        r.last_error.as_deref() == Some("http_503")
    })
    .await;
    let cleared = h
        .api
        .patch(
            "/v1/agents/agent-b/receiver",
            &b.api_key,
            json!({"receiver_url": null}),
        )
        .await;
    assert_eq!(cleared.body["state"], "no_receiver");
    let row = h.row("agent-b").await;
    assert_eq!(row.state, "pending", "state is kept");
    assert_eq!(row.next_due_at, None);
    assert_eq!(row.route_version, 2);

    let attempts = mock.count();
    tokio::time::sleep(h.tuning.quiet_period()).await;
    assert_eq!(mock.count(), attempts, "no attempt without a URL");
    h.shutdown().await;
}

// ---- Scenario 6: one in flight per agent, coalescing ---------------------------

#[tokio::test]
async fn one_attempt_in_flight_per_agent_and_the_head_is_coalesced() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let hold = tuning.request_timeout / 3;
    let mock = MockReceiver::start().await;
    mock.push(MockResponse::accepted().after(hold));
    mock.push(MockResponse::accepted().after(hold));
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    mock.wait_for(1, WAIT).await;
    h.dm(&sender, "agent-b").await;
    h.dm(&sender, "agent-b").await;
    let retried = h.retry("agent-b", OWNER_TOKEN).await;
    assert_eq!(
        retried.body["state"], "pending",
        "merged into the running attempt"
    );

    let received = mock.wait_for(2, WAIT).await;
    assert_eq!(
        received.iter().map(Received::inbox_seq).collect::<Vec<_>>(),
        ["1", "3"],
        "posted 1, then the coalesced head 3"
    );
    let during = h.status("agent-b").await;
    assert_eq!(during["last_accepted_seq"], "1", "1's 202 is not 3's");
    assert_eq!(during["pending_seq"], "3");

    h.wait_status("agent-b", "3 accepted", |s| s["last_accepted_seq"] == "3")
        .await;
    tokio::time::sleep(hold + SLACK).await;
    assert_eq!(mock.count(), 2, "no further request");
    assert_eq!(mock.max_in_flight(), 1, "never two requests at once");
    h.shutdown().await;
}

// ---- Scenario 7: transient failures, backoff, Retry-After, timeouts ------------

#[tokio::test]
async fn server_errors_and_429_back_off_within_the_configured_range() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let mock = MockReceiver::start().await;
    for status in [503, 500, 429] {
        mock.push(MockResponse::status(status));
    }
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let failing = h
        .wait_status("agent-b", "first failure", |s| {
            s["last_error"] == "http_503"
        })
        .await;
    assert_eq!(failing["state"], "pending");

    let received = mock.wait_for(4, WAIT).await;
    for pair in received.windows(2) {
        assert_within(
            gap(&pair[0], &pair[1]),
            tuning.backoff_min,
            tuning.backoff_max + SLACK,
            "retry interval",
        );
        assert_eq!(pair[1].inbox_seq(), "1");
    }
    let done = h
        .wait_status("agent-b", "accepted", |s| s["last_accepted_seq"] == "1")
        .await;
    assert_eq!(done["last_error"], Value::Null);
    assert_eq!(h.row("agent-b").await.attempt, 0);
    h.shutdown().await;
}

#[tokio::test]
async fn retry_after_is_honoured_and_capped_by_backoff_max() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    // Longer than any first backoff (backoff_min with +50% jitter).
    let requested = tuning.backoff_max;
    assert!(requested > tuning.backoff_min.mul_f64(1.5));
    let mock = MockReceiver::start().await;
    mock.push(
        MockResponse::status(429).with_header("retry-after", &requested.as_secs().to_string()),
    );
    mock.push(MockResponse::status(503).with_header("retry-after", "86400"));
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let received = mock.wait_for(3, WAIT).await;
    assert_within(
        gap(&received[0], &received[1]),
        requested,
        requested + SLACK,
        "Retry-After seconds",
    );
    assert_within(
        gap(&received[1], &received[2]),
        tuning.backoff_min,
        tuning.backoff_max + SLACK,
        "Retry-After capped at backoff_max",
    );
    h.shutdown().await;
}

#[tokio::test]
async fn a_whole_request_timeout_is_a_transient_failure() {
    let tuning = Tuning {
        request_timeout: Duration::from_millis(500),
        ..Tuning::fast()
    };
    let h = start(tuning).await;
    let hold = tuning.request_timeout * 4;
    let mock = MockReceiver::start().await;
    mock.push(MockResponse::accepted().after(hold));
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let first = mock.wait_for(1, WAIT).await.remove(0);
    let row = h
        .wait_row("agent-b", "timeout recorded", |r| {
            r.last_error.as_deref() == Some("timeout")
        })
        .await;
    assert!(
        first.at.elapsed() < hold,
        "gave up before the receiver answered"
    );
    assert_eq!((row.state.as_str(), row.attempt), ("pending", 1));

    let received = mock.wait_for(2, WAIT).await;
    assert_eq!(received[1].inbox_seq(), "1");
    h.wait_status("agent-b", "accepted", |s| s["last_accepted_seq"] == "1")
        .await;
    h.shutdown().await;
}

#[tokio::test]
async fn a_refused_connection_is_a_transient_failure() {
    let h = start(Tuning::fast()).await;
    let closed_port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let sender = h.register("sender", None).await;
    h.register(
        "agent-b",
        Some(&format!("http://127.0.0.1:{closed_port}/wake")),
    )
    .await;

    h.dm(&sender, "agent-b").await;
    let row = h
        .wait_row("agent-b", "connect failure", |r| {
            r.last_error.as_deref() == Some("connect")
        })
        .await;
    assert_eq!(row.state, "pending");
    assert!(row.attempt >= 1);
    assert!(row.next_due_at.is_some());
    assert_eq!(h.status("agent-b").await["state"], "pending");
    h.shutdown().await;
}

#[tokio::test]
async fn new_messages_and_retry_keep_the_backoff_deadline() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let mock = MockReceiver::start().await;
    mock.push(
        MockResponse::status(503)
            .with_header("retry-after", &tuning.backoff_max.as_secs().to_string()),
    );
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let failed = h
        .wait_row("agent-b", "first failure", |r| {
            r.last_error.as_deref() == Some("http_503")
        })
        .await;
    let due = failed.next_due_at.clone();

    h.dm(&sender, "agent-b").await;
    h.dm(&sender, "agent-b").await;
    let coalesced = h.row("agent-b").await;
    assert_eq!(
        coalesced.next_due_at, due,
        "traffic does not reset the deadline"
    );
    assert_eq!(coalesced.target_seq, 3);

    let retried = h.retry("agent-b", &b.api_key).await;
    assert_eq!(retried.body["state"], "pending");
    assert_eq!(retried.body["pending_seq"], "3");
    let after_retry = h.row("agent-b").await;
    assert_eq!(
        after_retry.next_due_at, due,
        "retry keeps a pending due time"
    );
    assert_eq!(after_retry.attempt, 1);

    let received = mock.wait_for(2, WAIT).await;
    assert_eq!(received[1].inbox_seq(), "3");
    assert!(
        received[1].wall + EARLY >= deadline(&failed),
        "not before the deadline"
    );
    h.wait_status("agent-b", "accepted", |s| s["last_accepted_seq"] == "3")
        .await;
    assert_eq!(mock.count(), 2);
    h.shutdown().await;
}

// ---- Configuration failures and manual retry -----------------------------------

#[tokio::test]
async fn configuration_failures_block_the_route_and_keep_the_inbox() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let redirect_target = MockReceiver::start().await;
    let cases = [
        ("agent-404", MockResponse::status(404), "http_404"),
        ("agent-401", MockResponse::status(401), "http_401"),
        (
            "agent-301",
            MockResponse::status(301).with_header("location", &redirect_target.url()),
            "http_301",
        ),
        ("agent-200", MockResponse::status(200), "http_200"),
        (
            "agent-body",
            MockResponse::accepted().with_body(MockBody::Bytes(b"ok".to_vec())),
            "protocol_mismatch",
        ),
        (
            "agent-chunked",
            MockResponse::accepted().with_body(MockBody::ChunkedEmpty),
            "protocol_mismatch",
        ),
    ];
    let sender = h.register("sender", None).await;
    let mut mocks = Vec::new();
    for (agent_id, response, _) in &cases {
        let mock = MockReceiver::with_fallback(response.clone()).await;
        h.register(agent_id, Some(&mock.url())).await;
        h.dm(&sender, agent_id).await;
        mocks.push(mock);
    }
    for (agent_id, _, code) in &cases {
        let status = h
            .wait_status(agent_id, code, |s| s["state"] == "blocked")
            .await;
        assert_eq!(status["last_error"], *code, "{agent_id}");
        let row = h.row(agent_id).await;
        assert_eq!(
            (row.next_due_at, row.redrive_due_at),
            (None, None),
            "{agent_id}"
        );
        h.dm(&sender, agent_id).await; // blocked never rejects a send
    }

    tokio::time::sleep(tuning.quiet_period()).await;
    for ((agent_id, _, _), mock) in cases.iter().zip(&mocks) {
        assert_eq!(
            mock.count(),
            1,
            "{agent_id}: no automatic attempt while blocked"
        );
        let status = h.status(agent_id).await;
        assert_eq!(status["state"], "blocked", "{agent_id}");
        assert_eq!(status["pending_seq"], "2", "{agent_id}");
        assert_eq!(h.row(agent_id).await.target_seq, 2, "{agent_id}");
        let inbox = h
            .api
            .get(&format!("/v1/agents/{agent_id}/inbox"), OWNER_TOKEN)
            .await;
        assert_eq!(
            inbox.body["items"].as_array().unwrap().len(),
            2,
            "{agent_id}"
        );
    }
    assert_eq!(redirect_target.count(), 0, "redirects are not followed");
    h.shutdown().await;
}

#[tokio::test]
async fn manual_retry_releases_a_blocked_route_at_once() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let mock = MockReceiver::start().await;
    mock.push(MockResponse::status(404));
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    h.wait_status("agent-b", "blocked", |s| s["state"] == "blocked")
        .await;

    let retried_at = Instant::now();
    let retried = h.retry("agent-b", &b.api_key).await;
    assert_eq!(retried.status.as_u16(), 200, "{}", retried.body);
    assert_eq!(
        retried.body,
        json!({"state": "pending", "route_version": 1, "pending_seq": "1", "last_accepted_seq": null})
    );
    let again = h.retry("agent-b", OWNER_TOKEN).await;
    assert_eq!(again.body["state"], "pending", "a repeated retry coalesces");

    let received = mock.wait_for(2, WAIT).await;
    assert!(
        received[1].at.duration_since(retried_at) < tuning.backoff_min,
        "scheduled at once, not after a backoff"
    );
    h.wait_status("agent-b", "accepted", |s| s["last_accepted_seq"] == "1")
        .await;
    tokio::time::sleep(tuning.backoff_min).await;
    assert_eq!(mock.count(), 2, "one request for both retries");
    h.shutdown().await;
}

// ---- Scenario 8: redrive and restart -----------------------------------------

#[tokio::test]
async fn an_accepted_head_is_redriven_while_the_marker_stalls() {
    let tuning = Tuning {
        redrive_after: Duration::from_secs(1),
        ..Tuning::fast()
    };
    let h = start(tuning).await;
    let mock = MockReceiver::start().await;
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let received = mock.wait_for(2, WAIT).await;
    assert_eq!(received[1].inbox_seq(), "1", "the same head again");
    assert_within(
        gap(&received[0], &received[1]),
        tuning.redrive_after,
        tuning.redrive_after + SLACK,
        "redrive interval",
    );

    h.put_marker(&b, "1").await;
    assert_eq!(h.status("agent-b").await["state"], "idle");
    let attempts = mock.count();
    tokio::time::sleep(tuning.redrive_after + SLACK).await;
    assert_eq!(mock.count(), attempts, "no redrive once read");
    h.shutdown().await;
}

#[tokio::test]
async fn pending_backoff_survives_a_restart() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let mock = MockReceiver::with_fallback(
        MockResponse::status(503)
            .with_header("retry-after", &tuning.backoff_max.as_secs().to_string()),
    )
    .await;
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    let failed = h
        .wait_row("agent-b", "first failure", |r| {
            r.last_error.as_deref() == Some("http_503")
        })
        .await;
    let Harness {
        fx,
        config,
        server,
        tuning,
        ..
    } = h;
    server.shutdown().await.expect("first run stops");
    mock.set_fallback(MockResponse::accepted());

    // Same database file, new process state: nothing in memory survives.
    let server = RunningServer::start(&config).await;
    let api = server.api();
    let h = Harness {
        fx,
        config,
        server,
        api,
        tuning,
    };
    let resumed = h.row("agent-b").await;
    if mock.count() == 1 {
        assert_eq!(resumed, failed, "pending row and deadline as stored");
    }

    let received = mock.wait_for(2, WAIT).await;
    assert_eq!(received[1].inbox_seq(), "1");
    assert!(
        received[1].wall + EARLY >= deadline(&failed),
        "the stored backoff deadline is honoured after the restart"
    );
    h.wait_status("agent-b", "accepted after restart", |s| {
        s["last_accepted_seq"] == "1"
    })
    .await;
    h.shutdown().await;
}

// ---- Scenario 9: late responses from an older route ----------------------------

#[tokio::test]
async fn a_late_202_from_the_old_url_does_not_count() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let hold = tuning.request_timeout / 3;
    let old = MockReceiver::with_fallback(MockResponse::accepted().after(hold)).await;
    let new = MockReceiver::with_fallback(MockResponse::status(404)).await;
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&old.url())).await;

    h.dm(&sender, "agent-b").await;
    old.wait_for(1, WAIT).await;
    let patched = h
        .api
        .patch(
            "/v1/agents/agent-b/receiver",
            &b.api_key,
            json!({"receiver_url": new.url()}),
        )
        .await;
    assert_eq!(patched.body["route_version"], 2);

    new.wait_for(1, WAIT).await;
    let status = h
        .wait_status("agent-b", "new route outcome", |s| s["state"] == "blocked")
        .await;
    assert_eq!(
        status["last_accepted_seq"],
        Value::Null,
        "old route's 202 ignored"
    );
    assert_eq!(status["last_error"], "http_404");
    assert_eq!(status["route_version"], 2);
    assert_eq!(old.count(), 1);
    h.shutdown().await;
}

#[tokio::test]
async fn a_late_401_for_the_old_secret_does_not_block_the_rotated_route() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let hold = tuning.request_timeout / 3;
    let mock = MockReceiver::start().await;
    mock.push(MockResponse::status(401).after(hold));
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    mock.wait_for(1, WAIT).await;
    let rotated = h.api.rotate("agent-b", OWNER_TOKEN).await;
    assert_eq!(rotated.status.as_u16(), 200, "{}", rotated.body);
    let new_secret = rotated.body["receiver_secret"].as_str().unwrap().to_owned();

    let received = mock.wait_for(2, WAIT).await;
    let old_auth = format!("Bearer {}", b.receiver_secret);
    let new_auth = format!("Bearer {new_secret}");
    assert_eq!(received[0].authorization(), Some(old_auth.as_str()));
    assert_eq!(received[1].authorization(), Some(new_auth.as_str()));
    let status = h
        .wait_status("agent-b", "accepted on the new route", |s| {
            s["last_accepted_seq"] == "1"
        })
        .await;
    assert_eq!(status["state"], "pending");
    assert_eq!(status["last_error"], Value::Null);
    assert_eq!(status["route_version"], 2);
    h.shutdown().await;
}

// ---- S1 / S3 hooks -------------------------------------------------------------

#[tokio::test]
async fn a_read_marker_at_the_head_idles_a_blocked_route() {
    let h = start(Tuning::fast()).await;
    let mock = MockReceiver::with_fallback(MockResponse::status(404)).await;
    let sender = h.register("sender", None).await;
    let b = h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    h.dm(&sender, "agent-b").await;
    h.wait_status("agent-b", "blocked", |s| s["state"] == "blocked")
        .await;

    h.put_marker(&b, "1").await;
    assert_eq!(
        h.status("agent-b").await["state"],
        "blocked",
        "below the head"
    );

    h.put_marker(&b, "2").await;
    let status = h.status("agent-b").await;
    assert_eq!(status["state"], "idle");
    assert_eq!(status["pending_seq"], Value::Null);
    let row = h.row("agent-b").await;
    assert_eq!(row.state, "idle");
    assert_eq!(row.attempt, 0);
    assert_eq!((row.next_due_at, row.redrive_due_at), (None, None));
    h.shutdown().await;
}

#[tokio::test]
async fn rotation_and_a_new_url_release_a_blocked_route() {
    let h = start(Tuning::fast()).await;
    let rotated_mock = MockReceiver::start().await;
    rotated_mock.push(MockResponse::status(401));
    let rejecting = MockReceiver::with_fallback(MockResponse::status(404)).await;
    let replacement = MockReceiver::start().await;
    let sender = h.register("sender", None).await;
    h.register("agent-r", Some(&rotated_mock.url())).await;
    let p = h.register("agent-p", Some(&rejecting.url())).await;

    h.dm(&sender, "agent-r").await;
    h.dm(&sender, "agent-p").await;
    for agent_id in ["agent-r", "agent-p"] {
        h.wait_status(agent_id, "blocked", |s| s["state"] == "blocked")
            .await;
    }

    let rotated = h.api.rotate("agent-r", OWNER_TOKEN).await;
    let new_auth = format!(
        "Bearer {}",
        rotated.body["receiver_secret"].as_str().unwrap()
    );
    let received = rotated_mock.wait_for(2, WAIT).await;
    assert_eq!(received[1].authorization(), Some(new_auth.as_str()));
    let status = h
        .wait_status("agent-r", "accepted after rotation", |s| {
            s["last_accepted_seq"] == "1"
        })
        .await;
    assert_eq!(status["state"], "pending");
    assert_eq!(status["last_error"], Value::Null);

    let patched = h
        .api
        .patch(
            "/v1/agents/agent-p/receiver",
            &p.api_key,
            json!({"receiver_url": replacement.url()}),
        )
        .await;
    assert_eq!(patched.body["state"], "pending");
    replacement.wait_for(1, WAIT).await;
    h.wait_status("agent-p", "accepted on the new URL", |s| {
        s["last_accepted_seq"] == "1"
    })
    .await;
    assert_eq!(rejecting.count(), 1);
    h.shutdown().await;
}

// ---- IF-OPS-001: shutdown --------------------------------------------------------

#[tokio::test]
async fn shutdown_does_not_wait_for_an_attempt_in_flight() {
    let tuning = Tuning {
        request_timeout: Duration::from_secs(30),
        ..Tuning::fast()
    };
    let h = start(tuning).await;
    let hold = tuning.request_timeout;
    let mock = MockReceiver::with_fallback(MockResponse::accepted().after(hold)).await;
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    mock.wait_for(1, WAIT).await;
    let stopping = Instant::now();
    h.shutdown().await;
    assert!(
        stopping.elapsed() < hold / 2,
        "shutdown waited for the receiver: {:?}",
        stopping.elapsed()
    );
}

// ---- S12: deletion and re-registration -----------------------------------------

/// An attempt in flight while its agent is deleted and the id registered
/// again: the late response belongs to neither route (`IF-DLV-001`
/// "삭제·재등록"). The successor starts at route_version 1 like the old
/// agent, so only the changed receiver secret tells them apart.
#[tokio::test]
async fn a_late_response_for_a_deleted_agent_does_not_touch_its_successor() {
    let tuning = Tuning::fast();
    let h = start(tuning).await;
    let hold = tuning.request_timeout / 3;
    let mock = MockReceiver::with_fallback(MockResponse::status(404).after(hold)).await;
    let sender = h.register("sender", None).await;
    h.register("agent-b", Some(&mock.url())).await;

    h.dm(&sender, "agent-b").await;
    mock.wait_for(1, WAIT).await;
    let deleted = h
        .api
        .send(
            Method::DELETE,
            "/v1/agents/agent-b",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.body);
    let successor = h.register("agent-b", None).await;
    assert_eq!(successor.body["route_version"], 1);
    h.dm(&sender, "agent-b").await;
    let before = h.row("agent-b").await;
    assert_eq!(before.route_version, 1);
    assert_eq!(before.state, "pending");

    // Long enough for the held 404 to arrive and be processed.
    tokio::time::sleep(hold + SLACK).await;
    let after = h.row("agent-b").await;
    assert_eq!(after, before, "the old route's 404 is not recorded");
    let status = h.status("agent-b").await;
    assert_eq!(status["state"], "no_receiver");
    assert_eq!(status["last_error"], Value::Null);
    assert_eq!(status["last_attempt_at"], Value::Null);
    assert_eq!(mock.count(), 1);
    h.shutdown().await;
}
