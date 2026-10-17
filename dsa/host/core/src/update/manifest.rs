//! The release list: which Android images exist, which major versions are supported, and
//! the Ed25519 signature that proves the project published it.
//!
//! The list is JSON, signed as the exact bytes served; the signature is 64 bytes in
//! hexadecimal, served beside it. Unknown keys are an error, so a list written for a newer
//! format is refused rather than half read.
//!
//! ```json
//! {
//!   "format": 1,
//!   "supported_majors": [15, 16],
//!   "images": [
//!     {
//!       "version": "16.0.3",
//!       "android_major": 16,
//!       "build": 2026101500,
//!       "files": [
//!         {"name": "system.vhdx", "url": "https://...", "size": 123, "sha256": "..."}
//!       ]
//!     }
//!   ]
//! }
//! ```

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use super::{check_signature, from_hex, UpdateError};

/// The release list format this build reads.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    /// The Android major versions the project supports: the latest and the one before.
    pub supported_majors: Vec<u32>,
    pub images: Vec<ImageEntry>,
}

/// One Android image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImageEntry {
    /// Names the image in messages and on disk. Letters, digits, `.`, `-` and `_` only.
    pub version: String,
    pub android_major: u32,
    /// Orders images of the same major version; higher is newer.
    pub build: u64,
    pub files: Vec<FileEntry>,
}

/// One file of an image.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    /// The file's name in the image folder. Letters, digits, `.`, `-` and `_` only.
    pub name: String,
    pub url: String,
    pub size: u64,
    /// SHA-256 of the file, in hexadecimal.
    pub sha256: String,
}

/// Whether `name` is safe as a single path component.
pub(crate) fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// Parses a trusted public key: 32 bytes in hexadecimal.
pub fn parse_key(hex: &str) -> Result<VerifyingKey, UpdateError> {
    let bytes = from_hex::<32>(hex).ok_or_else(|| {
        UpdateError::BadSignature(format!("{hex:?} is not a 32-byte key in hexadecimal"))
    })?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|e| UpdateError::BadSignature(format!("{hex:?} is not an Ed25519 key: {e}")))
}

/// Checks `signature_hex` over `bytes` against `keys`, then parses and checks the list.
/// Nothing is parsed before the signature has been verified.
pub fn verify(
    bytes: &[u8],
    signature_hex: &str,
    keys: &[VerifyingKey],
) -> Result<Manifest, UpdateError> {
    check_signature(bytes, signature_hex, keys).map_err(UpdateError::BadSignature)?;
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|e| UpdateError::BadManifest(e.to_string()))?;
    if manifest.format != FORMAT {
        return Err(UpdateError::BadManifest(format!(
            "format {} (this build reads format {FORMAT}); update DSA itself first",
            manifest.format
        )));
    }
    for image in &manifest.images {
        if !is_plain_name(&image.version) {
            return Err(UpdateError::BadManifest(format!(
                "the image version {:?} is not a plain name",
                image.version
            )));
        }
        for file in &image.files {
            if !is_plain_name(&file.name) {
                return Err(UpdateError::BadManifest(format!(
                    "the file name {:?} in {} is not a plain name",
                    file.name, image.version
                )));
            }
            if from_hex::<32>(&file.sha256).is_none() {
                return Err(UpdateError::BadManifest(format!(
                    "the hash of {} in {} is not SHA-256 in hexadecimal",
                    file.name, image.version
                )));
            }
        }
    }
    Ok(manifest)
}
