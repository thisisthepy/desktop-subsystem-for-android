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
//! - The guest display's refresh rate follows the user's fastest monitor
//!   ([`Session::set_monitor_refresh`]) and the host's battery saver, which caps it at 60 Hz
//!   ([`crate::guest_refresh`]). It goes to the guest whenever it changes and each time the
//!   agent connects, once the monitors are known.
//! - Whether the host is short of memory ([`Session::host_memory_low`]) goes to the guest
//!   whenever it changes, and when the agent connects while the host is short. The guest
//!   then ends its cached app processes and drops its caches, so the host gets memory back.
//! - When the host is about to sleep ([`Session::host_sleeping`]), the guest's screen goes off
//!   and its daemon puts its pending data on disk and says so. When the host wakes
//!   ([`Session::host_woke`]), the guest's clock is set to the host's, and its screen comes back
//!   on if it was on, so the session returns to the state it had. Time asleep does not count
//!   towards the freeze and boot timers.
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
//! APKs from the host go to the guest with [`Session::install_apk`], and apps made of several
//! APKs and expansion files with [`Session::install_parts`], streamed in chunks paced by the
//! agent's progress reports, one install at a time; a second waits for the first.
//!
//! The widgets the agent hosts go to every [`Session::watch_widgets`] watcher, with what each
//! one shows ([`crate::widgets`]). They are kept while the guest is stopped, so their windows
//! stay, and the list the agent sends on each connection says which are still there. The
//! gallery ([`Session::widget_providers`]) and placing a widget ([`Session::add_widget`]) ask
//! the agent and wait for its answer. A widget's frames come while the guest is in Doze too:
//! the agent draws them at a low rate without waking the guest. A press in a widget's window
//! wakes a guest in Doze, as a click on a notification does, since it may open an app.
//!
//! The guest's log flows only while someone reads it ([`Session::watch_log`]): the first
//! reader asks the guest's channel daemon to read logd, and the last one leaving tells it to
//! stop. The records arrive on a port of their own ([`Session::serve_log_port`]), so they
//! never queue behind or ahead of the control channel's messages, and they are kept only in
//! a small ring in memory ([`crate::guest_log`]). A one-off dump ([`Session::dump_log`]) and
//! the guest's diagnostics ([`Session::collect_diagnostics`]) come the same way.
//!
//! The documents apps in the guest print come from the agent's print service in pieces, are
//! put together and checked ([`crate::print`]), and go whole to one
//! [`Session::watch_print_jobs`] watcher, which prints them on the host. Its outcome
//! ([`Session::print_result`]) goes back to the guest. A job canceled in the guest, or one the
//! guest's agent no longer has because it went away, is withdrawn from the watcher.
//!
//! The requests of the agent's text-to-speech engine, speech recognition service and spell
//! checker go to the first [`Session::serve_language`] client, which answers them with the
//! host's own services ([`crate::language`]); a request nobody serves fails at once.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body};
use dsa_transport::{Connection, FrameSender, Listener, TransportError};

use crate::accessibility::{
    AccessibilityWatch, ActionRequest, ActionResult, GuestAccessibility, TreeUpdate,
};
use crate::adb_bridge::{AdbBridge, BridgeStream, GuestAdb};
use crate::adb_discovery::{AdbAdvertise, AdbServerLink, AutoConnectState, AutoConnectWatcher};
use crate::apk::{self, InstallOutcome, InstallPart, InstalledPackage};
use crate::app_audio::{self, AppAudioHub, AppAudioWatch, AppStream, VolumeChange};
use crate::apps::UninstallOutcome;
use crate::arm_translation::GuestArmTranslation;
use crate::background::{
    self, BackgroundApp, BackgroundCommand, BackgroundEvent, BackgroundHub, BackgroundWatch,
    MediaCommand, MediaSession,
};
use crate::clipboard;
use crate::clock_trace::ClockTrace;
use crate::drag::{self, DragChunk, DragItem, DragOffer, DragOut, DragRead};
use crate::file_share::{self, ShareHost, Sharing, SharingNow};
use crate::files::{self, FileAction, FileTransfer};
use crate::gamepads::{GamepadEvent, GamepadRumble, GamepadTable};
use crate::gapps::GuestGapps;
use crate::geocode::{self, GeocodeError, GeocodeErrorKind, GeocodeOutcome, GeocodeQuery};
use crate::guest_log::{
    serve_log_connection, DiagnosticsEvent, LogDumpEvent, LogEntry, LogFilter, LogHub,
    LogPortMessage, LogWatch, STREAM_TAIL,
};
use crate::ime::{ImeCursor, ImeEditor, ImeEvent};
use crate::input::{PointerAction, PointerEvent};
use crate::keys::KeyEvent;
use crate::language;
use crate::links::{self, LinkGate};
use crate::location::{LocationEvent, LocationHub, Wanted};
use crate::memory::{GrowthAction, GuestMemory, GuestMemoryReport, GuestPressure, MemoryGrowth};
use crate::native_abi::{self, NativeLibraries, RunAsArmOutcome, RunAsArmStatus};
use crate::notifications::{self, Notification, NotificationAction, NotificationEvent, RateLimit};
use crate::power::PowerState;
use crate::power_hints::PowerHint;
use crate::print::{self, PrintEvent, PrintJob, PrintOutcome};
use crate::root_approval::RootApproval;
use crate::root_elevation::{AppLevel, Level};
use crate::share::{ShareOutcome, ShareRequest};
use crate::system_settings::SystemSettings;
use crate::tasks::{
    AppInfo, AppSettingsPage, AppShortcut, Bounds, PinShortcutRequest, StatusBarAppearance, Task,
    TaskEvent, TaskOrientation, TaskRequest, TaskTable, TaskWatch,
};
use crate::widgets::{
    self, Placement, Widget, WidgetEvent, WidgetFrame, WidgetProvider, WidgetSize,
};

mod dns_port;
mod host_services;
pub use host_services::{KeyMintKeyState, RestoreOutcome};

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

    /// The memory the running guest started with and may grow to, in MiB, when the launcher
    /// can change it while the guest runs; `None` when its memory is fixed.
    fn memory_range_mb(&self) -> Option<(u64, u64)> {
        None
    }

    /// Sets the running guest's memory to `mb` MiB: more is added to it, less is taken back.
    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String> {
        Err(format!("this kind of guest cannot be resized to {mb} MiB"))
    }

    /// A handle that resizes the running guest's memory from a thread of its own, for a
    /// platform whose resize can take long (Hyper-V answered one after 45 s). The session's
    /// thread then keeps serving windows, input and requests meanwhile. `None`, the default,
    /// has the session call [`GuestLauncher::set_memory_mb`] on its own thread, for a resize
    /// that returns at once.
    fn memory_resizer(&mut self) -> Option<Box<dyn MemoryResizer>> {
        None
    }

    /// Growing the guest's memory did not work, for the reason given. The launcher starts the
    /// next guest at a fixed size instead.
    fn memory_growth_failed(&mut self, why: &str) {
        let _ = why;
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

/// What the host's system says that the guest follows.
enum SystemInput {
    Settings(SystemSettings),
    Gamepad(GamepadEvent),
}

/// What the guest display's refresh rate follows.
#[derive(Debug, Clone, Copy)]
enum RefreshInput {
    /// The refresh rate of the user's fastest monitor, in hertz.
    MonitorHz(u32),
    /// Whether the host's battery saver is on.
    BatterySaver(bool),
    /// Whether any task window is being resized live changed.
    LiveResize,
}

enum Command {
    Start,
    Stop,
    Send(Vec<u8>, Sender<Result<(), SessionError>>),
    Shutdown,
    /// Whether any task window is shown.
    WindowsShown(bool),
    /// The host's power changed.
    Power(PowerState),
    /// The user's fastest monitor or the host's battery saver changed.
    Refresh(RefreshInput),
    /// Whether the host is short of memory changed.
    HostMemoryLow(bool),
    /// The host's system settings or one of its game controllers changed.
    System(SystemInput),
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
    /// The user acted on an app that runs in the background, or pressed a media key.
    Background(BackgroundRequest, Sender<Result<(), SessionError>>),
    /// The user pressed in a widget's window: a guest in Doze wakes, as the press may open
    /// an app.
    WidgetPressed(Sender<()>),
    /// The host is about to sleep; the sender is told once the guest's data is on disk.
    HostSleep(Sender<()>),
    /// The host woke, with its wall clock then.
    HostWoke(SystemTime),
    /// A resize of the guest's memory, run on a thread of its own, finished.
    MemoryResized(ResizeOutcome),
}

/// How a resize of the guest's memory went, for the session to take in.
struct ResizeOutcome {
    /// Which boot of the guest asked for it.
    generation: u64,
    before: u64,
    mb: u64,
    /// The guest's memory when it was asked for, for the log.
    available_mb: u64,
    stall_percent: f64,
    result: Result<(), String>,
}

/// Resizes the running guest's memory from a thread other than the session's
/// ([`GuestLauncher::memory_resizer`]).
pub trait MemoryResizer: Send + 'static {
    /// Sets the guest's memory to `mb` MiB, as [`GuestLauncher::set_memory_mb`] does. It may
    /// take as long as the platform takes; the session goes on meanwhile.
    fn set_memory_mb(&mut self, mb: u64) -> Result<(), String>;
}

/// Says the device name a guest is given: the computer's name, as its last boot found it.
pub type DeviceNameSource = Arc<dyn Fn() -> Option<String> + Send + Sync>;

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
    /// The VM's memory as the host last set it, in MiB, while the platform resizes it (see
    /// [`MemoryGrowth`]); `None` when it does not, and while the guest is stopped.
    vm_memory_mb: Mutex<Option<u64>>,
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
    /// The task windows being resized live, which hold the guest's refresh rate down until
    /// they end. The caller that sends a resize marks it, and the worker reads it for the rate.
    live_resizes: Mutex<crate::guest_refresh::LiveResizes>,
    /// Who hears of the text the guest copies.
    clipboard_watchers: Mutex<Vec<Sender<String>>>,
    /// Who plays the rumble effects the guest's apps play on the host's controllers.
    rumble_watchers: Mutex<Vec<Sender<GamepadRumble>>>,
    /// Who hears of the guest's notifications.
    notification_watchers: Mutex<Vec<Sender<NotificationEvent>>>,
    /// Who prints the documents the guest prints.
    print_watchers: Mutex<Vec<Sender<PrintEvent>>>,
    /// The guest's print jobs on their way, and those a watcher has.
    prints: Mutex<print::Assembler>,
    /// Who answers the address lookups of the guest's apps.
    geocode_watchers: Mutex<Vec<Sender<GeocodeQuery>>>,
    /// Who shows what the guest's apps share with the host.
    share_watchers: Mutex<Vec<Sender<ShareRequest>>>,
    /// The shares a watcher has and has not answered yet, by their ID.
    open_shares: Mutex<HashSet<u32>>,
    /// The apps that run in the background, their media sessions, and who watches them.
    background: Mutex<BackgroundHub>,
    /// Which app each output of the guest's sound plays and its volume, and who watches it.
    app_audio: Mutex<AppAudioHub>,
    /// The install under way, by its transfer ID, with who waits for the agent's word on it.
    installs: Mutex<HashMap<u32, Sender<InstallEvent>>>,
    /// The transfer ID of the last install.
    last_install: AtomicU32,
    /// Held for the length of an install, so a second one waits for the first.
    install_turn: Mutex<()>,
    /// Who waits for the control daemon's word on ARM translation, while one exchange is under
    /// way.
    arm_translation: Mutex<Option<Sender<control::ArmTranslationState>>>,
    /// Held for the length of an exchange about ARM translation, so they go one at a time.
    arm_translation_turn: Mutex<()>,
    /// Who waits for the control daemon's word on the Google apps, while one exchange is under
    /// way.
    gapps: Mutex<Option<Sender<control::GappsState>>>,
    /// Held for the length of an exchange about the Google apps, so they go one at a time.
    gapps_turn: Mutex<()>,
    /// Who waits for the control daemon's part of an add-on image an older DSA left on the
    /// user's disk, while one read is under way.
    addon_image: Mutex<Option<Sender<control::AddonImagePart>>>,
    /// Held for the length of one read of an add-on image, so they go one at a time.
    addon_image_turn: Mutex<()>,
    /// Who waits for the agent's word on the Google Services Framework Android ID.
    device_id: Mutex<Option<Sender<String>>>,
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
    /// The drags out of a window waiting for the agent's offer, by request ID.
    drag_offers: Mutex<HashMap<u32, Sender<control::DragOffer>>>,
    /// The reads of dragged files waiting for the agent's data, by request ID.
    drag_reads: Mutex<HashMap<u32, Sender<control::DragData>>>,
    /// The ID of the last request about a drag.
    last_drag_request: AtomicU32,
    /// The uninstalls waiting for the agent's answer, by request ID.
    uninstalls: Mutex<HashMap<u32, Sender<UninstallOutcome>>>,
    /// The ID of the last uninstall.
    last_uninstall: AtomicU32,
    /// The installs of an app again with another ABI waiting for the agent's answer, by
    /// request ID.
    reinstalls: Mutex<HashMap<u32, Sender<control::ReinstallResult>>>,
    /// The ID of the last install of an app again, whoever asked for it.
    last_reinstall: AtomicU32,
    /// Who waits for the agent's next list of installed apps.
    app_refreshes: Mutex<Vec<Sender<()>>>,
    /// Who watches each task's accessibility tree.
    accessibility: Mutex<AccessibilityHub>,
    /// Who hears of the guest's power hints.
    power_hint_watchers: Mutex<Vec<Sender<GuestPower>>>,
    /// Whether the guest wants the host's location, and who hears of it.
    location: Mutex<LocationHub>,
    /// Who serves the guest's speech and spelling, and which of its requests each one has.
    language: Mutex<language::relay::Relay>,
    /// Whether the user's developer mode is on, which each agent that connects is told.
    developer_mode: AtomicBool,
    /// Who reads the guest's log, its recent records, and the dumps and diagnostics under way.
    logs: Mutex<LogHub>,
    /// The running boot's secret, which a connection on the log port must prove it knows.
    boot_secret: Mutex<Option<BootSecret>>,
    /// Set when the session ends, so the log port's threads end with it.
    log_port_stop: Arc<AtomicBool>,
    /// The guest's connections on the DNS port that passed the handshake and are still open:
    /// while there is one, the host's resolver answers the guest's names.
    dns_connections: AtomicUsize,
    /// Whether the platform serves the DNS port, so the guest's names can be answered here.
    dns_port_served: AtomicBool,
    /// Which of the user's host folders their Android shows, which each agent that connects
    /// is told.
    file_sharing: SharingNow,
    /// The host's file server once the platform serves one: its port, and the folder in the
    /// guest's shared storage the shared folders go under.
    share_server: Mutex<Option<(u32, String)>>,
    /// The token in the address of the guest's shared storage on the host, while the host
    /// serves it.
    android_files_token: Mutex<Option<String>>,
    /// Says the device name the guest's agent is given each time it connects.
    device_name: Mutex<Option<DeviceNameSource>>,
    /// The user's root elevation settings, and whether the running boot started with the
    /// feature on.
    root_config: Mutex<RootElevationNow>,
    /// Who decides the su calls of the guest's apps, once the platform says, with a way to
    /// send each decision from the thread that waits for the administrator.
    root_approval: Mutex<Option<(Arc<RootApproval>, Sender<Command>)>>,
    /// The backups, passkeys and KeyMint root key the platform serves the guest.
    host_services: Mutex<host_services::HostServices>,
}

/// The root elevation settings as the guest is told them.
#[derive(Debug, Clone, Default)]
struct RootElevationNow {
    /// The computer's switch, as it stands now.
    on: bool,
    /// The apps with a level the user set, off included: apps that share an Android user ID
    /// are one group, and one of them set to off hides su from the whole group, which only
    /// the guest can tell apart by uid.
    apps: Vec<AppLevel>,
    /// The switch as it stood when the running boot started; `None` while no boot runs. The
    /// kernel's su guard and the relay are set up at boot, so a boot that started with the
    /// feature off keeps it off until the next start, and is told nothing.
    boot_on: Option<bool>,
    /// Whether the switch was turned off while the running boot runs. Off applies at once,
    /// and the boot stays off until the next start even when the switch is turned on again.
    off_since_boot: bool,
    /// What the running boot was last told, so an unchanged config is not sent again.
    sent: Option<control::RootElevationConfig>,
}

impl RootElevationNow {
    /// What the running boot's daemon is told now: `None` when the boot started with the
    /// feature off, or no boot runs. Turning the switch off applies at once (su is hidden from
    /// every app); turning it on applies from the next start.
    fn config(&self) -> Option<control::RootElevationConfig> {
        if self.boot_on != Some(true) {
            return None;
        }
        Some(control::RootElevationConfig {
            enabled: self.on && !self.off_since_boot,
            apps: self
                .apps
                .iter()
                .map(|app| control::RootApp {
                    package: app.package.clone(),
                    // The guest knows each package's uid; the host does not.
                    uid: 0,
                    level: match app.level {
                        Level::Off => control::RootLevel::Off,
                        Level::Ask => control::RootLevel::Ask,
                        Level::Remember => control::RootLevel::Remember,
                    }
                    .into(),
                })
                .collect(),
        })
    }
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
    /// Whether any task window is shown on the host, as the host said it.
    WindowsShown(bool),
    /// The host passed the user's input (a press, a release, a scroll or a key) to the guest.
    /// The host hears of it before Android does, so its boost starts here.
    HostInput,
}

/// What the user asks of an app that runs in the background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackgroundRequest {
    App(String, BackgroundCommand),
    Media(String, MediaCommand),
}

/// What the agent answers about widgets.
enum WidgetAnswer {
    Providers(Vec<WidgetProvider>, bool),
    Added(Result<i32, String>),
    /// How a widget's setup, opened again, ended: Ok when the user finished it.
    Reconfigured(Result<(), String>),
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
    /// The tasks whose first tree since their newest watch began the service's log has
    /// named, so a watch that never gets one shows by the missing line.
    announced: HashSet<i32>,
}

/// The guest's tasks and who is watching them, under one lock, so a new watcher's starting
/// point and the changes it receives afterwards never overlap or leave a gap.
#[derive(Default)]
struct TaskHub {
    table: TaskTable,
    watchers: Vec<Sender<TaskEvent>>,
}

impl Shared {
    /// What the guest is told of the shared folders, once the host serves them.
    fn file_sharing_body(&self) -> Option<Body> {
        let (port, location) = self
            .share_server
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let sharing = self
            .file_sharing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Some(Body::FileSharing(control::FileSharing {
            on: sharing.on,
            port,
            location,
            folders: if sharing.on {
                sharing.folders
            } else {
                Vec::new()
            },
        }))
    }

    /// The root elevation config the running boot's daemon has yet to be told: `None` when
    /// it has been told this one already, or is told nothing (see [`RootElevationNow`]).
    /// `again` forgets what was sent, for a daemon or an agent that connected anew.
    fn root_elevation_body(&self, again: bool) -> Option<Body> {
        let mut now = self.root_config.lock().unwrap_or_else(|e| e.into_inner());
        if again {
            now.sent = None;
        }
        let config = now.config()?;
        if now.sent.as_ref() == Some(&config) {
            return None;
        }
        now.sent = Some(config.clone());
        Some(Body::RootElevationConfig(config))
    }

    /// Notes whether a boot runs, and with the switch as it stands as that boot's.
    fn root_elevation_boot(&self, running: bool) {
        let mut now = self.root_config.lock().unwrap_or_else(|e| e.into_inner());
        now.boot_on = running.then_some(now.on);
        now.off_since_boot = false;
        now.sent = None;
    }

    /// What the guest's daemon is told to serve its shared storage, while the host shows it.
    fn android_files_body(&self) -> Option<Body> {
        let token = self
            .android_files_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        Some(Body::AndroidFiles(control::AndroidFiles {
            port: file_share::ANDROID_FILES_PORT,
            token,
        }))
    }

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

    /// Forgets why an earlier boot stopped, once a newer one is up and well. Shown beside a
    /// healthy Android, the old cause reads as a current fault: a refusal from the boot before
    /// an update went on reading "was built from a different protocol definition" after the
    /// updated guest had connected.
    fn forget_last_reason(&self) {
        self.status.lock().unwrap_or_else(|e| e.into_inner()).1 = None;
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

    /// Keeps the guest's latest memory report. A report that follows one from the same boot
    /// says how fast memory went back to the host in between ([`GuestMemory::following`]).
    fn set_guest_memory(&self, memory: Option<(GuestMemory, Instant)>) {
        let mut kept = self.guest_memory.lock().unwrap_or_else(|e| e.into_inner());
        *kept = match (memory, kept.take()) {
            (Some((memory, at)), Some((before, then))) => Some((
                memory.following(&before, at.saturating_duration_since(then)),
                at,
            )),
            (memory, _) => memory,
        };
    }

    fn set_vm_memory_mb(&self, mb: Option<u64>) {
        *self.vm_memory_mb.lock().unwrap_or_else(|e| e.into_inner()) = mb;
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

    /// The installed app `package`, as the guest last listed it.
    fn installed_app(&self, package: &str) -> Option<AppInfo> {
        let hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        hub.table
            .installed()?
            .into_iter()
            .find(|app| app.package_name == package)
    }

    /// The ID of the next install of an app again.
    fn next_reinstall(&self) -> u32 {
        self.last_reinstall
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
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

    /// Records the launcher shortcuts of `package`, and hands the list of installed apps that
    /// carries them to the watchers when they changed it.
    fn publish_shortcuts(&self, package: &str, shortcuts: Vec<AppShortcut>) {
        let event = {
            let mut hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
            hub.table.shortcuts_event(package, shortcuts)
        };
        if let Some(event) = event {
            self.publish_task(event);
        }
    }

    /// Tells whoever waits for a fresh list of installed apps that it arrived.
    fn apps_refreshed(&self) {
        let waiting: Vec<Sender<()>> = self
            .app_refreshes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        for waiter in waiting {
            let _ = waiter.send(());
        }
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
    /// Hands the control daemon's word on ARM translation to whoever waits for it.
    fn arm_translation_event(&self, state: control::ArmTranslationState) {
        let waiting = self
            .arm_translation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match waiting.as_ref() {
            Some(waiting) => {
                let _ = waiting.send(state);
            }
            None => eprintln!("arm translation: dropped the guest's word, which nobody waits for"),
        }
    }

    /// Hands the control daemon's word on the Google apps to whoever waits for it.
    fn gapps_event(&self, state: control::GappsState) {
        let waiting = self.gapps.lock().unwrap_or_else(|e| e.into_inner());
        match waiting.as_ref() {
            Some(waiting) => {
                let _ = waiting.send(state);
            }
            None => eprintln!("gapps: dropped the guest's word, which nobody waits for"),
        }
    }

    /// Hands the control daemon's part of an add-on image to whoever reads it.
    fn addon_image_event(&self, part: control::AddonImagePart) {
        let waiting = self.addon_image.lock().unwrap_or_else(|e| e.into_inner());
        match waiting.as_ref() {
            Some(waiting) => {
                let _ = waiting.send(part);
            }
            None => eprintln!("add-ons: dropped a part of an image, which nobody reads"),
        }
    }

    /// Hands the agent's Google Services Framework Android ID to whoever asked for it.
    fn device_id_event(&self, id: String) {
        if let Some(waiting) = self
            .device_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = waiting.send(id);
        }
    }

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
        if let GuestAccessibility::Tree(tree) = &message {
            if hub.watchers.contains_key(&task_id) && hub.announced.insert(task_id) {
                eprintln!(
                    "accessibility: Android sent task {task_id}'s tree ({} nodes)",
                    tree.nodes.len()
                );
            }
        }
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
    fn publish_rumble(&self, rumble: GamepadRumble) {
        self.rumble_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(rumble).is_ok());
    }

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

    /// Hands a whole print job to the first watcher still listening, and only to it, so one
    /// dialog shows for it. False when nobody listens.
    fn hand_print_job(&self, job: PrintJob) -> bool {
        let mut delivered = false;
        self.print_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| {
                if delivered {
                    return true;
                }
                let sent = watcher.send(PrintEvent::Job(job.clone())).is_ok();
                delivered = sent;
                sent
            });
        delivered
    }

    /// Hands a lookup to the first watcher still listening, and only to it. False when nobody
    /// listens.
    fn hand_geocode_query(&self, query: GeocodeQuery) -> bool {
        hand_to_first(&self.geocode_watchers, query)
    }

    /// Hands a share to the first watcher still listening, and only to it, so one share UI
    /// shows for it. False when nobody listens.
    fn hand_share(&self, request: ShareRequest) -> bool {
        let share_id = request.share_id;
        let handed = hand_to_first(&self.share_watchers, request);
        if handed {
            self.open_shares
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(share_id);
        }
        handed
    }

    /// Tells every watcher a print job is gone; only the one that has it acts on it.
    fn withdraw_print_job(&self, job_id: u32) {
        self.print_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|watcher| watcher.send(PrintEvent::Withdrawn { job_id }).is_ok());
    }

    fn language(&self) -> std::sync::MutexGuard<'_, language::relay::Relay> {
        self.language.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// What the guest wants of the host's location now; every watcher hears a change.
    fn set_location(&self, wanted: Wanted) {
        self.location
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set(wanted);
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
    /// How the platform reaches the server of the guest's shared storage, when it can.
    android_files_connect: Mutex<Option<AdbConnect>>,
    /// The loopback bridge to the guest's shared storage, once a client has asked for it.
    android_files_bridge: Mutex<Option<AdbBridge>>,
    /// The user's auto-connect setting: whether developer mode's port is attached to the
    /// user's adb server by itself. On unless the user turned it off.
    adb_auto_connect: AtomicBool,
    /// What attaches developer mode's port to the user's adb server, once the platform says
    /// how to reach that server.
    adb_watcher: Mutex<Option<AutoConnectWatcher>>,
    /// How the platform advertises developer mode's port over mDNS, when it can.
    adb_advertise: Mutex<Option<AdbAdvertise>>,
    /// The advertisement of developer mode's port and the port it names, while it lasts.
    adb_advert: Mutex<Option<(u16, Box<dyn Send>)>>,
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
            vm_memory_mb: Mutex::new(None),
            problem: Mutex::new(None),
            link_watchers: Mutex::new(Vec::new()),
            transfers: Mutex::new(HashMap::new()),
            last_transfer: AtomicU32::new(0),
            clock_trace: Mutex::new(None),
            live_resizes: Mutex::new(Default::default()),
            clipboard_watchers: Mutex::new(Vec::new()),
            rumble_watchers: Mutex::new(Vec::new()),
            notification_watchers: Mutex::new(Vec::new()),
            print_watchers: Mutex::new(Vec::new()),
            prints: Mutex::new(print::Assembler::new()),
            geocode_watchers: Mutex::new(Vec::new()),
            share_watchers: Mutex::new(Vec::new()),
            open_shares: Mutex::new(HashSet::new()),
            background: Mutex::new(BackgroundHub::default()),
            app_audio: Mutex::new(AppAudioHub::default()),
            installs: Mutex::new(HashMap::new()),
            last_install: AtomicU32::new(0),
            install_turn: Mutex::new(()),
            arm_translation: Mutex::new(None),
            arm_translation_turn: Mutex::new(()),
            gapps: Mutex::new(None),
            gapps_turn: Mutex::new(()),
            addon_image: Mutex::new(None),
            addon_image_turn: Mutex::new(()),
            device_id: Mutex::new(None),
            widgets: Mutex::new(WidgetHub::default()),
            widget_requests: Mutex::new(HashMap::new()),
            last_widget_request: AtomicU32::new(0),
            package_queries: Mutex::new(HashMap::new()),
            last_package_query: AtomicU32::new(0),
            drag_offers: Mutex::new(HashMap::new()),
            drag_reads: Mutex::new(HashMap::new()),
            last_drag_request: AtomicU32::new(0),
            uninstalls: Mutex::new(HashMap::new()),
            last_uninstall: AtomicU32::new(0),
            reinstalls: Mutex::new(HashMap::new()),
            last_reinstall: AtomicU32::new(0),
            app_refreshes: Mutex::new(Vec::new()),
            accessibility: Mutex::new(AccessibilityHub::default()),
            power_hint_watchers: Mutex::new(Vec::new()),
            location: Mutex::new(LocationHub::default()),
            language: Mutex::new(language::relay::Relay::new()),
            developer_mode: AtomicBool::new(false),
            logs: Mutex::new(LogHub::default()),
            boot_secret: Mutex::new(None),
            log_port_stop: Arc::new(AtomicBool::new(false)),
            dns_connections: AtomicUsize::new(0),
            dns_port_served: AtomicBool::new(false),
            file_sharing: Arc::new(Mutex::new(Sharing::default())),
            share_server: Mutex::new(None),
            android_files_token: Mutex::new(None),
            device_name: Mutex::new(None),
            root_config: Mutex::new(RootElevationNow::default()),
            root_approval: Mutex::new(None),
            host_services: Mutex::new(host_services::HostServices::default()),
        });
        let freeze_delay = config.freeze_after.unwrap_or(DEFAULT_FREEZE_AFTER);
        let (commands, command_rx) = mpsc::channel();
        let incoming = Arc::new(Mutex::new(None));
        let worker = Worker {
            own_commands: commands.clone(),
            memory_generation: 0,
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
            monitor_hz: None,
            battery_saver: false,
            refresh_sent: None,
            refresh_pending: false,
            host_memory_low: false,
            host_memory_pending: false,
            system_settings: None,
            gamepads: GamepadTable::new(),
            system_pending: false,
            memory_growth: None,
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
            offered_reinstalls: HashMap::new(),
            host_asleep: None,
            sleep_waiters: Vec::new(),
            clock_pending: None,
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
            android_files_connect: Mutex::new(None),
            android_files_bridge: Mutex::new(None),
            adb_auto_connect: AtomicBool::new(true),
            adb_watcher: Mutex::new(None),
            adb_advertise: Mutex::new(None),
            adb_advert: Mutex::new(None),
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
        let open = bridge.as_ref().map(|(_, bridge)| bridge.address().port());
        drop(bridge);
        self.follow_adb_port(open);
        if self.shared.developer_mode.swap(on, Ordering::SeqCst) != on {
            send_quietly(
                &self.shared,
                &self.commands,
                Body::DeveloperMode(control::DeveloperMode { on }),
            );
        }
        Ok(())
    }

    /// Lets developer mode attach its port to the user's adb server, reached through `link`,
    /// so adb lists the guest without `adb connect`. It watches from now on, while developer
    /// mode and the auto-connect setting are both on.
    pub fn set_adb_server(&self, link: AdbServerLink) {
        let shared = Arc::clone(&self.shared);
        // Android answers adb once it has booted with its agent connected; frozen, starting or
        // stopping, it does not.
        let guest_ready: Arc<dyn Fn() -> bool + Send + Sync> = Arc::new(move || {
            matches!(
                shared.status.lock().unwrap_or_else(|e| e.into_inner()).0,
                SessionState::Active | SessionState::Dozing
            )
        });
        match AutoConnectWatcher::start(link, guest_ready, crate::adb_discovery::POLL_INTERVAL) {
            Ok(watcher) => {
                watcher.set_auto_connect(self.adb_auto_connect.load(Ordering::SeqCst));
                watcher.set_developer_mode(self.adb_address().map(|address| address.port()));
                *self.adb_watcher.lock().unwrap_or_else(|e| e.into_inner()) = Some(watcher);
            }
            Err(error) => {
                eprintln!("adb auto-connect: cannot watch the adb server ({error}); adb connect still works")
            }
        }
    }

    /// Lets developer mode advertise its port over mDNS through `advertise`, while it is on.
    pub fn set_adb_advertise(&self, advertise: AdbAdvertise) {
        *self.adb_advertise.lock().unwrap_or_else(|e| e.into_inner()) = Some(advertise);
        self.follow_adb_port(self.adb_address().map(|address| address.port()));
    }

    /// Turns the user's auto-connect setting on or off. Off, the host sends the adb server
    /// nothing more, and a device already attached stays as it is.
    pub fn adb_auto_connect(&self, on: bool) {
        self.adb_auto_connect.store(on, Ordering::SeqCst);
        if let Some(watcher) = self
            .adb_watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            watcher.set_auto_connect(on);
        }
    }

    /// Where auto-connect stands: off, waiting for an adb server or for Android, attached, or
    /// detached by the user.
    pub fn adb_auto_connect_state(&self) -> AutoConnectState {
        self.adb_watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map_or(AutoConnectState::Off, AutoConnectWatcher::state)
    }

    /// Points auto-connect and the mDNS advertisement at developer mode's port, or ends both
    /// when it is closed.
    fn follow_adb_port(&self, open: Option<u16>) {
        if let Some(watcher) = self
            .adb_watcher
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            watcher.set_developer_mode(open);
        }
        let mut advert = self.adb_advert.lock().unwrap_or_else(|e| e.into_inner());
        if advert.as_ref().map(|(port, _)| *port) == open {
            return;
        }
        // The old advertisement ends before a new one names another port.
        *advert = None;
        let Some(port) = open else {
            return;
        };
        let advertise = self
            .adb_advertise
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(handle) = advertise.and_then(|advertise| advertise(port)) {
            eprintln!(
                "developer mode: advertised {} over mDNS as {} on 127.0.0.1 only",
                crate::adb_discovery::mdns_instance_name(port),
                crate::adb_discovery::MDNS_SERVICE_TYPE
            );
            *advert = Some((port, handle));
        }
    }

    /// Shares the user's host folders named in `folders` with the guest while `on`, as the
    /// user's file sharing setting says. The guest mounts them, or takes them away, as soon as
    /// it is told; the file server refuses a folder from the moment it is no longer shared.
    pub fn share_folders(&self, on: bool, folders: Vec<String>) {
        let wanted = Sharing { on, folders };
        let changed = {
            let mut sharing = self
                .shared
                .file_sharing
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let changed = *sharing != wanted;
            *sharing = wanted;
            changed
        };
        if changed {
            if let Some(body) = self.shared.file_sharing_body() {
                send_quietly(&self.shared, &self.commands, body);
            }
        }
    }

    /// Sets the user's root elevation settings: the computer's switch and the apps with a
    /// level the user set, off included. The running Android is told at once, so a changed level applies
    /// without a restart; so does turning the switch off. Turning it on applies from the next
    /// start of Android, as the kernel's su guard and the relay are set up at boot. Each boot
    /// is told again once its agent connects.
    pub fn set_root_elevation_config(&self, on: bool, apps: Vec<AppLevel>) {
        {
            let mut now = self
                .shared
                .root_config
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            now.on = on;
            if !on && now.boot_on.is_some() {
                now.off_since_boot = true;
            }
            now.apps = apps;
            now.apps.sort_by(|a, b| a.package.cmp(&b.package));
            now.apps.dedup_by(|a, b| a.package == b.package);
        }
        if let Some(body) = self.shared.root_elevation_body(false) {
            send_quietly(&self.shared, &self.commands, body);
        }
    }

    /// The folders shared now, as the user's setting says.
    pub fn sharing(&self) -> Sharing {
        self.shared
            .file_sharing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Serves the user's host folders to the guest on `listener` for the life of the session,
    /// with `host` saying where each folder is and running the server with the user's rights.
    /// Only a connection that proves it knows the running boot's secret gets in.
    pub fn serve_file_shares(&self, listener: Box<dyn Listener>, host: Arc<dyn ShareHost>) {
        *self
            .shared
            .share_server
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((file_share::SHARE_PORT, host.location()));
        let sharing = Arc::clone(&self.shared.file_sharing);
        let secret_of = Arc::clone(&self.shared);
        let stop_of = Arc::clone(&self.shared.log_port_stop);
        let spawned = thread::Builder::new()
            .name("dsa-file-shares".into())
            .spawn(move || {
                file_share::serve_shares(
                    listener,
                    host,
                    sharing,
                    Arc::new(move || {
                        secret_of
                            .boot_secret
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone()
                    }),
                    Arc::new(move || stop_of.load(Ordering::Relaxed)),
                )
            });
        if let Err(error) = spawned {
            eprintln!("file sharing: the file server cannot be served: {error}");
        }
    }

    /// Lets the host show the guest's shared storage through `connect`, which opens a stream
    /// to the guest's server of it ([`file_share::ANDROID_FILES_PORT`]) in the VM with the
    /// runtime ID it is given.
    pub fn set_android_files_connect(&self, connect: AdbConnect) {
        *self
            .android_files_connect
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(connect);
    }

    /// The address the host's file manager opens the guest's shared storage at, as a WebDAV
    /// URL on the loopback address. The first call starts the guest if it is stopped, opens
    /// the loopback port, and has the guest serve its shared storage; the address then holds
    /// for the life of the session.
    pub fn android_files(&self) -> Result<String, String> {
        self.android_files_under(None)
    }

    /// As [`Session::android_files`], with the shared storage served under `token`. When it
    /// was served under another token, the guest is told the new one, and the old address
    /// stops working.
    pub fn android_files_with_token(&self, token: &str) -> Result<String, String> {
        self.android_files_under(Some(token))
    }

    fn android_files_under(&self, wanted: Option<&str>) -> Result<String, String> {
        let connect = self
            .android_files_connect
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .ok_or_else(|| "this host cannot reach Android's files".to_string())?;
        if self.state() == SessionState::Stopped {
            self.start();
        }
        let mut bridge = self
            .android_files_bridge
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if bridge.is_none() {
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
            let opened = AdbBridge::start(0, guest).map_err(|error| {
                format!("cannot open a loopback port for Android's files: {error}")
            })?;
            let token = match wanted {
                Some(token) => token.to_string(),
                None => dsa_protocol::auth::to_hex(&dsa_protocol::auth::nonce()?),
            };
            *self
                .shared
                .android_files_token
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(token);
            *bridge = Some(opened);
            if let Some(body) = self.shared.android_files_body() {
                send_quietly(&self.shared, &self.commands, body);
            }
        } else if let Some(token) = wanted {
            let changed = {
                let mut current = self
                    .shared
                    .android_files_token
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let changed = current.as_deref() != Some(token);
                if changed {
                    *current = Some(token.to_string());
                }
                changed
            };
            if changed {
                if let Some(body) = self.shared.android_files_body() {
                    send_quietly(&self.shared, &self.commands, body);
                }
            }
        }
        let address = bridge
            .as_ref()
            .map(AdbBridge::address)
            .ok_or_else(|| "the loopback port for Android's files closed".to_string())?;
        let token = self
            .shared
            .android_files_token
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
        Ok(format!("http://{address}/{token}/"))
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
    ///
    /// The memory the guest was given is the VM's size as the host last set it, when the
    /// platform resizes the VM. The guest counts its online memory blocks, and memory that was
    /// hot-added and then ballooned back to the host stays online in the guest, so its count
    /// alone takes that memory for the kernel's own: on the owner's PC, a VM grown to 3840 MiB
    /// and shrunk back to 2560 MiB showed 1357 MiB "reserved by the kernel at boot", where the
    /// kernel had kept 77 MiB and the host had the other 1280 MiB back.
    pub fn guest_memory(&self) -> Option<GuestMemoryReport> {
        let vm_mb = *self
            .shared
            .vm_memory_mb
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.shared
            .guest_memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(memory, at)| GuestMemoryReport {
                memory: memory.given(vm_mb),
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
        self.install_apk_for(name, size, None, content)
    }

    /// Installs an APK as [`install_apk`](Self::install_apk) does, with its native code for
    /// `abi` (`x86_64` or `arm64-v8a`), which the agent gives the package installer as its ABI
    /// override. `None` leaves the choice to the package manager.
    pub fn install_apk_for(
        &self,
        name: &str,
        size: u64,
        abi: Option<&str>,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, SessionError> {
        apk::check_name(name)
            .and_then(|()| apk::check_size(size))
            .map_err(SessionError::InstallRefused)?;
        self.install(name, size, abi, &[], content)
    }

    /// Installs an app made of several files in the guest: the APKs of `parts` go into one
    /// install session, with their native code for `abi` as in
    /// [`install_apk_for`](Self::install_apk_for), and the agent copies the expansion files
    /// among them into the app's folder under `Android/obb/` once the app is installed. Their
    /// bytes are read from `content` one after another, in the order of `parts`; `name` is the
    /// bundle they come from, for the log. Otherwise as [`install_apk`](Self::install_apk).
    pub fn install_parts(
        &self,
        name: &str,
        parts: &[InstallPart],
        abi: Option<&str>,
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, SessionError> {
        let size = apk::check_parts(name, parts).map_err(SessionError::InstallRefused)?;
        self.install(name, size, abi, parts, content)
    }

    fn install(
        &self,
        name: &str,
        size: u64,
        abi: Option<&str>,
        parts: &[InstallPart],
        content: &mut dyn Read,
    ) -> Result<InstallOutcome, SessionError> {
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
        eprintln!(
            "install: sending {name} ({size} bytes, {} files) as install {transfer_id}{}",
            parts.len().max(1),
            abi.map(|abi| format!(", for {abi}")).unwrap_or_default()
        );
        let announce = control::InstallApk {
            transfer_id,
            name: name.to_string(),
            size,
            abi: abi.unwrap_or_default().to_string(),
            parts: parts.iter().map(InstallPart::to_control).collect(),
        };
        let result = self.send_apk(announce, content, &events);
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

    /// Announces the install `install` and sends its content, paced by the agent's progress.
    fn send_apk(
        &self,
        install: control::InstallApk,
        content: &mut dyn Read,
        events: &Receiver<InstallEvent>,
    ) -> Result<InstallOutcome, SessionError> {
        let transfer_id = install.transfer_id;
        let size = install.size;
        let name = install.name.clone();
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::InstallApk(install)),
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

    /// Asks the guest's control daemon to remove the ARM translation package's image an older
    /// DSA left on the user's disk.
    pub fn remove_arm_translation(&self) -> Result<GuestArmTranslation, SessionError> {
        let state = self.arm_translation_exchange(Body::ArmTranslationRemove(
            control::ArmTranslationRemove {},
        ))?;
        if state.error.is_empty() {
            Ok(guest_arm_translation(state))
        } else {
            Err(SessionError::InstallRefused(state.error))
        }
    }

    /// Asks the guest's control daemon where ARM translation stands. Needs a guest that is
    /// active or dozing: a frozen one is not thawed for it.
    pub fn arm_translation(&self) -> Result<GuestArmTranslation, SessionError> {
        let state = self.state();
        if !matches!(state, SessionState::Active | SessionState::Dozing) {
            return Err(SessionError::NotActive(state));
        }
        self.arm_translation_exchange(Body::ArmTranslationQuery(control::ArmTranslationQuery {}))
            .map(guest_arm_translation)
    }

    /// Sends `body` to the control daemon and waits for its ArmTranslationState.
    fn arm_translation_exchange(
        &self,
        body: Body,
    ) -> Result<control::ArmTranslationState, SessionError> {
        let _turn = self
            .shared
            .arm_translation_turn
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tx, events) = mpsc::channel();
        *self
            .shared
            .arm_translation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let sent = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(body),
        }));
        let answer = sent.and_then(|()| {
            events.recv_timeout(ARM_TRANSLATION_WAIT).map_err(|_| {
                SessionError::NoAnswer("Android did not say where ARM translation stands".into())
            })
        });
        *self
            .shared
            .arm_translation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        answer
    }

    /// Asks the guest's control daemon to remove the Google apps' image an older DSA left on
    /// the user's disk.
    pub fn remove_gapps(&self) -> Result<GuestGapps, SessionError> {
        let state = self.gapps_exchange(Body::GappsRemove(control::GappsRemove {}))?;
        if state.error.is_empty() {
            Ok(guest_gapps(state))
        } else {
            Err(SessionError::InstallRefused(state.error))
        }
    }

    /// Asks the guest where the Google apps stand, with the Google Services Framework Android
    /// ID when they run. Needs a guest that is active or dozing: a frozen one is not thawed
    /// for it.
    pub fn gapps(&self) -> Result<GuestGapps, SessionError> {
        let state = self.state();
        if !matches!(state, SessionState::Active | SessionState::Dozing) {
            return Err(SessionError::NotActive(state));
        }
        let mut guest = self
            .gapps_exchange(Body::GappsQuery(control::GappsQuery {}))
            .map(guest_gapps)?;
        if guest.active {
            guest.device_id = self.google_device_id();
        }
        Ok(guest)
    }

    /// The Google Services Framework Android ID the agent reads, or empty when it does not
    /// answer in time or there is none yet.
    fn google_device_id(&self) -> String {
        let (tx, answers) = mpsc::channel();
        *self
            .shared
            .device_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let sent = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::GoogleDeviceIdQuery(control::GoogleDeviceIdQuery {})),
        }));
        let id = match sent {
            Ok(()) => answers.recv_timeout(DEVICE_ID_WAIT).unwrap_or_default(),
            Err(_) => String::new(),
        };
        *self
            .shared
            .device_id
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        id
    }

    /// Reads up to `length` bytes from `offset` of the add-on image `image` that an older DSA
    /// left on the user's disk, which the host moves onto the add-on disk. Says why when the
    /// guest cannot read it. Needs a guest that is active or dozing.
    pub fn read_addon_image(
        &self,
        image: control::AddonImage,
        offset: u64,
        length: u32,
    ) -> Result<control::AddonImagePart, SessionError> {
        let state = self.state();
        if !matches!(state, SessionState::Active | SessionState::Dozing) {
            return Err(SessionError::NotActive(state));
        }
        let _turn = self
            .shared
            .addon_image_turn
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tx, parts) = mpsc::channel();
        *self
            .shared
            .addon_image
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let sent = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::AddonImageRead(control::AddonImageRead {
                image: image as i32,
                offset,
                length,
            })),
        }));
        let answer = sent.and_then(|()| loop {
            match parts.recv_timeout(CHUNK_TIMEOUT) {
                // An answer to an earlier read that gave up waiting is not this one's.
                Ok(part) if part.image() != image || part.offset != offset => continue,
                Ok(part) if !part.error.is_empty() => {
                    break Err(SessionError::InstallRefused(part.error))
                }
                Ok(part) => break Ok(part),
                Err(_) => {
                    break Err(SessionError::NoAnswer(
                        "Android did not send the part of the add-on image it was asked for".into(),
                    ))
                }
            }
        });
        *self
            .shared
            .addon_image
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        answer
    }

    /// Sends `body` to the control daemon and waits for its GappsState.
    fn gapps_exchange(&self, body: Body) -> Result<control::GappsState, SessionError> {
        let _turn = self
            .shared
            .gapps_turn
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (tx, events) = mpsc::channel();
        *self.shared.gapps.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let sent = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(body),
        }));
        let answer = sent.and_then(|()| {
            events.recv_timeout(GAPPS_WAIT).map_err(|_| {
                SessionError::NoAnswer("Android did not say where the Google apps stand".into())
            })
        });
        *self.shared.gapps.lock().unwrap_or_else(|e| e.into_inner()) = None;
        answer
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
        self.shared.publish_power(GuestPower::WindowsShown(shown));
        self.commands
            .send(Command::WindowsShown(shown))
            .map_err(|_| SessionError::Gone)
    }

    /// Records the host's power, which the guest is told now if it is up, and each time its
    /// agent connects.
    pub fn set_power(&self, state: PowerState) {
        let _ = self.commands.send(Command::Power(state));
    }

    /// Records the refresh rate of the user's fastest monitor, in hertz. With the host's
    /// battery saver it sets the guest display's refresh rate
    /// ([`crate::guest_refresh::refresh_rate`]), which the guest is told now if it is up, on
    /// each change, and each time its agent connects.
    pub fn set_monitor_refresh(&self, hz: u32) {
        let _ = self
            .commands
            .send(Command::Refresh(RefreshInput::MonitorHz(hz)));
    }

    /// Records whether the host's battery saver is on, for a host that reads it apart from the
    /// rest of its power. A host that calls [`Session::set_power`] needs no more: the
    /// `power_saver` in its state counts the same.
    pub fn set_battery_saver(&self, on: bool) {
        let _ = self
            .commands
            .send(Command::Refresh(RefreshInput::BatterySaver(on)));
    }

    /// Records whether the host is short of memory. The guest is told now if it is up, and
    /// when its agent connects while the host is short, so it gives back what it can spare.
    pub fn host_memory_low(&self, low: bool) {
        let _ = self.commands.send(Command::HostMemoryLow(low));
    }

    /// Records the host's system settings (time zone, languages, regional format, theme and
    /// accent color). The guest's agent is told now if it is there, and each time it connects.
    pub fn set_system_settings(&self, settings: SystemSettings) {
        let input = SystemInput::Settings(settings.checked());
        let _ = self.commands.send(Command::System(input));
    }

    /// Has `approval` decide the su calls of the guest's apps: the host's administrator prompt
    /// and the host log. Without it, every su call is refused at once. Its policy starts again
    /// at each start of Android, which clears every app's grace.
    pub fn set_root_approval(&self, approval: Arc<RootApproval>) {
        *self
            .shared
            .root_approval
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((approval, self.commands.clone()));
    }

    /// Asks the computer's administrator, through the same prompt as su calls, to let
    /// `package` keep root without asking each time. The prompt shows the app's name and icon
    /// as the guest last listed them. `Err` says why not, including when this host has no
    /// prompt to ask through.
    pub fn approve_keep_root(&self, package: &str) -> Result<(), String> {
        let approval = self
            .shared
            .root_approval
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|(approval, _)| Arc::clone(approval));
        let Some(approval) = approval else {
            return Err("this computer cannot ask its administrator for root approval".into());
        };
        let app = self
            .shared
            .installed_app(package)
            .unwrap_or_else(|| AppInfo {
                package_name: package.to_string(),
                ..Default::default()
            });
        approval.approve_keep_root(app)
    }

    /// Gives the guest's agent the device name `source` says each time it connects, which the
    /// agent makes the guest's.
    pub fn set_device_name_source(&self, source: DeviceNameSource) {
        *self
            .shared
            .device_name
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(source);
    }

    /// Passes a change to the host's game controllers on. The session keeps the controllers
    /// connected, so a guest that boots later, or whose control daemon restarts, gets them too.
    pub fn gamepad(&self, event: GamepadEvent) {
        let _ = self
            .commands
            .send(Command::System(SystemInput::Gamepad(event)));
    }

    /// The rumble effects the guest's apps play on the host's controllers, from now on. The
    /// watch lasts across restarts of the guest.
    pub fn watch_gamepad_rumble(&self) -> Receiver<GamepadRumble> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .rumble_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Says that the host is about to sleep (sleep, hibernate or modern standby). A running
    /// guest's screen goes off, so it goes into Doze, and its control daemon writes its pending
    /// file data to disk. The receiver gets a message once the guest says that is done, or at
    /// once when no guest is running to say it; it is closed instead if the guest goes away or
    /// the host wakes first. The caller decides how long to wait.
    pub fn host_sleeping(&self) -> Receiver<()> {
        let (reply, done) = mpsc::channel();
        if let Err(mpsc::SendError(Command::HostSleep(reply))) =
            self.commands.send(Command::HostSleep(reply))
        {
            let _ = reply.send(());
        }
        done
    }

    /// Says that the host woke, its wall clock reading `wall_clock`. The guest's clock is set to
    /// it, now or once the guest can hear (thawed, or its agent connected again), and the
    /// guest's screen comes back on if it was on before the host slept. Time the host slept
    /// does not count towards the freeze and boot timers.
    pub fn host_woke(&self, wall_clock: SystemTime) {
        let _ = self.commands.send(Command::HostWoke(wall_clock));
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

    /// The documents the guest prints from now on, each whole job to one watcher only, and
    /// each job withdrawn from the guest. The watch lasts across restarts of the guest.
    pub fn watch_print_jobs(&self) -> Receiver<PrintEvent> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .print_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Tells the guest what became of a print job a watcher was given, so it ends the job in
    /// its print spooler the same way. A job the guest no longer has (it was withdrawn) is
    /// left alone.
    pub fn print_result(&self, job_id: u32, outcome: &PrintOutcome) -> Result<(), SessionError> {
        let guest_id = self
            .shared
            .prints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .answered(job_id);
        let Some(guest_id) = guest_id else {
            eprintln!("print: the outcome of job {job_id} came after Android let go of it");
            return Ok(());
        };
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::PrintJobResult(outcome.to_control(guest_id))),
        }))
    }

    /// Serves the guest's speech and spelling from now on: its requests come to the returned
    /// watch while no client that came before still serves them. The watch lasts across
    /// restarts of the guest. Each answer goes back through [`Session::language_answer`], and
    /// [`Session::language_server_gone`] says when the client has gone.
    pub fn serve_language(&self) -> language::relay::Served {
        self.shared.language().serve()
    }

    /// Passes an answer of client `server`'s to the guest, if it answers a request the client
    /// has. Anything that is no answer is an error, and reaches nobody.
    pub fn language_answer(&self, server: u64, envelope: control::Envelope) -> Result<(), String> {
        let Some(body) = envelope.body else {
            return Err("an empty message".into());
        };
        if let Some(answer) = self.shared.language().answer(server, body)? {
            send_quietly(&self.shared, &self.commands, answer);
        }
        Ok(())
    }

    /// Client `server` no longer serves the guest's speech and spelling: what it had not
    /// answered fails in the guest.
    pub fn language_server_gone(&self, server: u64) {
        let failures = self.shared.language().server_gone(server);
        for failure in failures {
            send_quietly(&self.shared, &self.commands, failure);
        }
    }

    /// The address lookups of the guest's apps from now on, each to one watcher only, which
    /// answers it with [`Session::geocode_result`]. The watch lasts across restarts of the
    /// guest.
    pub fn watch_geocoding(&self) -> Receiver<GeocodeQuery> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .geocode_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Gives the guest what lookup `query_id` found. An answer for a guest that has stopped is
    /// dropped: its app no longer waits.
    pub fn geocode_result(
        &self,
        query_id: u32,
        outcome: &GeocodeOutcome,
    ) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::GeocodeResult(geocode::result_to_control(
                query_id, outcome,
            ))),
        }))
    }

    /// What the guest's apps share with the host from now on, each share to one watcher only,
    /// which reads its files, shows the host's share UI and answers with
    /// [`Session::share_result`]. The watch lasts across restarts of the guest.
    pub fn watch_shares(&self) -> Receiver<ShareRequest> {
        let (tx, rx) = mpsc::channel();
        self.shared
            .share_watchers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        rx
    }

    /// Tells the guest what became of share `share_id`. A share no watcher was given is left
    /// alone.
    pub fn share_result(&self, share_id: u32, outcome: &ShareOutcome) -> Result<(), SessionError> {
        let open = self
            .shared
            .open_shares
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&share_id);
        if !open {
            eprintln!("share: the outcome of share {share_id} came for no share Android has open");
            return Ok(());
        }
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::ShareResult(outcome.to_control(share_id))),
        }))
    }

    /// Whether the guest wants the host's location, from what it wants now on. The watch
    /// lasts across restarts of the guest, which wants no location while it is stopped.
    pub fn watch_location(&self) -> Receiver<Wanted> {
        self.shared
            .location
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watch()
    }

    /// Gives the guest what the host's location service reported, while the guest wants a
    /// location. What comes after it stopped wanting one is dropped.
    pub fn host_location(&self, event: LocationEvent) {
        let wanted = self
            .shared
            .location
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .wanted();
        if wanted.active {
            send_quietly(&self.shared, &self.commands, event.to_control());
        }
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

    /// The apps that run in the background and the active media sessions now, and every
    /// change to them from now on. The watch lasts across restarts of the guest.
    pub fn watch_background(&self) -> BackgroundWatch {
        self.shared
            .background
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watch()
    }

    /// The apps that run in the background and the active media sessions now.
    pub fn background_apps(&self) -> (Vec<BackgroundApp>, Vec<MediaSession>) {
        let hub = self
            .shared
            .background
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        (hub.apps(), hub.media())
    }

    /// Passes what the user did with an app that runs in the background to the guest.
    /// Opening it wakes a guest in Doze or frozen first; any other command thaws a frozen one
    /// without turning its screen on. Needs a running guest.
    pub fn background_action(&self, request: BackgroundRequest) -> Result<(), SessionError> {
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Background(request, reply_tx))
            .map_err(|_| SessionError::Gone)?;
        reply_rx.recv().map_err(|_| SessionError::Gone)?
    }

    /// Which app each output of the guest's sound plays and its volume now, and every list
    /// after it. The watch lasts across restarts of the guest; a guest that stops has none.
    pub fn watch_app_audio(&self) -> AppAudioWatch {
        self.shared
            .app_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .watch()
    }

    /// Which app each output of the guest's sound plays and its volume now.
    pub fn app_audio(&self) -> Vec<AppStream> {
        self.shared
            .app_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .streams()
    }

    /// Gives the guest the volume the user set for an app on the host. A change for a guest
    /// that is not running is dropped: its apps are gone with it.
    pub fn set_app_volume(&self, change: &VolumeChange) {
        send_quietly(
            &self.shared,
            &self.commands,
            Body::SetAppVolume(change.to_control()),
        );
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
            hub.announced.remove(&task_id);
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
                package: file.package.clone(),
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
            TaskRequest::Launch { package_name } => Body::LaunchApp(control::LaunchApp {
                package_name,
                new_window: false,
            }),
            TaskRequest::NewWindow { package_name } => Body::LaunchApp(control::LaunchApp {
                package_name,
                new_window: true,
            }),
            TaskRequest::OpenAppSettings { package_name, page } => {
                return self.open_app_settings_page(Some(&package_name), page)
            }
            TaskRequest::StopApp { package_name } => {
                Body::StopApp(control::StopApp { package_name })
            }
            TaskRequest::LaunchShortcut {
                package_name,
                shortcut_id,
            } => Body::LaunchShortcut(control::LaunchShortcut {
                package_name,
                shortcut_id,
            }),
            TaskRequest::AnswerPinShortcut {
                request_id,
                accepted,
            } => Body::PinShortcutAnswer(control::PinShortcutAnswer {
                request_id,
                accepted,
            }),
            TaskRequest::Resize {
                task_id,
                width,
                height,
                live,
            } => {
                return self.resize(control::ResizeTask {
                    task_id,
                    width,
                    height,
                    live,
                })
            }
            TaskRequest::SetDensity {
                task_id,
                density_dpi,
            } => Body::SetTaskDensity(control::SetTaskDensity {
                task_id,
                density_dpi,
            }),
            TaskRequest::SetCaption {
                task_id,
                height,
                start_width,
                end_width,
            } => Body::SetTaskCaption(control::SetTaskCaption {
                task_id,
                height,
                start_width,
                end_width,
            }),
        };
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(body),
        }))
    }

    /// Sends a step of a task window's resize, and holds the guest's refresh rate down while
    /// any window is resized live ([`crate::guest_refresh::LiveResizes`]).
    ///
    /// A step that starts or ends the live resizes goes through the worker, right behind the
    /// word that changes the rate, so the first live step reaches a guest already at the lower
    /// rate. The caller waits for the worker to send it, so the steps after it, which go out
    /// at once as every other step does, never overtake it.
    fn resize(&self, resize: control::ResizeTask) -> Result<(), SessionError> {
        let changed = self
            .shared
            .live_resizes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .step(resize.task_id, resize.live, Instant::now());
        let frame = dsa_protocol::encode(&control::Envelope {
            body: Some(Body::ResizeTask(resize)),
        });
        if !changed {
            return self.send(frame);
        }
        let _ = self
            .commands
            .send(Command::Refresh(RefreshInput::LiveResize));
        let (reply_tx, reply_rx) = mpsc::channel();
        self.commands
            .send(Command::Send(frame, reply_tx))
            .map_err(|_| SessionError::Gone)?;
        reply_rx.recv().map_err(|_| SessionError::Gone)?
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

    /// Asks the agent whether an app is dragging something out of the window the pointer
    /// left (`out`), and waits up to `timeout` for its answer. When one is, the agent ends the
    /// drag in the guest and offers what it carries; `None` when nothing is being dragged.
    /// Needs an active session.
    pub fn drag_out(
        &self,
        out: DragOut,
        timeout: Duration,
    ) -> Result<Option<DragOffer>, SessionError> {
        let request_id = self.next_drag_request();
        let (tx, answer) = mpsc::channel();
        self.shared
            .drag_offers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        let result = self
            .send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::DragOut(control::DragOut {
                    request_id,
                    task_id: out.task_id,
                    x: out.x,
                    y: out.y,
                    tool: out.tool,
                    pointer_id: out.pointer_id,
                })),
            }))
            .and_then(|()| {
                answer.recv_timeout(timeout).map_err(|_| {
                    SessionError::NoAnswer(format!(
                        "Android did not say within {} seconds what is being dragged",
                        timeout.as_secs()
                    ))
                })
            });
        // Once the request is gone, an offer that comes late ends its own drag.
        self.shared
            .drag_offers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        let offer = result?;
        if !offer.error.trim().is_empty() {
            if offer.drag_id != 0 {
                let _ = self.end_drag(offer.drag_id, false);
            }
            return Err(SessionError::FileRefused(
                offer.error.chars().take(512).collect(),
            ));
        }
        if offer.items.is_empty() {
            if offer.drag_id != 0 {
                let _ = self.end_drag(offer.drag_id, false);
            }
            return Ok(None);
        }
        Ok(Some(DragOffer {
            drag_id: offer.drag_id,
            items: offer
                .items
                .iter()
                .enumerate()
                .map(|(index, item)| DragItem::from_control(item, index))
                .collect(),
        }))
    }

    /// Reads part of a file of a drag the agent offered, waiting up to `timeout`. Needs an
    /// active session.
    pub fn read_drag(&self, read: DragRead, timeout: Duration) -> Result<DragChunk, SessionError> {
        let length = read.length.clamp(1, drag::READ_BYTES);
        let request_id = self.next_drag_request();
        let (tx, answer) = mpsc::channel();
        self.shared
            .drag_reads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        let result = self
            .send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::DragRead(control::DragRead {
                    request_id,
                    drag_id: read.drag_id,
                    item: read.item,
                    offset: read.offset,
                    length,
                })),
            }))
            .and_then(|()| {
                answer.recv_timeout(timeout).map_err(|_| {
                    SessionError::NoAnswer(format!(
                        "Android did not send the dragged file within {} seconds",
                        timeout.as_secs()
                    ))
                })
            });
        self.shared
            .drag_reads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        let data = result?;
        if !data.error.trim().is_empty() {
            return Err(SessionError::FileRefused(
                data.error.chars().take(512).collect(),
            ));
        }
        if data.data.len() > length as usize {
            return Err(SessionError::FileRefused(
                "Android sent more of the dragged file than was asked for".into(),
            ));
        }
        Ok(DragChunk {
            data: data.data,
            end: data.end,
        })
    }

    /// Tells the agent drag `drag_id` is over on the host, dropped or not, so it closes what
    /// it read and gives back its access to the drag's content.
    pub fn end_drag(&self, drag_id: u32, dropped: bool) -> Result<(), SessionError> {
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::DragEnd(control::DragEnd { drag_id, dropped })),
        }))
    }

    fn next_drag_request(&self) -> u32 {
        self.shared
            .last_drag_request
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    /// Asks the agent to read the installed apps again, sizes included, and waits up to
    /// `timeout` for the fresh list, which then replaces the one the watchers have. Needs an
    /// active session.
    pub fn refresh_apps(&self, timeout: Duration) -> Result<(), SessionError> {
        let (tx, answer) = mpsc::channel();
        self.shared
            .app_refreshes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(tx);
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::RefreshApps(control::RefreshApps {})),
        }))?;
        answer.recv_timeout(timeout).map_err(|_| {
            SessionError::NoAnswer(format!(
                "Android did not list its apps within {} seconds",
                timeout.as_secs()
            ))
        })
    }

    /// Opens Android's own settings of `package` (its permissions, storage and
    /// notifications) as a task in its own window, or the list of every app when it is
    /// `None`. Needs an active session.
    pub fn open_app_settings(&self, package: Option<&str>) -> Result<(), SessionError> {
        self.open_app_settings_page(package, AppSettingsPage::Details)
    }

    /// Opens one of Android's settings pages of `package` (`page`), or the list of every app
    /// when it is `None`. Needs an active session.
    pub fn open_app_settings_page(
        &self,
        package: Option<&str>,
        page: AppSettingsPage,
    ) -> Result<(), SessionError> {
        let page = match page {
            AppSettingsPage::Details => control::AppSettingsPage::Details,
            AppSettingsPage::AspectRatio => control::AppSettingsPage::AspectRatio,
            AppSettingsPage::OpenByDefault => control::AppSettingsPage::OpenByDefault,
        };
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::OpenAppSettings(control::OpenAppSettings {
                package_name: package.unwrap_or_default().to_string(),
                page: page as i32,
            })),
        }))
    }

    /// Uninstalls the user app `package`, waiting up to `timeout` for Android to say how it
    /// went. Needs an active session.
    pub fn uninstall_app(
        &self,
        package: &str,
        timeout: Duration,
    ) -> Result<UninstallOutcome, SessionError> {
        let request_id = self
            .shared
            .last_uninstall
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        let (tx, answer) = mpsc::channel();
        self.shared
            .uninstalls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        let result = self
            .send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::UninstallApp(control::UninstallApp {
                    request_id,
                    package_name: package.to_string(),
                })),
            }))
            .and_then(|()| {
                answer.recv_timeout(timeout).map_err(|_| {
                    SessionError::NoAnswer(format!(
                        "Android did not say within {} seconds whether {package} was uninstalled",
                        timeout.as_secs()
                    ))
                })
            });
        self.shared
            .uninstalls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        result
    }

    /// Installs the app `package` again from its APKs in the guest, with its native code for
    /// arm64-v8a (`on`, run through the translator) or for x86_64, and says how it went. The
    /// app keeps its data; when Android cannot keep it, the answer says so and nothing changes,
    /// unless `erase_data` is set. Turning it on needs ARM translation in the running guest.
    /// Waits up to `timeout` for the agent, which copies and checks every APK of the app.
    pub fn set_run_as_arm(
        &self,
        package: &str,
        on: bool,
        erase_data: bool,
        timeout: Duration,
    ) -> Result<RunAsArmOutcome, SessionError> {
        if on {
            let guest = self.arm_translation()?;
            if !native_abi::translates_arm64(&guest.abilist) {
                return Ok(RunAsArmOutcome {
                    package_name: package.to_string(),
                    status: RunAsArmStatus::TranslationOff,
                    message: format!("the guest's ABI list is {:?}", guest.abilist),
                    on: false,
                });
            }
        }
        let abi = if on {
            native_abi::ARM64
        } else {
            native_abi::X86_64
        };
        let request_id = self.shared.next_reinstall();
        let (tx, answer) = mpsc::channel();
        self.shared
            .reinstalls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(request_id, tx);
        eprintln!("apps: installing {package} again for {abi} (erase data: {erase_data})");
        let result = self
            .send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::ReinstallApp(control::ReinstallApp {
                    request_id,
                    package_name: package.to_string(),
                    abi: abi.to_string(),
                    erase_data,
                })),
            }))
            .and_then(|()| {
                answer.recv_timeout(timeout).map_err(|_| {
                    SessionError::NoAnswer(format!(
                        "Android did not say within {} seconds whether {package} was installed \
                         again",
                        timeout.as_secs()
                    ))
                })
            })
            .map(|result| RunAsArmOutcome::from_control(&result));
        self.shared
            .reinstalls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&request_id);
        if let Ok(outcome) = &result {
            eprintln!("apps: {package}: {outcome}");
        }
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
                    Ok(WidgetAnswer::Added(_)) | Ok(WidgetAnswer::Reconfigured(_)) => {}
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
                    Ok(WidgetAnswer::Providers(..)) | Ok(WidgetAnswer::Reconfigured(_)) => {}
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

    /// Opens the setup of a placed widget again, as its app's window, and waits up to
    /// `timeout` for the user to finish it (Ok) or leave it (WidgetRefused with the reason).
    /// Only a widget whose app says it can be set up again is asked for; the widget stays
    /// placed either way, and once the user finishes, the agent describes and draws it anew.
    /// A press wakes a guest in Doze first, as the setup is a window the user works in.
    /// Needs an active session.
    pub fn reconfigure_widget(
        &self,
        widget_id: i32,
        timeout: Duration,
    ) -> Result<(), SessionError> {
        let provider = self
            .widgets()
            .into_iter()
            .find(|(id, _)| *id == widget_id)
            .map(|(_, provider)| provider);
        match provider {
            None => {
                return Err(SessionError::WidgetRefused(format!(
                    "there is no widget {widget_id}"
                )))
            }
            Some(provider) if !provider.reconfigurable => {
                return Err(SessionError::WidgetRefused(format!(
                    "the widget {} cannot be set up again; its app offers no settings for it",
                    if provider.label.is_empty() {
                        provider.provider
                    } else {
                        provider.label
                    }
                )))
            }
            Some(_) => {}
        }
        let (reply_tx, reply_rx) = mpsc::channel();
        if self.commands.send(Command::WidgetPressed(reply_tx)).is_ok() {
            let _ = reply_rx.recv();
        }
        let (request_id, answers) = self.widget_request();
        let result = (|| {
            self.send(dsa_protocol::encode(&control::Envelope {
                body: Some(Body::ReconfigureWidget(control::ReconfigureWidget {
                    request_id,
                    widget_id,
                })),
            }))?;
            let deadline = Instant::now() + timeout;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match answers.recv_timeout(left) {
                    Ok(WidgetAnswer::Reconfigured(Ok(()))) => return Ok(()),
                    Ok(WidgetAnswer::Reconfigured(Err(why))) => {
                        return Err(SessionError::WidgetRefused(why))
                    }
                    Ok(WidgetAnswer::Providers(..)) | Ok(WidgetAnswer::Added(_)) => {}
                    Err(_) => {
                        return Err(SessionError::WidgetRefused(format!(
                            "the widget's settings did not close within {} seconds",
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
        if matches!(
            event.action,
            PointerAction::Down | PointerAction::Up | PointerAction::Scroll
        ) {
            self.shared.publish_power(GuestPower::HostInput);
        }
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::PointerEvent(event.to_control())),
        }))
    }

    /// Passes a key pressed or released in a task's window to the guest. Needs an active
    /// session.
    pub fn key(&self, event: &KeyEvent) -> Result<(), SessionError> {
        self.shared.publish_power(GuestPower::HostInput);
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

impl Session {
    /// Reads the guest's log: the records the host holds that pass `filter`, then each new one
    /// that does. The first reader asks the guest to read its log, and dropping the last tells
    /// it to stop, so a guest whose log nobody reads pays nothing for it. The watch lasts
    /// across restarts of the guest: each boot is asked again.
    pub fn watch_log(&self, filter: LogFilter) -> LogWatch {
        let (id, history, entries, stream) = {
            let mut hub = self.shared.logs.lock().unwrap_or_else(|e| e.into_inner());
            let first = hub.reader_count() == 0;
            let (id, history, entries) = hub.subscribe(filter);
            (id, history, entries, (hub.stream(), first))
        };
        let (stream, first) = stream;
        if first {
            eprintln!("guest log: a reader is attached; asking Android for its log");
        }
        send_quietly(&self.shared, &self.commands, Body::LogStream(stream));
        let shared = Arc::clone(&self.shared);
        let commands = self.commands.clone();
        LogWatch::new(history, entries, move || {
            let stream = {
                let mut hub = shared.logs.lock().unwrap_or_else(|e| e.into_inner());
                hub.unsubscribe(id);
                hub.stream()
            };
            if !stream.on {
                eprintln!("guest log: no reader is attached; Android stops sending its log");
            }
            send_quietly(&shared, &commands, Body::LogStream(stream));
        })
    }

    /// The guest's records the host holds now, oldest first.
    pub fn recent_log(&self) -> Vec<LogEntry> {
        let hub = self.shared.logs.lock().unwrap_or_else(|e| e.into_inner());
        hub.ring().iter().cloned().collect()
    }

    /// Asks the guest for everything its log holds now that passes `filter`. The records
    /// arrive on the receiver, then its end. Needs a running guest.
    pub fn dump_log(&self, filter: LogFilter) -> Result<Receiver<LogDumpEvent>, SessionError> {
        let least = filter.least_priority();
        let buffers = filter.buffers;
        let (request_id, events) = self
            .shared
            .logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin_dump(filter);
        let sent = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::LogDump(control::LogDump {
                request_id,
                buffers,
                min_priority: u32::from(least.min(crate::guest_log::FATAL)),
            })),
        }));
        if let Err(error) = sent {
            self.shared
                .logs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .end_dump(request_id);
            return Err(error);
        }
        Ok(events)
    }

    /// Asks the guest for its diagnostics. Its files arrive on the receiver, then the end.
    /// Needs a running guest.
    pub fn collect_diagnostics(&self) -> Result<Receiver<DiagnosticsEvent>, SessionError> {
        let (request_id, events) = self
            .shared
            .logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .begin_diagnostics();
        let sent = self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(Body::CollectDiagnostics(control::CollectDiagnostics {
                request_id,
            })),
        }));
        if let Err(error) = sent {
            self.shared
                .logs
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .end_diagnostics(request_id);
            return Err(error);
        }
        Ok(events)
    }

    /// Serves the guest's log port on `listener` for the life of the session: each connection
    /// that proves it knows the running boot's secret may send log records and diagnostics. A
    /// connection that sends anything else, or a record that breaks the rules, is closed; the
    /// control channel and the session go on.
    pub fn serve_log_port(&self, listener: Box<dyn Listener>, build: &str) {
        let shared = Arc::clone(&self.shared);
        let commands = self.commands.clone();
        let hello = dsa_protocol::handshake::local_hello(control::Role::Host, build);
        let spawned = thread::Builder::new()
            .name("dsa-guest-log".into())
            .spawn(move || accept_log_connections(listener, shared, commands, hello));
        if let Err(error) = spawned {
            eprintln!("guest log: the log port cannot be served: {error}");
        }
    }
}

/// How long a connection on the log port may take to pass the handshake.
const LOG_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn accept_log_connections(
    mut listener: Box<dyn Listener>,
    shared: Arc<Shared>,
    commands: Sender<Command>,
    hello: control::Hello,
) {
    let mut reported = false;
    while !shared.log_port_stop.load(Ordering::Relaxed) {
        let mut connection = match listener.accept(Duration::from_millis(200)) {
            Ok(connection) => connection,
            Err(TransportError::Timeout) => continue,
            Err(error) => {
                if !reported {
                    eprintln!("guest log: accepting on the log port failed: {error}");
                    reported = true;
                }
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        reported = false;
        let secret = shared
            .boot_secret
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(secret) = secret else {
            connection.close();
            continue;
        };
        let (shared, commands, hello) = (Arc::clone(&shared), commands.clone(), hello.clone());
        let spawned = thread::Builder::new()
            .name("dsa-guest-log-connection".into())
            .spawn(move || {
                let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if let Err(error) = dsa_protocol::handshake::host_accept(
                        connection.as_mut(),
                        &hello,
                        &secret,
                        LOG_HANDSHAKE_TIMEOUT,
                    ) {
                        connection.close();
                        eprintln!("guest log: refused a connection on the log port: {error}");
                        return;
                    }
                    let stop = || shared.log_port_stop.load(Ordering::Relaxed);
                    let ended = serve_log_connection(connection.as_mut(), &stop, &mut |message| {
                        deliver_log(&shared, &commands, message)
                    });
                    if let Err(why) = ended {
                        eprintln!(
                            "guest log: closed the log connection, which sent {why}; the control \
                             channel goes on"
                        );
                    }
                }));
                if served.is_err() {
                    eprintln!("guest log: a log connection failed; the control channel goes on");
                }
            });
        if let Err(error) = spawned {
            eprintln!("guest log: a log connection cannot be served: {error}");
        }
    }
}

/// Hands one message from the log port to whoever waits for it.
fn deliver_log(shared: &Shared, commands: &Sender<Command>, message: LogPortMessage) {
    match message {
        LogPortMessage::Records {
            request_id: 0,
            entries,
            error,
            ..
        } => {
            if !error.is_empty() {
                eprintln!("guest log: Android cannot send its log: {error}");
            }
            let stream = {
                let mut hub = shared.logs.lock().unwrap_or_else(|e| e.into_inner());
                hub.publish(entries).then(|| hub.stream())
            };
            if let Some(stream) = stream {
                send_quietly(shared, commands, Body::LogStream(stream));
            }
        }
        LogPortMessage::Records {
            request_id,
            entries,
            end,
            error,
        } => {
            let mut hub = shared.logs.lock().unwrap_or_else(|e| e.into_inner());
            if !hub.dump_records(request_id, entries, end, &error) {
                eprintln!(
                    "guest log: dropped records of dump {request_id}, which nobody waits for"
                );
            }
        }
        LogPortMessage::Diagnostics { request_id, event } => {
            let mut hub = shared.logs.lock().unwrap_or_else(|e| e.into_inner());
            if !hub.diagnostics_event(request_id, event) {
                eprintln!(
                    "guest log: dropped diagnostics of request {request_id}, which nobody waits for"
                );
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // The adb server lets go of developer mode's port before it closes.
        drop(
            self.adb_watcher
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
        drop(
            self.adb_advert
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take(),
        );
        self.shared.log_port_stop.store(true, Ordering::Relaxed);
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

/// Hands `item` to the first of `watchers` still listening, and only to it, dropping those
/// that have gone. False when nobody listens.
fn hand_to_first<T: Clone>(watchers: &Mutex<Vec<Sender<T>>>, item: T) -> bool {
    let mut delivered = false;
    watchers
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|watcher| {
            if delivered {
                return true;
            }
            let sent = watcher.send(item.clone()).is_ok();
            delivered = sent;
            sent
        });
    delivered
}

/// How often an active session checks for requests while it waits on the channel.
const POLL: Duration = Duration::from_millis(20);

/// How long a question about ARM translation waits for the guest's control daemon.
const ARM_TRANSLATION_WAIT: Duration = Duration::from_secs(5);

/// How long a question about the Google apps waits for the guest.
const GAPPS_WAIT: Duration = Duration::from_secs(5);

/// How long the agent has to say the Google Services Framework Android ID, which it reads
/// at once.
const DEVICE_ID_WAIT: Duration = Duration::from_secs(2);

/// One of an app's launcher shortcuts, as the agent describes it.
fn shortcut_from_control(shortcut: control::AppShortcut) -> AppShortcut {
    AppShortcut {
        id: shortcut.id,
        short_label: shortcut.short_label,
        long_label: shortcut.long_label,
        icon_png: shortcut.icon_png,
        rank: shortcut.rank,
        manifest: shortcut.manifest,
        dynamic: shortcut.dynamic,
        pinned: shortcut.pinned,
    }
}

/// What the guest's control daemon says of the Google apps, as the host keeps it.
fn guest_gapps(state: control::GappsState) -> GuestGapps {
    GuestGapps {
        installed: false,
        left_on_data: state.installed,
        active: state.active,
        sdk: state.sdk,
        abi: state.abi,
        abilist: state.abilist,
        device_id: String::new(),
    }
}

/// What the guest's control daemon says of ARM translation, as the host keeps it.
fn guest_arm_translation(state: control::ArmTranslationState) -> GuestArmTranslation {
    GuestArmTranslation {
        installed: false,
        left_on_data: state.installed,
        active: state.active,
        abilist: state.abilist,
    }
}

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
    /// Where work the session hands to other threads posts its outcome.
    own_commands: Sender<Command>,
    /// Counts the guest's boots, so a resize from an earlier one is not taken for this one's.
    memory_generation: u64,
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
    /// The user's fastest monitor's refresh rate, in hertz, once the platform knows it.
    monitor_hz: Option<u32>,
    /// Whether the host's battery saver is on, from its power or from the platform directly.
    battery_saver: bool,
    /// The display refresh rate the guest was last told.
    refresh_sent: Option<u32>,
    /// Set when the rate changed while the guest was frozen, so it is told after the thaw.
    refresh_pending: bool,
    /// Whether the host is short of memory, as last recorded.
    host_memory_low: bool,
    /// Set when that changed while the guest was frozen, so it is told after the thaw.
    host_memory_pending: bool,
    /// The host's system settings, as last recorded.
    system_settings: Option<SystemSettings>,
    /// The host's game controllers that are connected, and the latest state of each.
    gamepads: GamepadTable,
    /// Set when the system settings or a controller changed while the guest was frozen, so
    /// it is given them all after the thaw.
    system_pending: bool,
    /// Grows and shrinks the guest's memory while it runs, when the launcher can.
    memory_growth: Option<MemoryGrowth>,
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
    /// The installs of an app again as ARM that the user asked for from a notification, by
    /// request ID, with the app's package: their outcome is a notification too.
    offered_reinstalls: HashMap<u32, String>,
    /// While the host sleeps: when it said it would, and whether the guest's screen was on.
    host_asleep: Option<(Instant, bool)>,
    /// Who waits for the guest to say its data is on disk before the host sleeps.
    sleep_waiters: Vec<Sender<()>>,
    /// The host's wall clock when it woke, and when that was, while the guest has yet to be
    /// told: it was frozen or its agent was not connected.
    clock_pending: Option<(SystemTime, Instant)>,
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
                        | Ok(Command::Notification(_, reply))
                        | Ok(Command::Background(_, reply)) => {
                            let _ = reply.send(Err(SessionError::NotActive(SessionState::Stopped)));
                        }
                        Ok(Command::WindowsShown(shown)) => self.set_shown(shown),
                        Ok(Command::Power(state)) => self.set_power(state),
                        Ok(Command::Refresh(input)) => self.refresh_input(input),
                        Ok(Command::HostMemoryLow(low)) => self.set_host_memory_low(low),
                        Ok(Command::System(input)) => self.system_input(input),
                        Ok(Command::FreezeAfter(after)) => self.config.freeze_after = after,
                        Ok(Command::LinksInAndroid(on)) => self.set_links_in_android(on),
                        Ok(Command::HostClipboard(text)) => self.host_clipboard(text),
                        Ok(Command::ShareClipboard(on)) => self.share_clipboard(on),
                        Ok(Command::WidgetPressed(reply)) | Ok(Command::HostSleep(reply)) => {
                            let _ = reply.send(());
                        }
                        // A stopped guest's clock is set when it next boots.
                        Ok(Command::HostWoke(_)) => self.host_asleep = None,
                        Ok(Command::MemoryResized(outcome)) => self.memory_resized(outcome),
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
                        | Ok(Command::Notification(_, reply))
                        | Ok(Command::Background(_, reply)) => {
                            let _ =
                                reply.send(Err(SessionError::NotActive(SessionState::Stopping)));
                        }
                        Ok(Command::WindowsShown(shown)) => self.set_shown(shown),
                        Ok(Command::Power(state)) => self.set_power(state),
                        Ok(Command::Refresh(input)) => self.refresh_input(input),
                        Ok(Command::HostMemoryLow(low)) => self.set_host_memory_low(low),
                        Ok(Command::System(input)) => self.system_input(input),
                        Ok(Command::FreezeAfter(after)) => self.config.freeze_after = after,
                        Ok(Command::LinksInAndroid(on)) => self.set_links_in_android(on),
                        Ok(Command::HostClipboard(text)) => self.host_clipboard(text),
                        Ok(Command::ShareClipboard(on)) => self.share_clipboard(on),
                        Ok(Command::WidgetPressed(reply)) | Ok(Command::HostSleep(reply)) => {
                            let _ = reply.send(());
                        }
                        // A stopped guest's clock is set when it next boots.
                        Ok(Command::HostWoke(_)) => self.host_asleep = None,
                        Ok(Command::MemoryResized(outcome)) => self.memory_resized(outcome),
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
            Command::Refresh(input) => {
                self.refresh_input(input);
                true
            }
            Command::HostMemoryLow(low) => {
                self.set_host_memory_low(low);
                true
            }
            Command::System(input) => {
                self.system_input(input);
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
            Command::Background(request, reply) => {
                let _ = reply.send(self.act_on_background(request));
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
            Command::HostSleep(reply) => {
                self.host_sleeping(reply);
                true
            }
            Command::HostWoke(wall_clock) => {
                self.host_woke(wall_clock);
                self.running()
            }
            Command::MemoryResized(outcome) => {
                self.memory_resized(outcome);
                self.running()
            }
        }
    }

    /// The host is about to sleep. A guest that can hear turns its screen off and is asked to
    /// put its data on disk; `reply` is told when it has. A frozen guest runs nothing that could
    /// write, so `reply` is told at once.
    fn host_sleeping(&mut self, reply: Sender<()>) {
        if self.host_asleep.is_none() {
            self.host_asleep = Some((Instant::now(), self.screen_on));
        }
        match self.state() {
            SessionState::Active | SessionState::Dozing => {
                if self.screen_on {
                    self.screen_on = false;
                    self.send_body(Body::ScreenState(control::ScreenState { on: false }));
                }
            }
            SessionState::Starting | SessionState::Stalled => {}
            _ => {
                let _ = reply.send(());
                return;
            }
        }
        eprintln!("lifecycle: the host is going to sleep; asking the guest to write its data");
        self.send_body(Body::HostSleep(control::HostSleep {
            asleep: true,
            host_unix_time_ms: 0,
        }));
        self.sleep_waiters.push(reply);
    }

    /// The guest's daemon says its data is on disk.
    fn guest_sleep_ready(&mut self) {
        for waiter in self.sleep_waiters.drain(..) {
            let _ = waiter.send(());
        }
    }

    /// The host woke. Time asleep does not count against the guest, its clock is set, and its
    /// screen comes back on if it was on before.
    fn host_woke(&mut self, wall_clock: SystemTime) {
        // Whoever still waits for the guest is let go.
        self.sleep_waiters.clear();
        let now = Instant::now();
        self.clock_pending = Some((wall_clock, now));
        let Some((slept_at, screen_was_on)) = self.host_asleep.take() else {
            self.send_clock();
            return;
        };
        let slept = now.saturating_duration_since(slept_at);
        eprintln!(
            "lifecycle: the host woke after {:.1} s; setting the guest's clock",
            slept.as_secs_f64()
        );
        self.discount_frozen_time(slept);
        self.hidden_since += slept;
        if let Some(since) = &mut self.dozing_since {
            *since += slept;
        }
        if let Some(frozen_at) = &mut self.frozen_at {
            *frozen_at += slept;
        }
        self.send_clock();
        if screen_was_on
            && !self.screen_on
            && matches!(self.state(), SessionState::Active | SessionState::Dozing)
        {
            self.wake();
        }
    }

    /// Sends the host's wall clock from when it woke, moved on by the time since, to a guest
    /// whose agent can set it. A frozen guest or one without its agent is told later.
    fn send_clock(&mut self) {
        let Some((wall_clock, at)) = self.clock_pending else {
            return;
        };
        let can_hear = self.agent_connected
            && matches!(
                self.state(),
                SessionState::Starting
                    | SessionState::Stalled
                    | SessionState::Active
                    | SessionState::Dozing
            );
        if !can_hear {
            return;
        }
        self.clock_pending = None;
        let now = wall_clock + at.elapsed();
        let host_unix_time_ms = now
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_millis() as u64)
            .unwrap_or(0);
        self.send_body(Body::HostSleep(control::HostSleep {
            asleep: false,
            host_unix_time_ms,
        }));
    }

    fn prints(&self) -> std::sync::MutexGuard<'_, print::Assembler> {
        let prints = &self.shared.prints;
        prints.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Acts on what the print assembler made of a message from the guest.
    fn print_step(&mut self, step: print::Step) {
        match step {
            print::Step::Wait => {}
            print::Step::Complete(job) => {
                let job_id = job.job_id;
                let bytes = job.pdf.len();
                eprintln!("print: an app sent a document of {bytes} bytes to print");
                if !self.shared.hand_print_job(job) {
                    let guest_id = self.prints().answered(job_id);
                    if let Some(guest_id) = guest_id {
                        let outcome = PrintOutcome::Failed(
                            "no desktop session is open to show the print dialog in".into(),
                        );
                        self.send_body(Body::PrintJobResult(outcome.to_control(guest_id)));
                    }
                }
            }
            print::Step::Refuse { guest_id, why } => {
                eprintln!("print: refused print job {guest_id} from the guest: {why}");
                let outcome = PrintOutcome::Failed(why);
                self.send_body(Body::PrintJobResult(outcome.to_control(guest_id)));
            }
            print::Step::Withdraw { job_id } => self.shared.withdraw_print_job(job_id),
        }
    }

    /// Hands an app's address lookup to a watcher in the user's session, or answers it at
    /// once when it cannot be looked up.
    fn geocode_request(&mut self, request: control::GeocodeRequest) {
        let request_id = request.request_id;
        let refused = match GeocodeQuery::from_control(&request) {
            Err(error) => Some(error),
            Ok(query) => (!self.shared.hand_geocode_query(query)).then(|| {
                GeocodeError::new(
                    GeocodeErrorKind::Refused,
                    "no desktop session is open to look the address up in",
                )
            }),
        };
        if let Some(error) = refused {
            let outcome: GeocodeOutcome = Err(error);
            self.send_body(Body::GeocodeResult(geocode::result_to_control(
                request_id, &outcome,
            )));
        }
    }

    /// Hands what an app shared to a watcher in the user's session. A share that cannot be
    /// shown fails at once, and the guest gives back its access to the files.
    fn share_to_host(&mut self, share: control::ShareToHost) {
        let share_id = share.share_id;
        let failed = match ShareRequest::from_control(&share) {
            Err(why) => Some(why),
            Ok(request) => {
                eprintln!(
                    "share: an app shares {} item(s) with the host",
                    request.items.len()
                );
                (!self.shared.hand_share(request))
                    .then(|| "no desktop session is open to show the share window in".to_string())
            }
        };
        if let Some(why) = failed {
            eprintln!("share: refused share {share_id} from the guest: {why}");
            self.send_body(Body::ShareResult(
                ShareOutcome::Failed(why).to_control(share_id),
            ));
            if share_id != 0 {
                self.send_body(Body::DragEnd(control::DragEnd {
                    drag_id: share_id,
                    dropped: false,
                }));
            }
        }
    }

    /// Ends every print job of the agent that was connected: the ones on their way are
    /// dropped, and the ones a watcher has are withdrawn.
    fn end_print_jobs(&mut self) {
        let gone = self.prints().clear();
        for job_id in gone {
            self.shared.withdraw_print_job(job_id);
        }
    }

    /// Shows one of the host's own notifications, replacing the one with its key.
    fn post_notice(&mut self, notification: Notification) {
        self.deliver_notification(notification);
    }

    /// Removes one of the host's own notifications, if it is shown.
    fn remove_notice(&mut self, key: &str) {
        if self.notifications.remove(key).is_some() {
            self.shared
                .publish_notification(NotificationEvent::Removed {
                    key: key.to_string(),
                });
        }
    }

    /// The agent says an app ended because it could not load a native library. When the app's
    /// APKs on the device hold its ARM code, it does not run as ARM yet, and the guest
    /// translates ARM code, the user is offered to install it again as ARM.
    fn native_library_crash(&mut self, crash: control::NativeLibraryCrash) {
        let package = crash.package_name;
        if !apk::valid_package_name(&package) {
            eprintln!("apps: dropped a crash record that names no package");
            return;
        }
        let detail: String = crash.detail.chars().take(512).collect();
        eprintln!("apps: {package} ended because a native library did not load: {detail}");
        let Some(app) = self.shared.installed_app(&package) else {
            eprintln!("apps: {package} is not among the installed apps; nothing is offered");
            return;
        };
        let switch = native_abi::run_as_arm(
            app.system_app,
            &app.primary_abi,
            &app.native_libraries,
            crash.arm64_supported,
        );
        if switch.available() && !switch.on {
            self.post_notice(native_abi::Notice::Offer.notification(&app));
            return;
        }
        if switch.on {
            eprintln!("apps: {package} runs as ARM already; nothing is offered");
        } else if let Some(why) = switch.unavailable {
            eprintln!(
                "apps: {package} cannot run as ARM ({}); nothing is offered",
                why.code()
            );
        }
        // A build made to run as 32-bit code, which the 64-bit-only guest does not run: the
        // user is told to install its 64-bit build.
        if native_abi::needs_64_bit_build(&app.native_libraries, switch, &detail) {
            eprintln!("apps: {package} holds 32-bit code, which Android here does not run");
            self.post_notice(native_abi::Notice::Needs64Bit.notification(&app));
        }
    }

    /// What the user did with one of the host's notifications about running an app as ARM.
    /// A click on the offer installs the app again as ARM, keeping its data; a click on the
    /// news that it worked opens the app. Any other click, and any dismissal, removes it.
    fn act_on_notice(
        &mut self,
        notice: native_abi::Notice,
        package: String,
        action: &NotificationAction,
    ) -> Result<(), SessionError> {
        let key = action.key().to_string();
        self.remove_notice(&key);
        if !matches!(action, NotificationAction::Open { .. }) {
            return Ok(());
        }
        match notice {
            native_abi::Notice::Offer => {
                self.wake();
                if !self.running() {
                    return Err(SessionError::NotActive(self.state()));
                }
                let request_id = self.shared.next_reinstall();
                self.offered_reinstalls.insert(request_id, package.clone());
                eprintln!("apps: installing {package} again as ARM, as the user asked");
                self.send_body(Body::ReinstallApp(control::ReinstallApp {
                    request_id,
                    package_name: package,
                    abi: native_abi::ARM64.to_string(),
                    erase_data: false,
                }));
            }
            native_abi::Notice::Done => {
                self.hidden_since = Instant::now();
                self.wake();
                if !self.running() {
                    return Err(SessionError::NotActive(self.state()));
                }
                self.send_body(Body::LaunchApp(control::LaunchApp {
                    package_name: package,
                    new_window: false,
                }));
            }
            native_abi::Notice::NeedsErase
            | native_abi::Notice::Failed
            | native_abi::Notice::Needs64Bit => {}
        }
        Ok(())
    }

    /// The agent says how installing an app again went: to whoever waits for it, or, when the
    /// user asked for it from a notification, as a notification.
    fn guest_reinstall_result(&mut self, result: control::ReinstallResult) {
        let waiting = self
            .shared
            .reinstalls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&result.request_id);
        if let Some(waiting) = waiting {
            let _ = waiting.send(result);
            return;
        }
        let Some(package) = self.offered_reinstalls.remove(&result.request_id) else {
            eprintln!(
                "apps: dropped the agent's answer to reinstall request {}, which nobody waits for",
                result.request_id
            );
            return;
        };
        let outcome = RunAsArmOutcome::from_control(&result);
        eprintln!("apps: {package}: {outcome}");
        let app = self
            .shared
            .installed_app(&package)
            .unwrap_or_else(|| AppInfo {
                package_name: package.clone(),
                label: package.clone(),
                ..Default::default()
            });
        self.post_notice(native_abi::Notice::of_outcome(&outcome).notification(&app));
    }

    /// Decides an app's su call on a thread of its own, since the host's administrator may
    /// take up to a minute to answer, and sends the decision back to the guest's su relay. A
    /// host that cannot ask an administrator refuses at once, so su does not wait for nothing.
    fn su_request(&mut self, request: control::SuRequest) {
        let approval = self
            .shared
            .root_approval
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some((approval, commands)) = approval else {
            eprintln!(
                "root: refused {}'s su call (request {}): this host cannot ask its \
                 administrator",
                request.package, request.request_id
            );
            self.send_body(Body::SuDecision(control::SuDecision {
                request_id: request.request_id,
                approved: false,
                reason: "this computer cannot ask its administrator for root approval".to_string(),
            }));
            return;
        };
        // The guest names every app of a shared user ID group, since the uid alone is all the
        // kernel knows of the caller; the prompt shows each of them with its name and icon.
        let packages = crate::root_approval::package_group(&request.package);
        let app = packages
            .first()
            .and_then(|package| self.shared.installed_app(package));
        let mut approval_request =
            crate::root_approval::ApprovalRequest::from_su(&request, app.as_ref());
        if packages.len() > 1 {
            let shared_apps = packages
                .iter()
                .map(|package| {
                    self.shared
                        .installed_app(package)
                        .unwrap_or_else(|| AppInfo {
                            package_name: package.to_string(),
                            ..Default::default()
                        })
                })
                .collect();
            approval_request = approval_request.with_shared_apps(shared_apps);
        }
        let shared = Arc::clone(&self.shared);
        let request_id = request.request_id;
        let spawned = thread::Builder::new()
            .name("dsa-su-approval".into())
            .spawn(move || {
                let decision = approval.answer(&approval_request);
                send_quietly(&shared, &commands, Body::SuDecision(decision));
            });
        if let Err(error) = spawned {
            eprintln!("root: refused su request {request_id}: no thread to ask on: {error}");
            self.send_body(Body::SuDecision(control::SuDecision {
                request_id,
                approved: false,
                reason: "the computer could not ask its administrator".to_string(),
            }));
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
        self.offered_reinstalls.clear();
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
        if let Some((notice, package)) = native_abi::Notice::parse(action.key()) {
            let package = package.to_string();
            return self.act_on_notice(notice, package, &action);
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

    fn publish_background(&mut self, event: BackgroundEvent) {
        self.shared
            .background
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .apply(event);
    }

    /// Passes what the user did with an app that runs in the background to the guest.
    ///
    /// Opening it is a click: the guest wakes, as for a notification. Any other command
    /// leaves the screen as it is, since playback and a stop need no screen; a frozen guest
    /// is thawed for it, because a frozen guest reads nothing. A package the guest does not
    /// run in the background is ignored, except for a stop, which the guest carries out for
    /// any app.
    fn act_on_background(&mut self, request: BackgroundRequest) -> Result<(), SessionError> {
        let state = self.state();
        if !matches!(
            state,
            SessionState::Active | SessionState::Dozing | SessionState::Frozen
        ) || !self.agent_connected
        {
            return Err(SessionError::NotActive(state));
        }
        let body = match request {
            BackgroundRequest::App(package_name, command) => {
                let known = self
                    .shared
                    .background
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .has_app(&package_name);
                let (command, action) = match command {
                    BackgroundCommand::Open => (control::BackgroundAppCommand::Open, 0),
                    BackgroundCommand::Action(index) => (
                        control::BackgroundAppCommand::Action,
                        i32::try_from(index).unwrap_or(i32::MAX),
                    ),
                    BackgroundCommand::Stop => (control::BackgroundAppCommand::Stop, 0),
                };
                if !known && command != control::BackgroundAppCommand::Stop {
                    eprintln!(
                        "background apps: the host acted on an app that does not run in the                          background; ignored"
                    );
                    return Ok(());
                }
                if command == control::BackgroundAppCommand::Open {
                    // The app opens on a screen that is on, as for a notification's click.
                    self.hidden_since = Instant::now();
                    self.wake();
                } else {
                    self.thaw_for_work();
                }
                Body::BackgroundAppAction(control::BackgroundAppAction {
                    package_name,
                    command: command as i32,
                    action,
                })
            }
            BackgroundRequest::Media(package_name, command) => {
                let known = self
                    .shared
                    .background
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .has_media(&package_name);
                if !known {
                    eprintln!(
                        "background apps: a media key for a package with no media session; ignored"
                    );
                    return Ok(());
                }
                self.thaw_for_work();
                Body::MediaControl(control::MediaControl {
                    package_name,
                    command: command.to_control() as i32,
                })
            }
        };
        if !self.running() {
            return Err(SessionError::NotActive(self.state()));
        }
        self.send_body(body);
        Ok(())
    }

    /// Thaws a frozen guest so it reads what comes next, without turning its screen on: the
    /// guest stays in Doze, and is frozen again by the usual rules.
    fn thaw_for_work(&mut self) {
        if self.state() != SessionState::Frozen {
            return;
        }
        if !self.resume_frozen() {
            return;
        }
        self.dozing_since = Some(Instant::now());
        self.shared.set(SessionState::Dozing, None);
        self.send_clock();
        self.send_dismissals();
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
        if self.state() == SessionState::Frozen && !self.resume_frozen() {
            return;
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
        // A guest frozen while the host slept has its clock set now.
        self.send_clock();
        self.send_dismissals();
    }

    /// Resumes a frozen guest and sends it what waited for it. False when it could not be
    /// resumed, and the session stopped.
    fn resume_frozen(&mut self) -> bool {
        if let Err(why) = self.launcher.thaw() {
            self.stop(StopReason::GuestExited {
                why: format!("the frozen guest could not be resumed: {why}"),
                diagnostics: None,
            });
            return false;
        }
        if let Some(frozen_at) = self.frozen_at.take() {
            self.discount_frozen_time(frozen_at.elapsed());
        }
        if std::mem::take(&mut self.power_pending) {
            if let Some(power) = self.power {
                self.send_body(Body::PowerState(power.to_control()));
            }
        }
        if std::mem::take(&mut self.refresh_pending) {
            self.update_refresh();
        }
        if std::mem::take(&mut self.host_memory_pending) {
            let low = self.host_memory_low;
            self.send_body(Body::HostMemory(control::HostMemory { low }));
        }
        if std::mem::take(&mut self.system_pending) {
            self.replay_system_settings();
            self.replay_gamepads();
        }
        // A change to the host's root certificates while the guest was frozen was not heard.
        let body = self.shared.host_certificates_body();
        self.send_body(body);
        true
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
        self.refresh_input(RefreshInput::BatterySaver(state.power_saver));
    }

    /// Records a change to the user's fastest monitor or the host's battery saver, and tells a
    /// guest that can hear it the display refresh rate that follows.
    fn refresh_input(&mut self, input: RefreshInput) {
        match input {
            RefreshInput::MonitorHz(hz) => self.monitor_hz = Some(hz),
            RefreshInput::BatterySaver(on) => self.battery_saver = on,
            RefreshInput::LiveResize => {}
        }
        self.update_refresh();
    }

    fn resizing_live(&self) -> bool {
        self.shared
            .live_resizes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .any()
    }

    /// Ends the live resizes that have heard nothing for a while, and tells the guest the rate
    /// that follows.
    fn tick_refresh(&mut self) {
        let expired = self
            .shared
            .live_resizes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .expire(Instant::now());
        if expired {
            eprintln!("lifecycle: a live resize ended without its last step");
            self.update_refresh();
        }
    }

    /// Tells the guest its display refresh rate, if it changed since it was last told and the
    /// monitors are known. Until they are, the guest keeps the rate it booted with.
    fn update_refresh(&mut self) {
        let Some(monitor_hz) = self.monitor_hz else {
            return;
        };
        let resizing = self.resizing_live();
        let hz = crate::guest_refresh::refresh_rate_while(monitor_hz, self.battery_saver, resizing);
        if self.refresh_sent == Some(hz) {
            return;
        }
        match self.state() {
            SessionState::Starting
            | SessionState::Stalled
            | SessionState::Active
            | SessionState::Dozing => {
                eprintln!(
                    "lifecycle: the guest's display refreshes at {hz} Hz (fastest monitor \
                     {monitor_hz} Hz, battery saver {}, live resize {})",
                    if self.battery_saver { "on" } else { "off" },
                    if resizing { "on" } else { "off" }
                );
                self.refresh_sent = Some(hz);
                self.send_body(Body::DisplayRefresh(control::DisplayRefresh { hz }));
            }
            SessionState::Frozen => self.refresh_pending = true,
            _ => {}
        }
    }

    /// Records a change to the host's system settings or controllers, and tells a guest that
    /// can hear it.
    fn system_input(&mut self, input: SystemInput) {
        let body = match input {
            SystemInput::Settings(settings) => {
                if self.system_settings.as_ref() == Some(&settings) {
                    return;
                }
                let body = Body::SystemSettings(settings.to_control());
                self.system_settings = Some(settings);
                body
            }
            SystemInput::Gamepad(event) => {
                if !self.gamepads.apply(&event) {
                    return;
                }
                event.to_control()
            }
        };
        match self.state() {
            SessionState::Starting
            | SessionState::Stalled
            | SessionState::Active
            | SessionState::Dozing => self.send_body(body),
            SessionState::Frozen => self.system_pending = true,
            _ => {}
        }
    }

    /// Gives a guest that has just come every connected controller and its state.
    fn replay_gamepads(&mut self) {
        for event in self.gamepads.replay() {
            self.send_body(event.to_control());
        }
    }

    /// Gives an agent that has just come the host's system settings.
    fn replay_system_settings(&mut self) {
        if let Some(settings) = &self.system_settings {
            let body = Body::SystemSettings(settings.to_control());
            self.send_body(body);
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
        // Nothing moves on while the host sleeps; its timers start again when it wakes.
        if self.host_asleep.is_some() {
            return;
        }
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

    /// The guest's control daemon says how much memory it has: the VM grows when the guest is
    /// short and gives memory back once it is not (see [`MemoryGrowth`]). A resize the
    /// launcher refuses or that times out is tried again in smaller steps; only memory that
    /// was added and never reached the guest stops the growing.
    ///
    /// The resize itself runs on a thread of its own when the launcher gives a
    /// [`MemoryResizer`]: Hyper-V took 45 s to answer one, and the session's thread, which
    /// serves windows, input and the apps' requests, must not wait for it. Its outcome comes
    /// back as a command ([`Worker::memory_resized`]). [`MemoryGrowth`] asks for no other
    /// resize while one is in flight.
    fn guest_memory_pressure(&mut self, pressure: GuestPressure) {
        let Some(growth) = self.memory_growth.as_mut() else {
            return;
        };
        let before = growth.current_mb();
        let mb = match growth.on_pressure(&pressure, Instant::now()) {
            None => return,
            Some(GrowthAction::GiveUp(why)) => {
                eprintln!("memory: Android's memory no longer grows: {why}");
                self.launcher.memory_growth_failed(&why);
                return;
            }
            Some(GrowthAction::Resize(mb)) => mb,
        };
        let outcome = |result| ResizeOutcome {
            generation: 0,
            before,
            mb,
            available_mb: pressure.mem_available_kb / 1024,
            stall_percent: pressure.stall_percent,
            result,
        };
        let generation = self.memory_generation;
        let Some(mut resizer) = self.launcher.memory_resizer() else {
            let result = self.launcher.set_memory_mb(mb);
            self.memory_resized(ResizeOutcome {
                generation,
                ..outcome(result)
            });
            return;
        };
        let commands = self.own_commands.clone();
        let mut done = ResizeOutcome {
            generation,
            ..outcome(Ok(()))
        };
        let spawned = thread::Builder::new()
            .name("dsa-memory-resize".into())
            .spawn(move || {
                done.result = resizer.set_memory_mb(done.mb);
                let _ = commands.send(Command::MemoryResized(done));
            });
        if let Err(error) = spawned {
            self.memory_resized(ResizeOutcome {
                generation,
                ..outcome(Err(format!("the resize could not be started: {error}")))
            });
        }
    }

    /// A resize asked for by [`Worker::guest_memory_pressure`] finished. One from an earlier
    /// boot of the guest says nothing about this one and is dropped.
    fn memory_resized(&mut self, outcome: ResizeOutcome) {
        if outcome.generation != self.memory_generation {
            return;
        }
        let Some(growth) = self.memory_growth.as_mut() else {
            return;
        };
        let ResizeOutcome {
            before, mb, result, ..
        } = outcome;
        match result {
            Ok(()) => {
                growth.applied();
                self.shared.set_vm_memory_mb(Some(mb));
                eprintln!(
                    "memory: Android's memory {} from {before} to {mb} MiB ({} MiB available, \
                     {:.1}% stalled)",
                    if mb > before { "grows" } else { "shrinks" },
                    outcome.available_mb,
                    outcome.stall_percent
                )
            }
            Err(why) => {
                // A refused or timed-out resize is tried again in a smaller step; it never
                // stops later ones (see MemoryGrowth::refused).
                let now = Instant::now();
                growth.refused(now);
                let wait = growth.retry_in(now).unwrap_or_default().as_secs();
                eprintln!(
                    "memory: Android's memory could not be set from {before} to {mb} MiB \
                     ({why}); it stays at {before} MiB and is resized again in smaller steps \
                     in {wait} s"
                );
            }
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
        // The jobs of the agent that was there end with it.
        self.end_print_jobs();
        // A new agent takes the screen to be on, so what the last one said of Doze is over.
        self.guest_is_idle = false;
        self.stall_doze = None;
        if boot.agent_connected {
            self.agent_lost_at = None;
            eprintln!("lifecycle: the agent is connected");
            if let Some(power) = self.power {
                self.send_body(Body::PowerState(power.to_control()));
            }
            // A new agent holds the display at the rate it is told, from the start. A resize
            // under way went to the agent before it, and the next live step starts it again.
            self.refresh_sent = None;
            self.shared
                .live_resizes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            self.update_refresh();
            if self.host_memory_low {
                self.send_body(Body::HostMemory(control::HostMemory { low: true }));
            }
            // The host's system settings become the guest's, and its controllers are there.
            self.replay_system_settings();
            self.replay_gamepads();
            // The computer's name becomes the guest's device name.
            let source = self
                .shared
                .device_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if let Some(name) = source.and_then(|source| source()) {
                self.send_body(Body::DeviceName(control::DeviceName { name }));
            }
            if !self.screen_on {
                self.send_body(Body::ScreenState(control::ScreenState { on: false }));
            }
            // An agent that was gone when the host woke has the guest's clock set now.
            self.send_clock();
            // A new agent takes links to go to the host.
            if self.links_in_android {
                self.send_body(Body::LinkHandling(control::LinkHandling { in_guest: true }));
            }
            // A guest boots with adbd stopped.
            if self.shared.developer_mode.load(Ordering::SeqCst) {
                self.send_body(Body::DeveloperMode(control::DeveloperMode { on: true }));
            }
            // A guest boots with no host folder mounted, and without serving its own files.
            if let Some(body @ Body::FileSharing(control::FileSharing { on: true, .. })) =
                self.shared.file_sharing_body()
            {
                self.send_body(body);
            }
            if let Some(body) = self.shared.android_files_body() {
                self.send_body(body);
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
            // With the agent there, the package manager knows every app's uid, so the daemon
            // can set up the kernel's su guard for the user's root elevation settings.
            if let Some(body) = self.shared.root_elevation_body(true) {
                self.send_body(body);
            }
            // A new agent keeps Android's backup manager from backing up to the host until it
            // hears that the user turned backup on.
            let body = self.shared.backup_setting_body();
            self.send_body(body);
            // The root certificates added on the host again: a change the daemon could not
            // hear while Android was starting reaches it now, and the new agent has the apps
            // that started before it forget the anchors they looked up.
            let body = self.shared.host_certificates_body();
            self.send_body(body);
        } else {
            self.agent_lost_at = Some(now);
            // The tasks are what the agent reported, and the agent goes when Android's framework
            // restarts, which ends every app's task with it. Their windows close rather than
            // keep a last frame nothing answers, and a task number the new framework hands out
            // again is a new task. Tasks that outlived an agent alone are reported again by the
            // next one, and their windows open again.
            self.shared.clear_tasks();
            eprintln!("lifecycle: the agent is gone; the tasks it reported have ended");
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
                self.shared.forget_last_reason();
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
        // The boot takes the root elevation switch as it stands now.
        self.shared.root_elevation_boot(true);
        let started = self.launcher.start(&secret);
        *self
            .shared
            .boot_secret
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(secret.clone());
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
        self.memory_generation = self.memory_generation.wrapping_add(1);
        self.memory_growth = self.launcher.memory_range_mb().map(|(base, max)| {
            eprintln!("memory: Android starts with {base} MiB and may grow to {max} MiB");
            MemoryGrowth::new(base, max)
        });
        self.shared
            .set_vm_memory_mb(self.memory_growth.as_ref().map(MemoryGrowth::current_mb));
        let started = Instant::now();
        self.started_at = started;
        // A new boot of Android: the root policy is read again and no app keeps a grace.
        if let Some((approval, _)) = &*self
            .shared
            .root_approval
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            approval.restart();
        }
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
                            // A new boot's daemon reads no log until it is asked; with readers
                            // waiting, it starts with the newest records the boot has logged.
                            let mut stream = self
                                .shared
                                .logs
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .stream();
                            if stream.on {
                                stream.tail = STREAM_TAIL;
                                self.send_body(Body::LogStream(stream));
                            }
                            // The agent takes the screen to be on when it connects. The session
                            // stays Starting until the guest says it is ready.
                            self.screen_on = true;
                            self.hidden_since = Instant::now();
                            self.dozing_since = None;
                            self.power_pending = false;
                            self.host_memory_pending = false;
                            // A new boot's control daemon has no controller yet.
                            self.system_pending = false;
                            self.replay_gamepads();
                            // A boot trusts none of the root certificates added on the host
                            // until its daemon is told them, before any app reaches the network.
                            let body = self.shared.host_certificates_body();
                            self.send_body(body);
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
            Command::Send(_, reply)
            | Command::Freeze(reply)
            | Command::Notification(_, reply)
            | Command::Background(_, reply) => {
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
            Command::Refresh(input) => {
                self.refresh_input(input);
                true
            }
            Command::HostMemoryLow(low) => {
                self.set_host_memory_low(low);
                true
            }
            Command::System(input) => {
                self.system_input(input);
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
            // No daemon is connected yet, so nothing in the guest waits to be written.
            Command::WidgetPressed(reply) | Command::HostSleep(reply) => {
                let _ = reply.send(());
                true
            }
            // A guest still connecting is told the clock once its agent is there.
            Command::HostWoke(wall_clock) => {
                self.host_asleep = None;
                self.clock_pending = Some((wall_clock, Instant::now()));
                true
            }
            Command::MemoryResized(outcome) => {
                self.memory_resized(outcome);
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
            self.tick_refresh();
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
                let orientation = match update.orientation() {
                    control::TaskOrientation::Portrait => TaskOrientation::Portrait,
                    control::TaskOrientation::Landscape => TaskOrientation::Landscape,
                    control::TaskOrientation::Unspecified => TaskOrientation::Unspecified,
                };
                let status_bar = match update.status_bar_appearance() {
                    control::StatusBarAppearance::Light => StatusBarAppearance::Light,
                    control::StatusBarAppearance::Dark => StatusBarAppearance::Dark,
                    control::StatusBarAppearance::Unspecified => StatusBarAppearance::Unspecified,
                };
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
                    status_bar,
                    status_bar_color: update.status_bar_color,
                    orientation,
                    fullscreen: update.fullscreen,
                    picture_in_picture: update.picture_in_picture,
                    transparent_caption: update.transparent_caption,
                    caption_interactive: update
                        .caption_interactive
                        .iter()
                        .map(|rect| Bounds {
                            left: rect.left,
                            top: rect.top,
                            right: rect.right,
                            bottom: rect.bottom,
                        })
                        .collect(),
                    can_go_back: update.can_go_back,
                    handles_density_change: update.handles_density_change,
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
                    caption_icon_png: app.caption_icon_png,
                    multiple_windows: app.multiple_windows,
                    ..Default::default()
                }));
            }
            Some(Body::InstalledApp(app)) => {
                self.shared.describe_installed(AppInfo {
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
                    native_libraries: NativeLibraries::from_control(&app.native_libraries),
                    primary_abi: native_abi::checked_abi(&app.primary_abi),
                    shared_user_id: app.shared_user_id,
                    ..Default::default()
                });
            }
            Some(Body::InstalledApps(apps)) => {
                self.shared.publish_installed(&apps.package_names);
                self.shared.apps_refreshed();
            }
            Some(Body::AppShortcuts(shortcuts)) => {
                let list = shortcuts
                    .shortcuts
                    .into_iter()
                    .map(shortcut_from_control)
                    .collect();
                self.shared.publish_shortcuts(&shortcuts.package_name, list);
            }
            Some(Body::PinShortcutRequest(request)) => {
                let Some(shortcut) = request.shortcut else {
                    eprintln!(
                        "apps: {} asked to pin a shortcut but gave none; it is ignored",
                        request.package_name
                    );
                    return;
                };
                self.shared
                    .publish_task(TaskEvent::PinShortcut(PinShortcutRequest {
                        request_id: request.request_id,
                        package_name: request.package_name,
                        app_label: request.app_label,
                        shortcut: shortcut_from_control(shortcut),
                    }));
            }
            Some(Body::ReinstallResult(result)) => self.guest_reinstall_result(result),
            Some(Body::NativeLibraryCrash(crash)) => self.native_library_crash(crash),
            Some(Body::UninstallResult(result)) => {
                let outcome = UninstallOutcome::from_control(&result);
                let waiting = self
                    .shared
                    .uninstalls
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&result.request_id);
                match waiting {
                    Some(waiting) => {
                        let _ = waiting.send(outcome);
                    }
                    None => eprintln!(
                        "apps: dropped the agent's answer to uninstall request {}, which nobody \
                         waits for",
                        result.request_id
                    ),
                }
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
            Some(Body::ImeEditor(editor)) => match ImeEditor::from_control(&editor) {
                Ok((task_id, editor)) => self
                    .shared
                    .publish_task(TaskEvent::ImeEditor { task_id, editor }),
                Err(why) => eprintln!(
                    "text input: dropped the agent's description of the editor in task {}: \
                     {why}",
                    editor.task_id
                ),
            },
            Some(Body::GuestMemory(memory)) => {
                let memory = GuestMemory::from_control(memory);
                self.shared.set_guest_memory(Some((memory, Instant::now())));
            }
            Some(Body::GuestMemoryPressure(pressure)) => {
                self.guest_memory_pressure(GuestPressure::from_control(&pressure))
            }
            Some(Body::DeviceIdle(idle)) => self.guest_idle(idle.idle),
            Some(Body::GuestSleepReady(_)) => self.guest_sleep_ready(),
            Some(Body::OpenLink(link)) => self.guest_link(link.url),
            Some(Body::FileProgress(progress)) => self.shared.file_progress(progress),
            Some(Body::ClipboardText(clip)) => self.guest_clipboard(clip.text),
            Some(Body::GamepadRumble(rumble)) => self
                .shared
                .publish_rumble(GamepadRumble::from_control(&rumble)),
            Some(Body::Notification(notification)) => self.guest_notification(notification),
            Some(Body::NotificationRemoved(removed)) => {
                self.guest_notification_removed(removed.key)
            }
            Some(Body::BackgroundApp(app)) => match background::checked_app(app) {
                Ok(app) => self.publish_background(BackgroundEvent::App(app)),
                Err(why) => eprintln!("background apps: dropped one from the guest: {why}"),
            },
            Some(Body::BackgroundAppGone(gone)) => {
                self.publish_background(BackgroundEvent::AppRemoved {
                    package_name: gone.package_name,
                })
            }
            Some(Body::MediaSession(session)) => match background::checked_media(session) {
                Ok(session) => self.publish_background(BackgroundEvent::Media(session)),
                Err(why) => eprintln!("background apps: dropped a media session: {why}"),
            },
            Some(Body::MediaSessionGone(gone)) => {
                self.publish_background(BackgroundEvent::MediaRemoved {
                    package_name: gone.package_name,
                })
            }
            Some(Body::AppAudio(list)) => self
                .shared
                .app_audio
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .apply(app_audio::checked_streams(list)),
            Some(Body::InstallProgress(progress)) => self.shared.install_event(
                progress.transfer_id,
                InstallEvent::Progress(progress.received),
            ),
            Some(Body::ArmTranslationState(state)) => self.shared.arm_translation_event(state),
            Some(Body::GappsState(state)) => self.shared.gapps_event(state),
            Some(Body::AddonImagePart(part)) => self.shared.addon_image_event(part),
            Some(Body::GoogleDeviceId(id)) => self.shared.device_id_event(id.id),
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
            Some(Body::WidgetReconfigured(answer)) => self.shared.widget_answer(
                answer.request_id,
                WidgetAnswer::Reconfigured(if answer.finished {
                    Ok(())
                } else if answer.error.trim().is_empty() {
                    Err("the widget's settings were closed without a change".into())
                } else {
                    Err(answer.error.chars().take(512).collect())
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
            Some(Body::DragOffer(offer)) => {
                let waiting = self
                    .shared
                    .drag_offers
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&offer.request_id);
                let late = match waiting {
                    Some(waiting) => waiting.send(offer).err().map(|e| e.0),
                    None => Some(offer),
                };
                // Nobody waits for this offer any more, so the drag is over on the host: the
                // guest gives its access back now rather than holding it.
                if let Some(late) = late {
                    if late.drag_id != 0 {
                        eprintln!(
                            "drag: Android offered drag {} after the host stopped asking; ending it",
                            late.drag_id
                        );
                        self.send_body(Body::DragEnd(control::DragEnd {
                            drag_id: late.drag_id,
                            dropped: false,
                        }));
                    }
                }
            }
            Some(Body::DragData(data)) => {
                let waiting = self
                    .shared
                    .drag_reads
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&data.request_id);
                if let Some(waiting) = waiting {
                    let _ = waiting.send(data);
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
            Some(Body::PrintJob(job)) => {
                let step = self.prints().start(job);
                self.print_step(step);
            }
            Some(Body::PrintJobChunk(chunk)) => {
                let step = self.prints().chunk(chunk);
                self.print_step(step);
            }
            Some(Body::PrintJobCancel(cancel)) => {
                let step = self.prints().cancel(cancel.job_id);
                self.print_step(step);
            }
            Some(Body::GeocodeRequest(request)) => self.geocode_request(request),
            Some(Body::ShareToHost(share)) => self.share_to_host(share),
            Some(Body::PowerHint(hint)) => match PowerHint::from_control(&hint) {
                Ok(hint) => self.shared.publish_power(GuestPower::Hint(hint)),
                Err(why) => eprintln!("power: dropped a hint from the guest: {why}"),
            },
            Some(Body::LocationRequest(request)) => {
                self.shared.set_location(Wanted::from_control(&request))
            }
            Some(Body::SuRequest(request)) => self.su_request(request),
            Some(
                body @ (Body::BackupUpload(_)
                | Body::BackupCommit(_)
                | Body::BackupDiscard(_)
                | Body::BackupClear(_)
                | Body::BackupQuery(_)
                | Body::BackupFetch(_)
                | Body::BackupRunResult(_)
                | Body::PasskeyRequest(_)),
            ) => {
                let (shared, commands) = (Arc::clone(&self.shared), self.own_commands.clone());
                let reply = move |body| send_quietly(&shared, &commands, body);
                if !self.shared.host_services_message(body, reply) {
                    eprintln!("control channel: dropping a message no host service takes");
                }
            }
            Some(Body::KeymintRootKeyRequest(_)) => {
                eprintln!(
                    "control channel: dropping a request for the KeyMint root key, which belongs \
                     on the KeyMint port"
                );
            }
            Some(Body::DnsQuery(_) | Body::DnsAnswer(_)) => {
                eprintln!("control channel: dropping a DNS message, which belongs on the DNS port");
            }
            Some(
                body @ (Body::ListSpeechVoices(_)
                | Body::SpeakText(_)
                | Body::StopSpeaking(_)
                | Body::StartRecognition(_)
                | Body::StopRecognition(_)
                | Body::CancelRecognition(_)
                | Body::CheckSpelling(_)),
            ) => {
                let replies = self.shared.language().from_guest(body);
                for reply in replies {
                    self.send_body(reply);
                }
            }
            Some(Body::SpeechVoices(_))
            | Some(Body::SpeechAudio(_))
            | Some(Body::SpeechDone(_))
            | Some(Body::RecognitionEvent(_))
            | Some(Body::SpellingResult(_)) => {
                eprintln!(
                    "control channel: dropping a speech or spelling answer only the host sends"
                );
            }
            Some(Body::ScreenState(_))
            | Some(Body::PowerState(_))
            | Some(Body::DisplayRefresh(_))
            | Some(Body::HostMemory(_))
            | Some(Body::HostSleep(_))
            | Some(Body::CloseTask(_))
            | Some(Body::LaunchApp(_))
            | Some(Body::ResizeTask(_))
            | Some(Body::SetTaskDensity(_))
            | Some(Body::SetTaskCaption(_))
            | Some(Body::PointerEvent(_))
            | Some(Body::KeyEvent(_))
            | Some(Body::ImeComposition(_))
            | Some(Body::ImeCommit(_))
            | Some(Body::ImeDeleteSurrounding(_))
            | Some(Body::ImeSetSelection(_))
            | Some(Body::ImeSetComposingRegion(_))
            | Some(Body::LinkHandling(_))
            | Some(Body::OpenFile(_))
            | Some(Body::FileChunk(_))
            | Some(Body::ClipboardSharing(_))
            | Some(Body::DeveloperMode(_))
            | Some(Body::FileSharing(_))
            | Some(Body::AndroidFiles(_))
            | Some(Body::DragOut(_))
            | Some(Body::DragRead(_))
            | Some(Body::DragEnd(_))
            | Some(Body::ArmTranslationRemove(_))
            | Some(Body::ArmTranslationQuery(_))
            | Some(Body::GappsRemove(_))
            | Some(Body::AddonImageRead(_))
            | Some(Body::GappsQuery(_))
            | Some(Body::GoogleDeviceIdQuery(_))
            | Some(Body::NotificationClicked(_))
            | Some(Body::NotificationDismissed(_))
            | Some(Body::BackgroundAppAction(_))
            | Some(Body::MediaControl(_))
            | Some(Body::InstallApk(_))
            | Some(Body::ApkChunk(_))
            | Some(Body::CancelInstall(_))
            | Some(Body::PrintJobResult(_))
            | Some(Body::GeocodeResult(_))
            | Some(Body::ShareResult(_))
            | Some(Body::QueryPackage(_))
            | Some(Body::UninstallApp(_))
            | Some(Body::ReinstallApp(_))
            | Some(Body::RefreshApps(_))
            | Some(Body::OpenAppSettings(_))
            | Some(Body::RequestGuestMemory(_))
            | Some(Body::StopApp(_))
            | Some(Body::LaunchShortcut(_))
            | Some(Body::PinShortcutAnswer(_))
            | Some(Body::ListWidgetProviders(_))
            | Some(Body::AddWidget(_))
            | Some(Body::RemoveWidget(_))
            | Some(Body::ReconfigureWidget(_))
            | Some(Body::ResizeWidget(_))
            | Some(Body::WidgetPointer(_))
            | Some(Body::AccessibilityWatch(_))
            | Some(Body::AccessibilityAction(_))
            | Some(Body::LogStream(_))
            | Some(Body::LogDump(_))
            | Some(Body::CollectDiagnostics(_))
            | Some(Body::ClockProbe(_))
            | Some(Body::RootElevationConfig(_))
            | Some(Body::SuDecision(_))
            | Some(Body::AudioOutputOpen(_))
            | Some(Body::AudioOutputReady(_))
            | Some(Body::AudioPcm(_))
            | Some(Body::AudioOutputControl(_))
            | Some(Body::AudioInputOpen(_))
            | Some(Body::AudioInputOpened(_))
            | Some(Body::VideoCapabilities(_))
            | Some(Body::VideoDecoderOpen(_))
            | Some(Body::VideoDecoderOpened(_))
            | Some(Body::VideoPacket(_))
            | Some(Body::VideoFrame(_))
            | Some(Body::VideoFlush(_))
            | Some(Body::VideoFlushed(_))
            | Some(Body::VideoError(_))
            | Some(Body::LocationStatus(_))
            | Some(Body::LocationFix(_))
            | Some(Body::SystemSettings(_))
            | Some(Body::DeviceName(_))
            | Some(Body::GamepadConnected(_))
            | Some(Body::GamepadDisconnected(_))
            | Some(Body::GamepadState(_))
            | Some(Body::BackupAnswer(_))
            | Some(Body::BackupContents(_))
            | Some(Body::BackupFetchData(_))
            | Some(Body::BackupRun(_))
            | Some(Body::BackupRestore(_))
            | Some(Body::PasskeyResponse(_))
            | Some(Body::KeymintRootKey(_))
            | Some(Body::BackupSetting(_))
            | Some(Body::HostCertificates(_))
            | Some(Body::SetAppVolume(_)) => {
                eprintln!("control channel: dropping a request only the host sends");
            }
            Some(Body::SensorList(_))
            | Some(Body::SensorActivate(_))
            | Some(Body::SensorReadings(_)) => {
                eprintln!(
                    "control channel: dropping a sensor message, which belongs on the sensor port"
                );
            }
            Some(Body::CameraList(_))
            | Some(Body::CameraOpen(_))
            | Some(Body::CameraOpened(_))
            | Some(Body::CameraFrame(_))
            | Some(Body::CameraStop(_))
            | Some(Body::CameraStill(_))
            | Some(Body::CameraStillTaken(_))
            | Some(Body::CameraError(_)) => {
                eprintln!(
                    "control channel: dropping a camera message, which belongs on the camera port"
                );
            }
            Some(Body::LogRecords(_))
            | Some(Body::DiagnosticsFile(_))
            | Some(Body::DiagnosticsDone(_)) => {
                eprintln!("control channel: dropping a log message, which belongs on the log port");
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
        // So do its print jobs.
        self.end_print_jobs();
        // And its shares: an outcome that comes later is for no share the next guest has.
        self.shared
            .open_shares
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // So do its notifications.
        self.clear_notifications();
        // And its background apps and media sessions.
        self.shared
            .background
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // And its outputs of sound.
        self.shared
            .app_audio
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // And the modes and boosts its power HAL asked for.
        self.shared.publish_power(GuestPower::Stopped);
        // And its wish for the host's location, so the host's location service stops.
        self.shared.set_location(Wanted::NONE);
        // And its backups, restores and passkey ceremonies under way.
        self.shared.host_services_gone();
        // And what it asked of the host's speech and spelling.
        self.shared.language().guest_gone();
        self.shared.set_guest_memory(None);
        self.shared.set_vm_memory_mb(None);
        // A dump or diagnostics under way gets no more from this guest.
        self.shared
            .logs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .guest_gone("Android stopped");
        *self
            .shared
            .boot_secret
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        self.shared.root_elevation_boot(false);
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
