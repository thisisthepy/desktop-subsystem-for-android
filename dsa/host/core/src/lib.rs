//! The platform-neutral part of the host.
//!
//! Nothing here names a host operating system API. The platform crates under
//! `dsa/host/platform/` supply the pieces that do (the guest launcher and the transport)
//! through the traits defined here and in `dsa-transport`.

pub mod handshake;
pub mod session;
pub mod update;
