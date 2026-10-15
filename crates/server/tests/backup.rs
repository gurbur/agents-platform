//! `platformd backup` against a server that is writing at the same time.

mod common;

use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use common::{Fixture, RunningServer};

const BIN: &str = env!("CARGO_BIN_EXE_platformd");
const AGENTS: usize = 3;

fn run_backup(config: &Path, out: &Path) -> Output {
    Command::new(BIN)
        .arg("backup")
        .arg("--config")
        .arg(config)
        .arg("--out")
        .arg(out)
        .env_remove("PLATFORMD_CONFIG")
        .output()
        .expect("spawn platformd backup")
}

/// Reads `row_count <table> <rows>` lines from the backup summary.
fn reported_rows(stdout: &str, table: &str) -> Option<i64> {
    stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        if fields.next()? != "row_count" || fields.next()? != table {
            return None;
        }
        fields.next()?.parse().ok()
    })
}

fn count(conn: &rusqlite::Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn backup_while_the_server_writes_matches_and_passes_integrity_check() {
    let fx = Fixture::new();
    let server = RunningServer::start(&fx.load_config()).await;
    let api = server.api();
    let mut keys = Vec::new();
    for index in 0..AGENTS {
        keys.push(api.register(&format!("agent-{index}"), None).await.api_key);
    }

    // Keep the server committing messages for the whole backup.
    let writing = Arc::new(AtomicBool::new(true));
    let written = Arc::new(AtomicUsize::new(0));
    let writer = {
        let writing = writing.clone();
        let written = written.clone();
        let sender = keys[0].clone();
        tokio::spawn(async move {
            let mut index = 0;
            while writing.load(Ordering::Relaxed) {
                api.sent(&sender, "all", "load", &format!("load-{index}"))
                    .await;
                written.fetch_add(1, Ordering::Relaxed);
                index += 1;
            }
        })
    };
    common::eventually(
        std::time::Duration::from_secs(10),
        "the writer commits",
        || async { (written.load(Ordering::Relaxed) > 0).then_some(()) },
    )
    .await;

    // The config file the fixture wrote for `load_config` is reused as is.
    let config = fx.path().join("platformd.toml");
    let out = fx.path().join("snapshot.db");
    let written_before_backup = written.load(Ordering::Relaxed);
    let output = tokio::task::spawn_blocking({
        let config = config.clone();
        let out = out.clone();
        move || run_backup(&config, &out)
    })
    .await
    .unwrap();
    let written_after_backup = written.load(Ordering::Relaxed);
    writing.store(false, Ordering::Relaxed);
    writer.await.unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(stdout.contains("integrity_check: ok"), "{stdout}");
    assert!(
        written_after_backup > written_before_backup,
        "messages were committed while the backup ran"
    );

    let snapshot = rusqlite::Connection::open(&out).unwrap();
    let integrity: String = snapshot
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    let live_counts = fx
        .read_db(|conn| {
            Ok((
                count(conn, "schema_migrations"),
                count(conn, "agents"),
                count(conn, "messages"),
            ))
        })
        .await;
    let (live_migrations, live_agents, live_messages) = live_counts;
    assert_eq!(count(&snapshot, "schema_migrations"), live_migrations);
    assert_eq!(count(&snapshot, "agents"), live_agents);
    assert_eq!(live_agents, AGENTS as i64);
    let snapshot_messages = count(&snapshot, "messages");
    assert!(snapshot_messages <= live_messages);
    // A consistent snapshot: every message's fanout rows came with it.
    let orphan_rows: i64 = snapshot
        .query_row(
            "SELECT COUNT(*) FROM inbox_rows WHERE message_id NOT IN (SELECT message_id FROM messages)",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(orphan_rows, 0);
    assert_eq!(
        count(&snapshot, "inbox_rows"),
        snapshot_messages * AGENTS as i64
    );
    assert_eq!(reported_rows(&stdout, "agents"), Some(live_agents));
    assert_eq!(reported_rows(&stdout, "messages"), Some(snapshot_messages));

    server.shutdown().await.unwrap();
}

#[test]
fn backup_refuses_an_existing_output_file() {
    let fx = Fixture::new();
    let config = fx.write_config("127.0.0.1:0");
    let out = fx.path().join("snapshot.db");
    std::fs::write(&out, b"previous backup").unwrap();

    let output = run_backup(&config, &out);

    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("already exists"));
    assert_eq!(std::fs::read(&out).unwrap(), b"previous backup");
}

#[test]
fn backup_with_an_invalid_config_exits_with_2() {
    let fx = Fixture::new();
    let config = fx.write_config_text("bogus = true\n");
    let output = run_backup(&config, &fx.path().join("snapshot.db"));
    assert_eq!(output.status.code(), Some(2));
}
