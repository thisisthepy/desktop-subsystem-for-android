//! The control channel's transport.
//!
//! The control channel carries frames: opaque byte strings, delivered whole and in order.
//! What a frame contains is the protocol's business; this crate only moves frames between
//! the host and the guest.
//!
//! Every transport implements the same two traits, [`Connection`] and [`Listener`] (and
//! [`Connector`] on the side that dials), so the code above it does not know whether it is
//! talking over hvsocket, vsock, a Unix socket or the in-memory fake in [`memory`]. The
//! socket transports share one framing implementation in [`stream`].
//!
//! A transport never panics on what the peer sends. A malformed or oversized frame is an
//! error on that connection, reported to the caller, so a bad guest cannot take the host
//! down.

use std::fmt;
use std::io;
use std::time::Duration;

pub mod local;
pub mod memory;
pub mod stream;

#[cfg(feature = "conformance")]
pub mod conformance;

/// The largest frame either side accepts, in bytes.
///
/// A peer that announces a larger frame is treated as broken rather than trusted with an
/// allocation of that size.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// What can go wrong on a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportError {
    /// Nothing arrived within the time the caller allowed. The connection is still usable.
    Timeout,
    /// The peer closed the connection, or it was lost.
    Closed,
    /// A frame was larger than [`MAX_FRAME_LEN`]. On send nothing was written; on receive the
    /// connection is no longer usable, because the stream position is unknown.
    FrameTooLarge { len: usize, max: usize },
    /// This transport cannot be used here, for example because the platform lacks the
    /// socket family.
    Unsupported(String),
    /// Any other I/O failure.
    Io {
        kind: io::ErrorKind,
        message: String,
    },
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Timeout => write!(f, "timed out"),
            TransportError::Closed => write!(f, "the connection is closed"),
            TransportError::FrameTooLarge { len, max } => {
                write!(
                    f,
                    "frame of {len} bytes is larger than the limit of {max} bytes"
                )
            }
            TransportError::Unsupported(why) => write!(f, "transport unavailable: {why}"),
            TransportError::Io { kind, message } => write!(f, "I/O error ({kind:?}): {message}"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<io::Error> for TransportError {
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => TransportError::Timeout,
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected => TransportError::Closed,
            kind => TransportError::Io {
                kind,
                message: error.to_string(),
            },
        }
    }
}

/// One established connection between the host and the guest.
pub trait Connection: Send {
    /// Sends one frame. Returns once the frame is handed to the transport.
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError>;

    /// Receives the next frame, waiting at most `timeout` (forever when `None`).
    ///
    /// A [`TransportError::Timeout`] leaves the connection usable: a frame that was partly
    /// received is kept and completed by a later call.
    fn recv(&mut self, timeout: Option<Duration>) -> Result<Vec<u8>, TransportError>;

    /// Closes the connection. The peer's next `recv` reports [`TransportError::Closed`].
    /// Dropping a connection closes it too.
    fn close(&mut self);

    /// A second handle that sends on this connection, so one thread can send while another
    /// waits in `recv`. `None` when the transport cannot share the connection.
    fn try_clone_sender(&self) -> Option<Box<dyn FrameSender>> {
        None
    }
}

/// The sending half of a [`Connection`], from [`Connection::try_clone_sender`].
pub trait FrameSender: Send {
    /// Sends one frame, as [`Connection::send`] does.
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError>;
}

/// The accepting side: where the host waits for the guest to connect.
pub trait Listener: Send {
    /// Waits at most `timeout` for the next connection.
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError>;
}

/// The dialling side.
pub trait Connector: Send {
    /// Opens a connection to the listener this connector was made for.
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError>;
}

/// Rejects a frame length larger than [`MAX_FRAME_LEN`].
pub fn check_frame_len(len: usize) -> Result<(), TransportError> {
    if len > MAX_FRAME_LEN {
        Err(TransportError::FrameTooLarge {
            len,
            max: MAX_FRAME_LEN,
        })
    } else {
        Ok(())
    }
}
