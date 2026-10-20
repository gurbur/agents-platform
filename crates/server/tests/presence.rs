//! S14 presence (`IF-WIRE-013`, `IF-WIRE-001`, ADR-0012; acceptance
//! scenarios 22–24): the PUT and DELETE check order, the declared entry and
//! its TTL on an injected clock, the directory view for agents and the
//! owner, the `last_seen_at` update point and its throttle, restart, and
//! deletion with re-registration.

mod common;

use std::time::Duration;

use common::{Api, Fixture, OWNER_TOKEN, RunningServer, eventually};
use platform_wire::{MAX_PRESENCE_NOTE_CHARS, PRESENCE_TTL_CEILING_SECS};
use platformd::clock::{Clock, ClockControl, parse_rfc3339_ms};
use platformd::config::{Config, PresenceConfig};
use platformd::store::StoreError;
use reqwest::Method;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(5);

/// How long a negative check waits for a write that must not happen.
const SETTLE: Duration = Duration::from_millis(150);

struct Hub {
    fx: Fixture,
    config: Config,
    server: RunningServer,
    api: Api,
    clock: ClockControl,
}

impl Hub {
    async fn start() -> Self {
        let fx = Fixture::new();
        let config = fx.load_config();
        Self::start_on(fx, config).await
    }

    async fn start_on(fx: Fixture, config: Config) -> Self {
        let (clock, control) = Clock::adjustable();
        let server = RunningServer::start_with_clock(&config, clock).await;
        let api = server.api();
        Self {
            fx,
            config,
            server,
            api,
            clock: control,
        }
    }

    fn presence_config(&self) -> PresenceConfig {
        self.config.presence
    }

    async fn put(&self, agent_id: &str, token: &str, body: Value) -> common::Reply {
        self.api.put(&presence_path(agent_id), token, body).await
    }

    async fn delete(&self, agent_id: &str, token: Option<&str>) -> common::Reply {
        self.api
            .send(Method::DELETE, &presence_path(agent_id), token, None)
            .await
    }

    /// `agent_id`'s item in the directory as `token` sees it.
    async fn item(&self, token: &str, agent_id: &str) -> Value {
        let reply = self.api.get("/v1/agents", token).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
        reply.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["agent_id"] == agent_id)
            .unwrap_or_else(|| panic!("{agent_id} not in the directory"))
            .clone()
    }

    /// `last_seen_at` as the owner sees it (an owner read touches nothing).
    async fn last_seen(&self, agent_id: &str) -> Value {
        self.item(OWNER_TOKEN, agent_id).await["last_seen_at"].clone()
    }

    /// Waits until the owner sees a `last_seen_at` other than `before`.
    async fn last_seen_changes(&self, agent_id: &str, before: &Value) -> String {
        eventually(WAIT, "last_seen_at is written", || async move {
            let now = self.last_seen(agent_id).await;
            (now != *before).then(|| now.as_str().expect("an RFC 3339 string").to_owned())
        })
        .await
    }

    /// Gives a write that must not happen time to happen.
    async fn settle(&self) {
        tokio::time::sleep(SETTLE).await;
        self.api.get("/v1/agents", OWNER_TOKEN).await;
    }

    /// Counts every `agents.last_seen_at` write from now on.
    async fn count_touches(&self) {
        self.fx
            .read_db(|conn| {
                conn.execute_batch(
                    "CREATE TABLE touch_log (agent_id TEXT NOT NULL, at TEXT);
                     CREATE TRIGGER touch_counter AFTER UPDATE OF last_seen_at ON agents
                     BEGIN INSERT INTO touch_log VALUES (NEW.agent_id, NEW.last_seen_at); END;",
                )?;
                Ok(())
            })
            .await;
    }

    async fn touches(&self, agent_id: &'static str) -> i64 {
        self.fx
            .read_db(move |conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM touch_log WHERE agent_id = ?1",
                    [agent_id],
                    |row| row.get(0),
                )?)
            })
            .await
    }

    async fn shutdown(self) -> Fixture {
        self.server.shutdown().await.unwrap();
        self.fx
    }
}

fn presence_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/presence")
}

fn instant(text: &Value) -> std::time::SystemTime {
    parse_rfc3339_ms(text.as_str().expect("a timestamp string")).expect("RFC 3339 ms")
}

fn offline() -> Value {
    json!({"state": "offline", "note": null, "updated_at": null, "expires_at": null})
}

// ---- PUT: check order --------------------------------------------------------------

#[tokio::test]
async fn put_runs_the_checks_in_the_interface_order() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    let b = hub.api.register("agent-b", None).await;
    let api = &hub.api;
    let path = presence_path("agent-a");
    let valid = json!({"state": "online"});

    // 1. 401 before anything else, even with a broken body.
    api.send_raw(
        Method::PUT,
        &path,
        None,
        Some("text/plain"),
        Some(b"[".to_vec()),
    )
    .await
    .assert_error(401, "unauthorized");
    api.put(&path, &a.receiver_secret, valid.clone())
        .await
        .assert_error(401, "unauthorized");
    // 2. 403: another agent's path, whether or not it exists, before the body.
    for target in ["agent-a", "agent-zz", "Not-A-Slug"] {
        api.send_raw(
            Method::PUT,
            &presence_path(target),
            Some(&b.api_key),
            Some("application/json"),
            Some(b"[".to_vec()),
        )
        .await
        .assert_error(403, "forbidden");
    }
    // 3. 404 for the owner on a path that is not a slug, before the body.
    api.send_raw(
        Method::PUT,
        &presence_path("Not-A-Slug"),
        Some(OWNER_TOKEN),
        Some("text/plain"),
        Some(b"x".to_vec()),
    )
    .await
    .assert_error(404, "agent_not_found");
    // 4. StrictJson.
    api.send_raw(
        Method::PUT,
        &path,
        Some(&a.api_key),
        Some("text/plain"),
        Some(b"{}".to_vec()),
    )
    .await
    .assert_error(415, "unsupported_media_type");
    api.send_raw(
        Method::PUT,
        &path,
        Some(&a.api_key),
        Some("application/json"),
        Some(b"{".to_vec()),
    )
    .await
    .assert_error(400, "invalid_json");
    api.send_raw(
        Method::PUT,
        &path,
        Some(&a.api_key),
        Some("application/json"),
        Some(b"[]".to_vec()),
    )
    .await
    .assert_error(422, "invalid_field");
    let huge = format!(
        "{{\"state\":\"online\",\"note\":\"{}\"}}",
        "x".repeat(130 * 1024)
    );
    api.send_raw(
        Method::PUT,
        &path,
        Some(&a.api_key),
        Some("application/json"),
        Some(huge.into_bytes()),
    )
    .await
    .assert_error(413, "request_too_large");
    // 5. Unknown keys, before missing or mistyped ones.
    let unexpected = api
        .put(
            &path,
            &a.api_key,
            json!({"state": 1, "agent_id": "agent-a"}),
        )
        .await;
    unexpected.assert_error(422, "unexpected_field");
    assert_eq!(unexpected.error_field(), Some("agent_id"));
    // 6. Missing and mistyped fields.
    for (body, field) in [
        (json!({}), "state"),
        (json!({"note": "x"}), "state"),
        (json!({"state": null}), "state"),
        (json!({"state": 1}), "state"),
        (json!({"state": "online", "note": 7}), "note"),
        (json!({"state": "online", "ttl_secs": 1.5}), "ttl_secs"),
        (json!({"state": "online", "ttl_secs": "60"}), "ttl_secs"),
    ] {
        let reply = api.put(&path, &a.api_key, body.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{body}");
    }
    // 7. Values: state, then note, then ttl_secs.
    let too_long = "가".repeat(MAX_PRESENCE_NOTE_CHARS + 1);
    for (body, field) in [
        (json!({"state": "offline"}), "state"),
        (json!({"state": "Online"}), "state"),
        (json!({"state": ""}), "state"),
        (
            json!({"state": "offline", "note": "", "ttl_secs": 0}),
            "state",
        ),
        (json!({"state": "busy", "note": ""}), "note"),
        (json!({"state": "busy", "note": "  "}), "note"),
        (json!({"state": "busy", "note": too_long}), "note"),
        (json!({"state": "busy", "note": "a\nb"}), "note"),
        (json!({"state": "busy", "note": "a\u{1b}b"}), "note"),
        (json!({"state": "busy", "note": "", "ttl_secs": 0}), "note"),
        (json!({"state": "online", "ttl_secs": 0}), "ttl_secs"),
        (json!({"state": "online", "ttl_secs": -5}), "ttl_secs"),
    ] {
        let reply = api.put(&path, &a.api_key, body.clone()).await;
        reply.assert_error(422, "invalid_field");
        assert_eq!(reply.error_field(), Some(field), "{body}");
    }
    // 8. Above the configured maximum, and above the contract ceiling.
    let max = hub.presence_config().max_ttl.as_secs();
    for ttl in [max + 1, PRESENCE_TTL_CEILING_SECS + 1] {
        api.put(
            &path,
            &a.api_key,
            json!({"state": "online", "ttl_secs": ttl}),
        )
        .await
        .assert_error(422, "ttl_too_long");
    }
    let at_max = api
        .put(
            &path,
            &a.api_key,
            json!({"state": "online", "ttl_secs": max}),
        )
        .await;
    assert_eq!(at_max.status.as_u16(), 200, "{}", at_max.body);
    // 9. Not registered: after every body check.
    api.put(
        &presence_path("agent-zz"),
        OWNER_TOKEN,
        json!({"state": "offline"}),
    )
    .await
    .assert_error(422, "invalid_field");
    api.put(&presence_path("agent-zz"), OWNER_TOKEN, valid.clone())
        .await
        .assert_error(404, "agent_not_found");
    // 10. 200 for the agent itself and for the owner.
    for token in [&a.api_key, OWNER_TOKEN] {
        let reply = api.put(&path, token, valid.clone()).await;
        assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    }
    hub.shutdown().await;
}

// ---- PUT: entry, TTL and directory -------------------------------------------------

#[tokio::test]
async fn a_declaration_is_returned_and_shown_to_every_principal() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    let b = hub.api.register("agent-b", None).await;

    let reply = hub
        .put(
            "agent-a",
            &a.api_key,
            json!({"state": "busy", "note": "  배포 점검 중 ", "ttl_secs": 300}),
        )
        .await;

    assert_eq!(reply.status.as_u16(), 200, "{}", reply.body);
    let keys = hub
        .api
        .keys_of(
            Method::PUT,
            &presence_path("agent-a"),
            &a.api_key,
            Some(json!({"state": "busy", "note": "배포 점검 중", "ttl_secs": 300})),
        )
        .await;
    assert_eq!(keys, ["agent_id", "presence", "last_seen_at"]);
    assert_eq!(reply.body["agent_id"], "agent-a");
    let presence = &reply.body["presence"];
    assert_eq!(presence["state"], "busy");
    assert_eq!(presence["note"], "배포 점검 중", "trimmed");
    let lifetime = instant(&presence["expires_at"])
        .duration_since(instant(&presence["updated_at"]))
        .unwrap();
    assert_eq!(lifetime, Duration::from_secs(300));
    assert!(reply.body.get("last_seen_at").is_some());

    // `keys_of` declared again; the directory shows that latest entry.
    let latest = hub
        .put(
            "agent-a",
            &a.api_key,
            json!({"state": "busy", "note": "배포 점검 중", "ttl_secs": 300}),
        )
        .await;
    let to_agent = hub.item(&b.api_key, "agent-a").await["presence"].clone();
    let to_owner = hub.item(OWNER_TOKEN, "agent-a").await["presence"].clone();
    assert_eq!(to_agent, latest.body["presence"]);
    assert_eq!(to_owner, to_agent, "the agent and owner views agree");
    assert_eq!(
        hub.item(OWNER_TOKEN, "agent-b").await["presence"],
        offline()
    );
    hub.shutdown().await;
}

#[tokio::test]
async fn the_default_ttl_applies_and_an_expired_entry_reads_offline() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    let default_ttl = hub.presence_config().default_ttl;

    let first = hub
        .put("agent-a", &a.api_key, json!({"state": "online"}))
        .await;
    assert_eq!(first.status.as_u16(), 200, "{}", first.body);
    let presence = &first.body["presence"];
    assert_eq!(presence["note"], Value::Null);
    let lifetime = instant(&presence["expires_at"])
        .duration_since(instant(&presence["updated_at"]))
        .unwrap();
    assert_eq!(
        lifetime, default_ttl,
        "an omitted ttl_secs is the configured default"
    );

    hub.clock.advance(default_ttl - Duration::from_secs(1));
    assert_eq!(
        hub.item(OWNER_TOKEN, "agent-a").await["presence"]["state"],
        "online"
    );
    hub.clock.advance(Duration::from_secs(1));
    assert_eq!(
        hub.item(OWNER_TOKEN, "agent-a").await["presence"],
        offline()
    );
    assert_eq!(hub.item(&a.api_key, "agent-a").await["presence"], offline());

    let again = hub
        .put("agent-a", &a.api_key, json!({"state": "online"}))
        .await;
    let renewed = &again.body["presence"];
    assert!(instant(&renewed["updated_at"]) > instant(&presence["updated_at"]));
    assert!(instant(&renewed["expires_at"]) > instant(&presence["expires_at"]));
    assert_eq!(hub.item(OWNER_TOKEN, "agent-a").await["presence"], *renewed);
    hub.shutdown().await;
}

#[tokio::test]
async fn a_put_replaces_the_whole_entry() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    hub.put(
        "agent-a",
        &a.api_key,
        json!({"state": "busy", "note": "점검", "ttl_secs": 600}),
    )
    .await;

    let replaced = hub
        .put(
            "agent-a",
            OWNER_TOKEN,
            json!({"state": "online", "ttl_secs": 30}),
        )
        .await;

    assert_eq!(replaced.status.as_u16(), 200, "{}", replaced.body);
    let shown = hub.item(OWNER_TOKEN, "agent-a").await["presence"].clone();
    assert_eq!(shown["state"], "online");
    assert_eq!(
        shown["note"],
        Value::Null,
        "an omitted note removes the old one"
    );
    let lifetime = instant(&shown["expires_at"])
        .duration_since(instant(&shown["updated_at"]))
        .unwrap();
    assert_eq!(lifetime, Duration::from_secs(30));
    hub.shutdown().await;
}

// ---- DELETE ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_withdraws_at_once_with_or_without_an_entry() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    let b = hub.api.register("agent-b", None).await;

    hub.delete("agent-a", None)
        .await
        .assert_error(401, "unauthorized");
    hub.delete("agent-a", Some(&b.api_key))
        .await
        .assert_error(403, "forbidden");
    hub.delete("agent-zz", Some(&b.api_key))
        .await
        .assert_error(403, "forbidden");
    hub.delete("Not-A-Slug", Some(OWNER_TOKEN))
        .await
        .assert_error(404, "agent_not_found");
    hub.delete("agent-zz", Some(OWNER_TOKEN))
        .await
        .assert_error(404, "agent_not_found");

    let nothing = hub.delete("agent-a", Some(&a.api_key)).await;
    assert_eq!(nothing.status.as_u16(), 204, "no entry is still 204");
    assert_eq!(nothing.body, Value::Null);
    for token in [a.api_key.as_str(), OWNER_TOKEN] {
        hub.put("agent-a", &a.api_key, json!({"state": "busy", "note": "x"}))
            .await;
        // The body is not read: a JSON body or garbage is ignored alike.
        let cleared = hub
            .api
            .send_raw(
                Method::DELETE,
                &presence_path("agent-a"),
                Some(token),
                Some("text/plain"),
                Some(b"{".to_vec()),
            )
            .await;
        assert_eq!(cleared.status.as_u16(), 204);
        assert_eq!(hub.item(&b.api_key, "agent-a").await["presence"], offline());
    }
    hub.shutdown().await;
}

// ---- directory shape -----------------------------------------------------------------

#[tokio::test]
async fn directory_items_end_with_presence_and_last_seen_at_in_both_views() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    hub.api.register("agent-b", None).await;
    hub.put("agent-a", &a.api_key, json!({"state": "busy", "note": "n"}))
        .await;

    for token in [a.api_key.as_str(), OWNER_TOKEN] {
        let raw = hub
            .api
            .send_bytes(Method::GET, "/v1/agents", Some(token), &[], None)
            .await
            .text();
        let items = item_texts(&raw);
        assert_eq!(items.len(), 2, "{raw}");
        for item in items {
            let keys = common::ordered_keys(item);
            assert_eq!(
                keys[keys.len() - 3..],
                ["avatar", "presence", "last_seen_at"]
            );
            let value: Value = serde_json::from_str(item).unwrap();
            let presence = value["presence"].as_object().unwrap();
            assert_eq!(presence.len(), 4, "{item}");
            assert!(value.get("last_seen_at").is_some(), "never omitted");
        }
    }
    hub.shutdown().await;
}

/// Each element of the top-level `items` array as its wire text, so the
/// key order is the server's (`Value` sorts keys).
fn item_texts(raw: &str) -> Vec<&str> {
    const ITEMS: &str = "\"items\":[";
    let start = raw.find(ITEMS).expect("an items array") + ITEMS.len();
    let (mut depth, mut in_string, mut escaped, mut item_start) = (0usize, false, false, 0);
    let mut items = Vec::new();
    for (index, byte) in raw.bytes().enumerate().skip(start) {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    item_start = index;
                }
                depth += 1;
            }
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    items.push(&raw[item_start..=index]);
                }
            }
            b']' if depth == 0 => break,
            _ => {}
        }
    }
    items
}

// ---- last_seen_at ---------------------------------------------------------------------

#[tokio::test]
async fn an_agent_request_sets_last_seen_at_once_per_interval() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    hub.count_touches().await;
    let interval = hub.presence_config().last_seen_interval;
    assert_eq!(hub.last_seen("agent-a").await, Value::Null, "never seen");

    hub.api.get("/v1/inbox", &a.api_key).await;
    let first = hub.last_seen_changes("agent-a", &Value::Null).await;
    assert_eq!(hub.touches("agent-a").await, 1);

    // Within the interval: no write, the value stays.
    hub.clock.advance(interval / 2);
    hub.api.get("/v1/inbox", &a.api_key).await;
    hub.api.get("/v1/agents", &a.api_key).await;
    hub.settle().await;
    assert_eq!(hub.last_seen("agent-a").await, first.as_str());
    assert_eq!(
        hub.touches("agent-a").await,
        1,
        "one write inside the interval"
    );

    // Once the interval has passed since the last write: written again.
    hub.clock.advance(interval - interval / 2);
    hub.api.get("/v1/inbox", &a.api_key).await;
    let second = hub.last_seen_changes("agent-a", &json!(first)).await;
    assert!(
        parse_rfc3339_ms(&second).unwrap() >= parse_rfc3339_ms(&first).unwrap() + interval,
        "{first} -> {second}"
    );
    assert_eq!(hub.touches("agent-a").await, 2);
    hub.shutdown().await;
}

#[tokio::test]
async fn owner_and_unauthenticated_requests_never_touch_last_seen_at() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    hub.count_touches().await;

    hub.api.get("/v1/agents/agent-a/inbox", OWNER_TOKEN).await;
    let put = hub
        .put(
            "agent-a",
            OWNER_TOKEN,
            json!({"state": "busy", "note": "owner"}),
        )
        .await;
    assert_eq!(put.status.as_u16(), 200, "{}", put.body);
    assert_eq!(put.body["last_seen_at"], Value::Null);
    hub.delete("agent-a", Some(OWNER_TOKEN)).await;
    hub.api
        .get("/v1/agents/agent-a/delivery", OWNER_TOKEN)
        .await;
    hub.api.get("/v1/inbox", &a.receiver_secret).await;
    hub.api.get("/v1/inbox", "pak_not-a-key").await;
    hub.settle().await;

    assert_eq!(hub.last_seen("agent-a").await, Value::Null);
    assert_eq!(hub.touches("agent-a").await, 0);
    hub.shutdown().await;
}

#[tokio::test]
async fn every_request_an_agent_key_authenticates_counts() {
    let fx = Fixture::new();
    let config = fx.load_config_with_identity("issuer = \"hub\"\naudiences = [\"garage-docs\"]\n");
    let hub = Hub::start_on(fx, config).await;
    let inbox = hub.api.register("agent-p", None).await;
    let sender = hub.api.register("agent-q", None).await;
    let refused = hub.api.register("agent-r", None).await;
    let attested = hub.api.register("agent-s", None).await;
    let declared = hub.api.register("agent-t", None).await;

    hub.api.get("/v1/inbox", &inbox.api_key).await;
    hub.api.sent(&sender.api_key, "all", "hello", "k-1").await;
    hub.api
        .get("/v1/agents/agent-p/inbox", &refused.api_key)
        .await
        .assert_error(403, "forbidden");
    let token = hub
        .api
        .post(
            "/v1/identity/tokens",
            &attested.api_key,
            json!({"aud": "garage-docs"}),
        )
        .await;
    assert_eq!(token.status.as_u16(), 201, "{}", token.body);
    let put = hub
        .put("agent-t", &declared.api_key, json!({"state": "online"}))
        .await;
    assert_eq!(put.status.as_u16(), 200, "{}", put.body);

    for agent_id in ["agent-p", "agent-q", "agent-r", "agent-s", "agent-t"] {
        hub.last_seen_changes(agent_id, &Value::Null).await;
    }
    hub.shutdown().await;
}

#[tokio::test]
async fn a_failed_write_leaves_the_request_alone() {
    let hub = Hub::start().await;
    let a = hub.api.register("agent-a", None).await;
    let interval = hub.presence_config().last_seen_interval;
    hub.fx
        .read_db(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER refuse_touch BEFORE UPDATE OF last_seen_at ON agents
                 BEGIN SELECT RAISE(ABORT, 'injected store failure'); END;",
            )?;
            Ok::<_, StoreError>(())
        })
        .await;

    let inbox = hub.api.get("/v1/inbox", &a.api_key).await;
    assert_eq!(inbox.status.as_u16(), 200, "{}", inbox.body);
    let put = hub
        .put("agent-a", &a.api_key, json!({"state": "online"}))
        .await;
    assert_eq!(put.status.as_u16(), 200, "{}", put.body);
    hub.settle().await;
    assert_eq!(hub.last_seen("agent-a").await, Value::Null);

    hub.fx
        .read_db(|conn| {
            conn.execute_batch("DROP TRIGGER refuse_touch")?;
            Ok::<_, StoreError>(())
        })
        .await;
    hub.clock.advance(interval);
    hub.api.get("/v1/inbox", &a.api_key).await;
    hub.last_seen_changes("agent-a", &Value::Null).await;
    hub.shutdown().await;
}

// ---- restart, deletion and re-registration --------------------------------------------

/// A fixed interval long enough that "within the interval" holds for the
/// whole test; the value itself is not under test.
const LONG_INTERVAL: &str = "last_seen_interval_secs = 3600";

#[tokio::test]
async fn a_restart_forgets_presence_and_the_throttle_but_keeps_last_seen_at() {
    let fx = Fixture::new();
    let config = fx.load_config_with_presence(LONG_INTERVAL);
    let hub = Hub::start_on(fx, config.clone()).await;
    let a = hub.api.register("agent-a", None).await;
    hub.put(
        "agent-a",
        &a.api_key,
        json!({"state": "busy", "note": "x", "ttl_secs": 3600}),
    )
    .await;
    let seen = hub.last_seen_changes("agent-a", &Value::Null).await;
    let fx = hub.shutdown().await;

    let hub = Hub::start_on(fx, config).await;
    let item = hub.item(OWNER_TOKEN, "agent-a").await;
    assert_eq!(item["presence"], offline());
    assert_eq!(item["last_seen_at"], seen.as_str(), "persisted");

    // Well inside the old interval, the first request after the restart writes.
    hub.clock.advance(Duration::from_secs(1));
    hub.api.get("/v1/inbox", &a.api_key).await;
    let after = hub.last_seen_changes("agent-a", &json!(seen)).await;
    assert!(parse_rfc3339_ms(&after) > parse_rfc3339_ms(&seen));
    hub.shutdown().await;
}

#[tokio::test]
async fn a_re_registered_agent_starts_offline_and_never_seen() {
    let fx = Fixture::new();
    let config = fx.load_config_with_presence(LONG_INTERVAL);
    let hub = Hub::start_on(fx, config).await;
    let old = hub.api.register("agent-a", None).await;
    hub.put(
        "agent-a",
        &old.api_key,
        json!({"state": "busy", "note": "x", "ttl_secs": 3600}),
    )
    .await;
    hub.last_seen_changes("agent-a", &Value::Null).await;

    let deleted = hub
        .api
        .send(
            Method::DELETE,
            "/v1/agents/agent-a",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204);
    let new = hub.api.register("agent-a", None).await;

    let item = hub.item(OWNER_TOKEN, "agent-a").await;
    assert_eq!(item["presence"], offline());
    assert_eq!(item["last_seen_at"], Value::Null);
    // The old throttle record is gone: the first request writes at once.
    hub.api.get("/v1/inbox", &new.api_key).await;
    hub.last_seen_changes("agent-a", &Value::Null).await;
    hub.shutdown().await;
}
