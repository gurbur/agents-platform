//! Connection PRAGMAs and schema migrations (`IF-DB-001`).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::StoreError;
use crate::clock;

/// Latest schema version this binary knows. `/healthz` reports it.
pub const LATEST_SCHEMA_VERSION: u32 = 3;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// `PRAGMA synchronous` value for FULL.
const SYNCHRONOUS_FULL: i64 = 2;

/// Versions start at 1 and are contiguous; each is applied in one transaction.
const MIGRATIONS: &[(u32, &str)] = &[(1, V1_SCHEMA), (2, V2_SCHEMA), (3, V3_SCHEMA)];

const BOOTSTRAP: &str = "CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);";

const V1_SCHEMA: &str = "
CREATE TABLE agents (
    agent_id TEXT PRIMARY KEY,
    api_key_hash TEXT NOT NULL UNIQUE,
    receiver_secret TEXT NOT NULL,
    receiver_url TEXT,
    route_version INTEGER NOT NULL DEFAULT 1,
    inbox_head INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE memberships (
    room_id TEXT NOT NULL,
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    joined INTEGER NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 1,
    effective_event_seq INTEGER NOT NULL,
    PRIMARY KEY (room_id, agent_id)
);

CREATE TABLE sequences (
    name TEXT PRIMARY KEY,
    value INTEGER NOT NULL
);
INSERT INTO sequences (name, value) VALUES ('event_seq', 0);

CREATE TABLE messages (
    message_id TEXT PRIMARY KEY,
    event_seq INTEGER NOT NULL UNIQUE,
    room_id TEXT NOT NULL,
    sender_id TEXT NOT NULL REFERENCES agents(agent_id),
    body TEXT NOT NULL,
    reply_to TEXT REFERENCES messages(message_id),
    created_at TEXT NOT NULL
);
CREATE INDEX messages_room_event ON messages(room_id, event_seq);

CREATE TABLE inbox_rows (
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    inbox_seq INTEGER NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    PRIMARY KEY (agent_id, inbox_seq)
);

CREATE TABLE read_state (
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id),
    read_marker INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE idempotency (
    sender_id TEXT NOT NULL REFERENCES agents(agent_id),
    client_key TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    PRIMARY KEY (sender_id, client_key)
);

CREATE TABLE notification_pending (
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id),
    route_version INTEGER NOT NULL,
    state TEXT NOT NULL,
    target_seq INTEGER NOT NULL,
    last_accepted_seq INTEGER,
    attempt INTEGER NOT NULL DEFAULT 0,
    next_due_at TEXT,
    redrive_due_at TEXT,
    last_attempt_at TEXT,
    last_error TEXT
);
";

/// v2 (S9): the directory profile. Nullable columns only, so every v1 row
/// stays as it is with `NULL` in each new column; the owner fills `name` by
/// hand. No CHECK constraints: `platform-wire` is the one validation point.
const V2_SCHEMA: &str = "
ALTER TABLE agents ADD COLUMN name TEXT;
ALTER TABLE agents ADD COLUMN gender_form TEXT;
ALTER TABLE agents ADD COLUMN age_band TEXT;
ALTER TABLE agents ADD COLUMN role TEXT;
ALTER TABLE agents ADD COLUMN intro TEXT;
ALTER TABLE agents ADD COLUMN profile_updated_at TEXT;
";

/// v3 (S11): the directory avatar, one row per agent that has one. A new
/// table only; every v2 table and row stays as it is. No CHECK constraints:
/// `platform-wire::avatar` is the one validation point.
const V3_SCHEMA: &str = "
CREATE TABLE agent_avatars (
    agent_id TEXT PRIMARY KEY REFERENCES agents(agent_id),
    content_type TEXT NOT NULL,
    bytes BLOB NOT NULL,
    sha256 TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
";

/// Opens the database file and applies and verifies the connection PRAGMAs.
/// Refuses to continue unless the journal mode is WAL.
pub fn open_connection(path: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open(path)?;
    configure(&conn)?;
    Ok(conn)
}

fn configure(conn: &Connection) -> Result<(), StoreError> {
    let journal_mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::JournalMode(journal_mode));
    }
    conn.pragma_update(None, "synchronous", "FULL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(BUSY_TIMEOUT)?;

    expect_pragma(conn, "synchronous", SYNCHRONOUS_FULL)?;
    expect_pragma(conn, "foreign_keys", 1)?;
    Ok(())
}

fn expect_pragma(conn: &Connection, name: &'static str, expected: i64) -> Result<(), StoreError> {
    let actual: i64 = conn.pragma_query_value(None, name, |row| row.get(0))?;
    if actual != expected {
        return Err(StoreError::Pragma {
            name,
            expected,
            actual,
        });
    }
    Ok(())
}

/// Applies every pending migration and returns the resulting schema version.
/// Idempotent: an up-to-date database is left unchanged.
pub fn apply(conn: &mut Connection) -> Result<u32, StoreError> {
    apply_through(conn, LATEST_SCHEMA_VERSION)
}

/// Applies pending migrations up to and including `max_version` and returns
/// the resulting schema version. [`apply`] calls it with the latest version;
/// a lower bound builds an older schema, e.g. a v1 database for migration
/// tests. A database newer than this binary is refused either way.
pub fn apply_through(conn: &mut Connection, max_version: u32) -> Result<u32, StoreError> {
    conn.execute_batch(BOOTSTRAP)?;
    let found = current_version(conn)?;
    if found > LATEST_SCHEMA_VERSION {
        return Err(StoreError::SchemaTooNew {
            found,
            supported: LATEST_SCHEMA_VERSION,
        });
    }

    for &(version, sql) in MIGRATIONS
        .iter()
        .filter(|(version, _)| *version <= max_version)
    {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Re-read inside the write lock so a concurrent opener cannot double-apply.
        if current_version(&tx)? >= version {
            continue;
        }
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
            params![version, clock::now_rfc3339_ms()],
        )?;
        tx.commit()?;
        tracing::info!(version, "applied schema migration");
    }

    let version = current_version(conn)?;
    verify_contiguous(conn, version)?;
    Ok(version)
}

/// Highest applied schema version, or 0 for an empty database.
pub fn current_version(conn: &Connection) -> Result<u32, StoreError> {
    let has_table: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if has_table.is_none() {
        return Ok(0);
    }
    let version: u32 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;
    Ok(version)
}

fn verify_contiguous(conn: &Connection, max: u32) -> Result<(), StoreError> {
    let (count, min): (u32, u32) = conn.query_row(
        "SELECT COUNT(*), COALESCE(MIN(version), 0) FROM schema_migrations",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let empty = count == 0 && max == 0;
    if empty || (min == 1 && count == max) {
        return Ok(());
    }
    Err(StoreError::SchemaGap { count, max })
}
