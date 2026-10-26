//! The Android VM's configuration, as the HCS JSON document that creates it.
//!
//! The settings follow the ones WSL uses for its own VM, because they are what keeps a
//! Hyper-V VM light on a desktop:
//! - memory backed by the pagefile and committed on first touch (`AllowOvercommit`,
//!   `EnableDeferredCommit`), and free guest pages handed back to the host
//!   (`EnableColdDiscardHint` plus `page_reporting.page_reporting_order` on the kernel
//!   command line, and `EnableColdHint` for zeroed pages). `EnableHotHint` is left off: it
//!   lets the guest pull pages in ahead of use, which returns nothing to the host;
//! - the kernel booted directly (`LinuxKernelDirect`), straight into Android's init;
//! - the system image attached read only, the user data on a disk that may stay sparse;
//! - Hyper-V sockets limited to SYSTEM and the user the VM belongs to;
//! - the guest's console on the first serial port, which HCS connects to a named pipe the
//!   host serves, as WSL does for its debug console. A Linux guest prints there from the
//!   first instruction (`earlycon`), so a boot that fails before Android's agent connects
//!   still leaves a record of why.
//!
//! This module only builds the document, so it is tested on every platform. Creating the
//! VM from it is `hcs` (Windows only).

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::guid::Guid;

/// HCS allocates VM memory in 2 MiB units.
const MEMORY_GRANULARITY_MB: u32 = 2;

/// The kernel's console: the first serial port (COM1, I/O port 0x3f8) from the first
/// instruction through `earlycon`, then the 8250 driver. HCS connects that port to the
/// console pipe.
pub const CONSOLE_CMDLINE: &str = "earlycon=uart8250,io,0x3f8,115200 console=ttyS0,115200";

const PIPE_PREFIX: &str = r"\\.\pipe\";

/// The pipe the console of VM `vm_name` is served on: `\\.\pipe\dsa-console-<vm_name>`.
/// Each user's VM has its own name, so each has its own pipe.
pub fn console_pipe_path(vm_name: &str) -> String {
    format!("{PIPE_PREFIX}dsa-console-{vm_name}")
}

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
    /// A separate Android system image (VHDX), attached read only. Images made from the
    /// published AOSP builds have none: Android and the user's data share one disk.
    pub system_image: Option<PathBuf>,
    /// The user's disk (VHDX), attached writable and allowed to stay sparse. With no
    /// separate system image it holds Android as well.
    pub data_disk: PathBuf,
    /// SIDs, besides SYSTEM, allowed to bind and connect Hyper-V sockets to this VM: the user
    /// the VM runs for.
    pub socket_sids: Vec<String>,
    /// Free page reporting granularity, as a page order (5 is 128 KiB, 9 is 2 MiB). WSL uses
    /// 5 on recent Windows builds and 9 on older ones.
    pub page_reporting_order: u8,
    /// The named pipe (`\\.\pipe\...`) HCS connects the guest's serial console to. The
    /// host must serve it before the VM starts; without it the VM has no serial port.
    pub console_pipe: Option<String>,
    /// The version of the image booted, for the console log. It does not change the VM.
    pub image_version: Option<String>,
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
        let mut paths = vec![("kernel", &self.kernel), ("data disk", &self.data_disk)];
        if let Some(system) = &self.system_image {
            paths.push(("system image", system));
        }
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
        if let Some(pipe) = &self.console_pipe {
            let name = pipe.strip_prefix(PIPE_PREFIX).unwrap_or_default();
            if name.is_empty() || name.contains(['\\', '/', '"']) {
                return Err(ConfigError(format!(
                    "the console pipe {pipe:?} must be {PIPE_PREFIX}<name>, with no slashes \
                     or quotes in the name"
                )));
            }
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
            "{CONSOLE_CMDLINE} panic=-1 page_reporting.page_reporting_order={}",
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
        let data = json!({
            "Type": "VirtualDisk",
            "Path": path_string(&self.data_disk),
            "ReadOnly": false,
            "AlwaysAllowSparseFiles": true,
        });
        let mut devices = json!({
            "Scsi": {
                "0": {
                    "Attachments": Value::Null,
                },
            },
            "HvSocket": {
                "HvSocketConfig": {
                    "DefaultBindSecurityDescriptor": sddl,
                    "DefaultConnectSecurityDescriptor": sddl,
                },
            },
        });
        if let Some(pipe) = &self.console_pipe {
            // Port "0" is COM1, the port CONSOLE_CMDLINE names.
            devices["ComPorts"] = json!({ "0": { "NamedPipe": pipe } });
        }
        let attachments = match &self.system_image {
            Some(system) => json!({
                "0": { "Type": "VirtualDisk", "Path": path_string(system), "ReadOnly": true },
                "1": data,
            }),
            None => json!({ "0": data }),
        };
        devices["Scsi"]["0"]["Attachments"] = attachments;
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
                        // Backed by the host's pagefile instead of reserved RAM, so the host
                        // can page out and take back what the guest does not touch.
                        "AllowOvercommit": true,
                        // Committed on first touch, so memory the guest never uses costs
                        // the host nothing.
                        "EnableDeferredCommit": true,
                        // The pages the guest frees and reports (free page reporting) are
                        // discarded from the VM's memory and return to the host.
                        "EnableColdDiscardHint": true,
                        // Lets the guest also hand back pages it has zeroed, which the host
                        // then trims from the VM's working set. A guest without support for
                        // it is unaffected.
                        "EnableColdHint": true,
                    },
                    "Processor": { "Count": self.processors },
                },
                "Devices": devices,
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
