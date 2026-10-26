//! The host's configuration file: where the Android VM's files are and how it runs.
//!
//! JSON, read by `dsa-hostd`. Unknown keys are an error, so a misspelt setting is reported
//! rather than silently ignored.
//!
//! ```json
//! {
//!   "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
//!   "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx",
//!   "memory_mb": 4096
//! }
//! ```
//!
//! The service runs one VM per user. Each user's VM is named after the user's SID and lets
//! only that user (and SYSTEM) use its Hyper-V sockets. Its disk is in the user's own
//! profile, `%LOCALAPPDATA%\DSA\android.vhdx`, as WSL keeps each user's distributions in
//! theirs. `data_dir` keeps every user's disk in one protected folder instead, as
//! `<data_dir>\<SID>\data.vhdx`, and `data_disk` names a single disk, for running one VM
//! by hand.
//!
//! With `images_dir`, the Android image comes from the update store there instead of
//! `system_image`: `first_image` names the image the installer put in it, and `update`
//! says where the signed release list is and which keys may sign it.
//!
//! `update.program_list_url` and `update.program_dir` let the service update DSA itself
//! from the signed program list, downloading setup files into that folder;
//! `"program": false` turns that off.
//!
//! `settings_dir` is where the service keeps the settings users change from the settings
//! app (`%ProgramData%\DSA\settings` when absent). The settings can turn either kind of
//! update off; they cannot turn on what this file leaves out.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use dsa_host_core::session::SessionConfig;
use serde::Deserialize;

use dsa_host_core::update::manifest;
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::program_service::ProgramSettings;
use ed25519_dalek::VerifyingKey;

use crate::vm::{console_pipe_path, is_sid, AndroidVmConfig};

/// `<dir>\<sid>\data.vhdx`. Built as text so it is a Windows path on every platform the
/// tests run on.
fn user_data_disk(dir: &std::path::Path, sid: &str) -> PathBuf {
    let dir = dir.to_string_lossy();
    PathBuf::from(format!("{}\\{sid}\\data.vhdx", dir.trim_end_matches('\\')))
}

/// What a VM boots from one image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFiles {
    pub kernel: PathBuf,
    pub initrd: Option<PathBuf>,
    pub kernel_cmdline: String,
    /// A separate, read-only system disk (older images).
    pub system_image: Option<PathBuf>,
    /// The template a user's Android disk is made from (images with `image.json`).
    pub disk_template: Option<PathBuf>,
}

/// One user's VM: its HCS name, its configuration and how large its data disk may grow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserVm {
    pub vm_name: String,
    pub config: AndroidVmConfig,
    pub data_disk_size_gb: u32,
    /// The disk is in the user's own profile, which already admits only them, SYSTEM and
    /// administrators. A disk under `data_dir` is in a folder the service protects itself.
    pub disk_in_profile: bool,
    /// Where the VM's console output is kept, `android-<SID>.log`.
    pub console_log: PathBuf,
}

/// `name` in the folder that holds `path`, a Windows path. Split as text, so it is a
/// Windows path on every platform the tests run on.
fn beside(path: &std::path::Path, name: &str) -> Option<PathBuf> {
    let text = path.to_string_lossy();
    let end = text.rfind(['\\', '/'])?;
    Some(PathBuf::from(format!("{}{name}", &text[..=end])))
}

/// Finds a user's local application data folder from their SID.
pub type LocalAppData<'a> = &'a dyn Fn(&str) -> Result<PathBuf, String>;

/// The local application data folder of the user with SID `sid`, from the registry.
pub fn local_app_data(sid: &str) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        crate::profile::local_app_data(sid)
    }
    #[cfg(not(windows))]
    {
        Err(format!(
            "{sid}'s profile can only be found on Windows; set data_dir or data_disk"
        ))
    }
}

/// Replaces `{data}` and `{program}` in the configuration file's text with the folders the
/// installer put the data and the program in. The user chooses both while installing, so
/// the file cannot name them when it is built.
pub fn expand_locations(text: &str, data: &std::path::Path, program: &std::path::Path) -> String {
    let json = |path: &std::path::Path| {
        let path = path.to_string_lossy();
        serde_json::to_string(path.trim_end_matches(['\\', '/']))
            .expect("a string serialises")
            .trim_matches('"')
            .to_string()
    };
    text.replace("{data}", &json(data))
        .replace("{program}", &json(program))
}

/// Where a user's Android disk is kept in their profile, below their local application data.
pub fn profile_disk(local_app_data: &std::path::Path) -> PathBuf {
    let dir = local_app_data.to_string_lossy();
    PathBuf::from(format!("{}\\DSA\\android.vhdx", dir.trim_end_matches('\\')))
}

fn default_vm_name() -> String {
    "dsa-android".into()
}
fn default_memory_mb() -> u32 {
    4096
}
fn default_processors() -> u32 {
    4
}
fn default_page_reporting_order() -> u8 {
    9
}
fn default_control_port() -> u32 {
    5000
}
fn default_connect_timeout_secs() -> u64 {
    120
}
fn default_stop_timeout_secs() -> u64 {
    30
}
fn default_data_disk_size_gb() -> u32 {
    64
}

/// The local API's named pipe when the configuration names none. The compositor and the
/// CLI connect to it too.
pub const DEFAULT_PIPE_NAME: &str = "dsa-host";

fn default_pipe_name() -> String {
    DEFAULT_PIPE_NAME.into()
}

/// The host's settings, as written in the configuration file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostConfig {
    /// The VM's name in HCS. Must be unique on the machine.
    #[serde(default = "default_vm_name")]
    pub vm_name: String,
    pub kernel: PathBuf,
    #[serde(default)]
    pub initrd: Option<PathBuf>,
    /// Extra kernel command line for the Android image.
    #[serde(default)]
    pub kernel_cmdline: String,
    #[serde(default = "default_memory_mb")]
    pub memory_mb: u32,
    #[serde(default = "default_processors")]
    pub processors: u32,
    /// The Android system image, when updates are not used.
    #[serde(default)]
    pub system_image: Option<PathBuf>,
    /// The update store: `<images_dir>\images\<version>\system.vhdx`, and the
    /// image's own `bzImage` when it has one.
    #[serde(default)]
    pub images_dir: Option<PathBuf>,
    /// The image the installer put in `images_dir`.
    #[serde(default)]
    pub first_image: Option<InstalledImage>,
    /// Where to look for new images.
    #[serde(default)]
    pub update: Option<UpdateSettings>,
    /// One data disk, for running a single VM by hand.
    #[serde(default)]
    pub data_disk: Option<PathBuf>,
    /// One folder for every user's disk, as `<data_dir>\<SID>\data.vhdx`. Without it each
    /// user's disk is in their own profile.
    #[serde(default)]
    pub data_dir: Option<PathBuf>,
    /// The size a new user's data disk may grow to. It starts empty and grows as used.
    #[serde(default = "default_data_disk_size_gb")]
    pub data_disk_size_gb: u32,
    /// The local API's named pipe, `\\.\pipe\<pipe_name>`.
    #[serde(default = "default_pipe_name")]
    pub pipe_name: String,
    /// Who may open the pipe, as SDDL. When absent, SYSTEM and interactively signed-in
    /// users. The service acts on each caller's own VM either way.
    #[serde(default)]
    pub pipe_sddl: Option<String>,
    /// Where the service writes its log. A service has no console, so without this its
    /// messages are lost.
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    /// Where the service keeps the settings users change from the settings app.
    #[serde(default)]
    pub settings_dir: Option<PathBuf>,
    /// The user allowed to use the VM's Hyper-V sockets besides SYSTEM. When absent, the
    /// user running `dsa-hostd`.
    #[serde(default)]
    pub user_sid: Option<String>,
    /// Free page reporting granularity as a page order: 9 (2 MiB) works on every Windows
    /// build; 5 (128 KiB) returns memory more finely on recent builds.
    #[serde(default = "default_page_reporting_order")]
    pub page_reporting_order: u8,
    /// The vsock port the guest's channel daemon connects to.
    #[serde(default = "default_control_port")]
    pub control_port: u32,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_stop_timeout_secs")]
    pub stop_timeout_secs: u64,
}

fn default_check_interval_hours() -> u64 {
    24
}
fn default_boot_timeout_secs() -> u64 {
    300
}
fn default_true() -> bool {
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

impl HostConfig {
    /// Parses the configuration file's contents.
    pub fn parse(json: &str) -> Result<HostConfig, HostConfigError> {
        let config: HostConfig =
            serde_json::from_str(json).map_err(|e| HostConfigError(e.to_string()))?;
        match (
            &config.system_image,
            &config.images_dir,
            &config.first_image,
        ) {
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
        if let Some(update) = &config.update {
            if config.images_dir.is_none() {
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
        Ok(config)
    }

    /// How the service updates DSA itself, running version `running`. `None` when the
    /// file names no folder for setup files, so DSA does not update itself.
    pub fn program_settings(
        &self,
        running: &str,
    ) -> Result<Option<ProgramSettings>, HostConfigError> {
        let Some(update) = &self.update else {
            return Ok(None);
        };
        let Some(dir) = &update.program_dir else {
            return Ok(None);
        };
        let log_dir = self.log_dir().unwrap_or_else(|| dir.clone());
        Ok(Some(ProgramSettings {
            list_url: update.program_list_url.clone(),
            enabled: update.program,
            keys: update.keys()?,
            dir: dir.clone(),
            log_dir,
            running: running.to_string(),
            stop_timeout: Duration::from_secs(self.stop_timeout_secs + 15),
            settle: Duration::from_secs(60),
        }))
    }

    /// Whether this installation downloads new Android images at all.
    pub fn image_updates_available(&self) -> bool {
        self.update.is_some() && self.images_dir.is_some()
    }

    /// Whether this installation updates DSA itself at all.
    pub fn program_updates_available(&self) -> bool {
        self.update.as_ref().is_some_and(|update| {
            update.program && update.program_list_url.is_some() && update.program_dir.is_some()
        })
    }

    /// The service's log folder: the folder of `log_file`, when there is one. Split as
    /// text, so it is a Windows path on every platform the tests run on.
    pub fn log_dir(&self) -> Option<PathBuf> {
        self.log_file.as_ref().and_then(|log| {
            let log = log.to_string_lossy();
            log.rfind(['\\', '/']).map(|end| PathBuf::from(&log[..end]))
        })
    }

    /// Where the console of `user`'s VM is kept: `android-<user>.log` in the service's log
    /// folder, or beside the user's disk `data_disk` when the file names no log.
    pub fn console_log_path(&self, user: &str, data_disk: &std::path::Path) -> PathBuf {
        let name = format!("android-{user}.log");
        self.log_file
            .as_deref()
            .and_then(|log| beside(log, &name))
            .or_else(|| beside(data_disk, &name))
            .unwrap_or_else(|| PathBuf::from(name))
    }

    /// The VM configuration for running one VM by hand. `fallback_sid` is used when the file
    /// names no user.
    pub fn vm_config(
        &self,
        fallback_sid: Option<&str>,
    ) -> Result<AndroidVmConfig, HostConfigError> {
        self.vm_config_with(fallback_sid, &local_app_data)
    }

    /// [`vm_config`](Self::vm_config), finding a profile with `local_app_data`.
    pub fn vm_config_with(
        &self,
        fallback_sid: Option<&str>,
        local_app_data: LocalAppData,
    ) -> Result<AndroidVmConfig, HostConfigError> {
        let user = self.user_sid.as_deref().or(fallback_sid);
        let socket_sids = user.map(|sid| vec![sid.to_string()]).unwrap_or_default();
        let data_disk = match (&self.data_disk, &self.data_dir, user) {
            (Some(disk), _, _) => disk.clone(),
            (None, Some(dir), Some(sid)) => user_data_disk(dir, sid),
            (None, Some(dir), None) => dir.join("data.vhdx"),
            (None, None, Some(sid)) => profile_disk(&local_app_data(sid).map_err(HostConfigError)?),
            (None, None, None) => {
                return Err(HostConfigError(
                    "no user to keep the disk for; name data_disk, data_dir or user_sid".into(),
                ))
            }
        };
        Ok(self.base_vm_config(&self.vm_name, data_disk, socket_sids))
    }

    /// The VM of the user with SID `sid`, as the service runs it.
    pub fn user_vm(&self, sid: &str) -> Result<UserVm, HostConfigError> {
        self.user_vm_with(sid, &local_app_data)
    }

    /// [`user_vm`](Self::user_vm), finding the user's profile with `local_app_data`.
    pub fn user_vm_with(
        &self,
        sid: &str,
        local_app_data: LocalAppData,
    ) -> Result<UserVm, HostConfigError> {
        // The SID becomes part of a path and a VM name, so only a well-formed one is used.
        if !is_sid(sid) {
            return Err(HostConfigError(format!(
                "{sid:?} is not a Windows SID, so no VM is made for it"
            )));
        }
        let (disk, disk_in_profile) = match &self.data_dir {
            Some(dir) => (user_data_disk(dir, sid), false),
            None => (
                profile_disk(&local_app_data(sid).map_err(HostConfigError)?),
                true,
            ),
        };
        let vm_name = format!("{}-{sid}", self.vm_name);
        let console_log = self.console_log_path(sid, &disk);
        let config = self.base_vm_config(&vm_name, disk, vec![sid.into()]);
        Ok(UserVm {
            vm_name,
            config,
            data_disk_size_gb: self.data_disk_size_gb,
            disk_in_profile,
            console_log,
        })
    }

    /// What to boot from `image_dir`, an image folder of the update store. Without a
    /// folder, the first image's folder, or `system_image` when updates are not used.
    ///
    /// An image made from the published AOSP builds carries `image.json`, `initrd.img` and
    /// `disk.vhdx`, the template of a user's Android disk; its kernel command line comes from
    /// `image.json`. An older image carries `system.vhdx`, attached beside the user's disk.
    /// The image's own `bzImage` wins over `kernel` either way.
    pub fn image_files(&self, image_dir: Option<&std::path::Path>) -> ImageFiles {
        let dir = image_dir.map(std::path::Path::to_path_buf).or_else(|| {
            match (&self.system_image, &self.images_dir, &self.first_image) {
                (None, Some(root), Some(first)) => Some(root.join("images").join(&first.version)),
                _ => None,
            }
        });
        let Some(dir) = dir else {
            return ImageFiles {
                kernel: self.kernel.clone(),
                initrd: self.initrd.clone(),
                kernel_cmdline: self.kernel_cmdline.clone(),
                system_image: self.system_image.clone(),
                disk_template: None,
            };
        };
        let own_kernel = dir.join("bzImage");
        let kernel = if own_kernel.exists() {
            own_kernel
        } else {
            self.kernel.clone()
        };
        let manifest = std::fs::read_to_string(dir.join("image.json"))
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        match manifest {
            Some(manifest) => {
                let image_cmdline = manifest["kernel_cmdline"].as_str().unwrap_or_default();
                let system_vhdx = dir.join("system.vhdx");
                let has_two_disks =
                    system_vhdx.exists() || manifest["files"].get("system.vhdx").is_some();
                let (system_image, disk_template) = if has_two_disks {
                    (Some(system_vhdx), Some(dir.join("data.vhdx")))
                } else {
                    (None, Some(dir.join("disk.vhdx")))
                };
                ImageFiles {
                    kernel,
                    initrd: Some(dir.join("initrd.img")),
                    kernel_cmdline: format!("{image_cmdline} {}", self.kernel_cmdline)
                        .trim()
                        .to_string(),
                    system_image,
                    disk_template,
                }
            }
            None => ImageFiles {
                kernel,
                initrd: self.initrd.clone(),
                kernel_cmdline: self.kernel_cmdline.clone(),
                system_image: Some(dir.join("system.vhdx")),
                disk_template: None,
            },
        }
    }

    fn base_vm_config(
        &self,
        vm_name: &str,
        data_disk: PathBuf,
        socket_sids: Vec<String>,
    ) -> AndroidVmConfig {
        let files = self.image_files(None);
        AndroidVmConfig {
            owner: format!("DSA ({vm_name})"),
            kernel: files.kernel,
            initrd: files.initrd,
            kernel_cmdline: files.kernel_cmdline,
            memory_mb: self.memory_mb,
            processors: self.processors,
            system_image: files.system_image,
            data_disk,
            socket_sids,
            page_reporting_order: self.page_reporting_order,
            console_pipe: Some(console_pipe_path(vm_name)),
            image_version: match (&self.system_image, &self.first_image) {
                (None, Some(first)) => Some(first.version.clone()),
                _ => None,
            },
        }
    }

    pub fn session_config(&self) -> SessionConfig {
        SessionConfig {
            connect_timeout: Duration::from_secs(self.connect_timeout_secs),
            stop_timeout: Duration::from_secs(self.stop_timeout_secs),
        }
    }
}
