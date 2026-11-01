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
//! [`reclaim`] decides when the guest drops its caches so the host gets the memory back.
//!
//! The daemon answers the host's clock probes itself ([`clock_answer`]), from the thread that
//! reads the connection, so the answer carries the guest's clock as close to the probe's
//! arrival as the daemon can read it.

pub mod aidl;
#[cfg(target_os = "android")]
pub mod binder;
pub mod reclaim;
pub mod router;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{ClockAnswer, Envelope, Hello, RejectReason};
use dsa_protocol::handshake::{guest_connect, HandshakeError};
use dsa_transport::{Connection, Connector, FrameSender, TransportError};

/// The guest's `CLOCK_MONOTONIC`, the clock the guest's trace lines carry as `mono`.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub fn guest_monotonic() -> Duration {
    use std::ffi::{c_int, c_long};
    #[repr(C)]
    struct Timespec {
        tv_sec: c_long,
        tv_nsec: c_long,
    }
    extern "C" {
        fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
    }
    const CLOCK_MONOTONIC: c_int = 1;
    let mut time = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: the call writes one timespec, which lives on this stack frame.
    if unsafe { clock_gettime(CLOCK_MONOTONIC, &mut time) } != 0 {
        return Duration::ZERO;
    }
    Duration::new(time.tv_sec.max(0) as u64, time.tv_nsec.clamp(0, 999_999_999) as u32)
}

/// Off Linux there is no guest clock to pair; the time since the daemon first looked stands
/// in for it.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub fn guest_monotonic() -> Duration {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// The daemon's answer when `frame` is the host's clock probe: the probe's send time and
/// `guest_now`, the guest's clock when the probe was read. `None` for any other frame, which
/// goes on to the guest's clients as before.
pub fn clock_answer(frame: &[u8], guest_now: Duration) -> Option<Vec<u8>> {
    let Ok(Envelope {
        body: Some(Body::ClockProbe(probe)),
    }) = dsa_protocol::decode(frame)
    else {
        return None;
    };
    Some(dsa_protocol::encode(&Envelope {
        body: Some(Body::ClockAnswer(ClockAnswer {
            host_sent_ns: probe.host_sent_ns,
            guest_mono_ns: guest_now.as_nanos() as u64,
        })),
    }))
}

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

/// How long the threads of [`pump`] wait before looking whether they should stop.
const POLL: Duration = Duration::from_millis(20);

/// Carries frames over one connection until it is lost or `stop` is set: frames from the
/// host go to `to_guest`, frames on `from_guest` go to the host. Returns why it ended.
///
/// Each direction has a thread, so a frame for the host goes the moment it is handed over
/// rather than when the wait for the host's next frame times out.
pub fn pump(
    connection: &mut dyn Connection,
    to_guest: &Sender<Vec<u8>>,
    from_guest: &Receiver<Vec<u8>>,
    stop: &AtomicBool,
) -> TransportError {
    let Some(sender) = connection.try_clone_sender() else {
        return pump_in_turns(connection, to_guest, from_guest, stop);
    };
    // Shared with the reader, which answers clock probes on it; the lock keeps the two
    // threads' frames whole.
    let sender = Mutex::new(sender);
    let ended = AtomicBool::new(false);
    let send_failed = AtomicBool::new(false);
    thread::scope(|scope| {
        let reader = scope.spawn(|| {
            let why = read_until_lost(connection, to_guest, stop, &send_failed, &sender);
            ended.store(true, Ordering::Relaxed);
            why
        });
        let mut send_error = None;
        while !ended.load(Ordering::Relaxed) {
            match from_guest.recv_timeout(POLL) {
                Ok(frame) => {
                    let sent = sender
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .send(&frame);
                    if let Err(error) = sent {
                        send_error = Some(error);
                        send_failed.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                // Nobody in the guest can send any more; keep delivering the host's frames.
                Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL),
            }
        }
        let read_error = reader.join().unwrap_or(TransportError::Closed);
        send_error.unwrap_or(read_error)
    })
}

fn read_until_lost(
    connection: &mut dyn Connection,
    to_guest: &Sender<Vec<u8>>,
    stop: &AtomicBool,
    send_failed: &AtomicBool,
    sender: &Mutex<Box<dyn FrameSender>>,
) -> TransportError {
    loop {
        if stop.load(Ordering::Relaxed) || send_failed.load(Ordering::Relaxed) {
            connection.close();
            return TransportError::Closed;
        }
        match connection.recv(Some(POLL)) {
            Ok(frame) => {
                if let Some(answer) = clock_answer(&frame, guest_monotonic()) {
                    // A failed send shows up on the sending thread's next frame.
                    let _ = sender
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .send(&answer);
                    continue;
                }
                // A guest side that has gone away just stops receiving.
                let _ = to_guest.send(frame);
            }
            Err(TransportError::Timeout) => {}
            Err(error) => return error,
        }
    }
}

/// [`pump`] on a transport that cannot share its connection between two threads.
fn pump_in_turns(
    connection: &mut dyn Connection,
    to_guest: &Sender<Vec<u8>>,
    from_guest: &Receiver<Vec<u8>>,
    stop: &AtomicBool,
) -> TransportError {
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
                if let Some(answer) = clock_answer(&frame, guest_monotonic()) {
                    if let Err(error) = connection.send(&answer) {
                        return error;
                    }
                    continue;
                }
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
