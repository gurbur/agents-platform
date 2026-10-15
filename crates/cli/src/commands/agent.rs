//! `agent register|list|inbox|tokens rotate` → `POST /v1/agents`,
//! `GET /v1/agents`, `GET /v1/agents/{id}/inbox`,
//! `POST /v1/agents/{id}/tokens/rotate`.

use clap::Subcommand;
use platform_wire::{
    AgentEntry, AgentId, AgentList, InboxPage, OwnerAgentEntry, RegisterAgentRequest,
    RegisterAgentResponse, RotateTokensResponse,
};
use reqwest::Method;
use serde::Deserialize;

use super::inbox::PageArgs;
use super::{Context, agent_path};
use crate::error::CliError;
use crate::output;

const SHOWN_ONCE: &str = "agent_api_key and receiver_secret are shown once; store them now";

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Register an agent (owner) — POST /v1/agents
    Register {
        agent_id: AgentId,
        /// Receiver URL for wake notifications; omitted means none
        #[arg(long, value_name = "URL")]
        receiver_url: Option<String>,
    },
    /// List registered agents — GET /v1/agents
    List,
    /// Read another agent's inbox (owner) — GET /v1/agents/{agent_id}/inbox
    Inbox {
        agent_id: AgentId,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Credential commands
    #[command(subcommand)]
    Tokens(TokensCommand),
}

#[derive(Debug, Subcommand)]
pub enum TokensCommand {
    /// Issue new credentials and revoke the old ones (owner) —
    /// POST /v1/agents/{agent_id}/tokens/rotate
    Rotate { agent_id: AgentId },
}

/// `GET /v1/agents` answers with richer entries for the owner
/// (`IF-WIRE-001`). The owner shape is tried first because an owner entry
/// also satisfies the agent shape.
#[derive(Deserialize)]
#[serde(untagged)]
enum Directory {
    Owner(AgentList<OwnerAgentEntry>),
    Agent(AgentList<AgentEntry>),
}

pub async fn run(command: AgentCommand, context: &Context) -> Result<(), CliError> {
    match command {
        AgentCommand::Register {
            agent_id,
            receiver_url,
        } => register(agent_id, receiver_url, context).await,
        AgentCommand::List => list(context).await,
        AgentCommand::Inbox { agent_id, page } => {
            let path = agent_path(&agent_id, "/inbox");
            let response = context
                .client
                .get::<InboxPage>(&path, &page.query())
                .await?;
            context.show(&response, output::inbox_page)
        }
        AgentCommand::Tokens(TokensCommand::Rotate { agent_id }) => {
            let path = agent_path(&agent_id, "/tokens/rotate");
            let response = context
                .client
                .post_empty::<RotateTokensResponse>(&path)
                .await?;
            context.show(&response, output::rotation)?;
            output::warn(SHOWN_ONCE);
            Ok(())
        }
    }
}

async fn register(
    agent_id: AgentId,
    receiver_url: Option<String>,
    context: &Context,
) -> Result<(), CliError> {
    let request = RegisterAgentRequest {
        agent_id: agent_id.to_string(),
        receiver_url,
    };
    let response = context
        .client
        .send_json::<RegisterAgentResponse, _>(Method::POST, "/v1/agents", &request)
        .await?;
    context.show(&response, output::registration)?;
    output::warn(SHOWN_ONCE);
    Ok(())
}

async fn list(context: &Context) -> Result<(), CliError> {
    let response = context
        .client
        .get::<Directory>("/v1/agents", &Vec::new())
        .await?;
    context.show(&response, |directory| match directory {
        Directory::Owner(list) => output::owner_directory(&list.items),
        Directory::Agent(list) => output::agent_directory(&list.items),
    })
}
