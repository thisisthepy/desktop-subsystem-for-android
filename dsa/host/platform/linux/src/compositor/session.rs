//! What ties the windows to the DSA service: the user's tasks, their frames, the guest's
//! input, the clipboard, notifications, controllers, launcher entries, dropped files, and the
//! widgets placed on the desktop.
//!
//! The windows and the window rules (`TaskWindows`) live on the main thread with the Wayland
//! connection. Each connection to the service runs on a thread of its own and reconnects
//! when the service restarts; what a thread has for the windows goes through a channel, and
//! a pipe wakes the main thread. The graphics renderer runs in a process of its own (`dsa-gfx`),
//! which the compositor starts and starts again whenever it ends. A broken stream, a dead
//! guest, a crashed or missing renderer is reported and waited out; nothing of it ends the
//! compositor, and the windows keep their last frame meanwhile.

use std::collections::{BTreeMap, HashMap};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::frame_stall::{restart_in_background, FrameStall, Stall};
use dsa_host_core::compositor::gpu_reset::{GpuResetWindows, Report};
use dsa_host_core::compositor::shown_windows::ShownWindows;
use dsa_host_core::compositor::task_windows::{TaskWindows, WindowChange};
use dsa_host_core::compositor::window_hosts::{FromHost, MenuWork};
use dsa_host_core::drag::{DragOffer, DragOut};
use dsa_host_core::files::{FileAction, FileTransfer};
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::input::InputEvent;
use dsa_host_core::native_abi::in_user_language;
use dsa_host_core::notifications::{NotificationAction, NotificationEvent};
use dsa_host_core::tasks::{AppInfo, PinShortcutRequest, TaskEvent, TaskRequest};
use dsa_host_core::widget_windows::{WidgetWindows, WindowCommand, WindowReport};
use dsa_host_unix::socket::UnixLocalConnector;
use dsa_transport::Connector;

use super::frames::{LatestFrames, Shown};
use super::gfxstream::{self, FrameSink};
use super::wayland::{Desktop, Event, Events, Wake};
use super::window_menu::{app_page_arguments, menu_work, reads_korean_in};
use crate::accessibility::objects::{window_facts, TitleCommand, WindowFacts};
use crate::accessibility::watch::{ApiConnect, Shared as Accessibility};
use crate::input::evdev::EvdevGamepads;
use crate::renderer::client::RendererClient;
use crate::shell::dbus::{Notifier, Signal};
use crate::shell::desktop_entry::{self, Folders};
use crate::shell::notifications::{dsa_notice, Shown as ShownNotifications, UPDATE_PAGE_ARGUMENTS};
use crate::shell::pin_shortcuts::{self, Questions};
use dsa_host_core::renderer::{spawn_supervisor, RendererHandler};
use dsa_host_core::update::alerts::is_update_key;
use dsa_host_unix::renderer_launch::{CurrentFds, UnixLauncher};

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

/// Opens DSA Settings (the settings app installed beside this program) on the Apps page at
/// `package`, as the window menu's App info does. The settings app runs on its own; nothing
/// waits for it.
fn open_app_page(package: &str) -> Result<(), String> {
    let program = std::env::current_exe()
        .map_err(|e| format!("this program's place is not known: {e}"))?
        .with_file_name("dsa-settings");
    std::process::Command::new(&program)
        .args(app_page_arguments(package))
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {}: {e}", program.display()))
}

/// Opens DSA Settings on the Updates page, as a click on DSA's news about an update does.
fn open_update_page() -> Result<(), String> {
    let program = std::env::current_exe()
        .map_err(|e| format!("this program's place is not known: {e}"))?
        .with_file_name("dsa-settings");
    std::process::Command::new(&program)
        .args(UPDATE_PAGE_ARGUMENTS)
        .spawn()
        .map(drop)
        .map_err(|e| format!("could not start {}: {e}", program.display()))
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
    /// An accessibility client activated a title bar button.
    TitleBar(TitleCommand),
    /// What the widgets' watch asks of the widget windows.
    Widget(WindowCommand),
    /// The desktop's dark theme and accent color, which the widget windows are drawn in.
    WidgetLook {
        dark: bool,
        accent: u32,
    },
}

/// What the widget windows' watch calls itself to the service.
const WIDGETS_BUILD: &str = "dsa-compositor widgets";

/// How long the desktop's theme is waited on before it is read again.
const LOOK_EVERY: Duration = Duration::from_secs(60);

/// The widget windows on the main thread, as the widgets' watch drives them: its commands go
/// to the main thread, which hands the windows' reports back.
struct MainWidgets {
    to_main: ToMain,
    reports: Receiver<WindowReport>,
}

impl WidgetWindows for MainWidgets {
    fn command(&mut self, command: WindowCommand) {
        self.to_main.send(Message::Widget(command));
    }

    fn reports(&mut self) -> Vec<WindowReport> {
        self.reports.try_iter().collect()
    }
}

/// Shows the widgets placed on the desktop: the watch runs on a thread of its own and
/// reconnects when the service restarts, and the desktop's theme is followed on another, so
/// the edit border and handles of a widget window match it.
fn show_widgets(to_main: ToMain, reports: Receiver<WindowReport>) {
    {
        let to_main = to_main.clone();
        std::thread::spawn(move || {
            use dsa_host_core::system_settings::SystemSettingsSource as _;
            let mut settings = crate::devices::settings::DesktopSettings::open();
            loop {
                let read = settings.read();
                let look = Message::WidgetLook {
                    dark: read.dark_mode,
                    accent: read.accent_color,
                };
                if !to_main.send(look) {
                    return;
                }
                settings.wait_for_change(LOOK_EVERY);
            }
        });
    }
    std::thread::spawn(move || {
        let mut windows = MainWidgets { to_main, reports };
        let stop = Arc::new(AtomicBool::new(false));
        dsa_host_core::widget_windows::sync(
            Box::new(connector()),
            &mut windows,
            stop,
            WIDGETS_BUILD,
            Arc::new(log),
        );
    });
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

/// What the thread that asks about pin requests works on.
enum PinWork {
    Ask(PinShortcutRequest),
    Signal(Signal),
}

/// Asks the user about each app's request to pin a shortcut that arrives on the returned
/// sender, on threads of their own so the compositor never waits on the notification
/// service. The answer goes to Android; on yes the shortcut's entry goes on the desktop in
/// `folders`. A question left unanswered is declined after [`pin_shortcuts::ASK_FOR`].
fn ask_pins(folders: Folders) -> Sender<PinShortcutRequest> {
    let (sender, requests) = mpsc::channel::<PinShortcutRequest>();
    let (work, todo) = mpsc::channel::<PinWork>();
    {
        let work = work.clone();
        std::thread::spawn(move || {
            for request in requests {
                if work.send(PinWork::Ask(request)).is_err() {
                    return;
                }
            }
        });
    }
    let icons = folders.icons.join("notifications");
    {
        let icons = icons.clone();
        std::thread::spawn(move || loop {
            match Notifier::connect(icons.clone()) {
                Ok(notifier) => {
                    let watched = notifier.watch(|signal| {
                        let _ = work.send(PinWork::Signal(signal));
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
    std::thread::spawn(move || {
        let mut questions = Questions::new();
        let mut notifier: Option<Notifier> = None;
        let answer = |request: &PinShortcutRequest, accepted: bool| {
            let answered = connect().and_then(|mut client| {
                client
                    .answer_pin_shortcut(request.request_id, accepted)
                    .map_err(|error| error.to_string())
            });
            if let Err(why) = answered {
                log(&format!(
                    "{} did not hear the answer about its shortcut: {why}",
                    request.package_name
                ));
            }
            if !accepted {
                return;
            }
            let placed = std::env::current_exe()
                .map_err(|e| format!("this program's place is not known: {e}"))
                .and_then(|program| {
                    desktop_entry::place_pinned_in(
                        &folders,
                        &request.package_name,
                        &request.shortcut,
                        &program,
                    )
                });
            match placed {
                Ok(path) => log(&format!(
                    "put a shortcut on the desktop: {}",
                    path.display()
                )),
                Err(why) => log(&format!("the shortcut is not on the desktop: {why}")),
            }
        };
        loop {
            let next = todo.recv_timeout(Duration::from_secs(1));
            match next {
                Ok(PinWork::Ask(request)) => {
                    if notifier.is_none() {
                        notifier = Notifier::connect(icons.clone())
                            .map_err(|why| log(&why))
                            .ok();
                    }
                    let question = pin_shortcuts::question(&request, reads_korean());
                    let shown = notifier
                        .as_ref()
                        .ok_or_else(|| "the notification service cannot be reached".to_string())
                        .and_then(|notifier| {
                            notifier.notify(&question, &request.shortcut.icon_png)
                        });
                    match shown {
                        Ok(id) => questions.asked(id, request, Instant::now()),
                        Err(why) => {
                            log(&format!("could not ask about a shortcut to pin: {why}"));
                            notifier = None;
                            answer(&request, false);
                        }
                    }
                }
                Ok(PinWork::Signal(Signal::ActionInvoked { id, action })) => {
                    if let Some((request, accepted)) = questions.action(id, &action) {
                        if let Some(notifier) = &notifier {
                            notifier.close(id);
                        }
                        answer(&request, accepted);
                    }
                }
                Ok(PinWork::Signal(Signal::Closed { id, .. })) => {
                    if let Some(request) = questions.closed(id) {
                        answer(&request, false);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
            for (id, request) in questions.expired(Instant::now()) {
                if let Some(notifier) = &notifier {
                    notifier.close(id);
                }
                answer(&request, false);
            }
        }
    });
    sender
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
                        // [Restart now] on DSA's question: the service restarts Android into
                        // the new image.
                        if let Signal::ActionInvoked { id, action } = &signal {
                            let restart = shown
                                .lock()
                                .is_ok_and(|shown| shown.restart_asked(*id, action));
                            if restart {
                                let asked = connect().and_then(|mut client| {
                                    client.restart_for_update().map_err(|e| e.to_string())
                                });
                                if let Err(why) = asked {
                                    log(&format!("Android was not restarted: {why}"));
                                }
                                return;
                            }
                        }
                        let action = match shown.lock() {
                            Ok(mut shown) => match signal {
                                Signal::ActionInvoked { id, action } => {
                                    shown.action_invoked(id, &action)
                                }
                                Signal::Closed { id, reason } => shown.closed(id, reason),
                            },
                            Err(_) => None,
                        };
                        match action {
                            // DSA's own news about an update opens the settings where the
                            // updates are; nothing about it goes to the guest.
                            Some(NotificationAction::Open { key }) if is_update_key(&key) => {
                                if let Err(why) = open_update_page() {
                                    log(&why);
                                }
                            }
                            Some(action) if is_update_key(action.key()) => {}
                            Some(action) => {
                                let _ = requests.send(Request::Notification(action));
                            }
                            None => {}
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
                        Ok(shown) => shown.notify_in(&notification, reads_korean()),
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

/// Starts the graphics renderer process (`dsa-gfx`), and starts it again whenever it ends;
/// its frames go to `frames` and `images`. The renderer runs on the GPU the user's render GPU
/// setting names, read once now.
fn start_renderer(
    frames: Arc<Mutex<LatestFrames>>,
    images: Option<gfxstream::GpuImages>,
    waker: Arc<PipeWaker>,
) -> Arc<RendererClient> {
    let render_gpu = connect()
        .and_then(|mut client| client.settings().map_err(|error| error.to_string()))
        .map(|report| report.settings.render_gpu)
        .unwrap_or_default();
    let sink = FrameSink {
        frames,
        waker,
        images,
    };
    let fds: CurrentFds = Arc::default();
    let client = RendererClient::new(sink, Arc::clone(&fds), render_gpu, Box::new(log));
    let stop = Arc::new(AtomicBool::new(false));
    if let Err(why) = spawn_supervisor(
        Box::new(UnixLauncher::installed(fds)),
        Arc::clone(&client) as Arc<dyn RendererHandler>,
        format!("dsa-compositor {BUILD}"),
        stop,
    ) {
        log(&format!(
            "graphics: the renderer cannot be started ({why}); task windows open but show no \
             frames"
        ));
    }
    client
}

/// Starts the windows' AT-SPI provider: their trees are watched through the service, and the
/// title bar buttons a client activates are carried out on the main thread.
fn start_accessibility(to_main: ToMain) -> Arc<Accessibility> {
    let connect: ApiConnect = Arc::new(|| Box::new(connector()) as Box<dyn Connector>);
    let to_main = Mutex::new(to_main);
    crate::accessibility::start(
        connect,
        Arc::new(move |command: TitleCommand| {
            if let Ok(to_main) = to_main.lock() {
                to_main.send(Message::TitleBar(command));
            }
        }),
        Arc::new(log),
    )
}

/// The guest's compositor did not recover from a GPU reset: tells the user, as DSA's own
/// notice in the session's notification service, and restarts Android through the service
/// when `stall` says so, opening `packages` (the apps that had windows) again. Both on threads
/// of their own, so the windows never wait for them.
fn recover(stall: &Stall, packages: Vec<String>, icons: Option<PathBuf>) {
    let notice = stall.notice().notification(reads_korean());
    let icons = icons.unwrap_or_else(std::env::temp_dir);
    std::thread::spawn(move || {
        let shown = Notifier::connect(icons)
            .and_then(|notifier| notifier.notify(&dsa_notice(&notice), &[]).map(drop));
        if let Err(why) = shown {
            log(&format!(
                "the user was not told that Android restarts: {why}"
            ));
        }
    });
    if !stall.restart {
        return;
    }
    let told: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line: &str| log(line));
    if let Err(why) = restart_in_background(
        Box::new(connector()),
        format!("dsa-compositor {BUILD} restart after a GPU reset"),
        packages,
        told,
    ) {
        log(&format!("could not start restarting Android: {why}"));
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
    /// Each window's AT-SPI tree, for screen readers.
    accessibility: Arc<Accessibility>,
    /// What each window last told its accessibility tree of itself.
    accessible: HashMap<i32, WindowFacts>,
    /// Where the apps' requests to pin a shortcut go to be asked about.
    pins: Option<Sender<PinShortcutRequest>>,
    /// Where the widget windows' reports go, to the widgets' watch.
    widget_reports: Sender<WindowReport>,
    /// The renderer process, whose word on its GPU device is followed.
    renderer: Arc<RendererClient>,
    /// The renderer's count of device changes last seen: a change is a GPU reset.
    device_changes: u64,
    /// The windows kept through a GPU reset, and which task each window shows.
    resets: GpuResetWindows,
    /// Whether the guest draws again after a GPU reset.
    stall: FrameStall,
    /// How many frames have reached a task window.
    frames_shown: u64,
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

    /// The task rules' changes the guest did not cause by ending a task: to the desktop, with
    /// the IDs of the windows the tasks show.
    fn changes(&mut self, changes: Vec<WindowChange>) {
        let changes = self.resets.changes(changes);
        self.to_desktop(changes);
    }

    /// The task rules' changes after the guest's tasks changed. A task that ended soon after a
    /// GPU reset keeps its window and its app is started again; the app's new task takes the
    /// window over.
    fn guest_changes(&mut self, changes: Vec<WindowChange>) {
        let now = Instant::now();
        let outcome = self.resets.guest_changes(changes, now);
        for window in &outcome.kept {
            log(&format!(
                "task window {window}'s task ended right after a GPU reset; the window stays \
                 open and its app is started again"
            ));
        }
        for package in outcome.relaunch {
            self.request(TaskRequest::Launch {
                package_name: package,
            });
        }
        for (task_id, size) in outcome.adopted {
            log(&format!(
                "task {task_id} took over task window {} after the GPU reset",
                self.resets.window_of(task_id)
            ));
            // The new task takes the size of the window it shows.
            if let Some(size) = size {
                let requests = self.windows.window_resized(task_id, size, now);
                self.requests(requests);
            }
        }
        self.to_desktop(outcome.changes);
    }

    /// Changes by window ID, to the desktop's windows.
    fn to_desktop(&mut self, changes: Vec<WindowChange>) {
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
                if let Some((size, requests, more)) = self.windows.window_opened(task_id, dpi) {
                    self.desktop.state.change(&WindowChange::Resize {
                        task_id,
                        width: size.0,
                        height: size.1,
                    });
                    // The scale the window shows a task at that keeps its density.
                    for change in &more {
                        self.desktop.state.change(change);
                    }
                    self.requests(requests);
                }
            }
        }
        self.after_changes();
    }

    fn after_changes(&mut self) {
        if let Some(request) = self.shown.update_tasks(&self.windows.windows()) {
            self.request(request);
        }
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
                self.guest_changes(changes);
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
            Message::TitleBar(command) => self
                .desktop
                .state
                .title_bar_action(command.task_id, command.action),
            Message::Widget(command) => self.desktop.state.widget_command(command),
            Message::WidgetLook { dark, accent } => self.desktop.state.widget_look(dark, accent),
        }
    }

    fn task_event(&mut self, event: &TaskEvent) {
        self.shell_event(event);
        let changes = self.windows.apply(event);
        for note in self.windows.take_notes() {
            log(&note);
        }
        self.guest_changes(changes);
    }

    /// What a task event changes for the desktop's app menu.
    fn shell_event(&mut self, event: &TaskEvent) {
        match event {
            TaskEvent::Installed(apps) => {
                let known: Vec<String> = apps.iter().map(|app| app.package_name.clone()).collect();
                self.apps.retain(|package, _| known.contains(package));
                for app in apps {
                    self.apps.insert(app.package_name.clone(), app.clone());
                    self.desktop.state.set_app(app);
                }
                self.sync_entries();
            }
            TaskEvent::PinShortcut(request) => match &self.pins {
                Some(pins) => {
                    let _ = pins.send(request.clone());
                }
                // With no folders to put the shortcut in, the app hears no at once.
                None => self.request(TaskRequest::AnswerPinShortcut {
                    request_id: request.request_id,
                    accepted: false,
                }),
            },
            TaskEvent::App(app) => {
                let changed = self.apps.get(&app.package_name) != Some(app);
                if changed {
                    self.apps.insert(app.package_name.clone(), app.clone());
                    self.desktop.state.set_app(app);
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
                // A window a task took over after a GPU reset knows only the task it opened
                // for; its input goes to the task it shows now.
                let input = match self.resets.task_of(input.task_id()) {
                    Some(task_id) => input.for_task(task_id),
                    None => return,
                };
                let _ = self.input.send(input);
            }
            Event::PipInput { task_id, input } => {
                self.desktop.state.pointer_input(task_id, input);
            }
            Event::X11Menu(input) => self.desktop.state.x11_menu_input(input),
            Event::X11Widget { widget_id, input } => {
                self.desktop.state.x11_widget_input(widget_id, input)
            }
            Event::HostClipboard(text) => {
                if self.guest_clipboard.as_deref() != Some(text.as_str()) {
                    let _ = self.clipboard.send(text);
                }
            }
            Event::DragOut { mut out, serial } => {
                let Some(task_id) = self.resets.task_of(out.task_id) else {
                    return;
                };
                out.task_id = task_id;
                ask_drag_out(self.to_main.clone(), out, serial)
            }
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
            Event::Report(report) => match self.resets.report(report) {
                Report::Task(report) => self.reported(report, now),
                Report::CloseKept(close) => {
                    log("a window kept through a GPU reset was closed before its app came back");
                    self.to_desktop(vec![close]);
                }
                Report::Dropped => {}
            },
        }
    }

    /// A window reported what the user did to it, with the ID of the task it shows.
    fn reported(&mut self, report: FromHost, now: Instant) {
        match report {
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
                let (changes, requests) = self.windows.window_dpi_changed(task_id, dpi);
                self.changes(changes);
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
            FromHost::Caption { task_id, band } => {
                let requests = self.windows.window_caption(task_id, band);
                self.requests(requests);
            }
            FromHost::MenuChosen { task_id, action } => {
                let window = self.resets.window_of(task_id);
                self.menu_chosen(window, action)
            }
            // Only a Windows taskbar preview has media buttons; these windows send none.
            FromHost::MediaButton { .. } => {}
        }
    }

    /// The user chose `action` in the window menu of window `task_id`.
    fn menu_chosen(
        &mut self,
        task_id: i32,
        action: dsa_host_core::compositor::window_hosts::MenuAction,
    ) {
        let Some(package) = self
            .desktop
            .state
            .windows
            .get(&task_id)
            .map(|window| window.model.package.clone())
        else {
            return;
        };
        let multiple_windows = self
            .apps
            .get(&package)
            .is_some_and(|app| app.multiple_windows);
        match menu_work(action, &package, multiple_windows) {
            Some(MenuWork::Request(request)) => self.request(request),
            Some(MenuWork::AppPage { package }) => {
                if let Err(why) = open_app_page(&package) {
                    log(&format!("App info of {package} did not open: {why}"));
                }
            }
            None => {}
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
            // A task that took a window over after a GPU reset draws into that window.
            let window = self.resets.window_of(task_id);
            if self.desktop.state.windows.contains_key(&window) {
                self.frames_shown += 1;
            }
            match frame {
                Shown::Pixels(frame) => self.desktop.state.present(window, frame),
                Shown::Gpu(frame) => self.desktop.state.present_gpu(window, display_id, frame),
            }
        }
        self.desktop.state.draw_waiting();
    }

    /// Follows the renderer's word on its GPU device: a change is a GPU reset. A task that
    /// ends soon after one keeps its window, and the windows are watched for the frames that
    /// follow; when none comes for long, the guest's compositor did not recover, and Android is
    /// restarted.
    fn follow_gpu_resets(&mut self, now: Instant) {
        let changes = self.renderer.device_changes();
        if changes != self.device_changes {
            self.device_changes = changes;
            self.resets.gpu_reset(now);
            self.stall.gpu_reset(now, self.frames_shown);
        }
        let late = self.resets.tick(now);
        if !late.is_empty() {
            log(&format!(
                "{} task window(s) kept through a GPU reset close: their app did not come back",
                late.len()
            ));
            self.to_desktop(late);
        }
        let shown = self.shown.shown_among(&self.windows.windows());
        let Some(stall) = self.stall.tick(now, &shown, self.frames_shown) else {
            return;
        };
        log(&stall.reason());
        let mut packages = self.windows.window_packages();
        for package in self.resets.kept_packages() {
            if !packages.contains(&package) {
                packages.push(package);
            }
        }
        let icons = self
            .folders
            .as_ref()
            .map(|folders| folders.icons.join("notifications"));
        recover(&stall, packages, icons);
    }

    fn tick(&mut self, now: Instant) {
        self.follow_gpu_resets(now);
        let changes = self.windows.tick(now);
        if !changes.is_empty() {
            self.changes(changes);
        }
        let requests = self.windows.reconfigures(now);
        self.requests(requests);
        self.desktop.tick(now);
        // What the widget windows did since the last turn goes to the service.
        for report in self.desktop.state.widget_reports() {
            let _ = self.widget_reports.send(report);
        }
        self.sync_accessibility();
    }

    /// Tells the accessibility tree what each window shows of itself, while the provider is
    /// on the accessibility bus. Off it nothing is kept, so a session without a screen reader
    /// does no work for it.
    fn sync_accessibility(&mut self) {
        if !self.accessibility.connected() {
            for (task_id, _) in self.accessible.drain() {
                self.accessibility.close(task_id);
            }
            return;
        }
        let windows = &self.desktop.state.windows;
        let accessibility = &self.accessibility;
        self.accessible.retain(|task_id, _| {
            let open = windows.contains_key(task_id);
            if !open {
                accessibility.close(*task_id);
            }
            open
        });
        for (task_id, window) in windows {
            let facts = window_facts(&window.model, &window.title_bar);
            if self.accessible.get(task_id) != Some(&facts) {
                accessibility.sync(*task_id, facts.clone());
                self.accessible.insert(*task_id, facts);
            }
        }
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
    let pins = folders.clone().map(ask_pins);
    follow_gamepads(active.clone());
    let images = desktop
        .state
        .takes_gpu_frames()
        .then(|| Arc::new(Mutex::new(Vec::new())));
    let renderer = start_renderer(frames.clone(), images.clone(), waker);
    let accessibility = start_accessibility(to_main.clone());
    let (widget_reports, widget_report_receiver) = mpsc::channel();
    show_widgets(to_main.clone(), widget_report_receiver);

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
        accessibility,
        accessible: HashMap::new(),
        pins,
        widget_reports,
        device_changes: renderer.device_changes(),
        renderer,
        resets: GpuResetWindows::new(),
        stall: FrameStall::new(),
        frames_shown: 0,
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

/// `--launch-shortcut <package> <id>`, a shortcut action of an app's launcher entry or a
/// pinned shortcut on the desktop: starts Android when it is off, then the shortcut.
pub fn launch_shortcut(package: &str, id: &str) -> Result<(), String> {
    let mut client = connect()?;
    client
        .ensure_running(Duration::from_secs(120), Duration::from_millis(500), |_| {})
        .map_err(|error| error.to_string())?;
    client
        .launch_shortcut(package, id)
        .map_err(|error| error.to_string())
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
