//! `platformd` configuration (`IF-CFG-001`).
//!
//! Relative paths are resolved against the directory containing the config
//! file. Secrets never live in the file itself: the owner token is read from
//! `owner_token_file` and the identity signing seed from
//! `[identity].signing_key_file`.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

use platform_wire::AgentId;
use platform_wire::identity::MAX_TOKEN_LIFETIME_SECS;
use serde::Deserialize;

use crate::identity::{self, IssuePolicy, Issuer, KeyFileError, SigningIdentity};

/// Minimum owner token length in bytes after trimming.
pub const OWNER_TOKEN_MIN_BYTES: usize = 32;

/// `[identity]` lifetime defaults (IDENTITY_ATTESTATION §9). Tuning values:
/// tests never assert them.
const DEFAULT_IDENTITY_TTL_SECS: u64 = 600;
const DEFAULT_IDENTITY_MAX_TTL_SECS: u64 = 900;

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
    #[error("{0}")]
    IdentityKey(#[from] KeyFileError),
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
    /// `None` when the `[identity]` table is absent: identity attestation is
    /// disabled.
    pub identity: Option<IdentityConfig>,
    pub owner: OwnerConfig,
}

/// `[owner]` after validation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OwnerConfig {
    /// Agents the owner may name as `sender_id` of `POST /v1/messages`;
    /// they may also be mentioned in any room (S13). Empty (the default)
    /// turns owner posting off.
    pub post_as: Vec<AgentId>,
}

/// On-disk `[owner]`; the table and `post_as` are optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct OwnerFile {
    post_as: Vec<String>,
}

impl OwnerFile {
    /// Startup checks: every entry is an `agent_id` (reserved words
    /// refused) and listed once.
    fn validate(self) -> Result<OwnerConfig, String> {
        let mut post_as: Vec<AgentId> = Vec::with_capacity(self.post_as.len());
        for raw in self.post_as {
            let agent_id: AgentId = raw
                .parse()
                .map_err(|err| format!("owner.post_as entry {raw:?}: {err}"))?;
            if post_as.contains(&agent_id) {
                return Err(format!("owner.post_as lists {raw:?} twice"));
            }
            post_as.push(agent_id);
        }
        Ok(OwnerConfig { post_as })
    }
}

/// `[identity]` after validation, with the signing key loaded.
#[derive(Debug, Clone)]
pub struct IdentityConfig {
    pub signing_key_file: PathBuf,
    pub policy: IssuePolicy,
    pub signing: SigningIdentity,
}

impl IdentityConfig {
    pub fn issuer(&self) -> Issuer {
        Issuer::new(self.signing.clone(), self.policy.clone())
    }
}

/// On-disk `[identity]`. `signing_key_file`, `issuer` and `audiences` are
/// required.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityFile {
    signing_key_file: PathBuf,
    issuer: String,
    audiences: Vec<String>,
    #[serde(default = "default_identity_ttl_secs")]
    default_ttl_secs: u64,
    #[serde(default = "default_identity_max_ttl_secs")]
    max_ttl_secs: u64,
}

fn default_identity_ttl_secs() -> u64 {
    DEFAULT_IDENTITY_TTL_SECS
}

fn default_identity_max_ttl_secs() -> u64 {
    DEFAULT_IDENTITY_MAX_TTL_SECS
}

impl IdentityFile {
    /// Startup checks: a non-empty issuer, at least one audience, every
    /// audience non-empty and listed once, and
    /// `1 <= default_ttl_secs <= max_ttl_secs <= MAX_TOKEN_LIFETIME_SECS`.
    fn validate(&self) -> Result<(), String> {
        if self.issuer.is_empty() {
            return Err("identity.issuer must not be empty".into());
        }
        if self.audiences.is_empty() {
            return Err("identity.audiences must list at least one audience".into());
        }
        for (index, audience) in self.audiences.iter().enumerate() {
            if audience.is_empty() {
                return Err("identity.audiences must not contain an empty string".into());
            }
            if self.audiences[..index].contains(audience) {
                return Err(format!("identity.audiences lists {audience:?} twice"));
            }
        }
        let in_order = 1 <= self.default_ttl_secs
            && self.default_ttl_secs <= self.max_ttl_secs
            && self.max_ttl_secs <= MAX_TOKEN_LIFETIME_SECS;
        if !in_order {
            return Err(format!(
                "identity requires 1 <= default_ttl_secs ({}) <= max_ttl_secs ({}) <= {MAX_TOKEN_LIFETIME_SECS}",
                self.default_ttl_secs, self.max_ttl_secs
            ));
        }
        Ok(())
    }
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
    #[serde(default)]
    identity: Option<IdentityFile>,
    #[serde(default)]
    owner: OwnerFile,
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
        let invalid = |message| ConfigError::Invalid {
            path: path.to_owned(),
            message,
        };
        file.delivery.validate().map_err(invalid)?;
        if let Some(identity) = &file.identity {
            identity.validate().map_err(invalid)?;
        }
        let owner = file.owner.validate().map_err(invalid)?;

        let base = config_dir(path);
        let db_path = base.join(&file.db_path);
        let owner_token_file = base.join(&file.owner_token_file);
        let owner_token = read_owner_token(&owner_token_file)?;
        let identity = file
            .identity
            .map(|identity| load_identity(&base, identity))
            .transpose()?;

        Ok(Self {
            listen: file.listen,
            db_path,
            owner_token_file,
            owner_token,
            delivery: file.delivery,
            identity,
            owner,
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

/// Reads the signing seed; a missing file or a wrong length refuses startup.
fn load_identity(base: &Path, file: IdentityFile) -> Result<IdentityConfig, ConfigError> {
    let signing_key_file = base.join(&file.signing_key_file);
    let signing = identity::load_signing_key(&signing_key_file)?;
    Ok(IdentityConfig {
        signing_key_file,
        policy: IssuePolicy {
            issuer: file.issuer,
            audiences: file.audiences,
            default_ttl_secs: file.default_ttl_secs,
            max_ttl_secs: file.max_ttl_secs,
        },
        signing,
    })
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

    /// The Pi example carries a placeholder address the operator replaces;
    /// with a real address in its place it must load unchanged otherwise.
    #[test]
    fn pi_example_config_loads_once_the_address_is_filled_in() {
        let example = include_str!("../../../deploy/platformd.pi.example.toml");
        assert!(example.contains("\"100.x.y.z:7340\""));
        let filled = example.replace("100.x.y.z", "100.64.0.1");

        let file: ConfigFile = toml::from_str(&filled).unwrap();

        assert_eq!(file.listen, "100.64.0.1:7340".parse().unwrap());
        assert_eq!(file.db_path, Path::new("/var/lib/platformd/platform.db"));
        assert_eq!(
            file.owner_token_file,
            Path::new("/etc/platformd/owner.token")
        );
    }

    // ---- [identity] ----------------------------------------------------------

    const SEED: [u8; 32] = [9; 32];

    /// `MINIMAL` plus an `[identity]` table holding `body`, with a valid key.
    fn identity_config(fx: &Fixture, body: &str) -> PathBuf {
        fx.write("owner.token", VALID_TOKEN);
        std::fs::write(fx.dir.path().join("identity.key"), SEED).unwrap();
        fx.config(&format!("{MINIMAL}[identity]\n{body}"))
    }

    const IDENTITY_REQUIRED: &str = "signing_key_file = \"identity.key\"\n\
         issuer = \"agents-platform\"\n\
         audiences = [\"garage-docs\"]\n";

    fn assert_invalid(path: &Path, needle: &str) {
        let err = Config::load(path).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }), "{err}");
        assert!(err.to_string().contains(needle), "{err}");
    }

    #[test]
    fn identity_is_disabled_without_the_table() {
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        assert!(
            Config::load(&fx.config(MINIMAL))
                .unwrap()
                .identity
                .is_none()
        );
    }

    #[test]
    fn identity_applies_explicit_values_and_loads_the_key() {
        let fx = Fixture::new();
        let path = identity_config(
            &fx,
            "signing_key_file = \"identity.key\"\n\
             issuer = \"hub-x\"\n\
             audiences = [\"garage-docs\", \"other\"]\n\
             default_ttl_secs = 7\n\
             max_ttl_secs = 8\n",
        );

        let identity = Config::load(&path).unwrap().identity.unwrap();

        assert_eq!(
            identity.signing_key_file,
            fx.dir.path().join("identity.key")
        );
        assert_eq!(
            identity.policy,
            IssuePolicy {
                issuer: "hub-x".into(),
                audiences: vec!["garage-docs".into(), "other".into()],
                default_ttl_secs: 7,
                max_ttl_secs: 8,
            }
        );
        assert_eq!(
            identity.signing.kid(),
            SigningIdentity::from_seed(&SEED).kid()
        );
    }

    #[test]
    fn identity_defaults_are_within_bounds() {
        let fx = Fixture::new();
        let identity = Config::load(&identity_config(&fx, IDENTITY_REQUIRED))
            .unwrap()
            .identity
            .unwrap();
        let policy = identity.policy;
        assert!(1 <= policy.default_ttl_secs);
        assert!(policy.default_ttl_secs <= policy.max_ttl_secs);
        assert!(policy.max_ttl_secs <= MAX_TOKEN_LIFETIME_SECS);
    }

    #[test]
    fn identity_ttl_bounds_are_enforced() {
        let limit = MAX_TOKEN_LIFETIME_SECS;
        let accepted = [(1, 1), (1, limit), (limit, limit)];
        for (default, max) in accepted {
            let fx = Fixture::new();
            let path = identity_config(
                &fx,
                &format!("{IDENTITY_REQUIRED}default_ttl_secs = {default}\nmax_ttl_secs = {max}\n"),
            );
            assert!(Config::load(&path).is_ok(), "{default} {max}");
        }
        let rejected = [
            (0, 1),
            (0, 0),
            (2, 1),
            (1, limit + 1),
            (limit + 1, limit + 1),
        ];
        for (default, max) in rejected {
            let fx = Fixture::new();
            let path = identity_config(
                &fx,
                &format!("{IDENTITY_REQUIRED}default_ttl_secs = {default}\nmax_ttl_secs = {max}\n"),
            );
            assert_invalid(&path, "max_ttl_secs");
        }
    }

    #[test]
    fn identity_max_alone_cannot_drop_below_the_default() {
        let fx = Fixture::new();
        let path = identity_config(&fx, &format!("{IDENTITY_REQUIRED}max_ttl_secs = 1\n"));
        assert_invalid(&path, "default_ttl_secs");
    }

    #[test]
    fn identity_audiences_and_issuer_are_checked() {
        let cases = [
            ("audiences = []\n", "at least one"),
            ("audiences = [\"\"]\n", "empty string"),
            ("audiences = [\"a\", \"\"]\n", "empty string"),
            ("audiences = [\"a\", \"b\", \"a\"]\n", "twice"),
        ];
        for (audiences, needle) in cases {
            let fx = Fixture::new();
            let path = identity_config(
                &fx,
                &format!("signing_key_file = \"identity.key\"\nissuer = \"i\"\n{audiences}"),
            );
            assert_invalid(&path, needle);
        }
        let fx = Fixture::new();
        let path = identity_config(
            &fx,
            "signing_key_file = \"identity.key\"\nissuer = \"\"\naudiences = [\"a\"]\n",
        );
        assert_invalid(&path, "issuer");
    }

    #[test]
    fn identity_rejects_unknown_and_missing_keys() {
        for body in [
            format!("{IDENTITY_REQUIRED}seed = \"inline\"\n"),
            "issuer = \"i\"\naudiences = [\"garage-docs\"]\n".to_owned(),
            "signing_key_file = \"identity.key\"\naudiences = [\"garage-docs\"]\n".to_owned(),
            "signing_key_file = \"identity.key\"\nissuer = \"i\"\n".to_owned(),
        ] {
            let fx = Fixture::new();
            let path = identity_config(&fx, &body);
            let err = Config::load(&path).unwrap_err();
            assert!(matches!(err, ConfigError::Parse { .. }), "{body}: {err}");
        }
    }

    #[test]
    fn identity_refuses_a_bad_key_file() {
        for contents in [&b""[..], &[1u8; 31], &[1u8; 33]] {
            let fx = Fixture::new();
            let path = identity_config(&fx, IDENTITY_REQUIRED);
            std::fs::write(fx.dir.path().join("identity.key"), contents).unwrap();
            let err = Config::load(&path).unwrap_err();
            assert!(
                matches!(err, ConfigError::IdentityKey(KeyFileError::Length { .. })),
                "{err}"
            );
            assert!(!err.to_string().contains('\n'), "one line: {err}");
        }
        let fx = Fixture::new();
        let path = identity_config(&fx, IDENTITY_REQUIRED);
        std::fs::remove_file(fx.dir.path().join("identity.key")).unwrap();
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::IdentityKey(KeyFileError::Read { .. }))
        ));
    }

    /// The example's `[identity]` block is commented out with `#` directly
    /// before each TOML line; uncommented, it must load.
    fn uncomment_identity_block(example: &str) -> String {
        let start = example
            .find("#[identity]")
            .expect("commented [identity] block");
        let (before, block) = example.split_at(start);
        let uncommented: Vec<&str> = block
            .lines()
            .map(|line| match line.strip_prefix('#') {
                Some(rest) if !rest.starts_with([' ', '#']) && !rest.is_empty() => rest,
                _ => line,
            })
            .collect();
        format!("{before}{}\n", uncommented.join("\n"))
    }

    #[test]
    fn example_identity_block_loads_once_uncommented() {
        let example = include_str!("../../../platformd.example.toml");
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let enabled = uncomment_identity_block(example);
        let path = fx.config(&enabled);
        let key_name = "identity.key";
        assert!(enabled.contains(&format!("signing_key_file = \"{key_name}\"")));
        std::fs::write(fx.dir.path().join(key_name), SEED).unwrap();

        let identity = Config::load(&path).unwrap().identity.unwrap();

        assert_eq!(identity.policy.audiences, ["garage-docs"]);
        assert!(
            Config::load(&fx.config(example))
                .unwrap()
                .identity
                .is_none()
        );
    }

    #[test]
    fn pi_example_identity_block_parses_once_uncommented() {
        let example = include_str!("../../../deploy/platformd.pi.example.toml")
            .replace("100.x.y.z", "100.64.0.1");
        let file: ConfigFile = toml::from_str(&uncomment_identity_block(&example)).unwrap();
        let identity = file.identity.expect("identity table");
        identity.validate().unwrap();
        assert_eq!(
            identity.signing_key_file,
            Path::new("/etc/platformd/identity.key")
        );
        let commented: ConfigFile = toml::from_str(&example).unwrap();
        assert!(commented.identity.is_none());
    }

    // ---- [owner] -------------------------------------------------------------

    fn owner_config(fx: &Fixture, table: &str) -> PathBuf {
        fx.write("owner.token", VALID_TOKEN);
        fx.config(&format!("{MINIMAL}{table}"))
    }

    #[test]
    fn owner_posting_is_off_without_the_table() {
        let fx = Fixture::new();
        let config = Config::load(&owner_config(&fx, "")).unwrap();
        assert!(config.owner.post_as.is_empty());
        let config = Config::load(&owner_config(&fx, "[owner]\n")).unwrap();
        assert!(config.owner.post_as.is_empty(), "post_as defaults to empty");
    }

    #[test]
    fn owner_post_as_is_applied_in_order() {
        let fx = Fixture::new();
        let path = owner_config(&fx, "[owner]\npost_as = [\"human\", \"ops-desk\"]\n");
        let config = Config::load(&path).unwrap();
        let expected: Vec<AgentId> = vec!["human".parse().unwrap(), "ops-desk".parse().unwrap()];
        assert_eq!(config.owner.post_as, expected);
    }

    #[test]
    fn owner_post_as_entries_are_agent_ids_listed_once() {
        for (list, needle) in [
            ("[\"Human\"]", "Human"),
            ("[\"owner\"]", "owner"),
            ("[\"all\"]", "all"),
            ("[\"\"]", "owner.post_as"),
            ("[\"human\", \"human\"]", "twice"),
        ] {
            let fx = Fixture::new();
            let path = owner_config(&fx, &format!("[owner]\npost_as = {list}\n"));
            assert_invalid(&path, needle);
        }
    }

    #[test]
    fn owner_rejects_unknown_keys() {
        let fx = Fixture::new();
        let path = owner_config(&fx, "[owner]\npost_as = []\npost_as_anyone = true\n");
        assert!(matches!(
            Config::load(&path),
            Err(ConfigError::Parse { .. })
        ));
    }

    /// Uncomments every `#`-prefixed TOML line of the example's `#[owner]`
    /// block, which ends at the first line that is not commented TOML.
    fn uncomment_owner_block(example: &str) -> String {
        let start = example.find("#[owner]").expect("commented [owner] block");
        let (before, block) = example.split_at(start);
        let mut lines: Vec<String> = Vec::new();
        let mut in_block = true;
        for line in block.lines() {
            match line.strip_prefix('#') {
                Some(rest) if in_block && !rest.starts_with([' ', '#']) && !rest.is_empty() => {
                    lines.push(rest.to_owned());
                }
                _ => {
                    in_block = false;
                    lines.push(line.to_owned());
                }
            }
        }
        format!("{before}{}\n", lines.join("\n"))
    }

    #[test]
    fn example_owner_block_loads_once_uncommented() {
        let example = include_str!("../../../platformd.example.toml");
        let fx = Fixture::new();
        fx.write("owner.token", VALID_TOKEN);
        let enabled = uncomment_owner_block(example);
        assert!(enabled.contains("\n[owner]\n"), "{enabled}");

        let config = Config::load(&fx.config(&enabled)).unwrap();

        assert!(config.owner.post_as.is_empty(), "the example keeps it off");
        assert!(config.identity.is_none(), "[identity] stays commented");
    }

    #[test]
    fn pi_example_owner_block_parses_once_uncommented() {
        let example = include_str!("../../../deploy/platformd.pi.example.toml")
            .replace("100.x.y.z", "100.64.0.1");
        let enabled = uncomment_owner_block(&example);
        assert!(enabled.contains("\n[owner]\n"), "{enabled}");
        let file: ConfigFile = toml::from_str(&enabled).unwrap();
        assert!(file.owner.validate().unwrap().post_as.is_empty());
        let commented: ConfigFile = toml::from_str(&example).unwrap();
        assert!(commented.owner.post_as.is_empty());
    }

    #[test]
    fn debug_redacts_owner_token() {
        let token = OwnerToken(VALID_TOKEN.to_owned());
        assert!(!format!("{token:?}").contains(VALID_TOKEN));
    }
}
