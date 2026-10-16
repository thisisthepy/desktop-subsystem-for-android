//! The local API's behaviour over any transport, as tests that run against each of them.
//!
//! A crate that implements a local API transport runs the suite with
//! [`host_api_conformance!`](crate::host_api_conformance), passing an expression that makes
//! a fresh listener, a connector that reaches it, and the user the transport will report
//! for this process. The in-memory transport runs the same suite.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dsa_protocol::host_api::types::{ErrorCode, Role};
use dsa_protocol::host_api::{client_connect, local_hello, HandshakeError};
use dsa_transport::local::LocalListener;
use dsa_transport::Connector;

use crate::host_api::{ClientError, HostApiClient, HostApiService, UserAndroid, UserAndroids};
use crate::session::SessionState;

/// A fresh listener, a connector that reaches it, and the user it reports for this process.
pub type LocalPair = (Box<dyn LocalListener>, Box<dyn Connector>, String);

/// Makes a [`LocalPair`].
pub type MakeLocal<'a> = &'a mut dyn FnMut() -> LocalPair;

const WAIT: Duration = Duration::from_secs(10);

/// An Android that only remembers what it was asked.
pub struct FakeAndroid {
    state: Mutex<SessionState>,
}

impl FakeAndroid {
    pub fn new() -> FakeAndroid {
        FakeAndroid {
            state: Mutex::new(SessionState::Stopped),
        }
    }
}

impl Default for FakeAndroid {
    fn default() -> Self {
        FakeAndroid::new()
    }
}

impl UserAndroid for FakeAndroid {
    fn state(&self) -> SessionState {
        *self.state.lock().unwrap()
    }
    fn last_reason(&self) -> Option<String> {
        (self.state() == SessionState::Stopped).then(|| "stopped on request".into())
    }
    fn start(&self) {
        *self.state.lock().unwrap() = SessionState::Active;
    }
    fn stop(&self) {
        *self.state.lock().unwrap() = SessionState::Stopped;
    }
}

/// One [`FakeAndroid`] per user, except for the users it refuses.
#[derive(Default)]
pub struct FakeAndroids {
    pub androids: Mutex<HashMap<String, Arc<FakeAndroid>>>,
    pub refused: Vec<String>,
}

impl FakeAndroids {
    /// The Android of `user`, if that user has asked for it.
    pub fn of(&self, user: &str) -> Option<Arc<FakeAndroid>> {
        self.androids.lock().unwrap().get(user).cloned()
    }
}

impl UserAndroids for FakeAndroids {
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        if self.refused.iter().any(|refused| refused == user) {
            return Err(format!("{user} may not run Android on this machine"));
        }
        let android: Arc<FakeAndroid> = Arc::clone(
            self.androids
                .lock()
                .unwrap()
                .entry(user.to_string())
                .or_default(),
        );
        Ok(android)
    }
}

/// A service running on its own thread, stopped and joined on drop.
pub struct RunningService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RunningService {
    pub fn start(listener: Box<dyn LocalListener>, androids: Arc<dyn UserAndroids>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let service = Arc::new(HostApiService::new(androids, "test service"));
        let flag = Arc::clone(&stop);
        let thread = thread::spawn(move || service.serve(listener, flag));
        RunningService {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for RunningService {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Generates one `#[test]` per check, named after the requirement it defends.
///
/// ```text
/// dsa_host_core::host_api_conformance!(memory_local_pair());
/// ```
#[macro_export]
macro_rules! host_api_conformance {
    ($make:expr) => {
        #[test]
        fn pr5_the_client_reads_the_state_of_its_users_android() {
            $crate::host_api_conformance::reads_its_users_state(&mut || $make);
        }
        #[test]
        fn pr5_start_and_stop_reach_the_callers_android() {
            $crate::host_api_conformance::start_and_stop_reach_the_caller(&mut || $make);
        }
        #[test]
        fn pr5_a_client_from_a_different_definition_is_refused() {
            $crate::host_api_conformance::different_definition_is_refused(&mut || $make);
        }
        #[test]
        fn pr5_a_broken_client_does_not_stop_the_service() {
            $crate::host_api_conformance::broken_client_does_not_stop_the_service(&mut || $make);
        }
        #[test]
        fn pr5_a_user_without_an_android_is_told_why() {
            $crate::host_api_conformance::user_without_an_android_is_told_why(&mut || $make);
        }
    };
}

fn connect(connector: &mut dyn Connector) -> HostApiClient {
    HostApiClient::connect(connector, "test client", WAIT).expect("connecting to the service")
}

pub fn reads_its_users_state(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    let status = client.status().expect("status");
    assert_eq!(status.state, SessionState::Stopped);
    assert_eq!(status.last_reason.as_deref(), Some("stopped on request"));
    let users: Vec<String> = androids.androids.lock().unwrap().keys().cloned().collect();
    assert_eq!(
        users,
        vec![user],
        "the service must act for the user the transport reports"
    );
}

pub fn start_and_stop_reach_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    assert_eq!(client.start().unwrap().state, SessionState::Active);
    assert_eq!(androids.of(&user).unwrap().state(), SessionState::Active);
    assert_eq!(client.stop().unwrap().state, SessionState::Stopped);
    assert_eq!(androids.of(&user).unwrap().state(), SessionState::Stopped);
}

pub fn different_definition_is_refused(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut connection = connector.connect(WAIT).unwrap();
    let mut hello = local_hello(Role::Client, "old client");
    hello.schema_hash[0] ^= 0xff;
    let result = client_connect(&mut *connection, &hello, WAIT);
    assert!(
        matches!(result, Err(HandshakeError::Rejected { .. })),
        "{result:?}"
    );
}

pub fn broken_client_does_not_stop_the_service(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    // One client sends garbage before the handshake, another after it.
    let mut garbage = connector.connect(WAIT).unwrap();
    garbage.send(&[0xff; 7]).unwrap();
    let mut after = connector.connect(WAIT).unwrap();
    client_connect(&mut *after, &local_hello(Role::Client, "client"), WAIT).unwrap();
    after.send(&[0xff; 7]).unwrap();
    assert!(
        after.recv(Some(WAIT)).is_err(),
        "the service closes a broken client"
    );
    let mut client = connect(&mut *connector);
    assert_eq!(client.status().unwrap().state, SessionState::Stopped);
}

pub fn user_without_an_android_is_told_why(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids {
        refused: vec![user.clone()],
        ..FakeAndroids::default()
    });
    let _service = RunningService::start(listener, androids);
    let mut client = connect(&mut *connector);
    match client.start() {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::Unavailable);
            assert!(detail.contains(&user), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
}
