//! `send` → `POST /v1/messages`.

use std::io::Read;
use std::path::{Path, PathBuf};

use clap::{ArgGroup, Args};
use platform_wire::{MessageId, MessageResponse, Recipient, SendMessageRequest};
use reqwest::Method;

use super::Context;
use crate::error::CliError;
use crate::output;

/// `--body-file` value that means standard input.
const STDIN_PATH: &str = "-";

#[derive(Debug, Args)]
#[command(group(ArgGroup::new("body_source").required(true).args(["body", "body_file"])))]
pub struct SendArgs {
    /// `all` or the recipient agent_id
    #[arg(long, value_name = "all|AGENT_ID")]
    to: Recipient,

    /// Message body, sent byte for byte
    #[arg(long, value_name = "TEXT")]
    body: Option<String>,

    /// Read the body from PATH, or from stdin with `-` (not trimmed)
    #[arg(long, value_name = "PATH")]
    body_file: Option<PathBuf>,

    /// Idempotency key; a new ULID when omitted. Reuse it to retry safely.
    #[arg(long, value_name = "KEY")]
    client_key: Option<String>,

    /// Message this one replies to (same room)
    #[arg(long, value_name = "MESSAGE_ID")]
    reply_to: Option<MessageId>,
}

pub async fn run(args: SendArgs, context: &Context) -> Result<(), CliError> {
    let body = match (args.body, args.body_file) {
        (Some(body), _) => body,
        (None, Some(path)) => read_body(&path)?,
        // The required `body_source` group guarantees one of the two.
        (None, None) => return Err(CliError::Usage("missing --body or --body-file".into())),
    };
    let generated = args.client_key.is_none();
    let client_key = args
        .client_key
        .unwrap_or_else(|| ulid::Ulid::generate().to_string());
    let request = SendMessageRequest {
        to: args.to.to_string(),
        body,
        client_key: client_key.clone(),
        reply_to: args.reply_to.map(String::from),
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

fn read_body(path: &Path) -> Result<String, CliError> {
    let (bytes, source) = if path == Path::new(STDIN_PATH) {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|err| CliError::io("cannot read the body from stdin", err))?;
        (bytes, "stdin".to_owned())
    } else {
        let bytes = std::fs::read(path)
            .map_err(|err| CliError::io(format!("cannot read {}", path.display()), err))?;
        (bytes, path.display().to_string())
    };
    String::from_utf8(bytes)
        .map_err(|_| CliError::Usage(format!("the body from {source} is not valid UTF-8")))
}
