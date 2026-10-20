//! One module per top-level subcommand. Each module owns its clap arguments,
//! maps them onto one API_V1 "CLI 매핑" request and hands the decoded
//! response to [`crate::output`].

pub mod agent;
pub mod all;
pub mod avatar;
pub mod delivery;
pub mod health;
pub mod history;
pub mod identity;
pub mod inbox;
pub mod message;
pub mod profile;
pub mod receiver;
pub mod rooms;
pub mod send;

use std::io::Read;
use std::path::Path;

use crate::args::Command;
use crate::client::{ApiClient, Decoded};
use crate::error::CliError;
use crate::output;

/// Everything a command needs besides its own arguments.
pub struct Context {
    pub client: ApiClient,
    pub json: bool,
}

impl Context {
    /// Prints the raw body under `--json`, the human rendering otherwise.
    fn show<T>(
        &self,
        response: &Decoded<T>,
        render: impl FnOnce(&T) -> String,
    ) -> Result<(), CliError> {
        if self.json {
            return output::emit_raw(&response.raw);
        }
        output::emit(&render(&response.value))
    }
}

pub async fn run(command: Command, context: &Context) -> Result<(), CliError> {
    match command {
        Command::Agent(command) => agent::run(command, context).await,
        Command::Receiver(command) => receiver::run(command, context).await,
        Command::All(command) => all::run(command, context).await,
        Command::Send(args) => send::run(args, context).await,
        Command::Inbox(args) => inbox::run(args, context).await,
        Command::History(args) => history::run(args, context).await,
        Command::Message(command) => message::run(command, context).await,
        Command::Delivery(args) => delivery::run(args, context).await,
        Command::Identity(command) => identity::run(command, context).await,
        Command::Profile(command) => profile::run(command, context).await,
        Command::Room(command) => rooms::run(command, context).await,
        Command::Health => health::run(context).await,
    }
}

/// `/v1/agents/{agent_id}` plus `suffix`. `AgentId` only holds slug
/// characters, so it needs no percent-encoding.
fn agent_path(agent_id: &platform_wire::AgentId, suffix: &str) -> String {
    format!("/v1/agents/{agent_id}{suffix}")
}

/// File argument value that means standard input.
const STDIN_PATH: &str = "-";

/// Reads `path` (or stdin for `-`) as UTF-8, byte for byte (not trimmed).
/// `what` names the content in error messages, e.g. "body".
fn read_text(path: &Path, what: &str) -> Result<String, CliError> {
    let (bytes, source) = if path == Path::new(STDIN_PATH) {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|err| CliError::io(format!("cannot read the {what} from stdin"), err))?;
        (bytes, "stdin".to_owned())
    } else {
        let bytes = std::fs::read(path)
            .map_err(|err| CliError::io(format!("cannot read {}", path.display()), err))?;
        (bytes, path.display().to_string())
    };
    String::from_utf8(bytes)
        .map_err(|_| CliError::Usage(format!("the {what} from {source} is not valid UTF-8")))
}
