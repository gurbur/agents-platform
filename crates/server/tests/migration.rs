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
        // `event_seq` only: v8 adds the `change_seq` row (S16).
        "SELECT name, value FROM sequences WHERE name = 'event_seq' ORDER BY name",
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
/// path writes v2 columns. The DM is no reply: a v1 server refused a reply
/// into another room (`reply_to_room_mismatch`), and v7's thread invariant
/// refuses such a row.
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
           ('{MESSAGE_DM}', 2, 'dm:agent-a:agent-b', 'agent-a', 'hello b', NULL, '2026-10-15T00:00:03.000Z');
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
    // v7 adds `thread_id` and `corrects`, which refer to messages too.
    let mut fks = fks;
    fks.sort();
    assert_eq!(
        fks,
        ["corrects", "reply_to", "thread_id"].map(|from| (
            "messages".to_owned(),
            from.to_owned(),
            "message_id".to_owned()
        )),
        "no foreign key to agents: only references to other messages"
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
        // `event_seq` only: v8 adds the `change_seq` row (S16).
        "SELECT name, value FROM sequences WHERE name = 'event_seq' ORDER BY name",
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
        // `event_seq` only: v8 adds the `change_seq` row (S16).
        "SELECT name, value FROM sequences WHERE name = 'event_seq' ORDER BY name",
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
    // S15: the latest version is 7 and includes v6.
    assert_eq!(health["schema_version"], LATEST_SCHEMA_VERSION);
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
    assert_eq!(versions, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>());
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
    assert_eq!(health["schema_version"], LATEST_SCHEMA_VERSION);
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
    assert_eq!(numbers, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>());

    // Reopening is a no-op.
    let server = RunningServer::start(&config).await;
    server.shutdown().await.unwrap();
    let (again, versions_again) = fx
        .read_db(|conn| Ok((v5_snapshot(conn)?, applied_migrations(conn)?)))
        .await;
    assert_eq!(again, before);
    assert_eq!(versions_again, versions, "nothing re-applied");
}

// ---- v7 (S15): threads and corrections -----------------------------------------------

const R1: &str = "msg-01M700000000000000000007R1";
const C1: &str = "msg-01M700000000000000000008C1";
const C2: &str = "msg-01M700000000000000000009C2";
const C3: &str = "msg-01M70000000000000000000AC3";
const R2: &str = "msg-01M70000000000000000000BR2";
const D1: &str = "msg-01M70000000000000000000CD1";
const D2: &str = "msg-01M70000000000000000000DD2";
const E1: &str = "msg-01M70000000000000000000EE1";
const E2: &str = "msg-01M70000000000000000000FE2";
const L1: &str = "msg-01M70000000000000000000GN1";

/// S13/S14-era rows on top of the v5 fixture (which already holds the reply
/// `V5_DM_2 → V5_DM_1` and agent-z's root `V5_ALL_Z`; agent-z is not
/// registered): a chain three replies deep, a root with two replies one of
/// which mentions a registered and an unregistered agent, a deleted agent's
/// root with a reply from it and to it, and a message nobody answered.
fn v6_extra() -> String {
    format!(
        "UPDATE sequences SET value = 16 WHERE name = 'event_seq';
         INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at, wake) VALUES
           ('{R1}', 7, 'all', 'agent-a', 'r1', NULL, '2026-10-20T01:00:07.000Z', 1),
           ('{C1}', 8, 'all', 'agent-b', 'c1', '{R1}', '2026-10-20T01:00:08.000Z', 1),
           ('{C2}', 9, 'all', 'agent-a', 'c2', '{C1}', '2026-10-20T01:00:09.000Z', 1),
           ('{C3}', 10, 'all', 'agent-c', 'c3', '{C2}', '2026-10-20T01:00:10.000Z', 0),
           ('{R2}', 11, 'all', 'agent-b', 'r2', NULL, '2026-10-20T01:00:11.000Z', 1),
           ('{D1}', 12, 'all', 'agent-a', 'd1', '{R2}', '2026-10-20T01:00:12.000Z', 1),
           ('{D2}', 13, 'all', 'agent-b', 'd2', '{D1}', '2026-10-20T01:00:13.000Z', 1),
           ('{E1}', 14, 'all', 'agent-a', 'e1', '{V5_ALL_Z}', '2026-10-20T01:00:14.000Z', 1),
           ('{E2}', 15, 'all', 'agent-z', 'e2', '{E1}', '2026-10-20T01:00:15.000Z', 1),
           ('{L1}', 16, 'all', 'agent-b', 'l1', NULL, '2026-10-20T01:00:16.000Z', 1);
         INSERT INTO message_mentions (message_id, agent_id) VALUES
           ('{D1}', 'agent-c'),
           ('{D1}', 'agent-z');
         UPDATE agents SET last_seen_at = '2026-10-20T02:00:00.000Z' WHERE agent_id = 'agent-a';"
    )
}

/// Every v6 column: the v5 rows plus `agents.last_seen_at` and the v6
/// migration row.
fn v6_snapshot(conn: &rusqlite::Connection) -> Result<Rows, StoreError> {
    let mut rows = v5_snapshot(conn)?;
    rows.push((
        "agents.last_seen_at",
        select_all(
            conn,
            "SELECT agent_id, last_seen_at FROM agents ORDER BY agent_id",
        )?,
    ));
    rows.push((
        "schema_migrations v6",
        select_all(
            conn,
            "SELECT version, applied_at FROM schema_migrations WHERE version = 6",
        )?,
    ));
    Ok(rows)
}

/// Builds a v6 database (the v5 fixture migrated, plus [`v6_extra`] and
/// `extra`) and returns its rows.
async fn write_v6_database(fx: &Fixture, extra: &'static str) -> Rows {
    write_v5_database(fx).await;
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 6))
        .await
        .unwrap();
    assert_eq!(version, 6);
    let rows = v6_extra();
    let snapshot = store
        .transaction(move |tx| {
            tx.execute_batch(&rows)?;
            tx.execute_batch(extra)?;
            v6_snapshot(tx)
        })
        .await
        .unwrap();
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
    snapshot
}

fn int(value: i64) -> SqlValue {
    SqlValue::Integer(value)
}

#[tokio::test]
async fn an_empty_database_starts_at_v7_with_thread_columns_and_participants() {
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

    let (columns, participants, indexes) = fx
        .read_db(|conn| {
            Ok((
                select_all(
                    conn,
                    "SELECT name FROM pragma_table_info('messages')
                     WHERE name IN ('thread_id', 'corrects') ORDER BY name",
                )?,
                select_all(
                    conn,
                    "SELECT name FROM pragma_table_info('thread_participants') ORDER BY cid",
                )?,
                index_names(conn, "messages")?,
            ))
        })
        .await;
    assert_eq!(columns, [vec![text("corrects")], vec![text("thread_id")]]);
    assert_eq!(
        participants,
        ["thread_id", "agent_id", "joined_via", "muted", "joined_at"].map(|name| vec![text(name)])
    );
    assert!(
        indexes.iter().any(|name| name == "messages_thread_event"),
        "{indexes:?}"
    );
}

#[tokio::test]
async fn a_v6_database_binds_old_reply_chains_into_threads() {
    let fx = Fixture::new();
    let before = write_v6_database(&fx, "").await;
    let config = fx.load_config();
    let server = RunningServer::start(&config).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], LATEST_SCHEMA_VERSION);
    // The API sees the backfill: a top-level view without the old replies.
    let top = server
        .api()
        .get("/v1/messages?room_id=all&top_level=true", OWNER_TOKEN)
        .await;
    let summary = top.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["message_id"] == R1)
        .map(|item| item["thread_summary"].clone())
        .unwrap();
    assert_eq!(summary, json!({"reply_count": 3, "last_event_seq": "10"}));
    server.shutdown().await.unwrap();

    let (after, threads, participants, violations, post_check, versions) = fx
        .read_db(|conn| {
            Ok((
                v6_snapshot(conn)?,
                select_all(
                    conn,
                    "SELECT message_id, thread_id, corrects FROM messages ORDER BY event_seq",
                )?,
                select_all(
                    conn,
                    "SELECT thread_id, agent_id, joined_via, muted, joined_at FROM thread_participants
                     ORDER BY thread_id, joined_at, agent_id",
                )?,
                foreign_key_violations(conn)?,
                select_all(
                    conn,
                    "SELECT t.message_id FROM messages t JOIN messages r ON r.message_id = t.thread_id
                     WHERE r.thread_id IS NOT NULL OR r.room_id <> t.room_id
                     UNION ALL SELECT message_id FROM messages WHERE reply_to IS NOT NULL AND thread_id IS NULL",
                )?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(after, before, "every v6 row and value is unchanged");
    let thread_of = |message: &str| -> SqlValue {
        threads
            .iter()
            .find(|row| row[0] == text(message))
            .map(|row| row[1].clone())
            .unwrap()
    };
    for (message, root) in [
        (V5_DM_2, Some(V5_DM_1)),
        (C1, Some(R1)),
        (C2, Some(R1)),
        (C3, Some(R1)),
        (D1, Some(R2)),
        (D2, Some(R2)),
        (E1, Some(V5_ALL_Z)),
        (E2, Some(V5_ALL_Z)),
        (V5_ALL_1, None),
        (V5_DM_1, None),
        (V5_DM_Z, None),
        (V5_ALL_Z, None),
        (R1, None),
        (R2, None),
        (L1, None),
    ] {
        let expected = root.map_or(SqlValue::Null, text);
        assert_eq!(thread_of(message), expected, "{message}");
    }
    assert!(
        threads.iter().all(|row| row[2] == SqlValue::Null),
        "no corrects backfill"
    );
    let row = |thread: &str, agent: &str, via: &str, at: &str| {
        vec![text(thread), text(agent), text(via), int(0), text(at)]
    };
    let mut expected = vec![
        row(V5_DM_1, "agent-a", "root", "2026-10-19T01:00:02.000Z"),
        row(V5_DM_1, "agent-b", "opener", "2026-10-19T01:00:02.000Z"),
        row(R1, "agent-a", "root", "2026-10-20T01:00:08.000Z"),
        row(R1, "agent-b", "opener", "2026-10-20T01:00:08.000Z"),
        row(R1, "agent-c", "poster", "2026-10-20T01:00:10.000Z"),
        row(R2, "agent-a", "opener", "2026-10-20T01:00:12.000Z"),
        row(R2, "agent-b", "root", "2026-10-20T01:00:12.000Z"),
        row(R2, "agent-c", "mentioned", "2026-10-20T01:00:12.000Z"),
        // agent-z's own root and reply register nobody but agent-a.
        row(V5_ALL_Z, "agent-a", "opener", "2026-10-20T01:00:14.000Z"),
    ];
    expected.sort_by(|left, right| {
        let key = |row: &Vec<SqlValue>| format!("{:?}{:?}{:?}", row[0], row[4], row[1]);
        key(left).cmp(&key(right))
    });
    assert_eq!(participants, expected);
    assert!(
        post_check.is_empty(),
        "the post check holds: {post_check:?}"
    );
    assert_eq!(violations, 0, "PRAGMA foreign_key_check is empty");
    let numbers: Vec<u32> = versions.iter().map(|(version, _)| *version).collect();
    assert_eq!(numbers, (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>());

    // Reopening is a no-op.
    let server = RunningServer::start(&config).await;
    server.shutdown().await.unwrap();
    let (again, participants_again, versions_again) = fx
        .read_db(|conn| {
            Ok((
                v6_snapshot(conn)?,
                select_all(conn, "SELECT COUNT(*) FROM thread_participants")?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(again, before);
    assert_eq!(participants_again, [vec![int(9)]]);
    assert_eq!(versions_again, versions, "nothing re-applied");
}

#[tokio::test]
async fn a_v7_post_check_violation_rolls_back_and_refuses_to_start() {
    let fx = Fixture::new();
    // An old reply into another room breaks the thread invariant.
    write_v6_database(
        &fx,
        "UPDATE sequences SET value = 17 WHERE name = 'event_seq';
         INSERT INTO messages (message_id, event_seq, room_id, sender_id, body, reply_to, created_at, wake)
         VALUES ('msg-01M70000000000000000000HX1', 17, 'dm:agent-a:agent-b', 'agent-a', 'x',
                 'msg-01M700000000000000000007R1', '2026-10-20T03:00:00.000Z', 1);",
    )
    .await;

    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let (refused, version, has_participants, has_thread_column) = store
        .run(|conn| {
            let refused = migrations::apply(conn).is_err();
            let version = migrations::current_version(conn)?;
            let has_participants: i64 = conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'thread_participants'",
                [],
                |row| row.get(0),
            )?;
            let has_thread_column: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'thread_id'",
                [],
                |row| row.get(0),
            )?;
            Ok((refused, version, has_participants, has_thread_column))
        })
        .await
        .unwrap();
    assert_eq!(
        (refused, version, has_participants, has_thread_column),
        (true, 6, 0, 0),
        "rolled back to v6"
    );
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();

    assert!(
        platformd::start(&fx.load_config()).await.is_err(),
        "startup is refused"
    );
}

// ---- v8 (S16): reactions and the kind registry ------------------------------------------

/// The seven seeded kinds `(slug, glyph, label, sort, enabled)` in order.
fn seeded_kinds() -> Vec<Vec<SqlValue>> {
    [
        ("ack", "👌", "확인했다", 10),
        ("thumbsup", "👍", "좋다", 20),
        ("heart", "❤️", "고마워", 30),
        ("eyes", "👀", "보고 있다", 40),
        ("question", "❓", "확인 필요", 50),
        ("done", "☑️", "처리 끝", 60),
        ("claim", "🙋", "맡음", 70),
    ]
    .into_iter()
    .map(|(slug, glyph, label, sort)| vec![text(slug), text(glyph), text(label), int(sort), int(1)])
    .collect()
}

/// Every v7 column and row this test compares: the v6 rows, the thread
/// columns, the participants, every inbox row (S13 columns included) and
/// the v7 migration row.
fn v7_snapshot(conn: &rusqlite::Connection) -> Result<Rows, StoreError> {
    let mut rows = v6_snapshot(conn)?;
    for (table, sql) in [
        (
            "messages v7",
            "SELECT message_id, thread_id, corrects FROM messages ORDER BY event_seq",
        ),
        (
            "thread_participants",
            "SELECT thread_id, agent_id, joined_via, muted, joined_at FROM thread_participants
             ORDER BY thread_id, agent_id",
        ),
        (
            "inbox_rows v7",
            "SELECT agent_id, inbox_seq, message_id, kind, wake, woke FROM inbox_rows
             ORDER BY agent_id, inbox_seq",
        ),
        (
            "schema_migrations v7",
            "SELECT version, applied_at FROM schema_migrations WHERE version = 7",
        ),
    ] {
        rows.push((table, select_all(conn, sql)?));
    }
    Ok(rows)
}

/// Builds a v7 database (the v6 fixture migrated, with its old replies
/// bound into threads) and returns its rows.
async fn write_v7_database(fx: &Fixture) -> Rows {
    write_v6_database(fx, "").await;
    let (store, thread) = Store::open(&fx.db_path()).unwrap();
    let version = store
        .run(|conn| migrations::apply_through(conn, 7))
        .await
        .unwrap();
    assert_eq!(version, 7);
    let snapshot = store.run(|conn| v7_snapshot(conn)).await.unwrap();
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.unwrap();
    snapshot
}

#[tokio::test]
async fn an_empty_database_starts_at_v8_with_the_seeded_registry() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], 8);
    assert_eq!(LATEST_SCHEMA_VERSION, 8);
    server.shutdown().await.unwrap();

    let (kinds, change_seq, tables, inbox_column) = fx
        .read_db(|conn| {
            Ok((
                select_all(
                    conn,
                    "SELECT slug, glyph, label, sort, enabled FROM reaction_kinds ORDER BY sort",
                )?,
                select_all(
                    conn,
                    "SELECT value FROM sequences WHERE name = 'change_seq'",
                )?,
                select_all(
                    conn,
                    "SELECT name FROM sqlite_master WHERE type = 'table'
                     AND name IN ('reaction_kinds', 'reaction_events', 'reactions') ORDER BY name",
                )?,
                select_all(
                    conn,
                    "SELECT name FROM pragma_table_info('inbox_rows') WHERE name = 'change_seq'",
                )?,
            ))
        })
        .await;
    assert_eq!(kinds, seeded_kinds());
    assert_eq!(change_seq, [vec![int(0)]]);
    assert_eq!(
        tables,
        ["reaction_events", "reaction_kinds", "reactions"].map(|name| vec![text(name)])
    );
    assert_eq!(inbox_column, [vec![text("change_seq")]]);
}

#[tokio::test]
async fn a_v7_database_gains_the_registry_and_keeps_every_row() {
    let fx = Fixture::new();
    let before = write_v7_database(&fx).await;
    let config = fx.load_config();
    let server = RunningServer::start(&config).await;
    let health: Value = reqwest::get(server.url("/healthz"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["schema_version"], 8);
    // Old messages read with an empty summary.
    let history = server
        .api()
        .get("/v1/messages?room_id=all", OWNER_TOKEN)
        .await;
    assert_eq!(history.status.as_u16(), 200, "{}", history.body);
    let items = history.body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|item| item["reactions"] == json!([])));
    server.shutdown().await.unwrap();

    let (after, kinds, created, change_seq, inbox_change_seqs, counts, violations, versions) = fx
        .read_db(|conn| {
            Ok((
                v7_snapshot(conn)?,
                select_all(
                    conn,
                    "SELECT slug, glyph, label, sort, enabled FROM reaction_kinds ORDER BY sort",
                )?,
                select_all(
                    conn,
                    "SELECT COUNT(DISTINCT created_at) FROM reaction_kinds
                     WHERE created_at GLOB '[0-9][0-9][0-9][0-9]-*T*Z'",
                )?,
                select_all(conn, "SELECT value FROM sequences WHERE name = 'change_seq'")?,
                select_all(
                    conn,
                    "SELECT COUNT(*), COUNT(change_seq) FROM inbox_rows",
                )?,
                select_all(
                    conn,
                    "SELECT (SELECT COUNT(*) FROM reactions), (SELECT COUNT(*) FROM reaction_events)",
                )?,
                foreign_key_violations(conn)?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(after, before, "every v7 row and value is unchanged");
    assert_eq!(kinds, seeded_kinds());
    assert_eq!(created, [vec![int(1)]], "seeded at the migration time");
    assert_eq!(change_seq, [vec![int(0)]]);
    let SqlValue::Integer(inbox_rows) = inbox_change_seqs[0][0] else {
        panic!("{inbox_change_seqs:?}");
    };
    assert!(inbox_rows > 0, "the fixture has inbox rows");
    assert_eq!(
        inbox_change_seqs[0][1],
        int(0),
        "old rows have NULL change_seq"
    );
    assert_eq!(counts, [vec![int(0), int(0)]]);
    assert_eq!(violations, 0, "PRAGMA foreign_key_check is empty");
    let numbers: Vec<u32> = versions.iter().map(|(version, _)| *version).collect();
    assert_eq!(numbers, (1..=8).collect::<Vec<u32>>());

    // Reopening is a no-op.
    let server = RunningServer::start(&config).await;
    server.shutdown().await.unwrap();
    let (again, kinds_again, versions_again) = fx
        .read_db(|conn| {
            Ok((
                v7_snapshot(conn)?,
                select_all(conn, "SELECT COUNT(*) FROM reaction_kinds")?,
                applied_migrations(conn)?,
            ))
        })
        .await;
    assert_eq!(again, before);
    assert_eq!(kinds_again, [vec![int(7)]]);
    assert_eq!(versions_again, versions, "nothing re-applied");
}
