//! The local API in Rust: the generated message types, the frame codec and the
//! connection handshake.
//!
//! The types come from `dsa/protocol/host_api.proto` through `dsa-protocol-codegen`. The
//! handshake follows the control channel's: the client sends a `Hello` with its protocol
//! version and schema hash, and the service answers `HelloAccepted` or `HelloRejected`
//! and refuses a client generated from a different definition.

use std::fmt;
use std::time::Duration;

use dsa_transport::{Connection, TransportError};
use prost::Message;

/// The generated message types.
#[allow(clippy::all)]
pub mod types {
    include!("generated/dsa.hostapi.v1.rs");
}

#[allow(clippy::all)]
mod schema {
    include!("generated/host_api_schema.rs");
}

pub use schema::{SCHEMA_HASH, SCHEMA_HASH_HEX};

use types::{envelope::Body, Envelope, Hello, HelloAccepted, HelloRejected, RejectReason, Role};

/// The protocol version of this build, from the definition's `ProtocolVersion` enum.
pub const PROTOCOL_VERSION: u32 = types::ProtocolVersion::Current as u32;

/// Encodes one envelope as one frame.
pub fn encode(envelope: &Envelope) -> Vec<u8> {
    envelope.encode_to_vec()
}

/// Decodes one frame. Malformed input is an error, never a panic.
pub fn decode(frame: &[u8]) -> Result<Envelope, crate::DecodeError> {
    Envelope::decode(frame).map_err(|error| crate::DecodeError(error.to_string()))
}

/// Wraps `body` in an envelope and sends it as one frame.
pub fn send(connection: &mut dyn Connection, body: Body) -> Result<(), TransportError> {
    connection.send(&encode(&Envelope { body: Some(body) }))
}

/// This build's `Hello` for `role`.
pub fn local_hello(role: Role, build: &str) -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        schema_hash: SCHEMA_HASH.to_vec(),
        role: role as i32,
        build: build.to_string(),
    }
}

/// Why a handshake did not complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandshakeError {
    /// The connection failed or timed out.
    Transport(TransportError),
    /// The peer sent something that is not a usable handshake message.
    Malformed(String),
    /// The two sides do not match. On the service this is the refusal it sent; on the
    /// client it is the refusal it received.
    Rejected {
        reason: RejectReason,
        detail: String,
    },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandshakeError::Transport(error) => write!(f, "local API: {error}"),
            HandshakeError::Malformed(why) => write!(f, "invalid local API handshake: {why}"),
            HandshakeError::Rejected { detail, .. } => {
                write!(f, "the DSA service refused the connection: {detail}")
            }
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<TransportError> for HandshakeError {
    fn from(error: TransportError) -> Self {
        HandshakeError::Transport(error)
    }
}

fn short(hash: &[u8]) -> String {
    crate::handshake_hex(hash).chars().take(12).collect()
}

/// Compares a client's `Hello` with the service's. Returns the refusal to send when they
/// differ.
pub fn check(peer: &Hello, local: &Hello) -> Result<(), (RejectReason, String)> {
    if peer.protocol_version != local.protocol_version {
        return Err((
            RejectReason::ProtocolVersionMismatch,
            format!(
                "the client ({}) speaks local API version {}, and the service ({}) speaks \
                 version {}; update the one that is older",
                peer.build, peer.protocol_version, local.build, local.protocol_version
            ),
        ));
    }
    if peer.schema_hash.len() != SCHEMA_HASH.len() {
        return Err((
            RejectReason::MalformedHello,
            format!(
                "the client ({}) sent a schema hash of {} bytes; it must be {} bytes",
                peer.build,
                peer.schema_hash.len(),
                SCHEMA_HASH.len()
            ),
        ));
    }
    if peer.schema_hash != local.schema_hash {
        return Err((
            RejectReason::SchemaHashMismatch,
            format!(
                "the client ({}) was built from a different local API definition \
                 (schema hash {}, expected {}); install the client and the service from \
                 the same release",
                peer.build,
                short(&peer.schema_hash),
                short(&local.schema_hash)
            ),
        ));
    }
    if peer.role != Role::Client as i32 {
        return Err((
            RejectReason::UnexpectedRole,
            format!(
                "the peer ({}) did not say it is a client (role {})",
                peer.build, peer.role
            ),
        ));
    }
    Ok(())
}

fn refuse(
    connection: &mut dyn Connection,
    local: &Hello,
    reason: RejectReason,
    detail: String,
) -> HandshakeError {
    let rejected = HelloRejected {
        reason: reason as i32,
        detail: detail.clone(),
        service: Some(local.clone()),
    };
    // The refusal is a courtesy to the client; the connection closes either way.
    let _ = send(connection, Body::HelloRejected(rejected));
    connection.close();
    HandshakeError::Rejected { reason, detail }
}

/// The service side: waits at most `timeout` for the client's `Hello`, checks it and
/// answers. Returns the client's `Hello` when accepted.
pub fn service_accept(
    connection: &mut dyn Connection,
    local: &Hello,
    timeout: Duration,
) -> Result<Hello, HandshakeError> {
    let frame = connection.recv(Some(timeout))?;
    let peer = match decode(&frame) {
        Ok(Envelope {
            body: Some(Body::Hello(hello)),
        }) => hello,
        Ok(_) => {
            let detail = "the first message on the connection was not a Hello".to_string();
            return Err(refuse(
                connection,
                local,
                RejectReason::MalformedHello,
                detail,
            ));
        }
        Err(error) => {
            let detail = error.to_string();
            return Err(refuse(
                connection,
                local,
                RejectReason::MalformedHello,
                detail,
            ));
        }
    };
    if let Err((reason, detail)) = check(&peer, local) {
        return Err(refuse(connection, local, reason, detail));
    }
    send(
        connection,
        Body::HelloAccepted(HelloAccepted {
            service: Some(local.clone()),
        }),
    )?;
    Ok(peer)
}

/// The client side: sends `local` and waits at most `timeout` for the answer. Returns the
/// service's `Hello` when accepted.
pub fn client_connect(
    connection: &mut dyn Connection,
    local: &Hello,
    timeout: Duration,
) -> Result<Hello, HandshakeError> {
    send(connection, Body::Hello(local.clone()))?;
    let frame = connection.recv(Some(timeout))?;
    match decode(&frame) {
        Ok(Envelope {
            body: Some(Body::HelloAccepted(accepted)),
        }) => accepted.service.ok_or_else(|| {
            HandshakeError::Malformed("HelloAccepted without the service's Hello".into())
        }),
        Ok(Envelope {
            body: Some(Body::HelloRejected(rejected)),
        }) => Err(HandshakeError::Rejected {
            reason: RejectReason::try_from(rejected.reason).unwrap_or(RejectReason::Unspecified),
            detail: rejected.detail,
        }),
        Ok(_) => Err(HandshakeError::Malformed(
            "the service answered with something other than HelloAccepted or HelloRejected".into(),
        )),
        Err(error) => Err(HandshakeError::Malformed(error.to_string())),
    }
}
