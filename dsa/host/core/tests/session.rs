//! Session start and stop, through the session's public API, over the in-memory transport.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::apk::{self, InstallStatus};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::ime::{ImeCursor, ImeEvent};
use dsa_host_core::input::{buttons, PointerAction, PointerEvent, PointerTool};
use dsa_host_core::keys::{keycode, meta, KeyAction, KeyEvent};
use dsa_host_core::notifications::{Notification, NotificationAction, NotificationEvent};
use dsa_host_core::power::{PowerHub, PowerState};
use dsa_host_core::power_hints::{mode, PowerHint};
use dsa_host_core::session::{
    BootProblem, GuestLauncher, GuestPower, GuestStall, Session, SessionConfig, SessionError,
    SessionEvent, SessionState, StopReason,
};
use dsa_host_core::tasks::{Bounds, Task, TaskEvent, TaskRequest};
use dsa_host_core::widgets::{self, Placement, Widget, WidgetEvent, WidgetSize};
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
    /// What the guest cannot run without and is missing now.
    missing: Option<String>,
    /// What the launcher tells about a guest that is late.
    stall: Option<String>,
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

    fn prerequisite(&mut self) -> Option<String> {
        self.0.lock().unwrap().missing.clone()
    }

    fn explain_stall(&mut self, _stall: GuestStall) -> Option<String> {
        self.0.lock().unwrap().stall.clone()
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
    let mut agent = connect_daemon(connector, launcher);
    report_boot(&mut agent, true, true);
    agent
}

/// Reports how far the guest has got, as its channel daemon does.
fn report_boot(daemon: &mut Box<dyn Connection>, boot_completed: bool, agent_connected: bool) {
    send_body(
        daemon,
        Body::GuestBoot(dsa_protocol::control::GuestBoot {
            boot_completed,
            agent_connected,
        }),
    );
}

/// Connects as the channel daemon does, long before Android has finished booting.
fn connect_daemon(connector: &mut MemoryConnector, launcher: &FakeLauncher) -> Box<dyn Connection> {
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

/// A session with `config`, and the time it allows for the guest's handshake.
fn timed_session(launcher: FakeLauncher, config: SessionConfig) -> (Session, MemoryConnector) {
    let (listener, connector) = memory::pair();
    let config = SessionConfig {
        connect_timeout: WAIT,
        stop_timeout: WAIT,
        ..config
    };
    (
        Session::new(
            launcher,
            Box::new(listener),
            ControlHandshake::new("dsa-host test", WAIT),
            config,
        ),
        connector,
    )
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + WAIT;
    while !done() {
        assert!(std::time::Instant::now() < deadline, "never: {what}");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn fr2_the_session_stays_starting_until_the_guest_has_booted_and_its_agent_is_connected() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut daemon = connect_daemon(&mut connector, &launcher);

    // The handshake alone, and then each half of being ready, leave it Starting.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(session.state(), SessionState::Starting);
    report_boot(&mut daemon, false, true);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(session.state(), SessionState::Starting);
    report_boot(&mut daemon, true, false);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(session.state(), SessionState::Starting);
    assert_eq!(session.problem(), None);

    report_boot(&mut daemon, true, true);
    assert!(session.wait_for(SessionState::Active, WAIT));
}

#[test]
fn fr2_requests_are_refused_until_the_guest_is_ready() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut daemon = connect_daemon(&mut connector, &launcher);
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        session.request(TaskRequest::Launch {
            package_name: "com.android.settings".into()
        }),
        Err(SessionError::NotActive(SessionState::Starting))
    );
    report_boot(&mut daemon, true, true);
    assert!(session.wait_for(SessionState::Active, WAIT));
}

#[test]
fn fr2_a_guest_that_does_not_finish_booting_in_time_stalls_with_the_cause_and_recovers() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().stall =
        Some("the window manager was not running: 29 restarts".into());
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            ready_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        },
    );
    session.start();
    let mut daemon = connect_daemon(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    match session.problem() {
        Some(BootProblem::BootUnfinished { detail, .. }) => {
            assert!(detail.unwrap().contains("29 restarts"))
        }
        other => panic!("expected BootUnfinished, got {other:?}"),
    }
    assert_eq!(launcher.processes(), 3, "the guest is left running");
    assert!(session
        .problem()
        .unwrap()
        .to_string()
        .contains("29 restarts"));

    // It may only be slow: when it does finish, nothing else is needed.
    report_boot(&mut daemon, true, true);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(session.problem(), None);
}

#[test]
fn fr2_a_booted_guest_whose_agent_cannot_reach_the_channel_stalls_with_an_actionable_problem() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().stall = Some("move the data disk aside".into());
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            agent_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        },
    );
    session.start();
    let mut daemon = connect_daemon(&mut connector, &launcher);
    report_boot(&mut daemon, true, false);
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    let problem = session.problem().expect("a problem").to_string();
    assert!(problem.contains("agent"), "{problem}");
    assert!(problem.contains("move the data disk aside"), "{problem}");
    assert_eq!(launcher.processes(), 3, "the user's Android keeps running");

    report_boot(&mut daemon, true, true);
    assert!(session.wait_for(SessionState::Active, WAIT));
}

#[test]
fn fr2_an_agent_that_dies_and_does_not_come_back_stalls_a_running_session() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            agent_timeout: Duration::from_millis(100),
            ..SessionConfig::default()
        },
    );
    session.start();
    let mut daemon = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    report_boot(&mut daemon, true, false);
    assert!(session.wait_for(SessionState::Stalled, WAIT));

    report_boot(&mut daemon, true, true);
    assert!(session.wait_for(SessionState::Active, WAIT));
}

#[test]
fn fr2_a_guest_is_not_started_while_what_it_cannot_run_without_is_missing() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().missing = Some("the window manager cannot start".into());
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            prerequisite_grace: Duration::from_millis(50),
            ..SessionConfig::default()
        },
    );
    session.start();
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    assert_eq!(
        launcher.0.lock().unwrap().starts,
        0,
        "no guest to crash-loop"
    );
    assert_eq!(
        session.problem(),
        Some(BootProblem::Prerequisite(
            "the window manager cannot start".into()
        ))
    );

    launcher.0.lock().unwrap().missing = None;
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(session.problem(), None);
}

#[test]
fn fr2_a_waiting_start_can_be_stopped() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().missing = Some("the window manager cannot start".into());
    let (session, _connector) = timed_session(launcher.clone(), SessionConfig::default());
    session.start();
    assert!(session.wait_for(SessionState::Starting, WAIT));
    wait_until("the problem is reported", || session.problem().is_some());
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.problem(), None);
}

#[test]
fn nfr5_a_running_guest_is_stopped_when_what_it_cannot_run_without_stays_gone() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            prerequisite_grace: Duration::from_millis(50),
            prerequisite_stop_after: Duration::from_millis(1500),
            ..SessionConfig::default()
        },
    );
    session.start();
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    launcher.0.lock().unwrap().missing = Some("no one is signed in".into());
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(
        session.last_reason(),
        Some(StopReason::PrerequisiteLost("no one is signed in".into()))
    );
    assert_eq!(launcher.processes(), 0);
}

#[test]
fn nfr5_a_window_manager_that_comes_back_returns_the_session_to_active() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            prerequisite_grace: Duration::from_millis(50),
            ..SessionConfig::default()
        },
    );
    session.start();
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    launcher.0.lock().unwrap().missing = Some("the window manager has gone".into());
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    launcher.0.lock().unwrap().missing = None;
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(launcher.processes(), 3);
}

#[test]
fn fr2_an_agent_that_connects_again_is_told_the_screen_and_the_power() {
    let (session, mut daemon, _launcher) = lifecycle_session(None);
    assert_eq!(next_body(&mut daemon), screen(false));
    session.set_power(on_battery(40));
    assert_eq!(
        next_body(&mut daemon),
        Body::PowerState(on_battery(40).to_control())
    );
    report_boot(&mut daemon, true, false);
    thread::sleep(Duration::from_millis(50));
    report_boot(&mut daemon, true, true);
    assert_eq!(
        next_body(&mut daemon),
        Body::PowerState(on_battery(40).to_control())
    );
    assert_eq!(next_body(&mut daemon), screen(false));
}

#[test]
fn pr4_the_guests_power_hints_reach_the_host_and_end_when_the_guest_stops() {
    let (session, mut daemon, _launcher) = lifecycle_session(None);
    let hints = session.watch_power_hints();
    send_body(
        &mut daemon,
        Body::PowerHint(dsa_protocol::control::PowerHint {
            kind: dsa_protocol::control::PowerHintKind::Mode as i32,
            id: mode::LAUNCH,
            enabled: true,
            duration_ms: 0,
        }),
    );
    assert_eq!(
        hints.recv_timeout(WAIT),
        Ok(GuestPower::Hint(PowerHint::Mode {
            id: mode::LAUNCH,
            enabled: true,
        }))
    );
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(hints.recv_timeout(WAIT), Ok(GuestPower::Stopped));
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
        min_width: 700,
        min_height: 500,
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
        min_width: 700,
        min_height: 500,
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
fn fr5_a_press_and_a_shown_window_reach_the_power_watcher_before_the_guest() {
    let (session, _agent, _launcher) = active_session();
    let hints = session.watch_power_hints();
    let down = pointer_down(7);
    let mut moved = down;
    moved.action = PointerAction::Move;
    session.pointer(&moved).expect("the move is sent");
    session.pointer(&down).expect("the down is sent");
    assert_eq!(hints.recv_timeout(WAIT), Ok(GuestPower::HostInput));
    session.windows_shown(true).expect("the session takes it");
    assert_eq!(hints.recv_timeout(WAIT), Ok(GuestPower::WindowsShown(true)));
    assert!(hints.try_recv().is_err(), "a move is not a boost");
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
    report_boot(&mut agent, true, true);
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
            ..SessionConfig::default()
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
            ..SessionConfig::default()
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

fn host_sleep(asleep: bool, host_unix_time_ms: u64) -> Body {
    Body::HostSleep(dsa_protocol::control::HostSleep {
        asleep,
        host_unix_time_ms,
    })
}

fn sleep_ready() -> Body {
    Body::GuestSleepReady(dsa_protocol::control::GuestSleepReady {})
}

/// The wall clock in a `HostSleep` the guest got on waking, checked to be within a second of
/// `expected`.
fn assert_clock_near(body: Body, expected: std::time::SystemTime) {
    let Body::HostSleep(sleep) = body else {
        panic!("expected the host's clock, got {body:?}");
    };
    assert!(!sleep.asleep);
    let expected = expected
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i128;
    let got = i128::from(sleep.host_unix_time_ms);
    assert!((got - expected).abs() < 1000, "{got} vs {expected}");
}

#[test]
fn fr10_an_active_guest_writes_its_data_before_the_host_sleeps_and_is_active_again_on_waking() {
    let hub = Arc::new(PowerHub::new());
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let session = Arc::new(session);
    session.windows_shown(true).unwrap();
    hub.attach(&session);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    let sleeping = {
        let hub = Arc::clone(&hub);
        thread::spawn(move || hub.host_sleeping(Duration::from_secs(2)))
    };
    assert_eq!(next_body(&mut agent), screen(false));
    assert_eq!(next_body(&mut agent), host_sleep(true, 0));
    send_body(&mut agent, sleep_ready());
    assert!(
        sleeping.join().unwrap(),
        "the wait ends with the guest's answer"
    );

    // A clock an hour off shows the guest is told the host's clock, not its own.
    let host_clock = std::time::SystemTime::now() + Duration::from_secs(3600);
    hub.host_woke(host_clock);
    let woke = std::time::Instant::now();
    assert_clock_near(next_body(&mut agent), host_clock);
    assert_eq!(next_body(&mut agent), screen(true));
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert!(woke.elapsed() < Duration::from_secs(1));
}

#[test]
fn fr10_the_wait_before_the_host_sleeps_ends_in_time_without_an_answer() {
    let hub = Arc::new(PowerHub::new());
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let session = Arc::new(session);
    session.windows_shown(true).unwrap();
    hub.attach(&session);
    session.start();
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let started = std::time::Instant::now();
    assert!(!hub.host_sleeping(Duration::from_millis(300)));
    let waited = started.elapsed();
    assert!(waited < Duration::from_secs(2), "waited {waited:?}");
}

#[test]
fn fr10_a_dozing_guest_stays_dozing_when_the_host_wakes_and_has_its_clock_set() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    dozing(&session, &mut agent);
    let done = session.host_sleeping();
    // The screen is off already; only the word that the host sleeps goes.
    assert_eq!(next_body(&mut agent), host_sleep(true, 0));
    send_body(&mut agent, sleep_ready());
    done.recv_timeout(WAIT).expect("the guest's answer");
    let host_clock = std::time::SystemTime::now();
    session.host_woke(host_clock);
    assert_clock_near(next_body(&mut agent), host_clock);
    thread::sleep(DOZE_DELAY * 2);
    assert_eq!(session.state(), SessionState::Dozing);
    assert!(
        agent.recv(Some(Duration::from_millis(100))).is_err(),
        "nothing else is sent"
    );
}

#[test]
fn fr10_a_stopped_session_does_not_hold_up_the_host_sleeping() {
    let launcher = FakeLauncher::default();
    let (session, _connector) = session_with(launcher, WAIT);
    let done = session.host_sleeping();
    assert!(done.recv_timeout(Duration::from_secs(1)).is_ok());
    session.host_woke(std::time::SystemTime::now());
    assert_eq!(session.state(), SessionState::Stopped);
}

#[test]
fn fr10_an_agent_that_reconnects_after_the_host_woke_is_told_the_clock() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.windows_shown(true).unwrap();
    session.start();
    let mut daemon = connect_daemon(&mut connector, &launcher);
    report_boot(&mut daemon, true, true);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let _ = session.host_sleeping();
    assert_eq!(next_body(&mut daemon), screen(false));
    assert_eq!(next_body(&mut daemon), host_sleep(true, 0));
    // The agent went away while the host slept.
    report_boot(&mut daemon, true, false);
    thread::sleep(Duration::from_millis(200));
    let host_clock = std::time::SystemTime::now();
    session.host_woke(host_clock);
    // Nothing of the clock reaches a guest without its agent; the screen does.
    assert_eq!(next_body(&mut daemon), screen(true));
    assert!(daemon.recv(Some(Duration::from_millis(200))).is_err());
    report_boot(&mut daemon, true, true);
    loop {
        match next_body(&mut daemon) {
            body @ Body::HostSleep(_) => {
                assert_clock_near(body, host_clock);
                break;
            }
            _ => continue,
        }
    }
}

fn host_memory(low: bool) -> Body {
    Body::HostMemory(dsa_protocol::control::HostMemory { low })
}

#[test]
fn nfr2_a_host_short_of_memory_tells_the_guest_when_it_changes_and_when_the_agent_connects() {
    let hub = PowerHub::new();
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let session = Arc::new(session);
    session.windows_shown(true).unwrap();
    // Short before the guest runs: told as soon as the agent connects.
    hub.update_memory(true);
    hub.attach(&session);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(next_body(&mut agent), host_memory(true));
    // The same word again is not news; the end of the shortage is.
    hub.update_memory(true);
    hub.update_memory(false);
    assert_eq!(next_body(&mut agent), host_memory(false));
    assert!(!hub.memory_low());
    hub.update_memory(true);
    assert_eq!(next_body(&mut agent), host_memory(true));
}

#[test]
fn nfr2_the_host_is_short_of_memory_below_a_tenth_free_and_recovers_above_a_fifth() {
    use dsa_host_core::power::MemoryPressure;
    let total = 16 * 1024 * 1024;
    let mut pressure = MemoryPressure::new();
    assert!(!pressure.update(total, total / 2));
    assert!(!pressure.update(total, total * 11 / 100));
    assert!(pressure.update(total, total * 9 / 100));
    // Between the two lines it stays as it was, so it does not flap.
    assert!(pressure.update(total, total * 15 / 100));
    assert!(!pressure.update(total, total * 21 / 100));
    assert!(!pressure.update(total, total * 15 / 100));
    // An unknown total changes nothing.
    assert!(!pressure.update(0, 0));
}

/// An active session in Doze whose prerequisite stalls it soon after going missing.
fn dozing_session_that_can_stall(
    freeze_after: Option<Duration>,
) -> (Session, Box<dyn Connection>, FakeLauncher) {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            doze_delay: DOZE_DELAY,
            freeze_after,
            prerequisite_grace: Duration::from_millis(50),
            ..SessionConfig::default()
        },
    );
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    dozing(&session, &mut agent);
    (session, agent, launcher)
}

#[test]
fn fr10_a_stall_during_doze_returns_to_dozing() {
    let (session, _agent, launcher) = dozing_session_that_can_stall(None);

    launcher.0.lock().unwrap().missing = Some("the window manager has gone".into());
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    launcher.0.lock().unwrap().missing = None;

    // The agent says nothing new: it has been idle throughout.
    assert!(session.wait_for(SessionState::Dozing, WAIT));
    assert_eq!(session.problem(), None);
}

#[test]
fn fr10_time_stalled_during_doze_does_not_count_towards_the_freeze() {
    let freeze_after = Duration::from_secs(3);
    let (session, _agent, launcher) = dozing_session_that_can_stall(Some(freeze_after));

    launcher.0.lock().unwrap().missing = Some("the window manager has gone".into());
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    thread::sleep(freeze_after + Duration::from_millis(200));
    assert_eq!(
        launcher.0.lock().unwrap().freezes,
        0,
        "a stalled guest stays"
    );
    launcher.0.lock().unwrap().missing = None;

    assert!(session.wait_for(SessionState::Dozing, WAIT));
    assert_eq!(session.state(), SessionState::Dozing, "not frozen at once");
    assert_eq!(launcher.0.lock().unwrap().freezes, 0);
    // The rest of the timer still runs.
    assert!(session.wait_for(SessionState::Frozen, WAIT));
}

#[test]
fn fr10_a_window_shown_during_a_stall_ends_the_doze_it_interrupted() {
    let (session, mut agent, launcher) = dozing_session_that_can_stall(None);

    launcher.0.lock().unwrap().missing = Some("the window manager has gone".into());
    assert!(session.wait_for(SessionState::Stalled, WAIT));
    session.windows_shown(true).unwrap();
    assert_eq!(next_body(&mut agent), screen(true));
    launcher.0.lock().unwrap().missing = None;

    assert!(session.wait_for(SessionState::Active, WAIT));
}

#[test]
fn fr10_a_freeze_does_not_age_the_wait_for_an_agent_that_has_gone() {
    let agent_timeout = Duration::from_secs(2);
    let launcher = FakeLauncher::default();
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            agent_timeout,
            ..SessionConfig::default()
        },
    );
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    report_boot(&mut agent, true, false);
    thread::sleep(Duration::from_millis(100));
    session.freeze().expect("the guest is frozen");
    thread::sleep(agent_timeout + Duration::from_millis(200));
    session.windows_shown(true).unwrap();
    assert!(session.wait_for(SessionState::Active, WAIT));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(session.state(), SessionState::Active, "it has time left");

    // The agent is still gone, and what was left of the wait runs out.
    assert!(session.wait_for(SessionState::Stalled, WAIT));
}

#[test]
fn fr10_a_freeze_does_not_age_the_wait_for_a_missing_prerequisite() {
    let limit = Duration::from_secs(3);
    let launcher = FakeLauncher::default();
    let (session, mut connector) = timed_session(
        launcher.clone(),
        SessionConfig {
            prerequisite_grace: limit,
            prerequisite_stop_after: limit,
            ..SessionConfig::default()
        },
    );
    session.start();
    let _agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    launcher.0.lock().unwrap().missing = Some("the window manager has gone".into());
    // Long enough for the session, which looks once a second, to have seen it.
    thread::sleep(Duration::from_millis(1200));
    session.freeze().expect("the guest is frozen");
    thread::sleep(limit + Duration::from_millis(200));
    session.windows_shown(true).unwrap();
    assert!(session.wait_for(SessionState::Active, WAIT));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(session.state(), SessionState::Active, "it has time left");
    assert_eq!(launcher.processes(), 3);

    launcher.0.lock().unwrap().missing = None;
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

fn open_link(url: &str) -> Body {
    Body::OpenLink(dsa_protocol::control::OpenLink { url: url.into() })
}

#[test]
fn fr9_a_web_link_opened_in_the_guest_reaches_a_link_watcher() {
    let (session, mut agent, _launcher) = active_session();
    let links = session.watch_links();
    // Only http and https links reach the host; the rest are dropped and logged.
    send_body(
        &mut agent,
        open_link("file:///C:/Windows/System32/calc.exe"),
    );
    send_body(&mut agent, open_link("ms-settings:"));
    send_body(&mut agent, open_link("https://example.com/news?id=1"));
    assert_eq!(
        links.recv_timeout(WAIT).unwrap(),
        "https://example.com/news?id=1"
    );
    assert!(links.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr9_a_flood_of_links_from_the_guest_is_cut_off() {
    let (session, mut agent, _launcher) = active_session();
    let links = session.watch_links();
    for i in 0..20 {
        send_body(&mut agent, open_link(&format!("https://example.com/{i}")));
    }
    let mut arrived = 0;
    while links.recv_timeout(Duration::from_millis(300)).is_ok() {
        arrived += 1;
    }
    assert_eq!(arrived, dsa_host_core::links::LINKS_PER_WINDOW);
}

#[test]
fn fr9_links_kept_in_android_are_not_sent_to_the_host_and_the_agent_is_told() {
    let (session, mut agent, _launcher) = active_session();
    let links = session.watch_links();
    session.links_in_android(true);
    assert_eq!(
        next_body(&mut agent),
        Body::LinkHandling(dsa_protocol::control::LinkHandling { in_guest: true })
    );
    send_body(&mut agent, open_link("https://example.com/"));
    assert!(links.recv_timeout(Duration::from_millis(200)).is_err());
    // An agent that connects again takes links to go to the host, so it is told again.
    report_boot(&mut agent, true, false);
    thread::sleep(Duration::from_millis(50));
    report_boot(&mut agent, true, true);
    assert_eq!(
        next_body(&mut agent),
        Body::LinkHandling(dsa_protocol::control::LinkHandling { in_guest: true })
    );
    session.links_in_android(false);
    assert_eq!(
        next_body(&mut agent),
        Body::LinkHandling(dsa_protocol::control::LinkHandling { in_guest: false })
    );
    send_body(&mut agent, open_link("https://example.com/"));
    assert_eq!(links.recv_timeout(WAIT).unwrap(), "https://example.com/");
}

fn progress(transfer_id: u32, received: u64, error: &str) -> Body {
    Body::FileProgress(dsa_protocol::control::FileProgress {
        transfer_id,
        received,
        error: error.into(),
    })
}

#[test]
fn fr9_a_host_file_reaches_the_guest_in_chunks_paced_by_the_agent() {
    use dsa_host_core::files::{FileAction, FileTransfer, CHUNKS_IN_FLIGHT, CHUNK_BYTES};
    let (session, mut agent, _launcher) = active_session();
    let content: Vec<u8> = (0..(3 * CHUNK_BYTES + 77))
        .map(|i| (i % 253) as u8)
        .collect();
    let file = FileTransfer {
        name: "report.pdf".into(),
        content: content.clone(),
        action: FileAction::View,
    };
    thread::scope(|scope| {
        let sending = scope.spawn(|| session.open_file(&file));
        let Body::OpenFile(open) = next_body(&mut agent) else {
            panic!("expected the file's announcement");
        };
        assert_eq!(open.name, "report.pdf");
        assert_eq!(open.size, content.len() as u64);
        assert_eq!(open.action, dsa_protocol::control::FileAction::View as i32);
        let mut received = Vec::new();
        loop {
            // The host sends no more than the window ahead of what the agent has stored.
            let mut batch = 0u64;
            while let Ok(frame) = agent.recv(Some(Duration::from_millis(200))) {
                let Some(Body::FileChunk(chunk)) = dsa_protocol::decode(&frame).unwrap().body
                else {
                    panic!("expected a chunk");
                };
                assert_eq!(chunk.transfer_id, open.transfer_id);
                assert_eq!(chunk.offset, received.len() as u64);
                assert!(chunk.data.len() <= CHUNK_BYTES);
                received.extend_from_slice(&chunk.data);
                batch += 1;
            }
            assert!(batch <= CHUNKS_IN_FLIGHT, "{batch} chunks in flight");
            send_body(
                &mut agent,
                progress(open.transfer_id, received.len() as u64, ""),
            );
            if received.len() == content.len() {
                break;
            }
        }
        assert_eq!(sending.join().unwrap(), Ok(()));
        assert_eq!(received, content);
    });
}

#[test]
fn fr9_a_file_the_agent_cannot_take_is_refused_with_its_reason() {
    use dsa_host_core::files::{FileAction, FileTransfer};
    let (session, mut agent, _launcher) = active_session();
    let file = FileTransfer {
        name: "a.bin".into(),
        content: vec![1, 2, 3],
        action: FileAction::Send,
    };
    thread::scope(|scope| {
        let sending = scope.spawn(|| session.open_file(&file));
        let Body::OpenFile(open) = next_body(&mut agent) else {
            panic!("expected the file's announcement");
        };
        assert_eq!(open.action, dsa_protocol::control::FileAction::Send as i32);
        let _chunk = next_body(&mut agent);
        // Progress on a transfer that is not under way is dropped.
        send_body(&mut agent, progress(open.transfer_id + 100, 3, ""));
        send_body(
            &mut agent,
            progress(open.transfer_id, 0, "the guest's storage is full"),
        );
        match sending.join().unwrap() {
            Err(SessionError::FileRefused(why)) => {
                assert!(why.contains("storage is full"), "{why}")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    });
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr9_a_file_is_refused_before_sending_when_too_large_or_badly_named() {
    use dsa_host_core::files::{FileAction, FileTransfer, MAX_FILE_BYTES};
    let (session, mut agent, _launcher) = active_session();
    let large = FileTransfer {
        name: "big.iso".into(),
        content: vec![0; MAX_FILE_BYTES as usize + 1],
        action: FileAction::View,
    };
    assert!(matches!(
        session.open_file(&large),
        Err(SessionError::FileRefused(_))
    ));
    let named = FileTransfer {
        name: "../etc/passwd".into(),
        content: vec![1],
        action: FileAction::View,
    };
    assert!(matches!(
        session.open_file(&named),
        Err(SessionError::FileRefused(_))
    ));
    assert!(agent.recv(Some(Duration::from_millis(100))).is_err());
}

#[test]
fn fr9_a_file_for_a_stopped_guest_is_refused() {
    use dsa_host_core::files::{FileAction, FileTransfer};
    let launcher = FakeLauncher::default();
    let (session, _connector) = session_with(launcher, WAIT);
    let file = FileTransfer {
        name: "a.txt".into(),
        content: vec![1],
        action: FileAction::View,
    };
    assert_eq!(
        session.open_file(&file),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

fn clipboard_text(text: &str) -> Body {
    Body::ClipboardText(dsa_protocol::control::ClipboardText { text: text.into() })
}

fn clipboard_sharing(on: bool) -> Body {
    Body::ClipboardSharing(dsa_protocol::control::ClipboardSharing { on })
}

/// Asserts that the guest is sent nothing for a while.
fn nothing_for_the_guest(agent: &mut Box<dyn Connection>) {
    match agent.recv(Some(Duration::from_millis(300))) {
        Err(TransportError::Timeout) => {}
        Ok(frame) => panic!(
            "the guest was sent {:?}",
            dsa_protocol::decode(&frame).map(|envelope| envelope.body)
        ),
        Err(error) => panic!("the channel broke: {error}"),
    }
}

/// Asserts that no watcher hears of anything for a while.
fn nothing_for_the_host(watch: &Receiver<String>) {
    if let Ok(text) = watch.recv_timeout(Duration::from_millis(300)) {
        panic!("the host was given {text:?}");
    }
}

#[test]
fn fr13_text_copied_on_the_host_becomes_the_guests_clip() {
    let (session, mut agent, _launcher) = active_session();
    session.host_clipboard("copied on the host".into());
    assert_eq!(next_body(&mut agent), clipboard_text("copied on the host"));
}

#[test]
fn fr13_text_copied_in_the_guest_reaches_the_host() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_clipboard();
    send_body(&mut agent, clipboard_text("\u{c548}\u{b155} from Android"));
    assert_eq!(
        watch.recv_timeout(WAIT).expect("the guest's text"),
        "\u{c548}\u{b155} from Android"
    );
}

#[test]
fn fr13_the_hosts_text_goes_to_the_guest_once_when_android_is_ready() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.host_clipboard("before the start".into());
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(next_body(&mut agent), clipboard_text("before the start"));
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr13_a_clip_is_never_sent_back_to_the_side_it_came_from() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_clipboard();

    // From the guest: the host puts it on its clipboard, whose listener hears it again.
    send_body(&mut agent, clipboard_text("from the guest"));
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), "from the guest");
    session.host_clipboard("from the guest".into());
    nothing_for_the_guest(&mut agent);

    // From the host: the guest sets its primary clip, whose listener hears it again.
    session.host_clipboard("from the host".into());
    assert_eq!(next_body(&mut agent), clipboard_text("from the host"));
    send_body(&mut agent, clipboard_text("from the host"));
    nothing_for_the_host(&watch);

    // The same text copied again after something else does cross.
    send_body(&mut agent, clipboard_text("from the guest"));
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), "from the guest");
}

#[test]
fn fr13_with_sharing_off_nothing_crosses_and_turning_it_on_resumes() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_clipboard();

    session.share_clipboard(false);
    assert_eq!(next_body(&mut agent), clipboard_sharing(false));
    session.host_clipboard("secret on the host".into());
    nothing_for_the_guest(&mut agent);
    send_body(&mut agent, clipboard_text("secret in the guest"));
    nothing_for_the_host(&watch);

    session.share_clipboard(true);
    assert_eq!(next_body(&mut agent), clipboard_sharing(true));
    // What was copied while sharing was off stays where it was.
    nothing_for_the_guest(&mut agent);
    session.host_clipboard("shared again".into());
    assert_eq!(next_body(&mut agent), clipboard_text("shared again"));
    send_body(&mut agent, clipboard_text("from the guest again"));
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), "from the guest again");
}

#[test]
fn fr13_an_agent_that_connects_while_sharing_is_off_is_told() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.share_clipboard(false);
    session.host_clipboard("not shared".into());
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(next_body(&mut agent), clipboard_sharing(false));
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr13_text_longer_than_the_limit_does_not_cross_either_way() {
    use dsa_host_core::clipboard::MAX_CLIPBOARD_BYTES;
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_clipboard();
    let long = "x".repeat(MAX_CLIPBOARD_BYTES + 1);

    session.host_clipboard(long.clone());
    nothing_for_the_guest(&mut agent);
    send_body(&mut agent, clipboard_text(&long));
    nothing_for_the_host(&watch);

    // Text at the limit crosses whole, and the session is still there.
    let most = "y".repeat(MAX_CLIPBOARD_BYTES);
    session.host_clipboard(most.clone());
    assert_eq!(next_body(&mut agent), clipboard_text(&most));
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn nfr5_a_clipboard_sharing_message_from_the_guest_is_dropped() {
    let (session, mut agent, _launcher) = active_session();
    send_body(&mut agent, clipboard_sharing(false));
    thread::sleep(Duration::from_millis(100));
    assert_eq!(session.state(), SessionState::Active);
    // Sharing is still on.
    session.host_clipboard("still shared".into());
    assert_eq!(next_body(&mut agent), clipboard_text("still shared"));
}

fn guest_notification(key: &str, title: &str) -> dsa_protocol::control::GuestNotification {
    dsa_protocol::control::GuestNotification {
        key: key.into(),
        package_name: "com.example.chat".into(),
        app_label: "Chat".into(),
        title: title.into(),
        text: "see you at noon".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
        importance: 4,
        high_priority: true,
        clearable: true,
        when_millis: 1_700_000_000_000,
    }
}

fn posted(key: &str, title: &str) -> NotificationEvent {
    NotificationEvent::Posted(Notification {
        key: key.into(),
        package_name: "com.example.chat".into(),
        app_label: "Chat".into(),
        title: title.into(),
        text: "see you at noon".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
        importance: 4,
        high_priority: true,
        clearable: true,
        when_millis: 1_700_000_000_000,
    })
}

const CHAT: &str = "0|com.example.chat|1|null|10100";

fn clicked(key: &str) -> Body {
    Body::NotificationClicked(dsa_protocol::control::NotificationClicked { key: key.into() })
}

fn nothing_for_the_watcher(watch: &Receiver<NotificationEvent>) {
    if let Ok(event) = watch.recv_timeout(Duration::from_millis(300)) {
        panic!("the watcher heard of {event:?}");
    }
}

#[test]
fn fr8_a_notification_posted_updated_and_removed_in_the_guest_reaches_the_watcher() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), posted(CHAT, "Ann"));
    // The same again changes nothing; a new version replaces it.
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    nothing_for_the_watcher(&watch);
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann (2 messages)")),
    );
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        posted(CHAT, "Ann (2 messages)")
    );
    send_body(
        &mut agent,
        Body::NotificationRemoved(dsa_protocol::control::NotificationRemoved { key: CHAT.into() }),
    );
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        NotificationEvent::Removed { key: CHAT.into() }
    );
}

#[test]
fn fr8_clicking_a_notification_while_active_reaches_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    watch.recv_timeout(WAIT).unwrap();
    session
        .notification_action(NotificationAction::Open { key: CHAT.into() })
        .expect("taken");
    assert_eq!(next_body(&mut agent), clicked(CHAT));
}

#[test]
fn fr8_a_high_priority_notification_while_dozing_shows_and_clicking_it_wakes_the_guest_first() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    let watch = session.watch_notifications();
    dozing(&session, &mut agent);
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), posted(CHAT, "Ann"));
    assert_eq!(session.state(), SessionState::Dozing);

    session
        .notification_action(NotificationAction::Open { key: CHAT.into() })
        .expect("taken while dozing");
    let woken = std::time::Instant::now();
    // The screen comes on before the click, so the app opens on a screen that is on.
    assert_eq!(next_body(&mut agent), screen(true));
    assert_eq!(next_body(&mut agent), clicked(CHAT));
    assert_eq!(session.state(), SessionState::Active);
    // The screen stays on for the doze delay while the app's window comes.
    assert_eq!(next_body(&mut agent), screen(false));
    assert!(woken.elapsed() >= DOZE_DELAY / 2);
}

#[test]
fn fr8_clicking_a_notification_of_a_frozen_guest_thaws_it_first() {
    let (session, mut agent, launcher) = lifecycle_session(None);
    let watch = session.watch_notifications();
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    watch.recv_timeout(WAIT).unwrap();
    session.freeze().expect("frozen");
    assert_eq!(next_body(&mut agent), screen(false));
    session
        .notification_action(NotificationAction::Open { key: CHAT.into() })
        .expect("taken while frozen");
    assert_eq!(launcher.0.lock().unwrap().thaws, 1);
    assert_eq!(next_body(&mut agent), screen(true));
    assert_eq!(next_body(&mut agent), clicked(CHAT));
}

#[test]
fn fr8_dismissing_a_notification_on_the_host_cancels_it_in_the_guest_unless_it_is_kept() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    let kept = "0|com.example.music|1|null|10101";
    send_body(
        &mut agent,
        Body::Notification(dsa_protocol::control::GuestNotification {
            clearable: false,
            ..guest_notification(kept, "Playing")
        }),
    );
    watch.recv_timeout(WAIT).unwrap();
    watch.recv_timeout(WAIT).unwrap();
    session
        .notification_action(NotificationAction::Dismiss { key: kept.into() })
        .expect("taken");
    session
        .notification_action(NotificationAction::Dismiss { key: CHAT.into() })
        .expect("taken");
    assert_eq!(
        next_body(&mut agent),
        Body::NotificationDismissed(dsa_protocol::control::NotificationDismissed {
            key: CHAT.into()
        })
    );
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr8_acting_on_a_notification_the_guest_never_showed_sends_nothing() {
    let (session, mut agent, _launcher) = active_session();
    session
        .notification_action(NotificationAction::Open {
            key: "0|com.example|9|null|1".into(),
        })
        .expect("ignored, not refused");
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr8_acting_on_a_notification_of_a_stopped_guest_says_it_is_not_running() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.notification_action(NotificationAction::Open { key: CHAT.into() }),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}

#[test]
fn fr8_the_notifications_of_a_guest_that_stops_are_removed() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    watch.recv_timeout(WAIT).unwrap();
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        NotificationEvent::Removed { key: CHAT.into() }
    );
}

#[test]
fn fr8_a_flood_of_notifications_is_limited_and_the_rest_follow_in_their_latest_form() {
    use dsa_host_core::notifications::{BURST, REFILL};
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    for i in 0..BURST {
        send_body(
            &mut agent,
            Body::Notification(guest_notification(&format!("k{i}"), "spam")),
        );
    }
    for percent in [1, 50, 99] {
        send_body(
            &mut agent,
            Body::Notification(guest_notification("download", &format!("{percent}%"))),
        );
    }
    for _ in 0..BURST {
        watch.recv_timeout(WAIT).expect("the burst");
    }
    let started = std::time::Instant::now();
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), posted("download", "99%"));
    assert!(started.elapsed() >= REFILL / 4, "the rest waited its turn");
    nothing_for_the_watcher(&watch);
}

#[test]
fn nfr5_a_malformed_notification_from_the_guest_is_dropped() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    send_body(
        &mut agent,
        Body::Notification(guest_notification("", "no key")),
    );
    send_body(&mut agent, clicked(CHAT));
    nothing_for_the_watcher(&watch);
    assert_eq!(session.state(), SessionState::Active);
    send_body(
        &mut agent,
        Body::Notification(guest_notification(CHAT, "Ann")),
    );
    assert_eq!(watch.recv_timeout(WAIT).unwrap(), posted(CHAT, "Ann"));
}

/// The next message about an install the host sends the guest.
fn next_install_body(agent: &mut Box<dyn Connection>) -> Body {
    loop {
        match next_body(agent) {
            body @ (Body::InstallApk(_) | Body::ApkChunk(_) | Body::CancelInstall(_)) => {
                return body
            }
            _ => continue,
        }
    }
}

fn install_progress(transfer_id: u32, received: u64) -> Body {
    Body::InstallProgress(dsa_protocol::control::InstallProgress {
        transfer_id,
        received,
    })
}

fn install_result(
    transfer_id: u32,
    status: dsa_protocol::control::InstallStatus,
    package_name: &str,
    message: &str,
) -> Body {
    Body::InstallResult(dsa_protocol::control::InstallResult {
        transfer_id,
        status: status as i32,
        package_name: package_name.into(),
        message: message.into(),
    })
}

/// Plays the agent's part of one install: takes the APK chunk by chunk, in order, says how far
/// it got after each, and returns the transfer, the name and the APK.
fn receive_apk(agent: &mut Box<dyn Connection>) -> (u32, String, Vec<u8>) {
    let (transfer_id, name, size) = match next_install_body(agent) {
        Body::InstallApk(install) => (install.transfer_id, install.name, install.size),
        other => panic!("expected an install, got {other:?}"),
    };
    let mut content = Vec::new();
    while (content.len() as u64) < size {
        match next_install_body(agent) {
            Body::ApkChunk(chunk) => {
                assert_eq!(chunk.transfer_id, transfer_id);
                assert_eq!(chunk.offset, content.len() as u64, "chunks arrive in order");
                assert!(chunk.data.len() <= apk::CHUNK_BYTES);
                content.extend_from_slice(&chunk.data);
                send_body(agent, install_progress(transfer_id, content.len() as u64));
            }
            other => panic!("expected a piece of the APK, got {other:?}"),
        }
    }
    (transfer_id, name, content)
}

fn apk_bytes(size: usize) -> Vec<u8> {
    (0..size).map(|i| (i % 253) as u8).collect()
}

#[test]
fn fr11_an_apk_reaches_the_agent_in_paced_chunks_and_its_result_comes_back() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let content = apk_bytes(apk::CHUNK_BYTES * 5 + 77);
    let installing = {
        let session = Arc::clone(&session);
        let content = content.clone();
        thread::spawn(move || {
            session.install_apk("notes.apk", content.len() as u64, &mut &content[..])
        })
    };
    let (transfer_id, size) = match next_install_body(&mut agent) {
        Body::InstallApk(install) => (install.transfer_id, install.size),
        other => panic!("expected an install, got {other:?}"),
    };
    assert_eq!(size, content.len() as u64);
    // The host sends no more than the window before the agent has said anything.
    let mut received = Vec::new();
    for _ in 0..apk::CHUNKS_IN_FLIGHT {
        match next_install_body(&mut agent) {
            Body::ApkChunk(chunk) => received.extend_from_slice(&chunk.data),
            other => panic!("expected a piece of the APK, got {other:?}"),
        }
    }
    nothing_for_the_guest(&mut agent);
    // From here on, each word of progress lets the next chunk go.
    while (received.len() as u64) < size {
        send_body(
            &mut agent,
            install_progress(transfer_id, received.len() as u64),
        );
        match next_install_body(&mut agent) {
            Body::ApkChunk(chunk) => {
                assert_eq!(chunk.offset, received.len() as u64);
                received.extend_from_slice(&chunk.data);
            }
            other => panic!("expected a piece of the APK, got {other:?}"),
        }
    }
    assert!(received == content, "the APK arrived changed");
    send_body(&mut agent, install_progress(transfer_id, size));
    send_body(
        &mut agent,
        install_result(
            transfer_id,
            dsa_protocol::control::InstallStatus::Success,
            "com.example.notes",
            "",
        ),
    );
    let outcome = installing.join().unwrap().expect("installed");
    assert_eq!(outcome.status, InstallStatus::Installed);
    assert_eq!(outcome.package_name.as_deref(), Some("com.example.notes"));
}

#[test]
fn fr11_an_installed_app_reaches_the_start_menu_through_the_installed_apps_report() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let watch = session.watch_tasks();
    let content = apk_bytes(1000);
    let installing = {
        let session = Arc::clone(&session);
        let content = content.clone();
        thread::spawn(move || {
            session.install_apk("notes.apk", content.len() as u64, &mut &content[..])
        })
    };
    let (transfer_id, _, _) = receive_apk(&mut agent);
    send_body(
        &mut agent,
        install_result(
            transfer_id,
            dsa_protocol::control::InstallStatus::Success,
            "com.example.notes",
            "",
        ),
    );
    assert!(installing.join().unwrap().unwrap().installed());
    // The agent's launcher apps listener reports the new app as it reports any other, and the
    // window manager makes Start menu entries from this list.
    send_body(
        &mut agent,
        Body::InstalledApp(dsa_protocol::control::InstalledApp {
            package_name: "com.example.notes".into(),
            label: "Notes".into(),
            icon_png: vec![0x89, b'P', b'N', b'G'],
        }),
    );
    send_body(
        &mut agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec!["com.example.notes".into()],
        }),
    );
    loop {
        if let TaskEvent::Installed(apps) = watch.events.recv_timeout(WAIT).expect("the apps") {
            assert_eq!(apps.len(), 1);
            assert_eq!(apps[0].package_name, "com.example.notes");
            assert_eq!(apps[0].label, "Notes");
            break;
        }
    }
}

#[test]
fn fr11_the_install_window_learns_the_installed_version_and_signer_from_the_agent() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let asking = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.package_state("com.example.notes", WAIT))
    };
    let request_id = loop {
        if let Body::QueryPackage(query) = next_body(&mut agent) {
            assert_eq!(query.package_name, "com.example.notes");
            break query.request_id;
        }
    };
    send_body(
        &mut agent,
        Body::PackageState(dsa_protocol::control::PackageState {
            request_id,
            installed: true,
            version_code: 41,
            version_name: "4.1".into(),
            signer_sha256: vec![vec![5u8; 32]],
        }),
    );
    let installed = asking
        .join()
        .unwrap()
        .expect("an answer")
        .expect("installed");
    assert_eq!(installed.version_code, 41);
    assert_eq!(installed.version_name, "4.1");
    assert_eq!(installed.signer_sha256, vec![vec![5u8; 32]]);

    // A package the guest does not have is answered as not installed.
    let asking = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.package_state("com.example.missing", WAIT))
    };
    let request_id = loop {
        if let Body::QueryPackage(query) = next_body(&mut agent) {
            break query.request_id;
        }
    };
    send_body(
        &mut agent,
        Body::PackageState(dsa_protocol::control::PackageState {
            request_id,
            ..Default::default()
        }),
    );
    assert_eq!(asking.join().unwrap().expect("an answer"), None);
}

#[test]
fn fr11_a_refusal_from_android_carries_its_reason_and_words() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let installing = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.install_apk("old.apk", 10, &mut &b"0123456789"[..]))
    };
    let (transfer_id, name, content) = receive_apk(&mut agent);
    assert_eq!(name, "old.apk");
    assert_eq!(content, b"0123456789");
    send_body(
        &mut agent,
        install_result(
            transfer_id,
            dsa_protocol::control::InstallStatus::Downgrade,
            "com.example.notes",
            "INSTALL_FAILED_VERSION_DOWNGRADE",
        ),
    );
    let outcome = installing.join().unwrap().expect("a refusal is an outcome");
    assert_eq!(outcome.status, InstallStatus::Downgrade);
    assert_eq!(outcome.message, "INSTALL_FAILED_VERSION_DOWNGRADE");
}

/// Gives a few bytes, then fails, as a client that goes away does.
struct BrokenApk(usize);

impl std::io::Read for BrokenApk {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.0 == 0 {
            return Err(std::io::Error::other("the client went away"));
        }
        let n = buf.len().min(self.0);
        buf[..n].fill(7);
        self.0 -= n;
        Ok(n)
    }
}

#[test]
fn fr11_an_apk_that_does_not_arrive_whole_is_cancelled_in_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let installing = {
        let session = Arc::clone(&session);
        thread::spawn(move || {
            session.install_apk(
                "notes.apk",
                apk::CHUNK_BYTES as u64 * 3,
                &mut BrokenApk(100),
            )
        })
    };
    let transfer_id = match next_install_body(&mut agent) {
        Body::InstallApk(install) => install.transfer_id,
        other => panic!("expected an install, got {other:?}"),
    };
    match next_install_body(&mut agent) {
        Body::CancelInstall(cancel) => assert_eq!(cancel.transfer_id, transfer_id),
        other => panic!("expected the install to be cancelled, got {other:?}"),
    }
    send_body(
        &mut agent,
        install_result(
            transfer_id,
            dsa_protocol::control::InstallStatus::Other,
            "",
            "cancelled",
        ),
    );
    match installing.join().unwrap() {
        Err(SessionError::InstallRefused(why)) => {
            assert!(why.contains("did not arrive whole"), "{why}")
        }
        other => panic!("expected the install to fail, got {other:?}"),
    }
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr11_a_second_install_waits_until_the_first_has_ended() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let first = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.install_apk("one.apk", 3, &mut &b"one"[..]))
    };
    let (one, name, _) = receive_apk(&mut agent);
    assert_eq!(name, "one.apk");
    let second = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.install_apk("two.apk", 3, &mut &b"two"[..]))
    };
    // Nothing of the second goes out while the first is under way.
    nothing_for_the_guest(&mut agent);
    send_body(
        &mut agent,
        install_result(
            one,
            dsa_protocol::control::InstallStatus::Success,
            "a.one",
            "",
        ),
    );
    assert!(first.join().unwrap().unwrap().installed());
    let (two, name, _) = receive_apk(&mut agent);
    assert_eq!(name, "two.apk");
    assert_ne!(one, two);
    send_body(
        &mut agent,
        install_result(
            two,
            dsa_protocol::control::InstallStatus::Success,
            "a.two",
            "",
        ),
    );
    assert!(second.join().unwrap().unwrap().installed());
}

#[test]
fn fr11_an_install_while_the_guest_is_not_running_says_so() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    match session.install_apk("notes.apk", 3, &mut &b"apk"[..]) {
        Err(SessionError::NotActive(SessionState::Stopped)) => {}
        other => panic!("expected the install to need a running guest, got {other:?}"),
    }
}

#[test]
fn nfr5_word_on_an_install_that_is_not_under_way_is_dropped() {
    let (session, mut agent, _launcher) = active_session();
    send_body(&mut agent, install_progress(99, 10));
    send_body(
        &mut agent,
        install_result(99, dsa_protocol::control::InstallStatus::Success, "x.y", ""),
    );
    // An install message only the host sends, from the guest, is dropped as well.
    send_body(
        &mut agent,
        Body::ApkChunk(dsa_protocol::control::ApkChunk {
            transfer_id: 1,
            offset: 0,
            data: vec![1, 2, 3],
        }),
    );
    thread::sleep(Duration::from_millis(200));
    assert_eq!(session.state(), SessionState::Active);
    session
        .request(TaskRequest::Launch {
            package_name: "com.example.clock".into(),
        })
        .expect("the session goes on");
}

fn accessibility_watch(task_id: i32, on: bool) -> Body {
    Body::AccessibilityWatch(dsa_protocol::control::AccessibilityWatch { task_id, on })
}

fn accessibility_tree(task_id: i32, epoch: u32, nodes: u32) -> Body {
    use dsa_protocol::control::{AccessibilityNode, AccessibilityTree};
    Body::AccessibilityTree(AccessibilityTree {
        task_id,
        epoch,
        full: true,
        root_ids: vec![1],
        nodes: (1..=nodes)
            .map(|id| AccessibilityNode {
                id,
                child_ids: if id == 1 {
                    (2..=nodes).collect()
                } else {
                    vec![]
                },
                class_name: "android.widget.TextView".into(),
                text: format!("item {id}"),
                ..AccessibilityNode::default()
            })
            .collect(),
        ..AccessibilityTree::default()
    })
}

#[test]
fn fr18_the_guest_reads_a_tree_only_while_a_host_client_watches_it() {
    use dsa_host_core::accessibility::GuestAccessibility;
    let (session, mut agent, _launcher) = active_session();
    let first = session.watch_accessibility(7);
    assert_eq!(next_body(&mut agent), accessibility_watch(7, true));
    let second = session.watch_accessibility(7);
    // A second watch asks for a full tree again, for its own start.
    assert_eq!(next_body(&mut agent), accessibility_watch(7, true));
    send_body(&mut agent, accessibility_tree(7, 1, 3));
    for watch in [&first, &second] {
        match watch.updates.recv_timeout(WAIT).unwrap() {
            GuestAccessibility::Tree(tree) => {
                assert_eq!(tree.task_id, 7);
                assert_eq!(tree.nodes.len(), 3);
                assert_eq!(tree.nodes[1].text, "item 2");
            }
            other => panic!("expected a tree, got {other:?}"),
        }
    }
    // Another task's tree reaches neither.
    send_body(&mut agent, accessibility_tree(8, 1, 2));
    assert!(first
        .updates
        .recv_timeout(Duration::from_millis(200))
        .is_err());
    drop(first);
    nothing_for_the_guest(&mut agent);
    drop(second);
    assert_eq!(next_body(&mut agent), accessibility_watch(7, false));
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr18_a_new_agent_is_asked_again_for_every_watched_tree() {
    let (session, mut agent, _launcher) = active_session();
    let _watch = session.watch_accessibility(7);
    assert_eq!(next_body(&mut agent), accessibility_watch(7, true));
    report_boot(&mut agent, true, false);
    thread::sleep(Duration::from_millis(50));
    report_boot(&mut agent, true, true);
    assert_eq!(next_body(&mut agent), accessibility_watch(7, true));
}

#[test]
fn fr18_actions_go_to_the_guest_and_their_results_come_back() {
    use dsa_host_core::accessibility::{ActionRequest, GuestAccessibility, NodeAction};
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_accessibility(7);
    assert_eq!(next_body(&mut agent), accessibility_watch(7, true));
    let action = ActionRequest {
        epoch: 1,
        node_id: 3,
        action: NodeAction::Click,
        text: String::new(),
        request_id: 5,
    };
    session.accessibility_action(7, &action).unwrap();
    assert_eq!(
        next_body(&mut agent),
        Body::AccessibilityAction(action.to_control(7))
    );
    send_body(
        &mut agent,
        Body::AccessibilityActionResult(dsa_protocol::control::AccessibilityActionResult {
            task_id: 7,
            request_id: 5,
            done: true,
            error: String::new(),
        }),
    );
    match watch.updates.recv_timeout(WAIT).unwrap() {
        GuestAccessibility::ActionResult(result) => {
            assert_eq!(result.request_id, 5);
            assert_eq!(result.outcome, Ok(()));
        }
        other => panic!("expected a result, got {other:?}"),
    }
}

#[test]
fn nfr5_an_oversized_accessibility_tree_is_dropped_and_the_session_goes_on() {
    use dsa_host_core::accessibility::MAX_NODES;
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_accessibility(7);
    assert_eq!(next_body(&mut agent), accessibility_watch(7, true));
    send_body(&mut agent, accessibility_tree(7, 1, MAX_NODES as u32 + 10));
    assert!(watch
        .updates
        .recv_timeout(Duration::from_millis(300))
        .is_err());
    // Host-only messages from the guest are dropped too.
    send_body(&mut agent, accessibility_watch(7, true));
    send_body(&mut agent, accessibility_tree(7, 2, 2));
    assert!(watch.updates.recv_timeout(WAIT).is_ok());
    assert_eq!(session.state(), SessionState::Active);
}

fn clock_widget() -> dsa_protocol::control::WidgetProvider {
    dsa_protocol::control::WidgetProvider {
        provider: "com.example.clock/.ClockWidget".into(),
        package_name: "com.example.clock".into(),
        label: "Clock".into(),
        preview_png: vec![0x89, b'P', b'N', b'G'],
        min_width_dp: 110,
        min_height_dp: 40,
        default_width_dp: 180,
        default_height_dp: 110,
        max_width_dp: 0,
        max_height_dp: 0,
        resize_mode: 3,
        needs_configuration: false,
    }
}

fn widget_updated(widget_id: i32) -> Body {
    Body::WidgetUpdated(dsa_protocol::control::WidgetUpdated {
        widget_id,
        provider: Some(clock_widget()),
    })
}

fn widget_frame(widget_id: i32, png: &[u8]) -> Body {
    Body::WidgetFrame(dsa_protocol::control::WidgetFrame {
        widget_id,
        width: 2,
        height: 1,
        png: png.to_vec(),
    })
}

fn placed(widget_id: i32) -> WidgetEvent {
    WidgetEvent::Placed(Widget {
        widget_id,
        provider: widgets::checked_provider(clock_widget()).unwrap(),
        placement: Placement::default(),
    })
}

#[test]
fn fr12_widgets_the_agent_hosts_reach_a_watcher_with_what_they_show() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_widgets();
    assert!(watch.current.is_empty());
    send_body(&mut agent, widget_updated(12));
    assert_eq!(watch.events.recv_timeout(WAIT).unwrap(), placed(12));
    send_body(&mut agent, widget_frame(12, b"\x89PNG1"));
    match watch.events.recv_timeout(WAIT).unwrap() {
        WidgetEvent::Frame(frame) => assert_eq!(frame.png, b"\x89PNG1"),
        other => panic!("expected a frame, got {other:?}"),
    }
    // A late watcher starts from the widget and what it shows now.
    let late = session.watch_widgets();
    assert_eq!(late.current.len(), 2);
    assert_eq!(late.current[0], placed(12));
    // The list the agent sends on each connection removes the widgets it no longer has.
    send_body(&mut agent, widget_updated(15));
    assert_eq!(watch.events.recv_timeout(WAIT).unwrap(), placed(15));
    send_body(
        &mut agent,
        Body::WidgetList(dsa_protocol::control::WidgetList {
            widget_ids: vec![15],
        }),
    );
    assert_eq!(
        watch.events.recv_timeout(WAIT).unwrap(),
        WidgetEvent::Removed { widget_id: 12 }
    );
    assert_eq!(
        watch.events.recv_timeout(WAIT).unwrap(),
        WidgetEvent::Listed(vec![15])
    );
    assert_eq!(
        session
            .widgets()
            .iter()
            .map(|(id, _)| *id)
            .collect::<Vec<_>>(),
        vec![15]
    );
}

#[test]
fn fr12_the_gallery_gathers_every_part_of_the_agents_answer() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let asking = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.widget_providers(WAIT))
    };
    let request_id = match next_body(&mut agent) {
        Body::ListWidgetProviders(list) => list.request_id,
        other => panic!("expected the gallery request, got {other:?}"),
    };
    let notes = dsa_protocol::control::WidgetProvider {
        provider: "com.example.notes/.NoteWidget".into(),
        label: "a note".into(),
        ..clock_widget()
    };
    let broken = dsa_protocol::control::WidgetProvider {
        provider: "not a component".into(),
        ..clock_widget()
    };
    send_body(
        &mut agent,
        Body::WidgetProviders(dsa_protocol::control::WidgetProviders {
            request_id,
            providers: vec![notes, broken],
            complete: false,
        }),
    );
    send_body(
        &mut agent,
        Body::WidgetProviders(dsa_protocol::control::WidgetProviders {
            request_id,
            providers: vec![clock_widget()],
            complete: true,
        }),
    );
    let providers = asking.join().unwrap().expect("the gallery");
    // In label order; the broken one is left out.
    assert_eq!(
        providers
            .iter()
            .map(|p| p.provider.as_str())
            .collect::<Vec<_>>(),
        vec![
            "com.example.notes/.NoteWidget",
            "com.example.clock/.ClockWidget"
        ]
    );
}

#[test]
fn fr12_placing_a_widget_waits_for_the_agent_and_a_refusal_says_why() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let adding = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.add_widget("com.example.clock/.ClockWidget", WAIT))
    };
    let request_id = match next_body(&mut agent) {
        Body::AddWidget(add) => {
            assert_eq!(add.provider, "com.example.clock/.ClockWidget");
            add.request_id
        }
        other => panic!("expected the request to place a widget, got {other:?}"),
    };
    send_body(&mut agent, widget_updated(12));
    send_body(
        &mut agent,
        Body::WidgetAdded(dsa_protocol::control::WidgetAdded {
            request_id,
            widget_id: 12,
            error: String::new(),
        }),
    );
    assert_eq!(adding.join().unwrap(), Ok(12));
    assert_eq!(session.widgets().len(), 1);

    let refused = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.add_widget("com.example.clock/.ClockWidget", WAIT))
    };
    let request_id = match next_body(&mut agent) {
        Body::AddWidget(add) => add.request_id,
        other => panic!("expected the request to place a widget, got {other:?}"),
    };
    send_body(
        &mut agent,
        Body::WidgetAdded(dsa_protocol::control::WidgetAdded {
            request_id,
            widget_id: 0,
            error: "the user canceled the widget's setup".into(),
        }),
    );
    assert_eq!(
        refused.join().unwrap(),
        Err(SessionError::WidgetRefused(
            "the user canceled the widget's setup".into()
        ))
    );
    assert!(matches!(
        session.add_widget("../etc", WAIT),
        Err(SessionError::WidgetRefused(_))
    ));
}

#[test]
fn fr12_a_widget_window_size_reaches_the_agent_in_pixels_and_dp() {
    let (session, mut agent, _launcher) = active_session();
    send_body(&mut agent, widget_updated(12));
    session
        .resize_widget(12, WidgetSize::of_pixels(360, 220, 240))
        .unwrap();
    assert_eq!(
        next_body(&mut agent),
        Body::ResizeWidget(dsa_protocol::control::ResizeWidget {
            widget_id: 12,
            width: 360,
            height: 220,
            density_dpi: 240,
            width_dp: 240,
            height_dp: 147,
        })
    );
}

#[test]
fn fr12_removing_a_widget_reaches_the_agent_and_closes_its_window() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_widgets();
    send_body(&mut agent, widget_updated(12));
    assert_eq!(watch.events.recv_timeout(WAIT).unwrap(), placed(12));
    session.remove_widget(12).unwrap();
    assert_eq!(
        next_body(&mut agent),
        Body::RemoveWidget(dsa_protocol::control::RemoveWidget { widget_id: 12 })
    );
    assert_eq!(
        watch.events.recv_timeout(WAIT).unwrap(),
        WidgetEvent::Removed { widget_id: 12 }
    );
    assert!(session.widgets().is_empty());
}

#[test]
fn fr12_a_widget_keeps_showing_while_dozing_and_a_press_wakes_the_guest_first() {
    let (session, mut agent, _launcher) = lifecycle_session(None);
    let watch = session.watch_widgets();
    send_body(&mut agent, widget_updated(12));
    assert_eq!(watch.events.recv_timeout(WAIT).unwrap(), placed(12));
    dozing(&session, &mut agent);
    // The clock ticks in Doze: its frames still reach the watcher, and the guest dozes on.
    for second in 0..3u8 {
        send_body(
            &mut agent,
            widget_frame(12, &[0x89, b'P', b'N', b'G', second]),
        );
        match watch.events.recv_timeout(WAIT).unwrap() {
            WidgetEvent::Frame(frame) => assert_eq!(frame.png[4], second),
            other => panic!("expected a frame, got {other:?}"),
        }
    }
    assert_eq!(session.state(), SessionState::Dozing);
    let press =
        |action: dsa_protocol::control::PointerAction| dsa_protocol::control::WidgetPointer {
            widget_id: 12,
            action: action as i32,
            pointer_id: 0,
            x: 10.0,
            y: 20.0,
        };
    session
        .widget_pointer(press(dsa_protocol::control::PointerAction::Down))
        .unwrap();
    assert_eq!(next_body(&mut agent), screen(true));
    assert_eq!(
        next_body(&mut agent),
        Body::WidgetPointer(press(dsa_protocol::control::PointerAction::Down))
    );
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr12_widgets_stay_while_the_guest_is_stopped() {
    let (session, mut agent, _launcher) = active_session();
    send_body(&mut agent, widget_updated(12));
    wait_until("the widget is placed", || session.widgets().len() == 1);
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.widgets().len(), 1);
    assert_eq!(session.watch_widgets().current, vec![placed(12)]);
}

#[test]
fn nfr5_malformed_widget_messages_are_dropped_and_the_session_stays_active() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_widgets();
    // No provider, a bad provider, no ID, a frame of a widget not placed, a frame that is no
    // PNG, and an answer nobody asked for.
    send_body(
        &mut agent,
        Body::WidgetUpdated(dsa_protocol::control::WidgetUpdated {
            widget_id: 3,
            provider: None,
        }),
    );
    send_body(
        &mut agent,
        Body::WidgetUpdated(dsa_protocol::control::WidgetUpdated {
            widget_id: 3,
            provider: Some(dsa_protocol::control::WidgetProvider {
                provider: "x".into(),
                ..clock_widget()
            }),
        }),
    );
    send_body(&mut agent, widget_updated(0));
    send_body(&mut agent, widget_frame(99, b"\x89PNG"));
    send_body(&mut agent, widget_updated(12));
    assert_eq!(watch.events.recv_timeout(WAIT).unwrap(), placed(12));
    send_body(&mut agent, widget_frame(12, b"GIF89a"));
    send_body(
        &mut agent,
        Body::WidgetAdded(dsa_protocol::control::WidgetAdded {
            request_id: 4242,
            widget_id: 1,
            error: String::new(),
        }),
    );
    // A request only the host sends is dropped too.
    send_body(
        &mut agent,
        Body::RemoveWidget(dsa_protocol::control::RemoveWidget { widget_id: 12 }),
    );
    send_body(&mut agent, widget_frame(12, b"\x89PNG"));
    match watch.events.recv_timeout(WAIT).unwrap() {
        WidgetEvent::Frame(frame) => assert_eq!(frame.png, b"\x89PNG"),
        other => panic!("only the good frame arrives, got {other:?}"),
    }
    assert_eq!(session.state(), SessionState::Active);
    assert_eq!(session.widgets().len(), 1);
}
