//! The local API: the service that answers the apps in users' sessions, and the client
//! those apps use.
//!
//! The service takes each caller's user from the transport (the operating system reports
//! it) and acts on that user's Android only. Nothing a client sends can name another user.
//! A connection that breaks the rules is closed; the service and its other connections go
//! on.
//!
//! A client that asks to watch the tasks of its Android ([`HostApiClient::watch_tasks`])
//! gets the tasks there are now, and then the connection carries every change to them, as
//! the compositor needs to keep one window per task.
//!
//! A client that streams input ([`HostApiClient::stream_input`]) sends pointer events on its
//! connection from then on, and the service passes each to the caller's Android as it
//! arrives, without answering, so a pointer that moves fast is never held back.
//!
//! A client that carries the clipboard ([`HostApiClient::watch_clipboard`]) sends the text the
//! user copies on the host, and receives the text the caller's Android copies, on the same
//! connection, unanswered in both directions.
//!
//! A client that installs an APK ([`HostApiClient::install_apk`]) streams its content in pieces
//! after the first answer, and the service streams it on to the caller's Android as it comes,
//! then answers again with how the install ended.
//! A client that watches links ([`HostApiClient::watch_links`]) receives each web link the
//! caller's Android opens, for the host's browser. A client that sends a file
//! ([`HostApiClient::open_file`]) sends its content in pieces after the first answer, and the
//! service answers again once Android has the file.
//!
//! A client that watches notifications ([`HostApiClient::watch_notifications`]) receives the
//! notifications of the caller's Android as they are posted, changed and removed. What the user
//! does with one on the host goes back with [`HostApiClient::act_on_notification`], on any
//! connection.
//!
//! A client that watches widgets ([`HostApiClient::watch_widgets`]) receives the widgets placed
//! on the caller's desktop, each with where its window goes, then every frame and change. The
//! service remembers where each window is ([`HostApiClient::place_widget`]) in a
//! [`WidgetPlacements`] store, and forgets it when the widget is removed.
//!
//! A client that watches a task's accessibility tree ([`HostApiClient::watch_accessibility`])
//! receives the tree and its changes, and sends actions on its nodes, on the same connection.
//! The caller's Android reads the task's tree only while such a connection is open.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::host_api::types::{
    self, envelope::Body, request, response, Envelope, ErrorCode, Request, Response, Role,
};
use dsa_protocol::host_api::{
    client_connect, decode, local_hello, send, service_accept, HandshakeError,
};
use dsa_transport::local::{LocalListener, Peer};
use dsa_transport::{Connection, Connector, TransportError};

use crate::accessibility::{
    AccessibilityWatch, ActionRequest, ActionResult, GuestAccessibility, TreeUpdate,
};
use crate::apk::{self, InstallOutcome, InstalledPackage};
use crate::diagnostics::HostFile;
use crate::files::{self, FileAction, FileTransfer};
use crate::guest_log::{DiagnosticsEvent, LogDumpEvent, LogEntry, LogFilter, LogWatch};
use crate::ime::{ImeCursor, ImeEvent};
use crate::input::{InputEvent, PointerEvent};
use crate::keys::KeyEvent;
use crate::memory::{
    snapshot_from_wire, snapshot_to_wire, GuestMemoryReport, HostMemory, MemorySnapshot,
};
use crate::notifications::{self, Notification, NotificationAction, NotificationEvent};
use crate::resize_trace::{self, detail_requested, ResizeTrace, DETAIL_ENV};
use crate::session::{Session, SessionError, SessionState};
use crate::settings::{Settings, SettingsStore};
use crate::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest, TaskRequestError, TaskWatch};
use crate::widgets::{
    self, MemoryWidgetPlacements, Placement, Widget, WidgetError, WidgetEvent, WidgetFrame,
    WidgetPlacements, WidgetProvider, WidgetSize,
};
use dsa_protocol::control;

/// How long the service waits for Android to say whether a package is installed.
const PACKAGE_QUERY_WAIT: Duration = Duration::from_secs(10);

/// How long the service waits for Android to list its widgets.
const WIDGET_LIST_WAIT: Duration = Duration::from_secs(20);

/// How long the service waits for Android to place a widget, which includes the user
/// setting it up when its app asks them to.
pub const WIDGET_ADD_WAIT: Duration = Duration::from_secs(600);

/// One user's Android, as the service drives it.
pub trait UserAndroid: Send + Sync {
    fn state(&self) -> SessionState;
    /// Why it last stopped, in words a person can act on.
    fn last_reason(&self) -> Option<String>;
    /// What keeps it from being ready, and what to do, while something does.
    fn problem(&self) -> Option<String> {
        None
    }
    fn start(&self);
    fn stop(&self);
    /// News about the Android image for the user, if there is any.
    fn update_notice(&self) -> Option<String> {
        None
    }
    /// The runtime ID of the VM (such as a GUID on Windows), if running.
    fn vm_id(&self) -> Option<String> {
        None
    }
    /// The app tasks of this Android now, and every change to them from now on. `None` when
    /// this Android does not report its tasks.
    fn watch_tasks(&self) -> Option<TaskWatch> {
        None
    }
    /// Asks this Android to close a task or to launch an app.
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        let _ = request;
        Err(TaskRequestError::Failed(
            "this Android does not take requests about its tasks".into(),
        ))
    }
    /// Passes pointer input in a task's window to this Android. Required, with key and text
    /// input, so a wrapper around another Android cannot silently drop input by not
    /// forwarding it.
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError>;
    /// Passes a key pressed or released in a task's window to this Android.
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError>;
    /// Passes text composed or committed in a task's window to this Android.
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError>;
    /// The last report of how this Android uses its memory, if it has sent one since it
    /// started.
    fn guest_memory(&self) -> Option<GuestMemoryReport> {
        None
    }
    /// Asks this Android for a memory report now.
    fn request_guest_memory(&self) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed(
            "this Android does not report its memory".into(),
        ))
    }
    /// Lets this Android be frozen once it has been in Doze a while (`on`), or never, as the
    /// user's freeze setting says.
    fn freeze_when_idle(&self, on: bool) {
        let _ = on;
    }
    /// Freezes this Android until a task window is shown again.
    fn freeze(&self) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed(
            "this Android cannot be frozen".into(),
        ))
    }
    /// Shares the user's clipboard with this Android (`on`) or stops, as the user's clipboard
    /// setting says.
    fn share_clipboard(&self, on: bool);
    /// The user's host clipboard got `text`, for this Android to take while it is shared.
    fn host_clipboard(&self, text: String);
    /// The text this Android copies from now on, while the clipboard is shared. `None` when
    /// this Android does not share its clipboard at all.
    fn watch_clipboard(&self) -> Option<Receiver<String>>;
    /// Lets the web links apps in this Android open go to apps in it (`on`), or to the host,
    /// as the user's setting says.
    fn links_in_android(&self, on: bool) {
        let _ = on;
    }
    /// The http and https links this Android opens from now on, while they go to the host.
    /// `None` when this Android does not send its links.
    fn watch_links(&self) -> Option<Receiver<String>> {
        None
    }
    /// Turns developer mode on (adb reaches this Android through `127.0.0.1:port`) or off,
    /// as the user's setting says. An error says why the port could not be opened.
    fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        if on {
            Err(format!(
                "this Android has no adb bridge, so 127.0.0.1:{port} stays closed"
            ))
        } else {
            Ok(())
        }
    }
    /// Sends a file to this Android, which asks the user which app opens or takes it.
    fn open_file(&self, file: &FileTransfer) -> Result<(), TaskRequestError> {
        let _ = file;
        Err(TaskRequestError::Failed(
            "this Android does not take files".into(),
        ))
    }
    /// The notifications of this Android from now on. `None` when it does not report them.
    fn watch_notifications(&self) -> Option<Receiver<NotificationEvent>> {
        None
    }
    /// The widgets this Android hosts now and every change to them. `None` when it hosts none.
    fn watch_widgets(&self) -> Option<crate::session::WidgetWatch> {
        None
    }
    /// The widgets this Android hosts now, in ID order.
    fn widgets(&self) -> Vec<(i32, WidgetProvider)> {
        Vec::new()
    }
    /// The widgets the apps in this Android offer.
    fn widget_providers(&self) -> Result<Vec<WidgetProvider>, WidgetError> {
        Err(WidgetError::Refused(
            "this Android does not host widgets".into(),
        ))
    }
    /// Places a new widget of `provider` and returns its ID.
    fn add_widget(&self, provider: &str) -> Result<i32, WidgetError> {
        let _ = provider;
        Err(WidgetError::Refused(
            "this Android does not host widgets".into(),
        ))
    }
    /// Removes a widget.
    fn remove_widget(&self, widget_id: i32) -> Result<(), TaskRequestError> {
        let _ = widget_id;
        Err(TaskRequestError::Failed(
            "this Android does not host widgets".into(),
        ))
    }
    /// Tells this Android the size of a widget's window.
    fn resize_widget(&self, widget_id: i32, size: WidgetSize) -> Result<(), TaskRequestError> {
        let _ = (widget_id, size);
        Err(TaskRequestError::Failed(
            "this Android does not host widgets".into(),
        ))
    }
    /// Passes pointer input in a widget's window to this Android.
    fn widget_pointer(&self, pointer: control::WidgetPointer) -> Result<(), TaskRequestError> {
        let _ = pointer;
        Err(TaskRequestError::Failed(
            "this Android does not host widgets".into(),
        ))
    }
    /// Passes what the user did with a notification on the host to this Android.
    fn notification_action(&self, action: NotificationAction) -> Result<(), TaskRequestError> {
        let _ = action;
        Err(TaskRequestError::Failed(
            "this Android does not show its notifications on the host".into(),
        ))
    }
    /// Installs the APK called `name`, of `size` bytes read from `content`, in this Android,
    /// and returns how the install ended. One install runs at a time; a second waits.
    fn install_apk(
        &self,
        name: &str,
        size: u64,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, InstallError> {
        let _ = (name, size, content);
        Err(InstallError::Refused(
            "this Android does not install APKs".into(),
        ))
    }
    /// What this Android has installed of `package`, if anything: its version and signing
    /// certificates.
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        let _ = package;
        Err(TaskRequestError::Failed(
            "this Android cannot say which apps it has installed".into(),
        ))
    }
    /// Watches a task's accessibility tree; Android reads it while any watch of the task
    /// lasts. `None` when this Android does not export its accessibility tree.
    fn watch_accessibility(&self, task_id: i32) -> Option<AccessibilityWatch> {
        let _ = task_id;
        None
    }
    /// Asks this Android to do an action on a node of a task; the answer arrives on the
    /// task's watches.
    fn accessibility_action(
        &self,
        task_id: i32,
        action: &ActionRequest,
    ) -> Result<(), TaskRequestError> {
        let _ = (task_id, action);
        Err(TaskRequestError::Failed(
            "this Android does not export its accessibility tree".into(),
        ))
    }
    /// Reads this Android's log: the records the host holds that pass `filter`, then each new
    /// one. This Android reads its log only while a watch lasts. `None` when it cannot send
    /// its log.
    fn watch_log(&self, filter: LogFilter) -> Option<LogWatch> {
        let _ = filter;
        None
    }
    /// Asks this Android for everything its log holds now that passes `filter`.
    fn dump_log(&self, filter: LogFilter) -> Result<Receiver<LogDumpEvent>, TaskRequestError> {
        let _ = filter;
        Err(TaskRequestError::Failed(
            "this Android cannot send its log".into(),
        ))
    }
    /// The records of this Android's log the host holds now, oldest first.
    fn recent_log(&self) -> Vec<LogEntry> {
        Vec::new()
    }
    /// Asks this Android for its diagnostics.
    fn collect_diagnostics(&self) -> Result<Receiver<DiagnosticsEvent>, TaskRequestError> {
        Err(TaskRequestError::Failed(
            "this Android cannot gather its diagnostics".into(),
        ))
    }
}

/// Why an APK did not reach Android.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallError {
    /// The Android is not running.
    NotRunning(String),
    /// The APK was not taken, or did not arrive whole; the detail says why.
    Refused(String),
}

impl UserAndroid for Session {
    fn state(&self) -> SessionState {
        Session::state(self)
    }
    fn last_reason(&self) -> Option<String> {
        Session::last_reason(self).map(|reason| reason.to_string())
    }
    fn problem(&self) -> Option<String> {
        Session::problem(self).map(|problem| problem.to_string())
    }
    /// On a stopped session, returns once it has moved on, so that what the caller reads
    /// next is the start's progress or its failure, never the state from before it.
    fn start(&self) {
        if Session::state(self) != SessionState::Stopped {
            return Session::start(self);
        }
        let events = self.events();
        Session::start(self);
        let _ = events.recv_timeout(Duration::from_secs(5));
    }
    fn stop(&self) {
        Session::stop(self)
    }
    fn vm_id(&self) -> Option<String> {
        Session::vm_id(self)
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        Some(Session::watch_tasks(self))
    }
    fn watch_log(&self, filter: LogFilter) -> Option<LogWatch> {
        Some(Session::watch_log(self, filter))
    }
    fn dump_log(&self, filter: LogFilter) -> Result<Receiver<LogDumpEvent>, TaskRequestError> {
        Session::dump_log(self, filter).map_err(request_error)
    }
    fn recent_log(&self) -> Vec<LogEntry> {
        Session::recent_log(self)
    }
    fn collect_diagnostics(&self) -> Result<Receiver<DiagnosticsEvent>, TaskRequestError> {
        Session::collect_diagnostics(self).map_err(request_error)
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        Session::request(self, request).map_err(request_error)
    }
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError> {
        Session::pointer(self, event).map_err(request_error)
    }
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError> {
        Session::key(self, event).map_err(request_error)
    }
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError> {
        Session::ime(self, event).map_err(request_error)
    }
    fn freeze(&self) -> Result<(), TaskRequestError> {
        Session::freeze(self).map_err(request_error)
    }
    fn freeze_when_idle(&self, on: bool) {
        Session::freeze_when_idle(self, on)
    }
    fn guest_memory(&self) -> Option<GuestMemoryReport> {
        Session::guest_memory(self)
    }
    fn request_guest_memory(&self) -> Result<(), TaskRequestError> {
        Session::request_guest_memory(self).map_err(request_error)
    }
    fn share_clipboard(&self, on: bool) {
        Session::share_clipboard(self, on)
    }
    fn host_clipboard(&self, text: String) {
        Session::host_clipboard(self, text)
    }
    fn watch_clipboard(&self) -> Option<Receiver<String>> {
        Some(Session::watch_clipboard(self))
    }
    fn links_in_android(&self, on: bool) {
        Session::links_in_android(self, on)
    }
    fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        Session::developer_mode(self, on, port)
    }
    fn watch_links(&self) -> Option<Receiver<String>> {
        Some(Session::watch_links(self))
    }
    fn open_file(&self, file: &FileTransfer) -> Result<(), TaskRequestError> {
        Session::open_file(self, file).map_err(request_error)
    }
    fn watch_notifications(&self) -> Option<Receiver<NotificationEvent>> {
        Some(Session::watch_notifications(self))
    }
    fn notification_action(&self, action: NotificationAction) -> Result<(), TaskRequestError> {
        Session::notification_action(self, action).map_err(request_error)
    }
    fn watch_widgets(&self) -> Option<crate::session::WidgetWatch> {
        Some(Session::watch_widgets(self))
    }
    fn widgets(&self) -> Vec<(i32, WidgetProvider)> {
        Session::widgets(self)
    }
    fn widget_providers(&self) -> Result<Vec<WidgetProvider>, WidgetError> {
        Session::widget_providers(self, WIDGET_LIST_WAIT).map_err(widget_error)
    }
    fn add_widget(&self, provider: &str) -> Result<i32, WidgetError> {
        Session::add_widget(self, provider, WIDGET_ADD_WAIT).map_err(widget_error)
    }
    fn remove_widget(&self, widget_id: i32) -> Result<(), TaskRequestError> {
        Session::remove_widget(self, widget_id).map_err(request_error)
    }
    fn resize_widget(&self, widget_id: i32, size: WidgetSize) -> Result<(), TaskRequestError> {
        Session::resize_widget(self, widget_id, size).map_err(request_error)
    }
    fn widget_pointer(&self, pointer: control::WidgetPointer) -> Result<(), TaskRequestError> {
        Session::widget_pointer(self, pointer).map_err(request_error)
    }
    fn install_apk(
        &self,
        name: &str,
        size: u64,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, InstallError> {
        Session::install_apk(self, name, size, content).map_err(|error| match error {
            SessionError::NotActive(state) => InstallError::NotRunning(format!(
                "Android is {state}; start it and install the APK again"
            )),
            SessionError::InstallRefused(why) => InstallError::Refused(why),
            other => InstallError::Refused(format!("the APK did not reach Android: {other}")),
        })
    }
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        Session::package_state(self, package, PACKAGE_QUERY_WAIT).map_err(request_error)
    }
    fn watch_accessibility(&self, task_id: i32) -> Option<AccessibilityWatch> {
        Some(Session::watch_accessibility(self, task_id))
    }
    fn accessibility_action(
        &self,
        task_id: i32,
        action: &ActionRequest,
    ) -> Result<(), TaskRequestError> {
        Session::accessibility_action(self, task_id, action).map_err(request_error)
    }
}

fn widget_error(error: SessionError) -> WidgetError {
    match error {
        SessionError::WidgetRefused(why) => WidgetError::Refused(why),
        other => match request_error(other) {
            TaskRequestError::NotRunning(why) => WidgetError::NotRunning(why),
            TaskRequestError::Failed(why) => WidgetError::Refused(why),
        },
    }
}

fn request_error(error: SessionError) -> TaskRequestError {
    match error {
        SessionError::NotActive(state @ (SessionState::Starting | SessionState::Stalled)) => {
            TaskRequestError::NotRunning(format!(
                "Android is {state}, not ready yet; `dsa status` says how far it has got"
            ))
        }
        SessionError::NotActive(state) => TaskRequestError::NotRunning(format!(
            "Android is {state}, not running; start it first (dsa start)"
        )),
        other => TaskRequestError::Failed(other.to_string()),
    }
}

/// Finds or creates the Android of a user.
pub trait UserAndroids: Send + Sync {
    /// The Android of `user` (an identity as the transport reports it), created on first
    /// use. An error says why this user cannot have one.
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String>;
    /// The users whose Android is not stopped, with its state.
    fn running(&self) -> Vec<(String, SessionState)>;
    /// Stops every user's Android and waits at most `timeout` for all of them. Returns the
    /// users whose Android had not stopped by then, with its state.
    fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)>;
}

/// The answer to a request to update DSA itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramUpdate {
    /// The version setup is installing; `None` when there is nothing newer.
    pub installing: Option<String>,
    /// What happens next, in words for the user.
    pub detail: String,
}

/// Updates DSA itself when a user asks (`dsa update`).
pub trait UpdateProgram: Send + Sync {
    /// Installs the newest version now. An error says why it cannot.
    fn update_now(&self) -> Result<ProgramUpdate, String>;
}

/// One Android per user, each created by `create` the first time its user asks.
pub struct PerUser<F> {
    create: F,
    androids: Mutex<HashMap<String, Arc<dyn UserAndroid>>>,
}

impl<F> PerUser<F>
where
    F: Fn(&str) -> Result<Arc<dyn UserAndroid>, String> + Send + Sync,
{
    pub fn new(create: F) -> PerUser<F> {
        PerUser {
            create,
            androids: Mutex::new(HashMap::new()),
        }
    }
}

/// The users of `androids` whose Android is not stopped, with its state.
pub fn running_of<'a>(
    androids: impl IntoIterator<Item = (&'a String, &'a Arc<dyn UserAndroid>)>,
) -> Vec<(String, SessionState)> {
    androids
        .into_iter()
        .map(|(user, android)| (user.clone(), android.state()))
        .filter(|(_, state)| *state != SessionState::Stopped)
        .collect()
}

/// Stops every Android of `androids` and waits at most `timeout` for all of them. Returns
/// the users whose Android had not stopped by then, with its state.
pub fn stop_every(
    androids: Vec<(String, Arc<dyn UserAndroid>)>,
    timeout: Duration,
) -> Vec<(String, SessionState)> {
    for (_, android) in &androids {
        android.stop();
    }
    let deadline = Instant::now() + timeout;
    loop {
        let running = running_of(androids.iter().map(|(user, android)| (user, android)));
        if running.is_empty() || Instant::now() >= deadline {
            return running;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

impl<F> PerUser<F> {
    fn snapshot(&self) -> Vec<(String, Arc<dyn UserAndroid>)> {
        self.androids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(user, android)| (user.clone(), Arc::clone(android)))
            .collect()
    }
}

impl<F> UserAndroids for PerUser<F>
where
    F: Fn(&str) -> Result<Arc<dyn UserAndroid>, String> + Send + Sync,
{
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        let mut androids = self.androids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(android) = androids.get(user) {
            return Ok(Arc::clone(android));
        }
        let android = (self.create)(user)?;
        androids.insert(user.to_string(), Arc::clone(&android));
        Ok(android)
    }
    fn running(&self) -> Vec<(String, SessionState)> {
        let androids = self.snapshot();
        running_of(androids.iter().map(|(user, android)| (user, android)))
    }
    fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)> {
        stop_every(self.snapshot(), timeout)
    }
}

fn to_wire(state: SessionState) -> types::SessionState {
    match state {
        SessionState::Stopped => types::SessionState::Stopped,
        SessionState::Starting => types::SessionState::Starting,
        SessionState::Active => types::SessionState::Active,
        SessionState::Dozing => types::SessionState::Dozing,
        SessionState::Frozen => types::SessionState::Frozen,
        SessionState::Stopping => types::SessionState::Stopping,
        SessionState::Stalled => types::SessionState::Stalled,
    }
}

fn task_to_wire(task: Task) -> types::Task {
    types::Task {
        task_id: task.task_id,
        bounds: Some(types::Rect {
            left: task.bounds.left,
            top: task.bounds.top,
            right: task.bounds.right,
            bottom: task.bounds.bottom,
        }),
        package_name: task.package_name,
        label: task.label,
        focused: task.focused,
        density_dpi: task.density_dpi,
        max_width: task.max_width,
        max_height: task.max_height,
        min_width: task.min_width,
        min_height: task.min_height,
    }
}

fn task_from_wire(task: types::Task) -> Task {
    let bounds = task.bounds.unwrap_or_default();
    Task {
        task_id: task.task_id,
        bounds: Bounds {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        },
        package_name: task.package_name,
        label: task.label,
        focused: task.focused,
        density_dpi: task.density_dpi,
        max_width: task.max_width,
        max_height: task.max_height,
        min_width: task.min_width,
        min_height: task.min_height,
    }
}

fn app_to_wire(app: AppInfo) -> types::AppInfo {
    types::AppInfo {
        package_name: app.package_name,
        label: app.label,
        icon_png: app.icon_png,
    }
}

fn app_from_wire(app: types::AppInfo) -> AppInfo {
    AppInfo {
        package_name: app.package_name,
        label: app.label,
        icon_png: app.icon_png,
    }
}

fn event_to_wire(event: TaskEvent) -> types::TaskEvent {
    use types::task_event::Body as Event;
    let body = match event {
        TaskEvent::Updated(task) => Event::Updated(task_to_wire(task)),
        TaskEvent::Removed { task_id } => Event::Removed(types::TaskRemoved { task_id }),
        TaskEvent::Frames {
            task_id,
            display_id,
        } => Event::Frames(types::TaskFrames {
            task_id,
            display_id,
        }),
        TaskEvent::App(app) => Event::App(app_to_wire(app)),
        TaskEvent::Installed(apps) => Event::Installed(installed_to_wire(Some(apps))),
        TaskEvent::ImeCursor { task_id, cursor } => Event::ImeCursor(types::ImeCursor {
            task_id,
            cursor: Some(types::Rect {
                left: cursor.left,
                top: cursor.top,
                right: cursor.right,
                bottom: cursor.bottom,
            }),
        }),
    };
    types::TaskEvent { body: Some(body) }
}

fn event_from_wire(event: types::TaskEvent) -> Option<TaskEvent> {
    use types::task_event::Body as Event;
    Some(match event.body? {
        Event::Updated(task) => TaskEvent::Updated(task_from_wire(task)),
        Event::Removed(removed) => TaskEvent::Removed {
            task_id: removed.task_id,
        },
        Event::Frames(frames) => TaskEvent::Frames {
            task_id: frames.task_id,
            display_id: frames.display_id,
        },
        Event::App(app) => TaskEvent::App(app_from_wire(app)),
        Event::Installed(installed) => TaskEvent::Installed(installed_from_wire(installed)?),
        Event::ImeCursor(cursor) => {
            let rect = cursor.cursor.unwrap_or_default();
            TaskEvent::ImeCursor {
                task_id: cursor.task_id,
                cursor: ImeCursor {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                },
            }
        }
    })
}

/// The tasks of `events`, as a list of tasks, a list of their displays and a list of their
/// apps.
fn tasks_to_wire(events: Vec<TaskEvent>) -> types::Tasks {
    let mut tasks = types::Tasks::default();
    for event in events {
        match event {
            TaskEvent::Updated(task) => tasks.tasks.push(task_to_wire(task)),
            TaskEvent::Frames {
                task_id,
                display_id,
            } => tasks.frames.push(types::TaskFrames {
                task_id,
                display_id,
            }),
            TaskEvent::App(app) => tasks.apps.push(app_to_wire(app)),
            TaskEvent::Installed(apps) => tasks.installed = Some(installed_to_wire(Some(apps))),
            TaskEvent::Removed { .. } | TaskEvent::ImeCursor { .. } => {}
        }
    }
    tasks
}

/// The installed apps on the wire; `None` when the Android has not said which it has.
fn installed_to_wire(apps: Option<Vec<AppInfo>>) -> types::InstalledApps {
    types::InstalledApps {
        known: apps.is_some(),
        apps: apps
            .unwrap_or_default()
            .into_iter()
            .map(app_to_wire)
            .collect(),
    }
}

fn installed_from_wire(installed: types::InstalledApps) -> Option<Vec<AppInfo>> {
    installed
        .known
        .then(|| installed.apps.into_iter().map(app_from_wire).collect())
}

/// The installed apps among `events`, if the Android has said which it has.
fn installed_of(events: Vec<TaskEvent>) -> Option<Vec<AppInfo>> {
    events.into_iter().find_map(|event| match event {
        TaskEvent::Installed(apps) => Some(apps),
        _ => None,
    })
}

fn tasks_from_wire(tasks: types::Tasks) -> TaskList {
    TaskList {
        installed: tasks.installed.and_then(installed_from_wire),
        tasks: tasks.tasks.into_iter().map(task_from_wire).collect(),
        frames: tasks
            .frames
            .into_iter()
            .map(|frames| (frames.task_id, frames.display_id))
            .collect(),
        apps: tasks.apps.into_iter().map(app_from_wire).collect(),
    }
}

/// How often a watching connection looks for its client having gone away while no task
/// changes.
const WATCH_POLL: Duration = Duration::from_millis(100);

/// How long a connection that carries the clipboard waits for the client before it looks
/// for text from Android again.
const CLIPBOARD_POLL: Duration = Duration::from_millis(50);

/// How long the service waits for the next piece of an APK a client sends. A client reads the
/// file as it sends it, so a gap this long means it is stuck or gone.
const APK_CHUNK_WAIT: Duration = Duration::from_secs(30);

/// How long a client waits for the service to say how an install ended: one install waits for
/// another, and the guest may take minutes to verify and compile a large app.
const INSTALL_ANSWER_WAIT: Duration = Duration::from_secs(30 * 60);

/// How long a connection that carries links waits for its client before it looks for links
/// again.
const LINK_POLL: Duration = Duration::from_millis(100);

/// How long a connection that watches an accessibility tree waits for its client's next action
/// before it looks for news from Android again. Short against the second a change may take to
/// reach a host client.
const ACCESSIBILITY_POLL: Duration = Duration::from_millis(30);

/// How long the service waits for the next piece of a file a client sends. A client reads
/// the file before it starts, so a gap this long means it is stuck or gone.
const FILE_CHUNK_WAIT: Duration = Duration::from_secs(30);

/// How long a client waits for the service to say Android has a file: sending 64 MiB into
/// the guest takes seconds, and each chunk may wait half a minute for the agent.
const FILE_ANSWER_WAIT: Duration = Duration::from_secs(5 * 60);

fn from_wire(state: i32) -> Option<SessionState> {
    match types::SessionState::try_from(state).ok()? {
        types::SessionState::Unspecified => None,
        types::SessionState::Stopped => Some(SessionState::Stopped),
        types::SessionState::Starting => Some(SessionState::Starting),
        types::SessionState::Active => Some(SessionState::Active),
        types::SessionState::Dozing => Some(SessionState::Dozing),
        types::SessionState::Frozen => Some(SessionState::Frozen),
        types::SessionState::Stopping => Some(SessionState::Stopping),
        types::SessionState::Stalled => Some(SessionState::Stalled),
    }
}

/// What the service tells a client about itself with the settings, which no client changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServiceFacts {
    /// The version of DSA the service runs.
    pub version: String,
    /// The folder the service writes its log to, if it keeps one.
    pub log_dir: Option<String>,
    /// Whether this installation can update DSA itself.
    pub program_updates_available: bool,
    /// Whether this installation can download new Android images.
    pub image_updates_available: bool,
    /// The version of the Android image the next start boots, if the service knows it.
    pub android_version: Option<String>,
}

/// Says which Android image the next start boots. An image update changes it while the
/// service runs, so the service asks each time a client reads the settings.
pub type AndroidVersionSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The settings as the service reported them, with what it says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsReport {
    pub settings: Settings,
    pub facts: ServiceFacts,
}
/// How long the service waits for the fresh memory report it asks Android for.
const MEMORY_WAIT: Duration = Duration::from_secs(3);

/// The service end of the local API.
pub struct HostApiService {
    androids: Arc<dyn UserAndroids>,
    program: Option<Arc<dyn UpdateProgram>>,
    settings: Option<(Arc<dyn SettingsStore>, ServiceFacts)>,
    android_version: Option<AndroidVersionSource>,
    hello: types::Hello,
    handshake_timeout: Duration,
    /// Logs when each resize was handed to Android, and how long that took.
    resize_trace: Mutex<ResizeTrace>,
    /// Measures the host processes of each user's Android for `dsa memory`.
    host_memory: Option<Arc<dyn HostMemory>>,
    /// How long `dsa memory` waits for the fresh report it asks Android for.
    memory_wait: Duration,
    /// Where each user's widget windows are.
    placements: Arc<dyn WidgetPlacements>,
    /// The host's logs that go in a user's diagnostics, by user.
    host_logs: Option<HostLogs>,
}

/// Names the host's log files that go in a user's diagnostics archive: the service's own and
/// those of the processes in that user's session, never another user's.
pub type HostLogs = Arc<dyn Fn(&str) -> Vec<HostFile> + Send + Sync>;

impl HostApiService {
    pub fn new(androids: Arc<dyn UserAndroids>, build: &str) -> HostApiService {
        HostApiService {
            androids,
            program: None,
            settings: None,
            android_version: None,
            hello: local_hello(Role::Service, build),
            handshake_timeout: Duration::from_secs(5),
            resize_trace: Mutex::new(ResizeTrace::new(
                detail_requested(std::env::var(DETAIL_ENV).ok().as_deref(), false),
                Instant::now(),
            )),
            host_memory: None,
            memory_wait: MEMORY_WAIT,
            placements: Arc::new(MemoryWidgetPlacements::default()),
            host_logs: None,
        }
    }

    /// Puts the host log files `host_logs` names for a user in that user's diagnostics.
    pub fn with_host_logs(mut self, host_logs: HostLogs) -> HostApiService {
        self.host_logs = Some(host_logs);
        self
    }

    /// Keeps where each user's widget windows are in `placements`, which outlives the
    /// service; without it they are kept in memory.
    pub fn with_widget_placements(
        mut self,
        placements: Arc<dyn WidgetPlacements>,
    ) -> HostApiService {
        self.placements = placements;
        self
    }

    /// Measures the host processes of each user's Android with `host_memory`, for
    /// `dsa memory`.
    pub fn with_host_memory(mut self, host_memory: Arc<dyn HostMemory>) -> HostApiService {
        self.host_memory = Some(host_memory);
        self
    }

    /// Waits at most `wait` for the fresh memory report `dsa memory` asks Android for.
    pub fn with_memory_wait(mut self, wait: Duration) -> HostApiService {
        self.memory_wait = wait;
        self
    }

    /// Turns on the per-stage resize trace lines, as when the file that asks for them is
    /// present.
    pub fn with_resize_trace_detail(self, detailed: bool) -> HostApiService {
        if detailed {
            *self.resize_trace.lock().unwrap_or_else(|e| e.into_inner()) =
                ResizeTrace::new(true, Instant::now());
        }
        self
    }

    /// The service that answers `dsa update` with `program`.
    pub fn with_program_updates(mut self, program: Arc<dyn UpdateProgram>) -> HostApiService {
        self.program = Some(program);
        self
    }

    /// The service that keeps the settings in `store` and reports `facts` with them.
    pub fn with_settings(
        mut self,
        store: Arc<dyn SettingsStore>,
        facts: ServiceFacts,
    ) -> HostApiService {
        // The resize trace setting turns on the service's per-stage lines too.
        let detailed = store.machine().resize_trace;
        self.settings = Some((store, facts));
        self.with_resize_trace_detail(detailed)
    }

    /// Reports the Android image version `source` gives with the settings, in place of the
    /// one in the facts.
    pub fn with_android_version(mut self, source: AndroidVersionSource) -> HostApiService {
        self.android_version = Some(source);
        self
    }

    /// The facts as they stand now.
    fn facts_now(&self, facts: &ServiceFacts) -> ServiceFacts {
        let mut facts = facts.clone();
        if let Some(source) = &self.android_version {
            facts.android_version = source().filter(|version| !version.is_empty());
        }
        facts
    }

    /// Accepts connections until `stop` is set, serving each on its own thread. A failed
    /// accept is logged and the loop goes on; only `stop` ends it.
    pub fn serve(self: Arc<Self>, mut listener: Box<dyn LocalListener>, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept(Duration::from_millis(200)) {
                Ok((connection, peer)) => {
                    let service = Arc::clone(&self);
                    let spawned =
                        thread::Builder::new()
                            .name("dsa-host-api".into())
                            .spawn(move || {
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        service.serve_connection(connection, peer);
                                    }));
                            });
                    if let Err(error) = spawned {
                        eprintln!("local API: could not start a connection thread: {error}");
                    }
                }
                Err(TransportError::Timeout) => {}
                Err(error) => {
                    eprintln!("local API: accepting a connection failed: {error}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// Serves one client until it disconnects or breaks the protocol.
    pub fn serve_connection(&self, mut connection: Box<dyn Connection>, peer: Peer) {
        if let Err(error) = service_accept(&mut *connection, &self.hello, self.handshake_timeout) {
            eprintln!("local API: refused a client of {}: {error}", peer.user);
            return;
        }
        loop {
            let frame = match connection.recv(None) {
                Ok(frame) => frame,
                Err(_) => return,
            };
            let request = match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::Request(request)),
                }) => request,
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than a request; \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame ({error}); \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            };
            if let Some(request::Body::WatchTasks(_)) = request.body {
                return self.watch_tasks(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchLog(watch)) = &request.body {
                let android = self.androids.android(&peer.user);
                return crate::guest_log_api::serve_watch_log(
                    connection, android, request.id, watch,
                );
            }
            if let Some(request::Body::CollectDiagnostics(_)) = request.body {
                let android = self.androids.android(&peer.user);
                let host_files = self
                    .host_logs
                    .as_ref()
                    .map(|files| files(&peer.user))
                    .unwrap_or_default();
                return crate::guest_log_api::serve_collect_diagnostics(
                    connection,
                    android,
                    request.id,
                    &host_files,
                    &self.hello.build,
                );
            }
            if let Some(request::Body::StreamInput(_)) = request.body {
                return self.stream_input(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchClipboard(_)) = request.body {
                return self.watch_clipboard(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchNotifications(_)) = request.body {
                return self.watch_notifications(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchWidgets(_)) = request.body {
                return self.watch_widgets(connection, &peer, request.id);
            }
            if let Some(request::Body::InstallApk(install)) = &request.body {
                return self.install_apk(connection, &peer, request.id, install.clone());
            }
            if let Some(request::Body::WatchLinks(_)) = request.body {
                return self.watch_links(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchAccessibility(watch)) = &request.body {
                return self.watch_accessibility(connection, &peer, request.id, watch.task_id);
            }
            if let Some(request::Body::OpenFile(open)) = &request.body {
                return self.open_file(connection, &peer, request.id, open.clone());
            }
            let response = self.answer(&peer, request);
            if send(&mut *connection, Body::Response(response)).is_err() {
                return;
            }
        }
    }

    /// Answers a WatchTasks request with the tasks there are now, then sends every change
    /// until the client goes away.
    fn watch_tasks(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watch = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => android.watch_tasks().ok_or_else(|| {
                error(
                    ErrorCode::Unavailable,
                    "this Android does not report its tasks".into(),
                )
            }),
        };
        let watch = match watch {
            Ok(watch) => watch,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Tasks(tasks_to_wire(watch.current))),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            match watch.events.recv_timeout(WATCH_POLL) {
                Ok(event) => {
                    let pending = std::iter::once(event).chain(watch.events.try_iter());
                    for event in pending {
                        let frame = Body::TaskEvent(event_to_wire(event));
                        if send(&mut *connection, frame).is_err() {
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    connection.close();
                    return;
                }
            }
            match connection.recv(Some(Duration::from_millis(1))) {
                Err(TransportError::Timeout) => {}
                Err(_) => return,
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent a frame on a connection that watches \
                         tasks; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            }
        }
    }

    /// Answers a StreamInput request, then passes every pointer, key and text event the client sends to
    /// its Android until the client goes away. Input to an Android that is not running is
    /// dropped: the window that sends it can do nothing better with it.
    /// Answers a WatchAccessibility request, then sends the task's tree and the answers to the
    /// client's actions as they come from the caller's Android, and passes each action the
    /// client sends to it, until the client goes away. Closing the connection ends the watch,
    /// so Android stops reading the task's tree once no client watches it. A client that sends
    /// anything but an action is disconnected.
    fn watch_accessibility(
        &self,
        mut connection: Box<dyn Connection>,
        peer: &Peer,
        id: u64,
        task_id: i32,
    ) {
        let watch = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => match android.watch_accessibility(task_id) {
                Some(watch) => Ok((android, watch)),
                None => Err(error(
                    ErrorCode::Unavailable,
                    "this Android does not export its accessibility tree".into(),
                )),
            },
        };
        let (android, watch) = match watch {
            Ok(watch) => watch,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            loop {
                let frame = match watch.updates.try_recv() {
                    Ok(GuestAccessibility::Tree(tree)) => {
                        Body::AccessibilityTree(tree.to_host_api())
                    }
                    Ok(GuestAccessibility::ActionResult(result)) => {
                        Body::AccessibilityActionResult(result.to_host_api())
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connection.close();
                        return;
                    }
                };
                if send(&mut *connection, frame).is_err() {
                    return;
                }
            }
            let frame = match connection.recv(Some(ACCESSIBILITY_POLL)) {
                Err(TransportError::Timeout) => continue,
                Err(_) => return,
                Ok(frame) => frame,
            };
            let action = match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::AccessibilityAction(action)),
                }) => ActionRequest::from_host_api(action),
                _ => Err("something other than an action arrived".to_string()),
            };
            match action {
                Ok(action) => {
                    let request_id = action.request_id;
                    if let Err(error) = android.accessibility_action(task_id, &action) {
                        let why = match error {
                            TaskRequestError::NotRunning(why) | TaskRequestError::Failed(why) => {
                                why
                            }
                        };
                        let result = ActionResult {
                            request_id,
                            outcome: Err(why),
                        };
                        if send(
                            &mut *connection,
                            Body::AccessibilityActionResult(result.to_host_api()),
                        )
                        .is_err()
                        {
                            return;
                        }
                    }
                }
                Err(why) => {
                    eprintln!(
                        "local API: a client of {} broke the accessibility watch of task \
                         {task_id} ({why}); closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            }
        }
    }

    /// Answers a WatchLinks request, then sends each web link the caller's Android opens until
    /// the client goes away. A client that sends anything on this connection is disconnected.
    fn watch_links(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watch = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => android.watch_links().ok_or_else(|| {
                error(
                    ErrorCode::Unavailable,
                    "this Android does not send its links".into(),
                )
            }),
        };
        let links = match watch {
            Ok(links) => links,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            loop {
                match links.try_recv() {
                    Ok(url) => {
                        if send(&mut *connection, Body::OpenLink(types::OpenLink { url })).is_err()
                        {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connection.close();
                        return;
                    }
                }
            }
            match connection.recv(Some(LINK_POLL)) {
                Err(TransportError::Timeout) => {}
                Err(_) => return,
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent a frame on a connection that carries \
                         links; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            }
        }
    }

    /// Answers an OpenFile request: refuses a file that is too large or badly named, else
    /// takes its content in FileChunk frames, sends it to the caller's Android and answers
    /// again with the outcome. The connection ends with the transfer.
    fn open_file(
        &self,
        mut connection: Box<dyn Connection>,
        peer: &Peer,
        id: u64,
        open: types::OpenFile,
    ) {
        let answer = |connection: &mut dyn Connection, body: response::Body| {
            let _ = send(
                connection,
                Body::Response(Response {
                    id,
                    body: Some(body),
                }),
            );
        };
        let action = match types::FileAction::try_from(open.action) {
            Ok(types::FileAction::View) => FileAction::View,
            Ok(types::FileAction::Send) => FileAction::Send,
            _ => {
                answer(
                    &mut *connection,
                    error(
                        ErrorCode::FileRefused,
                        "the request says neither to open nor to share the file".into(),
                    ),
                );
                connection.close();
                return;
            }
        };
        let checked = files::check_name(&open.name).and_then(|()| files::check_size(open.size));
        let android = match (checked, self.androids.android(&peer.user)) {
            (Err(why), _) => {
                eprintln!("files: refused a file from {}: {why}", peer.user);
                answer(&mut *connection, error(ErrorCode::FileRefused, why));
                connection.close();
                return;
            }
            (_, Err(why)) => {
                answer(&mut *connection, error(ErrorCode::Unavailable, why));
                connection.close();
                return;
            }
            (Ok(()), Ok(android)) => android,
        };
        answer(&mut *connection, response::Body::Done(types::Done {}));
        let mut content = Vec::with_capacity(open.size as usize);
        while (content.len() as u64) < open.size {
            let refuse = |why: String| error(ErrorCode::FileRefused, why);
            let frame = match connection.recv(Some(FILE_CHUNK_WAIT)) {
                Ok(frame) => frame,
                Err(error) => {
                    eprintln!(
                        "files: the file from {} did not arrive whole ({error}); dropping it",
                        peer.user
                    );
                    answer(
                        &mut *connection,
                        refuse(format!("the file did not arrive whole: {error}")),
                    );
                    connection.close();
                    return;
                }
            };
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::FileChunk(chunk)),
                }) if content.len() as u64 + chunk.data.len() as u64 <= open.size => {
                    content.extend_from_slice(&chunk.data);
                }
                Ok(Envelope {
                    body: Some(Body::FileChunk(_)),
                }) => {
                    eprintln!(
                        "files: a client of {} sent more than the {} bytes it announced; \
                         dropping the file",
                        peer.user, open.size
                    );
                    answer(
                        &mut *connection,
                        refuse(format!(
                            "more than the {} bytes announced arrived",
                            open.size
                        )),
                    );
                    connection.close();
                    return;
                }
                Ok(_) | Err(_) => {
                    eprintln!(
                        "files: a client of {} sent something other than a piece of its file; \
                         dropping the file and closing its connection",
                        peer.user
                    );
                    answer(
                        &mut *connection,
                        refuse("something other than a piece of the file arrived".into()),
                    );
                    connection.close();
                    return;
                }
            }
        }
        let file = FileTransfer {
            name: open.name,
            content,
            action,
        };
        eprintln!(
            "files: {} asked to {} {} ({} bytes) in Android",
            peer.user,
            file.action,
            file.name,
            file.content.len()
        );
        let body = match android.open_file(&file) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => {
                eprintln!("files: {} was not opened in Android: {why}", file.name);
                error(ErrorCode::FileRefused, why)
            }
        };
        answer(&mut *connection, body);
        connection.close();
    }

    fn stream_input(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(error(ErrorCode::Unavailable, why)),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        // Said once per run of failures, not once per event.
        let mut failing = false;
        loop {
            let frame = match connection.recv(None) {
                Ok(frame) => frame,
                Err(_) => return,
            };
            let event = match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::PointerEvent(event)),
                }) => PointerEvent::from_wire(&event)
                    .map(InputEvent::Pointer)
                    .map_err(|why| format!("dropped a pointer event: {why}")),
                Ok(Envelope {
                    body: Some(Body::KeyEvent(event)),
                }) => KeyEvent::from_wire(&event)
                    .map(InputEvent::Key)
                    .map_err(|why| format!("dropped a key event: {why}")),
                Ok(Envelope {
                    body: Some(Body::ImeComposition(composition)),
                }) => ImeEvent::from_wire_composition(&composition)
                    .map(InputEvent::Ime)
                    .map_err(|why| format!("dropped a composition: {why}")),
                Ok(Envelope {
                    body: Some(Body::ImeCommit(commit)),
                }) => ImeEvent::from_wire_commit(&commit)
                    .map(InputEvent::Ime)
                    .map_err(|why| format!("dropped committed text: {why}")),
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than input on a \
                         connection that streams input; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame ({error}); \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            };
            let outcome = event.and_then(|event| match event {
                InputEvent::Pointer(event) => android
                    .pointer(&event)
                    .map_err(|why| format!("pointer input did not reach Android: {why}")),
                InputEvent::Key(event) => android
                    .key(&event)
                    .map_err(|why| format!("key input did not reach Android: {why}")),
                InputEvent::Ime(event) => android
                    .ime(&event)
                    .map_err(|why| format!("text input did not reach Android: {why}")),
            });
            match outcome {
                Ok(()) => failing = false,
                Err(why) => {
                    if !failing {
                        eprintln!("local API: {}: {why}", peer.user);
                    }
                    failing = true;
                }
            }
        }
    }

    /// Answers a WatchClipboard request, then passes the text the client copies to its
    /// Android, and the text its Android copies to the client, until the client goes away.
    /// What the client sends that is not clipboard text closes the connection; a frame that
    /// cannot be read is dropped.
    fn watch_clipboard(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => match android.watch_clipboard() {
                Some(watch) => Ok((android, watch)),
                None => Err(error(
                    ErrorCode::Unavailable,
                    "this Android does not share its clipboard".into(),
                )),
            },
        };
        let (android, watch) = match watched {
            Ok(watched) => watched,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            loop {
                match watch.try_recv() {
                    Ok(text) => {
                        let frame = Body::ClipboardText(types::ClipboardText { text });
                        if send(&mut *connection, frame).is_err() {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connection.close();
                        return;
                    }
                }
            }
            let frame = match connection.recv(Some(CLIPBOARD_POLL)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(_) => return,
            };
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::ClipboardText(clip)),
                }) => android.host_clipboard(clip.text),
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than clipboard text on \
                         a connection that carries the clipboard; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame on a connection \
                         that carries the clipboard ({error}); dropping it",
                        peer.user
                    );
                }
            }
        }
    }

    /// Answers a WatchNotifications request, then passes the notifications of the client's
    /// Android to it until the client goes away. The client sends nothing more: anything it
    /// sends closes the connection, except a frame that cannot be read, which is dropped.
    fn watch_notifications(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => android.watch_notifications().ok_or_else(|| {
                error(
                    ErrorCode::Unavailable,
                    "this Android does not show its notifications on the host".into(),
                )
            }),
        };
        let watch = match watched {
            Ok(watch) => watch,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            loop {
                match watch.try_recv() {
                    Ok(event) => {
                        if send(&mut *connection, notification_to_wire(event)).is_err() {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connection.close();
                        return;
                    }
                }
            }
            let frame = match connection.recv(Some(CLIPBOARD_POLL)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(_) => return,
            };
            match decode(&frame) {
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something on a connection that watches \
                         notifications; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame on a connection \
                         that watches notifications ({error}); dropping it",
                        peer.user
                    );
                }
            }
        }
    }

    /// The placements the service keeps for `user`. A broken store is logged and read as
    /// empty, so widgets still show, at default places.
    fn placements_of(&self, user: &str) -> std::collections::BTreeMap<i32, Placement> {
        self.placements.all(user).unwrap_or_else(|why| {
            eprintln!("widgets: {why}; placing the widgets of {user} afresh");
            Default::default()
        })
    }

    /// The widget with its placement: the one kept for it, else a new one, which is kept.
    fn placed(
        &self,
        user: &str,
        widget_id: i32,
        provider: WidgetProvider,
        kept: &mut std::collections::BTreeMap<i32, Placement>,
    ) -> Widget {
        let placement = match kept.get(&widget_id) {
            Some(placement) => *placement,
            None => {
                let others: Vec<Placement> = kept.values().copied().collect();
                let placement = widgets::default_placement(&provider, &others);
                if let Err(why) = self.placements.set(user, widget_id, placement) {
                    eprintln!("widgets: the place of widget {widget_id} is not kept: {why}");
                }
                kept.insert(widget_id, placement);
                placement
            }
        };
        Widget {
            widget_id,
            provider,
            placement,
        }
    }

    /// The widgets of the caller's Android with their placements.
    fn list_widgets(&self, peer: &Peer) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        let mut kept = self.placements_of(&peer.user);
        let remembered = kept.len() as u32;
        let widgets = android
            .widgets()
            .into_iter()
            .map(|(id, provider)| widget_to_wire(self.placed(&peer.user, id, provider, &mut kept)))
            .collect();
        response::Body::Widgets(types::Widgets {
            widgets,
            remembered,
        })
    }

    /// Answers a WatchWidgets request with the widgets there are now, then passes every change
    /// to them until the client goes away. The client sends nothing more.
    fn watch_widgets(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => android.watch_widgets().ok_or_else(|| {
                error(
                    ErrorCode::Unavailable,
                    "this Android does not host widgets".into(),
                )
            }),
        };
        let watch = match watched {
            Ok(watch) => watch,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let mut kept = self.placements_of(&peer.user);
        let remembered = kept.len() as u32;
        let mut current = Vec::new();
        let mut frames = Vec::new();
        for event in watch.current {
            match event {
                WidgetEvent::Placed(widget) => current.push(widget_to_wire(self.placed(
                    &peer.user,
                    widget.widget_id,
                    widget.provider,
                    &mut kept,
                ))),
                WidgetEvent::Frame(frame) => frames.push(frame),
                WidgetEvent::Removed { .. } | WidgetEvent::Listed(_) => {}
            }
        }
        let answer = Response {
            id,
            body: Some(response::Body::Widgets(types::Widgets {
                widgets: current,
                remembered,
            })),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        for frame in frames {
            if send(&mut *connection, frame_to_wire(frame)).is_err() {
                return;
            }
        }
        loop {
            loop {
                let event = match watch.events.try_recv() {
                    Ok(event) => event,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        connection.close();
                        return;
                    }
                };
                let body = match event {
                    WidgetEvent::Placed(widget) => {
                        let mut kept = self.placements_of(&peer.user);
                        Body::WidgetEvent(types::WidgetEvent {
                            body: Some(types::widget_event::Body::Placed(widget_to_wire(
                                self.placed(
                                    &peer.user,
                                    widget.widget_id,
                                    widget.provider,
                                    &mut kept,
                                ),
                            ))),
                        })
                    }
                    WidgetEvent::Frame(frame) => frame_to_wire(frame),
                    WidgetEvent::Removed { widget_id } => {
                        if let Err(why) = self.placements.remove(&peer.user, widget_id) {
                            eprintln!("widgets: the place of removed widget {widget_id} stays kept: {why}");
                        }
                        Body::WidgetEvent(types::WidgetEvent {
                            body: Some(types::widget_event::Body::Removed(types::WidgetRemoved {
                                widget_id,
                            })),
                        })
                    }
                    WidgetEvent::Listed(ids) => {
                        for widget_id in self.placements_of(&peer.user).into_keys() {
                            if !ids.contains(&widget_id) {
                                let _ = self.placements.remove(&peer.user, widget_id);
                            }
                        }
                        continue;
                    }
                };
                if send(&mut *connection, body).is_err() {
                    return;
                }
            }
            let frame = match connection.recv(Some(CLIPBOARD_POLL)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(_) => return,
            };
            match decode(&frame) {
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something on a connection that watches \
                         widgets; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => eprintln!(
                    "local API: a client of {} sent an unreadable frame on a connection that \
                     watches widgets ({error}); dropping it",
                    peer.user
                ),
            }
        }
    }

    fn package_state(&self, peer: &Peer, package: &str) -> response::Body {
        if !apk::valid_package_name(package) {
            return error(
                ErrorCode::UnknownRequest,
                format!(
                    "{:?} is not a package name, such as com.example.notes",
                    package.chars().take(80).collect::<String>()
                ),
            );
        }
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.package_state(package) {
            Ok(installed) => {
                response::Body::PackageState(InstalledPackage::to_wire(installed.as_ref()))
            }
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn widget_providers(&self, peer: &Peer) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.widget_providers() {
            Ok(providers) => response::Body::WidgetProviders(types::WidgetProviders {
                providers: providers.iter().map(provider_to_wire).collect(),
            }),
            Err(WidgetError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(WidgetError::Refused(why)) => error(ErrorCode::WidgetRefused, why),
        }
    }

    fn add_widget(&self, peer: &Peer, add: types::AddWidget) -> response::Body {
        if !widgets::valid_provider_name(&add.provider) {
            return error(
                ErrorCode::UnknownRequest,
                format!(
                    "{:?} is not a widget's name; list the widgets to see them",
                    add.provider.chars().take(80).collect::<String>()
                ),
            );
        }
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        let widget_id = match android.add_widget(&add.provider) {
            Ok(widget_id) => widget_id,
            Err(WidgetError::NotRunning(why)) => return error(ErrorCode::NotRunning, why),
            Err(WidgetError::Refused(why)) => return error(ErrorCode::WidgetRefused, why),
        };
        let provider = android
            .widgets()
            .into_iter()
            .find(|(id, _)| *id == widget_id)
            .map(|(_, provider)| provider)
            .unwrap_or_else(|| WidgetProvider {
                provider: add.provider.clone(),
                ..WidgetProvider::default()
            });
        let mut kept = self.placements_of(&peer.user);
        eprintln!(
            "widgets: {} placed widget {widget_id} ({})",
            peer.user, add.provider
        );
        response::Body::Widget(widget_to_wire(
            self.placed(&peer.user, widget_id, provider, &mut kept),
        ))
    }

    fn remove_widget(&self, peer: &Peer, widget_id: i32) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        let known = android.widgets().iter().any(|(id, _)| *id == widget_id);
        let kept = self.placements_of(&peer.user).contains_key(&widget_id);
        if !known && !kept {
            return error(
                ErrorCode::UnknownRequest,
                format!("there is no widget {widget_id}"),
            );
        }
        if let Err(why) = self.placements.remove(&peer.user, widget_id) {
            eprintln!("widgets: the place of removed widget {widget_id} stays kept: {why}");
        }
        if !known {
            return response::Body::Done(types::Done {});
        }
        match android.remove_widget(widget_id) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn place_widget(&self, peer: &Peer, place: types::PlaceWidget) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        let Some(provider) = android
            .widgets()
            .into_iter()
            .find(|(id, _)| *id == place.widget_id)
            .map(|(_, provider)| provider)
        else {
            return error(
                ErrorCode::UnknownRequest,
                format!("there is no widget {}", place.widget_id),
            );
        };
        let asked = place.placement.unwrap_or_default();
        let (width_dp, height_dp) = widgets::clamp_size(&provider, asked.width_dp, asked.height_dp);
        let placement = Placement {
            x: asked.x,
            y: asked.y,
            width_dp,
            height_dp,
        };
        if let Err(why) = self.placements.set(&peer.user, place.widget_id, placement) {
            eprintln!(
                "widgets: the place of widget {} is not kept: {why}",
                place.widget_id
            );
        }
        if place.width == 0 || place.height == 0 {
            return response::Body::Done(types::Done {});
        }
        let size = WidgetSize::of_pixels(place.width, place.height, place.density_dpi);
        match android.resize_widget(place.widget_id, size) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn widget_pointer(&self, peer: &Peer, pointer: types::WidgetPointer) -> response::Body {
        if !pointer.x.is_finite()
            || !pointer.y.is_finite()
            || pointer.x.abs() > widgets::MAX_SIDE as f32 * 2.0
            || pointer.y.abs() > widgets::MAX_SIDE as f32 * 2.0
        {
            return error(
                ErrorCode::UnknownRequest,
                "a widget's pointer position is outside its window".into(),
            );
        }
        let action = match types::PointerAction::try_from(pointer.action) {
            Ok(types::PointerAction::Down) => control::PointerAction::Down,
            Ok(types::PointerAction::Move) => control::PointerAction::Move,
            Ok(types::PointerAction::Up) => control::PointerAction::Up,
            Ok(types::PointerAction::Cancel) => control::PointerAction::Cancel,
            _ => {
                return error(
                    ErrorCode::UnknownRequest,
                    "a widget takes presses, moves, releases and cancels".into(),
                )
            }
        };
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        if !android
            .widgets()
            .iter()
            .any(|(id, _)| *id == pointer.widget_id)
        {
            return error(
                ErrorCode::UnknownRequest,
                format!("there is no widget {}", pointer.widget_id),
            );
        }
        match android.widget_pointer(control::WidgetPointer {
            widget_id: pointer.widget_id,
            action: action as i32,
            pointer_id: pointer.pointer_id.min(31),
            x: pointer.x,
            y: pointer.y,
        }) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Passes what the user did with a notification to the caller's Android.
    fn act_on_notification(&self, peer: &Peer, act: types::ActOnNotification) -> response::Body {
        if !notifications::valid_key(&act.key) {
            return error(
                ErrorCode::UnknownRequest,
                format!(
                    "a notification key is 1 to {} bytes, not {}",
                    notifications::MAX_KEY_BYTES,
                    act.key.len()
                ),
            );
        }
        let action = match types::NotificationAction::try_from(act.action) {
            Ok(types::NotificationAction::Open) => NotificationAction::Open { key: act.key },
            Ok(types::NotificationAction::Dismiss) => NotificationAction::Dismiss { key: act.key },
            _ => {
                return error(
                    ErrorCode::UnknownRequest,
                    "the action on a notification is to open it or to dismiss it".into(),
                )
            }
        };
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.notification_action(action) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Answers an InstallApk request: refuses an APK that is too large or not named as one,
    /// else streams the ApkChunk frames that follow to the caller's Android and answers again
    /// with how the install ended. The connection ends with the install.
    fn install_apk(
        &self,
        mut connection: Box<dyn Connection>,
        peer: &Peer,
        id: u64,
        install: types::InstallApk,
    ) {
        let answer = |connection: &mut dyn Connection, body: response::Body| {
            let _ = send(
                connection,
                Body::Response(Response {
                    id,
                    body: Some(body),
                }),
            );
        };
        let checked = apk::check_name(&install.name).and_then(|()| apk::check_size(install.size));
        let android = match (checked, self.androids.android(&peer.user)) {
            (Err(why), _) => {
                eprintln!("install: refused an APK from {}: {why}", peer.user);
                answer(&mut *connection, error(ErrorCode::InstallRefused, why));
                connection.close();
                return;
            }
            (_, Err(why)) => {
                answer(&mut *connection, error(ErrorCode::Unavailable, why));
                connection.close();
                return;
            }
            (Ok(()), Ok(android)) => android,
        };
        let state = android.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            answer(
                &mut *connection,
                error(
                    ErrorCode::NotRunning,
                    format!("Android is {state}; start it and install the APK again"),
                ),
            );
            connection.close();
            return;
        }
        answer(&mut *connection, response::Body::Done(types::Done {}));
        eprintln!(
            "install: {} asked to install {} ({} bytes)",
            peer.user, install.name, install.size
        );
        let mut content = ApkChunks {
            connection: &mut *connection,
            user: &peer.user,
            left: install.size,
            piece: Vec::new(),
            at: 0,
        };
        let body = match android.install_apk(&install.name, install.size, &mut content) {
            Ok(outcome) => response::Body::InstallResult(outcome.to_wire()),
            Err(InstallError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(InstallError::Refused(why)) => error(ErrorCode::InstallRefused, why),
        };
        answer(&mut *connection, body);
        connection.close();
    }

    fn answer(&self, peer: &Peer, request: Request) -> Response {
        let body = match request.body {
            None => error(
                ErrorCode::UnknownRequest,
                "the request is empty or of a kind this service does not know; \
                 update the service"
                    .into(),
            ),
            Some(request::Body::UpdateProgram(_)) => self.update_program(peer),
            Some(request::Body::GetSettings(_)) => self.get_settings(peer),
            Some(request::Body::SetSettings(set)) => self.set_settings(peer, set),
            Some(request::Body::GetMemory(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => {
                    let memory = self.memory_of(&peer.user, &*android);
                    response::Body::Memory(snapshot_to_wire(&memory))
                }
            },
            Some(request::Body::GetVmId(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => response::Body::VmId(types::VmId {
                    vm_id: android.vm_id().unwrap_or_default(),
                }),
            },
            Some(request::Body::ListTasks(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => response::Body::Tasks(tasks_to_wire(
                    android
                        .watch_tasks()
                        .map(|watch| watch.current)
                        .unwrap_or_default(),
                )),
            },
            Some(request::Body::ListApps(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => response::Body::InstalledApps(installed_to_wire(
                    android
                        .watch_tasks()
                        .and_then(|watch| installed_of(watch.current)),
                )),
            },
            Some(request::Body::CloseTask(close)) => self.request(
                peer,
                TaskRequest::Close {
                    task_id: close.task_id,
                },
            ),
            Some(request::Body::LaunchApp(launch)) => self.request(
                peer,
                TaskRequest::Launch {
                    package_name: launch.package_name,
                },
            ),
            Some(request::Body::StopApp(stop)) => self.request(
                peer,
                TaskRequest::StopApp {
                    package_name: stop.package_name,
                },
            ),
            Some(request::Body::ResizeTask(resize)) => {
                let started = Instant::now();
                let answer = self.request(
                    peer,
                    TaskRequest::Resize {
                        task_id: resize.task_id,
                        width: resize.width,
                        height: resize.height,
                        live: resize.live,
                    },
                );
                self.trace_resize(&resize, started);
                answer
            }
            Some(request::Body::SetTaskDensity(density)) => self.request(
                peer,
                TaskRequest::SetDensity {
                    task_id: density.task_id,
                    density_dpi: density.density_dpi,
                },
            ),
            Some(request::Body::SetWindowsShown(shown)) => {
                self.request(peer, TaskRequest::WindowsShown { shown: shown.shown })
            }
            Some(request::Body::ActOnNotification(act)) => self.act_on_notification(peer, act),
            Some(request::Body::ListWidgetProviders(_)) => self.widget_providers(peer),
            Some(request::Body::GetPackage(get)) => self.package_state(peer, &get.package_name),
            Some(request::Body::AddWidget(add)) => self.add_widget(peer, add),
            Some(request::Body::RemoveWidget(remove)) => self.remove_widget(peer, remove.widget_id),
            Some(request::Body::ListWidgets(_)) => self.list_widgets(peer),
            Some(request::Body::PlaceWidget(place)) => self.place_widget(peer, place),
            Some(request::Body::WidgetPointer(pointer)) => self.widget_pointer(peer, pointer),
            Some(request::Body::FreezeSession(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => match android.freeze() {
                    Ok(()) => status(android.as_ref()),
                    Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
                    Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
                },
            },
            Some(request::Body::WatchLog(_)) | Some(request::Body::CollectDiagnostics(_)) => error(
                ErrorCode::UnknownRequest,
                "the log and the diagnostics are read on a connection of their own".into(),
            ),
            Some(body) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => {
                    match body {
                        request::Body::GetStatus(_) => {}
                        request::Body::StartSession(_) => android.start(),
                        request::Body::StopSession(_) => android.stop(),
                        request::Body::UpdateProgram(_)
                        | request::Body::GetVmId(_)
                        | request::Body::ListTasks(_)
                        | request::Body::ListApps(_)
                        | request::Body::WatchTasks(_)
                        | request::Body::CloseTask(_)
                        | request::Body::LaunchApp(_)
                        | request::Body::ResizeTask(_)
                        | request::Body::SetTaskDensity(_)
                        | request::Body::SetWindowsShown(_)
                        | request::Body::FreezeSession(_)
                        | request::Body::GetSettings(_)
                        | request::Body::SetSettings(_)
                        | request::Body::GetMemory(_)
                        | request::Body::StreamInput(_)
                        | request::Body::WatchClipboard(_)
                        | request::Body::InstallApk(_)
                        | request::Body::WatchLinks(_)
                        | request::Body::OpenFile(_)
                        | request::Body::WatchNotifications(_)
                        | request::Body::ActOnNotification(_)
                        | request::Body::ListWidgetProviders(_)
                        | request::Body::GetPackage(_)
                        | request::Body::AddWidget(_)
                        | request::Body::RemoveWidget(_)
                        | request::Body::ListWidgets(_)
                        | request::Body::WatchWidgets(_)
                        | request::Body::PlaceWidget(_)
                        | request::Body::WidgetPointer(_)
                        | request::Body::WatchAccessibility(_)
                        | request::Body::WatchLog(_)
                        | request::Body::CollectDiagnostics(_)
                        | request::Body::StopApp(_) => unreachable!("answered above"),
                    }
                    status(android.as_ref())
                }
            },
        };
        Response {
            id: request.id,
            body: Some(body),
        }
    }
}

impl HostApiService {
    /// Logs that the resize went to Android: each size when detail is on, and otherwise the
    /// end of a drag, at most a few times a second.
    fn trace_resize(&self, resize: &types::ResizeTask, started: Instant) {
        let now = Instant::now();
        let size = (resize.width, resize.height);
        let detail = format!(
            "live={} took={}",
            resize.live,
            resize_trace::millis(now.saturating_duration_since(started))
        );
        let wall = resize_trace::wall_now();
        let mut trace = self.resize_trace.lock().unwrap_or_else(|e| e.into_inner());
        let line = if trace.detailed() {
            trace.stage(
                resize.task_id,
                "service.forwarded",
                size,
                &detail,
                now,
                wall,
            )
        } else if !resize.live {
            trace.summary(
                resize.task_id,
                "service.forwarded",
                size,
                &detail,
                now,
                wall,
            )
        } else {
            None
        };
        if let Some(line) = line {
            eprintln!("local API: {line}");
        }
    }

    fn request(&self, peer: &Peer, request: TaskRequest) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.request(request) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    /// The memory of `user`'s Android now. A running Android is asked for a fresh report,
    /// which is waited for a moment; a slow one leaves the last report in its place.
    pub fn memory_of(&self, user: &str, android: &dyn UserAndroid) -> MemorySnapshot {
        let before = android.guest_memory().map(|report| report.age);
        if android.request_guest_memory().is_ok() {
            let deadline = Instant::now() + self.memory_wait;
            while Instant::now() < deadline {
                let now = android.guest_memory().map(|report| report.age);
                // A report younger than the one before arrived since the request.
                if now.is_some_and(|age| before.is_none_or(|before| age < before)) {
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        let vm_id = android.vm_id();
        MemorySnapshot {
            guest: android.guest_memory(),
            host: self
                .host_memory
                .as_ref()
                .map(|host| host.processes(user, vm_id.as_deref()))
                .unwrap_or_default(),
        }
    }

    fn update_program(&self, peer: &Peer) -> response::Body {
        let Some(program) = &self.program else {
            return error(
                ErrorCode::UpdateFailed,
                "this service does not update DSA; install the new version by hand".into(),
            );
        };
        eprintln!("local API: {} asked to update DSA now", peer.user);
        match program.update_now() {
            Ok(update) => response::Body::ProgramUpdate(types::ProgramUpdate {
                installing: update.installing.unwrap_or_default(),
                detail: update.detail,
            }),
            Err(why) => error(ErrorCode::UpdateFailed, why),
        }
    }
}

impl HostApiService {
    fn no_settings() -> response::Body {
        error(
            ErrorCode::Unavailable,
            "this service keeps no settings; update the service".into(),
        )
    }

    fn get_settings(&self, peer: &Peer) -> response::Body {
        let Some((store, facts)) = &self.settings else {
            return Self::no_settings();
        };
        match store.get(&peer.user) {
            Ok(settings) => {
                response::Body::Settings(settings_to_wire(&settings, &self.facts_now(facts)))
            }
            Err(why) => error(ErrorCode::Unavailable, why),
        }
    }

    fn set_settings(&self, peer: &Peer, set: types::SetSettings) -> response::Body {
        let Some((store, facts)) = &self.settings else {
            return Self::no_settings();
        };
        let Some(wanted) = set.settings else {
            return error(
                ErrorCode::InvalidSetting,
                "the request carries no settings".into(),
            );
        };
        let wanted = Settings {
            render_gpu: wanted.render_gpu,
            freeze: wanted.freeze,
            clipboard: wanted.clipboard,
            links_in_android: wanted.links_in_android,
            memory_limit_mb: wanted.memory_limit_mb,
            program_updates: wanted.program_updates,
            image_updates: wanted.image_updates,
            input_trace: wanted.input_trace,
            resize_trace: wanted.resize_trace,
            developer_mode: wanted.developer_mode,
            adb_port: match u16::try_from(wanted.adb_port) {
                Ok(port) => port,
                Err(_) => {
                    return error(
                        ErrorCode::InvalidSetting,
                        format!(
                            "the adb port {} is not a TCP port; use one from 1 to 65535",
                            wanted.adb_port
                        ),
                    )
                }
            },
        };
        // The update and trace settings apply to every user of the machine, so only an
        // administrator may change them; the caller's own settings are theirs to change.
        let machine = store.machine();
        let changed: Vec<&str> = [
            (
                "program updates",
                wanted.program_updates != machine.program_updates,
            ),
            (
                "image updates",
                wanted.image_updates != machine.image_updates,
            ),
            ("input trace", wanted.input_trace != machine.input_trace),
            ("resize trace", wanted.resize_trace != machine.resize_trace),
        ]
        .into_iter()
        .filter_map(|(name, changed)| changed.then_some(name))
        .collect();
        if !changed.is_empty() && !peer.administrator {
            return error(
                ErrorCode::NotAllowed,
                format!(
                    "only an administrator can change this setting ({}); it applies to every \
                     user of this computer. Run the command as an administrator.",
                    changed.join(", ")
                ),
            );
        }
        match store.set(&peer.user, &wanted) {
            Ok(settings) => {
                // The caller's Android follows the freeze, clipboard, link and developer mode
                // settings at once.
                if let Ok(android) = self.androids.android(&peer.user) {
                    android.freeze_when_idle(settings.freeze);
                    android.share_clipboard(settings.clipboard);
                    android.links_in_android(settings.links_in_android);
                    if let Err(why) =
                        android.developer_mode(settings.developer_mode, settings.adb_port)
                    {
                        // Kept as chosen, so it applies when the port is free again (the
                        // next start of the service), but the caller learns why adb cannot
                        // connect now.
                        eprintln!("local API: developer mode for {}: {why}", peer.user);
                        return error(ErrorCode::InvalidSetting, why);
                    }
                }
                if settings.resize_trace != machine.resize_trace {
                    *self.resize_trace.lock().unwrap_or_else(|e| e.into_inner()) =
                        ResizeTrace::new(settings.resize_trace, Instant::now());
                }
                eprintln!(
                    "local API: {} changed the settings: render GPU {}, freeze {}, clipboard \
                     {}, links in Android {}, program updates {}, image updates {}, input \
                     trace {}, resize trace {}, developer mode {} (adb port {})",
                    peer.user,
                    settings.render_gpu,
                    on_off(settings.freeze),
                    on_off(settings.clipboard),
                    on_off(settings.links_in_android),
                    on_off(settings.program_updates),
                    on_off(settings.image_updates),
                    on_off(settings.input_trace),
                    on_off(settings.resize_trace),
                    on_off(settings.developer_mode),
                    settings.adb_port
                );
                response::Body::Settings(settings_to_wire(&settings, &self.facts_now(facts)))
            }
            Err(why) => error(ErrorCode::InvalidSetting, why),
        }
    }
}

fn on_off(value: bool) -> &'static str {
    if value {
        "on"
    } else {
        "off"
    }
}

fn settings_to_wire(settings: &Settings, facts: &ServiceFacts) -> types::Settings {
    types::Settings {
        render_gpu: settings.render_gpu.clone(),
        freeze: settings.freeze,
        links_in_android: settings.links_in_android,
        memory_limit_mb: settings.memory_limit_mb,
        program_updates: settings.program_updates,
        image_updates: settings.image_updates,
        clipboard: settings.clipboard,
        input_trace: settings.input_trace,
        resize_trace: settings.resize_trace,
        developer_mode: settings.developer_mode,
        adb_port: settings.adb_port.into(),
        program_updates_available: facts.program_updates_available,
        image_updates_available: facts.image_updates_available,
        version: facts.version.clone(),
        log_dir: facts.log_dir.clone().unwrap_or_default(),
        android_version: facts.android_version.clone().unwrap_or_default(),
    }
}

fn settings_from_wire(settings: types::Settings) -> SettingsReport {
    SettingsReport {
        settings: Settings {
            render_gpu: settings.render_gpu,
            links_in_android: settings.links_in_android,
            memory_limit_mb: settings.memory_limit_mb,
            freeze: settings.freeze,
            clipboard: settings.clipboard,
            program_updates: settings.program_updates,
            image_updates: settings.image_updates,
            input_trace: settings.input_trace,
            resize_trace: settings.resize_trace,
            developer_mode: settings.developer_mode,
            adb_port: crate::settings::normalise_adb_port(
                u16::try_from(settings.adb_port).unwrap_or(0),
            ),
        },
        facts: ServiceFacts {
            version: settings.version,
            log_dir: Some(settings.log_dir).filter(|dir| !dir.is_empty()),
            program_updates_available: settings.program_updates_available,
            image_updates_available: settings.image_updates_available,
            android_version: Some(settings.android_version).filter(|v| !v.is_empty()),
        },
    }
}

fn error(code: ErrorCode, detail: String) -> response::Body {
    response::Body::Error(types::Error {
        code: code as i32,
        detail,
    })
}

fn status(android: &dyn UserAndroid) -> response::Body {
    response::Body::Status(types::Status {
        state: to_wire(android.state()) as i32,
        last_reason: android.last_reason().unwrap_or_default(),
        update_notice: android.update_notice().unwrap_or_default(),
        problem: android.problem().unwrap_or_default(),
    })
}

/// The caller's Android as the service reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: SessionState,
    pub last_reason: Option<String>,
    /// News about the Android image, such as an update waiting for the next start.
    pub update_notice: Option<String>,
    /// What keeps it from being ready, and what to do, while something does.
    pub problem: Option<String>,
}

/// The tasks of the caller's Android as the service reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskList {
    /// In task ID order.
    pub tasks: Vec<Task>,
    /// The display of each task whose frames have been placed, as `(task_id, display_id)`,
    /// in task ID order.
    pub frames: Vec<(i32, u32)>,
    /// The name and icon of each app that has a task, in package order.
    pub apps: Vec<AppInfo>,
    /// The apps installed in the Android that a launcher lists, in package order; `None`
    /// until it has said which.
    pub installed: Option<Vec<AppInfo>>,
}

impl TaskList {
    /// The events that bring an empty list of tasks to this one: the installed apps when
    /// they are known, then every app, then every task, then every task's display.
    pub fn events(&self) -> Vec<TaskEvent> {
        let installed = self.installed.clone().map(TaskEvent::Installed);
        let apps = installed
            .into_iter()
            .chain(self.apps.iter().cloned().map(TaskEvent::App));
        let tasks = self.tasks.iter().cloned().map(TaskEvent::Updated);
        let frames = self
            .frames
            .iter()
            .map(|(task_id, display_id)| TaskEvent::Frames {
                task_id: *task_id,
                display_id: *display_id,
            });
        apps.chain(tasks).chain(frames).collect()
    }
}

/// Why a local API call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// Connecting or the handshake failed; the detail says why.
    Connect(HandshakeError),
    /// The connection broke or timed out.
    Transport(TransportError),
    /// The service answered with an error.
    Service { code: ErrorCode, detail: String },
    /// The service's answer did not make sense.
    Malformed(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Connect(error) => write!(f, "{error}"),
            ClientError::Transport(error) => write!(f, "the DSA service: {error}"),
            ClientError::Service { detail, .. } => write!(f, "the DSA service: {detail}"),
            ClientError::Malformed(why) => write!(f, "the DSA service answered oddly: {why}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// The client end of the local API.
pub struct HostApiClient {
    connection: Box<dyn Connection>,
    next_id: u64,
    timeout: Duration,
}

impl HostApiClient {
    /// Connects through `connector` and completes the handshake, waiting at most `timeout`
    /// for each step.
    pub fn connect(
        connector: &mut dyn Connector,
        build: &str,
        timeout: Duration,
    ) -> Result<HostApiClient, ClientError> {
        let mut connection = connector
            .connect(timeout)
            .map_err(|error| ClientError::Connect(HandshakeError::Transport(error)))?;
        client_connect(&mut *connection, &local_hello(Role::Client, build), timeout)
            .map_err(ClientError::Connect)?;
        Ok(HostApiClient {
            connection,
            next_id: 1,
            timeout,
        })
    }

    pub fn status(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::GetStatus(types::GetStatus {}))
    }

    /// Asks for the caller's Android to start. Returns the state right after the request.
    pub fn start(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::StartSession(types::StartSession {}))
    }

    /// Asks for the caller's Android to stop. Returns the state right after the request.
    pub fn stop(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::StopSession(types::StopSession {}))
    }

    /// Freezes the caller's Android until a task window is shown again. Returns the state
    /// right after the request.
    pub fn freeze(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::FreezeSession(types::FreezeSession {}))
    }

    /// Says whether any task window of the caller's Android is shown.
    pub fn set_windows_shown(&mut self, shown: bool) -> Result<(), ClientError> {
        self.done(request::Body::SetWindowsShown(types::SetWindowsShown {
            shown,
        }))
    }

    /// Asks the service to install the newest version of DSA now. Downloading it can take
    /// a while, so the answer is awaited for at most `timeout`.
    pub fn update_program(&mut self, timeout: Duration) -> Result<ProgramUpdate, ClientError> {
        match self.exchange(
            request::Body::UpdateProgram(types::UpdateProgram {}),
            timeout,
        )? {
            response::Body::ProgramUpdate(update) => Ok(ProgramUpdate {
                installing: Some(update.installing).filter(|version| !version.is_empty()),
                detail: update.detail,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer to an update request".into(),
            )),
        }
    }

    /// Asks for the runtime ID of the caller's Android VM, if running.
    pub fn vm_id(&mut self) -> Result<Option<String>, ClientError> {
        match self.exchange(request::Body::GetVmId(types::GetVmId {}), self.timeout)? {
            response::Body::VmId(vm_id) => Ok(Some(vm_id.vm_id).filter(|id| !id.is_empty())),
            _ => Err(ClientError::Malformed("expected a vm_id response".into())),
        }
    }

    /// How much memory the caller's Android takes, in the guest and on the host. The service
    /// asks Android for a fresh report first, so this takes a moment.
    pub fn memory(&mut self) -> Result<MemorySnapshot, ClientError> {
        let timeout = self.timeout + MEMORY_WAIT;
        match self.exchange(request::Body::GetMemory(types::GetMemory {}), timeout)? {
            response::Body::Memory(memory) => Ok(snapshot_from_wire(memory)),
            _ => Err(ClientError::Malformed(
                "expected the memory of the caller's Android".into(),
            )),
        }
    }

    /// The app tasks of the caller's Android.
    pub fn tasks(&mut self) -> Result<TaskList, ClientError> {
        match self.exchange(request::Body::ListTasks(types::ListTasks {}), self.timeout)? {
            response::Body::Tasks(tasks) => Ok(tasks_from_wire(tasks)),
            _ => Err(ClientError::Malformed("expected the list of tasks".into())),
        }
    }

    /// The settings of the caller's DSA, with what the service says about itself.
    pub fn settings(&mut self) -> Result<SettingsReport, ClientError> {
        match self.exchange(
            request::Body::GetSettings(types::GetSettings {}),
            self.timeout,
        )? {
            response::Body::Settings(settings) => Ok(settings_from_wire(settings)),
            _ => Err(ClientError::Malformed("expected the settings".into())),
        }
    }

    /// Replaces the settings of the caller's DSA, and returns them as the service now keeps
    /// them.
    pub fn set_settings(&mut self, settings: &Settings) -> Result<SettingsReport, ClientError> {
        let body = request::Body::SetSettings(types::SetSettings {
            settings: Some(settings_to_wire(settings, &ServiceFacts::default())),
        });
        match self.exchange(body, self.timeout)? {
            response::Body::Settings(settings) => Ok(settings_from_wire(settings)),
            _ => Err(ClientError::Malformed("expected the settings".into())),
        }
    }

    /// The apps installed in the caller's Android that a launcher lists, in package order;
    /// `None` until it has said which since the service started.
    pub fn installed_apps(&mut self) -> Result<Option<Vec<AppInfo>>, ClientError> {
        match self.exchange(request::Body::ListApps(types::ListApps {}), self.timeout)? {
            response::Body::InstalledApps(installed) => Ok(installed_from_wire(installed)),
            _ => Err(ClientError::Malformed(
                "expected the list of installed apps".into(),
            )),
        }
    }

    /// Makes sure the caller's Android is ready: starts it when it is stopped, and waits at
    /// most `timeout` for it to have finished booting with its agent connected, asking every
    /// `poll`. `waiting` is called once whenever the state it waits through changes, so the
    /// caller can show progress. A start that fails, or an Android that has stalled, is an error
    /// carrying the reason the service gives.
    pub fn ensure_running(
        &mut self,
        timeout: Duration,
        poll: Duration,
        mut waiting: impl FnMut(SessionState),
    ) -> Result<(), ClientError> {
        let deadline = Instant::now() + timeout;
        let mut status = self.status()?;
        let mut started = false;
        let mut last = None;
        loop {
            match status.state {
                SessionState::Active | SessionState::Dozing | SessionState::Frozen => return Ok(()),
                SessionState::Stalled => {
                    return Err(ClientError::Service {
                        code: ErrorCode::Unavailable,
                        detail: status.problem.unwrap_or_else(|| {
                            "Android runs but has taken too long to be ready".into()
                        }),
                    })
                }
                SessionState::Stopped if started => {
                    return Err(ClientError::Service {
                        code: ErrorCode::Unavailable,
                        detail: status
                            .last_reason
                            .unwrap_or_else(|| "Android stopped while it was starting".into()),
                    })
                }
                SessionState::Stopped => {
                    started = true;
                    status = self.start()?;
                    continue;
                }
                SessionState::Starting | SessionState::Stopping => {}
            }
            if last != Some(status.state) {
                last = Some(status.state);
                waiting(status.state);
            }
            if Instant::now() >= deadline {
                return Err(ClientError::Transport(TransportError::Timeout));
            }
            thread::sleep(poll);
            // A start asked for while it was stopping goes in once it has stopped.
            status = self.status()?;
        }
    }

    /// Asks the caller's Android to end a task, as when the user closes its window.
    pub fn close_task(&mut self, task_id: i32) -> Result<(), ClientError> {
        self.done(request::Body::CloseTask(types::CloseTask { task_id }))
    }

    /// Asks the caller's Android to end every process of an app and give its memory back,
    /// even one a widget or a foreground service keeps running: the user's Stop.
    pub fn stop_app(&mut self, package_name: &str) -> Result<(), ClientError> {
        self.done(request::Body::StopApp(types::StopApp {
            package_name: package_name.to_string(),
        }))
    }

    /// Asks the caller's Android to start an app in a new task of its own.
    pub fn launch_app(&mut self, package_name: &str) -> Result<(), ClientError> {
        self.done(request::Body::LaunchApp(types::LaunchApp {
            package_name: package_name.into(),
        }))
    }

    /// Asks the caller's Android to give a task the client size of its window, in Android
    /// display pixels, as when the user resizes the window. `live` is true while the user is
    /// still dragging the window's edge: the task takes the size without being reconfigured,
    /// and the resize with `live` false at the end of the drag reconfigures it once.
    pub fn resize_task(
        &mut self,
        task_id: i32,
        width: u32,
        height: u32,
        live: bool,
    ) -> Result<(), ClientError> {
        self.done(request::Body::ResizeTask(types::ResizeTask {
            task_id,
            width,
            height,
            live,
        }))
    }

    /// Asks the caller's Android to draw a task at `density_dpi`, as when its window moves
    /// to a monitor of another scale.
    pub fn set_task_density(&mut self, task_id: i32, density_dpi: u32) -> Result<(), ClientError> {
        self.done(request::Body::SetTaskDensity(types::SetTaskDensity {
            task_id,
            density_dpi,
        }))
    }

    /// Carries out `request` against the caller's Android.
    pub fn request(&mut self, request: &TaskRequest) -> Result<(), ClientError> {
        match request {
            TaskRequest::Close { task_id } => self.close_task(*task_id),
            TaskRequest::Launch { package_name } => self.launch_app(package_name),
            TaskRequest::Resize {
                task_id,
                width,
                height,
                live,
            } => self.resize_task(*task_id, *width, *height, *live),
            TaskRequest::SetDensity {
                task_id,
                density_dpi,
            } => self.set_task_density(*task_id, *density_dpi),
            TaskRequest::WindowsShown { shown } => self.set_windows_shown(*shown),
            TaskRequest::StopApp { package_name } => self.stop_app(package_name),
        }
    }

    /// Watches the app tasks of the caller's Android. The connection carries nothing else
    /// from here on, so the client is used up.
    pub fn watch_tasks(mut self) -> Result<TaskStream, ClientError> {
        match self.exchange(
            request::Body::WatchTasks(types::WatchTasks {}),
            self.timeout,
        )? {
            response::Body::Tasks(tasks) => Ok(TaskStream {
                connection: self.connection,
                current: tasks_from_wire(tasks),
            }),
            _ => Err(ClientError::Malformed("expected the list of tasks".into())),
        }
    }

    /// Reads the guest log of the caller's Android: the records the service holds that pass
    /// `filter`, then each new one; with `dump`, what Android's log holds now, then the end.
    /// The connection carries nothing else from here on, so the client is used up.
    pub fn watch_log(
        mut self,
        filter: &LogFilter,
        dump: bool,
    ) -> Result<crate::guest_log_api::LogReader, ClientError> {
        match self.exchange(
            request::Body::WatchLog(filter.to_host_api(dump)),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(crate::guest_log_api::LogReader::new(self.connection)),
            _ => Err(ClientError::Malformed(
                "expected the answer that the log is sent".into(),
            )),
        }
    }

    /// Gathers the diagnostics of the caller's Android and of DSA on this computer into one
    /// zip archive. The connection carries nothing else from here on, so the client is used up.
    pub fn collect_diagnostics(
        mut self,
    ) -> Result<crate::guest_log_api::DiagnosticsReader, ClientError> {
        match self.exchange(
            request::Body::CollectDiagnostics(types::CollectDiagnostics {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(crate::guest_log_api::DiagnosticsReader::new(
                self.connection,
            )),
            _ => Err(ClientError::Malformed(
                "expected the answer that the diagnostics are gathered".into(),
            )),
        }
    }

    /// Streams input to the caller's Android. The connection carries nothing else from here
    /// on, so the client is used up.
    pub fn stream_input(mut self) -> Result<InputStream, ClientError> {
        match self.exchange(
            request::Body::StreamInput(types::StreamInput {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(InputStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that input may be sent".into(),
            )),
        }
    }

    /// Carries the clipboard between the host and the caller's Android. The connection carries
    /// nothing else from here on, so the client is used up.
    pub fn watch_clipboard(mut self) -> Result<ClipboardStream, ClientError> {
        match self.exchange(
            request::Body::WatchClipboard(types::WatchClipboard {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(ClipboardStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that the clipboard is carried".into(),
            )),
        }
    }

    /// Installs the APK called `name`, of `size` bytes read from `content`, in the caller's
    /// Android, and returns how the install ended: a refusal by Android is an outcome, not an
    /// error. An error means the APK did not get there. The connection ends with the install,
    /// so the client is used up.
    pub fn install_apk(
        mut self,
        name: &str,
        size: u64,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, ClientError> {
        let id = self.next_id;
        match self.exchange(
            request::Body::InstallApk(types::InstallApk {
                name: name.to_string(),
                size,
            }),
            self.timeout,
        )? {
            response::Body::Done(_) => {}
            _ => {
                return Err(ClientError::Malformed(
                    "expected the answer that the APK is taken".into(),
                ))
            }
        }
        let mut left = size;
        let mut piece = vec![0u8; apk::LOCAL_CHUNK_BYTES];
        let mut failed = None;
        while left > 0 {
            let length = left.min(piece.len() as u64) as usize;
            if let Err(error) = content.read_exact(&mut piece[..length]) {
                failed = Some(error);
                break;
            }
            // A service that has answered already (Android refused the APK before the end)
            // closes the connection; its answer is still there to read.
            if send(
                &mut *self.connection,
                Body::ApkChunk(types::ApkChunk {
                    data: piece[..length].to_vec(),
                }),
            )
            .is_err()
            {
                break;
            }
            left -= length as u64;
        }
        if let Some(error) = failed {
            self.connection.close();
            return Err(ClientError::Service {
                code: ErrorCode::InstallRefused,
                detail: format!("the APK could not be read: {error}"),
            });
        }
        match self.answer(id, INSTALL_ANSWER_WAIT)? {
            response::Body::InstallResult(result) => Ok(InstallOutcome::from_wire(result)),
            _ => Err(ClientError::Malformed(
                "expected how the install ended".into(),
            )),
        }
    }

    /// Receives the web links the caller's Android opens, for the host's browser. The
    /// connection carries nothing else from here on, so the client is used up.
    pub fn watch_links(mut self) -> Result<LinkStream, ClientError> {
        match self.exchange(
            request::Body::WatchLinks(types::WatchLinks {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(LinkStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that links are sent".into(),
            )),
        }
    }

    /// Watches the accessibility tree of the caller's task `task_id`: its tree and the answers
    /// to actions arrive on the stream, which also sends the actions. The caller's Android
    /// reads the tree until the stream is dropped. The connection carries nothing else from
    /// here on, so the client is used up.
    pub fn watch_accessibility(mut self, task_id: i32) -> Result<AccessibilityStream, ClientError> {
        match self.exchange(
            request::Body::WatchAccessibility(types::WatchAccessibility { task_id }),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(AccessibilityStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that the tree is watched".into(),
            )),
        }
    }

    /// Sends `file` to the caller's Android, which asks the user which app opens or takes it.
    /// Returns once Android has asked. The connection ends with the transfer, so the client
    /// is used up.
    pub fn open_file(mut self, file: &FileTransfer) -> Result<(), ClientError> {
        let action = match file.action {
            FileAction::View => types::FileAction::View,
            FileAction::Send => types::FileAction::Send,
        };
        let id = self.next_id;
        self.exchange(
            request::Body::OpenFile(types::OpenFile {
                name: file.name.clone(),
                size: file.content.len() as u64,
                action: action as i32,
            }),
            self.timeout,
        )
        .and_then(|body| match body {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed(
                "expected the answer that the file is taken".into(),
            )),
        })?;
        for piece in file.content.chunks(files::LOCAL_CHUNK_BYTES) {
            send(
                &mut *self.connection,
                Body::FileChunk(types::FileChunk {
                    data: piece.to_vec(),
                }),
            )
            .map_err(ClientError::Transport)?;
        }
        match self.answer(id, FILE_ANSWER_WAIT)? {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed(
                "expected the answer that Android has the file".into(),
            )),
        }
    }

    /// Watches the notifications of the caller's Android. The connection carries nothing
    /// else from here on, so the client is used up.
    pub fn watch_notifications(mut self) -> Result<NotificationStream, ClientError> {
        match self.exchange(
            request::Body::WatchNotifications(types::WatchNotifications {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(NotificationStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that notifications are watched".into(),
            )),
        }
    }

    /// Passes what the user did with a notification on the host to the caller's Android.
    pub fn act_on_notification(&mut self, action: &NotificationAction) -> Result<(), ClientError> {
        let (key, kind) = match action {
            NotificationAction::Open { key } => (key, types::NotificationAction::Open),
            NotificationAction::Dismiss { key } => (key, types::NotificationAction::Dismiss),
        };
        self.done(request::Body::ActOnNotification(types::ActOnNotification {
            key: key.clone(),
            action: kind as i32,
        }))
    }

    /// What the caller's Android has installed of `package`, if anything. Android must be
    /// running.
    pub fn package_state(
        &mut self,
        package: &str,
    ) -> Result<Option<InstalledPackage>, ClientError> {
        match self.exchange(
            request::Body::GetPackage(types::GetPackage {
                package_name: package.to_string(),
            }),
            self.timeout
                .max(PACKAGE_QUERY_WAIT + Duration::from_secs(5)),
        )? {
            response::Body::PackageState(state) => Ok(InstalledPackage::from_wire(state)),
            _ => Err(ClientError::Malformed(
                "expected what Android has of the package".into(),
            )),
        }
    }

    /// The widgets the apps in the caller's Android offer, in label order.
    pub fn widget_providers(&mut self) -> Result<Vec<WidgetProvider>, ClientError> {
        match self.exchange(
            request::Body::ListWidgetProviders(types::ListWidgetProviders {}),
            self.timeout.max(WIDGET_LIST_WAIT + Duration::from_secs(5)),
        )? {
            response::Body::WidgetProviders(list) => {
                Ok(list.providers.into_iter().map(provider_from_wire).collect())
            }
            _ => Err(ClientError::Malformed(
                "expected the widgets on offer".into(),
            )),
        }
    }

    /// Places a new widget of `provider` and returns it, waiting at most `timeout`, which
    /// includes the user setting the widget up when its app asks them to.
    pub fn add_widget(&mut self, provider: &str, timeout: Duration) -> Result<Widget, ClientError> {
        match self.exchange(
            request::Body::AddWidget(types::AddWidget {
                provider: provider.to_string(),
            }),
            timeout,
        )? {
            response::Body::Widget(widget) => Ok(widget_from_wire(widget)),
            _ => Err(ClientError::Malformed("expected the placed widget".into())),
        }
    }

    /// Removes a widget.
    pub fn remove_widget(&mut self, widget_id: i32) -> Result<(), ClientError> {
        self.done(request::Body::RemoveWidget(types::RemoveWidget {
            widget_id,
        }))
    }

    /// The widgets placed on the caller's desktop, and how many places the service keeps.
    pub fn widgets(&mut self) -> Result<(Vec<Widget>, u32), ClientError> {
        match self.exchange(
            request::Body::ListWidgets(types::ListWidgets {}),
            self.timeout,
        )? {
            response::Body::Widgets(list) => Ok((
                list.widgets.into_iter().map(widget_from_wire).collect(),
                list.remembered,
            )),
            _ => Err(ClientError::Malformed("expected the placed widgets".into())),
        }
    }

    /// Remembers where a widget's window is and tells Android its size: `width` by `height`
    /// pixels on a monitor of `density_dpi`. A size of 0 only moves it.
    pub fn place_widget(
        &mut self,
        widget_id: i32,
        placement: Placement,
        width: u32,
        height: u32,
        density_dpi: u32,
    ) -> Result<(), ClientError> {
        self.done(request::Body::PlaceWidget(types::PlaceWidget {
            widget_id,
            placement: Some(placement_to_wire(placement)),
            width,
            height,
            density_dpi,
        }))
    }

    /// Passes pointer input in a widget's window to the caller's Android. `action` is one of
    /// press, move, release and cancel.
    pub fn widget_pointer(
        &mut self,
        widget_id: i32,
        action: types::PointerAction,
        pointer_id: u32,
        x: f32,
        y: f32,
    ) -> Result<(), ClientError> {
        self.done(request::Body::WidgetPointer(types::WidgetPointer {
            widget_id,
            action: action as i32,
            pointer_id,
            x,
            y,
        }))
    }

    /// Watches the widgets of the caller's Android. The connection carries nothing else from
    /// here on, so the client is used up.
    pub fn watch_widgets(mut self) -> Result<WidgetStream, ClientError> {
        match self.exchange(
            request::Body::WatchWidgets(types::WatchWidgets {}),
            self.timeout,
        )? {
            response::Body::Widgets(list) => Ok(WidgetStream {
                connection: self.connection,
                current: list.widgets.into_iter().map(widget_from_wire).collect(),
                remembered: list.remembered,
            }),
            _ => Err(ClientError::Malformed(
                "expected the widgets there are now".into(),
            )),
        }
    }

    fn done(&mut self, body: request::Body) -> Result<(), ClientError> {
        match self.exchange(body, self.timeout)? {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed(
                "expected the answer that the request was carried out".into(),
            )),
        }
    }

    fn call(&mut self, body: request::Body) -> Result<Status, ClientError> {
        match self.exchange(body, self.timeout)? {
            response::Body::Status(status) => Ok(Status {
                state: from_wire(status.state).ok_or_else(|| {
                    ClientError::Malformed(format!("unknown session state {}", status.state))
                })?,
                last_reason: Some(status.last_reason).filter(|reason| !reason.is_empty()),
                update_notice: Some(status.update_notice).filter(|notice| !notice.is_empty()),
                problem: Some(status.problem).filter(|problem| !problem.is_empty()),
            }),
            _ => Err(ClientError::Malformed("expected a status".into())),
        }
    }

    /// Sends `body` and returns the answer's body; an error answer is an error.
    fn exchange(
        &mut self,
        body: request::Body,
        timeout: Duration,
    ) -> Result<response::Body, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        send(
            &mut *self.connection,
            Body::Request(Request {
                id,
                body: Some(body),
            }),
        )
        .map_err(ClientError::Transport)?;
        self.answer(id, timeout)
    }

    /// Waits at most `timeout` for the answer to request `id` and returns its body; an error
    /// answer is an error.
    fn answer(&mut self, id: u64, timeout: Duration) -> Result<response::Body, ClientError> {
        let frame = self
            .connection
            .recv(Some(timeout))
            .map_err(ClientError::Transport)?;
        let response = match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::Response(response)),
            }) => response,
            Ok(_) => return Err(ClientError::Malformed("expected a response".into())),
            Err(error) => return Err(ClientError::Malformed(error.to_string())),
        };
        if response.id != id {
            return Err(ClientError::Malformed(format!(
                "the answer is for request {}, expected {id}",
                response.id
            )));
        }
        match response.body {
            Some(response::Body::Error(error)) => Err(ClientError::Service {
                code: ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified),
                detail: error.detail,
            }),
            Some(body) => Ok(body),
            None => Err(ClientError::Malformed("an empty response".into())),
        }
    }
}

/// The content of an APK a client sends, read from its ApkChunk frames as the install needs it.
/// Anything else the client sends, a frame that cannot be read, more bytes than announced, or a
/// gap longer than [`APK_CHUNK_WAIT`] ends the content with an error, and the install with it.
struct ApkChunks<'a> {
    connection: &'a mut dyn Connection,
    user: &'a str,
    /// The bytes announced that have not arrived yet.
    left: u64,
    piece: Vec<u8>,
    /// How much of `piece` has been read.
    at: usize,
}

impl Read for ApkChunks<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at == self.piece.len() {
            if self.left == 0 {
                return Ok(0);
            }
            let frame = self
                .connection
                .recv(Some(APK_CHUNK_WAIT))
                .map_err(|error| {
                    eprintln!(
                        "install: the APK from {} did not arrive whole ({error})",
                        self.user
                    );
                    io::Error::other(error.to_string())
                })?;
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::ApkChunk(chunk)),
                }) if chunk.data.len() as u64 <= self.left => {
                    self.left -= chunk.data.len() as u64;
                    self.piece = chunk.data;
                    self.at = 0;
                }
                Ok(Envelope {
                    body: Some(Body::ApkChunk(_)),
                }) => {
                    eprintln!(
                        "install: a client of {} sent more of its APK than it announced",
                        self.user
                    );
                    return Err(io::Error::other("more bytes arrived than were announced"));
                }
                Ok(_) | Err(_) => {
                    eprintln!(
                        "install: a client of {} sent something other than a piece of its APK; \
                         dropping the install",
                        self.user
                    );
                    return Err(io::Error::other(
                        "something other than a piece of the APK arrived",
                    ));
                }
            }
        }
        let length = buf.len().min(self.piece.len() - self.at);
        buf[..length].copy_from_slice(&self.piece[self.at..self.at + length]);
        self.at += length;
        Ok(length)
    }
}

/// The tasks of the caller's Android and every change to them, from
/// [`HostApiClient::watch_tasks`].
pub struct TaskStream {
    connection: Box<dyn Connection>,
    current: TaskList,
}

impl TaskStream {
    /// The tasks there were when the watch began.
    pub fn current(&self) -> &TaskList {
        &self.current
    }

    /// The next change, waiting at most `timeout`. `Ok(None)` means none came in time; an
    /// error means the watch has ended, and a new connection is needed for another.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<TaskEvent>, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = match self.connection.recv(Some(remaining)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => return Ok(None),
                Err(error) => return Err(ClientError::Transport(error)),
            };
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::TaskEvent(event)),
                }) => match event_from_wire(event) {
                    Some(event) => return Ok(Some(event)),
                    // A kind of change this client does not know: a newer service told it
                    // something it can do nothing with.
                    None => continue,
                },
                Ok(_) => return Err(ClientError::Malformed("expected a task event".into())),
                Err(error) => return Err(ClientError::Malformed(error.to_string())),
            }
        }
    }
}

/// A connection that carries the clipboard between the host and the caller's Android, from
/// [`HostApiClient::watch_clipboard`].
pub struct ClipboardStream {
    connection: Box<dyn Connection>,
}

impl ClipboardStream {
    /// Says that the host's clipboard got `text`. Nothing is awaited. An error means the
    /// connection has ended, and a new one is needed.
    pub fn send(&mut self, text: &str) -> Result<(), ClientError> {
        send(
            &mut *self.connection,
            Body::ClipboardText(types::ClipboardText {
                text: text.to_string(),
            }),
        )
        .map_err(ClientError::Transport)
    }

    /// The next text the caller's Android copied, waiting at most `timeout`. `Ok(None)` means
    /// none came in time; an error means the connection has ended.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<String>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::ClipboardText(clip)),
            }) => Ok(Some(clip.text)),
            Ok(_) => Err(ClientError::Malformed("expected clipboard text".into())),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

/// A task's accessibility tree, from [`HostApiClient::watch_accessibility`].
pub struct AccessibilityStream {
    connection: Box<dyn Connection>,
}

impl AccessibilityStream {
    /// The next tree, change or action result, waiting at most `timeout`. `Ok(None)` means
    /// nothing came in time; an error means the connection has ended. A tree that breaks the
    /// rules (too many nodes, say) is an error too, and the stream should be dropped.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<GuestAccessibility>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::AccessibilityTree(tree)),
            }) => TreeUpdate::from_host_api(tree)
                .map(|tree| Some(GuestAccessibility::Tree(tree)))
                .map_err(ClientError::Malformed),
            Ok(Envelope {
                body: Some(Body::AccessibilityActionResult(result)),
            }) => Ok(Some(GuestAccessibility::ActionResult(
                ActionResult::from_host_api(result),
            ))),
            Ok(_) => Err(ClientError::Malformed(
                "expected an accessibility tree".into(),
            )),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }

    /// Asks for an action on a node; its result arrives through [`AccessibilityStream::next`].
    pub fn perform(&mut self, action: &ActionRequest) -> Result<(), ClientError> {
        send(
            &mut *self.connection,
            Body::AccessibilityAction(action.to_host_api()),
        )
        .map_err(ClientError::Transport)
    }
}

/// The web links the caller's Android opens, from [`HostApiClient::watch_links`].
pub struct LinkStream {
    connection: Box<dyn Connection>,
}

impl LinkStream {
    /// The next link, waiting at most `timeout`. `Ok(None)` means none came in time; an error
    /// means the connection has ended, and a new one is needed. The link is as the service
    /// sent it: the caller checks it again before opening it.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<String>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::OpenLink(link)),
            }) => Ok(Some(link.url)),
            Ok(_) => Err(ClientError::Malformed("expected a link".into())),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

/// A connection that watches the notifications of the caller's Android, from
/// [`HostApiClient::watch_notifications`].
pub struct NotificationStream {
    connection: Box<dyn Connection>,
}

impl NotificationStream {
    /// The next change to the notifications, waiting at most `timeout`. `Ok(None)` means none
    /// came in time; an error means the connection has ended.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<NotificationEvent>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::Notification(n)),
            }) => Ok(Some(NotificationEvent::Posted(Notification {
                key: n.key,
                package_name: n.package_name,
                app_label: n.app_label,
                title: n.title,
                text: n.text,
                icon_png: n.icon_png,
                importance: n.importance,
                high_priority: n.high_priority,
                clearable: n.clearable,
                when_millis: n.when_millis,
            }))),
            Ok(Envelope {
                body: Some(Body::NotificationRemoved(removed)),
            }) => Ok(Some(NotificationEvent::Removed { key: removed.key })),
            Ok(_) => Err(ClientError::Malformed("expected a notification".into())),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

/// The widgets of the caller's Android and every change to them, from
/// [`HostApiClient::watch_widgets`].
pub struct WidgetStream {
    connection: Box<dyn Connection>,
    current: Vec<Widget>,
    remembered: u32,
}

impl WidgetStream {
    /// The widgets there were when the watch began, with their placements.
    pub fn current(&self) -> &[Widget] {
        &self.current
    }

    /// How many widgets the service remembered a place for when the watch began.
    pub fn remembered(&self) -> u32 {
        self.remembered
    }

    /// The next change, waiting at most `timeout`. `Ok(None)` means none came in time; an
    /// error means the watch has ended.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<WidgetEvent>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        let event = match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::WidgetEvent(event)),
            }) => event,
            Ok(_) => return Err(ClientError::Malformed("expected a widget event".into())),
            Err(error) => return Err(ClientError::Malformed(error.to_string())),
        };
        match event.body {
            Some(types::widget_event::Body::Placed(widget)) => {
                Ok(Some(WidgetEvent::Placed(widget_from_wire(widget))))
            }
            Some(types::widget_event::Body::Frame(frame)) => {
                Ok(Some(WidgetEvent::Frame(WidgetFrame {
                    widget_id: frame.widget_id,
                    width: frame.width,
                    height: frame.height,
                    png: frame.png,
                })))
            }
            Some(types::widget_event::Body::Removed(removed)) => Ok(Some(WidgetEvent::Removed {
                widget_id: removed.widget_id,
            })),
            None => Err(ClientError::Malformed("an empty widget event".into())),
        }
    }
}

fn provider_to_wire(provider: &WidgetProvider) -> types::WidgetProvider {
    types::WidgetProvider {
        provider: provider.provider.clone(),
        package_name: provider.package_name.clone(),
        label: provider.label.clone(),
        preview_png: provider.preview_png.clone(),
        min_width_dp: provider.min_width_dp,
        min_height_dp: provider.min_height_dp,
        default_width_dp: provider.default_width_dp,
        default_height_dp: provider.default_height_dp,
        max_width_dp: provider.max_width_dp,
        max_height_dp: provider.max_height_dp,
        resize_mode: provider.resize_mode,
        needs_configuration: provider.needs_configuration,
        app_label: provider.app_label.clone(),
    }
}

fn provider_from_wire(provider: types::WidgetProvider) -> WidgetProvider {
    WidgetProvider {
        provider: provider.provider,
        package_name: provider.package_name,
        label: provider.label,
        preview_png: provider.preview_png,
        min_width_dp: provider.min_width_dp,
        min_height_dp: provider.min_height_dp,
        default_width_dp: provider.default_width_dp,
        default_height_dp: provider.default_height_dp,
        max_width_dp: provider.max_width_dp,
        max_height_dp: provider.max_height_dp,
        resize_mode: provider.resize_mode,
        needs_configuration: provider.needs_configuration,
        app_label: provider.app_label,
    }
}

fn placement_to_wire(placement: Placement) -> types::WidgetPlacement {
    types::WidgetPlacement {
        x: placement.x,
        y: placement.y,
        width_dp: placement.width_dp,
        height_dp: placement.height_dp,
    }
}

fn widget_to_wire(widget: Widget) -> types::Widget {
    types::Widget {
        widget_id: widget.widget_id,
        provider: Some(provider_to_wire(&widget.provider)),
        placement: Some(placement_to_wire(widget.placement)),
    }
}

fn widget_from_wire(widget: types::Widget) -> Widget {
    let placement = widget.placement.unwrap_or_default();
    Widget {
        widget_id: widget.widget_id,
        provider: widget.provider.map(provider_from_wire).unwrap_or_default(),
        placement: Placement {
            x: placement.x,
            y: placement.y,
            width_dp: placement.width_dp,
            height_dp: placement.height_dp,
        },
    }
}

fn frame_to_wire(frame: WidgetFrame) -> Body {
    Body::WidgetEvent(types::WidgetEvent {
        body: Some(types::widget_event::Body::Frame(types::WidgetFrame {
            widget_id: frame.widget_id,
            width: frame.width,
            height: frame.height,
            png: frame.png,
        })),
    })
}

fn notification_to_wire(event: NotificationEvent) -> Body {
    match event {
        NotificationEvent::Posted(n) => Body::Notification(types::Notification {
            key: n.key,
            package_name: n.package_name,
            app_label: n.app_label,
            title: n.title,
            text: n.text,
            icon_png: n.icon_png,
            importance: n.importance,
            high_priority: n.high_priority,
            clearable: n.clearable,
            when_millis: n.when_millis,
        }),
        NotificationEvent::Removed { key } => {
            Body::NotificationRemoved(types::NotificationRemoved { key })
        }
    }
}

/// A connection that carries input to the caller's Android, from
/// [`HostApiClient::stream_input`].
pub struct InputStream {
    connection: Box<dyn Connection>,
}

impl InputStream {
    /// Sends one pointer event. It returns once the event is on its way; nothing is
    /// awaited. An error means the connection has ended, and a new one is needed.
    pub fn send(&mut self, event: &PointerEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, Body::PointerEvent(event.to_wire()))
            .map_err(ClientError::Transport)
    }

    /// Sends one key event, the same way as [`InputStream::send`].
    pub fn send_key(&mut self, event: &KeyEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, Body::KeyEvent(event.to_wire())).map_err(ClientError::Transport)
    }

    /// Sends text composed or committed in a task's window, the same way as
    /// [`InputStream::send`].
    pub fn send_ime(&mut self, event: &ImeEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, event.to_wire()).map_err(ClientError::Transport)
    }

    /// Sends one event of any kind.
    pub fn send_input(&mut self, event: &InputEvent) -> Result<(), ClientError> {
        match event {
            InputEvent::Pointer(event) => self.send(event),
            InputEvent::Key(event) => self.send_key(event),
            InputEvent::Ime(event) => self.send_ime(event),
        }
    }
}
