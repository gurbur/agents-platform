//! `receiver set|unset` → `PATCH /v1/agents/{id}/receiver`.

use clap::Subcommand;
use platform_wire::{AgentId, ReceiverResponse, UpdateReceiverRequest};
use reqwest::Method;

use super::{Context, agent_path};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ReceiverCommand {
    /// Point wake notifications at URL — PATCH /v1/agents/{agent_id}/receiver
    Set {
        agent_id: AgentId,
        /// Named apart from the global `--url`: clap ids share one namespace.
        #[arg(value_name = "URL")]
        receiver_url: String,
    },
    /// Disconnect the receiver route (inbox is kept) —
    /// PATCH /v1/agents/{agent_id}/receiver with null
    Unset { agent_id: AgentId },
}

pub async fn run(command: ReceiverCommand, context: &Context) -> Result<(), CliError> {
    let (agent_id, receiver_url) = match command {
        ReceiverCommand::Set {
            agent_id,
            receiver_url,
        } => (agent_id, Some(receiver_url)),
        ReceiverCommand::Unset { agent_id } => (agent_id, None),
    };
    let path = agent_path(&agent_id, "/receiver");
    let request = UpdateReceiverRequest { receiver_url };
    let response = context
        .client
        .send_json::<ReceiverResponse, _>(Method::PATCH, &path, &request)
        .await?;
    context.show(&response, output::receiver)
}
