//! The session: the guest container's life from start to stop, as the host sees it.
//!
//! A [`Session`] owns a worker thread. The thread starts the guest through a
//! [`GuestLauncher`], waits for the agent to connect on the control channel, checks the
//! connection with a [`Handshake`], and reports every state change. Requests return at
//! once; callers watch the state with [`Session::state`], [`Session::wait_for`] or
//! [`Session::events`].
//!
//! While the guest is up, the session keeps the latest state of the guest's app tasks from
//! the task messages the guest sends, and hands every change to [`Session::watch_tasks`]
//! subscribers. When the guest goes away, every task it had is removed.
//!
//! It also keeps the guest's last memory report ([`Session::guest_memory`]), which the agent
//! sends every few minutes and when the host asks ([`Session::request_guest_memory`]).
//!
//! The states are those of the lifecycle: `Stopped`, `Starting`, `Active`, `Dozing`,
//! `Frozen` and `Stopping`, and `Stalled` for a guest that runs but has taken too long to be
//! ready.
//!
//! A connection on the control channel says only that the guest's kernel and init run: its
//! daemon connects long before Android has finished starting. So the session stays `Starting`
//! until the daemon reports that the guest has finished booting and that the agent is connected
//! ([`control::GuestBoot`]). A guest that does not get there in time is `Stalled`, with a
//! [`BootProblem`] saying why; the guest keeps running, because the cause may go away, and the
//! session moves on by itself when it does. What the guest cannot run without at all
//! ([`GuestLauncher::prerequisite`]) is waited for before the guest starts, so a guest never
//! crash-loops for want of it; lost for good while the guest runs, it stops the guest.
//!
//! While the guest is up the session follows the lifecycle:
//!
//! - Once no task window has been shown ([`TaskRequest::WindowsShown`]) for
//!   [`SessionConfig::doze_delay`], the session tells the guest its screen is off. The agent
//!   puts the device into Doze and says so, and the session becomes `Dozing`.
//! - As soon as a window is shown, the session tells the guest its screen is on and becomes
//!   `Active` at once, without waiting for the guest.
//! - Freezing stops the guest's processes through the launcher. It happens only when asked
//!   for ([`Session::freeze`]) or, when [`SessionConfig::freeze_after`] is set, after the
//!   session has been `Dozing` that long; by default it never happens. A shown window thaws
//!   the guest and wakes it.
//! - The host's power ([`Session::set_power`]) goes to the guest whenever it changes and
//!   each time the agent connects.
//!
//! The session also keeps the host's and the guest's text clipboards in step while sharing is
//! on ([`Session::share_clipboard`], on by default). Text the host copies
//! ([`Session::host_clipboard`]) goes to the guest while it is `Active` or `Dozing`, and the
//! host's latest text goes to it again each time it gets there, as when Android starts. Text
//! the guest copies goes to every [`Session::watch_clipboard`] watcher. Text that came from one
//! side is never sent back to it, and text longer than
//! [`MAX_CLIPBOARD_BYTES`](crate::clipboard::MAX_CLIPBOARD_BYTES) does not cross.
//!
//! APKs from the host go to the guest with [`Session::install_apk`], streamed in chunks paced
//! by the agent's progress reports, one install at a time; a second waits for the first.

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body};
use dsa_transport::{Connection, FrameSender, Listener, TransportError};

use crate::apk::{self, InstallOutcome};
use crate::clipboard;
use crate::ime::{ImeCursor, ImeEvent};
use crate::input::PointerEvent;
use crate::keys::KeyEvent;
use crate::memory::{GuestMemory, GuestMemoryReport};
use crate::power::PowerState;
use crate::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest, TaskTable, TaskWatch};

/// Where the session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// No guest is running.
    Stopped,
    /// The guest is being started, and has not yet finished booting with its agent connected.
    Starting,
    /// The guest has finished booting and the agent is connected.
    Active,
    /// No task window is visible and the guest is in Doze.
    Dozing,
    /// The guest's processes are frozen.
    Frozen,
    /// The guest is being stopped.
    Stopping,
    /// The guest runs but is not ready and has taken too long; [`Session::problem`] says why.
    /// The session leaves this state by itself when the cause goes away.
    Stalled,
}

impl fmt::Display for SessionState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// Why a session is not running, or why stopping it went wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    /// Someone asked for the session to stop.
    Requested,
    /// The launcher could not start the guest.
    GuestFailedToStart(String),
    /// The agent did not connect within the configured time. `diagnostics` is what the
    /// launcher could tell about the guest's boot, such as the end of its console output.
    AgentDidNotConnect {
        waited: Duration,
        diagnostics: Option<String>,
    },
    /// The guest ended by itself before the agent connected. `why` is the launcher's account
    /// of it and `diagnostics` what it could tell about the boot.
    GuestExited {
        why: String,
        diagnostics: Option<String>,
    },
    /// The agent connected but the handshake refused it.
    HandshakeRejected(String),
    /// The host could not listen for the agent.
    ListenerFailed(String),
    /// The control channel was lost while the session was active.
    ChannelLost(String),
    /// Stopping the guest failed and some of its processes may still run. The session stays
    /// in `Stopping`; asking to stop again retries.
    StopFailed(String),
    /// What the guest cannot run without was lost while it ran and did not come back. The
    /// guest was stopped so that it does not crash-loop; a new start waits for it.
    PrerequisiteLost(String),
}

impl fmt::Display for StopReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StopReason::Requested => write!(f, "stopped on request"),
            StopReason::GuestFailedToStart(why) => write!(f, "the guest did not start: {why}"),
            StopReason::AgentDidNotConnect {
                waited,
                diagnostics,
            } => {
                write!(
                    f,
                    "the agent did not connect to the control channel within {:.1} s",
                    waited.as_secs_f64()
                )?;
                write_diagnostics(f, diagnostics)
            }
            StopReason::GuestExited { why, diagnostics } => {
                write!(f, "{why}")?;
                write_diagnostics(f, diagnostics)
            }
            StopReason::HandshakeRejected(why) => {
                write!(f, "the agent's connection was refused: {why}")
            }
            StopReason::ListenerFailed(why) => {
                write!(f, "could not listen for the agent: {why}")
            }
            StopReason::ChannelLost(why) => write!(f, "the control channel was lost: {why}"),
            StopReason::StopFailed(why) => write!(f, "the guest did not stop: {why}"),
            StopReason::PrerequisiteLost(why) => {
                write!(
                    f,
                    "Android was stopped so that it does not keep failing: {why}"
                )
            }
        }
    }
}

/// What keeps a guest from being ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootProblem {
    /// What the guest cannot run without is missing ([`GuestLauncher::prerequisite`]). Not the
    /// guest's fault, so an image on trial is not blamed for it.
    Prerequisite(String),
    /// The guest has not finished booting. `detail` is what the launcher can tell about it.
    BootUnfinished {
        waited: Duration,
        detail: Option<String>,
    },
    /// The guest has finished booting, but its agent is not connected.
    AgentUnreachable {
        waited: Duration,
        detail: Option<String>,
    },
    /// The same, and what the guest printed shows the disk was made under other signing keys
    /// ([`GuestLauncher::agent_refused_for_keys`]). The disk is the cause, whichever image runs.
    DiskFromOtherKeys {
        waited: Duration,
        detail: Option<String>,
    },
}

impl BootProblem {
    fn same_kind(&self, other: &BootProblem) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

impl fmt::Display for BootProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BootProblem::Prerequisite(why) => f.write_str(why),
            BootProblem::BootUnfinished { waited, detail } => {
                write!(
                    f,
                    "Android has not finished booting after {} s, and keeps trying",
                    waited.as_secs()
                )?;
                write_diagnostics(f, detail)
            }
            BootProblem::AgentUnreachable { waited, detail }
            | BootProblem::DiskFromOtherKeys { waited, detail } => {
                write!(
                    f,
                    "Android finished booting, but its agent has not reached the control channel \
                     for {} s, so there are no app windows, no memory reports and no Doze",
                    waited.as_secs()
                )?;
                write_diagnostics(f, detail)
            }
        }
    }
}

/// Which delay a launcher is asked to explain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestStall {
    BootUnfinished,
    AgentUnreachable,
}

/// Puts a launcher's diagnostics on the lines after a reason, so they can be copied whole.
fn write_diagnostics(f: &mut fmt::Formatter<'_>, diagnostics: &Option<String>) -> fmt::Result {
    match diagnostics.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => write!(f, "\n{text}"),
        _ => Ok(()),
    }
}

/// A state change, as delivered to [`Session::events`] subscribers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEvent {
    pub state: SessionState,
    /// Set when the change has a cause worth reporting: always on entering `Stopped`, and on
    /// a failed stop.
    pub reason: Option<StopReason>,
}

/// Starts and stops the guest container. Implemented per platform.
pub trait GuestLauncher: Send + 'static {
    /// Starts the container. Returns once it has been started; the agent connects later.
    ///
    /// `secret` is new for this start. The launcher hands it to the guest where only the
    /// guest's control daemon can read it, never where an app could, and keeps no copy.
    fn start(&mut self, secret: &BootSecret) -> Result<(), String>;

    /// Stops the container. Returns `Ok` only once every process in it has ended, and an
    /// error if that did not happen within `timeout`.
    fn stop(&mut self, timeout: Duration) -> Result<(), String>;

    /// Whether the guest has ended by itself since it was started, as a sentence saying so.
    /// The session asks while it waits for the agent, so a guest that dies during boot ends
    /// the start at once instead of after the whole connect timeout. It must not block.
    fn exited(&mut self) -> Option<String> {
        None
    }

    /// What the launcher can tell about the last boot, for a start that failed: for example
    /// the last lines the guest printed and where the rest is kept. The session asks after
    /// the guest has been stopped and puts the answer in the reason it reports.
    fn diagnostics(&mut self) -> Option<String> {
        None
    }

    /// The runtime ID of the guest, if running (such as the VM GUID on Windows).
    fn vm_id(&self) -> Option<String> {
        None
    }

    /// What the guest cannot run without and is missing now, as a sentence saying so and what
    /// to do; `None` when nothing is. The launcher tries to provide it each time it is asked.
    /// The session asks before it starts the guest, and about once a second while the guest
    /// runs, so it must not take long.
    fn prerequisite(&mut self) -> Option<String> {
        None
    }

    /// What the launcher can tell about a running guest that is late, such as what its console
    /// shows and what to do. It must not take long.
    fn explain_stall(&mut self, stall: GuestStall) -> Option<String> {
        let _ = stall;
        None
    }

    /// Whether what the guest printed shows its agent was refused the control channel because
    /// its data disk was made under other signing keys. It must not take long.
    fn agent_refused_for_keys(&mut self) -> bool {
        false
    }

    /// Stops every process of the running guest where it is, keeping its memory, until
    /// [`thaw`](Self::thaw). The session asks only when freezing was chosen. A launcher that
    /// cannot freeze its guest says so, and the guest stays as it was.
    fn freeze(&mut self) -> Result<(), String> {
        Err("this kind of guest cannot be frozen".into())
    }

    /// Lets a frozen guest's processes run again.
    fn thaw(&mut self) -> Result<(), String> {
        Ok(())
    }
}

/// Checks a new control channel connection before the session trusts it.
///
/// The protocol implements this: it exchanges the protocol version and schema hash,
/// refuses a peer built from a different definition, and has the peer prove it knows the
/// secret the launcher gave this boot. It must bound its own waiting.
pub trait Handshake: Send + 'static {
    fn accept(
        &mut self,
        connection: &mut dyn Connection,
        secret: &BootSecret,
    ) -> Result<(), Refusal>;
}

/// Why a handshake refused a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The guest's daemon cannot work with this host, for example because it was built
    /// from a different protocol definition. Waiting longer cannot help, so the start fails.
    Incompatible(String),
    /// The connection is not the guest's daemon: it did not prove it knows this boot's
    /// secret, or it broke off. Any app in the guest can open such a connection, so it is
    /// closed and the session goes on waiting for the daemon.
    NotTheGuest(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::Incompatible(why) | Refusal::NotTheGuest(why) => f.write_str(why),
        }
    }
}

/// Timing for a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionConfig {
    /// How long to wait for the guest's daemon to connect after the guest starts.
    pub connect_timeout: Duration,
    /// How long the guest may take to finish booting before the session is `Stalled`.
    pub ready_timeout: Duration,
    /// How long a booted guest may be without its agent before the session is `Stalled`.
    pub agent_timeout: Duration,
    /// How long a prerequisite may be missing, while it may still be on its way, before the
    /// session is `Stalled`.
    pub prerequisite_grace: Duration,
    /// How long a running guest may be without a prerequisite before it is stopped.
    pub prerequisite_stop_after: Duration,
    /// How long the launcher may take to end every guest process.
    pub stop_timeout: Duration,
    /// How long no task window may be shown before the guest's screen goes off and the
    /// guest goes into Doze.
    pub doze_delay: Duration,
    /// How long the guest stays in Doze before it is frozen. `None`, the default, never
    /// freezes it on its own: a frozen guest gets no notifications, alarms or widget
    /// updates.
    pub freeze_after: Option<Duration>,
}

/// The default of [`SessionConfig::doze_delay`].
pub const DEFAULT_DOZE_DELAY: Duration = Duration::from_secs(30);

/// How long the guest stays in Doze before it is frozen, once the user has chosen freezing
/// and nothing else says how long.
pub const DEFAULT_FREEZE_AFTER: Duration = Duration::from_secs(30 * 60);

/// The default of [`SessionConfig::ready_timeout`]. A first boot on a new disk compiles the
/// apps' code, which takes minutes, so it is long; going past it only reports the delay.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(300);

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            connect_timeout: Duration::from_secs(120),
            ready_timeout: DEFAULT_READY_TIMEOUT,
            agent_timeout: Duration::from_secs(60),
            prerequisite_grace: Duration::from_secs(30),
            prerequisite_stop_after: Duration::from_secs(120),
            stop_timeout: Duration::from_secs(30),
            doze_delay: DEFAULT_DOZE_DELAY,
            freeze_after: None,
        }
    }
}

/// Why a request to the session could not be carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// The request needs an active session.
    NotActive(SessionState),
    /// Sending on the control channel failed.
    Transport(TransportError),
    /// The session's worker has ended.
    Gone,
    /// The guest could not be frozen; the detail says why.
    FreezeFailed(String),
    /// An APK was not taken, or did not reach the guest whole; the detail says why.
    InstallRefused(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::NotActive(state) => write!(f, "the session is {state}, not Active"),
            SessionError::Transport(error) => write!(f, "control channel: {error}"),
            SessionError::Gone => write!(f, "the session has shut down"),
            SessionError::FreezeFailed(why) => write!(f, "the guest was not frozen: {why}"),
            SessionError::InstallRefused(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SessionError {}

enum Command {
    Start,
    Stop,
    Send(Vec<u8>, Sender<Result<(), SessionError>>),
    Shutdown,
    /// Whether any task window is shown.
    WindowsShown(bool),
    /// The host's power changed.
    Power(PowerState),
    /// Freeze the guest after this long in Doze, or never.
    FreezeAfter(Option<Duration>),
    /// Freeze the guest now.
    Freeze(Sender<Result<(), SessionError>>),
    /// The host's clipboard got new text.
    HostClipboard(String),
    /// Whether the clipboards are shared.
    ShareClipboard(bool),
}

struct Shared {
    status: Mutex<(SessionState, Option<StopReason>)>,
    vm_id: Mutex<Option<String>>,
    changed: Condvar,
    subscribers: Mutex<Vec<Sender<SessionEvent>>>,
    tasks: Mutex<TaskHub>,
    /// A second handle that sends on the active connection, when the transport has one.
    /// Every frame to the guest goes through it while it is set, so a caller's frame goes out
    /// at once instead of waiting for the worker to stop waiting on the channel, and frames
    /// from different threads never interleave.
    sender: Mutex<Option<Box<dyn FrameSender>>>,
    /// The guest's last memory report, and when it arrived. Cleared when the guest stops.
    guest_memory: Mutex<Option<(GuestMemory, Instant)>>,
    /// What keeps the guest from being ready, while something does.
    problem: Mutex<Option<BootProblem>>,
    /// Who hears of the text the guest copies.
    clipboard_watchers: Mutex<Vec<Sender<String>>>,
    /// The install under way, by its transfer ID, with who waits for the agent's word on it.
    installs: Mutex<HashMap<u32, Sender<InstallEvent>>>,
    /// The transfer ID of the last install.
    last_install: AtomicU32,
    /// Held for the length of an install, so a second one waits for the first.
    install_turn: Mutex<()>,
}

/// What the agent says about an install under way.
enum InstallEvent {
    Progress(u64),
    Done(InstallOutcome),
}

/// The guest's tasks and who is watching them, under one lock, so a new watcher's starting
/// point and the changes it receives afterwards never overlap or leave a gap.
#[derive(Default)]
struct TaskHub {
    table: TaskTable,
    watchers: Vec<Sender<TaskEvent>>,
}

impl Shared {
    fn set(&self, state: SessionState, reason: Option<StopReason>) {
        // Holding the subscriber list across the update means a subscriber sees exactly the
        // changes made after it subscribed, never one from just before.
        let mut subscribers = self.subscribers.lock().unwrap_or_else(|e| e.into_inner());
        {
            let mut status = self.status.lock().unwrap_or_else(|e| e.into_inner());
            status.0 = state;
            if reason.is_some() {
                status.1 = reason.clone();
            }
            if state == SessionState::Stopped {
                *self.vm_id.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
        }
        self.changed.notify_all();
        let event = SessionEvent { state, reason };
        subscribers.retain(|subscriber| subscriber.send(event.clone()).is_ok());
    }

    fn set_vm_id(&self, vm_id: Option<String>) {
        *self.vm_id.lock().unwrap_or_else(|e| e.into_inner()) = vm_id;
    }

    fn set_problem(&self, problem: Option<BootProblem>) {
        *self.problem.lock().unwrap_or_else(|e| e.into_inner()) = problem;
    }

    fn problem(&self) -> Option<BootProblem> {
        self.problem
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn set_guest_memory(&self, memory: Option<(GuestMemory, Instant)>) {
        *self.guest_memory.lock().unwrap_or_else(|e| e.into_inner()) = memory;
    }

    /// Records a task message and hands it to the watchers, unless it changes nothing.
    fn publish_task(&self, event: TaskEvent) {
        let mut hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if !hub.table.apply(&event) {
            return;
        }
        hub.watchers
            .retain(|watcher| watcher.send(event.clone()).is_ok());
    }

    /// Records the name and icon of an installed app, for the list that names it next.
    fn describe_installed(&self, app: AppInfo) {
        let mut hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        hub.table.describe_installed(&app);
    }

    /// Makes `packages` the installed apps and hands the list to the watchers, unless it
    /// changes nothing.
    fn publish_installed(&self, packages: &[String]) {
        let event = {
            let mut hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            hub.table.installed_event(packages)
        };
        self.publish_task(event);
    }

    fn set_sender(&self, sender: Option<Box<dyn FrameSender>>) {
        *self.sender.lock().unwrap_or_else(|e| e.into_inner()) = sender;
    }

    /// Sends `frame` through the connection's second handle. `None` when there is none, or
    /// the guest cannot take frames now (it is not running, or it is frozen), so the worker
    /// must decide. A guest in Doze takes them: Android wakes for what it is sent.
    fn send_direct(&self, frame: &[u8]) -> Option<Result<(), SessionError>> {
        let mut sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        let sender = sender.as_mut()?;
        let state = self.status.lock().unwrap_or_else(|e| e.into_inner()).0;
        if !matches!(state, SessionState::Active | SessionState::Dozing) {
            return None;
        }
        Some(sender.send(frame).map_err(SessionError::Transport))
    }

    /// Hands the agent's word on an install to whoever sends that APK. Word on an install
    /// that is not under way is dropped and logged.
    fn install_event(&self, transfer_id: u32, event: InstallEvent) {
        let installs = self.installs.lock().unwrap_or_else(|e| e.into_inner());
        match installs.get(&transfer_id) {
            Some(waiting) => {
                let _ = waiting.send(event);
            }
            None => eprintln!(
                "install: dropped the agent's word on install {transfer_id}, which is not under way"
            ),
        }
    }

    /// Hands text the guest copied to every clipboard watcher still listening.
    fn publish_clipboard(&self, text: &str) {
        self.clipboard_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(text.to_string()).is_ok());
    }

    /// Removes every task, as when the guest has gone away.
    fn clear_tasks(&self) {
        let mut hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let removed = hub.table.clear();
        hub.watchers.retain(|watcher| {
            removed
                .iter()
                .all(|event| watcher.send(event.clone()).is_ok())
        });
    }
}

/// A guest session. Dropping it stops the guest if it is running.
pub struct Session {
    commands: Sender<Command>,
    shared: Arc<Shared>,
    incoming: Arc<Mutex<Option<Sender<Vec<u8>>>>>,
    worker: Option<JoinHandle<()>>,
    /// How long a guest the user lets be frozen stays in Doze first.
    freeze_delay: Duration,
}

impl Session {
    /// Creates a stopped session. Nothing starts until [`Session::start`].
    pub fn new(
        launcher: impl GuestLauncher,
        listener: Box<dyn Listener>,
        handshake: impl Handshake,
        config: SessionConfig,
    ) -> Session {
        let shared = Arc::new(Shared {
            status: Mutex::new((SessionState::Stopped, None)),
            vm_id: Mutex::new(None),
            changed: Condvar::new(),
            subscribers: Mutex::new(Vec::new()),
            tasks: Mutex::new(TaskHub::default()),
            sender: Mutex::new(None),
            guest_memory: Mutex::new(None),
            problem: Mutex::new(None),
            clipboard_watchers: Mutex::new(Vec::new()),
            installs: Mutex::new(HashMap::new()),
            last_install: AtomicU32::new(0),
            install_turn: Mutex::new(()),
        });
        let freeze_delay = config.freeze_after.unwrap_or(DEFAULT_FREEZE_AFTER);
        let (commands, command_rx) = mpsc::channel();
        let incoming = Arc::new(Mutex::new(None));
        let worker = Worker {
            launcher: Box::new(launcher),
            listener,
            handshake: Box::new(handshake),
            config,
            shared: Arc::clone(&shared),
            commands: command_rx,
            incoming: Arc::clone(&incoming),
            connection: None,
            secret: None,
            shutting_down: false,
            shown: false,
            hidden_since: Instant::now(),
            screen_on: true,
            dozing_since: None,
            power: None,
            power_pending: false,
            started_at: Instant::now(),
            boot_completed_at: None,
            agent_connected: false,
            agent_lost_at: None,
            prerequisite_missing: None,
            next_prerequisite_check: Instant::now(),
            next_diagnosis: Instant::now(),
            guest_is_idle: false,
            stall_doze: None,
            frozen_at: None,
            clipboard_sharing: true,
            host_clipboard: None,
            clipboard_last: None,
        };
        let worker = thread::Builder::new()
            .name("dsa-session".into())
            .spawn(move || worker.run())
            .expect("spawning the session thread");
        Session {
            commands,
            shared,
            incoming,
            worker: Some(worker),
            freeze_delay,
        }
    }

    /// Freezes the guest after `after` in Doze from now on, or never with `None`, as when the
    /// user turns freezing on or off.
    pub fn freeze_after(&self, after: Option<Duration>) {
        let _ = self.commands.send(Command::FreezeAfter(after));
    }

    /// Lets the guest be frozen once it has been in Doze for the configured time
    /// ([`SessionConfig::freeze_after`], or [`DEFAULT_FREEZE_AFTER`]), or never.
    pub fn freeze_when_idle(&self, on: bool) {
        self.freeze_after(on.then_some(self.freeze_delay));
    }

    /// Asks the session to start. Ignored unless the session is `Stopped`.
    pub fn start(&self) {
        let _ = self.commands.send(Command::Start);
    }

    /// Asks the session to stop. From `Stopping` after a failed stop, it retries.
    pub fn stop(&self) {
        let _ = self.commands.send(Command::Stop);
    }

    /// The current state.
    pub fn state(&self) -> SessionState {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
    }

    /// The cause of the last stop or failed stop, if there has been one.
    pub fn last_reason(&self) -> Option<StopReason> {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .clone()
    }

    /// What keeps the guest from being ready, while something does.
    pub fn problem(&self) -> Option<BootProblem> {
        self.shared.problem()
    }

    /// The runtime ID of the guest, if running.
    pub fn vm_id(&self) -> Option<String> {
        self.shared
            .vm_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Waits until the session is in `state`, for at most `timeout`. Returns whether it got
    /// there.
    pub fn wait_for(&self, state: SessionState, timeout: Duration) -> bool {
        let status = self.shared.status.lock().unwrap_or_else(|e| e.into_inner());
        let (status, _) = self
            .shared
            .changed
            .wait_timeout_while(status, timeout, |status| status.0 != state)
            .unwrap_or_else(|e| e.into_inner());
        status.0 == state
    }

    /// Subscribes to state changes from now on.
    pub fn events(&self) -> Receiver<SessionEvent> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .subscribers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Returns the stream of frames the agent sends while the session is active.
    ///
    /// There is one reader at a time: a new call replaces the previous stream. Frames that
    /// arrive while nobody holds a stream are discarded, so the reader subscribes before
    /// starting the session.
    pub fn incoming(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        *self.incoming.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        rx
    }

    /// The guest's last memory report and how old it is. `None` until the agent has sent one
    /// since the guest started.
    pub fn guest_memory(&self) -> Option<GuestMemoryReport> {
        self.shared
            .guest_memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(memory, at)| GuestMemoryReport {
                memory: memory.clone(),
                age: at.elapsed(),
            })
    }

    /// Asks the agent for a memory report now; it arrives in [`guest_memory`](Self::guest_memory).
    /// Needs an active session.
    pub fn request_guest_memory(&self) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::RequestGuestMemory(control::RequestGuestMemory {})),
        }))
    }

    /// Installs the APK called `name`, of `size` bytes read from `content`, in the guest.
    /// Returns how the install ended, as the agent said, or an error when the APK did not get
    /// there: the guest is not running, `content` ended early or failed, or the agent stopped
    /// answering. Needs a running guest; a frozen one is thawed by the first frame.
    ///
    /// The APK goes in chunks of [`apk::CHUNK_BYTES`], never more than
    /// [`apk::CHUNKS_IN_FLIGHT`] ahead of what the agent says it has written. One install runs
    /// at a time: a second call waits until the first has ended.
    pub fn install_apk(
        &self,
        name: &str,
        size: u64,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, SessionError> {
        apk::check_name(name)
            .and_then(|()| apk::check_size(size))
            .map_err(SessionError::InstallRefused)?;
        let _turn = self
            .shared
            .install_turn
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let state = self.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) {
            return Err(SessionError::NotActive(state));
        }
        let transfer_id = self
            .shared
            .last_install
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let (tx, events) = mpsc::channel();
        self.shared
            .installs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(transfer_id, tx);
        eprintln!("install: sending {name} ({size} bytes) to Android as install {transfer_id}");
        let result = self.send_apk(transfer_id, name, size, content, &events);
        self.shared
            .installs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&transfer_id);
        match &result {
            Ok(outcome) => eprintln!("install: {name}: {} ({})", outcome, outcome.status),
            Err(why) => eprintln!("install: {name} did not reach Android: {why}"),
        }
        result
    }

    fn send_apk(
        &self,
        transfer_id: u32,
        name: &str,
        size: u64,
        content: &mut dyn Read,
        events: &Receiver<InstallEvent>,
    ) -> Result<InstallOutcome, SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::InstallApk(control::InstallApk {
                transfer_id,
                name: name.to_string(),
                size,
            })),
        }))?;
        let window = apk::CHUNK_BYTES as u64 * apk::CHUNKS_IN_FLIGHT;
        let mut sent = 0u64;
        let mut written = 0u64;
        let mut waited = Duration::ZERO;
        loop {
            while sent < size && sent < written + window {
                let length = (size - sent).min(apk::CHUNK_BYTES as u64) as usize;
                let mut data = vec![0u8; length];
                if let Err(error) = content.read_exact(&mut data) {
                    self.cancel_install(transfer_id, events);
                    return Err(SessionError::InstallRefused(format!(
                        "the APK did not arrive whole: {error}"
                    )));
                }
                if let Err(error) = self.send(dsa_protocol::encode(&control::Envelope {
                    body: Some(Body::ApkChunk(control::ApkChunk {
                        transfer_id,
                        offset: sent,
                        data,
                    })),
                })) {
                    self.cancel_install(transfer_id, events);
                    return Err(error);
                }
                sent += length as u64;
            }
            match events.recv_timeout(INSTALL_POLL) {
                Ok(InstallEvent::Done(outcome)) => return Ok(outcome),
                Ok(InstallEvent::Progress(received)) => {
                    if received > size {
                        self.cancel_install(transfer_id, events);
                        return Err(SessionError::InstallRefused(format!(
                            "Android says it has {received} bytes of {name}, which has {size}"
                        )));
                    }
                    written = written.max(received);
                    waited = Duration::ZERO;
                }
                Err(RecvTimeoutError::Timeout) => {
                    let state = self.state();
                    if !matches!(
                        state,
                        SessionState::Active | SessionState::Dozing | SessionState::Frozen
                    ) {
                        return Err(SessionError::NotActive(state));
                    }
                    waited += INSTALL_POLL;
                    // Once the whole APK is there the guest verifies and installs it, which
                    // takes far longer than writing a chunk.
                    let limit = if written >= size {
                        INSTALL_TIMEOUT
                    } else {
                        CHUNK_TIMEOUT
                    };
                    if waited >= limit {
                        self.cancel_install(transfer_id, events);
                        return Err(SessionError::InstallRefused(format!(
                            "Android has not answered about {name} for {} s; it may be busy, \
                             try again",
                            limit.as_secs()
                        )));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(SessionError::Gone),
            }
        }
    }

    /// Tells the agent an install will not be finished, and waits a moment for it to say it
    /// has dropped it, so the next install does not start beside it.
    fn cancel_install(&self, transfer_id: u32, events: &Receiver<InstallEvent>) {
        let cancel = dsa_protocol::encode(&control::Envelope {
            body: Some(Body::CancelInstall(control::CancelInstall { transfer_id })),
        });
        if self.send(cancel).is_err() {
            return;
        }
        let deadline = Instant::now() + CANCEL_WAIT;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match events.recv_timeout(left) {
                Ok(InstallEvent::Done(_)) | Err(_) => return,
                Ok(InstallEvent::Progress(_)) => {}
            }
        }
    }

    /// Sends a frame to the agent. Needs an active session.
    pub fn send(&self, frame: Vec<u8>) -> Result<(), SessionError> {
        if let Some(result) = self.shared.send_direct(&frame) {
            return result;
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Send(frame, reply_tx))
            .map_err(|_| SessionError::Gone)?;
        reply_rx.recv().map_err(|_| SessionError::Gone)?
    }

    /// The guest's tasks now, and every change to them from now on. The watch lasts across
    /// restarts of the guest: when it stops, its tasks are removed, and the next boot's
    /// tasks arrive as they appear.
    pub fn watch_tasks(&self) -> TaskWatch {
        let (tx, events) = mpsc::channel();
        let mut hub = self.shared.tasks.lock().unwrap_or_else(|e| e.into_inner());
        let current = hub.table.snapshot();
        hub.watchers.push(tx);
        TaskWatch { current, events }
    }

    /// The guest's tasks as the agent last described them, in task ID order.
    pub fn tasks(&self) -> Vec<Task> {
        self.shared
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .table
            .tasks()
    }

    /// Says whether any task window is shown. The session keeps the latest word in every
    /// state, so a guest that starts with no window shown goes into Doze after the delay.
    pub fn windows_shown(&self, shown: bool) -> Result<(), SessionError> {
        self.commands
            .send(Command::WindowsShown(shown))
            .map_err(|_| SessionError::Gone)
    }

    /// Records the host's power, which the guest is told now if it is up, and each time its
    /// agent connects.
    pub fn set_power(&self, state: PowerState) {
        let _ = self.commands.send(Command::Power(state));
    }

    /// Says that the host's clipboard got `text`. It goes to the guest while sharing is on and
    /// the guest is `Active` or `Dozing`; otherwise the latest text waits for the guest to get
    /// there. Taken in every state.
    pub fn host_clipboard(&self, text: String) {
        let _ = self.commands.send(Command::HostClipboard(text));
    }

    /// Shares the clipboards (`on`, the default) or stops sharing them, as the user's setting
    /// says. While they are not shared, nothing crosses in either direction.
    pub fn share_clipboard(&self, on: bool) {
        let _ = self.commands.send(Command::ShareClipboard(on));
    }

    /// The text the guest copies from now on, while sharing is on. The watch lasts across
    /// restarts of the guest.
    pub fn watch_clipboard(&self) -> Receiver<String> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .clipboard_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Freezes the guest now, as when the user chooses it. It stays frozen until a task
    /// window is shown or it is stopped. Needs a running guest.
    pub fn freeze(&self) -> Result<(), SessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Freeze(reply_tx))
            .map_err(|_| SessionError::Gone)?;
        reply_rx.recv().map_err(|_| SessionError::Gone)?
    }

    /// Asks the guest to close, resize or rescale a task, or to launch an app. Needs an
    /// active session, except for the word on shown windows, which is always taken.
    pub fn request(&self, request: TaskRequest) -> Result<(), SessionError> {
        let body = match request {
            TaskRequest::WindowsShown { shown } => return self.windows_shown(shown),
            TaskRequest::Close { task_id } => Body::CloseTask(control::CloseTask { task_id }),
            TaskRequest::Launch { package_name } => {
                Body::LaunchApp(control::LaunchApp { package_name })
            }
            TaskRequest::Resize {
                task_id,
                width,
                height,
                live,
            } => Body::ResizeTask(control::ResizeTask {
                task_id,
                width,
                height,
                live,
            }),
            TaskRequest::SetDensity {
                task_id,
                density_dpi,
            } => Body::SetTaskDensity(control::SetTaskDensity {
                task_id,
                density_dpi,
            }),
        };
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(body),
        }))
    }

    /// Passes pointer input in a task's window to the guest. Needs an active session.
    pub fn pointer(&self, event: &PointerEvent) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::PointerEvent(event.to_control())),
        }))
    }

    /// Passes a key pressed or released in a task's window to the guest. Needs an active
    /// session.
    pub fn key(&self, event: &KeyEvent) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::KeyEvent(event.to_control())),
        }))
    }

    /// Passes text composed or committed in a task's window to the guest. Needs an active
    /// session.
    pub fn ime(&self, event: &ImeEvent) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(event.to_control()),
        }))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// How often an active session checks for requests while it waits on the channel.
const POLL: Duration = Duration::from_millis(20);

/// How often an install looks at the session while it waits for the agent.
const INSTALL_POLL: Duration = Duration::from_millis(500);

/// How long an install waits for the agent to write a chunk. Writing 128 KiB takes
/// milliseconds; this long means the agent is stuck or gone.
const CHUNK_TIMEOUT: Duration = Duration::from_secs(30);

/// How long an install waits for the guest to install a whole APK: verifying and compiling a
/// large app takes minutes on a slow disk.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long a cancelled install waits for the agent to say it dropped it.
const CANCEL_WAIT: Duration = Duration::from_secs(5);

/// How often the session asks the launcher for what the guest cannot run without.
const PREREQUISITE_POLL: Duration = Duration::from_secs(1);

/// How often a reported problem's detail is read again, as the counts in it grow.
const DIAGNOSIS_POLL: Duration = Duration::from_secs(10);

struct Worker {
    launcher: Box<dyn GuestLauncher>,
    listener: Box<dyn Listener>,
    handshake: Box<dyn Handshake>,
    config: SessionConfig,
    shared: Arc<Shared>,
    commands: Receiver<Command>,
    #[allow(dead_code)]
    incoming: Arc<Mutex<Option<Sender<Vec<u8>>>>>,
    connection: Option<Box<dyn Connection>>,
    /// The running boot's secret, made anew at each start.
    secret: Option<BootSecret>,
    shutting_down: bool,
    /// Whether any task window is shown, as last said. Kept across restarts.
    shown: bool,
    /// Since when no window has been shown, or since the agent connected if later.
    hidden_since: Instant,
    /// Whether the guest was last told its screen is on. An agent that connects takes it to
    /// be on.
    screen_on: bool,
    /// Since when the session has been `Dozing`.
    dozing_since: Option<Instant>,
    /// The host's power, as last recorded.
    power: Option<PowerState>,
    /// Set when the power changed while the guest was frozen, so it is told after the thaw.
    power_pending: bool,
    /// When the guest was started.
    started_at: Instant,
    /// When the guest said it had finished booting.
    boot_completed_at: Option<Instant>,
    /// Whether the guest says its agent is connected.
    agent_connected: bool,
    /// When the guest said its agent had gone.
    agent_lost_at: Option<Instant>,
    /// Since when a prerequisite is missing, and what the launcher says about it.
    prerequisite_missing: Option<(Instant, String)>,
    next_prerequisite_check: Instant,
    next_diagnosis: Instant,
    /// The agent's last word on whether the device is idle. Unknown, so false, for a new agent.
    guest_is_idle: bool,
    /// Set when a stall interrupted Doze with the agent still there: how long the freeze timer
    /// had counted, if it was still counting. Time spent stalled does not count.
    stall_doze: Option<Option<Duration>>,
    /// When the guest was frozen, while it is. Nothing runs in it then, so that time does not
    /// count against it.
    frozen_at: Option<Instant>,
    /// Whether the clipboards are shared, as the user last said.
    clipboard_sharing: bool,
    /// The host's latest clipboard text while sharing is on, for a guest that gets ready later.
    host_clipboard: Option<String>,
    /// The last text that crossed, either way, since the agent connected. Text equal to it is
    /// not sent, so what one side was given never comes back to it.
    clipboard_last: Option<String>,
}

impl Worker {
    fn state(&self) -> SessionState {
        self.shared
            .status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .0
    }

    fn run(mut self) {
        loop {
            match self.state() {
                SessionState::Stopped => {
                    if self.shutting_down {
                        return;
                    }
                    match self.commands.recv() {
                        Ok(Command::Start) => self.start(),
                        Ok(Command::Stop) => {}
                        Ok(Command::Send(_, reply)) | Ok(Command::Freeze(reply)) => {
                            let _ = reply.send(Err(SessionError::NotActive(SessionState::Stopped)));
                        }
                        Ok(Command::WindowsShown(shown)) => self.set_shown(shown),
                        Ok(Command::Power(state)) => self.set_power(state),
                        Ok(Command::FreezeAfter(after)) => self.config.freeze_after = after,
                        Ok(Command::HostClipboard(text)) => self.host_clipboard(text),
                        Ok(Command::ShareClipboard(on)) => self.share_clipboard(on),
                        Ok(Command::Shutdown) | Err(_) => return,
                    }
                }
                SessionState::Stopping => {
                    // Only reached after a failed stop: the guest may still be running.
                    if self.shutting_down {
                        return;
                    }
                    match self.commands.recv() {
                        Ok(Command::Stop) => self.stop(StopReason::Requested),
                        Ok(Command::Start) => {}
                        Ok(Command::Send(_, reply)) | Ok(Command::Freeze(reply)) => {
                            let _ =
                                reply.send(Err(SessionError::NotActive(SessionState::Stopping)));
                        }
                        Ok(Command::WindowsShown(shown)) => self.set_shown(shown),
                        Ok(Command::Power(state)) => self.set_power(state),
                        Ok(Command::FreezeAfter(after)) => self.config.freeze_after = after,
                        Ok(Command::HostClipboard(text)) => self.host_clipboard(text),
                        Ok(Command::ShareClipboard(on)) => self.share_clipboard(on),
                        Ok(Command::Shutdown) | Err(_) => return,
                    }
                }
                // start() returns in these only once the guest's daemon has connected.
                SessionState::Starting
                | SessionState::Stalled
                | SessionState::Active
                | SessionState::Dozing
                | SessionState::Frozen => self.serve(),
            }
        }
    }

    /// Handles a request that arrives while the guest is up. Returns false once the session
    /// has left the running states.
    fn handle_running(&mut self, command: Command) -> bool {
        match command {
            Command::Start => true,
            Command::Stop => {
                self.stop(StopReason::Requested);
                false
            }
            Command::Shutdown => {
                self.shutting_down = true;
                self.stop(StopReason::Requested);
                false
            }
            Command::Send(frame, reply) => {
                // Something for a frozen guest, such as input in a window it still shows,
                // thaws it first.
                if self.state() == SessionState::Frozen {
                    self.wake();
                }
                let state = self.state();
                let result = match self.shared.send_direct(&frame) {
                    Some(result) => result,
                    None => match (state, self.connection.as_mut()) {
                        (SessionState::Active | SessionState::Dozing, Some(connection)) => {
                            connection.send(&frame).map_err(SessionError::Transport)
                        }
                        _ => Err(SessionError::NotActive(state)),
                    },
                };
                let _ = reply.send(result);
                self.running()
            }
            Command::WindowsShown(shown) => {
                self.set_shown(shown);
                self.running()
            }
            Command::FreezeAfter(after) => {
                self.config.freeze_after = after;
                true
            }
            Command::Power(state) => {
                self.set_power(state);
                true
            }
            Command::Freeze(reply) => {
                let _ = reply.send(self.freeze_now());
                true
            }
            Command::HostClipboard(text) => {
                self.host_clipboard(text);
                true
            }
            Command::ShareClipboard(on) => {
                self.share_clipboard(on);
                true
            }
        }
    }

    /// Takes new text from the host's clipboard. It is sent at once if the guest can take it,
    /// and kept for when it can.
    fn host_clipboard(&mut self, text: String) {
        if !self.clipboard_sharing {
            return;
        }
        if !clipboard::fits(&text) {
            eprintln!("{}", clipboard::too_long(&text, "host"));
            return;
        }
        self.host_clipboard = Some(text);
        self.offer_host_clipboard();
    }

    /// Sends the host's latest clipboard text to a guest that is ready for it and does not
    /// have it yet.
    fn offer_host_clipboard(&mut self) {
        if !self.clipboard_sharing
            || !self.agent_connected
            || !matches!(self.state(), SessionState::Active | SessionState::Dozing)
        {
            return;
        }
        let Some(text) = &self.host_clipboard else {
            return;
        };
        if self.clipboard_last.as_ref() == Some(text) {
            return;
        }
        let text = text.clone();
        self.clipboard_last = Some(text.clone());
        self.send_body(Body::ClipboardText(control::ClipboardText { text }));
    }

    /// Takes text the guest copied, and hands it to the watchers unless it is what the guest
    /// was just given.
    fn guest_clipboard(&mut self, text: String) {
        if !self.clipboard_sharing {
            eprintln!("clipboard: dropped text copied in Android; sharing is off");
            return;
        }
        if !clipboard::fits(&text) {
            eprintln!("{}", clipboard::too_long(&text, "guest"));
            return;
        }
        if self.clipboard_last.as_ref() == Some(&text) {
            return;
        }
        self.clipboard_last = Some(text.clone());
        // The host's clipboard is about to hold it, so it is not offered back.
        self.host_clipboard = Some(text.clone());
        self.shared.publish_clipboard(&text);
    }

    /// Turns clipboard sharing on or off, and tells a connected agent.
    fn share_clipboard(&mut self, on: bool) {
        if on == self.clipboard_sharing {
            return;
        }
        self.clipboard_sharing = on;
        // Nothing copied while sharing was off crosses later; the next copy does.
        self.host_clipboard = None;
        self.clipboard_last = None;
        eprintln!("clipboard: sharing is {}", if on { "on" } else { "off" });
        if self.running() && self.agent_connected {
            self.send_body(Body::ClipboardSharing(control::ClipboardSharing { on }));
        }
    }

    fn running(&self) -> bool {
        matches!(
            self.state(),
            SessionState::Starting
                | SessionState::Stalled
                | SessionState::Active
                | SessionState::Dozing
                | SessionState::Frozen
        )
    }

    /// Sends a message of the lifecycle to the guest, in any running state. A frame that does
    /// not go out is logged; a lost channel is noticed by the next read.
    fn send_body(&mut self, body: Body) {
        let frame = dsa_protocol::encode(&control::Envelope { body: Some(body) });
        let direct = {
            let mut sender = self.shared.sender.lock().unwrap_or_else(|e| e.into_inner());
            sender.as_mut().map(|sender| sender.send(&frame))
        };
        let result = match direct {
            Some(result) => result,
            None => match self.connection.as_mut() {
                Some(connection) => connection.send(&frame),
                None => return,
            },
        };
        if let Err(error) = result {
            eprintln!("control channel: could not tell the guest of its screen or power: {error}");
        }
    }

    /// Takes the word on shown windows. A window shown again wakes a running guest.
    fn set_shown(&mut self, shown: bool) {
        if shown == self.shown {
            return;
        }
        self.shown = shown;
        if !shown {
            self.hidden_since = Instant::now();
        } else if self.running() {
            self.wake();
        }
    }

    /// Thaws a frozen guest, turns its screen on, and makes the session `Active`. The session
    /// does not wait for the guest to leave Doze: the screen coming on is what ends it.
    fn wake(&mut self) {
        if self.state() == SessionState::Frozen {
            if let Err(why) = self.launcher.thaw() {
                self.stop(StopReason::GuestExited {
                    why: format!("the frozen guest could not be resumed: {why}"),
                    diagnostics: None,
                });
                return;
            }
            if let Some(frozen_at) = self.frozen_at.take() {
                self.discount_frozen_time(frozen_at.elapsed());
            }
            if std::mem::take(&mut self.power_pending) {
                if let Some(power) = self.power {
                    self.send_body(Body::PowerState(power.to_control()));
                }
            }
        }
        if !self.screen_on {
            self.screen_on = true;
            self.guest_is_idle = false;
            self.stall_doze = None;
            self.send_body(Body::ScreenState(control::ScreenState { on: true }));
        }
        self.dozing_since = None;
        if matches!(self.state(), SessionState::Dozing | SessionState::Frozen) {
            self.shared.set(SessionState::Active, None);
        }
    }

    /// Moves what the boot timers count from past a freeze of length `frozen`, so a thawed guest
    /// has the time it had left when it was frozen.
    fn discount_frozen_time(&mut self, frozen: Duration) {
        self.started_at += frozen;
        if let Some(booted) = &mut self.boot_completed_at {
            *booted += frozen;
        }
        if let Some(lost) = &mut self.agent_lost_at {
            *lost += frozen;
        }
        if let Some((since, _)) = &mut self.prerequisite_missing {
            *since += frozen;
        }
    }

    /// Records the host's power and tells a guest that can hear it.
    fn set_power(&mut self, state: PowerState) {
        if self.power == Some(state) {
            return;
        }
        self.power = Some(state);
        match self.state() {
            // A guest still starting may have its agent already; if not, the agent is told
            // when it connects.
            SessionState::Starting
            | SessionState::Stalled
            | SessionState::Active
            | SessionState::Dozing => self.send_body(Body::PowerState(state.to_control())),
            SessionState::Frozen => self.power_pending = true,
            _ => {}
        }
    }

    /// Freezes the running guest, turning its screen off first so it wakes as a device whose
    /// screen comes on.
    fn freeze_now(&mut self) -> Result<(), SessionError> {
        match self.state() {
            SessionState::Frozen => return Ok(()),
            SessionState::Active | SessionState::Dozing => {}
            other => return Err(SessionError::NotActive(other)),
        }
        if self.screen_on {
            self.screen_on = false;
            self.send_body(Body::ScreenState(control::ScreenState { on: false }));
        }
        self.launcher.freeze().map_err(SessionError::FreezeFailed)?;
        self.frozen_at = Some(Instant::now());
        self.dozing_since = None;
        self.shared.set(SessionState::Frozen, None);
        Ok(())
    }

    /// Moves the lifecycle on with time: the screen goes off once no window has been shown
    /// for the doze delay, and a guest in Doze is frozen after `freeze_after`, when set.
    fn tick_lifecycle(&mut self) {
        let now = Instant::now();
        match self.state() {
            SessionState::Active
                if !self.shown
                    && self.screen_on
                    && now.duration_since(self.hidden_since) >= self.config.doze_delay =>
            {
                self.screen_on = false;
                self.send_body(Body::ScreenState(control::ScreenState { on: false }));
            }
            SessionState::Dozing => {
                let (Some(after), Some(since)) = (self.config.freeze_after, self.dozing_since)
                else {
                    return;
                };
                if now.duration_since(since) >= after {
                    if let Err(why) = self.freeze_now() {
                        // Tried once per stay in Doze, not again at every turn of the loop.
                        self.dozing_since = None;
                        eprintln!("lifecycle: {why}");
                    }
                }
            }
            _ => {}
        }
    }

    /// The agent says whether the guest is in Doze. The session becomes `Dozing` once the
    /// guest is in Doze with its screen off. The guest leaves deep idle for short maintenance
    /// windows while it dozes; the session stays `Dozing` through them, and leaves it only
    /// when the screen comes on.
    fn guest_idle(&mut self, idle: bool) {
        self.guest_is_idle = idle;
        if idle && !self.screen_on && self.state() == SessionState::Active {
            self.dozing_since = Some(Instant::now());
            self.shared.set(SessionState::Dozing, None);
        }
    }

    /// The guest says how far it has got. An agent that comes takes the screen to be on and
    /// knows nothing of the power, so it is told both.
    fn guest_boot(&mut self, boot: control::GuestBoot) {
        let now = Instant::now();
        if boot.boot_completed && self.boot_completed_at.is_none() {
            self.boot_completed_at = Some(now);
            eprintln!(
                "lifecycle: Android finished booting after {:.1} s",
                now.duration_since(self.started_at).as_secs_f64()
            );
        }
        if boot.agent_connected == self.agent_connected {
            return;
        }
        self.agent_connected = boot.agent_connected;
        // A new agent takes the screen to be on, so what the last one said of Doze is over.
        self.guest_is_idle = false;
        self.stall_doze = None;
        if boot.agent_connected {
            self.agent_lost_at = None;
            eprintln!("lifecycle: the agent is connected");
            if let Some(power) = self.power {
                self.send_body(Body::PowerState(power.to_control()));
            }
            if !self.screen_on {
                self.send_body(Body::ScreenState(control::ScreenState { on: false }));
            }
            // A new agent takes the clipboards to be shared, and has none of the host's text.
            self.clipboard_last = None;
            if !self.clipboard_sharing {
                self.send_body(Body::ClipboardSharing(control::ClipboardSharing {
                    on: false,
                }));
            }
        } else {
            self.agent_lost_at = Some(now);
            eprintln!("lifecycle: the agent is gone");
        }
    }

    /// Asks the launcher, every so often, for what the guest cannot run without.
    fn check_prerequisite(&mut self, now: Instant) {
        if now < self.next_prerequisite_check {
            return;
        }
        self.next_prerequisite_check = now + PREREQUISITE_POLL;
        let missing = self.launcher.prerequisite();
        self.prerequisite_missing = missing.map(|why| {
            let since = self
                .prerequisite_missing
                .as_ref()
                .map_or(now, |(since, _)| *since);
            (since, why)
        });
    }

    /// What keeps the running guest from being ready and has lasted too long, if anything.
    fn overdue(&mut self, now: Instant) -> Option<BootProblem> {
        if let Some((since, why)) = &self.prerequisite_missing {
            if now.duration_since(*since) >= self.config.prerequisite_grace {
                return Some(BootProblem::Prerequisite(why.clone()));
            }
        }
        let (stall, waited, limit) = match self.boot_completed_at {
            None => (
                GuestStall::BootUnfinished,
                now.duration_since(self.started_at),
                self.config.ready_timeout,
            ),
            Some(_) if self.agent_connected => return None,
            Some(booted) => (
                GuestStall::AgentUnreachable,
                now.duration_since(self.agent_lost_at.map_or(booted, |lost| lost.max(booted))),
                self.config.agent_timeout,
            ),
        };
        if waited < limit {
            return None;
        }
        // The launcher reads its console for this, so only every so often.
        let known = if now < self.next_diagnosis {
            match (stall, self.shared.problem()) {
                (GuestStall::BootUnfinished, Some(BootProblem::BootUnfinished { detail, .. })) => {
                    Some((detail, false))
                }
                (
                    GuestStall::AgentUnreachable,
                    Some(BootProblem::AgentUnreachable { detail, .. }),
                ) => Some((detail, false)),
                (
                    GuestStall::AgentUnreachable,
                    Some(BootProblem::DiskFromOtherKeys { detail, .. }),
                ) => Some((detail, true)),
                _ => None,
            }
        } else {
            None
        };
        let (detail, other_keys) = known.unwrap_or_else(|| {
            self.next_diagnosis = now + DIAGNOSIS_POLL;
            let other_keys =
                stall == GuestStall::AgentUnreachable && self.launcher.agent_refused_for_keys();
            (self.launcher.explain_stall(stall), other_keys)
        });
        Some(match stall {
            GuestStall::BootUnfinished => BootProblem::BootUnfinished { waited, detail },
            GuestStall::AgentUnreachable if other_keys => {
                BootProblem::DiskFromOtherKeys { waited, detail }
            }
            GuestStall::AgentUnreachable => BootProblem::AgentUnreachable { waited, detail },
        })
    }

    /// Records `problem` as what keeps the guest from being ready, logging a new kind of it.
    fn report_problem(&mut self, problem: BootProblem) {
        let new = self
            .shared
            .problem()
            .is_none_or(|old| !old.same_kind(&problem));
        if new {
            eprintln!("lifecycle: {problem}");
        }
        self.shared.set_problem(Some(problem));
    }

    /// Moves the session on with what the guest last said. Returns false once the session has
    /// left the running states.
    fn tick_boot(&mut self) -> bool {
        if self.state() == SessionState::Frozen {
            return true;
        }
        let now = Instant::now();
        self.check_prerequisite(now);
        if let Some((since, why)) = &self.prerequisite_missing {
            if now.duration_since(*since) >= self.config.prerequisite_stop_after {
                let why = why.clone();
                self.stop(StopReason::PrerequisiteLost(why));
                return false;
            }
        }
        if let Some(problem) = self.overdue(now) {
            self.report_problem(problem);
            if self.state() != SessionState::Stalled {
                if self.state() == SessionState::Dozing && self.agent_connected {
                    self.stall_doze =
                        Some(self.dozing_since.map(|since| now.duration_since(since)));
                }
                self.dozing_since = None;
                self.shared.set(SessionState::Stalled, None);
            }
            return true;
        }
        let ready = self.boot_completed_at.is_some()
            && self.agent_connected
            && self.prerequisite_missing.is_none();
        match self.state() {
            SessionState::Starting | SessionState::Stalled if ready => {
                self.shared.set_problem(None);
                // The doze delay counts from here when no window is shown.
                self.hidden_since = now;
                let doze = self.stall_doze.take();
                if !self.screen_on && (doze.is_some() || self.guest_is_idle) {
                    // The guest has dozed all along. A timer that had given up stays given up.
                    self.dozing_since = match doze {
                        Some(counted) => {
                            counted.map(|counted| now.checked_sub(counted).unwrap_or(now))
                        }
                        None => Some(now),
                    };
                    self.shared.set(SessionState::Dozing, None);
                } else {
                    self.shared.set(SessionState::Active, None);
                }
            }
            SessionState::Stalled => {
                self.shared.set_problem(None);
                self.shared.set(SessionState::Starting, None);
            }
            _ => {}
        }
        true
    }

    /// Waits until the guest can run. Returns false once the start was abandoned.
    fn wait_for_prerequisite(&mut self) -> bool {
        let began = Instant::now();
        loop {
            let Some(why) = self.launcher.prerequisite() else {
                self.shared.set_problem(None);
                if self.state() == SessionState::Stalled {
                    self.shared.set(SessionState::Starting, None);
                }
                return true;
            };
            self.report_problem(BootProblem::Prerequisite(why));
            if self.state() == SessionState::Starting
                && began.elapsed() >= self.config.prerequisite_grace
            {
                self.shared.set(SessionState::Stalled, None);
            }
            match self.commands.recv_timeout(PREREQUISITE_POLL) {
                Ok(command) => {
                    if !self.handle_starting(command) {
                        return false;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.shutting_down = true;
                    self.stop(StopReason::Requested);
                    return false;
                }
            }
        }
    }

    fn start(&mut self) {
        self.shared.set(SessionState::Starting, None);
        self.boot_completed_at = None;
        self.agent_connected = false;
        self.agent_lost_at = None;
        self.prerequisite_missing = None;
        self.guest_is_idle = false;
        self.stall_doze = None;
        self.frozen_at = None;
        if !self.wait_for_prerequisite() {
            return;
        }
        let secret = match BootSecret::generate() {
            Ok(secret) => secret,
            Err(why) => {
                self.shared.set(
                    SessionState::Stopped,
                    Some(StopReason::GuestFailedToStart(why)),
                );
                return;
            }
        };
        let started = self.launcher.start(&secret);
        self.secret = Some(secret);
        if let Err(why) = started {
            // The launcher may have got part of the way; make sure nothing is left behind.
            let _ = self.launcher.stop(self.config.stop_timeout);
            self.shared.set(
                SessionState::Stopped,
                Some(StopReason::GuestFailedToStart(why)),
            );
            return;
        }
        // The guest renders while it boots, before the agent connects, and the compositor
        // binds its sockets to this ID, so it is known from the moment the guest runs.
        self.shared.set_vm_id(self.launcher.vm_id());
        let started = Instant::now();
        self.started_at = started;
        self.next_prerequisite_check = started + PREREQUISITE_POLL;
        let deadline = started + self.config.connect_timeout;
        loop {
            loop {
                match self.commands.try_recv() {
                    Ok(command) => {
                        if !self.handle_starting(command) {
                            return;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        self.shutting_down = true;
                        self.stop(StopReason::Requested);
                        return;
                    }
                }
            }
            if let Some(why) = self.launcher.exited() {
                self.stop_failed_start(|diagnostics| StopReason::GuestExited { why, diagnostics });
                return;
            }
            let now = Instant::now();
            if now >= deadline {
                let waited = now - started;
                self.stop_failed_start(|diagnostics| StopReason::AgentDidNotConnect {
                    waited,
                    diagnostics,
                });
                return;
            }
            match self.listener.accept((deadline - now).min(POLL * 5)) {
                Ok(mut connection) => {
                    let Some(secret) = self.secret.as_ref() else {
                        unreachable!("start() sets the secret before it listens");
                    };
                    match self.handshake.accept(connection.as_mut(), secret) {
                        Ok(()) => {
                            self.shared.set_sender(connection.try_clone_sender());
                            self.connection = Some(connection);
                            self.shared.set_vm_id(self.launcher.vm_id());
                            // The agent takes the screen to be on when it connects. The session
                            // stays Starting until the guest says it is ready.
                            self.screen_on = true;
                            self.hidden_since = Instant::now();
                            self.dozing_since = None;
                            self.power_pending = false;
                            return;
                        }
                        Err(Refusal::NotTheGuest(why)) => {
                            connection.close();
                            eprintln!("control channel: refused a connection: {why}");
                        }
                        Err(Refusal::Incompatible(why)) => {
                            connection.close();
                            self.stop(StopReason::HandshakeRejected(why));
                            return;
                        }
                    }
                }
                Err(TransportError::Timeout) => {}
                Err(error) => {
                    self.stop(StopReason::ListenerFailed(error.to_string()));
                    return;
                }
            }
        }
    }

    /// Handles a request while starting. Returns false once the start was abandoned.
    fn handle_starting(&mut self, command: Command) -> bool {
        match command {
            Command::Start => true,
            Command::Send(_, reply) | Command::Freeze(reply) => {
                let _ = reply.send(Err(SessionError::NotActive(SessionState::Starting)));
                true
            }
            Command::WindowsShown(shown) => {
                self.set_shown(shown);
                true
            }
            Command::FreezeAfter(after) => {
                self.config.freeze_after = after;
                true
            }
            Command::Power(state) => {
                self.set_power(state);
                true
            }
            Command::HostClipboard(text) => {
                self.host_clipboard(text);
                true
            }
            Command::ShareClipboard(on) => {
                self.share_clipboard(on);
                true
            }
            Command::Stop => {
                self.stop(StopReason::Requested);
                false
            }
            Command::Shutdown => {
                self.shutting_down = true;
                self.stop(StopReason::Requested);
                false
            }
        }
    }

    /// Runs while the guest is up: forwards the agent's frames and handles requests.
    fn serve(&mut self) {
        loop {
            match self.commands.recv_timeout(Duration::ZERO) {
                Ok(command) => {
                    if !self.handle_running(command) {
                        return;
                    }
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    self.shutting_down = true;
                    self.stop(StopReason::Requested);
                    return;
                }
            }
            if !self.tick_boot() {
                return;
            }
            self.tick_lifecycle();
            self.offer_host_clipboard();
            let Some(connection) = self.connection.as_mut() else {
                self.stop(StopReason::ChannelLost("no connection".into()));
                return;
            };
            match connection.recv(Some(POLL)) {
                Ok(frame) => match dsa_protocol::decode(&frame) {
                    Ok(envelope) => self.received(envelope.body),
                    Err(error) => {
                        eprintln!("control channel: dropping malformed frame: {error}");
                    }
                },
                Err(TransportError::Timeout) => {}
                Err(error) => {
                    // A guest that dies while it boots has its console to show for it.
                    if matches!(self.state(), SessionState::Starting | SessionState::Stalled) {
                        self.stop_failed_start(|diagnostics| StopReason::GuestExited {
                            why: format!("the control channel was lost during boot: {error}"),
                            diagnostics,
                        });
                    } else {
                        self.stop(StopReason::ChannelLost(error.to_string()));
                    }
                    return;
                }
            }
        }
    }

    /// Acts on one message from the guest. A message the host does not expect is dropped
    /// and logged; it never ends the session.
    fn received(&mut self, body: Option<Body>) {
        match body {
            Some(Body::Hello(_))
            | Some(Body::HelloAccepted(_))
            | Some(Body::HelloRejected(_))
            | Some(Body::AuthChallenge(_))
            | Some(Body::AuthProof(_)) => {
                eprintln!(
                    "control channel: dropping unexpected handshake message on active session"
                );
            }
            Some(Body::TaskUpdated(update)) => {
                let bounds = update.bounds.unwrap_or_default();
                self.shared.publish_task(TaskEvent::Updated(Task {
                    task_id: update.task_id,
                    bounds: Bounds {
                        left: bounds.left,
                        top: bounds.top,
                        right: bounds.right,
                        bottom: bounds.bottom,
                    },
                    package_name: update.package_name,
                    label: update.label,
                    focused: update.focused,
                    density_dpi: update.density_dpi,
                    max_width: update.max_width,
                    max_height: update.max_height,
                    min_width: update.min_width,
                    min_height: update.min_height,
                }));
            }
            Some(Body::TaskRemoved(removed)) => {
                self.shared.publish_task(TaskEvent::Removed {
                    task_id: removed.task_id,
                });
            }
            Some(Body::TaskFrames(frames)) => {
                self.shared.publish_task(TaskEvent::Frames {
                    task_id: frames.task_id,
                    display_id: frames.display_id,
                });
            }
            Some(Body::AppInfo(app)) => {
                self.shared.publish_task(TaskEvent::App(AppInfo {
                    package_name: app.package_name,
                    label: app.label,
                    icon_png: app.icon_png,
                }));
            }
            Some(Body::InstalledApp(app)) => {
                self.shared.describe_installed(AppInfo {
                    package_name: app.package_name,
                    label: app.label,
                    icon_png: app.icon_png,
                });
            }
            Some(Body::InstalledApps(apps)) => {
                self.shared.publish_installed(&apps.package_names);
            }
            Some(Body::ImeCursor(cursor)) => {
                let rect = cursor.cursor.unwrap_or_default();
                self.shared.publish_task(TaskEvent::ImeCursor {
                    task_id: cursor.task_id,
                    cursor: ImeCursor {
                        left: rect.left,
                        top: rect.top,
                        right: rect.right,
                        bottom: rect.bottom,
                    },
                });
            }
            Some(Body::GuestMemory(memory)) => {
                let memory = GuestMemory::from_control(memory);
                self.shared.set_guest_memory(Some((memory, Instant::now())));
            }
            Some(Body::DeviceIdle(idle)) => self.guest_idle(idle.idle),
            Some(Body::ClipboardText(clip)) => self.guest_clipboard(clip.text),
            Some(Body::InstallProgress(progress)) => self.shared.install_event(
                progress.transfer_id,
                InstallEvent::Progress(progress.received),
            ),
            Some(Body::InstallResult(result)) => self.shared.install_event(
                result.transfer_id,
                InstallEvent::Done(InstallOutcome::from_control(&result)),
            ),
            Some(Body::GuestBoot(boot)) => self.guest_boot(boot),
            Some(Body::AgentConnected(_)) => {
                eprintln!("control channel: dropping a message that stops at the guest's daemon");
            }
            Some(Body::ScreenState(_))
            | Some(Body::PowerState(_))
            | Some(Body::CloseTask(_))
            | Some(Body::LaunchApp(_))
            | Some(Body::ResizeTask(_))
            | Some(Body::SetTaskDensity(_))
            | Some(Body::PointerEvent(_))
            | Some(Body::KeyEvent(_))
            | Some(Body::ImeComposition(_))
            | Some(Body::ImeCommit(_))
            | Some(Body::ClipboardSharing(_))
            | Some(Body::InstallApk(_))
            | Some(Body::ApkChunk(_))
            | Some(Body::CancelInstall(_))
            | Some(Body::RequestGuestMemory(_)) => {
                eprintln!("control channel: dropping a request only the host sends");
            }
            None => {
                eprintln!("control channel: dropping message with unknown body");
            }
        }
    }

    fn stop(&mut self, reason: StopReason) {
        self.stop_with(|_| reason);
    }

    /// Stops a guest whose start failed, and reports `reason` with the launcher's
    /// diagnostics. They are asked for once the guest is gone, so they cover all of its
    /// output.
    fn stop_failed_start(&mut self, reason: impl FnOnce(Option<String>) -> StopReason) {
        self.stop_with(|launcher| reason(launcher.diagnostics()));
    }

    fn stop_with(&mut self, reason: impl FnOnce(&mut dyn GuestLauncher) -> StopReason) {
        // A frozen guest cannot shut down in an orderly way; it is let run first.
        if self.state() == SessionState::Frozen {
            if let Err(why) = self.launcher.thaw() {
                eprintln!("lifecycle: the frozen guest was not resumed before stopping: {why}");
            }
        }
        self.shared.set_vm_id(None);
        self.shared.set_problem(None);
        self.shared.set(SessionState::Stopping, None);
        self.shared.set_sender(None);
        if let Some(mut connection) = self.connection.take() {
            connection.close();
        }
        // The guest's tasks end with it, so their windows close even when it died.
        self.shared.clear_tasks();
        self.shared.set_guest_memory(None);
        match self.launcher.stop(self.config.stop_timeout) {
            Ok(()) => {
                let reason = reason(self.launcher.as_mut());
                self.shared.set(SessionState::Stopped, Some(reason))
            }
            Err(why) => self
                .shared
                .set(SessionState::Stopping, Some(StopReason::StopFailed(why))),
        }
    }
}
