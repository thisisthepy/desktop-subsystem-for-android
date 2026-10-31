//! Local connections, where the operating system tells the server who connected.
//!
//! The local API between the host service and the user's apps runs over a named pipe on
//! Windows and a Unix socket on Linux and macOS. Unlike the control channel, the server
//! must know which user is on the other end, and it learns that from the OS (pipe client
//! impersonation, `SO_PEERCRED`, `getpeereid`), never from the messages. A
//! [`LocalListener`] therefore hands back the [`Peer`] with each connection.

use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use crate::memory;
use crate::{Connection, Connector, Listener, TransportError};

/// The user on the other end of a local connection, as the OS identified them: a SID
/// (`S-1-5-21-...`) on Windows, `uid:<n>` on Linux and macOS.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Peer {
    pub user: String,
    /// Whether the OS says the caller acts as an administrator now: on Windows, a token that
    /// is a member of BUILTIN\Administrators, which takes an elevated process when UAC is on;
    /// on Linux and macOS, root. Settings that apply to every user of the machine need it.
    pub administrator: bool,
}

impl fmt::Display for Peer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.user)
    }
}

/// The accepting side of the local API.
pub trait LocalListener: Send {
    /// Waits at most `timeout` for the next connection and says who made it.
    fn accept(&mut self, timeout: Duration) -> Result<(Box<dyn Connection>, Peer), TransportError>;
}

/// Uses a [`LocalListener`] where a plain [`Listener`] is expected, dropping the peer. For
/// running the transport conformance suite over local transports.
pub struct PeerIgnoringListener<L: LocalListener>(pub L);

impl<L: LocalListener> Listener for PeerIgnoringListener<L> {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.accept(timeout).map(|(connection, _)| connection)
    }
}

/// The in-memory local transport: the test says who each connection comes from, standing
/// in for what the OS would report.
pub fn pair() -> (MemoryLocalListener, MemoryLocalConnector) {
    let (tx, rx) = mpsc::channel();
    (
        MemoryLocalListener { incoming: rx },
        MemoryLocalConnector { listener: tx },
    )
}

pub struct MemoryLocalListener {
    incoming: Receiver<(Box<dyn Connection>, Peer)>,
}

#[derive(Clone)]
pub struct MemoryLocalConnector {
    listener: Sender<(Box<dyn Connection>, Peer)>,
}

impl MemoryLocalConnector {
    /// Connects as `peer`.
    pub fn connect_as(&mut self, peer: Peer) -> Result<Box<dyn Connection>, TransportError> {
        let (mut listener, mut connector) = memory::pair();
        let ours = connector.connect(Duration::ZERO)?;
        let theirs = listener.accept(Duration::ZERO)?;
        self.listener
            .send((theirs, peer))
            .map_err(|_| TransportError::Closed)?;
        Ok(ours)
    }
}

/// Connects as the user `test`, so the fake also runs the plain conformance suite.
impl Connector for MemoryLocalConnector {
    fn connect(&mut self, _timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.connect_as(Peer {
            user: "test".into(),
            administrator: false,
        })
    }
}

impl LocalListener for MemoryLocalListener {
    fn accept(&mut self, timeout: Duration) -> Result<(Box<dyn Connection>, Peer), TransportError> {
        match self.incoming.recv_timeout(timeout) {
            Ok(accepted) => Ok(accepted),
            Err(RecvTimeoutError::Timeout) => Err(TransportError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(TransportError::Closed),
        }
    }
}
