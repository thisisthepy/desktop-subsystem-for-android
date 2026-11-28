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

use super::program::ReleaseChannel;

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

/// DSA's own revision of an image, read from its version
/// (`<android>-<build id>-<revision>[-dev.<k>][-<arch>]`). The revision says the release
/// channel the image was made for:
///
/// - a stable image is `-<n>` (`17.0.0-16373615-15`);
/// - a pre-release image is `-<n>.<m>` (`17.0.0-16373615-15.1`), made on top of the stable
///   `-<n>` and before the next stable `-<n+1>`;
/// - a development build is `-<n>.<m>-dev.<k>` (or `-<n>-dev.<k>` from before the channels),
///   made for testing on one computer and never published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageRevision {
    /// The `<n>`: the stable revision the image is made on.
    pub revision: u64,
    /// The `<m>` of a pre-release (`-<n>.<m>`); 0 for a stable image.
    pub prerelease: u64,
    /// The `<k>` of a development build (`-dev.<k>`); `None` for a published image.
    pub development: Option<u64>,
}

impl ImageRevision {
    /// Whether the image is a stable release: neither a pre-release nor a development build.
    pub fn is_stable(&self) -> bool {
        self.prerelease == 0 && self.development.is_none()
    }
}

/// A whole number written in ASCII digits only.
fn whole_number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The revision an image version names ([`ImageRevision`]). `None` for a version not in the
/// form `<android>-<build id>-<revision>...`, such as a name a test made up.
pub fn image_revision_of(version: &str) -> Option<ImageRevision> {
    let mut fields = version.trim().split('-');
    let _android = fields.next()?;
    whole_number(fields.next()?)?;
    let label = fields.next()?;
    let (revision, prerelease) = match label.split_once('.') {
        Some((n, m)) => (whole_number(n)?, whole_number(m)?),
        None => (whole_number(label)?, 0),
    };
    let mut development = None;
    for field in fields {
        if let Some(k) = field.strip_prefix("dev.") {
            development = Some(whole_number(k)?);
        }
    }
    Some(ImageRevision {
        revision,
        prerelease,
        development,
    })
}

/// The number of the development build an image version names: `7` in
/// `17.0.0-16373615-15.1-dev.7` (and in `17.0.0-16373615-15-dev.7`, from before the
/// channels). The Development build workflow makes such images for testing on one computer
/// and never publishes them. `None` for a released image.
pub fn development_build_of(version: &str) -> Option<u64> {
    if let Some(revision) = image_revision_of(version) {
        return revision.development;
    }
    let (_, number) = version.trim().rsplit_once("-dev.")?;
    whole_number(number)
}

/// Whether an image version is a stable release, which a computer on the stable channel
/// takes: not a pre-release (`-<n>.<m>`) and not a development build (`-dev.<k>`). A version
/// in another form is stable unless it ends in `-dev.<k>`.
pub fn is_stable_image(version: &str) -> bool {
    match image_revision_of(version) {
        Some(revision) => revision.is_stable(),
        None => development_build_of(version).is_none(),
    }
}

/// Whether a computer on the release channel `release` takes the image `version` from a
/// published list: on the stable channel stable images only, on the pre-release channel
/// stable and pre-release images. Development builds are never published, so a list that
/// names one is not followed on either channel; they are installed from a folder instead.
pub fn channel_takes_image(release: ReleaseChannel, version: &str) -> bool {
    match release {
        ReleaseChannel::Stable => is_stable_image(version),
        ReleaseChannel::Prerelease => development_build_of(version).is_none(),
    }
}

/// The address of the pre-release image list beside the stable one at `stable`: the release
/// publishes `images-prerelease.json` beside `images.json`, and
/// `images-prerelease-arm64.json` beside `images-arm64.json`. `None` for a list of another
/// name, such as a folder a development build left, which has no pre-release list.
pub fn prerelease_image_list_url(stable: &str) -> Option<String> {
    for (name, prerelease) in [
        ("images-arm64.json", "images-prerelease-arm64.json"),
        ("images.json", "images-prerelease.json"),
    ] {
        if let Some(base) = stable.strip_suffix(name) {
            if base.is_empty() || base.ends_with(['/', '\\']) {
                return Some(format!("{base}{prerelease}"));
            }
        }
    }
    None
}

/// How images are ordered, newest last: by major version, then build (which carries the
/// stable revision `<n>`), then the pre-release `<m>` (0 for stable), and within one of those a
/// released image after every development build of it, and development builds by their
/// number. So `-15` < `-15.1-dev.k` < `-15.1` < `-15.2` < `-16`: a development build is
/// replaced by the release it leads up to, and a newer development build of the same revision
/// replaces an older one.
pub fn image_order(android_major: u32, build: u64, version: &str) -> (u32, u64, u64, u64) {
    let prerelease = image_revision_of(version).map_or(0, |revision| revision.prerelease);
    let within = development_build_of(version).map_or(u64::MAX, |n| n.min(u64::MAX - 1));
    (android_major, build, prerelease, within)
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

impl VersionReport {
    /// Whether an Android image is installed. An installer without a bundled image records the
    /// name `none` with Android 0 until the first image is downloaded and has booted; that is
    /// no Android version, and is shown as no image rather than as "Android 0".
    pub fn has_image(&self) -> bool {
        self.android_major != 0
    }
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
