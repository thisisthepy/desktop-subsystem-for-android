//! The shared framing, over a real loopback socket.
//!
//! The production transports (hvsocket, vsock) open their sockets differently but frame
//! through the same `FramedConnection`, so the conformance suite runs here against a TCP
//! loopback socket, which every CI machine has. TCP is a test vehicle only; nothing in
//! the product listens on TCP.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::time::Duration;

use dsa_transport::stream::{
    accept_with_timeout, FramedConnection, RawStreamConnection, StreamSocket,
};
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
    fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Tcp)
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

/// A socket whose peer has gone and that refuses read timeouts, as macOS does once both
/// directions are shut down. It still holds one frame that arrived before.
struct ShutDownSocket {
    buffered: io::Cursor<Vec<u8>>,
}

impl Read for ShutDownSocket {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.buffered.read(buf)
    }
}

impl Write for ShutDownSocket {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl StreamSocket for ShutDownSocket {
    fn set_read_timeout(&mut self, _: Option<Duration>) -> io::Result<()> {
        // What EINVAL maps to on macOS.
        Err(io::ErrorKind::InvalidInput.into())
    }
    fn shutdown(&mut self) {}
}

#[test]
fn pr1_a_socket_that_refuses_timeouts_after_the_peer_left_still_reports_closed() {
    let mut wire = 3u32.to_le_bytes().to_vec();
    wire.extend_from_slice(b"end");
    let mut connection = FramedConnection::new(ShutDownSocket {
        buffered: io::Cursor::new(wire),
    });
    assert_eq!(
        connection.recv(Some(Duration::from_secs(1))).unwrap(),
        b"end"
    );
    assert_eq!(
        connection.recv(Some(Duration::from_secs(1))),
        Err(TransportError::Closed)
    );
}

/// The host renderer replies to the guest while it waits for the guest's next bytes. With
/// one handle for both, every reply waited for the read to time out, and a Vulkan start
/// that makes thousands of round trips took minutes.
#[test]
fn pr3_a_raw_stream_sends_while_another_thread_waits_to_receive() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let mut guest = TcpStream::connect(listener.local_addr().unwrap()).expect("connect");
    let (accepted, _) = listener.accept().expect("accept");
    let mut host = RawStreamConnection::new(Tcp(accepted));
    let mut sender = host
        .try_clone_sender()
        .expect("a raw stream can share its socket");

    let reader = std::thread::spawn(move || host.recv(Some(Duration::from_secs(5))));
    std::thread::sleep(Duration::from_millis(100));

    let started = std::time::Instant::now();
    sender.send(b"reply").expect("send while the reader waits");
    let mut reply = [0u8; 5];
    guest
        .read_exact(&mut reply)
        .expect("the guest gets the reply");
    assert_eq!(&reply, b"reply");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );

    guest.write_all(b"next").expect("guest writes");
    assert_eq!(reader.join().unwrap().expect("the reader gets it"), b"next");
}

/// Pointer input goes to the guest while the host waits on the control channel for the
/// guest's next message. With one handle for both, every pointer event waited for that
/// read to time out.
#[test]
fn fr5_a_framed_connection_sends_while_another_thread_waits_to_receive() {
    let (mut host, raw) = framed_and_raw();
    let mut guest: Box<dyn Connection> = Box::new(FramedConnection::new(Tcp(raw)));
    let mut sender = host
        .try_clone_sender()
        .expect("a framed connection can share its socket");

    let reader = std::thread::spawn(move || host.recv(Some(Duration::from_secs(5))));
    std::thread::sleep(Duration::from_millis(100));

    let started = std::time::Instant::now();
    sender
        .send(b"pointer")
        .expect("send while the reader waits");
    sender.send(b"").expect("an empty frame is a frame");
    assert_eq!(
        guest.recv(Some(Duration::from_secs(1))).unwrap(),
        b"pointer"
    );
    assert_eq!(guest.recv(Some(Duration::from_secs(1))).unwrap(), b"");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );

    guest.send(b"next").expect("guest writes");
    assert_eq!(reader.join().unwrap().expect("the reader gets it"), b"next");
}

#[test]
fn fr5_a_framed_sender_refuses_a_frame_over_the_limit() {
    let (host, _raw) = framed_and_raw();
    let mut sender = host.try_clone_sender().expect("a sender");
    assert!(matches!(
        sender.send(&vec![0u8; MAX_FRAME_LEN + 1]),
        Err(TransportError::FrameTooLarge { .. })
    ));
}
