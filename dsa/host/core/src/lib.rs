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
//! - [`compositor`]: platform-neutral graphics stream bookkeeping, renderer interfaces, and
//!   which window each task and each frame goes to.

pub mod bootconfig;
pub mod compositor;
pub mod handshake;
pub mod host_api;
#[cfg(feature = "conformance")]
pub mod host_api_conformance;
pub mod input;
pub mod session;
pub mod tasks;
pub mod update;
