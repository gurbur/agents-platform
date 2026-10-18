//! `agent register|list|delete|inbox|tokens rotate` → `POST /v1/agents`,
//! `GET /v1/agents`, `DELETE /v1/agents/{id}`, `GET /v1/agents/{id}/inbox`,
//! `POST /v1/agents/{id}/tokens/rotate`.

use std::io::BufRead as _;

use clap::Subcommand;
use platform_wire::{
    AgentEntry, AgentId, AgentList, InboxPage, OwnerAgentEntry, RegisterAgentRequest,
    RegisterAgentResponse, RotateTokensResponse, validate_name,
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
        /// Display name shown in the directory (1-32 characters; the owner
        /// changes it later with `profile set-name`)
        #[arg(long, value_name = "NAME")]
        name: String,
        /// Receiver URL for wake notifications; omitted means none
        #[arg(long, value_name = "URL")]
        receiver_url: Option<String>,
    },
    /// List registered agents — GET /v1/agents
    List,
    /// Delete an agent and its state; its messages are kept (owner) —
    /// DELETE /v1/agents/{agent_id}
    Delete {
        agent_id: AgentId,
        /// Skip typing the agent_id to confirm
        #[arg(long)]
        yes: bool,
    },
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
            name,
            receiver_url,
        } => register(agent_id, name, receiver_url, context).await,
        AgentCommand::List => list(context).await,
        AgentCommand::Delete { agent_id, yes } => delete(&agent_id, yes, context).await,
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
    name: String,
    receiver_url: Option<String>,
    context: &Context,
) -> Result<(), CliError> {
    // The server trims; checking here only turns a sure 422 into exit 2.
    validate_name(&name).map_err(|err| CliError::Usage(err.to_string()))?;
    let request = RegisterAgentRequest {
        agent_id: agent_id.to_string(),
        name,
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

async fn delete(agent_id: &AgentId, yes: bool, context: &Context) -> Result<(), CliError> {
    if !yes {
        confirm_deletion(agent_id)?;
    }
    context.client.delete(&agent_path(agent_id, "")).await?;
    if context.json {
        let json = serde_json::json!({"agent_id": agent_id.as_str(), "deleted": true});
        return output::emit_raw(json.to_string().as_bytes());
    }
    output::emit(&format!("deleted: {agent_id}\n"))
}

/// Asks on stderr and reads one line from stdin; only the agent_id itself
/// (surrounding whitespace aside) confirms. Anything else, or EOF, is a
/// usage error and nothing is sent.
fn confirm_deletion(agent_id: &AgentId) -> Result<(), CliError> {
    output::warn(&format!(
        "deleting {agent_id} removes its credentials, inbox and delivery state (its messages are kept); \
         type the agent_id to confirm, or pass --yes:"
    ));
    let mut answer = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut answer)
        .map_err(|err| CliError::io("cannot read the confirmation from stdin", err))?;
    if answer.trim() == agent_id.as_str() {
        return Ok(());
    }
    Err(CliError::Usage(format!(
        "not confirmed: {agent_id} was not deleted"
    )))
}
