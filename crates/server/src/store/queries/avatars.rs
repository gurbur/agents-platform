//! Directory avatars (`IF-DB-001` v3 table `agent_avatars`): one image per
//! agent, replaced in place and removed by `DELETE`. The bytes arrive
//! already validated by `platform_wire::validate_avatar`.

use platform_wire::{AgentId, Avatar, AvatarContentType};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::agents::agent_exists;
use crate::store::StoreError;

/// A validated image ready to store.
pub struct NewAvatar {
    pub content_type: AvatarContentType,
    pub bytes: Vec<u8>,
    /// SHA-256 of `bytes`, lowercase hex.
    pub sha256: String,
}

/// A stored image with its description, as `GET …/avatar` serves it.
pub struct StoredAvatar {
    pub avatar: Avatar,
    pub bytes: Vec<u8>,
}

/// What a lookup or a delete found.
pub enum AvatarLookup<T> {
    NoAgent,
    NoAvatar,
    Found(T),
}

/// Stores `image` as the agent's avatar. When the stored image already has
/// the same type and hash nothing is written and its `updated_at` stays;
/// otherwise the row is inserted or replaced with `updated_at = now`.
/// `None` when the agent does not exist.
pub fn put_avatar(
    tx: &Transaction,
    agent_id: &AgentId,
    image: &NewAvatar,
    now: &str,
) -> Result<Option<Avatar>, StoreError> {
    if !agent_exists(tx, agent_id)? {
        return Ok(None);
    }
    if let Some(current) = describe(tx, agent_id)?
        && current.content_type == image.content_type
        && current.sha256 == image.sha256
    {
        return Ok(Some(current));
    }
    tx.execute(
        "INSERT INTO agent_avatars (agent_id, content_type, bytes, sha256, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (agent_id) DO UPDATE SET
             content_type = excluded.content_type,
             bytes = excluded.bytes,
             sha256 = excluded.sha256,
             updated_at = excluded.updated_at",
        params![
            agent_id.as_str(),
            image.content_type.as_str(),
            image.bytes,
            image.sha256,
            now,
        ],
    )?;
    Ok(Some(Avatar {
        content_type: image.content_type,
        bytes: byte_len(image.bytes.len()),
        sha256: image.sha256.clone(),
        updated_at: now.to_owned(),
    }))
}

/// The agent's avatar with its bytes.
pub fn get_avatar(
    conn: &Connection,
    agent_id: &AgentId,
) -> Result<AvatarLookup<StoredAvatar>, StoreError> {
    let row: Option<(String, Vec<u8>, String, String)> = conn
        .query_row(
            "SELECT content_type, bytes, sha256, updated_at FROM agent_avatars WHERE agent_id = ?1",
            params![agent_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let Some((content_type, bytes, sha256, updated_at)) = row else {
        return missing(conn, agent_id);
    };
    let avatar = Avatar {
        content_type: stored_content_type(&content_type)?,
        bytes: byte_len(bytes.len()),
        sha256,
        updated_at,
    };
    Ok(AvatarLookup::Found(StoredAvatar { avatar, bytes }))
}

/// Removes the agent's avatar and returns what was removed.
pub fn delete_avatar(
    tx: &Transaction,
    agent_id: &AgentId,
) -> Result<AvatarLookup<Avatar>, StoreError> {
    let Some(removed) = describe(tx, agent_id)? else {
        return missing(tx, agent_id);
    };
    tx.execute(
        "DELETE FROM agent_avatars WHERE agent_id = ?1",
        params![agent_id.as_str()],
    )?;
    Ok(AvatarLookup::Found(removed))
}

/// The stored description without reading the bytes column.
fn describe(conn: &Connection, agent_id: &AgentId) -> Result<Option<Avatar>, StoreError> {
    let row: Option<(String, i64, String, String)> = conn
        .query_row(
            "SELECT content_type, length(bytes), sha256, updated_at
             FROM agent_avatars WHERE agent_id = ?1",
            params![agent_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    row.map(|(content_type, length, sha256, updated_at)| {
        Ok(Avatar {
            content_type: stored_content_type(&content_type)?,
            bytes: u64::try_from(length).map_err(|_| {
                StoreError::Invariant("agent_avatars.bytes has a negative length".to_owned())
            })?,
            sha256,
            updated_at,
        })
    })
    .transpose()
}

/// Tells "no such agent" from "no avatar" once the avatar row is absent.
fn missing<T>(conn: &Connection, agent_id: &AgentId) -> Result<AvatarLookup<T>, StoreError> {
    if agent_exists(conn, agent_id)? {
        Ok(AvatarLookup::NoAvatar)
    } else {
        Ok(AvatarLookup::NoAgent)
    }
}

/// Values are written only from a validated wire type, so anything else is
/// a broken invariant.
fn stored_content_type(stored: &str) -> Result<AvatarContentType, StoreError> {
    AvatarContentType::from_header(stored).ok_or_else(|| {
        StoreError::Invariant("agent_avatars.content_type holds an unknown value".to_owned())
    })
}

fn byte_len(len: usize) -> u64 {
    // usize is at most 64 bits on every supported target.
    len as u64
}
