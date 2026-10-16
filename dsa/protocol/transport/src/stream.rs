//! Framing over a byte stream, shared by every socket transport.
//!
//! On the wire a frame is its length as a little-endian `u32`, then that many bytes. The
//! socket transports (hvsocket on Windows, vsock in the guest, Unix sockets on Linux)
//! differ only in how the socket is opened; once it is, they wrap it in
//! [`FramedConnection`] and behave the same.

use std::io::{self, Read, Write};
use std::thread;
use std::time::{Duration, Instant};

use crate::{check_frame_len, Connection, TransportError};

const HEADER_LEN: usize = 4;

/// A connected stream socket that [`FramedConnection`] can drive.
pub trait StreamSocket: Read + Write + Send {
    /// Sets how long a `read` may block; `None` blocks indefinitely.
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()>;

    /// Shuts the socket down in both directions, so the peer sees end of stream.
    fn shutdown(&mut self);
}

/// A [`Connection`] that frames messages over a [`StreamSocket`].
pub struct FramedConnection<S: StreamSocket> {
    socket: S,
    /// Bytes received but not yet returned: a partial frame kept across timeouts.
    pending: Vec<u8>,
    /// Set once the stream position is unknown (an oversized length) or the socket closed.
    broken: bool,
}

impl<S: StreamSocket> FramedConnection<S> {
    pub fn new(socket: S) -> Self {
        FramedConnection {
            socket,
            pending: Vec::new(),
            broken: false,
        }
    }

    /// Returns a complete frame from `pending`, if there is one.
    fn take_frame(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        if self.pending.len() < HEADER_LEN {
            return Ok(None);
        }
        let mut header = [0u8; HEADER_LEN];
        header.copy_from_slice(&self.pending[..HEADER_LEN]);
        let len = u32::from_le_bytes(header) as usize;
        if let Err(error) = check_frame_len(len) {
            self.broken = true;
            self.pending.clear();
            return Err(error);
        }
        if self.pending.len() < HEADER_LEN + len {
            return Ok(None);
        }
        let frame = self.pending[HEADER_LEN..HEADER_LEN + len].to_vec();
        self.pending.drain(..HEADER_LEN + len);
        Ok(Some(frame))
    }
}

impl<S: StreamSocket> Connection for FramedConnection<S> {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        check_frame_len(frame.len())?;
        if self.broken {
            return Err(TransportError::Closed);
        }
        let mut wire = Vec::with_capacity(HEADER_LEN + frame.len());
        wire.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        wire.extend_from_slice(frame);
        let result = self
            .socket
            .write_all(&wire)
            .and_then(|()| self.socket.flush());
        result.map_err(|error| {
            let error = TransportError::from(error);
            if error == TransportError::Closed {
                self.broken = true;
            }
            error
        })
    }

    fn recv(&mut self, timeout: Option<Duration>) -> Result<Vec<u8>, TransportError> {
        if self.broken {
            return Err(TransportError::Closed);
        }
        if let Some(frame) = self.take_frame()? {
            return Ok(frame);
        }
        let deadline = timeout.map(|timeout| Instant::now() + timeout);
        let mut chunk = vec![0u8; 64 * 1024];
        let mut first_read = true;
        loop {
            let remaining = match deadline {
                None => None,
                Some(deadline) => {
                    let now = Instant::now();
                    // Always read once, so a zero timeout still picks up what has arrived.
                    if now >= deadline && !first_read {
                        return Err(TransportError::Timeout);
                    }
                    // A zero read timeout means "block forever" to the OS, so never pass one.
                    Some(
                        deadline
                            .saturating_duration_since(now)
                            .max(Duration::from_millis(1)),
                    )
                }
            };
            first_read = false;
            self.socket.set_read_timeout(remaining)?;
            match self.socket.read(&mut chunk) {
                Ok(0) => {
                    self.broken = true;
                    return Err(TransportError::Closed);
                }
                Ok(read) => {
                    self.pending.extend_from_slice(&chunk[..read]);
                    if let Some(frame) = self.take_frame()? {
                        return Ok(frame);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    let error = TransportError::from(error);
                    if error == TransportError::Closed {
                        self.broken = true;
                    }
                    return Err(error);
                }
            }
        }
    }

    fn close(&mut self) {
        if !self.broken {
            self.socket.shutdown();
            self.broken = true;
        }
    }
}

impl<S: StreamSocket> Drop for FramedConnection<S> {
    fn drop(&mut self) {
        self.close();
    }
}

/// Repeats a non-blocking accept until it yields a connection or `timeout` passes.
///
/// `try_accept` must return `WouldBlock` while nothing is waiting. Socket listeners use
/// this so that every transport supports an accept timeout the same way.
pub fn accept_with_timeout<T>(
    timeout: Duration,
    mut try_accept: impl FnMut() -> io::Result<T>,
) -> Result<T, TransportError> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_accept() {
            Ok(accepted) => return Ok(accepted),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock
                    || error.kind() == io::ErrorKind::Interrupted =>
            {
                if Instant::now() >= deadline {
                    return Err(TransportError::Timeout);
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error.into()),
        }
    }
}
