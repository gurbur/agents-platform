//! Embeds the git short hash for `platformd --version` (`IF-OPS-001`).
//!
//! Precedence: `PLATFORMD_GIT_SHA` from the environment, then
//! `git rev-parse --short HEAD`, then `unknown`.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=PLATFORMD_GIT_SHA");
    watch_git_head();

    let sha = std::env::var("PLATFORMD_GIT_SHA")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .or_else(git_short_sha)
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=PLATFORMD_GIT_SHA={sha}");
}

fn git_short_sha() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    if sha.is_empty() { None } else { Some(sha) }
}

/// Re-run when HEAD moves. Only existing paths are registered: cargo treats a
/// missing `rerun-if-changed` path as always dirty.
fn watch_git_head() {
    let git_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.git");
    let head = git_dir.join("HEAD");
    if !head.is_file() {
        return;
    }
    println!("cargo:rerun-if-changed={}", head.display());
    let Ok(contents) = std::fs::read_to_string(&head) else {
        return;
    };
    let Some(reference) = contents.trim().strip_prefix("ref: ") else {
        return;
    };
    let ref_path = git_dir.join(reference);
    if ref_path.is_file() {
        println!("cargo:rerun-if-changed={}", ref_path.display());
    }
}
