//! An app's window host on macOS: the process that owns the app's task windows.
//!
//! Each Android app's bundle in `~/Applications/DSA Apps` runs a copy of this program, so
//! the Dock shows the app as itself, with its own icon, apart from every other app. The
//! window host connects to the window manager over a Unix socket and is told which windows
//! to show; each task's frames arrive in an `IOSurface` the window manager writes, handed
//! over as a Mach port ([`super::surface_port`]), and are shown from it on the GPU. The
//! windows' input goes straight to the service's local API.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::caption::CaptionBand;
use dsa_host_core::compositor::window_hosts::{
    connect_window_host, report_to_wire, to_host_from_wire, FromHost, ToHost,
};
use dsa_host_core::compositor::window_mode::WindowMode;
use dsa_host_core::compositor::window_tasks::WindowTasks;
use dsa_host_core::drag::{DragOffer, DragOut};
use dsa_host_core::files::{FileAction, FileTransfer};
use dsa_host_core::host_api::{HostApiClient, InputStream};
use dsa_host_core::ime::ImeCursor;
use dsa_host_core::input::InputEvent;
use dsa_host_unix::socket::UnixLocalConnector;
use dsa_protocol::window_host::types::{envelope::Body, Attach, FramePresented};
use dsa_protocol::window_host::{decode, encode_body};
use dsa_transport::{Connector, FrameSender};
use objc2::msg_send;
use objc2::runtime::AnyObject;

use super::surface::Surface;
use super::surface_port::receive_surfaces;
use super::title_bar::back_keys;
use super::window::{TaskWindow, WindowEvents};
use super::window_menu::{self, WindowMenuItem};
use super::{menu_bar, screenshot};
use crate::input::view::InputSink;
use crate::objc::{class, on_main, string_of};
use crate::shell::app_stubs::{PACKAGE_KEY, SHORTCUT_KEY};

/// The build named in the handshake.
pub const BUILD: &str = concat!("dsa-app-window ", env!("CARGO_PKG_VERSION"));
/// How long the connection's opening may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long input waits before it tries the service again after it could not reach it.
const INPUT_RETRY: Duration = Duration::from_secs(2);

/// The window manager's socket for a user whose DSA folder is `support`.
pub fn window_manager_socket(support: &Path) -> PathBuf {
    support.join("window-manager.sock")
}

/// The local API's socket for a user whose DSA folder is `support`.
pub fn host_api_socket(support: &Path) -> PathBuf {
    support.join("host.sock")
}

/// The user's DSA folder: `~/Library/Application Support/DSA`.
pub fn support_folder() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support/DSA"))
}

/// The package this program shows: the `--package` argument, else the `DSAPackage` key of
/// the bundle it runs from.
pub fn package_of(args: &[String]) -> Option<String> {
    if let Some(at) = args.iter().position(|a| a == "--package") {
        return args.get(at + 1).cloned();
    }
    unsafe {
        let bundle: *mut AnyObject = msg_send![class(c"NSBundle").ok()?, mainBundle];
        let value: *mut AnyObject = msg_send![
            bundle,
            objectForInfoDictionaryKey: &*crate::objc::ns_string(PACKAGE_KEY)
        ];
        let package = string_of(value);
        (!package.is_empty()).then_some(package)
    }
}

/// The shortcut this program starts: the `--shortcut` argument, which starts one of the
/// app's shortcuts in its window. `None` starts the app.
pub fn shortcut_argument(args: &[String]) -> Option<String> {
    let at = args.iter().position(|a| a == "--shortcut")?;
    args.get(at + 1)
        .filter(|id| dsa_host_core::tasks::is_valid_shortcut_id(id))
        .cloned()
}

/// The shortcut the bundle this program runs from starts, when it is the bundle of a pinned
/// shortcut on the Desktop (its `DSAShortcut` key).
pub fn pinned_shortcut() -> Option<String> {
    unsafe {
        let bundle: *mut AnyObject = msg_send![class(c"NSBundle").ok()?, mainBundle];
        let value: *mut AnyObject = msg_send![
            bundle,
            objectForInfoDictionaryKey: &*crate::objc::ns_string(SHORTCUT_KEY)
        ];
        let id = string_of(value);
        (!id.is_empty()).then_some(id)
    }
}

/// A pinned shortcut's bundle opened from the Desktop: starts Android when it is off, then
/// the shortcut `id` of `package`, whose task opens in the app's own window host; this
/// program shows nothing and ends.
pub fn start_pinned(host_api: &Path, package: &str, id: &str) -> Result<(), String> {
    let mut connector = UnixLocalConnector {
        path: host_api.to_path_buf(),
    };
    let mut client = HostApiClient::connect(&mut connector, BUILD, CONNECT_TIMEOUT)
        .map_err(|e| format!("the DSA service is not running: {e}"))?;
    client
        .ensure_running(Duration::from_secs(120), Duration::from_millis(500), |_| {})
        .map_err(|e| format!("Android did not start: {e}"))?;
    client
        .launch_shortcut(package, id)
        .map_err(|e| e.to_string())
}

type Reports = Arc<Mutex<Box<dyn FrameSender>>>;

fn report(reports: &Reports, message: FromHost) {
    if let Ok(mut sender) = reports.lock() {
        let _ = sender.send(&encode_body(report_to_wire(&message)));
    }
}

/// Sends the windows' input to the service until every sender is gone. Each event names the
/// window it was made in, and goes to the task that window shows now, as `tasks` says: another
/// task once one took the window over after a GPU reset.
fn serve_input(mut connector: impl Connector, events: Receiver<InputEvent>, tasks: WindowTasks) {
    let mut stream: Option<InputStream> = None;
    let mut retry_at: Option<Instant> = None;
    while let Ok(event) = events.recv() {
        let event = tasks.input(event);
        for _ in 0..2 {
            if stream.is_none() {
                if retry_at.is_some_and(|at| Instant::now() < at) {
                    break;
                }
                match HostApiClient::connect(&mut connector, BUILD, CONNECT_TIMEOUT)
                    .and_then(|client| client.stream_input())
                {
                    Ok(connected) => {
                        stream = Some(connected);
                        retry_at = None;
                    }
                    Err(error) => {
                        eprintln!("dsa-app-window: input cannot reach Android: {error}");
                        retry_at = Some(Instant::now() + INPUT_RETRY);
                        break;
                    }
                }
            }
            let Some(connected) = stream.as_mut() else {
                break;
            };
            match connected.send_input(&event) {
                Ok(()) => break,
                Err(_) => stream = None,
            }
        }
    }
}

struct Events {
    task_id: i32,
    reports: Reports,
    /// The windows' input, for the back button's keys.
    input: Sender<InputEvent>,
}

/// The host's clock, in milliseconds of the system's uptime, which wraps: the clock of the
/// events AppKit gives.
fn uptime_ms() -> u32 {
    unsafe {
        let Ok(info) = class(c"NSProcessInfo") else {
            return 0;
        };
        let info: *mut AnyObject = msg_send![info, processInfo];
        let uptime: f64 = msg_send![info, systemUptime];
        ((uptime * 1000.0) as u64 & u64::from(u32::MAX)) as u32
    }
}

impl WindowEvents for Events {
    fn resized(&mut self, pixels: (u32, u32)) {
        report(
            &self.reports,
            FromHost::Resized {
                task_id: self.task_id,
                width: pixels.0,
                height: pixels.1,
            },
        );
    }

    fn close_requested(&mut self) {
        report(
            &self.reports,
            FromHost::Closed {
                task_id: self.task_id,
            },
        );
    }

    fn focused(&mut self, focused: bool) {
        report(
            &self.reports,
            FromHost::Focus {
                task_id: self.task_id,
                focused,
            },
        );
    }

    fn scale_changed(&mut self, scale: f64) {
        report(
            &self.reports,
            FromHost::Dpi {
                task_id: self.task_id,
                dpi: (scale * 96.0).round() as u32,
            },
        );
        // Reported from inside a window call the state is busy with: the window then takes
        // the scale with its next frame instead.
        STATE.with(|state| {
            if let Ok(mut state) = state.try_borrow_mut() {
                if let Some(window) = state.windows.get_mut(&self.task_id) {
                    window.set_scale(scale);
                }
            }
        });
    }

    fn shown(&mut self, shown: bool) {
        if !shown {
            // A window in the Dock shows no frames: its surfaces go, and the window manager
            // stops sending frames until it comes back.
            let task_id = self.task_id;
            on_main(move || {
                STATE.with(|state| {
                    if let Ok(mut state) = state.try_borrow_mut() {
                        state.textures.retain(|_, (task, _)| *task != task_id);
                    }
                })
            });
        }
        report(
            &self.reports,
            FromHost::Shown {
                task_id: self.task_id,
                shown,
            },
        );
    }

    fn dragging(&mut self, dragging: bool) {
        report(
            &self.reports,
            FromHost::Dragging {
                task_id: self.task_id,
                dragging,
            },
        );
    }

    fn caption(&mut self, band: CaptionBand) {
        report(
            &self.reports,
            FromHost::Caption {
                task_id: self.task_id,
                band,
            },
        );
    }

    fn back_pressed(&mut self) {
        // Through the same path as the keyboard's keys, so Back reaches the task as a key.
        for key in back_keys(self.task_id, uptime_ms()) {
            let _ = self.input.send(InputEvent::Key(key));
        }
    }
}

type Cursors = Arc<Mutex<HashMap<i32, ImeCursor>>>;

/// The editor focused in each task, as the agent last described it.
type Editors = Arc<Mutex<HashMap<i32, dsa_host_core::ime::ImeEditor>>>;

struct Input {
    task_id: i32,
    events: Sender<InputEvent>,
    cursors: Cursors,
    editors: Editors,
    files: Sender<PathBuf>,
    drags: Sender<DragOut>,
}

impl InputSink for Input {
    fn input(&mut self, event: InputEvent) {
        let _ = self.events.send(event);
    }

    fn files_dropped(&mut self, paths: Vec<PathBuf>) {
        for path in paths {
            let _ = self.files.send(path);
        }
    }

    fn ime_cursor(&self) -> Option<ImeCursor> {
        self.cursors.lock().ok()?.get(&self.task_id).copied()
    }

    fn ime_editor(&self) -> Option<dsa_host_core::ime::ImeEditor> {
        self.editors.lock().ok()?.get(&self.task_id).cloned()
    }

    fn drag_left(&mut self, x: f32, y: f32) {
        let _ = self.drags.send(DragOut {
            task_id: self.task_id,
            x,
            y,
            tool: dsa_protocol::control::PointerTool::Mouse as i32,
            pointer_id: 0,
        });
    }
}

#[derive(Default)]
struct State {
    windows: HashMap<i32, TaskWindow>,
    /// Each frame texture, by its ID: the task it is for and its surface.
    textures: HashMap<u64, (i32, Surface)>,
    /// Surfaces that came as Mach ports before their `FrameTexture`, by texture ID.
    arrived: HashMap<u64, Surface>,
    /// Textures announced before their surface's Mach port came: the task of each.
    awaited: HashMap<u64, i32>,
    reports: Option<Reports>,
    input: Option<Sender<InputEvent>>,
    files: Option<Sender<PathBuf>>,
    drags: Option<Sender<DragOut>>,
    cursors: Cursors,
    editors: Editors,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// For each press that left a window (`left`, by the window's ID), asks the service through a
/// connection from `connect` whether the app drags something out, about the task the window
/// shows now, as `tasks` says. An offer goes to `offered` with the window's ID and the
/// connection that reads it.
fn ask_drags(
    mut connect: impl FnMut() -> Box<dyn Connector>,
    left: Receiver<DragOut>,
    tasks: WindowTasks,
    offered: impl Fn(i32, DragOffer, HostApiClient),
) {
    for out in left {
        let window = out.task_id;
        let out = tasks.drag_out(out);
        let mut connector = connect();
        let asked = HostApiClient::connect(&mut *connector, BUILD, CONNECT_TIMEOUT)
            .and_then(|mut client| client.drag_out(out).map(|offer| (offer, client)));
        match asked {
            Ok((Some(offer), client)) => offered(window, offer, client),
            Ok((None, _)) => {}
            Err(why) => eprintln!(
                "dsa-app-window: a drag out of task {} did not start: {why}",
                out.task_id
            ),
        }
    }
}

/// Starts an app's drag out of task `task_id`'s window on the desktop, or ends it in the
/// guest when it cannot start. On the main thread.
fn start_drag(task_id: i32, offer: DragOffer, client: HostApiClient) {
    let drag_id = offer.drag_id;
    let mut client = Some(client);
    let started = STATE.with(|state| -> Result<(), String> {
        let state = state
            .try_borrow()
            .map_err(|_| "the windows are busy".to_string())?;
        let window = state
            .windows
            .get(&task_id)
            .ok_or_else(|| "the window has closed".to_string())?;
        let client = client.take().ok_or_else(|| "no connection".to_string())?;
        crate::input::drag_source::begin(window.view(), offer, client)
    });
    if let Err(why) = started {
        eprintln!("dsa-app-window: the drag out of task {task_id} did not start: {why}");
        if let Some(mut client) = client {
            let _ = client.end_drag(drag_id, false);
        }
    }
}

fn show(task_id: i32, title: &str, size: (u32, u32), focused: bool) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        if let Some(window) = state.windows.get(&task_id) {
            window.set_title(title);
            if focused {
                window.activate();
            }
            return;
        }
        let (Some(reports), Some(input), Some(files), Some(drags)) = (
            state.reports.clone(),
            state.input.clone(),
            state.files.clone(),
            state.drags.clone(),
        ) else {
            return;
        };
        let events = Box::new(Events {
            task_id,
            reports,
            input: input.clone(),
        });
        let sink = Box::new(Input {
            task_id,
            events: input,
            cursors: Arc::clone(&state.cursors),
            editors: Arc::clone(&state.editors),
            files,
            drags,
        });
        match TaskWindow::open(task_id, title, size, events, sink) {
            Ok(window) => {
                if focused {
                    window.activate();
                }
                state.windows.insert(task_id, window);
            }
            Err(why) => {
                eprintln!("dsa-app-window: the window of task {task_id} did not open: {why}")
            }
        }
    });
}

fn handle(message: ToHost) {
    match message {
        ToHost::Show {
            task_id,
            title,
            width,
            height,
            focused,
        } => show(task_id, &title, (width, height), focused),
        ToHost::Retitle { task_id, title } => STATE.with(|state| {
            if let Some(window) = state.borrow().windows.get(&task_id) {
                window.set_title(&title);
            }
        }),
        ToHost::Resize {
            task_id,
            width,
            height,
        } => STATE.with(|state| {
            if let Some(window) = state.borrow_mut().windows.get_mut(&task_id) {
                window.resize_to((width, height));
            }
        }),
        ToHost::Close { task_id } => STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.windows.remove(&task_id);
            state.textures.retain(|_, (task, _)| *task != task_id);
            state.awaited.retain(|_, task| *task != task_id);
            if state.windows.is_empty() {
                // The app has no window left: its Dock icon goes with this process.
                quit();
            }
        }),
        ToHost::TaskSize {
            task_id,
            min,
            max,
            scale,
            ..
        } => STATE.with(|state| {
            if let Some(window) = state.borrow().windows.get(&task_id) {
                window.set_limits(min, max);
            }
            // The layer and the pointer take the scale; AppKit calls nothing back for it.
            if let Ok(mut state) = state.try_borrow_mut() {
                if let Some(window) = state.windows.get_mut(&task_id) {
                    window.set_frame_scale(scale);
                }
            }
        }),
        ToHost::ImeCursor { task_id, cursor } => STATE.with(|state| {
            if let Ok(mut cursors) = state.borrow().cursors.lock() {
                cursors.insert(task_id, cursor);
            }
        }),
        // The window's input method reads it at its next call.
        ToHost::ImeEditor { task_id, editor } => STATE.with(|state| {
            if let Ok(mut editors) = state.borrow().editors.lock() {
                editors.insert(task_id, editor);
            }
        }),
        // macOS draws the title bar and its buttons in the system's own colors.
        ToHost::CaptionTone { .. } => {}
        ToHost::Mode {
            task_id,
            mode,
            resizable,
        } => STATE.with(|state| {
            if let Some(window) = state.borrow_mut().windows.get_mut(&task_id) {
                window.set_resizable(resizable);
                window.set_picture_in_picture(mode == WindowMode::PictureInPicture);
                window.set_full_screen(mode == WindowMode::Fullscreen);
            }
        }),
        ToHost::CaptionMode {
            task_id,
            app_drawn,
            interactive,
        } => STATE.with(|state| {
            if let Some(window) = state.borrow_mut().windows.get_mut(&task_id) {
                window.set_app_drawn(app_drawn, interactive);
            }
        }),
        ToHost::CaptionBack { task_id, back } => STATE.with(|state| {
            if let Some(window) = state.borrow().windows.get(&task_id) {
                window.set_back(back);
            }
        }),
        // The window host's `WindowTasks` took it before it reached this thread, so the
        // window's input goes to the task it shows; the window itself shows the same.
        ToHost::ShownTask { .. } => {}
        ToHost::AppMenu { new_window } => menu_bar::set_new_window(new_window),
        ToHost::AppIcon { icon_png, .. } => set_dock_icon(&icon_png),
        // The media buttons are a Windows taskbar preview's; macOS has no such preview.
        ToHost::AppMedia { .. } => {}
    }
}

/// Shows `png` as the app's icon in the Dock and the app switcher at once, when the guest
/// gives a new one while the app's windows are open. The bundle's own icon takes it the next
/// time the bundle is made.
fn set_dock_icon(png: &[u8]) {
    use objc2::rc::{Allocated, Retained};
    let Ok(data) = crate::objc::ns_data(png) else {
        return;
    };
    // SAFETY: AppKit calls on the main thread, where the window host handles its messages,
    // on objects that live through them.
    unsafe {
        let Ok(image_class) = class(c"NSImage") else {
            return;
        };
        let image: Allocated<AnyObject> = msg_send![image_class, alloc];
        let image: Option<Retained<AnyObject>> = msg_send![image, initWithData: &*data];
        let Some(image) = image else {
            return;
        };
        let Ok(app_class) = class(c"NSApplication") else {
            return;
        };
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        if !app.is_null() {
            let _: () = msg_send![app, setApplicationIconImage: &*image];
        }
    }
}

/// The task of the app's front window, which the Window menu's entries act on.
fn front_task(state: &State) -> Option<i32> {
    unsafe {
        let app: *mut AnyObject = msg_send![class(c"NSApplication").ok()?, sharedApplication];
        let front: *mut AnyObject = msg_send![app, mainWindow];
        if front.is_null() {
            return None;
        }
        state
            .windows
            .iter()
            .find(|(_, window)| std::ptr::eq(window.ns_window(), front as *const AnyObject))
            .map(|(task_id, _)| *task_id)
    }
}

/// Whether a task window is in front, for the Window menu's Android entries.
fn has_front_task() -> bool {
    STATE.with(|state| {
        state
            .try_borrow()
            .is_ok_and(|state| front_task(&state).is_some())
    })
}

/// The user chose an Android entry of the Window menu (or of the band's context menu): the
/// window host takes a screenshot itself, and the window manager carries out the rest.
fn menu_chosen(item: WindowMenuItem) {
    STATE.with(|state| {
        let Ok(state) = state.try_borrow() else {
            return;
        };
        let Some(task_id) = front_task(&state) else {
            return;
        };
        if item == WindowMenuItem::Screenshot {
            if let Err(why) = screenshot(&state, task_id) {
                eprintln!("dsa-app-window: the screenshot was not taken: {why}");
            }
            return;
        }
        if let (Some(action), Some(reports)) = (window_menu::action(item), &state.reports) {
            report(reports, FromHost::MenuChosen { task_id, action });
        }
    });
}

/// Takes a screenshot of task `task_id`'s window: the frame it shows, without the band of an
/// app's own title bar. The frame is read here, while its surface holds it; it is encoded and
/// saved on a thread of its own.
fn screenshot(state: &State, task_id: i32) -> Result<(), String> {
    let window = state.windows.get(&task_id).ok_or("the window has closed")?;
    let (texture_id, content) = window
        .shown_frame()
        .ok_or("the app has not drawn its window yet")?;
    let (_, surface) = state
        .textures
        .get(&texture_id)
        .ok_or("the window's frame is gone")?;
    let band = window.band_rows_in_frame();
    let shot = surface
        .read(|pixels, stride| screenshot::app_content(pixels, stride, content, band))?
        .ok_or("the window shows nothing under its title bar")?;
    screenshot::take(shot);
    Ok(())
}

/// How many surfaces a task's frames use at once; older ones are let go.
const SURFACES_PER_TASK: usize = 2;

/// Takes `surface` as texture `texture_id` of task `task_id`, letting go of the task's
/// surfaces the window manager no longer uses.
fn keep_texture(state: &mut State, task_id: i32, texture_id: u64, surface: Surface) {
    state.textures.insert(texture_id, (task_id, surface));
    let mut ids: Vec<u64> = state
        .textures
        .iter()
        .filter(|(_, (task, _))| *task == task_id)
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    let extra = ids.len().saturating_sub(SURFACES_PER_TASK);
    for id in &ids[..extra] {
        state.textures.remove(id);
    }
}

/// From now on task `task_id`'s frames may arrive in texture `texture_id`: the surface with
/// global ID `handle`, or, when `handle` is 0, the surface sent as a Mach port.
fn texture(task_id: i32, texture_id: u64, handle: u64) {
    if handle == 0 {
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            match state.arrived.remove(&texture_id) {
                Some(surface) => keep_texture(&mut state, task_id, texture_id, surface),
                None => {
                    state.awaited.insert(texture_id, task_id);
                }
            }
        });
        return;
    }
    match u32::try_from(handle)
        .map_err(|_| format!("{handle} is not a frame surface ID"))
        .and_then(Surface::lookup)
    {
        Ok(surface) => STATE.with(|state| {
            keep_texture(&mut state.borrow_mut(), task_id, texture_id, surface);
        }),
        Err(why) => eprintln!("dsa-app-window: task {task_id}'s frames cannot be shown: {why}"),
    }
}

/// A surface came as a Mach port for texture `texture_id`.
fn surface_arrived(texture_id: u64, surface: Surface) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        match state.awaited.remove(&texture_id) {
            Some(task_id) => keep_texture(&mut state, task_id, texture_id, surface),
            None => {
                // Its FrameTexture is still on the way. Only the newest few are kept, so a
                // surface whose announcement never comes is not held for long.
                if state.arrived.len() >= 2 * SURFACES_PER_TASK {
                    state.arrived.clear();
                }
                state.arrived.insert(texture_id, surface);
            }
        }
    });
}

fn frame(task_id: i32, texture_id: u64, sequence: u64, content: (u32, u32)) {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let State {
            windows,
            textures,
            reports,
            ..
        } = &mut *state;
        if let (Some(window), Some((_, surface))) =
            (windows.get_mut(&task_id), textures.get(&texture_id))
        {
            if let Err(why) = window.present_surface(texture_id, surface, content) {
                eprintln!("dsa-app-window: a frame of task {task_id} was not shown: {why}");
            }
        }
        // Answered whether or not it was shown, so the window manager is never held up.
        if let Some(reports) = reports {
            if let Ok(mut sender) = reports.lock() {
                let _ = sender.send(&encode_body(Body::FramePresented(FramePresented {
                    task_id,
                    sequence,
                })));
            }
        }
    });
}

fn quit() {
    unsafe {
        if let Ok(app_class) = class(c"NSApplication") {
            let app: *mut AnyObject = msg_send![app_class, sharedApplication];
            let _: () = msg_send![app, terminate: std::ptr::null::<AnyObject>()];
        }
    }
}

/// What the window host runs with.
pub struct HostSetup {
    pub package: String,
    pub window_manager: PathBuf,
    pub host_api: PathBuf,
    /// The user started the program (from Launchpad or the Dock), not the window manager.
    pub launch: bool,
    /// With `launch`, the app's shortcut to start rather than the app.
    pub shortcut: Option<String>,
}

/// Runs the window host until the window manager lets it go. Must be called on the main
/// thread; it runs the application's run loop.
pub fn run(setup: HostSetup) -> Result<(), String> {
    if !crate::objc::is_main_thread() {
        return Err("the window host must run on the main thread".into());
    }
    let mut connection = UnixLocalConnector {
        path: setup.window_manager.clone(),
    }
    .connect(CONNECT_TIMEOUT)
    .map_err(|e| format!("the window manager is not running: {e}"))?;
    connect_window_host(
        &mut *connection,
        BUILD,
        Attach {
            package_name: setup.package.clone(),
            process_id: std::process::id(),
            launch: setup.launch,
            shortcut_id: setup.shortcut.clone().unwrap_or_default(),
        },
        CONNECT_TIMEOUT,
    )?;
    let sender = connection
        .try_clone_sender()
        .ok_or("the window manager's connection cannot be shared")?;
    let reports: Reports = Arc::new(Mutex::new(sender));

    // The frame surfaces come as Mach ports where the window manager has its Mach service;
    // otherwise by their global IDs, and nothing more is needed here.
    let surfaces = receive_surfaces(crate::launchd::WM_LABEL, |texture_id, surface| {
        on_main(move || surface_arrived(texture_id, surface))
    });
    if let Err(why) = surfaces {
        eprintln!("dsa-app-window: {why}; frames come by their global surface IDs");
    }

    // The task each window shows, which what the windows send Android itself goes to.
    let tasks = WindowTasks::new();
    let (input, input_events) = channel();
    let host_api = UnixLocalConnector {
        path: setup.host_api.clone(),
    };
    let input_tasks = tasks.clone();
    std::thread::Builder::new()
        .name("dsa-app-window-input".into())
        .spawn(move || serve_input(host_api, input_events, input_tasks))
        .map_err(|e| format!("the window input could not start: {e}"))?;

    // Files dropped on a window are opened in the app through the local API.
    let (files, dropped) = channel::<PathBuf>();
    let open_with = setup.host_api.clone();
    let package = setup.package.clone();
    std::thread::Builder::new()
        .name("dsa-app-window-files".into())
        .spawn(move || {
            for path in dropped {
                let mut connector = UnixLocalConnector {
                    path: open_with.clone(),
                };
                // Dropped on the app's window: shared with that app, as on every host.
                let opened = FileTransfer::read(&path, FileAction::Send).and_then(|mut file| {
                    file.package = package.clone();
                    HostApiClient::connect(&mut connector, BUILD, CONNECT_TIMEOUT)
                        .and_then(|client| client.open_file(&file))
                        .map_err(|e| e.to_string())
                });
                if let Err(why) = opened {
                    eprintln!("dsa-app-window: {} did not open: {why}", path.display());
                }
            }
        })
        .map_err(|e| format!("the file drop could not start: {e}"))?;

    // A press that leaves a window asks the local API whether the app drags out of it; an
    // offer becomes an AppKit drag on the main thread.
    let (drags, left) = channel::<DragOut>();
    let ask_with = setup.host_api.clone();
    let drag_tasks = tasks.clone();
    std::thread::Builder::new()
        .name("dsa-app-window-drags".into())
        .spawn(move || {
            ask_drags(
                move || {
                    Box::new(UnixLocalConnector {
                        path: ask_with.clone(),
                    }) as Box<dyn Connector>
                },
                left,
                drag_tasks,
                |window, offer, client| on_main(move || start_drag(window, offer, client)),
            )
        })
        .map_err(|e| format!("the drags out of the windows could not start: {e}"))?;

    STATE.with(|state| {
        let mut state = state.borrow_mut();
        state.reports = Some(reports);
        state.input = Some(input);
        state.files = Some(files);
        state.drags = Some(drags);
    });

    std::thread::Builder::new()
        .name("dsa-app-window-messages".into())
        .spawn(move || loop {
            let received = match connection.recv(None) {
                Ok(received) => received,
                Err(_) => {
                    // The window manager went away: so do the windows.
                    on_main(quit);
                    return;
                }
            };
            let Ok(envelope) = decode(&received) else {
                continue;
            };
            let Some(body) = envelope.body else { continue };
            match body {
                Body::FrameTexture(t) => {
                    on_main(move || texture(t.task_id, t.texture_id, t.handle))
                }
                Body::Frame(f) => {
                    on_main(move || frame(f.task_id, f.texture_id, f.sequence, (f.width, f.height)))
                }
                body => {
                    if let Some(message) = to_host_from_wire(&body) {
                        // Before the windows act on it, so what they send next goes where it
                        // now belongs.
                        tasks.follow(&message);
                        on_main(move || handle(message));
                    }
                }
            }
        })
        .map_err(|e| format!("the window manager's messages could not be read: {e}"))?;

    // The menu bar, with the Android window menu's entries in its Window menu. New Window
    // shows once the window manager says the app supports several windows.
    if let Err(why) = menu_bar::install(false, menu_chosen, has_front_task) {
        eprintln!("dsa-app-window: the menu bar is incomplete: {why}");
    }
    // The Dock menu, with the app's shortcuts.
    if let Err(why) = super::dock_menu::install(&setup.package, setup.host_api.clone()) {
        eprintln!("dsa-app-window: the Dock menu has no shortcuts: {why}");
    }

    unsafe {
        let app_class = class(c"NSApplication")?;
        let app: *mut AnyObject = msg_send![app_class, sharedApplication];
        // A regular app: in the Dock, with a menu bar, taking the keyboard.
        let _: bool = msg_send![app, setActivationPolicy: 0isize];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];
        let _: () = msg_send![app, run];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::host_api::{UserAndroid, UserAndroids};
    use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
    use dsa_host_core::input::{PointerAction, PointerEvent, PointerTool};
    use dsa_transport::local::{self, MemoryLocalConnector, Peer};
    use dsa_transport::{Connection, TransportError};

    const USER: &str = "test-user";

    /// Connects to the in-memory local API as [`USER`].
    struct AsUser(MemoryLocalConnector);

    impl Connector for AsUser {
        fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
            self.0.connect_as(Peer {
                user: USER.into(),
                administrator: false,
            })
        }
    }

    /// The in-memory service with the user's fake Android running.
    fn service() -> (RunningService, MemoryLocalConnector, Arc<FakeAndroid>) {
        let (listener, connector) = local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let _ = androids.android(USER);
        let android = androids.of(USER).expect("the user's Android");
        android.start();
        let service = RunningService::start(Box::new(listener), androids);
        (service, connector, android)
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// After a GPU reset the app's new task 14 took over the window opened for task 9.
    fn taken_over() -> WindowTasks {
        let tasks = WindowTasks::new();
        tasks.follow(&ToHost::ShownTask {
            task_id: 9,
            shown_task_id: 14,
        });
        tasks
    }

    #[test]
    fn nfr5_input_in_a_window_taken_over_after_a_gpu_reset_reaches_the_new_task() {
        let (_service, connector, android) = service();
        let (events, received) = channel();
        let tasks = taken_over();
        let thread = std::thread::spawn(move || serve_input(AsUser(connector), received, tasks));
        let tap = PointerEvent::new(9, PointerAction::Down, PointerTool::Touch, (5.0, 6.0));
        events.send(InputEvent::Pointer(tap)).unwrap();
        for key in back_keys(9, 1234) {
            events.send(InputEvent::Key(key)).unwrap();
        }
        events
            .send(InputEvent::Ime(dsa_host_core::ime::ImeEvent::Commit {
                task_id: 9,
                text: "a".into(),
            }))
            .unwrap();
        wait_until("the input to reach Android", || {
            android.pointers.lock().unwrap().len() == 1
                && android.keys.lock().unwrap().len() == 2
                && android.text.lock().unwrap().len() == 1
        });
        assert_eq!(android.pointers.lock().unwrap()[0].task_id, 14);
        assert!(android
            .keys
            .lock()
            .unwrap()
            .iter()
            .all(|key| key.task_id == 14));
        assert_eq!(android.text.lock().unwrap()[0].task_id(), 14);
        drop(events);
        thread.join().unwrap();
    }

    #[test]
    fn nfr5_a_drag_out_of_a_window_taken_over_after_a_gpu_reset_asks_the_new_task() {
        // The fake Android has something to drag out of task 7 only.
        let (_service, connector, _android) = service();
        let tasks = WindowTasks::new();
        tasks.follow(&ToHost::ShownTask {
            task_id: 3,
            shown_task_id: 7,
        });
        let (drags, left) = channel();
        drags
            .send(DragOut {
                task_id: 3,
                x: 1.0,
                y: 2.0,
                tool: dsa_protocol::control::PointerTool::Mouse as i32,
                pointer_id: 0,
            })
            .unwrap();
        drop(drags);
        let offers: Arc<Mutex<Vec<(i32, usize)>>> = Arc::default();
        let seen = Arc::clone(&offers);
        ask_drags(
            move || Box::new(AsUser(connector.clone())) as Box<dyn Connector>,
            left,
            tasks,
            move |window, offer, _| seen.lock().unwrap().push((window, offer.items.len())),
        );
        // Asked of task 7, and offered to the window the press left, by its own ID.
        assert_eq!(*offers.lock().unwrap(), vec![(3, 2)]);
    }
}
