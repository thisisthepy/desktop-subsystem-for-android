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
//! 0.1.9. A development build (the Development build workflow makes one, and never publishes
//! it) adds `-dev.<n>`: 0.2.4-dev.7 is newer than 0.2.3 and than 0.2.4-dev.6, and older than
//! 0.2.4, so the release it leads up to replaces it. A version in any other form is refused,
//! never guessed at.
//!
//! The project publishes one such list per release channel: the stable list names the newest
//! stable release, the pre-release list the newest pre-release. A computer on the stable
//! channel reads the stable list alone; one on the pre-release channel reads both and takes
//! the higher version, so a stable release newer than every pre-release reaches it too
//! ([`choose`]).

use std::cmp::Ordering;
use std::fmt;

use ed25519_dalek::VerifyingKey;
use serde::{Deserialize, Serialize};

use super::{check_signature, from_hex};

/// The program list format this build reads.
pub const FORMAT: u32 = 1;

/// Which releases of DSA a computer takes: stable releases only, or pre-releases too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReleaseChannel {
    /// Stable releases only.
    Stable,
    /// Pre-releases as well as stable releases.
    Prerelease,
}

impl ReleaseChannel {
    /// The channel's name as configuration files, package repositories and the release
    /// workflow write it: `stable` or `prerelease`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReleaseChannel::Stable => "stable",
            ReleaseChannel::Prerelease => "prerelease",
        }
    }

    /// The channel a name stands for. `pre-release` is read as `prerelease`.
    pub fn parse(text: &str) -> Result<ReleaseChannel, String> {
        match text.trim().to_ascii_lowercase().as_str() {
            "stable" => Ok(ReleaseChannel::Stable),
            "prerelease" | "pre-release" => Ok(ReleaseChannel::Prerelease),
            _ => Err(format!(
                "{text:?} is not a release channel; use stable or prerelease"
            )),
        }
    }

    /// The channel a release of `version` was published on when nothing else says: every
    /// release below 1.0 is a pre-release, and from 1.0 a release is stable unless the person
    /// releasing it chose otherwise, which the installer then records. A version that cannot
    /// be read counts as a pre-release, the channel that takes the most.
    pub fn of_version(version: &str) -> ReleaseChannel {
        match Version::parse(version) {
            Ok(version) if version >= Version::parse("1.0").expect("1.0 is a version") => {
                ReleaseChannel::Stable
            }
            _ => ReleaseChannel::Prerelease,
        }
    }

    /// The channel of the switch "take pre-releases too".
    pub fn from_prereleases(on: bool) -> ReleaseChannel {
        if on {
            ReleaseChannel::Prerelease
        } else {
            ReleaseChannel::Stable
        }
    }

    /// Whether this channel takes pre-releases.
    pub fn takes_prereleases(self) -> bool {
        self == ReleaseChannel::Prerelease
    }
}

impl fmt::Display for ReleaseChannel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ReleaseChannel::Stable => "stable",
            ReleaseChannel::Prerelease => "pre-release",
        })
    }
}

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
/// count as zero, so 0.1 and 0.1.0 are the same version. A development build's version ends
/// in `-dev.<n>`, and is older than the release of the same numbers.
#[derive(Debug, Clone)]
pub struct Version {
    parts: Vec<u64>,
    /// The `<n>` of `-dev.<n>`; `None` for a release.
    dev: Option<u64>,
    text: String,
}

impl Version {
    /// Parses `text`, or says why it is not a version this build can compare.
    pub fn parse(text: &str) -> Result<Version, String> {
        let number = |part: &str| {
            (!part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
                .then(|| part.parse::<u64>().ok())
                .flatten()
        };
        let (release, dev) = match text.split_once("-dev.") {
            Some((release, dev)) => (release, Some(number(dev))),
            None => (text, None),
        };
        let parts: Option<Vec<u64>> = release.split('.').map(number).collect();
        match (parts, dev) {
            (Some(parts), None) if !text.is_empty() => Ok(Version {
                parts,
                dev: None,
                text: text.to_string(),
            }),
            (Some(parts), Some(Some(dev))) => Ok(Version {
                parts,
                dev: Some(dev),
                text: text.to_string(),
            }),
            _ => Err(format!(
                "{text:?} is not a version of dot-separated numbers (such as 0.1.2), or of \
                 such numbers and -dev.<n> for a development build (such as 0.1.2-dev.5)"
            )),
        }
    }

    /// Whether this is a development build's version.
    pub fn is_development(&self) -> bool {
        self.dev.is_some()
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
            // A release comes after every development build of its numbers.
            .unwrap_or_else(|| match (self.dev, other.dev) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
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

/// Picks the list a computer on `channel` follows, from the lists that could be read: the
/// stable list alone on the stable channel, and on the pre-release channel whichever list
/// offers the higher version. When both offer the same version the stable list is taken, so
/// what is installed stays installable after a switch to the stable channel. Returns the list
/// and the channel it was published on, or `None` when the channel has no list to follow.
pub fn choose<'a>(
    channel: ReleaseChannel,
    stable: Option<&'a ProgramManifest>,
    prerelease: Option<&'a ProgramManifest>,
) -> Option<(ReleaseChannel, &'a ProgramManifest)> {
    let stable = stable.map(|list| (ReleaseChannel::Stable, list));
    if channel == ReleaseChannel::Stable {
        return stable;
    }
    let prerelease = prerelease.map(|list| (ReleaseChannel::Prerelease, list));
    match (stable, prerelease) {
        (Some(stable), Some(prerelease)) => {
            let version = |list: &ProgramManifest| Version::parse(&list.version).ok();
            if version(prerelease.1) > version(stable.1) {
                Some(prerelease)
            } else {
                Some(stable)
            }
        }
        (stable, prerelease) => stable.or(prerelease),
    }
}
