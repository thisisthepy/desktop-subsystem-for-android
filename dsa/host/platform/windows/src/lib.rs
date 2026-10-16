//! The Windows host.
//!
//! - [`vm`]: the Android VM's configuration, as the HCS document that creates it.
//! - [`hcs`] and [`android_vm`]: creating, starting and stopping that VM (Windows only).
//! - [`hvsocket`]: the control channel's transport, Hyper-V sockets (Windows only).
//! - [`guid`]: the VM and service IDs those sockets are addressed by.
//! - [`wsl`]: a launcher for a container in WSL, kept for a later second backend.

#[cfg(windows)]
pub mod android_vm;
pub mod guid;
#[cfg(windows)]
pub mod hcs;
#[cfg(windows)]
pub mod hvsocket;
pub mod vm;
pub mod wsl;
