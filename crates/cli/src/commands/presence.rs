//! `presence set|clear` → `PUT` and `DELETE /v1/agents/{id}/presence`
//! (`IF-CLI-001` S14, `IF-WIRE-013`).
//!
//! `set` sends only the options given. The state, the note and `--ttl` are
//! checked first, so a sure 422 becomes exit 2 locally; whether `--ttl`
//! fits the hub's maximum is the hub's call. The note goes as given (the
//! hub trims).

use clap::Subcommand;
use clap::builder::{PossibleValuesParser, TypedValueParser};
use platform_wire::{
    AgentId, Presence, PresenceRequest, PresenceResponse, PresenceState, validate_presence_note,
};
use reqwest::Method;

use super::{Context, agent_path};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum PresenceCommand {
    /// Declare that the agent is online or busy for a while (the agent
    /// itself or the owner) — PUT /v1/agents/{agent_id}/presence
    Set {
        agent_id: AgentId,
        /// `online` or `busy`; withdraw with `presence clear`
        #[arg(value_parser = declarable_states())]
        state: PresenceState,
        /// One-line note, 1-80 characters (replaces any earlier note;
        /// omitted means none)
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,
        /// Seconds until the declaration expires; omitted means the hub's
        /// default
        #[arg(long, value_name = "SECS", value_parser = clap::value_parser!(i64).range(1..))]
        ttl: Option<i64>,
    },
    /// Withdraw the declaration at once (the agent itself or the owner) —
    /// DELETE /v1/agents/{agent_id}/presence
    Clear { agent_id: AgentId },
}

fn declarable_states() -> impl TypedValueParser<Value = PresenceState> {
    PossibleValuesParser::new(PresenceState::DECLARABLE.map(PresenceState::as_str))
        .try_map(|text| PresenceState::declared(&text))
}

pub async fn run(command: PresenceCommand, context: &Context) -> Result<(), CliError> {
    match command {
        PresenceCommand::Set {
            agent_id,
            state,
            note,
            ttl,
        } => {
            let request = set_request(state, note, ttl)?;
            set(&agent_id, &request, context).await
        }
        PresenceCommand::Clear { agent_id } => clear(&agent_id, context).await,
    }
}

/// The body of `presence set`: `state`, and `note` and `ttl_secs` only when
/// given. A note that breaks the wire rules is a usage error.
fn set_request(
    state: PresenceState,
    note: Option<String>,
    ttl: Option<i64>,
) -> Result<PresenceRequest, CliError> {
    if let Some(note) = &note {
        validate_presence_note(note).map_err(|err| CliError::Usage(err.to_string()))?;
    }
    Ok(PresenceRequest {
        state: state.as_str().to_owned(),
        note,
        ttl_secs: ttl,
    })
}

async fn set(
    agent_id: &AgentId,
    request: &PresenceRequest,
    context: &Context,
) -> Result<(), CliError> {
    let response = context
        .client
        .send_json::<PresenceResponse, _>(Method::PUT, &agent_path(agent_id, "/presence"), request)
        .await?;
    context.show(&response, output::presence)
}

async fn clear(agent_id: &AgentId, context: &Context) -> Result<(), CliError> {
    context
        .client
        .delete(&agent_path(agent_id, "/presence"))
        .await?;
    if context.json {
        let json =
            serde_json::json!({"agent_id": agent_id.as_str(), "presence": Presence::offline()});
        return output::emit_raw(json.to_string().as_bytes());
    }
    output::emit(&format!("presence cleared: {agent_id}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::json;

    #[derive(Debug, Parser)]
    struct Harness {
        #[command(subcommand)]
        command: PresenceCommand,
    }

    fn parse(args: &[&str]) -> Result<PresenceCommand, clap::Error> {
        Harness::try_parse_from(std::iter::once("presence").chain(args.iter().copied()))
            .map(|harness| harness.command)
    }

    fn body(args: &[&str]) -> Result<serde_json::Value, CliError> {
        let PresenceCommand::Set {
            state, note, ttl, ..
        } = parse(&[&["set", "agent-a"][..], args].concat()).unwrap()
        else {
            panic!("not a set command");
        };
        set_request(state, note, ttl).map(|request| serde_json::to_value(request).unwrap())
    }

    #[test]
    fn set_sends_the_given_options_only() {
        assert_eq!(body(&["online"]).unwrap(), json!({"state": "online"}));
        assert_eq!(
            body(&["busy", "--note", "x", "--ttl", "60"]).unwrap(),
            json!({"state": "busy", "note": "x", "ttl_secs": 60})
        );
    }

    #[test]
    fn only_declarable_states_and_positive_ttls_parse() {
        for args in [
            &["set", "agent-a", "offline"][..],
            &["set", "agent-a", "BUSY"],
            &["set", "agent-a", "online", "--ttl", "0"],
            &["set", "agent-a", "online", "--ttl", "x"],
        ] {
            assert_eq!(parse(args).unwrap_err().exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn a_bad_note_is_a_usage_error() {
        for note in ["", " ", "a\nb"] {
            assert_eq!(
                body(&["busy", "--note", note]).unwrap_err().exit_code(),
                2,
                "{note:?}"
            );
        }
    }
}
