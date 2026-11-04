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
//! - Whether the host is short of memory ([`Session::host_memory_low`]) goes to the guest
//!   whenever it changes, and when the agent connects while the host is short. The guest
//!   then ends its cached app processes and drops its caches, so the host gets memory back.
//! - The guest's power HAL hints at the performance it needs; each hint goes to every
//!   [`Session::watch_power_hints`] watcher, and [`GuestPower::Stopped`] when the guest stops,
//!   which ends every mode and boost it asked for.
//!
//! The web links apps in the guest open go to every [`Session::watch_links`] watcher, which
//! opens them in the host's browser, unless the user lets them open in the guest
//! ([`Session::links_in_android`]). Only http and https links pass, and only a few in a row.
//! Files from the host go to the guest with [`Session::open_file`], in chunks paced by the
//! agent's progress reports.
//!
//! While the guest traces input ([`Session::set_clock_trace`]), the session also pairs the
//! host's clock with the guest's once a second (see [`crate::clock_trace`]).
//!
//! The session also keeps the host's and the guest's text clipboards in step while sharing is
//! on ([`Session::share_clipboard`], on by default). Text the host copies
//! ([`Session::host_clipboard`]) goes to the guest while it is `Active` or `Dozing`, and the
//! host's latest text goes to it again each time it gets there, as when Android starts. Text
//! the guest copies goes to every [`Session::watch_clipboard`] watcher. Text that came from one
//! side is never sent back to it, and text longer than
//! [`MAX_CLIPBOARD_BYTES`](crate::clipboard::MAX_CLIPBOARD_BYTES) does not cross.
//!
//! The guest's notifications go to every [`Session::watch_notifications`] watcher, checked
//! and limited in rate ([`crate::notifications`]); when the guest stops, each one shown is
//! removed. What the user does with one ([`Session::notification_action`]) goes back to the
//! guest: a click first wakes a guest in Doze, as a shown window does, so the app opens on a
//! screen that is on, and a dismissal waits for a frozen guest to be thawed.
//! APKs from the host go to the guest with [`Session::install_apk`], streamed in chunks paced
//! by the agent's progress reports, one install at a time; a second waits for the first.
//!
//! The widgets the agent hosts go to every [`Session::watch_widgets`] watcher, with what each
//! one shows ([`crate::widgets`]). They are kept while the guest is stopped, so their windows
//! stay, and the list the agent sends on each connection says which are still there. The
//! gallery ([`Session::widget_providers`]) and placing a widget ([`Session::add_widget`]) ask
//! the agent and wait for its answer. A widget's frames come while the guest is in Doze too:
//! the agent draws them at a low rate without waking the guest. A press in a widget's window
//! wakes a guest in Doze, as a click on a notification does, since it may open an app.

use std::collections::HashMap;
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body};
use dsa_transport::{Connection, FrameSender, Listener, TransportError};

use crate::accessibility::{
    AccessibilityWatch, ActionRequest, ActionResult, GuestAccessibility, TreeUpdate,
};
use crate::adb_bridge::{AdbBridge, BridgeStream, GuestAdb};
use crate::apk::{self, InstallOutcome, InstalledPackage};
use crate::clipboard;
use crate::clock_trace::ClockTrace;
use crate::files::{self, FileAction, FileTransfer};
use crate::ime::{ImeCursor, ImeEvent};
use crate::input::PointerEvent;
use crate::keys::KeyEvent;
use crate::links::{self, LinkGate};
use crate::memory::{GuestMemory, GuestMemoryReport};
use crate::notifications::{self, Notification, NotificationAction, NotificationEvent, RateLimit};
use crate::power::PowerState;
use crate::power_hints::PowerHint;
use crate::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest, TaskTable, TaskWatch};
use crate::widgets::{
    self, Placement, Widget, WidgetEvent, WidgetFrame, WidgetProvider, WidgetSize,
};

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
    /// A file was not sent, or the guest did not take it; the detail says why.
    FileRefused(String),
    /// A widget was not placed, or the agent did not answer about widgets; the detail says
    /// why.
    WidgetRefused(String),
    /// The agent did not answer a question in time; the detail says which.
    NoAnswer(String),
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::NotActive(state) => write!(f, "the session is {state}, not Active"),
            SessionError::Transport(error) => write!(f, "control channel: {error}"),
            SessionError::Gone => write!(f, "the session has shut down"),
            SessionError::FreezeFailed(why) => write!(f, "the guest was not frozen: {why}"),
            SessionError::InstallRefused(why) => f.write_str(why),
            SessionError::FileRefused(why) => f.write_str(why),
            SessionError::WidgetRefused(why) => f.write_str(why),
            SessionError::NoAnswer(why) => f.write_str(why),
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
    /// Whether the host is short of memory changed.
    HostMemoryLow(bool),
    /// Freeze the guest after this long in Doze, or never.
    FreezeAfter(Option<Duration>),
    /// Freeze the guest now.
    Freeze(Sender<Result<(), SessionError>>),
    /// Whether web links open in the guest rather than on the host.
    LinksInAndroid(bool),
    /// The host's clipboard got new text.
    HostClipboard(String),
    /// Whether the clipboards are shared.
    ShareClipboard(bool),
    /// The user did something with a notification on the host.
    Notification(NotificationAction, Sender<Result<(), SessionError>>),
    /// The user pressed in a widget's window: a guest in Doze wakes, as the press may open
    /// an app.
    WidgetPressed(Sender<()>),
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
    /// Who opens the web links the guest sends.
    link_watchers: Mutex<Vec<Sender<String>>>,
    /// The file transfers under way, by ID, each with who waits for the agent's progress.
    transfers: Mutex<HashMap<u32, Sender<control::FileProgress>>>,
    /// The ID of the last file transfer.
    last_transfer: AtomicU32,
    /// How to pair the host's clock with the guest's, when the platform traces input.
    clock_trace: Mutex<Option<ClockTrace>>,
    /// Who hears of the text the guest copies.
    clipboard_watchers: Mutex<Vec<Sender<String>>>,
    /// Who hears of the guest's notifications.
    notification_watchers: Mutex<Vec<Sender<NotificationEvent>>>,
    /// The install under way, by its transfer ID, with who waits for the agent's word on it.
    installs: Mutex<HashMap<u32, Sender<InstallEvent>>>,
    /// The transfer ID of the last install.
    last_install: AtomicU32,
    /// Held for the length of an install, so a second one waits for the first.
    install_turn: Mutex<()>,
    /// The widgets the agent hosts and who is watching them.
    widgets: Mutex<WidgetHub>,
    /// The requests about widgets waiting for the agent's answer, by request ID.
    widget_requests: Mutex<HashMap<u32, Sender<WidgetAnswer>>>,
    /// The ID of the last request about widgets.
    last_widget_request: AtomicU32,
    /// The questions about installed packages waiting for the agent's answer, by request ID.
    package_queries: Mutex<HashMap<u32, Sender<Option<InstalledPackage>>>>,
    /// The ID of the last question about an installed package.
    last_package_query: AtomicU32,
    /// Who watches each task's accessibility tree.
    accessibility: Mutex<AccessibilityHub>,
    /// Who hears of the guest's power hints.
    power_hint_watchers: Mutex<Vec<Sender<GuestPower>>>,
    /// Whether the user's developer mode is on, which each agent that connects is told.
    developer_mode: AtomicBool,
}

/// Opens a stream to the guest's adbd in the VM with the given runtime ID: the platform's
/// part of developer mode's bridge.
pub type AdbConnect = Arc<dyn Fn(&str) -> std::io::Result<Box<dyn BridgeStream>> + Send + Sync>;

/// What the guest's power HAL asks of the host's processors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestPower {
    /// One hint, as the guest sent it.
    Hint(PowerHint),
    /// The guest stopped: none of its modes or boosts is on any longer.
    Stopped,
}

/// What the agent answers about widgets.
enum WidgetAnswer {
    Providers(Vec<WidgetProvider>, bool),
    Added(Result<i32, String>),
}

/// The widgets the agent hosts, each with what it last showed, and who is watching them,
/// under one lock, so a new watcher's starting point and the changes it receives afterwards
/// never overlap or leave a gap.
#[derive(Default)]
struct WidgetHub {
    widgets: std::collections::BTreeMap<i32, (WidgetProvider, Option<WidgetFrame>)>,
    watchers: Vec<Sender<WidgetEvent>>,
}

impl WidgetHub {
    fn publish(&mut self, event: WidgetEvent) {
        self.watchers
            .retain(|watcher| watcher.send(event.clone()).is_ok());
    }

    fn snapshot(&self) -> Vec<WidgetEvent> {
        let mut events = Vec::new();
        for (widget_id, (provider, frame)) in &self.widgets {
            events.push(WidgetEvent::Placed(Widget {
                widget_id: *widget_id,
                provider: provider.clone(),
                placement: Placement::default(),
            }));
            if let Some(frame) = frame {
                events.push(WidgetEvent::Frame(frame.clone()));
            }
        }
        events
    }
}

/// The widgets the agent hosts now, and every change to them from then on. A placed
/// widget's placement is left at its default: the service keeps where its window is.
pub struct WidgetWatch {
    pub current: Vec<WidgetEvent>,
    pub events: Receiver<WidgetEvent>,
}

/// What the agent says about an install under way.
enum InstallEvent {
    Progress(u64),
    Done(InstallOutcome),
}

/// The watchers of each task's accessibility tree. The guest reads a task's tree while the
/// task has at least one.
#[derive(Default)]
struct AccessibilityHub {
    next_watch: u64,
    watchers: HashMap<i32, Vec<(u64, Sender<GuestAccessibility>)>>,
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

    /// Hands a web link the guest opened to every link watcher still listening. False when
    /// nobody was listening.
    fn publish_link(&self, url: &str) -> bool {
        let mut watchers = self.link_watchers.lock().unwrap_or_else(|e| e.into_inner());
        watchers.retain(|watcher| watcher.send(url.to_string()).is_ok());
        !watchers.is_empty()
    }

    /// Hands the agent's progress on a transfer to whoever sends that file.
    fn file_progress(&self, progress: control::FileProgress) {
        let transfers = self.transfers.lock().unwrap_or_else(|e| e.into_inner());
        match transfers.get(&progress.transfer_id) {
            Some(waiting) => {
                let _ = waiting.send(progress);
            }
            None => eprintln!(
                "files: dropped the agent's progress on transfer {}, which is not under way",
                progress.transfer_id
            ),
        }
    }

    /// Hands what the guest sent about a task's accessibility to the task's watchers.
    fn publish_accessibility(&self, task_id: i32, message: GuestAccessibility) {
        let mut hub = self.accessibility.lock().unwrap_or_else(|e| e.into_inner());
        match hub.watchers.get_mut(&task_id) {
            Some(watchers) => watchers.retain(|(_, w)| w.send(message.clone()).is_ok()),
            None => eprintln!(
                "accessibility: dropped the guest's tree of task {task_id}, which nobody watches"
            ),
        }
    }

    /// The tasks whose accessibility tree is watched.
    fn watched_tasks(&self) -> Vec<i32> {
        let hub = self.accessibility.lock().unwrap_or_else(|e| e.into_inner());
        let mut tasks: Vec<i32> = hub.watchers.keys().copied().collect();
        tasks.sort();
        tasks
    }

    /// Hands text the guest copied to every clipboard watcher still listening.
    fn publish_clipboard(&self, text: &str) {
        self.clipboard_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(text.to_string()).is_ok());
    }

    /// Hands the agent's answer about widgets to whoever asked. An answer nobody waits for
    /// is dropped and logged.
    fn widget_answer(&self, request_id: u32, answer: WidgetAnswer) {
        let requests = self
            .widget_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match requests.get(&request_id) {
            Some(waiting) => {
                let _ = waiting.send(answer);
            }
            None => eprintln!(
                "widgets: dropped the agent's answer to request {request_id}, which nobody waits for"
            ),
        }
    }

    /// Hands a change to the guest's notifications to every watcher still listening.
    fn publish_power(&self, event: GuestPower) {
        self.power_hint_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(event).is_ok());
    }

    fn publish_notification(&self, event: NotificationEvent) {
        self.notification_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(event.clone()).is_ok());
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
    /// How the platform reaches the guest's adbd, when it can.
    adb_connect: Mutex<Option<AdbConnect>>,
    /// Developer mode's loopback port and the bridge on it, while developer mode is on.
    adb_bridge: Mutex<Option<(u16, AdbBridge)>>,
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
            link_watchers: Mutex::new(Vec::new()),
            transfers: Mutex::new(HashMap::new()),
            last_transfer: AtomicU32::new(0),
            clock_trace: Mutex::new(None),
            clipboard_watchers: Mutex::new(Vec::new()),
            notification_watchers: Mutex::new(Vec::new()),
            installs: Mutex::new(HashMap::new()),
            last_install: AtomicU32::new(0),
            install_turn: Mutex::new(()),
            widgets: Mutex::new(WidgetHub::default()),
            widget_requests: Mutex::new(HashMap::new()),
            last_widget_request: AtomicU32::new(0),
            package_queries: Mutex::new(HashMap::new()),
            last_package_query: AtomicU32::new(0),
            accessibility: Mutex::new(AccessibilityHub::default()),
            power_hint_watchers: Mutex::new(Vec::new()),
            developer_mode: AtomicBool::new(false),
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
            host_memory_low: false,
            host_memory_pending: false,
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
            links_in_android: false,
            link_gate: LinkGate::default(),
            next_clock_probe: Instant::now(),
            clipboard_sharing: true,
            host_clipboard: None,
            clipboard_last: None,
            notifications: HashMap::new(),
            notification_limit: RateLimit::new(Instant::now()),
            dismissals: Vec::new(),
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
            adb_connect: Mutex::new(None),
            adb_bridge: Mutex::new(None),
        }
    }

    /// Lets developer mode reach the guest's adbd through `connect`, which opens a stream to
    /// it in the VM with the runtime ID it is given.
    pub fn set_adb_connect(&self, connect: AdbConnect) {
        *self.adb_connect.lock().unwrap_or_else(|e| e.into_inner()) = Some(connect);
    }

    /// Turns developer mode on or off, as the user's setting says. On, the guest starts adbd
    /// and the host listens on `127.0.0.1:port` (port 0 takes any free port) and bridges each
    /// connection to it; off, the port closes and the guest stops adbd. It fails, and changes
    /// nothing, when the port cannot be opened or the platform cannot reach the guest's adbd.
    pub fn developer_mode(&self, on: bool, port: u16) -> Result<(), String> {
        let mut bridge = self.adb_bridge.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            let connect = self
                .adb_connect
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .ok_or_else(|| {
                    "this host cannot reach adb in Android, so developer mode stays off".to_string()
                })?;
            if bridge.as_ref().map(|(open, _)| *open) != Some(port) {
                // A new port: the old one closes first, so the same port can be chosen again.
                *bridge = None;
                let shared = Arc::clone(&self.shared);
                let guest: GuestAdb = Arc::new(move || {
                    let vm_id = shared
                        .vm_id
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .clone()
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "Android is not running",
                            )
                        })?;
                    connect(&vm_id)
                });
                let opened = AdbBridge::start(port, guest).map_err(|error| {
                    format!(
                        "cannot listen for adb on 127.0.0.1:{port} ({error}); another program \
                         may use it, so choose another port (dsa settings set adb-port <port>)"
                    )
                })?;
                eprintln!(
                    "developer mode: on; adb connects to {} (adb connect {})",
                    opened.address(),
                    opened.address()
                );
                *bridge = Some((port, opened));
            }
        } else if bridge.take().is_some() {
            eprintln!("developer mode: off; the adb port is closed");
        }
        drop(bridge);
        if self.shared.developer_mode.swap(on, Ordering::SeqCst) != on {
            send_quietly(
                &self.shared,
                &self.commands,
                Body::DeveloperMode(control::DeveloperMode { on }),
            );
        }
        Ok(())
    }

    /// The address adb connects to while developer mode is on.
    pub fn adb_address(&self) -> Option<std::net::SocketAddr> {
        self.adb_bridge
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(_, bridge)| bridge.address())
    }

    /// Pairs the host's clock with the guest's while the guest runs and `trace` says the
    /// input trace is on: a probe goes to the guest once a second, and each answer becomes a
    /// `clock` line. `None` stops it.
    pub fn set_clock_trace(&self, trace: Option<ClockTrace>) {
        *self
            .shared
            .clock_trace
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = trace;
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

    /// Records whether the host is short of memory. The guest is told now if it is up, and
    /// when its agent connects while the host is short, so it gives back what it can spare.
    pub fn host_memory_low(&self, low: bool) {
        let _ = self.commands.send(Command::HostMemoryLow(low));
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

    /// The guest's power hints from now on, and each time the guest stops. The watch lasts
    /// across restarts of the guest.
    pub fn watch_power_hints(&self) -> Receiver<GuestPower> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .power_hint_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// The guest's notifications from now on: each one posted or changed, and each one gone.
    /// The watch lasts across restarts of the guest.
    pub fn watch_notifications(&self) -> Receiver<NotificationEvent> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .notification_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Passes what the user did with a notification to the guest. Opening one wakes a guest
    /// in Doze or frozen first. Needs a running guest that has the notification; one the
    /// guest has not shown is ignored.
    pub fn notification_action(&self, action: NotificationAction) -> Result<(), SessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Notification(action, reply_tx))
            .map_err(|_| SessionError::Gone)?;
        reply_rx.recv().map_err(|_| SessionError::Gone)?
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

    /// Sends web links the guest opens to the host (`false`, the default) or lets them open
    /// in apps in the guest (`true`), as the user's setting says. A connected agent is told
    /// at once, and each agent that connects is told when links open in the guest.
    pub fn links_in_android(&self, on: bool) {
        let _ = self.commands.send(Command::LinksInAndroid(on));
    }

    /// The web links the guest opens from now on, while they go to the host. Each is an http
    /// or https link ([`links::web_url`]). The watch lasts across restarts of the guest.
    pub fn watch_links(&self) -> Receiver<String> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .link_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Watches a task's accessibility tree, for a host accessibility client. The first watch
    /// of a task asks the guest to read the task's tree, and dropping the last tells it to
    /// stop, so the guest reads nothing nobody watches. A new watch of a task already watched
    /// asks the guest for a full tree again, which every watcher receives. The watch lasts
    /// across restarts of the guest: each agent that connects is asked again.
    pub fn watch_accessibility(&self, task_id: i32) -> AccessibilityWatch {
        let (tx, updates) = mpsc::channel();
        let id = {
            let mut hub = self
                .shared
                .accessibility
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            hub.next_watch += 1;
            let id = hub.next_watch;
            hub.watchers.entry(task_id).or_default().push((id, tx));
            id
        };
        eprintln!(
            "accessibility: a host client watches task {task_id}; asking Android for its tree"
        );
        send_quietly(
            &self.shared,
            &self.commands,
            Body::AccessibilityWatch(control::AccessibilityWatch { task_id, on: true }),
        );
        let shared = Arc::clone(&self.shared);
        let commands = self.commands.clone();
        AccessibilityWatch::new(updates, move || {
            let last = {
                let mut hub = shared
                    .accessibility
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let watchers = hub.watchers.entry(task_id).or_default();
                watchers.retain(|(watch, _)| *watch != id);
                let last = watchers.is_empty();
                if last {
                    hub.watchers.remove(&task_id);
                }
                last
            };
            if last {
                eprintln!(
                    "accessibility: no host client watches task {task_id}; Android stops reading it"
                );
                send_quietly(
                    &shared,
                    &commands,
                    Body::AccessibilityWatch(control::AccessibilityWatch { task_id, on: false }),
                );
            }
        })
    }

    /// Asks the guest to do `action` on a node of a task. The answer arrives on the task's
    /// watches. Needs a running guest.
    pub fn accessibility_action(
        &self,
        task_id: i32,
        action: &ActionRequest,
    ) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::AccessibilityAction(action.to_control(task_id))),
        }))
    }

    /// Sends `file` to the guest's agent, which asks the user which app opens it (or takes
    /// it, to share). Returns once the agent has the whole file and has asked; an error says
    /// why it did not get there. Needs a running guest.
    ///
    /// The file goes in chunks of [`files::CHUNK_BYTES`], never more than
    /// [`files::CHUNKS_IN_FLIGHT`] ahead of what the agent says it has stored.
    pub fn open_file(&self, file: &FileTransfer) -> Result<(), SessionError> {
        file.check().map_err(SessionError::FileRefused)?;
        let transfer_id = self
            .shared
            .last_transfer
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let (tx, progress) = mpsc::channel();
        self.shared
            .transfers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(transfer_id, tx);
        let result = self.send_file(transfer_id, file, &progress);
        self.shared
            .transfers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&transfer_id);
        result
    }

    fn send_file(
        &self,
        transfer_id: u32,
        file: &FileTransfer,
        progress: &Receiver<control::FileProgress>,
    ) -> Result<(), SessionError> {
        let size = file.content.len() as u64;
        let action = match file.action {
            FileAction::View => control::FileAction::View,
            FileAction::Send => control::FileAction::Send,
        };
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::OpenFile(control::OpenFile {
                transfer_id,
                name: file.name.clone(),
                size,
                action: action as i32,
            })),
        }))?;
        let window = files::CHUNK_BYTES as u64 * files::CHUNKS_IN_FLIGHT;
        let mut sent = 0u64;
        let mut stored = 0u64;
        let mut waited = Duration::ZERO;
        loop {
            while sent < size && sent < stored + window {
                let end = (sent + files::CHUNK_BYTES as u64).min(size);
                self.send(dsa_protocol::encode(&control::Envelope {
                    body: Some(Body::FileChunk(control::FileChunk {
                        transfer_id,
                        offset: sent,
                        data: file.content[sent as usize..end as usize].to_vec(),
                    })),
                }))?;
                sent = end;
            }
            match progress.recv_timeout(PROGRESS_POLL) {
                Ok(report) => {
                    waited = Duration::ZERO;
                    if !report.error.is_empty() {
                        return Err(SessionError::FileRefused(format!(
                            "Android did not take {}: {}",
                            file.name, report.error
                        )));
                    }
                    if report.received > size {
                        return Err(SessionError::FileRefused(format!(
                            "Android says it has {} bytes of {}, which has {size}",
                            report.received, file.name
                        )));
                    }
                    stored = stored.max(report.received);
                    if stored == size && sent == size {
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    let state = self.state();
                    if !matches!(
                        state,
                        SessionState::Active | SessionState::Dozing | SessionState::Frozen
                    ) {
                        return Err(SessionError::NotActive(state));
                    }
                    waited += PROGRESS_POLL;
                    if waited >= PROGRESS_TIMEOUT {
                        return Err(SessionError::FileRefused(format!(
                            "Android has not taken {} after {} s; it may be busy, try again",
                            file.name,
                            PROGRESS_TIMEOUT.as_secs()
                        )));
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return Err(SessionError::Gone),
            }
        }
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

    /// The widgets the agent hosts now, and every change to them from now on. The watch lasts
    /// across restarts of the guest.
    pub fn watch_widgets(&self) -> WidgetWatch {
        let (tx, events) = mpsc::channel();
        let mut hub = self
            .shared
            .widgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let current = hub.snapshot();
        hub.watchers.push(tx);
        WidgetWatch { current, events }
    }

    /// The widgets the agent hosts now, in ID order.
    pub fn widgets(&self) -> Vec<(i32, WidgetProvider)> {
        self.shared
            .widgets
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .widgets
            .iter()
            .map(|(id, (provider, _))| (*id, provider.clone()))
            .collect()
    }

    /// Registers a request about widgets and returns its ID and where its answer arrives.
    fn widget_request(&self) -> (u32, Receiver<WidgetAnswer>) {
        let request_id = self
            .shared
            .last_widget_request
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let (tx, rx) = mpsc::channel();
        self.shared
            .widget_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        (request_id, rx)
    }

    fn forget_widget_request(&self, request_id: u32) {
        self.shared
            .widget_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
    }

    /// What the guest has installed of `package`, if anything: its version and its signing
    /// certificates, asking the agent and waiting up to `timeout` for its answer. Needs an
    /// active session.
    pub fn package_state(
        &self,
        package: &str,
        timeout: Duration,
    ) -> Result<Option<InstalledPackage>, SessionError> {
        let request_id = self
            .shared
            .last_package_query
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let (tx, answer) = mpsc::channel();
        self.shared
            .package_queries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        let result = self
            .send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::QueryPackage(control::QueryPackage {
                    request_id,
                    package_name: package.to_string(),
                })),
            }))
            .and_then(|()| {
                answer.recv_timeout(timeout).map_err(|_| {
                    SessionError::NoAnswer(format!(
                        "Android did not say within {} seconds whether {package} is installed",
                        timeout.as_secs()
                    ))
                })
            });
        self.shared
            .package_queries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        result
    }

    /// The widgets the apps in the guest offer, in label order, asking the agent and waiting
    /// up to `timeout` for its whole answer. Needs an active session.
    pub fn widget_providers(&self, timeout: Duration) -> Result<Vec<WidgetProvider>, SessionError> {
        let (request_id, answers) = self.widget_request();
        let result = (|| {
            self.send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::ListWidgetProviders(control::ListWidgetProviders {
                    request_id,
                })),
            }))?;
            let deadline = Instant::now() + timeout;
            let mut providers: Vec<WidgetProvider> = Vec::new();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match answers.recv_timeout(left) {
                    Ok(WidgetAnswer::Providers(part, complete)) => {
                        for provider in part {
                            if providers.len() >= widgets::MAX_PROVIDERS {
                                break;
                            }
                            if !providers.iter().any(|p| p.provider == provider.provider) {
                                providers.push(provider);
                            }
                        }
                        if complete {
                            break;
                        }
                    }
                    Ok(WidgetAnswer::Added(_)) => {}
                    Err(_) => {
                        return Err(SessionError::WidgetRefused(format!(
                            "Android did not list its widgets within {} seconds",
                            timeout.as_secs()
                        )))
                    }
                }
            }
            providers.sort_by(|a, b| {
                a.label
                    .to_lowercase()
                    .cmp(&b.label.to_lowercase())
                    .then_with(|| a.provider.cmp(&b.provider))
            });
            Ok(providers)
        })();
        self.forget_widget_request(request_id);
        result
    }

    /// Places a new widget of `provider`, waiting up to `timeout` for the agent, and for the
    /// user when the widget's app asks them to set it up first. Needs an active session.
    pub fn add_widget(&self, provider: &str, timeout: Duration) -> Result<i32, SessionError> {
        if !widgets::valid_provider_name(provider) {
            return Err(SessionError::WidgetRefused(format!(
                "{provider:?} is not a widget's name; `dsa widgets list` shows them"
            )));
        }
        if self.widgets().len() >= widgets::MAX_WIDGETS {
            return Err(SessionError::WidgetRefused(format!(
                "{} widgets are placed already; remove one first",
                widgets::MAX_WIDGETS
            )));
        }
        let (request_id, answers) = self.widget_request();
        let result = (|| {
            self.send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::AddWidget(control::AddWidget {
                    request_id,
                    provider: provider.to_string(),
                })),
            }))?;
            let deadline = Instant::now() + timeout;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match answers.recv_timeout(left) {
                    Ok(WidgetAnswer::Added(Ok(widget_id))) => return Ok(widget_id),
                    Ok(WidgetAnswer::Added(Err(why))) => {
                        return Err(SessionError::WidgetRefused(why))
                    }
                    Ok(WidgetAnswer::Providers(..)) => {}
                    Err(_) => {
                        return Err(SessionError::WidgetRefused(format!(
                            "Android did not place the widget within {} seconds",
                            timeout.as_secs()
                        )))
                    }
                }
            }
        })();
        self.forget_widget_request(request_id);
        result
    }

    /// Removes a widget: the agent deletes it, and the watchers hear it is gone once it says
    /// so. A widget the guest does not have is removed from the host's list at once. Needs an
    /// active session to reach the guest.
    pub fn remove_widget(&self, widget_id: i32) -> Result<(), SessionError> {
        let result = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::RemoveWidget(control::RemoveWidget { widget_id })),
        }));
        // The window goes at once; the agent's list confirms it.
        let mut hub = self
            .shared
            .widgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if hub.widgets.remove(&widget_id).is_some() {
            hub.publish(WidgetEvent::Removed { widget_id });
        }
        result
    }

    /// Tells the agent the size of a widget's window. Needs an active session.
    pub fn resize_widget(&self, widget_id: i32, size: WidgetSize) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::ResizeWidget(size.to_control(widget_id))),
        }))
    }

    /// Passes pointer input in a widget's window to the agent. A press wakes a guest in Doze
    /// first. Needs an active session.
    pub fn widget_pointer(&self, pointer: control::WidgetPointer) -> Result<(), SessionError> {
        if pointer.action == control::PointerAction::Down as i32 {
            let (reply_tx, reply_rx) = mpsc::channel();
            if self.commands.send(Command::WidgetPressed(reply_tx)).is_ok() {
                let _ = reply_rx.recv();
            }
        }
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::WidgetPointer(pointer)),
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

/// Sends `body` to the guest without waiting for it to go out, from any thread: through the
/// connection's second handle when the guest can take it, else through the worker. A frame
/// for a guest that is not running is dropped; what it said is sent again when the agent
/// connects.
fn send_quietly(shared: &Shared, commands: &Sender<Command>, body: Body) {
    let frame = dsa_protocol::encode(&control::Envelope { body: Some(body) });
    match shared.send_direct(&frame) {
        Some(Ok(())) => {}
        Some(Err(error)) => {
            eprintln!("control channel: a message to the guest did not go out: {error}")
        }
        None => {
            // Nobody waits for the answer.
            let (reply, _) = mpsc::channel();
            let _ = commands.send(Command::Send(frame, reply));
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

/// How often a file transfer looks at the session while it waits for the agent.
const PROGRESS_POLL: Duration = Duration::from_millis(500);

/// How long a file transfer waits for the agent to store a chunk. Storing 128 KiB takes
/// milliseconds; this long means the agent is stuck or gone.
const PROGRESS_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// Whether the host is short of memory, as last recorded.
    host_memory_low: bool,
    /// Set when that changed while the guest was frozen, so it is told after the thaw.
    host_memory_pending: bool,
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
    /// Whether web links open in the guest, as the user last said.
    links_in_android: bool,
    /// Keeps a flood of links from reaching the host.
    link_gate: LinkGate,
    /// When the next clock probe goes to the guest, while the input trace is on.
    next_clock_probe: Instant,
    /// Whether the clipboards are shared, as the user last said.
    clipboard_sharing: bool,
    /// The host's latest clipboard text while sharing is on, for a guest that gets ready later.
    host_clipboard: Option<String>,
    /// The last text that crossed, either way, since the agent connected. Text equal to it is
    /// not sent, so what one side was given never comes back to it.
    clipboard_last: Option<String>,
    /// The guest's notifications the watchers were given, by key.
    notifications: HashMap<String, Notification>,
    /// How many notifications reach the watchers.
    notification_limit: RateLimit,
    /// Dismissals for a frozen guest, sent once it is thawed.
    dismissals: Vec<String>,
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
                        Ok(Command::Send(_, reply))
                        | Ok(Command::Freeze(reply))
                        | Ok(Command::Notification(_, reply)) => {
                            let _ = reply.send(Err(SessionError::NotActive(SessionState::Stopped)));
                        }
                        Ok(Command::WindowsShown(shown)) => self.set_shown(shown),
                        Ok(Command::Power(state)) => self.set_power(state),
                        Ok(Command::HostMemoryLow(low)) => self.set_host_memory_low(low),
                        Ok(Command::FreezeAfter(after)) => self.config.freeze_after = after,
                        Ok(Command::LinksInAndroid(on)) => self.set_links_in_android(on),
                        Ok(Command::HostClipboard(text)) => self.host_clipboard(text),
                        Ok(Command::ShareClipboard(on)) => self.share_clipboard(on),
                        Ok(Command::WidgetPressed(reply)) => {
                            let _ = reply.send(());
                        }
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
                        Ok(Command::Send(_, reply))
                        | Ok(Command::Freeze(reply))
                        | Ok(Command::Notification(_, reply)) => {
                            let _ =
                                reply.send(Err(SessionError::NotActive(SessionState::Stopping)));
                        }
                        Ok(Command::WindowsShown(shown)) => self.set_shown(shown),
                        Ok(Command::Power(state)) => self.set_power(state),
                        Ok(Command::HostMemoryLow(low)) => self.set_host_memory_low(low),
                        Ok(Command::FreezeAfter(after)) => self.config.freeze_after = after,
                        Ok(Command::LinksInAndroid(on)) => self.set_links_in_android(on),
                        Ok(Command::HostClipboard(text)) => self.host_clipboard(text),
                        Ok(Command::ShareClipboard(on)) => self.share_clipboard(on),
                        Ok(Command::WidgetPressed(reply)) => {
                            let _ = reply.send(());
                        }
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
            Command::HostMemoryLow(low) => {
                self.set_host_memory_low(low);
                true
            }
            Command::LinksInAndroid(on) => {
                self.set_links_in_android(on);
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
            Command::Notification(action, reply) => {
                let _ = reply.send(self.act_on_notification(action));
                self.running()
            }
            Command::WidgetPressed(reply) => {
                if matches!(self.state(), SessionState::Dozing | SessionState::Frozen) {
                    // The press may open an app, which opens on a screen that is on; the
                    // screen's word reaches the agent before the press does.
                    self.hidden_since = Instant::now();
                    self.wake();
                }
                let _ = reply.send(());
                self.running()
            }
        }
    }

    /// Takes a notification the guest posted or changed, and hands it to the watchers when its
    /// turn comes. One the same as what the watchers have is not handed again.
    fn guest_notification(&mut self, message: control::GuestNotification) {
        let notification = match notifications::checked(message) {
            Ok(notification) => notification,
            Err(why) => {
                eprintln!("notifications: dropped one from the guest: {why}");
                return;
            }
        };
        if self.notifications.get(&notification.key) == Some(&notification) {
            self.notification_limit.remove(&notification.key);
            return;
        }
        if let Some(due) = self.notification_limit.offer(notification, Instant::now()) {
            self.deliver_notification(due);
        }
    }

    fn deliver_notification(&mut self, notification: Notification) {
        self.notifications
            .insert(notification.key.clone(), notification.clone());
        self.shared
            .publish_notification(NotificationEvent::Posted(notification));
    }

    /// Hands on the notifications whose turn has come.
    fn tick_notifications(&mut self) {
        for due in self.notification_limit.due(Instant::now()) {
            self.deliver_notification(due);
        }
    }

    /// The guest says a notification is gone.
    fn guest_notification_removed(&mut self, key: String) {
        self.notification_limit.remove(&key);
        if self.notifications.remove(&key).is_some() {
            self.shared
                .publish_notification(NotificationEvent::Removed { key });
        }
    }

    /// Removes every notification the watchers have, as when the guest stops.
    fn clear_notifications(&mut self) {
        self.notification_limit.clear();
        self.dismissals.clear();
        let keys: Vec<String> = self.notifications.drain().map(|(key, _)| key).collect();
        for key in keys {
            self.shared
                .publish_notification(NotificationEvent::Removed { key });
        }
    }

    /// Hands part of the agent's widget gallery to whoever asked for it. Entries that do not
    /// pass the checks are dropped and logged.
    fn guest_widget_providers(&mut self, list: control::WidgetProviders) {
        let mut providers = Vec::new();
        for provider in list.providers.into_iter().take(widgets::MAX_PROVIDERS) {
            match widgets::checked_provider(provider) {
                Ok(provider) => providers.push(provider),
                Err(why) => eprintln!("widgets: dropped one from the gallery: {why}"),
            }
        }
        self.shared.widget_answer(
            list.request_id,
            WidgetAnswer::Providers(providers, list.complete),
        );
    }

    /// The agent describes a placed widget.
    fn guest_widget(&mut self, updated: control::WidgetUpdated) {
        if updated.widget_id <= 0 {
            eprintln!("widgets: dropped a widget without an ID");
            return;
        }
        let provider = match updated
            .provider
            .ok_or_else(|| "it names no provider".to_string())
            .and_then(widgets::checked_provider)
        {
            Ok(provider) => provider,
            Err(why) => {
                eprintln!("widgets: dropped widget {}: {why}", updated.widget_id);
                return;
            }
        };
        let mut hub = self
            .shared
            .widgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let known = hub.widgets.get(&updated.widget_id).map(|(p, _)| p.clone());
        if known.is_none() && hub.widgets.len() >= widgets::MAX_WIDGETS {
            eprintln!(
                "widgets: dropped widget {}: {} are placed already",
                updated.widget_id,
                widgets::MAX_WIDGETS
            );
            return;
        }
        let frame = hub
            .widgets
            .remove(&updated.widget_id)
            .and_then(|(_, frame)| frame);
        hub.widgets
            .insert(updated.widget_id, (provider.clone(), frame));
        // Described again on each connection, so the window manager sizes it again.
        hub.publish(WidgetEvent::Placed(Widget {
            widget_id: updated.widget_id,
            provider,
            placement: Placement::default(),
        }));
    }

    /// The agent says which widgets are placed; any other is gone.
    fn guest_widget_list(&mut self, ids: Vec<i32>) {
        let mut hub = self
            .shared
            .widgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let gone: Vec<i32> = hub
            .widgets
            .keys()
            .copied()
            .filter(|id| !ids.contains(id))
            .collect();
        for widget_id in gone {
            hub.widgets.remove(&widget_id);
            hub.publish(WidgetEvent::Removed { widget_id });
        }
        let mut listed = ids;
        listed.sort_unstable();
        listed.dedup();
        hub.publish(WidgetEvent::Listed(listed));
    }

    /// What a widget shows now. A frame of a widget the host does not know is dropped.
    fn guest_widget_frame(&mut self, frame: control::WidgetFrame) {
        let frame = match widgets::checked_frame(frame) {
            Ok(frame) => frame,
            Err(why) => {
                eprintln!("widgets: dropped a frame: {why}");
                return;
            }
        };
        let mut hub = self
            .shared
            .widgets
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match hub.widgets.get_mut(&frame.widget_id) {
            Some((_, last)) => *last = Some(frame.clone()),
            None => {
                eprintln!(
                    "widgets: dropped a frame of widget {}, which is not placed",
                    frame.widget_id
                );
                return;
            }
        }
        hub.publish(WidgetEvent::Frame(frame));
    }

    /// Passes what the user did with a notification to the guest.
    fn act_on_notification(&mut self, action: NotificationAction) -> Result<(), SessionError> {
        let state = self.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) || !self.agent_connected
        {
            return Err(SessionError::NotActive(state));
        }
        let Some(clearable) = self
            .notifications
            .get(action.key())
            .map(|shown| shown.clearable)
        else {
            eprintln!("notifications: the host acted on one the guest does not have; ignored");
            return Ok(());
        };
        match action {
            NotificationAction::Open { key } => {
                // The app opens on a screen that is on: the guest leaves Doze before the
                // click, which it reads after the screen's word on the same channel. The
                // screen stays on for the doze delay while the app's window comes.
                self.hidden_since = Instant::now();
                self.wake();
                if !self.running() {
                    return Err(SessionError::NotActive(self.state()));
                }
                self.send_body(Body::NotificationClicked(control::NotificationClicked {
                    key,
                }));
            }
            NotificationAction::Dismiss { key } => {
                if !clearable {
                    eprintln!(
                        "notifications: the host dismissed one its app keeps; it stays in the guest"
                    );
                    return Ok(());
                }
                if state == SessionState::Frozen {
                    // Not worth waking the guest for; it hears of it when it is thawed.
                    self.dismissals.push(key);
                } else {
                    self.send_body(Body::NotificationDismissed(
                        control::NotificationDismissed { key },
                    ));
                }
            }
        }
        Ok(())
    }

    /// Sends the dismissals that waited for the guest to be thawed.
    fn send_dismissals(&mut self) {
        for key in std::mem::take(&mut self.dismissals) {
            self.send_body(Body::NotificationDismissed(
                control::NotificationDismissed { key },
            ));
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

    /// Takes the user's word on where web links open, and tells a connected agent.
    fn set_links_in_android(&mut self, on: bool) {
        if on == self.links_in_android {
            return;
        }
        self.links_in_android = on;
        eprintln!(
            "links: web links open {}",
            if on { "in Android" } else { "on the host" }
        );
        if self.running() && self.agent_connected {
            self.send_body(Body::LinkHandling(control::LinkHandling { in_guest: on }));
        }
    }

    /// Takes a web link an app in the guest opened, and hands it to the link watchers if it is
    /// one the host may open.
    fn guest_link(&mut self, url: String) {
        if self.links_in_android {
            eprintln!("links: dropped a link from Android; links open in Android");
            return;
        }
        let url = match links::web_url(&url) {
            Ok(url) => url.to_string(),
            Err(why) => {
                eprintln!("links: dropped a link from Android: {why}");
                return;
            }
        };
        if !self.link_gate.allow(Instant::now()) {
            eprintln!(
                "links: dropped a link from Android: more than {} in {} s",
                links::LINKS_PER_WINDOW,
                links::LINK_WINDOW.as_secs()
            );
            return;
        }
        if !self.shared.publish_link(&url) {
            eprintln!(
                "links: dropped a link from Android: no window manager is running to open it"
            );
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
            eprintln!("control channel: a message to the guest did not go out: {error}");
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
            if std::mem::take(&mut self.host_memory_pending) {
                let low = self.host_memory_low;
                self.send_body(Body::HostMemory(control::HostMemory { low }));
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
        self.send_dismissals();
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

    /// Records whether the host is short of memory and tells a guest that can hear it.
    fn set_host_memory_low(&mut self, low: bool) {
        if self.host_memory_low == low {
            return;
        }
        self.host_memory_low = low;
        if low {
            eprintln!("lifecycle: the host is short of memory; asking the guest to give some back");
        }
        match self.state() {
            SessionState::Starting
            | SessionState::Stalled
            | SessionState::Active
            | SessionState::Dozing => {
                self.send_body(Body::HostMemory(control::HostMemory { low }));
            }
            SessionState::Frozen => self.host_memory_pending = true,
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
            if self.host_memory_low {
                self.send_body(Body::HostMemory(control::HostMemory { low: true }));
            }
            if !self.screen_on {
                self.send_body(Body::ScreenState(control::ScreenState { on: false }));
            }
            // A new agent takes links to go to the host.
            if self.links_in_android {
                self.send_body(Body::LinkHandling(control::LinkHandling { in_guest: true }));
            }
            // A guest boots with adbd stopped.
            if self.shared.developer_mode.load(Ordering::SeqCst) {
                self.send_body(Body::DeveloperMode(control::DeveloperMode { on: true }));
            }
            // A new agent reads no task's accessibility tree until it is asked.
            for task_id in self.shared.watched_tasks() {
                self.send_body(Body::AccessibilityWatch(control::AccessibilityWatch {
                    task_id,
                    on: true,
                }));
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
                            self.host_memory_pending = false;
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
            Command::Send(_, reply) | Command::Freeze(reply) | Command::Notification(_, reply) => {
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
            Command::HostMemoryLow(low) => {
                self.set_host_memory_low(low);
                true
            }
            Command::LinksInAndroid(on) => {
                self.set_links_in_android(on);
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
            Command::WidgetPressed(reply) => {
                let _ = reply.send(());
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
            self.tick_clock();
            self.offer_host_clipboard();
            self.tick_notifications();
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

    /// Sends the guest's daemon a clock probe when one is due and the input trace is on. A
    /// frozen guest cannot answer, so it is not asked.
    fn tick_clock(&mut self) {
        let trace = self
            .shared
            .clock_trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(trace) = trace else {
            return;
        };
        let now = Instant::now();
        if !trace.is_on() || now < self.next_clock_probe || self.state() == SessionState::Frozen {
            return;
        }
        self.next_clock_probe = now + trace.interval;
        let host_sent_ns = trace.clock.now().as_nanos() as u64;
        self.send_body(Body::ClockProbe(control::ClockProbe { host_sent_ns }));
    }

    /// The guest's daemon answered a clock probe: logs the pairing.
    fn clock_answered(&mut self, answer: control::ClockAnswer) {
        let trace = self
            .shared
            .clock_trace
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(trace) = trace {
            trace.answered(
                Duration::from_nanos(answer.host_sent_ns),
                Duration::from_nanos(answer.guest_mono_ns),
            );
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
            Some(Body::OpenLink(link)) => self.guest_link(link.url),
            Some(Body::FileProgress(progress)) => self.shared.file_progress(progress),
            Some(Body::ClipboardText(clip)) => self.guest_clipboard(clip.text),
            Some(Body::Notification(notification)) => self.guest_notification(notification),
            Some(Body::NotificationRemoved(removed)) => {
                self.guest_notification_removed(removed.key)
            }
            Some(Body::InstallProgress(progress)) => self.shared.install_event(
                progress.transfer_id,
                InstallEvent::Progress(progress.received),
            ),
            Some(Body::InstallResult(result)) => self.shared.install_event(
                result.transfer_id,
                InstallEvent::Done(InstallOutcome::from_control(&result)),
            ),
            Some(Body::WidgetProviders(list)) => self.guest_widget_providers(list),
            Some(Body::WidgetAdded(added)) => self.shared.widget_answer(
                added.request_id,
                WidgetAnswer::Added(if added.widget_id > 0 {
                    Ok(added.widget_id)
                } else if added.error.trim().is_empty() {
                    Err("Android did not place the widget".into())
                } else {
                    Err(added.error.chars().take(512).collect())
                }),
            ),
            Some(Body::WidgetUpdated(updated)) => self.guest_widget(updated),
            Some(Body::WidgetList(list)) => self.guest_widget_list(list.widget_ids),
            Some(Body::PackageState(state)) => {
                let installed = InstalledPackage::from_control(&state);
                let queries = self
                    .shared
                    .package_queries
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                match queries.get(&state.request_id) {
                    Some(waiting) => {
                        let _ = waiting.send(installed);
                    }
                    None => eprintln!(
                        "packages: dropped the agent's answer to question {}, which nobody \
                         waits for",
                        state.request_id
                    ),
                }
            }
            Some(Body::WidgetFrame(frame)) => self.guest_widget_frame(frame),
            Some(Body::GuestBoot(boot)) => self.guest_boot(boot),
            Some(Body::ClockAnswer(answer)) => self.clock_answered(answer),
            Some(Body::AgentConnected(_)) => {
                eprintln!("control channel: dropping a message that stops at the guest's daemon");
            }
            Some(Body::AccessibilityTree(tree)) => {
                let task_id = tree.task_id;
                match TreeUpdate::from_control(tree) {
                    Ok(update) => self
                        .shared
                        .publish_accessibility(task_id, GuestAccessibility::Tree(update)),
                    Err(why) => eprintln!(
                        "accessibility: dropped the guest's tree of task {task_id}: {why}"
                    ),
                }
            }
            Some(Body::AccessibilityActionResult(result)) => {
                let task_id = result.task_id;
                self.shared.publish_accessibility(
                    task_id,
                    GuestAccessibility::ActionResult(ActionResult::from_control(result)),
                );
            }
            Some(Body::PowerHint(hint)) => match PowerHint::from_control(&hint) {
                Ok(hint) => self.shared.publish_power(GuestPower::Hint(hint)),
                Err(why) => eprintln!("power: dropped a hint from the guest: {why}"),
            },
            Some(Body::ScreenState(_))
            | Some(Body::PowerState(_))
            | Some(Body::HostMemory(_))
            | Some(Body::CloseTask(_))
            | Some(Body::LaunchApp(_))
            | Some(Body::ResizeTask(_))
            | Some(Body::SetTaskDensity(_))
            | Some(Body::PointerEvent(_))
            | Some(Body::KeyEvent(_))
            | Some(Body::ImeComposition(_))
            | Some(Body::ImeCommit(_))
            | Some(Body::LinkHandling(_))
            | Some(Body::OpenFile(_))
            | Some(Body::FileChunk(_))
            | Some(Body::ClipboardSharing(_))
            | Some(Body::NotificationClicked(_))
            | Some(Body::NotificationDismissed(_))
            | Some(Body::InstallApk(_))
            | Some(Body::ApkChunk(_))
            | Some(Body::CancelInstall(_))
            | Some(Body::QueryPackage(_))
            | Some(Body::RequestGuestMemory(_))
            | Some(Body::ListWidgetProviders(_))
            | Some(Body::AddWidget(_))
            | Some(Body::RemoveWidget(_))
            | Some(Body::ResizeWidget(_))
            | Some(Body::WidgetPointer(_))
            | Some(Body::AccessibilityWatch(_))
            | Some(Body::AccessibilityAction(_))
            | Some(Body::ClockProbe(_)) => {
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
        // So do its notifications.
        self.clear_notifications();
        // And the modes and boosts its power HAL asked for.
        self.shared.publish_power(GuestPower::Stopped);
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
