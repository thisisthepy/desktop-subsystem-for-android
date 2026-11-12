//! The graphics renderer in a process of its own, `dsa-gfx`, and the window manager that
//! starts it.
//!
//! The renderer (gfxstream) owns the guest's graphics streams and the render GPU, and puts
//! each task's frames into GPU shared textures. A crash in it, or in the GPU driver under it,
//! used to take the window manager and every app window with it. Now only the renderer
//! process ends: the window manager and the app windows stay, the windows keep their last
//! frame, and the window manager starts the renderer again. The guest sees its graphics
//! streams drop and connects again, and the windows draw again when new frames arrive.
//!
//! This module holds what every host shares:
//!
//! - [`supervise`]: the window manager's side. It starts the renderer through a
//!   [`RendererLauncher`], checks its handshake, sends [`Start`], hands every message to a
//!   [`RendererHandler`], and starts it again when it ends, waiting longer after each quick
//!   failure ([`RestartBackoff`]). Each end is logged with its reason ([`restart_line`]).
//! - [`WindowManagerLink`]: the renderer's side: its requests for a frame's texture, which
//!   wait for the window manager's answer, and the messages it sends.
//! - [`TextureBook`] and [`HeldTargets`]: which texture each display was given, on each side,
//!   so a texture crosses between the processes once and not with every frame.
//!
//! How a texture crosses (a Direct3D handle, an IOSurface, a dmabuf) is the platform's, in
//! its launcher and handler.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::renderer::types::{
    envelope::Body, DeviceState, MemoryReport, Note, RendererLog, Role, Start, Started, Stop,
    Target, TargetRequest,
};
use dsa_protocol::renderer::{
    decode_body, local_hello, renderer_connect, send_on, window_manager_accept,
};
use dsa_transport::{Connection, FrameSender, TransportError};

use crate::memory::{mib, HeldMemory, RendererMemory};

/// The renderer's program name, without the platform's extension.
pub const RENDERER_PROGRAM: &str = "dsa-gfx";

/// The argument that tells the renderer where its window manager waits: `dsa-gfx
/// --window-manager <address>`, where the address is a pipe name (Windows) or a socket's
/// descriptor or path (Linux, macOS).
pub const WINDOW_MANAGER_ARGUMENT: &str = "--window-manager";

/// Where the renderer finds its window manager's address when it is not given as an
/// argument: a test that runs the renderer inside its own test program passes it this way.
pub const ADDRESS_VARIABLE: &str = "DSA_RENDERER_ADDRESS";

/// The window manager's address from the renderer's arguments (`--window-manager
/// <address>`), else from [`ADDRESS_VARIABLE`].
pub fn address_from(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg == WINDOW_MANAGER_ARGUMENT)
        .and_then(|at| args.get(at + 1).cloned())
        .or_else(|| std::env::var(ADDRESS_VARIABLE).ok())
        .filter(|address| !address.is_empty())
}

/// How long the window manager waits for a renderer it started to say hello.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the window manager waits for the renderer to load gfxstream and answer
/// `Start`. Software Vulkan on a slow machine takes seconds.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);

/// How long the renderer waits for the window manager to say where a frame goes before it
/// drops the frame.
pub const TARGET_TIMEOUT: Duration = Duration::from_secs(1);

/// How long a question to the renderer (its memory) waits for the answer.
pub const QUESTION_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a renderer asked to stop has before it is ended.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

/// How the restarts of a renderer that keeps ending are spaced: the first comes at once,
/// each quick failure after it waits twice as long as the one before, up to
/// [`RestartBackoff::LONGEST`], and a renderer that ran for [`RestartBackoff::STABLE`] starts
/// the count again. A renderer that cannot start at all (a broken driver) is then tried
/// twice a minute rather than in a busy loop, and one that crashes once a day restarts at
/// once each time.
#[derive(Debug, Clone)]
pub struct RestartBackoff {
    failures: u32,
    started: Option<Instant>,
    restarts: u64,
}

impl Default for RestartBackoff {
    fn default() -> Self {
        RestartBackoff::new()
    }
}

impl RestartBackoff {
    /// The wait before the second restart in a row.
    pub const FIRST: Duration = Duration::from_millis(250);
    /// The longest wait between restarts.
    pub const LONGEST: Duration = Duration::from_secs(30);
    /// How long a renderer must run for its next end to count as the first again.
    pub const STABLE: Duration = Duration::from_secs(60);

    pub fn new() -> RestartBackoff {
        RestartBackoff {
            failures: 0,
            started: None,
            restarts: 0,
        }
    }

    /// The renderer started at `now`.
    pub fn started(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// The renderer ended (or could not start) at `now`: how long to wait before starting
    /// it again.
    pub fn ended(&mut self, now: Instant) -> Duration {
        let ran = self
            .started
            .take()
            .map(|started| now.saturating_duration_since(started));
        if ran.is_some_and(|ran| ran >= Self::STABLE) {
            self.failures = 0;
        }
        let wait = match self.failures {
            0 => Duration::ZERO,
            n => Self::FIRST
                .saturating_mul(1u32 << (n - 1).min(16))
                .min(Self::LONGEST),
        };
        self.failures = self.failures.saturating_add(1);
        self.restarts += 1;
        wait
    }

    /// How many times the renderer has been started again.
    pub fn restarts(&self) -> u64 {
        self.restarts
    }
}

/// Why a process ended, from its exit code, in words a log reader can act on. On Windows the
/// code is the process's exit status, which an exception that ended it sets (an `NTSTATUS`).
pub fn describe_exit_code(code: i64) -> String {
    let status = code as u32;
    let named = match status {
        0 => return "it exited normally (exit code 0)".into(),
        1 => {
            return "it was ended from outside (exit code 1, as Task Manager's End task \
                    leaves it)"
                .into()
        }
        3 => Some("it aborted"),
        0xC000_0005 => Some("it crashed: an access violation"),
        0xC000_001D => Some("it crashed: an illegal instruction"),
        0xC000_0094 => Some("it crashed: an integer division by zero"),
        0xC000_00FD => Some("it crashed: a stack overflow"),
        0xC000_0374 => Some("it crashed: the heap was corrupted"),
        0xC000_0409 => Some("it stopped itself on a fatal error (a fail-fast or abort)"),
        0xC000_013A => Some("it was ended by Ctrl+C or a sign-out"),
        0xC000_0142 => Some("it could not start: a library failed to initialise"),
        0xC000_0135 => Some("it could not start: a library it needs is missing"),
        _ => None,
    };
    match named {
        Some(what) => format!("{what} (exit code 0x{status:08X})"),
        None if !(0..=255).contains(&code) => format!("it exited with code 0x{status:08X}"),
        None => format!("it exited with code {code}"),
    }
}

/// Why a process ended, from the signal that ended it.
pub fn describe_signal(signal: i32) -> String {
    let what = match signal {
        4 => "it crashed: an illegal instruction (SIGILL)",
        5 => "it stopped on a trap (SIGTRAP)",
        6 => "it aborted (SIGABRT)",
        8 => "it crashed: an arithmetic error (SIGFPE)",
        9 => "it was killed (SIGKILL)",
        11 => "it crashed: a segmentation fault (SIGSEGV)",
        15 => "it was asked to end (SIGTERM)",
        _ => return format!("it was ended by signal {signal}"),
    };
    what.to_string()
}

/// The log line for a renderer that ended: who, why, and when it starts again.
pub fn restart_line(process_id: u32, reason: &str, wait: Duration, restarts: u64) -> String {
    let when = if wait.is_zero() {
        "starting it again now".to_string()
    } else {
        format!("starting it again in {:.1} s", wait.as_secs_f64())
    };
    format!(
        "the graphics renderer ({RENDERER_PROGRAM}, pid {process_id}) ended: {reason}; the \
         windows stay and keep their last frame; {when} (restart {restarts})"
    )
}

/// What a renderer process holds, as it reports itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RendererProcessMemory {
    pub process_id: u32,
    /// As the operating system counts the process.
    pub held: HeldMemory,
    /// The renderer's own count of its GPU allocations, when it gives one.
    pub counted: Option<RendererMemory>,
}

impl RendererProcessMemory {
    /// The memory a renderer's report gives.
    pub fn from_report(report: &MemoryReport) -> RendererProcessMemory {
        let known = |kb: u64| (kb > 0).then_some(kb);
        RendererProcessMemory {
            process_id: report.process_id,
            held: HeldMemory {
                working_set_kb: known(report.working_set_kb),
                private_kb: known(report.private_kb),
                gpu_local_kb: None,
                gpu_non_local_kb: None,
            },
            counted: report.counted.then_some(RendererMemory {
                device_local_kb: report.device_local_kb,
                device_local_count: report.device_local_count,
                host_visible_kb: report.host_visible_kb,
                host_visible_count: report.host_visible_count,
                imported_kb: report.imported_kb,
                imported_count: report.imported_count,
                largest_kb: report.largest_kb,
                guest_processes: report.guest_processes,
            }),
        }
    }

    /// The report the renderer sends for `request`.
    pub fn to_report(&self, request: u64) -> MemoryReport {
        let counted = self.counted.unwrap_or_default();
        MemoryReport {
            request,
            process_id: self.process_id,
            working_set_kb: self.held.working_set_kb.unwrap_or(0),
            private_kb: self.held.private_kb.unwrap_or(0),
            counted: self.counted.is_some(),
            device_local_kb: counted.device_local_kb,
            device_local_count: counted.device_local_count,
            host_visible_kb: counted.host_visible_kb,
            host_visible_count: counted.host_visible_count,
            imported_kb: counted.imported_kb,
            imported_count: counted.imported_count,
            largest_kb: counted.largest_kb,
            guest_processes: counted.guest_processes,
        }
    }
}

/// The log line that says what the renderer process holds, beside the window manager's own
/// breakdown, in MiB.
pub fn renderer_line(memory: Option<&RendererProcessMemory>) -> String {
    let Some(memory) = memory else {
        return format!(
            "graphics renderer ({RENDERER_PROGRAM}): not running or not answering, so its \
             memory is not known"
        );
    };
    let show = |kb: Option<u64>| kb.map_or("?".to_string(), |kb| mib(kb).to_string());
    let mut line = format!(
        "graphics renderer ({RENDERER_PROGRAM}, pid {}): working set {} MiB, private {} MiB",
        memory.process_id,
        show(memory.held.working_set_kb),
        show(memory.held.private_kb),
    );
    match &memory.counted {
        Some(r) => line.push_str(&format!(
            "; GPU-only {} MiB in {}, CPU-mapped {} MiB in {}, imported {} MiB in {}, largest {} \
             MiB, {} guest processes connected",
            mib(r.device_local_kb),
            r.device_local_count,
            mib(r.host_visible_kb),
            r.host_visible_count,
            mib(r.imported_kb),
            r.imported_count,
            mib(r.largest_kb),
            r.guest_processes,
        )),
        None => line.push_str("; its GPU allocations: no count"),
    }
    line
}

/// Answers that a thread waits for, by request number.
pub struct Pending<T> {
    next: AtomicU64,
    waiting: Mutex<HashMap<u64, Option<T>>>,
    arrived: Condvar,
    closed: AtomicBool,
}

impl<T> Default for Pending<T> {
    fn default() -> Self {
        Pending {
            next: AtomicU64::new(1),
            waiting: Mutex::new(HashMap::new()),
            arrived: Condvar::new(),
            closed: AtomicBool::new(false),
        }
    }
}

impl<T> Pending<T> {
    pub fn new() -> Pending<T> {
        Pending::default()
    }

    /// A new request number, whose answer [`Pending::wait`] then waits for.
    pub fn begin(&self) -> u64 {
        let request = self.next.fetch_add(1, Ordering::Relaxed);
        self.waiting
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request, None);
        request
    }

    /// The answer to `request`, or `None` when none came within `timeout` or the connection
    /// closed.
    pub fn wait(&self, request: u64, timeout: Duration) -> Option<T> {
        let deadline = Instant::now() + timeout;
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(Some(_)) = waiting.get(&request) {
                return waiting.remove(&request).flatten();
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() || self.closed.load(Ordering::Relaxed) {
                waiting.remove(&request);
                return None;
            }
            waiting = self
                .arrived
                .wait_timeout(waiting, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// The answer to `request` arrived. An answer nobody waits for (it came too late) is
    /// dropped.
    pub fn complete(&self, request: u64, answer: T) {
        let mut waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = waiting.get_mut(&request) {
            *slot = Some(answer);
            self.arrived.notify_all();
        }
    }

    /// The connection closed: everyone waiting gets `None` now, and so does every later
    /// wait.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        let _waiting = self.waiting.lock().unwrap_or_else(|e| e.into_inner());
        self.arrived.notify_all();
    }
}

/// The window manager's sending half of its connection to the current renderer.
#[derive(Clone)]
pub struct RendererLink {
    sender: Arc<Mutex<Option<Box<dyn FrameSender>>>>,
    process_id: u32,
}

impl RendererLink {
    fn new(sender: Option<Box<dyn FrameSender>>, process_id: u32) -> RendererLink {
        RendererLink {
            sender: Arc::new(Mutex::new(sender)),
            process_id,
        }
    }

    /// A link to nothing, which drops everything sent on it.
    pub fn detached() -> RendererLink {
        RendererLink::new(None, 0)
    }

    /// Sends `body` to the renderer. False when it is gone.
    pub fn send(&self, body: Body) -> bool {
        let mut sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        match sender.as_mut() {
            Some(sender) => send_on(&mut **sender, body).is_ok(),
            None => false,
        }
    }

    /// The renderer's process ID.
    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    fn close(&self) {
        *self.sender.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// A renderer process the window manager started.
pub trait RendererChild: Send {
    /// Its process ID.
    fn id(&self) -> u32;
    /// Why it ended, once it has; `None` while it runs. Never blocks.
    fn exited(&mut self) -> Option<String>;
    /// Ends it now.
    fn kill(&mut self);
}

/// A started renderer and the connection it dialled back on.
pub struct Launched {
    pub process: Box<dyn RendererChild>,
    pub connection: Box<dyn Connection>,
}

/// Starts the renderer process: the platform's part.
pub trait RendererLauncher: Send {
    /// Starts a renderer and waits for it to dial back. An error says why it could not.
    fn launch(&mut self) -> Result<Launched, String>;
}

/// What the window manager does with its renderer: the platform's part.
pub trait RendererHandler: Send + Sync {
    /// How a new renderer starts.
    fn start(&self) -> Start;
    /// A renderer started and answered `Start`; `link` sends to it from now on.
    fn connected(&self, started: &Started, link: &RendererLink);
    /// One message from the renderer. Messages arrive in order on one thread; an answer
    /// sent from here reaches the renderer before the next message is read.
    fn message(&self, body: Body, link: &RendererLink);
    /// The renderer is gone (it ended, or its connection broke).
    fn disconnected(&self);
    /// A line for the window manager's log.
    fn log(&self, line: &str);
    /// A renderer could not be started, or ended before it answered `Start`.
    fn start_failed(&self) {}
}

/// Waits up to `limit` for `stop`, in small steps. True when it was set.
fn sleep_unless(stop: &AtomicBool, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if stop.load(Ordering::Relaxed) {
            return true;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return false;
        }
        thread::sleep(left.min(Duration::from_millis(50)));
    }
}

/// Waits up to `limit` for `child` to end; ends it when it does not. Returns why it ended.
fn reap(child: &mut dyn RendererChild, limit: Duration) -> String {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(reason) = child.exited() {
            return reason;
        }
        if Instant::now() >= deadline {
            child.kill();
            let killed = Instant::now() + Duration::from_secs(5);
            while Instant::now() < killed {
                if let Some(reason) = child.exited() {
                    return format!("it stopped answering and was ended ({reason})");
                }
                thread::sleep(Duration::from_millis(20));
            }
            return "it stopped answering and could not be ended".into();
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// How one renderer's life ended.
enum Ending {
    /// The window manager is stopping.
    Stopping,
    /// The renderer ended or broke its connection.
    Ended,
}

/// Runs one started renderer: the handshake, `Start`, and its messages until it ends.
fn run_one(
    launched: &mut Launched,
    handler: &dyn RendererHandler,
    build: &str,
    stop: &AtomicBool,
    backoff: &mut RestartBackoff,
) -> Result<Ending, String> {
    let connection = &mut launched.connection;
    window_manager_accept(
        &mut **connection,
        &local_hello(Role::WindowManager, build),
        HELLO_TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    send(&mut **connection, Body::Start(handler.start())).map_err(|e| e.to_string())?;
    let started = wait_started(&mut **connection, &mut *launched.process, handler, stop)?;
    let Some(started) = started else {
        return Ok(Ending::Stopping);
    };
    if !started.error.is_empty() {
        return Err(format!("it could not start: {}", started.error));
    }
    backoff.started(Instant::now());
    let link = RendererLink::new(connection.try_clone_sender(), launched.process.id());
    handler.connected(&started, &link);
    let ending = loop {
        if stop.load(Ordering::Relaxed) {
            link.send(Body::Stop(Stop {}));
            break Ending::Stopping;
        }
        match connection.recv(Some(Duration::from_millis(100))) {
            Ok(frame) => match decode_body(&frame) {
                Ok(body) => {
                    // A failure in the platform's part must not end the supervision; the
                    // message is lost.
                    let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        handler.message(body, &link)
                    }));
                    if handled.is_err() {
                        handler.log("a message from the graphics renderer could not be handled");
                    }
                }
                Err(why) => {
                    handler.log(&format!(
                        "the graphics renderer sent something that is not a message ({why}); \
                         it is ended"
                    ));
                    break Ending::Ended;
                }
            },
            Err(TransportError::Timeout) => {
                if launched.process.exited().is_some() {
                    break Ending::Ended;
                }
            }
            Err(_) => break Ending::Ended,
        }
    };
    link.close();
    handler.disconnected();
    Ok(ending)
}

fn send(connection: &mut dyn Connection, body: Body) -> Result<(), TransportError> {
    dsa_protocol::renderer::send(connection, body)
}

/// Waits for the renderer's `Started`. `None` when the window manager is stopping.
fn wait_started(
    connection: &mut dyn Connection,
    child: &mut dyn RendererChild,
    handler: &dyn RendererHandler,
    stop: &AtomicBool,
) -> Result<Option<Started>, String> {
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(None);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "it did not start within {} s",
                START_TIMEOUT.as_secs()
            ));
        }
        match connection.recv(Some(Duration::from_millis(200))) {
            Ok(frame) => match decode_body(&frame) {
                Ok(Body::Started(started)) => return Ok(Some(started)),
                // Log lines may come while gfxstream starts; anything else is out of order.
                Ok(body @ (Body::RendererLog(_) | Body::Note(_))) => {
                    handler.message(body, &RendererLink::detached());
                }
                Ok(_) => return Err("it sent another message before Started".into()),
                Err(why) => return Err(format!("it sent something that is not a message: {why}")),
            },
            Err(TransportError::Timeout) => {
                if child.exited().is_some() {
                    return Err("it ended while it started".into());
                }
            }
            Err(error) => return Err(format!("its connection broke while it started: {error}")),
        }
    }
}

/// Keeps a renderer running until `stop` is set: starts it through `launcher`, hands it to
/// `handler`, and starts it again whenever it ends, logging why. Returns once `stop` is set
/// and the renderer has ended.
pub fn supervise(
    launcher: &mut dyn RendererLauncher,
    handler: &dyn RendererHandler,
    build: &str,
    stop: &AtomicBool,
) {
    let mut backoff = RestartBackoff::new();
    while !stop.load(Ordering::Relaxed) {
        let mut launched = match launcher.launch() {
            Ok(launched) => launched,
            Err(why) => {
                handler.start_failed();
                let wait = backoff.ended(Instant::now());
                handler.log(&format!(
                    "the graphics renderer ({RENDERER_PROGRAM}) could not be started: {why}; \
                     trying again in {:.1} s",
                    wait.as_secs_f64()
                ));
                if sleep_unless(stop, wait.max(RestartBackoff::FIRST)) {
                    return;
                }
                continue;
            }
        };
        let process_id = launched.process.id();
        let outcome = run_one(&mut launched, handler, build, stop, &mut backoff);
        let stopping = matches!(outcome, Ok(Ending::Stopping));
        if stopping {
            launched.connection.close();
            let reason = reap(&mut *launched.process, STOP_GRACE);
            handler.log(&format!(
                "the graphics renderer ({RENDERER_PROGRAM}, pid {process_id}) stopped: {reason}"
            ));
            return;
        }
        // The renderer may still be writing its last words; it gets a moment to end by itself
        // before it is ended.
        let reason = match outcome {
            Err(why) => {
                handler.start_failed();
                launched.connection.close();
                let ended = reap(&mut *launched.process, Duration::from_secs(1));
                format!("{why}; {ended}")
            }
            Ok(_) => {
                launched.connection.close();
                reap(&mut *launched.process, Duration::from_secs(2))
            }
        };
        let wait = backoff.ended(Instant::now());
        handler.log(&restart_line(process_id, &reason, wait, backoff.restarts()));
        if sleep_unless(stop, wait) {
            return;
        }
    }
}

/// Runs [`supervise`] on a thread of its own, which ends after `stop` is set.
pub fn spawn_supervisor(
    mut launcher: Box<dyn RendererLauncher>,
    handler: Arc<dyn RendererHandler>,
    build: String,
    stop: Arc<AtomicBool>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("dsa-wm-renderer".into())
        .spawn(move || supervise(&mut *launcher, &*handler, &build, &stop))
}

/// How many textures one display keeps on each side: a display may take turns between two
/// (one shown while the next is written), and a resize brings new ones.
pub const TEXTURES_PER_DISPLAY: usize = 4;

/// Which textures each display was given, on the window manager's side: a texture crosses
/// to the renderer once, and a frame into the same texture names it by its ID. `H` is the
/// platform's texture handle in the window manager's process.
#[derive(Debug)]
pub struct TextureBook<H> {
    given: HashMap<u32, Vec<(H, u64)>>,
    last: HashMap<u32, u64>,
    next: u64,
}

impl<H> Default for TextureBook<H> {
    fn default() -> Self {
        TextureBook {
            given: HashMap::new(),
            last: HashMap::new(),
            next: 1,
        }
    }
}

/// What a display's next frame goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Given {
    /// No window shows the display: the frame is dropped.
    Drop,
    /// A texture the renderer holds, by its ID.
    Same(u64),
    /// A texture the renderer does not hold: it must be handed over, with this new ID.
    New(u64),
}

impl<H: PartialEq + Copy> TextureBook<H> {
    pub fn new() -> TextureBook<H> {
        TextureBook::default()
    }

    /// `display_id`'s next frame goes into `handle` (`None`: nowhere). `held` are the
    /// textures the renderer says it holds for the display: one it does not hold goes over
    /// again, under a new ID.
    pub fn give(&mut self, display_id: u32, handle: Option<H>, held: &[u64]) -> Given {
        let Some(handle) = handle else {
            self.withdraw(display_id);
            return Given::Drop;
        };
        let list = self.given.entry(display_id).or_default();
        if let Some(at) = list.iter().position(|(given, _)| *given == handle) {
            let texture_id = list[at].1;
            if held.contains(&texture_id) {
                self.last.insert(display_id, texture_id);
                return Given::Same(texture_id);
            }
            list.remove(at);
        }
        let texture_id = self.next;
        self.next += 1;
        list.push((handle, texture_id));
        if list.len() > TEXTURES_PER_DISPLAY {
            list.remove(0);
        }
        self.last.insert(display_id, texture_id);
        Given::New(texture_id)
    }

    /// The texture `display_id`'s last frame was given, by its ID.
    pub fn texture_of(&self, display_id: u32) -> Option<u64> {
        self.last.get(&display_id).copied()
    }

    /// `display_id` has no textures now (its window closed, or a handover failed).
    pub fn withdraw(&mut self, display_id: u32) {
        self.given.remove(&display_id);
        self.last.remove(&display_id);
    }

    /// A new renderer has none of the textures: each goes over again with its next frame.
    pub fn clear(&mut self) {
        self.given.clear();
        self.last.clear();
    }
}

/// The textures the renderer holds for each display, on the renderer's side. `H` is the
/// texture as the renderer opened it (a handle, a surface).
#[derive(Debug)]
pub struct HeldTargets<H> {
    held: HashMap<u32, Vec<(u64, H)>>,
    last: HashMap<u32, u64>,
    keep: usize,
}

impl<H> Default for HeldTargets<H> {
    fn default() -> Self {
        HeldTargets {
            held: HashMap::new(),
            last: HashMap::new(),
            keep: TEXTURES_PER_DISPLAY,
        }
    }
}

impl<H: Copy> HeldTargets<H> {
    pub fn new() -> HeldTargets<H> {
        HeldTargets::default()
    }

    /// Holds at most `keep` textures per display (at least one). A window manager that gives
    /// each display one texture at a time, and a new one only when the display outgrows it,
    /// has the renderer keep one: an older texture it holds would stay alive in the renderer
    /// with nothing to use it, a whole buffer of the window's last size for every resize.
    pub fn keeping(keep: usize) -> HeldTargets<H> {
        HeldTargets {
            keep: keep.max(1),
            ..HeldTargets::default()
        }
    }

    /// The IDs of the textures held for `display_id`, for the next request.
    pub fn held(&self, display_id: u32) -> Vec<u64> {
        self.held
            .get(&display_id)
            .map(|list| list.iter().map(|(texture_id, _)| *texture_id).collect())
            .unwrap_or_default()
    }

    /// Takes the window manager's answer for `display_id`'s next frame. `open` opens a
    /// texture the renderer does not hold yet. Returns the texture to write into, if any, and
    /// those to let go of now (after the new one was opened, so two never share a handle
    /// value).
    pub fn take(
        &mut self,
        display_id: u32,
        target: &Target,
        open: impl FnOnce(&Target) -> Option<H>,
    ) -> (Option<H>, Vec<H>) {
        if target.drop {
            return (None, self.forget(display_id));
        }
        let list = self.held.entry(display_id).or_default();
        if let Some((_, held)) = list.iter().find(|(id, _)| *id == target.texture_id) {
            let held = *held;
            self.last.insert(display_id, target.texture_id);
            return (Some(held), Vec::new());
        }
        let Some(opened) = open(target) else {
            return (None, Vec::new());
        };
        list.push((target.texture_id, opened));
        let mut released = Vec::new();
        while list.len() > self.keep {
            released.push(list.remove(0).1);
        }
        self.last.insert(display_id, target.texture_id);
        (Some(opened), released)
    }

    /// The texture `display_id`'s last frame went into, with its ID.
    pub fn current(&self, display_id: u32) -> Option<(u64, H)> {
        let last = *self.last.get(&display_id)?;
        self.held
            .get(&display_id)?
            .iter()
            .find(|(texture_id, _)| *texture_id == last)
            .copied()
    }

    /// `display_id` is shown nowhere now: its textures, to let go of.
    pub fn forget(&mut self, display_id: u32) -> Vec<H> {
        self.last.remove(&display_id);
        self.held
            .remove(&display_id)
            .map(|list| list.into_iter().map(|(_, held)| held).collect())
            .unwrap_or_default()
    }

    /// Every texture held, to let go of.
    pub fn drain(&mut self) -> Vec<H> {
        self.last.clear();
        self.held
            .drain()
            .flat_map(|(_, list)| list.into_iter().map(|(_, held)| held))
            .collect()
    }
}

/// The renderer's side of its connection to the window manager.
pub struct WindowManagerLink {
    sender: Mutex<Box<dyn FrameSender>>,
    targets: Pending<Target>,
    gone: AtomicBool,
}

impl WindowManagerLink {
    /// The link that sends on `sender`.
    pub fn new(sender: Box<dyn FrameSender>) -> Arc<WindowManagerLink> {
        Arc::new(WindowManagerLink {
            sender: Mutex::new(sender),
            targets: Pending::new(),
            gone: AtomicBool::new(false),
        })
    }

    /// Sends `body`. False when the window manager is gone.
    pub fn send(&self, body: Body) -> bool {
        if self.gone.load(Ordering::Relaxed) {
            return false;
        }
        let mut sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        let sent = send_on(&mut **sender, body).is_ok();
        if !sent {
            self.gone.store(true, Ordering::Relaxed);
            self.targets.close();
        }
        sent
    }

    /// Asks where display `display_id`'s new frame of `width` x `height` goes, and waits for
    /// the answer. `None` when the window manager did not answer in time or is gone; the
    /// frame is then dropped.
    /// `held` are the textures the renderer holds for the display.
    pub fn target(
        &self,
        display_id: u32,
        width: u32,
        height: u32,
        exact: bool,
        held: Vec<u64>,
    ) -> Option<Target> {
        let request = self.targets.begin();
        let asked = self.send(Body::TargetRequest(TargetRequest {
            request,
            display_id,
            width,
            height,
            exact,
            held_textures: held,
        }));
        if !asked {
            return None;
        }
        self.targets.wait(request, TARGET_TIMEOUT)
    }

    /// A line for the window manager's log.
    pub fn note(&self, text: &str) {
        self.send(Body::Note(Note {
            text: text.to_string(),
        }));
    }

    /// One of gfxstream's lines, `severity` being its `W`, `E` or `F`.
    pub fn renderer_log(&self, severity: u8, message: &str) {
        self.send(Body::RendererLog(RendererLog {
            severity: u32::from(severity),
            message: message.to_string(),
        }));
    }

    /// Takes a message from the window manager: an answer to a frame's request goes to the
    /// thread that waits for it, and anything else comes back for the caller to act on.
    pub fn take(&self, body: Body) -> Option<Body> {
        match body {
            Body::Target(target) => {
                self.targets.complete(target.request, target);
                None
            }
            other => Some(other),
        }
    }

    /// The window manager is gone: nothing waits for it any more.
    pub fn close(&self) {
        self.gone.store(true, Ordering::Relaxed);
        self.targets.close();
    }

    /// Whether the window manager is gone.
    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Relaxed)
    }
}

/// How often the renderer process looks whether its Vulkan device was lost or made again.
/// Reading the two counts costs two atomic loads, and the window manager hears of a GPU reset
/// within this of the renderer finding it.
pub const DEVICE_STATE_POLL: Duration = Duration::from_millis(50);

/// What the window manager was last told of the renderer's device, so a change is sent once.
///
/// A GPU reset lost the renderer's device 13 s before the window manager's own Direct3D
/// device failed on the owner's PC: the window manager read the renderer's count only when
/// both ran in one process. The counts now cross to the window manager the moment the
/// renderer finds the loss.
#[derive(Debug, Default)]
pub struct DeviceWatch {
    told: (u64, u64),
}

impl DeviceWatch {
    pub fn new() -> DeviceWatch {
        DeviceWatch::default()
    }

    /// The renderer found its device lost `losses` times and made it again `generation`
    /// times: what to send, or `None` when the window manager knows it already. Nothing is
    /// sent for a renderer that never lost its device.
    pub fn look(&mut self, losses: u64, generation: u64) -> Option<DeviceState> {
        let now = (losses, generation);
        let before = std::mem::replace(&mut self.told, now);
        (now != before).then_some(DeviceState { losses, generation })
    }
}

/// The window manager's line for a renderer's DeviceState.
pub fn describe_device_state(state: &DeviceState) -> String {
    if state.losses > state.generation {
        format!(
            "the graphics renderer found its GPU device lost (loss {}); task windows are kept \
             while it makes a new one, and the guest's calls on the lost one fail at once",
            state.losses
        )
    } else {
        format!(
            "the graphics renderer made its GPU device again (number {} since it started)",
            state.generation
        )
    }
}

/// What a window manager keeps of its renderer's DeviceState messages: a count that changes
/// with each one, which the window manager follows as a GPU reset. It is kept across renderer
/// restarts, whose own counts start from zero again, so it never goes back to a value already
/// seen.
#[derive(Debug, Default)]
pub struct DeviceChanges(std::sync::atomic::AtomicU64);

impl DeviceChanges {
    pub fn new() -> DeviceChanges {
        DeviceChanges::default()
    }

    /// The renderer sent `state`: the count changes, and the line for the log is returned.
    pub fn told(&self, state: &DeviceState) -> String {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        describe_device_state(state)
    }

    /// The count: any change since the last look is a GPU reset.
    pub fn count(&self) -> u64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// One number for the renderer's device that goes up the moment the renderer finds it lost
/// and again when it has made it again: the window manager takes any change as a GPU reset.
pub fn device_resets(state: &DeviceState) -> u64 {
    state.losses.saturating_add(state.generation)
}

/// Tells the window manager on `link` of the renderer's device, as `read` gives its counts
/// (losses, then generation), every [`DEVICE_STATE_POLL`] until the window manager is gone.
pub fn follow_device_state(
    link: Arc<WindowManagerLink>,
    read: Box<dyn Fn() -> (u64, u64) + Send>,
) -> std::io::Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("dsa-gfx-device-state".into())
        .spawn(move || {
            let mut watch = DeviceWatch::new();
            while !link.is_gone() {
                let (losses, generation) = read();
                if let Some(state) = watch.look(losses, generation) {
                    link.send(Body::DeviceState(state));
                }
                thread::sleep(DEVICE_STATE_POLL);
            }
        })
}

/// The renderer's side of the handshake: says hello on `connection` as `build`, and waits for
/// the window manager's `Start`.
pub fn open_renderer(connection: &mut dyn Connection, build: &str) -> Result<Start, String> {
    renderer_connect(
        connection,
        &local_hello(Role::Renderer, build),
        HELLO_TIMEOUT,
    )
    .map_err(|e| e.to_string())?;
    loop {
        let frame = connection
            .recv(Some(START_TIMEOUT))
            .map_err(|e| format!("waiting for the window manager's Start: {e}"))?;
        match decode_body(&frame) {
            Ok(Body::Start(start)) => return Ok(start),
            Ok(Body::Stop(_)) => return Err("the window manager stopped before the start".into()),
            Ok(_) => {}
            Err(why) => return Err(format!("the window manager sent no Start: {why}")),
        }
    }
}

/// The window manager's GPU request, as `Start` carries it: the rule, the LUID (8 bytes) and
/// the name.
pub fn gpu_request_of(start: &Start) -> (i32, Option<[u8; 8]>, String) {
    let luid = <[u8; 8]>::try_from(start.gpu_luid.as_slice()).ok();
    (start.gpu_rule, luid, start.gpu_name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr5_restarts_come_at_once_then_wait_longer_up_to_a_limit() {
        let mut backoff = RestartBackoff::new();
        let t = Instant::now();
        assert_eq!(backoff.ended(t), Duration::ZERO);
        assert_eq!(backoff.ended(t), RestartBackoff::FIRST);
        assert_eq!(backoff.ended(t), RestartBackoff::FIRST * 2);
        for _ in 0..20 {
            backoff.ended(t);
        }
        assert_eq!(backoff.ended(t), RestartBackoff::LONGEST);
        assert_eq!(backoff.restarts(), 24);
    }

    #[test]
    fn nfr5_the_window_manager_is_told_of_a_lost_device_at_once_and_once() {
        let mut watch = DeviceWatch::new();
        // A renderer that never lost its device says nothing.
        assert_eq!(watch.look(0, 0), None);
        assert_eq!(watch.look(0, 0), None);
        // The loss is told the moment the renderer finds it, before the device is made again.
        let lost = watch.look(1, 0).expect("the loss is told");
        assert_eq!((lost.losses, lost.generation), (1, 0));
        assert_eq!(watch.look(1, 0), None);
        // And the new device once it is made.
        let made = watch.look(1, 1).expect("the new device is told");
        assert_eq!((made.losses, made.generation), (1, 1));
        assert_eq!(watch.look(1, 1), None);
    }

    #[test]
    fn nfr5_each_loss_and_each_new_device_changes_what_the_window_manager_follows() {
        let counts = [(0, 0), (1, 0), (1, 1), (2, 1), (2, 2)];
        let resets: Vec<u64> = counts
            .iter()
            .map(|&(losses, generation)| device_resets(&DeviceState { losses, generation }))
            .collect();
        assert!(resets.windows(2).all(|pair| pair[1] > pair[0]));
    }

    #[test]
    fn nfr5_every_device_state_a_renderer_sends_is_a_change_the_window_manager_follows() {
        let changes = DeviceChanges::new();
        let mut seen = vec![changes.count()];
        // A loss, the new device, then a restarted renderer that counts from zero again.
        for (losses, generation) in [(1, 0), (1, 1), (1, 0), (1, 1)] {
            let line = changes.told(&DeviceState { losses, generation });
            assert!(line.starts_with("the graphics renderer "), "{line}");
            seen.push(changes.count());
        }
        assert!(seen.windows(2).all(|pair| pair[1] > pair[0]));
        let lost = describe_device_state(&DeviceState {
            losses: 3,
            generation: 2,
        });
        assert!(
            lost.contains("lost (loss 3); task windows are kept while"),
            "{lost}"
        );
    }

    #[test]
    fn nfr5_the_device_state_is_looked_at_well_within_androids_input_timeout() {
        // Android ends an app whose input goes unanswered for five seconds; the window
        // manager must know of the reset long before.
        assert!(DEVICE_STATE_POLL <= Duration::from_millis(100));
    }

    #[test]
    fn nfr5_a_renderer_that_ran_long_restarts_at_once_again() {
        let mut backoff = RestartBackoff::new();
        let t = Instant::now();
        backoff.ended(t);
        backoff.ended(t);
        backoff.started(t);
        assert_eq!(
            backoff.ended(t + RestartBackoff::STABLE + Duration::from_secs(1)),
            Duration::ZERO
        );
    }
}
