//! `message show` → `GET /v1/messages/{message_id}`.

use clap::Subcommand;
use platform_wire::{Message, MessageId};

use super::Context;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum MessageCommand {
    /// Show one message — GET /v1/messages/{message_id}
    Show { message_id: MessageId },
}

pub async fn run(command: MessageCommand, context: &Context) -> Result<(), CliError> {
    let MessageCommand::Show { message_id } = command;
    // A validated `MessageId` is `msg-` plus Crockford base32: path-safe.
    let path = format!("/v1/messages/{message_id}");
    let response = context.client.get::<Message>(&path, &Vec::new()).await?;
    context.show(&response, output::message)
}
