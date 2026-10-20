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
        // `all` only: v5 adds DM membership rows next to these (S13).
        "SELECT room_id, agent_id, joined, epoch, effective_event_seq FROM memberships WHERE room_id = 'all' ORDER BY agent_id",
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

// ---- v4 → v5 (S13): rooms as a table, DM memberships, event rows ---------------

const V5_ALL_1: &str = "msg-01M500000000000000000005A1";
const V5_DM_1: &str = "msg-01M500000000000000000005D1";
const V5_DM_2: &str = "msg-01M500000000000000000005D2";
const V5_DM_Z: &str = "msg-01M500000000000000000005Z1";
const V5_ALL_Z: &str = "msg-01M500000000000000000005Z2";

/// Rows a v4 server could have written: agent-a and agent-b in `all`,
/// agent-c who left it, a DM between a and b, and a DM and broadcast from
/// agent-z, deleted since (its messages stay, its rows are gone).
fn v4_rows() -> String {
    format!(
        "INSERT INTO agents (agent_id, api_key_hash, receiver_secret, receiver_url, route_version, inbox_head, created_at, name, profile_updated_at) VALUES
           ('agent-a', '{hash_a}', 'prs_v4-a', NULL, 1, 4, '2026-10-19T00:00:00.000Z', 'A', '2026-10-19T00:00:00.000Z'),
           ('agent-b', '{hash_b}', 'prs_v4-b', 'http://127.0.0.1:9/n', 2, 3, '2026-10-19T00:00:01.000Z', 'B', '2026-10-19T00:00:01.000Z'),
           ('agent-c', 'hash-c', 'prs_v4-c', NULL, 1, 0, '2026-10-19T00:00:02.000Z', 'C', '2026-10-19T00:00:02.000Z');
         INSERT INTO memberships (room_id, agent_id, joined, epoch, effective_event_seq) VALUES
           ('all', 'agent-a', 1, 1, 0),
           ('all', 'agent-b', 1, 1, 0),
           ('all', 'agent-c', 0, 2, 1);
         UPDATE sequences SET value = 5 WHERE name = 'event_seq';
         INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at) VALUES
           ('{V5_ALL_1}', 1, 'all', 'agent-a', 'all 1', NULL, '2026-10-19T01:00:00.000Z'),
           ('{V5_DM_1}', 2, 'dm:agent-a:agent-b', 'agent-a', 'dm 1', NULL, '2026-10-19T01:00:01.000Z'),
           ('{V5_DM_2}', 3, 'dm:agent-a:agent-b', 'agent-b', 'dm 2', '{V5_DM_1}', '2026-10-19T01:00:02.000Z'),
           ('{V5_DM_Z}', 4, 'dm:agent-a:agent-z', 'agent-z', 'from z', NULL, '2026-10-19T01:00:03.000Z'),
           ('{V5_ALL_Z}', 5, 'all', 'agent-z', 'z to all', NULL, '2026-10-19T01:00:04.000Z');
         INSERT INTO inbox_rows (agent_id, inbox_seq, message_id) VALUES
           ('agent-a', 1, '{V5_ALL_1}'),
           ('agent-b', 1, '{V5_ALL_1}'),
           ('agent-b', 2, '{V5_DM_1}'),
           ('agent-a', 2, '{V5_DM_2}'),
           ('agent-a', 3, '{V5_DM_Z}'),
           ('agent-a', 4, '{V5_ALL_Z}'),
           ('agent-b', 3, '{V5_ALL_Z}');
         INSERT INTO read_state (agent_id, read_marker) VALUES ('agent-a', 2), ('agent-b', 3), ('agent-c', 0);
         INSERT INTO idempotency (sender_id, client_key, request_hash, message_id) VALUES
           ('agent-a', 'key-dm', 'deadbeef', '{V5_DM_1}');
         INSERT INTO notification_pending (agent_id, route_version, state, target_seq, last_accepted_seq, attempt, next_due_at, redrive_due_at, last_attempt_at, last_error) VALUES
           ('agent-a', 1, 'pending', 4, NULL, 0, NULL, NULL, NULL, NULL),
           ('agent-b', 2, 'idle', 3, 3, 0, NULL, NULL, '2026-10-19T02:00:00.000Z', NULL),
           ('agent-c', 1, 'idle', 0, NULL, 0, NULL, NULL, NULL, NULL);
         INSERT INTO agent_avatars (agent_id, content_type, bytes, sha256, updated_at)
           VALUES ('agent-a', 'image/png', X'89504E470D0A1A0A00', 'abc', '2026-10-19T00:00:00.000Z');",
        hash_a = hash_api_key(KEY_A),
        hash_b = hash_api_key(KEY_B),
    )
}

/// Every v4 column of every table, in a stable order. `memberships` is
/// restricted to `all`: v5 adds DM rows next to the existing ones.
const V4_ROWS: [(&str, &str); 9] = [
    (
        "agents",
        "SELECT agent_id, api_key_hash, receiver_secret, receiver_url, route_version, inbox_head, created_at, name, gender_form, age_band, role, intro, profile_updated_at FROM agents ORDER BY agent_id",
    ),
    (
        "memberships",
        "SELECT room_id, agent_id, joined, epoch, effective_event_seq FROM memberships WHERE room_id = 'all' ORDER BY agent_id",
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
    (
        "agent_avatars",
        "SELECT agent_id, content_type, bytes, sha256, updated_at FROM agent_avatars ORDER BY agent_id",
    ),
];

fn select_all(conn: &rusqlite::Connection, sql: &str) -> Result<Vec<Vec<SqlValue>>, StoreError> {
    let mut statement = conn.prepare(sql)?;
    let width = statement.column_count();
    let rows = statement
        .query_map([], |row| {
            (0..width)
                .map(|index| row.get::<_, SqlValue>(index))
                .collect()
        })?
        .collect::<Result<Vec<Vec<SqlValue>>, _>>()?;
    Ok(rows)
}

fn v4_snapshot(conn: &rusqlite::Connection) -> Result<Rows, StoreError> {
    V4_ROWS
        .iter()
        .map(|(table, sql)| Ok((*table, select_all(conn, sql)?)))
        .collect()
}

/// Builds a v4 database at the fixture's path, runs `extra` on it and
/// returns its rows.
async fn write_v4_database(fx: &Fixture, extra: &'static str) -> Rows {
    std::fs::create_dir_all(fx.db_path().parent().unwrap()).unwrap();
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 4))
        .await
        .unwrap();
    assert_eq!(version, 4);
    let rows = v4_rows();
    let snapshot = store
        .transaction(move |tx| {
            tx.execute_batch(&rows)?;
            tx.execute_batch(extra)?;
            v4_snapshot(tx)
        })
        .await
        .unwrap();
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
    snapshot
}

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
}

#[tokio::test]
async fn an_empty_database_starts_at_v5_with_only_the_all_room() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], LATEST_SCHEMA_VERSION);
    server.shutdown().await.unwrap();

    let (rooms, memberships, mentions) = fx
        .read_db(|conn| {
            Ok((
                select_all(
                    conn,
                    "SELECT room_id, kind, title, created_by, archived_at FROM rooms",
                )?,
                select_all(conn, "SELECT * FROM memberships")?,
                select_all(conn, "SELECT * FROM message_mentions")?,
            ))
        })
        .await;
    assert_eq!(
        rooms,
        [vec![
            text("all"),
            text("all"),
            SqlValue::Null,
            text("owner"),
            SqlValue::Null
        ]]
    );
    assert!(memberships.is_empty() && mentions.is_empty());
    let created_at: String = fx
        .read_db(|conn| Ok(conn.query_row("SELECT created_at FROM rooms", [], |row| row.get(0))?))
        .await;
    assert!(
        created_at.len() == 24 && created_at.ends_with('Z') && created_at.as_bytes()[19] == b'.',
        "RFC 3339 with milliseconds: {created_at}"
    );
}

#[tokio::test]
async fn a_v4_database_gains_rooms_dm_memberships_and_event_columns() {
    let fx = Fixture::new();
    let before = write_v4_database(&fx, "").await;
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

    let after = fx.read_db(|conn| v4_snapshot(conn)).await;
    assert_eq!(after, before, "every existing row and value is unchanged");

    let (rooms, dm_members, inbox, messages, policies, mentions, violations, orphans, versions) =
        fx.read_db(|conn| {
            Ok((
                select_all(
                    conn,
                    "SELECT room_id, kind, title, created_by, created_at, archived_at FROM rooms ORDER BY room_id",
                )?,
                select_all(
                    conn,
                    "SELECT room_id, agent_id, joined, epoch, effective_event_seq FROM memberships WHERE room_id <> 'all' ORDER BY room_id, agent_id",
                )?,
                select_all(conn, "SELECT DISTINCT kind, wake, woke FROM inbox_rows")?,
                select_all(conn, "SELECT DISTINCT wake FROM messages")?,
                select_all(conn, "SELECT DISTINCT wake_policy FROM agents")?,
                select_all(conn, "SELECT * FROM message_mentions")?,
                foreign_key_violations(conn)?,
                select_all(
                    conn,
                    "SELECT room_id FROM messages WHERE room_id NOT IN (SELECT room_id FROM rooms)
                     UNION ALL SELECT room_id FROM memberships WHERE room_id NOT IN (SELECT room_id FROM rooms)",
                )?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(
        rooms,
        [
            vec![
                text("all"),
                text("all"),
                SqlValue::Null,
                text("owner"),
                text("2026-10-19T01:00:00.000Z"),
                SqlValue::Null
            ],
            vec![
                text("dm:agent-a:agent-b"),
                text("dm"),
                SqlValue::Null,
                text("agent-a"),
                text("2026-10-19T01:00:01.000Z"),
                SqlValue::Null
            ],
            vec![
                text("dm:agent-a:agent-z"),
                text("dm"),
                SqlValue::Null,
                text("agent-z"),
                text("2026-10-19T01:00:03.000Z"),
                SqlValue::Null
            ],
        ]
    );
    let joined = |room: &str, agent: &str| {
        vec![
            text(room),
            text(agent),
            SqlValue::Integer(1),
            SqlValue::Integer(1),
            SqlValue::Integer(5),
        ]
    };
    assert_eq!(
        dm_members,
        [
            joined("dm:agent-a:agent-b", "agent-a"),
            joined("dm:agent-a:agent-b", "agent-b"),
            joined("dm:agent-a:agent-z", "agent-a"),
        ],
        "only registered members"
    );
    assert_eq!(
        inbox,
        [vec![
            text("message"),
            SqlValue::Integer(1),
            SqlValue::Integer(1)
        ]],
        "every v4 row was a woken row"
    );
    assert_eq!(messages, [vec![SqlValue::Integer(1)]]);
    assert_eq!(policies, [vec![text("sender")]]);
    assert!(mentions.is_empty());
    assert_eq!(violations, 0, "PRAGMA foreign_key_check is empty");
    assert!(orphans.is_empty(), "the post-check is empty");

    // The backfilled rooms serve the S13 API right away.
    let server = RunningServer::start(&config).await;
    let api = server.api();
    let listed = api.get("/v1/rooms", KEY_A).await;
    let ids: Vec<&str> = listed.body["rooms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|room| room["room_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["all", "dm:agent-a:agent-z", "dm:agent-a:agent-b"]);
    let item = api.get("/v1/inbox?after_seq=0&limit=1", KEY_A).await;
    assert_eq!(item.body["items"][0]["kind"], "message");
    assert_eq!(item.body["items"][0]["wake"], true);
    assert_eq!(item.body["items"][0]["woke"], true);
    assert_eq!(item.body["items"][0]["mentions"], json!([]));
    server.shutdown().await.unwrap();

    // Reopening is a no-op.
    let server = RunningServer::start(&config).await;
    server.shutdown().await.unwrap();
    let (again, versions_again) = fx
        .read_db(|conn| Ok((v4_snapshot(conn)?, applied_migrations(conn)?)))
        .await;
    assert_eq!(again, before);
    assert_eq!(versions_again, versions, "nothing re-applied");
}

#[tokio::test]
async fn a_v5_post_check_violation_rolls_back_and_refuses_to_start() {
    let fx = Fixture::new();
    // A message in a room the backfill cannot create a row for.
    write_v4_database(
        &fx,
        "INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at)
         VALUES ('msg-01M500000000000000000005X1', 6, 'lobby', 'agent-a', 'x', NULL, '2026-10-19T03:00:00.000Z');",
    )
    .await;

    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let (refused, version, has_rooms) = store
        .run(|conn| {
            let refused = migrations::apply(conn).is_err();
            let version = migrations::current_version(conn)?;
            let has_rooms: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'rooms'",
                [],
                |row| row.get(0),
            )?;
            Ok((refused, version, has_rooms))
        })
        .await
        .unwrap();
    assert_eq!(
        (refused, version, has_rooms),
        (true, 4, 0),
        "rolled back to v4"
    );
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();

    assert!(
        platformd::start(&fx.load_config()).await.is_err(),
        "startup is refused"
    );
}

// ---- v6 (S14): agents.last_seen_at -----------------------------------------------

/// Every v5 column of every table, in a stable order: v6 adds only
/// `agents.last_seen_at`, so the agents query names the v5 columns.
const V5_ROWS: [(&str, &str); 12] = [
    (
        "agents",
        "SELECT agent_id, api_key_hash, receiver_secret, receiver_url, route_version, inbox_head, created_at, name, gender_form, age_band, role, intro, profile_updated_at, wake_policy FROM agents ORDER BY agent_id",
    ),
    (
        "memberships",
        "SELECT room_id, agent_id, joined, epoch, effective_event_seq FROM memberships ORDER BY room_id, agent_id",
    ),
    (
        "rooms",
        "SELECT room_id, kind, title, created_by, created_at, archived_at FROM rooms ORDER BY room_id",
    ),
    (
        "sequences",
        "SELECT name, value FROM sequences ORDER BY name",
    ),
    (
        "messages",
        "SELECT message_id, event_seq, room_id, sender_id, body, reply_to, created_at, wake FROM messages ORDER BY event_seq",
    ),
    (
        "message_mentions",
        "SELECT message_id, agent_id FROM message_mentions ORDER BY message_id, agent_id",
    ),
    (
        "inbox_rows",
        "SELECT agent_id, inbox_seq, message_id, kind, wake, woke FROM inbox_rows ORDER BY agent_id, inbox_seq",
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
    (
        "agent_avatars",
        "SELECT agent_id, content_type, bytes, sha256, updated_at FROM agent_avatars ORDER BY agent_id",
    ),
    (
        "schema_migrations",
        "SELECT version, applied_at FROM schema_migrations WHERE version <= 5 ORDER BY version",
    ),
];

fn v5_snapshot(conn: &rusqlite::Connection) -> Result<Rows, StoreError> {
    V5_ROWS
        .iter()
        .map(|(table, sql)| Ok((*table, select_all(conn, sql)?)))
        .collect()
}

/// S13 rows on top of the v4 fixture: a topic room with a member, a quiet
/// message with a mention and an `always` wake policy.
const V5_EXTRA: &str = "
INSERT INTO rooms (room_id, kind, title, created_by, created_at, archived_at)
  VALUES ('room:launch', 'topic', '출시', 'owner', '2026-10-20T00:00:00.000Z', NULL);
INSERT INTO memberships (room_id, agent_id, joined, epoch, effective_event_seq)
  VALUES ('room:launch', 'agent-a', 1, 1, 5);
UPDATE sequences SET value = 6 WHERE name = 'event_seq';
INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at, wake)
  VALUES ('msg-01M600000000000000000006T1', 6, 'room:launch', 'agent-a', 'quiet', NULL, '2026-10-20T00:00:01.000Z', 0);
INSERT INTO message_mentions (message_id, agent_id) VALUES ('msg-01M600000000000000000006T1', 'agent-b');
UPDATE agents SET wake_policy = 'always' WHERE agent_id = 'agent-b';
";

/// Builds a v5 database (the v4 fixture migrated, plus [`V5_EXTRA`]) and
/// returns its rows.
async fn write_v5_database(fx: &Fixture) -> Rows {
    write_v4_database(fx, "").await;
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 5))
        .await
        .unwrap();
    assert_eq!(version, 5);
    let snapshot = store
        .transaction(|tx| {
            tx.execute_batch(V5_EXTRA)?;
            v5_snapshot(tx)
        })
        .await
        .unwrap();
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
    snapshot
}

fn has_last_seen_column(conn: &rusqlite::Connection) -> Result<bool, StoreError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('agents') WHERE name = 'last_seen_at'",
        [],
        |row| row.get(0),
    )?;
    Ok(count == 1)
}

#[tokio::test]
async fn an_empty_database_starts_at_v6() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], 6);
    assert_eq!(LATEST_SCHEMA_VERSION, 6);
    let api = server.api();
    api.register("agent-a", None).await;
    let directory = api.get("/v1/agents", OWNER_TOKEN).await;
    assert_eq!(directory.body["items"][0]["last_seen_at"], Value::Null);
    server.shutdown().await.unwrap();

    let (has_column, versions) = fx
        .read_db(|conn| Ok((has_last_seen_column(conn)?, applied_migrations(conn)?)))
        .await;
    assert!(has_column);
    let versions: Vec<u32> = versions.into_iter().map(|(version, _)| version).collect();
    assert_eq!(versions, (1..=6).collect::<Vec<u32>>());
}

#[tokio::test]
async fn a_v5_database_gains_an_empty_last_seen_at_column() {
    let fx = Fixture::new();
    let before = write_v5_database(&fx).await;
    let config = fx.load_config();
    let server = RunningServer::start(&config).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], 6);
    // The migrated agents read as offline and never seen.
    let directory = server.api().get("/v1/agents", OWNER_TOKEN).await;
    for item in directory.body["items"].as_array().unwrap() {
        assert_eq!(item["last_seen_at"], Value::Null, "{item}");
        assert_eq!(item["presence"]["state"], "offline", "{item}");
    }
    server.shutdown().await.unwrap();

    let (after, last_seen, violations, versions) = fx
        .read_db(|conn| {
            Ok((
                v5_snapshot(conn)?,
                select_all(conn, "SELECT DISTINCT last_seen_at FROM agents")?,
                foreign_key_violations(conn)?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(after, before, "every v5 row and value is unchanged");
    assert_eq!(last_seen, [vec![SqlValue::Null]], "no backfill");
    assert_eq!(violations, 0, "PRAGMA foreign_key_check is empty");
    let numbers: Vec<u32> = versions.iter().map(|(version, _)| *version).collect();
    assert_eq!(numbers, (1..=6).collect::<Vec<u32>>());

    // Reopening is a no-op.
    let server = RunningServer::start(&config).await;
    server.shutdown().await.unwrap();
    let (again, versions_again) = fx
        .read_db(|conn| Ok((v5_snapshot(conn)?, applied_migrations(conn)?)))
        .await;
    assert_eq!(again, before);
    assert_eq!(versions_again, versions, "nothing re-applied");
}
