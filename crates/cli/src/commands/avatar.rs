//! `profile avatar set|get|clear` → `PUT`, `GET` and `DELETE
//! /v1/agents/{id}/avatar` (`IF-CLI-001` S11, `IF-WIRE-008`).
//!
//! `set` reads the file, decides the `Content-Type` from its magic bytes and
//! checks the size with the `platform-wire` rules first, so a sure 413/415
//! becomes exit 2 locally. `get` writes only to a new file.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use platform_wire::avatar::check_size;
use platform_wire::{AgentId, AvatarContentType, AvatarResponse};

use super::{Context, agent_path};
use crate::error::CliError;
use crate::output::{self, SavedAvatar};

#[derive(Debug, Subcommand)]
pub enum AvatarCommand {
    /// Upload a PNG or JPEG of at most 512 KiB as the agent's avatar (the
    /// agent itself or the owner) — PUT /v1/agents/{agent_id}/avatar
    Set {
        agent_id: AgentId,
        /// Image file; its type is read from the first bytes
        file: PathBuf,
    },
    /// Save an agent's avatar to a new file — GET /v1/agents/{agent_id}/avatar
    Get {
        agent_id: AgentId,
        /// File to create; an existing file is never overwritten
        #[arg(long, value_name = "PATH")]
        out: PathBuf,
    },
    /// Remove the agent's avatar (the agent itself or the owner) —
    /// DELETE /v1/agents/{agent_id}/avatar
    Clear { agent_id: AgentId },
}

pub async fn run(command: AvatarCommand, context: &Context) -> Result<(), CliError> {
    match command {
        AvatarCommand::Set { agent_id, file } => set(&agent_id, &file, context).await,
        AvatarCommand::Get { agent_id, out } => get(&agent_id, &out, context).await,
        AvatarCommand::Clear { agent_id } => clear(&agent_id, context).await,
    }
}

fn avatar_path(agent_id: &AgentId) -> String {
    agent_path(agent_id, "/avatar")
}

/// The `Content-Type` for `bytes`, or a usage error naming `file`.
fn upload_type(file: &Path, bytes: &[u8]) -> Result<AvatarContentType, CliError> {
    let kind = AvatarContentType::sniff(bytes)
        .ok_or_else(|| CliError::Usage(format!("{} is not a PNG or JPEG image", file.display())))?;
    check_size(bytes.len()).map_err(|err| CliError::Usage(format!("{}: {err}", file.display())))?;
    Ok(kind)
}

async fn set(agent_id: &AgentId, file: &Path, context: &Context) -> Result<(), CliError> {
    let bytes = std::fs::read(file)
        .map_err(|err| CliError::io(format!("cannot read {}", file.display()), err))?;
    let kind = upload_type(file, &bytes)?;
    let response = context
        .client
        .put_bytes::<AvatarResponse>(&avatar_path(agent_id), kind.as_str(), bytes)
        .await?;
    context.show(&response, output::avatar)
}

async fn get(agent_id: &AgentId, out: &Path, context: &Context) -> Result<(), CliError> {
    // Checked before the request so a refusal costs nothing; `create_new`
    // below still guards against a file appearing in between.
    if out.exists() {
        return Err(CliError::Usage(format!(
            "{} already exists; refusing to overwrite it",
            out.display()
        )));
    }
    let response = context.client.get_bytes(&avatar_path(agent_id)).await?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(out)
        .map_err(|err| CliError::io(format!("cannot create {}", out.display()), err))?;
    file.write_all(&response.body)
        .and_then(|()| file.flush())
        .map_err(|err| CliError::io(format!("cannot write {}", out.display()), err))?;

    let saved = SavedAvatar {
        agent_id: agent_id.to_string(),
        content_type: response.header("content-type").to_owned(),
        bytes: response.body.len(),
        sha256: etag_hash(response.header("etag")).to_owned(),
        out: out.display().to_string(),
    };
    if context.json {
        let json = serde_json::to_vec(&saved)
            .map_err(|err| CliError::Usage(format!("cannot encode the result as JSON: {err}")))?;
        return output::emit_raw(&json);
    }
    output::emit(&output::avatar_saved(&saved))
}

/// The hash inside an `ETag` such as `"abc"` or `W/"abc"`.
fn etag_hash(etag: &str) -> &str {
    let strong = etag.strip_prefix("W/").unwrap_or(etag);
    strong.trim_matches('"')
}

async fn clear(agent_id: &AgentId, context: &Context) -> Result<(), CliError> {
    context.client.delete(&avatar_path(agent_id)).await?;
    if context.json {
        let json = serde_json::json!({"agent_id": agent_id.as_str(), "avatar": null});
        return output::emit_raw(json.to_string().as_bytes());
    }
    output::emit(&format!("avatar cleared: {agent_id}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use platform_wire::AVATAR_MAX_BYTES;

    const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn the_upload_type_comes_from_the_magic_bytes() {
        let file = Path::new("face.img");
        assert_eq!(
            upload_type(file, PNG_MAGIC).unwrap(),
            AvatarContentType::Png
        );
        assert_eq!(
            upload_type(file, &[0xFF, 0xD8, 0xFF, 0xE1]).unwrap(),
            AvatarContentType::Jpeg
        );
        for bytes in [&b"GIF89a"[..], &[], b"<svg/>"] {
            assert_eq!(upload_type(file, bytes).unwrap_err().exit_code(), 2);
        }
    }

    #[test]
    fn an_oversized_image_is_a_usage_error() {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize(AVATAR_MAX_BYTES, 0);
        assert!(upload_type(Path::new("a.png"), &bytes).is_ok());
        bytes.push(0);
        assert_eq!(
            upload_type(Path::new("a.png"), &bytes)
                .unwrap_err()
                .exit_code(),
            2
        );
    }

    #[test]
    fn etag_quotes_and_weak_prefix_are_removed() {
        assert_eq!(etag_hash("\"abc\""), "abc");
        assert_eq!(etag_hash("W/\"abc\""), "abc");
        assert_eq!(etag_hash(""), "");
    }
}
