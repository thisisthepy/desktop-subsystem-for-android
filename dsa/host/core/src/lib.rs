//! The platform-neutral part of the host.
//!
//! Nothing here names a host operating system API. The platform crates under
//! `dsa/host/platform/` supply the pieces that do (the guest launcher and the transport)
//! through the traits defined here and in `dsa-transport`.
//!
//! - [`session`]: one guest's life from start to stop.
//! - [`bootconfig`]: the kernel bootconfig that carries each boot's secret to the guest.
//! - [`handshake`]: the control channel's handshake as a session uses it.
//! - [`host_api`]: the local API service, which acts on each caller's own Android, and its
//!   client.
//! - [`tasks`]: the guest's app tasks, which the session keeps and hands on.
//! - [`input`]: pointer input in a task's window, as the guest task receives it.
//! - [`keys`]: key input in a task's window.
//! - [`power`]: the host computer's power, which each guest's battery state follows.
//! - [`ime`]: text input through the host's input method in a task's window.
//! - [`settings`]: the settings a user changes from the settings app, kept by the service.
//! - [`memory`]: how much memory a user's Android takes, in the guest and on the host.
//! - [`compositor`]: platform-neutral graphics stream bookkeeping, renderer interfaces, and
//!   which window each task and each frame goes to.

pub mod bootconfig;
pub mod compositor;
pub mod handshake;
pub mod host_api;
#[cfg(feature = "conformance")]
pub mod host_api_conformance;
pub mod ime;
pub mod input;
pub mod keys;
pub mod memory;
pub mod power;
pub mod resize_trace;
pub mod session;
pub mod settings;
pub mod tasks;
pub mod update;
