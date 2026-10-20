//! `receiver set|unset` → `PATCH /v1/agents/{id}/receiver`; `set` may also
//! carry `--wake-policy` (S13).

use clap::Subcommand;
use platform_wire::{AgentId, FieldPatch, ReceiverResponse, UpdateReceiverRequest, WakePolicy};
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
        /// `sender` follows the sender's choice; `always` also wakes for
        /// quiet events. Omitted keeps the stored policy.
        #[arg(long, value_name = "sender|always")]
        wake_policy: Option<WakePolicy>,
    },
    /// Disconnect the receiver route (inbox is kept) —
    /// PATCH /v1/agents/{agent_id}/receiver with null
    Unset { agent_id: AgentId },
}

pub async fn run(command: ReceiverCommand, context: &Context) -> Result<(), CliError> {
    let (agent_id, request) = match command {
        ReceiverCommand::Set {
            agent_id,
            receiver_url,
            wake_policy,
        } => (
            agent_id,
            UpdateReceiverRequest {
                receiver_url: FieldPatch::Set(receiver_url),
                wake_policy: wake_policy.map(|policy| policy.as_str().to_owned()),
            },
        ),
        ReceiverCommand::Unset { agent_id } => (
            agent_id,
            UpdateReceiverRequest {
                receiver_url: FieldPatch::Clear,
                wake_policy: None,
            },
        ),
    };
    let path = agent_path(&agent_id, "/receiver");
    let response = context
        .client
        .send_json::<ReceiverResponse, _>(Method::PATCH, &path, &request)
        .await?;
    context.show(&response, output::receiver)
}
