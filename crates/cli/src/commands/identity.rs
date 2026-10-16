//! `identity token` → `POST /v1/identity/tokens` (IDENTITY_ATTESTATION §10).
//!
//! Without `--json` the only stdout output is the token and one newline, so
//! `token=$(platform … identity token --aud …)` captures it as is. The CLI
//! never writes the token anywhere else.

use clap::Subcommand;
use platform_wire::{IdentityTokenRequest, IdentityTokenResponse};
use reqwest::Method;

use super::Context;
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum IdentityCommand {
    /// Get a short-lived signed identity token for a third party —
    /// POST /v1/identity/tokens (agent API key only)
    Token {
        /// Audience the token is for, e.g. garage-docs
        #[arg(long, value_name = "AUD")]
        aud: String,
        /// Lifetime in seconds; the server default when omitted
        #[arg(long, value_name = "SECS")]
        ttl: Option<u64>,
    },
}

pub async fn run(command: IdentityCommand, context: &Context) -> Result<(), CliError> {
    let IdentityCommand::Token { aud, ttl } = command;
    let request = IdentityTokenRequest { aud, ttl_secs: ttl };
    // Format errors are usage errors (exit 2) before any request is sent;
    // whether the audience is allowed is the server's decision.
    request
        .validate()
        .map_err(|err| CliError::Usage(err.to_string()))?;
    let response = context
        .client
        .send_json::<IdentityTokenResponse, _>(Method::POST, "/v1/identity/tokens", &request)
        .await?;
    context.show(&response, output::identity_token)
}
