//! `react`, `unreact`, `react --list` and `reaction-kind set` →
//! `PUT`/`DELETE /v1/messages/{message_id}/reactions/{kind}`,
//! `GET /v1/reaction-kinds` and `PUT /v1/reaction-kinds/{slug}`
//! (`IF-CLI-001` S16).
//!
//! The message id, the kind slug, a glyph and a label are checked with
//! `platform-wire` first, so a sure 422 becomes exit 2 locally. Whether a
//! kind exists or is enabled is the hub's call. The label goes as given
//! (the hub trims).

use clap::{Args, Subcommand};
use platform_wire::{
    AgentId, MessageId, PutReactionKindRequest, ReactRequest, ReactResponse, ReactionKind,
    ReactionKindEntry, ReactionKindList, validate_glyph, validate_reaction_label,
};
use reqwest::{Method, StatusCode};
use serde::Serialize;

use super::Context;
use crate::client::Query;
use crate::error::CliError;
use crate::output;

const KINDS_PATH: &str = "/v1/reaction-kinds";

#[derive(Debug, Args)]
pub struct ReactArgs {
    /// The message to react to
    #[arg(required_unless_present = "list", conflicts_with = "list")]
    message_id: Option<MessageId>,

    /// A kind slug from `react --list`, e.g. `thumbsup`
    #[arg(required_unless_present = "list", conflicts_with = "list")]
    kind: Option<ReactionKind>,

    /// Wake the message's author (reactions are quiet by default)
    #[arg(long, conflicts_with = "list")]
    wake: bool,

    /// The allowlisted identity to react as (owner token; `[owner] post_as`)
    #[arg(long = "as", value_name = "AGENT_ID", conflicts_with = "list")]
    agent: Option<AgentId>,

    /// List the reaction kinds instead — GET /v1/reaction-kinds
    #[arg(long)]
    list: bool,

    /// With --list: disabled kinds too (owner)
    #[arg(long, requires = "list")]
    all: bool,
}

#[derive(Debug, Args)]
pub struct UnreactArgs {
    message_id: MessageId,
    kind: ReactionKind,

    /// The allowlisted identity to remove the reaction of (owner token)
    #[arg(long = "as", value_name = "AGENT_ID")]
    agent: Option<AgentId>,
}

#[derive(Debug, Subcommand)]
pub enum ReactionKindCommand {
    /// Create or update a reaction kind (owner) — PUT /v1/reaction-kinds/{slug}
    Set {
        /// `^[a-z][a-z0-9_]{1,31}$`
        slug: ReactionKind,
        /// The emoji shown for the kind: 1-4 characters, no spaces
        #[arg(long, value_name = "GLYPH")]
        glyph: String,
        /// A short description, 1-32 characters after trimming
        #[arg(long, value_name = "TEXT")]
        label: String,
        /// Display order (ascending); a new kind without one goes last
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(i64).range(0..))]
        sort: Option<i64>,
        /// `false` stops new reactions of this kind; existing ones stay
        #[arg(long, value_name = "true|false")]
        enabled: Option<bool>,
    },
}

/// `unreact --json` output: what was removed.
#[derive(Serialize)]
struct Unreacted<'a> {
    message_id: &'a MessageId,
    kind: &'a ReactionKind,
    removed: bool,
}

pub async fn react(args: ReactArgs, context: &Context) -> Result<(), CliError> {
    if args.list {
        return list(args.all, context).await;
    }
    // clap makes both positionals required without --list.
    let (Some(message_id), Some(kind)) = (args.message_id, args.kind) else {
        return Err(CliError::Usage(
            "react needs a message id and a kind, or --list".into(),
        ));
    };
    let request = ReactRequest {
        wake: args.wake.then_some(true),
        sender_id: args.agent.map(String::from),
    };
    let response = context
        .client
        .send_json::<ReactResponse, _>(Method::PUT, &reaction_path(&message_id, &kind), &request)
        .await?;
    let status = if response.status == StatusCode::CREATED {
        "created"
    } else {
        "existing"
    };
    context.show(&response, |reacted| output::reacted(reacted, status))
}

pub async fn unreact(args: UnreactArgs, context: &Context) -> Result<(), CliError> {
    let path = reaction_path(&args.message_id, &args.kind);
    match args.agent {
        // An agent's body is not read; send none.
        None => context.client.delete(&path).await?,
        Some(agent) => {
            let request = ReactRequest {
                wake: None,
                sender_id: Some(agent.into()),
            };
            context.client.delete_json(&path, &request).await?
        }
    };
    if context.json {
        let removed = Unreacted {
            message_id: &args.message_id,
            kind: &args.kind,
            removed: true,
        };
        // Wire ids serialize without failure.
        let body = serde_json::to_vec(&removed).expect("unreact output serializes");
        return output::emit_raw(&body);
    }
    output::emit(&format!(
        "unreacted: {} on {}\n",
        args.kind, args.message_id
    ))
}

pub async fn run_kind(command: ReactionKindCommand, context: &Context) -> Result<(), CliError> {
    let ReactionKindCommand::Set {
        slug,
        glyph,
        label,
        sort,
        enabled,
    } = command;
    let request = kind_request(glyph, label, sort, enabled)?;
    let response = context
        .client
        .send_json::<ReactionKindEntry, _>(Method::PUT, &format!("{KINDS_PATH}/{slug}"), &request)
        .await?;
    let status = if response.status == StatusCode::CREATED {
        "created"
    } else {
        "updated"
    };
    context.show(&response, |entry| output::reaction_kind(entry, status))
}

async fn list(all: bool, context: &Context) -> Result<(), CliError> {
    let mut query = Query::new();
    if all {
        query.push(("all", "true".to_owned()));
    }
    let response = context
        .client
        .get::<ReactionKindList>(KINDS_PATH, &query)
        .await?;
    context.show(&response, |list| output::reaction_kinds(&list.kinds))
}

/// The body of `reaction-kind set`: glyph and label always, `sort` and
/// `enabled` only when given. A glyph or label that breaks the wire rules
/// is a usage error.
fn kind_request(
    glyph: String,
    label: String,
    sort: Option<i64>,
    enabled: Option<bool>,
) -> Result<PutReactionKindRequest, CliError> {
    validate_glyph(&glyph).map_err(|err| CliError::Usage(err.to_string()))?;
    validate_reaction_label(&label).map_err(|err| CliError::Usage(err.to_string()))?;
    Ok(PutReactionKindRequest {
        glyph,
        label,
        sort,
        enabled,
    })
}

/// A validated `MessageId` is `msg-` plus Crockford base32 and a
/// `ReactionKind` lower-case ASCII: both path-safe.
fn reaction_path(message_id: &MessageId, kind: &ReactionKind) -> String {
    format!("/v1/messages/{message_id}/reactions/{kind}")
}
