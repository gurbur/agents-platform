//! `all join|leave` → `PUT /v1/agents/{id}/membership/all`.

use clap::Subcommand;
use platform_wire::{AgentId, MembershipRequest, MembershipResponse};
use reqwest::Method;

use super::{Context, agent_path};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum AllCommand {
    /// Join the `all` room — PUT /v1/agents/{agent_id}/membership/all
    Join { agent_id: AgentId },
    /// Leave the `all` room — PUT /v1/agents/{agent_id}/membership/all
    Leave { agent_id: AgentId },
}

pub async fn run(command: AllCommand, context: &Context) -> Result<(), CliError> {
    let (agent_id, joined) = match command {
        AllCommand::Join { agent_id } => (agent_id, true),
        AllCommand::Leave { agent_id } => (agent_id, false),
    };
    let path = agent_path(&agent_id, "/membership/all");
    let request = MembershipRequest { joined };
    let response = context
        .client
        .send_json::<MembershipResponse, _>(Method::PUT, &path, &request)
        .await?;
    context.show(&response, output::membership)
}
