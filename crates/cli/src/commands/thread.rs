//! `thread show|mute|unmute` → `GET /v1/threads/{root_id}` and
//! `PUT /v1/threads/{root_id}/participation` (`IF-CLI-001` S15).

use clap::Subcommand;
use platform_wire::{AgentId, MessageId, ParticipationRequest, ParticipationResponse, ThreadView};
use reqwest::Method;

use super::Context;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ThreadCommand {
    /// Participants and counts of a thread — GET /v1/threads/{root_id}
    Show { root_id: MessageId },
    /// Stop being woken by the thread (mentions still wake) —
    /// PUT /v1/threads/{root_id}/participation
    Mute {
        root_id: MessageId,
        /// The allowlisted identity to mute (owner token; `[owner] post_as`)
        #[arg(long = "as", value_name = "AGENT_ID")]
        agent: Option<AgentId>,
    },
    /// Be woken by the thread again — PUT /v1/threads/{root_id}/participation
    Unmute {
        root_id: MessageId,
        /// The allowlisted identity to unmute (owner token; `[owner] post_as`)
        #[arg(long = "as", value_name = "AGENT_ID")]
        agent: Option<AgentId>,
    },
}

pub async fn run(command: ThreadCommand, context: &Context) -> Result<(), CliError> {
    match command {
        ThreadCommand::Show { root_id } => show(&root_id, context).await,
        ThreadCommand::Mute { root_id, agent } => {
            participate(&root_id, false, agent, context).await
        }
        ThreadCommand::Unmute { root_id, agent } => {
            participate(&root_id, true, agent, context).await
        }
    }
}

/// A validated `MessageId` is `msg-` plus Crockford base32: path-safe.
fn thread_path(root_id: &MessageId, suffix: &str) -> String {
    format!("/v1/threads/{root_id}{suffix}")
}

async fn show(root_id: &MessageId, context: &Context) -> Result<(), CliError> {
    let response = context
        .client
        .get::<ThreadView>(&thread_path(root_id, ""), &Vec::new())
        .await?;
    context.show(&response, output::thread)
}

async fn participate(
    root_id: &MessageId,
    wake: bool,
    agent: Option<AgentId>,
    context: &Context,
) -> Result<(), CliError> {
    let request = ParticipationRequest {
        wake,
        agent_id: agent.map(String::from),
    };
    let response = context
        .client
        .send_json::<ParticipationResponse, _>(
            Method::PUT,
            &thread_path(root_id, "/participation"),
            &request,
        )
        .await?;
    context.show(&response, output::participation)
}
