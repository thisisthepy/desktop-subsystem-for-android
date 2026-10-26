//! The local API: the service that answers the apps in users' sessions, and the client
//! those apps use.
//!
//! The service takes each caller's user from the transport (the operating system reports
//! it) and acts on that user's Android only. Nothing a client sends can name another user.
//! A connection that breaks the rules is closed; the service and its other connections go
//! on.
//!
//! A client that asks to watch the tasks of its Android ([`HostApiClient::watch_tasks`])
//! gets the tasks there are now, and then the connection carries every change to them, as
//! the compositor needs to keep one window per task.
//!
//! A client that streams input ([`HostApiClient::stream_input`]) sends pointer events on its
//! connection from then on, and the service passes each to the caller's Android as it
//! arrives, without answering, so a pointer that moves fast is never held back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_protocol::host_api::types::{
    self, envelope::Body, request, response, Envelope, ErrorCode, Request, Response, Role,
};
use dsa_protocol::host_api::{
    client_connect, decode, local_hello, send, service_accept, HandshakeError,
};
use dsa_transport::local::{LocalListener, Peer};
use dsa_transport::{Connection, Connector, TransportError};

use crate::ime::{ImeCursor, ImeEvent};
use crate::input::{InputEvent, PointerEvent};
use crate::keys::KeyEvent;
use crate::resize_trace::{self, detail_requested, ResizeTrace, DETAIL_ENV};
use crate::session::{Session, SessionError, SessionState};
use crate::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest, TaskRequestError, TaskWatch};

/// One user's Android, as the service drives it.
pub trait UserAndroid: Send + Sync {
    fn state(&self) -> SessionState;
    /// Why it last stopped, in words a person can act on.
    fn last_reason(&self) -> Option<String>;
    fn start(&self);
    fn stop(&self);
    /// News about the Android image for the user, if there is any.
    fn update_notice(&self) -> Option<String> {
        None
    }
    /// The runtime ID of the VM (such as a GUID on Windows), if running.
    fn vm_id(&self) -> Option<String> {
        None
    }
    /// The app tasks of this Android now, and every change to them from now on. `None` when
    /// this Android does not report its tasks.
    fn watch_tasks(&self) -> Option<TaskWatch> {
        None
    }
    /// Asks this Android to close a task or to launch an app.
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        let _ = request;
        Err(TaskRequestError::Failed(
            "this Android does not take requests about its tasks".into(),
        ))
    }
    /// Passes pointer input in a task's window to this Android.
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError> {
        let _ = event;
        Err(TaskRequestError::Failed(
            "this Android does not take pointer input".into(),
        ))
    }
    /// Passes a key pressed or released in a task's window to this Android.
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError> {
        let _ = event;
        Err(TaskRequestError::Failed(
            "this Android does not take key input".into(),
        ))
    }
    /// Passes text composed or committed in a task's window to this Android.
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError> {
        let _ = event;
        Err(TaskRequestError::Failed(
            "this Android does not take text input".into(),
        ))
    }
}

impl UserAndroid for Session {
    fn state(&self) -> SessionState {
        Session::state(self)
    }
    fn last_reason(&self) -> Option<String> {
        Session::last_reason(self).map(|reason| reason.to_string())
    }
    /// On a stopped session, returns once it has moved on, so that what the caller reads
    /// next is the start's progress or its failure, never the state from before it.
    fn start(&self) {
        if Session::state(self) != SessionState::Stopped {
            return Session::start(self);
        }
        let events = self.events();
        Session::start(self);
        let _ = events.recv_timeout(Duration::from_secs(5));
    }
    fn stop(&self) {
        Session::stop(self)
    }
    fn vm_id(&self) -> Option<String> {
        Session::vm_id(self)
    }
    fn watch_tasks(&self) -> Option<TaskWatch> {
        Some(Session::watch_tasks(self))
    }
    fn request(&self, request: TaskRequest) -> Result<(), TaskRequestError> {
        Session::request(self, request).map_err(request_error)
    }
    fn pointer(&self, event: &PointerEvent) -> Result<(), TaskRequestError> {
        Session::pointer(self, event).map_err(request_error)
    }
    fn key(&self, event: &KeyEvent) -> Result<(), TaskRequestError> {
        Session::key(self, event).map_err(request_error)
    }
    fn ime(&self, event: &ImeEvent) -> Result<(), TaskRequestError> {
        Session::ime(self, event).map_err(request_error)
    }
}

fn request_error(error: SessionError) -> TaskRequestError {
    match error {
        SessionError::NotActive(state) => TaskRequestError::NotRunning(format!(
            "Android is {state}, not running; start it first (dsa start)"
        )),
        other => TaskRequestError::Failed(other.to_string()),
    }
}

/// Finds or creates the Android of a user.
pub trait UserAndroids: Send + Sync {
    /// The Android of `user` (an identity as the transport reports it), created on first
    /// use. An error says why this user cannot have one.
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String>;
    /// The users whose Android is not stopped, with its state.
    fn running(&self) -> Vec<(String, SessionState)>;
    /// Stops every user's Android and waits at most `timeout` for all of them. Returns the
    /// users whose Android had not stopped by then, with its state.
    fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)>;
}

/// The answer to a request to update DSA itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramUpdate {
    /// The version setup is installing; `None` when there is nothing newer.
    pub installing: Option<String>,
    /// What happens next, in words for the user.
    pub detail: String,
}

/// Updates DSA itself when a user asks (`dsa update`).
pub trait UpdateProgram: Send + Sync {
    /// Installs the newest version now. An error says why it cannot.
    fn update_now(&self) -> Result<ProgramUpdate, String>;
}

/// One Android per user, each created by `create` the first time its user asks.
pub struct PerUser<F> {
    create: F,
    androids: Mutex<HashMap<String, Arc<dyn UserAndroid>>>,
}

impl<F> PerUser<F>
where
    F: Fn(&str) -> Result<Arc<dyn UserAndroid>, String> + Send + Sync,
{
    pub fn new(create: F) -> PerUser<F> {
        PerUser {
            create,
            androids: Mutex::new(HashMap::new()),
        }
    }
}

/// The users of `androids` whose Android is not stopped, with its state.
pub fn running_of<'a>(
    androids: impl IntoIterator<Item = (&'a String, &'a Arc<dyn UserAndroid>)>,
) -> Vec<(String, SessionState)> {
    androids
        .into_iter()
        .map(|(user, android)| (user.clone(), android.state()))
        .filter(|(_, state)| *state != SessionState::Stopped)
        .collect()
}

/// Stops every Android of `androids` and waits at most `timeout` for all of them. Returns
/// the users whose Android had not stopped by then, with its state.
pub fn stop_every(
    androids: Vec<(String, Arc<dyn UserAndroid>)>,
    timeout: Duration,
) -> Vec<(String, SessionState)> {
    for (_, android) in &androids {
        android.stop();
    }
    let deadline = Instant::now() + timeout;
    loop {
        let running = running_of(androids.iter().map(|(user, android)| (user, android)));
        if running.is_empty() || Instant::now() >= deadline {
            return running;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

impl<F> PerUser<F> {
    fn snapshot(&self) -> Vec<(String, Arc<dyn UserAndroid>)> {
        self.androids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(user, android)| (user.clone(), Arc::clone(android)))
            .collect()
    }
}

impl<F> UserAndroids for PerUser<F>
where
    F: Fn(&str) -> Result<Arc<dyn UserAndroid>, String> + Send + Sync,
{
    fn android(&self, user: &str) -> Result<Arc<dyn UserAndroid>, String> {
        let mut androids = self.androids.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(android) = androids.get(user) {
            return Ok(Arc::clone(android));
        }
        let android = (self.create)(user)?;
        androids.insert(user.to_string(), Arc::clone(&android));
        Ok(android)
    }
    fn running(&self) -> Vec<(String, SessionState)> {
        let androids = self.snapshot();
        running_of(androids.iter().map(|(user, android)| (user, android)))
    }
    fn stop_all(&self, timeout: Duration) -> Vec<(String, SessionState)> {
        stop_every(self.snapshot(), timeout)
    }
}

fn to_wire(state: SessionState) -> types::SessionState {
    match state {
        SessionState::Stopped => types::SessionState::Stopped,
        SessionState::Starting => types::SessionState::Starting,
        SessionState::Active => types::SessionState::Active,
        SessionState::Dozing => types::SessionState::Dozing,
        SessionState::Frozen => types::SessionState::Frozen,
        SessionState::Stopping => types::SessionState::Stopping,
    }
}

fn task_to_wire(task: Task) -> types::Task {
    types::Task {
        task_id: task.task_id,
        bounds: Some(types::Rect {
            left: task.bounds.left,
            top: task.bounds.top,
            right: task.bounds.right,
            bottom: task.bounds.bottom,
        }),
        package_name: task.package_name,
        label: task.label,
        focused: task.focused,
        density_dpi: task.density_dpi,
        max_width: task.max_width,
        max_height: task.max_height,
    }
}

fn task_from_wire(task: types::Task) -> Task {
    let bounds = task.bounds.unwrap_or_default();
    Task {
        task_id: task.task_id,
        bounds: Bounds {
            left: bounds.left,
            top: bounds.top,
            right: bounds.right,
            bottom: bounds.bottom,
        },
        package_name: task.package_name,
        label: task.label,
        focused: task.focused,
        density_dpi: task.density_dpi,
        max_width: task.max_width,
        max_height: task.max_height,
    }
}

fn app_to_wire(app: AppInfo) -> types::AppInfo {
    types::AppInfo {
        package_name: app.package_name,
        label: app.label,
        icon_png: app.icon_png,
    }
}

fn app_from_wire(app: types::AppInfo) -> AppInfo {
    AppInfo {
        package_name: app.package_name,
        label: app.label,
        icon_png: app.icon_png,
    }
}

fn event_to_wire(event: TaskEvent) -> types::TaskEvent {
    use types::task_event::Body as Event;
    let body = match event {
        TaskEvent::Updated(task) => Event::Updated(task_to_wire(task)),
        TaskEvent::Removed { task_id } => Event::Removed(types::TaskRemoved { task_id }),
        TaskEvent::Frames {
            task_id,
            display_id,
        } => Event::Frames(types::TaskFrames {
            task_id,
            display_id,
        }),
        TaskEvent::App(app) => Event::App(app_to_wire(app)),
        TaskEvent::ImeCursor { task_id, cursor } => Event::ImeCursor(types::ImeCursor {
            task_id,
            cursor: Some(types::Rect {
                left: cursor.left,
                top: cursor.top,
                right: cursor.right,
                bottom: cursor.bottom,
            }),
        }),
    };
    types::TaskEvent { body: Some(body) }
}

fn event_from_wire(event: types::TaskEvent) -> Option<TaskEvent> {
    use types::task_event::Body as Event;
    Some(match event.body? {
        Event::Updated(task) => TaskEvent::Updated(task_from_wire(task)),
        Event::Removed(removed) => TaskEvent::Removed {
            task_id: removed.task_id,
        },
        Event::Frames(frames) => TaskEvent::Frames {
            task_id: frames.task_id,
            display_id: frames.display_id,
        },
        Event::App(app) => TaskEvent::App(app_from_wire(app)),
        Event::ImeCursor(cursor) => {
            let rect = cursor.cursor.unwrap_or_default();
            TaskEvent::ImeCursor {
                task_id: cursor.task_id,
                cursor: ImeCursor {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                },
            }
        }
    })
}

/// The tasks of `events`, as a list of tasks, a list of their displays and a list of their
/// apps.
fn tasks_to_wire(events: Vec<TaskEvent>) -> types::Tasks {
    let mut tasks = types::Tasks::default();
    for event in events {
        match event {
            TaskEvent::Updated(task) => tasks.tasks.push(task_to_wire(task)),
            TaskEvent::Frames {
                task_id,
                display_id,
            } => tasks.frames.push(types::TaskFrames {
                task_id,
                display_id,
            }),
            TaskEvent::App(app) => tasks.apps.push(app_to_wire(app)),
            TaskEvent::Removed { .. } | TaskEvent::ImeCursor { .. } => {}
        }
    }
    tasks
}

fn tasks_from_wire(tasks: types::Tasks) -> TaskList {
    TaskList {
        tasks: tasks.tasks.into_iter().map(task_from_wire).collect(),
        frames: tasks
            .frames
            .into_iter()
            .map(|frames| (frames.task_id, frames.display_id))
            .collect(),
        apps: tasks.apps.into_iter().map(app_from_wire).collect(),
    }
}

/// How often a watching connection looks for its client having gone away while no task
/// changes.
const WATCH_POLL: Duration = Duration::from_millis(100);

fn from_wire(state: i32) -> Option<SessionState> {
    match types::SessionState::try_from(state).ok()? {
        types::SessionState::Unspecified => None,
        types::SessionState::Stopped => Some(SessionState::Stopped),
        types::SessionState::Starting => Some(SessionState::Starting),
        types::SessionState::Active => Some(SessionState::Active),
        types::SessionState::Dozing => Some(SessionState::Dozing),
        types::SessionState::Frozen => Some(SessionState::Frozen),
        types::SessionState::Stopping => Some(SessionState::Stopping),
    }
}

/// The service end of the local API.
pub struct HostApiService {
    androids: Arc<dyn UserAndroids>,
    program: Option<Arc<dyn UpdateProgram>>,
    hello: types::Hello,
    handshake_timeout: Duration,
    /// Logs when each resize was handed to Android, and how long that took.
    resize_trace: Mutex<ResizeTrace>,
}

impl HostApiService {
    pub fn new(androids: Arc<dyn UserAndroids>, build: &str) -> HostApiService {
        HostApiService {
            androids,
            program: None,
            hello: local_hello(Role::Service, build),
            handshake_timeout: Duration::from_secs(5),
            resize_trace: Mutex::new(ResizeTrace::new(
                detail_requested(std::env::var(DETAIL_ENV).ok().as_deref(), false),
                Instant::now(),
            )),
        }
    }

    /// Turns on the per-stage resize trace lines, as when the file that asks for them is
    /// present.
    pub fn with_resize_trace_detail(self, detailed: bool) -> HostApiService {
        if detailed {
            *self.resize_trace.lock().unwrap_or_else(|e| e.into_inner()) =
                ResizeTrace::new(true, Instant::now());
        }
        self
    }

    /// The service that answers `dsa update` with `program`.
    pub fn with_program_updates(mut self, program: Arc<dyn UpdateProgram>) -> HostApiService {
        self.program = Some(program);
        self
    }

    /// Accepts connections until `stop` is set, serving each on its own thread. A failed
    /// accept is logged and the loop goes on; only `stop` ends it.
    pub fn serve(self: Arc<Self>, mut listener: Box<dyn LocalListener>, stop: Arc<AtomicBool>) {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept(Duration::from_millis(200)) {
                Ok((connection, peer)) => {
                    let service = Arc::clone(&self);
                    let spawned =
                        thread::Builder::new()
                            .name("dsa-host-api".into())
                            .spawn(move || {
                                let _ =
                                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                        service.serve_connection(connection, peer);
                                    }));
                            });
                    if let Err(error) = spawned {
                        eprintln!("local API: could not start a connection thread: {error}");
                    }
                }
                Err(TransportError::Timeout) => {}
                Err(error) => {
                    eprintln!("local API: accepting a connection failed: {error}");
                    thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// Serves one client until it disconnects or breaks the protocol.
    pub fn serve_connection(&self, mut connection: Box<dyn Connection>, peer: Peer) {
        if let Err(error) = service_accept(&mut *connection, &self.hello, self.handshake_timeout) {
            eprintln!("local API: refused a client of {}: {error}", peer.user);
            return;
        }
        loop {
            let frame = match connection.recv(None) {
                Ok(frame) => frame,
                Err(_) => return,
            };
            let request = match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::Request(request)),
                }) => request,
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than a request; \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame ({error}); \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            };
            if let Some(request::Body::WatchTasks(_)) = request.body {
                return self.watch_tasks(connection, &peer, request.id);
            }
            if let Some(request::Body::StreamInput(_)) = request.body {
                return self.stream_input(connection, &peer, request.id);
            }
            let response = self.answer(&peer, request);
            if send(&mut *connection, Body::Response(response)).is_err() {
                return;
            }
        }
    }

    /// Answers a WatchTasks request with the tasks there are now, then sends every change
    /// until the client goes away.
    fn watch_tasks(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let watch = match self.androids.android(&peer.user) {
            Err(why) => Err(error(ErrorCode::Unavailable, why)),
            Ok(android) => android.watch_tasks().ok_or_else(|| {
                error(
                    ErrorCode::Unavailable,
                    "this Android does not report its tasks".into(),
                )
            }),
        };
        let watch = match watch {
            Ok(watch) => watch,
            Err(body) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(body),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Tasks(tasks_to_wire(watch.current))),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        loop {
            match watch.events.recv_timeout(WATCH_POLL) {
                Ok(event) => {
                    let pending = std::iter::once(event).chain(watch.events.try_iter());
                    for event in pending {
                        let frame = Body::TaskEvent(event_to_wire(event));
                        if send(&mut *connection, frame).is_err() {
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    connection.close();
                    return;
                }
            }
            match connection.recv(Some(Duration::from_millis(1))) {
                Err(TransportError::Timeout) => {}
                Err(_) => return,
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent a frame on a connection that watches \
                         tasks; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            }
        }
    }

    /// Answers a StreamInput request, then passes every pointer, key and text event the client sends to
    /// its Android until the client goes away. Input to an Android that is not running is
    /// dropped: the window that sends it can do nothing better with it.
    fn stream_input(&self, mut connection: Box<dyn Connection>, peer: &Peer, id: u64) {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => {
                let _ = send(
                    &mut *connection,
                    Body::Response(Response {
                        id,
                        body: Some(error(ErrorCode::Unavailable, why)),
                    }),
                );
                return;
            }
        };
        let answer = Response {
            id,
            body: Some(response::Body::Done(types::Done {})),
        };
        if send(&mut *connection, Body::Response(answer)).is_err() {
            return;
        }
        // Said once per run of failures, not once per event.
        let mut failing = false;
        loop {
            let frame = match connection.recv(None) {
                Ok(frame) => frame,
                Err(_) => return,
            };
            let event = match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::PointerEvent(event)),
                }) => PointerEvent::from_wire(&event)
                    .map(InputEvent::Pointer)
                    .map_err(|why| format!("dropped a pointer event: {why}")),
                Ok(Envelope {
                    body: Some(Body::KeyEvent(event)),
                }) => KeyEvent::from_wire(&event)
                    .map(InputEvent::Key)
                    .map_err(|why| format!("dropped a key event: {why}")),
                Ok(Envelope {
                    body: Some(Body::ImeComposition(composition)),
                }) => ImeEvent::from_wire_composition(&composition)
                    .map(InputEvent::Ime)
                    .map_err(|why| format!("dropped a composition: {why}")),
                Ok(Envelope {
                    body: Some(Body::ImeCommit(commit)),
                }) => ImeEvent::from_wire_commit(&commit)
                    .map(InputEvent::Ime)
                    .map_err(|why| format!("dropped committed text: {why}")),
                Ok(_) => {
                    eprintln!(
                        "local API: a client of {} sent something other than input on a \
                         connection that streams input; closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
                Err(error) => {
                    eprintln!(
                        "local API: a client of {} sent an unreadable frame ({error}); \
                         closing its connection",
                        peer.user
                    );
                    connection.close();
                    return;
                }
            };
            let outcome = event.and_then(|event| match event {
                InputEvent::Pointer(event) => android
                    .pointer(&event)
                    .map_err(|why| format!("pointer input did not reach Android: {why}")),
                InputEvent::Key(event) => android
                    .key(&event)
                    .map_err(|why| format!("key input did not reach Android: {why}")),
                InputEvent::Ime(event) => android
                    .ime(&event)
                    .map_err(|why| format!("text input did not reach Android: {why}")),
            });
            match outcome {
                Ok(()) => failing = false,
                Err(why) => {
                    if !failing {
                        eprintln!("local API: {}: {why}", peer.user);
                    }
                    failing = true;
                }
            }
        }
    }

    fn answer(&self, peer: &Peer, request: Request) -> Response {
        let body = match request.body {
            None => error(
                ErrorCode::UnknownRequest,
                "the request is empty or of a kind this service does not know; \
                 update the service"
                    .into(),
            ),
            Some(request::Body::UpdateProgram(_)) => self.update_program(peer),
            Some(request::Body::GetVmId(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => response::Body::VmId(types::VmId {
                    vm_id: android.vm_id().unwrap_or_default(),
                }),
            },
            Some(request::Body::ListTasks(_)) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => response::Body::Tasks(tasks_to_wire(
                    android
                        .watch_tasks()
                        .map(|watch| watch.current)
                        .unwrap_or_default(),
                )),
            },
            Some(request::Body::CloseTask(close)) => self.request(
                peer,
                TaskRequest::Close {
                    task_id: close.task_id,
                },
            ),
            Some(request::Body::LaunchApp(launch)) => self.request(
                peer,
                TaskRequest::Launch {
                    package_name: launch.package_name,
                },
            ),
            Some(request::Body::ResizeTask(resize)) => {
                let started = Instant::now();
                let answer = self.request(
                    peer,
                    TaskRequest::Resize {
                        task_id: resize.task_id,
                        width: resize.width,
                        height: resize.height,
                        live: resize.live,
                    },
                );
                self.trace_resize(&resize, started);
                answer
            }
            Some(request::Body::SetTaskDensity(density)) => self.request(
                peer,
                TaskRequest::SetDensity {
                    task_id: density.task_id,
                    density_dpi: density.density_dpi,
                },
            ),
            Some(body) => match self.androids.android(&peer.user) {
                Err(why) => error(ErrorCode::Unavailable, why),
                Ok(android) => {
                    match body {
                        request::Body::GetStatus(_) => {}
                        request::Body::StartSession(_) => android.start(),
                        request::Body::StopSession(_) => android.stop(),
                        request::Body::UpdateProgram(_)
                        | request::Body::GetVmId(_)
                        | request::Body::ListTasks(_)
                        | request::Body::WatchTasks(_)
                        | request::Body::CloseTask(_)
                        | request::Body::LaunchApp(_)
                        | request::Body::ResizeTask(_)
                        | request::Body::SetTaskDensity(_)
                        | request::Body::StreamInput(_) => unreachable!("answered above"),
                    }
                    response::Body::Status(types::Status {
                        state: to_wire(android.state()) as i32,
                        last_reason: android.last_reason().unwrap_or_default(),
                        update_notice: android.update_notice().unwrap_or_default(),
                    })
                }
            },
        };
        Response {
            id: request.id,
            body: Some(body),
        }
    }
}

impl HostApiService {
    /// Logs that the resize went to Android: each size when detail is on, and otherwise the
    /// end of a drag, at most a few times a second.
    fn trace_resize(&self, resize: &types::ResizeTask, started: Instant) {
        let now = Instant::now();
        let size = (resize.width, resize.height);
        let detail = format!(
            "live={} took={}",
            resize.live,
            resize_trace::millis(now.saturating_duration_since(started))
        );
        let wall = resize_trace::wall_now();
        let mut trace = self.resize_trace.lock().unwrap_or_else(|e| e.into_inner());
        let line = if trace.detailed() {
            trace.stage(
                resize.task_id,
                "service.forwarded",
                size,
                &detail,
                now,
                wall,
            )
        } else if !resize.live {
            trace.summary(
                resize.task_id,
                "service.forwarded",
                size,
                &detail,
                now,
                wall,
            )
        } else {
            None
        };
        if let Some(line) = line {
            eprintln!("local API: {line}");
        }
    }

    fn request(&self, peer: &Peer, request: TaskRequest) -> response::Body {
        let android = match self.androids.android(&peer.user) {
            Ok(android) => android,
            Err(why) => return error(ErrorCode::Unavailable, why),
        };
        match android.request(request) {
            Ok(()) => response::Body::Done(types::Done {}),
            Err(TaskRequestError::NotRunning(why)) => error(ErrorCode::NotRunning, why),
            Err(TaskRequestError::Failed(why)) => error(ErrorCode::Unavailable, why),
        }
    }

    fn update_program(&self, peer: &Peer) -> response::Body {
        let Some(program) = &self.program else {
            return error(
                ErrorCode::UpdateFailed,
                "this service does not update DSA; install the new version by hand".into(),
            );
        };
        eprintln!("local API: {} asked to update DSA now", peer.user);
        match program.update_now() {
            Ok(update) => response::Body::ProgramUpdate(types::ProgramUpdate {
                installing: update.installing.unwrap_or_default(),
                detail: update.detail,
            }),
            Err(why) => error(ErrorCode::UpdateFailed, why),
        }
    }
}

fn error(code: ErrorCode, detail: String) -> response::Body {
    response::Body::Error(types::Error {
        code: code as i32,
        detail,
    })
}

/// The caller's Android as the service reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub state: SessionState,
    pub last_reason: Option<String>,
    /// News about the Android image, such as an update waiting for the next start.
    pub update_notice: Option<String>,
}

/// The tasks of the caller's Android as the service reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskList {
    /// In task ID order.
    pub tasks: Vec<Task>,
    /// The display of each task whose frames have been placed, as `(task_id, display_id)`,
    /// in task ID order.
    pub frames: Vec<(i32, u32)>,
    /// The name and icon of each app that has a task, in package order.
    pub apps: Vec<AppInfo>,
}

impl TaskList {
    /// The events that bring an empty list of tasks to this one: every app, then every
    /// task, then every task's display.
    pub fn events(&self) -> Vec<TaskEvent> {
        let apps = self.apps.iter().cloned().map(TaskEvent::App);
        let tasks = self.tasks.iter().cloned().map(TaskEvent::Updated);
        let frames = self
            .frames
            .iter()
            .map(|(task_id, display_id)| TaskEvent::Frames {
                task_id: *task_id,
                display_id: *display_id,
            });
        apps.chain(tasks).chain(frames).collect()
    }
}

/// Why a local API call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientError {
    /// Connecting or the handshake failed; the detail says why.
    Connect(HandshakeError),
    /// The connection broke or timed out.
    Transport(TransportError),
    /// The service answered with an error.
    Service { code: ErrorCode, detail: String },
    /// The service's answer did not make sense.
    Malformed(String),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::Connect(error) => write!(f, "{error}"),
            ClientError::Transport(error) => write!(f, "the DSA service: {error}"),
            ClientError::Service { detail, .. } => write!(f, "the DSA service: {detail}"),
            ClientError::Malformed(why) => write!(f, "the DSA service answered oddly: {why}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// The client end of the local API.
pub struct HostApiClient {
    connection: Box<dyn Connection>,
    next_id: u64,
    timeout: Duration,
}

impl HostApiClient {
    /// Connects through `connector` and completes the handshake, waiting at most `timeout`
    /// for each step.
    pub fn connect(
        connector: &mut dyn Connector,
        build: &str,
        timeout: Duration,
    ) -> Result<HostApiClient, ClientError> {
        let mut connection = connector
            .connect(timeout)
            .map_err(|error| ClientError::Connect(HandshakeError::Transport(error)))?;
        client_connect(&mut *connection, &local_hello(Role::Client, build), timeout)
            .map_err(ClientError::Connect)?;
        Ok(HostApiClient {
            connection,
            next_id: 1,
            timeout,
        })
    }

    pub fn status(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::GetStatus(types::GetStatus {}))
    }

    /// Asks for the caller's Android to start. Returns the state right after the request.
    pub fn start(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::StartSession(types::StartSession {}))
    }

    /// Asks for the caller's Android to stop. Returns the state right after the request.
    pub fn stop(&mut self) -> Result<Status, ClientError> {
        self.call(request::Body::StopSession(types::StopSession {}))
    }

    /// Asks the service to install the newest version of DSA now. Downloading it can take
    /// a while, so the answer is awaited for at most `timeout`.
    pub fn update_program(&mut self, timeout: Duration) -> Result<ProgramUpdate, ClientError> {
        match self.exchange(
            request::Body::UpdateProgram(types::UpdateProgram {}),
            timeout,
        )? {
            response::Body::ProgramUpdate(update) => Ok(ProgramUpdate {
                installing: Some(update.installing).filter(|version| !version.is_empty()),
                detail: update.detail,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer to an update request".into(),
            )),
        }
    }

    /// Asks for the runtime ID of the caller's Android VM, if running.
    pub fn vm_id(&mut self) -> Result<Option<String>, ClientError> {
        match self.exchange(request::Body::GetVmId(types::GetVmId {}), self.timeout)? {
            response::Body::VmId(vm_id) => Ok(Some(vm_id.vm_id).filter(|id| !id.is_empty())),
            _ => Err(ClientError::Malformed("expected a vm_id response".into())),
        }
    }

    /// The app tasks of the caller's Android.
    pub fn tasks(&mut self) -> Result<TaskList, ClientError> {
        match self.exchange(request::Body::ListTasks(types::ListTasks {}), self.timeout)? {
            response::Body::Tasks(tasks) => Ok(tasks_from_wire(tasks)),
            _ => Err(ClientError::Malformed("expected the list of tasks".into())),
        }
    }

    /// Asks the caller's Android to end a task, as when the user closes its window.
    pub fn close_task(&mut self, task_id: i32) -> Result<(), ClientError> {
        self.done(request::Body::CloseTask(types::CloseTask { task_id }))
    }

    /// Asks the caller's Android to start an app in a new task of its own.
    pub fn launch_app(&mut self, package_name: &str) -> Result<(), ClientError> {
        self.done(request::Body::LaunchApp(types::LaunchApp {
            package_name: package_name.into(),
        }))
    }

    /// Asks the caller's Android to give a task the client size of its window, in Android
    /// display pixels, as when the user resizes the window. `live` is true while the user is
    /// still dragging the window's edge: the task takes the size without being reconfigured,
    /// and the resize with `live` false at the end of the drag reconfigures it once.
    pub fn resize_task(
        &mut self,
        task_id: i32,
        width: u32,
        height: u32,
        live: bool,
    ) -> Result<(), ClientError> {
        self.done(request::Body::ResizeTask(types::ResizeTask {
            task_id,
            width,
            height,
            live,
        }))
    }

    /// Asks the caller's Android to draw a task at `density_dpi`, as when its window moves
    /// to a monitor of another scale.
    pub fn set_task_density(&mut self, task_id: i32, density_dpi: u32) -> Result<(), ClientError> {
        self.done(request::Body::SetTaskDensity(types::SetTaskDensity {
            task_id,
            density_dpi,
        }))
    }

    /// Carries out `request` against the caller's Android.
    pub fn request(&mut self, request: &TaskRequest) -> Result<(), ClientError> {
        match request {
            TaskRequest::Close { task_id } => self.close_task(*task_id),
            TaskRequest::Launch { package_name } => self.launch_app(package_name),
            TaskRequest::Resize {
                task_id,
                width,
                height,
                live,
            } => self.resize_task(*task_id, *width, *height, *live),
            TaskRequest::SetDensity {
                task_id,
                density_dpi,
            } => self.set_task_density(*task_id, *density_dpi),
        }
    }

    /// Watches the app tasks of the caller's Android. The connection carries nothing else
    /// from here on, so the client is used up.
    pub fn watch_tasks(mut self) -> Result<TaskStream, ClientError> {
        match self.exchange(
            request::Body::WatchTasks(types::WatchTasks {}),
            self.timeout,
        )? {
            response::Body::Tasks(tasks) => Ok(TaskStream {
                connection: self.connection,
                current: tasks_from_wire(tasks),
            }),
            _ => Err(ClientError::Malformed("expected the list of tasks".into())),
        }
    }

    /// Streams input to the caller's Android. The connection carries nothing else from here
    /// on, so the client is used up.
    pub fn stream_input(mut self) -> Result<InputStream, ClientError> {
        match self.exchange(
            request::Body::StreamInput(types::StreamInput {}),
            self.timeout,
        )? {
            response::Body::Done(_) => Ok(InputStream {
                connection: self.connection,
            }),
            _ => Err(ClientError::Malformed(
                "expected the answer that input may be sent".into(),
            )),
        }
    }

    fn done(&mut self, body: request::Body) -> Result<(), ClientError> {
        match self.exchange(body, self.timeout)? {
            response::Body::Done(_) => Ok(()),
            _ => Err(ClientError::Malformed(
                "expected the answer that the request was carried out".into(),
            )),
        }
    }

    fn call(&mut self, body: request::Body) -> Result<Status, ClientError> {
        match self.exchange(body, self.timeout)? {
            response::Body::Status(status) => Ok(Status {
                state: from_wire(status.state).ok_or_else(|| {
                    ClientError::Malformed(format!("unknown session state {}", status.state))
                })?,
                last_reason: Some(status.last_reason).filter(|reason| !reason.is_empty()),
                update_notice: Some(status.update_notice).filter(|notice| !notice.is_empty()),
            }),
            _ => Err(ClientError::Malformed("expected a status".into())),
        }
    }

    /// Sends `body` and returns the answer's body; an error answer is an error.
    fn exchange(
        &mut self,
        body: request::Body,
        timeout: Duration,
    ) -> Result<response::Body, ClientError> {
        let id = self.next_id;
        self.next_id += 1;
        send(
            &mut *self.connection,
            Body::Request(Request {
                id,
                body: Some(body),
            }),
        )
        .map_err(ClientError::Transport)?;
        let frame = self
            .connection
            .recv(Some(timeout))
            .map_err(ClientError::Transport)?;
        let response = match decode(&frame) {
            Ok(Envelope {
                body: Some(Body::Response(response)),
            }) => response,
            Ok(_) => return Err(ClientError::Malformed("expected a response".into())),
            Err(error) => return Err(ClientError::Malformed(error.to_string())),
        };
        if response.id != id {
            return Err(ClientError::Malformed(format!(
                "the answer is for request {}, expected {id}",
                response.id
            )));
        }
        match response.body {
            Some(response::Body::Error(error)) => Err(ClientError::Service {
                code: ErrorCode::try_from(error.code).unwrap_or(ErrorCode::Unspecified),
                detail: error.detail,
            }),
            Some(body) => Ok(body),
            None => Err(ClientError::Malformed("an empty response".into())),
        }
    }
}

/// The tasks of the caller's Android and every change to them, from
/// [`HostApiClient::watch_tasks`].
pub struct TaskStream {
    connection: Box<dyn Connection>,
    current: TaskList,
}

impl TaskStream {
    /// The tasks there were when the watch began.
    pub fn current(&self) -> &TaskList {
        &self.current
    }

    /// The next change, waiting at most `timeout`. `Ok(None)` means none came in time; an
    /// error means the watch has ended, and a new connection is needed for another.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<TaskEvent>, ClientError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let frame = match self.connection.recv(Some(remaining)) {
                Ok(frame) => frame,
                Err(TransportError::Timeout) => return Ok(None),
                Err(error) => return Err(ClientError::Transport(error)),
            };
            match decode(&frame) {
                Ok(Envelope {
                    body: Some(Body::TaskEvent(event)),
                }) => match event_from_wire(event) {
                    Some(event) => return Ok(Some(event)),
                    // A kind of change this client does not know: a newer service told it
                    // something it can do nothing with.
                    None => continue,
                },
                Ok(_) => return Err(ClientError::Malformed("expected a task event".into())),
                Err(error) => return Err(ClientError::Malformed(error.to_string())),
            }
        }
    }
}

/// A connection that carries input to the caller's Android, from
/// [`HostApiClient::stream_input`].
pub struct InputStream {
    connection: Box<dyn Connection>,
}

impl InputStream {
    /// Sends one pointer event. It returns once the event is on its way; nothing is
    /// awaited. An error means the connection has ended, and a new one is needed.
    pub fn send(&mut self, event: &PointerEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, Body::PointerEvent(event.to_wire()))
            .map_err(ClientError::Transport)
    }

    /// Sends one key event, the same way as [`InputStream::send`].
    pub fn send_key(&mut self, event: &KeyEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, Body::KeyEvent(event.to_wire())).map_err(ClientError::Transport)
    }

    /// Sends text composed or committed in a task's window, the same way as
    /// [`InputStream::send`].
    pub fn send_ime(&mut self, event: &ImeEvent) -> Result<(), ClientError> {
        send(&mut *self.connection, event.to_wire()).map_err(ClientError::Transport)
    }

    /// Sends one event of any kind.
    pub fn send_input(&mut self, event: &InputEvent) -> Result<(), ClientError> {
        match event {
            InputEvent::Pointer(event) => self.send(event),
            InputEvent::Key(event) => self.send_key(event),
            InputEvent::Ime(event) => self.send_ime(event),
        }
    }
}
