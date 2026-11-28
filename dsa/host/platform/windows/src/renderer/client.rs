//! The window manager's side of the renderer process: starting `dsa-gfx.exe`, and handing it
//! the task windows' textures.
//!
//! [`ProgramLauncher`] starts the renderer beside the window manager with the name of a pipe
//! only this user may open, waits for it to dial back, and keeps a handle to its process, which
//! the textures are duplicated into. A renderer that kept running after the window manager
//! before this one ended (it crashed, or was ended) waits on [`adoption_pipe`]: the launcher
//! takes that one over instead, so the guest keeps everything it made in it. [`RendererClient`] is what the compositor sees as its
//! display presenter: it tells the renderer which VM's streams to take, answers each frame's
//! request with the texture the app window's frame router gives (handing a new texture over
//! once), passes each finished frame to the router, and asks the renderer for its memory.

use std::collections::HashSet;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::{GraphicsRenderer, GraphicsStreamKind};
use dsa_host_core::renderer::{
    describe_exit_code, Given, Launched, Pending, RendererChild, RendererHandler, RendererLauncher,
    RendererLink, RendererProcessMemory, TextureBook, ADDRESS_VARIABLE, HELLO_TIMEOUT,
    QUESTION_TIMEOUT, WINDOW_MANAGER_ARGUMENT,
};
use dsa_protocol::renderer::types::{
    envelope::Body, ForgetDisplay, GuestVm, MemoryReport, MemoryRequest, Start, Started, Target,
};
use dsa_transport::local::LocalListener;
use dsa_transport::TransportError;
use windows_sys::Win32::Foundation::{
    CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetExitCodeProcess, OpenProcess, TerminateProcess, WaitForSingleObject,
    PROCESS_DUP_HANDLE, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
};

use dsa_host_core::graphics_state::GraphicsState;

use super::{
    adoption_pipe, describe_frame_path, graphics_state_of, start_with, started_adapter,
    started_gpu, RENDERER_EXE,
};
use crate::compositor::adapter::{DisplayPresenter, DisplaySink};
use crate::compositor::log::{note, renderer_line, CompositorLogger};
use crate::guid::Guid;
use crate::pipe::{current_user_only_sddl, NamedPipeListener, SameUserPipeConnector};
use crate::render_gpu::{describe_outcome, GpuRequest};
use crate::vm_listener::VmIdSlot;
use crate::vulkan_driver::VulkanDriverSetting;

/// No console window for the renderer.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// How often the window manager looks whether the VM changed, to tell the renderer.
const VM_POLL: Duration = Duration::from_millis(100);

pub use dsa_host_core::renderer::describe_device_state;

/// The compositor's stream renderer once the renderer runs in its own process: the
/// compositor never takes a stream itself (`Compositor::without_graphics_streams`), so this
/// only closes one that reaches it.
pub struct ServedByRenderer;

impl GraphicsRenderer for ServedByRenderer {
    fn serve_stream(
        &self,
        _kind: GraphicsStreamKind,
        mut connection: Box<dyn dsa_transport::Connection>,
    ) -> Result<(), String> {
        connection.close();
        Err(format!(
            "the guest's graphics streams are served by {RENDERER_EXE}, not the window manager"
        ))
    }
}

/// A handle to a process, closed when dropped.
pub struct ProcessHandle(HANDLE);

// SAFETY: a kernel handle, usable from any thread.
unsafe impl Send for ProcessHandle {}
unsafe impl Sync for ProcessHandle {}

impl ProcessHandle {
    /// A handle of our own to `child`'s process, with the access `Command` opened it with
    /// (which includes duplicating handles into it).
    pub fn of(child: &Child) -> Result<ProcessHandle, String> {
        let mut copy: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates a handle of this process into this process.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                child.as_raw_handle() as HANDLE,
                GetCurrentProcess(),
                &mut copy,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(format!(
                "no handle to the renderer's process: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ProcessHandle(copy))
    }

    /// A handle to the running process `pid`, with `access`.
    pub fn open(pid: u32, access: u32) -> Result<ProcessHandle, String> {
        // SAFETY: opens a process by its ID; the handle is closed on drop.
        let handle = unsafe { OpenProcess(access, 0, pid) };
        if handle.is_null() {
            return Err(format!(
                "the renderer's process {pid} cannot be opened: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ProcessHandle(handle))
    }

    /// Duplicates `handle`, a handle of this process, into the process. Returns its value
    /// there; the process owns it from then on.
    pub fn give(&self, handle: isize) -> Result<u64, String> {
        let mut theirs: HANDLE = std::ptr::null_mut();
        // SAFETY: a handle of this process, duplicated into a process we hold a handle to.
        let duplicated = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle as HANDLE,
                self.0,
                &mut theirs,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if duplicated == 0 {
            return Err(format!(
                "a texture could not be handed to the renderer: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(theirs as usize as u64)
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The renderer the shared slot names, for the textures to go into.
pub type RendererProcess = Arc<Mutex<Option<ProcessHandle>>>;

/// A started `dsa-gfx.exe`.
struct ProcessChild(Child);

impl RendererChild for ProcessChild {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn exited(&mut self) -> Option<String> {
        match self.0.try_wait() {
            Ok(Some(status)) => Some(match status.code() {
                Some(code) => describe_exit_code(i64::from(code)),
                None => "it ended".to_string(),
            }),
            Ok(None) => None,
            Err(error) => Some(format!("its state cannot be read: {error}")),
        }
    }

    fn kill(&mut self) {
        let _ = self.0.kill();
    }
}

impl Drop for ProcessChild {
    fn drop(&mut self) {
        // A renderer that is still running when the window manager lets go of it is ended:
        // it must never outlive the window manager that hands it its textures.
        if matches!(self.0.try_wait(), Ok(None)) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

/// A `dsa-gfx.exe` that the window manager before this one started, and this one took over.
/// It is no child of this process, so it is watched and ended through a handle to it.
struct AdoptedChild {
    pid: u32,
    process: ProcessHandle,
}

impl RendererChild for AdoptedChild {
    fn id(&self) -> u32 {
        self.pid
    }

    fn exited(&mut self) -> Option<String> {
        // SAFETY: a process handle of ours with SYNCHRONIZE, waited on without blocking.
        if unsafe { WaitForSingleObject(self.process.0, 0) } != WAIT_OBJECT_0 {
            return None;
        }
        let mut code = 0u32;
        // SAFETY: a process handle of ours with query access, and a local to write to.
        if unsafe { GetExitCodeProcess(self.process.0, &mut code) } == 0 {
            return Some("it ended".to_string());
        }
        Some(describe_exit_code(i64::from(code as i32)))
    }

    fn kill(&mut self) {
        // SAFETY: a process handle of ours with PROCESS_TERMINATE.
        unsafe { TerminateProcess(self.process.0, 1) };
    }

    fn adopted(&self) -> bool {
        true
    }
}

impl Drop for AdoptedChild {
    fn drop(&mut self) {
        // As with a started one: a renderer this window manager lets go of while it still runs
        // is ended.
        if self.exited().is_none() {
            self.kill();
            // SAFETY: a process handle of ours with SYNCHRONIZE.
            unsafe { WaitForSingleObject(self.process.0, 5000) };
        }
    }
}

/// How long the launcher waits for a renderer waiting to be taken over to take the call. One
/// that waits answers at once; with none, the pipe does not exist and the call fails at once.
const ADOPTION_CALL: Duration = Duration::from_secs(2);

/// Starts `dsa-gfx.exe` from beside the window manager.
pub struct ProgramLauncher {
    /// The renderer's program.
    pub program: PathBuf,
    /// Variables the renderer gets beside the window manager's own (tests name the backend
    /// library this way).
    pub env: Vec<(String, String)>,
    /// The renderer's arguments instead of `--window-manager <pipe>`, which then goes in
    /// the environment: a test runs the renderer inside its own test program this way.
    pub args: Option<Vec<String>>,
    /// Where the started renderer's process handle goes.
    pub process: RendererProcess,
    /// Whether a renderer that kept running after the window manager before this one ended is
    /// taken over before a new one is started. Only the installed window manager does: a test
    /// that starts its own renderers must never take over the user's.
    pub adopt: bool,
}

/// How many renderer pipes this process has opened. The count is the process's, not one
/// launcher's: two launchers in one process (as two tests in one test program have) would
/// otherwise name the same pipe, and the second could not open it.
static PIPES: AtomicU64 = AtomicU64::new(0);

impl ProgramLauncher {
    /// Starts `program`, handing its process to `process`.
    pub fn new(program: PathBuf, process: RendererProcess) -> ProgramLauncher {
        ProgramLauncher {
            program,
            env: Vec::new(),
            args: None,
            process,
            adopt: false,
        }
    }

    /// The renderer installed beside this program.
    pub fn installed(process: RendererProcess) -> ProgramLauncher {
        let program = std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|dir| dir.join(RENDERER_EXE)))
            .unwrap_or_else(|| PathBuf::from(RENDERER_EXE));
        ProgramLauncher {
            adopt: true,
            ..ProgramLauncher::new(program, process)
        }
    }

    /// Takes over the renderer that waits on [`adoption_pipe`], if one does. `None` when none
    /// waits, or it cannot be taken over; a new one is then started.
    fn take_over(&mut self) -> Option<Launched> {
        let mut connector = SameUserPipeConnector {
            name: adoption_pipe(),
        };
        let (mut connection, pid) = connector.connect_to_server(ADOPTION_CALL).ok()?;
        let opened = ProcessHandle::open(
            pid,
            PROCESS_DUP_HANDLE
                | PROCESS_QUERY_LIMITED_INFORMATION
                | PROCESS_SYNCHRONIZE
                | PROCESS_TERMINATE,
        )
        .and_then(|watched| {
            let textures = ProcessHandle::open(pid, PROCESS_DUP_HANDLE)?;
            Ok((watched, textures))
        });
        let (watched, textures) = match opened {
            Ok(handles) => handles,
            Err(_) => {
                // Without a handle the textures cannot reach it: it ends once its call breaks,
                // and a fresh one is started.
                connection.close();
                return None;
            }
        };
        *self.process.lock().unwrap_or_else(|e| e.into_inner()) = Some(textures);
        Some(Launched {
            process: Box::new(AdoptedChild {
                pid,
                process: watched,
            }),
            connection,
        })
    }
}

impl RendererLauncher for ProgramLauncher {
    fn launch(&mut self) -> Result<Launched, String> {
        if self.adopt {
            if let Some(adopted) = self.take_over() {
                return Ok(adopted);
            }
        }
        if !self.program.is_file() {
            return Err(format!("{} is not installed", self.program.display()));
        }
        let count = PIPES.fetch_add(1, Ordering::Relaxed) + 1;
        let name = format!(
            "dsa-gfx-{}-{}-{count}",
            crate::compositor::current_session(),
            std::process::id(),
        );
        let sddl = current_user_only_sddl().map_err(|e| format!("reading this user: {e}"))?;
        let mut listener = NamedPipeListener::bind(&name, &sddl)
            .map_err(|e| format!("the renderer's pipe could not be opened: {e}"))?;
        let mut command = Command::new(&self.program);
        match &self.args {
            Some(args) => {
                command.args(args).env(ADDRESS_VARIABLE, &name);
            }
            None => {
                command.arg(WINDOW_MANAGER_ARGUMENT).arg(&name);
            }
        }
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW);
        for (key, value) in &self.env {
            command.env(key, value);
        }
        let child = command
            .spawn()
            .map_err(|e| format!("{} did not start: {e}", self.program.display()))?;
        let mut child = ProcessChild(child);
        let handle = ProcessHandle::of(&child.0)?;
        let deadline = Instant::now() + HELLO_TIMEOUT;
        let connection = loop {
            match listener.accept(Duration::from_millis(200)) {
                Ok((connection, _)) => break connection,
                Err(TransportError::Timeout) => {
                    if let Some(reason) = child.exited() {
                        return Err(format!("it ended before it connected: {reason}"));
                    }
                    if Instant::now() >= deadline {
                        child.kill();
                        return Err(format!(
                            "it did not connect within {} s",
                            HELLO_TIMEOUT.as_secs()
                        ));
                    }
                }
                Err(error) => {
                    child.kill();
                    return Err(format!("its connection could not be taken: {error}"));
                }
            }
        };
        *self.process.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
        Ok(Launched {
            process: Box::new(child),
            connection,
        })
    }
}

#[derive(Default)]
struct State {
    sink: Option<Arc<dyn DisplaySink>>,
    link: Option<RendererLink>,
    started: Option<Started>,
    book: TextureBook<isize>,
    /// How many renderers have started, and how many tries failed before one did.
    starts: u64,
    failures: u64,
    /// The VM the current renderer was last told of; `None` until it is told.
    told_vm: Option<Option<Guid>>,
    /// Displays whose texture could not be handed over, logged once each.
    refused: HashSet<u32>,
}

/// Told what each renderer that starts runs on: the window manager passes it to the service
/// and tells the user where they must act.
pub type GraphicsListener = Box<dyn Fn(&GraphicsState) + Send + Sync>;

/// The renderer process, as the compositor presents through it.
pub struct RendererClient {
    request: GpuRequest,
    vulkan: VulkanDriverSetting,
    on_graphics: Option<GraphicsListener>,
    ports: (u32, u32),
    state: Mutex<State>,
    changed: Condvar,
    memory: Pending<MemoryReport>,
    process: RendererProcess,
    logger: Option<Arc<CompositorLogger>>,
    /// How many times a renderer said its device was lost or made again: each DeviceState
    /// is a change. Kept across renderer restarts, whose counts start from zero again.
    device_changes: AtomicU64,
}

impl RendererClient {
    /// A client that starts the renderer on the GPU `request` asks for, taking the guest's
    /// streams on the vsock `ports` (the command stream's, then the process pipe's).
    pub fn new(
        request: GpuRequest,
        ports: (u32, u32),
        process: RendererProcess,
        logger: Option<Arc<CompositorLogger>>,
    ) -> Arc<RendererClient> {
        Self::with_vulkan(
            request,
            VulkanDriverSetting::Auto,
            None,
            ports,
            process,
            logger,
        )
    }

    /// [`new`](Self::new), with the user's Vulkan driver setting, and `on_graphics` told what
    /// each renderer runs on once it has started.
    pub fn with_vulkan(
        request: GpuRequest,
        vulkan: VulkanDriverSetting,
        on_graphics: Option<GraphicsListener>,
        ports: (u32, u32),
        process: RendererProcess,
        logger: Option<Arc<CompositorLogger>>,
    ) -> Arc<RendererClient> {
        Arc::new(RendererClient {
            request,
            vulkan,
            on_graphics,
            ports,
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            memory: Pending::new(),
            process,
            logger,
            device_changes: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn log_line(&self, line: &str) {
        note(self.logger.as_deref(), line);
    }

    /// Waits up to `timeout` for the first renderer to start, or for a try to fail. True
    /// when one started.
    pub fn wait_started(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.starts > 0 {
                return true;
            }
            if state.failures > 0 {
                return false;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            state = self
                .changed
                .wait_timeout(state, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// How many renderers have started so far.
    pub fn starts(&self) -> u64 {
        self.lock().starts
    }

    /// The process ID of the renderer running now.
    pub fn renderer_process_id(&self) -> Option<u32> {
        self.lock().link.as_ref().map(RendererLink::process_id)
    }

    /// Tells each renderer which VM's streams to take, from `vm` (the compositor's VM slot),
    /// on a thread of its own until `stop` is set.
    pub fn follow_vm(
        self: &Arc<Self>,
        vm: VmIdSlot,
        stop: Arc<AtomicBool>,
    ) -> std::io::Result<thread::JoinHandle<()>> {
        let client = Arc::clone(self);
        thread::Builder::new()
            .name("dsa-wm-renderer-vm".into())
            .spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let current = *vm.lock().unwrap_or_else(|e| e.into_inner());
                    client.tell_vm(current);
                    thread::sleep(VM_POLL);
                }
            })
    }

    /// Tells the renderer running now of `vm`, when it has not been told of it.
    pub fn tell_vm(&self, vm: Option<Guid>) {
        let mut state = self.lock();
        if state.told_vm == Some(vm) {
            return;
        }
        let Some(link) = state.link.clone() else {
            return;
        };
        let vm_id = vm.map(|id| id.to_string()).unwrap_or_default();
        if link.send(Body::GuestVm(GuestVm { vm_id })) {
            state.told_vm = Some(vm);
        }
    }

    fn ask_memory(&self, give_back: bool) -> Option<RendererProcessMemory> {
        let link = self.lock().link.clone()?;
        let request = self.memory.begin();
        if !link.send(Body::MemoryRequest(MemoryRequest { request, give_back })) {
            return None;
        }
        self.memory
            .wait(request, QUESTION_TIMEOUT)
            .map(|report| RendererProcessMemory::from_report(&report))
    }

    /// Answers a frame's request: the texture the frame router gives for the display, handed
    /// over to the renderer when it does not hold it yet.
    fn answer(&self, request: dsa_protocol::renderer::types::TargetRequest, link: &RendererLink) {
        let sink = self.lock().sink.clone();
        // The router makes the texture outside the client's lock: it may wait on the GPU.
        let handle =
            sink.and_then(|sink| sink.target(request.display_id, request.width, request.height));
        let mut target = Target {
            request: request.request,
            drop: true,
            texture_id: 0,
            handle: 0,
            width: request.width,
            height: request.height,
        };
        let mut refused = None;
        {
            let mut state = self.lock();
            match state
                .book
                .give(request.display_id, handle, &request.held_textures)
            {
                Given::Drop => {}
                Given::Same(texture_id) => {
                    target.drop = false;
                    target.texture_id = texture_id;
                }
                Given::New(texture_id) => {
                    let given = match (
                        handle,
                        self.process
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .as_ref(),
                    ) {
                        (Some(handle), Some(process)) => process.give(handle),
                        _ => Err("the renderer's process is not known".to_string()),
                    };
                    match given {
                        Ok(theirs) => {
                            target.drop = false;
                            target.texture_id = texture_id;
                            target.handle = theirs;
                            state.refused.remove(&request.display_id);
                        }
                        Err(why) => {
                            state.book.withdraw(request.display_id);
                            if state.refused.insert(request.display_id) {
                                refused = Some(why);
                            }
                        }
                    }
                }
            }
        }
        if let Some(why) = refused {
            self.log_line(&format!(
                "display {}'s frames cannot reach its window: {why}",
                request.display_id
            ));
        }
        link.send(Body::Target(target));
    }
}

impl RendererHandler for RendererClient {
    fn start(&self) -> Start {
        start_with(&self.request, self.vulkan, self.ports)
    }

    fn connected(&self, started: &Started, link: &RendererLink) {
        // Every frame's request passes through this thread on its way to a window.
        crate::scheduling::raise_this_thread();
        for line in &started.notes {
            self.log_line(line);
        }
        // Which Vulkan and why, once for each renderer that starts.
        let graphics = graphics_state_of(started);
        if !started.vulkan_driver_reason.is_empty() {
            self.log_line(&format!("Vulkan: {}", started.vulkan_driver_reason));
        }
        if let Some(listener) = &self.on_graphics {
            listener(&graphics);
        }
        match started_gpu(started) {
            Some(gpu) => self.log_line(&describe_outcome(&self.request, &gpu)),
            None => self.log_line("the graphics renderer does not say which GPU it renders on"),
        }
        let restarted = {
            let mut state = self.lock();
            let restarted = state.starts > 0;
            if restarted {
                let before = state.started.as_ref().and_then(started_adapter);
                if before.is_some() && before != started_adapter(started) {
                    self.log_line(
                        "the graphics renderer came back on another GPU; the windows' textures \
                         stay on the first one until the window manager restarts",
                    );
                }
            }
            state.link = Some(link.clone());
            state.started = Some(started.clone());
            state.book.clear();
            state.refused.clear();
            state.told_vm = None;
            state.starts += 1;
            restarted
        };
        self.log_line(&format!(
            "the graphics renderer ({RENDERER_EXE}, pid {}) is {}; {}",
            link.process_id(),
            if restarted {
                "running again"
            } else {
                "running"
            },
            describe_frame_path(started)
        ));
        self.changed.notify_all();
    }

    fn message(&self, body: Body, link: &RendererLink) {
        match body {
            Body::TargetRequest(request) => self.answer(request, link),
            Body::FrameDone(done) => {
                let (sink, current) = {
                    let state = self.lock();
                    (state.sink.clone(), state.book.texture_of(done.display_id))
                };
                // A frame for a texture the display no longer has is not shown.
                if current == Some(done.texture_id) {
                    if let Some(sink) = sink {
                        sink.frame(done.display_id, (done.width, done.height));
                    }
                }
            }
            Body::MemoryReport(report) => self.memory.complete(report.request, report),
            Body::DeviceState(state) => {
                // The window manager's thread looks at this within half a second and takes
                // the change as a GPU reset, without waiting for its own device to fail.
                self.device_changes.fetch_add(1, Ordering::SeqCst);
                self.log_line(&describe_device_state(&state));
            }
            Body::RendererLog(line) => {
                let severity = u8::try_from(line.severity).unwrap_or(b'E');
                if let Some(line) = renderer_line(severity, &line.message) {
                    self.log_line(&line);
                }
            }
            Body::Note(line) => self.log_line(&format!("graphics renderer: {}", line.text)),
            _ => {}
        }
    }

    fn disconnected(&self) {
        let mut state = self.lock();
        state.link = None;
        state.told_vm = None;
        *self.process.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn log(&self, line: &str) {
        self.log_line(line);
    }

    fn start_failed(&self) {
        self.lock().failures += 1;
        self.changed.notify_all();
    }
}

impl DisplayPresenter for RendererClient {
    fn present_displays(&self, sink: Option<Arc<dyn DisplaySink>>) -> Result<(), String> {
        let mut state = self.lock();
        state.sink = sink;
        // Textures the router no longer knows go over again.
        state.book.clear();
        Ok(())
    }

    fn adapter_luid(&self) -> Option<[u8; 8]> {
        self.lock().started.as_ref().and_then(started_adapter)
    }

    fn forget_display(&self, display_id: u32) {
        let link = {
            let mut state = self.lock();
            state.book.withdraw(display_id);
            state.link.clone()
        };
        if let Some(link) = link {
            link.send(Body::ForgetDisplay(ForgetDisplay { display_id }));
        }
    }

    fn memory(&self) -> Option<RendererProcessMemory> {
        self.ask_memory(false)
    }

    fn device_generation(&self) -> u64 {
        self.device_changes.load(Ordering::SeqCst)
    }

    fn give_back(&self) -> Option<RendererProcessMemory> {
        self.ask_memory(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_protocol::renderer::types::DeviceState;

    fn client() -> Arc<RendererClient> {
        RendererClient::new(GpuRequest::Ranking, (0, 0), Arc::default(), None)
    }

    #[test]
    fn pr3_each_started_renderer_says_what_it_runs_on_and_asks_with_the_setting() {
        let heard: Arc<Mutex<Vec<GraphicsState>>> = Arc::default();
        let into = Arc::clone(&heard);
        let client = RendererClient::with_vulkan(
            GpuRequest::Ranking,
            VulkanDriverSetting::Dozen,
            Some(Box::new(move |state: &GraphicsState| {
                into.lock().unwrap().push(state.clone())
            })),
            (0, 0),
            Arc::default(),
            None,
        );
        let start = RendererHandler::start(&*client);
        assert_eq!(
            super::super::vulkan_driver_of(&start),
            VulkanDriverSetting::Dozen
        );
        let mut started = Started::default();
        super::super::report_vulkan_driver(
            &mut started,
            crate::vulkan_driver::DriverOutcome::None,
            "install the pack",
        );
        RendererHandler::connected(&*client, &started, &RendererLink::detached());
        assert_eq!(
            *heard.lock().unwrap(),
            vec![GraphicsState::Unavailable("install the pack".into())]
        );
    }

    #[test]
    fn nfr5_the_renderers_lost_device_reaches_the_window_manager_across_the_processes() {
        let client = client();
        let link = RendererLink::detached();
        // Before, the window manager read 0 from a renderer in another process, whatever it did.
        assert_eq!(DisplayPresenter::device_generation(&*client), 0);
        let lost = DeviceState {
            losses: 1,
            generation: 0,
        };
        RendererHandler::message(&*client, Body::DeviceState(lost), &link);
        let at_loss = DisplayPresenter::device_generation(&*client);
        assert_ne!(at_loss, 0, "the loss is a change at once");
        let made = DeviceState {
            losses: 1,
            generation: 1,
        };
        RendererHandler::message(&*client, Body::DeviceState(made), &link);
        assert_ne!(DisplayPresenter::device_generation(&*client), at_loss);
    }

    #[test]
    fn nfr5_a_restarted_renderers_counts_from_zero_still_change_what_is_followed() {
        let client = client();
        let link = RendererLink::detached();
        let mut seen = vec![DisplayPresenter::device_generation(&*client)];
        for (losses, generation) in [(1, 0), (1, 1), (1, 0)] {
            let state = DeviceState { losses, generation };
            RendererHandler::message(&*client, Body::DeviceState(state), &link);
            seen.push(DisplayPresenter::device_generation(&*client));
        }
        assert!(seen.windows(2).all(|pair| pair[1] != pair[0]));
    }

    #[test]
    fn nfr5_the_log_says_whether_the_device_was_lost_or_made_again() {
        let lost = describe_device_state(&DeviceState {
            losses: 2,
            generation: 1,
        });
        assert!(lost.contains("lost (loss 2)"), "{lost}");
        let made = describe_device_state(&DeviceState {
            losses: 2,
            generation: 2,
        });
        assert!(
            made.contains("made its GPU device again (number 2"),
            "{made}"
        );
    }
}
