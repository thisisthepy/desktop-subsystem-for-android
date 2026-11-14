//! The Android VM's configuration, as the HCS JSON document that creates it.
//!
//! The settings follow the ones WSL uses for its own VM, because they are what keeps a
//! Hyper-V VM light on a desktop:
//! - memory backed by the pagefile and committed on first touch (`AllowOvercommit`,
//!   `EnableDeferredCommit`), and free guest pages handed back to the host
//!   (`EnableColdDiscardHint` plus `page_reporting.page_reporting_order` on the kernel
//!   command line, and `EnableColdHint` for zeroed pages). `EnableHotHint` is left off: it
//!   lets the guest pull pages in ahead of use, which returns nothing to the host;
//! - on Windows builds that can take it, small backing pages and 64 KiB fault clusters
//!   (`BackingPageSize`, `FaultClusterSizeShift`, `DirectMapFaultClusterSizeShift`). Without
//!   them one touched guest page can commit far more than itself on the host, and a freed
//!   128 KiB block need not free anything there. The guest's kernel is told the cluster
//!   (`page_reporting.fault_cluster_order`), so it reports again the free pages the host backs
//!   again together with a page the guest touches;
//! - on x86_64, the kernel booted directly (`LinuxKernelDirect`), straight into Android's
//!   init. Hyper-V on ARM64 has no direct Linux boot, so there, as WSL does, the VM's UEFI
//!   firmware starts the kernel's EFI stub from a folder it reads over VMBus (`VmbFs`), and
//!   the stub loads the initrd from the same folder (see [`GuestArch`]);
//! - the system image attached read only, the user data on a disk that may stay sparse. In the
//!   host's direct-mapped mode (off by default) the system disk and the add-on disk are virtual
//!   persistent memory instead, mapped from their files, so the guest maps their files rather
//!   than caching them (see [`DirectMapped`]);
//! - Hyper-V sockets limited to SYSTEM and the user the VM belongs to;
//! - the guest's console on the first serial port (COM1 on x86_64, a PL011 UART on ARM64),
//!   which HCS connects to a named pipe the host serves, as WSL does for its debug console.
//!   A Linux guest prints there from the first instruction (`earlycon`), so a boot that fails
//!   before Android's agent connects still leaves a record of why.
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

/// The ARM64 kernel's console: the VM's PL011 UART, at the address Hyper-V gives it on ARM64
/// (the one WSL's `earlycon` names), from the first instruction, then the AMBA driver's
/// `ttyAMA0`. HCS connects that port, as COM1, to the console pipe.
pub const ARM64_CONSOLE_CMDLINE: &str = "earlycon=pl011,0xeffec000,115200 console=ttyAMA0,115200";

/// The architecture of the guest, which is the host's: Hyper-V runs guests of its own
/// architecture only. It decides how the VM boots and which console parameters
/// the kernel is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestArch {
    /// `LinuxKernelDirect` boot of a `bzImage`, console on COM1.
    X86_64,
    /// UEFI boot of an `Image` with its EFI stub from a `VmbFs` folder, console on the PL011
    /// UART.
    Arm64,
}

impl GuestArch {
    /// The architecture this host was built for.
    pub const HOST: GuestArch = if cfg!(target_arch = "aarch64") {
        GuestArch::Arm64
    } else {
        GuestArch::X86_64
    };

    /// The console parameters for the kernel command line.
    pub fn console_cmdline(self) -> &'static str {
        match self {
            GuestArch::X86_64 => CONSOLE_CMDLINE,
            GuestArch::Arm64 => ARM64_CONSOLE_CMDLINE,
        }
    }

    /// Whether the kernel and the initrd must sit in one folder of their own: the firmware
    /// reads both from the `VmbFs` folder, and the VM can read everything in it.
    pub fn boots_from_a_folder(self) -> bool {
        self == GuestArch::Arm64
    }
}

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
    /// The small disk of fixed size the guest kernel keeps its last log on (pstore), attached
    /// writable after the other disks. With none the guest keeps no log across a panic.
    pub pstore_disk: Option<PathBuf>,
    /// The user's add-on disk (a fixed VHD holding the images of the add-ons they installed),
    /// attached read only after the pstore disk. With none the guest has no add-ons.
    pub addon_disk: Option<PathBuf>,
    /// SIDs, besides SYSTEM, allowed to bind and connect Hyper-V sockets to this VM: the user
    /// the VM runs for.
    pub socket_sids: Vec<String>,
    /// Free page reporting granularity, as a page order (0 is 4 KiB, 5 is 128 KiB, 9 is
    /// 2 MiB). DSA uses 0 on Windows builds with small backing pages and 9 on older ones.
    pub page_reporting_order: u8,
    /// The named pipe (`\\.\pipe\...`) HCS connects the guest's serial console to. The
    /// host must serve it before the VM starts; without it the VM has no serial port.
    pub console_pipe: Option<String>,
    /// The version of the image booted, for the console log. It does not change the VM.
    pub image_version: Option<String>,
    /// Appended to the name of the process that holds the VM's memory, so Task Manager
    /// shows `vmmem` followed by it (`VmmemDSA` for `DSA`). `None` leaves the name plain
    /// `vmmem`, which is what Windows builds without the setting need: they reject a
    /// document that names it. See [`hosting_process_name_suffix_supported`].
    pub hosting_process_name_suffix: Option<String>,
    /// Back the VM with small pages and fault 64 KiB at a time, which keeps the host's share
    /// close to what the guest uses. Only for builds where
    /// [`fine_grained_memory_supported`] says so.
    pub fine_grained_memory: bool,
    /// The synthetic network adapter, connected to the HCN endpoint the host made for this
    /// VM (see `network`). `None` gives the VM no network.
    pub network_adapter: Option<NetworkAdapter>,
    /// The host's direct-mapped mode: the system disk, and the add-on disk when there is one,
    /// attached as virtual persistent memory instead of SCSI disks, so the guest maps their
    /// files rather than caching them. `None` attaches them as disks.
    pub direct_mapped: Option<DirectMapped>,
}

/// How a direct-mapped boot attaches the read-only disks (see
/// [`AndroidVmConfig::direct_mapped`] and `dsa_host_core::direct_mapped`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMapped {
    /// The system disk as a fixed VHD: the uncompressed copy the service made of
    /// [`AndroidVmConfig::system_image`] and checked against the image's hash (virtual
    /// persistent memory takes a flat image only).
    pub system_disk: PathBuf,
    /// The size of the system disk's data, without its VHD footer: a whole number of 2 MiB.
    pub system_bytes: u64,
    /// The size of the add-on disk's data, without its footer, while it is attached; it is a
    /// fixed VHD already. `None` attaches no add-on disk as persistent memory.
    pub addon_bytes: Option<u64>,
    /// The image's support for the mode: the mount table it boots with.
    pub support: dsa_host_core::direct_mapped::ImageSupport,
}

/// The platform device the guest kernel puts Hyper-V's persistent memory bus (ACPI NFIT)
/// below, which a direct-mapped boot names as a boot device.
pub const PERSISTENT_MEMORY_BOOT_DEVICE: &str = "nfit";

/// Linux maps persistent memory in sections of this size; a device of another size gets no
/// page structs, and so no DAX.
pub const PERSISTENT_MEMORY_SECTION: u64 = 2 << 20;

/// A synthetic network adapter of the VM, which the guest's hv_netvsc drives as eth0.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAdapter {
    /// The HCN endpoint the adapter is connected to.
    pub endpoint_id: Guid,
    /// The adapter's MAC address, as HCN gave it to the endpoint (`00-15-5D-...`).
    pub mac_address: String,
}

/// The name of the VM's network adapter in the HCS document.
pub const NETWORK_ADAPTER_NAME: &str = "eth0";

impl NetworkAdapter {
    /// The adapter as the HCS document's `Devices.NetworkAdapters` entry, and as the settings
    /// of the modify request that adds it to a running VM.
    pub fn to_hcs(&self) -> Value {
        json!({
            "EndpointId": self.endpoint_id.to_string(),
            "MacAddress": self.mac_address,
        })
    }
}

/// Keeps what the guest kernel sets aside at boot small, as on every host
/// ([`dsa_host_core::memory::GUEST_BOOT_MEMORY_CMDLINE`] says what each setting saves).
pub const BOOT_MEMORY_CMDLINE: &str = dsa_host_core::memory::GUEST_BOOT_MEMORY_CMDLINE;

/// The fault cluster for fine-grained memory, as a power of two of 4 KiB pages: 64 KiB. Free
/// page reporting may report pieces smaller than it (order 0 reports every free 4 KiB page);
/// the guest is told the cluster (`page_reporting.fault_cluster_order`), so free pages the
/// host backs again with a neighbour in their cluster are reported again.
const FAULT_CLUSTER_SHIFT: u32 = 4;

/// The HCS modify request that sets a running VM's memory to `mb` MiB, rounded down to the
/// 2 MiB granularity. Memory above the size the VM started with is hot-added and onlined by
/// the guest's Hyper-V balloon driver; a smaller size is taken back by ballooning.
pub fn memory_resize_request(mb: u64) -> String {
    let mb = mb - mb % u64::from(MEMORY_GRANULARITY_MB);
    json!({
        "ResourcePath": "VirtualMachine/ComputeTopology/Memory/SizeInMB",
        "RequestType": "Update",
        "Settings": mb,
    })
    .to_string()
}

/// Whether this Windows build can back a VM with small pages and short fault clusters: those
/// with the fix for the deadlock such a VM can hit when it is torn down. These are the same
/// builds and revisions WSL checks before it sets the same fields.
pub fn fine_grained_memory_supported(build: u32, update_revision: u32) -> bool {
    const GERMANIUM: u32 = 26100;
    const COBALT: u32 = 22000;
    const IRON: u32 = 20348;
    const VIBRANIUM_22H2: u32 = 19045;
    build >= GERMANIUM
        || (build >= COBALT && update_revision >= 2360)
        || (build >= IRON && update_revision >= 1970)
        || (build >= VIBRANIUM_22H2 && update_revision >= 3393)
}

/// The suffix DSA gives its memory process, which Task Manager then shows as `VmmemDSA`.
pub const HOSTING_PROCESS_NAME_SUFFIX: &str = "DSA";

/// Whether HCS on this Windows build accepts `HostingProcessNameSuffix`. The setting came
/// with Windows 11 21H2 update 22000.71, the 22138 to 22349 refresh builds, and 22420 and
/// later; these are the same build numbers WSL checks before it sets the same field.
pub fn hosting_process_name_suffix_supported(build: u32, update_revision: u32) -> bool {
    const COBALT: u32 = 22000;
    const COBALT_RELEASE_UBR: u32 = 71;
    const COBALT_REFRESH: u32 = 22138;
    const NICKEL_FLOOR: u32 = 22350;
    const NICKEL: u32 = 22420;
    build >= NICKEL
        || (COBALT_REFRESH..NICKEL_FLOOR).contains(&build)
        || (build == COBALT && update_revision >= COBALT_RELEASE_UBR)
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
    fn validate(&self, arch: GuestArch) -> Result<u32, ConfigError> {
        let mut paths = vec![("kernel", &self.kernel), ("data disk", &self.data_disk)];
        if let Some(system) = &self.system_image {
            paths.push(("system image", system));
        }
        if let Some(pstore) = &self.pstore_disk {
            paths.push(("pstore disk", pstore));
        }
        if let Some(initrd) = &self.initrd {
            paths.push(("initrd", initrd));
        }
        if let Some(mapped) = &self.direct_mapped {
            paths.push(("persistent memory system disk", &mapped.system_disk));
            // hcsshim, Microsoft's own client of these devices, turns them off on ARM64.
            if arch != GuestArch::X86_64 {
                return Err(ConfigError(
                    "the direct-mapped mode needs virtual persistent memory, which DSA uses on \
                     x86_64 hosts only"
                        .into(),
                ));
            }
            if let Some(bad) = std::iter::once(mapped.system_bytes)
                .chain(mapped.addon_bytes)
                .find(|size| *size == 0 || size % PERSISTENT_MEMORY_SECTION != 0)
            {
                return Err(ConfigError(format!(
                    "a persistent memory disk of {bad} bytes is not a whole number of 2 MiB, \
                     so the guest could not map it"
                )));
            }
        }
        for (what, path) in paths {
            if !is_absolute(path) {
                return Err(ConfigError(format!(
                    "the {what} path {} must be absolute",
                    path.display()
                )));
            }
        }
        if arch.boots_from_a_folder() {
            let (kernel_folder, _) = boot_folder(&self.kernel)?;
            if let Some(initrd) = &self.initrd {
                let same = split_file(initrd)
                    .is_some_and(|(folder, _)| folder.eq_ignore_ascii_case(&kernel_folder));
                if !same {
                    return Err(ConfigError(format!(
                        "on ARM64 the initrd {} must be in the kernel's folder {}: the VM's \
                         firmware reads both from there",
                        initrd.display(),
                        kernel_folder
                    )));
                }
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
        if self.page_reporting_order > 10 {
            return Err(ConfigError(format!(
                "page reporting order {} is outside 0 to 10",
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

    /// The kernel command line on this host: the settings the VM needs, then
    /// [`kernel_cmdline`](Self::kernel_cmdline).
    pub fn full_kernel_cmdline(&self) -> String {
        self.full_kernel_cmdline_for(GuestArch::HOST)
    }

    /// The kernel command line for a guest of `arch`.
    pub fn full_kernel_cmdline_for(&self, arch: GuestArch) -> String {
        let fault_cluster = if self.fine_grained_memory {
            FAULT_CLUSTER_SHIFT as u8
        } else {
            0
        };
        let mut cmdline = format!(
            "{} panic=-1 {}",
            arch.console_cmdline(),
            dsa_host_core::memory::page_reporting_cmdline(self.page_reporting_order, fault_cluster)
        );
        cmdline.push(' ');
        cmdline.push_str(BOOT_MEMORY_CMDLINE);
        if self.pstore_disk.is_some() {
            cmdline.push(' ');
            cmdline.push_str(crate::pstore_disk::KERNEL_CMDLINE);
        }
        let image = match &self.direct_mapped {
            Some(mapped) => dsa_host_core::direct_mapped::kernel_cmdline(
                self.kernel_cmdline.trim(),
                PERSISTENT_MEMORY_BOOT_DEVICE,
                &mapped.support,
            ),
            None => self.kernel_cmdline.trim().to_string(),
        };
        if !image.is_empty() {
            cmdline.push(' ');
            cmdline.push_str(&image);
        }
        cmdline
    }

    /// The document's virtual persistent memory controller for a direct-mapped boot: device 0
    /// the system disk, device 1 the add-on disk while it is attached, both read only and
    /// backed by a mapping of their file (`Virtual`), never by memory of their own.
    fn persistent_memory(&self, mapped: &DirectMapped) -> Value {
        let mut devices = json!({
            "0": {
                "HostPath": path_string(&mapped.system_disk),
                "ReadOnly": true,
                "ImageFormat": "Vhd1",
            },
        });
        let mut largest = mapped.system_bytes;
        if let (Some(addons), Some(bytes)) = (&self.addon_disk, mapped.addon_bytes) {
            devices["1"] = json!({
                "HostPath": path_string(addons),
                "ReadOnly": true,
                "ImageFormat": "Vhd1",
            });
            largest = largest.max(bytes);
        }
        let count = devices.as_object().map_or(0, |d| d.len());
        json!({
            "Devices": devices,
            // As many devices as there are images: a larger count adds empty ones.
            "MaximumCount": count,
            "MaximumSizeBytes": largest.div_ceil(PERSISTENT_MEMORY_SECTION) * PERSISTENT_MEMORY_SECTION,
            "Backing": "Virtual",
        })
    }

    /// The HCS compute system document (schema 2.3) that creates the VM on this host.
    /// Checks what does not depend on where the boot's files are placed (paths absolute,
    /// processors, memory), so a bad configuration is refused before any file is copied.
    pub fn check_settings(&self) -> Result<(), ConfigError> {
        // x86_64 places no rule on the folders, which only the prepared boot can meet.
        self.validate(GuestArch::X86_64).map(|_| ())
    }

    pub fn to_hcs_document(&self) -> Result<Value, ConfigError> {
        self.to_hcs_document_for(GuestArch::HOST)
    }

    /// The boot settings of the document's `Chipset`, as `(key, value)`.
    fn boot_settings(&self, arch: GuestArch) -> Result<(&'static str, Value), ConfigError> {
        let cmdline = self.full_kernel_cmdline_for(arch);
        match arch {
            GuestArch::X86_64 => {
                let mut kernel = json!({
                    "KernelFilePath": path_string(&self.kernel),
                    "KernelCmdLine": cmdline,
                });
                if let Some(initrd) = &self.initrd {
                    kernel["InitRdPath"] = Value::String(path_string(initrd));
                }
                Ok(("LinuxKernelDirect", kernel))
            }
            GuestArch::Arm64 => {
                let (folder, kernel_name) = boot_folder(&self.kernel)?;
                // The EFI stub takes its command line from the boot entry's load options, and
                // loads the initrd that `initrd=` names from the folder it was started from.
                let options = match self.initrd.as_ref().and_then(|i| i.file_name()) {
                    Some(initrd) => format!("initrd=\\{} {cmdline}", initrd.to_string_lossy()),
                    None => cmdline,
                };
                let uefi = json!({
                    "BootThis": {
                        "DeviceType": "VmbFs",
                        "VmbFsRootPath": folder,
                        "DevicePath": format!("\\{kernel_name}"),
                        "OptionalData": options,
                    },
                });
                Ok(("Uefi", uefi))
            }
        }
    }

    /// The HCS compute system document (schema 2.3) that creates the VM for a guest of
    /// `arch`.
    pub fn to_hcs_document_for(&self, arch: GuestArch) -> Result<Value, ConfigError> {
        let memory_mb = self.validate(arch)?;
        let (boot_key, boot) = self.boot_settings(arch)?;
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
            // Port "0" is COM1 on x86_64 and the PL011 UART on ARM64: the port each
            // architecture's console parameters name.
            devices["ComPorts"] = json!({ "0": { "NamedPipe": pipe } });
        }
        // In the direct-mapped mode the read-only disks are persistent memory, not SCSI disks.
        let mapped = self.direct_mapped.as_ref();
        let system_on_scsi = self.system_image.as_ref().filter(|_| mapped.is_none());
        let attachments = match system_on_scsi {
            Some(system) => json!({
                "0": { "Type": "VirtualDisk", "Path": path_string(system), "ReadOnly": true },
                "1": data,
            }),
            None => json!({ "0": data }),
        };
        let mut attachments = attachments;
        if let Some(pstore) = &self.pstore_disk {
            let lun = attachments.as_object().map_or(0, |a| a.len()).to_string();
            attachments[lun] = json!({
                "Type": "VirtualDisk",
                "Path": path_string(pstore),
                "ReadOnly": false,
            });
        }
        let addons_on_scsi = self
            .addon_disk
            .as_ref()
            .filter(|_| mapped.is_none_or(|m| m.addon_bytes.is_none()));
        if let Some(addons) = addons_on_scsi {
            let lun = attachments.as_object().map_or(0, |a| a.len()).to_string();
            attachments[lun] = json!({
                "Type": "VirtualDisk",
                "Path": path_string(addons),
                "ReadOnly": true,
            });
        }
        devices["Scsi"]["0"]["Attachments"] = attachments;
        if let Some(mapped) = mapped {
            devices["VirtualPMem"] = self.persistent_memory(mapped);
        }
        if let Some(adapter) = &self.network_adapter {
            devices["NetworkAdapters"] = json!({});
            devices["NetworkAdapters"][NETWORK_ADAPTER_NAME] = adapter.to_hcs();
        }
        let mut memory = json!({
            "SizeInMB": memory_mb,
            "AllowOvercommit": true,
            "EnableDeferredCommit": true,
            "EnableColdDiscardHint": true,
            // Lets the guest also hand back pages it has zeroed, which the host then trims
            // from the VM's working set. A guest without support for it is unaffected.
            "EnableColdHint": true,
        });
        if self.fine_grained_memory {
            memory["BackingPageSize"] = json!("Small");
            memory["FaultClusterSizeShift"] = json!(FAULT_CLUSTER_SHIFT);
            memory["DirectMapFaultClusterSizeShift"] = json!(FAULT_CLUSTER_SHIFT);
        }
        if let Some(suffix) = &self.hosting_process_name_suffix {
            memory["HostingProcessNameSuffix"] = Value::String(suffix.clone());
        }
        let mut chipset = json!({ "UseUtc": true });
        chipset[boot_key] = boot;
        Ok(json!({
            "Owner": self.owner,
            "SchemaVersion": { "Major": 2, "Minor": 3 },
            "ShouldTerminateOnLastHandleClosed": true,
            "VirtualMachine": {
                "StopOnReset": true,
                "Chipset": chipset,
                "ComputeTopology": {
                    "Memory": memory,
                    "Processor": { "Count": self.processors },
                },
                "Devices": devices,
            },
        }))
    }
}

/// The folder the ARM64 VM's firmware reads the kernel from, and the kernel's name in it.
fn boot_folder(kernel: &Path) -> Result<(String, String), ConfigError> {
    match split_file(kernel) {
        Some(parts) => Ok(parts),
        None => Err(ConfigError(format!(
            "the kernel path {} must name a file in a folder",
            kernel.display()
        ))),
    }
}

/// Splits a Windows path into its folder and file name. Both `\` and `/` separate on every
/// build host, because HCS on Windows reads the path, not the process that wrote it.
fn split_file(path: &Path) -> Option<(String, String)> {
    let text = path_string(path);
    let at = text.rfind(['\\', '/'])?;
    let (folder, name) = (&text[..at], &text[at + 1..]);
    if folder.is_empty() || name.is_empty() {
        return None;
    }
    Some((folder.to_string(), name.to_string()))
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
