//! The macOS host: the Android VM on Apple's Virtualization framework.
//!
//! On macOS no privilege is needed to run a VM: a process signed with the
//! `com.apple.security.virtualization` entitlement may create one. So the host is a per-user
//! launchd agent, `dsa-hostd`, which runs as the user, owns that user's VM, and serves the
//! local API on a Unix socket in the user's own folder (`dsa-host-unix`). There is no system
//! service between the user and their Android.
//!
//! The VM boots the same image as the Windows host: the kernel (`Image` on Apple silicon,
//! `bzImage` on Intel Macs) with the image's initrd and a bootconfig carrying each boot's
//! secret, the image's system disk read only and the user's disk writable. The image ships
//! its disks as VHDX; the framework reads raw images, so the host converts them once
//! ([`vhdx`]) and keeps the result.
//!
//! The desktop side:
//!
//! - [`compositor`]: one `NSWindow` per Android task, each showing the task's frames through
//!   a `CAMetalLayer`, resized without stretching, with full screen and picture in picture.
//! - [`input`]: the mouse, the trackpad, the keyboard and the input method (through
//!   `NSTextInputClient`) of a task window, turned into what the guest receives.
//! - [`shell`]: the clipboard, notifications, Dock and Launchpad entries for Android apps,
//!   drag and drop, and opening links and files on the host.
//! - [`devices`]: sound, video decoding, cameras, location, printing, web links, shared
//!   folders, game controllers, and the system settings each user's Android follows, lent by
//!   the user's device lender (`dsa-devices`).
//! - [`power`]: the Mac's battery, Low Power Mode, and its sleep and wake, which each user's
//!   Android follows.
//! - [`wm`]: the window manager (`dsa-wm`): the renderer, the guest's graphics streams, and
//!   one window host per app, each given its tasks' frames.
//! - [`root_approval`]: the administrator dialog an Android app's root request waits for,
//!   through Authorization Services.
//! - [`keychain`], [`secure_enclave`] and `host_services`: the app data backup in `~/DSA Backup`,
//!   the passkey check and KeyMint's root key: the backup key in the user's login keychain, the
//!   KeyMint root key sealed by the Secure Enclave first.
//! - `host_certificates`: the root certificates the administrator's and the user's trust
//!   settings added, which the user's Android trusts beside its own.
//!
//! What can be checked anywhere (the kernel command line, the disk formats, the
//! configuration, the launchd agent, key tables, sizes, bundle layouts, value conversions) is
//! platform neutral and tested on every build host. The parts that call the frameworks are
//! compiled on macOS only.

pub mod cmdline;
pub mod compositor;
pub mod computer_identity;
pub mod devices;
pub mod display_power;
pub mod host_config;
pub mod input;
pub mod keychain;
pub mod launchd;
pub mod power;
pub mod root_approval;
pub mod secure_enclave;
pub mod shell;
pub mod user_disk;
/// The image's VHDX disks read into raw images, shared with the Linux host.
pub use dsa_host_unix::vhdx;
pub mod vm_config;
pub mod wm;

/// GUID partition tables, shared with the Windows host: the same disks, read the same way.
#[allow(dead_code)]
#[path = "../../windows/src/gpt.rs"]
pub(crate) mod gpt;

/// The pstore disk, shared with the Windows host. macOS attaches its disk part, without the
/// VHD footer.
#[allow(dead_code)]
#[path = "../../windows/src/pstore_disk.rs"]
pub(crate) mod pstore_disk;

#[cfg(target_os = "macos")]
pub mod adb_discovery;
#[cfg(target_os = "macos")]
pub mod android_vm;
#[cfg(target_os = "macos")]
pub mod fetch;
#[cfg(target_os = "macos")]
pub mod gfx;
#[cfg(target_os = "macos")]
pub mod host_certificates;
#[cfg(target_os = "macos")]
pub mod host_memory;
#[cfg(target_os = "macos")]
pub mod host_services;
#[cfg(target_os = "macos")]
pub mod objc;
#[cfg(target_os = "macos")]
pub mod security;
#[cfg(target_os = "macos")]
pub mod service;
#[cfg(target_os = "macos")]
pub mod vsock;
#[cfg(target_os = "macos")]
pub mod vz;

/// This program's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
