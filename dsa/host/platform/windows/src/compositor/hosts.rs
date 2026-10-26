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
//! passes through the CPU.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::task_windows::{TaskWindows, WindowChange};
use dsa_host_core::compositor::window_hosts::{
    accept_window_host, answer_attached, report_from_wire, to_wire, FromHost, HostAction, HostId,
    ToHost, WindowHosts,
};
use dsa_host_core::tasks::{AppInfo, TaskEvent, TaskRequest};
use dsa_protocol::window_host::types::{envelope::Body, Frame, FrameTexture};
use dsa_protocol::window_host::{decode, encode_body};
use dsa_transport::{Connection, FrameSender, Listener, TransportError};
use windows_sys::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

use super::frames::{FrameRouter, FrameTarget};
use super::log::{note, CompositorLogger};
use super::present::{Gpu, SharedTexture};

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

/// What the other threads tell the manager.
pub enum ManagerEvent {
    /// A new watch began; these are the guest's tasks now.
    Sync(Vec<TaskEvent>),
    /// A change to the guest's tasks.
    Task(TaskEvent),
    /// The service cannot be reached, so the guest's tasks are unknown.
    Lost,
    /// The request to end a task whose window the user closed did not reach the guest.
    CloseFailed(i32),
    /// A window host connected and said which app it shows.
    Attached {
        link: Arc<HostLink>,
        package: String,
        launch: bool,
    },
    /// A window host reported what the user did to a window.
    Report { host: HostId, report: FromHost },
    /// A window host's connection ended.
    Gone { host: HostId },
}

/// Starts each app's window program.
pub trait HostLauncher: Send {
    /// Starts the window host of `package`, named and drawn as `app` when it is known. The
    /// window host then connects by itself.
    fn start(&mut self, package: &str, app: Option<&AppInfo>) -> Result<(), String>;
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
    texture: Option<Arc<HostTexture>>,
    sequence: u64,
    closed: bool,
}

/// A task window in a window host, as the renderer's frame target.
pub struct RemoteWindow {
    task_id: i32,
    link: Arc<HostLink>,
    gpu: Arc<Gpu>,
    state: Mutex<RemoteState>,
    logger: Option<Arc<CompositorLogger>>,
}

impl RemoteWindow {
    pub fn new(
        task_id: i32,
        link: Arc<HostLink>,
        gpu: Arc<Gpu>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> Arc<RemoteWindow> {
        Arc::new(RemoteWindow {
            task_id,
            link,
            gpu,
            state: Mutex::new(RemoteState {
                texture: None,
                sequence: 0,
                closed: false,
            }),
            logger,
        })
    }

    /// The window is gone: nothing more is sent for it.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.texture = None;
    }
}

impl FrameTarget for RemoteWindow {
    type Texture = HostTexture;

    fn texture(&self, width: u32, height: u32) -> Option<Arc<HostTexture>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || self.link.is_closed() {
            return None;
        }
        if let Some(texture) = &state.texture {
            if texture.shared.size() == (width, height) {
                return Some(Arc::clone(texture));
            }
        }
        state.texture = None;
        let shared = match self.gpu.shared_texture(width, height) {
            Ok(shared) => shared,
            Err(why) => {
                note(self.logger.as_deref(), &why);
                return None;
            }
        };
        let Some(handle) = self.link.place(shared.handle()) else {
            note(
                self.logger.as_deref(),
                &format!(
                    "could not hand task {}'s frames to its window: {}",
                    self.task_id,
                    std::io::Error::last_os_error()
                ),
            );
            return None;
        };
        let id = self.link.next_texture.fetch_add(1, Ordering::Relaxed);
        let announced = self.link.send(Body::FrameTexture(FrameTexture {
            task_id: self.task_id,
            texture_id: id,
            handle,
            width,
            height,
        }));
        if !announced {
            return None;
        }
        let texture = Arc::new(HostTexture { shared, id });
        state.texture = Some(Arc::clone(&texture));
        Some(texture)
    }

    fn handle(texture: &HostTexture) -> isize {
        texture.shared.handle()
    }

    fn present(&self, texture: &HostTexture) {
        let sequence = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.closed {
                return;
            }
            state.sequence += 1;
            state.sequence
        };
        let sent = self.link.send(Body::Frame(Frame {
            task_id: self.task_id,
            texture_id: texture.id,
            sequence,
        }));
        // The renderer writes the texture again once this returns, so the window host must
        // have copied the frame out by then.
        if sent
            && !self
                .link
                .wait_presented(self.task_id, sequence, PRESENT_WAIT)
        {
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
    let attached = ManagerEvent::Attached {
        link: Arc::clone(&link),
        package: attach.package_name,
        launch: attach.launch,
    };
    if events.send(attached).is_err() {
        return;
    }
    while !stop.load(Ordering::Relaxed) && !link.is_closed() {
        let frame = match connection.recv(Some(Duration::from_millis(200))) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(_) => break,
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
    let _ = events.send(ManagerEvent::Gone { host: id });
}

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
}

struct Manager {
    windows: TaskWindows,
    hosts: WindowHosts,
    links: HashMap<HostId, Arc<HostLink>>,
    remotes: HashMap<i32, Arc<RemoteWindow>>,
    setup: ManagerSetup,
}

/// Runs the manager until the window manager stops, or every sender of its events is gone.
/// Every window host is disconnected when it ends.
pub fn manage(setup: ManagerSetup) {
    let mut manager = Manager {
        windows: TaskWindows::new(),
        hosts: WindowHosts::new(),
        links: HashMap::new(),
        remotes: HashMap::new(),
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
                for event in &current {
                    if let TaskEvent::App(app) = event {
                        self.hosts.app(app);
                    }
                }
                let changes = self.windows.sync(&current);
                self.change(changes, now);
            }
            ManagerEvent::Task(event) => {
                if let TaskEvent::App(app) = &event {
                    self.hosts.app(app);
                }
                let changes = self.windows.apply(&event);
                self.change(changes, now);
            }
            ManagerEvent::Lost => {
                let changes = self.windows.reset();
                self.change(changes, now);
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
            } => {
                let id = link.id();
                self.links.insert(id, link);
                let actions = self.hosts.attached(id, &package, launch, now);
                self.carry_out(actions);
            }
            ManagerEvent::Report { host, report } => {
                if let Some(report) = self.hosts.from_host(host, report) {
                    self.reported(report, now);
                }
            }
            ManagerEvent::Gone { host } => {
                self.links.remove(&host);
                for task_id in self.hosts.detached(host) {
                    self.drop_remote(task_id);
                    // Ending an app's window program closes its windows, as ending any
                    // program does: Android ends their tasks.
                    self.user_closed(task_id, now);
                }
            }
        }
    }

    fn reported(&mut self, report: FromHost, now: Instant) {
        match report {
            FromHost::Closed { task_id } => self.user_closed(task_id, now),
            // The guest learns of the window's size, scale and focus once the task follows
            // them; until then the window shows the task at the size the guest gave it.
            FromHost::Resized { .. } | FromHost::Dpi { .. } | FromHost::Focus { .. } => {}
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

    fn change(&mut self, changes: Vec<WindowChange>, now: Instant) {
        for change in changes {
            let actions = self.hosts.change(&change, now);
            self.carry_out(actions);
        }
    }

    fn tick(&mut self) {
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
                    if let Err(why) = self.setup.launcher.start(&package, app.as_ref()) {
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
                    match &message {
                        ToHost::Show { task_id, .. } => {
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
                        ToHost::Close { task_id } => self.drop_remote(*task_id),
                        ToHost::Retitle { .. } | ToHost::Resize { .. } => {}
                    }
                    link.send(to_wire(&message));
                }
                HostAction::Launch { package } => {
                    let request = TaskRequest::Launch {
                        package_name: package,
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
                        let keep = remote.link.id() != host;
                        if !keep {
                            remote.close();
                        }
                        keep
                    });
                }
            }
        }
    }

    /// Tells the frame router which display goes to which window, and lists the windows.
    fn publish(&self) {
        let routes: Vec<(u32, Arc<RemoteWindow>)> = self
            .windows
            .routes()
            .into_iter()
            .filter_map(|(display_id, task_id)| {
                let remote = self.remotes.get(&task_id)?;
                Some((display_id, Arc::clone(remote)))
            })
            .collect();
        self.setup.router.set_routes(routes);
        *self.setup.listing.lock().unwrap_or_else(|e| e.into_inner()) = self.windows.windows();
    }
}
