//! A user's Android's memory from end to end: the guest's agent reports it on the control
//! channel, a real session keeps it, and `dsa memory` reads it through the local API with
//! the host processes beside it, all over the in-memory transports.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::handshake::ControlHandshake;
use dsa_host_core::host_api::{HostApiClient, HostApiService, PerUser, UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::RunningService;
use dsa_host_core::memory::{
    snapshot_from_wire, snapshot_to_wire, summary_lines, GuestMemory, GuestMemoryReport,
    HostMemory, HostProcessMemory, HostRole, IdleTrim, MemorySnapshot,
};
use dsa_host_core::session::{GuestLauncher, Session, SessionConfig, SessionState};
use dsa_host_core::tasks::{Task, TaskEvent};
use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body, Envelope, Role};
use dsa_protocol::handshake::{guest_connect, local_hello};
use dsa_transport::local;
use dsa_transport::memory;
use dsa_transport::{Connection, Connector};

const WAIT: Duration = Duration::from_secs(10);
const MIB: u64 = 1024;

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
    fn vm_id(&self) -> Option<String> {
        Some("6a5f2a1e-0000-4000-8000-000000000001".into())
    }
}

/// The host side as a platform would measure it, recording what it was asked.
#[derive(Default)]
struct FakeHost {
    asked: Mutex<Vec<(String, Option<String>)>>,
}

impl HostMemory for FakeHost {
    fn processes(&self, user: &str, vm_id: Option<&str>) -> Vec<HostProcessMemory> {
        self.asked
            .lock()
            .unwrap()
            .push((user.to_string(), vm_id.map(str::to_string)));
        vec![
            host(HostRole::Vm, 4, 600),
            host(HostRole::WindowManager, 10, 120),
            host(HostRole::AppWindow, 11, 50),
            host(HostRole::AppWindow, 12, 50),
        ]
    }
}

fn host(role: HostRole, pid: u32, mib: u64) -> HostProcessMemory {
    HostProcessMemory {
        role,
        pid,
        working_set_kb: mib * MIB,
        private_kb: mib * MIB / 2,
    }
}

fn report() -> control::GuestMemory {
    control::GuestMemory {
        mem_total_kb: 4096 * MIB,
        mem_free_kb: 2048 * MIB,
        mem_available_kb: 3000 * MIB,
        buffers_kb: 10 * MIB,
        cached_kb: 900 * MIB,
        anon_pages_kb: 700 * MIB,
        shmem_kb: 20 * MIB,
        slab_reclaimable_kb: 80 * MIB,
        swap_total_kb: 2048 * MIB,
        swap_free_kb: 1948 * MIB,
        zram_original_kb: 100 * MIB,
        zram_used_kb: 30 * MIB,
        total_pss_kb: 650 * MIB,
        top_processes: vec![control::ProcessMemory {
            pid: 500,
            name: "system".into(),
            pss_kb: 180 * MIB,
            swap_pss_kb: MIB,
        }],
    }
}

struct Setup {
    session: Arc<Session>,
    agent: Box<dyn Connection>,
    api: local::MemoryLocalConnector,
    host: Arc<FakeHost>,
    _service: RunningService,
}

fn setup() -> Setup {
    let guest = Guest::default();
    let (listener, mut control) = memory::pair();
    let session = Arc::new(Session::new(
        guest.clone(),
        Box::new(listener),
        ControlHandshake::new("dsa-host test", WAIT),
        SessionConfig {
            connect_timeout: WAIT,
            stop_timeout: WAIT,
            ..SessionConfig::default()
        },
    ));
    let (api_listener, api) = local::pair();
    let for_service = Arc::clone(&session);
    let host = Arc::new(FakeHost::default());
    let androids = Arc::new(PerUser::new(move |_: &str| {
        Ok(Arc::clone(&for_service) as Arc<dyn UserAndroid>)
    }));
    let service = HostApiService::new(androids, "test service")
        .with_host_memory(Arc::clone(&host) as Arc<dyn HostMemory>)
        .with_memory_wait(WAIT);
    let service = RunningService::start_service(Box::new(api_listener), service);
    session.start();
    let deadline = Instant::now() + WAIT;
    let secret = loop {
        if let Some(secret) = guest.0.lock().unwrap().clone() {
            break secret;
        }
        assert!(Instant::now() < deadline, "the guest was never started");
        thread::sleep(Duration::from_millis(5));
    };
    let mut agent = control.connect(WAIT).expect("the agent connects");
    guest_connect(
        agent.as_mut(),
        &local_hello(Role::Guest, "dsa-guestd test"),
        &secret,
        WAIT,
    )
    .expect("the host accepts the agent");
    assert!(session.wait_for(SessionState::Active, WAIT));
    Setup {
        session,
        agent,
        api,
        host,
        _service: service,
    }
}

fn send(agent: &mut Box<dyn Connection>, body: Body) {
    agent
        .send(&dsa_protocol::encode(&Envelope { body: Some(body) }))
        .unwrap();
}

fn wait_for_report(session: &Session) -> GuestMemoryReport {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(report) = session.guest_memory() {
            return report;
        }
        assert!(Instant::now() < deadline, "the report never arrived");
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn nfr1_the_session_keeps_the_guests_last_memory_report_until_it_stops() {
    let mut setup = setup();
    assert_eq!(setup.session.guest_memory(), None);
    send(&mut setup.agent, Body::GuestMemory(report()));
    let kept = wait_for_report(&setup.session);
    assert_eq!(kept.memory, GuestMemory::from_control(report()));
    assert_eq!(kept.memory.top_processes[0].name, "system");

    setup.session.stop();
    assert!(setup.session.wait_for(SessionState::Stopped, WAIT));
    assert_eq!(setup.session.guest_memory(), None);
}

#[test]
fn nfr1_dsa_memory_asks_the_guest_and_shows_it_beside_the_host_processes() {
    let mut setup = setup();
    let api = &mut setup.api;
    let reader = thread::scope(|scope| {
        let reader = scope.spawn(|| {
            HostApiClient::connect(api, "test client", WAIT)
                .expect("connecting")
                .memory()
        });
        // The service asks the agent for a fresh report, which it answers.
        let asked = dsa_protocol::decode(&setup.agent.recv(Some(WAIT)).unwrap()).unwrap();
        assert_eq!(
            asked.body,
            Some(Body::RequestGuestMemory(control::RequestGuestMemory {}))
        );
        send(&mut setup.agent, Body::GuestMemory(report()));
        reader.join().unwrap()
    });
    let snapshot = reader.expect("the memory");
    let guest = snapshot.guest.as_ref().expect("the guest's report");
    assert_eq!(guest.memory, GuestMemory::from_control(report()));
    assert_eq!(snapshot.host.len(), 4);
    assert_eq!(snapshot.host_total_kb(), 820 * MIB);
    assert_eq!(
        setup.host.asked.lock().unwrap().as_slice(),
        [(
            "test".to_string(),
            Some("6a5f2a1e-0000-4000-8000-000000000001".to_string())
        )]
        .as_slice()
    );
}

fn snapshot() -> MemorySnapshot {
    MemorySnapshot {
        guest: Some(GuestMemoryReport {
            memory: GuestMemory::from_control(report()),
            age: Duration::from_secs(42),
        }),
        host: vec![
            host(HostRole::Vm, 4, 600),
            host(HostRole::WindowManager, 10, 120),
            host(HostRole::AppWindow, 11, 50),
            host(HostRole::AppWindow, 12, 50),
        ],
    }
}

#[test]
fn nfr1_the_log_line_sums_the_host_and_says_where_the_guests_memory_goes() {
    assert_eq!(
        snapshot().summary_line(),
        "memory: host 820 MiB (vm 600, window manager 120, 2 app windows 100); \
         guest 2048/4096 MiB used, cache 910, apps 700, swap 100 (zram 30), pss 650 MiB, 42 s ago"
    );
    let no_guest = MemorySnapshot {
        guest: None,
        host: vec![host(HostRole::Vm, 4, 600)],
    };
    assert_eq!(
        no_guest.summary_line(),
        "memory: host 600 MiB (vm 600, window manager 0, 0 app windows 0); guest has not reported"
    );
}

#[test]
fn nfr1_the_snapshot_crosses_the_local_api_unchanged() {
    let snapshot = snapshot();
    assert_eq!(snapshot_from_wire(snapshot_to_wire(&snapshot)), snapshot);
}

#[test]
fn nfr1_the_report_dsa_memory_prints_names_every_part() {
    let text = snapshot().to_string();
    for part in [
        "Host: 820 MiB in all",
        "vm",
        "window manager",
        "app window",
        "Android (42 s ago): 2048 of 4096 MiB in use",
        "page cache 900 MiB",
        "swap 100 of 2048 MiB used; zram holds 100 MiB in 30 MiB",
        "650 MiB PSS in all",
        "system",
    ] {
        assert!(text.contains(part), "{part:?} missing from:\n{text}");
    }
}

/// One user's running Android, as the service's log sees it.
struct OneUser(Arc<Session>);

impl UserAndroids for OneUser {
    fn android(&self, _user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        Ok(Arc::clone(&self.0) as Arc<dyn UserAndroid>)
    }
    fn running(&self) -> Vec<(String, SessionState)> {
        vec![("S-1-5-21-7".to_string(), self.0.state())]
    }
    fn stop_all(&self, _timeout: Duration) -> Vec<(String, SessionState)> {
        Vec::new()
    }
}

#[test]
fn nfr1_the_service_logs_one_memory_line_for_each_running_android() {
    let mut setup = setup();
    send(&mut setup.agent, Body::GuestMemory(report()));
    wait_for_report(&setup.session);
    let host = FakeHost::default();
    let lines = summary_lines(&OneUser(Arc::clone(&setup.session)), &host);
    assert_eq!(lines.len(), 1);
    assert!(
        lines[0].starts_with("S-1-5-21-7: memory: host 820 MiB (vm 600,"),
        "{}",
        lines[0]
    );
    assert!(
        lines[0].contains("guest 2048/4096 MiB used"),
        "{}",
        lines[0]
    );

    setup.session.stop();
    assert!(setup.session.wait_for(SessionState::Stopped, WAIT));
    assert!(summary_lines(&OneUser(Arc::clone(&setup.session)), &host).is_empty());
}

fn opened(task_id: i32) -> TaskEvent {
    TaskEvent::Updated(Task {
        task_id,
        ..Task::default()
    })
}

#[test]
fn nfr1_the_window_manager_gives_memory_back_once_every_window_is_minimized() {
    let start = Instant::now();
    let at = |s: u64| start + Duration::from_secs(s);
    let mut trim = IdleTrim::new(Duration::from_secs(30), start);
    trim.sync(&[opened(1)], at(0));
    assert!(!trim.due(at(100)));
    trim.windows_shown(false, at(100));
    assert!(!trim.due(at(110)));
    assert!(trim.due(at(130)));
    assert!(!trim.due(at(200)));
    // Shown again, then minimized again: a new idle stretch.
    trim.windows_shown(true, at(210));
    assert!(!trim.due(at(300)));
    trim.windows_shown(false, at(300));
    assert!(trim.due(at(330)));
}

#[test]
fn nfr2_the_window_manager_gives_memory_back_once_no_window_has_been_open_a_while() {
    let start = Instant::now();
    let delay = Duration::from_secs(30);
    let at = |s: u64| start + Duration::from_secs(s);
    let mut trim = IdleTrim::new(delay, start);
    // Idle from start-up: once, after the delay.
    assert!(!trim.due(at(10)));
    assert!(trim.due(at(30)));
    assert!(!trim.due(at(60)));

    trim.sync(&[opened(1), opened(2)], at(70));
    assert!(!trim.due(at(200)));
    trim.task_event(Some(&TaskEvent::Removed { task_id: 1 }), at(210));
    assert!(!trim.due(at(300)), "a window is still open");
    trim.task_event(Some(&TaskEvent::Removed { task_id: 2 }), at(310));
    assert!(!trim.due(at(320)));
    assert!(trim.due(at(340)));
    assert!(!trim.due(at(400)));

    // The service out of reach counts as no window.
    trim.task_event(Some(&opened(3)), at(410));
    trim.task_event(None, at(420));
    assert!(trim.due(at(450)));
}
