//! `platformd identity keygen --out <file>` through the real binary
//! (IDENTITY_ATTESTATION §7.1).

use std::path::Path;
use std::process::{Command, Output};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use sha2::{Digest, Sha256};

const BIN: &str = env!("CARGO_BIN_EXE_platformd");

/// Runs keygen under `umask` from an empty directory with no config, so
/// the result depends on neither.
fn keygen_with_umask(umask: &str, dir: &Path, out: &Path) -> Output {
    Command::new("sh")
        .arg("-c")
        .arg(format!(
            "umask {umask} && exec \"$0\" identity keygen --out \"$1\""
        ))
        .arg(BIN)
        .arg(out)
        .current_dir(dir)
        .env_remove("PLATFORMD_CONFIG")
        .output()
        .expect("spawn platformd")
}

fn keygen(dir: &Path, out: &Path) -> Output {
    keygen_with_umask("000", dir, out)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("UTF-8")
}

#[test]
fn writes_a_private_seed_and_prints_kid_and_public_key() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("identity.key");

    let output = keygen(dir.path(), &out);

    let stdout = text(&output.stdout);
    assert!(output.status.success(), "stderr: {}", text(&output.stderr));
    assert!(output.stderr.is_empty(), "{}", text(&output.stderr));
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 2, "{stdout:?}");
    assert!(stdout.ends_with('\n'));
    let kid = lines[0].strip_prefix("kid: ").expect("kid line");
    let public_key = lines[1]
        .strip_prefix("public_key: ")
        .expect("public_key line");

    assert_eq!(kid.len(), 8);
    assert!(
        kid.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "{kid}"
    );
    assert_eq!(public_key.len(), 44, "standard padded base64 of 32 bytes");
    let raw = STANDARD.decode(public_key).expect("standard base64");
    assert_eq!(raw.len(), 32);
    let digest = Sha256::digest(&raw);
    let expected_kid: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        kid, expected_kid,
        "kid = first 4 bytes of sha256(public key)"
    );

    let seed = std::fs::read(&out).unwrap();
    assert_eq!(seed.len(), 32, "raw seed only");
    let identity = platformd::identity::load_signing_key(&out).unwrap();
    assert_eq!(identity.public_key_base64(), public_key);
    let seed_hex: String = seed.iter().map(|b| format!("{b:02x}")).collect();
    assert!(!stdout.contains(&STANDARD.encode(&seed)) && !stdout.contains(&seed_hex));

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "mode {mode:o} despite umask 000");
    }
}

#[cfg(unix)]
#[test]
fn mode_is_0640_under_a_strict_umask_too() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("identity.key");
    let output = keygen_with_umask("077", dir.path(), &out);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o640, "mode {mode:o} despite umask 077");
}

#[test]
fn refuses_to_overwrite_an_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("identity.key");
    std::fs::write(&out, b"existing").unwrap();

    let output = keygen(dir.path(), &out);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    let stderr = text(&output.stderr);
    assert!(
        stderr.starts_with("platformd: ") && stderr.contains("already exists"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&out).unwrap(), b"existing");
}

#[test]
fn missing_out_is_a_usage_error() {
    let output = Command::new(BIN)
        .args(["identity", "keygen"])
        .env_remove("PLATFORMD_CONFIG")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
