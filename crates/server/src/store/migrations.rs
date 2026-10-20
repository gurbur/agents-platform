//! Connection PRAGMAs and schema migrations (`IF-DB-001`).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::StoreError;
use crate::clock;

/// Latest schema version this binary knows. `/healthz` reports it.
pub const LATEST_SCHEMA_VERSION: u32 = 5;

const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// `PRAGMA synchronous` value for FULL.
const SYNCHRONOUS_FULL: i64 = 2;

/// One schema version.
struct Migration {
    version: u32,
    sql: &'static str,
    /// Rebuilds a table that other tables reference: `foreign_keys` is off
    /// around the version's transaction and `PRAGMA foreign_key_check` must
    /// be empty before it commits (SQLite's table-rebuild procedure).
    rebuilds_tables: bool,
    /// A query run in the version's transaction right before it commits:
    /// any row it returns rolls the version back and refuses startup.
    /// `PRAGMA foreign_key_check` must then be empty as well.
    post_check: Option<&'static str>,
}

/// Versions start at 1 and are contiguous; each is applied in one transaction.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: V1_SCHEMA,
        rebuilds_tables: false,
        post_check: None,
    },
    Migration {
        version: 2,
        sql: V2_SCHEMA,
        rebuilds_tables: false,
        post_check: None,
    },
    Migration {
        version: 3,
        sql: V3_SCHEMA,
        rebuilds_tables: false,
        post_check: None,
    },
    Migration {
        version: 4,
        sql: V4_SCHEMA,
        rebuilds_tables: true,
        post_check: None,
    },
    Migration {
        version: 5,
        sql: V5_SCHEMA,
        rebuilds_tables: false,
        post_check: Some(V5_POST_CHECK),
    },
];

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

/// v4 (S12): `messages` without the `sender_id → agents` foreign key, so a
/// deleted agent's messages stay (ADR-0009). Same columns, the `reply_to`
/// foreign key, `UNIQUE(event_seq)` and `messages_room_event`; every row is
/// copied. The new table refers to `messages`, the name it is renamed to.
const V4_SCHEMA: &str = "
CREATE TABLE messages_v4 (
    message_id TEXT PRIMARY KEY,
    event_seq INTEGER NOT NULL UNIQUE,
    room_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    body TEXT NOT NULL,
    reply_to TEXT REFERENCES messages(message_id),
    created_at TEXT NOT NULL
);
INSERT INTO messages_v4 (message_id, event_seq, room_id, sender_id, body, reply_to, created_at)
    SELECT message_id, event_seq, room_id, sender_id, body, reply_to, created_at FROM messages;
DROP TABLE messages;
ALTER TABLE messages_v4 RENAME TO messages;
CREATE INDEX messages_room_event ON messages(room_id, event_seq);
";

/// v5 (S13): rooms as a table with `all` and every DM that has a message,
/// a joined membership for each registered DM member, and event columns
/// on messages, inbox rows and agents (ADR-0010, ADR-0011). One transaction,
/// no table rebuild; every v4 row stays. `agent_id` never contains `:`, so
/// the prefix and suffix comparisons find DM members exactly (`LIKE` would
/// treat `_` as a wildcard).
const V5_SCHEMA: &str = "
CREATE TABLE rooms (
    room_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    title TEXT,
    created_by TEXT NOT NULL,
    created_at TEXT NOT NULL,
    archived_at TEXT
);
INSERT INTO rooms (room_id, kind, title, created_by, created_at, archived_at)
  VALUES ('all', 'all', NULL, 'owner',
          COALESCE((SELECT MIN(created_at) FROM messages WHERE room_id = 'all'),
                   strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
          NULL);
INSERT INTO rooms (room_id, kind, title, created_by, created_at, archived_at)
  SELECT m.room_id, 'dm', NULL, m.sender_id, m.created_at, NULL
  FROM messages m
  WHERE substr(m.room_id, 1, 3) = 'dm:'
    AND m.event_seq = (SELECT MIN(event_seq) FROM messages WHERE room_id = m.room_id);
INSERT INTO memberships (room_id, agent_id, joined, epoch, effective_event_seq)
  SELECT r.room_id, a.agent_id, 1, 1, (SELECT value FROM sequences WHERE name = 'event_seq')
  FROM rooms r JOIN agents a
    ON substr(r.room_id, 1, length(a.agent_id) + 4) = 'dm:' || a.agent_id || ':'
    OR substr(r.room_id, -(length(a.agent_id) + 1)) = ':' || a.agent_id
  WHERE r.kind = 'dm';
CREATE INDEX memberships_agent ON memberships(agent_id);

ALTER TABLE messages ADD COLUMN wake INTEGER NOT NULL DEFAULT 1;
CREATE TABLE message_mentions (
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    agent_id TEXT NOT NULL,
    PRIMARY KEY (message_id, agent_id)
);
ALTER TABLE inbox_rows ADD COLUMN kind TEXT NOT NULL DEFAULT 'message';
ALTER TABLE inbox_rows ADD COLUMN wake INTEGER NOT NULL DEFAULT 1;
ALTER TABLE agents ADD COLUMN wake_policy TEXT NOT NULL DEFAULT 'sender';
";

/// No message or membership may name a room without a `rooms` row.
const V5_POST_CHECK: &str = "
SELECT room_id FROM messages WHERE room_id NOT IN (SELECT room_id FROM rooms)
UNION ALL
SELECT room_id FROM memberships WHERE room_id NOT IN (SELECT room_id FROM rooms)
LIMIT 1";

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

    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= max_version)
    {
        if current_version(conn)? >= migration.version {
            continue;
        }
        if migration.rebuilds_tables {
            apply_with_foreign_keys_off(conn, migration)?;
        } else {
            apply_one(conn, migration)?;
        }
    }

    let version = current_version(conn)?;
    verify_contiguous(conn, version)?;
    Ok(version)
}

/// Applies `migration` in one IMMEDIATE transaction, together with its
/// `schema_migrations` row; nothing when another opener applied it first.
fn apply_one(conn: &mut Connection, migration: &Migration) -> Result<(), StoreError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Re-read inside the write lock so a concurrent opener cannot double-apply.
    if current_version(&tx)? >= migration.version {
        return Ok(());
    }
    tx.execute_batch(migration.sql)?;
    tx.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES (?1, ?2)",
        params![migration.version, clock::now_rfc3339_ms()],
    )?;
    if let Some(check) = migration.post_check {
        let offending: Option<String> = tx.query_row(check, [], |row| row.get(0)).optional()?;
        if let Some(room_id) = offending {
            // Dropping `tx` rolls the version back.
            return Err(StoreError::PostCheck {
                version: migration.version,
                detail: format!("room_id '{room_id}' has no rooms row"),
            });
        }
    }
    if migration.rebuilds_tables || migration.post_check.is_some() {
        let violations = foreign_key_violations(&tx)?;
        if violations > 0 {
            // Dropping `tx` rolls the version back.
            return Err(StoreError::ForeignKeyViolations {
                version: migration.version,
                violations,
            });
        }
    }
    tx.commit()?;
    tracing::info!(version = migration.version, "applied schema migration");
    Ok(())
}

/// SQLite's table-rebuild procedure: `PRAGMA foreign_keys` cannot change
/// inside a transaction, so it is turned off around [`apply_one`] and back
/// on afterwards whether or not the migration succeeded.
fn apply_with_foreign_keys_off(
    conn: &mut Connection,
    migration: &Migration,
) -> Result<(), StoreError> {
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let applied = apply_one(conn, migration);
    let restored = conn
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(StoreError::from)
        .and_then(|()| expect_pragma(conn, "foreign_keys", 1));
    applied?;
    restored
}

/// Rows reported by `PRAGMA foreign_key_check` across every table.
fn foreign_key_violations(conn: &Connection) -> Result<usize, StoreError> {
    let mut statement = conn.prepare("PRAGMA foreign_key_check")?;
    let mut rows = statement.query([])?;
    let mut count = 0;
    while rows.next()?.is_some() {
        count += 1;
    }
    Ok(count)
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
