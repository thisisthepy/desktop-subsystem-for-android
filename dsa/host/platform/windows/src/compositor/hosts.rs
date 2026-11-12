//! The window manager's side of the app window hosts.
//!
//! The window manager owns no window. One thread, the manager, keeps the guest's tasks
//! (`TaskWindows`) and the window hosts (`WindowHosts`) and carries out what they decide:
//! it starts each app's window program, sends each window host its windows, and asks the
//! guest to end a task whose window the user closed. A listener thread takes the window
//! hosts' connections, and a thread per connection reads what each says.
//!
//! Frames go straight from the renderer to the window hosts: each task with a window has a
//! [`RemoteWindow`], the frame target the renderer copies into. It makes a shared texture on
//! the renderer's GPU, places the texture's NT handle in the window host's process, and for
//! each frame tells the window host and waits until it has copied the frame out. No frame
//! passes through the CPU. The texture has the size of the guest's buffers, the largest a
//! frame of the task can be, so resizing the window does not remake it: each frame fills its
//! top-left region and says how large that region is.
//!
//! A task's [`RemoteWindow`] is there from the moment the task has a window, before its window
//! host has connected. The frames drawn meanwhile land in its texture, and the newest is shown
//! as soon as the window host connects: an app's splash screen is drawn once, right after
//! launch, and would otherwise be lost, leaving the window empty until the app draws.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::background::MediaCommand;
use dsa_host_core::compositor::early_frames::EarlyFrames;
use dsa_host_core::compositor::frame_buffers::texture_fits;
use dsa_host_core::compositor::frame_stall::{FrameStall, Stall};
use dsa_host_core::compositor::gpu_reset::{GpuResetWindows, Report};
use dsa_host_core::compositor::program_exit::{describe_exit, ended_by_crash, CrashRestarts};
use dsa_host_core::compositor::shown_windows::ShownWindows;
use dsa_host_core::compositor::task_windows::{TaskWindows, WindowChange};
use dsa_host_core::compositor::window_hosts::{
    accept_window_host, answer_attached, report_from_wire, to_wire, FromHost, HostAction, HostId,
    MediaButtons, MenuWork, ToHost, WindowHosts,
};
use dsa_host_core::memory::IdleTrim;
use dsa_host_core::tasks::{AppInfo, TaskEvent, TaskRequest};
use dsa_protocol::window_host::types::{envelope::Body, Frame, FrameTexture};
use dsa_protocol::window_host::{decode, encode_body};
use dsa_transport::{Connection, FrameSender, Listener, TransportError};
use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, WaitForSingleObject, PROCESS_DUP_HANDLE,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
};

use super::frames::{FrameRouter, FrameTarget};
use super::log::{
    input_trace, input_trace_on, note, trace_detailed, trace_forget, trace_stage, trace_summary,
    CompositorLogger,
};
use super::present::{Gpu, SharedTexture};
use dsa_host_core::resize_trace::millis;

/// The build the window manager names itself by to the window hosts.
const BUILD: &str = "dsa-wm";

/// How long a window host may take over its handshake.
const HANDSHAKE: Duration = Duration::from_secs(5);

/// How long the renderer waits for a window host to copy a frame out of its texture.
const PRESENT_WAIT: Duration = Duration::from_millis(500);

/// How often the manager looks for window hosts that are late.
const TICK: Duration = Duration::from_millis(500);

/// The name of the pipe the window hosts of session `session` dial.
pub fn pipe_name(session: u32) -> String {
    format!("dsa-wm-{session}")
}

/// The arguments that open DSA's settings on the Apps page with `package`'s row in view.
pub fn app_page_arguments(package: &str) -> [String; 4] {
    [
        "--page".into(),
        "apps".into(),
        "--app".into(),
        package.into(),
    ]
}

/// Opens DSA's settings (the settings app installed beside this window manager) on the Apps
/// page at `package`, as the system menu's App info does. The settings app runs on its own;
/// nothing waits for it.
fn open_app_page(package: &str) -> Result<(), String> {
    let program =
        crate::start_menu::default_uninstaller().ok_or("the settings app's place is not known")?;
    std::process::Command::new(&program)
        .args(app_page_arguments(package))
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {}: {e}", program.display()))
}

/// What the other threads tell the manager.
pub enum ManagerEvent {
    /// A new watch began; these are the guest's tasks now.
    Sync(Vec<TaskEvent>),
    /// A change to the guest's tasks. Boxed, as an app's event carries its icons.
    Task(Box<TaskEvent>),
    /// The service cannot be reached, so the guest's tasks are unknown.
    Lost,
    /// The request to end a task whose window the user closed did not reach the guest.
    CloseFailed(i32),
    /// A window host connected and said which app it shows.
    Attached {
        link: Arc<HostLink>,
        package: String,
        launch: bool,
        /// With `launch`, the shortcut of the app to start rather than the app; empty for none.
        shortcut: String,
    },
    /// A window host reported what the user did to a window.
    Report { host: HostId, report: FromHost },
    /// A window host's connection ended. `exit` is its program's exit code when the program
    /// ended with it.
    Gone { host: HostId, exit: Option<u32> },
    /// The user's window settings for each app, as the service has them now.
    AppWindows(Vec<dsa_host_core::app_windows::AppWindowSettings>),
    /// The app's media session changed, or ended (`None`): the media buttons in the taskbar
    /// preview of its windows follow it.
    Media {
        package: String,
        media: Option<MediaButtons>,
    },
}

/// How many task windows are the active window now: the game controllers reach Android only
/// while one is, as they reach any program only while it is in front.
static FOCUSED_WINDOWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Whether a task window is the active window.
pub fn any_window_focused() -> bool {
    FOCUSED_WINDOWS.load(Ordering::Relaxed) > 0
}

/// Starts each app's window program.
pub trait HostLauncher: Send {
    /// Starts the window host of `package`, named and drawn as `app` when it is known. The
    /// window host then connects by itself.
    fn start(&mut self, package: &str, app: Option<&AppInfo>) -> Result<(), String>;

    /// The window host of `package` ended, and `app` is the app's name and icon as the guest
    /// last gave them. A program that could not take them while it ran takes them now.
    fn ended(&mut self, _package: &str, _app: Option<&AppInfo>) {}

    /// Called every tick of the manager, for work that waits.
    fn tick(&mut self) {}
}

/// The window manager's end of one window host's connection.
pub struct HostLink {
    id: HostId,
    sender: Mutex<Option<Box<dyn FrameSender>>>,
    /// The window host's process, which textures' handles are placed in.
    process: Option<ProcessHandle>,
    /// The last frame each task's window host has copied out, by task.
    presented: Mutex<HashMap<i32, u64>>,
    progress: Condvar,
    closed: AtomicBool,
    next_texture: AtomicU64,
}

struct ProcessHandle(HANDLE);

// SAFETY: a kernel handle, usable from any thread.
unsafe impl Send for ProcessHandle {}
unsafe impl Sync for ProcessHandle {}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: an owned handle, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

impl HostLink {
    fn new(id: HostId, sender: Box<dyn FrameSender>, process_id: u32) -> HostLink {
        // SAFETY: opened only to place handles in it, closed by ProcessHandle.
        let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, 0, process_id) };
        HostLink {
            id,
            sender: Mutex::new(Some(sender)),
            process: (!process.is_null()).then_some(ProcessHandle(process)),
            presented: Mutex::new(HashMap::new()),
            progress: Condvar::new(),
            closed: AtomicBool::new(false),
            next_texture: AtomicU64::new(1),
        }
    }

    pub fn id(&self) -> HostId {
        self.id
    }

    /// Sends `body` to the window host. False when the connection is gone.
    fn send(&self, body: Body) -> bool {
        let frame = encode_body(body);
        let mut sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        match sender.as_mut() {
            Some(sender) => sender.send(&frame).is_ok(),
            None => false,
        }
    }

    /// Ends the connection; the window host then exits.
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
        self.progress.notify_all();
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Places a copy of `handle` in the window host's process and returns its value there.
    fn place(&self, handle: isize) -> Option<u64> {
        let process = self.process.as_ref()?;
        let mut placed: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a handle of this process into a process opened for that.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle as HANDLE,
                process.0,
                &mut placed,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        (ok != 0).then_some(placed as usize as u64)
    }

    fn presented(&self, task_id: i32, sequence: u64) {
        self.presented
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(task_id, sequence);
        self.progress.notify_all();
    }

    /// Waits until the window host has copied out frame `sequence` of `task_id`.
    fn wait_presented(&self, task_id: i32, sequence: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut presented = self.presented.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if presented
                .get(&task_id)
                .is_some_and(|done| *done >= sequence)
            {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.is_closed() {
                return false;
            }
            presented = self
                .progress
                .wait_timeout(presented, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// A texture a window host shows a task's frames from.
pub struct HostTexture {
    shared: SharedTexture,
    id: u64,
}

struct RemoteState {
    /// The window host's connection; `None` until it connects.
    link: Option<Arc<HostLink>>,
    texture: Option<Arc<HostTexture>>,
    /// What has reached the window host of the texture and the frames in it.
    early: EarlyFrames,
    sequence: u64,
    closed: bool,
}

/// A task window in a window host, as the renderer's frame target.
///
/// It exists from the moment the task has a window, before the window's host has connected,
/// so the frames the task draws meanwhile (its splash screen) are copied into its texture
/// rather than dropped, and the newest is shown as soon as the window is there.
pub struct RemoteWindow {
    task_id: i32,
    /// The guest task that draws into the window: its own, or the one that took it over
    /// after a GPU reset.
    drawing: std::sync::atomic::AtomicI32,
    gpu: Arc<Gpu>,
    state: Mutex<RemoteState>,
    logger: Option<Arc<CompositorLogger>>,
}

impl RemoteWindow {
    /// The frame target of a task whose window host is connected on `link`.
    pub fn new(
        task_id: i32,
        link: Arc<HostLink>,
        gpu: Arc<Gpu>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> Arc<RemoteWindow> {
        Self::make(task_id, Some(link), EarlyFrames::linked(), gpu, logger)
    }

    /// The frame target of a task whose window host has not connected yet.
    pub fn waiting(
        task_id: i32,
        gpu: Arc<Gpu>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> Arc<RemoteWindow> {
        Self::make(task_id, None, EarlyFrames::waiting(), gpu, logger)
    }

    fn make(
        task_id: i32,
        link: Option<Arc<HostLink>>,
        early: EarlyFrames,
        gpu: Arc<Gpu>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> Arc<RemoteWindow> {
        Arc::new(RemoteWindow {
            task_id,
            drawing: std::sync::atomic::AtomicI32::new(task_id),
            gpu,
            state: Mutex::new(RemoteState {
                link,
                texture: None,
                early,
                sequence: 0,
                closed: false,
            }),
            logger,
        })
    }

    /// Whether the window host has not connected yet.
    fn is_waiting(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.link.is_none() && !state.closed
    }

    /// Whether the window host on `host` shows this window.
    fn is_on(&self, host: HostId) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.link.as_ref().is_some_and(|link| link.id() == host)
    }

    /// The window host connected on `link` and was sent the window. It is told of the texture
    /// the task's frames are in and shown the newest frame the task drew before it connected.
    /// The window is sent first, so the frame has a window to go to.
    pub fn attach(&self, link: Arc<HostLink>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.early.is_linked() {
            return;
        }
        let texture = state.texture.clone();
        let handover = state.early.link(texture.is_some());
        state.link = Some(Arc::clone(&link));
        let Some(texture) = texture else {
            return;
        };
        if handover.texture && !announce(&link, self.task_id, &texture) {
            // The window host cannot open it: the next frame makes a texture it can.
            state.texture = None;
            note(
                self.logger.as_deref(),
                &format!(
                    "could not hand task {}'s first frames to its window: {}",
                    self.task_id,
                    std::io::Error::last_os_error()
                ),
            );
            return;
        }
        if let Some((width, height)) = handover.frame {
            state.sequence += 1;
            let sequence = state.sequence;
            drop(state);
            // Not waited for: the renderer waits for the window host only before it writes
            // the texture again, and its next frame does.
            let sent = link.send(Body::Frame(Frame {
                task_id: self.task_id,
                texture_id: texture.id,
                sequence,
                width,
                height,
            }));
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "wm.early_frame",
                (width, height),
                &format!("texture={} sequence={sequence} sent={sent}", texture.id),
            );
        }
    }

    /// Task `task_id` draws into the window from now on.
    fn drawn_by(&self, task_id: i32) {
        self.drawing.store(task_id, Ordering::Relaxed);
    }

    /// The window is gone: nothing more is sent for it.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.texture = None;
        trace_forget(self.task_id);
    }
}

/// Places `texture`'s handle in the window host's process and tells it of the texture.
/// Returns whether that went.
fn announce(link: &HostLink, task_id: i32, texture: &HostTexture) -> bool {
    let Some(handle) = link.place(texture.shared.handle()) else {
        return false;
    };
    let (width, height) = texture.shared.size();
    link.send(Body::FrameTexture(FrameTexture {
        task_id,
        texture_id: texture.id,
        handle,
        width,
        height,
    }))
}

/// The textures of task windows whose window host has not connected yet are numbered from
/// here, apart from those a connected window host numbers from 1.
static EARLY_TEXTURE: AtomicU64 = AtomicU64::new(1 << 48);

impl FrameTarget for RemoteWindow {
    type Texture = HostTexture;

    fn texture(&self, width: u32, height: u32) -> Option<Arc<HostTexture>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.link.as_ref().is_some_and(|link| link.is_closed()) {
            return None;
        }
        // A GPU reset removes the device the textures are made on, and every texture with it:
        // the device is made again here, before the renderer copies into a lost texture, and
        // the window gets a new texture on the new device.
        // Until one can be made the frame is dropped; the window manager's tick says why.
        if self.gpu.recover().is_err() {
            return None;
        }
        let generation = self.gpu.generation();
        if let Some(texture) = &state.texture {
            if texture.shared.generation() == generation
                && texture_fits(texture.shared.size(), (width, height))
            {
                return Some(Arc::clone(texture));
            }
        }
        state.texture = None;
        // The renderer has the first frame for this window, or the guest's buffers changed
        // size, which they do only when its display does.
        let started = Instant::now();
        let shared = match self.gpu.shared_texture(width, height) {
            Ok(shared) => shared,
            Err(why) => {
                note(self.logger.as_deref(), &why);
                return None;
            }
        };
        let created = Instant::now();
        let link = state.link.clone();
        let id = match &link {
            Some(link) => link.next_texture.fetch_add(1, Ordering::Relaxed),
            None => EARLY_TEXTURE.fetch_add(1, Ordering::Relaxed),
        };
        let texture = Arc::new(HostTexture { shared, id });
        let now = state.early.texture_made();
        let announced = match (&link, now) {
            (Some(link), true) => {
                let announced = announce(link, self.task_id, &texture);
                if !announced {
                    note(
                        self.logger.as_deref(),
                        &format!(
                            "could not hand task {}'s frames to its window: {}",
                            self.task_id,
                            std::io::Error::last_os_error()
                        ),
                    );
                }
                announced
            }
            // Kept for the window host, which is told of it when it connects.
            _ => true,
        };
        let done = Instant::now();
        trace_stage(
            self.logger.as_deref(),
            self.task_id,
            "wm.texture",
            (width, height),
            &format!(
                "texture={id} create={} announce={} linked={} announced={announced}",
                millis(created.saturating_duration_since(started)),
                millis(done.saturating_duration_since(created)),
                link.is_some()
            ),
        );
        trace_summary(
            self.logger.as_deref(),
            self.task_id,
            "wm.summary",
            (width, height),
            &format!(
                "texture={id} create={} announce={}",
                millis(created.saturating_duration_since(started)),
                millis(done.saturating_duration_since(created))
            ),
        );
        if !announced {
            return None;
        }
        state.texture = Some(Arc::clone(&texture));
        Some(texture)
    }

    fn handle(texture: &HostTexture) -> isize {
        texture.shared.handle()
    }

    fn present(&self, texture: &HostTexture, (width, height): (u32, u32)) {
        let (link, sequence) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return;
            }
            if !state.early.frame((width, height)) {
                // No window host yet: the frame stays in the texture until one connects.
                return;
            }
            let Some(link) = state.link.clone() else {
                return;
            };
            state.sequence += 1;
            (link, state.sequence)
        };
        // The app has drawn this size: the next size of a drag may go to it.
        super::tasks::frame_drawn(self.drawing.load(Ordering::Relaxed), (width, height));
        let started = Instant::now();
        let sent = link.send(Body::Frame(Frame {
            task_id: self.task_id,
            texture_id: texture.id,
            sequence,
            width,
            height,
        }));
        // The renderer writes the texture again once this returns, so the window host must
        // have copied the frame out by then.
        let shown = sent && link.wait_presented(self.task_id, sequence, PRESENT_WAIT);
        if input_trace_on() {
            input_trace(
                "wm.frame",
                self.task_id,
                &format!(
                    "sequence={sequence} round_trip={}",
                    millis(started.elapsed())
                ),
            );
        }
        if trace_detailed() {
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "wm.frame",
                (width, height),
                &format!(
                    "texture={} sequence={sequence} shown={shown} round_trip={}",
                    texture.id,
                    millis(started.elapsed())
                ),
            );
        }
        if sent && !shown {
            note(
                self.logger.as_deref(),
                &format!(
                    "task {}'s window took longer than {} ms to show a frame",
                    self.task_id,
                    PRESENT_WAIT.as_millis()
                ),
            );
        }
    }
}

/// Takes the window hosts' connections until `stop` is set, and reads each one on a thread
/// of its own. `adapter_luid` is the GPU the frames are on, which each window host is told.
pub fn accept_hosts(
    mut listener: Box<dyn Listener>,
    events: Sender<ManagerEvent>,
    adapter_luid: Option<[u8; 8]>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut next = 1u64;
    while !stop.load(Ordering::Relaxed) {
        let connection = match listener.accept(Duration::from_millis(200)) {
            Ok(connection) => connection,
            Err(TransportError::Timeout) => continue,
            Err(error) => {
                note(
                    logger.as_deref(),
                    &format!("could not take an app window's connection: {error}"),
                );
                thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        let id = HostId(next);
        next += 1;
        let events = events.clone();
        let stop = Arc::clone(&stop);
        let logger = logger.clone();
        let _ = thread::Builder::new()
            .name("dsa-wm-host".into())
            .spawn(move || serve_host(connection, id, events, adapter_luid, stop, logger));
    }
}

/// How long the window manager waits, once a window program's connection broke, for the
/// program to end, so its exit code says whether it crashed.
const EXIT_WAIT: Duration = Duration::from_secs(3);

/// `STILL_ACTIVE`: what `GetExitCodeProcess` answers while the process runs.
const STILL_ACTIVE: u32 = 259;

/// An app window program's process, held from the moment it attached, so its exit code can be
/// read once its connection ends, though its process ID may name another process by then.
struct ProgramProcess(HANDLE);

impl ProgramProcess {
    fn open(process_id: u32) -> Option<ProgramProcess> {
        // SAFETY: a plain call; the handle it returns is closed once, in drop.
        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                0,
                process_id,
            )
        };
        (!handle.is_null()).then_some(ProgramProcess(handle))
    }

    /// The program's exit code, waiting `wait` at most for it to end; `None` while it runs.
    fn exit_code(&self, wait: Duration) -> Option<u32> {
        let mut code = STILL_ACTIVE;
        // SAFETY: plain calls on the handle this holds; `code` is a local.
        let read = unsafe {
            WaitForSingleObject(self.0, wait.as_millis().min(u32::MAX as u128) as u32);
            GetExitCodeProcess(self.0, &mut code)
        };
        (read != 0 && code != STILL_ACTIVE).then_some(code)
    }
}

impl Drop for ProgramProcess {
    fn drop(&mut self) {
        // SAFETY: the handle OpenProcess gave, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

fn serve_host(
    mut connection: Box<dyn Connection>,
    id: HostId,
    events: Sender<ManagerEvent>,
    adapter_luid: Option<[u8; 8]>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let attach = match accept_window_host(&mut *connection, BUILD, HANDSHAKE) {
        Ok(attach) => attach,
        Err(why) => {
            note(
                logger.as_deref(),
                &format!("refused an app window's connection: {why}"),
            );
            return;
        }
    };
    if let Err(why) = answer_attached(&mut *connection, adapter_luid) {
        note(logger.as_deref(), &why);
        return;
    }
    let Some(sender) = connection.try_clone_sender() else {
        note(
            logger.as_deref(),
            "an app window's connection cannot be shared",
        );
        return;
    };
    let link = Arc::new(HostLink::new(id, sender, attach.process_id));
    let package = attach.package_name.clone();
    // Held from now, so the exit code read when the connection ends is this program's.
    let process = ProgramProcess::open(attach.process_id);
    let mut exit = None;
    let attached = ManagerEvent::Attached {
        link: Arc::clone(&link),
        package: attach.package_name,
        launch: attach.launch,
        shortcut: attach.shortcut_id,
    };
    if events.send(attached).is_err() {
        return;
    }
    while !stop.load(Ordering::Relaxed) && !link.is_closed() {
        let frame = match connection.recv(Some(Duration::from_millis(200))) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(error) => {
                // Its windows close with it and Android ends their tasks, or, when it crashed,
                // it starts again with its windows; the log says which it was.
                exit = process
                    .as_ref()
                    .and_then(|process| process.exit_code(EXIT_WAIT));
                let how = exit.map_or_else(
                    || "it still runs, or its exit code cannot be read".to_string(),
                    describe_exit,
                );
                note(
                    logger.as_deref(),
                    &format!(
                        "{package}'s window program (process {}) is gone: {error} ({how})",
                        attach.process_id
                    ),
                );
                break;
            }
        };
        let Ok(envelope) = decode(&frame) else {
            note(
                logger.as_deref(),
                "dropped a malformed message from an app window",
            );
            continue;
        };
        let Some(body) = envelope.body else {
            continue;
        };
        if let Body::FramePresented(presented) = &body {
            link.presented(presented.task_id, presented.sequence);
        } else if let Some(report) = report_from_wire(&body) {
            if events
                .send(ManagerEvent::Report { host: id, report })
                .is_err()
            {
                break;
            }
        }
    }
    link.close();
    connection.close();
    let _ = events.send(ManagerEvent::Gone { host: id, exit });
}

/// What is done when the guest's compositor did not recover from a GPU reset, with the apps
/// that had windows.
pub type Recovery = Box<dyn Fn(&Stall, Vec<String>) + Send>;

/// What the manager thread is given.
pub struct ManagerSetup {
    pub events: Receiver<ManagerEvent>,
    /// Where the requests to the guest go.
    pub requests: Sender<TaskRequest>,
    pub router: Arc<FrameRouter<RemoteWindow>>,
    /// The GPU the frames are on, or `None` when frames cannot be shown (the windows still
    /// open and close with their tasks).
    pub gpu: Option<Arc<Gpu>>,
    pub launcher: Box<dyn HostLauncher>,
    pub logger: Option<Arc<CompositorLogger>>,
    /// Where the manager lists the tasks that have a window, in task ID order.
    pub listing: Arc<Mutex<Vec<i32>>>,
    /// Set when the window manager stops.
    pub stop: Arc<AtomicBool>,
    /// Told whether any window is shown, for the trim of idle memory.
    pub idle: Arc<Mutex<IdleTrim>>,
    /// Told, on each change, whether a task window is the active window, for Windows' touch
    /// gestures. None when nothing follows that.
    pub fronts: Option<Sender<bool>>,
    /// How many times the renderer has made its Vulkan device again after a GPU reset. None
    /// when the renderer cannot say.
    pub renderer_generation: Option<Box<dyn Fn() -> u64 + Send>>,
    /// Where a media button clicked in a window's taskbar preview goes: to the media
    /// controls' connection to the service, which sends it to the app's media session, as a
    /// media key's press goes. None when the media sessions are not followed.
    pub media_pressed: Option<Sender<(String, MediaCommand)>>,
    /// Told when the guest's compositor did not recover from a GPU reset, with the apps that
    /// had windows: it tells the user, and restarts Android when the stall says to. None when
    /// nothing is done about it.
    pub recover: Option<Recovery>,
}

struct Manager {
    windows: TaskWindows,
    /// The windows kept through a GPU reset, and which task each shows.
    resets: GpuResetWindows,
    /// How often each app's window program started again after a crash lately.
    crashes: CrashRestarts,
    /// Whether the guest draws again after a GPU reset.
    stall: FrameStall,
    /// The generations of the window manager's Direct3D device and of the renderer's Vulkan
    /// device last seen: a change is a GPU reset.
    gpu_generation: u64,
    renderer_generation: u64,
    hosts: WindowHosts,
    links: HashMap<HostId, Arc<HostLink>>,
    /// The package each window host that connected said it shows, until its connection ends.
    host_packages: HashMap<HostId, String>,
    remotes: HashMap<i32, Arc<RemoteWindow>>,
    shown: ShownWindows,
    /// The task windows that are the active window.
    focused: std::collections::BTreeSet<i32>,
    /// Whether a task window was the active window, as `fronts` was last told.
    front: bool,
    setup: ManagerSetup,
}

/// Runs the manager until the window manager stops, or every sender of its events is gone.
/// Every window host is disconnected when it ends.
pub fn manage(setup: ManagerSetup) {
    let gpu_generation = setup.gpu.as_ref().map_or(0, |gpu| gpu.generation());
    let renderer_generation = setup.renderer_generation.as_ref().map_or(0, |seen| seen());
    let mut manager = Manager {
        windows: TaskWindows::new(),
        resets: GpuResetWindows::new(),
        crashes: CrashRestarts::new(),
        stall: FrameStall::new(),
        gpu_generation,
        renderer_generation,
        hosts: WindowHosts::new(),
        links: HashMap::new(),
        host_packages: HashMap::new(),
        remotes: HashMap::new(),
        shown: ShownWindows::new(),
        focused: std::collections::BTreeSet::new(),
        front: false,
        setup,
    };
    let mut last_tick = Instant::now();
    while !manager.setup.stop.load(Ordering::Relaxed) {
        match manager.setup.events.recv_timeout(TICK) {
            Ok(event) => manager.handle(event),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if last_tick.elapsed() >= TICK {
            last_tick = Instant::now();
            manager.tick();
        }
        manager.publish();
    }
    let actions = manager.hosts.reset();
    manager.carry_out(actions);
    for link in manager.links.values() {
        link.close();
    }
    manager.setup.router.set_routes([]);
    manager.publish();
}

impl Manager {
    fn log(&self, message: &str) {
        note(self.setup.logger.as_deref(), message);
    }

    fn handle(&mut self, event: ManagerEvent) {
        let now = Instant::now();
        match event {
            ManagerEvent::Sync(current) => {
                // A new watch may follow a restart of the service, which forgot the word on
                // shown windows.
                self.shown.retell();
                for event in &current {
                    if let TaskEvent::App(app) = event {
                        let actions = self.hosts.app(app, now);
                        self.carry_out(actions);
                    }
                }
                let changes = self.windows.sync(&current);
                self.guest_change(changes, now);
            }
            ManagerEvent::Task(event) => {
                if let TaskEvent::App(app) = &*event {
                    let actions = self.hosts.app(app, now);
                    self.carry_out(actions);
                }
                let changes = self.windows.apply(&event);
                for note in self.windows.take_notes() {
                    self.log(&note);
                }
                self.guest_change(changes, now);
                // A task that took a size its window gave it live outside a drag (a
                // maximise, a snap) is reconfigured for it now, at a size it has.
                let requests = self.windows.reconfigures(now);
                self.request(requests);
            }
            ManagerEvent::Lost => {
                let changes = self.windows.reset();
                self.change(changes, now);
                // A window kept for its app's next task waits for nothing now.
                let kept = self.resets.close_all();
                self.send_to_hosts(kept, now);
            }
            ManagerEvent::CloseFailed(task_id) => {
                self.log(&format!(
                    "the request to end task {task_id} did not reach Android; its window \
                     opens again"
                ));
                let changes = self.windows.close_failed(task_id);
                self.change(changes, now);
            }
            ManagerEvent::Attached {
                link,
                package,
                launch,
                shortcut,
            } => {
                let id = link.id();
                self.links.insert(id, link);
                self.host_packages.insert(id, package.clone());
                let actions = self.hosts.attached_for(
                    id,
                    &package,
                    launch,
                    Some(shortcut.as_str()).filter(|id| !id.is_empty()),
                    now,
                );
                self.carry_out(actions);
            }
            ManagerEvent::Report { host, report } => {
                if let Some(report) = self.hosts.from_host(host, report) {
                    self.reported(report, now);
                }
            }
            ManagerEvent::AppWindows(apps) => {
                let changes = self.windows.set_app_windows(&apps);
                self.change(changes, now);
            }
            ManagerEvent::Media { package, media } => {
                let actions = self.hosts.media(&package, media);
                self.carry_out(actions);
            }
            ManagerEvent::Gone { host, exit } => {
                self.links.remove(&host);
                let package = self.hosts.package_of(host).map(str::to_string);
                let tasks = self.hosts.detached(host);
                let crashed = exit.is_some_and(ended_by_crash) && !tasks.is_empty();
                // A program that crashed did not mean its windows to close: it starts again
                // and opens them for the same tasks, which go on in Android. A GPU reset is
                // one way a window program can crash, and the app must not end with it.
                let restart = crashed
                    && package
                        .as_deref()
                        .is_some_and(|package| self.crashes.may_restart(package, now));
                if restart {
                    self.log(&format!(
                        "{}'s window program crashed with task windows {tasks:?} open; it \
                         starts again with them, and their tasks go on",
                        package.as_deref().unwrap_or_default()
                    ));
                    self.program_ended(host);
                    for window in tasks {
                        self.drop_remote(window);
                        // A window kept through a GPU reset has no task to show yet; the app's
                        // next task opens a window of its own.
                        let Some(task_id) = self.resets.window_gone(window) else {
                            continue;
                        };
                        // The new window becomes the active window when the user picks it.
                        if self.focused.remove(&task_id) {
                            self.focus_changed();
                        }
                        let changes = self.windows.reopen(task_id);
                        self.change(changes, now);
                    }
                    return;
                }
                if crashed {
                    self.log(
                        "the window program crashed too often of late to start it again; its \
                         windows close",
                    );
                }
                if !tasks.is_empty() {
                    self.log(&format!(
                        "a window program ended with task windows {tasks:?} open; they close, \
                         and Android ends their tasks"
                    ));
                }
                for window in tasks {
                    self.drop_remote(window);
                    // A window kept through a GPU reset has no task to end.
                    let Some(task_id) = self.resets.window_gone(window) else {
                        continue;
                    };
                    // A window that went with its program is no longer the active window,
                    // though it never said so.
                    if self.focused.remove(&task_id) {
                        self.focus_changed();
                    }
                    // Ending an app's window program closes its windows, as ending any
                    // program does: Android ends their tasks.
                    self.user_closed(task_id, now);
                }
                self.program_ended(host);
            }
        }
    }

    /// The window program on `host` no longer runs. One that ran under a name or icon the
    /// guest has since changed (or never gave) is made again now.
    fn program_ended(&mut self, host: HostId) {
        if let Some(package) = self.host_packages.remove(&host) {
            if !self.host_packages.values().any(|p| *p == package) {
                let app = self.hosts.app_info(&package).cloned();
                self.setup.launcher.ended(&package, app.as_ref());
            }
        }
    }

    fn reported(&mut self, report: FromHost, now: Instant) {
        // The window hosts name windows; the task rules, tasks.
        let report = match self.resets.report(report) {
            Report::Task(report) => report,
            Report::CloseKept(close) => {
                self.log("a window kept through a GPU reset was closed before its app came back");
                self.send_to_hosts(vec![close], now);
                return;
            }
            Report::Dropped => return,
        };
        match report {
            FromHost::Closed { task_id } => self.user_closed(task_id, now),
            // The task takes its window's client size, and draws at its monitor's density.
            // The requests go to the request thread, which sends only the latest size of a
            // task when several are waiting.
            FromHost::Resized {
                task_id,
                width,
                height,
            } => {
                trace_stage(
                    self.setup.logger.as_deref(),
                    task_id,
                    "wm.window_resized",
                    (width, height),
                    "",
                );
                let requests = self.windows.window_resized(task_id, (width, height), now);
                self.request(requests);
            }
            FromHost::Dpi { task_id, dpi } => {
                let (changes, requests) = self.windows.window_dpi(task_id, dpi);
                self.request(requests);
                self.change(changes, now);
            }
            // The sizes asked for while the user drags are live; the end of a drag that
            // resized the task asks once more, not live, which reconfigures the task.
            FromHost::Dragging { task_id, dragging } => {
                let requests = self.windows.window_dragging(task_id, dragging, now);
                self.request(requests);
            }
            FromHost::Caption { task_id, band } => {
                let requests = self.windows.window_caption(task_id, band);
                self.request(requests);
            }
            FromHost::Focus { task_id, focused } => {
                if focused {
                    self.focused.insert(task_id);
                } else {
                    self.focused.remove(&task_id);
                }
                self.focus_changed();
            }
            FromHost::Shown { task_id, shown } => self.shown.reported(task_id, shown),
            FromHost::MenuChosen { task_id, action } => {
                let window = self.resets.window_of(task_id);
                match self.hosts.menu_chosen(window, action) {
                    Some(MenuWork::Request(request)) => self.request(vec![request]),
                    Some(MenuWork::AppPage { package }) => {
                        if let Err(why) = open_app_page(&package) {
                            self.log(&format!("could not open the settings of {package}: {why}"));
                        }
                    }
                    None => {}
                }
            }
            FromHost::MediaButton { task_id, button } => {
                let window = self.resets.window_of(task_id);
                let Some(pressed) = self.hosts.media_pressed(window, button) else {
                    return;
                };
                let sent = self
                    .setup
                    .media_pressed
                    .as_ref()
                    .is_some_and(|media| media.send(pressed).is_ok());
                if !sent {
                    self.log(
                        "the media controls have ended; a media button does not reach Android",
                    );
                }
            }
        }
    }

    /// Publishes which task windows are the active window, and tells `fronts` when whether
    /// any is changes.
    fn focus_changed(&mut self) {
        FOCUSED_WINDOWS.store(self.focused.len(), Ordering::Relaxed);
        let front = !self.focused.is_empty();
        if front != self.front {
            self.front = front;
            if let Some(fronts) = &self.setup.fronts {
                // A touch gestures thread that ended has put the user's value back already.
                let _ = fronts.send(front);
            }
        }
    }

    fn request(&self, requests: Vec<TaskRequest>) {
        for request in requests {
            if self.setup.requests.send(request).is_err() {
                self.log("the request thread has ended; a window's change does not reach Android");
                return;
            }
        }
    }

    fn user_closed(&mut self, task_id: i32, now: Instant) {
        match self.windows.user_closed(task_id) {
            Some(request) => {
                if self.setup.requests.send(request).is_err() {
                    self.log("the request thread has ended; a closed window's task stays");
                }
            }
            None => return,
        }
        self.change(vec![WindowChange::Close { task_id }], now);
    }

    /// The task rules' changes the guest did not cause by ending a task: to the window hosts,
    /// with the IDs of the windows the tasks show.
    fn change(&mut self, changes: Vec<WindowChange>, now: Instant) {
        self.closing(&changes);
        let changes = self.resets.changes(changes);
        self.send_to_hosts(changes, now);
    }

    /// The task rules' changes after the guest's tasks changed. A task that ended soon after a
    /// GPU reset keeps its window and its app is started again; the app's new task takes the
    /// window over.
    fn guest_change(&mut self, changes: Vec<WindowChange>, now: Instant) {
        self.closing(&changes);
        let outcome = self.resets.guest_changes(changes, now);
        for window in &outcome.kept {
            self.log(&format!(
                "task window {window}'s task ended right after a GPU reset; the window stays \
                 open and its app is started again"
            ));
        }
        for package in outcome.relaunch {
            self.request(vec![TaskRequest::Launch {
                package_name: package,
            }]);
        }
        for (task_id, size) in outcome.adopted {
            self.log(&format!(
                "task {task_id} took over task window {} after the GPU reset",
                self.resets.window_of(task_id)
            ));
            // The new task takes the size of the window it shows.
            if let Some(size) = size {
                let requests = self.windows.window_resized(task_id, size, now);
                self.request(requests);
            }
        }
        self.send_to_hosts(outcome.changes, now);
    }

    /// A window that closes is no longer the active window, though it may never say so.
    fn closing(&mut self, changes: &[WindowChange]) {
        let mut changed = false;
        for change in changes {
            if let WindowChange::Close { task_id } = change {
                changed |= self.focused.remove(task_id);
            }
        }
        if changed {
            self.focus_changed();
        }
    }

    /// Changes by window ID, to the window hosts.
    fn send_to_hosts(&mut self, changes: Vec<WindowChange>, now: Instant) {
        for change in changes {
            let actions = self.hosts.change(&change, now);
            self.carry_out(actions);
        }
    }

    /// Notes a GPU reset: the window manager's Direct3D device or the renderer's Vulkan device
    /// was made again since the last look. The window manager's device is asked here too, so
    /// a reset is seen while no frame comes.
    fn follow_gpu_resets(&mut self, now: Instant) {
        let mut reset = false;
        if let Some(gpu) = self.setup.gpu.clone() {
            match gpu.recover() {
                Ok(Some((reason, generation))) => self.log(&format!(
                    "the GPU was reset (0x{:08x}); made a new Direct3D device for the task \
                     windows' textures (number {generation})",
                    reason as u32
                )),
                Ok(None) => {}
                Err(why) => self.log(&format!(
                    "the GPU was reset, and a new Direct3D device cannot be made yet: {why}"
                )),
            }
            let generation = gpu.generation();
            if generation != self.gpu_generation {
                self.gpu_generation = generation;
                reset = true;
            }
        }
        if let Some(seen) = &self.setup.renderer_generation {
            // It changes the moment the renderer finds its device lost, in its own process or
            // in this one, so the reset is followed long before this process's own Direct3D
            // device fails, and before Android ends a task that lost its rendering.
            let generation = seen();
            if generation != self.renderer_generation {
                self.renderer_generation = generation;
                reset = true;
            }
        }
        if reset {
            self.resets.gpu_reset(now);
            self.stall.gpu_reset(now, self.setup.router.frames_shown());
        }
    }

    /// After a GPU reset, looks whether the guest draws again: when no task window has had a
    /// frame for long, its compositor did not recover, and the user is told and Android
    /// restarted, with the apps that had windows opened again.
    fn follow_frame_stall(&mut self, now: Instant) {
        let shown = self.shown.shown_among(&self.windows.windows());
        let frames = self.setup.router.frames_shown();
        let Some(stall) = self.stall.tick(now, &shown, frames) else {
            return;
        };
        self.log(&stall.reason());
        let mut packages = self.windows.window_packages();
        for package in self.resets.kept_packages() {
            if !packages.contains(&package) {
                packages.push(package);
            }
        }
        if let Some(recover) = &self.setup.recover {
            recover(&stall, packages);
        }
    }

    fn tick(&mut self) {
        self.follow_gpu_resets(Instant::now());
        self.follow_frame_stall(Instant::now());
        // A window kept through a GPU reset whose app did not come back closes.
        let late = self.resets.tick(Instant::now());
        if !late.is_empty() {
            self.log(&format!(
                "{} task window(s) kept through a GPU reset close: their app did not come back",
                late.len()
            ));
        }
        self.send_to_hosts(late, Instant::now());
        // A window whose task did not take the size it asked for takes the task's size.
        let changes = self.windows.tick(Instant::now());
        self.change(changes, Instant::now());
        // A task that never took the size it was given live is reconfigured at the size it
        // stayed at, which its window has gone back to.
        let requests = self.windows.reconfigures(Instant::now());
        self.request(requests);
        self.setup.launcher.tick();
        let (failed, actions) = self.hosts.tick(Instant::now());
        for (package, tasks) in failed {
            self.log(&format!(
                "{package}'s window program did not start in time; {} window(s) stay closed",
                tasks.len()
            ));
        }
        self.carry_out(actions);
    }

    fn drop_remote(&mut self, task_id: i32) {
        if let Some(remote) = self.remotes.remove(&task_id) {
            remote.close();
        }
    }

    fn carry_out(&mut self, actions: Vec<HostAction>) {
        for action in actions {
            match action {
                HostAction::Start { package, app } => {
                    if let Err(why) = self.setup.launcher.start(&package, app.as_deref()) {
                        self.log(&format!(
                            "could not start {package}'s window program: {why}"
                        ));
                        self.hosts.start_failed(&package);
                    }
                }
                HostAction::Send { host, message } => {
                    let Some(link) = self.links.get(&host).cloned() else {
                        continue;
                    };
                    // A window whose task drew before its window host connected: its frames
                    // are handed over once the window host has the window.
                    let mut waiting = None;
                    match &message {
                        ToHost::Show { task_id, .. } => match self.remotes.get(task_id) {
                            Some(remote) if remote.is_waiting() => {
                                waiting = Some(Arc::clone(remote));
                            }
                            _ => {
                                if let Some(gpu) = &self.setup.gpu {
                                    let remote = RemoteWindow::new(
                                        *task_id,
                                        Arc::clone(&link),
                                        Arc::clone(gpu),
                                        self.setup.logger.clone(),
                                    );
                                    if let Some(old) = self.remotes.insert(*task_id, remote) {
                                        old.close();
                                    }
                                }
                            }
                        },
                        ToHost::Close { task_id } => {
                            self.focused.remove(task_id);
                            self.focus_changed();
                            self.drop_remote(*task_id)
                        }
                        ToHost::Retitle { .. }
                        | ToHost::AppMenu { .. }
                        | ToHost::AppIcon { .. }
                        | ToHost::AppMedia { .. }
                        | ToHost::Resize { .. }
                        | ToHost::TaskSize { .. }
                        | ToHost::ImeCursor { .. }
                        | ToHost::ImeEditor { .. }
                        | ToHost::CaptionTone { .. }
                        | ToHost::CaptionMode { .. }
                        | ToHost::CaptionBack { .. }
                        | ToHost::Mode { .. } => {}
                    }
                    link.send(to_wire(&message));
                    if let Some(remote) = waiting {
                        remote.attach(link);
                    }
                }
                HostAction::Launch { package, shortcut } => {
                    let request = match shortcut {
                        Some(shortcut_id) => TaskRequest::LaunchShortcut {
                            package_name: package,
                            shortcut_id,
                        },
                        None => TaskRequest::Launch {
                            package_name: package,
                        },
                    };
                    if self.setup.requests.send(request).is_err() {
                        self.log("the request thread has ended; an app cannot be started");
                    }
                }
                HostAction::Disconnect { host, why } => {
                    if let Some(link) = self.links.remove(&host) {
                        self.log(&format!("closing an app window program: {why}"));
                        link.close();
                    }
                    self.remotes.retain(|_, remote| {
                        let keep = !remote.is_on(host);
                        if !keep {
                            remote.close();
                        }
                        keep
                    });
                }
            }
        }
    }

    /// Tells the frame router which display goes to which window, lists the windows, and
    /// tells the guest when whether any window is shown changed, so it can sleep or wake.
    fn publish(&mut self) {
        let windows = self.windows.windows();
        if let Some(request) = self.shown.update(&windows) {
            if let TaskRequest::WindowsShown { shown } = request {
                self.setup
                    .idle
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .windows_shown(shown, Instant::now());
            }
            self.request(vec![request]);
        }
        // Each display's frames go to the window its task shows: a task that took a window
        // over after a GPU reset draws into that window.
        let drawing = self.windows.routes();
        let routed: Vec<(u32, i32)> = drawing
            .iter()
            .map(|(display_id, task_id)| (*display_id, self.resets.window_of(*task_id)))
            .collect();
        // Every task with a window takes its frames from now on, whether or not its window
        // host has connected: the first ones are often its splash screen, drawn once.
        if let Some(gpu) = &self.setup.gpu {
            let logger = self.setup.logger.clone();
            for (_, task_id) in &routed {
                self.remotes.entry(*task_id).or_insert_with(|| {
                    RemoteWindow::waiting(*task_id, Arc::clone(gpu), logger.clone())
                });
            }
        }
        // A task whose window went before its window host connected lets its texture go.
        self.remotes.retain(|task_id, remote| {
            let keep = !remote.is_waiting() || routed.iter().any(|(_, routed)| routed == task_id);
            if !keep {
                remote.close();
            }
            keep
        });
        let routes: Vec<(u32, Arc<RemoteWindow>)> = routed
            .into_iter()
            .zip(drawing)
            .filter_map(|((display_id, window), (_, task_id))| {
                let remote = self.remotes.get(&window)?;
                remote.drawn_by(task_id);
                Some((display_id, Arc::clone(remote)))
            })
            .collect();
        self.setup.router.set_routes(routes);
        *self.setup.listing.lock().unwrap_or_else(|e| e.into_inner()) = windows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr22_app_info_opens_the_apps_page_at_the_app() {
        assert_eq!(
            app_page_arguments("com.example.clock"),
            ["--page", "apps", "--app", "com.example.clock"].map(String::from)
        );
    }
}
