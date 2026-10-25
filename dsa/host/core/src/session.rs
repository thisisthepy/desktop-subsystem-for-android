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
//! The states are those of the lifecycle: `Stopped`, `Starting`, `Active`, `Dozing`,
//! `Frozen` and `Stopping`. This module moves between `Stopped`, `Starting`, `Active` and
//! `Stopping`. Entering `Dozing` and `Frozen` belongs to the lifecycle policy, which is not
//! implemented yet; a session in either state can still be stopped.

use std::fmt;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_protocol::auth::BootSecret;
use dsa_protocol::control::{self, envelope::Body};
use dsa_transport::{Connection, Listener, TransportError};

use crate::tasks::{Bounds, Task, TaskEvent, TaskRequest, TaskTable};

/// Where the session is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionState {
    /// No guest is running.
    Stopped,
    /// The guest was started and the host is waiting for the agent to connect.
    Starting,
    /// The agent is connected and the handshake succeeded.
    Active,
    /// No task window is visible and the guest is in Doze.
    Dozing,
    /// The guest's processes are frozen.
    Frozen,
    /// The guest is being stopped.
    Stopping,
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
        }
    }
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
    /// How long to wait for the agent to connect after the guest starts.
    pub connect_timeout: Duration,
    /// How long the launcher may take to end every guest process.
    pub stop_timeout: Duration,
}

impl Default for SessionConfig {
    fn default() -> Self {
        SessionConfig {
            connect_timeout: Duration::from_secs(120),
            stop_timeout: Duration::from_secs(30),
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
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionError::NotActive(state) => write!(f, "the session is {state}, not Active"),
            SessionError::Transport(error) => write!(f, "control channel: {error}"),
            SessionError::Gone => write!(f, "the session has shut down"),
        }
    }
}

impl std::error::Error for SessionError {}

enum Command {
    Start,
    Stop,
    Send(Vec<u8>, Sender<Result<(), SessionError>>),
    Shutdown,
}

struct Shared {
    status: Mutex<(SessionState, Option<StopReason>)>,
    vm_id: Mutex<Option<String>>,
    changed: Condvar,
    subscribers: Mutex<Vec<Sender<SessionEvent>>>,
    tasks: Mutex<TaskHub>,
}

/// The guest's tasks and who is watching them, under one lock, so a new watcher's starting
/// point and the changes it receives afterwards never overlap or leave a gap.
#[derive(Default)]
struct TaskHub {
    table: TaskTable,
    watchers: Vec<Sender<TaskEvent>>,
}

/// The guest's tasks as they are now, and every change to them from then on.
pub struct TaskWatch {
    /// The events that bring an empty list of tasks to the current one.
    pub current: Vec<TaskEvent>,
    /// The changes after `current`, in order.
    pub events: Receiver<TaskEvent>,
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

    /// Records a task message and hands it to the watchers, unless it changes nothing.
    fn publish_task(&self, event: TaskEvent) {
        let mut hub = self.tasks.lock().unwrap_or_else(|e| e.into_inner());
        if !hub.table.apply(&event) {
            return;
        }
        hub.watchers
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
        });
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
        }
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

    /// Sends a frame to the agent. Needs an active session.
    pub fn send(&self, frame: Vec<u8>) -> Result<(), SessionError> {
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

    /// Asks the guest to close a task or launch an app. Needs an active session.
    pub fn request(&self, request: TaskRequest) -> Result<(), SessionError> {
        let body = match request {
            TaskRequest::Close { task_id } => Body::CloseTask(control::CloseTask { task_id }),
            TaskRequest::Launch { package_name } => {
                Body::LaunchApp(control::LaunchApp { package_name })
            }
        };
        self.send(dsa_protocol::encode(&control::Envelope {
            body: Some(body),
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
                        Ok(Command::Send(_, reply)) => {
                            let _ = reply.send(Err(SessionError::NotActive(SessionState::Stopped)));
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
                        Ok(Command::Send(_, reply)) => {
                            let _ =
                                reply.send(Err(SessionError::NotActive(SessionState::Stopping)));
                        }
                        Ok(Command::Shutdown) | Err(_) => return,
                    }
                }
                SessionState::Starting => unreachable!("start() leaves Starting before returning"),
                SessionState::Active | SessionState::Dozing | SessionState::Frozen => self.serve(),
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
                let state = self.state();
                let result = match (state, self.connection.as_mut()) {
                    (SessionState::Active, Some(connection)) => {
                        connection.send(&frame).map_err(SessionError::Transport)
                    }
                    _ => Err(SessionError::NotActive(state)),
                };
                let _ = reply.send(result);
                true
            }
        }
    }

    fn start(&mut self) {
        self.shared.set(SessionState::Starting, None);
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
                            self.connection = Some(connection);
                            self.shared.set_vm_id(self.launcher.vm_id());
                            self.shared.set(SessionState::Active, None);
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
            Command::Send(_, reply) => {
                let _ = reply.send(Err(SessionError::NotActive(SessionState::Starting)));
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
                    self.stop(StopReason::ChannelLost(error.to_string()));
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
            Some(Body::CloseTask(_)) | Some(Body::LaunchApp(_)) => {
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
        self.shared.set_vm_id(None);
        self.shared.set(SessionState::Stopping, None);
        if let Some(mut connection) = self.connection.take() {
            connection.close();
        }
        // The guest's tasks end with it, so their windows close even when it died.
        self.shared.clear_tasks();
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
