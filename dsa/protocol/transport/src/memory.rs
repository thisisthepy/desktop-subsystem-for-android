//! The in-memory transport: a fake for tests, with the same behaviour as the socket
//! transports.
//!
//! [`pair`] returns a listener and a connector that only reach each other. Frames travel
//! through channels, so no socket or second process is needed, and the frame size limit
//! is enforced exactly as on a real transport.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::Duration;

use crate::{check_frame_len, Connection, Connector, FrameSender, Listener, TransportError};

/// Returns a listener and the one connector that reaches it.
pub fn pair() -> (MemoryListener, MemoryConnector) {
    let (tx, rx) = mpsc::channel();
    (
        MemoryListener { incoming: rx },
        MemoryConnector { listener: tx },
    )
}

/// The accepting end of [`pair`].
pub struct MemoryListener {
    incoming: Receiver<MemoryConnection>,
}

/// The dialling end of [`pair`]. It can be cloned to connect from several places.
#[derive(Clone)]
pub struct MemoryConnector {
    listener: Sender<MemoryConnection>,
}

/// One end of an in-memory connection.
pub struct MemoryConnection {
    outgoing: Option<Sender<Vec<u8>>>,
    incoming: Receiver<Vec<u8>>,
}

impl MemoryConnection {
    fn pair() -> (MemoryConnection, MemoryConnection) {
        let (a_tx, a_rx) = mpsc::channel();
        let (b_tx, b_rx) = mpsc::channel();
        (
            MemoryConnection {
                outgoing: Some(a_tx),
                incoming: b_rx,
            },
            MemoryConnection {
                outgoing: Some(b_tx),
                incoming: a_rx,
            },
        )
    }
}

impl Listener for MemoryListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        match self.incoming.recv_timeout(timeout) {
            Ok(connection) => Ok(Box::new(connection)),
            Err(RecvTimeoutError::Timeout) => Err(TransportError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(TransportError::Closed),
        }
    }
}

impl Connector for MemoryConnector {
    fn connect(&mut self, _timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let (ours, theirs) = MemoryConnection::pair();
        self.listener
            .send(theirs)
            .map_err(|_| TransportError::Closed)?;
        Ok(Box::new(ours))
    }
}

impl Connection for MemoryConnection {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        check_frame_len(frame.len())?;
        let outgoing = self.outgoing.as_ref().ok_or(TransportError::Closed)?;
        outgoing
            .send(frame.to_vec())
            .map_err(|_| TransportError::Closed)
    }

    fn recv(&mut self, timeout: Option<Duration>) -> Result<Vec<u8>, TransportError> {
        if self.outgoing.is_none() {
            return Err(TransportError::Closed);
        }
        match timeout {
            None => self.incoming.recv().map_err(|_| TransportError::Closed),
            Some(timeout) if timeout.is_zero() => match self.incoming.try_recv() {
                Ok(frame) => Ok(frame),
                Err(TryRecvError::Empty) => Err(TransportError::Timeout),
                Err(TryRecvError::Disconnected) => Err(TransportError::Closed),
            },
            Some(timeout) => match self.incoming.recv_timeout(timeout) {
                Ok(frame) => Ok(frame),
                Err(RecvTimeoutError::Timeout) => Err(TransportError::Timeout),
                Err(RecvTimeoutError::Disconnected) => Err(TransportError::Closed),
            },
        }
    }

    fn close(&mut self) {
        self.outgoing = None;
    }

    fn try_clone_sender(&self) -> Option<Box<dyn FrameSender>> {
        let outgoing = self.outgoing.clone()?;
        Some(Box::new(MemorySender { outgoing }))
    }
}

/// The sending half of a [`MemoryConnection`]. The peer sees the connection end once the
/// connection and every sender are gone.
pub struct MemorySender {
    outgoing: Sender<Vec<u8>>,
}

impl FrameSender for MemorySender {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        check_frame_len(frame.len())?;
        self.outgoing
            .send(frame.to_vec())
            .map_err(|_| TransportError::Closed)
    }
}
