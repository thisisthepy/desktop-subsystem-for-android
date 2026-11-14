//! The DSA Window Manager (`dsa-wm`): the user-session compositor for Windows.
//!
//! Runs in the user session. The graphics renderer runs in a process of its own
//! (`dsa-gfx.exe`, see `crate::renderer`), which the window manager starts and starts again
//! when it ends: it takes the guest's graphics streams (ports 5566 and 5567) on the VM's Hyper-V
//! sockets, which the window manager tells it of. A compositor can also serve the streams
//! itself through a [`GraphicsRenderer`], as the tests do.
//!
//! It owns no window. Each guest app task is shown in a native window of the app's own window
//! host, a small program named and drawn as the app (see `crate::app_window`). The window
//! manager watches the guest's tasks through the service's local API ([`tasks`]), starts
//! each app's window host and tells it which windows to open ([`hosts`]), and has the
//! renderer copy each task's frames, on the GPU, into a texture the window host shows
//! ([`frames`], [`present`]). When the user closes a window, the guest is asked to end its
//! task. The mouse, touch and pen input in a window goes from the window host to the service
//! itself, without passing through the window manager. While no task window is open, it gives
//! back the memory it is not using ([`trim`]). It also carries the text clipboard between
//! the user and their Android ([`clipboard`]), since the service cannot reach the user's
//! clipboard, and opens the web links apps in Android open in the user's default browser
//! ([`links`]). It shows the notifications of the user's Android as toasts, and passes the
//! user's dismissals back ([`notifications`]). It shows the widgets the user placed on the
//! desktop, each in a borderless window of its own on the desktop ([`widgets`]). It prints the documents apps in Android print on the user's
//! printers, through the Windows print dialog ([`print`]). It answers the address lookups of
//! the apps in Android ([`geocode`]), and shows what they share with the PC in Windows' share
//! window ([`share`]). It plays the sound of the user's
//! Android on their default output device ([`audio`]), and records their default capture
//! device for its microphone ([`microphone`]). It answers the text-to-speech engine, speech
//! recognition service and spell checker of the user's Android with Windows' own
//! ([`language`]).

pub mod adapter;
pub mod audio;
pub mod background;
pub mod clipboard;
pub mod committed;
pub mod device_recovery;
pub mod display_gpu;
pub mod fluent;
pub mod fluent_dialog;
pub mod fluent_menu;
pub mod frames;
pub mod gamepads;
pub mod geocode;
pub mod hosts;
pub mod language;
pub mod links;
pub mod location;
pub mod log;
pub mod microphone;
pub mod notifications;
pub mod passkeys;
pub mod pin_shortcuts;
pub mod present;
pub mod print;
pub mod share;
pub mod shutdown;
pub mod single_instance;
pub mod system_settings;
pub mod tasks;
pub mod touch_gestures;
pub mod trim;
pub mod widget_automation;
pub mod widget_menu;
pub mod widgets;
pub mod window_marks;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::app_audio::{self, AppStream, AppVolumes};
use dsa_host_core::audio::{serve_port as serve_audio_port, AudioDevice, AUDIO_PORT};
use dsa_host_core::compositor::frame_stall::{restart_in_background, SharedConnector, Stall};
use dsa_host_core::compositor::{
    GraphicsRenderer, GraphicsStreamKind, StreamTracker, GRAPHICS_COMMAND_PORT,
    GRAPHICS_PROCESS_PIPE_PORT,
};
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::memory::{IdleTrim, IDLE_TRIM_DELAY};
use dsa_host_core::session::SessionState;
use dsa_transport::local::PeerIgnoringListener;
use dsa_transport::{Connector, Listener, TransportError};

use self::adapter::{DisplayPresenter, DisplaySink};
use self::frames::FrameRouter;
use self::hosts::{HostLauncher, ManagerEvent, ManagerSetup};
use self::log::{note, CompositorLogger};
use self::present::Gpu;
use self::single_instance::DEFAULT_MUTEX_NAME;
use crate::guid::Guid;
use crate::hvsocket::{HvSocketListener, RawHvSocketListener};
use crate::pipe::{current_user_only_sddl, NamedPipeConnector, NamedPipeListener};
use crate::start_menu::StartMenuSetup;
use crate::vm_listener::{BindFn, VmBoundListener, VmIdSlot};
use dsa_host_core::tasks::{AppInfo, TaskEvent};

/// Configuration for the user-session compositor.
#[derive(Debug, Clone)]
pub struct CompositorConfig {
    /// Local API named pipe name.
    pub pipe_name: String,
    /// vsock port for the command stream (5566).
    pub command_port: u32,
    /// vsock port for the process pipe (5567).
    pub process_pipe_port: u32,
    /// Named mutex for single-instance per session enforcement.
    pub mutex_name: String,
    /// Optional log folder (defaults to `%LOCALAPPDATA%\DSA\logs`).
    pub log_dir: Option<PathBuf>,
    /// How often to poll the local API for VM state updates.
    pub status_poll_interval: Duration,
}

impl Default for CompositorConfig {
    fn default() -> Self {
        CompositorConfig {
            pipe_name: crate::host_config::DEFAULT_PIPE_NAME.into(),
            command_port: GRAPHICS_COMMAND_PORT,
            process_pipe_port: GRAPHICS_PROCESS_PIPE_PORT,
            mutex_name: DEFAULT_MUTEX_NAME.into(),
            log_dir: None,
            status_poll_interval: Duration::from_millis(100),
        }
    }
}

/// The Windows user-session compositor.
pub struct Compositor {
    config: CompositorConfig,
    renderer: Arc<dyn GraphicsRenderer>,
    tracker: Arc<StreamTracker>,
    vm_id_slot: VmIdSlot,
    local_api_connector: Box<dyn Fn() -> Box<dyn Connector> + Send>,
    bind_command: Option<BindFn>,
    bind_process_pipe: Option<BindFn>,
    /// Binds the audio port (5568), where the guest's audio module sends its sound.
    bind_audio: Option<BindFn>,
    /// Binds the video port, where the guest's codec HAL has its streams decoded. None in
    /// tests, which have no GPU decoders.
    bind_video: Option<BindFn>,
    /// Set when the last video decoder closed and its device was let go, so the idle trim
    /// gives back the memory that freed.
    video_let_go: Arc<AtomicBool>,
    /// Binds the sensor port, where the guest's sensors HAL reads this computer's sensors.
    /// None in tests, which read no sensors.
    bind_sensors: Option<BindFn>,
    /// Binds the camera port, where the guest's camera HAL uses this computer's cameras.
    /// None in tests, which have no cameras.
    bind_camera: Option<BindFn>,
    logger: Option<Arc<CompositorLogger>>,
    /// What copies each display's frames into the task windows, when there is one.
    presenter: Option<Arc<dyn DisplayPresenter>>,
    /// Opens where the app window hosts connect.
    host_listener: Option<HostListenerFn>,
    /// Starts each app's window host.
    launcher: Option<Box<dyn HostLauncher>>,
    windows: Arc<Mutex<Vec<i32>>>,
    /// Where the apps' Start menu shortcuts go, when they are kept.
    start_menu: Option<StartMenuSetup>,
    /// What opens the web links the user's Android opens, when they are followed.
    link_opener: Option<Box<dyn links::LinkOpener>>,
    /// Whether the user's clipboard is shared with their Android.
    share_clipboard: bool,
    /// Whether the notifications of the user's Android are shown as toasts.
    show_notifications: bool,
    /// Whether the widgets of the user's Android are shown on the desktop.
    show_widgets: bool,
    /// Whether the documents the user's Android prints are printed here.
    print_jobs: bool,
    /// Whether the passkeys the apps in the user's Android make and use go to Windows Hello.
    passkeys: bool,
    /// Whether the user's Android gets the computer's location from Windows.
    give_location: bool,
    /// Whether the user's Android speaks, recognises speech and checks spelling with Windows.
    serve_language: bool,
    /// Whether the address lookups of the user's Android are answered here, and what its apps
    /// share with the PC is shown in Windows' share window.
    host_services: bool,
    /// Whether the apps that run in the background in the user's Android are shown in the
    /// notification area, with their media sessions in Windows' media controls.
    show_background: bool,
    /// Turns Windows' touch gestures off while a task window is in front, when the user's
    /// setting says so. None leaves them alone.
    touch_gestures: Option<touch_gestures::TouchGestureSetup>,
    /// What joins the media sessions the tray follows to the media buttons in the taskbar
    /// previews of the task windows.
    media_windows: MediaWindows,
    /// DSA's own news the window manager raises, which the notifications' thread shows.
    dsa_news: notifications::DsaNews,
}

/// The way between the media sessions the tray follows and the media buttons in the task
/// windows' taskbar previews: each session goes to the manager, which tells the app's window
/// host, and each press of a button comes back to the tray's connection to the service, which
/// sends it to the app's session as it sends a media key's.
struct MediaWindows {
    /// The manager's events, once the task windows have started.
    events: Arc<Mutex<Option<mpsc::Sender<ManagerEvent>>>>,
    /// Where the manager sends a press.
    pressed_tx: mpsc::Sender<MediaPress>,
    /// Where the tray's thread reads the presses; taken when it starts.
    pressed_rx: Mutex<Option<mpsc::Receiver<MediaPress>>>,
}

/// A media button pressed in a task window's taskbar preview: the app's package and what the
/// button asks of its media session.
type MediaPress = (String, dsa_host_core::background::MediaCommand);

impl MediaWindows {
    fn new() -> MediaWindows {
        let (pressed_tx, pressed_rx) = mpsc::channel();
        MediaWindows {
            events: Arc::new(Mutex::new(None)),
            pressed_tx,
            pressed_rx: Mutex::new(Some(pressed_rx)),
        }
    }
}

/// The installed apps an event from the watch of the tasks carries, if it carries them.
pub fn installed_in(event: &ManagerEvent) -> Option<Vec<AppInfo>> {
    match event {
        ManagerEvent::Sync(current) => current.iter().find_map(|event| match event {
            TaskEvent::Installed(apps) => Some(apps.clone()),
            _ => None,
        }),
        ManagerEvent::Task(event) => match &**event {
            TaskEvent::Installed(apps) => Some(apps.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// The request to pin a shortcut an event from the watch of the tasks carries, if it carries
/// one. Only a live event does: a request is never part of the tasks a watch starts from.
pub fn pin_request_in(event: &ManagerEvent) -> Option<dsa_host_core::tasks::PinShortcutRequest> {
    match event {
        ManagerEvent::Task(event) => match &**event {
            TaskEvent::PinShortcut(request) => Some(request.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// Opens the listener the app window hosts connect to.
pub type HostListenerFn = Box<dyn FnOnce() -> Result<Box<dyn Listener>, String> + Send>;

/// The session this process runs in.
pub fn current_session() -> u32 {
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0u32;
    // SAFETY: our own process ID and a local to write to.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    session
}

/// The pipe the window hosts of this session dial, which only this user may open.
fn session_host_listener() -> HostListenerFn {
    Box::new(|| {
        let sddl = current_user_only_sddl().map_err(|e| format!("reading this user: {e}"))?;
        let listener = NamedPipeListener::bind(&hosts::pipe_name(current_session()), &sddl)
            .map_err(|e| format!("could not open the app windows' pipe: {e}"))?;
        Ok(Box::new(PeerIgnoringListener(listener)) as Box<dyn Listener>)
    })
}

impl Compositor {
    /// Creates a compositor with standard Windows Hyper-V socket binding functions.
    pub fn new(config: CompositorConfig, renderer: Arc<dyn GraphicsRenderer>) -> Compositor {
        let pipe_name = config.pipe_name.clone();
        let cmd_port = config.command_port;
        let proc_port = config.process_pipe_port;

        let local_api_connector: Box<dyn Fn() -> Box<dyn Connector> + Send> = Box::new(move || {
            Box::new(NamedPipeConnector {
                name: pipe_name.clone(),
            })
        });

        let bind_command: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(cmd_port);
            RawHvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        let bind_process_pipe: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(proc_port);
            RawHvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        // The audio port carries framed control messages, not a raw stream.
        let bind_audio: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(AUDIO_PORT);
            HvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        let bind_video: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(dsa_host_core::video::VIDEO_PORT);
            HvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        let bind_sensors: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(dsa_host_core::sensors::SENSOR_PORT);
            HvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });
        let bind_camera: BindFn = Box::new(move |vm_id| {
            let service_id = Guid::vsock_service(dsa_host_core::camera::CAMERA_PORT);
            HvSocketListener::bind(vm_id, service_id).map(|l| Box::new(l) as Box<dyn Listener>)
        });

        Compositor {
            config,
            renderer,
            tracker: Arc::new(StreamTracker::new()),
            vm_id_slot: VmIdSlot::default(),
            video_let_go: Arc::new(AtomicBool::new(false)),
            local_api_connector,
            bind_command: Some(bind_command),
            bind_process_pipe: Some(bind_process_pipe),
            bind_video: Some(bind_video),
            bind_audio: Some(bind_audio),
            bind_sensors: Some(bind_sensors),
            bind_camera: Some(bind_camera),
            logger: None,
            presenter: None,
            host_listener: Some(session_host_listener()),
            launcher: None,
            windows: Arc::new(Mutex::new(Vec::new())),
            start_menu: crate::start_menu::StartMenuSetup::default_for_user(),
            link_opener: Some(Box::new(links::ShellOpener)),
            share_clipboard: true,
            show_notifications: true,
            show_widgets: true,
            print_jobs: true,
            passkeys: true,
            give_location: true,
            serve_language: true,
            host_services: true,
            show_background: true,
            touch_gestures: touch_gestures::TouchGestureSetup::for_this_user(),
            media_windows: MediaWindows::new(),
            dsa_news: notifications::DsaNews::default(),
        }
    }

    /// Creates a compositor using custom connector and listener binding functions for testing.
    pub fn with_custom_transport(
        config: CompositorConfig,
        renderer: Arc<dyn GraphicsRenderer>,
        local_api_connector: Box<dyn Fn() -> Box<dyn Connector> + Send>,
        bind_command: BindFn,
        bind_process_pipe: BindFn,
    ) -> Compositor {
        Compositor {
            config,
            renderer,
            tracker: Arc::new(StreamTracker::new()),
            vm_id_slot: VmIdSlot::default(),
            video_let_go: Arc::new(AtomicBool::new(false)),
            local_api_connector,
            bind_command: Some(bind_command),
            bind_process_pipe: Some(bind_process_pipe),
            bind_video: None,
            bind_audio: None,
            bind_sensors: None,
            bind_camera: None,
            logger: None,
            presenter: None,
            host_listener: None,
            launcher: None,
            windows: Arc::new(Mutex::new(Vec::new())),
            start_menu: None,
            link_opener: None,
            share_clipboard: false,
            show_notifications: false,
            show_widgets: false,
            print_jobs: false,
            passkeys: false,
            give_location: false,
            serve_language: false,
            host_services: false,
            show_background: false,
            touch_gestures: None,
            media_windows: MediaWindows::new(),
            dsa_news: notifications::DsaNews::default(),
        }
    }

    /// Leaves the guest's graphics streams to the renderer process (`dsa-gfx.exe`), which
    /// binds the VM's graphics ports itself: this process then never listens on them. The
    /// compositor still follows the VM's ID ([`Compositor::vm_id_slot`]), which the renderer is
    /// told of.
    pub fn without_graphics_streams(mut self) -> Compositor {
        self.bind_command = None;
        self.bind_process_pipe = None;
        self
    }

    /// Keeps a Start menu shortcut for each app installed in the user's Android, as `setup`
    /// says where. [`Compositor::new`] keeps the user's own Start menu.
    pub fn with_start_menu(mut self, setup: StartMenuSetup) -> Compositor {
        self.start_menu = Some(setup);
        self
    }

    /// Opens the web links the user's Android opens with `opener`. [`Compositor::new`] opens
    /// them in the user's default browser.
    pub fn with_link_opener(mut self, opener: Box<dyn links::LinkOpener>) -> Compositor {
        self.link_opener = Some(opener);
        self
    }

    /// Starts opening the web links the user's Android opens, on a thread of its own that
    /// ends with `stop`.
    fn start_links(&mut self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let mut opener = self.link_opener.take()?;
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-links".into())
            .spawn(move || links::follow(connector, &mut *opener, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the links' thread: {e}"),
                )
            })
            .ok()
    }

    /// Takes the app window hosts' connections from `listener`, and starts each app's window
    /// host through `launcher`. Without them, the window hosts connect on this session's pipe
    /// and each app's window program is made and run from the installed window host.
    pub fn with_window_hosts(
        mut self,
        listener: HostListenerFn,
        launcher: Box<dyn HostLauncher>,
    ) -> Compositor {
        self.host_listener = Some(listener);
        self.launcher = Some(launcher);
        self
    }

    /// Turns Windows' touch gestures off through `setup` while a task window is in front, when
    /// the user's setting says so. [`Compositor::new`] does it for the user this process runs
    /// as; [`Compositor::with_custom_transport`] leaves them alone.
    pub fn with_touch_gestures(mut self, setup: touch_gestures::TouchGestureSetup) -> Compositor {
        self.touch_gestures = Some(setup);
        self
    }

    /// Shows each task's frames in its window through `presenter`. Without one, the task
    /// windows open and close with their tasks but stay empty.
    pub fn with_display_presenter(mut self, presenter: Arc<dyn DisplayPresenter>) -> Compositor {
        self.presenter = Some(presenter);
        self
    }

    /// The tasks that have a window, in task ID order, kept up to date while the window
    /// manager runs.
    pub fn task_windows(&self) -> Arc<Mutex<Vec<i32>>> {
        Arc::clone(&self.windows)
    }

    /// Sets an optional file logger.
    pub fn with_logger(mut self, logger: Arc<CompositorLogger>) -> Compositor {
        self.logger = Some(logger);
        self
    }

    /// The stream tracker recording active and total connections.
    pub fn stream_tracker(&self) -> Arc<StreamTracker> {
        Arc::clone(&self.tracker)
    }

    /// The shared slot holding the current VM ID.
    pub fn vm_id_slot(&self) -> VmIdSlot {
        Arc::clone(&self.vm_id_slot)
    }

    /// Runs the compositor until `stop` is set.
    pub fn run(mut self, stop: Arc<AtomicBool>) {
        if let Some(logger) = &self.logger {
            logger.log("dsa-wm starting");
        }

        // The guest's graphics streams, when this process serves them itself. The window
        // manager leaves them to its renderer process (`without_graphics_streams`).
        let cmd_listener = self
            .bind_command
            .take()
            .map(|bind| VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind));
        let proc_listener = self
            .bind_process_pipe
            .take()
            .map(|bind| VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind));

        // 0. The app window hosts, the watch of the guest's tasks, and the requests to end
        // them.
        let task_windows = self.start_task_windows(&stop);
        let links_thread = self.start_links(&stop);
        let clipboard_thread = self.start_clipboard(&stop);
        let location_thread = self.start_location(&stop);
        let language_thread = self.start_language(&stop);
        let notifications_thread = self.start_notifications(&stop);
        let widgets_thread = self.start_widgets(&stop);
        let background_thread = self.start_background(&stop);
        let video_thread = self.start_video(&stop);
        let print_thread = self.start_print(&stop);
        let passkeys_thread = self.start_passkeys(&stop);
        let geocode_thread = self.start_geocoding(&stop);
        let share_thread = self.start_shares(&stop);
        let audio_thread = self.start_audio(&stop);
        let sensors_thread = self.start_sensors(&stop);
        let camera_thread = self.start_camera(&stop);
        let system_settings_thread = self.start_system_settings(&stop);
        let gamepads_thread = self.start_gamepads(&stop);

        // 1. Monitor thread that tracks the VM ID and state via the local API.
        let monitor_stop = Arc::clone(&stop);
        let monitor_slot = Arc::clone(&self.vm_id_slot);
        let monitor_connector_factory = (self.local_api_connector)();
        let poll_interval = self.config.status_poll_interval;
        let monitor_logger = self.logger.clone();

        let monitor_thread = thread::Builder::new()
            .name("dsa-wm-monitor".into())
            .spawn(move || {
                let mut connector = monitor_connector_factory;
                let mut client_opt: Option<HostApiClient> = None;
                let mut monitors = crate::monitors::MonitorWatch::default();

                while !monitor_stop.load(Ordering::Relaxed) {
                    // The next start of Android sizes its display from this record.
                    if let Err(why) = monitors.check() {
                        if let Some(logger) = &monitor_logger {
                            logger.log(&why);
                        }
                    }
                    let client = match client_opt.as_mut() {
                        Some(c) => c,
                        None => {
                            match HostApiClient::connect(
                                &mut *connector,
                                "dsa-wm",
                                Duration::from_secs(1),
                            ) {
                                Ok(c) => {
                                    client_opt = Some(c);
                                    client_opt.as_mut().expect("client set")
                                }
                                Err(_) => {
                                    *monitor_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                    thread::sleep(poll_interval);
                                    continue;
                                }
                            }
                        }
                    };

                    match client.status() {
                        Ok(status) => {
                            // The guest renders (surfaceflinger, the boot animation)
                            // while it boots, before the agent connects, so the
                            // listeners stay bound whenever the VM is running.
                            let new_id = if matches!(
                                status.state,
                                SessionState::Starting
                                    | SessionState::Stalled
                                    | SessionState::Active
                                    | SessionState::Dozing
                                    | SessionState::Frozen
                            ) {
                                client
                                    .vm_id()
                                    .ok()
                                    .flatten()
                                    .and_then(|id_str| id_str.parse::<Guid>().ok())
                            } else {
                                None
                            };

                            let mut slot = monitor_slot.lock().unwrap_or_else(|e| e.into_inner());
                            if *slot != new_id {
                                if let Some(logger) = &monitor_logger {
                                    logger.log(&format!(
                                        "VM state changed: {:?}, ID: {:?}",
                                        status.state, new_id
                                    ));
                                }
                                *slot = new_id;
                            }
                        }
                        Err(_) => {
                            client_opt = None;
                            *monitor_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        }
                    }

                    thread::sleep(poll_interval);
                }
            })
            .expect("spawning compositor monitor thread");

        // 2. Command stream listener thread.
        let cmd_stop = Arc::clone(&stop);
        let cmd_renderer = Arc::clone(&self.renderer);
        let cmd_tracker = Arc::clone(&self.tracker);
        let cmd_logger = self.logger.clone();

        let cmd_thread = thread::Builder::new()
            .name("dsa-wm-cmd".into())
            .spawn(move || {
                let Some(mut cmd_listener) = cmd_listener else {
                    return;
                };
                while !cmd_stop.load(Ordering::Relaxed) {
                    match cmd_listener.accept(Duration::from_millis(100)) {
                        Ok(conn) => {
                            let renderer = Arc::clone(&cmd_renderer);
                            let tracker = Arc::clone(&cmd_tracker);
                            let logger = cmd_logger.clone();

                            let _ = thread::Builder::new().name("dsa-stream-cmd".into()).spawn(
                                move || {
                                    tracker.stream_started(GraphicsStreamKind::Command);
                                    if let Err(e) =
                                        renderer.serve_stream(GraphicsStreamKind::Command, conn)
                                    {
                                        if let Some(log) = logger {
                                            log.log(&format!("error serving command stream: {e}"));
                                        }
                                    }
                                    tracker.stream_ended(GraphicsStreamKind::Command);
                                },
                            );
                        }
                        Err(TransportError::Timeout) => {}
                        Err(e) => {
                            if let Some(log) = &cmd_logger {
                                log.log(&format!("command stream accept error: {e}"));
                            }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .expect("spawning compositor command thread");

        // 3. Process pipe listener thread.
        let proc_stop = Arc::clone(&stop);
        let proc_renderer = Arc::clone(&self.renderer);
        let proc_tracker = Arc::clone(&self.tracker);
        let proc_logger = self.logger.clone();

        let proc_thread = thread::Builder::new()
            .name("dsa-wm-pipe".into())
            .spawn(move || {
                let Some(mut proc_listener) = proc_listener else {
                    return;
                };
                while !proc_stop.load(Ordering::Relaxed) {
                    match proc_listener.accept(Duration::from_millis(100)) {
                        Ok(conn) => {
                            let renderer = Arc::clone(&proc_renderer);
                            let tracker = Arc::clone(&proc_tracker);
                            let logger = proc_logger.clone();

                            let _ = thread::Builder::new().name("dsa-stream-pipe".into()).spawn(
                                move || {
                                    tracker.stream_started(GraphicsStreamKind::ProcessPipe);
                                    if let Err(e) =
                                        renderer.serve_stream(GraphicsStreamKind::ProcessPipe, conn)
                                    {
                                        if let Some(log) = logger {
                                            log.log(&format!("error serving process pipe: {e}"));
                                        }
                                    }
                                    tracker.stream_ended(GraphicsStreamKind::ProcessPipe);
                                },
                            );
                        }
                        Err(TransportError::Timeout) => {}
                        Err(e) => {
                            if let Some(log) = &proc_logger {
                                log.log(&format!("process pipe accept error: {e}"));
                            }
                            thread::sleep(Duration::from_millis(50));
                        }
                    }
                }
            })
            .expect("spawning compositor process pipe thread");

        // Wait until stop signal.
        while !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
        }

        task_windows.stop(self.presenter.as_deref());
        let _ = monitor_thread.join();
        let _ = cmd_thread.join();
        let _ = proc_thread.join();
        if let Some(thread) = links_thread {
            let _ = thread.join();
        }
        if let Some(thread) = clipboard_thread {
            let _ = thread.join();
        }
        if let Some(thread) = location_thread {
            let _ = thread.join();
        }
        if let Some(thread) = language_thread {
            let _ = thread.join();
        }
        if let Some(thread) = notifications_thread {
            let _ = thread.join();
        }
        if let Some(thread) = widgets_thread {
            let _ = thread.join();
        }
        if let Some(thread) = background_thread {
            let _ = thread.join();
        }
        if let Some(thread) = video_thread {
            let _ = thread.join();
        }
        if let Some(thread) = print_thread {
            let _ = thread.join();
        }
        if let Some(thread) = passkeys_thread {
            let _ = thread.join();
        }
        if let Some(thread) = geocode_thread {
            let _ = thread.join();
        }
        if let Some(thread) = share_thread {
            let _ = thread.join();
        }
        if let Some(thread) = audio_thread {
            let _ = thread.join();
        }
        if let Some(thread) = sensors_thread {
            let _ = thread.join();
        }
        if let Some(thread) = camera_thread {
            let _ = thread.join();
        }
        if let Some(thread) = system_settings_thread {
            let _ = thread.join();
        }
        if let Some(thread) = gamepads_thread {
            let _ = thread.join();
        }

        if let Some(logger) = &self.logger {
            logger.log("dsa-wm stopped cleanly");
        }
    }

    /// Starts decoding the guest's video on this computer's GPU, on a thread of its own that
    /// ends with `stop`: the guest's codec HAL connects to the video port for each decoder.
    /// The formats offered are those this computer's GPU decodes, found once at start.
    fn start_video(&mut self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let bind = self.bind_video.take()?;
        let listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind);
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let let_go = Arc::clone(&self.video_let_go);
        thread::Builder::new()
            .name("dsa-wm-video".into())
            .spawn(move || {
                let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
                    if let Some(logger) = &logger {
                        logger.log(line);
                    }
                });
                let probe_log = Arc::clone(&log);
                let decoders = crate::video_decoder::MediaFoundationDecoders::lazy(
                    Box::new(move |line: &str| probe_log(line)),
                    // The trim gives back what the decoders freed a few seconds later.
                    Box::new(move || let_go.store(true, Ordering::SeqCst)),
                );
                let build = format!("dsa-wm {}", crate::VERSION);
                dsa_host_core::video::serve(
                    Box::new(listener),
                    Arc::new(decoders),
                    build,
                    stop,
                    log,
                );
            })
            .ok()
    }

    /// Starts playing the sound of the user's Android on their default output device, on a
    /// thread of its own that ends with `stop`: it listens on the audio port of the user's
    /// VM, and plays each stream the guest opens there. A device that cannot play is logged
    /// and the stream goes on unheard, so the guest is never held up. Each app's output plays
    /// as that app in Windows' volume mixer, at the volume Android gives it, and a volume the
    /// user sets there goes back to Android; a second thread follows that over the local API.
    fn start_audio(&mut self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let bind = self.bind_audio.take()?;
        let mut listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind);
        let mut connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-audio".into())
            .spawn(move || {
                let log: Arc<dyn Fn(&str) + Send + Sync> =
                    Arc::new(move |line: &str| note(logger.as_deref(), line));
                let build = format!("dsa-wm {}", crate::VERSION);
                let volumes = AppVolumes::new();
                let follower = {
                    let (volumes, stop, log, build) = (
                        Arc::clone(&volumes),
                        Arc::clone(&stop),
                        Arc::clone(&log),
                        build.clone(),
                    );
                    thread::Builder::new()
                        .name("dsa-wm-app-audio".into())
                        .spawn(move || {
                            app_audio::follow(
                                &mut *connector,
                                &build,
                                &volumes,
                                &stop,
                                &*log,
                                &|_: &[AppStream]| {},
                            )
                        })
                };
                if let Err(e) = &follower {
                    log(&format!(
                        "could not start the app volumes' thread; every app plays as one: {e}"
                    ));
                }
                let device: Arc<dyn AudioDevice> = Arc::new(audio::DefaultOutput::new(
                    Arc::clone(&log),
                    Arc::clone(&volumes),
                ));
                serve_audio_port(&mut listener, device, &build, &stop, log);
                if let Ok(follower) = follower {
                    let _ = follower.join();
                }
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the audio port's thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts serving this computer's sensors to the guest, on a thread of its own that ends
    /// with `stop`: the guest's sensors HAL connects to the sensor port, and each sensor runs
    /// only while an app in the guest listens to it. The window manager serves them because
    /// it runs in the user's session, where Windows lets the user's own apps read sensors.
    fn start_sensors(&mut self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let bind = self.bind_sensors.take()?;
        let listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind);
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-sensors".into())
            .spawn(move || {
                let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
                    if let Some(logger) = &logger {
                        logger.log(line);
                    }
                });
                let build = format!("dsa-wm {}", crate::VERSION);
                dsa_host_core::sensors::serve(
                    Box::new(listener),
                    Arc::new(crate::sensors::WindowsSensors),
                    build,
                    stop,
                    log,
                );
            })
            .ok()
    }

    /// Starts lending this computer's cameras to the apps in the guest, on a thread of its own
    /// that ends with `stop`: the guest's camera HAL connects to the camera port to list them
    /// and for each camera an app opens. A camera is opened only while an app uses it, in
    /// this user's session, so Windows applies this user's privacy settings and shows its
    /// in-use indicator for DSA.
    fn start_camera(&mut self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let bind = self.bind_camera.take()?;
        let listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind);
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-camera".into())
            .spawn(move || {
                let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| {
                    if let Some(logger) = &logger {
                        logger.log(line);
                    }
                });
                let camera_log = Arc::clone(&log);
                let cameras = crate::camera_capture::MediaFoundationCameras::new(Box::new(
                    move |line: &str| camera_log(line),
                ));
                let build = format!("dsa-wm {}", crate::VERSION);
                dsa_host_core::camera::serve(
                    Box::new(listener),
                    Arc::new(cameras),
                    build,
                    stop,
                    log,
                );
            })
            .ok()
    }

    /// Starts carrying the text clipboard between the user and their Android, on a thread of
    /// its own that ends with `stop`. A clipboard that cannot be followed is logged; the rest
    /// of the window manager goes on.
    fn start_clipboard(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.share_clipboard {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-clipboard".into())
            .spawn(move || {
                let (window, changes) = match clipboard::ClipboardWindow::open() {
                    Ok(opened) => opened,
                    Err(why) => {
                        note(
                            logger.as_deref(),
                            &format!("the clipboard is not shared with Android: {why}"),
                        );
                        return;
                    }
                };
                let mut host = window.clipboard();
                clipboard::sync(connector, &mut host, changes, stop, logger);
                drop(window);
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the clipboard's thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts giving the user's Android the user's time zone, languages, regional format, theme
    /// and accent color, on a thread of its own that ends with `stop`.
    fn start_system_settings(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-system-settings".into())
            .spawn(move || system_settings::follow(connector, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the system settings' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts carrying the user's game controllers to their Android, on a thread of its own
    /// that ends with `stop`.
    fn start_gamepads(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-gamepads".into())
            .spawn(move || gamepads::follow(connector, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the game controllers' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts giving the user's Android the computer's location, on a thread of its own that
    /// ends with `stop`. Windows' location service runs only while Android wants a location.
    fn start_location(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.give_location {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-location".into())
            .spawn(move || location::follow(connector, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the location's thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts answering the speech and spelling requests of the user's Android with Windows'
    /// voices, speech recognition and spell checker, on a thread of its own that ends with
    /// `stop`. Each of the three runs on a thread of its own besides.
    fn start_language(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.serve_language {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-language".into())
            .spawn(move || language::serve(connector, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the speech and spelling thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts showing the widgets of the user's Android on the desktop, on a thread of its own
    /// that ends with `stop`. Windows that cannot open are logged; the rest of the window
    /// manager goes on.
    fn start_widgets(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.show_widgets {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-widgets".into())
            .spawn(move || {
                let mut windows = match widgets::WindowsWidgetWindows::open(logger.clone()) {
                    Ok(windows) => windows,
                    Err(why) => {
                        note(
                            logger.as_deref(),
                            &format!("widgets are not shown on the desktop: {why}"),
                        );
                        return;
                    }
                };
                widgets::sync(connector, &mut windows, stop, logger);
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the widgets' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts showing the apps that run in the background in the user's Android in the
    /// notification area, on a thread of its own that ends with `stop`. A tray that cannot
    /// open is logged; the rest of the window manager goes on.
    fn start_background(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.show_background {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        let events = Arc::clone(&self.media_windows.events);
        let pressed = self
            .media_windows
            .pressed_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        thread::Builder::new()
            .name("dsa-wm-background".into())
            .spawn(move || {
                let mut tray = match background::WindowsTray::open(logger.clone()) {
                    Ok(tray) => tray,
                    Err(why) => {
                        note(
                            logger.as_deref(),
                            &format!("the apps that run in the background are not shown: {why}"),
                        );
                        return;
                    }
                };
                let Some(pressed) = pressed else {
                    background::sync(connector, &mut tray, stop, logger);
                    return;
                };
                // Each media session also reaches its app's windows, for the media buttons in
                // their taskbar previews, and those buttons' presses come back here.
                let mut linked = background::WithWindowMedia {
                    tray: &mut tray,
                    windows: Box::new(move |package: &str, media| {
                        let events = events.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(events) = events.as_ref() {
                            let _ = events.send(ManagerEvent::Media {
                                package: package.to_string(),
                                media,
                            });
                        }
                    }),
                    pressed,
                };
                background::sync(connector, &mut linked, stop, logger);
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the background apps' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts answering the address lookups of the user's Android, on a thread of its own that
    /// ends with `stop`.
    fn start_geocoding(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.host_services {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-geocode".into())
            .spawn(move || geocode::serve(connector, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the address lookups' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts showing what the apps in the user's Android share with the PC, on a thread of
    /// its own that ends with `stop`. Each share window runs on a thread of its own besides.
    fn start_shares(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.host_services {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-share".into())
            .spawn(move || share::serve(connector, stop, logger))
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the shares' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts printing the documents the user's Android prints, on a thread of its own that
    /// ends with `stop`. Each job's dialog runs on a thread of its own besides.
    fn start_print(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.print_jobs {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-print-jobs".into())
            .spawn(move || {
                let printer = Arc::new(print::WindowsPrinter);
                let spool = print::default_spool_dir();
                print::serve(connector, printer, spool, stop, logger);
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the print jobs' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts performing the passkey ceremonies of the user's Android with Windows Hello, on
    /// a thread of its own that ends with `stop`. Each ceremony runs on a thread of its own
    /// besides.
    fn start_passkeys(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        if !self.passkeys {
            return None;
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        thread::Builder::new()
            .name("dsa-wm-passkeys".into())
            .spawn(move || {
                let hello = Arc::new(passkeys::WindowsHello);
                passkeys::serve(connector, hello, stop, logger);
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the passkeys' thread: {e}"),
                )
            })
            .ok()
    }

    /// Starts showing the notifications of the user's Android as toasts, on a thread of its
    /// own that ends with `stop`. The protocol a click on a toast launches is registered for
    /// this program first; a problem with either is logged, and the rest of the window manager
    /// goes on.
    fn start_notifications(&self, stop: &Arc<AtomicBool>) -> Option<thread::JoinHandle<()>> {
        for why in crate::user_registry::remove_old_names_for_current_user() {
            note(
                self.logger.as_deref(),
                &format!("a registry key an earlier version wrote stays: {why}"),
            );
        }
        // "Android" in Explorer's navigation pane, kept pointing at this user's files door.
        if let Err(why) = crate::android_files::register_in_navigation_pane() {
            note(
                self.logger.as_deref(),
                &format!("Android is not in Explorer's navigation pane: {why}"),
            );
        }
        if !self.show_notifications {
            return None;
        }
        match std::env::current_exe() {
            Ok(program) => {
                if let Err(why) = notifications::register_protocol(&program) {
                    note(
                        self.logger.as_deref(),
                        &format!("clicking a notification cannot open it: {why}"),
                    );
                }
            }
            Err(why) => note(
                self.logger.as_deref(),
                &format!("clicking a notification cannot open it: {why}"),
            ),
        }
        let connector = (self.local_api_connector)();
        let stop = Arc::clone(stop);
        let logger = self.logger.clone();
        let failed = self.logger.clone();
        let news = Arc::clone(&self.dsa_news);
        thread::Builder::new()
            .name("dsa-wm-notifications".into())
            .spawn(move || {
                // WinRT, which shows the toasts, needs COM on this thread.
                let _com = crate::start_menu::ComApartment::enter();
                let mut toasts = notifications::WindowsToasts::new();
                notifications::sync(
                    connector,
                    &mut toasts,
                    notifications::default_icon_dir(),
                    news,
                    stop,
                    logger,
                );
            })
            .map_err(|e| {
                note(
                    failed.as_deref(),
                    &format!("could not start the notifications' thread: {e}"),
                )
            })
            .ok()
    }

    /// What the manager does when the guest's compositor did not recover from a GPU reset:
    /// tells the user, as DSA's own news, and restarts Android through the service on a
    /// thread of its own, opening the apps that had windows again.
    fn recovery(&self) -> hosts::Recovery {
        let connector = Arc::new(Mutex::new((self.local_api_connector)()));
        let news = Arc::clone(&self.dsa_news);
        let logger = self.logger.clone();
        Box::new(move |stall: &Stall, packages: Vec<String>| {
            news.lock().unwrap_or_else(|e| e.into_inner()).push_back(
                stall
                    .notice()
                    .notification(notifications::user_reads_korean()),
            );
            if !stall.restart {
                return;
            }
            let said = logger.clone();
            let log: Arc<dyn Fn(&str) + Send + Sync> =
                Arc::new(move |line: &str| note(said.as_deref(), line));
            if let Err(why) = restart_in_background(
                Box::new(SharedConnector(Arc::clone(&connector))),
                "dsa-wm restart after a GPU reset".into(),
                packages,
                Arc::clone(&log),
            ) {
                log(&format!("could not start restarting Android: {why}"));
            }
        })
    }

    /// Starts the task windows: the manager, the listener for the app window hosts, the watch
    /// of the guest's tasks and the requests to the guest. A part that cannot start is
    /// logged; the rest of the window manager goes on.
    fn start_task_windows(&mut self, stop: &Arc<AtomicBool>) -> TaskWindowThreads {
        let logger = self.logger.clone();
        let log = |message: &str| note(logger.as_deref(), message);
        // First of all, a value an earlier run left off is put back, even when no task window
        // can open now.
        let gestures = self.touch_gestures.take().map(|setup| {
            let (gestures, notes) = touch_gestures::TouchGestures::start(setup);
            for line in &notes {
                log(line);
            }
            gestures
        });
        let router = Arc::new(FrameRouter::new());
        if let Some(presenter) = &self.presenter {
            let presenter = Arc::clone(presenter);
            router.on_forget(Box::new(move |display| presenter.forget_display(display)));
        }
        let mut luid = None;
        let gpu = match &self.presenter {
            None => None,
            Some(presenter) => {
                luid = presenter.adapter_luid();
                match Gpu::new(luid, false) {
                    Ok(gpu) => {
                        if luid.is_some() && !gpu.on_renderer_gpu() {
                            log("the renderer's GPU was not found; task windows may stay empty");
                        }
                        match presenter
                            .present_displays(Some(Arc::clone(&router) as Arc<dyn DisplaySink>))
                        {
                            Ok(()) => Some(gpu),
                            Err(why) => {
                                log(&why);
                                None
                            }
                        }
                    }
                    Err(why) => {
                        log(&format!("task windows cannot show frames: {why}"));
                        None
                    }
                }
            }
        };
        let launcher: Box<dyn HostLauncher> = match self.launcher.take() {
            Some(launcher) => launcher,
            None => match (
                crate::app_exe::default_template(),
                crate::app_exe::default_apps_dir(),
            ) {
                (Some(template), Some(apps_dir)) => Box::new(crate::app_exe::ProgramLauncher::new(
                    template,
                    apps_dir,
                    self.logger.clone(),
                )),
                _ => {
                    log("cannot tell where the app window programs go; task windows cannot open");
                    return TaskWindowThreads::default();
                }
            },
        };
        let listener = match self.host_listener.take().map(|open| open()) {
            Some(Ok(listener)) => listener,
            Some(Err(why)) => {
                log(&format!("task windows cannot open: {why}"));
                return TaskWindowThreads::default();
            }
            None => {
                log("task windows cannot open: nowhere for the app windows to connect");
                return TaskWindowThreads::default();
            }
        };

        let (events_tx, events_rx) = mpsc::channel();
        let (requests_tx, requests_rx) = mpsc::channel();
        let manager_stop = Arc::new(AtomicBool::new(false));
        let (fronts_tx, fronts_rx) = mpsc::channel();
        let gestures_thread = gestures.and_then(|gestures| {
            let connector = (self.local_api_connector)();
            let stop = Arc::clone(&manager_stop);
            let logger = self.logger.clone();
            thread::Builder::new()
                .name("dsa-wm-touch-gestures".into())
                .spawn(move || touch_gestures::follow(connector, fronts_rx, gestures, stop, logger))
                .map_err(|e| log(&format!("could not start the touch gestures' thread: {e}")))
                .ok()
        });
        let fronts = gestures_thread.as_ref().map(|_| fronts_tx);
        let idle = Arc::new(Mutex::new(IdleTrim::new(IDLE_TRIM_DELAY, Instant::now())));
        let trim_gpu = gpu.clone();
        let trim_renderer = self.presenter.clone();
        let quiet_router = Arc::clone(&router);
        let setup = ManagerSetup {
            events: events_rx,
            requests: requests_tx,
            router,
            gpu,
            launcher,
            logger: self.logger.clone(),
            listing: Arc::clone(&self.windows),
            stop: Arc::clone(&manager_stop),
            idle: Arc::clone(&idle),
            fronts,
            renderer_generation: self.presenter.clone().map(|presenter| {
                Box::new(move || presenter.device_generation()) as Box<dyn Fn() -> u64 + Send>
            }),
            media_pressed: Some(self.media_windows.pressed_tx.clone()),
            recover: Some(self.recovery()),
        };
        // The media sessions the tray follows reach the windows through the manager.
        *self
            .media_windows
            .events
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(events_tx.clone());
        let manager = thread::Builder::new()
            .name("dsa-wm-manager".into())
            .spawn(move || hosts::manage(setup))
            .map_err(|e| log(&format!("could not start the window manager's thread: {e}")))
            .ok();

        let accept_events = events_tx.clone();
        let accept_stop = Arc::clone(&manager_stop);
        let accept_logger = self.logger.clone();
        let acceptor = thread::Builder::new()
            .name("dsa-wm-hosts".into())
            .spawn(move || {
                hosts::accept_hosts(listener, accept_events, luid, accept_stop, accept_logger)
            })
            .map_err(|e| log(&format!("could not take the app windows' connections: {e}")))
            .ok();

        // The Start menu follows the installed apps on a thread of its own, since making the
        // apps' programs and shortcuts takes a while and the windows must not wait on it.
        let mut start_menu = None;
        let mut installed_tx = None;
        // An app's request to pin a shortcut on the desktop is asked about on a thread of its
        // own, which waits on the user; it ends when the watch of tasks does.
        let mut pin_tx = None;
        if self.start_menu.is_some() {
            let (tx, rx) = mpsc::channel();
            let pin_connector = (self.local_api_connector)();
            let pin_logger = self.logger.clone();
            if thread::Builder::new()
                .name("dsa-wm-pin-shortcuts".into())
                .spawn(move || pin_shortcuts::serve(rx, pin_connector, pin_logger))
                .map_err(|e| log(&format!("could not start the pin requests' thread: {e}")))
                .is_ok()
            {
                pin_tx = Some(tx);
            }
        }
        if let Some(setup) = self.start_menu.take() {
            let (tx, rx) = mpsc::channel();
            let menu_logger = self.logger.clone();
            start_menu = thread::Builder::new()
                .name("dsa-wm-start-menu".into())
                .spawn(move || crate::start_menu::keep(rx, setup, menu_logger))
                .map_err(|e| log(&format!("could not start the Start menu's thread: {e}")))
                .ok();
            installed_tx = Some(tx);
        }
        let trim_idle = Arc::clone(&idle);
        let trim_let_go = Arc::clone(&self.video_let_go);
        let trim_stop = Arc::clone(&manager_stop);
        let trim_logger = self.logger.clone();
        let trimmer = thread::Builder::new()
            .name("dsa-wm-trim".into())
            .spawn(move || {
                let quiet_for = Box::new(move || quiet_router.quiet_for());
                trim::run(
                    trim_idle,
                    trim_gpu,
                    trim_renderer,
                    quiet_for,
                    trim_let_go,
                    trim_stop,
                    trim_logger,
                )
            })
            .map_err(|e| log(&format!("could not start the idle memory trim: {e}")))
            .ok();

        let watch_events = events_tx.clone();
        let watch_connector = (self.local_api_connector)();
        let watch_stop = Arc::clone(stop);
        let watch_logger = self.logger.clone();
        let watcher = thread::Builder::new()
            .name("dsa-wm-tasks".into())
            .spawn(move || {
                tasks::watch(
                    watch_connector,
                    |event| {
                        if let (Some(tx), Some(apps)) = (&installed_tx, installed_in(&event)) {
                            let _ = tx.send(apps);
                        }
                        if let (Some(tx), Some(request)) = (&pin_tx, pin_request_in(&event)) {
                            let _ = tx.send((request, Instant::now()));
                        }
                        note_tasks(&idle, &event);
                        watch_events.send(event).is_ok()
                    },
                    watch_stop,
                    watch_logger,
                )
            })
            .map_err(|e| log(&format!("could not start the watch of tasks: {e}")))
            .ok();

        let settings_events = events_tx.clone();
        let settings_connector = (self.local_api_connector)();
        let settings_stop = Arc::clone(&manager_stop);
        let settings_logger = self.logger.clone();
        let app_windows = thread::Builder::new()
            .name("dsa-wm-app-windows".into())
            .spawn(move || {
                tasks::follow_app_windows(
                    settings_connector,
                    |apps| settings_events.send(ManagerEvent::AppWindows(apps)).is_ok(),
                    settings_stop,
                    settings_logger,
                )
            })
            .map_err(|e| {
                log(&format!(
                    "could not start the watch of app window settings: {e}"
                ))
            })
            .ok();

        let request_events = events_tx;
        let request_connector = (self.local_api_connector)();
        let request_logger = self.logger.clone();
        let requester = thread::Builder::new()
            .name("dsa-wm-requests".into())
            .spawn(move || {
                tasks::serve_requests(
                    request_connector,
                    requests_rx,
                    |event| request_events.send(event).is_ok(),
                    request_logger,
                )
            })
            .map_err(|e| log(&format!("could not start the task requests: {e}")))
            .ok();

        TaskWindowThreads {
            stop: Some(manager_stop),
            threads: [
                manager,
                acceptor,
                watcher,
                requester,
                trimmer,
                start_menu,
                app_windows,
                gestures_thread,
            ]
            .into_iter()
            .flatten()
            .collect(),
        }
    }
}

/// Tells the idle memory trim which task windows are open.
fn note_tasks(idle: &Mutex<IdleTrim>, event: &ManagerEvent) {
    let mut idle = idle.lock().unwrap_or_else(|e| e.into_inner());
    let now = Instant::now();
    match event {
        ManagerEvent::Sync(events) => idle.sync(events, now),
        ManagerEvent::Task(event) => idle.task_event(Some(event.as_ref()), now),
        ManagerEvent::Lost => idle.task_event(None, now),
        _ => {}
    }
}

/// The threads behind the task windows.
#[derive(Default)]
struct TaskWindowThreads {
    /// Stops the manager and the listener for the app window hosts.
    stop: Option<Arc<AtomicBool>>,
    threads: Vec<thread::JoinHandle<()>>,
}

impl TaskWindowThreads {
    /// Stops the frames first, then the manager, which closes every app window host, then
    /// the other threads. The watch ends on the window manager's stop flag; the request
    /// thread ends with the manager.
    fn stop(self, presenter: Option<&dyn DisplayPresenter>) {
        if let Some(presenter) = presenter {
            let _ = presenter.present_displays(None);
        }
        if let Some(stop) = &self.stop {
            stop.store(true, Ordering::SeqCst);
        }
        for thread in self.threads {
            let _ = thread.join();
        }
    }
}
