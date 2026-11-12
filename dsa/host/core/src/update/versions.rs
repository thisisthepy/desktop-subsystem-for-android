//! Which Android versions DSA supports, and which one a computer follows.
//!
//! The supported major versions are written once, in `dsa/android-versions.json`. The host
//! reads them from there at build time, the image release workflow builds the release list
//! from the same file, and a repository check holds the README and the image workflows to it.
//! A release list that names other versions (a newer DSA has shipped a new major) wins at run
//! time, since it is what the project publishes now.
//!
//! The image store follows a [`Channel`]: the newest supported major (the default), or one
//! major the user chose. User data only ever moves up: Android cannot read data a newer major
//! wrote, so a move to an older major is taken only with the user's consent to erase the data.

use serde::{Deserialize, Serialize};

/// `dsa/android-versions.json`, as it was when this build was made.
pub const SUPPORTED_LIST: &str = include_str!("../../../../android-versions.json");

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SupportedList {
    supported_majors: Vec<u32>,
}

/// The Android major versions this build of DSA supports, oldest first.
pub fn supported_majors() -> Vec<u32> {
    let list: SupportedList =
        serde_json::from_str(SUPPORTED_LIST).expect("android-versions.json is valid");
    let mut majors = list.supported_majors;
    majors.sort_unstable();
    majors.dedup();
    majors
}

/// The newest Android major version this build supports: what a new installation runs.
pub fn newest_major() -> u32 {
    *supported_majors()
        .last()
        .expect("at least one supported version")
}

/// Which images a computer takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// The newest supported major version, moving up when a new one ships.
    #[default]
    Latest,
    /// Only images of this major version, until it loses support.
    Major(u32),
}

impl Channel {
    /// Parses `latest` or a major version number, as `dsa android use` takes it.
    pub fn parse(text: &str) -> Option<Channel> {
        match text.trim() {
            "latest" => Some(Channel::Latest),
            number => number.parse().ok().map(Channel::Major),
        }
    }

    /// The channel as the local API carries it: 0 for the newest.
    pub fn to_wire(self) -> u32 {
        match self {
            Channel::Latest => 0,
            Channel::Major(major) => major,
        }
    }

    pub fn from_wire(major: u32) -> Channel {
        match major {
            0 => Channel::Latest,
            major => Channel::Major(major),
        }
    }
}

/// The major version `channel` leads to among `supported`: the newest for
/// [`Channel::Latest`], the chosen one while it is supported, and once it is not, the oldest
/// supported major above it (or the newest, when none is above it). `None` without any
/// supported version.
pub fn target_major(channel: Channel, supported: &[u32]) -> Option<u32> {
    let newest = supported.iter().copied().max()?;
    Some(match channel {
        Channel::Latest => newest,
        Channel::Major(major) if supported.contains(&major) => major,
        Channel::Major(major) => supported
            .iter()
            .copied()
            .filter(|m| *m > major)
            .min()
            .unwrap_or(newest),
    })
}

/// The Android major version an image version names: its leading number (`17` in
/// `17.0.0-16373615-8`). `None` for a name that does not start with one.
pub fn major_of_version(version: &str) -> Option<u32> {
    let digits: String = version
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// The number of the development build an image version names: `7` in
/// `17.0.0-16373615-15-dev.7`. The Development build workflow makes such images for testing on
/// one computer and never publishes them. `None` for a released image.
pub fn development_build_of(version: &str) -> Option<u64> {
    let (_, number) = version.trim().rsplit_once("-dev.")?;
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

/// How images are ordered, newest last: by major version, then build, and within one build a
/// released image after every development build of it, and development builds by their
/// number. A development build takes the revision after the newest released one, so the
/// release that later takes that revision replaces it, and a newer development build of the
/// same revision replaces an older one.
pub fn image_order(android_major: u32, build: u64, version: &str) -> (u32, u64, u64) {
    let within = development_build_of(version).map_or(u64::MAX, |n| n.min(u64::MAX - 1));
    (android_major, build, within)
}

/// What a user sees of the Android version: what runs, what is chosen, and what can be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionReport {
    /// The image Android starts from.
    pub version: String,
    pub android_major: u32,
    pub build: u64,
    /// The AOSP build ID the image was made from, when its `image.json` says.
    pub build_id: Option<String>,
    /// The security patch level, `YYYY-MM-DD`, when its `image.json` says.
    pub security_patch: Option<String>,
    pub channel: Channel,
    /// The supported major versions, oldest first.
    pub supported_majors: Vec<u32>,
    /// The major version the channel leads to; it differs from `android_major` while a
    /// switch waits for its image.
    pub target_major: u32,
    /// Whether the image now current has not finished its first boot yet.
    pub on_trial: bool,
    /// The latest news about the image, if any.
    pub notice: Option<String>,
    /// The image the asking user's running Android waits to restart into, if one does: one
    /// the service downloaded after that Android started.
    pub restart_pending: Option<String>,
}

/// Why a version was not chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChooseError {
    /// The major version is not one DSA supports.
    Unsupported(String),
    /// Moving to it would erase user data, and the caller did not agree to that.
    DataWouldBeLost(String),
    /// The image store could not record it.
    Store(String),
}

impl std::fmt::Display for ChooseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChooseError::Unsupported(why)
            | ChooseError::DataWouldBeLost(why)
            | ChooseError::Store(why) => f.write_str(why),
        }
    }
}

/// What lets a user see and choose the Android version, as the local API asks for it.
pub trait AndroidVersions: Send + Sync {
    /// Whether only an administrator may choose: the image is shared by every user.
    fn needs_administrator(&self) -> bool {
        true
    }
    fn report(&self) -> Result<VersionReport, String>;
    /// Chooses `channel`; `erase_data` agrees to erasing user data when it leads to an
    /// older major. The image is downloaded by the update service's next check, which this
    /// asks for at once.
    fn choose(&self, channel: Channel, erase_data: bool) -> Result<VersionReport, ChooseError>;
}
