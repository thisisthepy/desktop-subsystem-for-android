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
//! - [`compositor`]: platform-neutral graphics stream bookkeeping, renderer interfaces, and
//!   task registry.

pub mod bootconfig;
pub mod compositor;
pub mod handshake;
pub mod host_api;
#[cfg(feature = "conformance")]
pub mod host_api_conformance;
pub mod session;
pub mod update;
