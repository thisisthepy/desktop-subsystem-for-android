//! The first exchange on every control channel connection.
//!
//! The guest sends a `Hello` carrying its protocol version and the schema hash of the
//! definition it was generated from. The host compares both with its own and answers
//! `HelloAccepted` or `HelloRejected`, and after a rejection closes the connection. Nothing
//! else is sent until the host has accepted, so two builds generated from different
//! definitions never exchange messages they would read differently.

use std::fmt;
use std::time::Duration;

use dsa_transport::{Connection, TransportError};

use crate::control::{
    envelope::Body, Envelope, Hello, HelloAccepted, HelloRejected, RejectReason, Role,
};
use crate::{decode, encode, PROTOCOL_VERSION, SCHEMA_HASH};

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
    /// The peer's `Hello` does not match this build. On the host this is the refusal it
    /// sent; on the guest it is the refusal it received.
    Rejected {
        reason: RejectReason,
        detail: String,
    },
}

impl fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HandshakeError::Transport(error) => write!(f, "control channel: {error}"),
            HandshakeError::Malformed(why) => write!(f, "invalid handshake: {why}"),
            HandshakeError::Rejected { detail, .. } => write!(f, "connection refused: {detail}"),
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
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    hex.chars().take(12).collect()
}

fn role_name(role: i32) -> String {
    match Role::try_from(role) {
        Ok(Role::Host) => "the host".into(),
        Ok(Role::Guest) => "the guest".into(),
        Ok(Role::Unspecified) => "no role".into(),
        Err(_) => format!("an unknown role ({role})"),
    }
}

/// Compares a peer's `Hello` with `local`. Returns the refusal to send when they differ.
pub fn check(
    peer: &Hello,
    local: &Hello,
    expected_role: Role,
) -> Result<(), (RejectReason, String)> {
    if peer.protocol_version != local.protocol_version {
        return Err((
            RejectReason::ProtocolVersionMismatch,
            format!(
                "the peer ({}) speaks protocol version {}, and this side ({}) speaks version {}",
                peer.build, peer.protocol_version, local.build, local.protocol_version
            ),
        ));
    }
    if peer.schema_hash.len() != SCHEMA_HASH.len() {
        return Err((
            RejectReason::MalformedHello,
            format!(
                "the peer ({}) sent a schema hash of {} bytes; it must be {} bytes",
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
                "the peer ({}) was built from a different protocol definition \
                 (schema hash {}, expected {}); rebuild both sides from the same control.proto",
                peer.build,
                short(&peer.schema_hash),
                short(&local.schema_hash)
            ),
        ));
    }
    if peer.role != expected_role as i32 {
        return Err((
            RejectReason::UnexpectedRole,
            format!(
                "the peer ({}) says it is {}, but {} was expected",
                peer.build,
                role_name(peer.role),
                role_name(expected_role as i32)
            ),
        ));
    }
    Ok(())
}

fn send(connection: &mut dyn Connection, body: Body) -> Result<(), HandshakeError> {
    let envelope = Envelope { body: Some(body) };
    connection
        .send(&encode(&envelope))
        .map_err(HandshakeError::from)
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
        host: Some(local.clone()),
    };
    // The refusal is a courtesy to the peer; the connection closes either way.
    let _ = send(connection, Body::HelloRejected(rejected));
    connection.close();
    HandshakeError::Rejected { reason, detail }
}

/// The host side: waits at most `timeout` for the guest's `Hello`, checks it against
/// `local`, and answers. Returns the guest's `Hello` when accepted.
pub fn host_accept(
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
            return Err(refuse(
                connection,
                local,
                RejectReason::MalformedHello,
                error.to_string(),
            ));
        }
    };
    if let Err((reason, detail)) = check(&peer, local, Role::Guest) {
        return Err(refuse(connection, local, reason, detail));
    }
    send(
        connection,
        Body::HelloAccepted(HelloAccepted {
            host: Some(local.clone()),
        }),
    )?;
    Ok(peer)
}

/// The guest side: sends `local` and waits at most `timeout` for the host's answer. Returns
/// the host's `Hello` when accepted.
pub fn guest_connect(
    connection: &mut dyn Connection,
    local: &Hello,
    timeout: Duration,
) -> Result<Hello, HandshakeError> {
    send(connection, Body::Hello(local.clone()))?;
    let frame = connection.recv(Some(timeout))?;
    match decode(&frame) {
        Ok(Envelope {
            body: Some(Body::HelloAccepted(accepted)),
        }) => accepted.host.ok_or_else(|| {
            HandshakeError::Malformed("HelloAccepted without the host's Hello".into())
        }),
        Ok(Envelope {
            body: Some(Body::HelloRejected(rejected)),
        }) => Err(HandshakeError::Rejected {
            reason: RejectReason::try_from(rejected.reason).unwrap_or(RejectReason::Unspecified),
            detail: rejected.detail,
        }),
        Ok(_) => Err(HandshakeError::Malformed(
            "the host answered with something other than HelloAccepted or HelloRejected".into(),
        )),
        Err(error) => Err(HandshakeError::Malformed(error.to_string())),
    }
}
