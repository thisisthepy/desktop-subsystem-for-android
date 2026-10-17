//! The shared framing, over a real loopback socket.
//!
//! The production transports (hvsocket, vsock) open their sockets differently but frame
//! through the same `FramedConnection`, so the conformance suite runs here against a TCP
//! loopback socket, which every CI machine has. TCP is a test vehicle only; nothing in
//! the product listens on TCP.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;

use dsa_transport::stream::{accept_with_timeout, FramedConnection, StreamSocket};
use dsa_transport::{Connection, Connector, Listener, TransportError, MAX_FRAME_LEN};

struct Tcp(TcpStream);

impl Read for Tcp {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for Tcp {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl StreamSocket for Tcp {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }
    fn shutdown(&mut self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

struct TcpLoopbackListener(TcpListener);

impl Listener for TcpLoopbackListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let (stream, _) = accept_with_timeout(timeout, || self.0.accept())?;
        stream.set_nonblocking(false)?;
        Ok(Box::new(FramedConnection::new(Tcp(stream))))
    }
}

struct TcpLoopbackConnector(std::net::SocketAddr);

impl Connector for TcpLoopbackConnector {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let stream = TcpStream::connect_timeout(&self.0, timeout)?;
        Ok(Box::new(FramedConnection::new(Tcp(stream))))
    }
}

fn tcp_pair() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().expect("address");
    (
        Box::new(TcpLoopbackListener(listener)),
        Box::new(TcpLoopbackConnector(address)),
    )
}

dsa_transport::transport_conformance!(tcp_pair());

/// A raw socket on the far side, so a test can put arbitrary bytes on the wire.
fn framed_and_raw() -> (Box<dyn Connection>, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let raw = TcpStream::connect(listener.local_addr().unwrap()).expect("connect");
    let (accepted, _) = listener.accept().expect("accept");
    (Box::new(FramedConnection::new(Tcp(accepted))), raw)
}

#[test]
fn pr1_a_frame_split_across_timeouts_is_reassembled() {
    let (mut framed, mut raw) = framed_and_raw();
    let mut wire = 5u32.to_le_bytes().to_vec();
    wire.extend_from_slice(b"hello");
    for byte in &wire[..6] {
        raw.write_all(&[*byte]).unwrap();
        assert_eq!(
            framed.recv(Some(Duration::from_millis(20))),
            Err(TransportError::Timeout)
        );
    }
    raw.write_all(&wire[6..]).unwrap();
    assert_eq!(
        framed.recv(Some(Duration::from_secs(10))).unwrap(),
        b"hello"
    );
}

#[test]
fn pr1_an_oversized_length_from_the_peer_is_an_error_not_an_allocation() {
    let (mut framed, mut raw) = framed_and_raw();
    raw.write_all(&u32::MAX.to_le_bytes()).unwrap();
    assert_eq!(
        framed.recv(Some(Duration::from_secs(10))),
        Err(TransportError::FrameTooLarge {
            len: u32::MAX as usize,
            max: MAX_FRAME_LEN
        })
    );
    // The stream position is now unknown, so the connection is finished.
    assert_eq!(
        framed.recv(Some(Duration::from_secs(1))),
        Err(TransportError::Closed)
    );
}

#[test]
fn pr1_end_of_stream_inside_a_frame_is_reported_as_closed() {
    let (mut framed, mut raw) = framed_and_raw();
    raw.write_all(&10u32.to_le_bytes()).unwrap();
    raw.write_all(b"abc").unwrap();
    drop(raw);
    assert_eq!(
        framed.recv(Some(Duration::from_secs(10))),
        Err(TransportError::Closed)
    );
}
