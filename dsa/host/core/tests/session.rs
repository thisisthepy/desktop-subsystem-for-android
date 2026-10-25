//! Session start and stop, through the session's public API, over the in-memory transport.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::session::{
    GuestLauncher, Session, SessionConfig, SessionError, SessionEvent, SessionState, StopReason,
};
use dsa_protocol::control::{envelope::Body, Envelope, Role};
use dsa_protocol::encode;
use dsa_protocol::handshake::{guest_connect, local_hello, HandshakeError};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, TransportError};

const WAIT: Duration = Duration::from_secs(10);

/// A guest whose "processes" are a counter: three while running, none once stopped.
#[derive(Default)]
struct Guest {
    processes: usize,
    starts: usize,
    fail_start: Option<String>,
    fail_stop: Option<String>,
    /// Set to make the guest end by itself, as a VM that crashes during boot does.
    exited: Option<String>,
    /// What the launcher tells about the boot when a start fails.
    diagnostics: Option<String>,
    /// How many times the session asked for the diagnostics, and whether the guest was
    /// already stopped each time.
    diagnostics_asked: Vec<bool>,
}

#[derive(Clone, Default)]
struct FakeLauncher(Arc<Mutex<Guest>>);

impl FakeLauncher {
    fn processes(&self) -> usize {
        self.0.lock().unwrap().processes
    }
}

impl GuestLauncher for FakeLauncher {
    fn start(&mut self) -> Result<(), String> {
        let mut guest = self.0.lock().unwrap();
        guest.starts += 1;
        if let Some(why) = guest.fail_start.clone() {
            return Err(why);
        }
        guest.processes = 3;
        Ok(())
    }

    fn stop(&mut self, _timeout: Duration) -> Result<(), String> {
        // Processes end one at a time, so a premature Stopped would see some still running.
        loop {
            {
                let mut guest = self.0.lock().unwrap();
                if let Some(why) = guest.fail_stop.clone() {
                    return Err(why);
                }
                if guest.processes == 0 {
                    return Ok(());
                }
                guest.processes -= 1;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn exited(&mut self) -> Option<String> {
        self.0.lock().unwrap().exited.clone()
    }

    fn diagnostics(&mut self) -> Option<String> {
        let mut guest = self.0.lock().unwrap();
        let stopped = guest.processes == 0;
        guest.diagnostics_asked.push(stopped);
        guest.diagnostics.clone()
    }

    fn vm_id(&self) -> Option<String> {
        (self.0.lock().unwrap().processes > 0).then(|| "fake-vm".into())
    }
}

/// A session that checks the agent with the real control protocol handshake.
fn session_with(launcher: FakeLauncher, connect_timeout: Duration) -> (Session, MemoryConnector) {
    let handshake = ControlHandshake::new("dsa-host test", WAIT);
    let (listener, connector) = memory::pair();
    let config = SessionConfig {
        connect_timeout,
        stop_timeout: WAIT,
    };
    (
        Session::new(launcher, Box::new(listener), handshake, config),
        connector,
    )
}

fn next_states(events: &Receiver<SessionEvent>, count: usize) -> Vec<SessionEvent> {
    (0..count)
        .map(|_| events.recv_timeout(WAIT).expect("a state change"))
        .collect()
}

fn states(events: &[SessionEvent]) -> Vec<SessionState> {
    events.iter().map(|event| event.state).collect()
}

/// Connects as the guest's channel daemon would: the transport, then the handshake.
fn connect_agent(connector: &mut MemoryConnector) -> Box<dyn Connection> {
    let mut agent = connector.connect(WAIT).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        WAIT,
    )
    .expect("the host accepts the agent");
    agent
}

#[test]
fn fr2_a_new_session_is_stopped() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(session.state(), SessionState::Stopped);
    assert_eq!(session.last_reason(), None);
}

/// The compositor binds its Hyper-V sockets to the VM's ID, and the guest renders before
/// the agent connects, so the ID is known as soon as the guest is running.
#[test]
fn fr3_the_vm_id_is_known_while_the_guest_boots() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    session.start();
    assert!(session.wait_for(SessionState::Starting, WAIT));
    let deadline = std::time::Instant::now() + WAIT;
    while session.vm_id().is_none() && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(session.vm_id().as_deref(), Some("fake-vm"));
    assert_eq!(session.state(), SessionState::Starting);
}

#[test]
fn fr2_the_session_is_active_once_the_agent_connects() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let events = session.events();
    session.start();
    assert!(session.wait_for(SessionState::Starting, WAIT));

    let _agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(launcher.processes(), 3, "the guest was started");
    assert_eq!(
        states(&next_states(&events, 2)),
        [SessionState::Starting, SessionState::Active]
    );
}

#[test]
fn fr2_an_agent_that_never_connects_returns_the_session_to_stopped_with_the_cause() {
    let launcher = FakeLauncher::default();
    let timeout = Duration::from_millis(200);
    let (session, _connector) = session_with(launcher.clone(), timeout);
    let events = session.events();
    session.start();

    let seen = next_states(&events, 3);
    assert_eq!(
        states(&seen),
        [
            SessionState::Starting,
            SessionState::Stopping,
            SessionState::Stopped
        ]
    );
    match seen[2].reason.clone() {
        Some(StopReason::AgentDidNotConnect { waited, .. }) => assert!(waited >= timeout),
        other => panic!("expected AgentDidNotConnect, got {other:?}"),
    }
    assert_eq!(session.last_reason(), seen[2].reason);
    assert_eq!(
        launcher.processes(),
        0,
        "the half-started guest was stopped"
    );
}

#[test]
fn fr2_stop_reports_stopped_only_after_every_guest_process_has_ended() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let _agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    let events = session.events();
    session.stop();
    let seen = next_states(&events, 2);
    assert_eq!(
        states(&seen),
        [SessionState::Stopping, SessionState::Stopped]
    );
    assert_eq!(launcher.processes(), 0);
    assert_eq!(seen[1].reason, Some(StopReason::Requested));
}

#[test]
fn fr2_a_stop_that_leaves_processes_running_is_not_reported_as_stopped() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let _agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    launcher.0.lock().unwrap().fail_stop = Some("lxc-stop timed out".into());
    let events = session.events();
    session.stop();
    let seen = next_states(&events, 2);
    assert_eq!(
        states(&seen),
        [SessionState::Stopping, SessionState::Stopping]
    );
    assert_eq!(
        seen[1].reason,
        Some(StopReason::StopFailed("lxc-stop timed out".into()))
    );
    assert_eq!(session.state(), SessionState::Stopping);

    // Once the guest can be stopped, asking again finishes the job.
    launcher.0.lock().unwrap().fail_stop = None;
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(launcher.processes(), 0);
}

#[test]
fn fr2_a_guest_that_fails_to_start_is_reported_with_the_cause() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().fail_start = Some("the distribution is not installed".into());
    let (session, _connector) = session_with(launcher, WAIT);
    let events = session.events();
    session.start();
    let seen = next_states(&events, 2);
    assert_eq!(
        states(&seen),
        [SessionState::Starting, SessionState::Stopped]
    );
    assert_eq!(
        seen[1].reason,
        Some(StopReason::GuestFailedToStart(
            "the distribution is not installed".into()
        ))
    );
}

#[test]
fn fr2_an_agent_from_a_different_protocol_definition_is_refused_and_the_guest_stopped() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let events = session.events();
    session.start();
    let mut agent = connector.connect(WAIT).expect("the agent connects");
    let mut hello = local_hello(Role::Guest, "dsa-guestd other");
    hello.schema_hash = vec![0x5a; 32];
    let refusal = guest_connect(agent.as_mut(), &hello, WAIT);
    assert!(
        matches!(refusal, Err(HandshakeError::Rejected { .. })),
        "the agent is told it was refused: {refusal:?}"
    );

    let seen = next_states(&events, 3);
    assert_eq!(
        states(&seen),
        [
            SessionState::Starting,
            SessionState::Stopping,
            SessionState::Stopped
        ]
    );
    match &seen[2].reason {
        Some(StopReason::HandshakeRejected(why)) => {
            assert!(why.contains("different protocol definition"), "{why}")
        }
        other => panic!("expected HandshakeRejected, got {other:?}"),
    }
    assert_eq!(launcher.processes(), 0);
    assert_eq!(agent.recv(Some(WAIT)), Err(TransportError::Closed));
}

#[test]
fn fr2_stop_while_starting_stops_the_guest() {
    let launcher = FakeLauncher::default();
    let (session, _connector) = session_with(launcher.clone(), WAIT);
    session.start();
    assert!(session.wait_for(SessionState::Starting, WAIT));
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.last_reason(), Some(StopReason::Requested));
    assert_eq!(launcher.processes(), 0);
}

#[test]
fn fr2_a_lost_control_channel_stops_the_session_and_says_so() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    drop(agent);
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert!(matches!(
        session.last_reason(),
        Some(StopReason::ChannelLost(_))
    ));
    assert_eq!(launcher.processes(), 0);
}

#[test]
fn fr2_a_stopped_session_can_start_again() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    for _ in 0..2 {
        session.start();
        let _agent = connect_agent(&mut connector);
        assert!(session.wait_for(SessionState::Active, WAIT));
        session.stop();
        assert!(session.wait_for(SessionState::Stopped, WAIT));
    }
    assert_eq!(launcher.0.lock().unwrap().starts, 2);
}

#[test]
fn fr2_the_host_can_send_frames_while_active() {
    let (session, mut connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.send(b"too early".to_vec()),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
    session.start();
    let mut agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    session.send(b"from the host".to_vec()).unwrap();
    assert_eq!(agent.recv(Some(WAIT)).unwrap(), b"from the host");
}

#[test]
fn nfr5_a_malformed_control_frame_is_dropped_and_the_session_stays_active() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // A malformed frame arrives after handshake: it is discarded and the session stays active.
    agent.send(b"not a valid protobuf envelope").unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(session.state(), SessionState::Active);

    // Host can still send to the agent, and clean shutdown works.
    session.send(b"from the host".to_vec()).unwrap();
    assert_eq!(agent.recv(Some(WAIT)).unwrap(), b"from the host");

    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.last_reason(), Some(StopReason::Requested));
}

#[test]
fn nfr5_an_unexpected_message_is_dropped_and_logged() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // A Hello after handshake has already completed is unexpected.
    let hello = local_hello(Role::Guest, "dsa-guestd test");
    let frame = encode(&Envelope {
        body: Some(Body::Hello(hello)),
    });
    agent.send(&frame).unwrap();

    // An empty envelope with no body is also dropped as unknown.
    let empty = encode(&Envelope { body: None });
    agent.send(&empty).unwrap();

    thread::sleep(Duration::from_millis(50));
    assert_eq!(session.state(), SessionState::Active);

    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.last_reason(), Some(StopReason::Requested));
}

#[test]
fn nfr5_a_guest_that_dies_leaves_the_host_alive_and_can_start_again() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // Guest VM dies and drops its channel.
    drop(agent);
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert!(matches!(
        session.last_reason(),
        Some(StopReason::ChannelLost(_))
    ));
    assert_eq!(launcher.processes(), 0);

    // Host remains alive and the session can be started again.
    session.start();
    let _second_agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(launcher.processes(), 3);
}

#[test]
fn fr2_dropping_an_active_session_stops_the_guest() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let _agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));
    drop(session);
    assert_eq!(launcher.processes(), 0);
}

#[test]
fn pr5_a_start_through_the_local_api_answers_after_the_session_has_moved() {
    use dsa_host_core::host_api::UserAndroid;
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().fail_start = Some("no kernel".into());
    let (session, _connector) = session_with(launcher, WAIT);
    // A failed start that happened before the answer shows in the answer, so a client
    // never mistakes the state before the start for its result.
    UserAndroid::start(&session);
    let state = session.state();
    let reason = UserAndroid::last_reason(&session).unwrap_or_default();
    assert!(
        state != SessionState::Stopped || reason.contains("no kernel"),
        "the answer shows the state from before the start: {state}, {reason:?}"
    );
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert!(UserAndroid::last_reason(&session)
        .unwrap()
        .contains("no kernel"));
}

#[test]
fn fr2_a_start_that_times_out_reports_what_the_launcher_saw_of_the_boot() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().diagnostics =
        Some("the guest's last console lines:\n  Kernel panic - not syncing: VFS".into());
    let timeout = Duration::from_millis(200);
    let (session, _connector) = session_with(launcher.clone(), timeout);
    session.start();
    assert!(session.wait_for(SessionState::Starting, WAIT));
    assert!(session.wait_for(SessionState::Stopped, WAIT));

    let reason = session.last_reason().expect("a reason");
    match &reason {
        StopReason::AgentDidNotConnect { diagnostics, .. } => assert_eq!(
            diagnostics.as_deref(),
            Some("the guest's last console lines:\n  Kernel panic - not syncing: VFS")
        ),
        other => panic!("expected AgentDidNotConnect, got {other:?}"),
    }
    let text = reason.to_string();
    assert!(
        text.starts_with("the agent did not connect to the control channel within"),
        "{text}"
    );
    assert!(
        text.ends_with("\nthe guest's last console lines:\n  Kernel panic - not syncing: VFS"),
        "{text}"
    );
    assert_eq!(
        launcher.0.lock().unwrap().diagnostics_asked,
        [true],
        "asked once, after the guest had stopped"
    );
}

#[test]
fn fr2_a_guest_that_exits_during_boot_ends_the_start_without_waiting_for_the_timeout() {
    let launcher = FakeLauncher::default();
    {
        let mut guest = launcher.0.lock().unwrap();
        guest.exited = Some("the guest exited during boot".into());
        guest.diagnostics = Some("last line: init: reboot".into());
    }
    let (session, _connector) = session_with(launcher.clone(), Duration::from_secs(600));
    let events = session.events();
    let asked = std::time::Instant::now();
    session.start();

    let seen = next_states(&events, 3);
    assert_eq!(
        states(&seen),
        [
            SessionState::Starting,
            SessionState::Stopping,
            SessionState::Stopped
        ]
    );
    assert!(
        asked.elapsed() < WAIT,
        "the start ended long before the connect timeout"
    );
    assert_eq!(
        seen[2].reason,
        Some(StopReason::GuestExited {
            why: "the guest exited during boot".into(),
            diagnostics: Some("last line: init: reboot".into()),
        })
    );
    assert_eq!(
        seen[2].reason.as_ref().unwrap().to_string(),
        "the guest exited during boot\nlast line: init: reboot"
    );
    assert_eq!(
        launcher.processes(),
        0,
        "what was left of the guest was stopped"
    );
}

#[test]
fn fr2_a_stop_on_request_does_not_ask_for_boot_diagnostics() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().diagnostics = Some("not wanted".into());
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let _agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.last_reason(), Some(StopReason::Requested));
    assert!(launcher.0.lock().unwrap().diagnostics_asked.is_empty());
}
