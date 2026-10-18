//! Directory avatar: one PNG or JPEG per agent (API_V1 "Directory avatar",
//! `IF-WIRE-008`), and the owner's identity list (`GET /v1/owner`).
//!
//! This module is the one place that decides whether bytes are an
//! acceptable avatar: the declared `Content-Type` must be PNG or JPEG, the
//! leading bytes (magic) must match it, and the size must stay within
//! [`AVATAR_MAX_BYTES`]. The image itself is never decoded. Hashing is left
//! to the callers so this crate keeps its minimal dependencies.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::AgentId;

/// Largest accepted avatar in bytes (512 KiB). A contract constant, not a
/// tuning value: tests derive their boundaries from it.
pub const AVATAR_MAX_BYTES: usize = 512 * 1024;

const PNG_MAGIC: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_MAGIC: &[u8] = &[0xFF, 0xD8, 0xFF];

/// The two accepted image formats, serialized as their media type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AvatarContentType {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
}

impl AvatarContentType {
    pub const ALL: [AvatarContentType; 2] = [AvatarContentType::Png, AvatarContentType::Jpeg];

    /// The canonical (lowercase) media type.
    pub fn as_str(self) -> &'static str {
        match self {
            AvatarContentType::Png => "image/png",
            AvatarContentType::Jpeg => "image/jpeg",
        }
    }

    /// The format a `Content-Type` header value declares: its essence (the
    /// part before `;`, trimmed) compared case-insensitively.
    pub fn from_header(value: &str) -> Option<Self> {
        let essence = value.split(';').next().unwrap_or_default().trim();
        Self::ALL
            .into_iter()
            .find(|kind| essence.eq_ignore_ascii_case(kind.as_str()))
    }

    /// The format whose magic `bytes` start with.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| bytes.starts_with(kind.magic()))
    }

    fn magic(self) -> &'static [u8] {
        match self {
            AvatarContentType::Png => PNG_MAGIC,
            AvatarContentType::Jpeg => JPEG_MAGIC,
        }
    }
}

impl fmt::Display for AvatarContentType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why bytes are not an acceptable avatar. The server answers the first
/// two with 415 `unsupported_media_type` and the last with 413
/// `avatar_too_large`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AvatarError {
    #[error("Content-Type must be image/png or image/jpeg")]
    UnsupportedType,
    #[error("the image bytes are not {0}")]
    MagicMismatch(AvatarContentType),
    #[error("the image is {size} bytes; at most {AVATAR_MAX_BYTES} are allowed")]
    TooLarge { size: usize },
}

/// Checks a declared `Content-Type` and the bytes against the avatar rules
/// in `IF-WIRE-008` order: type, size, magic.
pub fn validate_avatar(
    content_type: Option<&str>,
    bytes: &[u8],
) -> Result<AvatarContentType, AvatarError> {
    let kind = content_type
        .and_then(AvatarContentType::from_header)
        .ok_or(AvatarError::UnsupportedType)?;
    check_size(bytes.len())?;
    if !bytes.starts_with(kind.magic()) {
        return Err(AvatarError::MagicMismatch(kind));
    }
    Ok(kind)
}

/// `Err` when `size` exceeds [`AVATAR_MAX_BYTES`].
pub fn check_size(size: usize) -> Result<(), AvatarError> {
    if size > AVATAR_MAX_BYTES {
        return Err(AvatarError::TooLarge { size });
    }
    Ok(())
}

/// A stored avatar's description: `PUT …/avatar` 200 and the CLI output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Avatar {
    pub content_type: AvatarContentType,
    /// Size in bytes.
    pub bytes: u64,
    /// SHA-256 of the bytes, lowercase hex; also the `ETag`.
    pub sha256: String,
    pub updated_at: String,
}

impl Avatar {
    pub fn summary(&self) -> AvatarSummary {
        AvatarSummary {
            sha256: self.sha256.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

/// The `avatar` of a directory item: enough to tell whether a cached copy
/// is current.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarSummary {
    pub sha256: String,
    pub updated_at: String,
}

/// `PUT /v1/agents/{agent_id}/avatar` 200 body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvatarResponse {
    pub agent_id: AgentId,
    pub avatar: Avatar,
}

/// `GET /v1/owner` body: the `[owner] post_as` allowlist in configuration
/// order. The console speaks as the first entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerInfo {
    pub post_as: Vec<AgentId>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn png(len: usize) -> Vec<u8> {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize(len.max(PNG_MAGIC.len()), 0);
        bytes
    }

    fn jpeg(len: usize) -> Vec<u8> {
        let mut bytes = JPEG_MAGIC.to_vec();
        bytes.resize(len.max(JPEG_MAGIC.len()), 0);
        bytes
    }

    #[test]
    fn header_essence_is_matched_case_insensitively() {
        assert_eq!(
            AvatarContentType::from_header("image/png"),
            Some(AvatarContentType::Png)
        );
        assert_eq!(
            AvatarContentType::from_header(" Image/JPEG ; charset=binary"),
            Some(AvatarContentType::Jpeg)
        );
        for other in [
            "image/jpg",
            "image/gif",
            "application/json",
            "",
            "image/png+x",
        ] {
            assert_eq!(AvatarContentType::from_header(other), None, "{other}");
        }
    }

    #[test]
    fn magic_bytes_decide_the_sniffed_format() {
        assert_eq!(
            AvatarContentType::sniff(&png(20)),
            Some(AvatarContentType::Png)
        );
        assert_eq!(
            AvatarContentType::sniff(&jpeg(20)),
            Some(AvatarContentType::Jpeg)
        );
        assert_eq!(AvatarContentType::sniff(b"GIF89a"), None);
        assert_eq!(AvatarContentType::sniff(&PNG_MAGIC[..7]), None);
        assert_eq!(AvatarContentType::sniff(&[]), None);
    }

    #[test]
    fn validation_checks_type_then_size_then_magic() {
        assert_eq!(
            validate_avatar(Some("image/png"), &png(64)),
            Ok(AvatarContentType::Png)
        );
        assert_eq!(
            validate_avatar(Some("image/jpeg"), &jpeg(AVATAR_MAX_BYTES)),
            Ok(AvatarContentType::Jpeg)
        );
        assert_eq!(
            validate_avatar(None, &png(64)),
            Err(AvatarError::UnsupportedType)
        );
        assert_eq!(
            validate_avatar(Some("image/gif"), &vec![0; AVATAR_MAX_BYTES + 1]),
            Err(AvatarError::UnsupportedType)
        );
        assert_eq!(
            validate_avatar(Some("image/png"), &jpeg(AVATAR_MAX_BYTES + 1)),
            Err(AvatarError::TooLarge {
                size: AVATAR_MAX_BYTES + 1
            })
        );
        assert_eq!(
            validate_avatar(Some("image/png"), &jpeg(64)),
            Err(AvatarError::MagicMismatch(AvatarContentType::Png))
        );
        assert_eq!(
            validate_avatar(Some("image/jpeg"), &[]),
            Err(AvatarError::MagicMismatch(AvatarContentType::Jpeg))
        );
    }

    #[test]
    fn content_type_serializes_as_the_media_type() {
        assert_eq!(
            serde_json::to_value(AvatarContentType::Jpeg).unwrap(),
            json!("image/jpeg")
        );
        let avatar: Avatar = serde_json::from_value(json!({
            "content_type": "image/png", "bytes": 12, "sha256": "ab", "updated_at": "t"
        }))
        .unwrap();
        assert_eq!(avatar.content_type, AvatarContentType::Png);
        assert_eq!(
            serde_json::to_value(avatar.summary()).unwrap(),
            json!({"sha256": "ab", "updated_at": "t"})
        );
    }
}
