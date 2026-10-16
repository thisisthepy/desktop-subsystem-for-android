//! The host's configuration file: where the Android VM's files are and how it runs.
//!
//! JSON, read by `dsa-hostd`. Unknown keys are an error, so a misspelt setting is reported
//! rather than silently ignored.
//!
//! ```json
//! {
//!   "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
//!   "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx",
//!   "data_dir": "C:\\ProgramData\\DSA\\users",
//!   "memory_mb": 4096
//! }
//! ```
//!
//! The service runs one VM per user. Each user's VM is named after the user's SID, keeps
//! its data on a disk in its own folder under `data_dir`, and lets only that user (and
//! SYSTEM) use its Hyper-V sockets. `data_disk` names a single disk instead, for running
//! one VM by hand.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use dsa_host_core::session::SessionConfig;
use serde::Deserialize;

use crate::vm::{is_sid, AndroidVmConfig};

/// `<dir>\<sid>\data.vhdx`. Built as text so it is a Windows path on every platform the
/// tests run on.
fn user_data_disk(dir: &std::path::Path, sid: &str) -> PathBuf {
    let dir = dir.to_string_lossy();
    PathBuf::from(format!("{}\\{sid}\\data.vhdx", dir.trim_end_matches('\\')))
}

/// One user's VM: its HCS name, its configuration and how large its data disk may grow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserVm {
    pub vm_name: String,
    pub config: AndroidVmConfig,
    pub data_disk_size_gb: u32,
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
fn default_pipe_name() -> String {
    "dsa-host".into()
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
    pub system_image: PathBuf,
    /// One data disk, for running a single VM by hand.
    #[serde(default)]
    pub data_disk: Option<PathBuf>,
    /// The folder holding each user's data disk, as `<data_dir>\<SID>\data.vhdx`.
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
        if config.data_disk.is_none() && config.data_dir.is_none() {
            return Err(HostConfigError(
                "name either data_disk (one VM) or data_dir (one VM per user)".into(),
            ));
        }
        Ok(config)
    }

    /// The VM configuration for running one VM by hand. `fallback_sid` is used when the file
    /// names no user.
    pub fn vm_config(&self, fallback_sid: Option<&str>) -> AndroidVmConfig {
        let user = self.user_sid.as_deref().or(fallback_sid);
        let socket_sids = user.map(|sid| vec![sid.to_string()]).unwrap_or_default();
        let data_disk = match (&self.data_disk, &self.data_dir, user) {
            (Some(disk), _, _) => disk.clone(),
            (None, Some(dir), Some(sid)) => user_data_disk(dir, sid),
            (None, Some(dir), None) => dir.join("data.vhdx"),
            (None, None, _) => unreachable!("parse requires data_disk or data_dir"),
        };
        self.base_vm_config(&self.vm_name, data_disk, socket_sids)
    }

    /// The VM of the user with SID `sid`, as the service runs it.
    pub fn user_vm(&self, sid: &str) -> Result<UserVm, HostConfigError> {
        // The SID becomes part of a path and a VM name, so only a well-formed one is used.
        if !is_sid(sid) {
            return Err(HostConfigError(format!(
                "{sid:?} is not a Windows SID, so no VM is made for it"
            )));
        }
        let dir = self.data_dir.as_ref().ok_or_else(|| {
            HostConfigError(
                "serving users needs data_dir, the folder for each user's data disk".into(),
            )
        })?;
        let vm_name = format!("{}-{sid}", self.vm_name);
        let config = self.base_vm_config(&vm_name, user_data_disk(dir, sid), vec![sid.into()]);
        Ok(UserVm {
            vm_name,
            config,
            data_disk_size_gb: self.data_disk_size_gb,
        })
    }

    fn base_vm_config(
        &self,
        vm_name: &str,
        data_disk: PathBuf,
        socket_sids: Vec<String>,
    ) -> AndroidVmConfig {
        AndroidVmConfig {
            owner: format!("DSA ({vm_name})"),
            kernel: self.kernel.clone(),
            initrd: self.initrd.clone(),
            kernel_cmdline: self.kernel_cmdline.clone(),
            memory_mb: self.memory_mb,
            processors: self.processors,
            system_image: self.system_image.clone(),
            data_disk,
            socket_sids,
            page_reporting_order: self.page_reporting_order,
        }
    }

    pub fn session_config(&self) -> SessionConfig {
        SessionConfig {
            connect_timeout: Duration::from_secs(self.connect_timeout_secs),
            stop_timeout: Duration::from_secs(self.stop_timeout_secs),
        }
    }
}
