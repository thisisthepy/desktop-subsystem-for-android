//! The Android VM's configuration, as the HCS JSON document that creates it.
//!
//! The settings follow the ones WSL uses for its own VM, because they are what keeps a
//! Hyper-V VM light on a desktop:
//! - memory backed by the pagefile and committed on first touch (`AllowOvercommit`,
//!   `EnableDeferredCommit`), and free guest pages handed back to the host
//!   (`EnableColdDiscardHint` plus `page_reporting.page_reporting_order` on the kernel
//!   command line);
//! - the kernel booted directly (`LinuxKernelDirect`), straight into Android's init;
//! - the system image attached read only, the user data on a disk that may stay sparse;
//! - Hyper-V sockets limited to SYSTEM and the user the VM belongs to.
//!
//! This module only builds the document, so it is tested on every platform. Creating the
//! VM from it is `hcs` (Windows only).

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::guid::Guid;

/// HCS allocates VM memory in 2 MiB units.
const MEMORY_GRANULARITY_MB: u32 = 2;

/// Everything needed to describe the Android VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AndroidVmConfig {
    /// Shown as the VM's owner in HCS and in diagnostics.
    pub owner: String,
    /// The guest kernel, with binder built in.
    pub kernel: PathBuf,
    /// The initial ramdisk, if the kernel needs one.
    pub initrd: Option<PathBuf>,
    /// Extra kernel command line, after the settings this module adds.
    pub kernel_cmdline: String,
    /// Maximum guest memory in MiB. Rounded down to the 2 MiB granularity.
    pub memory_mb: u32,
    pub processors: u32,
    /// The Android system image (VHDX), attached read only.
    pub system_image: PathBuf,
    /// The user data disk (VHDX), attached writable and allowed to stay sparse.
    pub data_disk: PathBuf,
    /// SIDs, besides SYSTEM, allowed to bind and connect Hyper-V sockets to this VM: the user
    /// the VM runs for.
    pub socket_sids: Vec<String>,
    /// Free page reporting granularity, as a page order (5 is 128 KiB, 9 is 2 MiB). WSL uses
    /// 5 on recent Windows builds and 9 on older ones.
    pub page_reporting_order: u8,
}

/// Why a configuration cannot describe a VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid Android VM configuration: {}", self.0)
    }
}

impl std::error::Error for ConfigError {}

/// True for `C:\...`, `\\server\...` and `/...`, whatever platform this runs on, because
/// the document is always for Windows but is also built in tests elsewhere.
fn is_absolute(path: &Path) -> bool {
    let text = path.to_string_lossy();
    let bytes = text.as_bytes();
    path.is_absolute()
        || text.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub(crate) fn is_sid(text: &str) -> bool {
    text.starts_with("S-1-")
        && text[4..]
            .split('-')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

impl AndroidVmConfig {
    /// Checks the configuration and returns the memory size HCS will be given.
    fn validate(&self) -> Result<u32, ConfigError> {
        let mut paths = vec![
            ("kernel", &self.kernel),
            ("system image", &self.system_image),
            ("data disk", &self.data_disk),
        ];
        if let Some(initrd) = &self.initrd {
            paths.push(("initrd", initrd));
        }
        for (what, path) in paths {
            if !is_absolute(path) {
                return Err(ConfigError(format!(
                    "the {what} path {} must be absolute",
                    path.display()
                )));
            }
        }
        let memory_mb = self.memory_mb - self.memory_mb % MEMORY_GRANULARITY_MB;
        if memory_mb < 512 {
            return Err(ConfigError(format!(
                "{} MiB of memory is too little for Android (at least 512)",
                self.memory_mb
            )));
        }
        if self.processors == 0 {
            return Err(ConfigError("the VM needs at least one processor".into()));
        }
        if !(5..=10).contains(&self.page_reporting_order) {
            return Err(ConfigError(format!(
                "page reporting order {} is outside 5 to 10",
                self.page_reporting_order
            )));
        }
        if let Some(bad) = self.socket_sids.iter().find(|sid| !is_sid(sid)) {
            return Err(ConfigError(format!("{bad:?} is not a SID (S-1-...)")));
        }
        Ok(memory_mb)
    }

    /// The SDDL string that lets SYSTEM and [`socket_sids`](Self::socket_sids) use the VM's
    /// Hyper-V sockets, and nobody else.
    pub fn socket_security_descriptor(&self) -> String {
        let mut sddl = String::from("D:P(A;;FA;;;SY)");
        for sid in &self.socket_sids {
            sddl.push_str(&format!("(A;;FA;;;{sid})"));
        }
        sddl
    }

    /// The kernel command line: the settings the VM needs, then [`kernel_cmdline`](Self::kernel_cmdline).
    pub fn full_kernel_cmdline(&self) -> String {
        let mut cmdline = format!(
            "console=hvc0 panic=-1 page_reporting.page_reporting_order={}",
            self.page_reporting_order
        );
        if !self.kernel_cmdline.trim().is_empty() {
            cmdline.push(' ');
            cmdline.push_str(self.kernel_cmdline.trim());
        }
        cmdline
    }

    /// The HCS compute system document (schema 2.3) that creates the VM.
    pub fn to_hcs_document(&self) -> Result<Value, ConfigError> {
        let memory_mb = self.validate()?;
        let mut kernel = json!({
            "KernelFilePath": path_string(&self.kernel),
            "KernelCmdLine": self.full_kernel_cmdline(),
        });
        if let Some(initrd) = &self.initrd {
            kernel["InitRdPath"] = Value::String(path_string(initrd));
        }
        let sddl = self.socket_security_descriptor();
        Ok(json!({
            "Owner": self.owner,
            "SchemaVersion": { "Major": 2, "Minor": 3 },
            "ShouldTerminateOnLastHandleClosed": true,
            "VirtualMachine": {
                "StopOnReset": true,
                "Chipset": {
                    "UseUtc": true,
                    "LinuxKernelDirect": kernel,
                },
                "ComputeTopology": {
                    "Memory": {
                        "SizeInMB": memory_mb,
                        "AllowOvercommit": true,
                        "EnableDeferredCommit": true,
                        "EnableColdDiscardHint": true,
                    },
                    "Processor": { "Count": self.processors },
                },
                "Devices": {
                    "Scsi": {
                        "0": {
                            "Attachments": {
                                "0": {
                                    "Type": "VirtualDisk",
                                    "Path": path_string(&self.system_image),
                                    "ReadOnly": true,
                                },
                                "1": {
                                    "Type": "VirtualDisk",
                                    "Path": path_string(&self.data_disk),
                                    "ReadOnly": false,
                                    "AlwaysAllowSparseFiles": true,
                                },
                            },
                        },
                    },
                    "HvSocket": {
                        "HvSocketConfig": {
                            "DefaultBindSecurityDescriptor": sddl,
                            "DefaultConnectSecurityDescriptor": sddl,
                        },
                    },
                },
            },
        }))
    }
}

/// Finds the runtime ID in an HCS properties document (`RuntimeId`, or `RuntimeID` in
/// older schemas).
pub fn runtime_id_from_properties(properties: &str) -> Option<Guid> {
    let value: Value = serde_json::from_str(properties).ok()?;
    let object = value.as_object()?;
    object
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("RuntimeId"))
        .and_then(|(_, id)| id.as_str())
        .and_then(|id| id.parse().ok())
}
