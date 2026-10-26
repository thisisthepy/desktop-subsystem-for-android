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
    ServiceFacts, UpdateProgram, UserAndroid, UserAndroids,
};
use crate::ime::{ImeCursor, ImeEvent};
use crate::input::{buttons, PointerAction, PointerEvent, PointerTool};
use crate::keys::{keycode, meta, KeyAction, KeyEvent};
use crate::session::SessionState;
use crate::settings::{MemorySettings, Settings, SettingsStore};
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
    /// The pointer events it was given, in order.
    pub pointers: Mutex<Vec<PointerEvent>>,
    /// The key events it was given, in order.
    pub keys: Mutex<Vec<KeyEvent>>,
    /// The text input it was given, in order.
    pub text: Mutex<Vec<ImeEvent>>,
}

impl FakeAndroid {
    pub fn new() -> FakeAndroid {
        FakeAndroid {
            state: Mutex::new(SessionState::Stopped),
            tasks: Mutex::new((TaskTable::new(), Vec::new())),
            requests: Mutex::new(Vec::new()),
            pointers: Mutex::new(Vec::new()),
            keys: Mutex::new(Vec::new()),
            text: Mutex::new(Vec::new()),
        }
    }

    /// Ends every watch of its tasks, as a service that goes away does.
    pub fn end_watches(&self) {
        self.tasks.lock().unwrap().1.clear();
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
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError> {
        if self.state() != SessionState::Active {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.pointers.lock().unwrap().push(*event);
        Ok(())
    }
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError> {
        if self.state() != SessionState::Active {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.keys.lock().unwrap().push(*event);
        Ok(())
    }
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError> {
        if self.state() != SessionState::Active {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.text.lock().unwrap().push(event.clone());
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

    /// Serves `service`, configured as the test needs.
    pub fn start_service(listener: Box<dyn LocalListener>, service: HostApiService) -> Self {
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
        fn fr4_resizing_and_rescaling_a_task_reach_the_callers_android() {
            $crate::host_api_conformance::resize_and_density_reach_the_caller(&mut || $make);
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
        fn fr5_pointer_input_reaches_the_callers_android_in_order() {
            $crate::host_api_conformance::pointer_input_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr5_pointer_input_to_an_android_that_is_not_running_is_dropped() {
            $crate::host_api_conformance::pointer_input_to_stopped_android_is_dropped(&mut || {
                $make
            });
        }
        #[test]
        fn fr16_key_input_reaches_the_callers_android_in_order_with_pointer_input() {
            $crate::host_api_conformance::key_input_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr16_a_key_event_that_makes_no_sense_is_dropped_and_the_stream_goes_on() {
            $crate::host_api_conformance::bad_key_event_is_dropped(&mut || $make);
        }
        #[test]
        fn fr6_text_input_reaches_the_callers_android_in_order_with_key_input() {
            $crate::host_api_conformance::text_input_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr6_text_input_that_makes_no_sense_is_dropped_and_the_stream_goes_on() {
            $crate::host_api_conformance::bad_text_input_is_dropped(&mut || $make);
        }
        #[test]
        fn fr6_a_watcher_hears_where_the_cursor_of_a_tasks_editor_is() {
            $crate::host_api_conformance::watcher_receives_cursor(&mut || $make);
        }
        #[test]
        fn nfr5_a_client_that_streams_input_and_sends_a_request_is_disconnected() {
            $crate::host_api_conformance::input_stream_that_sends_a_request_is_disconnected(
                &mut || $make,
            );
        }
        #[test]
        fn fr17_settings_are_read_and_changed_through_the_service() {
            $crate::host_api_conformance::settings_round_trip(&mut || $make);
        }
        #[test]
        fn fr17_an_invalid_setting_is_refused_with_its_reason() {
            $crate::host_api_conformance::invalid_setting_is_refused(&mut || $make);
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
        density_dpi: 320,
        max_width: 3840,
        max_height: 2160,
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

pub fn resize_and_density_reach_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    client.start().unwrap();
    client.set_task_density(9, 320).expect("the density");
    // A step of a drag, and the resize that ends it.
    client
        .resize_task(9, 1280, 720, true)
        .expect("the live resize");
    client.resize_task(9, 1280, 720, false).expect("the resize");
    assert_eq!(
        *androids.of(&user).unwrap().requests.lock().unwrap(),
        vec![
            TaskRequest::SetDensity {
                task_id: 9,
                density_dpi: 320
            },
            TaskRequest::Resize {
                task_id: 9,
                width: 1280,
                height: 720,
                live: true
            },
            TaskRequest::Resize {
                task_id: 9,
                width: 1280,
                height: 720,
                live: false
            },
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

fn pointer(task_id: i32, action: PointerAction, x: f32) -> PointerEvent {
    let mut event = PointerEvent::new(task_id, action, PointerTool::Mouse, (x, 20.0));
    if action == PointerAction::Down || action == PointerAction::Move {
        event.buttons = buttons::PRIMARY;
        event.pressure = 1.0;
    }
    event
}

/// Waits until `android` has been given `count` pointer events, and returns them.
fn pointers_given(android: &FakeAndroid, count: usize) -> Vec<PointerEvent> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let given = android.pointers.lock().unwrap().clone();
        if given.len() >= count || std::time::Instant::now() >= deadline {
            return given;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub fn pointer_input_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut input = connect(&mut *connector)
        .stream_input()
        .expect("streaming input");
    let sent: Vec<PointerEvent> = (0..50)
        .map(|step| {
            let action = match step {
                0 => PointerAction::Down,
                49 => PointerAction::Up,
                _ => PointerAction::Move,
            };
            pointer(4, action, step as f32)
        })
        .collect();
    for event in &sent {
        input.send(event).expect("the event is sent");
    }
    let android = androids.of(&user).unwrap();
    assert_eq!(pointers_given(&android, sent.len()), sent);
}

pub fn pointer_input_to_stopped_android_is_dropped(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut input = connect(&mut *connector)
        .stream_input()
        .expect("streaming input");
    input
        .send(&pointer(4, PointerAction::Down, 1.0))
        .expect("the event is sent");
    // Once Android runs, the same stream carries input to it.
    connect(&mut *connector).start().unwrap();
    let up = pointer(4, PointerAction::Up, 2.0);
    input.send(&up).expect("the event is sent");
    let android = androids.of(&user).unwrap();
    assert_eq!(pointers_given(&android, 1), vec![up]);
}

fn key(task_id: i32, action: KeyAction, key_code: u32, meta_state: u32) -> KeyEvent {
    KeyEvent {
        task_id,
        action,
        key_code,
        repeat_count: 0,
        meta_state,
        event_time_ms: 100,
        canceled: false,
    }
}

/// Waits until `android` has been given `count` key events, and returns them.
fn keys_given(android: &FakeAndroid, count: usize) -> Vec<KeyEvent> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let given = android.keys.lock().unwrap().clone();
        if given.len() >= count || std::time::Instant::now() >= deadline {
            return given;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub fn key_input_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut input = connect(&mut *connector)
        .stream_input()
        .expect("streaming input");
    let ctrl = meta::CTRL_ON | meta::CTRL_LEFT_ON;
    let keys = [
        key(4, KeyAction::Down, keycode::CTRL_LEFT, ctrl),
        key(4, KeyAction::Down, keycode::A, ctrl),
        key(4, KeyAction::Up, keycode::A, ctrl),
        key(4, KeyAction::Up, keycode::CTRL_LEFT, 0),
    ];
    // Both kinds share the one stream.
    input
        .send(&pointer(4, PointerAction::Down, 1.0))
        .expect("the event is sent");
    for event in &keys {
        input.send_key(event).expect("the event is sent");
    }
    input
        .send_input(&crate::input::InputEvent::Pointer(pointer(
            4,
            PointerAction::Up,
            1.0,
        )))
        .expect("the event is sent");
    let android = androids.of(&user).unwrap();
    assert_eq!(keys_given(&android, keys.len()), keys.to_vec());
    assert_eq!(pointers_given(&android, 2).len(), 2);
}

pub fn bad_key_event_is_dropped(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut input = connect(&mut *connector)
        .stream_input()
        .expect("streaming input");
    input
        .send_key(&key(4, KeyAction::Down, 0, 0))
        .expect("the event is sent");
    let good = key(4, KeyAction::Down, keycode::ENTER, 0);
    input.send_key(&good).expect("the event is sent");
    let android = androids.of(&user).unwrap();
    assert_eq!(keys_given(&android, 1), vec![good]);
}

/// Waits until `android` has been given `count` pieces of text input, and returns them.
fn text_given(android: &FakeAndroid, count: usize) -> Vec<ImeEvent> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let given = android.text.lock().unwrap().clone();
        if given.len() >= count || std::time::Instant::now() >= deadline {
            return given;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

pub fn text_input_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut input = connect(&mut *connector)
        .stream_input()
        .expect("streaming input");
    let text = [
        ImeEvent::Composition {
            task_id: 4,
            text: "\u{3131}".into(),
            cursor: 1,
        },
        ImeEvent::Composition {
            task_id: 4,
            text: "\u{ac00}".into(),
            cursor: 1,
        },
        ImeEvent::Commit {
            task_id: 4,
            text: "\u{ac00}".into(),
        },
    ];
    for event in &text {
        input.send_ime(event).expect("the event is sent");
    }
    input
        .send_input(&crate::input::InputEvent::Key(key(
            4,
            KeyAction::Down,
            keycode::DPAD_LEFT,
            0,
        )))
        .expect("the event is sent");
    input
        .send_input(&crate::input::InputEvent::Ime(ImeEvent::Commit {
            task_id: 4,
            text: "a".into(),
        }))
        .expect("the event is sent");
    let android = androids.of(&user).unwrap();
    let given = text_given(&android, 4);
    assert_eq!(given[..3], text);
    assert_eq!(keys_given(&android, 1).len(), 1);
}

pub fn bad_text_input_is_dropped(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut input = connect(&mut *connector)
        .stream_input()
        .expect("streaming input");
    // A cursor past the end of its composition, and a commit of nothing.
    input
        .send_ime(&ImeEvent::Composition {
            task_id: 4,
            text: "a".into(),
            cursor: 5,
        })
        .expect("the event is sent");
    input
        .send_ime(&ImeEvent::Commit {
            task_id: 4,
            text: String::new(),
        })
        .expect("the event is sent");
    let good = ImeEvent::Commit {
        task_id: 4,
        text: "\u{d55c}".into(),
    };
    input.send_ime(&good).expect("the event is sent");
    let android = androids.of(&user).unwrap();
    assert_eq!(text_given(&android, 1), vec![good]);
}

pub fn watcher_receives_cursor(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut stream = connect(&mut *connector).watch_tasks().expect("watching");
    let android = androids.of(&user).unwrap();
    android.publish(TaskEvent::Updated(sample_task(4, "Notes")));
    let cursor = TaskEvent::ImeCursor {
        task_id: 4,
        cursor: ImeCursor {
            left: 30,
            top: 40,
            right: 32,
            bottom: 70,
        },
    };
    android.publish(cursor.clone());
    assert_eq!(
        next_event(&mut stream),
        TaskEvent::Updated(sample_task(4, "Notes"))
    );
    assert_eq!(next_event(&mut stream), cursor);
}

pub fn input_stream_that_sends_a_request_is_disconnected(make: MakeLocal<'_>) {
    use dsa_protocol::host_api::types::{envelope::Body, request, Request, StreamInput};
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let stream = Request {
        id: 1,
        body: Some(request::Body::StreamInput(StreamInput {})),
    };
    dsa_protocol::host_api::send(&mut *connection, Body::Request(stream.clone())).unwrap();
    connection
        .recv(Some(WAIT))
        .expect("the answer to the request to stream input");
    dsa_protocol::host_api::send(&mut *connection, Body::Request(stream)).unwrap();
    assert!(
        connection.recv(Some(WAIT)).is_err(),
        "the service closes a client that sends a request on a stream of input"
    );
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Stopped
    );
}

fn facts() -> ServiceFacts {
    ServiceFacts {
        version: "0.0.9".into(),
        log_dir: Some("C:\\ProgramData\\DSA\\logs".into()),
        program_updates_available: true,
        image_updates_available: false,
    }
}

pub fn settings_round_trip(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let store = Arc::new(MemorySettings::default());
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(store.clone(), facts());
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    let report = client.settings().expect("the settings");
    assert_eq!(report.settings, Settings::default());
    assert!(!report.settings.freeze, "freeze is off by default");
    assert_eq!(report.facts, facts());

    // The caller's own settings: any user may change them.
    let changed = client
        .set_settings(&Settings {
            render_gpu: "Discrete".into(),
            freeze: true,
            ..Settings::default()
        })
        .expect("changing the settings");
    assert_eq!(changed.settings.render_gpu, "discrete");
    assert!(changed.settings.freeze);
    assert_eq!(
        changed.facts.version, "0.0.9",
        "a client cannot change the facts"
    );
    assert_eq!(store.get(&user).unwrap().render_gpu, "discrete");
    assert!(store.machine().program_updates);
    assert_eq!(client.settings().unwrap().settings, changed.settings);
}

pub fn invalid_setting_is_refused(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(Arc::new(MemorySettings::default()), facts());
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    match client.set_settings(&Settings {
        render_gpu: "  ".into(),
        ..Settings::default()
    }) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::InvalidSetting);
            assert!(detail.contains("render GPU"), "{detail}");
        }
        other => panic!("expected the service to refuse, got {other:?}"),
    }
    assert_eq!(client.settings().unwrap().settings, Settings::default());
}
