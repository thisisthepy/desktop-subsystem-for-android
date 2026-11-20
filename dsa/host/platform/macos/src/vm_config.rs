//! What the macOS host's VM is made of, before the framework sees it.
//!
//! The Virtualization framework's own objects are built from this in `vz`, on macOS only;
//! this module decides everything that can be decided anywhere, so it is tested on every
//! build host:
//! - the kernel the image ships for the Mac's architecture: the uncompressed `Image` on
//!   Apple silicon (the framework's Linux boot loader takes no compressed kernel there), the
//!   `bzImage` on Intel Macs;
//! - the disks, in the order the guest names them: the image's system disk read only
//!   (`vda`), the user's disk (`vdb`), the pstore disk (`vdc`), and the user's add-on disk read
//!   only (`vdd`) while they have add-ons. Android finds its partitions by their GPT names, not
//!   by these names;
//! - the vsock ports the host listens on and the memory the balloon leaves the guest.

use std::path::{Path, PathBuf};

pub use dsa_host_core::image_files::GuestArch;
use dsa_host_core::image_files::{ImageFiles, ImageRulesOwned};

/// Whether `kernel` starts like a kernel the framework boots on `arch`: an arm64 `Image`
/// (its magic `ARM\x64` at offset 56; a compressed kernel has none) or an x86 `bzImage`
/// (`HdrS` at offset 0x202).
pub fn kernel_fits(kernel: &[u8], arch: GuestArch) -> bool {
    match arch {
        GuestArch::Arm64 => kernel.get(56..60) == Some(&b"ARM\x64"[..]),
        GuestArch::X86_64 => kernel.get(0x202..0x206) == Some(&b"HdrS"[..]),
    }
}

/// One disk of the VM.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub path: PathBuf,
    pub read_only: bool,
}

/// Everything the framework's VM is made from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacVmConfig {
    pub kernel: PathBuf,
    /// The boot's initrd: the image's, with the boot's secret appended as bootconfig.
    pub initrd: PathBuf,
    pub cmdline: String,
    /// The memory the VM is given, in MiB: as far as it may grow.
    pub memory_max_mb: u64,
    /// The memory the guest starts with, in MiB; the balloon holds the rest back.
    pub memory_start_mb: u64,
    pub processors: u32,
    /// In the order the guest sees them.
    pub disks: Vec<Disk>,
    /// Where the guest's console is written. Without it the VM has no console.
    pub console_log: Option<PathBuf>,
    /// Whether the VM has a network adapter, behind the Mac's NAT.
    pub network: bool,
    /// The adapter's MAC address (`aa:bb:cc:dd:ee:ff`), kept per user so Android sees the
    /// same network each boot. `None` lets the framework pick one.
    pub mac_address: Option<String>,
    /// The vsock ports the guest connects to on the host.
    pub listen_ports: Vec<u32>,
}

/// The least memory Android runs in.
pub const MIN_MEMORY_MB: u64 = 512;

impl MacVmConfig {
    /// Checks what the framework would refuse less clearly.
    pub fn validate(&self) -> Result<(), String> {
        let mut paths: Vec<(&str, &Path)> =
            vec![("kernel", &self.kernel), ("initrd", &self.initrd)];
        for disk in &self.disks {
            paths.push(("disk", &disk.path));
        }
        if let Some(log) = &self.console_log {
            paths.push(("console log", log));
        }
        for (what, path) in paths {
            if !path.is_absolute() {
                return Err(format!(
                    "the {what} path {} must be absolute",
                    path.display()
                ));
            }
        }
        if self.memory_max_mb < MIN_MEMORY_MB {
            return Err(format!(
                "{} MiB of memory is too little for Android (at least {MIN_MEMORY_MB})",
                self.memory_max_mb
            ));
        }
        if self.memory_start_mb < MIN_MEMORY_MB || self.memory_start_mb > self.memory_max_mb {
            return Err(format!(
                "the VM cannot start with {} MiB: it must be between {MIN_MEMORY_MB} and {} MiB",
                self.memory_start_mb, self.memory_max_mb
            ));
        }
        if self.processors == 0 {
            return Err("the VM needs at least one processor".into());
        }
        if self.disks.is_empty() {
            return Err("the VM has no disk to boot Android from".into());
        }
        if let Some(mac) = &self.mac_address {
            if !is_mac_address(mac) {
                return Err(format!("{mac:?} is not a MAC address (aa:bb:cc:dd:ee:ff)"));
            }
        }
        Ok(())
    }
}

/// Whether `text` is six hexadecimal pairs separated by colons.
pub fn is_mac_address(text: &str) -> bool {
    let parts: Vec<&str> = text.split(':').collect();
    parts.len() == 6
        && parts
            .iter()
            .all(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A locally administered unicast MAC address made from `seed` (the user's ID), so each
/// user's Android keeps one address.
pub fn mac_address_for(seed: &str) -> String {
    // FNV-1a: stable across builds and platforms, unlike the standard library's hasher.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in seed.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let bytes = hash.to_le_bytes();
    let first = (bytes[0] & 0xfc) | 0x02;
    format!(
        "{first:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]
    )
}

/// The rules this host reads an image by for a guest of `arch`: the kernel its framework
/// boots, and the raw disks a folder has, else the VHDX disks, which the host converts.
pub fn image_rules(arch: GuestArch) -> ImageRulesOwned {
    ImageRulesOwned {
        kernel_names: vec![arch.kernel_name().into()],
        disk_extensions: vec!["img".into(), "vhdx".into()],
        arch: Some(arch),
        fallback_kernel: None,
        extra_cmdline: String::new(),
    }
}

/// Refuses an image of `files`, read from `dir`, that is not of the published form: the
/// framework boots an image with an `initrd.img` and a disk to make the user's from.
pub fn check_published(dir: &Path, files: &ImageFiles) -> Result<(), String> {
    if files.initrd.is_none() {
        return Err(format!("the image in {} has no initrd.img", dir.display()));
    }
    if files.disk_template.is_none() {
        return Err(format!(
            "the image in {} has no disk to make the user's from",
            dir.display()
        ));
    }
    Ok(())
}

/// Reads the image in `dir` for a guest of `arch`. Raw disks (`system.img`, `data.img`,
/// `disk.img`) are used when the folder has them; otherwise its VHDX disks, which the host
/// converts. The framework boots an image of the published form only, so one without an
/// `initrd.img` or a disk to make the user's from is refused.
pub fn image_files(dir: &Path, arch: GuestArch) -> Result<ImageFiles, String> {
    let files = image_rules(arch).read(dir)?;
    check_published(dir, &files)?;
    Ok(files)
}
