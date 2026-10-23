//! The local API over the in-memory transport, where the test chooses each caller's user.

use std::sync::Arc;
use std::time::Duration;

use dsa_host_core::host_api::{HostApiClient, PerUser, UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::session::SessionState;
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
        })
    }
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
