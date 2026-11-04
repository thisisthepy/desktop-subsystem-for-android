//! Who gets which frame: the daemon's routing between the host and the guest's own users of
//! the channel.
//!
//! - A frame from the host goes to every live listener.
//! - A frame a guest client sends goes to the host. A task's bounds or its end
//!   (`TaskUpdated`, `TaskRemoved`) also go to every listener in the other guest processes,
//!   because the hwcomposer groups layers by the task bounds the agent reports.
//! - A listener whose process has died is dropped.
//! - The daemon keeps the latest state of each live task (its `TaskUpdated` and its
//!   `TaskFrames`) and the latest `AppInfo` of each app with a live task. A listener that
//!   subscribes later, such as a restarted hwcomposer, is told the tasks already there, and
//!   the host each time it connects again is told the apps and then the tasks, since a client
//!   sends them only when they change.
//! - The daemon keeps the host's latest `PowerState`, which a listener that subscribes later,
//!   such as a restarted health HAL, is told first, and each power mode the power HAL has on
//!   (`PowerHint`), which the host is told each time it connects again.
//! - The agent says it is there (`AgentConnected`); that stops here. The host hears how far the
//!   guest has got (`GuestBoot`) each time it connects and on each change: whether Android has
//!   finished booting, and whether the agent is connected, which ends when its process dies.
//!   Only the agent may say so: the platform gives the router a check for the caller, and
//!   [`agent_caller`] is the rule it applies to what binder and the kernel say of the caller.
//!
//! - The diagnostics the agent gathers go to the log port ([`Router::divert_diagnostics`]),
//!   never on the control channel, and only the agent may send them. The guest's log is the
//!   daemon's own to send; no client may.
//!
//! None of this knows about binder: a listener is anything that can take a frame, so the
//! rules run and are tested on any platform. The binder service in `binder` is a thin layer
//! that turns calls into these.

use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};

use dsa_protocol::control::envelope::Body;
use dsa_protocol::control::{Envelope, GuestBoot, PowerHintKind};

/// The agent's package, which is the name of its process.
pub const AGENT_PROCESS: &str = "io.github.thisisthepy.dsa.agent";

/// Whether a caller is the agent, from its SELinux context as binder reports it and its process's
/// `/proc/<pid>/cmdline`. The domain comes from the kernel, and keeps out the hwcomposer and
/// anything else that may call the daemon. The name tells the agent from other platform-signed
/// apps in the same domain. An error says what did not match.
pub fn agent_caller(context: Option<&str>, cmdline: Option<&[u8]>) -> Result<(), String> {
    let context = context.ok_or_else(|| "binder did not say the caller's context".to_string())?;
    let domain = context
        .strip_prefix("u:r:")
        .and_then(|rest| rest.split(':').next())
        .unwrap_or_default();
    if !matches!(domain, "platform_app" | "system_app") {
        return Err(format!(
            "the caller runs in {context:?}, not as a platform app"
        ));
    }
    let cmdline = cmdline.ok_or_else(|| "the caller's process name cannot be read".to_string())?;
    let process = cmdline.split(|byte| *byte == 0).next().unwrap_or_default();
    if process != AGENT_PROCESS.as_bytes() {
        return Err(format!(
            "the caller is the process {:?}, not {AGENT_PROCESS}",
            String::from_utf8_lossy(process)
        ));
    }
    Ok(())
}

/// The guest process a call came from (its process ID, as binder reports the caller).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Peer(pub i32);

/// Names one subscription, so the listener can be dropped when its process dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ListenerId(pub u64);

/// What became of one frame handed to a listener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    Delivered,
    /// The listener's process is gone; it never takes another frame.
    Gone,
    /// This frame did not arrive (the listener's queue was full, say), but the listener
    /// lives and takes the next one.
    Failed,
}

/// A guest client's callback for frames. It must not block: the router calls it with its
/// state locked, which keeps every listener's frames in the order they were routed.
pub trait Listener: Send + Sync {
    fn deliver(&self, frame: &[u8]) -> Delivery;
}

/// Why a frame a guest client sent was not passed on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// It is not one control message.
    Malformed(String),
    /// It is a message only the daemon itself may send, or carries no message at all.
    NotAllowed(String),
}

impl std::fmt::Display for SendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendError::Malformed(why) | SendError::NotAllowed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for SendError {}

struct Subscription {
    id: ListenerId,
    peer: Peer,
    listener: Arc<dyn Listener>,
}

/// The most apps the daemon keeps an `AppInfo` for. An app's is dropped when its last task
/// ends, so only a broken or hostile client reaches this; past it, the frames still go to the
/// host but are not kept.
pub const MAX_APPS: usize = 1024;

/// The most power modes the daemon keeps on. android.hardware.power has fewer than this.
pub const MAX_POWER_MODES: usize = 64;

/// The latest state of one live task, as the frames that carried it.
#[derive(Default)]
struct Task {
    /// The last `TaskUpdated`, and who sent it.
    updated: Option<(Peer, Vec<u8>)>,
    /// The package that `TaskUpdated` named.
    package: String,
    /// The last `TaskFrames`.
    frames: Option<Vec<u8>>,
}

#[derive(Default)]
struct State {
    listeners: Vec<Subscription>,
    tasks: BTreeMap<i32, Task>,
    /// The last `AppInfo` of each app, by package.
    apps: BTreeMap<String, Vec<u8>>,
    /// The host said no task window is shown: open tasks are minimized, and the guest dozes.
    screen_off: bool,
    /// The host said it is short of memory.
    host_memory_low: bool,
    next_id: u64,
    /// Android has finished booting.
    boot_completed: bool,
    /// The process of the agent, while it is connected.
    agent: Option<Peer>,
    /// The host's last `PowerState`.
    power_state: Option<Vec<u8>>,
    /// The `PowerHint` that turned each power mode on, by mode, while it is on.
    power_modes: BTreeMap<i32, Vec<u8>>,
}

impl State {
    /// Hands `frame` to every listener not in `except`'s process, and drops those whose
    /// process is gone.
    fn deliver(&mut self, frame: &[u8], except: Option<Peer>) {
        self.listeners
            .retain(|s| Some(s.peer) == except || s.listener.deliver(frame) != Delivery::Gone);
    }

    /// What the host is told about how far the guest has got.
    fn boot_report(&self) -> Vec<u8> {
        dsa_protocol::encode(&Envelope {
            body: Some(Body::GuestBoot(GuestBoot {
                boot_completed: self.boot_completed,
                agent_connected: self.agent.is_some(),
            })),
        })
    }

    /// Whether the agent's listener is gone, which ends its connection; then forgets it.
    fn agent_left(&mut self) -> bool {
        let gone = self
            .agent
            .is_some_and(|agent| !self.listeners.iter().any(|s| s.peer == agent));
        if gone {
            self.agent = None;
        }
        gone
    }
}

/// Routes frames between the host connection and the guest's listeners.
pub struct Router {
    to_host: Sender<Vec<u8>>,
    state: Mutex<State>,
    agent_check: AgentCheck,
    /// Where the agent's diagnostics go instead of the host's control channel.
    diagnostics: Mutex<Option<Sender<Vec<u8>>>>,
}

/// Says whether the caller of the current call is the agent. It runs on the caller's thread.
type AgentCheck = Box<dyn Fn(Peer) -> Result<(), String> + Send + Sync>;

impl Router {
    /// A router that hands the host's share of guest frames to `to_host`, the channel the
    /// connection pump drains. `agent_check` says whether a caller is the agent.
    pub fn new(
        to_host: Sender<Vec<u8>>,
        agent_check: impl Fn(Peer) -> Result<(), String> + Send + Sync + 'static,
    ) -> Router {
        Router {
            to_host,
            state: Mutex::new(State::default()),
            agent_check: Box::new(agent_check),
            diagnostics: Mutex::new(None),
        }
    }

    /// Whether the agent is connected.
    pub fn agent_connected(&self) -> bool {
        self.lock().agent.is_some()
    }

    /// Sends the diagnostics the agent gathers (`DiagnosticsFile`, `DiagnosticsDone`) to `to`
    /// instead of the host's control channel. Without it the agent's diagnostics are refused.
    pub fn divert_diagnostics(&self, to: Sender<Vec<u8>>) {
        *self.diagnostics.lock().unwrap_or_else(|e| e.into_inner()) = Some(to);
    }

    /// Adds a listener for `peer` and tells it the live tasks other processes reported. It
    /// takes frames until it is reported gone or removed.
    pub fn subscribe(&self, peer: Peer, listener: Arc<dyn Listener>) -> ListenerId {
        let mut state = self.lock();
        state.next_id += 1;
        let id = ListenerId(state.next_id);
        if let Some(frame) = &state.power_state {
            if listener.deliver(frame) == Delivery::Gone {
                return id;
            }
        }
        for task in state.tasks.values() {
            if let Some((sender, frame)) = &task.updated {
                if *sender != peer && listener.deliver(frame) == Delivery::Gone {
                    return id;
                }
            }
        }
        state.listeners.push(Subscription { id, peer, listener });
        id
    }

    /// Removes a listener, as when its process dies. Returns whether it was still there.
    pub fn remove(&self, id: ListenerId) -> bool {
        let mut state = self.lock();
        let before = state.listeners.len();
        state.listeners.retain(|s| s.id != id);
        let removed = state.listeners.len() != before;
        self.tell_host_if_agent_left(&mut state);
        removed
    }

    /// Android has finished booting. The host is told once.
    pub fn boot_completed(&self) {
        let mut state = self.lock();
        if !state.boot_completed {
            state.boot_completed = true;
            let _ = self.to_host.send(state.boot_report());
        }
    }

    fn tell_host_if_agent_left(&self, state: &mut State) {
        if state.agent_left() {
            let _ = self.to_host.send(state.boot_report());
        }
    }

    /// How many listeners take frames now.
    pub fn listener_count(&self) -> usize {
        self.lock().listeners.len()
    }

    /// How many app tasks the guest has open: those a client has reported and not yet ended.
    pub fn task_count(&self) -> usize {
        self.lock()
            .tasks
            .values()
            .filter(|task| task.updated.is_some())
            .count()
    }

    /// How many app tasks are in use: none while the host shows no window for them, when
    /// they are as idle as if they were closed.
    pub fn tasks_in_use(&self) -> usize {
        if self.lock().screen_off {
            0
        } else {
            self.task_count()
        }
    }

    /// Whether the host last said it is short of memory.
    pub fn host_memory_low(&self) -> bool {
        self.lock().host_memory_low
    }

    /// Passes a frame from the host to every listener.
    pub fn from_host(&self, frame: &[u8]) {
        let body = dsa_protocol::decode(frame).ok().and_then(|e| e.body);
        let mut state = self.lock();
        match body {
            Some(Body::ScreenState(screen)) => state.screen_off = !screen.on,
            Some(Body::HostMemory(memory)) => state.host_memory_low = memory.low,
            Some(Body::PowerState(_)) => state.power_state = Some(frame.to_vec()),
            _ => {}
        }
        state.deliver(frame, None);
        self.tell_host_if_agent_left(&mut state);
    }

    /// Tells a host that has just connected the state of every live task. Frames that were
    /// waiting for the connection go first, so the host ends with the latest state.
    pub fn host_connected(&self) {
        let mut state = self.lock();
        // The host takes the screen to be on when it connects, and says so if not; it says
        // again if it is short of memory.
        state.screen_off = false;
        state.host_memory_low = false;
        let _ = self.to_host.send(state.boot_report());
        for frame in state.power_modes.values() {
            let _ = self.to_host.send(frame.clone());
        }
        for frame in state.apps.values() {
            let _ = self.to_host.send(frame.clone());
        }
        for task in state.tasks.values() {
            let frames = task
                .updated
                .iter()
                .map(|(_, f)| f)
                .chain(task.frames.iter());
            for frame in frames {
                let _ = self.to_host.send(frame.clone());
            }
        }
    }

    /// Passes a frame a guest client sent: to the host, and task bounds and task ends also
    /// to the listeners of every other process.
    pub fn from_guest(&self, sender: Peer, frame: Vec<u8>) -> Result<(), SendError> {
        dsa_transport::check_frame_len(frame.len())
            .map_err(|error| SendError::Malformed(error.to_string()))?;
        let envelope = dsa_protocol::decode(&frame)
            .map_err(|error| SendError::Malformed(error.to_string()))?;
        let mut state = self.lock();
        match envelope.body {
            None => {
                return Err(SendError::NotAllowed(
                    "the frame carries no message".to_string(),
                ))
            }
            Some(
                Body::Hello(_)
                | Body::HelloAccepted(_)
                | Body::HelloRejected(_)
                | Body::AuthChallenge(_)
                | Body::AuthProof(_),
            ) => {
                return Err(SendError::NotAllowed(
                    "the handshake is the daemon's own; a guest client cannot send its messages"
                        .to_string(),
                ))
            }
            Some(Body::GuestBoot(_)) => {
                return Err(SendError::NotAllowed(
                    "how far the guest has got is the daemon's to say".to_string(),
                ))
            }
            Some(Body::AgentConnected(_)) => {
                if !state.listeners.iter().any(|s| s.peer == sender) {
                    return Err(SendError::NotAllowed(
                        "an agent subscribes before it says it is connected".to_string(),
                    ));
                }
                (self.agent_check)(sender).map_err(|why| {
                    SendError::NotAllowed(format!("only the agent may say it is connected: {why}"))
                })?;
                if state.agent != Some(sender) {
                    state.agent = Some(sender);
                    let _ = self.to_host.send(state.boot_report());
                }
                return Ok(());
            }
            Some(Body::TaskUpdated(updated)) => {
                let task = state.tasks.entry(updated.task_id).or_default();
                task.updated = Some((sender, frame.clone()));
                task.package = updated.package_name;
                state.deliver(&frame, Some(sender));
            }
            Some(Body::TaskRemoved(removed)) => {
                if let Some(task) = state.tasks.remove(&removed.task_id) {
                    if !state.tasks.values().any(|t| t.package == task.package) {
                        state.apps.remove(&task.package);
                    }
                }
                state.deliver(&frame, Some(sender));
            }
            Some(Body::AppInfo(app)) => {
                if state.apps.contains_key(&app.package_name) || state.apps.len() < MAX_APPS {
                    state.apps.insert(app.package_name, frame.clone());
                }
            }
            Some(Body::TaskFrames(frames)) => {
                state.tasks.entry(frames.task_id).or_default().frames = Some(frame.clone());
            }
            Some(Body::DiagnosticsFile(_) | Body::DiagnosticsDone(_)) => {
                // The agent's part of the diagnostics goes on the log port, never on the
                // control channel, so a large dump cannot hold up input.
                (self.agent_check)(sender).map_err(|why| {
                    SendError::NotAllowed(format!("only the agent sends diagnostics: {why}"))
                })?;
                let diverted = self
                    .diagnostics
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .as_ref()
                    .map(|to| to.send(frame).is_ok())
                    .unwrap_or(false);
                if !diverted {
                    return Err(SendError::NotAllowed(
                        "no diagnostics are being gathered".to_string(),
                    ));
                }
                return Ok(());
            }
            Some(
                Body::LogStream(_)
                | Body::LogDump(_)
                | Body::LogRecords(_)
                | Body::CollectDiagnostics(_),
            ) => {
                return Err(SendError::NotAllowed(
                    "the guest's log is the daemon's to send".to_string(),
                ))
            }
            Some(Body::PowerHint(hint)) if hint.kind == PowerHintKind::Mode as i32 => {
                if !hint.enabled {
                    state.power_modes.remove(&hint.id);
                } else if state.power_modes.contains_key(&hint.id)
                    || state.power_modes.len() < MAX_POWER_MODES
                {
                    state.power_modes.insert(hint.id, frame.clone());
                }
            }
            Some(_) => {}
        }
        self.tell_host_if_agent_left(&mut state);
        // Sent with the state locked, so the host gets the frames in the order the state
        // changed. The pump owns the other end for as long as the daemon runs; if it has
        // gone, the daemon is stopping and the frame has nowhere to go.
        let _ = self.to_host.send(frame);
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the state itself whole; keep routing.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
