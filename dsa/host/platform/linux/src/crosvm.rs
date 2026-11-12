//! The Android VM as a crosvm command line, under KVM.
//!
//! The VMM is crosvm (BSD-3-Clause), the VMM ChromeOS and the Android Virtualization Framework
//! run Android guests with. DSA builds it from a pinned upstream revision and ships it beside
//! its own programs (`/usr/lib/dsa/bin/crosvm`), so every distribution runs the same VMM. The
//! VM has:
//! - the kernel booted directly (the kernel, `--initrd`, `--params`), straight into Android's
//!   init, as the Windows host does with `LinuxKernelDirect`;
//! - the image's disks, the user's disk, the pstore disk and the user's add-on disk (read
//!   only, while they have add-ons) on virtio-blk, at fixed PCI addresses. Android's first stage init makes
//!   `/dev/block/by-name` links only for disks below the devices `androidboot.boot_devices`
//!   names, so the command line names those addresses in place of the image's `vmbus`. The
//!   partitions themselves are found by the disk's GPT GUID, whatever the bus. In the host's
//!   direct-mapped mode (off by default) the system disk and the add-on disk are virtio-pmem
//!   instead, which the guest maps its files from without caching them (see [`DirectMapped`]);
//! - vsock through the kernel's vhost-vsock, so the guest reaches the host at CID 2 on the same
//!   ports as on Hyper-V (5000 control, 5001 log, 5590 and 5591 file sharing, 5555 adb), and
//!   the host's `AF_VSOCK` sockets serve it (see `vsock`);
//! - memory that returns to the host: a virtio balloon with free page reporting, so the pages
//!   the guest frees are handed back to the host. A VM that may grow boots at its starting
//!   size, with the rest up to its largest size as a virtio-mem region (`--virtio-mem`, from
//!   the patch DSA's crosvm build applies); the host raises the region's requested size while
//!   the guest is short of memory and lowers it after, and the blocks the guest unplugs are
//!   freed on the host at once. The guest kernel keeps page structs only for the memory it
//!   has, in that memory (`memory_hotplug.memmap_on_memory=1`), and puts plugged memory in
//!   the movable zone so it can be unplugged again. Nothing pins guest memory: no huge pages,
//!   no locked memory;
//! - a virtio network adapter on passt's user-mode NAT (vhost-user), or on a macvtap on the
//!   host's LAN for advanced networking (see `network`);
//! - the guest's console on a virtio console (`hvc0`) and its earliest messages on the first
//!   serial port, both on crosvm's standard output, which the host keeps in the console log;
//! - crosvm's control socket, through which the host resizes, suspends, resumes and stops the
//!   VM (see `crosvm_control`);
//! - crosvm's sandbox: each device runs in a minijail of its own.
//!
//! This module only builds the command line, so it is tested on every platform.

use std::fmt;
use std::path::{Path, PathBuf};

/// The architecture of the guest, which is the host's: KVM runs guests of its own
/// architecture only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestArch {
    /// A `bzImage`; the PCI root is the legacy `pci0000:00`.
    X86_64,
    /// An `Image`; the PCI root is crosvm's generic host bridge at 0x10000.
    Arm64,
}

/// The PCI slots of the VM's disks and network adapter on bus 0, fixed so the guest finds
/// its disks at the same place at every boot.
pub mod slot {
    pub const NETWORK: u8 = 0x03;
    pub const SYSTEM_DISK: u8 = 0x10;
    pub const DATA_DISK: u8 = 0x11;
    pub const PSTORE_DISK: u8 = 0x12;
    pub const ADDON_DISK: u8 = 0x13;
}

/// The platform device of crosvm's arm64 PCI host bridge, as Linux names it from the device
/// tree crosvm writes (its `pci` node at 0x10000).
const ARM64_PCI_DEVICE: &str = "10000.pci";

impl GuestArch {
    /// The architecture this host was built for.
    pub const HOST: GuestArch = if cfg!(target_arch = "aarch64") {
        GuestArch::Arm64
    } else {
        GuestArch::X86_64
    };

    /// What `androidboot.boot_devices` names for disks in `slots`.
    pub fn boot_devices(self, slots: &[u8]) -> String {
        match self {
            GuestArch::X86_64 => slots
                .iter()
                .map(|slot| format!("pci0000:00/0000:00:{slot:02x}.0"))
                .collect::<Vec<_>>()
                .join(","),
            // Below the host bridge, the first platform device up the path.
            GuestArch::Arm64 => ARM64_PCI_DEVICE.to_string(),
        }
    }
}

/// The VM's network adapter, and what it is connected to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkBackend {
    /// No adapter.
    None,
    /// passt serving user-mode NAT as a vhost-user network device on this Unix socket.
    Passt { socket: PathBuf },
    /// A tap device (a macvtap on the host's LAN) open as this file descriptor in crosvm.
    Tap { fd: i32 },
}

/// Everything needed to describe the Android VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmConfig {
    /// The process's name (crosvm keeps its first 15 bytes) and the name in diagnostics.
    pub name: String,
    pub arch: GuestArch,
    /// The guest kernel, with binder built in.
    pub kernel: PathBuf,
    /// The boot's initrd, with its bootconfig.
    pub initrd: PathBuf,
    /// The image's kernel command line, after the settings this module adds.
    pub kernel_cmdline: String,
    /// The memory the VM starts with, in MiB.
    pub memory_mb: u32,
    /// How far the VM may grow while it runs, in MiB; `None` keeps it at `memory_mb`.
    pub memory_max_mb: Option<u32>,
    pub processors: u32,
    /// The Android system disk, attached read only. Images with one disk have none.
    pub system_image: Option<PathBuf>,
    /// The user's disk, attached writable and sparse, so the guest's discards leave the file.
    pub data_disk: PathBuf,
    /// The small raw disk the guest kernel keeps its last log on (pstore).
    pub pstore_disk: Option<PathBuf>,
    /// The user's add-on disk, attached read only: the images of the add-ons they installed.
    pub addon_disk: Option<PathBuf>,
    /// The guest's vsock address. Unique on the machine while the VM runs.
    pub guest_cid: u32,
    /// Free page reporting granularity, as a page order (9 is 2 MiB).
    pub page_reporting_order: u8,
    /// Where crosvm serves its control socket.
    pub control_socket: PathBuf,
    /// The empty folder crosvm's sandbox makes each device's root; `None` runs the devices
    /// without the sandbox.
    pub sandbox_root: Option<PathBuf>,
    pub network: NetworkBackend,
    /// The adapter's MAC address, for a tap. passt's adapter takes the address passt gives.
    pub mac_address: String,
    /// The host's direct-mapped mode: the system disk, and the add-on disk when
    /// [`DirectMapped::addons`] says so, on virtio-pmem instead of virtio-blk, so the guest maps
    /// their files from the host's mapping rather than caching them. `None` attaches them as
    /// disks.
    pub direct_mapped: Option<DirectMapped>,
}

/// How a direct-mapped boot attaches the read-only disks (`dsa_host_core::direct_mapped`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectMapped {
    /// The image's support for the mode: the mount table it boots with.
    pub support: dsa_host_core::direct_mapped::ImageSupport,
    /// Whether the add-on disk is persistent memory too: only one in the layout the mode maps,
    /// with a partition over each image.
    pub addons: bool,
}

/// The platform device the guest kernel puts each virtio-pmem bus below, which a
/// direct-mapped boot names as a boot device: crosvm picks a pmem device's PCI slot itself, so
/// no slot can be named for it.
pub const PERSISTENT_MEMORY_BOOT_DEVICE: &str = "virtio-pmem";

/// Why a configuration cannot describe a VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub String);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ConfigError {}

/// The pstore/blk settings for the kernel command line: the partition by its UUID, the same
/// disk layout as on every host (see `pstore`).
pub const PSTORE_CMDLINE: &str = "pstore_blk.blkdev=PARTUUID=5f8b3c2e-9a41-4d6e-b07c-1e2d3f4a5b6c \
     pstore_blk.best_effort=y pstore_blk.kmsg_size=64 pstore_blk.console_size=256 \
     pstore_blk.pmsg_size=256";

/// What the host writes to the console's input to stop the guest in order. On the virtio
/// console (`hvc0`) a Ctrl-O starts a SysRq: `s` syncs every file system, `u` remounts them
/// read only, `o` powers the VM off. It is what the Hyper-V host's shutdown request does in
/// the guest kernel (a sync, then power off), with the file systems left clean as well.
pub const SHUTDOWN_SEQUENCE: [&[u8]; 3] = [b"\x0fs", b"\x0fu", b"\x0fo"];

/// The memory hot-plug settings for the kernel command line: the page structs of plugged
/// memory live in that memory, so a small VM can take on a lot and keeps page structs only for
/// what it has, and plugged memory goes online in the movable zone (while the kernel zone
/// keeps enough to manage it), so the guest can unplug it again.
pub const MEMORY_HOTPLUG_CMDLINE: &str =
    "memory_hotplug.memmap_on_memory=1 memory_hotplug.online_policy=auto-movable";

/// The virtio-mem region is a whole number of these, in MiB: the guest's memory block, the unit
/// Linux adds and removes memory in.
pub const VIRTIO_MEM_GRANULE_MB: u32 = 128;

/// Writes `value` as one value of a crosvm `key=value,...` option: quoted, with `"` and `\`
/// escaped, when it holds a character that would end it.
pub fn escape(value: &str) -> String {
    if value.contains([',', '=', '"', '\\', '[', ']']) || value.contains(char::is_whitespace) {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_string()
    }
}

fn path_value(path: &Path) -> String {
    escape(&path.to_string_lossy())
}

/// Replaces `androidboot.boot_devices=...` in `cmdline` with `devices`, or adds it, and drops
/// the image's own `console=`: crosvm names the console it gives the guest.
pub fn with_boot_devices(cmdline: &str, devices: &str) -> String {
    let mut parts: Vec<String> = cmdline
        .split_whitespace()
        .filter(|part| !part.starts_with("androidboot.boot_devices="))
        .filter(|part| !part.starts_with("console="))
        .map(str::to_string)
        .collect();
    parts.push(format!("androidboot.boot_devices={devices}"));
    parts.join(" ")
}

fn pci_address(slot: u8) -> String {
    format!("00:{slot:02x}.0")
}

impl VmConfig {
    /// The slots of the disks the VM has, in order.
    pub fn disk_slots(&self) -> Vec<u8> {
        let mut slots = Vec::new();
        if self.system_on_block().is_some() {
            slots.push(slot::SYSTEM_DISK);
        }
        slots.push(slot::DATA_DISK);
        if self.pstore_disk.is_some() {
            slots.push(slot::PSTORE_DISK);
        }
        if self.addons_on_block().is_some() {
            slots.push(slot::ADDON_DISK);
        }
        slots
    }

    /// The system disk when it is a virtio-blk disk, as it is outside the direct-mapped mode.
    fn system_on_block(&self) -> Option<&PathBuf> {
        self.system_image
            .as_ref()
            .filter(|_| self.direct_mapped.is_none())
    }

    /// The system disk when it is virtio-pmem, in the direct-mapped mode.
    fn system_on_pmem(&self) -> Option<&PathBuf> {
        self.system_image
            .as_ref()
            .filter(|_| self.direct_mapped.is_some())
    }

    /// Whether the add-on disk is virtio-pmem this boot.
    fn addons_mapped(&self) -> bool {
        self.direct_mapped
            .as_ref()
            .is_some_and(|mapped| mapped.addons)
    }

    /// The add-on disk when it is a virtio-blk disk.
    fn addons_on_block(&self) -> Option<&PathBuf> {
        self.addon_disk.as_ref().filter(|_| !self.addons_mapped())
    }

    /// The kernel command line DSA passes: the memory settings, pstore, then the image's own
    /// with the disks' devices in place of Hyper-V's. crosvm puts its console settings first.
    pub fn full_kernel_cmdline(&self) -> String {
        // SysRq on the console is how the host asks the guest to sync its disks and power
        // off when it stops the VM (see `SHUTDOWN_SEQUENCE`).
        // The boot-time memory settings are the Windows host's, so the guest kernel sets aside
        // as little here as there, and the CI's memory measurement on this host reads the same
        // kernel costs a Windows user's guest has.
        let mut cmdline = format!(
            "panic=-1 sysrq_always_enabled=1 page_reporting.page_reporting_order={} {} {}",
            self.page_reporting_order,
            MEMORY_HOTPLUG_CMDLINE,
            dsa_host_core::memory::GUEST_BOOT_MEMORY_CMDLINE
        );
        if self.pstore_disk.is_some() {
            cmdline.push(' ');
            cmdline.push_str(PSTORE_CMDLINE);
        }
        let mut image = with_boot_devices(
            self.kernel_cmdline.trim(),
            &self.arch.boot_devices(&self.disk_slots()),
        );
        if let Some(mapped) = &self.direct_mapped {
            image = dsa_host_core::direct_mapped::kernel_cmdline(
                &image,
                PERSISTENT_MEMORY_BOOT_DEVICE,
                &mapped.support,
            );
        }
        cmdline.push(' ');
        cmdline.push_str(&image);
        // The boot's secret is bootconfig appended to the initrd, and the kernel reads that
        // only when the command line says so: without it the guest's daemon finds no secret.
        if !image
            .split_whitespace()
            .any(|word| word == dsa_host_core::bootconfig::CMDLINE_FLAG)
        {
            cmdline.push(' ');
            cmdline.push_str(dsa_host_core::bootconfig::CMDLINE_FLAG);
        }
        cmdline
    }

    fn check(&self) -> Result<(), ConfigError> {
        for (what, path) in [
            ("kernel", Some(&self.kernel)),
            ("initrd", Some(&self.initrd)),
            ("system image", self.system_image.as_ref()),
            ("data disk", Some(&self.data_disk)),
            ("pstore disk", self.pstore_disk.as_ref()),
            ("add-on disk", self.addon_disk.as_ref()),
            ("control socket", Some(&self.control_socket)),
            ("sandbox folder", self.sandbox_root.as_ref()),
        ] {
            if let Some(path) = path {
                if !path.is_absolute() {
                    return Err(ConfigError(format!(
                        "the {what} path {} is not absolute",
                        path.display()
                    )));
                }
            }
        }
        if self.processors == 0 {
            return Err(ConfigError("the VM needs at least one processor".into()));
        }
        if self.memory_mb < 256 {
            return Err(ConfigError(format!(
                "{} MiB is too little memory for Android",
                self.memory_mb
            )));
        }
        if self.guest_cid <= 2 || self.guest_cid == u32::MAX {
            return Err(ConfigError(format!(
                "{} is not a guest's vsock address",
                self.guest_cid
            )));
        }
        Ok(())
    }

    /// The memory the VM boots with and the virtio-mem region above it, in MiB. The region
    /// takes the VM to its largest size, rounded down to whole memory blocks.
    pub fn memory_layout(&self) -> (u32, Option<u32>) {
        let region = self
            .memory_max_mb
            .map(|max| max.saturating_sub(self.memory_mb))
            .map(|mb| mb / VIRTIO_MEM_GRANULE_MB * VIRTIO_MEM_GRANULE_MB)
            .filter(|mb| *mb > 0);
        (self.memory_mb, region)
    }

    /// crosvm's arguments, after the program name.
    pub fn crosvm_args(&self) -> Result<Vec<String>, ConfigError> {
        self.check()?;
        // Its own messages go to standard error, which the host keeps beside the console.
        let mut args: Vec<String> = ["--no-syslog", "run"].map(str::to_string).to_vec();
        if self.sandbox_root.is_none() {
            args.push("--disable-sandbox".to_string());
        }
        let mut push = |option: &str, value: String| {
            args.push(option.to_string());
            args.push(value);
        };
        push("--name", self.name.clone());
        push("--cpus", self.processors.to_string());
        let (size, region) = self.memory_layout();
        push("--mem", format!("size={size}"));
        if let Some(region) = region {
            push("--virtio-mem", region.to_string());
        }
        push(
            "--socket",
            self.control_socket.to_string_lossy().into_owned(),
        );
        if let Some(root) = &self.sandbox_root {
            push("--pivot-root", root.to_string_lossy().into_owned());
        }
        push(
            "--serial",
            "type=stdout,hardware=serial,num=1,earlycon".to_string(),
        );
        // The console's input is the host's way to send the SysRq a stop begins with.
        push(
            "--serial",
            "type=stdout,hardware=virtio-console,num=1,console,stdin".to_string(),
        );
        push("--vsock", format!("cid={}", self.guest_cid));
        // In the direct-mapped mode crosvm maps the read-only disks' files into the guest as
        // virtio-pmem, read only and shared with the host's page cache, and pads each to the
        // 2 MiB the guest maps persistent memory in.
        if let Some(system) = self.system_on_pmem() {
            push("--pmem", format!("path={},ro=true", path_value(system)));
        }
        if let Some(addons) = self.addon_disk.as_ref().filter(|_| self.addons_mapped()) {
            push("--pmem", format!("path={},ro=true", path_value(addons)));
        }
        if let Some(system) = self.system_on_block() {
            push(
                "--block",
                format!(
                    "path={},ro=true,lock=false,pci-address={}",
                    path_value(system),
                    pci_address(slot::SYSTEM_DISK)
                ),
            );
        }
        push(
            "--block",
            format!(
                "path={},sparse=true,pci-address={}",
                path_value(&self.data_disk),
                pci_address(slot::DATA_DISK)
            ),
        );
        if let Some(pstore) = &self.pstore_disk {
            push(
                "--block",
                format!(
                    "path={},sparse=false,pci-address={}",
                    path_value(pstore),
                    pci_address(slot::PSTORE_DISK)
                ),
            );
        }
        if let Some(addons) = self.addons_on_block() {
            push(
                "--block",
                format!(
                    "path={},ro=true,lock=false,pci-address={}",
                    path_value(addons),
                    pci_address(slot::ADDON_DISK)
                ),
            );
        }
        match &self.network {
            NetworkBackend::None => {}
            NetworkBackend::Passt { socket } => push(
                "--vhost-user",
                format!(
                    "type=net,socket={},pci-address={}",
                    path_value(socket),
                    pci_address(slot::NETWORK)
                ),
            ),
            NetworkBackend::Tap { fd } => push(
                "--net",
                format!(
                    "tap-fd={fd},mac={},pci-address={}",
                    self.mac_address,
                    pci_address(slot::NETWORK)
                ),
            ),
        }
        push("--initrd", self.initrd.to_string_lossy().into_owned());
        push("--params", self.full_kernel_cmdline());
        args.push("--balloon-page-reporting".to_string());
        args.push(self.kernel.to_string_lossy().into_owned());
        Ok(args)
    }
}

/// A locally administered MAC address for the VM of user `uid`, stable across boots so the
/// guest keeps its DHCP lease and Android sees the same network.
pub fn mac_address(uid: u32) -> String {
    let b = uid.to_be_bytes();
    format!("52:54:{:02x}:{:02x}:{:02x}:{:02x}", b[0], b[1], b[2], b[3])
}

/// The vsock address of the VM of user `uid`. Each user has their own, so the host tells
/// their guests apart by the address a connection comes from; it stays clear of the low
/// addresses other VM managers hand out from 3 upward.
pub fn guest_cid(uid: u32) -> u32 {
    const BASE: u32 = 0x0044_5341; // "DSA"
    let cid = BASE.wrapping_add(uid);
    if cid <= 2 || cid == u32::MAX {
        BASE
    } else {
        cid
    }
}

/// The virtio-mem requested size, in bytes, that gives a VM that boots with `base_mb` `mb` MiB.
pub fn virtio_mem_bytes(base_mb: u64, mb: u64) -> u64 {
    mb.saturating_sub(base_mb) * 1024 * 1024
}

// Linux paths: "/a" is not absolute on Windows, where the workspace is also tested.
#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn config() -> VmConfig {
        VmConfig {
            name: "dsa-1000".into(),
            arch: GuestArch::X86_64,
            kernel: "/var/lib/dsa/images/1/bzImage".into(),
            initrd: "/home/u/.local/share/dsa/tmp/boot.initrd".into(),
            kernel_cmdline:
                "console=ttyS0 androidboot.boot_devices=vmbus androidboot.hardware=cutf_cvm".into(),
            memory_mb: 2048,
            memory_max_mb: None,
            processors: 4,
            system_image: Some("/var/lib/dsa/images/1/system.img".into()),
            data_disk: "/home/u/.local/share/dsa/data.img".into(),
            pstore_disk: Some("/home/u/.local/share/dsa/pstore.img".into()),
            addon_disk: None,
            guest_cid: guest_cid(1000),
            page_reporting_order: 9,
            control_socket: "/home/u/.local/share/dsa/tmp/crosvm.sock".into(),
            sandbox_root: Some("/home/u/.local/share/dsa/tmp/jail".into()),
            network: NetworkBackend::Passt {
                socket: "/home/u/.local/share/dsa/tmp/passt.sock".into(),
            },
            mac_address: mac_address(1000),
            direct_mapped: None,
        }
    }

    fn direct_mapped(addons: bool) -> DirectMapped {
        DirectMapped {
            support: dsa_host_core::direct_mapped::ImageSupport {
                fstab_suffix: "dsa_dax".into(),
                uncompressed_erofs: true,
            },
            addons,
        }
    }

    #[test]
    fn nfr1_a_direct_mapped_vm_maps_its_read_only_disks_with_virtio_pmem() {
        let mut config = config();
        config.addon_disk = Some("/home/u/.local/share/dsa/addons.img".into());
        config.kernel_cmdline =
            "console=ttyS0 androidboot.boot_devices=vmbus androidboot.fstab_suffix=dsa".into();
        config.direct_mapped = Some(direct_mapped(true));
        let args = config.crosvm_args().unwrap();
        assert_eq!(
            value_of(&args, "--pmem"),
            [
                "path=/var/lib/dsa/images/1/system.img,ro=true",
                "path=/home/u/.local/share/dsa/addons.img,ro=true"
            ]
        );
        // Only the user's disk and the pstore disk stay on virtio-blk.
        let blocks = value_of(&args, "--block");
        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert!(blocks[0].starts_with("path=/home/u/.local/share/dsa/data.img,"));
        assert_eq!(config.disk_slots(), [slot::DATA_DISK, slot::PSTORE_DISK]);
        // The guest boots the image's table for the mode, with the persistent memory's
        // platform device among the boot devices.
        let params = &value_of(&args, "--params")[0];
        assert!(
            params.contains(
                "androidboot.boot_devices=pci0000:00/0000:00:11.0,pci0000:00/0000:00:12.0,virtio-pmem"
            ),
            "{params}"
        );
        assert!(
            params.contains("androidboot.fstab_suffix=dsa_dax"),
            "{params}"
        );
        assert_eq!(params.matches("androidboot.fstab_suffix=").count(), 1);
        // An add-on disk not in the mode's layout stays a disk.
        config.direct_mapped = Some(direct_mapped(false));
        let args = config.crosvm_args().unwrap();
        assert_eq!(
            value_of(&args, "--pmem"),
            ["path=/var/lib/dsa/images/1/system.img,ro=true"]
        );
        assert!(value_of(&args, "--block")
            .iter()
            .any(|b| b.starts_with("path=/home/u/.local/share/dsa/addons.img,ro=true")));
    }

    #[test]
    fn nfr1_without_the_direct_mapped_mode_no_disk_is_persistent_memory() {
        let mut config = config();
        config.addon_disk = Some("/home/u/.local/share/dsa/addons.img".into());
        let args = config.crosvm_args().unwrap();
        assert!(value_of(&args, "--pmem").is_empty());
        assert!(!value_of(&args, "--params")[0].contains("virtio-pmem"));
    }

    fn value_of<'a>(args: &'a [String], option: &str) -> Vec<&'a str> {
        args.windows(2)
            .filter(|pair| pair[0] == option)
            .map(|pair| pair[1].as_str())
            .collect()
    }

    #[test]
    fn fr2_the_vm_boots_the_kernel_directly_under_crosvm() {
        let args = config().crosvm_args().unwrap();
        assert_eq!(&args[..2], ["--no-syslog", "run"]);
        assert_eq!(args.last().unwrap(), "/var/lib/dsa/images/1/bzImage");
        assert_eq!(
            value_of(&args, "--initrd"),
            ["/home/u/.local/share/dsa/tmp/boot.initrd"]
        );
        assert_eq!(value_of(&args, "--cpus"), ["4"]);
        assert_eq!(
            value_of(&args, "--socket"),
            ["/home/u/.local/share/dsa/tmp/crosvm.sock"]
        );
    }

    #[test]
    fn fr2_the_devices_run_in_crosvms_sandbox() {
        let args = config().crosvm_args().unwrap();
        assert_eq!(
            value_of(&args, "--pivot-root"),
            ["/home/u/.local/share/dsa/tmp/jail"]
        );
        assert!(!args.iter().any(|a| a == "--disable-sandbox"));
        let mut open = config();
        open.sandbox_root = None;
        let args = open.crosvm_args().unwrap();
        assert!(args.iter().any(|a| a == "--disable-sandbox"));
        assert!(value_of(&args, "--pivot-root").is_empty());
    }

    #[test]
    fn fr2_the_disks_are_found_at_the_pci_addresses_the_command_line_names() {
        let config = config();
        let cmdline = config.full_kernel_cmdline();
        assert!(!cmdline.contains("boot_devices=vmbus"));
        assert!(cmdline.contains(
            "androidboot.boot_devices=pci0000:00/0000:00:10.0,pci0000:00/0000:00:11.0,\
             pci0000:00/0000:00:12.0"
        ));
        let args = config.crosvm_args().unwrap();
        let blocks = value_of(&args, "--block");
        assert_eq!(
            blocks[0],
            "path=/var/lib/dsa/images/1/system.img,ro=true,lock=false,pci-address=00:10.0"
        );
        assert_eq!(
            blocks[1],
            "path=/home/u/.local/share/dsa/data.img,sparse=true,pci-address=00:11.0"
        );
        assert!(blocks[2].ends_with("pci-address=00:12.0"));
        assert_eq!(blocks.len(), 3, "no add-on disk without add-ons");
    }

    #[test]
    fn fr32_the_addon_disk_is_attached_read_only_where_the_guest_finds_it() {
        let mut config = config();
        config.addon_disk = Some("/home/u/.local/share/dsa/addons.img".into());
        let args = config.crosvm_args().unwrap();
        let blocks = value_of(&args, "--block");
        assert_eq!(
            blocks[3],
            "path=/home/u/.local/share/dsa/addons.img,ro=true,lock=false,pci-address=00:13.0"
        );
        assert!(config
            .full_kernel_cmdline()
            .contains("pci0000:00/0000:00:12.0,pci0000:00/0000:00:13.0"));
    }

    #[test]
    fn fr2_an_arm64_guest_finds_its_disks_below_crosvms_host_bridge() {
        let mut config = config();
        config.arch = GuestArch::Arm64;
        assert!(config
            .full_kernel_cmdline()
            .contains("androidboot.boot_devices=10000.pci"));
    }

    #[test]
    fn fr21_the_console_is_crosvms_virtio_console_with_an_early_serial_console() {
        let config = config();
        let args = config.crosvm_args().unwrap();
        assert_eq!(
            value_of(&args, "--serial"),
            [
                "type=stdout,hardware=serial,num=1,earlycon",
                "type=stdout,hardware=virtio-console,num=1,console,stdin"
            ]
        );
        // The image's own console (Hyper-V's COM1) gives way to the one crosvm names.
        assert!(!config.full_kernel_cmdline().contains("console=ttyS0"));
    }

    #[test]
    fn pr1_the_guest_has_a_vhost_vsock_device_at_the_users_address() {
        let args = config().crosvm_args().unwrap();
        let cid = guest_cid(1000);
        assert_eq!(value_of(&args, "--vsock"), [format!("cid={cid}").as_str()]);
        assert_ne!(guest_cid(1000), guest_cid(1001));
        assert!(guest_cid(u32::MAX) > 2);
    }

    #[test]
    fn nfr2_free_guest_pages_are_reported_back_to_the_host() {
        let config = config();
        let args = config.crosvm_args().unwrap();
        assert!(args.iter().any(|a| a == "--balloon-page-reporting"));
        assert!(config
            .full_kernel_cmdline()
            .contains("page_reporting.page_reporting_order=9"));
        // Nothing that pins or preallocates the guest's memory.
        let joined = args.join(" ");
        for pinned in ["--hugepages", "--lock-guest-memory", "--no-balloon"] {
            assert!(!joined.contains(pinned), "{pinned} pins guest memory");
        }
        assert_eq!(value_of(&args, "--mem"), ["size=2048"]);
        assert!(value_of(&args, "--init-mem").is_empty());
    }

    #[test]
    fn nfr2_the_linux_vm_boots_at_its_base_size_with_the_rest_as_virtio_mem() {
        let mut config = config();
        config.memory_max_mb = Some(16384);
        let args = config.crosvm_args().unwrap();
        // The guest kernel sees only the base at boot, so it keeps page structs for 2 GiB, not
        // for 16 GiB; the rest is plugged in as it is needed.
        assert_eq!(value_of(&args, "--mem"), ["size=2048"]);
        assert_eq!(value_of(&args, "--virtio-mem"), ["14336"]);
        assert!(value_of(&args, "--init-mem").is_empty());
        assert!(config
            .full_kernel_cmdline()
            .contains("memory_hotplug.memmap_on_memory=1"));
        assert!(config
            .full_kernel_cmdline()
            .contains("memory_hotplug.online_policy=auto-movable"));
        // Whole memory blocks only.
        config.memory_max_mb = Some(2048 + 1000);
        assert_eq!(config.memory_layout(), (2048, Some(896)));
        config.memory_max_mb = Some(2048 + 100);
        assert_eq!(config.memory_layout(), (2048, None));
        config.memory_max_mb = Some(1024);
        assert_eq!(config.memory_layout(), (2048, None));
        assert_eq!(virtio_mem_bytes(2048, 2048), 0);
        assert_eq!(virtio_mem_bytes(2048, 16384), 14336 * 1024 * 1024);
        assert_eq!(virtio_mem_bytes(2048, 1024), 0);
    }

    #[test]
    fn nfr1_the_guest_kernel_sets_aside_as_little_at_boot_as_on_windows() {
        let cmdline = config().full_kernel_cmdline();
        let words: Vec<&str> = cmdline.split_whitespace().collect();
        for word in [
            "swiotlb=2048",
            "kfence.sample_interval=0",
            "dhash_entries=65536",
            "ihash_entries=32768",
            "thash_entries=16384",
            "uhash_entries=1024",
        ] {
            assert!(words.contains(&word), "{word} in {cmdline}");
        }
    }

    #[test]
    fn fr28_the_vm_has_a_network_adapter_on_the_nat_it_was_given() {
        let args = config().crosvm_args().unwrap();
        assert_eq!(
            value_of(&args, "--vhost-user"),
            ["type=net,socket=/home/u/.local/share/dsa/tmp/passt.sock,pci-address=00:03.0"]
        );
        let mut tap = config();
        tap.network = NetworkBackend::Tap { fd: 3 };
        assert_eq!(
            value_of(&tap.crosvm_args().unwrap(), "--net"),
            ["tap-fd=3,mac=52:54:00:00:03:e8,pci-address=00:03.0"]
        );
        let mut none = config();
        none.network = NetworkBackend::None;
        let args = none.crosvm_args().unwrap();
        assert!(value_of(&args, "--net").is_empty());
        assert!(value_of(&args, "--vhost-user").is_empty());
    }

    #[test]
    fn fr2_a_stop_can_ask_the_guest_to_sync_and_power_off() {
        assert!(config()
            .full_kernel_cmdline()
            .contains("sysrq_always_enabled=1"));
        assert_eq!(SHUTDOWN_SEQUENCE, [b"\x0fs", b"\x0fu", b"\x0fo"]);
    }

    #[test]
    fn pr1_the_kernel_reads_the_bootconfig_that_carries_the_boot_secret() {
        let cmdline = config().full_kernel_cmdline();
        let words: Vec<&str> = cmdline.split_whitespace().collect();
        assert_eq!(
            words.iter().filter(|w| **w == "bootconfig").count(),
            1,
            "{cmdline}"
        );
        let mut config = config();
        config.kernel_cmdline = format!("{} bootconfig", config.kernel_cmdline);
        let cmdline = config.full_kernel_cmdline();
        assert_eq!(
            cmdline
                .split_whitespace()
                .filter(|w| *w == "bootconfig")
                .count(),
            1
        );
    }

    #[test]
    fn fr2_a_path_with_a_comma_stays_one_option_value() {
        let mut config = config();
        config.data_disk = "/home/a,b/data.img".into();
        let args = config.crosvm_args().unwrap();
        assert!(value_of(&args, "--block")[1].starts_with("path=\"/home/a,b/data.img\","));
        assert_eq!(escape("a\"b c"), "\"a\\\"b c\"");
    }

    #[test]
    fn fr2_a_relative_path_is_refused() {
        let mut config = config();
        config.kernel = "bzImage".into();
        assert!(config.crosvm_args().is_err());
    }
}
