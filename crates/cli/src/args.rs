//! Global options and subcommands. Flags take precedence over environment
//! variables; `--token-file` takes precedence over `PLATFORM_TOKEN`.

use std::path::{Path, PathBuf};

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};

use crate::client::BearerToken;
use crate::commands::agent::AgentCommand;
use crate::commands::all::AllCommand;
use crate::commands::delivery::DeliveryArgs;
use crate::commands::history::HistoryArgs;
use crate::commands::identity::IdentityCommand;
use crate::commands::inbox::InboxArgs;
use crate::commands::message::MessageCommand;
use crate::commands::profile::ProfileCommand;
use crate::commands::receiver::ReceiverCommand;
use crate::commands::rooms::RoomCommand;
use crate::commands::send::SendArgs;
use crate::error::CliError;

#[derive(Debug, Parser)]
#[command(name = "platform", version, about = "agents-platform HTTP client")]
struct Cli {
    /// Base URL of platformd, e.g. http://127.0.0.1:7340
    #[arg(long, global = true, env = "PLATFORM_URL")]
    url: Option<String>,

    /// Bearer token (owner token or agent API key)
    #[arg(long, global = true, env = "PLATFORM_TOKEN", hide_env_values = true)]
    token: Option<String>,

    /// Read the bearer token from a file (trimmed)
    #[arg(long, global = true, value_name = "PATH")]
    token_file: Option<PathBuf>,

    /// Print the raw JSON response body
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

/// The API_V1 "CLI 매핑" table plus `health`.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Agent registration, directory, owner inbox view and token rotation
    #[command(subcommand)]
    Agent(AgentCommand),
    /// Directory profile: display name and self-declared introduction
    #[command(subcommand)]
    Profile(ProfileCommand),
    /// Receiver route of an agent
    #[command(subcommand)]
    Receiver(ReceiverCommand),
    /// Membership of the `all` room
    #[command(subcommand)]
    All(AllCommand),
    /// Send to `all` or one agent — POST /v1/messages
    Send(SendArgs),
    /// Read your inbox — GET /v1/inbox
    Inbox(InboxArgs),
    /// Read a room's history — GET /v1/messages
    History(HistoryArgs),
    /// List every room, agent-to-agent DMs included (owner) — GET /v1/rooms
    Rooms,
    /// Post into a room as an allowlisted agent (owner)
    #[command(subcommand)]
    Room(RoomCommand),
    /// Single message lookup
    #[command(subcommand)]
    Message(MessageCommand),
    /// Delivery diagnostics and manual retry
    Delivery(DeliveryArgs),
    /// Identity tokens for third parties
    #[command(subcommand)]
    Identity(IdentityCommand),
    /// Server readiness — GET /healthz
    Health,
}

/// Fully resolved invocation.
pub struct Settings {
    pub url: String,
    pub token: BearerToken,
    pub json: bool,
    pub command: Command,
}

impl Settings {
    pub fn from_env_and_args() -> Result<Self, CliError> {
        let matches = Cli::command().get_matches();
        let token_from_flag = matches!(
            matches.value_source("token"),
            Some(ValueSource::CommandLine)
        );
        let cli = parse(&matches);
        Self::resolve(cli, token_from_flag)
    }

    fn resolve(cli: Cli, token_from_flag: bool) -> Result<Self, CliError> {
        let url = cli
            .url
            .filter(|url| !url.trim().is_empty())
            .ok_or_else(|| {
                CliError::Usage("missing server URL: pass --url or set PLATFORM_URL".into())
            })?;
        let token = match (cli.token_file, cli.token) {
            (Some(_), Some(_)) if token_from_flag => {
                return Err(CliError::Usage(
                    "--token and --token-file cannot be used together".into(),
                ));
            }
            (Some(path), _) => read_token_file(&path)?,
            (None, Some(token)) => BearerToken::new(token)?,
            (None, None) => {
                return Err(CliError::Usage(
                    "missing token: pass --token, --token-file or set PLATFORM_TOKEN".into(),
                ));
            }
        };
        Ok(Self {
            url,
            token,
            json: cli.json,
            command: cli.command,
        })
    }
}

fn parse(matches: &ArgMatches) -> Cli {
    // `get_matches` already validated the arguments against this same
    // definition, so conversion cannot fail.
    Cli::from_arg_matches(matches).expect("matches come from Cli::command()")
}

fn read_token_file(path: &Path) -> Result<BearerToken, CliError> {
    let raw = std::fs::read_to_string(path).map_err(|err| {
        CliError::Usage(format!("cannot read token file {}: {err}", path.display()))
    })?;
    BearerToken::new(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cli(args: &[&str]) -> Cli {
        let matches = Cli::command()
            .try_get_matches_from(std::iter::once("platform").chain(args.iter().copied()))
            .unwrap();
        parse(&matches)
    }

    #[test]
    fn global_options_are_accepted_after_the_subcommand() {
        let cli = cli(&["health", "--url", "http://x", "--token", "t", "--json"]);
        let settings = Settings::resolve(cli, true).unwrap();
        assert_eq!(settings.url, "http://x");
        assert!(settings.json);
    }

    /// clap keys values by arg id across the whole tree, so a subcommand
    /// field named like a global option silently overwrites it.
    #[test]
    fn no_subcommand_argument_shadows_a_global_option() {
        fn check(command: &clap::Command, globals: &[String]) {
            for sub in command.get_subcommands() {
                for arg in sub.get_arguments() {
                    let id = arg.get_id().as_str();
                    assert!(
                        !globals.iter().any(|global| global == id),
                        "`{}` redefines global `{id}`",
                        sub.get_name()
                    );
                }
                check(sub, globals);
            }
        }
        let root = Cli::command();
        let globals: Vec<String> = root
            .get_arguments()
            .filter(|arg| arg.is_global_set())
            .map(|arg| arg.get_id().to_string())
            .collect();
        assert!(globals.contains(&"url".to_owned()));
        check(&root, &globals);
    }

    #[test]
    fn receiver_url_does_not_replace_the_server_url() {
        let cli = cli(&[
            "receiver",
            "set",
            "agent-a",
            "http://receiver/n",
            "--url",
            "http://x",
            "--token",
            "t",
        ]);
        let settings = Settings::resolve(cli, true).unwrap();
        assert_eq!(settings.url, "http://x");
    }

    #[test]
    fn missing_url_is_a_usage_error() {
        let cli = Cli {
            url: None,
            token: Some("t".into()),
            token_file: None,
            json: false,
            command: Command::Health,
        };
        let err = Settings::resolve(cli, true).err().unwrap();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn missing_token_is_a_usage_error() {
        let cli = Cli {
            url: Some("http://x".into()),
            token: None,
            token_file: None,
            json: false,
            command: Command::Health,
        };
        let err = Settings::resolve(cli, false).err().unwrap();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn token_flag_and_file_conflict() {
        let cli = Cli {
            url: Some("http://x".into()),
            token: Some("t".into()),
            token_file: Some("t.token".into()),
            json: false,
            command: Command::Health,
        };
        assert_eq!(Settings::resolve(cli, true).err().unwrap().exit_code(), 2);
    }
}
