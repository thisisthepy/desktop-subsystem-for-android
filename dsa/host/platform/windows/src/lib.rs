//! The Windows host.
//!
//! - [`wsl`]: starts and stops the guest container inside WSL.
//! - [`hvsocket`]: the control channel's transport, Hyper-V sockets (Windows only).
//! - [`guid`]: the VM and service IDs those sockets are addressed by.

pub mod guid;
#[cfg(windows)]
pub mod hvsocket;
pub mod wsl;
