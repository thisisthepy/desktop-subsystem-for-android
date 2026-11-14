//! What the host core's integration tests share.

use std::time::{Duration, Instant};

use dsa_protocol::control::envelope::Body;
use dsa_transport::{Connection, FrameSender, TransportError};

/// The guest's side of the control channel, past the settings the session sends each time the
/// guest connects: the host's root certificates and whether backup is on. Their own tests read
/// them; these tests are about everything else, and would otherwise meet them first.
pub struct PastConnectSettings(pub Box<dyn Connection>);

impl Connection for PastConnectSettings {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        self.0.send(frame)
    }

    fn recv(&mut self, timeout: Option<Duration>) -> Result<Vec<u8>, TransportError> {
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        loop {
            let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            let frame = self.0.recv(left)?;
            let setting = matches!(
                dsa_protocol::decode(&frame).map(|envelope| envelope.body),
                Ok(Some(Body::HostCertificates(_) | Body::BackupSetting(_)))
            );
            if !setting {
                return Ok(frame);
            }
        }
    }

    fn close(&mut self) {
        self.0.close()
    }

    fn try_clone_sender(&self) -> Option<Box<dyn FrameSender>> {
        self.0.try_clone_sender()
    }
}
