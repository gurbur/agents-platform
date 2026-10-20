//! S1: registration, directory, receiver route, `all` membership, rotation,
//! authentication and the `IF-WIRE-002` error mapping, over HTTP.

mod common;

use std::collections::BTreeSet;

use common::{Api, Fixture, OWNER_TOKEN, RunningServer, default_name};
use platformd::api::{MAX_RECEIVER_URL_LEN, MAX_WIRE_BODY_BYTES};
use platformd::auth::credential::hash_api_key;
use platformd::store::Store;
use reqwest::Method;
use serde_json::{Value, json};

const STORE_JOIN_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// The five keys every `profile` object carries (`IF-WIRE-001`).
const PROFILE_KEYS: [&str; 5] = ["gender_form", "age_band", "role", "intro", "updated_at"];

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

fn route_version(body: &Value) -> i64 {
    body["route_version"].as_i64().expect("route_version")
}

fn epoch(body: &Value) -> i64 {
    body["membership_epoch"].as_i64().expect("membership_epoch")
}

fn receiver_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/receiver")
}

fn membership_path(agent_id: &str) -> String {
    format!("/v1/agents/{agent_id}/membership/all")
}

async fn start() -> (Fixture, RunningServer, Api) {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    (fx, server, api)
}

// ---- Scenario 1: registration and directory -------------------------------

#[tokio::test]
async fn registration_returns_one_time_credentials_and_joins_all() {
    let (_fx, server, api) = start().await;

    let agent = api
        .register("agent-a", Some("http://100.64.0.10:8080/notify"))
        .await;

    assert_eq!(
        keys(&agent.body),
        set(&[
            "agent_id",
            "name",
            "joined_all",
            "receiver_url",
            "route_version",
            "profile",
            "agent_api_key",
            "receiver_secret"
        ])
    );
    assert_eq!(agent.body["agent_id"], "agent-a");
    assert_eq!(agent.body["joined_all"], true);
    assert_eq!(agent.body["receiver_url"], "http://100.64.0.10:8080/notify");
    assert_eq!(route_version(&agent.body), 1);
    assert!(agent.api_key.starts_with("pak_"), "agent_api_key prefix");
    assert!(
        agent.receiver_secret.starts_with("prs_"),
        "receiver_secret prefix"
    );
    assert_ne!(agent.api_key, agent.receiver_secret);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn directory_never_contains_credentials() {
    let (_fx, server, api) = start().await;
    let a = api.register("agent-a", None).await;
    let b = api
        .register("agent-b", Some("http://receiver.test/notify"))
        .await;

    let owner_view = api.get("/v1/agents", OWNER_TOKEN).await;
    let agent_view = api.get("/v1/agents", &a.api_key).await;

    for reply in [&owner_view, &agent_view] {
        assert_eq!(reply.status, 200, "{}", reply.body);
        let text = reply.body.to_string();
        for secret in [
            &a.api_key,
            &a.receiver_secret,
            &b.api_key,
            &b.receiver_secret,
        ] {
            assert!(
                !text.contains(secret.as_str()),
                "directory leaks a credential"
            );
        }
        assert!(!text.contains("pak_") && !text.contains("prs_"));
    }

    let agent_items = agent_view.body["items"].as_array().unwrap();
    assert_eq!(agent_items.len(), 2);
    for item in agent_items {
        assert_eq!(
            keys(item),
            set(&["agent_id", "name", "joined_all", "profile", "avatar"])
        );
        assert_eq!(item["joined_all"], true);
        assert_eq!(keys(&item["profile"]), set(&PROFILE_KEYS));
    }

    let owner_items = owner_view.body["items"].as_array().unwrap();
    for item in owner_items {
        assert_eq!(
            keys(item),
            set(&[
                "agent_id",
                "name",
                "joined_all",
                "profile",
                "receiver_url",
                "wake_policy",
                "route_version",
                "created_at",
                "delivery",
                "avatar"
            ])
        );
        assert_eq!(keys(&item["delivery"]), set(&["state"]));
        assert_eq!(keys(&item["profile"]), set(&PROFILE_KEYS));
    }
    for (reply, agent_id) in [(&owner_view, "agent-a"), (&agent_view, "agent-b")] {
        let item = reply.body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["agent_id"] == agent_id)
            .unwrap();
        assert_eq!(item["name"], default_name(agent_id).as_str());
    }
    assert_eq!(owner_items[0]["agent_id"], "agent-a");
    assert_eq!(owner_items[0]["receiver_url"], Value::Null);
    assert_eq!(owner_items[0]["delivery"]["state"], "no_receiver");
    assert_eq!(owner_items[1]["agent_id"], "agent-b");
    assert_eq!(
        owner_items[1]["receiver_url"],
        "http://receiver.test/notify"
    );
    assert_eq!(owner_items[1]["delivery"]["state"], "idle");
    let created_at = owner_items[0]["created_at"].as_str().unwrap();
    assert_eq!(created_at.len(), "2026-10-15T12:34:56.789Z".len());
    assert!(created_at.ends_with('Z'));

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn directory_is_sorted_by_agent_id() {
    let (_fx, server, api) = start().await;
    for id in ["charlie", "alpha", "bravo-2", "bravo"] {
        api.register(id, None).await;
    }

    let reply = api.get("/v1/agents", OWNER_TOKEN).await;

    let ids: Vec<&str> = reply.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["agent_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["alpha", "bravo", "bravo-2", "charlie"]);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn registration_writes_all_rows_in_one_transaction() {
    let fx = Fixture::new();
    let config = fx.load_config();
    let server = RunningServer::start(&config).await;
    let agent = server.api().register("agent-a", None).await;
    server.shutdown().await.unwrap();

    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let rows = store
        .run(|conn| {
            let agent: (String, String, i64) = conn.query_row(
                "SELECT api_key_hash, receiver_secret, route_version FROM agents WHERE agent_id = 'agent-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let membership: (i64, i64, i64) = conn.query_row(
                "SELECT joined, epoch, effective_event_seq FROM memberships
                 WHERE room_id = 'all' AND agent_id = 'agent-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            let marker: i64 = conn.query_row(
                "SELECT read_marker FROM read_state WHERE agent_id = 'agent-a'",
                [],
                |row| row.get(0),
            )?;
            let pending: (String, i64, i64) = conn.query_row(
                "SELECT state, route_version, target_seq FROM notification_pending
                 WHERE agent_id = 'agent-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            Ok((agent, membership, marker, pending))
        })
        .await
        .unwrap();
    drop(store);
    thread.shutdown(STORE_JOIN_LIMIT).await.unwrap();

    let ((hash, stored_secret, version), membership, marker, pending) = rows;
    assert_eq!(
        hash,
        hash_api_key(&agent.api_key),
        "only the SHA-256 hex is stored"
    );
    assert_eq!(
        stored_secret, agent.receiver_secret,
        "receiver secret is stored"
    );
    assert_eq!(version, 1);
    assert_eq!(
        membership,
        (1, 1, 0),
        "joined, epoch 1, effective at event_seq 0"
    );
    assert_eq!(marker, 0);
    assert_eq!(pending, ("idle".to_owned(), 1, 0));
}

#[tokio::test]
async fn duplicate_agent_id_is_409() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;

    let reply = api
        .post(
            "/v1/agents",
            OWNER_TOKEN,
            json!({"agent_id": "agent-a", "name": "A"}),
        )
        .await;

    reply.assert_error(409, "agent_id_taken");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_and_reserved_agent_ids_are_422() {
    let (_fx, server, api) = start().await;
    let too_long = "a".repeat(65);

    for agent_id in [
        "owner", "all", "Agent-A", "1agent", "-a", "agent.a", "", &too_long,
    ] {
        let reply = api
            .post(
                "/v1/agents",
                OWNER_TOKEN,
                json!({"agent_id": agent_id, "name": "A"}),
            )
            .await;
        reply.assert_error(422, "invalid_agent_id");
        assert_eq!(reply.error_field(), Some("agent_id"));
    }

    let mistyped = api
        .post("/v1/agents", OWNER_TOKEN, json!({"agent_id": 42}))
        .await;
    mistyped.assert_error(422, "invalid_field");
    assert_eq!(mistyped.error_field(), Some("agent_id"));

    let missing = api.post("/v1/agents", OWNER_TOKEN, json!({})).await;
    missing.assert_error(422, "invalid_field");
    assert_eq!(missing.error_field(), Some("agent_id"));

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_receiver_urls_are_422_on_register_and_patch() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;
    let too_long = format!("http://receiver.test/{}", "a".repeat(MAX_RECEIVER_URL_LEN));

    for url in [
        "ftp://receiver.test/notify",
        "http://user:pass@receiver.test/notify",
        "http://receiver.test/notify#fragment",
        "/relative/path",
        too_long.as_str(),
    ] {
        let register = api
            .post(
                "/v1/agents",
                OWNER_TOKEN,
                json!({"agent_id": "agent-b", "name": "B", "receiver_url": url}),
            )
            .await;
        register.assert_error(422, "invalid_receiver_url");

        let patch = api
            .patch(
                &receiver_path("agent-a"),
                &agent.api_key,
                json!({"receiver_url": url}),
            )
            .await;
        patch.assert_error(422, "invalid_receiver_url");
    }

    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    assert_eq!(directory.body["items"].as_array().unwrap().len(), 1);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn fields_outside_the_allowed_set_are_unexpected_field() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;

    let register = api
        .post(
            "/v1/agents",
            OWNER_TOKEN,
            json!({"agent_id": "agent-b", "api_key": "pak_chosen"}),
        )
        .await;
    register.assert_error(422, "unexpected_field");
    assert_eq!(register.error_field(), Some("api_key"));

    let patch = api
        .patch(
            &receiver_path("agent-a"),
            &agent.api_key,
            json!({"receiver_url": null, "receiver_secret": "prs_x"}),
        )
        .await;
    patch.assert_error(422, "unexpected_field");
    assert_eq!(patch.error_field(), Some("receiver_secret"));

    let put = api
        .put(
            &membership_path("agent-a"),
            &agent.api_key,
            json!({"joined": true, "room_id": "all"}),
        )
        .await;
    put.assert_error(422, "unexpected_field");
    assert_eq!(put.error_field(), Some("room_id"));

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_or_mistyped_fields_are_invalid_field() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;

    let patch = api
        .patch(&receiver_path("agent-a"), &agent.api_key, json!({}))
        .await;
    // S13: `{}` names neither receiver_url nor wake_policy (IF-WIRE-004).
    patch.assert_error(422, "invalid_field");
    assert_eq!(patch.error_field(), None);

    let put = api
        .put(
            &membership_path("agent-a"),
            &agent.api_key,
            json!({"joined": "no"}),
        )
        .await;
    put.assert_error(422, "invalid_field");
    assert_eq!(put.error_field(), Some("joined"));

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_json_is_400_and_other_media_types_are_415() {
    let (_fx, server, api) = start().await;

    for body in [
        "{",
        "{\"agent_id\": }",
        "not json",
        "{\"agent_id\":\"a\",\"name\":\"A\"} trailing",
    ] {
        let reply = api
            .send_raw(
                Method::POST,
                "/v1/agents",
                Some(OWNER_TOKEN),
                Some("application/json"),
                Some(body.as_bytes().to_vec()),
            )
            .await;
        reply.assert_error(400, "invalid_json");
    }

    for content_type in [Some("text/plain"), None] {
        let reply = api
            .send_raw(
                Method::POST,
                "/v1/agents",
                Some(OWNER_TOKEN),
                content_type,
                Some(br#"{"agent_id":"agent-a"}"#.to_vec()),
            )
            .await;
        reply.assert_error(415, "unsupported_media_type");
    }

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn bodies_over_the_wire_limit_are_413() {
    let (_fx, server, api) = start().await;
    let padding = "a".repeat(MAX_WIRE_BODY_BYTES);
    let body = format!(r#"{{"agent_id":"agent-a","receiver_url":"{padding}"}}"#);

    let reply = api
        .send_raw(
            Method::POST,
            "/v1/agents",
            Some(OWNER_TOKEN),
            Some("application/json"),
            Some(body.into_bytes()),
        )
        .await;

    reply.assert_error(413, "request_too_large");
    server.shutdown().await.unwrap();
}

// ---- Authentication ---------------------------------------------------------

#[tokio::test]
async fn missing_wrong_and_receiver_secret_tokens_are_401() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;

    let no_token = api.send(Method::GET, "/v1/agents", None, None).await;
    no_token.assert_error(401, "unauthorized");
    assert_eq!(no_token.headers["www-authenticate"], "Bearer");

    let wrong_tokens = [
        "pak_not-a-real-key".to_owned(),
        format!("{OWNER_TOKEN}x"),
        OWNER_TOKEN[..OWNER_TOKEN.len() - 1].to_owned(),
        agent.receiver_secret.clone(),
    ];
    for token in &wrong_tokens {
        let reply = api.get("/v1/agents", token).await;
        reply.assert_error(401, "unauthorized");
        assert!(!reply.body.to_string().contains(token.as_str()));
    }

    let secret_write = api
        .patch(
            &receiver_path("agent-a"),
            &agent.receiver_secret,
            json!({"receiver_url": null}),
        )
        .await;
    secret_write.assert_error(401, "unauthorized");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn unauthenticated_requests_are_rejected_before_the_body_is_read() {
    let (_fx, server, api) = start().await;

    let reply = api
        .send_raw(
            Method::POST,
            "/v1/agents",
            None,
            Some("text/plain"),
            Some(b"{".to_vec()),
        )
        .await;

    reply.assert_error(401, "unauthorized");
    server.shutdown().await.unwrap();
}

// ---- Scenario 10 (part): an agent cannot change another agent ----------------

#[tokio::test]
async fn agent_cannot_manage_another_agent_or_act_as_owner() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", Some("http://receiver.test/a"))
        .await;
    let b = api.register("agent-b", None).await;

    let receiver = api
        .patch(
            &receiver_path("agent-a"),
            &b.api_key,
            json!({"receiver_url": null}),
        )
        .await;
    receiver.assert_error(403, "forbidden");

    let membership = api
        .put(
            &membership_path("agent-a"),
            &b.api_key,
            json!({"joined": false}),
        )
        .await;
    membership.assert_error(403, "forbidden");

    let unknown_target = api
        .put(
            &membership_path("agent-zzz"),
            &b.api_key,
            json!({"joined": false}),
        )
        .await;
    unknown_target.assert_error(403, "forbidden");

    let register = api
        .post("/v1/agents", &b.api_key, json!({"agent_id": "agent-c"}))
        .await;
    register.assert_error(403, "forbidden");

    for target in ["agent-a", "agent-b"] {
        api.rotate(target, &b.api_key)
            .await
            .assert_error(403, "forbidden");
    }

    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    let a = &directory.body["items"][0];
    assert_eq!(a["agent_id"], "agent-a");
    assert_eq!(a["receiver_url"], "http://receiver.test/a");
    assert_eq!(a["joined_all"], true);
    assert_eq!(route_version(a), 1);
    assert_eq!(directory.body["items"].as_array().unwrap().len(), 2);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn owner_gets_404_for_unknown_or_malformed_agent_paths() {
    let (_fx, server, api) = start().await;

    for agent_id in ["agent-zzz", "Not-A-Slug", "owner"] {
        api.patch(
            &receiver_path(agent_id),
            OWNER_TOKEN,
            json!({"receiver_url": null}),
        )
        .await
        .assert_error(404, "agent_not_found");
        api.put(
            &membership_path(agent_id),
            OWNER_TOKEN,
            json!({"joined": true}),
        )
        .await
        .assert_error(404, "agent_not_found");
        api.rotate(agent_id, OWNER_TOKEN)
            .await
            .assert_error(404, "agent_not_found");
    }

    server.shutdown().await.unwrap();
}

// ---- Receiver route ---------------------------------------------------------

#[tokio::test]
async fn receiver_route_version_changes_only_with_the_value() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;
    let path = receiver_path("agent-a");
    let initial = route_version(&agent.body);

    let connected = api
        .patch(
            &path,
            &agent.api_key,
            json!({"receiver_url": "http://receiver.test/a"}),
        )
        .await;
    assert_eq!(connected.status, 200, "{}", connected.body);
    assert_eq!(
        keys(&connected.body),
        set(&[
            "agent_id",
            "receiver_url",
            "route_version",
            "wake_policy",
            "state"
        ])
    );
    assert_eq!(connected.body["agent_id"], "agent-a");
    assert_eq!(connected.body["receiver_url"], "http://receiver.test/a");
    assert_eq!(connected.body["state"], "idle");
    assert_eq!(route_version(&connected.body), initial + 1, "null -> URL");

    let same = api
        .patch(
            &path,
            OWNER_TOKEN,
            json!({"receiver_url": "http://receiver.test/a"}),
        )
        .await;
    assert_eq!(same.status, 200);
    assert_eq!(
        route_version(&same.body),
        initial + 1,
        "same URL is a no-op"
    );

    let changed = api
        .patch(
            &path,
            OWNER_TOKEN,
            json!({"receiver_url": "https://receiver.test/b"}),
        )
        .await;
    assert_eq!(
        route_version(&changed.body),
        initial + 2,
        "URL -> other URL"
    );

    let cleared = api
        .patch(&path, &agent.api_key, json!({"receiver_url": null}))
        .await;
    assert_eq!(cleared.status, 200);
    assert_eq!(cleared.body["receiver_url"], Value::Null);
    assert_eq!(cleared.body["state"], "no_receiver");
    assert_eq!(route_version(&cleared.body), initial + 3, "URL -> null");

    let cleared_again = api
        .patch(&path, &agent.api_key, json!({"receiver_url": null}))
        .await;
    assert_eq!(
        route_version(&cleared_again.body),
        initial + 3,
        "null -> null"
    );

    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    assert_eq!(route_version(&directory.body["items"][0]), initial + 3);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn equivalent_receiver_url_spellings_do_not_bump_the_route() {
    let (_fx, server, api) = start().await;
    let agent = api
        .register("agent-a", Some("http://receiver.test/notify"))
        .await;

    let reply = api
        .patch(
            &receiver_path("agent-a"),
            &agent.api_key,
            json!({"receiver_url": "HTTP://Receiver.TEST/notify"}),
        )
        .await;

    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["receiver_url"], "http://receiver.test/notify");
    assert_eq!(route_version(&reply.body), route_version(&agent.body));
    server.shutdown().await.unwrap();
}

// ---- all membership -----------------------------------------------------------

#[tokio::test]
async fn membership_epoch_changes_only_with_the_value() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;
    let path = membership_path("agent-a");

    let same = api
        .put(&path, &agent.api_key, json!({"joined": true}))
        .await;
    assert_eq!(same.status, 200, "{}", same.body);
    assert_eq!(
        keys(&same.body),
        set(&[
            "agent_id",
            "joined_all",
            "membership_epoch",
            "effective_event_seq"
        ])
    );
    assert_eq!(same.body["agent_id"], "agent-a");
    assert_eq!(same.body["joined_all"], true);
    assert_eq!(
        same.body["effective_event_seq"], "0",
        "sequence as a string"
    );
    let initial = epoch(&same.body);

    let left = api
        .put(&path, &agent.api_key, json!({"joined": false}))
        .await;
    assert_eq!(left.body["joined_all"], false);
    assert_eq!(epoch(&left.body), initial + 1);

    let left_again = api.put(&path, OWNER_TOKEN, json!({"joined": false})).await;
    assert_eq!(left_again.status, 200);
    assert_eq!(
        epoch(&left_again.body),
        initial + 1,
        "same value is a no-op"
    );

    let rejoined = api.put(&path, OWNER_TOKEN, json!({"joined": true})).await;
    assert_eq!(rejoined.body["joined_all"], true);
    assert_eq!(epoch(&rejoined.body), initial + 2);

    let directory = api.get("/v1/agents", &agent.api_key).await;
    assert_eq!(directory.body["items"][0]["joined_all"], true);

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn leaving_all_shows_in_the_directory() {
    let (_fx, server, api) = start().await;
    let agent = api.register("agent-a", None).await;

    api.put(
        &membership_path("agent-a"),
        &agent.api_key,
        json!({"joined": false}),
    )
    .await;

    for token in [OWNER_TOKEN, agent.api_key.as_str()] {
        let directory = api.get("/v1/agents", token).await;
        assert_eq!(directory.body["items"][0]["joined_all"], false);
    }
    server.shutdown().await.unwrap();
}

// ---- Scenario 12 (part): token rotation ------------------------------------

#[tokio::test]
async fn rotation_issues_new_keys_once_and_revokes_the_old_ones() {
    let (_fx, server, api) = start().await;
    let agent = api
        .register("agent-a", Some("http://receiver.test/notify"))
        .await;
    let before = route_version(&agent.body);

    let rotated = api.rotate("agent-a", OWNER_TOKEN).await;

    assert_eq!(rotated.status, 200, "{}", rotated.body);
    assert_eq!(
        keys(&rotated.body),
        set(&[
            "agent_id",
            "route_version",
            "agent_api_key",
            "receiver_secret",
            "previous_keys_revoked"
        ])
    );
    assert_eq!(rotated.body["agent_id"], "agent-a");
    assert_eq!(rotated.body["previous_keys_revoked"], true);
    assert_eq!(route_version(&rotated.body), before + 1);
    let new_key = rotated.body["agent_api_key"].as_str().unwrap().to_owned();
    let new_secret = rotated.body["receiver_secret"].as_str().unwrap().to_owned();
    assert!(new_key.starts_with("pak_") && new_secret.starts_with("prs_"));
    assert_ne!(new_key, agent.api_key);
    assert_ne!(new_secret, agent.receiver_secret);

    api.get("/v1/agents", &agent.api_key)
        .await
        .assert_error(401, "unauthorized");
    api.put(
        &membership_path("agent-a"),
        &agent.api_key,
        json!({"joined": false}),
    )
    .await
    .assert_error(401, "unauthorized");
    api.get("/v1/agents", &new_secret)
        .await
        .assert_error(401, "unauthorized");
    let with_new_key = api.get("/v1/agents", &new_key).await;
    assert_eq!(with_new_key.status, 200);

    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    let text = directory.body.to_string();
    assert!(!text.contains(&new_key) && !text.contains(&new_secret));
    assert_eq!(route_version(&directory.body["items"][0]), before + 1);

    let again = api.rotate("agent-a", OWNER_TOKEN).await;
    assert_eq!(route_version(&again.body), before + 2);
    assert_ne!(again.body["agent_api_key"], new_key.as_str());
    api.get("/v1/agents", &new_key)
        .await
        .assert_error(401, "unauthorized");

    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn rotation_ignores_a_request_body() {
    let (_fx, server, api) = start().await;
    api.register("agent-a", None).await;

    let reply = api
        .send(
            Method::POST,
            "/v1/agents/agent-a/tokens/rotate",
            Some(OWNER_TOKEN),
            Some(json!({})),
        )
        .await;

    assert_eq!(reply.status, 200, "{}", reply.body);
    server.shutdown().await.unwrap();
}

// ---- Common error bodies ------------------------------------------------------

#[tokio::test]
async fn unknown_paths_and_methods_use_the_error_body() {
    let (_fx, server, api) = start().await;

    api.send(Method::GET, "/v1/nope", Some(OWNER_TOKEN), None)
        .await
        .assert_error(404, "not_found");
    api.send(Method::DELETE, "/v1/agents", Some(OWNER_TOKEN), None)
        .await
        .assert_error(405, "method_not_allowed");

    server.shutdown().await.unwrap();
}
