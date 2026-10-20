mod common;

use std::time::Duration;

use common::Fixture;
use platformd::store::migrations::{self, LATEST_SCHEMA_VERSION};
use platformd::store::{Store, StoreError};

const JOIN_LIMIT: Duration = Duration::from_secs(5);

const V1_TABLES: [&str; 9] = [
    "agents",
    "idempotency",
    "inbox_rows",
    "memberships",
    "messages",
    "notification_pending",
    "read_state",
    "schema_migrations",
    "sequences",
];

async fn open_migrated(fx: &Fixture) -> (Store, platformd::store::StoreThread) {
    std::fs::create_dir_all(fx.db_path().parent().unwrap()).unwrap();
    let (store, thread) = Store::open(&fx.db_path()).expect("open");
    let version = store.run(migrations::apply).await.expect("migrate");
    assert_eq!(version, LATEST_SCHEMA_VERSION);
    (store, thread)
}

async fn close(store: Store, thread: platformd::store::StoreThread) {
    drop(store);
    thread.shutdown(JOIN_LIMIT).await.expect("db thread joins");
}

#[tokio::test]
async fn connection_uses_wal_full_and_foreign_keys() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;

    let (journal_mode, synchronous, foreign_keys) = store
        .run(|conn| {
            let journal: String = conn.pragma_query_value(None, "journal_mode", |r| r.get(0))?;
            let sync: i64 = conn.pragma_query_value(None, "synchronous", |r| r.get(0))?;
            let fk: i64 = conn.pragma_query_value(None, "foreign_keys", |r| r.get(0))?;
            Ok((journal, sync, fk))
        })
        .await
        .unwrap();

    assert_eq!(journal_mode, "wal");
    assert_eq!(synchronous, 2, "synchronous=FULL");
    assert_eq!(foreign_keys, 1);
    close(store, thread).await;
}

#[tokio::test]
async fn migrations_are_idempotent_across_reopen() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;
    close(store, thread).await;

    let (store, thread) = open_migrated(&fx).await;
    let rows: Vec<u32> = store
        .run(|conn| {
            let mut stmt =
                conn.prepare("SELECT version FROM schema_migrations ORDER BY version")?;
            let versions = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<u32>, _>>()?;
            Ok(versions)
        })
        .await
        .unwrap();
    assert_eq!(
        rows,
        (1..=LATEST_SCHEMA_VERSION).collect::<Vec<u32>>(),
        "exactly one schema_migrations row per version"
    );
    close(store, thread).await;
}

#[tokio::test]
async fn migrations_create_all_tables() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;

    let tables: Vec<String> = store
        .run(|conn| {
            let mut stmt = conn.prepare(
                "SELECT name FROM sqlite_master WHERE type = 'table' \
                 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )?;
            let names = stmt
                .query_map([], |row| row.get(0))?
                .collect::<Result<Vec<String>, _>>()?;
            Ok(names)
        })
        .await
        .unwrap();

    // v2 adds columns only; v3 adds `agent_avatars` (S11); v5 adds `rooms`
    // and `message_mentions` (S13); v7 adds `thread_participants` (S15);
    // v8 adds the reaction tables (S16).
    let mut expected: Vec<&str> = V1_TABLES.to_vec();
    expected.extend([
        "agent_avatars",
        "rooms",
        "message_mentions",
        "thread_participants",
        "reaction_kinds",
        "reaction_events",
        "reactions",
    ]);
    expected.sort_unstable();
    assert_eq!(tables, expected);
    close(store, thread).await;
}

#[tokio::test]
async fn v2_adds_nullable_profile_columns_to_agents() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;

    let columns: Vec<(String, String, bool)> = store
        .run(|conn| {
            let mut stmt =
                conn.prepare("SELECT name, type, \"notnull\" FROM pragma_table_info('agents')")?;
            let rows = stmt
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .await
        .unwrap();

    // v5 appends `wake_policy` after the six v2 columns (S13), v6
    // `last_seen_at` after it (S14).
    let profile: Vec<&(String, String, bool)> = columns.iter().skip(7).take(6).collect();
    let appended: Vec<&str> = columns
        .iter()
        .skip(13)
        .map(|(name, _, _)| name.as_str())
        .collect();
    assert_eq!(appended, ["wake_policy", "last_seen_at"]);
    let expected = [
        "name",
        "gender_form",
        "age_band",
        "role",
        "intro",
        "profile_updated_at",
    ];
    assert_eq!(profile.len(), expected.len(), "{columns:?}");
    for ((name, kind, not_null), want) in profile.into_iter().zip(expected) {
        assert_eq!(name, want);
        assert_eq!(kind, "TEXT");
        assert!(!not_null, "{name} is nullable");
    }
    close(store, thread).await;
}

#[tokio::test]
async fn refuses_a_database_from_a_newer_binary() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;
    store
        .run(|conn| {
            conn.execute(
                "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, 'x')",
                [LATEST_SCHEMA_VERSION + 1],
            )?;
            Ok(())
        })
        .await
        .unwrap();

    let result = store.run(migrations::apply).await;

    assert!(
        matches!(result, Err(StoreError::SchemaTooNew { .. })),
        "{result:?}"
    );
    close(store, thread).await;
}

#[tokio::test]
async fn transaction_commits_on_ok_and_rolls_back_on_error() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;

    store
        .transaction(|tx| {
            tx.execute(
                "UPDATE sequences SET value = 5 WHERE name = 'event_seq'",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    let failed: Result<(), StoreError> = store
        .transaction(|tx| {
            tx.execute(
                "UPDATE sequences SET value = 9 WHERE name = 'event_seq'",
                [],
            )?;
            Err(StoreError::Closed)
        })
        .await;
    assert!(failed.is_err());

    let value: i64 = store
        .run(|conn| {
            Ok(conn.query_row(
                "SELECT value FROM sequences WHERE name = 'event_seq'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(value, 5, "the failed transaction was rolled back");
    close(store, thread).await;
}

#[tokio::test]
async fn queued_commands_finish_before_the_thread_stops() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;

    let pending: Vec<_> = (0..32)
        .map(|_| {
            let store = store.clone();
            tokio::spawn(async move {
                store
                    .transaction(|tx| {
                        tx.execute(
                            "UPDATE sequences SET value = value + 1 WHERE name = 'event_seq'",
                            [],
                        )?;
                        Ok(())
                    })
                    .await
            })
        })
        .collect();
    for task in pending {
        task.await.unwrap().unwrap();
    }
    close(store, thread).await;

    let (store, thread) = open_migrated(&fx).await;
    let value: i64 = store
        .run(|conn| {
            Ok(conn.query_row(
                "SELECT value FROM sequences WHERE name = 'event_seq'",
                [],
                |row| row.get(0),
            )?)
        })
        .await
        .unwrap();
    assert_eq!(value, 32);
    close(store, thread).await;
}

#[tokio::test]
async fn shutdown_is_bounded_while_a_store_handle_is_alive() {
    let fx = Fixture::new();
    let (store, thread) = open_migrated(&fx).await;

    let result = thread.shutdown(Duration::from_millis(50)).await;

    assert!(
        matches!(result, Err(StoreError::ShutdownTimeout(_))),
        "{result:?}"
    );
    // The thread keeps serving the live handle.
    assert!(store.run(|_| Ok(())).await.is_ok());
}
