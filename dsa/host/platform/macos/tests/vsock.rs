//! The transport over the connections the Virtualization framework hands the host.
//!
//! The framework gives each of the guest's connections to a host port as a connected
//! stream socket. A socket pair stands in for one here: one end goes to the port's channel,
//! as the listener delegate sends it, and the other is the guest's side.
#![cfg(target_os = "macos")]

use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_macos::vsock::{Framing, VsockListener, VsockStream};
use dsa_host_macos::vz::PortChannels;
use dsa_transport::stream::FramedConnection;
use dsa_transport::{Connection, Connector, Listener, TransportError};

/// The guest's side: each connect makes a pair and hands the host's end to the channel.
struct GuestDialer(Sender<OwnedFd>);

impl Connector for GuestDialer {
    fn connect(&mut self, _timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let (host, guest) = UnixStream::pair()?;
        self.0
            .send(OwnedFd::from(host))
            .map_err(|_| TransportError::Closed)?;
        Ok(Box::new(FramedConnection::new(VsockStream::from_fd(
            OwnedFd::from(guest),
        ))))
    }
}

fn vsock_pair() -> (Box<dyn Listener>, Box<dyn Connector>) {
    let (tx, rx) = mpsc::channel();
    (
        Box::new(VsockListener::new(
            Arc::new(Mutex::new(rx)),
            Framing::Framed,
        )),
        Box::new(GuestDialer(tx)),
    )
}

dsa_transport::transport_conformance!(vsock_pair());

#[test]
fn pr1_each_port_has_one_channel_across_boots() {
    let channels = PortChannels::default();
    let first = channels.receiver(5000);
    let again = channels.receiver(5000);
    assert!(Arc::ptr_eq(&first, &again));
    assert!(!Arc::ptr_eq(&first, &channels.receiver(5001)));
}

/// A boot drains the connections an earlier VM left, while the readers of every port keep
/// waiting for the next one: each takes its port's lock for a whole wait and takes it again
/// at once. The drain must not wait for a turn that the macOS mutex never gives it, or the
/// VM is never started and Android stays in "Starting".
#[test]
fn fr1_draining_the_ports_for_a_boot_is_not_held_up_by_readers_waiting_on_them() {
    let channels = Arc::new(PortChannels::default());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readers: Vec<_> = (5566..5570)
        .map(|port| {
            let incoming = channels.receiver(port);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    let rx = incoming.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = rx.recv_timeout(Duration::from_millis(500));
                }
            })
        })
        .collect();
    // Let every reader settle into its wait.
    std::thread::sleep(Duration::from_millis(200));
    let (done_tx, done_rx) = mpsc::channel();
    let drainer = Arc::clone(&channels);
    std::thread::spawn(move || {
        drainer.drain();
        let _ = done_tx.send(());
    });
    let drained = done_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for reader in readers {
        let _ = reader.join();
    }
    assert!(
        drained,
        "drain did not return while readers waited on the ports"
    );
}

#[test]
fn fr29_the_file_server_port_carries_a_plain_byte_stream() {
    let (tx, rx) = mpsc::channel();
    let mut listener = VsockListener::new(Arc::new(Mutex::new(rx)), Framing::Raw);
    let (host, mut guest) = UnixStream::pair().unwrap();
    tx.send(OwnedFd::from(host)).unwrap();
    let mut connection = listener.accept(Duration::from_secs(5)).unwrap();
    std::io::Write::write_all(&mut guest, b"9P2000").unwrap();
    let got = connection.recv(Some(Duration::from_secs(5))).unwrap();
    assert_eq!(got, b"9P2000");
}

#[test]
fn nfr5_a_listener_with_no_guest_times_out() {
    let (_tx, rx) = mpsc::channel::<OwnedFd>();
    let mut listener = VsockListener::new(Arc::new(Mutex::new(rx)), Framing::Framed);
    assert!(matches!(
        listener.accept(Duration::from_millis(50)),
        Err(TransportError::Timeout)
    ));
}
