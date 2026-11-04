//! The parts of the host's configuration file that are the same on every host: the update
//! settings, the default values, the rules that decide whether a file is usable, and the
//! placeholders an installer fills in.
//!
//! The file itself, with the keys only one host has (its VM's sockets, its pipe, how it finds
//! a user's profile), is read by each platform crate, which takes its defaults and its rules
//! from here so every host reads the same file the same way.

use std::fmt;
use std::path::{Path, PathBuf};

use ed25519_dalek::VerifyingKey;
use serde::Deserialize;

use crate::update::manifest;
use crate::update::policy::InstalledImage;

/// The VM's memory when the configuration names none and the user set no limit, in MiB.
pub const DEFAULT_MEMORY_MB: u32 = 3072;

/// Whether "automatic" sizes the VM to the computer (its memory less what the host keeps, see
/// [`automatic_vm_memory_mb`]) rather than [`DEFAULT_MEMORY_MB`]. Off for now: the guest
/// kernel keeps its page records for the VM's whole maximum resident, about 1.6% of it (some
/// 460 MiB for a 29.5 GiB VM), so a large maximum raises idle memory until that cost is
/// measured and accepted.
///
/// [`automatic_vm_memory_mb`]: crate::memory::automatic_vm_memory_mb
pub const SIZE_TO_COMPUTER: bool = false;

/// The VM's memory in MiB for a user whose limit is `limit_mb` (0 for automatic), on a
/// computer with `total_mb` of memory when that is known: their limit, never more than the
/// computer has, else [`DEFAULT_MEMORY_MB`] (or the computer's size, with
/// [`SIZE_TO_COMPUTER`]).
pub fn sized_memory_mb(limit_mb: u32, total_mb: Option<u64>) -> u32 {
    if limit_mb == 0 && !SIZE_TO_COMPUTER {
        return DEFAULT_MEMORY_MB;
    }
    match total_mb {
        Some(total_mb) => {
            let sized = crate::memory::vm_memory_mb(total_mb, limit_mb);
            u32::try_from(sized).unwrap_or(u32::MAX)
        }
        None if limit_mb > 0 => limit_mb,
        None => DEFAULT_MEMORY_MB,
    }
}

pub fn default_vm_name() -> String {
    "dsa-android".into()
}
pub fn default_processors() -> u32 {
    4
}
pub fn default_control_port() -> u32 {
    5000
}
pub fn default_log_port() -> u32 {
    5001
}
pub fn default_connect_timeout_secs() -> u64 {
    120
}
pub fn default_stop_timeout_secs() -> u64 {
    30
}
pub fn default_data_disk_size_gb() -> u32 {
    64
}
pub fn default_ready_timeout_secs() -> u64 {
    crate::session::DEFAULT_READY_TIMEOUT.as_secs()
}
pub fn default_doze_delay_secs() -> u64 {
    crate::session::DEFAULT_DOZE_DELAY.as_secs()
}
pub fn default_check_interval_hours() -> u64 {
    24
}
pub fn default_boot_timeout_secs() -> u64 {
    300
}
pub fn default_true() -> bool {
    true
}

/// Where new images are published and who may sign them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateSettings {
    /// The signed release list. Its signature is at the same URL plus `.sig`.
    pub list_url: String,
    /// Public keys, 32 bytes in hexadecimal, any of which may sign the list.
    pub trusted_keys: Vec<String>,
    #[serde(default = "default_check_interval_hours")]
    pub check_interval_hours: u64,
    /// How long a new image may take to finish its first boot before it is undone.
    #[serde(default = "default_boot_timeout_secs")]
    pub boot_timeout_secs: u64,
    /// The signed program list, for updating DSA itself. Its signature is at the same URL
    /// plus `.sig`, and the same keys may sign it. Without it DSA is not updated.
    #[serde(default)]
    pub program_list_url: Option<String>,
    /// False turns updating DSA itself off; the program list is then never fetched.
    #[serde(default = "default_true")]
    pub program: bool,
    /// The service's own folder for downloaded setup files.
    #[serde(default)]
    pub program_dir: Option<PathBuf>,
}

impl UpdateSettings {
    pub fn keys(&self) -> Result<Vec<VerifyingKey>, HostConfigError> {
        self.trusted_keys
            .iter()
            .map(|key| manifest::parse_key(key).map_err(|e| HostConfigError(e.to_string())))
            .collect()
    }
}

/// Why a configuration file could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostConfigError(pub String);

impl fmt::Display for HostConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the host configuration is not usable: {}", self.0)
    }
}

impl std::error::Error for HostConfigError {}

/// Checks the keys that decide where the Android image comes from and how it is updated: an
/// image or an update store, the first image in that store, and a folder for setup files
/// when DSA updates itself.
pub fn check_image_sources(
    system_image: Option<&Path>,
    images_dir: Option<&Path>,
    first_image: Option<&InstalledImage>,
    update: Option<&UpdateSettings>,
) -> Result<(), HostConfigError> {
    match (system_image, images_dir, first_image) {
        (None, None, _) => {
            return Err(HostConfigError(
                "name either system_image or images_dir (with updates)".into(),
            ))
        }
        (_, Some(_), None) => {
            return Err(HostConfigError(
                "images_dir needs first_image, the image the installer put there".into(),
            ))
        }
        _ => {}
    }
    if let Some(update) = update {
        if images_dir.is_none() {
            return Err(HostConfigError("update needs images_dir".into()));
        }
        update.keys()?;
        if update.program_list_url.is_some() && update.program_dir.is_none() {
            return Err(HostConfigError(
                "program_list_url needs program_dir, the folder setup files are \
                 downloaded to"
                    .into(),
            ));
        }
    }
    Ok(())
}

/// Replaces `{data}` and `{program}` in the configuration file's text with the folders the
/// installer put the data and the program in. The user chooses both while installing, so
/// the file cannot name them when it is built.
pub fn expand_locations(text: &str, data: &Path, program: &Path) -> String {
    let json = |path: &Path| {
        let path = path.to_string_lossy();
        serde_json::to_string(path.trim_end_matches(['\\', '/']))
            .expect("a string serialises")
            .trim_matches('"')
            .to_string()
    };
    text.replace("{data}", &json(data))
        .replace("{program}", &json(program))
}
