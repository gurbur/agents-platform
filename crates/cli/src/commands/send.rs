//! `send` → `POST /v1/messages` (`IF-CLI-001` S13, S15): exactly one of
//! `--room`, `--to` and `--reply-to`; `--quiet`, `--mention`, `--as` and
//! `--corrects` fill the rest of the one send body. A `--reply-to` lands
//! in the original's thread; there is no option to pick a thread.

use std::path::PathBuf;

use clap::{ArgGroup, Args};
use platform_wire::{AgentId, MessageId, MessageResponse, RoomId, SendMessageRequest};
use reqwest::Method;

use super::{Context, read_text};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("destination").required(true).args(["room", "to", "reply_to"])))]
#[command(group(ArgGroup::new("body_source").required(true).args(["body", "body_file"])))]
pub struct SendArgs {
    /// A new message in this room: `all`, `dm:<a>:<b>` or `room:<slug>`
    #[arg(long, value_name = "ROOM_ID")]
    room: Option<RoomId>,

    /// A DM to this agent
    #[arg(long, value_name = "AGENT_ID")]
    to: Option<AgentId>,

    /// A reply; it lands in the room and the thread of that message
    #[arg(long, value_name = "MESSAGE_ID")]
    reply_to: Option<MessageId>,

    /// Mark this message as a correction of your own MESSAGE_ID in the same
    /// room (with one of the destinations above)
    #[arg(long, value_name = "MESSAGE_ID", requires = "destination")]
    corrects: Option<MessageId>,

    /// Message body, sent byte for byte
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,

    /// Read the body from PATH, or from stdin with `-` (not trimmed)
    #[arg(long, value_name = "PATH")]
    body_file: Option<PathBuf>,

    /// Wake no one (`"wake": false`); mentioned agents still wake
    #[arg(long)]
    quiet: bool,

    /// Name an agent who is woken whatever `--quiet` says (repeatable)
    #[arg(long = "mention", value_name = "AGENT_ID")]
    mentions: Vec<AgentId>,

    /// Post as this allowlisted agent (owner token; `[owner] post_as`)
    #[arg(long = "as", value_name = "AGENT_ID")]
    sender: Option<AgentId>,

    /// Idempotency key; a new ULID when omitted. Reuse it to retry safely.
    #[arg(long, value_name = "KEY")]
    client_key: Option<String>,
}

pub async fn run(args: SendArgs, context: &Context) -> Result<(), CliError> {
    let body = match (args.body, args.body_file) {
        (Some(body), _) => body,
        (None, Some(path)) => read_text(&path, "body")?,
        // The required `body_source` group guarantees one of the two.
        (None, None) => return Err(CliError::Usage("missing --body or --body-file".into())),
    };
    let generated = args.client_key.is_none();
    let client_key = args
        .client_key
        .unwrap_or_else(|| ulid::Ulid::generate().to_string());
    // Only the given options become keys: no `wake` without `--quiet`, no
    // `mentions` without `--mention` (the server's defaults apply).
    let request = SendMessageRequest {
        room_id: args.room.map(String::from),
        reply_to: args.reply_to.map(String::from),
        to: args.to.map(String::from),
        body,
        client_key: client_key.clone(),
        wake: args.quiet.then_some(false),
        mentions: (!args.mentions.is_empty())
            .then(|| args.mentions.into_iter().map(String::from).collect()),
        sender_id: args.sender.map(String::from),
        corrects: args.corrects.map(String::from),
    };
    let response = context
        .client
        .send_json::<MessageResponse, _>(Method::POST, "/v1/messages", &request)
        .await?;
    if context.json && generated {
        // The raw body has no client_key; keep stdout pure JSON.
        output::warn(&format!("client_key {client_key}"));
    }
    context.show(&response, |sent| output::sent(sent, &client_key))
}
