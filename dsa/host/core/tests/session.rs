//! Session start and stop, through the session's public API, over the in-memory transport.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::ime::{ImeCursor, ImeEvent};
use dsa_host_core::input::{buttons, PointerAction, PointerEvent, PointerTool};
use dsa_host_core::keys::{keycode, meta, KeyAction, KeyEvent};
use dsa_host_core::power::{PowerHub, PowerState};
use dsa_host_core::session::{
    GuestLauncher, Session, SessionConfig, SessionError, SessionEvent, SessionState, StopReason,
};
use dsa_host_core::tasks::{Bounds, Task, TaskEvent, TaskRequest};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{envelope::Body, Envelope, RejectReason, Role};
use dsa_protocol::encode;
use dsa_protocol::handshake::{guest_connect, local_hello, HandshakeError};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::stream::{accept_with_timeout, FramedConnection, StreamSocket};
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
    /// The secret each start handed the guest, in order.
    secrets: Vec<BootSecret>,
    /// The running boot's secret, as only the guest's daemon would know it.
    secret: Option<BootSecret>,
    /// Whether the guest's processes are frozen, and how many times they were frozen and
    /// thawed.
    frozen: bool,
    freezes: usize,
    thaws: usize,
    /// Set to make freezing fail, as on a host that cannot pause the guest.
    cannot_freeze: Option<String>,
}

#[derive(Clone, Default)]
struct FakeLauncher(Arc<Mutex<Guest>>);

impl FakeLauncher {
    fn processes(&self) -> usize {
        self.0.lock().unwrap().processes
    }

    /// Waits for the running boot's secret, which the session hands the launcher as it starts.
    fn secret(&self) -> BootSecret {
        let deadline = std::time::Instant::now() + WAIT;
        loop {
            if let Some(secret) = self.0.lock().unwrap().secret.clone() {
                return secret;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the guest was never started"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl GuestLauncher for FakeLauncher {
    fn start(&mut self, secret: &BootSecret) -> Result<(), String> {
        let mut guest = self.0.lock().unwrap();
        guest.starts += 1;
        guest.secrets.push(secret.clone());
        if let Some(why) = guest.fail_start.clone() {
            return Err(why);
        }
        guest.processes = 3;
        guest.secret = Some(secret.clone());
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
                    guest.secret = None;
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

    fn freeze(&mut self) -> Result<(), String> {
        let mut guest = self.0.lock().unwrap();
        if let Some(why) = guest.cannot_freeze.clone() {
            return Err(why);
        }
        guest.frozen = true;
        guest.freezes += 1;
        Ok(())
    }

    fn thaw(&mut self) -> Result<(), String> {
        let mut guest = self.0.lock().unwrap();
        guest.frozen = false;
        guest.thaws += 1;
        Ok(())
    }
}

/// A session that checks the agent with the real control protocol handshake.
fn session_with(launcher: FakeLauncher, connect_timeout: Duration) -> (Session, MemoryConnector) {
    let handshake = ControlHandshake::new("dsa-host test", WAIT);
    let (listener, connector) = memory::pair();
    let config = SessionConfig {
        connect_timeout,
        stop_timeout: WAIT,
        ..SessionConfig::default()
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

/// Connects as the guest's channel daemon would: the transport, then the handshake with the
/// secret the launcher gave this boot.
fn connect_agent(connector: &mut MemoryConnector, launcher: &FakeLauncher) -> Box<dyn Connection> {
    let secret = launcher.secret();
    let mut agent = connector.connect(WAIT).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &secret,
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

    let _agent = connect_agent(&mut connector, &launcher);
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
    let _agent = connect_agent(&mut connector, &launcher);
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
    let _agent = connect_agent(&mut connector, &launcher);
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
    let refusal = guest_connect(agent.as_mut(), &hello, &launcher.secret(), WAIT);
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
    let agent = connect_agent(&mut connector, &launcher);
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
        let _agent = connect_agent(&mut connector, &launcher);
        assert!(session.wait_for(SessionState::Active, WAIT));
        session.stop();
        assert!(session.wait_for(SessionState::Stopped, WAIT));
    }
    assert_eq!(launcher.0.lock().unwrap().starts, 2);
}

#[test]
fn fr2_the_host_can_send_frames_while_active() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    assert_eq!(
        session.send(b"too early".to_vec()),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    session.send(b"from the host".to_vec()).unwrap();
    assert_eq!(agent.recv(Some(WAIT)).unwrap(), b"from the host");
}

#[test]
fn nfr5_a_malformed_control_frame_is_dropped_and_the_session_stays_active() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
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
    let mut agent = connect_agent(&mut connector, &launcher);
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
    let agent = connect_agent(&mut connector, &launcher);
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
    let _second_agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(launcher.processes(), 3);
}

#[test]
fn fr2_dropping_an_active_session_stops_the_guest() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let _agent = connect_agent(&mut connector, &launcher);
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
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.last_reason(), Some(StopReason::Requested));
    assert!(launcher.0.lock().unwrap().diagnostics_asked.is_empty());
}

#[test]
fn pr6_each_start_hands_the_guest_a_new_secret() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let first = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    drop(first);

    session.start();
    let _second = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let secrets = launcher.0.lock().unwrap().secrets.clone();
    assert_eq!(secrets.len(), 2);
    assert_ne!(secrets[0], secrets[1]);
}

#[test]
fn pr6_a_connection_without_the_boot_secret_is_refused_and_the_daemon_still_gets_in() {
    // Any app in the guest can reach the control port. It must neither get in nor be able
    // to make the start fail.
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let real = launcher.secret();
    let mut impostor = connector.connect(WAIT).expect("the impostor connects");
    let refusal = guest_connect(
        impostor.as_mut(),
        &local_hello(Role::Guest, "an app"),
        &BootSecret::from_bytes([0x42; 32]),
        WAIT,
    );
    match refusal {
        Err(HandshakeError::Rejected { reason, .. }) => {
            assert_eq!(reason, RejectReason::AuthenticationFailed)
        }
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert_eq!(impostor.recv(Some(WAIT)), Err(TransportError::Closed));
    assert_eq!(session.state(), SessionState::Starting);

    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(launcher.secret(), real);
}

#[test]
fn pr6_the_previous_boots_secret_does_not_open_the_next_boot() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let old = launcher.secret();
    let agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    drop(agent);

    session.start();
    launcher.secret();
    let mut replay = connector.connect(WAIT).expect("connects");
    let refusal = guest_connect(
        replay.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &old,
        WAIT,
    );
    assert!(
        matches!(
            refusal,
            Err(HandshakeError::Rejected {
                reason: RejectReason::AuthenticationFailed,
                ..
            })
        ),
        "{refusal:?}"
    );
    assert_eq!(session.state(), SessionState::Starting);
}

fn send_body(agent: &mut Box<dyn Connection>, body: Body) {
    agent
        .send(&encode(&Envelope { body: Some(body) }))
        .expect("the agent sends");
}

fn task_updated(task_id: i32, label: &str) -> Body {
    Body::TaskUpdated(dsa_protocol::control::TaskUpdated {
        task_id,
        bounds: Some(dsa_protocol::control::Rect {
            left: 10,
            top: 20,
            right: 410,
            bottom: 820,
        }),
        package_name: "com.example.clock".into(),
        label: label.into(),
        focused: true,
        density_dpi: 240,
        max_width: 1920,
        max_height: 1080,
    })
}

fn clock(task_id: i32, label: &str) -> Task {
    Task {
        task_id,
        bounds: Bounds {
            left: 10,
            top: 20,
            right: 410,
            bottom: 820,
        },
        package_name: "com.example.clock".into(),
        label: label.into(),
        focused: true,
        density_dpi: 240,
        max_width: 1920,
        max_height: 1080,
    }
}

fn active_session() -> (Session, Box<dyn Connection>, FakeLauncher) {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    (session, agent, launcher)
}

#[test]
fn fr3_task_messages_from_the_guest_reach_a_watcher() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    assert!(watch.current.is_empty());

    send_body(&mut agent, task_updated(7, "Clock"));
    send_body(
        &mut agent,
        Body::TaskFrames(dsa_protocol::control::TaskFrames {
            task_id: 7,
            display_id: 2,
        }),
    );
    send_body(
        &mut agent,
        Body::TaskRemoved(dsa_protocol::control::TaskRemoved { task_id: 7 }),
    );

    let events: Vec<TaskEvent> = (0..3)
        .map(|_| watch.events.recv_timeout(WAIT).expect("a task event"))
        .collect();
    assert_eq!(
        events,
        vec![
            TaskEvent::Updated(clock(7, "Clock")),
            TaskEvent::Frames {
                task_id: 7,
                display_id: 2
            },
            TaskEvent::Removed { task_id: 7 },
        ]
    );
    assert!(session.tasks().is_empty());
}

#[test]
fn fr3_a_late_watcher_starts_from_the_tasks_that_exist_now() {
    let (session, mut agent, _launcher) = active_session();
    let early = session.watch_tasks();
    send_body(&mut agent, task_updated(3, "Clock"));
    send_body(&mut agent, task_updated(3, "Alarm"));
    send_body(
        &mut agent,
        Body::TaskFrames(dsa_protocol::control::TaskFrames {
            task_id: 3,
            display_id: 5,
        }),
    );
    for _ in 0..3 {
        early.events.recv_timeout(WAIT).expect("a task event");
    }

    let late = session.watch_tasks();
    assert_eq!(
        late.current,
        vec![
            TaskEvent::Updated(clock(3, "Alarm")),
            TaskEvent::Frames {
                task_id: 3,
                display_id: 5
            },
        ]
    );
    assert_eq!(session.tasks(), vec![clock(3, "Alarm")]);
}

#[test]
fn fr3_closing_a_task_and_launching_an_app_reach_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    session
        .request(TaskRequest::Close { task_id: 7 })
        .expect("the close is sent");
    session
        .request(TaskRequest::Launch {
            package_name: "com.example.clock".into(),
        })
        .expect("the launch is sent");

    let first = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        first.body,
        Some(Body::CloseTask(dsa_protocol::control::CloseTask {
            task_id: 7
        }))
    );
    let second = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        second.body,
        Some(Body::LaunchApp(dsa_protocol::control::LaunchApp {
            package_name: "com.example.clock".into()
        }))
    );
}

#[test]
fn fr4_resizing_and_rescaling_a_task_reach_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    session
        .request(TaskRequest::SetDensity {
            task_id: 7,
            density_dpi: 320,
        })
        .expect("the density is sent");
    session
        .request(TaskRequest::Resize {
            task_id: 7,
            width: 1096,
            height: 1948,
            live: false,
        })
        .expect("the resize is sent");

    let first = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        first.body,
        Some(Body::SetTaskDensity(
            dsa_protocol::control::SetTaskDensity {
                task_id: 7,
                density_dpi: 320
            }
        ))
    );
    let second = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        second.body,
        Some(Body::ResizeTask(dsa_protocol::control::ResizeTask {
            task_id: 7,
            width: 1096,
            height: 1948,
            live: false
        }))
    );
}

#[test]
fn fr4_a_resize_while_the_window_is_dragged_reaches_the_guest_as_live() {
    let (session, mut agent, _launcher) = active_session();
    for (width, live) in [(900, true), (960, true), (960, false)] {
        session
            .request(TaskRequest::Resize {
                task_id: 7,
                width,
                height: 700,
                live,
            })
            .expect("the resize is sent");
    }
    for (width, live) in [(900, true), (960, true), (960, false)] {
        let sent = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
        assert_eq!(
            sent.body,
            Some(Body::ResizeTask(dsa_protocol::control::ResizeTask {
                task_id: 7,
                width,
                height: 700,
                live
            }))
        );
    }
}

#[test]
fn fr3_a_request_while_the_guest_is_not_running_says_so() {
    let launcher = FakeLauncher::default();
    let (session, _connector) = session_with(launcher, WAIT);
    assert_eq!(
        session.request(TaskRequest::Launch {
            package_name: "com.example.clock".into()
        }),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

#[test]
fn nfr5_a_guest_that_dies_removes_every_task_it_had() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    send_body(&mut agent, task_updated(1, "Clock"));
    send_body(&mut agent, task_updated(2, "Maps"));
    for _ in 0..2 {
        watch.events.recv_timeout(WAIT).expect("a task event");
    }

    agent.close();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    let removed: Vec<TaskEvent> = (0..2)
        .map(|_| watch.events.recv_timeout(WAIT).expect("a removal"))
        .collect();
    assert_eq!(
        removed,
        vec![
            TaskEvent::Removed { task_id: 1 },
            TaskEvent::Removed { task_id: 2 }
        ]
    );
    assert!(session.tasks().is_empty());
    assert!(session.watch_tasks().current.is_empty());
}

#[test]
fn nfr5_a_repeated_task_message_is_handed_on_once() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    send_body(&mut agent, task_updated(1, "Clock"));
    send_body(&mut agent, task_updated(1, "Clock"));
    send_body(&mut agent, task_updated(1, "Clock 2"));
    assert_eq!(
        watch.events.recv_timeout(WAIT).unwrap(),
        TaskEvent::Updated(clock(1, "Clock"))
    );
    assert_eq!(
        watch.events.recv_timeout(WAIT).unwrap(),
        TaskEvent::Updated(clock(1, "Clock 2"))
    );
    assert!(watch
        .events
        .recv_timeout(Duration::from_millis(100))
        .is_err());
}

fn pointer_down(task_id: i32) -> PointerEvent {
    let mut event = PointerEvent::new(
        task_id,
        PointerAction::Down,
        PointerTool::Mouse,
        (12.5, 40.0),
    );
    event.buttons = buttons::SECONDARY;
    event.pressure = 1.0;
    event
}

#[test]
fn fr5_pointer_input_reaches_the_guest_as_one_message_per_event() {
    let (session, mut agent, _launcher) = active_session();
    let down = pointer_down(7);
    let mut up = down;
    up.action = PointerAction::Up;
    up.buttons = 0;
    session.pointer(&down).expect("the down is sent");
    session.pointer(&up).expect("the up is sent");
    for expected in [down, up] {
        let message = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
        assert_eq!(
            message.body,
            Some(Body::PointerEvent(expected.to_control()))
        );
    }
}

#[test]
fn fr5_pointer_input_while_the_guest_is_not_running_says_so() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.pointer(&pointer_down(7)),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

fn enter(task_id: i32, action: KeyAction) -> KeyEvent {
    KeyEvent {
        task_id,
        action,
        key_code: keycode::ENTER,
        repeat_count: 0,
        meta_state: meta::SHIFT_ON | meta::SHIFT_LEFT_ON,
        event_time_ms: 42,
        canceled: false,
    }
}

#[test]
fn fr16_key_input_reaches_the_guest_as_one_message_per_event() {
    let (session, mut agent, _launcher) = active_session();
    let down = enter(7, KeyAction::Down);
    let up = enter(7, KeyAction::Up);
    session.key(&down).expect("the down is sent");
    session.key(&up).expect("the up is sent");
    for expected in [down, up] {
        let message = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
        assert_eq!(message.body, Some(Body::KeyEvent(expected.to_control())));
    }
}

#[test]
fn fr16_key_input_while_the_guest_is_not_running_says_so() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.key(&enter(7, KeyAction::Down)),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

#[test]
fn fr6_text_input_reaches_the_guest_as_one_message_per_change() {
    let (session, mut agent, _launcher) = active_session();
    let composing = ImeEvent::Composition {
        task_id: 7,
        text: "\u{d55c}".into(),
        cursor: 1,
    };
    let commit = ImeEvent::Commit {
        task_id: 7,
        text: "\u{d55c}".into(),
    };
    session.ime(&composing).expect("the composition is sent");
    session.ime(&commit).expect("the commit is sent");
    for expected in [composing, commit] {
        let message = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
        assert_eq!(message.body, Some(expected.to_control()));
    }
}

#[test]
fn fr6_text_input_while_the_guest_is_not_running_says_so() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.ime(&ImeEvent::Commit {
            task_id: 7,
            text: "a".into()
        }),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

#[test]
fn fr6_the_cursor_of_a_tasks_editor_reaches_a_watcher() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    send_body(&mut agent, task_updated(7, "Clock"));
    let cursor = Body::ImeCursor(dsa_protocol::control::ImeCursor {
        task_id: 7,
        cursor: Some(dsa_protocol::control::Rect {
            left: 12,
            top: 30,
            right: 14,
            bottom: 60,
        }),
    });
    send_body(&mut agent, cursor.clone());
    // The same place again is not news.
    send_body(&mut agent, cursor);
    send_body(
        &mut agent,
        Body::TaskRemoved(dsa_protocol::control::TaskRemoved { task_id: 7 }),
    );
    let events: Vec<TaskEvent> = (0..3)
        .map(|_| watch.events.recv_timeout(WAIT).expect("a task event"))
        .collect();
    assert_eq!(
        events,
        vec![
            TaskEvent::Updated(clock(7, "Clock")),
            TaskEvent::ImeCursor {
                task_id: 7,
                cursor: ImeCursor {
                    left: 12,
                    top: 30,
                    right: 14,
                    bottom: 60,
                },
            },
            TaskEvent::Removed { task_id: 7 },
        ]
    );
}

/// A control channel over a loopback socket, which can share its socket between a reader
/// and a sender as hvsocket and vsock do; the in-memory one cannot.
struct Tcp(std::net::TcpStream);

impl std::io::Read for Tcp {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read(buf)
    }
}

impl std::io::Write for Tcp {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl StreamSocket for Tcp {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> std::io::Result<()> {
        self.0.set_read_timeout(timeout)
    }
    fn shutdown(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
    fn try_clone(&self) -> std::io::Result<Self> {
        self.0.try_clone().map(Tcp)
    }
}

struct TcpListener(std::net::TcpListener);

impl dsa_transport::Listener for TcpListener {
    fn accept(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        let (stream, _) = accept_with_timeout(timeout, || self.0.accept())?;
        stream.set_nonblocking(false)?;
        Ok(Box::new(FramedConnection::new(Tcp(stream))))
    }
}

#[test]
fn fr5_pointer_input_does_not_wait_for_the_session_to_stop_reading_the_channel() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.set_nonblocking(true).expect("nonblocking");
    let address = listener.local_addr().unwrap();
    let launcher = FakeLauncher::default();
    let session = Session::new(
        launcher.clone(),
        Box::new(TcpListener(listener)),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            ..SessionConfig::default()
        },
    );
    session.start();
    let secret = launcher.secret();
    let mut agent: Box<dyn Connection> = Box::new(FramedConnection::new(Tcp(
        std::net::TcpStream::connect(address).expect("the agent connects"),
    )));
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the agent");
    assert!(session.wait_for(SessionState::Active, WAIT));

    // The session waits on the channel for up to 20 ms at a time. An event that waited for
    // that would take a second for these; a mouse sends this many in well under one.
    let started = std::time::Instant::now();
    for _ in 0..100 {
        session.pointer(&pointer_down(7)).expect("sent");
    }
    let took = started.elapsed();
    for _ in 0..100 {
        let message = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
        assert_eq!(
            message.body,
            Some(Body::PointerEvent(pointer_down(7).to_control()))
        );
    }
    assert!(
        took < Duration::from_millis(400),
        "100 events took {took:?}"
    );
}

const DOZE_DELAY: Duration = Duration::from_millis(200);

/// An active session that lets the guest's screen go off after [`DOZE_DELAY`] with no
/// window shown, and freezes a dozing guest after `freeze_after` when set.
fn lifecycle_session(
    freeze_after: Option<Duration>,
) -> (Session, Box<dyn Connection>, FakeLauncher) {
    let launcher = FakeLauncher::default();
    let (listener, mut connector) = memory::pair();
    let session = Session::new(
        launcher.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            doze_delay: DOZE_DELAY,
            freeze_after,
        },
    );
    session.start();
    let agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    (session, agent, launcher)
}

fn next_body(agent: &mut Box<dyn Connection>) -> Body {
    let frame = agent.recv(Some(WAIT)).expect("a message for the guest");
    dsa_protocol::decode(&frame)
        .expect("a well-formed message")
        .body
        .expect("a message with a body")
}

fn screen(on: bool) -> Body {
    Body::ScreenState(dsa_protocol::control::ScreenState { on })
}

fn device_idle(idle: bool) -> Body {
    Body::DeviceIdle(dsa_protocol::control::DeviceIdle { idle })
}

/// Takes the session into Doze as the guest would: the screen goes off, and the agent says
/// the device is idle.
fn dozing(session: &Session, agent: &mut Box<dyn Connection>) {
    assert_eq!(next_body(agent), screen(false));
    send_body(agent, device_idle(true));
    assert!(session.wait_for(SessionState::Dozing, WAIT));
}

#[test]
fn fr10_with_every_window_closed_the_session_dozes_once_the_guest_is_idle() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    let started = std::time::Instant::now();
    // No window has been shown since the agent connected.
    assert_eq!(next_body(&mut agent), screen(false));
    assert!(
        started.elapsed() >= DOZE_DELAY,
        "the screen went off before the delay"
    );
    // The screen is off but the guest is not in Doze yet.
    assert_eq!(session.state(), SessionState::Active);
    send_body(&mut agent, device_idle(true));
    assert!(session.wait_for(SessionState::Dozing, WAIT));
}

#[test]
fn fr10_showing_a_window_while_dozing_makes_the_session_active_within_a_second() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    dozing(&session, &mut agent);

    let shown = std::time::Instant::now();
    session
        .request(TaskRequest::WindowsShown { shown: true })
        .expect("the word is taken");
    assert!(session.wait_for(SessionState::Active, Duration::from_secs(1)));
    assert!(shown.elapsed() < Duration::from_secs(1));
    assert_eq!(next_body(&mut agent), screen(true));
}

#[test]
fn fr10_a_shown_window_keeps_the_screen_on() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    session.windows_shown(true).unwrap();
    match agent.recv(Some(DOZE_DELAY * 3)) {
        Err(TransportError::Timeout) => {}
        other => panic!("expected nothing for the guest, got {other:?}"),
    }
    assert_eq!(session.state(), SessionState::Active);

    // Minimizing the last window starts the delay again.
    session.windows_shown(false).unwrap();
    let hidden = std::time::Instant::now();
    assert_eq!(next_body(&mut agent), screen(false));
    assert!(hidden.elapsed() >= DOZE_DELAY);
}

#[test]
fn fr10_the_word_on_shown_windows_is_kept_while_the_guest_is_stopped() {
    let launcher = FakeLauncher::default();
    let (listener, mut connector) = memory::pair();
    let session = Session::new(
        launcher.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            doze_delay: DOZE_DELAY,
            freeze_after: None,
        },
    );
    session
        .request(TaskRequest::WindowsShown { shown: true })
        .expect("taken while stopped");
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    match agent.recv(Some(DOZE_DELAY * 3)) {
        Err(TransportError::Timeout) => {}
        other => panic!("expected the screen to stay on, got {other:?}"),
    }
}

#[test]
fn fr10_the_guest_leaving_idle_for_maintenance_keeps_the_session_dozing() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    dozing(&session, &mut agent);
    send_body(&mut agent, device_idle(false));
    send_body(&mut agent, device_idle(true));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(session.state(), SessionState::Dozing);
}

#[test]
fn fr10_requests_and_input_reach_a_dozing_guest() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    dozing(&session, &mut agent);
    session
        .pointer(&pointer_down(7))
        .expect("input reaches Doze");
    assert_eq!(
        next_body(&mut agent),
        Body::PointerEvent(pointer_down(7).to_control())
    );
}

fn on_battery(percent: u8) -> PowerState {
    PowerState {
        ac_online: false,
        battery_present: true,
        battery_percent: percent,
        charging: false,
        power_saver: percent < 20,
    }
}

#[test]
fn fr10_the_hosts_power_reaches_the_guest_when_it_connects_and_when_it_changes() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    // Known before the guest runs: told as soon as the agent connects.
    session.set_power(on_battery(80));
    session.windows_shown(true).unwrap();
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(on_battery(80).to_control())
    );
    // The same state again is not news; a change is.
    session.set_power(on_battery(80));
    session.set_power(on_battery(15));
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(on_battery(15).to_control())
    );
    assert!(on_battery(15).to_control().power_saver);
}

#[test]
fn fr10_a_host_without_a_battery_is_a_full_battery_on_ac_power() {
    let power = PowerState::MAINS_ONLY.to_control();
    assert!(power.ac_online);
    assert!(!power.battery_present);
    assert_eq!(power.battery_percent, 100);
}

#[test]
fn fr10_the_power_hub_tells_every_session_and_each_new_one() {
    let hub = PowerHub::new();
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let session = Arc::new(session);
    session.windows_shown(true).unwrap();
    hub.update(on_battery(50));
    hub.attach(&session);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(on_battery(50).to_control())
    );
    hub.update(PowerState::MAINS_ONLY);
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(PowerState::MAINS_ONLY.to_control())
    );
    assert_eq!(hub.latest(), Some(PowerState::MAINS_ONLY));
}

#[test]
fn fr10_a_dozing_guest_is_never_frozen_by_default() {
    let (session, mut agent, launcher) = lifecycle_session(None);
    dozing(&session, &mut agent);
    thread::sleep(Duration::from_millis(500));
    assert_eq!(session.state(), SessionState::Dozing);
    assert_eq!(launcher.0.lock().unwrap().freezes, 0);
}

#[test]
fn fr10_with_freezing_chosen_a_dozing_guest_is_frozen_and_a_shown_window_thaws_it() {
    let (session, mut agent, launcher) = lifecycle_session(Some(Duration::from_millis(100)));
    dozing(&session, &mut agent);
    assert!(session.wait_for(SessionState::Frozen, WAIT));
    assert!(launcher.0.lock().unwrap().frozen);

    let shown = std::time::Instant::now();
    session.windows_shown(true).unwrap();
    assert!(session.wait_for(SessionState::Active, Duration::from_secs(1)));
    assert!(shown.elapsed() < Duration::from_secs(1));
    assert!(!launcher.0.lock().unwrap().frozen);
    assert_eq!(next_body(&mut agent), screen(true));
}

#[test]
fn fr10_a_freeze_the_user_asks_for_turns_the_screen_off_and_input_thaws_it() {
    let (session, mut agent, launcher) = lifecycle_session(None);
    session.windows_shown(true).unwrap();
    session.freeze().expect("the guest is frozen");
    assert_eq!(session.state(), SessionState::Frozen);
    assert_eq!(next_body(&mut agent), screen(false));
    assert_eq!(launcher.0.lock().unwrap().freezes, 1);

    // Input in a window the frozen guest still shows wakes it first.
    session
        .pointer(&pointer_down(7))
        .expect("input thaws the guest");
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(launcher.0.lock().unwrap().thaws, 1);
    assert_eq!(next_body(&mut agent), screen(true));
    assert_eq!(
        next_body(&mut agent),
        Body::PointerEvent(pointer_down(7).to_control())
    );
}

#[test]
fn fr10_the_users_freeze_setting_turns_freezing_in_doze_off_and_on() {
    let (session, mut agent, launcher) = lifecycle_session(Some(Duration::from_millis(100)));
    session.freeze_when_idle(false);
    dozing(&session, &mut agent);
    thread::sleep(Duration::from_millis(400));
    assert_eq!(session.state(), SessionState::Dozing);
    assert_eq!(launcher.0.lock().unwrap().freezes, 0);

    session.freeze_when_idle(true);
    assert!(session.wait_for(SessionState::Frozen, WAIT));
}

#[test]
fn fr10_a_guest_that_cannot_be_frozen_stays_as_it_was_and_says_why() {
    let (session, _agent, launcher) = lifecycle_session(None);
    launcher.0.lock().unwrap().cannot_freeze = Some("no pause on this host".into());
    match session.freeze() {
        Err(SessionError::FreezeFailed(why)) => assert!(why.contains("no pause"), "{why}"),
        other => panic!("expected the freeze to fail, got {other:?}"),
    }
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr10_freezing_a_stopped_guest_says_it_is_not_running() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.freeze(),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

#[test]
fn fr10_stopping_a_frozen_guest_thaws_it_first_and_stops_it() {
    let (session, _agent, launcher) = lifecycle_session(None);
    session.freeze().expect("frozen");
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    let guest = launcher.0.lock().unwrap();
    assert_eq!(guest.thaws, 1);
    assert_eq!(guest.processes, 0);
}
