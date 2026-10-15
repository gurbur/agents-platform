//! Tests that run the real `platformd` binary.

mod common;

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use common::Fixture;

const BIN: &str = env!("CARGO_BIN_EXE_platformd");
const CONFIG_ERROR_EXIT: i32 = 2;

fn run_with_config(config: &std::path::Path) -> Output {
    Command::new(BIN)
        .arg("--config")
        .arg(config)
        .env_remove("PLATFORMD_CONFIG")
        .output()
        .expect("spawn platformd")
}

fn assert_config_error(output: &Output) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(CONFIG_ERROR_EXIT),
        "stderr: {stderr}"
    );
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "one human-readable line: {stderr:?}");
    assert!(lines[0].starts_with("platformd: "), "{stderr:?}");
}

#[test]
fn unknown_config_key_exits_with_2() {
    let fx = Fixture::new();
    let config = fx.write_config_text(
        "db_path = \"data/platform.db\"\nowner_token_file = \"owner.token\"\nport = 1\n",
    );
    let output = run_with_config(&config);
    assert_config_error(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("port"));
}

#[test]
fn short_owner_token_exits_with_2_without_echoing_it() {
    let fx = Fixture::new();
    std::fs::write(fx.path().join("owner.token"), "too-short\n").unwrap();
    let config = fx.write_config("127.0.0.1:0");
    let output = run_with_config(&config);
    assert_config_error(&output);
    assert!(!String::from_utf8_lossy(&output.stderr).contains("too-short"));
}

#[test]
fn missing_config_file_exits_with_2() {
    let fx = Fixture::new();
    let output = run_with_config(&fx.path().join("absent.toml"));
    assert_config_error(&output);
}

#[test]
fn missing_token_file_exits_with_2() {
    let fx = Fixture::new();
    std::fs::remove_file(fx.path().join("owner.token")).unwrap();
    let config = fx.write_config("127.0.0.1:0");
    let output = run_with_config(&config);
    assert_config_error(&output);
}

#[test]
fn config_path_can_come_from_the_environment() {
    let fx = Fixture::new();
    let config = fx.write_config_text("bogus = true\n");
    let output = Command::new(BIN)
        .env("PLATFORMD_CONFIG", &config)
        .output()
        .expect("spawn platformd");
    assert_config_error(&output);
    assert!(String::from_utf8_lossy(&output.stderr).contains("bogus"));
}

#[test]
fn version_prints_one_line_with_package_version_and_sha() {
    let output = Command::new(BIN).arg("--version").output().unwrap();
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout:?}");
    let expected_prefix = format!("platformd {} (", env!("CARGO_PKG_VERSION"));
    assert!(lines[0].starts_with(&expected_prefix), "{stdout:?}");
    assert!(lines[0].ends_with(')'), "{stdout:?}");
}

#[cfg(unix)]
#[test]
fn sigterm_shuts_down_cleanly() {
    let fx = Fixture::new();
    let port = free_port();
    let config = fx.write_config(&format!("127.0.0.1:{port}"));
    let child = Command::new(BIN)
        .arg("--config")
        .arg(&config)
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    wait_until_listening(port, Duration::from_secs(10));
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());

    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("shutdown complete"), "{stderr}");
    assert!(
        !stderr.contains(common::OWNER_TOKEN),
        "owner token must never be logged"
    );
}

fn free_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

fn wait_until_listening(port: u16, limit: Duration) {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("platformd did not start listening on {port}");
}
