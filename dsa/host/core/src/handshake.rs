//! The session's handshake: the control protocol's version and schema hash check.

use std::time::Duration;

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{Hello, RejectReason, Role};
use dsa_protocol::handshake::{host_accept, local_hello, HandshakeError};
use dsa_transport::Connection;

use crate::session::{Handshake, Refusal};

/// Accepts a guest only if it was generated from the same protocol definition as this
/// host. A refused guest is told why, and the session reports the same reason.
pub struct ControlHandshake {
    local: Hello,
    timeout: Duration,
}

impl ControlHandshake {
    /// `build` names this host build in what the guest is told; `timeout` bounds the wait
    /// for the guest's first message.
    pub fn new(build: &str, timeout: Duration) -> ControlHandshake {
        ControlHandshake {
            local: local_hello(Role::Host, build),
            timeout,
        }
    }
}

impl Handshake for ControlHandshake {
    fn accept(
        &mut self,
        connection: &mut dyn Connection,
        secret: &BootSecret,
    ) -> Result<(), Refusal> {
        host_accept(connection, &self.local, secret, self.timeout)
            .map(|_guest| ())
            .map_err(|error| match error {
                HandshakeError::Rejected {
                    reason: RejectReason::AuthenticationFailed,
                    ..
                }
                | HandshakeError::Transport(_) => Refusal::NotTheGuest(error.to_string()),
                HandshakeError::Rejected { .. } | HandshakeError::Malformed(_) => {
                    Refusal::Incompatible(error.to_string())
                }
            })
    }
}
