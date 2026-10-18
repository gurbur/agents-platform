//! `rooms` → `GET /v1/rooms`, `room post` → `POST /v1/rooms/{room_id}/messages`
//! (owner; `IF-CLI-001` S10).

use clap::{Args, Subcommand};
use platform_wire::{AgentId, MessageId, MessageResponse, RoomId, RoomList, RoomPostRequest};
use reqwest::Method;

use super::Context;
use crate::client::Query;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum RoomCommand {
    /// Post into any room as an allowlisted agent (owner) —
    /// POST /v1/rooms/{room_id}/messages
    Post(PostArgs),
}

#[derive(Debug, Args)]
pub struct PostArgs {
    /// `all` or a canonical `dm:<a>:<b>` room (a < b)
    room: RoomId,

    /// Agent to post as; it must be in the server's `[owner] post_as`
    #[arg(long = "as", value_name = "AGENT_ID")]
    sender: AgentId,

    /// Message body, sent byte for byte
    #[arg(long, value_name = "TEXT")]
    body: String,

    /// Message this one replies to (same room)
    #[arg(long, value_name = "MESSAGE_ID")]
    reply_to: Option<MessageId>,

    /// Idempotency key in the sender's namespace; a new ULID when omitted.
    /// Reuse it to retry safely.
    #[arg(long, value_name = "KEY")]
    client_key: Option<String>,
}

pub async fn list(context: &Context) -> Result<(), CliError> {
    let response = context
        .client
        .get::<RoomList>("/v1/rooms", &Query::new())
        .await?;
    context.show(&response, |list| output::rooms(&list.rooms))
}

pub async fn run(command: RoomCommand, context: &Context) -> Result<(), CliError> {
    match command {
        RoomCommand::Post(args) => post(args, context).await,
    }
}

async fn post(args: PostArgs, context: &Context) -> Result<(), CliError> {
    let generated = args.client_key.is_none();
    let client_key = args
        .client_key
        .unwrap_or_else(|| ulid::Ulid::generate().to_string());
    let request = RoomPostRequest {
        sender_id: args.sender.to_string(),
        body: args.body,
        client_key: client_key.clone(),
        reply_to: args.reply_to.map(String::from),
    };
    // `RoomId` and `AgentId` hold only slug characters and `:`, which need no
    // percent-encoding in a path segment.
    let path = format!("/v1/rooms/{}/messages", args.room);
    let response = context
        .client
        .send_json::<MessageResponse, _>(Method::POST, &path, &request)
        .await?;
    if context.json && generated {
        // The raw body has no client_key; keep stdout pure JSON.
        output::warn(&format!("client_key {client_key}"));
    }
    context.show(&response, |sent| output::sent(sent, &client_key))
}
