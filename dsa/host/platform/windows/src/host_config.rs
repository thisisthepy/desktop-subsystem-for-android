//! The host's configuration file: where the Android VM's files are and how it runs.
//!
//! JSON, read by `dsa-hostd`. Unknown keys are an error, so a misspelt setting is reported
//! rather than silently ignored.
//!
//! ```json
//! {
//!   "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
//!   "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx",
//!   "memory_mb": 3072
//! }
//! ```
//!
//! Without `memory_mb`, which an installation leaves out, the VM starts at 3 GiB and grows to the
//! limit the user set (`dsa settings set memory-limit <MiB>`), or with none to the computer's
//! memory less what Windows keeps. It holds only what Android uses.
//!
//! The service runs one VM per user. Each user's VM is named after the user's SID and lets
//! only that user (and SYSTEM) use its Hyper-V sockets. Its disk is in the user's own
//! profile, `%LOCALAPPDATA%\DSA\android.vhdx`, as WSL keeps each user's distributions in
//! theirs. `data_dir` keeps every user's disk in one protected folder instead, as
//! `<data_dir>\<SID>\data.vhdx`, and `data_disk` names a single disk, for running one VM
//! by hand. Both are for tests and development: an installation names neither, and the
//! service moves a disk the first releases kept in `%ProgramData%\DSA\users` into the
//! user's profile.
//!
//! With `images_dir`, the Android image comes from the update store there instead of
//! `system_image`: `first_image` names the image the installer put in it, and `update`
//! says where the signed release list is and which keys may sign it.
//!
//! `update.program_list_url` and `update.program_dir` let the service update DSA itself
//! from the signed program list, downloading setup files into that folder;
//! `"program": false` turns that off. `update.program_list_url` is the stable channel's list
//! and `update.prerelease_program_list_url` the pre-release channel's; `update.channel` is the
//! channel the installer was published on, which a computer starts on.
//!
//! `settings_dir` is where the service keeps the settings users change from the settings
//! app (`%ProgramData%\DSA\settings` when absent). The settings can turn either kind of
//! update off; they cannot turn on what this file leaves out.
//!
//! The defaults, the update settings and the rules every host shares are in
//! `dsa_host_core::host_config`; this module adds the keys only Windows has and resolves
//! the user's profile from the registry.

use std::path::PathBuf;
use std::time::Duration;

use dsa_host_core::session::SessionConfig;
use serde::Deserialize;

use dsa_host_core::host_config::{
    check_image_sources, default_connect_timeout_secs, default_control_port,
    default_data_disk_size_gb, default_doze_delay_secs, default_log_port, default_processors,
    default_ready_timeout_secs, default_stop_timeout_secs, default_true, default_vm_name,
};
pub use dsa_host_core::host_config::{
    expand_locations, HostConfigError, UpdateSettings, DEFAULT_MEMORY_MB,
};
use dsa_host_core::update::policy::InstalledImage;
use dsa_host_core::update::program::ReleaseChannel;
use dsa_host_core::update::program_service::ProgramSettings;

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

/// Where a user's Android disk is kept in their profile, below their local application data.
pub fn profile_disk(local_app_data: &std::path::Path) -> PathBuf {
    crate::paths::UserRoot::in_local_app_data(local_app_data).android_disk()
}

/// The memory a VM starts with for a user whose limit is `limit_mb` (0 for automatic):
/// [`DEFAULT_MEMORY_MB`], or the user's limit when it is smaller.
///
/// The VM starts small because the guest kernel keeps its page records for all the memory
/// it has resident, about 1.6% of it, used or not: a VM made at the computer's size from the
/// start would cost some 460 MiB on a 32 GiB computer while idle. It grows from there while
/// the guest needs more (see [`growth_max_mb`]).
pub fn host_sized_memory_mb(limit_mb: u32) -> u32 {
    match limit_mb {
        0 => DEFAULT_MEMORY_MB,
        limit => limit.clamp(2048, DEFAULT_MEMORY_MB),
    }
}

/// How far a VM may grow while it runs, in MiB, for a user whose limit is `limit_mb`: their
/// limit, never more than the computer has, or with no limit the computer's memory less what
/// Windows keeps (see [`automatic_vm_memory_mb`]). `None` when the computer's memory cannot
/// be read.
///
/// [`automatic_vm_memory_mb`]: dsa_host_core::memory::automatic_vm_memory_mb
pub fn growth_max_mb(limit_mb: u32) -> Option<u64> {
    #[cfg(windows)]
    let total_mb = crate::power::read_memory().map(|(total_kb, _)| total_kb / 1024);
    #[cfg(not(windows))]
    let total_mb: Option<u64> = None;
    total_mb.map(|total_mb| dsa_host_core::memory::vm_memory_mb(total_mb, limit_mb))
}
fn default_page_reporting_order() -> u8 {
    page_reporting_order_for_build(windows_build())
}

/// The free page reporting order for a Windows build and update revision: 0 (4 KiB, one page)
/// where the VM can have fine-grained memory, which the host backs in 4 KiB pages, and 9
/// (2 MiB) elsewhere or when the build is unknown. A guest's free memory is scattered, so the
/// smaller the pieces it may report, the more of it the host gets back and the sooner: a
/// freed page goes back at once instead of waiting for compaction to gather a whole block.
/// The host can only take pieces as small as it backs; a Hyper-V that refuses small pieces
/// makes the guest's balloon driver fall back to 2 MiB by itself. WSL uses 5 (128 KiB).
pub fn page_reporting_order_for_build(build: Option<(u32, u32)>) -> u8 {
    match build {
        Some((build, revision)) if crate::vm::fine_grained_memory_supported(build, revision) => 0,
        _ => 9,
    }
}

/// The build number and update revision of the running Windows, from the registry.
#[cfg(windows)]
fn windows_build() -> Option<(u32, u32)> {
    crate::android_vm::windows_build()
}

/// Off Windows (in tests) there is no build to read.
#[cfg(not(windows))]
fn windows_build() -> Option<(u32, u32)> {
    None
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
    /// The VM's memory in MiB. Unset, it is sized to the computer and the user's limit
    /// ([`host_sized_memory_mb`]); set, it is that, whatever the user's limit, for tests and
    /// development.
    #[serde(default)]
    pub memory_mb: Option<u32>,
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
    /// Set by a package built only to test the installer: its kernel and graphics library
    /// are empty stand-ins and its image cannot boot, so the service says so when it starts.
    #[serde(default)]
    pub test_package: bool,
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
    /// build; 5 (128 KiB) returns memory more finely on recent builds. By default, 5 on
    /// Windows 11 and later and 9 before.
    #[serde(default = "default_page_reporting_order")]
    pub page_reporting_order: u8,
    /// The vsock port the guest's channel daemon connects to.
    #[serde(default = "default_control_port")]
    pub control_port: u32,
    /// The vsock port the guest's channel daemon sends Android's log and diagnostics on, while
    /// the host asks for them. A port of its own, so a busy log never delays the control
    /// channel's messages.
    #[serde(default = "default_log_port")]
    pub log_port: u32,
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u64,
    #[serde(default = "default_stop_timeout_secs")]
    pub stop_timeout_secs: u64,
    /// How long Android may take to finish booting before it is reported as stalled. It keeps
    /// trying after that; this only decides when the user is told.
    #[serde(default = "default_ready_timeout_secs")]
    pub ready_timeout_secs: u64,
    /// Whether the service starts each user's window manager in their session when their
    /// Android starts, and again if it dies, since Android cannot boot without its renderer.
    /// Off only for running without windows, as the tests do.
    #[serde(default = "default_true")]
    pub keep_window_manager: bool,
    /// How long no Android window may be shown before Android's screen goes off and it goes
    /// into Doze.
    #[serde(default = "default_doze_delay_secs")]
    pub doze_delay_secs: u64,
    /// How long Android stays in Doze before it is frozen, for a user who turned freezing on
    /// in their settings (30 minutes when absent). The service freezes only such a user's
    /// Android: a frozen Android gets no notifications, alarms or widget updates. Run by hand
    /// (`dsa-hostd <config.json>`), the setting alone turns freezing on.
    #[serde(default)]
    pub freeze_after_minutes: Option<u64>,
    /// The direct-mapped mode: each boot attaches the image's system disk and the user's
    /// add-on disk as virtual persistent memory, mapped from their files, instead of SCSI
    /// disks, so the guest maps the read-only partitions' files rather than keeping a page
    /// cache of them on top of Windows' (`dsa_host_core::direct_mapped`). Off until its saving
    /// is measured; on x86_64 hosts only, and only for images that carry the mode's mount table.
    /// The first boot of each image makes a fixed copy of its system disk beside it.
    #[serde(default)]
    pub direct_mapped_images: bool,
    /// The machine's DSA folder: the folder the configuration file is in, which `dsa-hostd`
    /// fills in when it reads the file. Not a key of the file.
    #[serde(skip)]
    pub machine_root: Option<PathBuf>,
    /// The image the package installed last put in `images_dir`, from
    /// [`INSTALLED_PACKAGE_FILE`]. Not a key of the file.
    #[serde(skip)]
    pub bundled_image: Option<InstalledImage>,
    /// What [`HostConfig::apply_package`] changed or found, for the service's log. Not a key
    /// of the file.
    #[serde(skip)]
    pub package_notes: Vec<String>,
}

/// The file, beside the configuration, in which every installation records what its package
/// carries. The configuration itself is kept across upgrades, since a user may have edited
/// it, so it goes on naming the first installation's image and update lists; this file is
/// replaced by every installation.
pub const INSTALLED_PACKAGE_FILE: &str = "installed-package.json";

/// What [`INSTALLED_PACKAGE_FILE`] says about the package installed last.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledPackage {
    /// The image the package put in the image store (`"none"` when it bundled none).
    pub first_image: InstalledImage,
    /// Whether the package was built only to test the installer.
    #[serde(default)]
    pub test_package: bool,
    /// The release lists the package was built to use.
    #[serde(default)]
    pub update: Option<UpdateSettings>,
}

impl InstalledPackage {
    pub fn parse(json: &str) -> Result<InstalledPackage, HostConfigError> {
        serde_json::from_str(json).map_err(|e| HostConfigError(e.to_string()))
    }
}

/// Whether `location` is a web address rather than a file or a folder on some computer,
/// such as the `D:\a\...` mirror a test package is built with.
fn is_web_address(location: &str) -> bool {
    let lower = location.to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

impl HostConfig {
    /// Parses the configuration file's contents.
    pub fn parse(json: &str) -> Result<HostConfig, HostConfigError> {
        let config: HostConfig =
            serde_json::from_str(json).map_err(|e| HostConfigError(e.to_string()))?;
        check_image_sources(
            config.system_image.as_deref(),
            config.images_dir.as_deref(),
            config.first_image.as_ref(),
            config.update.as_ref(),
        )?;
        Ok(config)
    }

    /// Brings in what the package installed last carries, which a configuration kept from an
    /// earlier installation does not know: the image it bundled (booted when newer than the
    /// store's current one), whether it is a test package, and its release lists when the
    /// configuration's are not web addresses (a test package's local mirror, left behind by
    /// an earlier installation). The user's own choices in the configuration (how often to
    /// check, how long a first boot may take, updating DSA itself or not) stay. Returns what
    /// it changed or found, one line each, also kept in `package_notes`.
    pub fn apply_package(&mut self, package: &InstalledPackage) -> Vec<String> {
        let mut notes = Vec::new();
        if package.first_image.version != "none" {
            self.bundled_image = Some(package.first_image.clone());
        }
        if self.test_package != package.test_package {
            notes.push(if package.test_package {
                "the installed package is a test package; the configuration did not say so".into()
            } else {
                "the configuration was written by a test package; the installed package is a \
                 real one, so it is no longer treated as a test package"
                    .into()
            });
            self.test_package = package.test_package;
        }
        let has_store = self.images_dir.is_some();
        match (&mut self.update, &package.update) {
            (Some(ours), Some(theirs)) => {
                // A configuration from before the release channels names one program list
                // and no channel; the package's channel lists replace it, so the computer
                // reads the list of the channel it follows.
                if ours.prerelease_program_list_url.is_none()
                    && ours.channel.is_none()
                    && theirs.prerelease_program_list_url.is_some()
                {
                    notes.push(format!(
                        "the configuration names one program list {:?} from before release \
                         channels; using the installed package's stable list {:?} and \
                         pre-release list {:?}",
                        ours.program_list_url,
                        theirs.program_list_url,
                        theirs.prerelease_program_list_url
                    ));
                    ours.program_list_url = theirs.program_list_url.clone();
                    ours.prerelease_program_list_url = theirs.prerelease_program_list_url.clone();
                    if ours.program_dir.is_none() {
                        ours.program_dir = theirs.program_dir.clone();
                    }
                }
                // The channel of the installer used last, for a computer whose settings
                // record none yet.
                ours.channel = theirs.channel.or(ours.channel);
                let differs = ours.list_url != theirs.list_url
                    || ours.program_list_url != theirs.program_list_url
                    || ours.trusted_keys != theirs.trusted_keys;
                let stale = !is_web_address(&ours.list_url)
                    || ours
                        .program_list_url
                        .as_deref()
                        .is_some_and(|url| !is_web_address(url));
                if differs && stale && is_web_address(&theirs.list_url) {
                    notes.push(format!(
                        "the configuration names the release list {:?}, which is not a web \
                         address (left by an earlier test installation); using the installed \
                         package's {:?} instead",
                        ours.list_url, theirs.list_url
                    ));
                    ours.list_url = theirs.list_url.clone();
                    ours.trusted_keys = theirs.trusted_keys.clone();
                    ours.program_list_url = theirs.program_list_url.clone();
                    ours.prerelease_program_list_url = theirs.prerelease_program_list_url.clone();
                    ours.program_dir = theirs.program_dir.clone();
                } else if differs {
                    notes.push(format!(
                        "the configuration names the release list {:?}, not the installed \
                         package's {:?}; keeping the configuration's",
                        ours.list_url, theirs.list_url
                    ));
                }
            }
            (None, Some(theirs)) if has_store => {
                notes.push(format!(
                    "the configuration names no release list; using the installed package's {:?}",
                    theirs.list_url
                ));
                self.update = Some(theirs.clone());
            }
            _ => {}
        }
        self.package_notes.extend(notes.iter().cloned());
        notes
    }

    /// The machine's DSA folder: where the configuration file is, or `%ProgramData%\DSA` for
    /// a configuration that was not read from a file.
    pub fn machine_root(&self) -> crate::paths::MachineRoot {
        match &self.machine_root {
            Some(dir) => crate::paths::MachineRoot::at(dir.clone()),
            None => crate::paths::MachineRoot::default_location(),
        }
    }

    /// Where the service keeps the settings: `settings_dir`, or the machine root's `settings`.
    pub fn settings_dir(&self) -> PathBuf {
        self.settings_dir
            .clone()
            .unwrap_or_else(|| self.machine_root().settings())
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
            prerelease_list_url: update.prerelease_program_list_url.clone(),
            channel: update.installer_channel(running),
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
            update.program && update.has_program_list() && update.program_dir.is_some()
        })
    }

    /// The channel of the installer that put version `running` here, which a computer whose
    /// settings record no channel yet starts on.
    pub fn installer_channel(&self, running: &str) -> ReleaseChannel {
        self.update.as_ref().map_or_else(
            || ReleaseChannel::of_version(running),
            |update| update.installer_channel(running),
        )
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
    /// The image's own kernel (`bzImage`, or `Image` for arm64) wins over `kernel` either way.
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
        // An x86_64 image names its kernel bzImage; an arm64 one, Image, the name the
        // arm64 kernel build gives it.
        let kernel = ["bzImage", "Image"]
            .iter()
            .map(|name| dir.join(name))
            .find(|own| own.exists())
            .unwrap_or_else(|| self.kernel.clone());
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
        let pstore_disk = Some(crate::paths::pstore_disk_beside(&data_disk));
        let addon_disk = Some(crate::paths::addon_disk_beside(&data_disk));
        AndroidVmConfig {
            owner: format!("DSA ({vm_name})"),
            kernel: files.kernel,
            initrd: files.initrd,
            kernel_cmdline: files.kernel_cmdline,
            memory_mb: self.memory_mb.unwrap_or_else(|| host_sized_memory_mb(0)),
            processors: self.processors,
            system_image: files.system_image,
            data_disk,
            pstore_disk,
            addon_disk,
            socket_sids,
            page_reporting_order: self.page_reporting_order,
            console_pipe: Some(console_pipe_path(vm_name)),
            image_version: match (&self.system_image, &self.first_image) {
                (None, Some(first)) => Some(first.version.clone()),
                _ => None,
            },
            hosting_process_name_suffix: None,
            fine_grained_memory: false,
            network_adapter: None,
            direct_mapped: None,
        }
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
