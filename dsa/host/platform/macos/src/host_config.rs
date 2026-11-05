//! The macOS host's configuration file: where the image is and how the user's VM runs.
//!
//! JSON, read by `dsa-hostd`, the per-user agent. Unknown keys are an error, so a misspelt
//! setting is reported rather than ignored. The agent looks for it in the user's DSA folder
//! (`~/Library/Application Support/DSA/dsa-hostd.json`), then in the machine's
//! (`/Library/Application Support/DSA/dsa-hostd.json`), where the installer puts it.
//!
//! ```json
//! {
//!   "images_dir": "/Library/Application Support/DSA",
//!   "first_image": { "version": "16.0.0-13004325", "android_major": 16, "build": 1300432501 }
//! }
//! ```
//!
//! `image_dir` names one image folder to boot, for running by hand; `images_dir` is an
//! update store, whose current image is booted, with `first_image` the image the installer
//! put there and `update` where newer ones come from. Everything the user owns (their
//! Android disk, the raw copies of the image's disks, the logs, the socket of the local API)
//! is in their DSA folder, `data_dir` when it is set.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use dsa_host_core::host_config::{
    default_connect_timeout_secs, default_control_port, default_data_disk_size_gb,
    default_doze_delay_secs, default_log_port, default_processors, default_ready_timeout_secs,
    default_stop_timeout_secs, default_true, HostConfigError, UpdateSettings, DEFAULT_MEMORY_MB,
};
use dsa_host_core::session::SessionConfig;
use dsa_host_core::update::policy::InstalledImage;

/// The name of the configuration file.
pub const FILE_NAME: &str = "dsa-hostd.json";

/// The machine's DSA folder on macOS.
pub const MACHINE_ROOT: &str = "/Library/Application Support/DSA";

/// The user's DSA folder below their home.
pub fn user_root_in(home: &Path) -> PathBuf {
    home.join("Library").join("Application Support").join("DSA")
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MacHostConfig {
    /// One image folder to boot, without updates.
    #[serde(default)]
    pub image_dir: Option<PathBuf>,
    /// The update store: `<images_dir>/images/<version>`.
    #[serde(default)]
    pub images_dir: Option<PathBuf>,
    /// The image the installer put in `images_dir`.
    #[serde(default)]
    pub first_image: Option<InstalledImage>,
    /// Where to look for new images.
    #[serde(default)]
    pub update: Option<UpdateSettings>,
    /// More kernel command line, after the image's.
    #[serde(default)]
    pub kernel_cmdline: String,
    /// The VM's memory in MiB, fixed. Unset, it starts at the default size or the user's
    /// limit and grows as Android needs.
    #[serde(default)]
    pub memory_mb: Option<u32>,
    #[serde(default = "default_processors")]
    pub processors: u32,
    /// The user's DSA folder. Unset, `~/Library/Application Support/DSA`.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// How large the user's disk may grow when the image has no template for it.
    #[serde(default = "default_data_disk_size_gb")]
    pub data_disk_size_gb: u32,
    /// The local API's socket. Unset, `host.sock` in the user's DSA folder, where `dsa`
    /// looks for it.
    #[serde(default)]
    pub socket: Option<PathBuf>,
    /// Where the user's settings are kept. Unset, `settings` in the user's DSA folder.
    #[serde(default)]
    pub settings_dir: Option<PathBuf>,
    /// The vsock port the guest's channel daemon connects to.
    #[serde(default = "default_control_port")]
    pub control_port: u32,
    /// The vsock port Android's log comes on.
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
    #[serde(default)]
    pub freeze_after_minutes: Option<u64>,
    /// The window manager (the compositor that draws each Android task in a window of its
    /// own), which the agent keeps running while Android runs: Android's renderer is in it.
    /// Unset, the `dsa-wm` installed beside `dsa-hostd` (as in DSA.app), through its launchd
    /// agent; where there is none, nothing is started, for running without windows.
    #[serde(default)]
    pub window_manager: Option<PathBuf>,
    /// Whether the VM has a network adapter.
    #[serde(default = "default_true")]
    pub network: bool,
    /// Whether the guest kernel keeps its last log on a pstore disk.
    #[serde(default = "default_true")]
    pub pstore: bool,
    /// The folder the configuration file was read from. Not a key of the file.
    #[serde(skip)]
    pub config_dir: Option<PathBuf>,
}

impl MacHostConfig {
    /// Parses the configuration file's contents.
    pub fn parse(json: &str) -> Result<MacHostConfig, HostConfigError> {
        let config: MacHostConfig =
            serde_json::from_str(json).map_err(|e| HostConfigError(e.to_string()))?;
        match (&config.image_dir, &config.images_dir, &config.first_image) {
            (None, None, _) => {
                return Err(HostConfigError(
                    "name either image_dir or images_dir (with first_image)".into(),
                ))
            }
            (_, Some(_), None) => {
                return Err(HostConfigError(
                    "images_dir needs first_image, the image the installer put there".into(),
                ))
            }
            _ => {}
        }
        if let Some(update) = &config.update {
            if config.images_dir.is_none() {
                return Err(HostConfigError("update needs images_dir".into()));
            }
            update.keys()?;
        }
        if config.processors == 0 {
            return Err(HostConfigError("processors must be at least 1".into()));
        }
        Ok(config)
    }

    /// The configuration file for the user whose home is `home`: theirs, else the machine's.
    pub fn find(home: &Path) -> Option<PathBuf> {
        [
            user_root_in(home).join(FILE_NAME),
            Path::new(MACHINE_ROOT).join(FILE_NAME),
        ]
        .into_iter()
        .find(|path| path.exists())
    }

    /// Reads the configuration file at `path`.
    pub fn read(path: &Path) -> Result<MacHostConfig, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("reading {}: {e}", path.display()))?;
        let mut config =
            MacHostConfig::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        config.config_dir = path.parent().map(Path::to_path_buf);
        Ok(config)
    }

    /// The user's folders, below `home` unless `data_dir` says otherwise.
    pub fn user(&self, home: &Path) -> UserFolders {
        let root = self.data_dir.clone().unwrap_or_else(|| user_root_in(home));
        UserFolders { root }
    }

    pub fn socket(&self, user: &UserFolders) -> PathBuf {
        self.socket.clone().unwrap_or_else(|| user.socket())
    }

    pub fn settings_dir(&self, user: &UserFolders) -> PathBuf {
        self.settings_dir
            .clone()
            .unwrap_or_else(|| user.root.join("settings"))
    }

    /// The session's timings.
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

    /// The memory the VM starts with and may grow to, in MiB, for a user whose limit is
    /// `limit_mb` (0 for none) on a Mac with `total_mb` of memory when known.
    pub fn memory_mb(&self, limit_mb: u32, total_mb: Option<u64>) -> (u64, u64) {
        if let Some(fixed) = self.memory_mb {
            return (u64::from(fixed), u64::from(fixed));
        }
        memory_range_mb(limit_mb, total_mb)
    }
}

/// The memory a VM starts with and may grow to, in MiB: it starts at the default size (or
/// the user's lower limit) and grows to their limit, or with none to half the Mac's memory.
/// Growth and return follow the shared policy: the guest trims its page cache to its working
/// set all the time, and the balloon grows the VM when the guest runs short and takes memory
/// back once it is not.
pub fn memory_range_mb(limit_mb: u32, total_mb: Option<u64>) -> (u64, u64) {
    let start = match limit_mb {
        0 => DEFAULT_MEMORY_MB,
        limit => limit.clamp(2048, DEFAULT_MEMORY_MB),
    };
    let start = u64::from(start);
    let max = match total_mb {
        Some(total) => dsa_host_core::memory::half_host_vm_memory_mb(total, limit_mb),
        None if limit_mb > 0 => u64::from(limit_mb),
        None => start,
    };
    (start.min(max), max.max(start.min(max)))
}

/// The ports of the guest the window manager (`dsa-wm`) serves: its graphics streams.
pub const GRAPHICS_PORTS: [u32; 2] = [
    dsa_host_core::compositor::GRAPHICS_COMMAND_PORT,
    dsa_host_core::compositor::GRAPHICS_PROCESS_PIPE_PORT,
];

/// The ports of the guest the user's device lender (`dsa-devices`) serves: sound, video,
/// the sensors and the cameras.
pub const DEVICE_PORTS: [u32; 4] = [
    dsa_host_core::audio::AUDIO_PORT,
    dsa_host_core::video::VIDEO_PORT,
    dsa_host_core::sensors::SENSOR_PORT,
    dsa_host_core::camera::CAMERA_PORT,
];

/// Every port the agent relays from the VM to a program of the user's.
pub const RELAYED_PORTS: [u32; 6] = [
    GRAPHICS_PORTS[0],
    GRAPHICS_PORTS[1],
    DEVICE_PORTS[0],
    DEVICE_PORTS[1],
    DEVICE_PORTS[2],
    DEVICE_PORTS[3],
];

/// The Unix socket the agent hands the guest's connections to `port` to.
pub fn relay_socket(user: &UserFolders, port: u32) -> PathBuf {
    user.relay_dir().join(format!("{port}.sock"))
}

/// One user's DSA folder and what is in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserFolders {
    pub root: PathBuf,
}

impl UserFolders {
    /// The user's Android disk (raw).
    pub fn disk(&self) -> PathBuf {
        self.root.join("android.img")
    }
    /// The disk the guest kernel keeps its last log on.
    pub fn pstore_disk(&self) -> PathBuf {
        self.root.join("pstore.img")
    }
    /// The raw copies of image `version`'s disks.
    pub fn image_cache(&self, version: &str) -> PathBuf {
        self.root.join("cache").join("images").join(version)
    }
    /// Each boot's initrd, with its secret, while the VM starts.
    pub fn boot_dir(&self) -> PathBuf {
        self.root.join("tmp")
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    /// The guest's console.
    pub fn console_log(&self) -> PathBuf {
        self.logs().join("android-console.log")
    }
    /// The agent's own log, where launchd sends its output.
    pub fn agent_log(&self) -> PathBuf {
        self.logs().join("dsa-hostd.log")
    }
    pub fn socket(&self) -> PathBuf {
        self.root.join("host.sock")
    }
    /// The folder of the sockets the agent relays the guest's ports to.
    pub fn relay_dir(&self) -> PathBuf {
        self.root.join("vsock")
    }
    /// The device lender's own log.
    pub fn devices_log(&self) -> PathBuf {
        self.logs().join("dsa-devices.log")
    }
}
