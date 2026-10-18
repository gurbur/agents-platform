//! Schema v1 → v2 (S9, `IF-DB-001`): a database written by the v1 schema is
//! migrated in place on startup. Every v1 row and value survives, the new
//! profile columns are `NULL`, and the API serves the old agents with
//! `name: null`.

mod common;

use std::process::Command;

use common::{Fixture, OWNER_TOKEN, RunningServer};
use platformd::auth::credential::hash_api_key;
use platformd::store::migrations::{self, LATEST_SCHEMA_VERSION};
use platformd::store::{Store, StoreError};
use rusqlite::types::Value as SqlValue;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_platformd");
const JOIN_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Credentials of the two v1 agents. Any bearer text works: only its
/// SHA-256 is stored.
const KEY_A: &str = "pak_v1-agent-a-key";
const KEY_B: &str = "pak_v1-agent-b-key";

const MESSAGE_ALL: &str = "msg-01M500000000000000000000A1";
const MESSAGE_DM: &str = "msg-01M500000000000000000000D1";

/// Every v1 table with an explicit v1 column list, so the comparison does
/// not depend on columns v2 adds.
const V1_ROWS: [(&str, &str); 8] = [
    (
        "agents",
        "SELECT agent_id, api_key_hash, receiver_secret, receiver_url, route_version, inbox_head, created_at FROM agents ORDER BY agent_id",
    ),
    (
        "memberships",
        "SELECT room_id, agent_id, joined, epoch, effective_event_seq FROM memberships ORDER BY agent_id",
    ),
    (
        "sequences",
        "SELECT name, value FROM sequences ORDER BY name",
    ),
    (
        "messages",
        "SELECT message_id, event_seq, room_id, sender_id, body, reply_to, created_at FROM messages ORDER BY event_seq",
    ),
    (
        "inbox_rows",
        "SELECT agent_id, inbox_seq, message_id FROM inbox_rows ORDER BY agent_id, inbox_seq",
    ),
    (
        "read_state",
        "SELECT agent_id, read_marker FROM read_state ORDER BY agent_id",
    ),
    (
        "idempotency",
        "SELECT sender_id, client_key, request_hash, message_id FROM idempotency ORDER BY sender_id, client_key",
    ),
    (
        "notification_pending",
        "SELECT agent_id, route_version, state, target_seq, last_accepted_seq, attempt, next_due_at, redrive_due_at, last_attempt_at, last_error FROM notification_pending ORDER BY agent_id",
    ),
];

/// Rows a v1 server could have written: two agents in `all`, one broadcast
/// (fanned out to both) and one DM from agent-a to agent-b, an idempotency
/// record and a read marker. Hand-written because the current register
/// path writes v2 columns.
fn v1_rows() -> String {
    format!(
        "INSERT INTO agents (agent_id, api_key_hash, receiver_secret, receiver_url, route_version, inbox_head, created_at) VALUES
           ('agent-a', '{hash_a}', 'prs_v1-secret-a', NULL, 1, 1, '2026-10-15T00:00:00.000Z'),
           ('agent-b', '{hash_b}', 'prs_v1-secret-b', 'http://127.0.0.1:9/notify', 3, 2, '2026-10-15T00:00:01.000Z');
         INSERT INTO memberships (room_id, agent_id, joined, epoch, effective_event_seq) VALUES
           ('all', 'agent-a', 1, 1, 0),
           ('all', 'agent-b', 1, 1, 0);
         UPDATE sequences SET value = 2 WHERE name = 'event_seq';
         INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at) VALUES
           ('{MESSAGE_ALL}', 1, 'all', 'agent-a', 'hello all', NULL, '2026-10-15T00:00:02.000Z'),
           ('{MESSAGE_DM}', 2, 'dm:agent-a:agent-b', 'agent-a', 'hello b', '{MESSAGE_ALL}', '2026-10-15T00:00:03.000Z');
         INSERT INTO inbox_rows (agent_id, inbox_seq, message_id) VALUES
           ('agent-a', 1, '{MESSAGE_ALL}'),
           ('agent-b', 1, '{MESSAGE_ALL}'),
           ('agent-b', 2, '{MESSAGE_DM}');
         INSERT INTO read_state (agent_id, read_marker) VALUES ('agent-a', 1), ('agent-b', 1);
         INSERT INTO idempotency (sender_id, client_key, request_hash, message_id) VALUES
           ('agent-a', 'key-dm', 'deadbeef', '{MESSAGE_DM}');
         INSERT INTO notification_pending (agent_id, route_version, state, target_seq, last_accepted_seq, attempt, next_due_at, redrive_due_at, last_attempt_at, last_error) VALUES
           ('agent-a', 1, 'idle', 0, NULL, 0, NULL, NULL, NULL, NULL),
           ('agent-b', 3, 'blocked', 2, 1, 4, NULL, NULL, '2026-10-15T00:00:04.000Z', 'http_404');",
        hash_a = hash_api_key(KEY_A),
        hash_b = hash_api_key(KEY_B),
    )
}

type Rows = Vec<(&'static str, Vec<Vec<SqlValue>>)>;

fn v1_snapshot(conn: &rusqlite::Connection) -> Result<Rows, StoreError> {
    let mut tables = Vec::new();
    for (table, sql) in V1_ROWS {
        let mut statement = conn.prepare(sql)?;
        let width = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| row.get::<_, SqlValue>(index))
                    .collect()
            })?
            .collect::<Result<Vec<Vec<SqlValue>>, _>>()?;
        tables.push((table, rows));
    }
    Ok(tables)
}

fn applied_migrations(conn: &rusqlite::Connection) -> Result<Vec<(u32, String)>, StoreError> {
    let mut statement =
        conn.prepare("SELECT version, applied_at FROM schema_migrations ORDER BY version")?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<Vec<(u32, String)>, _>>()?;
    Ok(rows)
}

/// The v2 columns of every agent, in `agent_id` order.
fn profile_columns(conn: &rusqlite::Connection) -> Result<Vec<Vec<SqlValue>>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT name, gender_form, age_band, role, intro, profile_updated_at FROM agents ORDER BY agent_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            (0..6).map(|index| row.get::<_, SqlValue>(index)).collect()
        })?
        .collect::<Result<Vec<Vec<SqlValue>>, _>>()?;
    Ok(rows)
}

/// Builds a v1 database at the fixture's path and returns its rows.
async fn write_v1_database(fx: &Fixture) -> Rows {
    std::fs::create_dir_all(fx.db_path().parent().unwrap()).unwrap();
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 1))
        .await
        .unwrap();
    assert_eq!(version, 1);
    let rows = v1_rows();
    let snapshot = store
        .transaction(move |tx| {
            tx.execute_batch(&rows)?;
            v1_snapshot(tx)
        })
        .await
        .unwrap();
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
    snapshot
}

fn item<'a>(directory: &'a Value, agent_id: &str) -> &'a Value {
    directory["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["agent_id"] == agent_id)
        .unwrap()
}

#[tokio::test]
async fn a_v1_database_is_migrated_in_place_keeping_every_row() {
    let fx = Fixture::new();
    let before = write_v1_database(&fx).await;
    assert!(before.iter().all(|(_, rows)| !rows.is_empty()));

    let config = fx.load_config();
    let server = RunningServer::start(&config).await;
    let api = server.api();

    let health = reqwest::get(server.url("/healthz")).await.unwrap();
    let health: Value = health.json().await.unwrap();
    assert_eq!(health["schema_version"], LATEST_SCHEMA_VERSION);

    let owner_view = api.get("/v1/agents", OWNER_TOKEN).await;
    let agent_view = api.get("/v1/agents", KEY_B).await;
    for view in [&owner_view, &agent_view] {
        assert_eq!(view.status, 200, "{}", view.body);
        for agent_id in ["agent-a", "agent-b"] {
            let entry = item(&view.body, agent_id);
            assert_eq!(entry["name"], Value::Null);
            assert_eq!(
                entry["profile"],
                json!({"gender_form": null, "age_band": null, "role": null, "intro": null, "updated_at": null})
            );
        }
    }
    let b = item(&owner_view.body, "agent-b");
    assert_eq!(b["route_version"], 3);
    assert_eq!(b["delivery"]["state"], "blocked");
    assert_eq!(b["created_at"], "2026-10-15T00:00:01.000Z");

    let inbox = api.get("/v1/inbox?after_seq=0", KEY_B).await;
    assert_eq!(inbox.status, 200, "{}", inbox.body);
    assert_eq!(inbox.body["read_marker"], "1");
    let bodies: Vec<&str> = inbox.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["body"].as_str().unwrap())
        .collect();
    assert_eq!(bodies, ["hello all", "hello b"]);
    let history = api
        .get("/v1/messages?room_id=dm:agent-a:agent-b", KEY_A)
        .await;
    assert_eq!(history.body["items"][0]["message_id"], MESSAGE_DM);
    let replay = api
        .send_message(KEY_A, "agent-b", "hello b", "key-dm")
        .await;
    assert_eq!(
        replay.status, 409,
        "the v1 idempotency row still binds the key"
    );
    server.shutdown().await.unwrap();

    let (after, migrations_once, profiles) = fx
        .read_db(|conn| {
            Ok((
                v1_snapshot(conn)?,
                applied_migrations(conn)?,
                profile_columns(conn)?,
            ))
        })
        .await;
    assert_eq!(after, before, "every v1 row and value is unchanged");
    let versions: Vec<u32> = migrations_once
        .iter()
        .map(|(version, _)| *version)
        .collect();
    assert_eq!(versions, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>());
    assert!(
        profiles
            .iter()
            .flatten()
            .all(|value| *value == SqlValue::Null),
        "new columns are NULL: {profiles:?}"
    );

    // Reopening is a no-op.
    let server = RunningServer::start(&config).await;
    server.shutdown().await.unwrap();
    let (again, migrations_twice) = fx
        .read_db(|conn| Ok((v1_snapshot(conn)?, applied_migrations(conn)?)))
        .await;
    assert_eq!(again, before);
    assert_eq!(migrations_twice, migrations_once, "nothing re-applied");

    let backup = Command::new(BIN)
        .arg("backup")
        .arg("--config")
        .arg(fx.path().join("platformd.toml"))
        .arg("--out")
        .arg(fx.path().join("snapshot.db"))
        .env_remove("PLATFORMD_CONFIG")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&backup.stdout);
    assert!(backup.status.success(), "{stdout}");
    assert!(
        stdout
            .lines()
            .any(|line| line == format!("schema_version: {LATEST_SCHEMA_VERSION}")),
        "{stdout}"
    );
}

#[tokio::test]
async fn the_owner_names_a_migrated_agent() {
    let fx = Fixture::new();
    write_v1_database(&fx).await;
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();

    let named = api
        .put(
            "/v1/agents/agent-a/profile",
            OWNER_TOKEN,
            json!({"name": "에이전트 A"}),
        )
        .await;
    assert_eq!(named.status, 200, "{}", named.body);
    assert_eq!(named.body["name"], "에이전트 A");
    assert!(named.body["profile"]["updated_at"].is_string());
    let declared = api
        .put("/v1/agents/agent-b/profile", KEY_B, json!({"role": "리뷰"}))
        .await;
    assert_eq!(declared.status, 200, "{}", declared.body);
    assert_eq!(declared.body["name"], Value::Null, "still unnamed");
    assert_eq!(declared.body["profile"]["role"], "리뷰");

    server.shutdown().await.unwrap();
}

/// Schema v2 → v3 (S11): a v2 database gains an empty `agent_avatars`
/// table, keeps its agents (with their profile) and serves `avatar: null`.
#[tokio::test]
async fn a_v2_database_gains_an_empty_avatar_table() {
    let fx = Fixture::new();
    std::fs::create_dir_all(fx.db_path().parent().unwrap()).unwrap();
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 2))
        .await
        .unwrap();
    assert_eq!(version, 2);
    let rows = v1_rows();
    let before = store
        .transaction(move |tx| {
            tx.execute_batch(&rows)?;
            tx.execute(
                "UPDATE agents SET name = 'A', role = 'ops', profile_updated_at = created_at
                 WHERE agent_id = 'agent-a'",
                [],
            )?;
            Ok((v1_snapshot(tx)?, has_avatar_table(tx)?))
        })
        .await
        .unwrap();
    assert!(!before.1, "v2 has no avatar table");
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();

    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    let directory = api.get("/v1/agents", KEY_B).await;
    assert_eq!(directory.status, 200, "{}", directory.body);
    let a = item(&directory.body, "agent-a");
    assert_eq!(a["name"], "A");
    assert_eq!(a["profile"]["role"], "ops");
    assert_eq!(a["avatar"], Value::Null);
    server.shutdown().await.unwrap();

    let (after, has_table, avatars, versions) = fx
        .read_db(|conn| {
            let avatars: i64 =
                conn.query_row("SELECT COUNT(*) FROM agent_avatars", [], |row| row.get(0))?;
            Ok((
                v1_snapshot(conn)?,
                has_avatar_table(conn)?,
                avatars,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(after, before.0, "every row is unchanged");
    assert!(has_table);
    assert_eq!(avatars, 0);
    let versions: Vec<u32> = versions.iter().map(|(version, _)| *version).collect();
    assert_eq!(versions, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>());
}

fn has_avatar_table(conn: &rusqlite::Connection) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'agent_avatars'",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

/// Rows only v3 has, compared alongside [`V1_ROWS`].
fn v3_extra_rows(conn: &rusqlite::Connection) -> Result<Vec<Vec<SqlValue>>, StoreError> {
    let mut statement = conn.prepare(
        "SELECT agent_id, content_type, bytes, sha256, updated_at FROM agent_avatars ORDER BY agent_id",
    )?;
    let rows = statement
        .query_map([], |row| {
            (0..5).map(|index| row.get::<_, SqlValue>(index)).collect()
        })?
        .collect::<Result<Vec<Vec<SqlValue>>, _>>()?;
    Ok(rows)
}

/// `(table, from, to)` of every foreign key of `table`.
fn foreign_keys_of(
    conn: &rusqlite::Connection,
    table: &str,
) -> Result<Vec<(String, String, String)>, StoreError> {
    let mut statement = conn.prepare(&format!("PRAGMA foreign_key_list({table})"))?;
    let rows = statement
        .query_map([], |row| Ok((row.get(2)?, row.get(3)?, row.get(4)?)))?
        .collect::<Result<Vec<(String, String, String)>, _>>()?;
    Ok(rows)
}

fn foreign_key_violations(conn: &rusqlite::Connection) -> Result<usize, StoreError> {
    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    let count = statement.query_map([], |_| Ok(()))?.count();
    Ok(count)
}

fn index_names(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, StoreError> {
    let mut statement =
        conn.prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = ?1")?;
    let rows = statement
        .query_map([table], |row| row.get(0))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(rows)
}

/// A v3 database with every v1 row plus a profile and an avatar.
async fn write_v3_database(fx: &Fixture) -> (Rows, Vec<Vec<SqlValue>>) {
    std::fs::create_dir_all(fx.db_path().parent().unwrap()).unwrap();
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 3))
        .await
        .unwrap();
    assert_eq!(version, 3);
    let rows = v1_rows();
    let before = store
        .transaction(move |tx| {
            tx.execute_batch(&rows)?;
            tx.execute(
                "UPDATE agents SET name = 'A', profile_updated_at = created_at WHERE agent_id = 'agent-a'",
                [],
            )?;
            tx.execute(
                "INSERT INTO agent_avatars (agent_id, content_type, bytes, sha256, updated_at)
                 VALUES ('agent-a', 'image/png', X'89504E470D0A1A0A00', 'abc', '2026-10-18T00:00:00.000Z')",
                [],
            )?;
            Ok((v1_snapshot(tx)?, v3_extra_rows(tx)?))
        })
        .await
        .unwrap();
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
    before
}

/// Schema v3 → v4 (S12): `messages` is rebuilt without its `sender_id`
/// foreign key and keeps every row, so an agent that sent messages can be
/// deleted while its messages stay.
#[tokio::test]
async fn a_v3_database_rebuilds_messages_without_the_sender_foreign_key() {
    let fx = Fixture::new();
    let before = write_v3_database(&fx).await;
    let fks_before = fx.read_db(|conn| foreign_keys_of(conn, "messages")).await;
    assert!(
        fks_before
            .iter()
            .any(|(table, from, _)| table == "agents" && from == "sender_id"),
        "v3 still has the sender foreign key: {fks_before:?}"
    );

    let config = fx.load_config();
    let server = RunningServer::start(&config).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], LATEST_SCHEMA_VERSION);
    server.shutdown().await.unwrap();

    let (after, extra, fks, violations, indexes, versions) = fx
        .read_db(|conn| {
            Ok((
                v1_snapshot(conn)?,
                v3_extra_rows(conn)?,
                foreign_keys_of(conn, "messages")?,
                foreign_key_violations(conn)?,
                index_names(conn, "messages")?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(after, before.0, "every row and value is unchanged");
    assert_eq!(extra, before.1, "avatars are unchanged");
    assert_eq!(
        fks,
        [(
            "messages".to_owned(),
            "reply_to".to_owned(),
            "message_id".to_owned()
        )],
        "only reply_to keeps a foreign key"
    );
    assert_eq!(violations, 0, "PRAGMA foreign_key_check is empty");
    assert!(
        indexes.iter().any(|name| name == "messages_room_event"),
        "{indexes:?}"
    );
    let versions: Vec<u32> = versions.iter().map(|(version, _)| *version).collect();
    assert_eq!(versions, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>());

    // agent-a sent both messages; deleting it keeps them.
    let server = RunningServer::start(&config).await;
    let api_after = server.api();
    let deleted = api_after
        .send(
            reqwest::Method::DELETE,
            "/v1/agents/agent-a",
            Some(OWNER_TOKEN),
            None,
        )
        .await;
    assert_eq!(deleted.status.as_u16(), 204, "{}", deleted.body);
    let history = api_after
        .get("/v1/messages?room_id=dm:agent-a:agent-b", KEY_B)
        .await;
    assert_eq!(history.body["items"][0]["message_id"], MESSAGE_DM);
    assert_eq!(history.body["items"][0]["sender_id"], "agent-a");
    server.shutdown().await.unwrap();
}

/// The v4 migration turns `foreign_keys` off only around its own
/// transaction: the connection has it on afterwards, and a violation found
/// by `PRAGMA foreign_key_check` rolls the version back.
#[tokio::test]
async fn the_v4_migration_restores_foreign_keys_and_refuses_violations() {
    let fx = Fixture::new();
    write_v3_database(&fx).await;
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let (version, foreign_keys) = store
        .run(|conn| {
            let version = migrations::apply(conn)?;
            let foreign_keys: i64 =
                conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
            Ok((version, foreign_keys))
        })
        .await
        .unwrap();
    assert_eq!(version, LATEST_SCHEMA_VERSION);
    assert_eq!(foreign_keys, 1);
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();

    let broken = Fixture::new();
    write_v3_database(&broken).await;
    let (store, thread) = Store::open(&broken.db_path()).unwrap();
    let outcome = store
        .run(|conn| {
            // A dangling inbox row that only foreign_keys=OFF lets in.
            conn.pragma_update(None, "foreign_keys", "OFF")?;
            conn.execute(
                "INSERT INTO inbox_rows (agent_id, inbox_seq, message_id)
                 VALUES ('agent-a', 9, 'msg-01M5000000000000000000GONE')",
                [],
            )?;
            conn.pragma_update(None, "foreign_keys", "ON")?;
            let applied = migrations::apply(conn);
            let version = migrations::current_version(conn)?;
            let foreign_keys: i64 =
                conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
            let has_sender_fk = foreign_keys_of(conn, "messages")?
                .iter()
                .any(|(table, _, _)| table == "agents");
            Ok((applied.is_err(), version, foreign_keys, has_sender_fk))
        })
        .await
        .unwrap();
    assert_eq!(
        outcome,
        (true, 3, 1, true),
        "refused, rolled back to v3, foreign_keys on, messages untouched"
    );
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
}
