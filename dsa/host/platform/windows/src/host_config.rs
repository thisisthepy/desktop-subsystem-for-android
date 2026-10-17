//! The host's configuration file: where the Android VM's files are and how it runs.
//!
//! JSON, read by `dsa-hostd`. Unknown keys are an error, so a misspelt setting is reported
//! rather than silently ignored.
//!
//! ```json
//! {
//!   "kernel": "C:\\Program Files\\DSA\\vm\\bzImage",
//!   "system_image": "C:\\Program Files\\DSA\\vm\\system.vhdx",
//!   "data_disk": "C:\\ProgramData\\DSA\\data.vhdx",
//!   "memory_mb": 4096
//! }
//! ```

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use dsa_host_core::session::SessionConfig;
use serde::Deserialize;

use crate::vm::AndroidVmConfig;

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
    pub data_disk: PathBuf,
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
        serde_json::from_str(json).map_err(|e| HostConfigError(e.to_string()))
    }

    /// The VM configuration. `fallback_sid` is used when the file names no user.
    pub fn vm_config(&self, fallback_sid: Option<&str>) -> AndroidVmConfig {
        let socket_sids = self
            .user_sid
            .as_deref()
            .or(fallback_sid)
            .map(|sid| vec![sid.to_string()])
            .unwrap_or_default();
        AndroidVmConfig {
            owner: format!("DSA ({})", self.vm_name),
            kernel: self.kernel.clone(),
            initrd: self.initrd.clone(),
            kernel_cmdline: self.kernel_cmdline.clone(),
            memory_mb: self.memory_mb,
            processors: self.processors,
            system_image: self.system_image.clone(),
            data_disk: self.data_disk.clone(),
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
