//! The Windows host.
//!
//! - [`vm`]: the Android VM's configuration, as the HCS document that creates it.
//! - [`hcs`] and [`android_vm`]: creating, starting and stopping that VM (Windows only).
//! - [`hvsocket`]: the control channel's transport, Hyper-V sockets (Windows only).
//! - [`https`]: downloads for image updates, through WinHTTP (Windows only).
//! - [`pipe`]: the local API over a named pipe (Windows only).
//! - [`gpt`]: finding and copying the partitions of a user's Android disk.
//! - [`guid`]: the VM and service IDs those sockets are addressed by.
//! - [`vm_listener`]: a control channel listener that follows the VM's ID across restarts.
//! - [`host_config`]: the configuration file `dsa-hostd` reads.
//! - [`service`]: the local API and one VM per user behind it, as `dsa-hostd` serves them
//!   (Windows only).
//! - [`profile`]: where a user's local application data is, from their SID (Windows only).
//! - [`scm`]: running under the Service Control Manager (Windows only).
//! - [`uninstall`]: what uninstalling needs from `dsa-hostd`.
//! - [`user_disk`]: each user's Android disk (Windows only).
//! - [`account`]: the Windows account the host runs as (Windows only).
//! - [`wsl`]: a launcher for a container in WSL, kept for a later second backend.

/// The DSA release this program belongs to. The installer build sets `DSA_VERSION` to the
/// release's version; a plain `cargo build` falls back to the crate's version.
pub const VERSION: &str = match option_env!("DSA_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

#[cfg(windows)]
pub mod account;
#[cfg(windows)]
pub mod android_vm;
pub mod gpt;
pub mod guid;
#[cfg(windows)]
pub mod hcs;
pub mod host_config;
#[cfg(windows)]
pub mod https;
#[cfg(windows)]
pub mod hvsocket;
#[cfg(windows)]
pub mod pipe;
#[cfg(windows)]
pub mod profile;
#[cfg(windows)]
pub mod scm;
#[cfg(windows)]
pub mod service;
pub mod uninstall;
#[cfg(windows)]
pub mod user_disk;
pub mod vm;
pub mod vm_listener;
pub mod wsl;
