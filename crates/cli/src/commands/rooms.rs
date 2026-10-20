//! `room list|create|invite|remove|archive` → `GET /v1/rooms`,
//! `POST /v1/rooms`, `PUT·DELETE /v1/rooms/{room_id}/members/{agent_id}`,
//! `POST /v1/rooms/{room_id}/archive` (`IF-CLI-001` S13). Listing is for
//! both principals; managing topic rooms is the owner's.

use clap::{Args, Subcommand};
use platform_wire::{
    AgentId, CreateRoomRequest, RoomId, RoomKind, RoomList, RoomSummary, validate_room_title,
};
use reqwest::Method;
use serde::Serialize;

use super::Context;
use crate::client::Query;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum RoomCommand {
    /// The rooms you can read (owner: every room) — GET /v1/rooms
    List,
    /// Create a topic room (owner) — POST /v1/rooms
    Create {
        /// `room:<slug>`
        room_id: RoomId,
        /// Display name (1-64 characters after trimming)
        #[arg(long, value_name = "TEXT")]
        title: Option<String>,
    },
    /// Invite an agent into a topic room (owner) —
    /// PUT /v1/rooms/{room_id}/members/{agent_id}
    Invite(MemberArgs),
    /// Remove an agent from a topic room (owner) —
    /// DELETE /v1/rooms/{room_id}/members/{agent_id}
    Remove(MemberArgs),
    /// Archive a topic room; it stays readable (owner) —
    /// POST /v1/rooms/{room_id}/archive
    Archive { room_id: RoomId },
}

#[derive(Debug, Args)]
pub struct MemberArgs {
    /// `room:<slug>`
    room_id: RoomId,
    agent_id: AgentId,
}

/// `room remove --json` output: the 204 has no body.
#[derive(Serialize)]
struct Removed<'a> {
    room_id: &'a RoomId,
    agent_id: &'a AgentId,
    removed: bool,
}

pub async fn run(command: RoomCommand, context: &Context) -> Result<(), CliError> {
    match command {
        RoomCommand::List => list(context).await,
        RoomCommand::Create { room_id, title } => create(room_id, title, context).await,
        RoomCommand::Invite(args) => invite(args, context).await,
        RoomCommand::Remove(args) => remove(args, context).await,
        RoomCommand::Archive { room_id } => archive(room_id, context).await,
    }
}

async fn list(context: &Context) -> Result<(), CliError> {
    let response = context
        .client
        .get::<RoomList>("/v1/rooms", &Query::new())
        .await?;
    context.show(&response, |list| output::rooms(&list.rooms))
}

async fn create(room_id: RoomId, title: Option<String>, context: &Context) -> Result<(), CliError> {
    require_topic(&room_id)?;
    if let Some(title) = &title {
        validate_room_title(title).map_err(|err| CliError::Usage(err.to_string()))?;
    }
    let request = CreateRoomRequest {
        room_id: room_id.to_string(),
        title,
    };
    let response = context
        .client
        .send_json::<RoomSummary, _>(Method::POST, "/v1/rooms", &request)
        .await?;
    context.show(&response, output::room)
}

async fn invite(args: MemberArgs, context: &Context) -> Result<(), CliError> {
    require_topic(&args.room_id)?;
    let response = context
        .client
        .send_empty::<RoomSummary>(Method::PUT, &member_path(&args))
        .await?;
    context.show(&response, output::room)
}

async fn remove(args: MemberArgs, context: &Context) -> Result<(), CliError> {
    require_topic(&args.room_id)?;
    context.client.delete(&member_path(&args)).await?;
    if context.json {
        let body = Removed {
            room_id: &args.room_id,
            agent_id: &args.agent_id,
            removed: true,
        };
        // Serializing two ids and a bool cannot fail.
        let raw = serde_json::to_vec(&body).expect("removal serializes");
        return output::emit_raw(&raw);
    }
    output::emit(&format!(
        "removed: {} from {}\n",
        args.agent_id, args.room_id
    ))
}

async fn archive(room_id: RoomId, context: &Context) -> Result<(), CliError> {
    require_topic(&room_id)?;
    // `RoomId` holds only slug characters and `:`, path-safe as it is.
    let path = format!("/v1/rooms/{room_id}/archive");
    let response = context
        .client
        .send_empty::<RoomSummary>(Method::POST, &path)
        .await?;
    context.show(&response, output::room)
}

/// Topic rooms are the only ones managed; others are a usage error.
fn require_topic(room_id: &RoomId) -> Result<(), CliError> {
    if room_id.kind() == RoomKind::Topic {
        return Ok(());
    }
    Err(CliError::Usage(format!(
        "{room_id} is not a topic room (`room:<slug>`)"
    )))
}

fn member_path(args: &MemberArgs) -> String {
    format!("/v1/rooms/{}/members/{}", args.room_id, args.agent_id)
}
