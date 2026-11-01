//! Pairing the host's clock with the guest's: a real session over the in-memory transport
//! probes the guest's daemon while the input trace is on, and logs each answer.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::clock_trace::{ClockTrace, HostClock};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::memory;
use dsa_transport::{Connection, Connector, TransportError};

const WAIT: Duration = Duration::from_secs(10);

#[derive(Clone, Default)]
struct Guest(Arc<Mutex<Option<BootSecret>>>);

impl GuestLauncher for Guest {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        *self.0.lock().unwrap() = Some(secret.clone());
        Ok(())
    }
    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        *self.0.lock().unwrap() = None;
        Ok(())
    }
}

/// A host clock that moves on 2 ms each time it is read, from 1000 ms.
struct SteppingClock(AtomicU64);

impl HostClock for SteppingClock {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.fetch_add(2, Ordering::SeqCst))
    }
}

struct Setup {
    session: Session,
    guest: Box<dyn Connection>,
    lines: Arc<Mutex<Vec<String>>>,
}

fn setup(trace_on: bool) -> Setup {
    let launcher = Guest::default();
    let (listener, mut control) = memory::pair();
    let session = Session::new(
        launcher.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            ..SessionConfig::default()
        },
    );
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    let mut trace = ClockTrace::new(
        Arc::new(AtomicBool::new(trace_on)),
        Arc::new(SteppingClock(AtomicU64::new(1000))),
        Arc::new(move |line: &str| sink.lock().unwrap().push(line.to_string())),
    );
    trace.interval = Duration::from_millis(50);
    session.set_clock_trace(Some(trace));
    session.start();
    let deadline = Instant::now() + WAIT;
    let secret = loop {
        if let Some(secret) = launcher.0.lock().unwrap().clone() {
            break secret;
        }
        assert!(Instant::now() < deadline, "the guest was never started");
        thread::sleep(Duration::from_millis(5));
    };
    let mut guest = control.connect(WAIT).expect("the daemon connects");
    guest_connect(
        guest.as_mut(),
        &local_hello(Role::Guest, "dsa-channeld test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the daemon");
    guest
        .send(&dsa_protocol::encode(&Envelope {
            body: Some(Body::GuestBoot(control::GuestBoot {
                boot_completed: true,
                agent_connected: true,
            })),
        }))
        .unwrap();
    assert!(session.wait_for(SessionState::Active, WAIT));
    Setup {
        session,
        guest,
        lines,
    }
}

/// The next clock probe the guest receives within `within`, skipping other messages.
fn next_probe(guest: &mut Box<dyn Connection>, within: Duration) -> Option<control::ClockProbe> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        match guest.recv(Some(Duration::from_millis(20))) {
            Ok(frame) => {
                if let Some(Body::ClockProbe(probe)) = dsa_protocol::decode(&frame).unwrap().body
                {
                    return Some(probe);
                }
            }
            Err(TransportError::Timeout) => {}
            Err(error) => panic!("the channel broke: {error}"),
        }
    }
    None
}

#[test]
fn fr5_while_the_trace_is_on_the_session_probes_the_guests_clock_and_logs_each_answer() {
    let mut setup = setup(true);
    let probe = next_probe(&mut setup.guest, WAIT).expect("a probe arrives");
    assert!(probe.host_sent_ns >= 1_000_000_000);
    setup
        .guest
        .send(&dsa_protocol::encode(&Envelope {
            body: Some(Body::ClockAnswer(control::ClockAnswer {
                host_sent_ns: probe.host_sent_ns,
                guest_mono_ns: 500_000_000,
            })),
        }))
        .unwrap();
    let deadline = Instant::now() + WAIT;
    let line = loop {
        if let Some(line) = setup.lines.lock().unwrap().first().cloned() {
            break line;
        }
        assert!(Instant::now() < deadline, "no clock line was logged");
        thread::sleep(Duration::from_millis(5));
    };
    let sent = probe.host_sent_ns as f64 / 1e6;
    assert!(line.starts_with("input-trace stage=clock task=0 "), "{line}");
    assert!(
        line.contains(&format!(" host_sent={sent:.3} guest=500.000 ")),
        "{line}"
    );
    // The clock moves on 2 ms a read, and nothing else reads it between a probe and its
    // answer's arrival, unless another probe went out meanwhile; either way the round trip
    // is a whole number of steps and the offset is from its midpoint.
    let field = |name: &str| -> f64 {
        line.split(' ')
            .find_map(|part| part.strip_prefix(&format!("{name}=")))
            .unwrap()
            .parse()
            .unwrap()
    };
    let (received, rtt, offset) = (field("host_received"), field("rtt"), field("offset"));
    assert!(rtt >= 2.0, "{line}");
    assert!((received - sent - rtt).abs() < 1e-6, "{line}");
    assert!((offset - (500.0 - (sent + received) / 2.0)).abs() < 1e-6, "{line}");
    // The probes go on.
    assert!(next_probe(&mut setup.guest, WAIT).is_some());
    setup.session.stop();
}

#[test]
fn fr5_while_the_trace_is_off_the_session_sends_no_probe() {
    let mut setup = setup(false);
    assert_eq!(next_probe(&mut setup.guest, Duration::from_millis(500)), None);
    assert!(setup.lines.lock().unwrap().is_empty());
    setup.session.stop();
}
