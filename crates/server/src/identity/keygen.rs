//! `platformd identity keygen --out <path>` (IDENTITY_ATTESTATION §7.1).
//!
//! Draws 32 bytes from the OS CSPRNG, writes them as the raw seed to a new
//! file with mode 0640 regardless of the umask, and returns the `kid` and the
//! public key. The operator then changes the group to the service group, for
//! a final 0640 root:platformd like the owner token. The seed is never
//! printed.

use std::fs::{File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use rand::TryRng as _;
use rand::rngs::SysRng;

use super::{SEED_LEN, SigningIdentity};

/// Mode of a freshly written key file, independent of the umask.
#[cfg(unix)]
const KEY_FILE_MODE: u32 = 0o640;

/// What the operator copies into consumer configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedKey {
    pub kid: String,
    /// Standard, padded base64 of the raw 32-byte public key.
    pub public_key: String,
}

#[derive(Debug, thiserror::Error)]
pub enum KeygenError {
    #[error("output file {0} already exists; refusing to overwrite it")]
    OutputExists(PathBuf),
    #[error("cannot create output file {path}: {source}")]
    Create { path: PathBuf, source: io::Error },
    #[error("cannot write output file {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("the OS random number generator failed: {0}")]
    Random(String),
}

/// Generates a new signing key into the new file `out`. An existing `out`
/// is refused; on a write failure the partial file is removed.
pub fn generate(out: &Path) -> Result<GeneratedKey, KeygenError> {
    let mut seed = [0u8; SEED_LEN];
    SysRng
        .try_fill_bytes(&mut seed)
        .map_err(|err| KeygenError::Random(err.to_string()))?;
    let written = write_new_key_file(out, &seed);
    let identity = SigningIdentity::from_seed(&seed);
    // Best effort: the stack copy of the seed is not kept around.
    seed.fill(0);
    written?;
    Ok(GeneratedKey {
        kid: identity.kid().to_owned(),
        public_key: identity.public_key_base64(),
    })
}

fn write_new_key_file(out: &Path, seed: &[u8; SEED_LEN]) -> Result<(), KeygenError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(KEY_FILE_MODE);
    }
    let mut file = match options.open(out) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            return Err(KeygenError::OutputExists(out.to_owned()));
        }
        Err(source) => {
            return Err(KeygenError::Create {
                path: out.to_owned(),
                source,
            });
        }
    };
    let result = restrict_permissions(&file)
        .and_then(|()| file.write_all(seed))
        .and_then(|()| file.sync_all());
    if let Err(source) = result {
        drop(file);
        // Best effort: the write error being returned matters more.
        let _ = std::fs::remove_file(out);
        return Err(KeygenError::Write {
            path: out.to_owned(),
            source,
        });
    }
    Ok(())
}

/// `open(…, 0640)` is still masked by the umask; set the mode explicitly so
/// the result never depends on it.
#[cfg(unix)]
fn restrict_permissions(file: &File) -> io::Result<()> {
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(Permissions::from_mode(KEY_FILE_MODE))
}

#[cfg(not(unix))]
fn restrict_permissions(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::load_signing_key;

    #[test]
    fn writes_a_loadable_32_byte_seed() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("identity.key");
        let generated = generate(&out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap().len(), SEED_LEN);
        let loaded = load_signing_key(&out).unwrap();
        assert_eq!(loaded.kid(), generated.kid);
        assert_eq!(loaded.public_key_base64(), generated.public_key);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&out).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, KEY_FILE_MODE);
        }
    }

    #[test]
    fn two_runs_produce_different_keys() {
        let dir = tempfile::tempdir().unwrap();
        let first = generate(&dir.path().join("a.key")).unwrap();
        let second = generate(&dir.path().join("b.key")).unwrap();
        assert_ne!(first.public_key, second.public_key);
    }

    #[test]
    fn refuses_an_existing_file_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("identity.key");
        std::fs::write(&out, b"keep me").unwrap();
        assert!(matches!(generate(&out), Err(KeygenError::OutputExists(_))));
        assert_eq!(std::fs::read(&out).unwrap(), b"keep me");
    }
}
