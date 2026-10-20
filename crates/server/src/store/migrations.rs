//! Connection PRAGMAs and schema migrations (`IF-DB-001`).

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

use super::StoreError;
use crate::clock;

/// Latest schema version this binary knows. `/healthz` reports it.
pub const LATEST_SCHEMA_VERSION: u32 = 8;

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
    post_check: Option<PostCheck>,
}

/// A post-migration invariant: `sql` returns the first offending id (one
/// text column) or no row; `broken` says what such an id violates.
struct PostCheck {
    sql: &'static str,
    broken: &'static str,
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
        post_check: Some(PostCheck {
            sql: V5_POST_CHECK,
            broken: "has no rooms row",
        }),
    },
    Migration {
        version: 6,
        sql: V6_SCHEMA,
        rebuilds_tables: false,
        post_check: None,
    },
    Migration {
        version: 7,
        sql: V7_SCHEMA,
        rebuilds_tables: false,
        post_check: Some(PostCheck {
            sql: V7_POST_CHECK,
            broken: "breaks the thread invariant (its root is in a thread or in another room, \
                     or it is a reply outside any thread)",
        }),
    },
    Migration {
        version: 8,
        sql: V8_SCHEMA,
        rebuilds_tables: false,
        post_check: None,
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
/// on messages, inbox rows and agents (ADR-0010, ADR-0011). Every v4 inbox
/// row woke its recipient, so the backfill sets `woke = 1`. One transaction,
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
ALTER TABLE inbox_rows ADD COLUMN woke INTEGER NOT NULL DEFAULT 0;
UPDATE inbox_rows SET woke = 1;
ALTER TABLE agents ADD COLUMN wake_policy TEXT NOT NULL DEFAULT 'sender';
";

/// No message or membership may name a room without a `rooms` row.
const V5_POST_CHECK: &str = "
SELECT room_id FROM messages WHERE room_id NOT IN (SELECT room_id FROM rooms)
UNION ALL
SELECT room_id FROM memberships WHERE room_id NOT IN (SELECT room_id FROM rooms)
LIMIT 1";

/// v6 (S14): when each agent's key last authenticated a request
/// (ADR-0012), written at most once per throttle interval. Every v5 row
/// stays with `NULL` (never seen); no backfill. Presence itself lives in
/// memory, not here.
const V6_SCHEMA: &str = "
ALTER TABLE agents ADD COLUMN last_seen_at TEXT;
";

/// v7 (S15): threads and corrections (ADR-0013). Two message columns,
/// the thread index and the participant table, then the backfill: every
/// old `reply_to` chain is walked down from its root so each reply points
/// at the topmost message (no nesting), and the participants of each
/// thread are inserted in the order root, opener/poster, mentioned —
/// `INSERT OR IGNORE`, registered agents only, so the first row an agent
/// gets stays (its `joined_via` is approximate, ADR-0013). One
/// transaction, no table rebuild; every v6 row and value stays.
const V7_SCHEMA: &str = "
ALTER TABLE messages ADD COLUMN thread_id TEXT REFERENCES messages(message_id);
ALTER TABLE messages ADD COLUMN corrects TEXT REFERENCES messages(message_id);
CREATE INDEX messages_thread_event ON messages(thread_id, event_seq);
CREATE TABLE thread_participants (
    thread_id TEXT NOT NULL REFERENCES messages(message_id),
    agent_id TEXT NOT NULL REFERENCES agents(agent_id),
    joined_via TEXT NOT NULL,
    muted INTEGER NOT NULL DEFAULT 0,
    joined_at TEXT NOT NULL,
    PRIMARY KEY (thread_id, agent_id)
);
WITH RECURSIVE chain(message_id, root_id) AS (
  SELECT message_id, message_id FROM messages WHERE reply_to IS NULL
  UNION ALL
  SELECT m.message_id, chain.root_id FROM messages m JOIN chain ON m.reply_to = chain.message_id
)
UPDATE messages SET thread_id = (SELECT root_id FROM chain WHERE chain.message_id = messages.message_id)
WHERE reply_to IS NOT NULL;
INSERT OR IGNORE INTO thread_participants (thread_id, agent_id, joined_via, muted, joined_at)
  SELECT r.message_id, r.sender_id, 'root', 0,
         (SELECT created_at FROM messages t WHERE t.thread_id = r.message_id ORDER BY t.event_seq LIMIT 1)
  FROM messages r JOIN agents a ON a.agent_id = r.sender_id
  WHERE EXISTS (SELECT 1 FROM messages t WHERE t.thread_id = r.message_id);
INSERT OR IGNORE INTO thread_participants (thread_id, agent_id, joined_via, muted, joined_at)
  SELECT t.thread_id, t.sender_id,
         CASE WHEN t.event_seq = (SELECT MIN(event_seq) FROM messages WHERE thread_id = t.thread_id)
              THEN 'opener' ELSE 'poster' END,
         0, t.created_at
  FROM messages t JOIN agents a ON a.agent_id = t.sender_id
  WHERE t.thread_id IS NOT NULL ORDER BY t.event_seq;
INSERT OR IGNORE INTO thread_participants (thread_id, agent_id, joined_via, muted, joined_at)
  SELECT t.thread_id, mm.agent_id, 'mentioned', 0, t.created_at
  FROM messages t JOIN message_mentions mm ON mm.message_id = t.message_id
                  JOIN agents a ON a.agent_id = mm.agent_id
  WHERE t.thread_id IS NOT NULL ORDER BY t.event_seq;
";

/// The thread invariant: a thread's root is outside any thread and in the
/// same room, and every reply is in a thread.
const V7_POST_CHECK: &str = "
SELECT t.message_id FROM messages t JOIN messages r ON r.message_id = t.thread_id
WHERE r.thread_id IS NOT NULL OR r.room_id <> t.room_id
UNION ALL
SELECT message_id FROM messages WHERE reply_to IS NOT NULL AND thread_id IS NULL
LIMIT 1";

/// v8 (S16): reactions and the kind registry (ADR-0014). The registry with
/// its seven seeded kinds (`created_at` is the migration time), the
/// `change_seq` sequence, the append-only event log, the current-state
/// table with tombstones, and the change an inbox reaction event points
/// at. One transaction, no table rebuild; every v7 row stays (old inbox
/// rows get `NULL` `change_seq`).
const V8_SCHEMA: &str = "
CREATE TABLE reaction_kinds (
    slug TEXT PRIMARY KEY,
    glyph TEXT NOT NULL UNIQUE,
    label TEXT NOT NULL,
    sort INTEGER NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);
INSERT INTO reaction_kinds (slug, glyph, label, sort, enabled, created_at) VALUES
  ('ack',      '👌', '확인했다',  10, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  ('thumbsup', '👍', '좋다',      20, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  ('heart',    '❤️', '고마워',    30, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  ('eyes',     '👀', '보고 있다', 40, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  ('question', '❓', '확인 필요', 50, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  ('done',     '☑️', '처리 끝',   60, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
  ('claim',    '🙋', '맡음',      70, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
INSERT INTO sequences (name, value) VALUES ('change_seq', 0);
CREATE TABLE reaction_events (
    change_seq INTEGER PRIMARY KEY,
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    agent_id TEXT NOT NULL,
    kind TEXT NOT NULL REFERENCES reaction_kinds(slug),
    removed INTEGER NOT NULL,
    wake INTEGER NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX reaction_events_message ON reaction_events(message_id, change_seq);
CREATE TABLE reactions (
    message_id TEXT NOT NULL REFERENCES messages(message_id),
    agent_id TEXT NOT NULL,
    kind TEXT NOT NULL REFERENCES reaction_kinds(slug),
    created_at TEXT NOT NULL,
    removed INTEGER NOT NULL DEFAULT 0,
    change_seq INTEGER NOT NULL UNIQUE REFERENCES reaction_events(change_seq),
    PRIMARY KEY (message_id, agent_id, kind)
);
ALTER TABLE inbox_rows ADD COLUMN change_seq INTEGER REFERENCES reaction_events(change_seq);
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
    if let Some(check) = &migration.post_check {
        let offending: Option<String> = tx.query_row(check.sql, [], |row| row.get(0)).optional()?;
        if let Some(id) = offending {
            // Dropping `tx` rolls the version back.
            return Err(StoreError::PostCheck {
                version: migration.version,
                detail: format!("'{id}' {}", check.broken),
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
