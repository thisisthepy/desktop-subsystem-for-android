//! Session start and stop, through the session's public API, over the in-memory transport.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::session::{
    GuestLauncher, Session, SessionConfig, SessionError, SessionEvent, SessionState, StopReason,
};
use dsa_protocol::control::Role;
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
        Some(StopReason::AgentDidNotConnect { waited }) => assert!(waited >= timeout),
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
fn fr2_frames_flow_both_ways_while_active() {
    let (session, mut connector) = session_with(FakeLauncher::default(), WAIT);
    let incoming = session.incoming();
    assert_eq!(
        session.send(b"too early".to_vec()),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
    session.start();
    let mut agent = connect_agent(&mut connector);
    assert!(session.wait_for(SessionState::Active, WAIT));

    agent.send(b"from the agent").unwrap();
    assert_eq!(incoming.recv_timeout(WAIT).unwrap(), b"from the agent");
    session.send(b"from the host".to_vec()).unwrap();
    assert_eq!(agent.recv(Some(WAIT)).unwrap(), b"from the host");
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
