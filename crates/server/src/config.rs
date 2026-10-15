//! `platformd` configuration (`IF-CFG-001`).
//!
//! Relative paths are resolved against the directory containing the config
//! file. Secrets never live in the file itself: the owner token is read from
//! `owner_token_file`.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Minimum owner token length in bytes after trimming.
pub const OWNER_TOKEN_MIN_BYTES: usize = 32;

const DEFAULT_LISTEN: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 7340));

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config file {path}{location}: {message}")]
    Parse {
        path: PathBuf,
        location: String,
        message: String,
    },
    #[error("cannot read owner token file {path}: {source}")]
    TokenRead {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "owner token in {path} is {actual} bytes after trimming; at least {OWNER_TOKEN_MIN_BYTES} bytes are required"
    )]
    TokenTooShort { path: PathBuf, actual: usize },
    #[error("invalid config file {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

/// The owner bearer token. Kept in memory only; `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct OwnerToken(String);

impl OwnerToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for OwnerToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("OwnerToken(<redacted>)")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub listen: SocketAddr,
    pub db_path: PathBuf,
    pub owner_token_file: PathBuf,
    pub owner_token: OwnerToken,
    pub delivery: DeliveryConfig,
}

/// Receiver callback tuning. Defaults follow RECEIVER_WAKE_CONVENTION.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeliveryConfig {
    pub connect_timeout_ms: u64,
    pub request_timeout_ms: u64,
    pub backoff_min_secs: u64,
    pub backoff_max_secs: u64,
    pub redrive_after_secs: u64,
}

impl Default for DeliveryConfig {
    fn default() -> Self {
        Self {
            connect_timeout_ms: 2000,
            request_timeout_ms: 5000,
            backoff_min_secs: 1,
            backoff_max_secs: 300,
            redrive_after_secs: 300,
        }
    }
}

impl DeliveryConfig {
    fn validate(&self) -> Result<(), String> {
        let positive = [
            ("delivery.connect_timeout_ms", self.connect_timeout_ms),
            ("delivery.request_timeout_ms", self.request_timeout_ms),
            ("delivery.backoff_min_secs", self.backoff_min_secs),
            ("delivery.backoff_max_secs", self.backoff_max_secs),
            ("delivery.redrive_after_secs", self.redrive_after_secs),
        ];
        if let Some((name, _)) = positive.iter().find(|(_, value)| *value == 0) {
            return Err(format!("{name} must be greater than 0"));
        }
        if self.backoff_min_secs > self.backoff_max_secs {
            return Err(
                "delivery.backoff_min_secs must not exceed delivery.backoff_max_secs".into(),
            );
        }
        Ok(())
    }
}

/// On-disk shape. `listen` and the `[delivery]` table are optional.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    #[serde(default = "default_listen")]
    listen: SocketAddr,
    db_path: PathBuf,
    owner_token_file: PathBuf,
    #[serde(default)]
    delivery: DeliveryConfig,
}

fn default_listen() -> SocketAddr {
    DEFAULT_LISTEN
}

impl Config {
    /// Reads and validates the config file and the owner token file.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let file: ConfigFile = toml::from_str(&text).map_err(|err| ConfigError::Parse {
            path: path.to_owned(),
            location: line_suffix(&text, err.span()),
            message: err.message().to_owned(),
        })?;
        file.delivery
            .validate()
            .map_err(|message| ConfigError::Invalid {
                path: path.to_owned(),
                message,
            })?;

        let base = config_dir(path);
        let db_path = base.join(&file.db_path);
        let owner_token_file = base.join(&file.owner_token_file);
        let owner_token = read_owner_token(&owner_token_file)?;

        Ok(Self {
            listen: file.listen,
            db_path,
            owner_token_file,
            owner_token,
            delivery: file.delivery,
        })
    }
}

/// Directory the config file lives in; `Path::join` keeps absolute paths as-is.
fn config_dir(config_path: &Path) -> PathBuf {
    match config_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_owned(),
        _ => PathBuf::from("."),
    }
}

fn read_owner_token(path: &Path) -> Result<OwnerToken, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::TokenRead {
        path: path.to_owned(),
        source,
    })?;
    let token = raw.trim();
    if token.len() < OWNER_TOKEN_MIN_BYTES {
        return Err(ConfigError::TokenTooShort {
            path: path.to_owned(),
            actual: token.len(),
        });
    }
    Ok(OwnerToken(token.to_owned()))
}

/// " at line N" for a parse error span, so the error stays on one line.
fn line_suffix(text: &str, span: Option<std::ops::Range<usize>>) -> String {
    let Some(span) = span else {
        return String::new();
    };
    let start = span.start.min(text.len());
    let line = text.as_bytes()[..start]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1;
    format!(" at line {line}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_TOKEN: &str = "0123456789abcdef0123456789abcdef-padding";

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
            }
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            std::fs::write(&path, contents).unwrap();
            path
        }

        fn config(&self, toml: &str) -> PathBuf {
            self.write("platformd.toml", toml)
        }
    }

    const MINIMAL: &str = "db_path = \"data/platform.db\"\nowner_token_file = \"owner.token\"\n";

    #[test]
    fn loads_minimal_config_with_defaults_and_relative_paths() {
        let fx = Fixture::new();
        fx.write("owner.token", &format!("{VALID_TOKEN}\n"));
        let path = fx.config(MINIMAL);

        let config = Config::load(&path).unwrap();

        assert_eq!(config.listen, DEFAULT_LISTEN);
        assert_eq!(config.delivery, DeliveryConfig::default());
        assert_eq!(config.db_path, fx.dir.path().join("data/platform.db"));
        assert_eq!(config.owner_token_file, fx.dir.path().join("owner.token"));
        assert_eq!(config.owner_token.expose(), VALID_TOKEN, "token is trimmed");
    }

    #[test]
    fn applies_explicit_values() {
        let fx = Fixture::new();
        let token_path = fx.write("secret.token", VALID_TOKEN);
        let db_path = fx.dir.path().join("elsewhere/x.db");
        let path = fx.config(&format!(
            "listen = \"127.0.0.1:0\"\n\
             db_path = {db:?}\n\
             owner_token_file = {token:?}\n\
             [delivery]\n\
             connect_timeout_ms = 11\n\
             request_timeout_ms = 22\n\
             backoff_min_secs = 3\n\
             backoff_max_secs = 4\n\
             redrive_after_secs = 5\n",
            db = db_path.display().to_string(),
            token = token_path.display().to_string(),
        ));

        let config = Config::load(&path).unwrap();

        assert_eq!(config.listen, "127.0.0.1:0".parse().unwrap());
        assert_eq!(config.db_path, db_path, "absolute paths are kept");
        assert_eq!(config.owner_token_file, token_path);
        let expected = DeliveryConfig {
            connect_timeout_ms: 11,
            request_timeout_ms: 22,
            backoff_min_secs: 3,
            backoff_max_secs: 4,
            redrive_after_secs: 5,
        };
        assert_eq!(config.delivery, expected);
    }

    #[test]
    fn partial_delivery_table_keeps_other_defaults() {
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let path = fx.config(&format!("{MINIMAL}[delivery]\nredrive_after_secs = 9\n"));

        let config = Config::load(&path).unwrap();

        assert_eq!(
            config.delivery,
            DeliveryConfig {
                redrive_after_secs: 9,
                ..DeliveryConfig::default()
            }
        );
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let path = fx.config(&format!("{MINIMAL}owner_token = \"inline\"\n"));

        let err = Config::load(&path).unwrap_err();

        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
        let message = err.to_string();
        assert!(message.contains("owner_token"), "{message}");
        assert!(!message.contains('\n'), "one line: {message:?}");
    }

    #[test]
    fn rejects_unknown_delivery_key() {
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let path = fx.config(&format!("{MINIMAL}[delivery]\nretries = 3\n"));

        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Parse { .. })
        ));
    }

    #[test]
    fn rejects_short_token_without_echoing_it() {
        let fx = Fixture::new();
        let short = "x".repeat(OWNER_TOKEN_MIN_BYTES - 1);
        fx.write("owner.token", &format!("  {short}  \n"));
        let path = fx.config(MINIMAL);

        let err = Config::load(&path).unwrap_err();

        assert!(matches!(err, ConfigError::TokenTooShort { .. }), "{err}");
        assert!(!err.to_string().contains(&short));
    }

    #[test]
    fn rejects_missing_config_file() {
        let fx = Fixture::new();
        let err = Config::load(&fx.dir.path().join("absent.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Read { .. }), "{err}");
    }

    #[test]
    fn rejects_missing_token_file() {
        let fx = Fixture::new();
        let path = fx.config(MINIMAL);
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::TokenRead { .. }), "{err}");
    }

    #[test]
    fn rejects_inconsistent_backoff() {
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let path = fx.config(&format!(
            "{MINIMAL}[delivery]\nbackoff_min_secs = 10\nbackoff_max_secs = 5\n"
        ));
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn example_config_in_repository_loads() {
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let path = fx.config(include_str!("../../../platformd.example.toml"));

        let config = Config::load(&path).unwrap();

        assert_eq!(config.db_path, fx.dir.path().join("data/platform.db"));
    }

    #[test]
    fn debug_redacts_owner_token() {
        let token = OwnerToken(VALID_TOKEN.to_owned());
        assert!(!format!("{token:?}").contains(VALID_TOKEN));
    }
}
