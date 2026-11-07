//! The Linux service's configuration file, `/etc/dsa/dsa-hostd.json` by default.
//!
//! ```json
//! {
//!   "images_dir": "/var/lib/dsa",
//!   "first_image": { "version": "16.0.0-1", "android_major": 16, "build": 1234500 },
//!   "update": { "list_url": "https://.../releases.json", "trusted_keys": ["..."] }
//! }
//! ```
//!
//! Every key has a default except where Android's image is: either `image_dir`, one image
//! folder booted as it is, or `images_dir` and `first_image`, the update store the package
//! fills and image updates replace. The defaults, the ports and the timeouts are the same as on
//! every host.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dsa_host_core::host_config::{
    default_connect_timeout_secs, default_control_port, default_data_disk_size_gb,
    default_doze_delay_secs, default_log_port, default_processors, default_ready_timeout_secs,
    default_stop_timeout_secs, default_true, UpdateSettings,
};
use dsa_host_core::session::SessionConfig;
use dsa_host_core::update::policy::InstalledImage;
use serde::Deserialize;

/// Where the configuration file is, unless `dsa-hostd` is told otherwise.
pub const DEFAULT_CONFIG: &str = "/etc/dsa/dsa-hostd.json";
/// Where the local API is served, which is where `dsa` looks.
pub const DEFAULT_SOCKET: &str = "/run/dsa/host.sock";
/// The machine's DSA folder.
pub const DEFAULT_MACHINE_DIR: &str = "/var/lib/dsa";

fn default_socket() -> PathBuf {
    DEFAULT_SOCKET.into()
}
fn default_machine_dir() -> PathBuf {
    DEFAULT_MACHINE_DIR.into()
}
/// Free page reporting of every free 4 KiB page: memory the guest frees goes back to the host
/// at once, without waiting for compaction to gather it into 2 MiB blocks. crosvm returns each
/// reported range to the host as it comes.
fn default_page_reporting_order() -> u8 {
    0
}

/// A path that may be written as an empty string, which means none, so a package can fill in
/// only what it has.
fn optional_path<'de, D>(deserializer: D) -> Result<Option<PathBuf>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let path: Option<PathBuf> = Option::deserialize(deserializer)?;
    Ok(path.filter(|p| !p.as_os_str().is_empty()))
}

/// The service's settings, as written in the configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// The name each user's VM starts with, followed by their user ID.
    #[serde(default = "dsa_host_core::host_config::default_vm_name")]
    pub vm_name: String,
    /// One image folder, booted as it is, without updates.
    #[serde(default, deserialize_with = "optional_path")]
    pub image_dir: Option<PathBuf>,
    /// A disk of the image in `image_dir`; without `image_dir`, its folder is the image folder.
    #[serde(default, deserialize_with = "optional_path")]
    pub system_image: Option<PathBuf>,
    /// The update store: `<images_dir>/images/<version>/`. It is used with `first_image`.
    #[serde(default, deserialize_with = "optional_path")]
    pub images_dir: Option<PathBuf>,
    /// The image the package put in `images_dir`.
    #[serde(default)]
    pub first_image: Option<InstalledImage>,
    #[serde(default)]
    pub update: Option<UpdateSettings>,
    /// The kernel for an image that carries none of its own.
    #[serde(default, deserialize_with = "optional_path")]
    pub kernel: Option<PathBuf>,
    /// More kernel command line, after the image's.
    #[serde(default)]
    pub kernel_cmdline: String,
    /// The crosvm program; by default the one DSA's package installs beside the service.
    #[serde(default, deserialize_with = "optional_path")]
    pub crosvm: Option<PathBuf>,
    /// Whether crosvm runs each of the VM's devices in its sandbox (minijail).
    #[serde(default = "default_true")]
    pub sandbox: bool,
    /// The VM's starting memory in MiB, whatever the user's limit, for tests and development.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    /// Whether the VM may grow while it runs (virtio-mem plugs memory in). Off, it keeps its
    /// starting size.
    #[serde(default = "default_true")]
    pub grow_memory: bool,
    #[serde(default = "default_processors")]
    pub processors: u32,
    #[serde(default = "default_page_reporting_order")]
    pub page_reporting_order: u8,
    /// Whether the VM has a network adapter.
    #[serde(default = "default_true")]
    pub network: bool,
    #[serde(default = "default_data_disk_size_gb")]
    pub data_disk_size_gb: u32,
    #[serde(default = "default_control_port")]
    pub control_port: u32,
    #[serde(default = "default_log_port")]
    pub log_port: u32,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_stop_timeout_secs")]
    pub stop_timeout_secs: u64,
    #[serde(default = "default_ready_timeout_secs")]
    pub ready_timeout_secs: u64,
    #[serde(default = "default_doze_delay_secs")]
    pub doze_delay_secs: u64,
    /// How long Android stays in Doze before it is frozen, for a user who turned freezing on.
    #[serde(default)]
    pub freeze_after_minutes: Option<u64>,
    /// Where the local API is served.
    #[serde(default = "default_socket")]
    pub socket: PathBuf,
    /// The machine's DSA folder: settings, logs, and the update store unless `images_dir`
    /// says otherwise.
    #[serde(default = "default_machine_dir")]
    pub machine_dir: PathBuf,
    /// The program that shows the user's Android windows, with its arguments, started in the
    /// user's graphical session when their Android starts and again if it ends. Without it
    /// the service starts none.
    #[serde(default)]
    pub window_manager: Option<Vec<String>>,
}

/// Why a configuration file cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl HostConfig {
    pub fn parse(json: &str) -> Result<HostConfig, ConfigError> {
        let mut config: HostConfig =
            serde_json::from_str(json).map_err(|e| ConfigError(e.to_string()))?;
        if config.image_dir.is_none() {
            config.image_dir = config
                .system_image
                .as_deref()
                .and_then(Path::parent)
                .map(Path::to_path_buf);
        }
        if config.first_image.is_some() && config.images_dir.is_none() {
            return Err(ConfigError(
                "first_image names the image in the update store, which needs images_dir".into(),
            ));
        }
        if config.first_image.is_none() && config.image_dir.is_none() {
            return Err(ConfigError(
                "the configuration names no Android image: give images_dir with first_image, \
                 or image_dir"
                    .into(),
            ));
        }
        if config.update.is_some() && config.first_image.is_none() {
            return Err(ConfigError(
                "update needs images_dir and first_image, where new images go".into(),
            ));
        }
        if !config.socket.is_absolute() || !config.machine_dir.is_absolute() {
            return Err(ConfigError(
                "socket and machine_dir must be absolute paths".into(),
            ));
        }
        Ok(config)
    }

    pub fn load(path: &Path) -> Result<HostConfig, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError(format!("reading {}: {e}", path.display())))?;
        HostConfig::parse(&text).map_err(|e| ConfigError(format!("{}: {e}", path.display())))
    }

    pub fn settings_dir(&self) -> PathBuf {
        self.machine_dir.join("settings")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.machine_dir.join("logs")
    }

    pub fn session_config(&self) -> SessionConfig {
        SessionConfig {
            connect_timeout: Duration::from_secs(self.connect_timeout_secs),
            ready_timeout: Duration::from_secs(self.ready_timeout_secs),
            stop_timeout: Duration::from_secs(self.stop_timeout_secs),
            doze_delay: Duration::from_secs(self.doze_delay_secs),
            freeze_after: self
                .freeze_after_minutes
                .map(|minutes| Duration::from_secs(minutes.saturating_mul(60))),
            ..SessionConfig::default()
        }
    }
}

// Linux paths: "/a" is not absolute on Windows, where the workspace is also tested.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn fr14_an_update_store_configuration_is_read_with_the_defaults() {
        let config = HostConfig::parse(
            r#"{
                "images_dir": "/var/lib/dsa",
                "first_image": { "version": "16.0.0-1", "android_major": 16, "build": 1 }
            }"#,
        )
        .unwrap();
        assert_eq!(config.socket, PathBuf::from("/run/dsa/host.sock"));
        assert_eq!(config.control_port, 5000);
        assert_eq!(config.log_port, 5001);
        assert!(config.grow_memory && config.network && config.sandbox);
        // Freed memory goes back in pieces as small as a page, so none waits in the guest.
        assert_eq!(config.page_reporting_order, 0);
        assert_eq!(
            config.settings_dir(),
            PathBuf::from("/var/lib/dsa/settings")
        );
        assert_eq!(
            config.session_config().stop_timeout,
            Duration::from_secs(30)
        );
    }

    #[test]
    fn fr14_a_configuration_without_an_image_is_refused() {
        assert!(HostConfig::parse("{}").is_err());
        assert!(HostConfig::parse(r#"{ "image_dir": "", "system_image": "" }"#).is_err());
        assert!(HostConfig::parse(
            r#"{ "first_image": { "version": "1", "android_major": 16, "build": 1 } }"#
        )
        .is_err());
        assert!(HostConfig::parse(r#"{ "image_dir": "/a", "colour": 1 }"#).is_err());
        assert!(HostConfig::parse(r#"{ "image_dir": "/a" }"#).is_ok());
    }

    #[test]
    fn fr1_the_packages_configuration_is_read() {
        // As the Linux package writes it: empty paths for what it does not ship.
        let config = HostConfig::parse(
            r#"{
              "vm_name": "dsa-android",
              "kernel": "/usr/lib/dsa/vm/bzImage",
              "system_image": "/usr/lib/dsa/vm/image/system.vhdx",
              "image_dir": "",
              "images_dir": "/var/lib/dsa/images",
              "socket": "/run/dsa/host.sock",
              "data_disk_size_gb": 64,
              "kernel_cmdline": "",
              "processors": 4,
              "control_port": 5000,
              "log_port": 5001,
              "connect_timeout_secs": 120,
              "stop_timeout_secs": 30,
              "doze_delay_secs": 30
            }"#,
        )
        .unwrap();
        assert_eq!(
            config.image_dir,
            Some(PathBuf::from("/usr/lib/dsa/vm/image"))
        );
        assert_eq!(
            config.kernel,
            Some(PathBuf::from("/usr/lib/dsa/vm/bzImage"))
        );
        assert_eq!(config.vm_name, "dsa-android");
        assert!(config.first_image.is_none());
    }
}
