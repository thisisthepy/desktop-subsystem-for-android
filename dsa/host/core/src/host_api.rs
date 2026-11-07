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
//! A client that watches print jobs ([`HostApiClient::watch_print_jobs`]) receives the
//! documents apps in the caller's Android print, each whole job to one watcher only, and prints
//! them on the host. It says what became of each with [`HostApiClient::report_print_job`], on
//! any connection, and the service passes that to the caller's Android.
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
use crate::adb_discovery::AutoConnectState;
use crate::apk::{self, InstallOutcome, InstallPart, InstalledPackage};
use crate::app_windows::{self, AppWindowSettings};
use crate::apps::{self, UninstallOutcome};
use crate::arm_translation::{self, GuestArmTranslation, Package, Source, Translator};
use crate::background::{
    self, BackgroundApp, BackgroundCommand, BackgroundEvent, BackgroundWatch, MediaCommand,
    MediaSession, Playback,
};
use crate::data_reset::{UserDataReset, RESET_STOP_WAIT};
use crate::device_identity::{
    ComputerSource, DeviceIdentity, DeviceIdentityStore, IdentityReport, ImageBuild,
};
use crate::diagnostics::HostFile;
use crate::drag::{self, DragChunk, DragItem, DragOffer, DragOut, DragRead};
use crate::files::{self, FileAction, FileTransfer};
use crate::gamepads::{GamepadEvent, GamepadInfo, GamepadRumble, GamepadState};
use crate::gapps::{self, GappsPackage, GuestGapps};
use crate::guest_log::{DiagnosticsEvent, LogDumpEvent, LogEntry, LogFilter, LogWatch};
use crate::ime::{ImeCursor, ImeEvent};
use crate::input::{InputEvent, PointerEvent};
use crate::keys::KeyEvent;
use crate::location::{Fix, LocationEvent, Wanted};
use crate::memory::{
    snapshot_from_wire, snapshot_to_wire, GuestMemoryReport, HostMemory, MemorySnapshot,
};
use crate::native_abi::{self, NativeLibraries, RunAsArmOutcome};
use crate::notifications::{self, Notification, NotificationAction, NotificationEvent};
use crate::print::{self, PrintEvent, PrintJob, PrintOutcome, PrintSettings};
use crate::resize_trace::{self, detail_requested, ResizeTrace, DETAIL_ENV};
use crate::session::{BackgroundRequest, Session, SessionError, SessionState};
use crate::settings::{Settings, SettingsStore};
use crate::system_settings::SystemSettings;
use crate::tasks::{
    AppInfo, Bounds, StatusBarAppearance, Task, TaskEvent, TaskOrientation, TaskRequest,
    TaskRequestError, TaskWatch,
};
use crate::widgets::{
    self, MemoryWidgetPlacements, Placement, Widget, WidgetError, WidgetEvent, WidgetFrame,
    WidgetPlacements, WidgetProvider, WidgetSize,
};
use dsa_protocol::control;

/// How long the service waits for Android to say whether a package is installed.
const PACKAGE_QUERY_WAIT: Duration = Duration::from_secs(10);

/// How long the service waits for Android to say what is dragged out of a window. The user
/// holds the button meanwhile, so it is short: a drag that does not start soon is no drag.
const DRAG_OUT_WAIT: Duration = Duration::from_secs(3);

/// How long the service waits for one piece of a dragged file. The app's content provider
/// may have to fetch or decode it, so it gets longer than a question.
const DRAG_READ_WAIT: Duration = Duration::from_secs(30);

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
    /// Turns the user's adb auto-connect setting on or off: whether, while developer mode is
    /// on, the port is attached to the user's adb server by itself.
    fn adb_auto_connect(&self, on: bool) {
        let _ = on;
    }
    /// Where auto-connect stands for this Android.
    fn adb_auto_connect_state(&self) -> AutoConnectState {
        AutoConnectState::Off
    }
    /// Sends a file to this Android, which asks the user which app opens or takes it.
    fn open_file(&self, file: &FileTransfer) -> Result<(), TaskRequestError> {
        let _ = file;
        Err(TaskRequestError::Failed(
            "this Android does not take files".into(),
        ))
    }
    /// Shows the user's host folders named in `folders` in this Android's shared storage
    /// while `on`, as the user's file sharing setting says.
    fn share_folders(&self, on: bool, folders: Vec<String>) {
        let _ = (on, folders);
    }
    /// The address (a WebDAV URL on the loopback address) the host's file manager opens this
    /// Android's shared storage at. It starts this Android when it is stopped.
    fn android_files(&self) -> Result<String, String> {
        Err("this Android does not show its files on the host".into())
    }
    /// As [`UserAndroid::android_files`], with the shared storage served under `token`, the
    /// first segment of every path, instead of one the session makes up. The files door uses
    /// the user's own key here, so the address it keeps does not change between sessions.
    fn android_files_with_token(&self, token: &str) -> Result<String, String> {
        let _ = token;
        Err("this Android does not show its files on the host".into())
    }
    /// The notifications of this Android from now on. `None` when it does not report them.
    fn watch_notifications(&self) -> Option<Receiver<NotificationEvent>> {
        None
    }
    /// The documents this Android prints from now on. `None` when it does not print on the
    /// host.
    fn watch_print_jobs(&self) -> Option<Receiver<PrintEvent>> {
        None
    }
    /// Tells this Android what became of one of its print jobs.
    fn print_result(&self, job_id: u32, outcome: &PrintOutcome) -> Result<(), TaskRequestError> {
        let _ = (job_id, outcome);
        Err(TaskRequestError::Failed(
            "this Android does not print on the host".into(),
        ))
    }
    /// Whether this Android wants the host's location, from what it wants now on. `None`
    /// when it takes no location from the host.
    fn watch_location(&self) -> Option<Receiver<Wanted>> {
        None
    }
    /// What the host's location service reported, for this Android while it wants a location.
    fn host_location(&self, event: LocationEvent) {
        let _ = event;
    }
    /// The user's host system settings, which this Android follows: its time zone, languages,
    /// regional format, theme and accent color.
    fn set_system_settings(&self, settings: SystemSettings) {
        let _ = settings;
    }
    /// A change to the user's game controllers, which this Android shows its apps as gamepads.
    fn gamepad(&self, event: GamepadEvent) {
        let _ = event;
    }
    /// The rumble effects apps in this Android play on the user's controllers, from now on.
    /// `None` when this Android takes no controllers.
    fn watch_gamepad_rumble(&self) -> Option<Receiver<GamepadRumble>> {
        None
    }
    /// The apps that run in the background in this Android and every change to them.
    /// `None` when it does not report them.
    fn watch_background(&self) -> Option<BackgroundWatch> {
        None
    }
    /// The apps that run in the background in this Android now, and its media sessions.
    fn background_apps(&self) -> (Vec<BackgroundApp>, Vec<MediaSession>) {
        (Vec::new(), Vec::new())
    }
    /// Passes what the user did with an app that runs in the background to this Android.
    fn background_action(&self, request: BackgroundRequest) -> Result<(), TaskRequestError> {
        let _ = request;
        Err(TaskRequestError::Failed(
            "this Android does not report the apps that run in the background".into(),
        ))
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
    /// Installs an APK as [`install_apk`](UserAndroid::install_apk) does, with its native code
    /// for `abi` (`x86_64` or `arm64-v8a`); `None` leaves the choice to Android. An Android
    /// that cannot be told an ABI installs the APK as Android chooses.
    fn install_apk_for(
        &self,
        name: &str,
        size: u64,
        abi: Option<&str>,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, InstallError> {
        let _ = abi;
        self.install_apk(name, size, content)
    }
    /// Installs an app made of several files, `parts`, whose bytes are read from `content` one
    /// after another, in this Android, with its native code for `abi` as in
    /// [`install_apk_for`](UserAndroid::install_apk_for), and returns how the install ended:
    /// the APKs go into one install session, and the expansion files into the app's folder
    /// under `Android/obb/`. `name` is the bundle the parts come from. One install runs at a
    /// time; a second waits.
    fn install_parts(
        &self,
        name: &str,
        parts: &[InstallPart],
        abi: Option<&str>,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, InstallError> {
        let _ = (name, parts, abi, content);
        Err(InstallError::Refused(
            "this Android does not install apps made of several APKs".into(),
        ))
    }
    /// Installs the app `package` again from the APKs it has in this Android, with its native
    /// code for arm64-v8a (`on`) or x86_64, keeping its data; with `erase_data`, also when
    /// that erases its data. Says how it went.
    fn set_run_as_arm(
        &self,
        package: &str,
        on: bool,
        erase_data: bool,
    ) -> Result<RunAsArmOutcome, TaskRequestError> {
        let _ = (package, on, erase_data);
        Err(TaskRequestError::Failed(
            "this Android cannot install its apps again".into(),
        ))
    }
    /// Sends an ARM translation package's system image, `size` bytes read from `content` whose
    /// SHA-256 is `sha256`, to this Android's disk, and returns where translation stands once
    /// the image is in place. The package is used from Android's next start.
    fn install_arm_translation(
        &self,
        size: u64,
        sha256: &[u8],
        content: &mut dyn Read,
    ) -> Result<GuestArmTranslation, InstallError> {
        let _ = (size, sha256, content);
        Err(InstallError::Refused(
            "this Android cannot take an ARM translation package".into(),
        ))
    }
    /// Removes the ARM translation package's image from this Android's disk.
    fn remove_arm_translation(&self) -> Result<GuestArmTranslation, InstallError> {
        Err(InstallError::Refused(
            "this Android cannot take an ARM translation package".into(),
        ))
    }
    /// Where ARM translation stands in this Android while it runs; `None` when it is not
    /// running or does not say.
    fn arm_translation(&self) -> Option<GuestArmTranslation> {
        None
    }
    /// Sends the Google apps' system image, `size` bytes read from `content` whose SHA-256 is
    /// `sha256`, to this Android's disk, and returns where the Google apps stand once the image
    /// is in place. The image is used from Android's next start.
    fn install_gapps(
        &self,
        size: u64,
        sha256: &[u8],
        content: &mut dyn Read,
    ) -> Result<GuestGapps, InstallError> {
        let _ = (size, sha256, content);
        Err(InstallError::Refused(
            "this Android cannot take Google's apps".into(),
        ))
    }
    /// Removes the Google apps' image from this Android's disk.
    fn remove_gapps(&self) -> Result<GuestGapps, InstallError> {
        Err(InstallError::Refused(
            "this Android cannot take Google's apps".into(),
        ))
    }
    /// Where the Google apps stand in this Android while it runs, with the Android version
    /// and processor it reports; `None` when it is not running or does not say.
    fn gapps(&self) -> Option<GuestGapps> {
        None
    }
    /// What this Android has installed of `package`, if anything: its version and signing
    /// certificates.
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        let _ = package;
        Err(TaskRequestError::Failed(
            "this Android cannot say which apps it has installed".into(),
        ))
    }
    /// Asks this Android to read its installed apps again, sizes included, and waits for the
    /// fresh list, which the watch of its tasks then carries.
    fn refresh_apps(&self) -> Result<(), TaskRequestError> {
        Err(TaskRequestError::Failed(
            "this Android cannot list its apps again".into(),
        ))
    }
    /// The pointer left a task's window with a button held (`out`): what an app is dragging
    /// out of it, which Android hands over, or `None` when nothing is being dragged.
    fn drag_out(&self, out: DragOut) -> Result<Option<DragOffer>, TaskRequestError> {
        let _ = out;
        Ok(None)
    }
    /// Part of a file of a drag this Android handed over.
    fn read_drag(&self, read: DragRead) -> Result<DragChunk, TaskRequestError> {
        let _ = read;
        Err(TaskRequestError::Failed(
            "this Android cannot send dragged files".into(),
        ))
    }
    /// Ends a drag this Android handed over, dropped or cancelled.
    fn end_drag(&self, drag_id: u32, dropped: bool) -> Result<(), TaskRequestError> {
        let _ = (drag_id, dropped);
        Ok(())
    }
    /// Opens Android's own settings of `package` in a window of its own, or the list of every
    /// app when it is `None`.
    fn open_app_settings(&self, package: Option<&str>) -> Result<(), TaskRequestError> {
        let _ = package;
        Err(TaskRequestError::Failed(
            "this Android cannot open its app settings".into(),
        ))
    }
    /// Uninstalls the user app `package` and says how it went.
    fn uninstall_app(&self, package: &str) -> Result<UninstallOutcome, TaskRequestError> {
        let _ = package;
        Err(TaskRequestError::Failed(
            "this Android cannot uninstall apps".into(),
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

/// Why an ARM translation package's image did not reach the session's Android, as the local
/// API reports it.
fn arm_install_error(error: SessionError) -> InstallError {
    match error {
        SessionError::NotActive(state) => {
            InstallError::NotRunning(format!("Android is {state}; start it and try again"))
        }
        SessionError::InstallRefused(why) => InstallError::Refused(why),
        other => InstallError::Refused(format!("Android could not be reached: {other}")),
    }
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
    fn watch_location(&self) -> Option<Receiver<Wanted>> {
        Some(Session::watch_location(self))
    }
    fn host_location(&self, event: LocationEvent) {
        Session::host_location(self, event)
    }
    fn set_system_settings(&self, settings: SystemSettings) {
        Session::set_system_settings(self, settings)
    }
    fn gamepad(&self, event: GamepadEvent) {
        Session::gamepad(self, event)
    }
    fn watch_gamepad_rumble(&self) -> Option<Receiver<GamepadRumble>> {
        Some(Session::watch_gamepad_rumble(self))
    }
    fn links_in_android(&self, on: bool) {
        Session::links_in_android(self, on)
    }
    fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        Session::developer_mode(self, on, port)
    }
    fn adb_auto_connect(&self, on: bool) {
        Session::adb_auto_connect(self, on)
    }
    fn adb_auto_connect_state(&self) -> AutoConnectState {
        Session::adb_auto_connect_state(self)
    }
    fn watch_links(&self) -> Option<Receiver<String>> {
        Some(Session::watch_links(self))
    }
    fn open_file(&self, file: &FileTransfer) -> Result<(), TaskRequestError> {
        Session::open_file(self, file).map_err(request_error)
    }
    fn share_folders(&self, on: bool, folders: Vec<String>) {
        Session::share_folders(self, on, folders)
    }
    fn android_files(&self) -> Result<String, String> {
        Session::android_files(self)
    }
    fn android_files_with_token(&self, token: &str) -> Result<String, String> {
        Session::android_files_with_token(self, token)
    }
    fn watch_notifications(&self) -> Option<Receiver<NotificationEvent>> {
        Some(Session::watch_notifications(self))
    }
    fn notification_action(&self, action: NotificationAction) -> Result<(), TaskRequestError> {
        Session::notification_action(self, action).map_err(request_error)
    }
    fn watch_print_jobs(&self) -> Option<Receiver<PrintEvent>> {
        Some(Session::watch_print_jobs(self))
    }
    fn print_result(&self, job_id: u32, outcome: &PrintOutcome) -> Result<(), TaskRequestError> {
        Session::print_result(self, job_id, outcome).map_err(request_error)
    }
    fn watch_widgets(&self) -> Option<crate::session::WidgetWatch> {
        Some(Session::watch_widgets(self))
    }
    fn watch_background(&self) -> Option<BackgroundWatch> {
        Some(Session::watch_background(self))
    }
    fn background_apps(&self) -> (Vec<BackgroundApp>, Vec<MediaSession>) {
        Session::background_apps(self)
    }
    fn background_action(&self, request: BackgroundRequest) -> Result<(), TaskRequestError> {
        Session::background_action(self, request).map_err(request_error)
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
        UserAndroid::install_apk_for(self, name, size, None, content)
    }
    fn install_apk_for(
        &self,
        name: &str,
        size: u64,
        abi: Option<&str>,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, InstallError> {
        Session::install_apk_for(self, name, size, abi, content).map_err(install_error)
    }
    fn install_parts(
        &self,
        name: &str,
        parts: &[InstallPart],
        abi: Option<&str>,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, InstallError> {
        Session::install_parts(self, name, parts, abi, content).map_err(install_error)
    }
    fn set_run_as_arm(
        &self,
        package: &str,
        on: bool,
        erase_data: bool,
    ) -> Result<RunAsArmOutcome, TaskRequestError> {
        Session::set_run_as_arm(self, package, on, erase_data, native_abi::REINSTALL_WAIT)
            .map_err(request_error)
    }
    fn package_state(&self, package: &str) -> Result<Option<InstalledPackage>, TaskRequestError> {
        Session::package_state(self, package, PACKAGE_QUERY_WAIT).map_err(request_error)
    }
    fn drag_out(&self, out: DragOut) -> Result<Option<DragOffer>, TaskRequestError> {
        Session::drag_out(self, out, DRAG_OUT_WAIT).map_err(request_error)
    }
    fn read_drag(&self, read: DragRead) -> Result<DragChunk, TaskRequestError> {
        Session::read_drag(self, read, DRAG_READ_WAIT).map_err(request_error)
    }
    fn end_drag(&self, drag_id: u32, dropped: bool) -> Result<(), TaskRequestError> {
        Session::end_drag(self, drag_id, dropped).map_err(request_error)
    }
    fn install_arm_translation(
        &self,
        size: u64,
        sha256: &[u8],
        content: &mut dyn Read,
    ) -> Result<GuestArmTranslation, InstallError> {
        Session::install_arm_translation(self, size, sha256, content).map_err(arm_install_error)
    }
    fn remove_arm_translation(&self) -> Result<GuestArmTranslation, InstallError> {
        Session::remove_arm_translation(self).map_err(arm_install_error)
    }
    fn arm_translation(&self) -> Option<GuestArmTranslation> {
        Session::arm_translation(self).ok()
    }
    fn install_gapps(
        &self,
        size: u64,
        sha256: &[u8],
        content: &mut dyn Read,
    ) -> Result<GuestGapps, InstallError> {
        Session::install_gapps(self, size, sha256, content).map_err(arm_install_error)
    }
    fn remove_gapps(&self) -> Result<GuestGapps, InstallError> {
        Session::remove_gapps(self).map_err(arm_install_error)
    }
    fn gapps(&self) -> Option<GuestGapps> {
        Session::gapps(self).ok()
    }
    fn refresh_apps(&self) -> Result<(), TaskRequestError> {
        Session::refresh_apps(self, apps::REFRESH_WAIT).map_err(request_error)
    }
    fn open_app_settings(&self, package: Option<&str>) -> Result<(), TaskRequestError> {
        Session::open_app_settings(self, package).map_err(request_error)
    }
    fn uninstall_app(&self, package: &str) -> Result<UninstallOutcome, TaskRequestError> {
        Session::uninstall_app(self, package, apps::UNINSTALL_WAIT).map_err(request_error)
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

/// What an install that did not end with Android's answer tells a client.
fn install_error(error: SessionError) -> InstallError {
    match error {
        SessionError::NotActive(state) => InstallError::NotRunning(format!(
            "Android is {state}; start it and install the app again"
        )),
        SessionError::InstallRefused(why) => InstallError::Refused(why),
        other => InstallError::Refused(format!("the app did not reach Android: {other}")),
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
        status_bar_appearance: match task.status_bar {
            StatusBarAppearance::Unspecified => types::StatusBarAppearance::Unspecified,
            StatusBarAppearance::Light => types::StatusBarAppearance::Light,
            StatusBarAppearance::Dark => types::StatusBarAppearance::Dark,
        } as i32,
        status_bar_color: task.status_bar_color,
        orientation: match task.orientation {
            TaskOrientation::Unspecified => types::TaskOrientation::Unspecified,
            TaskOrientation::Portrait => types::TaskOrientation::Portrait,
            TaskOrientation::Landscape => types::TaskOrientation::Landscape,
        } as i32,
        fullscreen: task.fullscreen,
        picture_in_picture: task.picture_in_picture,
    }
}

fn task_from_wire(task: types::Task) -> Task {
    let status_bar = match task.status_bar_appearance() {
        types::StatusBarAppearance::Unspecified => StatusBarAppearance::Unspecified,
        types::StatusBarAppearance::Light => StatusBarAppearance::Light,
        types::StatusBarAppearance::Dark => StatusBarAppearance::Dark,
    };
    let orientation = match task.orientation() {
        types::TaskOrientation::Unspecified => TaskOrientation::Unspecified,
        types::TaskOrientation::Portrait => TaskOrientation::Portrait,
        types::TaskOrientation::Landscape => TaskOrientation::Landscape,
    };
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
        status_bar,
        status_bar_color: task.status_bar_color,
        orientation,
        fullscreen: task.fullscreen,
        picture_in_picture: task.picture_in_picture,
    }
}

/// The controller change a local API message carries; `None` when it carries none.
fn gamepad_from_wire(event: types::GamepadEvent) -> Option<GamepadEvent> {
    use types::gamepad_event::Event;
    let clamp = |value: i32| value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16;
    let trigger = |value: u32| value.min(255) as u8;
    let id = |value: u32| value.min(u32::from(u16::MAX)) as u16;
    Some(match event.event? {
        Event::Connected(pad) => GamepadEvent::Connected(GamepadInfo {
            pad_id: pad.pad_id,
            name: pad.name,
            vendor_id: id(pad.vendor_id),
            product_id: id(pad.product_id),
            rumble: pad.rumble,
        }),
        Event::Disconnected(pad) => GamepadEvent::Disconnected { pad_id: pad.pad_id },
        Event::State(state) => GamepadEvent::State(GamepadState {
            pad_id: state.pad_id,
            buttons: state.buttons & crate::gamepads::buttons::ALL,
            left_x: clamp(state.left_x),
            left_y: clamp(state.left_y),
            right_x: clamp(state.right_x),
            right_y: clamp(state.right_y),
            left_trigger: trigger(state.left_trigger),
            right_trigger: trigger(state.right_trigger),
        }),
    })
}

/// The local API message that carries a controller change.
fn gamepad_to_wire(event: &GamepadEvent) -> types::GamepadEvent {
    use types::gamepad_event::Event;
    let event = match event {
        GamepadEvent::Connected(info) => Event::Connected(types::GamepadConnected {
            pad_id: info.pad_id,
            name: info.name.clone(),
            vendor_id: u32::from(info.vendor_id),
            product_id: u32::from(info.product_id),
            rumble: info.rumble,
        }),
        GamepadEvent::Disconnected { pad_id } => {
            Event::Disconnected(types::GamepadDisconnected { pad_id: *pad_id })
        }
        GamepadEvent::State(state) => Event::State(types::GamepadState {
            pad_id: state.pad_id,
            buttons: state.buttons,
            left_x: i32::from(state.left_x),
            left_y: i32::from(state.left_y),
            right_x: i32::from(state.right_x),
            right_y: i32::from(state.right_y),
            left_trigger: u32::from(state.left_trigger),
            right_trigger: u32::from(state.right_trigger),
        }),
    };
    types::GamepadEvent { event: Some(event) }
}

fn app_to_wire(app: AppInfo) -> types::AppInfo {
    types::AppInfo {
        package_name: app.package_name,
        label: app.label,
        icon_png: app.icon_png,
        version_name: app.version_name,
        version_code: app.version_code,
        first_install_time_ms: app.first_install_time_ms,
        last_update_time_ms: app.last_update_time_ms,
        system_app: app.system_app,
        app_bytes: app.app_bytes,
        data_bytes: app.data_bytes,
        cache_bytes: app.cache_bytes,
        native_libraries: app.native_libraries.to_wire(),
        primary_abi: app.primary_abi,
    }
}

fn app_from_wire(app: types::AppInfo) -> AppInfo {
    AppInfo {
        package_name: app.package_name,
        label: app.label,
        icon_png: app.icon_png,
        version_name: app.version_name,
        version_code: app.version_code,
        first_install_time_ms: app.first_install_time_ms,
        last_update_time_ms: app.last_update_time_ms,
        system_app: app.system_app,
        app_bytes: app.app_bytes,
        data_bytes: app.data_bytes,
        cache_bytes: app.cache_bytes,
        primary_abi: app.primary_abi,
        native_libraries: NativeLibraries::from_wire(&app.native_libraries),
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

/// How long a client waits for the service to say where the Google apps stand: the service
/// asks the running Android, and its agent for the device's Android ID.
const GAPPS_ANSWER_WAIT: Duration = Duration::from_secs(30);

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
    /// The caller's network as it stands now; `None` from a service that does not report it.
    pub network: Option<NetworkStatus>,
    /// Where adb auto-connect stands for the caller's Android. Off from a service that does
    /// not report it.
    pub adb_auto_connect_state: AutoConnectState,
}

/// How a user's Android reaches the network.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkStatus {
    /// `nat`, `advanced` or `off`.
    pub mode: String,
    /// The guest's addresses, IPv4 and IPv6.
    pub addresses: Vec<String>,
    pub dns_servers: Vec<String>,
    pub gateway: String,
    /// Why the network is not what the settings ask for, if it is not.
    pub problem: Option<String>,
}

impl NetworkStatus {
    /// Behind the NAT network the service makes for the VM.
    pub const NAT: &'static str = "nat";
    /// On the host's LAN as a device of its own.
    pub const ADVANCED: &'static str = "advanced";
    /// No network, because Android is not running.
    pub const OFF: &'static str = "off";

    pub fn off() -> NetworkStatus {
        NetworkStatus {
            mode: NetworkStatus::OFF.into(),
            ..NetworkStatus::default()
        }
    }

    fn to_wire(&self) -> types::NetworkStatus {
        types::NetworkStatus {
            mode: self.mode.clone(),
            addresses: self.addresses.clone(),
            dns_servers: self.dns_servers.clone(),
            gateway: self.gateway.clone(),
            problem: self.problem.clone().unwrap_or_default(),
        }
    }

    fn from_wire(status: types::NetworkStatus) -> NetworkStatus {
        NetworkStatus {
            mode: status.mode,
            addresses: status.addresses,
            dns_servers: status.dns_servers,
            gateway: status.gateway,
            problem: Some(status.problem).filter(|p| !p.is_empty()),
        }
    }
}

/// Says how a user's Android reaches the network now, given the user's settings: `None` while
/// it has no network (it is not running). The platform asks its network service.
pub type NetworkSource = Arc<dyn Fn(&str, &Settings) -> Option<NetworkStatus> + Send + Sync>;

/// Where ARM translation stands for a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmTranslationStatus {
    /// The package the user installed (their own or Google's), if any.
    pub package: Option<Package>,
    /// The default package of the image the user's Android starts from; only an x86_64 image
    /// has one.
    pub default: Option<Package>,
    /// The translator the user's Android uses with these settings, from its next start.
    pub translator: Option<Translator>,
    /// The user's `arm-translation` setting.
    pub enabled: bool,
    /// What the user's running Android says; `None` when it is not running.
    pub guest: Option<GuestArmTranslation>,
    /// What is installed and switched on applies only once Android starts again.
    pub restart_needed: bool,
}

impl ArmTranslationStatus {
    fn to_wire(&self) -> types::ArmTranslation {
        let guest = self.guest.clone().unwrap_or_default();
        types::ArmTranslation {
            package: self.package.as_ref().map(package_to_wire),
            enabled: self.enabled,
            guest_known: self.guest.is_some(),
            guest_installed: guest.installed,
            guest_active: guest.active,
            abilist: guest.abilist,
            restart_needed: self.restart_needed,
            default_package: self.default.as_ref().map(package_to_wire),
            translator: self
                .translator
                .as_ref()
                .map(|translator| translator.source.as_str())
                .unwrap_or_default()
                .to_string(),
        }
    }

    fn from_wire(status: types::ArmTranslation) -> ArmTranslationStatus {
        let package = status.package.map(package_from_wire);
        let default = status.default_package.map(package_from_wire);
        let translator = Source::parse(&status.translator).and_then(|source| {
            let described = match source {
                Source::Default => default.clone(),
                Source::User | Source::Google => package.clone(),
            };
            described.map(|package| Translator { source, package })
        });
        ArmTranslationStatus {
            package,
            default,
            translator,
            enabled: status.enabled,
            guest: status.guest_known.then_some(GuestArmTranslation {
                installed: status.guest_installed,
                active: status.guest_active,
                abilist: status.abilist,
            }),
            restart_needed: status.restart_needed,
        }
    }
}

/// Where Google Play stands for a user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GappsStatus {
    /// The build the user installed, if any.
    pub package: Option<GappsPackage>,
    /// What the user's running Android says; `None` when it is not running.
    pub guest: Option<GuestGapps>,
    /// What is installed applies only once Android starts again.
    pub restart_needed: bool,
}

impl GappsStatus {
    fn to_wire(&self) -> types::Gapps {
        let guest = self.guest.clone().unwrap_or_default();
        types::Gapps {
            package: self.package.as_ref().map(|package| types::GappsPackage {
                variant: package.variant.clone(),
                version: package.version.clone(),
                android_version: package.android_version.clone(),
                sdk: package.sdk,
                arch: package.arch.clone(),
                file: package.file.clone(),
                source: package.source.clone(),
                md5: package.md5.clone(),
            }),
            guest_known: self.guest.is_some(),
            guest_installed: guest.installed,
            guest_active: guest.active,
            sdk: guest.sdk,
            abi: guest.abi,
            abilist: guest.abilist,
            device_id: guest.device_id,
            restart_needed: self.restart_needed,
        }
    }

    fn from_wire(status: types::Gapps) -> GappsStatus {
        GappsStatus {
            package: status.package.map(|package| GappsPackage {
                variant: package.variant,
                version: package.version,
                android_version: package.android_version,
                sdk: package.sdk,
                arch: package.arch,
                file: package.file,
                source: package.source,
                md5: package.md5,
            }),
            guest: status.guest_known.then_some(GuestGapps {
                installed: status.guest_installed,
                active: status.guest_active,
                sdk: status.sdk,
                abi: status.abi,
                abilist: status.abilist,
                device_id: status.device_id,
            }),
            restart_needed: status.restart_needed,
        }
    }
}

fn gapps_progress_to_wire(progress: gapps::Progress) -> types::GappsProgress {
    let stage = match progress.stage {
        gapps::Stage::Listing => types::GappsStage::Listing,
        gapps::Stage::Downloading => types::GappsStage::Downloading,
        gapps::Stage::Building => types::GappsStage::Building,
        gapps::Stage::Sending => types::GappsStage::Sending,
    };
    types::GappsProgress {
        stage: stage as i32,
        done: progress.done,
        total: progress.total,
    }
}

fn gapps_progress_from_wire(progress: &types::GappsProgress) -> Option<gapps::Progress> {
    let stage = match types::GappsStage::try_from(progress.stage).ok()? {
        types::GappsStage::Listing => gapps::Stage::Listing,
        types::GappsStage::Downloading => gapps::Stage::Downloading,
        types::GappsStage::Building => gapps::Stage::Building,
        types::GappsStage::Sending => gapps::Stage::Sending,
        types::GappsStage::Unspecified => return None,
    };
    Some(gapps::Progress {
        stage,
        done: progress.done,
        total: progress.total,
    })
}

fn package_to_wire(package: &Package) -> types::ArmTranslationPackage {
    types::ArmTranslationPackage {
        name: package.name.clone(),
        version: package.version.clone(),
        library: package.library.clone(),
        abis: package.abis.clone(),
        license: package.license.clone(),
    }
}

fn package_from_wire(package: types::ArmTranslationPackage) -> Package {
    Package {
        name: package.name,
        version: package.version,
        library: package.library,
        abis: package.abis,
        license: package.license,
    }
}
/// How long the service waits for the fresh memory report it asks Android for.
const MEMORY_WAIT: Duration = Duration::from_secs(3);

/// The service end of the local API.
pub struct HostApiService {
    androids: Arc<dyn UserAndroids>,
    program: Option<Arc<dyn UpdateProgram>>,
    settings: Option<(Arc<dyn SettingsStore>, ServiceFacts)>,
    android_version: Option<AndroidVersionSource>,
    /// Reports each user's network with the settings.
    network: Option<NetworkSource>,
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
    /// Where each user's device identity is kept, and what the image's build says.
    identities: Option<(Arc<dyn DeviceIdentityStore>, ImageBuildSource)>,
    /// Reads what the computer says it is, for the identity's defaults; without it they are
    /// DSA's own.
    computer: Option<ComputerSource>,
    /// What puts a fresh data disk in place of a user's, for a data reset.
    data_reset: Option<Arc<dyn UserDataReset>>,
    /// How long a data reset waits for the user's Android to stop.
    data_reset_wait: Duration,
    /// Which Android version runs, and choosing another.
    versions: Option<Arc<dyn crate::update::versions::AndroidVersions>>,
    /// Where Google Play is downloaded from, and where it is made ready.
    gapps: Option<gapps::Source>,
    /// The default ARM translation package of the image each user's Android starts from.
    default_arm_translation: Option<DefaultArmTranslationSource>,
}

/// Says what the build of the image a user's Android starts from is, by user.
pub type ImageBuildSource = Arc<dyn Fn(&str) -> Option<ImageBuild> + Send + Sync>;

/// Says which default ARM translation package the image a user's Android starts from carries,
/// by user: `None` for an image that has none, as every arm64 image.
pub type DefaultArmTranslationSource = Arc<dyn Fn(&str) -> Option<Package> + Send + Sync>;

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
            network: None,
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
            identities: None,
            computer: None,
            data_reset: None,
            data_reset_wait: RESET_STOP_WAIT,
            versions: None,
            gapps: None,
            default_arm_translation: None,
        }
    }

    /// Reports the default ARM translation package of the image each user's Android starts
    /// from, as `source` gives it, which their Android uses while they have installed none.
    pub fn with_default_arm_translation(
        mut self,
        source: DefaultArmTranslationSource,
    ) -> HostApiService {
        self.default_arm_translation = Some(source);
        self
    }

    /// Installs Google Play for users who ask, downloading it through `source`.
    pub fn with_gapps(mut self, source: gapps::Source) -> HostApiService {
        self.gapps = Some(source);
        self
    }

    /// Lets users see the Android version and choose another through `versions`.
    pub fn with_android_versions(
        mut self,
        versions: Arc<dyn crate::update::versions::AndroidVersions>,
    ) -> HostApiService {
        self.versions = Some(versions);
        self
    }

    /// Resets a user's Android data with `reset`, which puts a fresh data disk in place of
    /// theirs once their Android has stopped.
    pub fn with_data_reset(mut self, reset: Arc<dyn UserDataReset>) -> HostApiService {
        self.data_reset = Some(reset);
        self
    }

    /// Waits at most `wait` for a user's Android to stop before a data reset gives up.
    pub fn with_data_reset_wait(mut self, wait: Duration) -> HostApiService {
        self.data_reset_wait = wait;
        self
    }

    /// Keeps each user's device identity in `store`; `build` says what the image's own build
    /// is, for the values the user did not choose.
    pub fn with_device_identities(
        mut self,
        store: Arc<dyn DeviceIdentityStore>,
        build: ImageBuildSource,
    ) -> HostApiService {
        self.identities = Some((store, build));
        self
    }

    /// Takes the defaults of the device identity from what `computer` reads: the computer's
    /// manufacturer, brand, model, model name, name and chip.
    pub fn with_computer_identity(mut self, computer: ComputerSource) -> HostApiService {
        self.computer = Some(computer);
        self
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

    /// Reports each user's network, as `source` gives it, with the settings.
    pub fn with_network(mut self, source: NetworkSource) -> HostApiService {
        self.network = Some(source);
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
            if let Some(request::Body::WatchGamepads(_)) = request.body {
                return self.watch_gamepads(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchNotifications(_)) = request.body {
                return self.watch_notifications(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchPrintJobs(_)) = request.body {
                return self.watch_print_jobs(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchLocation(_)) = request.body {
                return self.watch_location(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchWidgets(_)) = request.body {
                return self.watch_widgets(connection, &peer, request.id);
            }
            if let Some(request::Body::WatchBackgroundApps(_)) = request.body {
                return self.watch_background(connection, &peer, request.id);
            }
            if let Some(request::Body::InstallApk(install)) = &request.body {
                return self.install_apk(connection, &peer, request.id, install.clone());
            }
            if let Some(request::Body::InstallArmTranslation(install)) = &request.body {
                return self.install_arm_translation(
                    connection,
                    &peer,
                    request.id,
                    install.clone(),
                );
            }
            if let Some(request::Body::InstallGapps(_)) = &request.body {
                return self.install_gapps(connection, &peer, request.id);
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
            package: open.package,
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
                Ok(Envelope {
                    body: Some(Body::ImeDeleteSurrounding(delete)),
                }) => ImeEvent::from_wire_delete(&delete)
                    .map(InputEvent::Ime)
                    .map_err(|why| format!("dropped a deletion: {why}")),
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

    /// Answers a WatchGamepads request, then passes the changes to the client's controllers to
    /// its Android, and the rumble its Android plays to the client, until the client goes
    /// away. The controllers it connected are disconnected then. What the client sends that is
    /// not a controller change closes the connection; a frame that cannot be read is dropped.
    fn watch_gamepads(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => match android.watch_gamepad_rumble() {
                Some(rumble) => Ok((android, rumble)),
                None => Err(error(
                    ErrorCode::Unavailable,
                    "this Android takes no game controllers".into(),
                )),
            },
        };
        let (android, rumble) = match watched {
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
        let mut connected: Vec<u32> = Vec::new();
        loop {
            while let Ok(effect) = rumble.try_recv() {
                if !connected.contains(&effect.pad_id) {
                    continue;
                }
                let frame = Body::GamepadRumble(types::GamepadRumble {
                    pad_id: effect.pad_id,
                    strong: u32::from(effect.strong),
                    weak: u32::from(effect.weak),
                    duration_ms: effect.duration_ms,
                });
                if send(&mut *connection, frame).is_err() {
                    break;
                }
            }
            let frame = match connection.recv(Some(CLIPBOARD_POLL)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(_) => break,
            };
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::GamepadEvent(event)),
                }) => {
                    let Some(event) = gamepad_from_wire(event) else {
                        continue;
                    };
                    match &event {
                        GamepadEvent::Connected(info) if !connected.contains(&info.pad_id) => {
                            connected.push(info.pad_id)
                        }
                        GamepadEvent::Disconnected { pad_id } => {
                            connected.retain(|known| known != pad_id)
                        }
                        _ => {}
                    }
                    android.gamepad(event);
                }
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than a controller \
                         change on a connection that carries the controllers; closing its \
                         connection",
                        peer.user
                    );
                    connection.close();
                    break;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame on a connection \
                         that carries the controllers ({error}); dropping it",
                        peer.user
                    );
                }
            }
        }
        // The controllers go with the window manager that read them.
        for pad_id in connected {
            android.gamepad(GamepadEvent::Disconnected { pad_id });
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

    /// Answers a WatchLocation request, then tells the client each time its Android starts or
    /// stops wanting the location, and passes the positions and statuses the client sends to
    /// its Android, until the client goes away. A position that is not on Earth is dropped,
    /// and so is a frame that cannot be read; anything else the client sends closes the
    /// connection.
    fn watch_location(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => match android.watch_location() {
                Some(watch) => Ok((android, watch)),
                None => Err(error(
                    ErrorCode::Unavailable,
                    "this Android takes no location from the host".into(),
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
                    Ok(wanted) => {
                        let frame = Body::LocationWanted(wanted.to_host_api());
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
                    body: Some(Body::LocationFix(fix)),
                }) => match Fix::from_host_api(&fix) {
                    Ok(fix) => android.host_location(LocationEvent::Fix(fix)),
                    Err(why) => eprintln!(
                        "local API: dropped a location from a client of {}: {why}",
                        peer.user
                    ),
                },
                Ok(Envelope {
                    body: Some(Body::LocationStatus(status)),
                }) => match LocationEvent::from_host_api_status(&status) {
                    Ok(event) => android.host_location(event),
                    Err(why) => eprintln!(
                        "local API: dropped a location status from a client of {}: {why}",
                        peer.user
                    ),
                },
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than a location on a \
                         connection that carries the location; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame on a connection \
                         that carries the location ({error}); dropping it",
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

    /// Answers a WatchPrintJobs request, then passes the print jobs of the client's Android to
    /// it until the client goes away: each job as its offer and its document in pieces, and
    /// each job withdrawn. The client sends nothing more: anything it sends closes the
    /// connection, except a frame that cannot be read, which is dropped. A job that cannot be
    /// sent fails in Android, so it does not wait for an answer that never comes.
    fn watch_print_jobs(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => match android.watch_print_jobs() {
                Some(watch) => Ok((android, watch)),
                None => Err(error(
                    ErrorCode::Unavailable,
                    "this Android does not print on the host".into(),
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
                    Ok(PrintEvent::Job(job)) => {
                        let job_id = job.job_id;
                        if send_print_job(&mut *connection, &job).is_err() {
                            let _ = android.print_result(
                                job_id,
                                &PrintOutcome::Failed(
                                    "the desktop session that was to print it went away".into(),
                                ),
                            );
                            return;
                        }
                    }
                    Ok(PrintEvent::Withdrawn { job_id }) => {
                        let withdrawn = types::PrintJobWithdrawn { job_id };
                        let withdrawn = Body::PrintJobWithdrawn(withdrawn);
                        if send(&mut *connection, withdrawn).is_err() {
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
                         print jobs; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame on a connection \
                         that watches print jobs ({error}); dropping it",
                        peer.user
                    );
                }
            }
        }
    }

    /// Passes what became of a print job to the caller's Android.
    fn report_print_job(&self, peer: &Peer, report: types::ReportPrintJob) -> response::Body {
        let outcome = match types::PrintOutcome::try_from(report.outcome) {
            Ok(types::PrintOutcome::Printed) => PrintOutcome::Printed,
            Ok(types::PrintOutcome::Canceled) => PrintOutcome::Canceled,
            Ok(types::PrintOutcome::Failed) => PrintOutcome::Failed(report.message),
            _ => {
                return error(
                    ErrorCode::UnknownRequest,
                    "a print job is printed, canceled or failed".into(),
                )
            }
        };
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.print_result(report.job_id, &outcome) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
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

    fn open_app_settings(&self, peer: &Peer, package: &str) -> response::Body {
        if !package.is_empty() && !apk::valid_package_name(package) {
            return not_a_package(package);
        }
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.open_app_settings(Some(package).filter(|p| !p.is_empty())) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn uninstall_app(&self, peer: &Peer, package: &str) -> response::Body {
        if !apk::valid_package_name(package) {
            return not_a_package(package);
        }
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.uninstall_app(package) {
            Ok(outcome) => response::Body::UninstallResult(outcome.to_wire()),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn set_run_as_arm(&self, peer: &Peer, set: types::SetRunAsArm) -> response::Body {
        if !apk::valid_package_name(&set.package_name) {
            return not_a_package(&set.package_name);
        }
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.set_run_as_arm(&set.package_name, set.on, set.erase_data) {
            Ok(outcome) => response::Body::RunAsArmResult(outcome.to_wire()),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn drag_out(&self, peer: &Peer, out: types::DragOut) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        if !(out.x.is_finite() && out.y.is_finite()) {
            return error(
                ErrorCode::UnknownRequest,
                "a drag leaves a window at a place on it".to_string(),
            );
        }
        match android.drag_out(DragOut {
            task_id: out.task_id,
            x: out.x,
            y: out.y,
            tool: out.tool,
            pointer_id: out.pointer_id,
        }) {
            Ok(offer) => response::Body::DragOffer(match offer {
                Some(offer) => types::DragOffer {
                    drag_id: offer.drag_id,
                    items: offer.items.iter().map(DragItem::to_wire).collect(),
                },
                None => types::DragOffer::default(),
            }),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn read_drag(&self, peer: &Peer, read: types::ReadDrag) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.read_drag(DragRead {
            drag_id: read.drag_id,
            item: read.item,
            offset: read.offset,
            length: read.length.clamp(1, drag::READ_BYTES),
        }) {
            Ok(chunk) => response::Body::DragData(types::DragData {
                data: chunk.data,
                end: chunk.end,
            }),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn end_drag(&self, peer: &Peer, end: types::EndDrag) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.end_drag(end.drag_id, end.dropped) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
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

    /// The apps that run in the background in the caller's Android and its media sessions.
    /// An Android that is not running has none.
    fn list_background(&self, peer: &Peer) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        let (apps, media) = android.background_apps();
        response::Body::BackgroundApps(background_to_wire(apps, media))
    }

    /// Answers a WatchBackgroundApps request with the apps there are now, then passes every
    /// change until the client goes away. The client sends nothing more: anything it sends
    /// closes the connection, except a frame that cannot be read, which is dropped.
    fn watch_background(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watched = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => android.watch_background().ok_or_else(|| {
                error(
                    ErrorCode::Unavailable,
                    "this Android does not report the apps that run in the background".into(),
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
            body: Some(response::Body::BackgroundApps(background_to_wire(
                watch.apps,
                watch.media,
            ))),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            loop {
                match watch.events.try_recv() {
                    Ok(event) => {
                        if send(&mut *connection, background_event_to_wire(event)).is_err() {
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
                         the apps that run in the background; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame on a connection \
                         that watches the apps that run in the background ({error}); dropping it",
                        peer.user
                    );
                }
            }
        }
    }

    /// Passes what the user did with an app that runs in the background to the caller's
    /// Android.
    fn act_on_background(&self, peer: &Peer, act: types::ActOnBackgroundApp) -> response::Body {
        if !background::valid_package(&act.package_name) {
            return error(
                ErrorCode::UnknownRequest,
                "the package name is not an Android package name".into(),
            );
        }
        let command = match types::BackgroundAppCommand::try_from(act.command) {
            Ok(types::BackgroundAppCommand::Open) => BackgroundCommand::Open,
            Ok(types::BackgroundAppCommand::Action) => match u32::try_from(act.action) {
                Ok(index) if (index as usize) < background::MAX_ACTIONS => {
                    BackgroundCommand::Action(index)
                }
                _ => {
                    return error(
                        ErrorCode::UnknownRequest,
                        format!(
                            "a notification has at most {} actions; there is no action {}",
                            background::MAX_ACTIONS,
                            act.action
                        ),
                    )
                }
            },
            Ok(types::BackgroundAppCommand::Stop) => BackgroundCommand::Stop,
            _ => {
                return error(
                    ErrorCode::UnknownRequest,
                    "an app that runs in the background can be opened, sent one of its \
                     notification's actions, or stopped"
                        .into(),
                )
            }
        };
        self.background_request(peer, BackgroundRequest::App(act.package_name, command))
    }

    /// Runs a media key's command on a media session of the caller's Android.
    fn control_media(&self, peer: &Peer, control: types::ControlMedia) -> response::Body {
        if !background::valid_package(&control.package_name) {
            return error(
                ErrorCode::UnknownRequest,
                "the package name is not an Android package name".into(),
            );
        }
        let command = match types::MediaCommand::try_from(control.command) {
            Ok(types::MediaCommand::Play) => MediaCommand::Play,
            Ok(types::MediaCommand::Pause) => MediaCommand::Pause,
            Ok(types::MediaCommand::Next) => MediaCommand::Next,
            Ok(types::MediaCommand::Previous) => MediaCommand::Previous,
            Ok(types::MediaCommand::Stop) => MediaCommand::Stop,
            _ => {
                return error(
                    ErrorCode::UnknownRequest,
                    "a media command is play, pause, next, previous or stop".into(),
                )
            }
        };
        self.background_request(
            peer,
            BackgroundRequest::Media(control.package_name, command),
        )
    }

    fn background_request(&self, peer: &Peer, request: BackgroundRequest) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.background_action(request) {
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
        let parts: Vec<InstallPart> = install
            .parts
            .iter()
            .cloned()
            .map(InstallPart::from_wire)
            .collect();
        let checked = if parts.is_empty() {
            apk::check_name(&install.name).and_then(|()| apk::check_size(install.size))
        } else {
            apk::check_parts(&install.name, &parts).and_then(|total| {
                if total == install.size {
                    Ok(())
                } else {
                    Err(format!(
                        "the files of {:?} add up to {total} bytes, not the {} announced",
                        install.name, install.size
                    ))
                }
            })
        };
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
        // x86_64 first: arm64-v8a only when the APK's x86_64 libraries leave out some of its
        // arm64 ones and the running Android translates ARM code. Android is asked only then.
        let libraries = NativeLibraries::from_wire(&install.native_libraries);
        let abi = native_abi::choose(&libraries, || {
            android
                .arm_translation()
                .is_some_and(|guest| native_abi::translates_arm64(&guest.abilist))
        });
        answer(&mut *connection, response::Body::Done(types::Done {}));
        eprintln!(
            "install: {} asked to install {} ({} bytes){}",
            peer.user,
            install.name,
            install.size,
            abi.map(|abi| format!(" as {abi}")).unwrap_or_default()
        );
        let mut content = ApkChunks {
            connection: &mut *connection,
            user: &peer.user,
            left: install.size,
            piece: Vec::new(),
            at: 0,
        };
        let installed = if parts.is_empty() {
            android.install_apk_for(&install.name, install.size, abi, &mut content)
        } else {
            android.install_parts(&install.name, &parts, abi, &mut content)
        };
        let body = match installed {
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
            Some(request::Body::ListAppWindowSettings(_)) => self.app_window_settings(peer, None),
            Some(request::Body::SetAppWindowSettings(set)) => {
                self.app_window_settings(peer, Some(set.app.unwrap_or_default()))
            }
            Some(request::Body::SetSystemSettings(set)) => {
                match self.androids.android(&peer.user) {
                    Err(why) => error(ErrorCode::Unavailable, why),
                    Ok(android) => {
                        let settings = SystemSettings::from_wire(&set.settings.unwrap_or_default());
                        android.set_system_settings(settings);
                        response::Body::Done(types::Done {})
                    }
                }
            }
            Some(request::Body::ResetData(_)) => self.reset_data(peer),
            Some(request::Body::GetAndroidVersions(_)) => self.android_versions(peer, None),
            Some(request::Body::UseAndroidVersion(chosen)) => {
                self.android_versions(peer, Some(chosen))
            }
            Some(request::Body::SetSettings(set)) => self.set_settings(peer, set),
            Some(request::Body::GetDeviceIdentity(_)) => self.device_identity(peer, None),
            Some(request::Body::SetDeviceIdentity(set)) => {
                self.device_identity(peer, Some(set.identity.unwrap_or_default()))
            }
            Some(request::Body::GetMemory(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => {
                    let memory = self.memory_of(&peer.user, &*android);
                    response::Body::Memory(snapshot_to_wire(&memory))
                }
            },
            Some(request::Body::GetAndroidFiles(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => match android.android_files() {
                    Ok(url) => {
                        response::Body::AndroidFiles(types::AndroidFilesLocation { location: url })
                    }
                    Err(why) => {
                        eprintln!("files: Android's files for {}: {why}", peer.user);
                        error(ErrorCode::Unavailable, why)
                    }
                },
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
            Some(request::Body::ListApps(list)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => match list.refresh.then(|| android.refresh_apps()) {
                    Some(Err(TaskRequestError::NotRunning(why))) => {
                        error(ErrorCode::NotRunning, why)
                    }
                    Some(Err(TaskRequestError::Failed(why))) => error(ErrorCode::Unavailable, why),
                    Some(Ok(())) | None => response::Body::InstalledApps(installed_to_wire(
                        android
                            .watch_tasks()
                            .and_then(|watch| installed_of(watch.current)),
                    )),
                },
            },
            Some(request::Body::OpenAppSettings(open)) => {
                self.open_app_settings(peer, &open.package_name)
            }
            Some(request::Body::UninstallApp(uninstall)) => {
                self.uninstall_app(peer, &uninstall.package_name)
            }
            Some(request::Body::SetRunAsArm(set)) => self.set_run_as_arm(peer, set),
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
            Some(request::Body::SetTaskCaption(caption)) => self.request(
                peer,
                TaskRequest::SetCaption {
                    task_id: caption.task_id,
                    height: caption.height,
                },
            ),
            Some(request::Body::SetWindowsShown(shown)) => {
                self.request(peer, TaskRequest::WindowsShown { shown: shown.shown })
            }
            Some(request::Body::ActOnNotification(act)) => self.act_on_notification(peer, act),
            Some(request::Body::ReportPrintJob(report)) => self.report_print_job(peer, report),
            Some(request::Body::ListBackgroundApps(_)) => self.list_background(peer),
            Some(request::Body::ActOnBackgroundApp(act)) => self.act_on_background(peer, act),
            Some(request::Body::ControlMedia(control)) => self.control_media(peer, control),
            Some(request::Body::ListWidgetProviders(_)) => self.widget_providers(peer),
            Some(request::Body::GetArmTranslation(_)) => self.get_arm_translation(peer),
            Some(request::Body::RemoveArmTranslation(_)) => self.remove_arm_translation(peer),
            Some(request::Body::GetGapps(_)) => self.get_gapps(peer),
            Some(request::Body::RemoveGapps(_)) => self.remove_gapps(peer),
            Some(request::Body::GetPackage(get)) => self.package_state(peer, &get.package_name),
            Some(request::Body::DragOut(out)) => self.drag_out(peer, out),
            Some(request::Body::ReadDrag(read)) => self.read_drag(peer, read),
            Some(request::Body::EndDrag(end)) => self.end_drag(peer, end),
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
                        | request::Body::GetAndroidFiles(_)
                        | request::Body::ListTasks(_)
                        | request::Body::ListApps(_)
                        | request::Body::OpenAppSettings(_)
                        | request::Body::UninstallApp(_)
                        | request::Body::WatchTasks(_)
                        | request::Body::CloseTask(_)
                        | request::Body::LaunchApp(_)
                        | request::Body::ResizeTask(_)
                        | request::Body::SetTaskDensity(_)
                        | request::Body::SetTaskCaption(_)
                        | request::Body::SetWindowsShown(_)
                        | request::Body::FreezeSession(_)
                        | request::Body::GetSettings(_)
                        | request::Body::SetSettings(_)
                        | request::Body::GetDeviceIdentity(_)
                        | request::Body::SetDeviceIdentity(_)
                        | request::Body::GetMemory(_)
                        | request::Body::StreamInput(_)
                        | request::Body::WatchClipboard(_)
                        | request::Body::InstallApk(_)
                        | request::Body::InstallArmTranslation(_)
                        | request::Body::GetArmTranslation(_)
                        | request::Body::RemoveArmTranslation(_)
                        | request::Body::InstallGapps(_)
                        | request::Body::GetGapps(_)
                        | request::Body::RemoveGapps(_)
                        | request::Body::WatchLinks(_)
                        | request::Body::OpenFile(_)
                        | request::Body::WatchNotifications(_)
                        | request::Body::ActOnNotification(_)
                        | request::Body::WatchPrintJobs(_)
                        | request::Body::ReportPrintJob(_)
                        | request::Body::ListBackgroundApps(_)
                        | request::Body::WatchBackgroundApps(_)
                        | request::Body::ActOnBackgroundApp(_)
                        | request::Body::ControlMedia(_)
                        | request::Body::ListWidgetProviders(_)
                        | request::Body::GetPackage(_)
                        | request::Body::DragOut(_)
                        | request::Body::ReadDrag(_)
                        | request::Body::EndDrag(_)
                        | request::Body::AddWidget(_)
                        | request::Body::RemoveWidget(_)
                        | request::Body::ListWidgets(_)
                        | request::Body::WatchWidgets(_)
                        | request::Body::PlaceWidget(_)
                        | request::Body::WidgetPointer(_)
                        | request::Body::WatchAccessibility(_)
                        | request::Body::WatchLog(_)
                        | request::Body::CollectDiagnostics(_)
                        | request::Body::StopApp(_)
                        | request::Body::WatchLocation(_)
                        | request::Body::SetSystemSettings(_)
                        | request::Body::WatchGamepads(_)
                        | request::Body::ListAppWindowSettings(_)
                        | request::Body::SetAppWindowSettings(_)
                        | request::Body::ResetData(_)
                        | request::Body::GetAndroidVersions(_)
                        | request::Body::UseAndroidVersion(_)
                        | request::Body::SetRunAsArm(_) => unreachable!("answered above"),
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
    /// Where ARM translation stands for `peer`: the package they installed, the image's
    /// default one, their setting, the translator those choose, and what their running Android
    /// says.
    fn arm_translation_status(
        &self,
        peer: &Peer,
        store: &dyn SettingsStore,
        guest: Option<GuestArmTranslation>,
    ) -> Result<ArmTranslationStatus, String> {
        let package = store.arm_translation(&peer.user)?;
        let enabled = store.get(&peer.user)?.arm_translation;
        let default = self
            .default_arm_translation
            .as_ref()
            .and_then(|source| source(&peer.user));
        let translator = arm_translation::choose(package.as_ref(), default.as_ref(), enabled);
        let guest = guest.or_else(|| {
            self.androids
                .android(&peer.user)
                .ok()
                .and_then(|android| android.arm_translation())
        });
        // A record that cannot be read only takes away telling one translator from another.
        let booted = store
            .arm_translation_boot(&peer.user)
            .unwrap_or_else(|why| {
                eprintln!("arm translation: {why}");
                None
            });
        Ok(ArmTranslationStatus {
            restart_needed: arm_translation::restart_needed(
                translator.as_ref(),
                booted.as_ref(),
                guest.as_ref(),
            ),
            package,
            default,
            translator,
            enabled,
            guest,
        })
    }

    fn get_arm_translation(&self, peer: &Peer) -> response::Body {
        let Some((store, _)) = &self.settings else {
            return Self::no_settings();
        };
        match self.arm_translation_status(peer, &**store, None) {
            Ok(status) => response::Body::ArmTranslation(status.to_wire()),
            Err(why) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Removes the caller's ARM translation package: from their Android's disk when it runs,
    /// and from their settings, so no later boot uses it.
    fn remove_arm_translation(&self, peer: &Peer) -> response::Body {
        let Some((store, _)) = &self.settings else {
            return Self::no_settings();
        };
        let mut guest = None;
        if let Ok(android) = self.androids.android(&peer.user) {
            if matches!(
                android.state(),
                SessionState::Active | SessionState::Dozing | SessionState::Frozen
            ) {
                match android.remove_arm_translation() {
                    Ok(state) => guest = Some(state),
                    Err(InstallError::NotRunning(why)) => return error(ErrorCode::NotRunning, why),
                    Err(InstallError::Refused(why)) => return error(ErrorCode::Unavailable, why),
                }
            }
        }
        if let Err(why) = store.set_arm_translation(&peer.user, None) {
            return error(ErrorCode::Unavailable, why);
        }
        eprintln!("arm translation: {} removed their package", peer.user);
        match self.arm_translation_status(peer, &**store, guest) {
            Ok(status) => response::Body::ArmTranslation(status.to_wire()),
            Err(why) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Answers an InstallArmTranslation request: refuses a package whose description is not
    /// usable, else streams the image the client sends after Done to the caller's Android, and
    /// once it is there records the package with the caller's settings and answers again with
    /// where translation stands. The connection ends with the install.
    fn install_arm_translation(
        &self,
        mut connection: Box<dyn Connection>,
        peer: &Peer,
        id: u64,
        install: types::InstallArmTranslation,
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
        let refuse = |mut connection: Box<dyn Connection>, body: response::Body| {
            answer(&mut *connection, body);
            connection.close();
        };
        let Some((store, _)) = &self.settings else {
            return refuse(connection, Self::no_settings());
        };
        let package = install
            .package
            .map(package_from_wire)
            .ok_or_else(|| "the request does not describe the package".to_string())
            .and_then(|package| package.check().map(|()| package))
            .and_then(|package| {
                if install.size == 0 || install.size > arm_translation::MAX_IMAGE_BYTES {
                    Err(format!(
                        "the package's image is {} bytes; it must have from 1 byte to {} MiB",
                        install.size,
                        arm_translation::MAX_IMAGE_BYTES >> 20
                    ))
                } else if install.sha256.len() != 32 {
                    Err("the request does not carry the image's SHA-256".to_string())
                } else {
                    Ok(package)
                }
            });
        let package = match package {
            Ok(package) => package,
            Err(why) => {
                eprintln!(
                    "arm translation: refused a package from {}: {why}",
                    peer.user
                );
                return refuse(connection, error(ErrorCode::InstallRefused, why));
            }
        };
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return refuse(connection, error(ErrorCode::Unavailable, why)),
        };
        let state = android.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            let why = format!("Android is {state}; start it and install the package again");
            return refuse(connection, error(ErrorCode::NotRunning, why));
        }
        answer(&mut *connection, response::Body::Done(types::Done {}));
        eprintln!(
            "arm translation: {} is installing {} {} ({} bytes)",
            peer.user, package.name, package.version, install.size
        );
        let mut content = ApkChunks {
            connection: &mut *connection,
            user: &peer.user,
            left: install.size,
            piece: Vec::new(),
            at: 0,
        };
        let installed =
            android.install_arm_translation(install.size, &install.sha256, &mut content);
        let body = match installed {
            Ok(guest) => match store.set_arm_translation(&peer.user, Some(&package)) {
                Ok(()) => match self.arm_translation_status(peer, &**store, Some(guest)) {
                    Ok(status) => response::Body::ArmTranslation(status.to_wire()),
                    Err(why) => error(ErrorCode::Unavailable, why),
                },
                Err(why) => error(ErrorCode::Unavailable, why),
            },
            Err(InstallError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(InstallError::Refused(why)) => error(ErrorCode::InstallRefused, why),
        };
        answer(&mut *connection, body);
        connection.close();
    }

    /// Where Google Play stands for `peer`: the build they installed, and what their running
    /// Android says.
    fn gapps_status(
        &self,
        peer: &Peer,
        store: &dyn SettingsStore,
        guest: Option<GuestGapps>,
    ) -> Result<GappsStatus, String> {
        let package = store.gapps(&peer.user)?;
        let guest = guest.or_else(|| {
            self.androids
                .android(&peer.user)
                .ok()
                .and_then(|android| android.gapps())
        });
        Ok(GappsStatus {
            restart_needed: gapps::restart_needed(package.as_ref(), guest.as_ref()),
            package,
            guest,
        })
    }

    fn get_gapps(&self, peer: &Peer) -> response::Body {
        let Some((store, _)) = &self.settings else {
            return Self::no_settings();
        };
        match self.gapps_status(peer, &**store, None) {
            Ok(status) => response::Body::Gapps(status.to_wire()),
            Err(why) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Removes the caller's Google apps: from their Android's disk when it runs, and from
    /// their settings, so no later boot lays them over the system.
    fn remove_gapps(&self, peer: &Peer) -> response::Body {
        let Some((store, _)) = &self.settings else {
            return Self::no_settings();
        };
        let mut guest = None;
        if let Ok(android) = self.androids.android(&peer.user) {
            if matches!(
                android.state(),
                SessionState::Active | SessionState::Dozing | SessionState::Frozen
            ) {
                match android.remove_gapps() {
                    Ok(state) => guest = Some(state),
                    Err(InstallError::NotRunning(why)) => return error(ErrorCode::NotRunning, why),
                    Err(InstallError::Refused(why)) => return error(ErrorCode::Unavailable, why),
                }
            }
        }
        if let Err(why) = store.set_gapps(&peer.user, None) {
            return error(ErrorCode::Unavailable, why);
        }
        eprintln!("gapps: {} removed Google Play", peer.user);
        match self.gapps_status(peer, &**store, guest) {
            Ok(status) => response::Body::Gapps(status.to_wire()),
            Err(why) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Answers an InstallGapps request: finds the build for the Android the caller runs,
    /// downloads and checks it, makes its image and sends that to the caller's Android,
    /// answering with GappsProgress on the way. Once Android has the image the build is
    /// recorded with the caller's settings, and the last answer says where Google Play stands.
    /// A build that is refused leaves the settings as they were. The connection ends with the
    /// install.
    fn install_gapps(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let answer = |connection: &mut dyn Connection, body: response::Body| {
            let _ = send(
                connection,
                Body::Response(Response {
                    id,
                    body: Some(body),
                }),
            );
        };
        let refuse = |mut connection: Box<dyn Connection>, body: response::Body| {
            answer(&mut *connection, body);
            connection.close();
        };
        let Some((store, _)) = &self.settings else {
            return refuse(connection, Self::no_settings());
        };
        let Some(source) = &self.gapps else {
            let why = "this service cannot download Google Play; update DSA".to_string();
            return refuse(connection, error(ErrorCode::Unavailable, why));
        };
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return refuse(connection, error(ErrorCode::Unavailable, why)),
        };
        let state = android.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            let why = format!("Android is {state}; start it and install Google Play again");
            return refuse(connection, error(ErrorCode::NotRunning, why));
        }
        let Some(guest) = android.gapps() else {
            let why = "Android did not say which version it runs; try again once it has \
                       finished starting"
                .to_string();
            return refuse(connection, error(ErrorCode::NotRunning, why));
        };
        let target = match gapps::Target::for_guest(guest.sdk, &guest.abi) {
            Ok(target) => target,
            Err(why) => return refuse(connection, error(ErrorCode::InstallRefused, why)),
        };
        eprintln!(
            "gapps: {} is installing Google Play for {} Android at API level {}",
            peer.user, target.arch, target.sdk
        );
        let prepared = source.prepare(&peer.user, &target, &mut |progress| {
            answer(
                &mut *connection,
                response::Body::GappsProgress(gapps_progress_to_wire(progress)),
            )
        });
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(why) => {
                eprintln!("gapps: refused the build for {}: {why}", peer.user);
                return refuse(connection, error(ErrorCode::InstallRefused, why));
            }
        };
        let mut image = match std::fs::File::open(&prepared.image) {
            Ok(image) => image,
            Err(e) => {
                let why = format!("the Google apps image cannot be read: {e}");
                return refuse(connection, error(ErrorCode::Unavailable, why));
            }
        };
        let size = prepared.size;
        let mut report = |done: u64| {
            let news = gapps::Progress {
                stage: gapps::Stage::Sending,
                done,
                total: size,
            };
            answer(
                &mut *connection,
                response::Body::GappsProgress(gapps_progress_to_wire(news)),
            )
        };
        let installed = android.install_gapps(
            size,
            &prepared.sha256,
            &mut gapps::Reporting::new(&mut image, &mut report),
        );
        let body = match installed {
            Ok(guest) => match store.set_gapps(&peer.user, Some(&prepared.package)) {
                Ok(()) => {
                    let package = &prepared.package;
                    let files = prepared.built.files;
                    eprintln!(
                        "gapps: {} installed LiteGapps {} {} ({files} files)",
                        peer.user, package.variant, package.version
                    );
                    match self.gapps_status(peer, &**store, Some(guest)) {
                        Ok(status) => response::Body::Gapps(status.to_wire()),
                        Err(why) => error(ErrorCode::Unavailable, why),
                    }
                }
                Err(why) => error(ErrorCode::Unavailable, why),
            },
            Err(InstallError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(InstallError::Refused(why)) => error(ErrorCode::InstallRefused, why),
        };
        drop(prepared);
        answer(&mut *connection, body);
        connection.close();
    }

    /// The caller's device identity, after replacing it with `wanted` when there is one.
    fn device_identity(
        &self,
        peer: &Peer,
        wanted: Option<types::DeviceIdentity>,
    ) -> response::Body {
        let Some((store, build)) = &self.identities else {
            return Self::no_settings();
        };
        let chosen = match wanted {
            Some(wanted) => {
                match store.set(&peer.user, &crate::device_identity::from_wire(wanted)) {
                    Ok(chosen) => chosen,
                    Err(why) => return error(ErrorCode::InvalidSetting, why),
                }
            }
            None => match store.get(&peer.user) {
                Ok(chosen) => chosen,
                Err(why) => return error(ErrorCode::Unavailable, why),
            },
        };
        let computer = match &self.computer {
            Some(read) => read().identity(),
            None => crate::computer_identity::ComputerIdentity::dsa(),
        };
        let report = IdentityReport::new(&chosen, build(&peer.user).as_ref(), &computer);
        response::Body::DeviceIdentity(crate::device_identity::report_to_wire(&report))
    }

    /// Answers with the caller's window settings for every app, after replacing one app's
    /// when `set` carries them.
    fn app_window_settings(
        &self,
        peer: &Peer,
        set: Option<types::AppWindowSettings>,
    ) -> response::Body {
        let Some((store, _)) = &self.settings else {
            return Self::no_settings();
        };
        let mut apps = match store.app_windows(&peer.user) {
            Ok(apps) => apps,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        if let Some(set) = set {
            let checked = AppWindowSettings::from_wire(&set)
                .checked()
                .and_then(|app| app_windows::replace(&apps, app));
            let replaced = match checked {
                Ok(replaced) => replaced,
                Err(why) => return error(ErrorCode::InvalidSetting, why),
            };
            if let Err(why) = store.set_app_windows(&peer.user, &replaced) {
                return error(ErrorCode::Unavailable, why);
            }
            eprintln!(
                "local API: {} changed the window settings of {}",
                peer.user, set.package_name
            );
            apps = replaced;
        }
        response::Body::AppWindowSettings(types::AppWindowSettingsList {
            apps: apps.iter().map(AppWindowSettings::to_wire).collect(),
        })
    }

    fn no_settings() -> response::Body {
        error(
            ErrorCode::Unavailable,
            "this service keeps no settings; update the service".into(),
        )
    }

    /// Reports the Android version, after choosing `chosen` when there is one.
    fn android_versions(
        &self,
        peer: &Peer,
        chosen: Option<types::UseAndroidVersion>,
    ) -> response::Body {
        use crate::update::versions::{Channel, ChooseError};
        let Some(versions) = &self.versions else {
            return error(
                ErrorCode::Unavailable,
                "this service does not keep Android images it can update, so the Android \
                 version cannot be chosen here"
                    .into(),
            );
        };
        let report = match chosen {
            None => versions
                .report()
                .map_err(|why| error(ErrorCode::Unavailable, why)),
            Some(_) if versions.needs_administrator() && !peer.administrator => Err(error(
                ErrorCode::NotAllowed,
                "only an administrator can choose the Android version; every user of this \
                 computer runs the same one. Run the command as an administrator."
                    .into(),
            )),
            Some(chosen) => versions
                .choose(Channel::from_wire(chosen.major), chosen.erase_data)
                .map_err(|why| match why {
                    ChooseError::Unsupported(why) => error(ErrorCode::InvalidSetting, why),
                    ChooseError::DataWouldBeLost(why) => error(ErrorCode::DataWouldBeLost, why),
                    ChooseError::Store(why) => error(ErrorCode::Unavailable, why),
                })
                .inspect(|report| {
                    eprintln!(
                        "local API: {} chose Android {} (now {}, moving to {})",
                        peer.user,
                        match report.channel {
                            Channel::Latest => "latest".to_string(),
                            Channel::Major(major) => major.to_string(),
                        },
                        report.android_major,
                        report.target_major
                    )
                }),
        };
        match report {
            Ok(report) => response::Body::AndroidVersions(versions_to_wire(&report)),
            Err(body) => body,
        }
    }

    /// Erases the caller's Android data: stops their Android and has the platform put a fresh
    /// data disk in place of theirs.
    fn reset_data(&self, peer: &Peer) -> response::Body {
        let Some(reset) = &self.data_reset else {
            return error(
                ErrorCode::Unavailable,
                "this service cannot reset Android's data; update the service".into(),
            );
        };
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match crate::data_reset::reset_user_data(
            android.as_ref(),
            reset.as_ref(),
            &peer.user,
            self.data_reset_wait,
        ) {
            Ok(()) => {
                // The widgets placed on the desktop were in the old data, so where their
                // windows were is forgotten too.
                if let Ok(placed) = self.placements.all(&peer.user) {
                    for widget_id in placed.keys() {
                        let _ = self.placements.remove(&peer.user, *widget_id);
                    }
                }
                eprintln!(
                    "local API: {} reset their Android data; the next start boots a new disk",
                    peer.user
                );
                response::Body::Done(types::Done {})
            }
            Err(why) => {
                eprintln!(
                    "local API: resetting the Android data of {}: {why}",
                    peer.user
                );
                error(ErrorCode::Unavailable, why)
            }
        }
    }

    fn get_settings(&self, peer: &Peer) -> response::Body {
        let Some((store, facts)) = &self.settings else {
            return Self::no_settings();
        };
        match store.get(&peer.user) {
            Ok(settings) => {
                let mut wire = settings_to_wire(&settings, &self.facts_now(facts));
                wire.network = self.network_now(&peer.user, &settings);
                wire.adb_auto_connect_state = self.adb_auto_connect_now(&peer.user);
                response::Body::Settings(wire)
            }
            Err(why) => error(ErrorCode::Unavailable, why),
        }
    }

    /// Where adb auto-connect stands for the caller's Android, or off when it has none.
    fn adb_auto_connect_now(&self, user: &str) -> String {
        self.androids
            .android(user)
            .map_or(AutoConnectState::Off, |android| {
                android.adb_auto_connect_state()
            })
            .as_str()
            .to_string()
    }

    /// The caller's network as the platform reports it, or "off" when it cannot say (no
    /// Android is running, so there is no network yet).
    fn network_now(&self, user: &str, settings: &Settings) -> Option<types::NetworkStatus> {
        let source = self.network.as_ref()?;
        let status = source(user, settings).unwrap_or_else(NetworkStatus::off);
        Some(status.to_wire())
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
            stop_without_asking: wanted.stop_without_asking,
            links_in_android: wanted.links_in_android,
            memory_limit_mb: wanted.memory_limit_mb,
            program_updates: wanted.program_updates,
            image_updates: wanted.image_updates,
            input_trace: wanted.input_trace,
            resize_trace: wanted.resize_trace,
            developer_mode: wanted.developer_mode,
            adb_auto_connect: wanted.adb_auto_connect,
            arm_translation: wanted.arm_translation,
            file_sharing: wanted.file_sharing,
            shared_folders: wanted.shared_folders,
            advanced_networking: wanted.advanced_networking,
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
                    android.share_folders(settings.file_sharing, settings.shared_folders.clone());
                    android.adb_auto_connect(settings.adb_auto_connect);
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
                     trace {}, resize trace {}, developer mode {} (adb port {}, auto-connect \
                     {}), file sharing {} ({}), advanced networking {}",
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
                    settings.adb_port,
                    on_off(settings.adb_auto_connect),
                    on_off(settings.file_sharing),
                    settings.shared_folders.join(", "),
                    on_off(settings.advanced_networking)
                );
                let mut wire = settings_to_wire(&settings, &self.facts_now(facts));
                wire.network = self.network_now(&peer.user, &settings);
                wire.adb_auto_connect_state = self.adb_auto_connect_now(&peer.user);
                response::Body::Settings(wire)
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

fn identity_report(body: response::Body) -> Result<IdentityReport, ClientError> {
    match body {
        response::Body::DeviceIdentity(report) => {
            Ok(crate::device_identity::report_from_wire(report))
        }
        _ => Err(ClientError::Malformed(
            "expected the device identity".into(),
        )),
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
        stop_without_asking: settings.stop_without_asking,
        input_trace: settings.input_trace,
        resize_trace: settings.resize_trace,
        developer_mode: settings.developer_mode,
        adb_port: settings.adb_port.into(),
        adb_auto_connect: settings.adb_auto_connect,
        adb_auto_connect_state: String::new(),
        arm_translation: settings.arm_translation,
        file_sharing: settings.file_sharing,
        shared_folders: settings.shared_folders.clone(),
        advanced_networking: settings.advanced_networking,
        network: None,
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
            stop_without_asking: settings.stop_without_asking,
            program_updates: settings.program_updates,
            image_updates: settings.image_updates,
            input_trace: settings.input_trace,
            resize_trace: settings.resize_trace,
            developer_mode: settings.developer_mode,
            adb_port: crate::settings::normalise_adb_port(
                u16::try_from(settings.adb_port).unwrap_or(0),
            ),
            adb_auto_connect: settings.adb_auto_connect,
            arm_translation: settings.arm_translation,
            file_sharing: settings.file_sharing,
            shared_folders: settings.shared_folders,
            advanced_networking: settings.advanced_networking,
        },
        facts: ServiceFacts {
            version: settings.version,
            log_dir: Some(settings.log_dir).filter(|dir| !dir.is_empty()),
            program_updates_available: settings.program_updates_available,
            image_updates_available: settings.image_updates_available,
            android_version: Some(settings.android_version).filter(|v| !v.is_empty()),
        },
        network: settings.network.map(NetworkStatus::from_wire),
        adb_auto_connect_state: AutoConnectState::parse(&settings.adb_auto_connect_state),
    }
}

fn versions_to_wire(report: &crate::update::versions::VersionReport) -> types::AndroidVersions {
    types::AndroidVersions {
        version: report.version.clone(),
        android_major: report.android_major,
        build: report.build,
        build_id: report.build_id.clone().unwrap_or_default(),
        security_patch: report.security_patch.clone().unwrap_or_default(),
        chosen_major: report.channel.to_wire(),
        supported_majors: report.supported_majors.clone(),
        target_major: report.target_major,
        on_trial: report.on_trial,
        notice: report.notice.clone().unwrap_or_default(),
    }
}

fn versions_from_wire(wire: types::AndroidVersions) -> crate::update::versions::VersionReport {
    let text = |s: String| Some(s).filter(|s| !s.is_empty());
    crate::update::versions::VersionReport {
        version: wire.version,
        android_major: wire.android_major,
        build: wire.build,
        build_id: text(wire.build_id),
        security_patch: text(wire.security_patch),
        channel: crate::update::versions::Channel::from_wire(wire.chosen_major),
        supported_majors: wire.supported_majors,
        target_major: wire.target_major,
        on_trial: wire.on_trial,
        notice: text(wire.notice),
    }
}

fn error(code: ErrorCode, detail: String) -> response::Body {
    response::Body::Error(types::Error {
        code: code as i32,
        detail,
    })
}

fn not_a_package(package: &str) -> response::Body {
    error(
        ErrorCode::UnknownRequest,
        format!(
            "{:?} is not a package name, such as com.example.notes",
            package.chars().take(80).collect::<String>()
        ),
    )
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

impl ClientError {
    /// Whether the service refused to move to an older Android major because that erases
    /// user data the request did not agree to lose.
    pub fn data_would_be_lost(&self) -> bool {
        matches!(
            self,
            ClientError::Service {
                code: ErrorCode::DataWouldBeLost,
                ..
            }
        )
    }
}

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

    /// Erases the caller's Android data (installed apps, accounts and app data) and puts a
    /// fresh data disk in its place. Android is stopped first; the system image and the
    /// settings stay. Waits as long as stopping Android and making the disk take.
    pub fn reset_data(&mut self) -> Result<(), ClientError> {
        match self.exchange(
            request::Body::ResetData(types::ResetData {}),
            crate::data_reset::RESET_ANSWER_WAIT,
        )? {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed(
                "expected the answer that the data was reset".into(),
            )),
        }
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
    /// Which Android version runs and which can be chosen.
    pub fn android_versions(
        &mut self,
    ) -> Result<crate::update::versions::VersionReport, ClientError> {
        let body = request::Body::GetAndroidVersions(types::GetAndroidVersions {});
        match self.exchange(body, self.timeout)? {
            response::Body::AndroidVersions(report) => Ok(versions_from_wire(report)),
            _ => Err(ClientError::Malformed(
                "expected the Android versions".into(),
            )),
        }
    }

    /// Chooses the Android version this computer follows. `erase_data` agrees to erasing
    /// every user's Android data when it is older than the running one.
    pub fn use_android_version(
        &mut self,
        channel: crate::update::versions::Channel,
        erase_data: bool,
    ) -> Result<crate::update::versions::VersionReport, ClientError> {
        let body = request::Body::UseAndroidVersion(types::UseAndroidVersion {
            major: channel.to_wire(),
            erase_data,
        });
        match self.exchange(body, self.timeout)? {
            response::Body::AndroidVersions(report) => Ok(versions_from_wire(report)),
            _ => Err(ClientError::Malformed(
                "expected the Android versions".into(),
            )),
        }
    }

    pub fn settings(&mut self) -> Result<SettingsReport, ClientError> {
        match self.exchange(
            request::Body::GetSettings(types::GetSettings {}),
            self.timeout,
        )? {
            response::Body::Settings(settings) => Ok(settings_from_wire(settings)),
            _ => Err(ClientError::Malformed("expected the settings".into())),
        }
    }

    /// The device identity the caller chose, and every value as Android reports it from its
    /// next start.
    pub fn device_identity(&mut self) -> Result<(DeviceIdentity, DeviceIdentity), ClientError> {
        let report = self.device_identity_report()?;
        Ok((report.chosen, report.effective))
    }

    /// The caller's device identity in full: what they chose, what Android reports, the
    /// defaults and where each came from.
    pub fn device_identity_report(&mut self) -> Result<IdentityReport, ClientError> {
        let body = request::Body::GetDeviceIdentity(types::GetDeviceIdentity {});
        identity_report(self.exchange(body, self.timeout)?)
    }

    /// Replaces the caller's device identity with `identity`, and returns it as
    /// [`device_identity`](Self::device_identity) does.
    pub fn set_device_identity(
        &mut self,
        identity: &DeviceIdentity,
    ) -> Result<(DeviceIdentity, DeviceIdentity), ClientError> {
        let report = self.set_device_identity_report(identity)?;
        Ok((report.chosen, report.effective))
    }

    /// Replaces the caller's device identity with `identity`, and returns it as
    /// [`device_identity_report`](Self::device_identity_report) does.
    pub fn set_device_identity_report(
        &mut self,
        identity: &DeviceIdentity,
    ) -> Result<IdentityReport, ClientError> {
        let body = request::Body::SetDeviceIdentity(types::SetDeviceIdentity {
            identity: Some(crate::device_identity::to_wire(identity)),
        });
        identity_report(self.exchange(body, self.timeout)?)
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

    /// The address (a WebDAV URL on the loopback address) the host's file manager opens the
    /// caller's Android shared storage at. The service starts the caller's Android when it is
    /// stopped.
    pub fn android_files(&mut self) -> Result<String, ClientError> {
        let body = request::Body::GetAndroidFiles(types::GetAndroidFiles {});
        match self.exchange(body, self.timeout)? {
            response::Body::AndroidFiles(files) => Ok(files.location),
            _ => Err(ClientError::Malformed(
                "expected where Android's files are".into(),
            )),
        }
    }

    /// Where ARM translation stands for the caller.
    pub fn arm_translation(&mut self) -> Result<ArmTranslationStatus, ClientError> {
        let body = request::Body::GetArmTranslation(types::GetArmTranslation {});
        match self.exchange(body, self.timeout)? {
            response::Body::ArmTranslation(status) => Ok(ArmTranslationStatus::from_wire(status)),
            _ => Err(ClientError::Malformed(
                "expected where ARM translation stands".into(),
            )),
        }
    }

    /// Removes the caller's ARM translation package; Android goes without it from its next
    /// start.
    pub fn remove_arm_translation(&mut self) -> Result<ArmTranslationStatus, ClientError> {
        let body = request::Body::RemoveArmTranslation(types::RemoveArmTranslation {});
        match self.exchange(body, INSTALL_ANSWER_WAIT)? {
            response::Body::ArmTranslation(status) => Ok(ArmTranslationStatus::from_wire(status)),
            _ => Err(ClientError::Malformed(
                "expected where ARM translation stands".into(),
            )),
        }
    }

    /// Installs the ARM translation package `package` for the caller: its system image,
    /// `size` bytes read from `content` whose SHA-256 is `sha256`, goes to the caller's
    /// Android, which uses it from its next start. The connection ends with the install, so
    /// the client is used up.
    pub fn install_arm_translation(
        mut self,
        package: &Package,
        size: u64,
        sha256: &[u8],
        content: &mut dyn Read,
    ) -> Result<ArmTranslationStatus, ClientError> {
        let id = self.next_id;
        let body = request::Body::InstallArmTranslation(types::InstallArmTranslation {
            package: Some(package_to_wire(package)),
            size,
            sha256: sha256.to_vec(),
        });
        match self.exchange(body, self.timeout)? {
            response::Body::Done(_) => {}
            _ => {
                return Err(ClientError::Malformed(
                    "expected the answer that the package is taken".into(),
                ))
            }
        }
        let mut left = size;
        let mut piece = vec![0u8; apk::LOCAL_CHUNK_BYTES];
        while left > 0 {
            let length = left.min(piece.len() as u64) as usize;
            if let Err(error) = content.read_exact(&mut piece[..length]) {
                self.connection.close();
                return Err(ClientError::Service {
                    code: ErrorCode::InstallRefused,
                    detail: format!("the package's image could not be read: {error}"),
                });
            }
            // A service that has answered already closes the connection; its answer is still
            // there to read.
            let chunk = Body::ApkChunk(types::ApkChunk {
                data: piece[..length].to_vec(),
            });
            if send(&mut *self.connection, chunk).is_err() {
                break;
            }
            left -= length as u64;
        }
        match self.answer(id, INSTALL_ANSWER_WAIT)? {
            response::Body::ArmTranslation(status) => Ok(ArmTranslationStatus::from_wire(status)),
            _ => Err(ClientError::Malformed(
                "expected where ARM translation stands".into(),
            )),
        }
    }

    /// Where Google Play stands for the caller.
    pub fn gapps(&mut self) -> Result<GappsStatus, ClientError> {
        let body = request::Body::GetGapps(types::GetGapps {});
        match self.exchange(body, GAPPS_ANSWER_WAIT)? {
            response::Body::Gapps(status) => Ok(GappsStatus::from_wire(status)),
            _ => Err(ClientError::Malformed(
                "expected where Google Play stands".into(),
            )),
        }
    }

    /// Removes the caller's Google apps; Android goes without them from its next start.
    pub fn remove_gapps(&mut self) -> Result<GappsStatus, ClientError> {
        let body = request::Body::RemoveGapps(types::RemoveGapps {});
        match self.exchange(body, INSTALL_ANSWER_WAIT)? {
            response::Body::Gapps(status) => Ok(GappsStatus::from_wire(status)),
            _ => Err(ClientError::Malformed(
                "expected where Google Play stands".into(),
            )),
        }
    }

    /// Installs Google Play for the caller: the service downloads the build for the caller's
    /// Android and sends its image there, which Android uses from its next start. `progress`
    /// hears what the service is doing on the way. The connection ends with the install, so
    /// the client is used up.
    pub fn install_gapps(
        mut self,
        progress: &mut dyn FnMut(gapps::Progress),
    ) -> Result<GappsStatus, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        send(
            &mut *self.connection,
            Body::Request(Request {
                id,
                body: Some(request::Body::InstallGapps(types::InstallGapps {})),
            }),
        )
        .map_err(ClientError::Transport)?;
        loop {
            match self.answer(id, INSTALL_ANSWER_WAIT)? {
                response::Body::GappsProgress(news) => {
                    if let Some(news) = gapps_progress_from_wire(&news) {
                        progress(news);
                    }
                }
                response::Body::Gapps(status) => return Ok(GappsStatus::from_wire(status)),
                _ => {
                    return Err(ClientError::Malformed(
                        "expected news of the install or where Google Play stands".into(),
                    ))
                }
            }
        }
    }

    /// The apps installed in the caller's Android that a launcher lists, in package order;
    /// `None` until it has said which since the service started.
    pub fn installed_apps(&mut self) -> Result<Option<Vec<AppInfo>>, ClientError> {
        self.list_apps(false)
    }

    /// The apps installed in the caller's Android, as [`HostApiClient::installed_apps`], after
    /// Android has read them again, so their sizes are current. Needs the Android to be running.
    pub fn refreshed_apps(&mut self) -> Result<Option<Vec<AppInfo>>, ClientError> {
        self.list_apps(true)
    }

    fn list_apps(&mut self, refresh: bool) -> Result<Option<Vec<AppInfo>>, ClientError> {
        let timeout = if refresh {
            self.timeout
                .max(apps::REFRESH_WAIT + Duration::from_secs(5))
        } else {
            self.timeout
        };
        match self.exchange(
            request::Body::ListApps(types::ListApps { refresh }),
            timeout,
        )? {
            response::Body::InstalledApps(installed) => Ok(installed_from_wire(installed)),
            _ => Err(ClientError::Malformed(
                "expected the list of installed apps".into(),
            )),
        }
    }

    /// Opens Android's own settings of `package` in a window of its own, or Android's list of
    /// every app when it is `None`.
    pub fn open_app_settings(&mut self, package: Option<&str>) -> Result<(), ClientError> {
        self.done(request::Body::OpenAppSettings(types::OpenAppSettings {
            package_name: package.unwrap_or_default().to_string(),
        }))
    }

    /// Uninstalls the user app `package` from the caller's Android and says how it went.
    pub fn uninstall_app(&mut self, package: &str) -> Result<UninstallOutcome, ClientError> {
        match self.exchange(
            request::Body::UninstallApp(types::UninstallApp {
                package_name: package.to_string(),
            }),
            self.timeout
                .max(apps::UNINSTALL_WAIT + Duration::from_secs(5)),
        )? {
            response::Body::UninstallResult(result) => Ok(UninstallOutcome::from_wire(result)),
            _ => Err(ClientError::Malformed(
                "expected how the uninstall went".into(),
            )),
        }
    }

    /// Installs the app `package` of the caller's Android again with its native code for
    /// arm64-v8a (`on`) or x86_64, keeping its data unless `erase_data`, and says how it went.
    pub fn set_run_as_arm(
        &mut self,
        package: &str,
        on: bool,
        erase_data: bool,
    ) -> Result<RunAsArmOutcome, ClientError> {
        match self.exchange(
            request::Body::SetRunAsArm(types::SetRunAsArm {
                package_name: package.to_string(),
                on,
                erase_data,
            }),
            self.timeout
                .max(native_abi::REINSTALL_WAIT + Duration::from_secs(5)),
        )? {
            response::Body::RunAsArmResult(result) => Ok(RunAsArmOutcome::from_wire(result)),
            _ => Err(ClientError::Malformed(
                "expected how installing the app again went".into(),
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

    /// Asks the caller's Android to give a task a top inset of `height` pixels, which its
    /// window's title bar covers.
    pub fn set_task_caption(&mut self, task_id: i32, height: u32) -> Result<(), ClientError> {
        self.done(request::Body::SetTaskCaption(types::SetTaskCaption {
            task_id,
            height,
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
            TaskRequest::SetCaption { task_id, height } => self.set_task_caption(*task_id, *height),
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

    /// Carries the location between the host and the caller's Android: the client hears when
    /// the Android wants it and sends what the host's location service reports. The
    /// connection carries nothing else from here on, so the client is used up.
    pub fn watch_location(mut self) -> Result<LocationStream, ClientError> {
        match self.exchange(
            request::Body::WatchLocation(types::WatchLocation {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(LocationStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that the location is carried".into(),
            )),
        }
    }

    /// Gives the caller's Android the system settings of the caller's host session.
    pub fn set_system_settings(&mut self, settings: &SystemSettings) -> Result<(), ClientError> {
        self.done(request::Body::SetSystemSettings(types::SetSystemSettings {
            settings: Some(settings.to_wire()),
        }))
    }

    /// Carries the caller's game controllers to its Android, and their rumble back. The
    /// connection carries nothing else from here on, so the client is used up.
    pub fn watch_gamepads(mut self) -> Result<GamepadStream, ClientError> {
        match self.exchange(
            request::Body::WatchGamepads(types::WatchGamepads {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(GamepadStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that the controllers are carried".into(),
            )),
        }
    }

    /// The caller's window settings for every app that has any.
    pub fn app_window_settings(&mut self) -> Result<Vec<AppWindowSettings>, ClientError> {
        let body = request::Body::ListAppWindowSettings(types::ListAppWindowSettings {});
        app_windows_report(self.exchange(body, self.timeout)?)
    }

    /// Replaces the caller's window settings for one app, and returns every app's as they are
    /// now.
    pub fn set_app_window_settings(
        &mut self,
        app: &AppWindowSettings,
    ) -> Result<Vec<AppWindowSettings>, ClientError> {
        let body = request::Body::SetAppWindowSettings(types::SetAppWindowSettings {
            app: Some(app.to_wire()),
        });
        app_windows_report(self.exchange(body, self.timeout)?)
    }

    /// Installs the APK called `name`, of `size` bytes read from `content`, in the caller's
    /// Android, and returns how the install ended: a refusal by Android is an outcome, not an
    /// error. An error means the APK did not get there. The connection ends with the install,
    /// so the client is used up.
    pub fn install_apk(
        self,
        name: &str,
        size: u64,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, ClientError> {
        self.install_apk_with_libraries(name, size, &NativeLibraries::new(), content)
    }

    /// Installs an APK as [`install_apk`](Self::install_apk) does, saying which native
    /// libraries it holds ([`crate::apk::ApkFile::native_libraries`]), from which the service
    /// chooses the ABI it is installed for.
    pub fn install_apk_with_libraries(
        self,
        name: &str,
        size: u64,
        libraries: &NativeLibraries,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, ClientError> {
        self.install(name, size, libraries, &[], content)
    }

    /// Installs an app made of several files in the caller's Android: `parts` names them, and
    /// their bytes are read from `content` one after another, in that order. `name` is the
    /// bundle they come from, and `libraries` the native libraries of its APKs among the
    /// parts, together. Otherwise as [`install_apk`](Self::install_apk).
    pub fn install_parts(
        self,
        name: &str,
        parts: &[InstallPart],
        libraries: &NativeLibraries,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, ClientError> {
        let size = parts.iter().map(|part| part.size).sum();
        self.install(name, size, libraries, parts, content)
    }

    fn install(
        mut self,
        name: &str,
        size: u64,
        libraries: &NativeLibraries,
        parts: &[InstallPart],
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, ClientError> {
        let id = self.next_id;
        match self.exchange(
            request::Body::InstallApk(types::InstallApk {
                name: name.to_string(),
                size,
                native_libraries: libraries.to_wire(),
                parts: parts.iter().map(InstallPart::to_wire).collect(),
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
                package: file.package.clone(),
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

    /// Watches the print jobs of the caller's Android. The connection carries nothing else
    /// from here on, so the client is used up.
    pub fn watch_print_jobs(mut self) -> Result<PrintStream, ClientError> {
        match self.exchange(
            request::Body::WatchPrintJobs(types::WatchPrintJobs {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(PrintStream {
                connection: self.connection,
                arriving: None,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that print jobs are watched".into(),
            )),
        }
    }

    /// The apps that run in the background in the caller's Android, and its media sessions.
    pub fn background_apps(
        &mut self,
    ) -> Result<(Vec<BackgroundApp>, Vec<MediaSession>), ClientError> {
        match self.exchange(
            request::Body::ListBackgroundApps(types::ListBackgroundApps {}),
            self.timeout,
        )? {
            response::Body::BackgroundApps(list) => Ok(background_from_wire(list)),
            _ => Err(ClientError::Malformed(
                "expected the apps that run in the background".into(),
            )),
        }
    }

    /// Tells the caller's Android what became of one of its print jobs.
    pub fn report_print_job(
        &mut self,
        job_id: u32,
        outcome: &PrintOutcome,
    ) -> Result<(), ClientError> {
        let (outcome, message) = match outcome {
            PrintOutcome::Printed => (types::PrintOutcome::Printed, String::new()),
            PrintOutcome::Canceled => (types::PrintOutcome::Canceled, String::new()),
            PrintOutcome::Failed(why) => (types::PrintOutcome::Failed, why.clone()),
        };
        self.done(request::Body::ReportPrintJob(types::ReportPrintJob {
            job_id,
            outcome: outcome as i32,
            message,
        }))
    }

    /// Watches the apps that run in the background in the caller's Android. The connection
    /// carries nothing else from here on, so the client is used up.
    pub fn watch_background(mut self) -> Result<BackgroundStream, ClientError> {
        match self.exchange(
            request::Body::WatchBackgroundApps(types::WatchBackgroundApps {}),
            self.timeout,
        )? {
            response::Body::BackgroundApps(list) => {
                let (apps, media) = background_from_wire(list);
                Ok(BackgroundStream {
                    apps,
                    media,
                    connection: self.connection,
                })
            }
            _ => Err(ClientError::Malformed(
                "expected the apps that run in the background".into(),
            )),
        }
    }

    /// Passes what the user did with an app that runs in the background to the caller's
    /// Android.
    pub fn act_on_background(
        &mut self,
        package: &str,
        command: BackgroundCommand,
    ) -> Result<(), ClientError> {
        let (kind, action) = match command {
            BackgroundCommand::Open => (types::BackgroundAppCommand::Open, 0),
            BackgroundCommand::Action(index) => (
                types::BackgroundAppCommand::Action,
                i32::try_from(index).unwrap_or(i32::MAX),
            ),
            BackgroundCommand::Stop => (types::BackgroundAppCommand::Stop, 0),
        };
        self.done(request::Body::ActOnBackgroundApp(
            types::ActOnBackgroundApp {
                package_name: package.to_string(),
                command: kind as i32,
                action,
            },
        ))
    }

    /// Runs a media key's command on the package's media session.
    pub fn control_media(
        &mut self,
        package: &str,
        command: MediaCommand,
    ) -> Result<(), ClientError> {
        let command = match command {
            MediaCommand::Play => types::MediaCommand::Play,
            MediaCommand::Pause => types::MediaCommand::Pause,
            MediaCommand::Next => types::MediaCommand::Next,
            MediaCommand::Previous => types::MediaCommand::Previous,
            MediaCommand::Stop => types::MediaCommand::Stop,
        };
        self.done(request::Body::ControlMedia(types::ControlMedia {
            package_name: package.to_string(),
            command: command as i32,
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

    /// The pointer left a task's window with a button held (`out`): what an app in the
    /// caller's Android drags out of it, or `None` when nothing is being dragged.
    pub fn drag_out(&mut self, out: DragOut) -> Result<Option<DragOffer>, ClientError> {
        match self.exchange(
            request::Body::DragOut(types::DragOut {
                task_id: out.task_id,
                x: out.x,
                y: out.y,
                tool: out.tool,
                pointer_id: out.pointer_id,
            }),
            self.timeout.max(DRAG_OUT_WAIT + Duration::from_secs(2)),
        )? {
            response::Body::DragOffer(offer) if offer.items.is_empty() => Ok(None),
            response::Body::DragOffer(offer) => Ok(Some(DragOffer {
                drag_id: offer.drag_id,
                items: offer
                    .items
                    .into_iter()
                    .enumerate()
                    .map(|(index, item)| DragItem::from_wire(item, index))
                    .collect(),
            })),
            _ => Err(ClientError::Malformed(
                "expected what is dragged out of the window".into(),
            )),
        }
    }

    /// Part of a file of a drag [`drag_out`](Self::drag_out) handed over.
    pub fn read_drag(&mut self, read: DragRead) -> Result<DragChunk, ClientError> {
        match self.exchange(
            request::Body::ReadDrag(types::ReadDrag {
                drag_id: read.drag_id,
                item: read.item,
                offset: read.offset,
                length: read.length,
            }),
            self.timeout.max(DRAG_READ_WAIT + Duration::from_secs(5)),
        )? {
            response::Body::DragData(data) => Ok(DragChunk {
                data: data.data,
                end: data.end,
            }),
            _ => Err(ClientError::Malformed(
                "expected part of the dragged file".into(),
            )),
        }
    }

    /// Ends a drag [`drag_out`](Self::drag_out) handed over, dropped or cancelled, so Android
    /// gives back its access to what was dragged.
    pub fn end_drag(&mut self, drag_id: u32, dropped: bool) -> Result<(), ClientError> {
        match self.exchange(
            request::Body::EndDrag(types::EndDrag { drag_id, dropped }),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed("expected Done".into())),
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

/// A connection that carries the caller's game controllers to its Android, from
/// [`HostApiClient::watch_gamepads`].
pub struct GamepadStream {
    connection: Box<dyn Connection>,
}

impl GamepadStream {
    /// Passes a change to a controller on. Nothing is awaited. An error means the connection
    /// has ended, and a new one is needed.
    pub fn send(&mut self, event: &GamepadEvent) -> Result<(), ClientError> {
        send(
            &mut *self.connection,
            Body::GamepadEvent(gamepad_to_wire(event)),
        )
        .map_err(ClientError::Transport)
    }

    /// The next rumble effect an app in the caller's Android played, waiting at most
    /// `timeout`. `Ok(None)` means none came in time; an error means the connection has ended.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<GamepadRumble>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::GamepadRumble(rumble)),
            }) => Ok(Some(GamepadRumble {
                pad_id: rumble.pad_id,
                strong: rumble.strong.min(u32::from(u16::MAX)) as u16,
                weak: rumble.weak.min(u32::from(u16::MAX)) as u16,
                duration_ms: rumble.duration_ms,
            })),
            Ok(_) => Err(ClientError::Malformed("expected a rumble effect".into())),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }
}

/// The app window settings an answer carries.
fn app_windows_report(body: response::Body) -> Result<Vec<AppWindowSettings>, ClientError> {
    match body {
        response::Body::AppWindowSettings(list) => {
            Ok(list.apps.iter().map(AppWindowSettings::from_wire).collect())
        }
        _ => Err(ClientError::Malformed(
            "expected the window settings of the apps".into(),
        )),
    }
}

/// A connection that carries the location between the host and the caller's Android, from
/// [`HostApiClient::watch_location`].
pub struct LocationStream {
    connection: Box<dyn Connection>,
}

impl LocationStream {
    /// The Android's next word on whether it wants the location, waiting at most `timeout`.
    /// `Ok(None)` means none came in time; an error means the connection has ended.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Wanted>, ClientError> {
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::LocationWanted(wanted)),
            }) => Ok(Some(Wanted::from_host_api(&wanted))),
            Ok(_) => Err(ClientError::Malformed(
                "expected whether the location is wanted".into(),
            )),
            Err(error) => Err(ClientError::Malformed(error.to_string())),
        }
    }

    /// Sends what the host's location service reported. Nothing is awaited. An error means
    /// the connection has ended, and a new one is needed.
    pub fn send(&mut self, event: &LocationEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, event.to_host_api()).map_err(ClientError::Transport)
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

/// A connection that watches the apps that run in the background in the caller's Android,
/// from [`HostApiClient::watch_background`], with the apps and sessions there were when it
/// started.
pub struct BackgroundStream {
    pub apps: Vec<BackgroundApp>,
    pub media: Vec<MediaSession>,
    connection: Box<dyn Connection>,
}

impl BackgroundStream {
    /// The next change, waiting at most `timeout`. `Ok(None)` means none came in time; an
    /// error means the connection has ended, and a new watch is needed.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<BackgroundEvent>, ClientError> {
        use types::background_app_event::Body as Event;
        let frame = match self.connection.recv(Some(timeout)) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => return Ok(None),
            Err(error) => return Err(ClientError::Transport(error)),
        };
        match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::BackgroundAppEvent(types::BackgroundAppEvent { body })),
            }) => match body {
                Some(Event::App(app)) => {
                    Ok(Some(BackgroundEvent::App(background_app_from_wire(app))))
                }
                Some(Event::AppRemoved(removed)) => Ok(Some(BackgroundEvent::AppRemoved {
                    package_name: removed.package_name,
                })),
                Some(Event::Media(session)) => {
                    Ok(Some(BackgroundEvent::Media(media_from_wire(session))))
                }
                Some(Event::MediaRemoved(removed)) => Ok(Some(BackgroundEvent::MediaRemoved {
                    package_name: removed.package_name,
                })),
                None => Err(ClientError::Malformed(
                    "an empty change to the apps that run in the background".into(),
                )),
            },
            Ok(_) => Err(ClientError::Malformed(
                "expected a change to the apps that run in the background".into(),
            )),
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

/// Sends a whole print job on a connection that watches them: its offer, then its document
/// in pieces.
fn send_print_job(connection: &mut dyn Connection, job: &PrintJob) -> Result<(), TransportError> {
    let offer = types::PrintJobOffer {
        job_id: job.job_id,
        package_name: job.package_name.clone(),
        app_label: job.app_label.clone(),
        document_name: job.document_name.clone(),
        size: job.pdf.len() as u64,
        page_count: job.settings.page_count,
        copies: job.settings.copies,
        color: job.settings.color,
        duplex: job.settings.duplex,
        landscape: job.settings.landscape,
        media_width_mils: job.settings.media_width_mils,
        media_height_mils: job.settings.media_height_mils,
    };
    send(connection, Body::PrintJobOffer(offer))?;
    let mut offset = 0usize;
    for piece in job.pdf.chunks(print::PIECE_BYTES) {
        send(
            connection,
            Body::PrintJobData(types::PrintJobData {
                job_id: job.job_id,
                offset: offset as u64,
                data: piece.to_vec(),
            }),
        )?;
        offset += piece.len();
    }
    Ok(())
}

/// A print job whose document is still arriving on a [`PrintStream`].
struct ArrivingJob {
    offer: types::PrintJobOffer,
    pdf: Vec<u8>,
}

/// A connection that watches the print jobs of the caller's Android, from
/// [`HostApiClient::watch_print_jobs`].
pub struct PrintStream {
    connection: Box<dyn Connection>,
    arriving: Option<ArrivingJob>,
}

impl PrintStream {
    /// The next whole print job or withdrawal, waiting at most `timeout` for each frame.
    /// `Ok(None)` means nothing whole came in time; an error means the connection has ended
    /// or the service sent something that is not a print job.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<PrintEvent>, ClientError> {
        loop {
            let frame = match self.connection.recv(Some(timeout)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => return Ok(None),
                Err(error) => return Err(ClientError::Transport(error)),
            };
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::PrintJobOffer(offer)),
                }) => {
                    if offer.size == 0 || offer.size > print::MAX_PRINT_BYTES {
                        return Err(ClientError::Malformed(format!(
                            "a print job of {} bytes",
                            offer.size
                        )));
                    }
                    self.arriving = Some(ArrivingJob {
                        offer,
                        pdf: Vec::new(),
                    });
                }
                Ok(Envelope {
                    body: Some(Body::PrintJobData(piece)),
                }) => {
                    let Some(arriving) = self.arriving.as_mut() else {
                        return Err(ClientError::Malformed(
                            "a piece of a print job that was not offered".into(),
                        ));
                    };
                    let received = arriving.pdf.len() as u64;
                    let end = received + piece.data.len() as u64;
                    let in_place = piece.job_id == arriving.offer.job_id
                        && piece.offset == received
                        && end <= arriving.offer.size;
                    if !in_place {
                        return Err(ClientError::Malformed(
                            "a piece of a print job out of place".into(),
                        ));
                    }
                    arriving.pdf.extend_from_slice(&piece.data);
                    if (arriving.pdf.len() as u64) < arriving.offer.size {
                        continue;
                    }
                    let Some(ArrivingJob { offer, pdf }) = self.arriving.take() else {
                        continue;
                    };
                    return Ok(Some(PrintEvent::Job(PrintJob {
                        job_id: offer.job_id,
                        package_name: offer.package_name,
                        app_label: offer.app_label,
                        document_name: offer.document_name,
                        settings: PrintSettings {
                            page_count: offer.page_count,
                            copies: offer.copies.max(1),
                            color: offer.color,
                            duplex: offer.duplex,
                            landscape: offer.landscape,
                            media_width_mils: offer.media_width_mils,
                            media_height_mils: offer.media_height_mils,
                        },
                        pdf: Arc::new(pdf),
                    })));
                }
                Ok(Envelope {
                    body: Some(Body::PrintJobWithdrawn(withdrawn)),
                }) => {
                    return Ok(Some(PrintEvent::Withdrawn {
                        job_id: withdrawn.job_id,
                    }));
                }
                Ok(_) => return Err(ClientError::Malformed("expected a print job".into())),
                Err(error) => return Err(ClientError::Malformed(error.to_string())),
            }
        }
    }
}

fn background_app_to_wire(app: BackgroundApp) -> types::BackgroundApp {
    types::BackgroundApp {
        package_name: app.package_name,
        app_label: app.app_label,
        icon_png: app.icon_png,
        title: app.title,
        text: app.text,
        actions: app.actions,
    }
}

fn media_to_wire(session: MediaSession) -> types::MediaSession {
    let playback = match session.playback {
        Playback::Stopped => types::MediaPlayback::Stopped,
        Playback::Playing => types::MediaPlayback::Playing,
        Playback::Paused => types::MediaPlayback::Paused,
    };
    types::MediaSession {
        package_name: session.package_name,
        app_label: session.app_label,
        title: session.title,
        artist: session.artist,
        album: session.album,
        art_png: session.art_png,
        playback: playback as i32,
        can_play: session.can_play,
        can_pause: session.can_pause,
        can_next: session.can_next,
        can_previous: session.can_previous,
    }
}

fn background_to_wire(apps: Vec<BackgroundApp>, media: Vec<MediaSession>) -> types::BackgroundApps {
    types::BackgroundApps {
        apps: apps.into_iter().map(background_app_to_wire).collect(),
        media: media.into_iter().map(media_to_wire).collect(),
    }
}

fn background_event_to_wire(event: BackgroundEvent) -> Body {
    use types::background_app_event::Body as Event;
    let body = match event {
        BackgroundEvent::App(app) => Event::App(background_app_to_wire(app)),
        BackgroundEvent::AppRemoved { package_name } => {
            Event::AppRemoved(types::BackgroundAppRemoved { package_name })
        }
        BackgroundEvent::Media(session) => Event::Media(media_to_wire(session)),
        BackgroundEvent::MediaRemoved { package_name } => {
            Event::MediaRemoved(types::MediaSessionRemoved { package_name })
        }
    };
    Body::BackgroundAppEvent(types::BackgroundAppEvent { body: Some(body) })
}

fn background_app_from_wire(app: types::BackgroundApp) -> BackgroundApp {
    BackgroundApp {
        package_name: app.package_name,
        app_label: app.app_label,
        icon_png: app.icon_png,
        title: app.title,
        text: app.text,
        actions: app.actions,
    }
}

fn media_from_wire(session: types::MediaSession) -> MediaSession {
    let playback = match types::MediaPlayback::try_from(session.playback) {
        Ok(types::MediaPlayback::Playing) => Playback::Playing,
        Ok(types::MediaPlayback::Paused) => Playback::Paused,
        _ => Playback::Stopped,
    };
    MediaSession {
        package_name: session.package_name,
        app_label: session.app_label,
        title: session.title,
        artist: session.artist,
        album: session.album,
        art_png: session.art_png,
        playback,
        can_play: session.can_play,
        can_pause: session.can_pause,
        can_next: session.can_next,
        can_previous: session.can_previous,
    }
}

fn background_from_wire(list: types::BackgroundApps) -> (Vec<BackgroundApp>, Vec<MediaSession>) {
    (
        list.apps
            .into_iter()
            .map(background_app_from_wire)
            .collect(),
        list.media.into_iter().map(media_from_wire).collect(),
    )
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
