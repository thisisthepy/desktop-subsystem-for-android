//! The app window host: the program that owns an Android app's task windows.
//!
//! Each app runs its own copy of this program, named and drawn as the app (see
//! [`crate::app_exe`]), so Task Manager and the taskbar show the app as itself. It connects to
//! the window manager in the same session, says which app it shows, opens the windows the
//! window manager asks for ([`window`]), and shows the frames the window manager tells it of.
//! It makes no Direct3D device and no DXGI factory, which would cost each app process tens of
//! MiB of driver memory: the window manager owns each window's swapchains and copies each
//! frame into them on its own device, when this process asks ([`SurfaceLink`]), and this
//! process shows them in its windows' DirectComposition visuals through the surface handles
//! the window manager places in it. Each frame is answered once it has been copied out, so the
//! renderer may write the task's texture again. It ends when the window manager closes the
//! connection after the app's last window closed.
//!
//! A window manager that crashes or restarts does not take the windows with it. They keep their
//! last frame and their taskbar entries, and this process dials the new window manager and says
//! which app it shows again (`window_reconnect`). The new window manager learns the app's tasks
//! from Android, shows the windows, and sends the next frames. The windows close when no window
//! manager comes back in time, or when Android has no task for them.

pub mod accessibility;
pub mod apk_install;
pub mod caption;
pub mod drag_source;
pub mod drop_target;
pub mod host_file;
pub mod ime;
pub mod keys;
pub mod launch;
pub mod live_icon;
pub mod messages;
pub mod overlay;
pub mod pointer;
pub mod screenshot;
pub mod splash;
pub mod system_menu;
pub mod text_store;
pub mod thumb_bar;
pub mod tsf;
pub mod window;
pub mod window_menu;

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::window_hosts::{
    connect_window_host, report_to_wire, to_host_from_wire, FromHost, ToHost,
};
use dsa_host_core::compositor::window_reconnect::{
    reconnect_window_host, ClosedSender, GaveUp, KeptWindows, Outage, REATTACH_SETTLE,
};
use dsa_protocol::window_host::types::{envelope::Body, Attach, Frame, FramePresented};
use dsa_protocol::window_host::{decode, encode_body};
use dsa_transport::{Connector, FrameSender, TransportError};

use self::window::{OpenWindow, Presenters, UiSetup};
use crate::compositor::log::{input_trace, input_trace_on, note, trace_forget, CompositorLogger};
use crate::compositor::present::SurfaceLink;
use crate::compositor::tasks::serve_window_input;
use dsa_host_core::compositor::window_tasks::WindowTasks;
use dsa_host_core::resize_trace::millis;

/// The build the window host names itself by to the window manager.
const BUILD: &str = "dsa-app-window";

/// The argument the window manager starts a window host with. Without it, the user started
/// it (from the Start menu, say), and the app is started in Android if it has no task yet.
pub const STARTED_BY_WINDOW_MANAGER: &str = "--started-by-window-manager";

/// The argument that names the app, for the installed window host, whose file name is not
/// a package's.
pub use dsa_host_core::compositor::window_hosts::PACKAGE_ARGUMENT;

/// The argument that names the pipe to dial instead of this session's window manager's, for
/// tests.
pub const PIPE_ARGUMENT: &str = "--pipe";

pub use dsa_host_core::compositor::window_hosts::argument;

/// Which app a window host shows: the one `--package` names, else the package its folder is
/// named after (`apps\<package>\<Label>.exe`), else the package its file is named after, as
/// a program of the earlier layout (`apps\<package>.exe`) is. `None` when none names a valid
/// package.
pub fn package_of(program: &std::path::Path, args: &[String]) -> Option<String> {
    use dsa_host_core::compositor::window_hosts::{is_valid_package, package_argument};
    if let Some(named) = package_argument(args) {
        return named;
    }
    let folder = program
        .parent()
        .and_then(|dir| dir.file_name())
        .and_then(|name| name.to_str());
    let stem = program.file_stem().and_then(|stem| stem.to_str());
    [folder, stem]
        .into_iter()
        .flatten()
        .find(|package| is_valid_package(package))
        .map(str::to_string)
}

/// Gives this process the app's own AppUserModelID, the one its Start menu shortcut carries,
/// so the taskbar groups the app's windows on one button with the app's icon, apart from
/// every other app, and pinning the button pins the shortcut.
pub fn use_app_user_model_id(package: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    let id = crate::start_menu::app_user_model_id(package);
    let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: a NUL-terminated string, read during the call.
    let result = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if result < 0 {
        return Err(format!(
            "could not take the app ID {id} (error 0x{:08x}); the taskbar may group the app \
             with the window manager",
            result as u32
        ));
    }
    Ok(())
}

/// The AppUserModelID this process took, if it took one.
pub fn this_process_app_user_model_id() -> Option<String> {
    use windows::Win32::System::Com::CoTaskMemFree;
    use windows::Win32::UI::Shell::GetCurrentProcessExplicitAppUserModelID;
    // SAFETY: the shell allocates the string, which is read and then freed once.
    unsafe {
        let id = GetCurrentProcessExplicitAppUserModelID().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const std::ffi::c_void));
        text
    }
}

/// How long connecting and the handshake may take.
const CONNECT: Duration = Duration::from_secs(5);

/// How long the connection thread waits for a message before it looks at the windows again.
const RECV_TICK: Duration = Duration::from_millis(200);

/// How long a frame for a window that is being opened waits for the window to be there. The
/// window manager sends the frames a task drew before this window host connected (its splash
/// screen) right after the window, and the window opens on another thread.
const WINDOW_OPENING: Duration = Duration::from_secs(1);

/// What a window host is given to run.
pub struct HostSetup {
    /// The Android package of the app.
    pub package: String,
    /// Whether the user started it, rather than the window manager.
    pub launch: bool,
    /// With `launch`, the shortcut of the app to start rather than the app itself: a jump list
    /// task or a desktop shortcut started the program with `--shortcut <id>`.
    pub shortcut: Option<String>,
    /// Dials the window manager.
    pub connector: Box<dyn Connector>,
    /// Dials the service's local API, which the windows' pointer and key input go to directly, so
    /// it never waits on the window manager.
    pub input: Box<dyn Connector>,
    /// The process the window manager places the windows' surface handles in: this one.
    pub process_id: u32,
    pub logger: Option<Arc<CompositorLogger>>,
    /// Where the windows that are open are listed, for tests.
    pub listing: Arc<Mutex<Vec<OpenWindow>>>,
    /// Dials the service's local API to watch a task's accessibility tree while a UI Automation
    /// client reads its window (see [`accessibility`]). `None` answers clients with an empty
    /// tree.
    pub accessibility: Option<accessibility::ApiConnect>,
}

/// Runs the window host until the window manager closes the connection. An error says why
/// it could not start.
pub fn run(mut setup: HostSetup) -> Result<(), String> {
    let log = setup.logger.clone();
    let mut connection = setup
        .connector
        .connect(CONNECT)
        .map_err(|e| format!("could not reach the DSA Window Manager: {e}"))?;
    let luid = connect_window_host(
        &mut *connection,
        BUILD,
        Attach {
            package_name: setup.package.clone(),
            process_id: setup.process_id,
            launch: setup.launch,
            shortcut_id: setup.shortcut.clone().unwrap_or_default(),
        },
        CONNECT,
    )?;
    let sender: Arc<Mutex<Box<dyn FrameSender>>> =
        Arc::new(Mutex::new(connection.try_clone_sender().ok_or(
            "the connection to the window manager cannot be shared",
        )?));
    // The window manager's GPU is the only one this process draws with, through it.
    let _ = luid;
    let surfaces = SurfaceLink::new(Arc::clone(&sender));
    let report_sender = Arc::clone(&sender);
    // The windows the user asked to close while there was no window manager to tell: they are
    // told once there is one again.
    let unsent_closes: Arc<Mutex<Vec<i32>>> = Arc::default();
    let report_unsent = Arc::clone(&unsent_closes);
    let report: window::Report = Arc::new(move |report: FromHost| {
        let frame = encode_body(report_to_wire(&report));
        let sent = report_sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(&frame);
        if let (Err(_), FromHost::Closed { task_id }) = (sent, &report) {
            report_unsent
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(*task_id);
        }
    });
    // The task each window shows, which what the windows send Android itself goes to.
    let tasks = WindowTasks::new();
    let (input_tx, input_rx) = std::sync::mpsc::channel();
    let input_connector = std::mem::replace(&mut setup.input, Box::new(NoConnector));
    let input_logger = log.clone();
    let input_tasks = tasks.clone();
    let input = std::thread::Builder::new()
        .name("dsa-app-window-input".into())
        .spawn(move || serve_window_input(input_connector, input_rx, input_tasks, input_logger))
        .map_err(|e| format!("could not start the window input: {e}"))?;
    let presenters: Presenters = Arc::default();
    let ui = window::start(UiSetup {
        report,
        input: input_tx,
        surfaces: Arc::clone(&surfaces),
        presenters: Arc::clone(&presenters),
        logger: log.clone(),
        listing: Arc::clone(&setup.listing),
        accessibility: setup.accessibility.clone(),
        tasks: tasks.clone(),
        drops: setup.accessibility.clone().map(|api| {
            let log = log.clone();
            drop_target::handler(
                setup.package.clone(),
                api,
                Arc::new(move |line: &str| note(log.as_deref(), line)),
            )
        }),
        drags: setup.accessibility.clone().map(|api| {
            let log = log.clone();
            drag_source::starter(
                api,
                tasks.clone(),
                Arc::new(move |line: &str| note(log.as_deref(), line)),
            )
        }),
        package: setup.package.clone(),
        // Beside the apps' window programs, in the user's own DSA folder.
        overlay_places: crate::paths::UserRoot::current().map(|root| root.apps()),
    })?;

    // Frames are shown on a thread of their own, which the renderer waits for: showing one
    // waits for the window manager's answer, which this thread reads.
    let (frames_tx, frames_rx) = std::sync::mpsc::channel::<(Frame, bool, u64)>();
    // Which window manager a frame came from: frames of one that is gone are neither shown nor
    // answered, since the new window manager numbers its frames from the start.
    let epoch = Arc::new(AtomicU64::new(0));
    let present_presenters = Arc::clone(&presenters);
    let present_sender = Arc::clone(&sender);
    let present_log = log.clone();
    let present_epoch = Arc::clone(&epoch);
    let presenting = std::thread::Builder::new()
        .name("dsa-app-window-present".into())
        .spawn(move || {
            crate::scheduling::raise_this_thread();
            show_frames(
                frames_rx,
                present_presenters,
                present_sender,
                present_epoch,
                present_log,
            )
        })
        .map_err(|e| format!("could not start the frames' thread: {e}"))?;
    // The windows asked for whose presenter may not be there yet.
    let mut opening: HashSet<i32> = HashSet::new();
    // The windows that are open, and which of them a new window manager still wants.
    let mut kept = KeptWindows::new();
    // What the window host says to a new window manager: the same app, never started by the
    // user.
    let reattach = Attach {
        package_name: setup.package.clone(),
        process_id: setup.process_id,
        launch: false,
        shortcut_id: String::new(),
    };
    // The time without a window manager, from the loss to a window manager that shows the
    // windows again.
    let mut outage: Option<Outage> = None;
    let mut reattached = false;
    'connected: loop {
        // One connection to a window manager, until it ends.
        loop {
            for task_id in kept.unwanted(Instant::now(), REATTACH_SETTLE) {
                note(
                    log.as_deref(),
                    &format!(
                        "the new window manager has no task {task_id} for its window; the \
                         window closes"
                    ),
                );
                let close = ToHost::Close { task_id };
                tasks.follow(&close);
                opening.remove(&task_id);
                trace_forget(task_id);
                if !ui.send(close) {
                    break 'connected;
                }
                if kept.is_empty() {
                    // The app has no window left, so its program ends, as when its last
                    // window closes.
                    break 'connected;
                }
            }
            let frame = match connection.recv(Some(RECV_TICK)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => continue,
                Err(TransportError::Closed) => {
                    // The window manager ends the connection when the app's last window
                    // closed, and when it stops or crashes.
                    note(
                        log.as_deref(),
                        &format!(
                            "the window manager closed the connection; {} window(s) open",
                            kept.tasks().len()
                        ),
                    );
                    break;
                }
                Err(error) => {
                    note(
                        log.as_deref(),
                        &format!("the connection to the window manager failed: {error}"),
                    );
                    break;
                }
            };
            let body = match decode(&frame) {
                Ok(envelope) => envelope.body,
                Err(error) => {
                    note(
                        log.as_deref(),
                        &format!("dropped a message from the window manager: {error}"),
                    );
                    continue;
                }
            };
            let Some(body) = body else {
                continue;
            };
            if let Some(command) = to_host_from_wire(&body) {
                // Before the windows act on it, so what they send next goes where it now
                // belongs.
                tasks.follow(&command);
                match &command {
                    ToHost::Close { task_id } => {
                        opening.remove(task_id);
                        kept.closed(*task_id);
                        trace_forget(*task_id);
                    }
                    ToHost::Show { task_id, .. } => {
                        opening.insert(*task_id);
                        kept.shown(*task_id);
                        // A window manager that shows the windows again has taken them over.
                        outage = None;
                    }
                    _ => {}
                }
                if !ui.send(command) {
                    note(
                        log.as_deref(),
                        "the windows' thread has ended, so the program ends with its windows",
                    );
                    break 'connected;
                }
                continue;
            }
            match body {
                Body::SurfaceAnswer(answer) => surfaces.answered(answer),
                Body::Frame(frame) => {
                    let opening = opening.remove(&frame.task_id);
                    let from = epoch.load(Ordering::SeqCst);
                    if frames_tx.send((frame, opening, from)).is_err() {
                        break 'connected;
                    }
                }
                _ => {}
            }
        }
        // The connection ended. With no window open there is nothing to keep, and the program
        // ends, as when the app's last window closes.
        if kept.is_empty() {
            break;
        }
        // The window manager is gone with windows open: they keep their last frame and their
        // taskbar entries, and this process dials the next window manager.
        let now = Instant::now();
        let mut lost = outage.take().unwrap_or_else(|| Outage::new(now));
        if reattached && !kept.any_named() {
            // Taken on and turned away, so it does not know the app's tasks (yet).
            lost.refused(now);
        }
        // Nothing waits on the window manager that went; anything sent to it fails at once.
        surfaces.close();
        *sender.lock().unwrap_or_else(|e| e.into_inner()) = Box::new(ClosedSender);
        epoch.fetch_add(1, Ordering::SeqCst);
        note(
            log.as_deref(),
            &format!(
                "no window manager: {} window(s) stay open showing their last frame, and this \
                 program dials a new one",
                kept.tasks().len()
            ),
        );
        let redialled = reconnect_window_host(
            &mut *setup.connector,
            BUILD,
            &reattach,
            &lost,
            &|| false,
            &std::thread::sleep,
        );
        let (next, _luid) = match redialled {
            Ok(next) => next,
            Err(GaveUp::TimedOut(why)) => {
                note(log.as_deref(), &format!("{why}; the windows close"));
                break;
            }
            Err(GaveUp::Stopped) => break,
        };
        let Some(next_sender) = next.try_clone_sender() else {
            note(
                log.as_deref(),
                "the new window manager's connection cannot be shared; the windows close",
            );
            break;
        };
        connection = next;
        *sender.lock().unwrap_or_else(|e| e.into_inner()) = next_sender;
        surfaces.reopen();
        outage = Some(lost);
        reattached = true;
        opening.clear();
        kept.reattached(Instant::now());
        for presenter in presenters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            presenter.window_manager_replaced();
        }
        // What the user closed meanwhile is told to the window manager that can end the task.
        let closed: Vec<i32> =
            std::mem::take(&mut *unsent_closes.lock().unwrap_or_else(|e| e.into_inner()));
        for task_id in closed.into_iter().filter(|id| kept.tasks().contains(id)) {
            let told = encode_body(report_to_wire(&FromHost::Closed { task_id }));
            let _ = sender.lock().unwrap_or_else(|e| e.into_inner()).send(&told);
        }
        note(
            log.as_deref(),
            "a new window manager took this program on; it names the windows that stay",
        );
    }
    // Nothing waits on the window manager any more.
    surfaces.close();
    drop(frames_tx);
    let _ = presenting.join();
    // The windows close first; the pointer input ends with them.
    ui.stop();
    let _ = input.join();
    Ok(())
}

/// Shows each frame `frames` brings in its window, and tells the window manager once it has
/// been copied out, until the connection ends.
fn show_frames(
    frames: std::sync::mpsc::Receiver<(Frame, bool, u64)>,
    presenters: Presenters,
    sender: Arc<Mutex<Box<dyn FrameSender>>>,
    epoch: Arc<AtomicU64>,
    log: Option<Arc<CompositorLogger>>,
) {
    for (frame, opening, from) in frames {
        // A frame of a window manager that is gone: its texture is gone with it, and the new
        // window manager sends the window's next frame itself.
        if epoch.load(Ordering::SeqCst) != from {
            continue;
        }
        let presenter = if opening {
            wait_for_presenter(&presenters, frame.task_id, WINDOW_OPENING)
        } else {
            presenters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&frame.task_id)
                .cloned()
        };
        if let Some(presenter) = presenter {
            let started = std::time::Instant::now();
            // A failure (a GPU reset among them) is logged and the window stays.
            if let Err(why) = presenter.present(frame.texture_id, (frame.width, frame.height)) {
                note(log.as_deref(), &why);
            }
            if input_trace_on() {
                input_trace(
                    "app.present",
                    frame.task_id,
                    &format!(
                        "sequence={} took={}",
                        frame.sequence,
                        millis(started.elapsed())
                    ),
                );
            }
        }
        if epoch.load(Ordering::SeqCst) != from {
            continue;
        }
        // Answered even when nothing was shown, so the renderer never waits on it.
        let presented = encode_body(Body::FramePresented(FramePresented {
            task_id: frame.task_id,
            sequence: frame.sequence,
        }));
        let sent = sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(&presented);
        // A connection that ended is the connection thread's to handle: it dials a new window
        // manager, and the frames that come after are answered to that one.
        if let Err(why) = sent {
            note(
                log.as_deref(),
                &format!("could not tell the window manager a frame was shown: {why}"),
            );
        }
    }
}

/// The presenter of task `task_id`'s window, waiting at most `timeout` for the window, which
/// opens on the UI thread, to make it.
fn wait_for_presenter(
    presenters: &Presenters,
    task_id: i32,
    timeout: Duration,
) -> Option<Arc<crate::compositor::present::WindowPresenter>> {
    let deadline = Instant::now() + timeout;
    loop {
        let presenter = presenters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&task_id)
            .cloned();
        if presenter.is_some() || Instant::now() >= deadline {
            return presenter;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Stands in for the input connector once it has been handed to its thread.
struct NoConnector;

impl Connector for NoConnector {
    fn connect(
        &mut self,
        _: Duration,
    ) -> Result<Box<dyn dsa_transport::Connection>, TransportError> {
        Err(TransportError::Closed)
    }
}
