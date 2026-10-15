//! `delivery <agent_id>` and `delivery retry <agent_id>` →
//! `GET /v1/agents/{id}/delivery`, `POST /v1/agents/{id}/delivery/retry`.

use clap::{Args, Subcommand};
use platform_wire::{AgentId, DeliveryRetryResponse, DeliveryStatus};

use super::{Context, agent_path};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct DeliveryArgs {
    #[command(subcommand)]
    command: Option<DeliveryCommand>,

    /// Show delivery diagnostics — GET /v1/agents/{agent_id}/delivery
    #[arg(required = true)]
    agent_id: Option<AgentId>,
}

#[derive(Debug, Subcommand)]
pub enum DeliveryCommand {
    /// Unblock and reschedule the wake notification —
    /// POST /v1/agents/{agent_id}/delivery/retry
    Retry { agent_id: AgentId },
}

pub async fn run(args: DeliveryArgs, context: &Context) -> Result<(), CliError> {
    if let Some(DeliveryCommand::Retry { agent_id }) = args.command {
        let path = agent_path(&agent_id, "/delivery/retry");
        let response = context
            .client
            .post_empty::<DeliveryRetryResponse>(&path)
            .await?;
        return context.show(&response, output::delivery_retry);
    }
    let Some(agent_id) = args.agent_id else {
        // clap enforces `agent_id` whenever no subcommand is given.
        return Err(CliError::Usage("missing <AGENT_ID>".into()));
    };
    let path = agent_path(&agent_id, "/delivery");
    let response = context
        .client
        .get::<DeliveryStatus>(&path, &Vec::new())
        .await?;
    context.show(&response, output::delivery_status)
}
