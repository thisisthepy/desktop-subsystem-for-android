//! What ties the windows to the DSA service: the user's tasks, their frames, the guest's
//! input, the clipboard, notifications, controllers, launcher entries and dropped files.
//!
//! The windows and the window rules (`TaskWindows`) live on the main thread with the Wayland
//! connection. Each connection to the service runs on a thread of its own and reconnects
//! when the service restarts; what a thread has for the windows goes through a channel, and
//! a pipe wakes the main thread. A broken stream, a dead guest or a missing renderer is
//! reported and waited out; nothing of it ends the compositor.

use std::collections::BTreeMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::shown_windows::ShownWindows;
use dsa_host_core::compositor::task_windows::{TaskWindows, WindowChange};
use dsa_host_core::compositor::window_hosts::FromHost;
use dsa_host_core::drag::{DragOffer, DragOut};
use dsa_host_core::files::{FileAction, FileTransfer};
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::input::InputEvent;
use dsa_host_core::native_abi::in_user_language;
use dsa_host_core::notifications::{NotificationAction, NotificationEvent};
use dsa_host_core::tasks::{AppInfo, TaskEvent, TaskRequest};
use dsa_host_unix::socket::UnixLocalConnector;
use dsa_transport::Connector;

use super::frames::{LatestFrames, Shown};
use super::gfxstream::{self, FrameSink, Listen, Renderer};
use super::wayland::{Desktop, Event, Events, Wake};
use crate::input::evdev::EvdevGamepads;
use crate::shell::dbus::{Notifier, Signal};
use crate::shell::desktop_entry::{self, Folders};
use crate::shell::notifications::Shown as ShownNotifications;

/// The build this program reports to the service.
pub const BUILD: &str = env!("CARGO_PKG_VERSION");

/// How long to wait for the service before trying again.
const RETRY: Duration = Duration::from_secs(2);

/// How long one call to the service may take.
const TIMEOUT: Duration = Duration::from_secs(5);

/// How often the main loop looks at the window rules' timers.
const TICK: Duration = Duration::from_millis(16);

/// The service's local socket: `DSA_SOCKET` when set, else the system service's.
pub fn socket_path() -> PathBuf {
    std::env::var_os("DSA_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/run/dsa/host.sock"))
}

/// Writes a line to the log (standard error, which the user's session journal keeps).
pub fn log(line: &str) {
    eprintln!("dsa-compositor: {line}");
}

/// Wakes the main loop through a pipe.
struct PipeWaker {
    write: OwnedFd,
}

impl Wake for PipeWaker {
    fn wake(&self) {
        let byte = 1u8;
        // SAFETY: writes one byte from a valid buffer; a full pipe already wakes the loop.
        unsafe {
            libc::write(self.write.as_raw_fd(), (&byte as *const u8).cast(), 1);
        }
    }
}

fn wake_pipe() -> Result<(OwnedFd, Arc<PipeWaker>), String> {
    let mut fds = [0; 2];
    // SAFETY: `fds` takes the two new descriptors, owned from here on.
    unsafe {
        if libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) != 0 {
            return Err("cannot make a pipe to wake the compositor".into());
        }
        Ok((
            OwnedFd::from_raw_fd(fds[0]),
            Arc::new(PipeWaker {
                write: OwnedFd::from_raw_fd(fds[1]),
            }),
        ))
    }
}

fn drain(read: &OwnedFd) {
    let mut buffer = [0u8; 64];
    // SAFETY: reads into a valid buffer from a non-blocking pipe.
    while unsafe { libc::read(read.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len()) } > 0 {}
}

/// Whether the session reads Korean: the language gettext would choose its messages in, from
/// `LC_ALL`, `LC_MESSAGES` or `LANG`, the first that is set.
fn reads_korean() -> bool {
    reads_korean_in(|name| std::env::var(name).ok())
}

fn reads_korean_in(var: impl Fn(&str) -> Option<String>) -> bool {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| var(name).filter(|value| !value.is_empty()))
        .is_some_and(|locale| locale.starts_with("ko"))
}

fn connector() -> UnixLocalConnector {
    UnixLocalConnector {
        path: socket_path(),
    }
}

fn connect() -> Result<HostApiClient, String> {
    HostApiClient::connect(&mut connector(), BUILD, TIMEOUT).map_err(|error| error.to_string())
}

/// What the service's threads have for the main thread.
enum Message {
    /// The tasks as a new watch starts: the windows sync to them.
    Tasks(Vec<TaskEvent>),
    Task(TaskEvent),
    /// The task watch ended: the windows stay until the next watch says otherwise.
    TasksLost,
    /// The guest copied text.
    GuestClipboard(String),
    /// An app drags `offer` out of task `task_id`'s window, from the press `serial`; `client`
    /// reads its files and ends it.
    DragOut {
        task_id: i32,
        offer: DragOffer,
        client: HostApiClient,
        serial: u32,
    },
}

/// What the main thread asks of the service.
enum Request {
    Task(TaskRequest),
    Notification(NotificationAction),
    OpenFile(FileTransfer),
}

/// Sends `message` and wakes the main loop.
#[derive(Clone)]
struct ToMain {
    sender: Sender<Message>,
    waker: Arc<PipeWaker>,
}

impl ToMain {
    fn send(&self, message: Message) -> bool {
        let sent = self.sender.send(message).is_ok();
        self.waker.wake();
        sent
    }
}

fn watch_tasks(to_main: ToMain) {
    std::thread::spawn(move || {
        let mut warned = false;
        loop {
            let watch = connect()
                .and_then(|client| client.watch_tasks().map_err(|error| error.to_string()));
            let mut stream = match watch {
                Ok(stream) => stream,
                Err(why) => {
                    if !warned {
                        log(&format!("waiting for the DSA service: {why}"));
                        warned = true;
                    }
                    std::thread::sleep(RETRY);
                    continue;
                }
            };
            warned = false;
            if !to_main.send(Message::Tasks(stream.current().events())) {
                return;
            }
            loop {
                match stream.next(Duration::from_secs(1)) {
                    Ok(Some(event)) => {
                        if !to_main.send(Message::Task(event)) {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(why) => {
                        log(&format!("the task watch ended: {why}"));
                        break;
                    }
                }
            }
            if !to_main.send(Message::TasksLost) {
                return;
            }
            std::thread::sleep(RETRY);
        }
    });
}

fn send_input(events: Receiver<InputEvent>) {
    std::thread::spawn(move || {
        let mut stream = None;
        for event in events {
            if stream.is_none() {
                stream = connect()
                    .and_then(|client| client.stream_input().map_err(|error| error.to_string()))
                    .map_err(|why| log(&format!("input cannot reach Android: {why}")))
                    .ok();
            }
            if let Some(open) = stream.as_mut() {
                if open.send_input(&event).is_err() {
                    stream = None;
                }
            }
        }
    });
}

fn serve_requests(requests: Receiver<Request>) {
    std::thread::spawn(move || {
        let mut client: Option<HostApiClient> = None;
        for request in requests {
            if let Request::OpenFile(file) = &request {
                let opened = connect()
                    .and_then(|client| client.open_file(file).map_err(|error| error.to_string()));
                if let Err(why) = opened {
                    log(&format!("{} did not open in Android: {why}", file.name));
                }
                continue;
            }
            if client.is_none() {
                client = connect()
                    .map_err(|why| log(&format!("cannot reach the DSA service: {why}")))
                    .ok();
            }
            let Some(open) = client.as_mut() else {
                continue;
            };
            let result = match &request {
                Request::Task(request) => open.request(request),
                Request::Notification(action) => open.act_on_notification(action),
                Request::OpenFile(_) => Ok(()),
            };
            if let Err(why) = result {
                log(&format!("a request to Android failed: {why}"));
                client = None;
            }
        }
    });
}

/// Asks the service whether an app drags out of the window a press left (`out`), on a thread
/// of its own, and hands an offer to the main thread to start on the desktop.
fn ask_drag_out(to_main: ToMain, out: DragOut, serial: u32) {
    std::thread::spawn(move || {
        let mut client = match connect() {
            Ok(client) => client,
            Err(why) => {
                log(&format!("a drag out of task {}: {why}", out.task_id));
                return;
            }
        };
        match client.drag_out(out) {
            Ok(Some(offer)) => {
                let drag_id = offer.drag_id;
                log(&format!(
                    "task {}: {} item(s) dragged out of the window",
                    out.task_id,
                    offer.items.len()
                ));
                let sent = to_main.send(Message::DragOut {
                    task_id: out.task_id,
                    offer,
                    client,
                    serial,
                });
                if !sent {
                    if let Ok(mut client) = connect() {
                        let _ = client.end_drag(drag_id, false);
                    }
                }
            }
            Ok(None) => {}
            Err(why) => log(&format!(
                "a drag out of task {} did not start: {why}",
                out.task_id
            )),
        }
    });
}

fn sync_clipboard(to_main: ToMain, outgoing: Receiver<String>) {
    let outgoing = Arc::new(Mutex::new(outgoing));
    std::thread::spawn(move || loop {
        let mut stream = match connect()
            .and_then(|client| client.watch_clipboard().map_err(|error| error.to_string()))
        {
            Ok(stream) => stream,
            Err(_) => {
                std::thread::sleep(RETRY);
                continue;
            }
        };
        loop {
            let mut ended = false;
            if let Ok(outgoing) = outgoing.lock() {
                while let Ok(text) = outgoing.try_recv() {
                    if stream.send(&text).is_err() {
                        ended = true;
                        break;
                    }
                }
            }
            if ended {
                break;
            }
            match stream.next(Duration::from_millis(100)) {
                Ok(Some(text)) => {
                    if !to_main.send(Message::GuestClipboard(text)) {
                        return;
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        std::thread::sleep(RETRY);
    });
}

fn show_notifications(requests: Sender<Request>, icons: PathBuf) {
    let shown = Arc::new(Mutex::new(ShownNotifications::new()));
    {
        let shown = shown.clone();
        let icons = icons.clone();
        std::thread::spawn(move || loop {
            match Notifier::connect(icons.clone()) {
                Ok(notifier) => {
                    let watched = notifier.watch(|signal| {
                        let action = match shown.lock() {
                            Ok(mut shown) => match signal {
                                Signal::ActionInvoked { id, action } => {
                                    shown.action_invoked(id, &action)
                                }
                                Signal::Closed { id, reason } => shown.closed(id, reason),
                            },
                            Err(_) => None,
                        };
                        if let Some(action) = action {
                            let _ = requests.send(Request::Notification(action));
                        }
                    });
                    if let Err(why) = watched {
                        log(&why);
                    }
                }
                Err(why) => log(&why),
            }
            std::thread::sleep(Duration::from_secs(10));
        });
    }
    std::thread::spawn(move || loop {
        let notifier = match Notifier::connect(icons.clone()) {
            Ok(notifier) => notifier,
            Err(why) => {
                log(&why);
                std::thread::sleep(Duration::from_secs(10));
                continue;
            }
        };
        let mut stream = match connect().and_then(|client| {
            client
                .watch_notifications()
                .map_err(|error| error.to_string())
        }) {
            Ok(stream) => stream,
            Err(_) => {
                std::thread::sleep(RETRY);
                continue;
            }
        };
        loop {
            match stream.next(Duration::from_secs(1)) {
                Ok(Some(NotificationEvent::Posted(notification))) => {
                    let notification = in_user_language(notification, reads_korean());
                    let notify = match shown.lock() {
                        Ok(shown) => shown.notify(&notification),
                        Err(_) => continue,
                    };
                    match notifier.notify(&notify, &notification.icon_png) {
                        Ok(id) => {
                            if let Ok(mut shown) = shown.lock() {
                                shown.shown(&notification.key, id);
                            }
                        }
                        Err(why) => log(&why),
                    }
                }
                Ok(Some(NotificationEvent::Removed { key })) => {
                    let id = shown.lock().ok().and_then(|mut shown| shown.removed(&key));
                    if let Some(id) = id {
                        notifier.close(id);
                    }
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        let ids = shown
            .lock()
            .map(|mut shown| shown.clear())
            .unwrap_or_default();
        for id in ids {
            notifier.close(id);
        }
        std::thread::sleep(RETRY);
    });
}

fn follow_gamepads(active: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let mut source = EvdevGamepads::new();
        let stop = AtomicBool::new(false);
        let mut connector: Box<dyn Connector> = Box::new(connector());
        dsa_host_core::gamepads::follow(
            &mut *connector,
            BUILD,
            &mut source,
            &|| active.load(Ordering::Relaxed),
            &stop,
            &log,
        );
    });
}

fn start_graphics(
    frames: Arc<Mutex<LatestFrames>>,
    images: Option<gfxstream::GpuImages>,
    waker: Arc<PipeWaker>,
) {
    let path = gfxstream::backend_path();
    let sink = FrameSink {
        frames,
        waker,
        images,
    };
    match Renderer::load(&path, sink) {
        Ok(renderer) => {
            let renderer = Arc::new(renderer);
            let listen = Listen::from_env();
            let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(log);
            for port in [gfxstream::COMMAND_PORT, gfxstream::PROCESS_PIPE_PORT] {
                gfxstream::serve_port(renderer.clone(), listen.clone(), port, log.clone());
            }
        }
        Err(why) => log(&format!(
            "graphics: {why}; task windows open but show no frames until the renderer is installed"
        )),
    }
}

/// The main thread's state.
struct Main {
    desktop: Desktop,
    windows: TaskWindows,
    shown: ShownWindows,
    requests: Sender<Request>,
    input: Sender<InputEvent>,
    clipboard: Sender<String>,
    frames: Arc<Mutex<LatestFrames>>,
    images: Option<gfxstream::GpuImages>,
    active: Arc<AtomicBool>,
    /// The apps the guest told of, for the launcher entries.
    apps: BTreeMap<String, AppInfo>,
    folders: Option<Folders>,
    /// What the guest copied last, so its echo from the desktop is not sent back.
    guest_clipboard: Option<String>,
    focused: Option<i32>,
    /// Where the threads that ask about drags hand their offers.
    to_main: ToMain,
}

impl Main {
    fn request(&self, request: TaskRequest) {
        let _ = self.requests.send(Request::Task(request));
    }

    fn requests(&self, requests: Vec<TaskRequest>) {
        for request in requests {
            self.request(request);
        }
    }

    fn changes(&mut self, changes: Vec<WindowChange>) {
        for change in changes {
            let opened = match &change {
                WindowChange::Open { task_id, .. } => Some(*task_id),
                _ => None,
            };
            self.desktop.state.change(&change);
            if let Some(task_id) = opened {
                let dpi = self
                    .desktop
                    .state
                    .windows
                    .get(&task_id)
                    .map(|window| window.model.dpi())
                    .unwrap_or(96);
                if let Some((size, requests)) = self.windows.window_opened(task_id, dpi) {
                    self.desktop.state.change(&WindowChange::Resize {
                        task_id,
                        width: size.0,
                        height: size.1,
                    });
                    self.requests(requests);
                }
            }
        }
        self.after_changes();
    }

    fn after_changes(&mut self) {
        if let Some(request) = self.shown.update(&self.windows.windows()) {
            self.request(request);
        }
    }

    fn sync_entries(&mut self) {
        let (Some(folders), Ok(program)) = (&self.folders, std::env::current_exe()) else {
            return;
        };
        let apps: Vec<AppInfo> = self.apps.values().cloned().collect();
        if let Err(why) = desktop_entry::sync(folders, &apps, &program) {
            log(&format!("the app menu entries could not be written: {why}"));
        }
    }

    fn message(&mut self, message: Message) {
        match message {
            Message::Tasks(events) => {
                let changes = self.windows.sync(&events);
                self.changes(changes);
                for event in &events {
                    self.shell_event(event);
                }
                self.shown.retell();
                self.after_changes();
            }
            Message::Task(event) => self.task_event(&event),
            Message::TasksLost => {}
            Message::GuestClipboard(text) => {
                self.guest_clipboard = Some(text.clone());
                self.desktop.state.set_clipboard(text);
            }
            Message::DragOut {
                task_id,
                offer,
                client,
                serial,
            } => self
                .desktop
                .state
                .start_drag_out(task_id, offer, client, serial),
        }
    }

    fn task_event(&mut self, event: &TaskEvent) {
        self.shell_event(event);
        let changes = self.windows.apply(event);
        self.changes(changes);
    }

    /// What a task event changes for the desktop's app menu.
    fn shell_event(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::Installed(apps) => {
                let known: Vec<String> = apps.iter().map(|app| app.package_name.clone()).collect();
                self.apps.retain(|package, _| known.contains(package));
                for app in apps {
                    self.apps.insert(app.package_name.clone(), app.clone());
                }
                self.sync_entries();
            }
            TaskEvent::App(app) => {
                let changed = self.apps.get(&app.package_name) != Some(app);
                if changed {
                    self.apps.insert(app.package_name.clone(), app.clone());
                    self.sync_entries();
                }
            }
            _ => {}
        }
    }

    fn event(&mut self, event: Event) {
        let now = Instant::now();
        match event {
            Event::Input(input) => {
                let _ = self.input.send(input);
            }
            Event::PipInput { task_id, input } => {
                self.desktop.state.pointer_input(task_id, input);
            }
            Event::HostClipboard(text) => {
                if self.guest_clipboard.as_deref() != Some(text.as_str()) {
                    let _ = self.clipboard.send(text);
                }
            }
            Event::DragOut { out, serial } => ask_drag_out(self.to_main.clone(), out, serial),
            Event::Dropped { package, paths, .. } => {
                for path in paths {
                    match FileTransfer::read(&path, FileAction::Send) {
                        Ok(mut file) => {
                            file.package = package.clone();
                            let _ = self.requests.send(Request::OpenFile(file));
                        }
                        Err(why) => log(&format!("{} cannot go to Android: {why}", path.display())),
                    }
                }
            }
            Event::Report(report) => match report {
                FromHost::Closed { task_id } => {
                    if let Some(request) = self.windows.user_closed(task_id) {
                        self.request(request);
                    }
                }
                FromHost::Resized {
                    task_id,
                    width,
                    height,
                } => {
                    let requests = self.windows.window_resized(task_id, (width, height), now);
                    self.requests(requests);
                }
                FromHost::Dpi { task_id, dpi } => {
                    let requests = self.windows.window_dpi_changed(task_id, dpi);
                    self.requests(requests);
                }
                FromHost::Focus { task_id, focused } => {
                    if focused {
                        self.focused = Some(task_id);
                    } else if self.focused == Some(task_id) {
                        self.focused = None;
                    }
                    self.active.store(self.focused.is_some(), Ordering::Relaxed);
                }
                FromHost::Dragging { task_id, dragging } => {
                    let requests = self.windows.window_dragging(task_id, dragging, now);
                    self.requests(requests);
                }
                FromHost::Shown { task_id, shown } => {
                    self.shown.reported(task_id, shown);
                    self.after_changes();
                }
                FromHost::Caption { task_id, height } => {
                    let requests = self.windows.window_caption(task_id, height);
                    self.requests(requests);
                }
            },
        }
    }

    fn present(&mut self) {
        let frames = match self.frames.lock() {
            Ok(mut frames) => frames.take(),
            Err(_) => return,
        };
        let images = self
            .images
            .as_ref()
            .and_then(|images| {
                images
                    .lock()
                    .ok()
                    .map(|mut images| std::mem::take(&mut *images))
            })
            .unwrap_or_default();
        for (image, fd) in images {
            self.desktop.state.add_gpu_image(image, fd);
        }
        for (display_id, frame) in frames {
            let Some(task_id) = self.windows.task_for_display(display_id) else {
                continue;
            };
            match frame {
                Shown::Pixels(frame) => self.desktop.state.present(task_id, frame),
                Shown::Gpu(frame) => self.desktop.state.present_gpu(task_id, display_id, frame),
            }
        }
        self.desktop.state.draw_waiting();
    }

    fn tick(&mut self, now: Instant) {
        let changes = self.windows.tick(now);
        if !changes.is_empty() {
            self.changes(changes);
        }
        let requests = self.windows.reconfigures(now);
        self.requests(requests);
        self.desktop.tick(now);
    }
}

/// Runs the compositor until the Wayland display goes away.
pub fn run() -> Result<(), String> {
    let (wake_read, waker) = wake_pipe()?;
    let (event_sender, event_receiver) = mpsc::channel();
    let events = Events::new(event_sender, waker.clone());
    let desktop = Desktop::connect(events)?;

    let (message_sender, messages) = mpsc::channel();
    let to_main = ToMain {
        sender: message_sender,
        waker: waker.clone(),
    };
    let (requests, request_receiver) = mpsc::channel();
    let (input, input_receiver) = mpsc::channel();
    let (clipboard, clipboard_receiver) = mpsc::channel();
    let frames = Arc::new(Mutex::new(LatestFrames::new()));
    let active = Arc::new(AtomicBool::new(false));
    let folders = Folders::from_env();

    watch_tasks(to_main.clone());
    send_input(input_receiver);
    serve_requests(request_receiver);
    sync_clipboard(to_main.clone(), clipboard_receiver);
    if let Some(folders) = &folders {
        show_notifications(requests.clone(), folders.icons.join("notifications"));
    }
    follow_gamepads(active.clone());
    let images = desktop
        .state
        .takes_gpu_frames()
        .then(|| Arc::new(Mutex::new(Vec::new())));
    start_graphics(frames.clone(), images.clone(), waker);

    let mut main = Main {
        desktop,
        windows: TaskWindows::new(),
        shown: ShownWindows::new(),
        requests,
        input,
        clipboard,
        frames,
        images,
        active,
        apps: BTreeMap::new(),
        folders,
        guest_clipboard: None,
        focused: None,
        to_main,
    };
    if let Ok(mut client) = connect() {
        if let Ok(settings) = client.app_window_settings() {
            let changes = main.windows.set_app_windows(&settings);
            main.changes(changes);
        }
    }
    loop {
        main.desktop.wait(wake_read.as_raw_fd(), TICK)?;
        drain(&wake_read);
        while let Ok(message) = messages.try_recv() {
            main.message(message);
        }
        while let Ok(event) = event_receiver.try_recv() {
            main.event(event);
        }
        main.present();
        main.tick(Instant::now());
        main.desktop.flush();
    }
}

/// `--launch <package>` and the launcher entry's actions: one request, then exit.
pub fn one_shot(action: &str, package: &str) -> Result<(), String> {
    let mut client = connect()?;
    match action {
        "--launch" => {
            client
                .ensure_running(Duration::from_secs(120), Duration::from_millis(500), |_| {})
                .map_err(|error| error.to_string())?;
            client
                .launch_app(package)
                .map_err(|error| error.to_string())
        }
        "--app-settings" => client
            .open_app_settings(Some(package))
            .map_err(|error| error.to_string()),
        "--uninstall" => client
            .uninstall_app(package)
            .map(|_| ())
            .map_err(|error| error.to_string()),
        other => Err(format!("unknown action {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr25_the_run_as_arm_notices_follow_the_sessions_language() {
        let korean = |name: &str| (name == "LANG").then(|| "ko_KR.UTF-8".to_string());
        let english = |name: &str| (name == "LANG").then(|| "en_US.UTF-8".to_string());
        // LC_ALL comes first, as gettext reads it.
        let both = |name: &str| match name {
            "LC_ALL" => Some("en_GB.UTF-8".to_string()),
            "LANG" => Some("ko_KR.UTF-8".to_string()),
            _ => None,
        };
        assert!(reads_korean_in(korean));
        assert!(!reads_korean_in(english));
        assert!(!reads_korean_in(both));
    }
}
