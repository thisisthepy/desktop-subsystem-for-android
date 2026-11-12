//! Session start and stop, through the session's public API, over the in-memory transport.

mod common;

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::apk::{self, InstallPart, InstallStatus};
use dsa_host_core::apps::UninstallStatus;
use dsa_host_core::background::{BackgroundCommand, BackgroundEvent, MediaCommand, Playback};
use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::ime::{ImeCursor, ImeEvent};
use dsa_host_core::input::{buttons, PointerAction, PointerEvent, PointerTool};
use dsa_host_core::keys::{keycode, meta, KeyAction, KeyEvent};
use dsa_host_core::location::{Fix, LocationEvent, Wanted};
use dsa_host_core::notifications::{Notification, NotificationAction, NotificationEvent};
use dsa_host_core::power::{PowerHub, PowerState};
use dsa_host_core::power_hints::{mode, PowerHint};
use dsa_host_core::print::{PrintEvent, PrintJob, PrintOutcome};
use dsa_host_core::session::{
    BackgroundRequest, BootProblem, GuestLauncher, GuestPower, GuestStall, MemoryResizer, Session,
    SessionConfig, SessionError, SessionEvent, SessionState, StopReason,
};
use dsa_host_core::tasks::{Bounds, StatusBarAppearance, Task, TaskEvent, TaskRequest};
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
    /// The memory range the launcher offers, and each size the session set.
    memory_range: Option<(u64, u64)>,
    memory_sizes: Vec<u64>,
    /// Sizes the platform fails to set once each, as Hyper-V timed out taking 1.5 GiB back,
    /// and how many times the session told the launcher growing failed for good.
    refuse_memory: Vec<u64>,
    growth_failed: usize,
    /// Set to resize on a thread of the launcher's own that waits until this is opened, as
    /// Hyper-V took 45 s over one resize.
    resize_gate: Option<Arc<(Mutex<bool>, std::sync::Condvar)>>,
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

    fn memory_range_mb(&self) -> Option<(u64, u64)> {
        self.0.lock().unwrap().memory_range
    }

    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String> {
        let mut guest = self.0.lock().unwrap();
        guest.memory_sizes.push(mb);
        if let Some(at) = guest.refuse_memory.iter().position(|&size| size == mb) {
            guest.refuse_memory.remove(at);
            return Err(format!(
                "HcsModifyComputeSystem failed (HRESULT 0x800705b4) setting {mb} MiB"
            ));
        }
        Ok(())
    }

    fn memory_growth_failed(&mut self, _why: &str) {
        self.0.lock().unwrap().growth_failed += 1;
    }

    fn memory_resizer(&mut self) -> Option<Box<dyn MemoryResizer>> {
        let gate = self.0.lock().unwrap().resize_gate.clone()?;
        Some(Box::new(BlockingResizer {
            launcher: self.clone(),
            gate,
        }))
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

/// A resize that waits until its gate opens, then records the size as the launcher does.
struct BlockingResizer {
    launcher: FakeLauncher,
    gate: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

impl MemoryResizer for BlockingResizer {
    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String> {
        let (open, changed) = &*self.gate;
        let mut open = open.lock().unwrap();
        while !*open {
            open = changed.wait(open).unwrap();
        }
        drop(open);
        self.launcher.set_memory_mb(mb)
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
    Box::new(common::PastConnectSettings(agent))
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

/// A position in Seoul with every value known.
fn seoul() -> Fix {
    Fix {
        latitude_degrees: 37.5,
        longitude_degrees: 127.0,
        horizontal_accuracy_meters: Some(10.0),
        altitude_meters: Some(50.0),
        vertical_accuracy_meters: Some(3.0),
        speed_meters_per_second: Some(2.5),
        bearing_degrees: Some(90.0),
        unix_time_ms: 1_700_000_000_000,
    }
}

/// The next location message for the guest, past any other.
fn next_location(agent: &mut Box<dyn Connection>) -> Body {
    loop {
        let body = next_body(agent);
        if matches!(body, Body::LocationFix(_) | Body::LocationStatus(_)) {
            return body;
        }
    }
}

#[test]
fn fr26_the_guests_location_request_reaches_the_host_and_its_fixes_reach_the_guest() {
    let (session, mut daemon, _launcher) = lifecycle_session(None);
    let wanted = session.watch_location();
    assert_eq!(wanted.recv_timeout(WAIT), Ok(Wanted::NONE));
    // A position from before the guest wants one is dropped.
    let equator = Fix {
        latitude_degrees: 0.0,
        ..seoul()
    };
    session.host_location(LocationEvent::Fix(equator));

    send_body(
        &mut daemon,
        Body::LocationRequest(dsa_protocol::control::LocationRequest {
            active: true,
            interval_ms: 2000,
        }),
    );
    assert_eq!(
        wanted.recv_timeout(WAIT),
        Ok(Wanted::every(Duration::from_secs(2)))
    );
    session.host_location(LocationEvent::Fix(seoul()));
    assert_eq!(
        next_location(&mut daemon),
        Body::LocationFix(seoul().to_control())
    );

    // A guest that stops wants no location any longer.
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(wanted.recv_timeout(WAIT), Ok(Wanted::NONE));
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
fn fr2_a_newer_boot_that_is_up_forgets_why_the_one_before_it_stopped() {
    // The boot before an update is refused for its old protocol definition...
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connector.connect(WAIT).expect("the agent connects");
    let mut hello = local_hello(Role::Guest, "dsa-guestd old");
    hello.schema_hash = vec![0x5a; 32];
    let _ = guest_connect(agent.as_mut(), &hello, &launcher.secret(), WAIT);
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert!(matches!(
        session.last_reason(),
        Some(StopReason::HandshakeRejected(_))
    ));

    // ...and the updated guest boots and connects. Its status shows no stop that is over.
    session.start();
    let _updated = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(session.last_reason(), None);

    // A stop of this boot is reported as before.
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(session.last_reason(), Some(StopReason::Requested));
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
        status_bar_appearance: dsa_protocol::control::StatusBarAppearance::Light as i32,
        status_bar_color: 0xFFFA_FAFA,
        ..Default::default()
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
        status_bar: StatusBarAppearance::Light,
        status_bar_color: 0xFFFA_FAFA,
        ..Task::default()
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
fn nfr2_the_guest_memory_grows_when_its_daemon_says_it_is_short() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().memory_range = Some((3072, 16384));
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    send_body(
        &mut agent,
        Body::GuestMemoryPressure(dsa_protocol::control::GuestMemoryPressure {
            mem_total_kb: 2900 * 1024,
            mem_available_kb: 200 * 1024,
            stall_centipercent: 0,
        }),
    );
    wait_until("the memory is grown", || {
        launcher.0.lock().unwrap().memory_sizes == vec![3712]
    });
}

#[test]
fn nfr2_memory_ballooned_back_to_the_host_is_not_counted_as_reserved_by_the_kernel() {
    let launcher = FakeLauncher::default();
    launcher.0.lock().unwrap().memory_range = Some((2048, 16384));
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let pressure = |total_mb: u64, available_mb: u64| {
        Body::GuestMemoryPressure(dsa_protocol::control::GuestMemoryPressure {
            mem_total_kb: total_mb * 1024,
            mem_available_kb: available_mb * 1024,
            stall_centipercent: 0,
        })
    };
    // Short: the VM grows by hot-add, to what the guest uses plus 768 MiB of headroom.
    send_body(&mut agent, pressure(1950, 300));
    wait_until("the memory is grown", || {
        launcher.0.lock().unwrap().memory_sizes == vec![2560]
    });
    // Calm again after the cooldown: it is ballooned back to the base.
    thread::sleep(Duration::from_millis(5100));
    send_body(&mut agent, pressure(2600, 2200));
    wait_until("the memory is given back", || {
        launcher.0.lock().unwrap().memory_sizes == vec![2560, 2048]
    });
    // The hot-added blocks stay online in the guest, which counts 2560 MiB; the kernel kept
    // 98 MiB of the 2048 MiB the VM has now, and the host has the rest back.
    send_body(
        &mut agent,
        Body::GuestMemory(dsa_protocol::control::GuestMemory {
            mem_total_kb: 1950 * 1024,
            mem_free_kb: 1000 * 1024,
            mem_available_kb: 1500 * 1024,
            mem_online_kb: 2560 * 1024,
            ..Default::default()
        }),
    );
    wait_until("the report arrives", || session.guest_memory().is_some());
    let memory = session.guest_memory().unwrap().memory;
    assert_eq!(memory.kernel.mem_online_kb, 2048 * 1024);
    assert_eq!(memory.reserved_at_boot_kb(), Some(98 * 1024));
}

#[test]
fn nfr2_a_resize_that_times_out_is_tried_again_smaller_and_later_resizes_still_happen() {
    let launcher = FakeLauncher::default();
    {
        let mut guest = launcher.0.lock().unwrap();
        guest.memory_range = Some((3072, 16384));
        guest.refuse_memory = vec![3712];
    }
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let short = || {
        Body::GuestMemoryPressure(dsa_protocol::control::GuestMemoryPressure {
            mem_total_kb: 2900 * 1024,
            mem_available_kb: 200 * 1024,
            stall_centipercent: 0,
        })
    };
    send_body(&mut agent, short());
    wait_until("the growth is asked for", || {
        launcher.0.lock().unwrap().memory_sizes == vec![3712]
    });
    // Within the retry wait nothing more is asked.
    send_body(&mut agent, short());
    thread::sleep(Duration::from_millis(200));
    assert_eq!(launcher.0.lock().unwrap().memory_sizes, vec![3712]);
    // After it, the growth is tried again in a step half the size of the one that failed.
    thread::sleep(Duration::from_millis(5100));
    send_body(&mut agent, short());
    wait_until("the growth is tried again, smaller", || {
        launcher.0.lock().unwrap().memory_sizes == vec![3712, 3456]
    });
    // The failure never told the launcher to stop growing.
    assert_eq!(launcher.0.lock().unwrap().growth_failed, 0);
}

#[test]
fn nfr2_a_slow_resize_runs_beside_the_session_which_keeps_serving_and_asks_for_no_second() {
    let launcher = FakeLauncher::default();
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    {
        let mut guest = launcher.0.lock().unwrap();
        guest.memory_range = Some((3072, 16384));
        guest.resize_gate = Some(Arc::clone(&gate));
    }
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let session = Arc::new(session);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    let short = || {
        Body::GuestMemoryPressure(dsa_protocol::control::GuestMemoryPressure {
            mem_total_kb: 2900 * 1024,
            mem_available_kb: 200 * 1024,
            stall_centipercent: 0,
        })
    };
    send_body(&mut agent, short());
    // The resize waits on its gate. Meanwhile the session's own thread still answers a press
    // in a widget's window, as it does input and the apps' requests.
    let asking = Arc::clone(&session);
    let (answered, answer) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let _ = answered.send(asking.widget_pointer(dsa_protocol::control::WidgetPointer {
            action: dsa_protocol::control::PointerAction::Down as i32,
            ..Default::default()
        }));
    });
    answer
        .recv_timeout(Duration::from_secs(2))
        .expect("the session answers while a resize is in flight")
        .expect("the press reaches the guest");
    assert_eq!(launcher.0.lock().unwrap().memory_sizes, Vec::<u64>::new());
    // Still short, past the cooldown: no second resize while the first is in flight.
    thread::sleep(Duration::from_millis(5100));
    send_body(&mut agent, short());
    thread::sleep(Duration::from_millis(200));
    // The platform answers: the size is set once.
    {
        let (open, changed) = &*gate;
        *open.lock().unwrap() = true;
        changed.notify_all();
    }
    wait_until("the resize lands", || {
        launcher.0.lock().unwrap().memory_sizes == vec![3712]
    });
    thread::sleep(Duration::from_millis(200));
    assert_eq!(launcher.0.lock().unwrap().memory_sizes, vec![3712]);
    assert_eq!(launcher.0.lock().unwrap().growth_failed, 0);
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
            package_name: "com.example.clock".into(),
            ..Default::default()
        }))
    );
}

#[test]
fn fr22_a_windows_caption_height_reaches_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    session
        .request(TaskRequest::SetCaption {
            task_id: 7,
            height: 62,
            start_width: 48,
            end_width: 138,
        })
        .expect("the caption height is sent");
    let sent = dsa_protocol::decode(&agent.recv(Some(WAIT)).unwrap()).unwrap();
    assert_eq!(
        sent.body,
        Some(Body::SetTaskCaption(
            dsa_protocol::control::SetTaskCaption {
                task_id: 7,
                height: 62,
                start_width: 48,
                end_width: 138,
            }
        ))
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

#[test]
fn fr6_the_editor_of_a_task_reaches_a_watcher_and_a_description_that_makes_no_sense_does_not() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    send_body(&mut agent, task_updated(7, "Clock"));
    let described = |selection_end| {
        Body::ImeEditor(dsa_protocol::control::ImeEditor {
            task_id: 7,
            kind: dsa_protocol::control::ImeInputKind::Password as i32,
            text: "ab".into(),
            text_start: 10,
            selection_start: 12,
            selection_end,
            composition_start: 0,
            composition_end: 0,
        })
    };
    // A selection past the end of the text is dropped; the next description is taken.
    send_body(&mut agent, described(40));
    send_body(&mut agent, described(11));
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
            TaskEvent::ImeEditor {
                task_id: 7,
                editor: dsa_host_core::ime::ImeEditor {
                    kind: dsa_host_core::ime::InputKind::Password,
                    text: "ab".into(),
                    text_start: 10,
                    selection: (12, 11),
                    composition: None,
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
    let mut agent: Box<dyn Connection> = Box::new(common::PastConnectSettings(agent));
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

fn display_refresh(hz: u32) -> Body {
    Body::DisplayRefresh(dsa_protocol::control::DisplayRefresh { hz })
}

#[test]
fn fr10_battery_saver_caps_the_guest_refresh_rate_at_once_and_lifts_it_when_off() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.set_power(PowerState::MAINS_ONLY);
    session.set_monitor_refresh(120);
    session.windows_shown(true).unwrap();
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(PowerState::MAINS_ONLY.to_control())
    );
    // A 120 Hz monitor: the guest refreshes at 120 Hz from the moment the agent is there.
    assert_eq!(next_body(&mut agent), display_refresh(120));
    // The saver on: 60 Hz, with no restart and nothing else in between.
    let saver = PowerState {
        power_saver: true,
        ..PowerState::MAINS_ONLY
    };
    let changed = std::time::Instant::now();
    session.set_power(saver);
    assert_eq!(next_body(&mut agent), Body::PowerState(saver.to_control()));
    assert_eq!(next_body(&mut agent), display_refresh(60));
    assert!(changed.elapsed() < Duration::from_secs(1));
    // The saver off: back to the monitor's 120 Hz.
    session.set_power(PowerState::MAINS_ONLY);
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(PowerState::MAINS_ONLY.to_control())
    );
    assert_eq!(next_body(&mut agent), display_refresh(120));
    // A host that reads its saver apart from its power gives it on its own.
    session.set_battery_saver(true);
    assert_eq!(next_body(&mut agent), display_refresh(60));
    session.set_battery_saver(false);
    assert_eq!(next_body(&mut agent), display_refresh(120));
}

#[test]
fn fr10_battery_saver_leaves_a_60_hz_monitor_alone() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.set_monitor_refresh(60);
    session.windows_shown(true).unwrap();
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(next_body(&mut agent), display_refresh(60));
    // The saver on and off again: the rate is the same, so the guest hears nothing of it,
    // and the next thing it hears is the battery state that follows.
    session.set_battery_saver(true);
    session.set_battery_saver(false);
    session.set_power(on_battery(80));
    assert_eq!(
        next_body(&mut agent),
        Body::PowerState(on_battery(80).to_control())
    );
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
fn fr10_every_hosts_sleep_notification_waits_at_most_two_seconds_for_the_guest() {
    use dsa_host_core::power::{SleepEvent, SLEEP_WAIT};
    assert_eq!(SLEEP_WAIT, Duration::from_secs(2));
    let hub = Arc::new(PowerHub::new());
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    let session = Arc::new(session);
    session.windows_shown(true).unwrap();
    hub.attach(&session);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));

    // A guest that answers ends the wait with its answer.
    let sleeping = {
        let hub = Arc::clone(&hub);
        thread::spawn(move || hub.on_sleep_event(SleepEvent::Sleeping))
    };
    assert_eq!(next_body(&mut agent), screen(false));
    assert_eq!(next_body(&mut agent), host_sleep(true, 0));
    send_body(&mut agent, sleep_ready());
    assert!(sleeping.join().unwrap());
    assert!(hub.on_sleep_event(SleepEvent::Woke));
    assert!(matches!(next_body(&mut agent), Body::HostSleep(_)));
    assert_eq!(next_body(&mut agent), screen(true));
    assert!(session.wait_for(SessionState::Active, WAIT));

    // A guest that does not answer holds the host's sleep for no more than two seconds.
    let started = std::time::Instant::now();
    assert!(!hub.on_sleep_event(SleepEvent::Sleeping));
    let waited = started.elapsed();
    assert!(
        waited < SLEEP_WAIT + Duration::from_millis(500),
        "waited {waited:?}"
    );
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
        package: String::new(),
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
fn fr29_a_file_dropped_on_a_window_reaches_that_app_whole() {
    use dsa_host_core::files::{FileAction, FileTransfer, CHUNK_BYTES};
    let (session, mut agent, _launcher) = active_session();
    let content: Vec<u8> = (0..(CHUNK_BYTES + 5)).map(|i| (i % 251) as u8).collect();
    let file = FileTransfer {
        name: "holiday.jpg".into(),
        content: content.clone(),
        action: FileAction::Send,
        package: "com.example.gallery".into(),
    };
    thread::scope(|scope| {
        let sending = scope.spawn(|| session.open_file(&file));
        let Body::OpenFile(open) = next_body(&mut agent) else {
            panic!("expected the file's announcement");
        };
        assert_eq!(open.action, dsa_protocol::control::FileAction::Send as i32);
        assert_eq!(open.package, "com.example.gallery");
        let mut received = Vec::new();
        while received.len() < content.len() {
            let Body::FileChunk(chunk) = next_body(&mut agent) else {
                panic!("expected a chunk");
            };
            received.extend_from_slice(&chunk.data);
            send_body(
                &mut agent,
                progress(open.transfer_id, received.len() as u64, ""),
            );
        }
        assert_eq!(sending.join().unwrap(), Ok(()));
        assert_eq!(
            received, content,
            "the app gets the file as it is on the host"
        );
    });
}

fn drag_out_here() -> dsa_host_core::drag::DragOut {
    dsa_host_core::drag::DragOut {
        task_id: 7,
        x: 300.0,
        y: 2.0,
        tool: dsa_protocol::control::PointerTool::Mouse as i32,
        pointer_id: 0,
    }
}

#[test]
fn fr29_a_file_dragged_out_of_a_window_reaches_the_host_whole_with_its_name() {
    use dsa_host_core::drag::{DragContent, DragRead};
    use dsa_protocol::control::{DragData, DragItem, DragOffer};
    use std::io::Read;
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let content: Vec<u8> = (0..300_000u32).map(|i| (i % 253) as u8).collect();
    let asking = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.drag_out(drag_out_here(), WAIT))
    };
    let request_id = loop {
        if let Body::DragOut(out) = next_body(&mut agent) {
            assert_eq!((out.task_id, out.x, out.y), (7, 300.0, 2.0));
            break out.request_id;
        }
    };
    send_body(
        &mut agent,
        Body::DragOffer(DragOffer {
            request_id,
            drag_id: 5,
            items: vec![DragItem {
                name: "../holiday.jpg".into(),
                size: content.len() as u64,
                size_known: true,
                mime_type: "image/jpeg".into(),
                ..Default::default()
            }],
            error: String::new(),
        }),
    );
    let offer = asking.join().unwrap().expect("an answer").expect("a drag");
    assert_eq!(offer.drag_id, 5);
    assert_eq!(
        offer.items[0].name, "holiday.jpg",
        "no path reaches the host"
    );
    assert_eq!(offer.items[0].size, Some(content.len() as u64));

    let copying = {
        let session = Arc::clone(&session);
        thread::spawn(move || {
            let mut copied = Vec::new();
            DragContent::new(5, 0, |read: DragRead| {
                session.read_drag(read, WAIT).map_err(|e| e.to_string())
            })
            .read_to_end(&mut copied)
            .map(|_| copied)
        })
    };
    loop {
        let Body::DragRead(read) = next_body(&mut agent) else {
            continue;
        };
        assert_eq!((read.drag_id, read.item), (5, 0));
        let start = read.offset as usize;
        let end = (start + read.length as usize).min(content.len());
        send_body(
            &mut agent,
            Body::DragData(DragData {
                request_id: read.request_id,
                data: content[start..end].to_vec(),
                end: end == content.len(),
                error: String::new(),
            }),
        );
        if end == content.len() {
            break;
        }
    }
    assert_eq!(copying.join().unwrap().expect("the copy"), content);

    session.end_drag(5, true).expect("the end goes out");
    loop {
        if let Body::DragEnd(end) = next_body(&mut agent) {
            assert_eq!((end.drag_id, end.dropped), (5, true));
            break;
        }
    }
}

#[test]
fn fr29_nothing_dragged_is_no_drag_and_a_cancelled_one_is_ended() {
    use dsa_protocol::control::DragOffer;
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let asking = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.drag_out(drag_out_here(), WAIT))
    };
    let request_id = loop {
        if let Body::DragOut(out) = next_body(&mut agent) {
            break out.request_id;
        }
    };
    send_body(
        &mut agent,
        Body::DragOffer(DragOffer {
            request_id,
            ..Default::default()
        }),
    );
    assert_eq!(asking.join().unwrap(), Ok(None));

    // An offer nobody waits for any more is ended at once, so the guest holds nothing.
    send_body(
        &mut agent,
        Body::DragOffer(DragOffer {
            request_id: 999,
            drag_id: 8,
            items: vec![dsa_protocol::control::DragItem {
                is_text: true,
                text: "hi".into(),
                ..Default::default()
            }],
            error: String::new(),
        }),
    );
    loop {
        if let Body::DragEnd(end) = next_body(&mut agent) {
            assert_eq!((end.drag_id, end.dropped), (8, false));
            break;
        }
    }
}

fn file_sharing(on: bool, folders: &[&str]) -> Body {
    Body::FileSharing(dsa_protocol::control::FileSharing {
        on,
        port: dsa_host_core::file_share::SHARE_PORT,
        location: "Host".into(),
        folders: folders.iter().map(|f| f.to_string()).collect(),
    })
}

#[test]
fn fr29_the_guest_is_told_to_mount_the_shared_folders_and_to_take_them_away() {
    use dsa_host_core::file_share::PlainShareHost;
    let (session, mut agent, _launcher) = active_session();
    let (listener, _connector) = memory::pair();
    session.serve_file_shares(
        Box::new(listener),
        Arc::new(PlainShareHost {
            folders: Vec::new(),
            location: "Host".into(),
        }),
    );
    session.share_folders(true, vec!["Documents".into(), "Pictures".into()]);
    assert_eq!(
        next_body(&mut agent),
        file_sharing(true, &["Documents", "Pictures"])
    );
    // A guest boots with nothing mounted, so an agent that connects again is told.
    report_boot(&mut agent, true, false);
    thread::sleep(Duration::from_millis(50));
    report_boot(&mut agent, true, true);
    assert_eq!(
        next_body(&mut agent),
        file_sharing(true, &["Documents", "Pictures"])
    );
    // Turned off, the guest takes every folder away.
    session.share_folders(false, vec!["Documents".into(), "Pictures".into()]);
    assert_eq!(next_body(&mut agent), file_sharing(false, &[]));
    assert!(!session.sharing().shares("Documents"));
}

#[test]
fn fr29_the_guest_is_told_nothing_of_folders_while_the_host_serves_none() {
    let (session, mut agent, _launcher) = active_session();
    session.share_folders(true, vec!["Documents".into()]);
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr29_android_files_open_a_loopback_address_and_have_the_guest_serve_them() {
    use std::io::{Read, Write};
    let (session, mut agent, _launcher) = active_session();
    assert!(
        session.android_files().is_err(),
        "a host that cannot reach the guest's files has no address"
    );
    let (connect, reached) = fake_adbd();
    session.set_android_files_connect(connect);
    let url = session.android_files().unwrap();
    let Body::AndroidFiles(files) = next_body(&mut agent) else {
        panic!("expected the guest to be told to serve its files");
    };
    assert_eq!(files.port, dsa_host_core::file_share::ANDROID_FILES_PORT);
    assert_eq!(files.token.len(), 64);
    let rest = url.strip_prefix("http://127.0.0.1:").expect(&url);
    let (port, path) = rest.split_once('/').unwrap();
    assert_eq!(path, format!("{}/", files.token));
    assert_eq!(session.android_files().unwrap(), url, "the address holds");

    let mut client =
        std::net::TcpStream::connect(("127.0.0.1", port.parse::<u16>().unwrap())).unwrap();
    client.set_read_timeout(Some(WAIT)).unwrap();
    client.write_all(b"PROPFIND").unwrap();
    let mut answer = [0u8; 8];
    client.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"PROPFIND");
    assert_eq!(*reached.lock().unwrap(), vec!["fake-vm".to_string()]);
}

#[test]
fn fr29_android_files_can_be_served_under_the_users_own_key() {
    let (session, mut agent, _launcher) = active_session();
    let (connect, _reached) = fake_adbd();
    session.set_android_files_connect(connect);
    let first = session.android_files().unwrap();
    let Body::AndroidFiles(_) = next_body(&mut agent) else {
        panic!("expected the guest to be told to serve its files");
    };
    // The files door asks for the user's key: the guest is told it, on the same port.
    let keyed = session.android_files_with_token("feedface").unwrap();
    let Body::AndroidFiles(files) = next_body(&mut agent) else {
        panic!("expected the guest to be told the new token");
    };
    assert_eq!(files.token, "feedface");
    assert!(keyed.ends_with("/feedface/"), "{keyed}");
    assert_eq!(
        first.rsplit_once('/').map(|_| first.split('/').nth(2)),
        keyed.rsplit_once('/').map(|_| keyed.split('/').nth(2)),
        "the loopback port holds"
    );
    // Asking again with the same key tells the guest nothing new.
    assert_eq!(session.android_files_with_token("feedface").unwrap(), keyed);
    assert_eq!(session.android_files().unwrap(), keyed);
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr9_a_file_the_agent_cannot_take_is_refused_with_its_reason() {
    use dsa_host_core::files::{FileAction, FileTransfer};
    let (session, mut agent, _launcher) = active_session();
    let file = FileTransfer {
        name: "a.bin".into(),
        content: vec![1, 2, 3],
        action: FileAction::Send,
        package: String::new(),
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
        package: String::new(),
    };
    assert!(matches!(
        session.open_file(&large),
        Err(SessionError::FileRefused(_))
    ));
    let named = FileTransfer {
        name: "../etc/passwd".into(),
        content: vec![1],
        action: FileAction::View,
        package: String::new(),
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
        package: String::new(),
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
            ..Default::default()
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

fn shortcut(id: &str, label: &str, rank: u32) -> dsa_protocol::control::AppShortcut {
    dsa_protocol::control::AppShortcut {
        id: id.into(),
        short_label: label.into(),
        icon_png: vec![0x89, b'P', b'N', b'G', rank as u8],
        rank,
        dynamic: true,
        ..Default::default()
    }
}

/// The installed apps the watcher hears of next.
fn next_installed(watch: &dsa_host_core::tasks::TaskWatch) -> Vec<dsa_host_core::tasks::AppInfo> {
    loop {
        if let TaskEvent::Installed(apps) = watch.events.recv_timeout(WAIT).expect("the apps") {
            return apps;
        }
    }
}

#[test]
fn fr7_app_shortcuts_are_jump_list_tasks_and_desktop_shortcuts() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    send_body(
        &mut agent,
        Body::InstalledApp(dsa_protocol::control::InstalledApp {
            package_name: "com.example.chat".into(),
            label: "Chat".into(),
            ..Default::default()
        }),
    );
    send_body(
        &mut agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec!["com.example.chat".into()],
        }),
    );
    assert!(next_installed(&watch)[0].shortcuts.is_empty());

    // The agent reports the app's shortcuts; the list of installed apps, which the window
    // manager builds the jump list from, carries them with their names and icons.
    send_body(
        &mut agent,
        Body::AppShortcuts(dsa_protocol::control::AppShortcuts {
            package_name: "com.example.chat".into(),
            shortcuts: vec![
                shortcut("new-chat", "New chat", 0),
                shortcut("ana", "Ana", 1),
            ],
        }),
    );
    let apps = next_installed(&watch);
    let tasks = dsa_host_core::tasks::jump_list_shortcuts(&apps[0].shortcuts);
    assert_eq!(
        tasks
            .iter()
            .map(|s| (s.id.as_str(), s.label()))
            .collect::<Vec<_>>(),
        vec![("new-chat", "New chat"), ("ana", "Ana")]
    );
    assert_eq!(tasks[0].icon_png, vec![0x89, b'P', b'N', b'G', 0]);

    // The app changes them; the jump list follows.
    send_body(
        &mut agent,
        Body::AppShortcuts(dsa_protocol::control::AppShortcuts {
            package_name: "com.example.chat".into(),
            shortcuts: vec![shortcut("new-chat", "New chat", 0)],
        }),
    );
    assert_eq!(next_installed(&watch)[0].shortcuts.len(), 1);

    // A jump list task starts its shortcut in Android.
    session
        .request(TaskRequest::LaunchShortcut {
            package_name: "com.example.chat".into(),
            shortcut_id: "new-chat".into(),
        })
        .expect("the launch");
    loop {
        if let Body::LaunchShortcut(launch) = next_body(&mut agent) {
            assert_eq!(launch.package_name, "com.example.chat");
            assert_eq!(launch.shortcut_id, "new-chat");
            break;
        }
    }

    // The app asks to pin a shortcut: the window manager hears of it, asks the user, and the
    // answer reaches the agent.
    send_body(
        &mut agent,
        Body::PinShortcutRequest(dsa_protocol::control::PinShortcutRequest {
            request_id: 7,
            package_name: "com.example.chat".into(),
            app_label: "Chat".into(),
            shortcut: Some(shortcut("ana", "Ana", 1)),
        }),
    );
    let request = loop {
        if let TaskEvent::PinShortcut(request) = watch.events.recv_timeout(WAIT).expect("pin") {
            break request;
        }
    };
    assert_eq!(request.request_id, 7);
    assert_eq!(request.app_label, "Chat");
    assert_eq!(request.shortcut.label(), "Ana");
    session
        .request(TaskRequest::AnswerPinShortcut {
            request_id: 7,
            accepted: true,
        })
        .expect("the answer");
    loop {
        if let Body::PinShortcutAnswer(answer) = next_body(&mut agent) {
            assert_eq!(answer.request_id, 7);
            assert!(answer.accepted);
            break;
        }
    }

    // Uninstalled: the app and its shortcuts leave the list, so the host removes its jump
    // list and desktop shortcuts.
    send_body(
        &mut agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec![],
        }),
    );
    assert!(next_installed(&watch).is_empty());
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

fn game_libraries(abi: &str, names: &[&str]) -> dsa_protocol::control::NativeLibraries {
    dsa_protocol::control::NativeLibraries {
        abi: abi.into(),
        names: names.iter().map(|name| name.to_string()).collect(),
    }
}

/// Lists one app, `com.example.game`, whose x86_64 libraries leave out one of its arm64 ones.
fn list_game(agent: &mut Box<dyn Connection>, watch: &dsa_host_core::tasks::TaskWatch) {
    send_body(
        agent,
        Body::InstalledApp(dsa_protocol::control::InstalledApp {
            package_name: "com.example.game".into(),
            label: "Game".into(),
            primary_abi: "x86_64".into(),
            native_libraries: vec![
                game_libraries("arm64-v8a", &["libmain.so", "libunity.so"]),
                game_libraries("x86_64", &["libmain.so"]),
            ],
            ..Default::default()
        }),
    );
    send_body(
        agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec!["com.example.game".into()],
        }),
    );
    loop {
        if let TaskEvent::Installed(apps) = watch.events.recv_timeout(WAIT).expect("the apps") {
            assert_eq!(apps[0].primary_abi, "x86_64");
            assert!(apps[0].native_libraries.has("arm64-v8a"));
            break;
        }
    }
}

fn game_crash(arm64_supported: bool) -> Body {
    Body::NativeLibraryCrash(dsa_protocol::control::NativeLibraryCrash {
        package_name: "com.example.game".into(),
        detail: "java.lang.UnsatisfiedLinkError: dlopen failed: library \"libunity.so\" not found"
            .into(),
        arm64_supported,
    })
}

#[test]
fn fr25_a_native_library_crash_offers_to_run_the_app_as_arm_and_a_click_reinstalls_it() {
    use dsa_host_core::native_abi::Notice;
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    let tasks = session.watch_tasks();
    list_game(&mut agent, &tasks);
    // Without a translator there is nothing to offer.
    send_body(&mut agent, game_crash(false));
    nothing_for_the_watcher(&watch);
    send_body(&mut agent, game_crash(true));
    let offer = match watch.recv_timeout(WAIT).unwrap() {
        NotificationEvent::Posted(offer) => offer,
        other => panic!("expected the offer, not {other:?}"),
    };
    assert_eq!(offer.key, Notice::Offer.key("com.example.game"));
    assert_eq!(offer.app_label, "Game");
    session
        .notification_action(NotificationAction::Open {
            key: offer.key.clone(),
        })
        .expect("taken");
    let reinstall = loop {
        if let Body::ReinstallApp(reinstall) = next_body(&mut agent) {
            break reinstall;
        }
    };
    assert_eq!(reinstall.package_name, "com.example.game");
    assert_eq!(reinstall.abi, "arm64-v8a");
    assert!(
        !reinstall.erase_data,
        "the data stays unless the user says otherwise"
    );
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        NotificationEvent::Removed { key: offer.key }
    );
    send_body(
        &mut agent,
        Body::ReinstallResult(dsa_protocol::control::ReinstallResult {
            request_id: reinstall.request_id,
            package_name: "com.example.game".into(),
            status: dsa_protocol::control::ReinstallStatus::Success as i32,
            message: String::new(),
            primary_abi: "arm64-v8a".into(),
        }),
    );
    match watch.recv_timeout(WAIT).unwrap() {
        NotificationEvent::Posted(done) => {
            assert_eq!(done.key, Notice::Done.key("com.example.game"))
        }
        other => panic!("expected the news that it worked, not {other:?}"),
    }
}

#[test]
fn fr25_an_app_that_runs_as_arm_or_has_no_arm_code_is_offered_nothing() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    let tasks = session.watch_tasks();
    list_game(&mut agent, &tasks);
    // A package that is not installed, and one with no ARM code, are offered nothing.
    send_body(
        &mut agent,
        Body::NativeLibraryCrash(dsa_protocol::control::NativeLibraryCrash {
            package_name: "com.example.missing".into(),
            detail: "dlopen failed".into(),
            arm64_supported: true,
        }),
    );
    send_body(
        &mut agent,
        Body::InstalledApp(dsa_protocol::control::InstalledApp {
            package_name: "com.example.game".into(),
            label: "Game".into(),
            primary_abi: "arm64-v8a".into(),
            native_libraries: vec![game_libraries("arm64-v8a", &["libmain.so"])],
            ..Default::default()
        }),
    );
    send_body(
        &mut agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec!["com.example.game".into()],
        }),
    );
    loop {
        if let TaskEvent::Installed(apps) = tasks.events.recv_timeout(WAIT).expect("the apps") {
            if apps[0].primary_abi == "arm64-v8a" {
                break;
            }
        }
    }
    send_body(&mut agent, game_crash(true));
    nothing_for_the_watcher(&watch);
}

#[test]
fn fr25_a_32_bit_build_that_ends_at_start_is_told_to_get_its_64_bit_build() {
    use dsa_host_core::native_abi::Notice;
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_notifications();
    let tasks = session.watch_tasks();
    // Chrome's build made to run as 32-bit code, as Google Play delivers it to a device
    // without 32-bit code: the same library under x86 and x86_64, and no ARM code.
    send_body(
        &mut agent,
        Body::InstalledApp(dsa_protocol::control::InstalledApp {
            package_name: "com.android.chrome".into(),
            label: "Chrome".into(),
            primary_abi: "x86_64".into(),
            version_code: 495106126,
            native_libraries: vec![
                game_libraries("x86", &["libmonochrome.so"]),
                game_libraries("x86_64", &["libmonochrome.so"]),
            ],
            ..Default::default()
        }),
    );
    send_body(
        &mut agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec!["com.android.chrome".into()],
        }),
    );
    loop {
        if let TaskEvent::Installed(apps) = tasks.events.recv_timeout(WAIT).expect("the apps") {
            if apps
                .iter()
                .any(|app| app.package_name == "com.android.chrome")
            {
                break;
            }
        }
    }
    send_body(
        &mut agent,
        Body::NativeLibraryCrash(dsa_protocol::control::NativeLibraryCrash {
            package_name: "com.android.chrome".into(),
            detail: "java.lang.RuntimeException: Starting in 64-bit mode requires the 64-bit \
                     native library."
                .into(),
            arm64_supported: true,
        }),
    );
    let notice = match watch.recv_timeout(WAIT).unwrap() {
        NotificationEvent::Posted(notice) => notice,
        other => panic!("expected the notice, not {other:?}"),
    };
    assert_eq!(notice.key, Notice::Needs64Bit.key("com.android.chrome"));
    assert_eq!(notice.app_label, "Chrome");
    assert!(notice.text.contains("x86_64 build"), "{}", notice.text);
    // A click removes it and installs nothing.
    session
        .notification_action(NotificationAction::Open {
            key: notice.key.clone(),
        })
        .expect("taken");
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        NotificationEvent::Removed { key: notice.key }
    );
}

#[test]
fn fr23_installed_apps_carry_their_version_dates_sizes_and_kind() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_tasks();
    send_body(
        &mut agent,
        Body::InstalledApp(dsa_protocol::control::InstalledApp {
            package_name: "com.example.notes".into(),
            label: "Notes".into(),
            icon_png: vec![0x89, b'P', b'N', b'G'],
            version_name: "4.1".into(),
            version_code: 41,
            first_install_time_ms: 1_700_000_000_000,
            last_update_time_ms: 1_710_000_000_000,
            system_app: false,
            app_bytes: 3_000_000,
            data_bytes: 20_000,
            cache_bytes: 1_000,
            ..Default::default()
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
            let notes = &apps[0];
            assert_eq!(notes.version_name, "4.1");
            assert_eq!(notes.version_code, 41);
            assert_eq!(notes.first_install_time_ms, 1_700_000_000_000);
            assert_eq!(notes.last_update_time_ms, 1_710_000_000_000);
            assert!(!notes.system_app);
            assert_eq!(notes.total_bytes(), 3_021_000);
            break;
        }
    }
}

#[test]
fn fr23_a_refresh_waits_for_the_agents_fresh_list() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let refreshing = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.refresh_apps(WAIT))
    };
    loop {
        if let Body::RefreshApps(_) = next_body(&mut agent) {
            break;
        }
    }
    send_body(
        &mut agent,
        Body::InstalledApps(dsa_protocol::control::InstalledApps {
            package_names: vec![],
        }),
    );
    refreshing.join().unwrap().expect("the fresh list");
}

#[test]
fn fr23_app_info_and_manage_apps_ask_android_for_its_settings() {
    let (session, mut agent, _launcher) = active_session();
    session
        .open_app_settings(Some("com.example.notes"))
        .expect("asked");
    session.open_app_settings(None).expect("asked");
    let mut opened = Vec::new();
    while opened.len() < 2 {
        if let Body::OpenAppSettings(open) = next_body(&mut agent) {
            opened.push(open.package_name);
        }
    }
    assert_eq!(opened, vec!["com.example.notes".to_string(), String::new()]);
}

#[test]
fn fr23_an_uninstall_carries_androids_answer() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    for (package, status, expected) in [
        (
            "com.example.notes",
            dsa_protocol::control::UninstallStatus::Success,
            UninstallStatus::Uninstalled,
        ),
        (
            "com.android.settings",
            dsa_protocol::control::UninstallStatus::SystemApp,
            UninstallStatus::SystemApp,
        ),
    ] {
        let uninstalling = {
            let session = Arc::clone(&session);
            thread::spawn(move || session.uninstall_app(package, WAIT))
        };
        let request_id = loop {
            if let Body::UninstallApp(request) = next_body(&mut agent) {
                assert_eq!(request.package_name, package);
                break request.request_id;
            }
        };
        send_body(
            &mut agent,
            Body::UninstallResult(dsa_protocol::control::UninstallResult {
                request_id,
                package_name: package.into(),
                status: status as i32,
                message: "from Android".into(),
            }),
        );
        let outcome = uninstalling.join().unwrap().expect("an answer");
        assert_eq!(outcome.status, expected);
        assert_eq!(outcome.package_name, package);
    }
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
fn fr11_a_split_install_names_its_parts_and_sends_them_one_after_another() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let base = apk_bytes(apk::CHUNK_BYTES * 2 + 5);
    let split = apk_bytes(1000);
    let obb = apk_bytes(77);
    let parts = vec![
        part("base.apk", base.len(), false),
        part("split_config.x86_64.apk", split.len(), false),
        part("main.42.com.example.notes.obb", obb.len(), true),
    ];
    let content = [base, split, obb].concat();
    let installing = {
        let session = Arc::clone(&session);
        let (parts, content) = (parts.clone(), content.clone());
        let abi = Some("arm64-v8a");
        thread::spawn(move || session.install_parts("notes.xapk", &parts, abi, &mut &content[..]))
    };
    let announced = match next_install_body(&mut agent) {
        Body::InstallApk(install) => install,
        other => panic!("expected an install, got {other:?}"),
    };
    assert_eq!(announced.name, "notes.xapk");
    assert_eq!(announced.size, content.len() as u64);
    assert_eq!(announced.abi, "arm64-v8a");
    let expected: Vec<_> = parts.iter().map(InstallPart::to_control).collect();
    assert_eq!(announced.parts, expected);
    let mut received = Vec::new();
    while received.len() < content.len() {
        match next_install_body(&mut agent) {
            Body::ApkChunk(chunk) => {
                assert_eq!(chunk.offset, received.len() as u64, "in order");
                received.extend_from_slice(&chunk.data);
                send_body(
                    &mut agent,
                    install_progress(announced.transfer_id, received.len() as u64),
                );
            }
            other => panic!("expected a piece of the install, got {other:?}"),
        }
    }
    assert!(received == content, "the files arrived changed");
    send_body(
        &mut agent,
        install_result(
            announced.transfer_id,
            dsa_protocol::control::InstallStatus::Success,
            "com.example.notes",
            "",
        ),
    );
    let outcome = installing.join().unwrap().expect("installed");
    assert!(outcome.installed());
}

#[test]
fn fr11_a_split_install_android_cannot_take_is_refused_before_anything_is_sent() {
    let (session, mut agent, _launcher) = active_session();
    let only_obb = [part("main.1.com.example.notes.obb", 3, true)];
    match session.install_parts("notes.xapk", &only_obb, None, &mut &b"obb"[..]) {
        Err(SessionError::InstallRefused(why)) => assert!(why.contains("no APK"), "{why}"),
        other => panic!("expected the install to be refused, got {other:?}"),
    }
    nothing_for_the_guest(&mut agent);
}

fn part(name: &str, size: usize, obb: bool) -> InstallPart {
    InstallPart {
        name: name.into(),
        size: size as u64,
        obb,
    }
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
        app_label: "Clock App".into(),
        reconfigurable: false,
    }
}

/// The agent's side of a widget's settings opened again: the long press's menu asks for
/// them, the agent runs the setup again for the same widget, and the caller hears how it
/// ended. Only a widget that says it can be set up again is asked of the agent.
#[test]
fn fr12_long_pressing_a_widget_edits_it_like_a_home_screen() {
    let (session, mut agent, _launcher) = active_session();
    let session = Arc::new(session);
    let weather = dsa_protocol::control::WidgetProvider {
        provider: "com.example.weather/.WeatherWidget".into(),
        needs_configuration: true,
        reconfigurable: true,
        ..clock_widget()
    };
    send_body(
        &mut agent,
        Body::WidgetUpdated(dsa_protocol::control::WidgetUpdated {
            widget_id: 21,
            provider: Some(weather.clone()),
        }),
    );
    send_body(&mut agent, widget_updated(12));
    let deadline = std::time::Instant::now() + WAIT;
    while session.widgets().len() < 2 {
        assert!(
            std::time::Instant::now() < deadline,
            "the widgets never came"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let settable = session
        .widgets()
        .into_iter()
        .find(|(id, _)| *id == 21)
        .map(|(_, provider)| provider)
        .unwrap();
    assert!(settable.reconfigurable, "the flag reaches the host");

    // The user finishes the setup.
    let asking = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.reconfigure_widget(21, WAIT))
    };
    let request_id = match next_body(&mut agent) {
        Body::ReconfigureWidget(asked) => {
            assert_eq!(asked.widget_id, 21);
            asked.request_id
        }
        other => panic!("expected the widget's settings to be asked for, got {other:?}"),
    };
    send_body(
        &mut agent,
        Body::WidgetReconfigured(dsa_protocol::control::WidgetReconfigured {
            request_id,
            widget_id: 21,
            finished: true,
            error: String::new(),
        }),
    );
    assert_eq!(asking.join().unwrap(), Ok(()));

    // The user leaves it: the reason reaches the caller, and the widget stays.
    let leaving = {
        let session = Arc::clone(&session);
        thread::spawn(move || session.reconfigure_widget(21, WAIT))
    };
    let request_id = match next_body(&mut agent) {
        Body::ReconfigureWidget(asked) => asked.request_id,
        other => panic!("expected the widget's settings to be asked for, got {other:?}"),
    };
    send_body(
        &mut agent,
        Body::WidgetReconfigured(dsa_protocol::control::WidgetReconfigured {
            request_id,
            widget_id: 21,
            finished: false,
            error: "the user canceled the widget's settings".into(),
        }),
    );
    assert_eq!(
        leaving.join().unwrap(),
        Err(SessionError::WidgetRefused(
            "the user canceled the widget's settings".into()
        ))
    );
    assert_eq!(session.widgets().len(), 2);

    // The clock offers no settings, and an unknown widget has none: neither reaches the agent.
    assert!(matches!(
        session.reconfigure_widget(12, WAIT),
        Err(SessionError::WidgetRefused(_))
    ));
    assert!(matches!(
        session.reconfigure_widget(99, WAIT),
        Err(SessionError::WidgetRefused(_))
    ));
    session.remove_widget(12).unwrap();
    assert_eq!(
        next_body(&mut agent),
        Body::RemoveWidget(dsa_protocol::control::RemoveWidget { widget_id: 12 }),
        "nothing was sent for the widgets that have no settings"
    );
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

fn music_app(title: &str) -> Body {
    Body::BackgroundApp(dsa_protocol::control::BackgroundApp {
        package_name: MUSIC.into(),
        app_label: "Music".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
        title: title.into(),
        text: "Artist".into(),
        actions: vec!["Pause".into(), "Next".into()],
    })
}

fn music_session() -> Body {
    Body::MediaSession(dsa_protocol::control::MediaSession {
        package_name: MUSIC.into(),
        app_label: "Music".into(),
        title: "Song".into(),
        artist: "Artist".into(),
        playback: dsa_protocol::control::MediaPlayback::Playing as i32,
        can_pause: true,
        can_next: true,
        ..Default::default()
    })
}

const MUSIC: &str = "com.example.music";

#[test]
fn fr24_an_app_that_runs_in_the_background_reaches_the_watcher_and_goes_when_its_service_ends() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_background();
    assert!(watch.apps.is_empty());
    send_body(&mut agent, music_app("Playing"));
    match watch.events.recv_timeout(WAIT).unwrap() {
        BackgroundEvent::App(app) => {
            assert_eq!(app.package_name, MUSIC);
            assert_eq!(app.title, "Playing");
            assert_eq!(app.actions, vec!["Pause".to_string(), "Next".to_string()]);
        }
        other => panic!("expected the app, got {other:?}"),
    }
    send_body(&mut agent, music_session());
    match watch.events.recv_timeout(WAIT).unwrap() {
        BackgroundEvent::Media(media) => {
            assert_eq!(media.title, "Song");
            assert_eq!(media.playback, Playback::Playing);
        }
        other => panic!("expected the media session, got {other:?}"),
    }
    assert_eq!(session.background_apps().0.len(), 1);
    send_body(
        &mut agent,
        Body::BackgroundAppGone(dsa_protocol::control::BackgroundAppGone {
            package_name: MUSIC.into(),
        }),
    );
    assert_eq!(
        watch.events.recv_timeout(WAIT).unwrap(),
        BackgroundEvent::AppRemoved {
            package_name: MUSIC.into()
        }
    );
    // A malformed package name is dropped, and the session goes on.
    send_body(
        &mut agent,
        Body::BackgroundApp(dsa_protocol::control::BackgroundApp {
            package_name: "../evil".into(),
            ..Default::default()
        }),
    );
    if let Ok(event) = watch.events.recv_timeout(Duration::from_millis(300)) {
        panic!("the watcher heard of {event:?}");
    }
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr24_the_background_apps_of_a_guest_that_stops_are_removed() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_background();
    send_body(&mut agent, music_app("Playing"));
    send_body(&mut agent, music_session());
    watch.events.recv_timeout(WAIT).unwrap();
    watch.events.recv_timeout(WAIT).unwrap();
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    let mut removed = vec![
        watch.events.recv_timeout(WAIT).unwrap(),
        watch.events.recv_timeout(WAIT).unwrap(),
    ];
    removed.sort_by_key(|event| format!("{event:?}"));
    assert_eq!(
        removed,
        vec![
            BackgroundEvent::AppRemoved {
                package_name: MUSIC.into()
            },
            BackgroundEvent::MediaRemoved {
                package_name: MUSIC.into()
            },
        ]
    );
    assert_eq!(session.background_apps(), (Vec::new(), Vec::new()));
}

#[test]
fn fr24_stop_and_an_action_reach_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_background();
    send_body(&mut agent, music_app("Playing"));
    watch.events.recv_timeout(WAIT).unwrap();
    session
        .background_action(BackgroundRequest::App(
            MUSIC.into(),
            BackgroundCommand::Action(1),
        ))
        .expect("taken");
    assert_eq!(
        next_body(&mut agent),
        Body::BackgroundAppAction(dsa_protocol::control::BackgroundAppAction {
            package_name: MUSIC.into(),
            command: dsa_protocol::control::BackgroundAppCommand::Action as i32,
            action: 1,
        })
    );
    // A stop goes for any app, known or not.
    session
        .background_action(BackgroundRequest::App(
            "com.example.other".into(),
            BackgroundCommand::Stop,
        ))
        .expect("taken");
    assert_eq!(
        next_body(&mut agent),
        Body::BackgroundAppAction(dsa_protocol::control::BackgroundAppAction {
            package_name: "com.example.other".into(),
            command: dsa_protocol::control::BackgroundAppCommand::Stop as i32,
            action: 0,
        })
    );
    // Anything else for an app that does not run in the background is ignored.
    session
        .background_action(BackgroundRequest::App(
            "com.example.other".into(),
            BackgroundCommand::Open,
        ))
        .expect("ignored");
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr24_a_media_key_for_a_frozen_guest_thaws_it_without_turning_the_screen_on() {
    let (session, mut agent, launcher) = lifecycle_session(None);
    let watch = session.watch_background();
    send_body(&mut agent, music_session());
    watch.events.recv_timeout(WAIT).unwrap();
    session.freeze().expect("frozen");
    assert_eq!(next_body(&mut agent), screen(false));
    session
        .background_action(BackgroundRequest::Media(MUSIC.into(), MediaCommand::Pause))
        .expect("taken while frozen");
    assert_eq!(launcher.0.lock().unwrap().thaws, 1);
    loop {
        match next_body(&mut agent) {
            Body::MediaControl(control) => {
                assert_eq!(control.package_name, MUSIC);
                assert_eq!(
                    control.command,
                    dsa_protocol::control::MediaCommand::Pause as i32
                );
                break;
            }
            Body::ScreenState(state) => panic!("the screen changed: {state:?}"),
            // What waited for the thaw (the clock, power) may come first.
            _ => {}
        }
    }
    assert_eq!(session.state(), SessionState::Dozing);
}

#[test]
fn fr24_acting_on_a_background_app_of_a_stopped_guest_says_it_is_not_running() {
    let (session, _connector) = session_with(FakeLauncher::default(), WAIT);
    assert_eq!(
        session.background_action(BackgroundRequest::App(
            MUSIC.into(),
            BackgroundCommand::Stop
        )),
        Err(SessionError::NotActive(SessionState::Stopped))
    );
}
fn developer_mode(on: bool) -> Body {
    Body::DeveloperMode(dsa_protocol::control::DeveloperMode { on })
}

/// A guest adbd that echoes what it is sent, and the VM IDs it was reached through.
fn fake_adbd() -> (dsa_host_core::session::AdbConnect, Arc<Mutex<Vec<String>>>) {
    use std::io::{Read, Write};
    let adbd = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = adbd.local_addr().unwrap();
    thread::spawn(move || {
        for mut stream in adbd.incoming().flatten() {
            thread::spawn(move || {
                let mut buf = [0u8; 64];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 || stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    let reached = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&reached);
    let connect: dsa_host_core::session::AdbConnect = Arc::new(move |vm_id: &str| {
        seen.lock().unwrap().push(vm_id.to_string());
        Ok(Box::new(std::net::TcpStream::connect(address)?)
            as Box<dyn dsa_host_core::adb_bridge::BridgeStream>)
    });
    (connect, reached)
}

#[test]
fn fr20_developer_mode_starts_adbd_and_opens_a_loopback_port_that_reaches_it() {
    use std::io::{Read, Write};
    let (session, mut agent, _launcher) = active_session();
    assert_eq!(
        session.adb_address(),
        None,
        "off by default: nothing listens"
    );
    let (connect, reached) = fake_adbd();
    session.set_adb_connect(connect);
    session.developer_mode(true, 0).unwrap();
    assert_eq!(next_body(&mut agent), developer_mode(true));
    let address = session
        .adb_address()
        .expect("a port while developer mode is on");
    assert!(address.ip().is_loopback());

    let mut adb = std::net::TcpStream::connect(address).unwrap();
    adb.set_read_timeout(Some(WAIT)).unwrap();
    adb.write_all(b"CNXN").unwrap();
    let mut answer = [0u8; 4];
    adb.read_exact(&mut answer).unwrap();
    assert_eq!(&answer, b"CNXN");
    assert_eq!(*reached.lock().unwrap(), vec!["fake-vm".to_string()]);

    // An agent that connects again is told, since a guest boots with adbd stopped.
    report_boot(&mut agent, true, false);
    thread::sleep(Duration::from_millis(50));
    report_boot(&mut agent, true, true);
    assert_eq!(next_body(&mut agent), developer_mode(true));

    session.developer_mode(false, 0).unwrap();
    assert_eq!(next_body(&mut agent), developer_mode(false));
    assert_eq!(session.adb_address(), None);
    assert!(
        std::net::TcpStream::connect(address).is_err(),
        "the port is closed"
    );
}

#[test]
fn fr20_developer_mode_attaches_its_port_to_the_adb_server_and_detaches_it_when_off() {
    use dsa_host_core::adb_discovery::{fake::FakeAdbServer, AutoConnectState};
    let (session, mut agent, _launcher) = active_session();
    let (connect, _) = fake_adbd();
    session.set_adb_connect(connect);
    let server = FakeAdbServer::start();
    session.set_adb_server(server.link());
    assert_eq!(session.adb_auto_connect_state(), AutoConnectState::Off);

    session.developer_mode(true, 0).unwrap();
    assert_eq!(next_body(&mut agent), developer_mode(true));
    let serial = session.adb_address().unwrap().to_string();
    wait_until("the port is attached to the adb server", || {
        session.adb_auto_connect_state() == AutoConnectState::Attached
    });
    assert_eq!(
        server.requests_starting("host:connect:"),
        vec![format!("host:connect:{serial}")]
    );

    session.developer_mode(false, 0).unwrap();
    wait_until("the adb server is asked to disconnect", || {
        !server.requests_starting("host:disconnect:").is_empty()
    });
    assert_eq!(
        server.requests_starting("host:disconnect:"),
        vec![format!("host:disconnect:{serial}")]
    );
    wait_until("auto connect is off", || {
        session.adb_auto_connect_state() == AutoConnectState::Off
    });
}

#[test]
fn fr20_with_auto_connect_off_developer_mode_sends_the_adb_server_nothing() {
    use dsa_host_core::adb_discovery::{fake::FakeAdbServer, AutoConnectState};
    let (session, mut agent, _launcher) = active_session();
    let (connect, _) = fake_adbd();
    session.set_adb_connect(connect);
    let server = FakeAdbServer::start();
    session.adb_auto_connect(false);
    session.set_adb_server(server.link());
    session.developer_mode(true, 0).unwrap();
    assert_eq!(next_body(&mut agent), developer_mode(true));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(session.adb_auto_connect_state(), AutoConnectState::Off);
    session.developer_mode(false, 0).unwrap();
    thread::sleep(Duration::from_millis(100));
    assert_eq!(server.connections(), 0);
}

#[test]
fn fr20_developer_mode_advertises_its_port_over_mdns_while_it_is_on() {
    struct Advert(u16, Arc<Mutex<Vec<String>>>);
    impl Drop for Advert {
        fn drop(&mut self) {
            self.1.lock().unwrap().push(format!("end {}", self.0));
        }
    }
    let (session, mut agent, _launcher) = active_session();
    let (connect, _) = fake_adbd();
    session.set_adb_connect(connect);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    session.set_adb_advertise(Arc::new(move |port: u16| {
        log.lock().unwrap().push(format!("advertise {port}"));
        Some(Box::new(Advert(port, Arc::clone(&log))) as Box<dyn Send>)
    }));
    assert!(seen.lock().unwrap().is_empty(), "nothing while it is off");
    session.developer_mode(true, 0).unwrap();
    assert_eq!(next_body(&mut agent), developer_mode(true));
    let port = session.adb_address().unwrap().port();
    assert_eq!(*seen.lock().unwrap(), vec![format!("advertise {port}")]);
    session.developer_mode(false, 0).unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![format!("advertise {port}"), format!("end {port}")]
    );
}

#[test]
fn fr20_developer_mode_stays_off_without_a_way_to_reach_adbd() {
    let (session, mut agent, _launcher) = active_session();
    assert!(session.developer_mode(true, 0).is_err());
    assert_eq!(session.adb_address(), None);
    nothing_for_the_guest(&mut agent);
}

const REPORT: &[u8] = b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\n%%EOF\n";

fn print_job(job_id: u32, size: u64) -> Body {
    Body::PrintJob(dsa_protocol::control::PrintJob {
        job_id,
        package_name: "com.example.docs".into(),
        app_label: "Docs".into(),
        document_name: "Report".into(),
        size,
        page_count: 1,
        copies: 2,
        color: true,
        duplex: false,
        landscape: true,
        media_width_mils: 8500,
        media_height_mils: 11000,
    })
}

fn print_piece(job_id: u32, offset: u64, data: &[u8]) -> Body {
    Body::PrintJobChunk(dsa_protocol::control::PrintJobChunk {
        job_id,
        offset,
        data: data.to_vec(),
    })
}

/// The next print job result the guest is sent, past anything else it is sent.
fn next_print_result(agent: &mut Box<dyn Connection>) -> dsa_protocol::control::PrintJobResult {
    loop {
        if let Body::PrintJobResult(result) = next_body(agent) {
            return result;
        }
    }
}

/// Sends a whole print job from the agent, and returns what the watcher was given.
fn print_from_guest(
    agent: &mut Box<dyn Connection>,
    watch: &Receiver<PrintEvent>,
    guest_id: u32,
) -> PrintJob {
    send_body(agent, print_job(guest_id, REPORT.len() as u64));
    send_body(agent, print_piece(guest_id, 0, &REPORT[..10]));
    send_body(agent, print_piece(guest_id, 10, &REPORT[10..]));
    match watch.recv_timeout(WAIT).expect("a print job") {
        PrintEvent::Job(job) => job,
        other => panic!("expected a print job, got {other:?}"),
    }
}

#[test]
fn fr26_a_document_the_guest_prints_reaches_the_watcher_whole() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_print_jobs();
    let job = print_from_guest(&mut agent, &watch, 7);
    assert_eq!(*job.pdf, REPORT);
    assert_eq!(job.document_name, "Report");
    assert_eq!(job.app_label, "Docs");
    assert_eq!(job.settings.copies, 2);
    assert!(job.settings.landscape);
    session
        .print_result(job.job_id, &PrintOutcome::Printed)
        .expect("told");
    let result = next_print_result(&mut agent);
    assert_eq!(result.job_id, 7, "the guest's own number for the job");
    assert_eq!(
        result.outcome,
        dsa_protocol::control::PrintOutcome::Printed as i32
    );
}

#[test]
fn fr26_canceling_the_hosts_print_dialog_cancels_the_job_in_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_print_jobs();
    let job = print_from_guest(&mut agent, &watch, 3);
    session
        .print_result(job.job_id, &PrintOutcome::Canceled)
        .expect("told");
    let result = next_print_result(&mut agent);
    assert_eq!(result.job_id, 3);
    assert_eq!(
        result.outcome,
        dsa_protocol::control::PrintOutcome::Canceled as i32
    );
}

#[test]
fn fr26_a_failure_on_the_host_reaches_the_guest_with_its_reason() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_print_jobs();
    let job = print_from_guest(&mut agent, &watch, 4);
    session
        .print_result(
            job.job_id,
            &PrintOutcome::Failed("the printer is offline".into()),
        )
        .expect("told");
    let result = next_print_result(&mut agent);
    assert_eq!(
        result.outcome,
        dsa_protocol::control::PrintOutcome::Failed as i32
    );
    assert_eq!(result.message, "the printer is offline");
}

#[test]
fn fr26_a_job_nobody_on_the_host_can_print_fails_in_the_guest() {
    let (_session, mut agent, _launcher) = active_session();
    send_body(&mut agent, print_job(9, REPORT.len() as u64));
    send_body(&mut agent, print_piece(9, 0, REPORT));
    let result = next_print_result(&mut agent);
    assert_eq!(result.job_id, 9);
    assert_eq!(
        result.outcome,
        dsa_protocol::control::PrintOutcome::Failed as i32
    );
    assert!(!result.message.is_empty());
}

#[test]
fn nfr5_a_bad_print_job_fails_in_the_guest_and_the_session_goes_on() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_print_jobs();
    send_body(
        &mut agent,
        print_job(1, dsa_host_core::print::MAX_PRINT_BYTES + 1),
    );
    assert_eq!(
        next_print_result(&mut agent).outcome,
        dsa_protocol::control::PrintOutcome::Failed as i32
    );
    send_body(&mut agent, print_job(2, 4));
    send_body(&mut agent, print_piece(2, 0, b"MZ\0\0"));
    let result = next_print_result(&mut agent);
    assert_eq!(result.job_id, 2);
    assert_eq!(
        result.outcome,
        dsa_protocol::control::PrintOutcome::Failed as i32
    );
    assert!(watch.recv_timeout(Duration::from_millis(300)).is_err());
    assert_eq!(session.state(), SessionState::Active);
}

#[test]
fn fr26_a_job_canceled_in_the_guest_is_withdrawn_from_the_watcher() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_print_jobs();
    let job = print_from_guest(&mut agent, &watch, 5);
    send_body(
        &mut agent,
        Body::PrintJobCancel(dsa_protocol::control::PrintJobCancel { job_id: 5 }),
    );
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        PrintEvent::Withdrawn { job_id: job.job_id }
    );
    // Its outcome no longer goes to the guest.
    session
        .print_result(job.job_id, &PrintOutcome::Canceled)
        .expect("taken");
    nothing_for_the_guest(&mut agent);
}

#[test]
fn fr26_a_guest_that_stops_withdraws_the_jobs_the_watcher_has() {
    let (session, mut agent, _launcher) = active_session();
    let watch = session.watch_print_jobs();
    let job = print_from_guest(&mut agent, &watch, 6);
    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(
        watch.recv_timeout(WAIT).unwrap(),
        PrintEvent::Withdrawn { job_id: job.job_id }
    );
    drop(agent);
}

fn seoul_settings(dark_mode: bool) -> dsa_host_core::system_settings::SystemSettings {
    dsa_host_core::system_settings::SystemSettings {
        time_zone: "Asia/Seoul".into(),
        locales: vec!["ko-KR".into(), "en-US".into()],
        regional_format: "ko-KR".into(),
        time_24_hour: false,
        first_day_of_week: 7,
        dark_mode,
        accent_color: 0xFF00_78D4,
        text_scale_percent: 100,
    }
}

#[test]
fn fr30_the_hosts_system_settings_reach_the_agent_when_it_connects_and_on_each_change() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.windows_shown(true).unwrap();
    session.set_system_settings(seoul_settings(true));
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(
        next_body(&mut agent),
        Body::SystemSettings(seoul_settings(true).to_control())
    );
    // The user switches the host to the light theme: the guest hears of it at once.
    session.set_system_settings(seoul_settings(false));
    assert_eq!(
        next_body(&mut agent),
        Body::SystemSettings(seoul_settings(false).to_control())
    );
    // The same settings again are no news.
    session.set_system_settings(seoul_settings(false));
    session.set_system_settings(dsa_host_core::system_settings::SystemSettings {
        time_zone: "Europe/Berlin".into(),
        ..seoul_settings(false)
    });
    let Body::SystemSettings(berlin) = next_body(&mut agent) else {
        panic!("expected the system settings");
    };
    assert_eq!(berlin.time_zone, "Europe/Berlin");
}

#[test]
fn fr4_text_follows_the_windows_text_size() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.windows_shown(true).unwrap();
    session.set_system_settings(seoul_settings(true));
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    // At a text size of 100% the guest's font scale is 1: text follows the same ratio as a
    // dp, 0.819 DIP, so body text of 16 sp is about 13 DIP, near the host's own.
    let Body::SystemSettings(at_100) = next_body(&mut agent) else {
        panic!("expected the system settings");
    };
    assert!((at_100.font_scale - 1.0).abs() < 1e-5);
    // The user makes Windows' text 150%: the guest hears of it at once, as a setting and
    // nothing else, so no task is asked to change and none is relaunched for it.
    session.set_system_settings(dsa_host_core::system_settings::SystemSettings {
        text_scale_percent: 150,
        ..seoul_settings(true)
    });
    let Body::SystemSettings(at_150) = next_body(&mut agent) else {
        panic!("expected the system settings");
    };
    assert!((at_150.font_scale - 1.5).abs() < 1e-5);
    assert_eq!(
        at_150,
        dsa_protocol::control::SystemSettings {
            font_scale: at_150.font_scale,
            ..at_100.clone()
        },
        "only the font scale changed"
    );
    // A text size Windows does not offer is kept within what a desktop may ask for.
    session.set_system_settings(dsa_host_core::system_settings::SystemSettings {
        text_scale_percent: 1000,
        ..seoul_settings(true)
    });
    let Body::SystemSettings(capped) = next_body(&mut agent) else {
        panic!("expected the system settings");
    };
    assert!((capped.font_scale - 3.0).abs() < 1e-5);
}

#[test]
fn fr17_the_agent_is_given_the_computers_name_as_the_device_name_each_time_it_connects() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.windows_shown(true).unwrap();
    session.set_device_name_source(std::sync::Arc::new(|| Some("BREW-BOOK".to_string())));
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(
        next_body(&mut agent),
        Body::DeviceName(dsa_protocol::control::DeviceName {
            name: "BREW-BOOK".into()
        })
    );
}

#[test]
fn fr17_without_a_device_name_the_agent_is_told_nothing_of_it() {
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.windows_shown(true).unwrap();
    session.set_device_name_source(std::sync::Arc::new(|| None));
    session.set_system_settings(seoul_settings(true));
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    // The first thing the agent hears is the system settings; no device name comes after.
    assert_eq!(
        next_body(&mut agent),
        Body::SystemSettings(seoul_settings(true).to_control())
    );
    session.set_system_settings(seoul_settings(false));
    assert_eq!(
        next_body(&mut agent),
        Body::SystemSettings(seoul_settings(false).to_control())
    );
}

#[test]
fn fr30_a_controller_connected_before_the_guest_reaches_it_and_its_rumble_comes_back() {
    use dsa_host_core::gamepads::{
        buttons, GamepadEvent, GamepadInfo, GamepadRumble, GamepadState,
    };
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.windows_shown(true).unwrap();
    let rumble = session.watch_gamepad_rumble();
    let pad = GamepadInfo {
        pad_id: 1,
        name: "Xbox Controller".into(),
        vendor_id: 0x045e,
        product_id: 0x028e,
        rumble: true,
    };
    session.gamepad(GamepadEvent::Connected(pad.clone()));
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert_eq!(
        next_body(&mut agent),
        GamepadEvent::Connected(pad.clone()).to_control()
    );
    // The control daemon is told again once the agent is there; it takes the same controller
    // once.
    assert_eq!(
        next_body(&mut agent),
        GamepadEvent::Connected(pad).to_control()
    );
    let state = GamepadState {
        pad_id: 1,
        buttons: buttons::A | buttons::DPAD_UP,
        left_x: -12000,
        left_y: 32767,
        right_trigger: 200,
        ..GamepadState::default()
    };
    session.gamepad(GamepadEvent::State(state));
    assert_eq!(
        next_body(&mut agent),
        GamepadEvent::State(state).to_control()
    );
    send_body(
        &mut agent,
        Body::GamepadRumble(dsa_protocol::control::GamepadRumble {
            pad_id: 1,
            strong: 40000,
            weak: 1000,
            duration_ms: 300,
        }),
    );
    assert_eq!(
        rumble.recv_timeout(WAIT).unwrap(),
        GamepadRumble {
            pad_id: 1,
            strong: 40000,
            weak: 1000,
            duration_ms: 300
        }
    );
    session.gamepad(GamepadEvent::Disconnected { pad_id: 1 });
    assert_eq!(
        next_body(&mut agent),
        GamepadEvent::Disconnected { pad_id: 1 }.to_control()
    );
}

#[test]
fn fr31_a_session_is_never_stopped_for_idle_time_after_the_last_window_closes() {
    // No window has been shown since the agent connected, as after the last one closed.
    let (session, mut agent, launcher) = lifecycle_session(None);
    dozing(&session, &mut agent);
    thread::sleep(Duration::from_secs(2));
    assert_eq!(
        session.state(),
        SessionState::Dozing,
        "an idle Android stays in Doze, so the next app opens without a boot"
    );
    assert_eq!(session.last_reason(), None, "nothing stopped it");
    assert_eq!(
        launcher.0.lock().unwrap().freezes,
        0,
        "and nothing froze it"
    );
}

fn root_app(
    package: &str,
    level: dsa_host_core::root_elevation::Level,
) -> dsa_host_core::root_elevation::AppLevel {
    dsa_host_core::root_elevation::AppLevel {
        package: package.into(),
        level,
    }
}

/// The next root elevation config the guest is sent, passing over everything else.
fn next_root_config(agent: &mut Box<dyn Connection>) -> dsa_protocol::control::RootElevationConfig {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "the guest was sent no root elevation config"
        );
        if let Body::RootElevationConfig(config) = next_body(agent) {
            return config;
        }
    }
}

/// Fails if the guest is sent a root elevation config within a short while.
fn no_root_config(agent: &mut Box<dyn Connection>) {
    let until = std::time::Instant::now() + Duration::from_millis(300);
    while let Some(left) = until.checked_duration_since(std::time::Instant::now()) {
        match agent.recv(Some(left)) {
            Ok(frame) => {
                if let Ok(Envelope {
                    body: Some(Body::RootElevationConfig(config)),
                }) = dsa_protocol::decode(&frame)
                {
                    panic!("the guest was sent {config:?}");
                }
            }
            Err(TransportError::Timeout) => return,
            Err(error) => panic!("the channel broke: {error}"),
        }
    }
}

#[test]
fn fr35_the_root_elevation_config_is_sent_once_the_agent_connects() {
    use dsa_host_core::root_elevation::Level;
    use dsa_protocol::control::RootLevel;
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.set_root_elevation_config(
        true,
        vec![
            root_app("com.example.ask", Level::Ask),
            root_app("com.example.off", Level::Off),
            root_app("com.example.keep", Level::Remember),
        ],
    );
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    let config = next_root_config(&mut agent);
    assert!(config.enabled);
    // By package, as the host knows them, in package order; off apps are not listed, and
    // the guest fills in each uid.
    let apps: Vec<(String, u32, RootLevel)> = config
        .apps
        .iter()
        .map(|app| (app.package.clone(), app.uid, app.level()))
        .collect();
    assert_eq!(
        apps,
        vec![
            ("com.example.ask".to_string(), 0, RootLevel::Ask),
            ("com.example.keep".to_string(), 0, RootLevel::Remember),
        ]
    );

    // An agent that connects again (the framework restarted) has it sent again.
    report_boot(&mut agent, true, false);
    thread::sleep(Duration::from_millis(50));
    report_boot(&mut agent, true, true);
    assert!(next_root_config(&mut agent).enabled);
}

#[test]
fn fr35_a_change_is_sent_to_the_running_android_at_once() {
    use dsa_host_core::root_elevation::Level;
    use dsa_protocol::control::RootLevel;
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.set_root_elevation_config(true, vec![root_app("com.example.ask", Level::Ask)]);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    next_root_config(&mut agent);

    // A level changes: the running Android is told, with no restart.
    session.set_root_elevation_config(true, vec![root_app("com.example.ask", Level::Remember)]);
    let config = next_root_config(&mut agent);
    assert!(config.enabled);
    assert_eq!(config.apps.len(), 1);
    assert_eq!(config.apps[0].level(), RootLevel::Remember);

    // The same settings again send nothing.
    session.set_root_elevation_config(true, vec![root_app("com.example.ask", Level::Remember)]);
    no_root_config(&mut agent);

    // The app set to off is no longer listed.
    session.set_root_elevation_config(true, Vec::new());
    assert!(next_root_config(&mut agent).apps.is_empty());

    // The switch turned off: the running Android hides su from every app at once.
    session.set_root_elevation_config(false, vec![root_app("com.example.ask", Level::Ask)]);
    assert!(!next_root_config(&mut agent).enabled);
}

#[test]
fn fr35_turning_root_elevation_on_applies_from_the_next_start() {
    use dsa_host_core::root_elevation::Level;
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    // A boot that started with the feature off is told nothing: its daemon hides su from
    // every app on its own, and keeps it hidden until the next start.
    no_root_config(&mut agent);
    session.set_root_elevation_config(true, vec![root_app("com.example.ask", Level::Ask)]);
    no_root_config(&mut agent);

    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    drop(agent);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    let config = next_root_config(&mut agent);
    assert!(config.enabled);
    assert_eq!(config.apps[0].package, "com.example.ask");
}

#[test]
fn fr35_turning_root_elevation_back_on_waits_for_the_next_start() {
    use dsa_host_core::root_elevation::Level;
    let launcher = FakeLauncher::default();
    let (session, mut connector) = session_with(launcher.clone(), WAIT);
    session.set_root_elevation_config(true, vec![root_app("com.example.ask", Level::Ask)]);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(session.wait_for(SessionState::Active, WAIT));
    assert!(next_root_config(&mut agent).enabled);

    // Off applies to the running Android at once.
    session.set_root_elevation_config(false, vec![root_app("com.example.ask", Level::Ask)]);
    assert!(!next_root_config(&mut agent).enabled);
    // On again: the running Android stays off until it starts again.
    session.set_root_elevation_config(true, vec![root_app("com.example.ask", Level::Ask)]);
    no_root_config(&mut agent);

    session.stop();
    assert!(session.wait_for(SessionState::Stopped, WAIT));
    drop(agent);
    session.start();
    let mut agent = connect_agent(&mut connector, &launcher);
    assert!(next_root_config(&mut agent).enabled);
}

/// The next su decision for the guest, past any other message.
fn next_su_decision(daemon: &mut Box<dyn Connection>) -> dsa_protocol::control::SuDecision {
    loop {
        if let Body::SuDecision(decision) = next_body(daemon) {
            return decision;
        }
    }
}

/// A prompt that approves every request it is shown.
struct ApprovingPrompt;

impl dsa_host_core::root_approval::ApprovalPrompt for ApprovingPrompt {
    fn ask(
        &self,
        _request: &dsa_host_core::root_approval::ApprovalRequest,
        _timeout: Duration,
    ) -> dsa_host_core::root_approval::PromptAnswer {
        dsa_host_core::root_approval::PromptAnswer::Approved
    }
}

fn su_request(request_id: u64, package: &str) -> Body {
    Body::SuRequest(dsa_protocol::control::SuRequest {
        request_id,
        package: package.into(),
        uid: 10_200,
        command: vec!["id".into()],
        interactive: false,
    })
}

#[test]
fn fr35_a_su_request_from_the_guest_gets_the_hosts_decision_back() {
    use dsa_host_core::root_approval::RootApproval;
    use dsa_host_core::settings::{MachineSettings, MemorySettings, SettingsStore};
    let (session, mut daemon, _launcher) = lifecycle_session(None);
    let store = Arc::new(MemorySettings::default());
    store
        .set_machine(MachineSettings {
            root_elevation: true,
            ..Default::default()
        })
        .unwrap();
    store.set_root_app("user", "com.ask.app", "ask").unwrap();
    session.set_root_approval(Arc::new(RootApproval::new(
        "user",
        store,
        Arc::new(ApprovingPrompt),
        Arc::new(|_: &str| {}),
    )));

    send_body(&mut daemon, su_request(41, "com.ask.app"));
    let approved = next_su_decision(&mut daemon);
    assert_eq!(approved.request_id, 41);
    assert!(approved.approved, "{approved:?}");

    // An app that may not ask is refused with a reason, and nobody is asked.
    send_body(&mut daemon, su_request(42, "com.not.allowed"));
    let refused = next_su_decision(&mut daemon);
    assert_eq!(refused.request_id, 42);
    assert!(!refused.approved);
    assert!(!refused.reason.is_empty());
}

#[test]
fn fr35_a_host_that_cannot_ask_its_administrator_refuses_at_once() {
    let (_session, mut daemon, _launcher) = lifecycle_session(None);
    send_body(&mut daemon, su_request(7, "com.ask.app"));
    let refused = next_su_decision(&mut daemon);
    assert_eq!(refused.request_id, 7);
    assert!(!refused.approved);
    assert!(!refused.reason.is_empty());
}

/// An administrator prompt that answers each request with the next of its answers.
struct ScriptedPrompt {
    answers: Mutex<Vec<dsa_host_core::root_approval::PromptAnswer>>,
    asked: Mutex<Vec<String>>,
}

impl dsa_host_core::root_approval::ApprovalPrompt for ScriptedPrompt {
    fn ask(
        &self,
        request: &dsa_host_core::root_approval::ApprovalRequest,
        _timeout: Duration,
    ) -> dsa_host_core::root_approval::PromptAnswer {
        self.asked
            .lock()
            .unwrap()
            .push(request.package().to_string());
        self.answers.lock().unwrap().remove(0)
    }
}

#[test]
fn fr35_an_ask_apps_su_request_is_answered_by_the_hosts_administrator_prompt() {
    use dsa_host_core::root_approval::{PromptAnswer, RootApproval};
    use dsa_host_core::root_elevation::{AppLevel, Level};
    let (session, mut agent, _launcher) = active_session();
    let prompt = Arc::new(ScriptedPrompt {
        answers: Mutex::new(vec![PromptAnswer::Denied, PromptAnswer::Approved]),
        asked: Mutex::new(Vec::new()),
    });
    let root = RootApproval::with_levels(
        "user",
        prompt.clone(),
        Arc::new(|| {
            (
                true,
                vec![AppLevel {
                    package: "com.ask.app".into(),
                    level: Level::Ask,
                }],
            )
        }),
        Arc::new(|_: &str| {}),
    );
    session.set_root_approval(Arc::new(root));

    send_body(&mut agent, su_request(1, "com.ask.app"));
    let refused = next_su_decision(&mut agent);
    assert_eq!(refused.request_id, 1);
    assert!(!refused.approved);
    assert!(refused.reason.contains("refused"), "{}", refused.reason);

    send_body(&mut agent, su_request(2, "com.ask.app"));
    let approved = next_su_decision(&mut agent);
    assert_eq!(approved.request_id, 2);
    assert!(approved.approved);

    // Within the grace: approved without a third prompt.
    send_body(&mut agent, su_request(3, "com.ask.app"));
    let again = next_su_decision(&mut agent);
    assert!(again.approved);
    // An app that may not use root never reaches the prompt.
    send_body(&mut agent, su_request(4, "com.other.app"));
    assert!(!next_su_decision(&mut agent).approved);
    assert_eq!(prompt.asked.lock().unwrap().len(), 2);
}

#[test]
fn fr35_letting_an_app_keep_root_is_asked_through_the_sessions_prompt() {
    use dsa_host_core::root_approval::{PromptAnswer, RootApproval};
    let (session, _agent, _launcher) = active_session();
    // Without a prompt, nobody can approve it.
    assert!(session.approve_keep_root("com.keep.app").is_err());
    let prompt = Arc::new(ScriptedPrompt {
        answers: Mutex::new(vec![PromptAnswer::Approved, PromptAnswer::Cancelled]),
        asked: Mutex::new(Vec::new()),
    });
    session.set_root_approval(Arc::new(RootApproval::with_levels(
        "user",
        prompt.clone(),
        Arc::new(|| (true, Vec::new())),
        Arc::new(|_: &str| {}),
    )));
    assert_eq!(session.approve_keep_root("com.keep.app"), Ok(()));
    let dismissed = session.approve_keep_root("com.keep.app").unwrap_err();
    assert!(dismissed.contains("dismissed"), "{dismissed}");
    assert_eq!(
        *prompt.asked.lock().unwrap(),
        vec!["com.keep.app".to_string(), "com.keep.app".to_string()]
    );
}

fn speak_text(request_id: u32) -> Body {
    Body::SpeakText(dsa_protocol::control::SpeakText {
        request_id,
        text: "Hello.".into(),
        voice_id: "aria".into(),
        language_tag: "en-US".into(),
        rate: 1.0,
        pitch: 1.0,
    })
}

/// The next speech or spelling answer the guest is sent, past anything else it is sent.
fn next_language_answer(agent: &mut Box<dyn Connection>) -> Body {
    loop {
        let body = next_body(agent);
        if dsa_host_core::language::relay::is_answer(&body) {
            return body;
        }
    }
}

/// The local API service over `session`, for every user, and a connector to it.
fn service_over(
    session: &Arc<Session>,
) -> (
    dsa_host_core::host_api_conformance::RunningService,
    dsa_transport::local::MemoryLocalConnector,
) {
    use dsa_host_core::host_api::{PerUser, UserAndroid};
    let (listener, connector) = dsa_transport::local::pair();
    let session = Arc::clone(session);
    let androids = Arc::new(PerUser::new(move |_: &str| {
        Ok(Arc::clone(&session) as Arc<dyn UserAndroid>)
    }));
    let service =
        dsa_host_core::host_api_conformance::RunningService::start(Box::new(listener), androids);
    (service, connector)
}

/// The next message for the guest that `pick` takes, past any other.
fn next_matching<T>(agent: &mut Box<dyn Connection>, pick: impl Fn(Body) -> Option<T>) -> T {
    loop {
        if let Some(found) = pick(next_body(agent)) {
            return found;
        }
    }
}

#[test]
fn fr36_the_guests_speech_requests_reach_the_host_session_and_its_answers_the_guest() {
    let (session, mut agent, _launcher) = active_session();
    let served = session.serve_language();
    send_body(&mut agent, speak_text(4));
    let request = served.requests.recv_timeout(WAIT).expect("the request");
    assert_eq!(request.body, Some(speak_text(4)));
    let audio = Body::SpeechAudio(dsa_protocol::control::SpeechAudio {
        request_id: 4,
        sample_rate_hz: 24000,
        channel_count: 1,
        pcm: vec![1, 0, 2, 0],
    });
    session
        .language_answer(
            served.id,
            Envelope {
                body: Some(audio.clone()),
            },
        )
        .expect("an answer");
    assert_eq!(next_language_answer(&mut agent), audio);
    // Something that is no answer reaches nobody.
    assert!(session
        .language_answer(
            served.id,
            Envelope {
                body: Some(speak_text(5)),
            },
        )
        .is_err());
    // The client goes away before it finished: the guest is told.
    session.language_server_gone(served.id);
    match next_language_answer(&mut agent) {
        Body::SpeechDone(done) => {
            assert_eq!(done.request_id, 4);
            assert_eq!(
                done.outcome,
                dsa_protocol::control::SpeechOutcome::Unavailable as i32
            );
        }
        other => panic!("expected the end of the speech, got {other:?}"),
    }
}

/// The host's geocoder over the boundary fixture (tests/data, written by
/// dsa/host/core/geodata/test_build_geodata.py), recording the lookups it is given.
struct RecordingGeocoder {
    seen: Arc<Mutex<Vec<dsa_host_core::geocode::GeocodeQuery>>>,
    inner: dsa_host_core::geocode::BoundaryGeocoder,
}

impl dsa_host_core::geocode::Geocoder for RecordingGeocoder {
    fn geocode(
        &mut self,
        query: &dsa_host_core::geocode::GeocodeQuery,
    ) -> dsa_host_core::geocode::GeocodeOutcome {
        self.seen.lock().unwrap().push(query.clone());
        dsa_host_core::geocode::Geocoder::geocode(&mut self.inner, query)
    }
}

fn boundary_fixture() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("data")
        .join("address-boundaries-fixture.bin")
}

/// Sends `request` until a watcher has it (one that comes before the watcher has connected is
/// refused at once), and returns the result.
fn geocode_until_answered(
    agent: &mut Box<dyn Connection>,
    request: dsa_protocol::control::GeocodeRequest,
) -> dsa_protocol::control::GeocodeResult {
    use dsa_protocol::control::GeocodeStatus;
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        send_body(agent, Body::GeocodeRequest(request.clone()));
        let result = next_matching(agent, |body| match body {
            Body::GeocodeResult(result) => Some(result),
            _ => None,
        });
        assert_eq!(result.request_id, request.request_id);
        if result.status != GeocodeStatus::Refused as i32 || std::time::Instant::now() > deadline {
            return result;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn fr36_network_location_and_geocoder_come_from_the_host() {
    use dsa_host_core::geocode;
    use dsa_protocol::control::{GeocodeRequest, GeocodeStatus, LocationRequest};
    use std::sync::atomic::{AtomicBool, Ordering};

    let (session, mut agent, _launcher) = lifecycle_session(None);
    let session = Arc::new(session);

    // The agent's network location provider asks for the host's location, and the host's
    // position reaches it as the network location.
    let wanted = session.watch_location();
    assert_eq!(wanted.recv_timeout(WAIT), Ok(Wanted::NONE));
    send_body(
        &mut agent,
        Body::LocationRequest(LocationRequest {
            active: true,
            interval_ms: 5000,
        }),
    );
    assert_eq!(
        wanted.recv_timeout(WAIT),
        Ok(Wanted::every(Duration::from_secs(5)))
    );
    session.host_location(LocationEvent::Fix(seoul()));
    assert_eq!(
        next_location(&mut agent),
        Body::LocationFix(seoul().to_control())
    );

    // Without Google Play the host's geocoder answers, through a watcher in the user's
    // session, from the boundary data the host keeps.
    let (_service, connector) = service_over(&session);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (seen, stop) = (Arc::clone(&seen), Arc::clone(&stop));
        thread::spawn(move || {
            let mut connector = connector;
            let mut geocoder = RecordingGeocoder {
                seen,
                inner: geocode::BoundaryGeocoder::new(boundary_fixture()),
            };
            geocode::serve(
                &mut connector,
                "test geocoder",
                &mut geocoder,
                &stop,
                &|_: &str| {},
            );
        })
    };

    let forward = geocode_until_answered(
        &mut agent,
        GeocodeRequest {
            request_id: 7,
            query: "Seoul City Hall".into(),
            max_results: 3,
            language: "ko-KR".into(),
            ..Default::default()
        },
    );
    assert_eq!(
        forward.status,
        GeocodeStatus::Ok as i32,
        "{}",
        forward.message
    );
    // An address is not turned into positions: the lookup runs and finds nothing.
    assert!(forward.addresses.is_empty(), "{:?}", forward.addresses);

    let reverse = geocode_until_answered(
        &mut agent,
        GeocodeRequest {
            request_id: 8,
            reverse: true,
            latitude_degrees: 37.5663,
            longitude_degrees: 126.9779,
            max_results: 1,
            language: "ko-KR".into(),
            ..Default::default()
        },
    );
    assert_eq!(
        reverse.status,
        GeocodeStatus::Ok as i32,
        "{}",
        reverse.message
    );
    // A known position comes back as its city, district and dong.
    assert_eq!(reverse.addresses.len(), 1);
    let address = &reverse.addresses[0];
    assert_eq!(address.country_code, "KR");
    assert_eq!(address.admin_area, "서울특별시");
    assert_eq!(address.locality, "서울특별시");
    assert_eq!(address.sub_locality, "중구");
    assert_eq!(address.thoroughfare, "명동");
    assert_eq!(address.address_lines, vec!["대한민국 서울특별시 중구 명동"]);
    assert!(address.has_position);

    let seen = seen.lock().unwrap().clone();
    let forward_seen = seen
        .iter()
        .find(|q| q.query_id == 7)
        .expect("the forward lookup");
    assert_eq!(
        forward_seen.lookup,
        geocode::Lookup::Forward {
            query: "Seoul City Hall".into(),
            bounds: None
        }
    );
    assert_eq!(forward_seen.max_results, 3);
    assert_eq!(forward_seen.language, "ko-KR");
    let reverse_seen = seen
        .iter()
        .find(|q| q.query_id == 8)
        .expect("the reverse lookup");
    assert_eq!(
        reverse_seen.lookup,
        geocode::Lookup::Reverse {
            latitude: 37.5663,
            longitude: 126.9779
        }
    );

    // A lookup that cannot be answered is answered at once, as invalid.
    send_body(
        &mut agent,
        Body::GeocodeRequest(GeocodeRequest {
            request_id: 9,
            reverse: true,
            latitude_degrees: 120.0,
            ..Default::default()
        }),
    );
    let invalid = next_matching(&mut agent, |body| match body {
        Body::GeocodeResult(result) => Some(result),
        _ => None,
    });
    assert_eq!(invalid.request_id, 9);
    assert_eq!(invalid.status, GeocodeStatus::Invalid as i32);

    stop.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
}

#[test]
fn fr36_a_lookup_with_no_desktop_session_is_refused_at_once() {
    use dsa_protocol::control::{GeocodeRequest, GeocodeStatus};
    let (_session, mut agent, _launcher) = lifecycle_session(None);
    send_body(
        &mut agent,
        Body::GeocodeRequest(GeocodeRequest {
            request_id: 1,
            query: "Busan".into(),
            max_results: 1,
            ..Default::default()
        }),
    );
    let result = next_matching(&mut agent, |body| match body {
        Body::GeocodeResult(result) => Some(result),
        _ => None,
    });
    assert_eq!(result.status, GeocodeStatus::Refused as i32);
}

/// What a share UI was given: the content, and each file's name and bytes.
type SharedContent = (dsa_host_core::share::ShareContent, Vec<(String, Vec<u8>)>);

/// A share UI that records what it was given, with each file's content, and shares it.
struct FakeShareUi {
    seen: Arc<Mutex<Vec<SharedContent>>>,
}

impl dsa_host_core::share::ShareUi for FakeShareUi {
    fn share(
        &mut self,
        content: &dsa_host_core::share::ShareContent,
    ) -> dsa_host_core::share::ShareOutcome {
        let files = content
            .files
            .iter()
            .map(|path| {
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read(path).unwrap())
            })
            .collect();
        self.seen.lock().unwrap().push((content.clone(), files));
        dsa_host_core::share::ShareOutcome::Shared
    }
}

fn shared_item(name: &str, size: u64, mime: &str) -> dsa_protocol::control::DragItem {
    dsa_protocol::control::DragItem {
        name: name.into(),
        size,
        size_known: true,
        mime_type: mime.into(),
        ..Default::default()
    }
}

fn shared_text(text: &str) -> dsa_protocol::control::DragItem {
    dsa_protocol::control::DragItem {
        mime_type: "text/plain".into(),
        is_text: true,
        text: text.into(),
        ..Default::default()
    }
}

#[test]
fn fr36_sharing_hands_content_to_the_host_share_ui() {
    use dsa_protocol::control::{DragData, DragEnd, ShareOutcome, ShareToHost};
    use std::sync::atomic::{AtomicBool, Ordering};

    let (session, mut agent, _launcher) = lifecycle_session(None);
    let session = Arc::new(session);
    let (_service, connector) = service_over(&session);
    let spool = std::env::temp_dir().join(format!("dsa-share-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&spool);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let watcher = {
        let (seen, stop, spool) = (Arc::clone(&seen), Arc::clone(&stop), spool.clone());
        thread::spawn(move || {
            let mut connector = connector;
            let mut ui = FakeShareUi { seen };
            dsa_host_core::share::serve(
                &mut connector,
                "test share",
                &mut ui,
                &spool,
                &stop,
                &|_: &str| {},
            );
        })
    };

    let photo = b"JPEG photo bytes".to_vec();
    let share = |share_id| ShareToHost {
        share_id,
        title: "Our trip".into(),
        package_name: "com.example.gallery".into(),
        app_label: "Gallery".into(),
        items: vec![
            shared_text("Look at this"),
            shared_text("https://example.com/trip"),
            shared_item("photo.jpg", photo.len() as u64, "image/jpeg"),
        ],
    };

    // A share that comes before the watcher has connected fails at once and gives the
    // access back; the next one is taken.
    let mut share_id = 1;
    let read = loop {
        send_body(&mut agent, Body::ShareToHost(share(share_id)));
        let next = next_matching(&mut agent, |body| match body {
            Body::DragRead(read) => Some(Ok(read)),
            Body::ShareResult(result) => Some(Err(result)),
            _ => None,
        });
        match next {
            Ok(read) => break read,
            Err(result) => {
                assert_eq!(result.share_id, share_id);
                assert_eq!(result.outcome, ShareOutcome::Failed as i32);
                let end = next_matching(&mut agent, |body| match body {
                    Body::DragEnd(end) => Some(end),
                    _ => None,
                });
                assert_eq!(end.drag_id, share_id);
                share_id += 1;
                thread::sleep(Duration::from_millis(50));
            }
        }
    };

    // The file is read as a drag's files are, under the share's ID.
    assert_eq!(read.drag_id, share_id);
    assert_eq!(read.item, 2);
    assert_eq!(read.offset, 0);
    send_body(
        &mut agent,
        Body::DragData(DragData {
            request_id: read.request_id,
            data: photo.clone(),
            end: true,
            error: String::new(),
        }),
    );
    let end = next_matching(&mut agent, |body| match body {
        Body::DragEnd(end) => Some(end),
        _ => None,
    });
    assert_eq!(
        end,
        DragEnd {
            drag_id: share_id,
            dropped: true
        }
    );
    let result = next_matching(&mut agent, |body| match body {
        Body::ShareResult(result) => Some(result),
        _ => None,
    });
    assert_eq!(result.share_id, share_id);
    assert_eq!(result.outcome, ShareOutcome::Shared as i32);

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let (content, files) = &seen[0];
    assert_eq!(content.title, "Our trip");
    assert_eq!(content.text.as_deref(), Some("Look at this"));
    assert_eq!(content.links, vec!["https://example.com/trip".to_string()]);
    assert_eq!(files, &vec![("photo.jpg".to_string(), photo)]);

    stop.store(true, Ordering::Relaxed);
    watcher.join().unwrap();
    let _ = std::fs::remove_dir_all(&spool);
}

#[test]
fn fr36_a_spelling_request_with_no_host_session_to_serve_it_is_unsupported() {
    let (_session, mut agent, _launcher) = active_session();
    send_body(
        &mut agent,
        Body::CheckSpelling(dsa_protocol::control::CheckSpelling {
            request_id: 2,
            language_tag: "en-US".into(),
            texts: vec!["teh".into()],
            max_suggestions: 3,
        }),
    );
    match next_language_answer(&mut agent) {
        Body::SpellingResult(result) => {
            assert_eq!(result.request_id, 2);
            assert!(!result.supported);
        }
        other => panic!("expected a spelling result, got {other:?}"),
    }
}
