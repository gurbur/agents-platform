//! Online snapshot of the database file (`platformd backup`).
//!
//! The snapshot is taken through a second, read-only connection with the
//! SQLite online backup API, so it can run while the service is writing: in
//! WAL mode the copy reads one consistent snapshot without blocking writers.
//! The result is a self-contained rollback-journal database file (no `-wal`
//! sidecar) that is checked with `PRAGMA integrity_check` before success is
//! reported.

use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

use super::migrations;

/// `sqlite3_backup_step` page count meaning "copy every remaining page".
/// One step holds one read transaction on the source, so concurrent writes
/// from the service cannot force the copy to restart.
const ALL_PAGES: std::ffi::c_int = -1;

/// How long the source connection waits for a lock before reporting busy.
const BUSY_TIMEOUT: Duration = Duration::from_millis(5000);

/// Retries of a step that reported `BUSY`/`LOCKED`, and the pause between them.
const STEP_RETRIES: u32 = 20;
const STEP_PAUSE: Duration = Duration::from_millis(250);

/// At most this many `integrity_check` rows are kept in an error.
const INTEGRITY_ROWS_REPORTED: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("output file {0} already exists; refusing to overwrite it")]
    OutputExists(PathBuf),
    #[error("cannot create output file {path}: {source}")]
    CreateOutput { path: PathBuf, source: io::Error },
    #[error("cannot open database {path} read-only: {source}")]
    OpenSource {
        path: PathBuf,
        source: rusqlite::Error,
    },
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("the database stayed busy for {STEP_RETRIES} attempts; try again")]
    Busy,
    #[error("integrity_check on the snapshot failed: {}", .0.join("; "))]
    Integrity(Vec<String>),
    #[error("cannot read the snapshot size: {0}")]
    Metadata(io::Error),
    #[error("{0}")]
    Schema(#[from] super::StoreError),
}

/// What a finished snapshot contains.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub path: PathBuf,
    pub bytes: u64,
    pub schema_version: u32,
    /// Every user table and its row count, ordered by table name.
    pub row_counts: Vec<(String, i64)>,
}

/// Copies `db_path` into the new file `out` and verifies the copy.
///
/// Refuses an existing `out`. On any failure the partially written `out`
/// is removed, so a failed run never leaves a file that looks like a backup.
pub fn snapshot(db_path: &Path, out: &Path) -> Result<Snapshot, BackupError> {
    create_output(out)?;
    let result = copy_and_verify(db_path, out);
    if result.is_err() {
        // Best effort: the error being returned matters more than this one.
        let _ = std::fs::remove_file(out);
    }
    result
}

fn copy_and_verify(db_path: &Path, out: &Path) -> Result<Snapshot, BackupError> {
    let source = open_source(db_path)?;
    {
        let mut destination = Connection::open(out)?;
        copy_all_pages(&source, &mut destination)?;
        // The pages carry the source's WAL flag; make the file self-contained.
        destination.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))?;
        destination.close().map_err(|(_, err)| err)?;
    }
    drop(source);
    verify(out)
}

/// Creates `out` exclusively. The snapshot holds receiver secrets in plain
/// text (`IF-AUTH-001`), so on Unix only the owner may read it.
fn create_output(out: &Path) -> Result<(), BackupError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(out) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            Err(BackupError::OutputExists(out.to_owned()))
        }
        Err(source) => Err(BackupError::CreateOutput {
            path: out.to_owned(),
            source,
        }),
    }
}

/// Opens the live database without the ability to write to it or create it.
fn open_source(db_path: &Path) -> Result<Connection, BackupError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn =
        Connection::open_with_flags(db_path, flags).map_err(|source| BackupError::OpenSource {
            path: db_path.to_owned(),
            source,
        })?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    Ok(conn)
}

fn copy_all_pages(source: &Connection, destination: &mut Connection) -> Result<(), BackupError> {
    let backup = Backup::new(source, destination)?;
    let mut busy_steps = 0;
    loop {
        match backup.step(ALL_PAGES)? {
            StepResult::Done => return Ok(()),
            StepResult::More => {}
            // `Busy`, `Locked`, and any result a later rusqlite adds: wait and retry.
            _ => {
                busy_steps += 1;
                if busy_steps >= STEP_RETRIES {
                    return Err(BackupError::Busy);
                }
                thread::sleep(STEP_PAUSE);
            }
        }
    }
}

/// Reopens the finished file read-only, runs `integrity_check` and reads
/// what the summary reports.
fn verify(out: &Path) -> Result<Snapshot, BackupError> {
    let conn = Connection::open_with_flags(out, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let problems = integrity_problems(&conn)?;
    if !problems.is_empty() {
        return Err(BackupError::Integrity(problems));
    }
    let schema_version = migrations::current_version(&conn)?;
    let row_counts = row_counts(&conn)?;
    drop(conn);
    let bytes = std::fs::metadata(out).map_err(BackupError::Metadata)?.len();
    Ok(Snapshot {
        path: out.to_owned(),
        bytes,
        schema_version,
        row_counts,
    })
}

/// Rows of `PRAGMA integrity_check` other than the single `ok`.
fn integrity_problems(conn: &Connection) -> Result<Vec<String>, rusqlite::Error> {
    let mut statement = conn.prepare("PRAGMA integrity_check")?;
    let rows: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if rows.len() == 1 && rows[0] == "ok" {
        return Ok(Vec::new());
    }
    Ok(rows.into_iter().take(INTEGRITY_ROWS_REPORTED).collect())
}

fn row_counts(conn: &Connection) -> Result<Vec<(String, i64)>, rusqlite::Error> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    tables
        .into_iter()
        .map(|table| {
            let sql = format!("SELECT COUNT(*) FROM {}", quote_identifier(&table));
            let count: i64 = conn.query_row(&sql, [], |row| row.get(0))?;
            Ok((table, count))
        })
        .collect()
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_db(dir: &Path) -> PathBuf {
        let path = dir.join("platform.db");
        let mut conn = migrations::open_connection(&path).unwrap();
        migrations::apply(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO agents (agent_id, api_key_hash, receiver_secret, created_at) \
             VALUES ('agent-a', 'h', 's', 't')",
            [],
        )
        .unwrap();
        path
    }

    #[test]
    fn snapshot_is_self_contained_and_counts_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = live_db(dir.path());
        let out = dir.path().join("snapshot.db");

        let snapshot = snapshot(&db, &out).unwrap();

        assert_eq!(snapshot.schema_version, migrations::LATEST_SCHEMA_VERSION);
        assert!(snapshot.row_counts.contains(&("agents".to_owned(), 1)));
        assert!(snapshot.bytes > 0);
        assert!(!dir.path().join("snapshot.db-wal").exists());
        let conn = Connection::open(&out).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "delete");
    }

    #[test]
    fn refuses_an_existing_output_and_leaves_it_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let db = live_db(dir.path());
        let out = dir.path().join("snapshot.db");
        std::fs::write(&out, b"keep me").unwrap();

        let err = snapshot(&db, &out).unwrap_err();

        assert!(matches!(err, BackupError::OutputExists(_)), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), b"keep me");
    }

    #[test]
    fn missing_source_is_not_created_and_leaves_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("absent.db");
        let out = dir.path().join("snapshot.db");

        let err = snapshot(&db, &out).unwrap_err();

        assert!(matches!(err, BackupError::OpenSource { .. }), "{err}");
        assert!(!db.exists());
        assert!(!out.exists());
    }

    #[cfg(unix)]
    #[test]
    fn output_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = live_db(dir.path());
        let out = dir.path().join("snapshot.db");

        snapshot(&db, &out).unwrap();

        let mode = std::fs::metadata(&out).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "mode {mode:o}");
    }
}
