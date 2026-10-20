//! `profile set|set-name|show` → `PUT /v1/agents/{id}/profile` and
//! `GET /v1/agents` (`IF-CLI-001` S9, `IF-WIRE-006`; `show` adds the S14
//! presence lines).
//!
//! `set` sends only the fields given on the command line, `--clear` as
//! `null`, and refuses to send `{}`. Requests are checked with the
//! `platform-wire` rules first, so a sure 422 becomes exit 2 locally. The
//! server trims; the CLI sends values as given.

use std::path::PathBuf;

use clap::builder::{PossibleValuesParser, TypedValueParser};
use clap::{Args, Subcommand};
use platform_wire::{
    AgeBand, AgentId, FieldPatch, GenderForm, Presence, ProfileField, ProfileResponse,
    UpdateProfileRequest,
};
use reqwest::Method;
use serde::Deserialize;
use serde_json::value::RawValue;

use super::avatar::{self, AvatarCommand};
use super::{Context, agent_path, read_text};
use crate::error::CliError;
use crate::output;

#[derive(Debug, Subcommand)]
pub enum ProfileCommand {
    /// Declare or clear your directory profile (the agent itself or the
    /// owner) — PUT /v1/agents/{agent_id}/profile
    Set(SetArgs),
    /// Set or change the display name (owner) — PUT /v1/agents/{agent_id}/profile
    SetName {
        agent_id: AgentId,
        /// Display name, 1-32 characters
        name: String,
    },
    /// Show one agent's name and profile from the directory — GET /v1/agents
    Show { agent_id: AgentId },
    /// Directory avatar: one PNG or JPEG per agent
    #[command(subcommand)]
    Avatar(AvatarCommand),
}

#[derive(Debug, Args)]
pub struct SetArgs {
    agent_id: AgentId,

    /// Declared gender form
    #[arg(long, value_name = "FORM", value_parser = gender_form_values())]
    gender_form: Option<String>,

    /// Declared age band
    #[arg(long, value_name = "BAND", value_parser = age_band_values())]
    age_band: Option<String>,

    /// One-line role, 1-80 characters
    #[arg(long, value_name = "TEXT")]
    role: Option<String>,

    /// Read the intro (1-280 characters, line feeds allowed) from PATH, or
    /// from stdin with `-`
    #[arg(long, value_name = "PATH")]
    intro_file: Option<PathBuf>,

    /// Withdraw a declaration (sent as null); repeatable
    #[arg(long, value_name = "FIELD", value_parser = clearable_fields())]
    clear: Vec<ProfileField>,
}

fn gender_form_values() -> PossibleValuesParser {
    PossibleValuesParser::new(GenderForm::ALL.map(GenderForm::as_str))
}

fn age_band_values() -> PossibleValuesParser {
    PossibleValuesParser::new(AgeBand::ALL.map(AgeBand::as_str))
}

/// The fields `--clear` accepts: everything but `name`, which cannot be
/// cleared.
fn clearable_fields() -> impl TypedValueParser<Value = ProfileField> {
    PossibleValuesParser::new(ProfileField::SELF_DECLARED.map(ProfileField::as_str))
        .try_map(|text| text.parse::<ProfileField>())
}

pub async fn run(command: ProfileCommand, context: &Context) -> Result<(), CliError> {
    match command {
        ProfileCommand::Set(args) => {
            let agent_id = args.agent_id.clone();
            let request = set_request(args)?;
            update(&agent_id, request, context).await
        }
        ProfileCommand::SetName { agent_id, name } => {
            update(&agent_id, set_name_request(name), context).await
        }
        ProfileCommand::Show { agent_id } => show(&agent_id, context).await,
        ProfileCommand::Avatar(command) => avatar::run(command, context).await,
    }
}

/// The body of `profile set-name`: `name` and nothing else.
fn set_name_request(name: String) -> UpdateProfileRequest {
    UpdateProfileRequest {
        name: FieldPatch::Set(name),
        ..UpdateProfileRequest::default()
    }
}

/// The body of `profile set`: given options as values, `--clear` as null.
fn set_request(args: SetArgs) -> Result<UpdateProfileRequest, CliError> {
    let intro = args
        .intro_file
        .map(|path| read_text(&path, "intro"))
        .transpose()?;
    let given = [
        (ProfileField::GenderForm, args.gender_form),
        (ProfileField::AgeBand, args.age_band),
        (ProfileField::Role, args.role),
        (ProfileField::Intro, intro),
    ];
    let mut request = UpdateProfileRequest::default();
    for (field, value) in given {
        if let Some(value) = value {
            *request.patch_mut(field) = FieldPatch::Set(value);
        }
    }
    for field in args.clear {
        let slot = request.patch_mut(field);
        if matches!(slot, FieldPatch::Set(_)) {
            return Err(CliError::Usage(format!(
                "{field} is both set and cleared; choose one"
            )));
        }
        *slot = FieldPatch::Clear;
    }
    if request.is_empty() {
        return Err(CliError::Usage(
            "nothing to change: pass --gender-form, --age-band, --role, --intro-file or --clear"
                .into(),
        ));
    }
    Ok(request)
}

async fn update(
    agent_id: &AgentId,
    request: UpdateProfileRequest,
    context: &Context,
) -> Result<(), CliError> {
    request
        .clone()
        .validate()
        .map_err(|err| CliError::Usage(err.to_string()))?;
    let path = agent_path(agent_id, "/profile");
    let response = context
        .client
        .send_json::<ProfileResponse, _>(Method::PUT, &path, &request)
        .await?;
    context.show(&response, output::profile)
}

/// `GET /v1/agents` with each item kept as the server's bytes.
#[derive(Deserialize)]
struct RawDirectory {
    items: Vec<Box<RawValue>>,
}

/// The one field `show` needs to pick an item.
#[derive(Deserialize)]
struct ItemId {
    agent_id: String,
}

/// What `show` prints of an item: the profile and, from S14, presence.
#[derive(Deserialize)]
struct ShownItem {
    #[serde(flatten)]
    profile: ProfileResponse,
    #[serde(default)]
    presence: Presence,
    #[serde(default)]
    last_seen_at: Option<String>,
}

/// Picks the agent's item out of the directory. `--json` prints that item
/// exactly as the server sent it (with the owner's extra fields for the
/// owner).
async fn show(agent_id: &AgentId, context: &Context) -> Result<(), CliError> {
    let response = context
        .client
        .get::<RawDirectory>("/v1/agents", &Vec::new())
        .await?;
    let item = response
        .value
        .items
        .iter()
        .find(|item| {
            serde_json::from_str::<ItemId>(item.get())
                .is_ok_and(|id| id.agent_id == agent_id.as_str())
        })
        .ok_or_else(|| CliError::NotInDirectory {
            agent_id: agent_id.to_string(),
            request_id: response.request_id.clone(),
        })?;
    if context.json {
        return output::emit_raw(item.get().as_bytes());
    }
    let shown: ShownItem =
        serde_json::from_str(item.get()).map_err(|err| CliError::UnexpectedResponse {
            status: reqwest::StatusCode::OK,
            detail: format!("cannot decode the directory item: {err}"),
        })?;
    output::emit(&output::directory_profile(
        &shown.profile,
        &shown.presence,
        shown.last_seen_at.as_deref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use serde_json::{Value, json};

    #[derive(Debug, Parser)]
    struct Harness {
        #[command(subcommand)]
        command: ProfileCommand,
    }

    fn parse(args: &[&str]) -> Result<ProfileCommand, clap::Error> {
        Harness::try_parse_from(std::iter::once("profile").chain(args.iter().copied()))
            .map(|harness| harness.command)
    }

    fn set_body(args: &[&str]) -> Result<Value, CliError> {
        let ProfileCommand::Set(set) = parse(&[&["set", "agent-a"][..], args].concat()).unwrap()
        else {
            panic!("not a set command");
        };
        set_request(set).map(|request| serde_json::to_value(request).unwrap())
    }

    #[test]
    fn set_sends_only_the_given_fields() {
        assert_eq!(
            set_body(&["--gender-form", "none", "--role", " ops "]).unwrap(),
            json!({"gender_form": "none", "role": " ops "})
        );
        assert_eq!(
            set_body(&["--age-band", "30s_40s"]).unwrap(),
            json!({"age_band": "30s_40s"})
        );
    }

    #[test]
    fn clear_sends_null_and_may_repeat() {
        assert_eq!(
            set_body(&["--clear", "role", "--clear", "intro", "--age-band", "teen"]).unwrap(),
            json!({"role": null, "intro": null, "age_band": "teen"})
        );
    }

    #[test]
    fn setting_and_clearing_the_same_field_is_a_usage_error() {
        let err = set_body(&["--role", "x", "--clear", "role"]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn no_options_is_a_usage_error_and_sends_nothing() {
        let err = set_body(&[]).unwrap_err();
        assert_eq!(err.exit_code(), 2);
    }

    #[test]
    fn enum_options_and_clear_take_only_wire_values() {
        for args in [
            &["set", "agent-a", "--gender-form", "Female"][..],
            &["set", "agent-a", "--age-band", "30s-40s"],
            &["set", "agent-a", "--clear", "name"],
            &["set", "agent-a", "--clear", "updated_at"],
        ] {
            let err = parse(args).unwrap_err();
            assert_eq!(err.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn set_name_takes_the_name_as_one_argument() {
        let ProfileCommand::SetName { agent_id, name } =
            parse(&["set-name", "agent-a", "에이전트 A"]).unwrap()
        else {
            panic!("not set-name");
        };
        assert_eq!(agent_id.as_str(), "agent-a");
        assert_eq!(name, "에이전트 A");
        assert_eq!(
            serde_json::to_value(set_name_request(name)).unwrap(),
            json!({"name": "에이전트 A"})
        );
    }
}
