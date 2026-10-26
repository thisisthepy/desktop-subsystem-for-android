//! The DSA Window Manager (`dsa-wm`): the user-session compositor for Windows.
//!
//! Runs in the user session, listens on Hyper-V sockets for guest graphics streams
//! (ports 5566 and 5567), and delegates accepted streams to a [`GraphicsRenderer`].
//!
//! It owns no window. Each guest app task is shown in a native window of the app's own window
//! host, a small program named and drawn as the app (see `crate::app_window`). The window
//! manager watches the guest's tasks through the service's local API ([`tasks`]), starts
//! each app's window host and tells it which windows to open ([`hosts`]), and has the
//! renderer copy each task's frames, on the GPU, into a texture the window host shows
//! ([`frames`], [`present`]). When the user closes a window, the guest is asked to end its
//! task. The mouse, touch and pen input in a window goes from the window host to the service
//! itself, without passing through the window manager.

pub mod adapter;
pub mod display_gpu;
pub mod frames;
pub mod hosts;
pub mod log;
pub mod present;
pub mod shutdown;
pub mod single_instance;
pub mod tasks;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::compositor::{
    GraphicsRenderer, GraphicsStreamKind, StreamTracker, GRAPHICS_COMMAND_PORT,
    GRAPHICS_PROCESS_PIPE_PORT,
};
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::session::SessionState;
use dsa_transport::local::PeerIgnoringListener;
use dsa_transport::{Connector, Listener, TransportError};

use self::adapter::{DisplayPresenter, DisplaySink};
use self::frames::FrameRouter;
use self::hosts::{HostLauncher, ManagerSetup};
use self::log::{note, CompositorLogger};
use self::present::Gpu;
use self::single_instance::DEFAULT_MUTEX_NAME;
use crate::guid::Guid;
use crate::hvsocket::RawHvSocketListener;
use crate::pipe::{current_user_only_sddl, NamedPipeConnector, NamedPipeListener};
use crate::vm_listener::{BindFn, VmBoundListener, VmIdSlot};

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
    logger: Option<Arc<CompositorLogger>>,
    /// What copies each display's frames into the task windows, when there is one.
    presenter: Option<Arc<dyn DisplayPresenter>>,
    /// Opens where the app window hosts connect.
    host_listener: Option<HostListenerFn>,
    /// Starts each app's window host.
    launcher: Option<Box<dyn HostLauncher>>,
    windows: Arc<Mutex<Vec<i32>>>,
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

        Compositor {
            config,
            renderer,
            tracker: Arc::new(StreamTracker::new()),
            vm_id_slot: VmIdSlot::default(),
            local_api_connector,
            bind_command: Some(bind_command),
            bind_process_pipe: Some(bind_process_pipe),
            logger: None,
            presenter: None,
            host_listener: Some(session_host_listener()),
            launcher: None,
            windows: Arc::new(Mutex::new(Vec::new())),
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
            local_api_connector,
            bind_command: Some(bind_command),
            bind_process_pipe: Some(bind_process_pipe),
            logger: None,
            presenter: None,
            host_listener: None,
            launcher: None,
            windows: Arc::new(Mutex::new(Vec::new())),
        }
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

        let bind_cmd = self.bind_command.take().expect("bind_command present");
        let bind_proc = self
            .bind_process_pipe
            .take()
            .expect("bind_process_pipe present");

        let mut cmd_listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind_cmd);
        let mut proc_listener = VmBoundListener::new(Arc::clone(&self.vm_id_slot), bind_proc);

        // 0. The app window hosts, the watch of the guest's tasks, and the requests to end
        // them.
        let task_windows = self.start_task_windows(&stop);

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

                while !monitor_stop.load(Ordering::Relaxed) {
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

        if let Some(logger) = &self.logger {
            logger.log("dsa-wm stopped cleanly");
        }
    }

    /// Starts the task windows: the manager, the listener for the app window hosts, the watch
    /// of the guest's tasks and the requests to the guest. A part that cannot start is
    /// logged; the rest of the window manager goes on.
    fn start_task_windows(&mut self, stop: &Arc<AtomicBool>) -> TaskWindowThreads {
        let logger = self.logger.clone();
        let log = |message: &str| note(logger.as_deref(), message);
        let router = Arc::new(FrameRouter::new());
        let mut luid = None;
        let gpu = match &self.presenter {
            None => None,
            Some(presenter) => {
                luid = presenter.adapter_luid();
                match Gpu::new(luid, false) {
                    Ok(gpu) => {
                        if luid.is_some() && !gpu.on_renderer_gpu {
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
                (Some(template), Some(apps_dir)) => Box::new(crate::app_exe::ProgramLauncher {
                    template,
                    apps_dir,
                    logger: self.logger.clone(),
                }),
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
        let setup = ManagerSetup {
            events: events_rx,
            requests: requests_tx,
            router,
            gpu,
            launcher,
            logger: self.logger.clone(),
            listing: Arc::clone(&self.windows),
            stop: Arc::clone(&manager_stop),
        };
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

        let watch_events = events_tx.clone();
        let watch_connector = (self.local_api_connector)();
        let watch_stop = Arc::clone(stop);
        let watch_logger = self.logger.clone();
        let watcher = thread::Builder::new()
            .name("dsa-wm-tasks".into())
            .spawn(move || {
                tasks::watch(
                    watch_connector,
                    |event| watch_events.send(event).is_ok(),
                    watch_stop,
                    watch_logger,
                )
            })
            .map_err(|e| log(&format!("could not start the watch of tasks: {e}")))
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
            threads: [manager, acceptor, watcher, requester]
                .into_iter()
                .flatten()
                .collect(),
        }
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
