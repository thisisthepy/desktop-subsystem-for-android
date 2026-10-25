//! The control protocol in Rust: the generated message types, the frame codec and the
//! connection handshake. [`host_api`] holds the same for the local API.
//!
//! The message types come from `dsa/protocol/control.proto` through
//! `dsa-protocol-codegen`; nothing in [`control`] is written by hand. Every frame on the
//! control channel is one [`control::Envelope`].

use std::fmt;

/// The generated message types.
#[allow(clippy::all)]
pub mod control {
    include!("generated/dsa.control.v1.rs");
}

#[allow(clippy::all)]
mod schema {
    include!("generated/schema.rs");
}

pub mod auth;
pub mod handshake;
pub mod host_api;

pub use schema::{SCHEMA_HASH, SCHEMA_HASH_HEX};

use control::Envelope;
use prost::Message;

/// The protocol version of this build, from the definition's `ProtocolVersion` enum.
pub const PROTOCOL_VERSION: u32 = control::ProtocolVersion::Current as u32;

/// A frame that is not a valid `Envelope`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "not a valid control message: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Encodes one envelope as one frame.
pub fn encode(envelope: &Envelope) -> Vec<u8> {
    envelope.encode_to_vec()
}

/// Decodes one frame. Malformed input is an error, never a panic.
pub fn decode(frame: &[u8]) -> Result<Envelope, DecodeError> {
    Envelope::decode(frame).map_err(|error| DecodeError(error.to_string()))
}

/// Lower-case hexadecimal of a hash, for error messages.
pub(crate) fn handshake_hex(hash: &[u8]) -> String {
    hash.iter().map(|b| format!("{b:02x}")).collect()
}
