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
