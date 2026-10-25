//! The guest's control channel daemon: the one process in the guest that holds the
//! connection to the host.
//!
//! It connects, passes the protocol handshake, and then carries frames between the host
//! and the guest's own users of the channel (the agent and the hwcomposer, which reach it
//! over binder inside the guest). When the connection is lost it connects again; when the
//! host refuses it, it stops, because a guest built from a different protocol definition
//! cannot become acceptable by retrying.
//!
//! The connection half and the routing ([`router`]) are independent of how the guest's
//! users reach the daemon, so they run and are tested on any Linux, over the in-memory
//! transport and over vsock. The binder service (`binder`, Android only) is the thin layer
//! that lets those users in; [`aidl`] holds the values it shares with their generated code.

pub mod aidl;
#[cfg(target_os = "android")]
pub mod binder;
pub mod router;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{Hello, RejectReason};
use dsa_protocol::handshake::{guest_connect, HandshakeError};
use dsa_transport::{Connection, Connector, TransportError};

/// How the daemon paces its attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// The wait after the first failed attempt. Each further failure doubles it.
    pub first_delay: Duration,
    /// The longest wait between attempts.
    pub max_delay: Duration,
    /// How long one attempt may take, from connecting to the host's answer.
    pub attempt_timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            first_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(5),
            attempt_timeout: Duration::from_secs(10),
        }
    }
}

/// What the daemon shows the host: this build's `Hello`, and the secret the host gave this
/// boot, which answers the host's challenge.
#[derive(Debug, Clone)]
pub struct Credentials {
    pub hello: Hello,
    pub secret: BootSecret,
}

/// Why the daemon stopped trying.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelError {
    /// The host refused this guest. Retrying cannot help.
    Refused {
        reason: RejectReason,
        detail: String,
    },
    /// Asked to stop.
    Stopped,
}

impl std::fmt::Display for ChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelError::Refused { detail, .. } => {
                write!(f, "the host refused this guest: {detail}")
            }
            ChannelError::Stopped => write!(f, "stopped"),
        }
    }
}

impl std::error::Error for ChannelError {}

/// Sleeps for `delay`, or less if `stop` is set meanwhile. Returns whether to go on.
fn pause(delay: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + delay;
    while Instant::now() < deadline {
        if stop.load(Ordering::Relaxed) {
            return false;
        }
        thread::sleep(Duration::from_millis(10).min(deadline - Instant::now()));
    }
    !stop.load(Ordering::Relaxed)
}

/// Connects and passes the handshake, retrying transport failures with backoff until it
/// succeeds, the host refuses, or `stop` is set. Returns the connection and the host's
/// `Hello`.
pub fn connect(
    connector: &mut dyn Connector,
    credentials: &Credentials,
    policy: RetryPolicy,
    stop: &AtomicBool,
) -> Result<(Box<dyn Connection>, Hello), ChannelError> {
    let mut delay = policy.first_delay;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(ChannelError::Stopped);
        }
        let attempt = connector
            .connect(policy.attempt_timeout)
            .map_err(HandshakeError::Transport)
            .and_then(|mut connection| {
                guest_connect(
                    connection.as_mut(),
                    &credentials.hello,
                    &credentials.secret,
                    policy.attempt_timeout,
                )
                .map(|host| (connection, host))
            });
        match attempt {
            Ok(connected) => return Ok(connected),
            Err(HandshakeError::Rejected { reason, detail }) => {
                return Err(ChannelError::Refused { reason, detail });
            }
            // The host is not up yet, or the VM's sockets are not ready: try again.
            Err(HandshakeError::Transport(_)) | Err(HandshakeError::Malformed(_)) => {
                if !pause(delay, stop) {
                    return Err(ChannelError::Stopped);
                }
                delay = (delay * 2).min(policy.max_delay);
            }
        }
    }
}

/// Carries frames over one connection until it is lost or `stop` is set: frames from the
/// host go to `to_guest`, frames on `from_guest` go to the host. Returns why it ended.
pub fn pump(
    connection: &mut dyn Connection,
    to_guest: &Sender<Vec<u8>>,
    from_guest: &Receiver<Vec<u8>>,
    stop: &AtomicBool,
) -> TransportError {
    const POLL: Duration = Duration::from_millis(20);
    loop {
        if stop.load(Ordering::Relaxed) {
            connection.close();
            return TransportError::Closed;
        }
        loop {
            match from_guest.recv_timeout(Duration::ZERO) {
                Ok(frame) => {
                    if let Err(error) = connection.send(&frame) {
                        return error;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                // Nobody in the guest can send any more; keep delivering the host's frames.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        match connection.recv(Some(POLL)) {
            Ok(frame) => {
                // A guest side that has gone away just stops receiving.
                let _ = to_guest.send(frame);
            }
            Err(TransportError::Timeout) => {}
            Err(error) => return error,
        }
    }
}

/// What the daemon reports as it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelEvent {
    /// Connected and accepted; carries the host's build.
    Connected { host_build: String },
    /// The connection was lost; the daemon connects again.
    Lost(TransportError),
}

/// Runs the daemon: connect, carry frames, and connect again whenever the connection is
/// lost, until the host refuses this guest or `stop` is set.
pub fn run(
    connector: &mut dyn Connector,
    credentials: &Credentials,
    policy: RetryPolicy,
    to_guest: &Sender<Vec<u8>>,
    from_guest: &Receiver<Vec<u8>>,
    events: &Sender<ChannelEvent>,
    stop: &AtomicBool,
) -> ChannelError {
    loop {
        let (mut connection, host) = match connect(connector, credentials, policy, stop) {
            Ok(connected) => connected,
            Err(error) => return error,
        };
        let _ = events.send(ChannelEvent::Connected {
            host_build: host.build,
        });
        let lost = pump(connection.as_mut(), to_guest, from_guest, stop);
        if stop.load(Ordering::Relaxed) {
            return ChannelError::Stopped;
        }
        let _ = events.send(ChannelEvent::Lost(lost));
    }
}
