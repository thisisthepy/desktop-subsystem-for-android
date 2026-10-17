//! The guest side of the control channel.
//!
//! In the guest the control channel is a vsock connection to the host. The agent reaches
//! this through its native layer; the transport traits and the framing are the shared ones
//! from `dsa-transport`, so both ends agree on what a frame is.

#[cfg(target_os = "linux")]
pub mod vsock;
