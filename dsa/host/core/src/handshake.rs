//! The session's handshake: the control protocol's version and schema hash check.

use std::time::Duration;

use dsa_protocol::control::{Hello, Role};
use dsa_protocol::handshake::{host_accept, local_hello};
use dsa_transport::Connection;

use crate::session::Handshake;

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
    fn accept(&mut self, connection: &mut dyn Connection) -> Result<(), String> {
        host_accept(connection, &self.local, self.timeout)
            .map(|_guest| ())
            .map_err(|error| error.to_string())
    }
}
