//! Updating the Android image: a signed list of images, the rules for choosing one, the
//! store that stages it beside the running image, and the trial boot that keeps it or
//! goes back.
//!
//! - [`manifest`]: the release list and its Ed25519 signature. Nothing in an unsigned or
//!   wrongly signed list is ever used.
//! - [`policy`]: which image to move to. Only supported Android versions, never a lower
//!   major version (Android cannot move user data back), and never a build that already
//!   failed to boot here.
//! - [`store`]: images side by side on disk. A new image is downloaded and checked against
//!   its hash before it becomes current, and the previous one is kept until the new one
//!   has booted.
//! - [`trial`]: boots a newly staged image and, when it does not finish booting in time,
//!   goes back to the previous image and starts that instead.
//!
//! - [`service`]: the periodic check and a user's Android that boots a new image on trial.
//!
//! Updating DSA itself follows the same pattern:
//!
//! - [`program`]: the signed program list, which names the newest release and its setup
//!   file, and the rule that only a newer version is ever installed.
//! - [`program_service`]: downloading that setup file, running it once no user's Android
//!   is running (or at once when a user asks), and telling the users how it went.
//!
//! Downloading is behind [`store::Fetch`]; the platform crates supply HTTPS.

pub mod manifest;
pub mod policy;
pub mod program;
pub mod program_service;
pub mod service;
pub mod store;
pub mod trial;

use std::fmt;

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Why an update was not applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// The release list's signature is missing or does not match a trusted key.
    BadSignature(String),
    /// The release list is not one this build can read.
    BadManifest(String),
    /// The offered image breaks a rule; the text says which and why.
    Refused(String),
    /// A download failed or did not match its hash or size.
    Download(String),
    /// Reading or writing the image store failed.
    Store(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::BadSignature(why) => {
                write!(f, "the release list is not signed by the project: {why}")
            }
            UpdateError::BadManifest(why) => write!(f, "the release list is not usable: {why}"),
            UpdateError::Refused(why) => write!(f, "the update was not applied: {why}"),
            UpdateError::Download(why) => write!(f, "downloading the update failed: {why}"),
            UpdateError::Store(why) => write!(f, "the image store: {why}"),
        }
    }
}

impl std::error::Error for UpdateError {}

/// Lower-case hexadecimal.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses lower- or upper-case hexadecimal of exactly `N` bytes.
pub(crate) fn from_hex<const N: usize>(text: &str) -> Option<[u8; N]> {
    let text = text.trim();
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// Checks `signature_hex` (64 bytes in hexadecimal) over `bytes` against `keys`. The error
/// says why in words, for the caller to say which list it was.
pub(crate) fn check_signature(
    bytes: &[u8],
    signature_hex: &str,
    keys: &[VerifyingKey],
) -> Result<(), String> {
    if keys.is_empty() {
        return Err("no trusted key is configured".into());
    }
    let signature = from_hex::<64>(signature_hex)
        .map(|bytes| Signature::from_bytes(&bytes))
        .ok_or("the signature is not 64 bytes in hexadecimal")?;
    if !keys.iter().any(|key| key.verify(bytes, &signature).is_ok()) {
        return Err("the signature does not match any trusted key".into());
    }
    Ok(())
}
