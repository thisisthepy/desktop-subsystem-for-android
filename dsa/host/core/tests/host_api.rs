//! The local API over the in-memory transport, where the test chooses each caller's user.

use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::host_api::{HostApiClient, PerUser, UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::ime::ImeEvent;
use dsa_host_core::input::PointerEvent;
use dsa_host_core::keys::KeyEvent;
use dsa_host_core::session::SessionState;
use dsa_host_core::tasks::TaskRequestError;
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::{Connection, Connector, TransportError};

const WAIT: Duration = Duration::from_secs(10);

fn memory_local_pair() -> dsa_host_core::host_api_conformance::LocalPair {
    let (listener, connector) = local::pair();
    (Box::new(listener), Box::new(connector), "test".into())
}

dsa_host_core::host_api_conformance!(memory_local_pair());

/// Connects as a chosen user, as if the OS had reported that user.
struct As(MemoryLocalConnector, &'static str);

impl Connector for As {
    fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.connect_as(Peer {
            user: self.1.into(),
            administrator: false,
        })
    }
}

/// Connects as a chosen user acting as an administrator, or not.
struct AsAdmin(MemoryLocalConnector, &'static str, bool);

impl Connector for AsAdmin {
    fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.connect_as(Peer {
            user: self.1.into(),
            administrator: self.2,
        })
    }
}

fn settings_service(
    listener: local::MemoryLocalListener,
) -> (RunningService, Arc<dsa_host_core::settings::MemorySettings>) {
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    let store = Arc::new(dsa_host_core::settings::MemorySettings::default());
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(store.clone(), ServiceFacts::default());
    (
        RunningService::start_service(Box::new(listener), service),
        store,
    )
}

#[test]
fn fr17_only_an_administrator_changes_the_machines_update_settings() {
    use dsa_host_core::host_api::ClientError;
    use dsa_host_core::settings::{Settings, SettingsStore};
    use dsa_protocol::host_api::types::ErrorCode;
    let (listener, connector) = local::pair();
    let (_service, store) = settings_service(listener);
    let mut user =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();
    let off = Settings {
        program_updates: false,
        ..Settings::default()
    };
    match user.set_settings(&off) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::NotAllowed);
            assert!(detail.contains("only an administrator"), "{detail}");
            assert!(detail.contains("program updates"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    assert!(store.machine().program_updates, "nothing changed");
    assert_eq!(store.get("alice").unwrap(), Settings::default());

    // The user's own settings stay theirs to change, with the machine's left as they are.
    let mine = user
        .set_settings(&Settings {
            render_gpu: "integrated".into(),
            freeze: true,
            ..Settings::default()
        })
        .unwrap();
    assert_eq!(mine.settings.render_gpu, "integrated");

    let mut admin =
        HostApiClient::connect(&mut AsAdmin(connector, "bob", true), "b", WAIT).unwrap();
    let changed = admin.set_settings(&off).unwrap();
    assert!(!changed.settings.program_updates);
    assert!(!store.machine().program_updates);
}

#[test]
fn fr19_the_trace_switches_are_machine_settings_only_an_administrator_changes() {
    use dsa_host_core::host_api::ClientError;
    use dsa_host_core::settings::{Settings, SettingsStore};
    let (listener, connector) = local::pair();
    let (_service, store) = settings_service(listener);
    let traced = Settings {
        input_trace: true,
        resize_trace: true,
        ..Settings::default()
    };
    let mut user =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();
    match user.set_settings(&traced) {
        Err(ClientError::Service { detail, .. }) => {
            assert!(detail.contains("input trace"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    assert!(!store.machine().input_trace);
    let mut admin =
        HostApiClient::connect(&mut AsAdmin(connector, "bob", true), "b", WAIT).unwrap();
    let changed = admin.set_settings(&traced).unwrap();
    assert!(changed.settings.input_trace && changed.settings.resize_trace);
    // Every user reads them, so their processes trace too.
    assert!(user.settings().unwrap().settings.input_trace);
}

#[test]
fn fr28_advanced_networking_is_the_users_and_the_network_is_reported_with_the_settings() {
    use dsa_host_core::host_api::{HostApiService, NetworkSource, NetworkStatus, ServiceFacts};
    use dsa_host_core::settings::{Settings, SettingsStore};
    let (listener, connector) = local::pair();
    let store = Arc::new(dsa_host_core::settings::MemorySettings::default());
    // Alice's Android runs; Bob's is not running.
    let network: NetworkSource = Arc::new(|user: &str, settings: &Settings| {
        (user == "alice").then(|| NetworkStatus {
            mode: if settings.advanced_networking {
                NetworkStatus::ADVANCED.into()
            } else {
                NetworkStatus::NAT.into()
            },
            addresses: vec!["172.28.0.2".into(), "fd5a:d5a::2".into()],
            dns_servers: vec!["172.28.0.1".into()],
            gateway: "172.28.0.1".into(),
            problem: None,
            dns_resolver: String::new(),
        })
    });
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(store.clone(), ServiceFacts::default())
        .with_network(network);
    let _service = RunningService::start_service(Box::new(listener), service);

    let mut alice =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();
    let report = alice.settings().unwrap();
    assert!(!report.settings.advanced_networking, "off by default");
    let status = report.network.expect("the network is reported");
    assert_eq!(status.mode, NetworkStatus::NAT);
    assert_eq!(status.addresses, vec!["172.28.0.2", "fd5a:d5a::2"]);
    assert_eq!(status.dns_servers, vec!["172.28.0.1"]);
    assert_eq!(status.gateway, "172.28.0.1");

    // Any user may turn it on for themselves, without being an administrator.
    let on = alice
        .set_settings(&Settings {
            advanced_networking: true,
            ..report.settings.clone()
        })
        .unwrap();
    assert!(on.settings.advanced_networking);
    assert_eq!(on.network.unwrap().mode, NetworkStatus::ADVANCED);
    assert!(store.get("alice").unwrap().advanced_networking);
    assert!(!store.get("bob").unwrap().advanced_networking);

    let mut bob = HostApiClient::connect(&mut AsAdmin(connector, "bob", false), "b", WAIT).unwrap();
    assert_eq!(
        bob.settings().unwrap().network.unwrap().mode,
        NetworkStatus::OFF
    );
}

#[test]
fn fr28_the_network_says_whether_the_hosts_resolver_answers_androids_names() {
    use dsa_host_core::host_api::{HostApiService, NetworkSource, NetworkStatus, ServiceFacts};
    use dsa_host_core::settings::Settings;
    let (listener, connector) = local::pair();
    // Alice's Android runs; Bob's is not running.
    let network: NetworkSource = Arc::new(|user: &str, _: &Settings| {
        (user == "alice").then(|| NetworkStatus {
            mode: NetworkStatus::NAT.into(),
            addresses: vec!["172.28.0.2".into()],
            dns_servers: vec!["172.28.0.1".into()],
            gateway: "172.28.0.1".into(),
            ..NetworkStatus::default()
        })
    });
    let androids = Arc::new(FakeAndroids::default());
    let service = HostApiService::new(androids.clone(), "test service")
        .with_settings(
            Arc::new(dsa_host_core::settings::MemorySettings::default()),
            ServiceFacts::default(),
        )
        .with_network(network);
    let _service = RunningService::start_service(Box::new(listener), service);
    let mut alice =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();

    // Still booting: nobody can say yet.
    let status = alice.settings().unwrap().network.unwrap();
    assert_eq!(status.dns_resolver, "");

    // The guest's control daemon holds the DNS port: the host's resolver answers.
    *androids
        .of("alice")
        .unwrap()
        .dns_answered_by_host
        .lock()
        .unwrap() = Some(true);
    let status = alice.settings().unwrap().network.unwrap();
    assert_eq!(status.dns_resolver, NetworkStatus::DNS_HOST);

    // It does not reach the host's DNS service: Android asks its network's DNS servers.
    *androids
        .of("alice")
        .unwrap()
        .dns_answered_by_host
        .lock()
        .unwrap() = Some(false);
    let status = alice.settings().unwrap().network.unwrap();
    assert_eq!(status.dns_resolver, NetworkStatus::DNS_NETWORK);
    assert_eq!(status.dns_servers, vec!["172.28.0.1"]);

    // A stopped Android has no network, so nothing answers its names.
    let mut bob = HostApiClient::connect(&mut AsAdmin(connector, "bob", false), "b", WAIT).unwrap();
    let status = bob.settings().unwrap().network.unwrap();
    assert_eq!(status.mode, NetworkStatus::OFF);
    assert_eq!(status.dns_resolver, "");
}

#[test]
fn pr5_each_user_reaches_only_their_own_android() {
    let (listener, connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(Box::new(listener), androids.clone());
    let mut alice = HostApiClient::connect(&mut As(connector.clone(), "alice"), "a", WAIT).unwrap();
    let mut bob = HostApiClient::connect(&mut As(connector, "bob"), "b", WAIT).unwrap();

    alice.start().unwrap();
    assert_eq!(androids.of("alice").unwrap().state(), SessionState::Active);
    assert_eq!(bob.status().unwrap().state, SessionState::Stopped);
    assert_eq!(androids.of("bob").unwrap().state(), SessionState::Stopped);

    bob.stop().unwrap();
    assert_eq!(
        alice.status().unwrap().state,
        SessionState::Active,
        "bob's stop must not reach alice's Android"
    );
}

#[test]
fn pr5_each_user_gets_one_android_created_on_first_use() {
    let created = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let log = Arc::clone(&created);
    let per_user = PerUser::new(move |user: &str| {
        log.lock().unwrap().push(user.to_string());
        Ok(Arc::new(FakeAndroid::new()) as Arc<dyn UserAndroid>)
    });
    let first = per_user.android("alice").unwrap();
    first.start();
    assert_eq!(
        per_user.android("alice").unwrap().state(),
        SessionState::Active
    );
    per_user.android("bob").unwrap();
    assert_eq!(*created.lock().unwrap(), vec!["alice", "bob"]);
}

#[test]
fn pr5_a_user_the_factory_refuses_is_retried_on_the_next_request() {
    let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let counter = Arc::clone(&calls);
    let per_user = PerUser::new(move |_: &str| {
        if counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
            Err("the VM image is still being installed".to_string())
        } else {
            Ok(Arc::new(FakeAndroid::new()) as Arc<dyn UserAndroid>)
        }
    });
    assert!(per_user.android("alice").is_err());
    assert!(per_user.android("alice").is_ok());
}

#[test]
fn fr2_stopping_the_service_stops_every_users_android() {
    let per_user = PerUser::new(|_: &str| Ok(Arc::new(FakeAndroid::new()) as Arc<dyn UserAndroid>));
    let alice = per_user.android("alice").unwrap();
    let bob = per_user.android("bob").unwrap();
    alice.start();
    bob.start();
    let not_stopped = per_user.stop_all(Duration::from_secs(5));
    assert!(not_stopped.is_empty(), "{not_stopped:?}");
    assert_eq!(alice.state(), SessionState::Stopped);
    assert_eq!(bob.state(), SessionState::Stopped);
}

/// An Android with news about an update.
struct Notifying;

impl UserAndroid for Notifying {
    fn state(&self) -> SessionState {
        SessionState::Stopped
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {}
    fn stop(&self) {}
    fn update_notice(&self) -> Option<String> {
        Some("Android 16.0.3 is downloaded and will be used at the next start".into())
    }
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
    }
}

#[test]
fn fr14_the_user_is_told_about_an_update_through_the_local_api() {
    let (listener, mut connector) = local::pair();
    let androids = Arc::new(PerUser::new(|_: &str| {
        Ok(Arc::new(Notifying) as Arc<dyn UserAndroid>)
    }));
    let _service = RunningService::start(Box::new(listener), androids);
    let mut client = HostApiClient::connect(&mut connector, "test", WAIT).unwrap();
    let status = client.status().unwrap();
    assert_eq!(
        status.update_notice.as_deref(),
        Some("Android 16.0.3 is downloaded and will be used at the next start")
    );
    assert_eq!(status.last_reason, None);
}

struct CrashedAndroid {
    state: std::sync::Mutex<SessionState>,
    reason: std::sync::Mutex<Option<String>>,
}

impl UserAndroid for CrashedAndroid {
    fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
    }
    fn last_reason(&self) -> Option<String> {
        self.reason.lock().unwrap().clone()
    }
    fn start(&self) {
        *self.state.lock().unwrap() = SessionState::Active;
        *self.reason.lock().unwrap() = None;
    }
    fn stop(&self) {
        *self.state.lock().unwrap() = SessionState::Stopped;
        *self.reason.lock().unwrap() = Some("stopped on request".into());
    }
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
    }
}

#[test]
fn nfr5_a_crashed_android_reports_stopped_and_can_start_again() {
    let (listener, mut connector) = local::pair();
    let android = Arc::new(CrashedAndroid {
        state: std::sync::Mutex::new(SessionState::Stopped),
        reason: std::sync::Mutex::new(None),
    });
    let androids = Arc::new(PerUser::new({
        let android = Arc::clone(&android);
        move |_: &str| Ok(android.clone() as Arc<dyn UserAndroid>)
    }));
    let _service = RunningService::start(Box::new(listener), androids);
    let mut client = HostApiClient::connect(&mut connector, "test", WAIT).unwrap();

    client.start().unwrap();
    assert_eq!(client.status().unwrap().state, SessionState::Active);

    // Simulate guest crash or channel loss by setting the guest state to Stopped with a cause.
    *android.state.lock().unwrap() = SessionState::Stopped;
    *android.reason.lock().unwrap() = Some("the control channel was lost: connection reset".into());

    let status = client.status().unwrap();
    assert_eq!(status.state, SessionState::Stopped);
    assert_eq!(
        status.last_reason.as_deref(),
        Some("the control channel was lost: connection reset")
    );

    // The user can start Android again.
    client.start().unwrap();
    assert_eq!(client.status().unwrap().state, SessionState::Active);
}

/// An Android that boots for a few status reads before it runs, or fails to boot.
struct Booting {
    state: std::sync::Mutex<SessionState>,
    reads_left: std::sync::Mutex<u32>,
    fails: bool,
    starts: std::sync::atomic::AtomicU32,
}

impl Booting {
    fn new(state: SessionState, reads: u32, fails: bool) -> Booting {
        Booting {
            state: std::sync::Mutex::new(state),
            reads_left: std::sync::Mutex::new(reads),
            fails,
            starts: std::sync::atomic::AtomicU32::new(0),
        }
    }

    fn starts(&self) -> u32 {
        self.starts.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl UserAndroid for Booting {
    fn state(&self) -> SessionState {
        let mut state = self.state.lock().unwrap();
        let mut left = self.reads_left.lock().unwrap();
        if matches!(*state, SessionState::Starting | SessionState::Stopping) {
            if *left == 0 {
                *state = match *state {
                    SessionState::Starting if self.fails => SessionState::Stopped,
                    SessionState::Starting => SessionState::Active,
                    _ => SessionState::Stopped,
                };
            } else {
                *left -= 1;
            }
        }
        *state
    }
    fn last_reason(&self) -> Option<String> {
        self.fails
            .then(|| "the guest did not start: no disk".to_string())
    }
    fn start(&self) {
        self.starts
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut state = self.state.lock().unwrap();
        if *state == SessionState::Stopped {
            *state = SessionState::Starting;
            *self.reads_left.lock().unwrap() = 3;
        }
    }
    fn stop(&self) {}
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
    }
}

fn booting_client(android: Arc<Booting>) -> (HostApiClient, RunningService) {
    let (listener, mut connector) = local::pair();
    let androids = Arc::new(PerUser::new(move |_: &str| {
        Ok(Arc::clone(&android) as Arc<dyn UserAndroid>)
    }));
    let service = RunningService::start(Box::new(listener), androids);
    let client = HostApiClient::connect(&mut connector, "test", WAIT).unwrap();
    (client, service)
}

#[test]
fn fr7_opening_an_app_starts_a_stopped_android_and_waits_until_it_runs() {
    let android = Arc::new(Booting::new(SessionState::Stopped, 0, false));
    let (mut client, _service) = booting_client(Arc::clone(&android));
    let mut seen = Vec::new();
    client
        .ensure_running(WAIT, Duration::from_millis(1), |state| seen.push(state))
        .expect("Android runs");
    assert_eq!(android.state(), SessionState::Active);
    assert_eq!(android.starts(), 1);
    assert_eq!(seen, vec![SessionState::Starting]);
}

#[test]
fn fr7_opening_an_app_while_android_runs_starts_nothing() {
    let android = Arc::new(Booting::new(SessionState::Active, 0, false));
    let (mut client, _service) = booting_client(Arc::clone(&android));
    client
        .ensure_running(WAIT, Duration::from_millis(1), |_| {})
        .expect("Android runs");
    assert_eq!(android.starts(), 0);
}

#[test]
fn fr7_opening_an_app_while_android_stops_starts_it_again_once_stopped() {
    let android = Arc::new(Booting::new(SessionState::Stopping, 2, false));
    let (mut client, _service) = booting_client(Arc::clone(&android));
    client
        .ensure_running(WAIT, Duration::from_millis(1), |_| {})
        .expect("Android runs");
    assert_eq!(android.state(), SessionState::Active);
    assert_eq!(android.starts(), 1);
}

/// An Android that runs but has stalled, and says why.
struct Stalled;

impl UserAndroid for Stalled {
    fn state(&self) -> SessionState {
        SessionState::Stalled
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn problem(&self) -> Option<String> {
        Some("the window manager cannot start".into())
    }
    fn start(&self) {}
    fn stop(&self) {}
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
    }
}

fn stalled_client() -> (HostApiClient, RunningService) {
    let (listener, mut connector) = local::pair();
    let androids = Arc::new(PerUser::new(move |_: &str| {
        Ok(Arc::new(Stalled) as Arc<dyn UserAndroid>)
    }));
    let service = RunningService::start(Box::new(listener), androids);
    let client = HostApiClient::connect(&mut connector, "test", WAIT).unwrap();
    (client, service)
}

#[test]
fn fr2_the_status_carries_the_stalled_state_and_the_problem() {
    let (mut client, _service) = stalled_client();
    let status = client.status().unwrap();
    assert_eq!(status.state, SessionState::Stalled);
    assert_eq!(
        status.problem.as_deref(),
        Some("the window manager cannot start")
    );
}

#[test]
fn fr7_opening_an_app_on_a_stalled_android_says_why_at_once() {
    let (mut client, _service) = stalled_client();
    match client.ensure_running(WAIT, Duration::from_millis(1), |_| {}) {
        Err(dsa_host_core::host_api::ClientError::Service { detail, .. }) => {
            assert!(detail.contains("window manager"), "{detail}")
        }
        other => panic!("expected the problem, got {other:?}"),
    }
}

#[test]
fn fr7_an_android_that_does_not_start_says_why() {
    let android = Arc::new(Booting::new(SessionState::Stopped, 0, true));
    let (mut client, _service) = booting_client(android);
    match client.ensure_running(WAIT, Duration::from_millis(1), |_| {}) {
        Err(dsa_host_core::host_api::ClientError::Service { detail, .. }) => {
            assert!(detail.contains("no disk"), "{detail}")
        }
        other => panic!("expected the reason, got {other:?}"),
    }
}

/// A folder of its own for one data reset test, empty.
fn reset_scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dsa-reset-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A data reset over the fake paths: each user's disk is `android.vhdx` in their root, and a
/// fresh one is a copy of `template` with the image version recorded beside it, unless
/// `fail` says why it cannot be made.
fn template_reset(
    paths: dsa_host_core::host_paths::FakeHostPaths,
    template: std::path::PathBuf,
    fail: Option<&'static str>,
) -> dsa_host_core::data_reset::TemplateDataDisk {
    use dsa_host_core::host_paths::HostPaths;
    dsa_host_core::data_reset::TemplateDataDisk::new(
        Box::new(move |user: &str| Ok(paths.user_data(user)?.join("android.vhdx"))),
        Box::new(move |disk: &std::path::Path| {
            if let Some(why) = fail {
                std::fs::write(disk, b"half a disk").unwrap();
                return Err(why.into());
            }
            std::fs::copy(&template, disk).map_err(|e| e.to_string())?;
            std::fs::write(disk.with_extension("image-version"), "2").map_err(|e| e.to_string())
        }),
    )
    .with_beside("image-version")
}

#[test]
fn fr31_a_data_reset_stops_android_and_gives_it_a_fresh_disk_keeping_the_image_and_settings() {
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::{Settings, SettingsStore};
    use dsa_host_core::widgets::WidgetPlacements;
    let dir = reset_scratch("fresh");
    let paths =
        dsa_host_core::host_paths::FakeHostPaths::new(dir.join("machine"), dir.join("users"));
    let images = dir.join("machine").join("images");
    std::fs::create_dir_all(&images).unwrap();
    let template = images.join("disk.vhdx");
    std::fs::write(&template, b"fresh Android").unwrap();
    let user_root = dir.join("users").join("alice");
    std::fs::create_dir_all(&user_root).unwrap();
    std::fs::write(
        user_root.join("android.vhdx"),
        b"apps, accounts and their data",
    )
    .unwrap();
    std::fs::write(user_root.join("android.image-version"), "1").unwrap();

    let store = Arc::new(dsa_host_core::settings::MemorySettings::default());
    let chosen = Settings {
        clipboard: false,
        ..Settings::default()
    };
    store.set("alice", &chosen).unwrap();
    let androids = Arc::new(FakeAndroids::default());
    let placements = Arc::new(dsa_host_core::widgets::MemoryWidgetPlacements::default());
    placements
        .set("alice", 7, dsa_host_core::widgets::Placement::default())
        .unwrap();
    let (listener, connector) = local::pair();
    let service = HostApiService::new(androids.clone(), "test service")
        .with_settings(store.clone(), ServiceFacts::default())
        .with_widget_placements(placements.clone())
        .with_data_reset(Arc::new(template_reset(paths, template.clone(), None)));
    let _service = RunningService::start_service(Box::new(listener), service);
    let mut client = HostApiClient::connect(&mut As(connector, "alice"), "a", WAIT).unwrap();
    client.start().unwrap();
    assert_eq!(androids.of("alice").unwrap().state(), SessionState::Active);

    client.reset_data().expect("the reset");
    assert_eq!(
        androids.of("alice").unwrap().state(),
        SessionState::Stopped,
        "Android was stopped first"
    );
    assert_eq!(
        std::fs::read(user_root.join("android.vhdx")).unwrap(),
        b"fresh Android",
        "the disk is a fresh copy of the image's template"
    );
    assert_eq!(
        std::fs::read_to_string(user_root.join("android.image-version")).unwrap(),
        "2"
    );
    assert!(
        !user_root.join("android.vhdx.before-reset").exists(),
        "the old disk is gone"
    );
    assert_eq!(
        std::fs::read(&template).unwrap(),
        b"fresh Android",
        "the image is untouched"
    );
    assert_eq!(store.get("alice").unwrap(), chosen, "the settings are kept");
    assert!(
        placements.all("alice").unwrap().is_empty(),
        "the widgets of the old data are forgotten"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr31_a_reset_that_cannot_make_a_new_disk_keeps_the_old_one() {
    use dsa_host_core::host_api::{ClientError, HostApiService};
    let dir = reset_scratch("failed");
    let paths =
        dsa_host_core::host_paths::FakeHostPaths::new(dir.join("machine"), dir.join("users"));
    let user_root = dir.join("users").join("alice");
    std::fs::create_dir_all(&user_root).unwrap();
    std::fs::write(user_root.join("android.vhdx"), b"the user's data").unwrap();
    std::fs::write(user_root.join("android.image-version"), "1").unwrap();
    let (listener, connector) = local::pair();
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_data_reset(Arc::new(template_reset(
            paths,
            dir.join("no-template"),
            Some("the disk is full"),
        )));
    let _service = RunningService::start_service(Box::new(listener), service);
    let mut client = HostApiClient::connect(&mut As(connector, "alice"), "a", WAIT).unwrap();
    match client.reset_data() {
        Err(ClientError::Service { detail, .. }) => {
            assert!(detail.contains("the disk is full"), "{detail}");
            assert!(detail.contains("old one was kept"), "{detail}");
        }
        other => panic!("expected the reset to fail, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(user_root.join("android.vhdx")).unwrap(),
        b"the user's data"
    );
    assert_eq!(
        std::fs::read_to_string(user_root.join("android.image-version")).unwrap(),
        "1"
    );
    // The connection goes on serving.
    assert_eq!(client.status().unwrap().state, SessionState::Stopped);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fr31_a_service_that_cannot_reset_says_so() {
    use dsa_host_core::host_api::ClientError;
    use dsa_protocol::host_api::types::ErrorCode;
    let (listener, connector) = local::pair();
    let _service = RunningService::start(Box::new(listener), Arc::new(FakeAndroids::default()));
    let mut client = HostApiClient::connect(&mut As(connector, "alice"), "a", WAIT).unwrap();
    match client.reset_data() {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::Unavailable);
            assert!(detail.contains("cannot reset"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
}

#[test]
fn fr15_only_an_administrator_chooses_the_release_channel_and_every_user_sees_it() {
    use dsa_host_core::host_api::ClientError;
    use dsa_host_core::settings::{Settings, SettingsStore};
    let (listener, connector) = local::pair();
    let (_service, store) = settings_service(listener);
    let prereleases = Settings {
        prerelease_updates: true,
        ..Settings::default()
    };
    let mut user =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();
    match user.set_settings(&prereleases) {
        Err(ClientError::Service { detail, .. }) => {
            assert!(detail.contains("pre-release updates"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    assert!(!store.machine().prerelease_updates);
    let mut admin =
        HostApiClient::connect(&mut AsAdmin(connector, "bob", true), "b", WAIT).unwrap();
    let changed = admin.set_settings(&prereleases).unwrap();
    assert!(changed.settings.prerelease_updates);
    assert!(store.machine().prerelease_updates);
    assert!(user.settings().unwrap().settings.prerelease_updates);
}

/// An Android that keeps the root elevation settings it is given.
#[derive(Default)]
struct RootRecorder {
    given: std::sync::Mutex<Vec<(bool, Vec<dsa_host_core::root_elevation::AppLevel>)>>,
    /// The administrator prompt's answers to letting an app keep root, in order, and the
    /// packages it was shown for.
    keep_root_answers: std::sync::Mutex<Vec<Result<(), String>>>,
    keep_root_asked: std::sync::Mutex<Vec<String>>,
}

impl RootRecorder {
    fn last(&self) -> Option<(bool, Vec<dsa_host_core::root_elevation::AppLevel>)> {
        self.given.lock().unwrap().last().cloned()
    }
}

impl UserAndroid for RootRecorder {
    fn state(&self) -> SessionState {
        SessionState::Active
    }
    fn last_reason(&self) -> Option<String> {
        None
    }
    fn start(&self) {}
    fn stop(&self) {}
    fn pointer(&self, _: &PointerEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn key(&self, _: &KeyEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn ime(&self, _: &ImeEvent) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed("no input here".into()))
    }
    fn share_clipboard(&self, _: bool) {}
    fn host_clipboard(&self, _: String) {}
    fn watch_clipboard(&self) -> Option<std::sync::mpsc::Receiver<String>> {
        None
    }
    fn set_root_elevation_config(
        &self,
        on: bool,
        apps: Vec<dsa_host_core::root_elevation::AppLevel>,
    ) {
        self.given.lock().unwrap().push((on, apps));
    }
    fn approve_keep_root(&self, package: &str) -> Result<(), String> {
        self.keep_root_asked
            .lock()
            .unwrap()
            .push(package.to_string());
        let mut answers = self.keep_root_answers.lock().unwrap();
        if answers.is_empty() {
            Err("the test gave no answer".into())
        } else {
            answers.remove(0)
        }
    }
}

#[test]
fn fr35_a_user_who_is_not_an_administrator_has_one_approve_remember_in_their_session() {
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::settings::{MachineSettings, SettingsStore};
    use dsa_protocol::host_api::types::AppRootStatus;
    use std::collections::HashMap;
    use std::sync::Mutex;

    let (listener, connector) = local::pair();
    let store = Arc::new(dsa_host_core::settings::MemorySettings::default());
    store
        .set_machine(MachineSettings {
            root_elevation: true,
            ..Default::default()
        })
        .unwrap();
    let made: Arc<Mutex<HashMap<String, Arc<RootRecorder>>>> = Arc::default();
    let androids = {
        let made = Arc::clone(&made);
        Arc::new(PerUser::new(move |user: &str| {
            let android = Arc::new(RootRecorder::default());
            *android.keep_root_answers.lock().unwrap() = vec![
                Err("the computer's administrator refused the root request".into()),
                Ok(()),
            ];
            made.lock()
                .unwrap()
                .insert(user.to_string(), Arc::clone(&android));
            Ok(android as Arc<dyn UserAndroid>)
        }))
    };
    let service = HostApiService::new(androids, "test service")
        .with_settings(store.clone(), ServiceFacts::default());
    let _service = RunningService::start_service(Box::new(listener), service);
    let android_of = |user: &str| made.lock().unwrap().get(user).cloned().unwrap();
    let level_of = |user: &str| {
        store
            .root_apps(user)
            .unwrap()
            .into_iter()
            .find(|app| app.package == "com.example.root")
            .map(|app| app.level)
    };

    // The administrator's prompt refuses: the level stays as it was, and the service says why.
    let mut alice =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();
    alice.set_app_root("com.example.root", "ask").unwrap();
    let refused = alice.set_app_root("com.example.root", "remember").unwrap();
    assert_eq!(refused.status(), AppRootStatus::NotPermitted);
    assert_eq!(refused.level, "ask");
    assert!(refused.message.contains("refused"), "{}", refused.message);
    assert_eq!(level_of("alice").as_deref(), Some("ask"));

    // An administrator approves in alice's session: the level is hers, set for her Android.
    let approved = alice.set_app_root("com.example.root", "remember").unwrap();
    assert_eq!(approved.status(), AppRootStatus::Success);
    assert_eq!(level_of("alice").as_deref(), Some("remember"));
    assert_eq!(
        *android_of("alice").keep_root_asked.lock().unwrap(),
        vec![
            "com.example.root".to_string(),
            "com.example.root".to_string()
        ]
    );
    // Setting it again asks nobody, and neither do the other levels.
    alice.set_app_root("com.example.root", "remember").unwrap();
    alice.set_app_root("com.example.root", "off").unwrap();
    assert_eq!(android_of("alice").keep_root_asked.lock().unwrap().len(), 2);

    // An administrator sets it without a prompt.
    let mut bob = HostApiClient::connect(&mut AsAdmin(connector, "bob", true), "b", WAIT).unwrap();
    let theirs = bob.set_app_root("com.example.root", "remember").unwrap();
    assert_eq!(theirs.status(), AppRootStatus::Success);
    assert!(android_of("bob").keep_root_asked.lock().unwrap().is_empty());
}

#[test]
fn fr35_the_running_android_is_given_each_change_to_root_elevation() {
    use dsa_host_core::host_api::{HostApiService, ServiceFacts};
    use dsa_host_core::root_elevation::{AppLevel, Level};
    use dsa_host_core::settings::Settings;
    use std::collections::HashMap;
    use std::sync::Mutex;

    let (listener, connector) = local::pair();
    let store = Arc::new(dsa_host_core::settings::MemorySettings::default());
    let made: Arc<Mutex<HashMap<String, Arc<RootRecorder>>>> = Arc::default();
    let androids = {
        let made = Arc::clone(&made);
        Arc::new(PerUser::new(move |user: &str| {
            let android = Arc::new(RootRecorder::default());
            made.lock()
                .unwrap()
                .insert(user.to_string(), Arc::clone(&android));
            Ok(android as Arc<dyn UserAndroid>)
        }))
    };
    let service = HostApiService::new(androids, "test service")
        .with_settings(store.clone(), ServiceFacts::default());
    let _service = RunningService::start_service(Box::new(listener), service);
    let android_of = |user: &str| made.lock().unwrap().get(user).cloned().unwrap();

    // An administrator turns the feature on: their Android is given the switch.
    let mut admin =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "bob", true), "b", WAIT).unwrap();
    admin
        .set_settings(&Settings {
            root_elevation: true,
            ..Settings::default()
        })
        .unwrap();
    assert_eq!(android_of("bob").last(), Some((true, Vec::new())));

    // A user sets an app to ask: their running Android is given the new level at once.
    let mut alice =
        HostApiClient::connect(&mut AsAdmin(connector.clone(), "alice", false), "a", WAIT).unwrap();
    let result = alice.set_app_root("com.example.root", "ask").unwrap();
    assert_eq!(result.level, "ask");
    let ask = vec![AppLevel {
        package: "com.example.root".into(),
        level: Level::Ask,
    }];
    assert_eq!(android_of("alice").last(), Some((true, ask.clone())));

    // Set back to off, the app is no longer listed.
    alice.set_app_root("com.example.root", "off").unwrap();
    assert_eq!(android_of("alice").last(), Some((true, Vec::new())));
    alice.set_app_root("com.example.root", "ask").unwrap();

    // The administrator turns the feature off: every user's Android is told, not only theirs.
    admin.set_settings(&Settings::default()).unwrap();
    assert_eq!(android_of("alice").last(), Some((false, ask)));
    assert_eq!(android_of("bob").last(), Some((false, Vec::new())));
}
