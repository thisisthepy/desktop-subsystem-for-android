//! Developer mode's adb bridge: a TCP port on the host's loopback address whose connections
//! go to the guest's adbd.
//!
//! The guest's adbd listens on vsock port [`GUEST_ADB_PORT`] and nowhere else. The bridge
//! listens on `127.0.0.1:<port>` only, never on another address, so only programs on this
//! computer reach it, and copies each connection's bytes both ways to a stream the platform
//! opens to that vsock port (on Windows a Hyper-V socket). adb then connects with
//! `adb connect 127.0.0.1:<port>` and sees an ordinary TCP device.
//!
//! The bridge runs while developer mode is on. Dropping it closes the port and every
//! connection through it. Nothing here names a host operating system API: the platform hands
//! in how to reach the guest.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

/// The vsock port the guest's adbd listens on.
pub const GUEST_ADB_PORT: u32 = 5555;

/// One end of a connection the bridge copies: readable, writable, and able to be cloned and
/// shut down, so one thread reads it while another writes it and either can end both.
pub trait BridgeStream: Read + Write + Send {
    /// A second handle on the same connection.
    fn try_clone_stream(&self) -> io::Result<Box<dyn BridgeStream>>;
    /// Ends the connection in both directions, for every handle on it.
    fn shutdown_stream(&self);
}

impl BridgeStream for TcpStream {
    fn try_clone_stream(&self) -> io::Result<Box<dyn BridgeStream>> {
        Ok(Box::new(self.try_clone()?))
    }

    fn shutdown_stream(&self) {
        let _ = self.shutdown(Shutdown::Both);
    }
}

/// Opens a connection to the guest's adbd. It fails when the guest is not running.
pub type GuestAdb = Arc<dyn Fn() -> io::Result<Box<dyn BridgeStream>> + Send + Sync>;

/// A running bridge. Dropping it stops it.
pub struct AdbBridge {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
    accepting: Option<JoinHandle<()>>,
}

impl AdbBridge {
    /// Listens on `127.0.0.1:port` and sends each connection to the stream `guest` opens.
    /// Port 0 takes any free port, for tests. It fails when the port is taken.
    pub fn start(port: u16, guest: GuestAdb) -> io::Result<AdbBridge> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let connections: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::default();
        let accepting = {
            let (stop, connections) = (Arc::clone(&stop), Arc::clone(&connections));
            thread::Builder::new()
                .name("dsa-adb-bridge".into())
                .spawn(move || accept(listener, guest, stop, connections))?
        };
        Ok(AdbBridge {
            address,
            stop,
            connections,
            accepting: Some(accepting),
        })
    }

    /// The address adb connects to.
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}

impl Drop for AdbBridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accepting thread waits in accept; a connection of our own wakes it to see the
        // stop.
        let _ = TcpStream::connect(self.address);
        if let Some(accepting) = self.accepting.take() {
            let _ = accepting.join();
        }
        for (_, connection) in self
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
        {
            let _ = connection.shutdown(Shutdown::Both);
        }
    }
}

fn accept(
    listener: TcpListener,
    guest: GuestAdb,
    stop: Arc<AtomicBool>,
    connections: Arc<Mutex<HashMap<u64, TcpStream>>>,
) {
    let next_id = AtomicU64::new(0);
    for incoming in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let client = match incoming {
            Ok(client) => client,
            Err(error) => {
                eprintln!("adb bridge: accepting a connection failed: {error}");
                continue;
            }
        };
        let _ = client.set_nodelay(true);
        let id = next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(kept) = client.try_clone() {
            connections
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id, kept);
        }
        let (guest, connections) = (Arc::clone(&guest), Arc::clone(&connections));
        let spawned = thread::Builder::new()
            .name("dsa-adb-connection".into())
            .spawn(move || {
                match guest() {
                    Ok(stream) => {
                        if let Err(error) = splice(Box::new(client), stream) {
                            eprintln!("adb bridge: a connection ended: {error}");
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "adb bridge: cannot reach adb in Android ({error}); is Android \
                             running?"
                        );
                        let _ = client.shutdown(Shutdown::Both);
                    }
                }
                connections
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&id);
            });
        if let Err(error) = spawned {
            eprintln!("adb bridge: cannot serve a connection: {error}");
        }
    }
}

/// Copies `a` to `b` and `b` to `a` until either ends, then ends both.
fn splice(a: Box<dyn BridgeStream>, b: Box<dyn BridgeStream>) -> io::Result<()> {
    let (a_reader, b_reader) = (a.try_clone_stream()?, b.try_clone_stream()?);
    let backward = thread::Builder::new()
        .name("dsa-adb-copy".into())
        .spawn(move || copy_then_end(b_reader, a))?;
    copy_then_end(a_reader, b);
    let _ = backward.join();
    Ok(())
}

fn copy_then_end(mut from: Box<dyn BridgeStream>, mut to: Box<dyn BridgeStream>) {
    let _ = io::copy(&mut from, &mut to);
    from.shutdown_stream();
    to.shutdown_stream();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A guest adbd that answers each line with the same line, upper-cased.
    fn fake_guest() -> (GuestAdb, Arc<AtomicU64>) {
        let adbd = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = adbd.local_addr().unwrap();
        let opened = Arc::new(AtomicU64::new(0));
        thread::spawn(move || {
            for stream in adbd.incoming().flatten() {
                thread::spawn(move || {
                    let mut reader = stream.try_clone().unwrap();
                    let mut writer = stream;
                    let mut buf = [0u8; 64];
                    while let Ok(n) = reader.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        let _ = writer.write_all(&buf[..n].to_ascii_uppercase());
                    }
                });
            }
        });
        let count = Arc::clone(&opened);
        let guest: GuestAdb = Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(TcpStream::connect(address)?) as Box<dyn BridgeStream>)
        });
        (guest, opened)
    }

    fn exchange(address: SocketAddr, text: &[u8]) -> Vec<u8> {
        let mut adb = TcpStream::connect(address).unwrap();
        adb.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        adb.write_all(text).unwrap();
        let mut answer = vec![0u8; text.len()];
        adb.read_exact(&mut answer).unwrap();
        answer
    }

    #[test]
    fn fr20_the_bridge_listens_on_the_loopback_address_only() {
        let (guest, _) = fake_guest();
        let bridge = AdbBridge::start(0, guest).unwrap();
        assert_eq!(bridge.address().ip(), Ipv4Addr::LOCALHOST);
    }

    #[test]
    fn fr20_adb_reaches_the_guest_through_the_bridge_both_ways() {
        let (guest, opened) = fake_guest();
        let bridge = AdbBridge::start(0, guest).unwrap();
        assert_eq!(exchange(bridge.address(), b"host:version"), b"HOST:VERSION");
        assert_eq!(exchange(bridge.address(), b"shell:ls"), b"SHELL:LS");
        assert_eq!(
            opened.load(Ordering::SeqCst),
            2,
            "one guest stream per connection"
        );
    }

    #[test]
    fn fr20_stopping_the_bridge_closes_the_port_and_its_connections() {
        let (guest, _) = fake_guest();
        let bridge = AdbBridge::start(0, guest).unwrap();
        let address = bridge.address();
        let mut open = TcpStream::connect(address).unwrap();
        open.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        open.write_all(b"x").unwrap();
        let mut one = [0u8; 1];
        open.read_exact(&mut one).unwrap();
        drop(bridge);
        assert!(TcpStream::connect(address).is_err(), "the port is closed");
        let mut rest = [0u8; 1];
        assert!(
            matches!(open.read(&mut rest), Ok(0) | Err(_)),
            "the open connection ends"
        );
    }

    #[test]
    fn fr20_a_guest_that_is_not_running_ends_the_connection_and_the_bridge_keeps_listening() {
        let refusing: GuestAdb =
            Arc::new(|| Err(io::Error::new(io::ErrorKind::NotConnected, "not running")));
        let bridge = AdbBridge::start(0, refusing).unwrap();
        let mut adb = TcpStream::connect(bridge.address()).unwrap();
        adb.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = [0u8; 1];
        assert!(matches!(adb.read(&mut buf), Ok(0) | Err(_)));
        assert!(TcpStream::connect(bridge.address()).is_ok());
    }

    #[test]
    fn fr20_a_port_already_taken_is_reported() {
        let (guest, _) = fake_guest();
        let first = AdbBridge::start(0, Arc::clone(&guest)).unwrap();
        assert!(AdbBridge::start(first.address().port(), guest).is_err());
    }
}
