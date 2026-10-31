//! The guest channel daemon against a host that runs the real handshake, over the
//! in-memory transport.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_guest_channeld::{
    connect, pump, run, ChannelError, ChannelEvent, Credentials, RetryPolicy,
};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{RejectReason, Role};
use dsa_protocol::handshake::{host_accept, local_hello};
use dsa_transport::memory::{self, MemoryConnector, MemoryListener};
use dsa_transport::{Connection, Connector, FrameSender, Listener, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn fast() -> RetryPolicy {
    RetryPolicy {
        first_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        attempt_timeout: WAIT,
    }
}

/// The secret the host gave this boot.
fn secret() -> BootSecret {
    BootSecret::from_bytes([0x11; 32])
}

fn credentials(hello: dsa_protocol::control::Hello) -> Credentials {
    Credentials {
        hello,
        secret: secret(),
    }
}

fn guest_hello() -> dsa_protocol::control::Hello {
    local_hello(Role::Guest, "dsa-channeld test")
}

/// Accepts one connection and runs the host side of the handshake on it.
fn host_accepts(listener: &mut MemoryListener) -> Box<dyn Connection> {
    let mut connection = listener.accept(WAIT).expect("the daemon connects");
    host_accept(
        connection.as_mut(),
        &local_hello(Role::Host, "dsa-hostd test"),
        &secret(),
        WAIT,
    )
    .expect("accepted");
    connection
}

/// A connector that fails a given number of times before connecting, as a host that is
/// still starting would.
struct FlakyConnector {
    inner: MemoryConnector,
    failures_left: Arc<AtomicU32>,
    attempts: Arc<AtomicU32>,
}

impl Connector for FlakyConnector {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.attempts.fetch_add(1, Ordering::Relaxed);
        if self.failures_left.load(Ordering::Relaxed) > 0 {
            self.failures_left.fetch_sub(1, Ordering::Relaxed);
            return Err(TransportError::Io {
                kind: std::io::ErrorKind::ConnectionRefused,
                message: "no listener yet".into(),
            });
        }
        self.inner.connect(timeout)
    }
}

#[test]
fn fr2_the_daemon_connects_once_the_host_listens() {
    let (mut listener, connector) = memory::pair();
    let attempts = Arc::new(AtomicU32::new(0));
    let mut flaky = FlakyConnector {
        inner: connector,
        failures_left: Arc::new(AtomicU32::new(3)),
        attempts: Arc::clone(&attempts),
    };
    let host = thread::spawn(move || host_accepts(&mut listener));
    let stop = AtomicBool::new(false);
    let (_connection, host_hello) =
        connect(&mut flaky, &credentials(guest_hello()), fast(), &stop).expect("connects");
    assert_eq!(host_hello.build, "dsa-hostd test");
    assert_eq!(
        attempts.load(Ordering::Relaxed),
        4,
        "three refused attempts, then one that worked"
    );
    host.join().unwrap();
}

#[test]
fn fr2_a_refusing_host_is_not_retried() {
    let (mut listener, mut connector) = memory::pair();
    let host = thread::spawn(move || {
        let mut connection = listener.accept(WAIT).unwrap();
        let host = local_hello(Role::Host, "dsa-hostd test");
        let _ = host_accept(connection.as_mut(), &host, &secret(), WAIT);
        // A second attempt would show up here.
        listener.accept(Duration::from_millis(200)).is_ok()
    });
    let mut stale = guest_hello();
    stale.schema_hash = vec![0x11; 32];
    let stop = AtomicBool::new(false);
    match connect(&mut connector, &credentials(stale.clone()), fast(), &stop) {
        Err(ChannelError::Refused { reason, detail }) => {
            assert_eq!(reason, RejectReason::SchemaHashMismatch);
            assert!(detail.contains("different protocol definition"), "{detail}");
        }
        Err(other) => panic!("expected a refusal, got {other:?}"),
        Ok(_) => panic!("expected a refusal, got a connection"),
    }
    assert!(
        !host.join().unwrap(),
        "the daemon must not try again after a refusal"
    );
}

#[test]
fn fr2_stopping_ends_the_retries() {
    let (listener, connector) = memory::pair();
    drop(listener);
    let mut flaky = FlakyConnector {
        inner: connector,
        failures_left: Arc::new(AtomicU32::new(u32::MAX)),
        attempts: Arc::new(AtomicU32::new(0)),
    };
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            stop.store(true, Ordering::Relaxed);
        })
    };
    assert_eq!(
        connect(&mut flaky, &credentials(guest_hello()), fast(), &stop).err(),
        Some(ChannelError::Stopped)
    );
    stopper.join().unwrap();
}

#[test]
fn fr2_frames_flow_both_ways_and_a_lost_channel_is_reconnected() {
    let (mut listener, mut connector) = memory::pair();
    let (to_guest_tx, to_guest_rx) = mpsc::channel();
    let (from_guest_tx, from_guest_rx) = mpsc::channel::<Vec<u8>>();
    let (events_tx, events_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let daemon = {
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            run(
                &mut connector,
                &credentials(guest_hello()),
                fast(),
                &to_guest_tx,
                &from_guest_rx,
                &events_tx,
                &stop,
            )
        })
    };

    // First connection: frames both ways, then the host side drops it.
    let mut first = host_accepts(&mut listener);
    assert_eq!(
        events_rx.recv_timeout(WAIT).unwrap(),
        ChannelEvent::Connected {
            host_build: "dsa-hostd test".into()
        }
    );
    first.send(b"to the guest").unwrap();
    assert_eq!(to_guest_rx.recv_timeout(WAIT).unwrap(), b"to the guest");
    from_guest_tx.send(b"to the host".to_vec()).unwrap();
    assert_eq!(first.recv(Some(WAIT)).unwrap(), b"to the host");
    drop(first);
    assert!(matches!(
        events_rx.recv_timeout(WAIT).unwrap(),
        ChannelEvent::Lost(_)
    ));

    // The daemon connects again by itself.
    let mut second = host_accepts(&mut listener);
    assert!(matches!(
        events_rx.recv_timeout(WAIT).unwrap(),
        ChannelEvent::Connected { .. }
    ));
    from_guest_tx.send(b"again".to_vec()).unwrap();
    assert_eq!(second.recv(Some(WAIT)).unwrap(), b"again");

    stop.store(true, Ordering::Relaxed);
    assert_eq!(daemon.join().unwrap(), ChannelError::Stopped);
}

/// A connection whose host says nothing until the test lets it, however long the wait asked.
struct SilentHost {
    release: mpsc::Receiver<()>,
    sent: mpsc::Sender<Vec<u8>>,
}

struct SilentHostSender(mpsc::Sender<Vec<u8>>);

impl FrameSender for SilentHostSender {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        self.0
            .send(frame.to_vec())
            .map_err(|_| TransportError::Closed)
    }
}

impl Connection for SilentHost {
    fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        self.sent
            .send(frame.to_vec())
            .map_err(|_| TransportError::Closed)
    }
    fn recv(&mut self, _timeout: Option<Duration>) -> Result<Vec<u8>, TransportError> {
        let _ = self.release.recv();
        Err(TransportError::Closed)
    }
    fn close(&mut self) {}
    fn try_clone_sender(&self) -> Option<Box<dyn FrameSender>> {
        Some(Box::new(SilentHostSender(self.sent.clone())))
    }
}

#[test]
fn fr5_a_frame_for_the_host_does_not_wait_for_the_hosts_next_frame() {
    let (release, released) = mpsc::channel();
    let (sent_tx, sent) = mpsc::channel();
    let (to_host, from_guest) = mpsc::channel();
    let (to_guest, _from_host) = mpsc::channel();
    let pumping = thread::spawn(move || {
        let stop = AtomicBool::new(false);
        let mut host = SilentHost {
            release: released,
            sent: sent_tx,
        };
        pump(&mut host, &to_guest, &from_guest, &stop)
    });
    // Let the pump settle into its wait for the host.
    thread::sleep(Duration::from_millis(100));
    to_host.send(vec![1, 2, 3]).unwrap();
    let frame = sent.recv_timeout(Duration::from_secs(2));
    release.send(()).unwrap();
    assert_eq!(frame.expect("the frame goes at once"), vec![1, 2, 3]);
    assert_eq!(pumping.join().unwrap(), TransportError::Closed);
}
