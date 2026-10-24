//! The program list: the newest release of DSA itself, where its setup file is, and the
//! Ed25519 signature that proves the project published it.
//!
//! The list is JSON, signed as the exact bytes served, with the signature beside it in
//! hexadecimal, exactly as the image list is. Unknown keys are an error, so a list written
//! for a newer format is refused rather than half read.
//!
//! ```json
//! {
//!   "format": 1,
//!   "version": "0.1.1",
//!   "setup": {"url": "https://.../setup-0.1.1.exe", "size": 123, "sha256": "..."}
//! }
//! ```
//!
//! Versions are dot-separated numbers and compare as numbers, so 0.1.10 is newer than
//! 0.1.9. A version that is not in that form is refused, never guessed at.

use std::cmp::Ordering;
use std::fmt;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use super::{check_signature, from_hex};

/// The program list format this build reads.
pub const FORMAT: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgramManifest {
    pub format: u32,
    /// The newest release of DSA, as dot-separated numbers.
    pub version: String,
    pub setup: SetupFile,
}

/// The setup file that installs [`ProgramManifest::version`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetupFile {
    pub url: String,
    pub size: u64,
    /// SHA-256 of the file, in hexadecimal.
    pub sha256: String,
}

/// A release version: dot-separated numbers, compared as numbers. Missing trailing parts
/// count as zero, so 0.1 and 0.1.0 are the same version.
#[derive(Debug, Clone)]
pub struct Version {
    parts: Vec<u64>,
    text: String,
}

impl Version {
    /// Parses `text`, or says why it is not a version this build can compare.
    pub fn parse(text: &str) -> Result<Version, String> {
        let parts: Option<Vec<u64>> = text
            .split('.')
            .map(|part| {
                (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                    .then(|| part.parse().ok())
                    .flatten()
            })
            .collect();
        match parts {
            Some(parts) if !text.is_empty() => Ok(Version {
                parts,
                text: text.to_string(),
            }),
            _ => Err(format!(
                "{text:?} is not a version of dot-separated numbers (such as 0.1.2)"
            )),
        }
    }

    fn part(&self, i: usize) -> u64 {
        self.parts.get(i).copied().unwrap_or(0)
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        let len = self.parts.len().max(other.parts.len());
        (0..len)
            .map(|i| self.part(i).cmp(&other.part(i)))
            .find(|order| order.is_ne())
            .unwrap_or(Ordering::Equal)
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

impl Eq for Version {}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

/// Why the program was not updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramError {
    /// Program updates are turned off, or no program list is configured.
    Off(String),
    /// The program list's signature is missing or does not match a trusted key.
    BadSignature(String),
    /// The program list is not one this build can read.
    BadList(String),
    /// The offered version breaks a rule; the text says which and why.
    Refused(String),
    /// The setup file could not be downloaded, or did not match its hash or size.
    Download(String),
    /// Reading or writing the update folder failed.
    Store(String),
    /// Setup could not be started.
    Install(String),
}

impl fmt::Display for ProgramError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProgramError::Off(why) => write!(f, "DSA does not update itself here: {why}"),
            ProgramError::BadSignature(why) => {
                write!(f, "the program list is not signed by the project: {why}")
            }
            ProgramError::BadList(why) => write!(f, "the program list is not usable: {why}"),
            ProgramError::Refused(why) => write!(f, "DSA was not updated: {why}"),
            ProgramError::Download(why) => {
                write!(f, "downloading the new version of DSA failed: {why}")
            }
            ProgramError::Store(why) => write!(f, "the program update folder: {why}"),
            ProgramError::Install(why) => write!(f, "starting setup failed: {why}"),
        }
    }
}

impl std::error::Error for ProgramError {}

/// Checks `signature_hex` over `bytes` against `keys`, then parses and checks the list.
/// Nothing is parsed before the signature has been verified.
pub fn verify(
    bytes: &[u8],
    signature_hex: &str,
    keys: &[VerifyingKey],
) -> Result<ProgramManifest, ProgramError> {
    check_signature(bytes, signature_hex, keys).map_err(ProgramError::BadSignature)?;
    let list: ProgramManifest =
        serde_json::from_slice(bytes).map_err(|e| ProgramError::BadList(e.to_string()))?;
    if list.format != FORMAT {
        return Err(ProgramError::BadList(format!(
            "format {} (this build reads format {FORMAT}); install the new version of DSA by hand",
            list.format
        )));
    }
    Version::parse(&list.version).map_err(ProgramError::BadList)?;
    if from_hex::<32>(&list.setup.sha256).is_none() {
        return Err(ProgramError::BadList(format!(
            "the hash of the setup file of {} is not SHA-256 in hexadecimal",
            list.version
        )));
    }
    Ok(list)
}

/// What to do with the version a list offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramDecision {
    /// Install it: it is newer than the running version.
    Install(Version),
    /// Nothing to do; the text says what was offered and why it is not installed.
    NotNewer(String),
}

/// Decides whether to move from `running` to what `list` offers. Only a newer version is
/// ever installed; an equal or lower one is not, and the reason says so.
pub fn decide(running: &str, list: &ProgramManifest) -> Result<ProgramDecision, ProgramError> {
    let running = Version::parse(running).map_err(|why| {
        ProgramError::Refused(format!(
            "the running version cannot be compared with the list's: {why}"
        ))
    })?;
    let offered = Version::parse(&list.version).map_err(ProgramError::BadList)?;
    Ok(match offered.cmp(&running) {
        Ordering::Greater => ProgramDecision::Install(offered),
        Ordering::Equal => ProgramDecision::NotNewer(format!(
            "the program list offers DSA {offered}, which is the version installed"
        )),
        Ordering::Less => ProgramDecision::NotNewer(format!(
            "the program list offers DSA {offered}, which is older than the installed \
             {running}; DSA is never moved to a lower version"
        )),
    })
}
