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
use std::time::{Duration, Instant};

use dsa_protocol::host_api::types::{self, envelope, ErrorCode, Role};
use dsa_protocol::host_api::{client_connect, decode, local_hello, send, HandshakeError};
use dsa_transport::local::LocalListener;
use dsa_transport::Connector;

use crate::accessibility::{
    AccessibilityWatch, ActionRequest, ActionResult, GuestAccessibility, Node, NodeAction,
    TreeUpdate,
};
use crate::apk::{self, InstallOutcome, InstallStatus, InstalledPackage};
use crate::files::{FileAction, FileTransfer, MAX_FILE_BYTES};
use crate::host_api::{
    running_of, stop_every, ClientError, HostApiClient, HostApiService, InstallError,
    ProgramUpdate, ServiceFacts, UpdateProgram, UserAndroid, UserAndroids,
};
use crate::ime::{ImeCursor, ImeEvent};
use crate::input::{buttons, PointerAction, PointerEvent, PointerTool};
use crate::keys::{keycode, meta, KeyAction, KeyEvent};
use crate::notifications::{Notification, NotificationAction, NotificationEvent};
use crate::session::SessionState;
use crate::settings::{MemorySettings, Settings, SettingsStore};
use crate::tasks::{Bounds, Task, TaskEvent, TaskRequest, TaskRequestError, TaskTable, TaskWatch};
use crate::widgets::{
    Placement, Widget, WidgetError, WidgetEvent, WidgetFrame, WidgetProvider, WidgetSize,
};

/// A fresh listener, a connector that reaches it, and the user it reports for this process.
pub type LocalPair = (Box<dyn LocalListener>, Box<dyn Connector>, String);

/// Makes a [`LocalPair`].
pub type MakeLocal<'a> = &'a mut dyn FnMut() -> LocalPair;

const WAIT: Duration = Duration::from_secs(10);

/// The widgets placed in a [`FakeAndroid`], by ID, and who watches them.
type FakeWidgets = (
    std::collections::BTreeMap<i32, WidgetProvider>,
    Vec<Sender<WidgetEvent>>,
);

/// An Android that only remembers what it was asked.
pub struct FakeAndroid {
    state: Mutex<SessionState>,
    tasks: Mutex<(TaskTable, Vec<Sender<TaskEvent>>)>,
    /// The requests about tasks it was asked to carry out, in order.
    pub requests: Mutex<Vec<TaskRequest>>,
    /// The pointer events it was given, in order.
    pub pointers: Mutex<Vec<PointerEvent>>,
    /// How many pointer events it refused because it was not running.
    pub pointers_refused: Mutex<usize>,
    /// The key events it was given, in order.
    pub keys: Mutex<Vec<KeyEvent>>,
    /// The text input it was given, in order.
    pub text: Mutex<Vec<ImeEvent>>,
    /// Each word on whether any task window is shown, in order. Taken in every state, as a
    /// session takes it.
    pub shown: Mutex<Vec<bool>>,
    /// Each word on whether it may be frozen when idle, in order.
    pub freeze_when_idle: Mutex<Vec<bool>>,
    /// Each word on whether web links open in it, in order.
    pub links_in_android: Mutex<Vec<bool>>,
    /// Each word on developer mode (on, and the adb port), in order.
    pub developer_mode: Mutex<Vec<(bool, u16)>>,
    /// Set to refuse developer mode, as when the port is taken.
    pub refuse_developer_mode: Mutex<Option<String>>,
    link_watchers: Mutex<Vec<Sender<String>>>,
    /// The files it was given, in order.
    pub files: Mutex<Vec<FileTransfer>>,
    /// Each word on whether the clipboard is shared, in order.
    pub clipboard_shared: Mutex<Vec<bool>>,
    /// The text copied on the host it was given, in order.
    pub host_clipboard: Mutex<Vec<String>>,
    clipboard_watchers: Mutex<Vec<Sender<String>>>,
    /// What the user did with its notifications on the host, in order.
    pub notification_actions: Mutex<Vec<NotificationAction>>,
    notification_watchers: Mutex<Vec<Sender<NotificationEvent>>>,
    /// The APKs it installed or refused, by name, with their whole content, in order.
    pub installs: Mutex<Vec<(String, Vec<u8>)>>,
    /// When each install began and ended, by name, in order: `+name` and `-name`.
    pub install_steps: Mutex<Vec<String>>,
    /// How long each install takes once its content is in.
    pub install_time: Mutex<Duration>,
    install_turn: Mutex<()>,
    /// The widgets its apps offer.
    pub widget_offer: Mutex<Vec<WidgetProvider>>,
    /// The widgets placed in it, by ID, and who watches them.
    widgets: Mutex<FakeWidgets>,
    /// The sizes its widgets were given, in order.
    pub widget_sizes: Mutex<Vec<(i32, WidgetSize)>>,
    /// The pointer input its widgets were given, in order.
    pub widget_pointers: Mutex<Vec<dsa_protocol::control::WidgetPointer>>,
    /// Each start (`true`) and end (`false`) of a watch of a task's accessibility tree, in
    /// order.
    pub accessibility_watches: Arc<Mutex<Vec<(i32, bool)>>>,
    accessibility_watchers: Mutex<Vec<(i32, Sender<GuestAccessibility>)>>,
    /// The accessibility actions it was asked for, in order.
    pub accessibility_actions: Mutex<Vec<(i32, ActionRequest)>>,
}

/// What the fake Android says of an APK: one whose content starts with [`BAD_APK`] is not an APK
/// it can read; any other is installed as [`INSTALLED_PACKAGE`].
pub const BAD_APK: &[u8] = b"not a zip";

/// The package the fake Android has installed already, at [`INSTALLED_VERSION`], signed by
/// the certificate whose SHA-256 digest is [`INSTALLED_SIGNER`].
pub const KNOWN_PACKAGE: &str = "com.example.notes";
/// The version of [`KNOWN_PACKAGE`] the fake Android has installed.
pub const INSTALLED_VERSION: i64 = 41;
/// The certificate [`KNOWN_PACKAGE`] is signed with in the fake Android: the one the APK
/// of `apk_info::fixture::notes_apk` is signed with.
pub const INSTALLED_SIGNER: &[u8] = b"\x30\x03\x02\x01\x07";

/// The package every APK the fake Android installs turns out to be.
pub const INSTALLED_PACKAGE: &str = "com.example.installed";

impl FakeAndroid {
    /// Sends what an agent says about a task's accessibility to the task's watchers.
    pub fn publish_accessibility(&self, task_id: i32, message: GuestAccessibility) {
        self.accessibility_watchers
            .lock()
            .unwrap()
            .retain(|(task, watcher)| *task != task_id || watcher.send(message.clone()).is_ok());
    }

    pub fn new() -> FakeAndroid {
        FakeAndroid {
            state: Mutex::new(SessionState::Stopped),
            tasks: Mutex::new((TaskTable::new(), Vec::new())),
            requests: Mutex::new(Vec::new()),
            pointers: Mutex::new(Vec::new()),
            pointers_refused: Mutex::new(0),
            keys: Mutex::new(Vec::new()),
            text: Mutex::new(Vec::new()),
            shown: Mutex::new(Vec::new()),
            freeze_when_idle: Mutex::new(Vec::new()),
            links_in_android: Mutex::new(Vec::new()),
            developer_mode: Mutex::new(Vec::new()),
            refuse_developer_mode: Mutex::new(None),
            link_watchers: Mutex::new(Vec::new()),
            files: Mutex::new(Vec::new()),
            clipboard_shared: Mutex::new(Vec::new()),
            host_clipboard: Mutex::new(Vec::new()),
            clipboard_watchers: Mutex::new(Vec::new()),
            notification_actions: Mutex::new(Vec::new()),
            notification_watchers: Mutex::new(Vec::new()),
            installs: Mutex::new(Vec::new()),
            install_steps: Mutex::new(Vec::new()),
            install_time: Mutex::new(Duration::ZERO),
            install_turn: Mutex::new(()),
            widget_offer: Mutex::new(Vec::new()),
            widgets: Mutex::new((std::collections::BTreeMap::new(), Vec::new())),
            widget_sizes: Mutex::new(Vec::new()),
            widget_pointers: Mutex::new(Vec::new()),
            accessibility_watches: Arc::new(Mutex::new(Vec::new())),
            accessibility_watchers: Mutex::new(Vec::new()),
            accessibility_actions: Mutex::new(Vec::new()),
        }
    }

    /// Changes its widgets as its agent would, and tells the watchers.
    pub fn widget_event(&self, event: WidgetEvent) {
        let mut widgets = self.widgets.lock().unwrap();
        match &event {
            WidgetEvent::Placed(widget) => {
                widgets.0.insert(widget.widget_id, widget.provider.clone());
            }
            WidgetEvent::Removed { widget_id } => {
                widgets.0.remove(widget_id);
            }
            WidgetEvent::Frame(_) | WidgetEvent::Listed(_) => {}
        }
        widgets
            .1
            .retain(|watcher| watcher.send(event.clone()).is_ok());
    }

    fn running_or(&self) -> Result<(), String> {
        if matches!(
            self.state(),
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            Ok(())
        } else {
            Err(format!("Android is {}, not running", self.state()))
        }
    }

    /// Changes its notifications as an app in this Android would, and tells the watchers.
    pub fn notify(&self, event: NotificationEvent) {
        self.notification_watchers
            .lock()
            .unwrap()
            .retain(|watcher| watcher.send(event.clone()).is_ok());
    }

    /// Opens `url` as an app in this Android would, and tells the link watchers.
    pub fn open_link_in_android(&self, url: &str) {
        self.link_watchers
            .lock()
            .unwrap()
            .retain(|watcher| watcher.send(url.to_string()).is_ok());
    }

    /// Copies `text` as an app in this Android would, and tells the clipboard watchers.
    pub fn copy_in_android(&self, text: &str) {
        self.clipboard_watchers
            .lock()
            .unwrap()
            .retain(|watcher| watcher.send(text.to_string()).is_ok());
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
    fn freeze_when_idle(&self, on: bool) {
        self.freeze_when_idle.lock().unwrap().push(on);
    }
    fn links_in_android(&self, on: bool) {
        self.links_in_android.lock().unwrap().push(on);
    }
    fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        if let Some(why) = self.refuse_developer_mode.lock().unwrap().clone() {
            return Err(why);
        }
        self.developer_mode.lock().unwrap().push((on, port));
        Ok(())
    }
    fn watch_links(&self) -> Option<mpsc::Receiver<String>> {
        let (tx, rx) = mpsc::channel();
        self.link_watchers.lock().unwrap().push(tx);
        Some(rx)
    }
    fn open_file(&self, file: &FileTransfer) -> Result<(), TaskRequestError> {
        if self.state() != SessionState::Active {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.files.lock().unwrap().push(file.clone());
        Ok(())
    }
    fn watch_accessibility(&self, task_id: i32) -> Option<AccessibilityWatch> {
        let (tx, rx) = mpsc::channel();
        self.accessibility_watchers
            .lock()
            .unwrap()
            .push((task_id, tx));
        self.accessibility_watches
            .lock()
            .unwrap()
            .push((task_id, true));
        let watches = Arc::clone(&self.accessibility_watches);
        Some(AccessibilityWatch::new(rx, move || {
            watches.lock().unwrap().push((task_id, false));
        }))
    }
    fn accessibility_action(
        &self,
        task_id: i32,
        action: &ActionRequest,
    ) -> Result<(), TaskRequestError> {
        if self.state() != SessionState::Active {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.accessibility_actions
            .lock()
            .unwrap()
            .push((task_id, action.clone()));
        Ok(())
    }
    fn share_clipboard(&self, on: bool) {
        self.clipboard_shared.lock().unwrap().push(on);
    }
    fn host_clipboard(&self, text: String) {
        self.host_clipboard.lock().unwrap().push(text);
    }
    fn watch_clipboard(&self) -> Option<mpsc::Receiver<String>> {
        let (tx, rx) = mpsc::channel();
        self.clipboard_watchers.lock().unwrap().push(tx);
        Some(rx)
    }
    fn watch_notifications(&self) -> Option<mpsc::Receiver<NotificationEvent>> {
        let (tx, rx) = mpsc::channel();
        self.notification_watchers.lock().unwrap().push(tx);
        Some(rx)
    }
    fn notification_action(&self, action: NotificationAction) -> Result<(), TaskRequestError> {
        if !matches!(
            self.state(),
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            return Err(TaskRequestError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.notification_actions.lock().unwrap().push(action);
        Ok(())
    }
    fn watch_widgets(&self) -> Option<crate::session::WidgetWatch> {
        let (tx, events) = mpsc::channel();
        let mut widgets = self.widgets.lock().unwrap();
        let current = widgets
            .0
            .iter()
            .map(|(id, provider)| {
                WidgetEvent::Placed(Widget {
                    widget_id: *id,
                    provider: provider.clone(),
                    placement: Placement::default(),
                })
            })
            .collect();
        widgets.1.push(tx);
        Some(crate::session::WidgetWatch { current, events })
    }
    fn widgets(&self) -> Vec<(i32, WidgetProvider)> {
        self.widgets
            .lock()
            .unwrap()
            .0
            .iter()
            .map(|(id, provider)| (*id, provider.clone()))
            .collect()
    }
    fn widget_providers(&self) -> Result<Vec<WidgetProvider>, WidgetError> {
        self.running_or().map_err(WidgetError::NotRunning)?;
        Ok(self.widget_offer.lock().unwrap().clone())
    }
    fn add_widget(&self, provider: &str) -> Result<i32, WidgetError> {
        self.running_or().map_err(WidgetError::NotRunning)?;
        let offered = self
            .widget_offer
            .lock()
            .unwrap()
            .iter()
            .find(|offered| offered.provider == provider)
            .cloned()
            .ok_or_else(|| WidgetError::Refused(format!("no app offers {provider}")))?;
        let widget_id = self
            .widgets
            .lock()
            .unwrap()
            .0
            .keys()
            .last()
            .map_or(1, |last| last + 1);
        self.widget_event(WidgetEvent::Placed(Widget {
            widget_id,
            provider: offered,
            placement: Placement::default(),
        }));
        Ok(widget_id)
    }
    fn remove_widget(&self, widget_id: i32) -> Result<(), TaskRequestError> {
        self.running_or().map_err(TaskRequestError::NotRunning)?;
        self.widget_event(WidgetEvent::Removed { widget_id });
        Ok(())
    }
    fn resize_widget(&self, widget_id: i32, size: WidgetSize) -> Result<(), TaskRequestError> {
        self.running_or().map_err(TaskRequestError::NotRunning)?;
        self.widget_sizes.lock().unwrap().push((widget_id, size));
        Ok(())
    }
    fn widget_pointer(
        &self,
        pointer: dsa_protocol::control::WidgetPointer,
    ) -> Result<(), TaskRequestError> {
        self.running_or().map_err(TaskRequestError::NotRunning)?;
        self.widget_pointers.lock().unwrap().push(pointer);
        Ok(())
    }
    fn install_apk(
        &self,
        name: &str,
        size: u64,
        content: &mut dyn std::io::Read,
    ) -> Result<InstallOutcome, InstallError> {
        // One at a time, as a session installs.
        let _turn = self.install_turn.lock().unwrap();
        if !matches!(
            self.state(),
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            return Err(InstallError::NotRunning(format!(
                "Android is {}, not running",
                self.state()
            )));
        }
        self.install_steps.lock().unwrap().push(format!("+{name}"));
        let mut apk = vec![0u8; size as usize];
        let read = content.read_exact(&mut apk);
        thread::sleep(*self.install_time.lock().unwrap());
        self.install_steps.lock().unwrap().push(format!("-{name}"));
        read.map_err(|e| InstallError::Refused(format!("the APK did not arrive whole: {e}")))?;
        self.installs
            .lock()
            .unwrap()
            .push((name.to_string(), apk.clone()));
        Ok(if apk.starts_with(BAD_APK) {
            InstallOutcome {
                status: InstallStatus::InvalidApk,
                package_name: None,
                message: "INSTALL_PARSE_FAILED_NOT_APK: not a zip".into(),
            }
        } else {
            InstallOutcome {
                status: InstallStatus::Installed,
                package_name: Some(INSTALLED_PACKAGE.into()),
                message: String::new(),
            }
        })
    }
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        self.running_or().map_err(TaskRequestError::NotRunning)?;
        use sha2::Digest;
        Ok((package == KNOWN_PACKAGE).then(|| InstalledPackage {
            version_code: INSTALLED_VERSION,
            version_name: "4.1".into(),
            signer_sha256: vec![sha2::Sha256::digest(INSTALLED_SIGNER).to_vec()],
        }))
    }
    fn freeze(&self) -> Result<(), TaskRequestError> {
        let mut state = self.state.lock().unwrap();
        match *state {
            SessionState::Active | SessionState::Dozing | SessionState::Frozen => {
                *state = SessionState::Frozen;
                Ok(())
            }
            other => Err(TaskRequestError::NotRunning(format!(
                "Android is {other}, not running"
            ))),
        }
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        if let TaskRequest::WindowsShown { shown } = request {
            self.shown.lock().unwrap().push(shown);
            return Ok(());
        }
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
            *self.pointers_refused.lock().unwrap() += 1;
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
        fn fr3_a_watch_made_while_android_is_stopped_receives_the_tasks_after_it_starts() {
            $crate::host_api_conformance::watch_made_while_stopped_receives_tasks_after_start(
                &mut || $make,
            );
        }
        #[test]
        fn fr3_waiting_for_a_task_change_returns_within_its_timeout() {
            $crate::host_api_conformance::waiting_for_a_change_returns_in_time(&mut || $make);
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
        fn fr10_the_word_on_shown_windows_reaches_the_callers_android_in_any_state() {
            $crate::host_api_conformance::windows_shown_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr10_freezing_reaches_the_callers_android_only_while_it_runs() {
            $crate::host_api_conformance::freeze_reaches_the_caller(&mut || $make);
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
        fn fr13_text_copied_on_the_host_reaches_the_callers_android() {
            $crate::host_api_conformance::host_clipboard_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr13_text_copied_in_the_callers_android_reaches_the_client() {
            $crate::host_api_conformance::guest_clipboard_reaches_the_client(&mut || $make);
        }
        #[test]
        fn fr13_turning_clipboard_sharing_off_and_on_reaches_the_callers_android() {
            $crate::host_api_conformance::clipboard_setting_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn nfr5_a_clipboard_client_that_sends_a_request_is_disconnected() {
            $crate::host_api_conformance::clipboard_client_that_sends_a_request_is_disconnected(
                &mut || $make,
            );
        }
        #[test]
        fn fr11_an_apk_a_client_sends_is_installed_in_the_callers_android() {
            $crate::host_api_conformance::apk_is_installed(&mut || $make);
        }
        #[test]
        fn fr11_an_apk_android_refuses_is_answered_with_the_reason() {
            $crate::host_api_conformance::refused_apk_has_its_reason(&mut || $make);
        }
        #[test]
        fn fr11_a_file_that_is_no_apk_or_too_large_is_refused_before_it_is_sent() {
            $crate::host_api_conformance::wrong_file_is_refused_before_it_is_sent(&mut || $make);
        }
        #[test]
        fn fr11_an_install_to_an_android_that_is_not_running_says_so() {
            $crate::host_api_conformance::install_needs_a_running_android(&mut || $make);
        }
        #[test]
        fn fr11_a_second_install_waits_for_the_first() {
            $crate::host_api_conformance::installs_run_one_at_a_time(&mut || $make);
        }
        #[test]
        fn nfr5_a_client_that_sends_something_else_during_an_install_is_refused() {
            $crate::host_api_conformance::malformed_install_is_refused(&mut || $make);
        }
        #[test]
        fn fr9_a_link_opened_in_the_callers_android_reaches_the_client() {
            $crate::host_api_conformance::link_reaches_the_client(&mut || $make);
        }
        #[test]
        fn fr9_the_link_setting_reaches_the_callers_android() {
            $crate::host_api_conformance::link_setting_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr20_developer_mode_is_off_by_default_and_reaches_the_callers_android() {
            $crate::host_api_conformance::developer_mode_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr20_a_developer_mode_port_that_cannot_open_is_reported() {
            $crate::host_api_conformance::developer_mode_port_refusal_is_reported(&mut || $make);
        }
        #[test]
        fn nfr5_a_link_client_that_sends_a_frame_is_disconnected() {
            $crate::host_api_conformance::link_client_that_sends_is_disconnected(&mut || $make);
        }
        #[test]
        fn fr18_a_watched_tree_and_action_results_reach_the_client() {
            $crate::host_api_conformance::accessibility_reaches_the_client(&mut || $make);
        }
        #[test]
        fn fr18_actions_reach_the_callers_android_and_closing_ends_the_watch() {
            $crate::host_api_conformance::accessibility_actions_reach_the_caller(&mut || $make);
        }
        #[test]
        fn nfr5_an_accessibility_client_that_sends_something_else_is_disconnected() {
            $crate::host_api_conformance::accessibility_client_that_sends_a_request_is_disconnected(
                &mut || $make,
            );
        }
        #[test]
        fn fr9_a_host_file_reaches_the_callers_android_whole() {
            $crate::host_api_conformance::file_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr9_a_file_larger_than_the_limit_is_refused() {
            $crate::host_api_conformance::oversized_file_is_refused(&mut || $make);
        }
        #[test]
        fn fr9_a_file_for_an_android_that_is_not_running_is_refused() {
            $crate::host_api_conformance::file_for_stopped_android_is_refused(&mut || $make);
        }
        #[test]
        fn nfr5_a_file_client_that_sends_something_else_is_refused() {
            $crate::host_api_conformance::malformed_file_is_refused(&mut || $make);
        }
        #[test]
        fn fr8_notifications_of_the_callers_android_reach_a_watcher() {
            $crate::host_api_conformance::notifications_reach_a_watcher(&mut || $make);
        }
        #[test]
        fn fr8_opening_and_dismissing_a_notification_reach_the_callers_android() {
            $crate::host_api_conformance::notification_actions_reach_the_caller(&mut || $make);
        }
        #[test]
        fn fr8_acting_on_a_notification_of_a_stopped_android_says_it_is_not_running() {
            $crate::host_api_conformance::notification_action_needs_a_running_android(&mut || {
                $make
            });
        }
        #[test]
        fn nfr5_a_malformed_action_on_a_notification_is_refused() {
            $crate::host_api_conformance::malformed_notification_action_is_refused(&mut || $make);
        }
        #[test]
        fn nfr5_a_notification_watcher_that_sends_something_is_disconnected() {
            $crate::host_api_conformance::notification_watcher_that_sends_is_disconnected(
                &mut || $make,
            );
        }
        #[test]
        fn fr12_the_gallery_lists_the_widgets_the_callers_android_offers() {
            $crate::host_api_conformance::widget_gallery_lists_the_offer(&mut || $make);
        }
        #[test]
        fn fr12_adding_a_widget_places_it_and_the_service_keeps_its_place() {
            $crate::host_api_conformance::added_widget_is_placed_and_kept(&mut || $make);
        }
        #[test]
        fn fr12_a_widget_watcher_receives_widgets_frames_and_removals() {
            $crate::host_api_conformance::widget_watcher_receives_changes(&mut || $make);
        }
        #[test]
        fn fr12_moving_or_resizing_a_widget_window_tells_android_its_size_in_dp() {
            $crate::host_api_conformance::placed_widget_size_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn fr12_pointer_input_in_a_widget_window_reaches_the_callers_android() {
            $crate::host_api_conformance::widget_pointer_reaches_the_caller(&mut || $make);
        }
        #[test]
        fn nfr5_malformed_widget_requests_are_refused_and_the_service_goes_on() {
            $crate::host_api_conformance::malformed_widget_requests_are_refused(&mut || $make);
        }
        #[test]
        fn fr17_settings_are_read_and_changed_through_the_service() {
            $crate::host_api_conformance::settings_round_trip(&mut || $make);
        }
        #[test]
        fn fr17_the_settings_name_the_android_image_the_next_start_boots() {
            $crate::host_api_conformance::android_version_follows_the_image(&mut || $make);
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
        min_width: 700,
        min_height: 500,
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

/// How long the tests below leave a watch idle before anything happens: long enough for
/// many rounds of the service's and the client's timed-out reads.
const IDLE: Duration = Duration::from_secs(3);

/// How much later than its timeout a wait for a task change may return.
const LATE: Duration = Duration::from_millis(500);

/// Waits on `stream` for `IDLE`, one short wait after another, as the compositor does.
fn idle(stream: &mut crate::host_api::TaskStream) {
    let deadline = Instant::now() + IDLE;
    while Instant::now() < deadline {
        assert_eq!(
            stream
                .next(Duration::from_millis(200))
                .expect("the watch goes on"),
            None
        );
    }
}

pub fn watch_made_while_stopped_receives_tasks_after_start(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut stream = connect(&mut *connector).watch_tasks().expect("watching");
    let android = androids.of(&user).unwrap();
    assert_eq!(android.state(), SessionState::Stopped);
    idle(&mut stream);

    let mut client = connect(&mut *connector);
    assert_eq!(client.start().unwrap().state, SessionState::Active);
    idle(&mut stream);
    android.publish(TaskEvent::Updated(sample_task(7, "Settings")));

    assert_eq!(
        next_event(&mut stream),
        TaskEvent::Updated(sample_task(7, "Settings"))
    );
}

pub fn waiting_for_a_change_returns_in_time(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut stream = connect(&mut *connector).watch_tasks().expect("watching");
    for timeout in [1, 50, 200, 500].map(Duration::from_millis) {
        let started = Instant::now();
        assert_eq!(stream.next(timeout).expect("the watch goes on"), None);
        let waited = started.elapsed();
        assert!(
            waited < timeout + LATE,
            "a wait of {timeout:?} for a task change took {waited:?}"
        );
    }
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

pub fn windows_shown_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    // Taken while the Android is stopped, for its next start.
    client
        .request(&TaskRequest::WindowsShown { shown: false })
        .expect("the word while stopped");
    client.start().unwrap();
    client
        .set_windows_shown(true)
        .expect("the word while running");
    let android = androids.of(&user).unwrap();
    assert_eq!(*android.shown.lock().unwrap(), vec![false, true]);
    assert!(android.requests.lock().unwrap().is_empty());
}

pub fn freeze_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids);
    let mut client = connect(&mut *connector);
    match client.freeze() {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::NotRunning),
        other => panic!("expected the service to say Android is not running, got {other:?}"),
    }
    client.start().unwrap();
    assert_eq!(client.freeze().unwrap().state, SessionState::Frozen);
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
    // The service reads the stream on a thread of its own, and nothing answers an input
    // event, so the test waits until Android has refused the first event before it starts.
    let deadline = std::time::Instant::now() + WAIT;
    while androids
        .of(&user)
        .is_none_or(|android| *android.pointers_refused.lock().unwrap() == 0)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the event sent while Android was stopped never reached it"
        );
        thread::sleep(Duration::from_millis(5));
    }
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

/// Waits until `android` has been given `count` texts copied on the host.
fn host_texts(android: &FakeAndroid, count: usize) -> Vec<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        let texts = android.host_clipboard.lock().unwrap().clone();
        if texts.len() >= count || Instant::now() >= deadline {
            return texts;
        }
        thread::sleep(Duration::from_millis(5));
    }
}

/// An APK of a few local API pieces, so it crosses in more than one.
fn sample_apk() -> Vec<u8> {
    (0..apk::LOCAL_CHUNK_BYTES * 2 + 12_345)
        .map(|i| (i % 251) as u8)
        .collect()
}

pub fn apk_is_installed(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let content = sample_apk();
    let outcome = connect(&mut *connector)
        .install_apk("notes.apk", content.len() as u64, &mut &content[..])
        .expect("installing");
    assert_eq!(outcome.status, InstallStatus::Installed);
    assert_eq!(outcome.package_name.as_deref(), Some(INSTALLED_PACKAGE));
    let android = androids.of(&user).unwrap();
    let installs = android.installs.lock().unwrap();
    assert_eq!(installs.len(), 1);
    assert_eq!(installs[0].0, "notes.apk");
    assert!(installs[0].1 == content, "the APK arrived changed");
}

pub fn refused_apk_has_its_reason(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids);
    connect(&mut *connector).start().unwrap();
    let mut content = BAD_APK.to_vec();
    content.extend_from_slice(&[0; 100]);
    let outcome = connect(&mut *connector)
        .install_apk("broken.apk", content.len() as u64, &mut &content[..])
        .expect("a refusal is an outcome");
    assert_eq!(outcome.status, InstallStatus::InvalidApk);
    assert!(
        outcome.message.contains("INSTALL_PARSE_FAILED_NOT_APK"),
        "{outcome:?}"
    );
    // The service goes on.
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Active
    );
}

pub fn wrong_file_is_refused_before_it_is_sent(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    for (name, size) in [
        ("notes.zip", 10),
        ("notes.apk", apk::MAX_APK_BYTES + 1),
        ("notes.apk", 0),
    ] {
        // Nothing may be read: the reader fails the test if it is.
        let mut never = Unread;
        match connect(&mut *connector).install_apk(name, size, &mut never) {
            Err(ClientError::Service { code, detail }) => {
                assert_eq!(code, ErrorCode::InstallRefused, "{detail}");
            }
            other => panic!("expected {name} of {size} bytes to be refused, got {other:?}"),
        }
    }
    assert!(androids
        .of(&user)
        .unwrap()
        .installs
        .lock()
        .unwrap()
        .is_empty());
}

/// A reader that must never be read.
struct Unread;

impl std::io::Read for Unread {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        panic!("the APK was read although the service refused it");
    }
}

pub fn install_needs_a_running_android(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let content = sample_apk();
    match connect(&mut *connector).install_apk("notes.apk", content.len() as u64, &mut &content[..])
    {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::NotRunning),
        other => panic!("expected the install to need a running Android, got {other:?}"),
    }
}

pub fn installs_run_one_at_a_time(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let android = androids.of(&user).unwrap();
    *android.install_time.lock().unwrap() = Duration::from_millis(300);
    let first = connect(&mut *connector);
    let second = connect(&mut *connector);
    let one = thread::spawn(move || first.install_apk("one.apk", 3, &mut &b"one"[..]));
    let two = thread::spawn(move || second.install_apk("two.apk", 3, &mut &b"two"[..]));
    for install in [one, two] {
        let outcome = install.join().unwrap().expect("both are installed");
        assert_eq!(outcome.status, InstallStatus::Installed);
    }
    let steps = android.install_steps.lock().unwrap().clone();
    assert_eq!(steps.len(), 4, "{steps:?}");
    // Each install ends before the next begins, whichever came first.
    assert_eq!(steps[0][1..], steps[1][1..], "{steps:?}");
    assert!(
        steps[0].starts_with('+') && steps[1].starts_with('-'),
        "{steps:?}"
    );
    assert_eq!(steps[2][1..], steps[3][1..], "{steps:?}");
}

pub fn malformed_install_is_refused(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let request = types::Request {
        id: 7,
        body: Some(types::request::Body::InstallApk(types::InstallApk {
            name: "notes.apk".into(),
            size: 1000,
        })),
    };
    send(&mut *connection, envelope::Body::Request(request)).unwrap();
    let answer = |connection: &mut dyn dsa_transport::Connection| match decode(
        &connection.recv(Some(WAIT)).expect("an answer"),
    ) {
        Ok(types::Envelope {
            body: Some(envelope::Body::Response(response)),
        }) => response,
        other => panic!("expected an answer, got {other:?}"),
    };
    assert!(matches!(
        answer(&mut *connection).body,
        Some(types::response::Body::Done(_))
    ));
    // Half the APK, then something that is not a piece of it.
    send(
        &mut *connection,
        envelope::Body::ApkChunk(types::ApkChunk { data: vec![1; 500] }),
    )
    .unwrap();
    send(
        &mut *connection,
        envelope::Body::ClipboardText(types::ClipboardText {
            text: "not an APK".into(),
        }),
    )
    .unwrap();
    let response = answer(&mut *connection);
    assert_eq!(response.id, 7);
    match response.body {
        Some(types::response::Body::Error(error)) => {
            assert_eq!(
                error.code,
                ErrorCode::InstallRefused as i32,
                "{}",
                error.detail
            );
        }
        other => panic!("expected the install to be refused, got {other:?}"),
    }
    assert!(androids
        .of(&user)
        .unwrap()
        .installs
        .lock()
        .unwrap()
        .is_empty());
    // The service and the next install go on.
    let content = sample_apk();
    let outcome = connect(&mut *connector)
        .install_apk("notes.apk", content.len() as u64, &mut &content[..])
        .unwrap();
    assert!(outcome.installed());
}

pub fn host_clipboard_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut clipboard = connect(&mut *connector)
        .watch_clipboard()
        .expect("carrying the clipboard");
    clipboard.send("copied on the host").unwrap();
    clipboard.send("\u{d55c}\u{ae00}").unwrap();
    let android = androids.of(&user).expect("the caller's Android");
    assert_eq!(
        host_texts(&android, 2),
        vec![
            "copied on the host".to_string(),
            "\u{d55c}\u{ae00}".to_string()
        ]
    );
}

pub fn guest_clipboard_reaches_the_client(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut clipboard = connect(&mut *connector)
        .watch_clipboard()
        .expect("carrying the clipboard");
    let android = androids.of(&user).expect("the caller's Android");
    assert_eq!(clipboard.next(Duration::from_millis(50)).unwrap(), None);
    android.copy_in_android("copied in Android");
    assert_eq!(
        clipboard.next(WAIT).unwrap().as_deref(),
        Some("copied in Android")
    );
}

pub fn clipboard_setting_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let store = Arc::new(MemorySettings::default());
    let androids = Arc::new(FakeAndroids::default());
    let service =
        HostApiService::new(androids.clone(), "test service").with_settings(store.clone(), facts());
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    assert!(
        client.settings().unwrap().settings.clipboard,
        "on by default"
    );
    let off = client
        .set_settings(&Settings {
            clipboard: false,
            ..Settings::default()
        })
        .expect("turning clipboard sharing off");
    assert!(!off.settings.clipboard);
    assert!(!store.get(&user).unwrap().clipboard);
    client.set_settings(&Settings::default()).unwrap();
    assert_eq!(
        *androids.of(&user).unwrap().clipboard_shared.lock().unwrap(),
        vec![false, true]
    );
}

pub fn clipboard_client_that_sends_a_request_is_disconnected(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let watch = dsa_protocol::host_api::types::Request {
        id: 1,
        body: Some(
            dsa_protocol::host_api::types::request::Body::WatchClipboard(
                dsa_protocol::host_api::types::WatchClipboard {},
            ),
        ),
    };
    dsa_protocol::host_api::send(
        &mut *connection,
        dsa_protocol::host_api::types::envelope::Body::Request(watch.clone()),
    )
    .unwrap();
    connection
        .recv(Some(WAIT))
        .expect("the answer to the watch");
    // A frame that cannot be read is dropped, and the connection goes on.
    connection.send(&[0xff, 0xff, 0xff]).unwrap();
    dsa_protocol::host_api::send(
        &mut *connection,
        dsa_protocol::host_api::types::envelope::Body::Request(watch),
    )
    .unwrap();
    assert!(
        connection.recv(Some(WAIT)).is_err(),
        "the service closes a clipboard client that sends a request"
    );
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Stopped
    );
}

fn small_tree(epoch: u32) -> TreeUpdate {
    TreeUpdate {
        task_id: 7,
        epoch,
        full: true,
        roots: vec![1],
        nodes: vec![
            Node {
                id: 1,
                children: vec![2],
                class_name: "android.widget.FrameLayout".into(),
                ..Node::default()
            },
            Node {
                id: 2,
                class_name: "android.widget.Button".into(),
                text: "OK".into(),
                actions: vec![NodeAction::Click],
                ..Node::default()
            },
        ],
        removed: vec![],
        focused: Some(2),
    }
}

/// Waits until `watches` holds `expected`, which the service records from its own thread.
fn wait_for_watches(android: &FakeAndroid, expected: &[(i32, bool)]) {
    let deadline = Instant::now() + WAIT;
    while *android.accessibility_watches.lock().unwrap() != expected {
        assert!(
            Instant::now() < deadline,
            "the watches were {:?}",
            android.accessibility_watches.lock().unwrap()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn accessibility_reaches_the_client(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut stream = connect(&mut *connector)
        .watch_accessibility(7)
        .expect("watching the tree");
    let android = androids.of(&user).expect("the caller's Android");
    wait_for_watches(&android, &[(7, true)]);
    assert_eq!(stream.next(Duration::from_millis(50)).unwrap(), None);
    // Another task's tree does not reach this watch.
    android.publish_accessibility(8, GuestAccessibility::Tree(small_tree(1)));
    android.publish_accessibility(7, GuestAccessibility::Tree(small_tree(1)));
    assert_eq!(
        stream.next(WAIT).unwrap(),
        Some(GuestAccessibility::Tree(small_tree(1)))
    );
    let result = ActionResult {
        request_id: 3,
        outcome: Err("the node is gone".into()),
    };
    android.publish_accessibility(7, GuestAccessibility::ActionResult(result.clone()));
    assert_eq!(
        stream.next(WAIT).unwrap(),
        Some(GuestAccessibility::ActionResult(result))
    );
}

pub fn accessibility_actions_reach_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    client.start().unwrap();
    let mut stream = client.watch_accessibility(7).expect("watching the tree");
    let android = androids.of(&user).expect("the caller's Android");
    let action = ActionRequest {
        epoch: 1,
        node_id: 2,
        action: NodeAction::Click,
        text: String::new(),
        request_id: 4,
    };
    stream.perform(&action).unwrap();
    let deadline = Instant::now() + WAIT;
    while android.accessibility_actions.lock().unwrap().is_empty() {
        assert!(Instant::now() < deadline, "the action never arrived");
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        *android.accessibility_actions.lock().unwrap(),
        vec![(7, action)]
    );
    // An action for an Android that is not running is answered with why.
    android.stop();
    let refused = ActionRequest {
        request_id: 5,
        ..ActionRequest {
            epoch: 1,
            node_id: 2,
            action: NodeAction::Click,
            text: String::new(),
            request_id: 0,
        }
    };
    stream.perform(&refused).unwrap();
    match stream.next(WAIT).unwrap() {
        Some(GuestAccessibility::ActionResult(result)) => {
            assert_eq!(result.request_id, 5);
            assert!(result.outcome.is_err());
        }
        other => panic!("expected the refusal, got {other:?}"),
    }
    // Closing the connection ends the watch, so Android stops reading the tree.
    drop(stream);
    wait_for_watches(&android, &[(7, true), (7, false)]);
}

pub fn accessibility_client_that_sends_a_request_is_disconnected(make: MakeLocal<'_>) {
    use dsa_protocol::host_api::types as wire;
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let watch = wire::Request {
        id: 1,
        body: Some(wire::request::Body::WatchAccessibility(
            wire::WatchAccessibility { task_id: 7 },
        )),
    };
    dsa_protocol::host_api::send(
        &mut *connection,
        wire::envelope::Body::Request(watch.clone()),
    )
    .unwrap();
    connection
        .recv(Some(WAIT))
        .expect("the answer to the watch");
    dsa_protocol::host_api::send(&mut *connection, wire::envelope::Body::Request(watch)).unwrap();
    assert!(
        connection.recv(Some(WAIT)).is_err(),
        "the service closes an accessibility client that sends a request"
    );
    let android = androids.of(&user).expect("the caller's Android");
    wait_for_watches(&android, &[(7, true), (7, false)]);
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Stopped
    );
}

pub fn link_reaches_the_client(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut links = connect(&mut *connector)
        .watch_links()
        .expect("watching links");
    let android = androids.of(&user).expect("the caller's Android");
    assert_eq!(links.next(Duration::from_millis(50)).unwrap(), None);
    android.open_link_in_android("https://example.com/page");
    assert_eq!(
        links.next(WAIT).unwrap().as_deref(),
        Some("https://example.com/page")
    );
}

pub fn developer_mode_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let store = Arc::new(MemorySettings::default());
    let androids = Arc::new(FakeAndroids::default());
    let service =
        HostApiService::new(androids.clone(), "test service").with_settings(store.clone(), facts());
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    let fresh = client.settings().unwrap().settings;
    assert!(!fresh.developer_mode, "off by default");
    assert_eq!(fresh.adb_port, 58526);
    let on = client
        .set_settings(&Settings {
            developer_mode: true,
            ..Settings::default()
        })
        .expect("turning developer mode on");
    assert!(on.settings.developer_mode);
    assert!(store.get(&user).unwrap().developer_mode);
    client
        .set_settings(&Settings {
            developer_mode: true,
            adb_port: 5037,
            ..Settings::default()
        })
        .unwrap();
    client.set_settings(&Settings::default()).unwrap();
    assert_eq!(
        *androids.of(&user).unwrap().developer_mode.lock().unwrap(),
        vec![(true, 58526), (true, 5037), (false, 58526)]
    );
}

pub fn developer_mode_port_refusal_is_reported(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let store = Arc::new(MemorySettings::default());
    let androids = Arc::new(FakeAndroids::default());
    let service =
        HostApiService::new(androids.clone(), "test service").with_settings(store.clone(), facts());
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    // The caller's Android is made by the first request that needs it.
    client.settings().unwrap();
    client.set_settings(&Settings::default()).unwrap();
    *androids
        .of(&user)
        .unwrap()
        .refuse_developer_mode
        .lock()
        .unwrap() = Some("cannot listen for adb on 127.0.0.1:58526".into());
    match client.set_settings(&Settings {
        developer_mode: true,
        ..Settings::default()
    }) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::InvalidSetting);
            assert!(detail.contains("127.0.0.1:58526"), "{detail}");
        }
        other => panic!("expected the service to report the port, got {other:?}"),
    }
}

pub fn link_setting_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let store = Arc::new(MemorySettings::default());
    let androids = Arc::new(FakeAndroids::default());
    let service =
        HostApiService::new(androids.clone(), "test service").with_settings(store.clone(), facts());
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    assert!(
        !client.settings().unwrap().settings.links_in_android,
        "links open on the host by default"
    );
    let changed = client
        .set_settings(&Settings {
            links_in_android: true,
            ..Settings::default()
        })
        .expect("letting links open in Android");
    assert!(changed.settings.links_in_android);
    assert!(store.get(&user).unwrap().links_in_android);
    client.set_settings(&Settings::default()).unwrap();
    assert_eq!(
        *androids.of(&user).unwrap().links_in_android.lock().unwrap(),
        vec![true, false]
    );
}

pub fn link_client_that_sends_is_disconnected(make: MakeLocal<'_>) {
    use dsa_protocol::host_api::types as wire;
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let watch = wire::Request {
        id: 1,
        body: Some(wire::request::Body::WatchLinks(wire::WatchLinks {})),
    };
    dsa_protocol::host_api::send(
        &mut *connection,
        wire::envelope::Body::Request(watch.clone()),
    )
    .unwrap();
    connection
        .recv(Some(WAIT))
        .expect("the answer to the watch");
    dsa_protocol::host_api::send(&mut *connection, wire::envelope::Body::Request(watch)).unwrap();
    assert!(
        connection.recv(Some(WAIT)).is_err(),
        "the service closes a link client that sends a frame"
    );
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Stopped,
        "the service goes on"
    );
}

pub fn file_reaches_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    // Larger than one piece on the local API, and not a whole number of them.
    let content: Vec<u8> = (0..(2 * crate::files::LOCAL_CHUNK_BYTES + 12_345))
        .map(|i| (i % 251) as u8)
        .collect();
    let file = FileTransfer {
        name: "\u{c0ac}\u{c9c4} photo.jpg".into(),
        content,
        action: FileAction::Send,
    };
    connect(&mut *connector)
        .open_file(&file)
        .expect("sending the file");
    let empty = FileTransfer {
        name: "empty.txt".into(),
        content: Vec::new(),
        action: FileAction::View,
    };
    connect(&mut *connector)
        .open_file(&empty)
        .expect("sending an empty file");
    assert_eq!(
        *androids.of(&user).unwrap().files.lock().unwrap(),
        vec![file, empty]
    );
}

/// Sends an OpenFile request by hand and returns the connection and the first answer.
fn raw_open_file(
    connector: &mut dyn Connector,
    open: dsa_protocol::host_api::types::OpenFile,
) -> (
    Box<dyn dsa_transport::Connection>,
    dsa_protocol::host_api::types::Response,
) {
    use dsa_protocol::host_api::types as wire;
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    dsa_protocol::host_api::send(
        &mut *connection,
        wire::envelope::Body::Request(wire::Request {
            id: 7,
            body: Some(wire::request::Body::OpenFile(open)),
        }),
    )
    .unwrap();
    let answer = response_of(&mut *connection);
    (connection, answer)
}

fn response_of(
    connection: &mut dyn dsa_transport::Connection,
) -> dsa_protocol::host_api::types::Response {
    use dsa_protocol::host_api::types as wire;
    let frame = connection.recv(Some(WAIT)).expect("an answer");
    match dsa_protocol::host_api::decode(&frame) {
        Ok(wire::Envelope {
            body: Some(wire::envelope::Body::Response(response)),
        }) => response,
        other => panic!("expected a response, got {other:?}"),
    }
}

fn refusal(response: &dsa_protocol::host_api::types::Response) -> Option<(i32, String)> {
    match &response.body {
        Some(dsa_protocol::host_api::types::response::Body::Error(error)) => {
            Some((error.code, error.detail.clone()))
        }
        _ => None,
    }
}

pub fn oversized_file_is_refused(make: MakeLocal<'_>) {
    use dsa_protocol::host_api::types as wire;
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let (_connection, answer) = raw_open_file(
        &mut *connector,
        wire::OpenFile {
            name: "movie.mp4".into(),
            size: MAX_FILE_BYTES + 1,
            action: wire::FileAction::View as i32,
        },
    );
    let (code, detail) = refusal(&answer).expect("the file is refused");
    assert_eq!(code, ErrorCode::FileRefused as i32);
    assert!(detail.contains("MiB"), "{detail}");
    let (_connection, answer) = raw_open_file(
        &mut *connector,
        wire::OpenFile {
            name: "..\\secret".into(),
            size: 3,
            action: wire::FileAction::View as i32,
        },
    );
    assert_eq!(
        refusal(&answer).map(|(code, _)| code),
        Some(ErrorCode::FileRefused as i32),
        "a name with a folder is refused"
    );
    assert!(androids.of(&user).unwrap().files.lock().unwrap().is_empty());
}

pub fn file_for_stopped_android_is_refused(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let file = FileTransfer {
        name: "a.txt".into(),
        content: b"abc".to_vec(),
        action: FileAction::View,
    };
    match connect(&mut *connector).open_file(&file) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::NotRunning);
            assert!(detail.contains("not running"), "{detail}");
        }
        other => panic!("expected the file to be refused, got {other:?}"),
    }
}

pub fn malformed_file_is_refused(make: MakeLocal<'_>) {
    use dsa_protocol::host_api::types as wire;
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let open = wire::OpenFile {
        name: "a.txt".into(),
        size: 4,
        action: wire::FileAction::View as i32,
    };

    // More bytes than announced.
    let (mut connection, answer) = raw_open_file(&mut *connector, open.clone());
    assert!(refusal(&answer).is_none(), "the file is taken at first");
    dsa_protocol::host_api::send(
        &mut *connection,
        wire::envelope::Body::FileChunk(wire::FileChunk {
            data: b"too long".to_vec(),
        }),
    )
    .unwrap();
    let (code, _) = refusal(&response_of(&mut *connection)).expect("refused");
    assert_eq!(code, ErrorCode::FileRefused as i32);

    // An unreadable frame in place of a piece.
    let (mut connection, _) = raw_open_file(&mut *connector, open.clone());
    connection.send(&[0xff, 0xff, 0xff]).unwrap();
    let (code, _) = refusal(&response_of(&mut *connection)).expect("refused");
    assert_eq!(code, ErrorCode::FileRefused as i32);

    // Neither open nor share.
    let (_connection, answer) = raw_open_file(
        &mut *connector,
        wire::OpenFile {
            action: wire::FileAction::Unspecified as i32,
            ..open
        },
    );
    assert!(refusal(&answer).is_some());

    assert!(androids.of(&user).unwrap().files.lock().unwrap().is_empty());
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Active,
        "the service goes on"
    );
}

fn sample_notification(key: &str, title: &str) -> Notification {
    Notification {
        key: key.into(),
        package_name: "com.example.chat".into(),
        app_label: "Chat".into(),
        title: title.into(),
        text: "\u{c548}\u{b155}".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
        importance: 4,
        high_priority: true,
        clearable: true,
        when_millis: 1_700_000_000_000,
    }
}

pub fn notifications_reach_a_watcher(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut watch = connect(&mut *connector)
        .watch_notifications()
        .expect("watching notifications");
    let android = androids.of(&user).expect("the caller's Android");
    assert_eq!(watch.next(Duration::from_millis(50)).unwrap(), None);
    let posted = sample_notification("0|com.example.chat|1|null|10100", "Ann");
    android.notify(NotificationEvent::Posted(posted.clone()));
    assert_eq!(
        watch.next(WAIT).unwrap(),
        Some(NotificationEvent::Posted(posted.clone()))
    );
    let updated = sample_notification("0|com.example.chat|1|null|10100", "Ann (2)");
    android.notify(NotificationEvent::Posted(updated.clone()));
    assert_eq!(
        watch.next(WAIT).unwrap(),
        Some(NotificationEvent::Posted(updated))
    );
    android.notify(NotificationEvent::Removed {
        key: posted.key.clone(),
    });
    assert_eq!(
        watch.next(WAIT).unwrap(),
        Some(NotificationEvent::Removed { key: posted.key })
    );
}

pub fn notification_actions_reach_the_caller(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    client.start().unwrap();
    let open = NotificationAction::Open {
        key: "0|com.example.chat|1|null|10100".into(),
    };
    let dismiss = NotificationAction::Dismiss {
        key: "0|com.example.chat|2|null|10100".into(),
    };
    client.act_on_notification(&open).expect("opened");
    client.act_on_notification(&dismiss).expect("dismissed");
    assert_eq!(
        *androids
            .of(&user)
            .unwrap()
            .notification_actions
            .lock()
            .unwrap(),
        vec![open, dismiss]
    );
}

pub fn notification_action_needs_a_running_android(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut client = connect(&mut *connector);
    match client.act_on_notification(&NotificationAction::Open { key: "k".into() }) {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::NotRunning),
        other => panic!("expected the Android not to be running, got {other:?}"),
    }
}

pub fn malformed_notification_action_is_refused(make: MakeLocal<'_>) {
    use dsa_protocol::host_api::types::{
        envelope::Body, request, response, ActOnNotification, Envelope, Request,
    };
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(listener, androids.clone());
    connect(&mut *connector).start().unwrap();
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let bad = [
        ActOnNotification {
            key: String::new(),
            action: 1,
        },
        ActOnNotification {
            key: "k".repeat(crate::notifications::MAX_KEY_BYTES + 1),
            action: 1,
        },
        ActOnNotification {
            key: "k".into(),
            action: 0,
        },
        ActOnNotification {
            key: "k".into(),
            action: 77,
        },
    ];
    for (id, act) in bad.into_iter().enumerate() {
        let request = Request {
            id: id as u64 + 1,
            body: Some(request::Body::ActOnNotification(act)),
        };
        dsa_protocol::host_api::send(&mut *connection, Body::Request(request)).unwrap();
        let frame = connection.recv(Some(WAIT)).expect("an answer");
        match dsa_protocol::host_api::decode(&frame) {
            Ok(Envelope {
                body:
                    Some(Body::Response(dsa_protocol::host_api::types::Response {
                        body: Some(response::Body::Error(_)),
                        ..
                    })),
            }) => {}
            other => panic!("expected the action to be refused, got {other:?}"),
        }
    }
    assert!(androids
        .of(&user)
        .unwrap()
        .notification_actions
        .lock()
        .unwrap()
        .is_empty());
    // The service is still there.
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Active
    );
}

pub fn notification_watcher_that_sends_is_disconnected(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let _service = RunningService::start(listener, Arc::new(FakeAndroids::default()));
    let mut connection = connector.connect(WAIT).unwrap();
    client_connect(&mut *connection, &local_hello(Role::Client, "client"), WAIT).unwrap();
    let watch = dsa_protocol::host_api::types::Request {
        id: 1,
        body: Some(
            dsa_protocol::host_api::types::request::Body::WatchNotifications(
                dsa_protocol::host_api::types::WatchNotifications {},
            ),
        ),
    };
    dsa_protocol::host_api::send(
        &mut *connection,
        dsa_protocol::host_api::types::envelope::Body::Request(watch.clone()),
    )
    .unwrap();
    connection
        .recv(Some(WAIT))
        .expect("the answer to the watch");
    // A frame that cannot be read is dropped, and the connection goes on.
    connection.send(&[0xff, 0xff, 0xff]).unwrap();
    dsa_protocol::host_api::send(
        &mut *connection,
        dsa_protocol::host_api::types::envelope::Body::Request(watch),
    )
    .unwrap();
    assert!(
        connection.recv(Some(WAIT)).is_err(),
        "the service closes a notification watcher that sends a request"
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
        android_version: Some("16.0.0-15581820-7".into()),
    }
}

pub fn android_version_follows_the_image(make: MakeLocal<'_>) {
    let (listener, mut connector, _) = make();
    let next = Arc::new(Mutex::new(Some("16.0.0-15581820-7".to_string())));
    let source = Arc::clone(&next);
    let service = HostApiService::new(Arc::new(FakeAndroids::default()), "test service")
        .with_settings(Arc::new(MemorySettings::default()), facts())
        .with_android_version(Arc::new(move || source.lock().unwrap().clone()));
    let _service = RunningService::start_service(listener, service);
    let mut client = connect(&mut *connector);
    assert_eq!(
        client.settings().unwrap().facts.android_version.as_deref(),
        Some("16.0.0-15581820-7")
    );
    // An image update moves the next start to another image while the service runs.
    *next.lock().unwrap() = Some("17.0.0-16373615-7".into());
    assert_eq!(
        client.settings().unwrap().facts.android_version.as_deref(),
        Some("17.0.0-16373615-7")
    );
    *next.lock().unwrap() = None;
    assert_eq!(client.settings().unwrap().facts.android_version, None);
}

pub fn settings_round_trip(make: MakeLocal<'_>) {
    let (listener, mut connector, user) = make();
    let store = Arc::new(MemorySettings::default());
    let androids = Arc::new(FakeAndroids::default());
    let service =
        HostApiService::new(androids.clone(), "test service").with_settings(store.clone(), facts());
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
    // The caller's Android follows the freeze setting at once.
    assert_eq!(
        *androids.of(&user).unwrap().freeze_when_idle.lock().unwrap(),
        vec![true]
    );
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

/// The clock widget of the widget checks.
pub fn sample_widget_provider() -> WidgetProvider {
    WidgetProvider {
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
    }
}

/// A started Android that offers the clock widget, and a client of it.
fn widget_android(
    make: MakeLocal<'_>,
) -> (
    RunningService,
    Arc<FakeAndroids>,
    Box<dyn Connector>,
    String,
    HostApiClient,
) {
    let (listener, mut connector, user) = make();
    let androids = Arc::new(FakeAndroids::default());
    let service = RunningService::start(listener, androids.clone());
    let mut client = connect(&mut *connector);
    client.start().unwrap();
    androids
        .of(&user)
        .unwrap()
        .widget_offer
        .lock()
        .unwrap()
        .push(sample_widget_provider());
    (service, androids, connector, user, client)
}

pub fn widget_gallery_lists_the_offer(make: MakeLocal<'_>) {
    let (_service, _androids, _connector, _user, mut client) = widget_android(make);
    assert_eq!(
        client.widget_providers().unwrap(),
        vec![sample_widget_provider()]
    );
    client.stop().unwrap();
    match client.widget_providers() {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::NotRunning),
        other => panic!("expected the Android not to be running, got {other:?}"),
    }
}

pub fn added_widget_is_placed_and_kept(make: MakeLocal<'_>) {
    let (_service, _androids, _connector, _user, mut client) = widget_android(make);
    let widget = client
        .add_widget("com.example.clock/.ClockWidget", WAIT)
        .unwrap();
    assert_eq!(widget.provider, sample_widget_provider());
    assert_eq!(
        widget.placement,
        Placement {
            x: 64,
            y: 64,
            width_dp: 180,
            height_dp: 110
        }
    );
    let second = client
        .add_widget("com.example.clock/.ClockWidget", WAIT)
        .unwrap();
    assert_ne!(
        second.placement, widget.placement,
        "a new widget hides none"
    );
    let (placed, remembered) = client.widgets().unwrap();
    assert_eq!(placed, vec![widget, second]);
    assert_eq!(remembered, 2);
    match client.add_widget("com.example.other/.Nothing", WAIT) {
        Err(ClientError::Service { code, detail }) => {
            assert_eq!(code, ErrorCode::WidgetRefused, "{detail}")
        }
        other => panic!("expected the widget to be refused, got {other:?}"),
    }
}

pub fn widget_watcher_receives_changes(make: MakeLocal<'_>) {
    let (_service, androids, mut connector, user, mut client) = widget_android(make);
    let first = client
        .add_widget("com.example.clock/.ClockWidget", WAIT)
        .unwrap();
    let mut watch = connect(&mut *connector).watch_widgets().unwrap();
    assert_eq!(watch.current(), std::slice::from_ref(&first));
    assert_eq!(watch.remembered(), 1);
    let android = androids.of(&user).unwrap();
    let frame = WidgetFrame {
        widget_id: first.widget_id,
        width: 2,
        height: 1,
        png: vec![0x89, b'P', b'N', b'G'],
    };
    android.widget_event(WidgetEvent::Frame(frame.clone()));
    assert_eq!(watch.next(WAIT).unwrap(), Some(WidgetEvent::Frame(frame)));
    // Described again, as when Android starts: the same place.
    android.widget_event(WidgetEvent::Placed(Widget {
        placement: Placement::default(),
        ..first.clone()
    }));
    assert_eq!(
        watch.next(WAIT).unwrap(),
        Some(WidgetEvent::Placed(first.clone()))
    );
    client.remove_widget(first.widget_id).unwrap();
    assert_eq!(
        watch.next(WAIT).unwrap(),
        Some(WidgetEvent::Removed {
            widget_id: first.widget_id
        })
    );
    assert_eq!(client.widgets().unwrap(), (Vec::new(), 0));
    match client.remove_widget(first.widget_id) {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::UnknownRequest),
        other => panic!("expected an unknown widget, got {other:?}"),
    }
}

pub fn placed_widget_size_reaches_the_caller(make: MakeLocal<'_>) {
    let (_service, androids, _connector, user, mut client) = widget_android(make);
    let widget = client
        .add_widget("com.example.clock/.ClockWidget", WAIT)
        .unwrap();
    let moved = Placement {
        x: -300,
        y: 20,
        width_dp: 240,
        height_dp: 147,
    };
    client
        .place_widget(widget.widget_id, moved, 360, 220, 240)
        .unwrap();
    let sizes = androids
        .of(&user)
        .unwrap()
        .widget_sizes
        .lock()
        .unwrap()
        .clone();
    assert_eq!(
        sizes,
        vec![(
            widget.widget_id,
            WidgetSize {
                width: 360,
                height: 220,
                density_dpi: 240,
                width_dp: 240,
                height_dp: 147
            }
        )]
    );
    assert_eq!(client.widgets().unwrap().0[0].placement, moved);
    // A size the widget does not allow is kept within its limits.
    client
        .place_widget(
            widget.widget_id,
            Placement {
                width_dp: 1,
                height_dp: 1,
                ..moved
            },
            0,
            0,
            0,
        )
        .unwrap();
    let kept = client.widgets().unwrap().0[0].placement;
    assert_eq!((kept.width_dp, kept.height_dp), (110, 40));
}

pub fn widget_pointer_reaches_the_caller(make: MakeLocal<'_>) {
    let (_service, androids, _connector, user, mut client) = widget_android(make);
    let widget = client
        .add_widget("com.example.clock/.ClockWidget", WAIT)
        .unwrap();
    client
        .widget_pointer(widget.widget_id, types::PointerAction::Down, 0, 10.5, 20.25)
        .unwrap();
    client
        .widget_pointer(widget.widget_id, types::PointerAction::Up, 0, 10.5, 20.25)
        .unwrap();
    let given = androids
        .of(&user)
        .unwrap()
        .widget_pointers
        .lock()
        .unwrap()
        .clone();
    assert_eq!(given.len(), 2);
    assert_eq!(
        given[0].action,
        dsa_protocol::control::PointerAction::Down as i32
    );
    assert_eq!((given[1].x, given[1].y), (10.5, 20.25));
}

pub fn malformed_widget_requests_are_refused(make: MakeLocal<'_>) {
    let (_service, androids, mut connector, user, mut client) = widget_android(make);
    let widget = client
        .add_widget("com.example.clock/.ClockWidget", WAIT)
        .unwrap();
    let refused = |result: Result<(), ClientError>| match result {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::UnknownRequest),
        other => panic!("expected a refusal, got {other:?}"),
    };
    refused(client.widget_pointer(
        widget.widget_id,
        types::PointerAction::Down,
        0,
        f32::NAN,
        1.0,
    ));
    refused(client.widget_pointer(widget.widget_id, types::PointerAction::Down, 0, 1e9, 1.0));
    refused(client.widget_pointer(widget.widget_id, types::PointerAction::Scroll, 0, 1.0, 1.0));
    refused(client.widget_pointer(999, types::PointerAction::Down, 0, 1.0, 1.0));
    refused(client.place_widget(999, Placement::default(), 10, 10, 160));
    match client.add_widget("../../etc", WAIT) {
        Err(ClientError::Service { code, .. }) => assert_eq!(code, ErrorCode::UnknownRequest),
        other => panic!("expected a refusal, got {other:?}"),
    }
    assert!(androids
        .of(&user)
        .unwrap()
        .widget_pointers
        .lock()
        .unwrap()
        .is_empty());
    assert_eq!(
        connect(&mut *connector).status().unwrap().state,
        SessionState::Active
    );
}
