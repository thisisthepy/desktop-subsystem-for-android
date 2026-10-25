//! The local API's behaviour over any transport, as tests that run against each of them.
//!
//! A crate that implements a local API transport runs the suite with
//! [`host_api_conformance!`](crate::host_api_conformance), passing an expression that makes
//! a fresh listener, a connector that reaches it, and the user the transport will report
//! for this process. The in-memory transport runs the same suite.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use dsa_protocol::host_api::types::{ErrorCode, Role};
use dsa_protocol::host_api::{client_connect, local_hello, HandshakeError};
use dsa_transport::local::LocalListener;
use dsa_transport::Connector;

use crate::host_api::{
    running_of, stop_every, ClientError, HostApiClient, HostApiService, ProgramUpdate,
    UpdateProgram, UserAndroid, UserAndroids,
};
use crate::session::SessionState;
use crate::tasks::{Bounds, Task, TaskEvent, TaskRequest, TaskRequestError, TaskTable, TaskWatch};

/// A fresh listener, a connector that reaches it, and the user it reports for this process.
pub type LocalPair = (Box<dyn LocalListener>, Box<dyn Connector>, String);

/// Makes a [`LocalPair`].
pub type MakeLocal<'a> = &'a mut dyn FnMut() -> LocalPair;

const WAIT: Duration = Duration::from_secs(10);

/// An Android that only remembers what it was asked.
pub struct FakeAndroid {
    state: Mutex<SessionState>,
    tasks: Mutex<(TaskTable, Vec<Sender<TaskEvent>>)>,
    /// The requests about tasks it was asked to carry out, in order.
    pub requests: Mutex<Vec<TaskRequest>>,
}

impl FakeAndroid {
    pub fn new() -> FakeAndroid {
        FakeAndroid {
            state: Mutex::new(SessionState::Stopped),
            tasks: Mutex::new((TaskTable::new(), Vec::new())),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Changes its tasks as a guest's task message would, and tells the watchers.
    pub fn publish(&self, event: TaskEvent) {
        let mut tasks = self.tasks.lock().unwrap();
        if tasks.0.apply(&event) {
            tasks
                .1
                .retain(|watcher| watcher.send(event.clone()).is_ok());
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
    fn vm_id(&self) -> Option<String> {
        (self.state() == SessionState::Active).then(|| "fake-vm-id-1234".into())
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        let (tx, events) = mpsc::channel();
        let mut tasks = self.tasks.lock().unwrap();
        tasks.1.push(tx);
        Some(TaskWatch {
            current: tasks.0.snapshot(),
            events,
        })
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        if self.state() != SessionState::Active {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.requests.lock().unwrap().push(request);
        Ok(())
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
    fn running(&self) -> Vec<(String, SessionState)> {
        let androids = self.all();
        running_of(androids.iter().map(|(user, android)| (user, android)))
    }
    fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)> {
        stop_every(self.all(), timeout)
    }
}

impl FakeAndroids {
    fn all(&self) -> Vec<(String, Arc<dyn UserAndroid>)> {
        self.androids
            .lock()
            .unwrap()
            .iter()
            .map(|(user, android)| (user.clone(), Arc::clone(android) as Arc<dyn UserAndroid>))
            .collect()
    }
}

/// A program updater that answers with `answer` and counts the requests.
pub struct FakeProgramUpdates {
    pub answer: Result<ProgramUpdate, String>,
    pub requests: Mutex<u32>,
}

impl FakeProgramUpdates {
    pub fn answering(answer: Result<ProgramUpdate, String>) -> FakeProgramUpdates {
        FakeProgramUpdates {
            answer,
            requests: Mutex::new(0),
        }
    }
}

impl UpdateProgram for FakeProgramUpdates {
    fn update_now(&self) -> Result<ProgramUpdate, String> {
        *self.requests.lock().unwrap() += 1;
        self.answer.clone()
    }
}

/// A service running on its own thread, stopped and joined on drop.
pub struct RunningService {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl RunningService {
    pub fn start(listener: Box<dyn LocalListener>, androids: Arc<dyn UserAndroids>) -> Self {
        Self::start_service(listener, HostApiService::new(androids, "test service"))
    }

    /// A service that answers `dsa update` with `program`.
    pub fn start_updating(
        listener: Box<dyn LocalListener>,
        androids: Arc<dyn UserAndroids>,
        program: Arc<dyn UpdateProgram>,
    ) -> Self {
        let service = HostApiService::new(androids, "test service").with_program_updates(program);
        Self::start_service(listener, service)
    }

    fn start_service(listener: Box<dyn LocalListener>, service: HostApiService) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let service = Arc::new(service);
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
        fn pr5_the_client_reads_the_vm_id_of_its_users_android() {
            $crate::host_api_conformance::reads_its_users_vm_id(&mut || $make);
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
        #[test]
        fn fr3_a_watching_client_receives_every_change_to_its_tasks() {
            $crate::host_api_conformance::watcher_receives_task_changes(&mut || $make);
        }
        #[test]
        fn fr3_a_client_that_watches_late_starts_from_the_current_tasks() {
            $crate::host_api_conformance::late_watcher_starts_from_current_tasks(&mut || $make);
        }
        #[test]
        fn fr3_closing_a_task_and_launching_an_app_reach_the_callers_android() {
            $crate::host_api_conformance::close_and_launch_reach_the_caller(&mut || $make);
        }
        #[test]
        fn fr3_a_request_to_an_android_that_is_not_running_says_so() {
            $crate::host_api_conformance::request_to_stopped_android_says_so(&mut || $make);
        }
        #[test]
        fn nfr5_a_watching_client_that_sends_a_request_is_disconnected() {
            $crate::host_api_conformance::watcher_that_sends_is_disconnected(&mut || $make);
        }
        #[test]
        fn fr15_an_update_request_reaches_the_program_updater() {
            $crate::host_api_conformance::update_request_reaches_the_updater(&mut || $make);
        }
        #[test]
        fn fr15_an_update_that_cannot_be_installed_is_reported_with_its_reason() {
            $crate::host_api_conformance::update_failure_is_reported(&mut || $make);
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

pub fn reads_its_users_vm_id(make: MakeLocal<'_>) {
    let (listener, mut connector, _user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids);
    let mut client = connect(&mut *connector);
    assert_eq!(client.vm_id().unwrap(), None);

    client.start().unwrap();
    assert_eq!(client.vm_id().unwrap().as_deref(), Some("fake-vm-id-1234"));

    client.stop().unwrap();
    assert_eq!(client.vm_id().unwrap(), None);
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

pub fn update_request_reaches_the_updater(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let program = Arc::new(FakeProgramUpdates::answering(Ok(ProgramUpdate {
        installing: Some("0.1.1".into()),
        detail: "installing DSA 0.1.1".into(),
    })));
    let _service = RunningService::start_updating(
        listener,
        Arc::new(FakeAndroids::default()),
        program.clone(),
    );
    let mut client = connect(&mut *connector);
    let update = client.update_program(WAIT).expect("the update request");
    assert_eq!(update.installing.as_deref(), Some("0.1.1"));
    assert_eq!(update.detail, "installing DSA 0.1.1");
    assert_eq!(*program.requests.lock().unwrap(), 1);
    // The connection goes on serving after the update request.
    assert_eq!(client.status().unwrap().state, SessionState::Stopped);
}

pub fn update_failure_is_reported(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let program = Arc::new(FakeProgramUpdates::answering(Err(
        "the program list is not signed by the project".into(),
    )));
    let _service =
        RunningService::start_updating(listener, Arc::new(FakeAndroids::default()), program);
    let mut client = connect(&mut *connector);
    match client.update_program(WAIT) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::UpdateFailed);
            assert!(detail.contains("not signed"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    // A service that does not update itself says so.
    let (listener, mut connector, _) = make();
    let _plain = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    match connect(&mut *connector).update_program(WAIT) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::UpdateFailed);
            assert!(detail.contains("by hand"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
}

/// A task as a guest's agent would describe it.
pub fn sample_task(task_id: i32, label: &str) -> Task {
    Task {
        task_id,
        bounds: Bounds {
            left: 0,
            top: 0,
            right: 1080,
            bottom: 1920,
        },
        package_name: "com.example.app".into(),
        label: label.into(),
        focused: false,
    }
}

fn next_event(stream: &mut crate::host_api::TaskStream) -> TaskEvent {
    stream
        .next(WAIT)
        .expect("the watch goes on")
        .expect("a task event in time")
}

pub fn watcher_receives_task_changes(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut stream = connect(&mut *connector).watch_tasks().expect("watching");
    assert!(stream.current().tasks.is_empty());
    let android = androids
        .of(&user)
        .expect("the watch asked for the caller's Android");

    android.publish(TaskEvent::Updated(sample_task(4, "Clock")));
    android.publish(TaskEvent::Frames {
        task_id: 4,
        display_id: 2,
    });
    android.publish(TaskEvent::Removed { task_id: 4 });

    assert_eq!(
        next_event(&mut stream),
        TaskEvent::Updated(sample_task(4, "Clock"))
    );
    assert_eq!(
        next_event(&mut stream),
        TaskEvent::Frames {
            task_id: 4,
            display_id: 2
        }
    );
    assert_eq!(next_event(&mut stream), TaskEvent::Removed { task_id: 4 });
    assert_eq!(
        stream
            .next(Duration::from_millis(50))
            .expect("still watching"),
        None
    );
}

pub fn late_watcher_starts_from_current_tasks(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    client.status().unwrap();
    let android = androids.of(&user).unwrap();
    android.publish(TaskEvent::Updated(sample_task(1, "Clock")));
    android.publish(TaskEvent::Updated(sample_task(2, "Maps")));
    android.publish(TaskEvent::Frames {
        task_id: 2,
        display_id: 7,
    });

    let list = client.tasks().expect("the list of tasks");
    assert_eq!(
        list.tasks,
        vec![sample_task(1, "Clock"), sample_task(2, "Maps")]
    );
    assert_eq!(list.frames, vec![(2, 7)]);

    let stream = client.watch_tasks().expect("watching");
    assert_eq!(stream.current(), &list);
}

pub fn close_and_launch_reach_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    client.start().unwrap();
    client.launch_app("com.example.clock").expect("the launch");
    client.close_task(9).expect("the close");
    assert_eq!(
        *androids.of(&user).unwrap().requests.lock().unwrap(),
        vec![
            TaskRequest::Launch {
                package_name: "com.example.clock".into()
            },
            TaskRequest::Close { task_id: 9 },
        ]
    );
}

pub fn request_to_stopped_android_says_so(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut client = connect(&mut *connector);
    match client.launch_app("com.example.clock") {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::NotRunning);
            assert!(detail.contains("not running"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    // The connection goes on serving.
    assert_eq!(client.status().unwrap().state, SessionState::Stopped);
}

pub fn watcher_that_sends_is_disconnected(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let watch = dsa_protocol::host_api::types::Request {
        id: 1,
        body: Some(dsa_protocol::host_api::types::request::Body::WatchTasks(
            dsa_protocol::host_api::types::WatchTasks {},
        )),
    };
    dsa_protocol::host_api::send(
        &mut *connection,
        dsa_protocol::host_api::types::envelope::Body::Request(watch.clone()),
    )
    .unwrap();
    connection
        .recv(Some(WAIT))
        .expect("the answer to the watch");
    dsa_protocol::host_api::send(
        &mut *connection,
        dsa_protocol::host_api::types::envelope::Body::Request(watch),
    )
    .unwrap();
    assert!(
        connection.recv(Some(WAIT)).is_err(),
        "the service closes a watching client that sends a request"
    );
    // Other clients are served.
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Stopped
    );
}
