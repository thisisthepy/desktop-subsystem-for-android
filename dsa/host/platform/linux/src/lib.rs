//! The Linux host: the Android VM over KVM and vsock, the host's devices lent to the guest,
//! and the display, input and desktop shell.
//!
//! [`compositor`] shows each Android task in a Wayland toplevel of its own, with the task's
//! frames from gfxstream's host renderer, which runs in a process of its own (`renderer`,
//! `dsa-gfx`), and runs the window rules the Windows host shares
//! (`dsa_host_core::compositor`). [`input`] turns the seat's keyboard, pointer, touch, pen,
//! input method and game controllers into the guest's input. [`shell`] gives the desktop
//! what it shows of the apps: launcher entries and icons, notifications, the clipboard and
//! dropped files.
//!
//! [`accessibility`] exports each task window's Android UI as an AT-SPI tree, with the title
//! bar's buttons, for screen readers and other accessibility clients.
//!
//! The VM and the service:
//! - [`crosvm`]: the VM as a crosvm command line (KVM, virtio disks, vhost-vsock, a balloon
//!   with free page reporting, virtio-mem for growth, the network adapter, the device
//!   sandbox);
//! - [`launcher`]: starts and stops each user's VM as that user, and resizes, freezes and stops
//!   it through crosvm's control socket ([`crosvm_control`]);
//! - [`vsock`]: the host's end of the guest's control, log and file ports, each connection
//!   routed to the session of the VM it comes from;
//! - [`network`]: NAT through passt, or the host's LAN through a macvtap;
//! - [`image`]: what a VM boots from an image folder, and each user's disk;
//! - [`service`]: the system service behind `/run/dsa/host.sock`, one session per user
//!   (`dsa-linux-hostd`);
//! - [`root_approval`]: the administrator prompt an Android app's root request waits for,
//!   through polkit in the user's desktop session;
//! - [`secret_service`]: each user's backup key, kept in their Secret Service through a
//!   helper that runs as them, and their KeyMint root key where no TPM 2.0 works;
//! - [`tpm2`]: each user's KeyMint root key, sealed by the computer's TPM 2.0 through the
//!   kernel's resource manager;
//! - `host_services`: the app data backup in `~/DSA Backup`, the passkey check and the
//!   KeyMint root key each user's session serves, and [`host_certificates`], the root
//!   certificates an administrator added to the system's store, which Android trusts too.
//!
//! The rules are plain functions and types, tested on every host. The parts that talk to
//! the window system, the buses, the devices and the VM build on Linux only, so the workspace
//! builds everywhere.

pub mod accessibility;
#[cfg(target_os = "linux")]
pub mod adb_discovery;
pub mod compositor;
pub mod computer_identity;
pub mod config;
pub mod console;
pub mod crosvm;
#[cfg(target_os = "linux")]
pub mod crosvm_control;
#[cfg(target_os = "linux")]
pub mod devices;
pub mod fetch;
pub mod host_certificates;
#[cfg(target_os = "linux")]
pub mod host_services;
pub mod image;
pub mod input;
#[cfg(target_os = "linux")]
pub mod launcher;
#[cfg(target_os = "linux")]
pub mod memory;
pub mod monitor_density;
pub mod monitor_refresh;
pub mod network;
pub mod package_updates;
#[cfg(target_os = "linux")]
pub mod performance;
pub mod pstore;
#[cfg(target_os = "linux")]
pub mod renderer;
pub mod root_approval;
pub mod secret_service;
#[cfg(target_os = "linux")]
pub mod service;
pub mod shell;
pub mod signs;
pub mod tpm2;
#[cfg(target_os = "linux")]
pub mod users;
#[cfg(target_os = "linux")]
pub mod vsock;
#[cfg(target_os = "linux")]
pub mod window_manager;

/// This build's version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
