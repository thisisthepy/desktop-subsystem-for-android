//! The app window host's task windows: native top-level windows, one per task of the app.
//!
//! All the windows live on one UI thread, which runs a message loop. The connection thread
//! reaches it through a [`UiHandle`] with what the window manager says. A hidden message-only
//! window receives the wake-ups, so they arrive even while Windows runs its own loop for a
//! live resize. What the user does to a window goes back to the window manager as a
//! [`FromHost`] report; even a close only reports, and the window closes when the window
//! manager says so. Each window hands its mouse, touch and pen messages to its
//! `PointerWindow` ([`super::pointer`]), which sends the task's pointer events on, its key
//! messages to its `KeyWindow` ([`super::keys`]), which sends its key events on the same way,
//! and its typed characters to its `ImeWindow` ([`super::ime`]), whose text services document
//! the input method composes in: it sends the task's text on and places the input method's
//! windows at the cursor the guest reports.
//!
//! A window reports its client size, its monitor's scale (once when it opens, then on every
//! move to another monitor) and the start and end of a drag; the window manager turns them
//! into the task's size and density. It never grows past the size the window manager says
//! Android can draw, not even when maximised: on a monitor larger than that, a maximised
//! window takes that size, centred on the work area, so the task stays 1:1, and it never
//! shrinks below the smallest size an Android task is given. Its size changes only together
//! with a frame of that size: a size the user drags to goes to the guest, the window keeps the
//! size of the frame it shows until the app has drawn a new one, and then takes that frame's
//! size and shows it in the same commit. It follows the mouse as fast as the app draws, and a
//! frame is never stretched, and never cropped or shown with a margin while the user resizes.
//! The one size it takes before a frame of it is the one Windows suggests when the window
//! moves to a monitor of another scale: kept at the old size, the window would be moved back
//! to the monitor it came from, and its scale and size would flap. Until the app draws the new
//! size, it shows its last frame 1:1.
//!
//! A window also follows how its app asks to be shown ([`ModeState`]): an app that fills the
//! screen gets a window that covers its monitor with no title bar, and one in
//! picture-in-picture a small window in the corner of the work area that stays above the
//! others. F11 turns full screen on and off. Each change of mode proposes the new size like any
//! other, so the window takes it together with a frame of that size. A window the user's
//! settings for the app make fixed in size cannot be resized or maximized by the user; it
//! still takes the sizes Android gives it.
//!
//! Each window's system menu carries the Android window menu after Windows' own items
//! ([`super::system_menu`]), and is shown as a Windows 11 menu rather than the classic one
//! ([`super::window_menu`]). Its Screenshot is taken here ([`super::screenshot`]); every other
//! item is reported to the window manager, which carries it out.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use dsa_host_core::compositor::caption::{BandSample, CaptionBand};
use dsa_host_core::compositor::task_windows::fit_within;
use dsa_host_core::compositor::window_hosts::{FromHost, ToHost};
use dsa_host_core::compositor::window_mode::{picture_in_picture_place, ModeState, WindowMode};
use dsa_host_core::compositor::window_sizing::{
    maximized_outer, maximized_position, min_client_size, rescaled_rect, to_task_size,
};
use dsa_host_core::compositor::window_tasks::WindowTasks;
use dsa_host_core::input::InputEvent;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_F11;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed, KillTimer, LoadCursorW, LoadIconW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA,
    HWND_MESSAGE, IDC_ARROW, MINMAXINFO, MSG, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SW_SHOW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WA_INACTIVE, WINDOWPOS,
    WINDOW_EX_STYLE, WINDOW_STYLE, WMSZ_BOTTOMLEFT, WMSZ_LEFT, WMSZ_TOP, WMSZ_TOPLEFT,
    WMSZ_TOPRIGHT, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND,
    WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_GETOBJECT, WM_NCCREATE, WM_NCDESTROY, WM_PAINT,
    WM_SETTINGCHANGE, WM_SIZE, WM_SIZING, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WM_TIMER,
    WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_EX_APPWINDOW,
    WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowPlacement, SetWindowPlacement, GWL_STYLE, HTBORDER, HTBOTTOMRIGHT,
    HTLEFT, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, SC_MAXIMIZE, SC_SIZE, SWP_FRAMECHANGED,
    SWP_NOOWNERZORDER, SW_RESTORE, WINDOWPLACEMENT, WM_COMMAND, WM_KEYDOWN, WM_KEYUP, WM_NCHITTEST,
    WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP, WS_POPUP,
};

use crate::compositor::log::{input_trace, input_trace_on};
use crate::compositor::log::{note, CompositorLogger};
use crate::compositor::present::{self, Gpu, WindowPresenter};
use crate::compositor::window_marks;

use super::accessibility::{AccessibilityWindow, ApiConnect, WM_APP_ACCESSIBILITY};
use super::caption::{task_frame, CaptionWindow};
use super::drag_source::{self, DragStarter, WM_APP_DRAG_OUT};
use super::drop_target::{self, DropHandler};
use super::ime::ImeWindow;
use super::keys::KeyWindow;
use super::pointer::PointerWindow;
use super::system_menu::{self, SystemMenuItem};
use super::tsf;
use super::window_menu::{self, MenuMessage};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, SendMessageW, SC_CLOSE};

/// A task window that is open, as tests and logs see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    pub task_id: i32,
    /// The window's handle, as an integer.
    pub hwnd: isize,
    /// Where the window places the input method's windows: the cursor of the task's focused
    /// editor, as the guest last reported it.
    pub ime_cursor: Option<dsa_host_core::ime::ImeCursor>,
    /// The guest's focused editor as the window's input method reads it, as the guest last
    /// described it.
    pub ime_editor: Option<dsa_host_core::ime::ImeEditor>,
}

/// Where a report about a window goes: to the window manager.
pub type Report = Arc<dyn Fn(FromHost) + Send + Sync>;

/// The windows' presenters by task, which the connection thread shows frames through.
pub type Presenters = Arc<Mutex<HashMap<i32, Arc<WindowPresenter>>>>;

const WM_APP_COMMANDS: u32 = WM_APP + 1;
const WM_APP_QUIT: u32 = WM_APP + 2;
/// Sent to a task window when a frame of another size than the window's is kept for it.
const WM_APP_FRAME_READY: u32 = WM_APP + 4;
/// Sent to a task window when what its presenter reads from the app's frame under the title
/// bar changes ([`pack_band_sample`]).
const WM_APP_BAND_COLOUR: u32 = WM_APP + 5;

/// A band sample as the parameters of [`WM_APP_BAND_COLOUR`]: the fill color in `wparam`, and
/// in `lparam` bit 0 set when there is a fill, bit 1 set when the band's luminance was read,
/// and the luminance itself from bit 16.
fn pack_band_sample(sample: BandSample) -> (WPARAM, LPARAM) {
    let wparam = sample.fill.unwrap_or(0) as WPARAM;
    let mut lparam = LPARAM::from(sample.fill.is_some());
    if let Some(luminance) = sample.luminance {
        lparam |= 2 | (luminance as LPARAM) << 16;
    }
    (wparam, lparam)
}

/// The band sample [`pack_band_sample`] put in a message's parameters.
fn unpack_band_sample(wparam: WPARAM, lparam: LPARAM) -> BandSample {
    BandSample {
        fill: (lparam & 1 != 0).then_some(wparam as u32),
        luminance: (lparam & 2 != 0).then_some(((lparam >> 16) & 0xFFFF) as u16),
    }
}

/// The message window's timer, which reports windows left showing a frame of an older size
/// and takes the commands whose wake-up message could not be posted.
const TICK_TIMER: usize = 1;
const TICK_MILLIS: u32 = 200;

const TASK_CLASS: &str = window_marks::TASK_WINDOW_CLASS;
const MESSAGE_CLASS: &str = "DsaAppWindowMessages";
const TASK_STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
// Without a redirection surface, everything in the client area comes from the visual.
const TASK_EX_STYLE: WINDOW_EX_STYLE = WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW;

/// The resource ID of the program's icon, which the windows show.
const ICON_ID: usize = 1;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .filter(|unit| *unit != 0)
        .chain(std::iter::once(0))
        .collect()
}

/// How the connection thread reaches the UI thread, and how to stop it.
pub struct UiHandle {
    message_window: isize,
    commands: Sender<ToHost>,
    thread: Option<JoinHandle<()>>,
}

impl UiHandle {
    /// Hands `command` to the UI thread. Returns false once the UI thread has ended.
    ///
    /// Only the end of the UI thread ends the program. A wake-up that cannot be posted (the
    /// UI thread's queue is full while it is busy, serving a UI Automation client, say) does
    /// not: the command waits in the channel, and the UI thread's timer takes it within
    /// `TICK_MILLIS`. Ending here would close every window of the app, and Android would
    /// end their tasks.
    pub fn send(&self, command: ToHost) -> bool {
        if self.commands.send(command).is_err() {
            return false;
        }
        // SAFETY: posting to a window of another thread; a stale handle only fails.
        unsafe { PostMessageW(self.message_window as HWND, WM_APP_COMMANDS, 0, 0) };
        true
    }

    /// Closes every window and ends the UI thread.
    pub fn stop(mut self) {
        self.stop_thread();
    }

    fn stop_thread(&mut self) {
        // SAFETY: posting to a window of another thread; a stale handle only fails.
        unsafe {
            PostMessageW(self.message_window as HWND, WM_APP_QUIT, 0, 0);
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for UiHandle {
    fn drop(&mut self) {
        self.stop_thread();
    }
}

/// What the UI thread is given to start.
pub struct UiSetup {
    /// Where reports about the windows go.
    pub report: Report,
    /// Where the pointer, key and text input in every task window goes.
    pub input: Sender<InputEvent>,
    /// The GPU devices, or `None` when frames cannot be shown (the windows still open).
    pub gpu: Option<Arc<Gpu>>,
    /// Where the UI thread keeps each window's presenter.
    pub presenters: Presenters,
    pub logger: Option<Arc<CompositorLogger>>,
    /// Where the UI thread lists the windows that are open, in task ID order.
    pub listing: Arc<Mutex<Vec<OpenWindow>>>,
    /// Dials the service's local API to watch a task's accessibility tree, while a UI
    /// Automation client reads its window. `None` answers clients with an empty tree.
    pub accessibility: Option<ApiConnect>,
    /// The task each window shows, which a window's accessibility tree is watched for.
    pub tasks: WindowTasks,
    /// What happens to files dropped on a task window. `None` takes no drops.
    pub drops: Option<DropHandler>,
    /// Asks whether an app drags out of a task window a press has left. `None` starts no
    /// drags on the host.
    pub drags: Option<DragStarter>,
}

/// Starts the UI thread. Returns once its message window exists.
pub fn start(setup: UiSetup) -> Result<UiHandle, String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (commands_tx, commands_rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("dsa-app-window-ui".into())
        .spawn(move || run(setup, commands_rx, ready_tx))
        .map_err(|e| format!("could not start the window thread: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(message_window)) => Ok(UiHandle {
            message_window,
            commands: commands_tx,
            thread: Some(thread),
        }),
        Ok(Err(why)) => {
            let _ = thread.join();
            Err(why)
        }
        Err(_) => {
            let _ = thread.join();
            Err("the window thread ended before it was ready".into())
        }
    }
}

/// Kept in each task window's GWLP_USERDATA.
struct WindowData {
    task_id: i32,
    report: Report,
    presenter: RefCell<Option<Arc<WindowPresenter>>>,
    pointer: PointerWindow,
    keys: KeyWindow,
    ime: ImeWindow,
    /// The task's UI Automation tree, for screen readers and other accessibility clients.
    accessibility: RefCell<AccessibilityWindow>,
    /// The title bar the window draws over the app's top, and its buttons.
    caption: CaptionWindow,
    /// The largest client area Android can draw for the task, as the window manager last
    /// said, for WM_GETMINMAXINFO.
    max_client: Cell<Option<(u32, u32)>>,
    /// The smallest size Android gives the task, as the window manager last said, for
    /// WM_GETMINMAXINFO.
    min_client: Cell<Option<(u32, u32)>>,
    /// How large the window shows each of its task's pixels (window pixels per task pixels),
    /// as the window manager last said: 1:1 unless the task keeps its density on a monitor of
    /// another scale.
    scale: Cell<(u32, u32)>,
    /// Whether the window reports its size: not until it has been given the task's.
    reporting: Cell<bool>,
    /// Whether the window is minimized, so a restore is reported once.
    minimized: Cell<bool>,
    /// Which edges the user drags (left, top), so a window that takes the size of a frame
    /// keeps the other edges where they are.
    drag_edges: Cell<(bool, bool)>,
    /// Whether the window is taking the size of a frame, which the guest is not asked for.
    following: Cell<bool>,
    /// What the app asked for and the user's F11 over it.
    mode: Cell<ModeState>,
    /// The mode the window is shown in now.
    shown_mode: Cell<WindowMode>,
    /// Whether the user may resize the window, as the user's settings for the app say.
    resizable: Cell<bool>,
    /// Where an ordinary window was before it filled its monitor or went into
    /// picture-in-picture, for when it comes back.
    restore: Cell<Option<WINDOWPLACEMENT>>,
    /// Whether the window is sizing itself (for Android, or for a change of mode) rather than
    /// being sized by the user.
    self_sizing: Cell<bool>,
    /// Whether the window is taking the size the desktop suggests for its monitor's new scale,
    /// which it takes at once rather than wait for a frame of it.
    rescaling: Cell<bool>,
    /// Where a screenshot that failed says why.
    logger: Option<Arc<CompositorLogger>>,
    /// The icons the window shows in place of its program's, once the window manager sent
    /// the app's new icon; kept while the window shows them.
    icons: RefCell<Option<super::live_icon::WindowIcons>>,
    /// The media buttons in the window's taskbar preview, while the app has a media session.
    thumb_bar: super::thumb_bar::ThumbBar,
}

/// Shows the app's media session on the media buttons of `hwnd`'s taskbar preview, and logs
/// why when the taskbar does not take them.
///
/// # Safety
///
/// `hwnd` is a live task window of the calling thread, and `data` its data.
unsafe fn sync_thumb_bar(hwnd: HWND, data: &WindowData, step: ThumbBarStep) {
    let result = match step {
        ThumbBarStep::Created => data.thumb_bar.button_created(hwnd as isize),
        ThumbBarStep::Changed => data.thumb_bar.update(hwnd as isize),
        ThumbBarStep::Restyled => data.thumb_bar.restyle(hwnd as isize),
    };
    if let Err(why) = result {
        note(data.logger.as_deref(), &why);
    }
}

/// What the media buttons of a window's taskbar preview follow.
#[derive(Clone, Copy)]
enum ThumbBarStep {
    /// The taskbar made the window's button.
    Created,
    /// The app's media session changed.
    Changed,
    /// The window's DPI or the theme changed.
    Restyled,
}

/// Gives the window the app's icon the window manager sent, if it sent one, at the sizes of
/// the window's DPI, and with `repaint` draws its title bar again with it (a window that is
/// still opening draws it when it first paints).
///
/// # Safety
///
/// `hwnd` is a live task window of the calling thread, and `data` its data.
unsafe fn take_live_icon(hwnd: HWND, data: &WindowData, repaint: bool) {
    let Some(icon) = super::live_icon::current() else {
        return;
    };
    if let Some(icons) = super::live_icon::apply(hwnd, &icon.icon_png) {
        // The icons the window showed until now are destroyed only after it let go of them.
        data.icons.replace(Some(icons));
    }
    if repaint {
        let presenter = data.presenter.borrow().clone();
        data.caption.repaint(hwnd, presenter.as_deref());
    }
}

/// The UI thread's state, reached from the message window's procedure.
struct Ui {
    open: HashMap<i32, HWND>,
    commands: Receiver<ToHost>,
    setup: UiSetup,
    /// Whether the windows' system menu offers New window, as the window manager last said.
    new_window: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn register_class(
    name: &str,
    procedure: unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT,
) -> Result<(), String> {
    let name = wide(name);
    // SAFETY: the class structure and its name live through the call.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        // The program's own icon: the app's, in an app's copy of this program.
        let icon = LoadIconW(module, ICON_ID as *const u16);
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: icon,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
            hIconSm: icon,
        };
        if RegisterClassExW(&class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return Err(format!(
                "could not register the window class: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn run(setup: UiSetup, commands: Receiver<ToHost>, ready: Sender<Result<isize, String>>) {
    // SAFETY: plain Win32 calls on this thread's own windows.
    unsafe {
        // Sizes are in real pixels on every monitor; a window that moves to a monitor of
        // another scale is told so (WM_DPICHANGED) instead of being stretched by Windows.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if let Err(why) = register_class(TASK_CLASS, task_window_proc)
            .and_then(|()| register_class(MESSAGE_CLASS, message_window_proc))
        {
            let _ = ready.send(Err(why));
            return;
        }
        let class = wide(MESSAGE_CLASS);
        let message_window = CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            HWND_MESSAGE,
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            std::ptr::null(),
        );
        if message_window.is_null() {
            let _ = ready.send(Err(format!(
                "could not create the window host's message window: {}",
                std::io::Error::last_os_error()
            )));
            return;
        }
        UI.with(|ui| {
            *ui.borrow_mut() = Some(Ui {
                open: HashMap::new(),
                commands,
                setup,
                new_window: false,
            });
        });
        let _ = ready.send(Ok(message_window as isize));
        SetTimer(message_window, TICK_TIMER, TICK_MILLIS, None);
        let takes_drops = UI.with(|ui| {
            ui.borrow()
                .as_ref()
                .is_some_and(|ui| ui.setup.drops.is_some())
        });
        if takes_drops {
            if let Err(why) = drop_target::prepare_thread() {
                UI.with(|ui| {
                    if let Some(ui) = ui.borrow_mut().as_mut() {
                        ui.setup.drops = None;
                        ui.log(&why);
                    }
                });
            }
        }
        // The input methods compose in the windows through the text services framework,
        // which runs on this thread for the windows it opens.
        if let Err(why) = tsf::start_thread() {
            UI.with(|ui| {
                if let Some(ui) = ui.borrow().as_ref() {
                    ui.log(&why);
                }
            });
        }

        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        let ui = UI.with(|ui| ui.borrow_mut().take());
        if let Some(mut ui) = ui {
            let open: Vec<i32> = ui.open.keys().copied().collect();
            for task_id in open {
                ui.close(task_id);
            }
            ui.publish();
        }
        tsf::end_thread();
        KillTimer(message_window, TICK_TIMER);
        DestroyWindow(message_window);
    }
}

unsafe extern "system" fn message_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_APP_COMMANDS => {
            let handled = UI.with(|ui| match ui.try_borrow_mut() {
                Ok(mut ui) => {
                    if let Some(ui) = ui.as_mut() {
                        ui.drain_commands();
                    }
                    true
                }
                Err(_) => false,
            });
            if !handled {
                // Reached from inside the UI's own work; try again once it is done.
                PostMessageW(hwnd, message, wparam, lparam);
            }
            0
        }
        WM_APP_QUIT => {
            PostQuitMessage(0);
            0
        }
        WM_TIMER => {
            UI.with(|ui| {
                // Commands whose wake-up could not be posted are taken here.
                if let Ok(mut ui) = ui.try_borrow_mut() {
                    if let Some(ui) = ui.as_mut() {
                        ui.drain_commands();
                    }
                }
                if let Ok(ui) = ui.try_borrow() {
                    if let Some(ui) = ui.as_ref() {
                        ui.tick();
                    }
                }
            });
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

unsafe fn window_data<'a>(hwnd: HWND) -> Option<&'a WindowData> {
    let data = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowData;
    data.as_ref()
}

/// The registered message dsa tap sends before a press it injects through the local API.
fn trace_press_message() -> u32 {
    thread_local! {
        static MESSAGE: u32 = window_marks::trace_press_message();
    }
    MESSAGE.with(|message| *message)
}

/// A press is coming at the point in lparam, in guest pixels, for the task in wparam.
/// While the input trace is on, the press is traced as one that came through the window, and
/// the window watches the pixels around it. Answers 1 when the window is the task's and
/// traces it.
unsafe fn trace_press(hwnd: HWND, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let Some(data) = window_data(hwnd) else {
        return 0;
    };
    if wparam as i32 != data.task_id || !input_trace_on() {
        return 0;
    }
    let (x, y) = window_marks::unpack_point(lparam);
    input_trace(
        "app.pointer",
        data.task_id,
        &format!("phase=Down source=dsa-tap x={x} y={y}"),
    );
    present::pressed(data.task_id, (x as f32, y as f32));
    1
}

unsafe extern "system" fn task_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(data) = window_data(hwnd) {
        // The taskbar made the window's button: its preview gets the media buttons.
        let created = super::thumb_bar::button_created_message();
        if created != 0 && message == created {
            sync_thumb_bar(hwnd, data, ThumbBarStep::Created);
            return 0;
        }
        // A click on a media button of the preview goes to the app's media session.
        if message == WM_COMMAND {
            if let Some(id) = super::thumb_bar::clicked(wparam) {
                let media = super::thumb_bar::current();
                if let Some(button) = super::thumb_bar::pressed(id, media.as_ref()) {
                    (data.report)(FromHost::MediaButton {
                        task_id: data.task_id,
                        button,
                    });
                }
                return 0;
            }
        }
        // The Android items of the system menu; Windows' own commands go on below.
        if message == WM_SYSCOMMAND {
            if let Some(item) = system_menu::chosen(wparam) {
                menu_chosen(hwnd, data, item);
                return 0;
            }
        }
        if let Some(result) = mode_message(hwnd, data, message, wparam, lparam) {
            return result;
        }
        // The title bar first: what lies in it is the window's, never the app's. A window that
        // fills its monitor has none.
        let presenter = data.presenter.borrow().clone();
        if data.shown_mode.get() != WindowMode::Fullscreen {
            // The window menu: a click on the icon, a right click on the title bar, Alt+Space.
            let hit = |x: i32, y: i32| {
                let at = ((y as u16 as LPARAM) << 16) | (x as u16 as LPARAM);
                SendMessageW(hwnd, WM_NCHITTEST, 0, at) as u32
            };
            if let Some(what) = window_menu::menu_message(message, wparam, lparam, &hit) {
                match what {
                    MenuMessage::Open(opening) => open_window_menu(hwnd, data, opening),
                    // Posted, as Windows posts it for a double click on a window's icon.
                    MenuMessage::Close => {
                        PostMessageW(hwnd, WM_SYSCOMMAND, SC_CLOSE as WPARAM, 0);
                    }
                    MenuMessage::Swallow => {}
                }
                return 0;
            }
            if let Some(result) =
                data.caption
                    .handle(hwnd, message, wparam, lparam, presenter.as_deref())
            {
                return result;
            }
        }
        if let Some(result) = data.pointer.handle(hwnd, message, wparam, lparam) {
            return result;
        }
        if let Some(result) = data.keys.handle(hwnd, message, wparam, lparam) {
            return result;
        }
        if let Some(result) = data.ime.handle(hwnd, message, wparam, lparam) {
            return result;
        }
    }
    if message == trace_press_message() {
        return trace_press(hwnd, wparam, lparam);
    }
    if message == WM_APP_DRAG_OUT {
        // An app's drag out of this window: the host's drag runs here until it is dropped.
        drag_source::run_pending(lparam);
        return 0;
    }
    match message {
        WM_NCCREATE => {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_SIZE => {
            // Minimizing hides the window, and restoring it shows it again: the window manager
            // lets Android sleep once no window is shown.
            if let Some(data) = window_data(hwnd) {
                let minimized = wparam as u32 == SIZE_MINIMIZED;
                // A minimized window lets its swapchain go, and a restored one shows its
                // newest frame again before the frame of its size goes up below.
                if let Some(presenter) = data.presenter.borrow().clone() {
                    if minimized {
                        presenter.hide();
                    } else {
                        presenter.show();
                    }
                }
                if data.reporting.get() && data.minimized.replace(minimized) != minimized {
                    (data.report)(FromHost::Shown {
                        task_id: data.task_id,
                        shown: !minimized,
                    });
                    if minimized {
                        crate::compositor::trim::empty_working_set();
                    }
                }
            }
            if wparam as u32 != SIZE_MINIMIZED {
                let width = (lparam & 0xffff) as u32;
                let height = ((lparam >> 16) & 0xffff) as u32;
                if let Some(data) = window_data(hwnd) {
                    if let Some(presenter) = data.presenter.borrow().clone() {
                        // The frame of this size goes up with it, in the same commit. The size
                        // was reported when it was proposed (WM_WINDOWPOSCHANGING).
                        presenter.view_resized((width, height));
                    }
                    // The buttons stay at the right end, and a maximized window's middle
                    // button restores it.
                    if titled(data) {
                        data.caption.paint(hwnd, data.presenter.borrow().as_deref());
                    }
                    // A narrow window has no room for the icon's cell, and so no back arrow.
                    sync_back_button(hwnd, data);
                }
            }
            0
        }
        WM_ENTERSIZEMOVE | WM_EXITSIZEMOVE => {
            if let Some(data) = window_data(hwnd) {
                if message == WM_ENTERSIZEMOVE {
                    data.drag_edges.set((false, false));
                }
                (data.report)(FromHost::Dragging {
                    task_id: data.task_id,
                    dragging: message == WM_ENTERSIZEMOVE,
                });
            }
            0
        }
        WM_SIZING => {
            // Which edge the user drags, for when the window later takes the size of a frame.
            if let Some(data) = window_data(hwnd) {
                let edge = wparam as u32;
                data.drag_edges.set((
                    matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT),
                    matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT),
                ));
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_APP_FRAME_READY => {
            if let Some(data) = window_data(hwnd) {
                take_kept_frame(hwnd, data);
            }
            0
        }
        WM_APP_BAND_COLOUR => {
            // The app's frames show whether it leaves the title bar's band empty, and how
            // light the band is.
            if let Some(data) = window_data(hwnd) {
                let sample = unpack_band_sample(wparam, lparam);
                let presenter = data.presenter.borrow().clone();
                data.caption.set_sample(hwnd, sample, presenter.as_deref());
            }
            0
        }
        WM_GETMINMAXINFO => {
            let Some(data) = window_data(hwnd) else {
                return DefWindowProcW(hwnd, message, wparam, lparam);
            };
            let dpi = GetDpiForWindow(hwnd).max(96);
            let info = &mut *(lparam as *mut MINMAXINFO);
            let max = data.max_client.get();
            // Never larger than Android can draw: a larger window could only show the
            // task's frames with an empty margin. Maximising takes this size too.
            if let Some(max) = max {
                // Windows keeps every size it applies within the tracking sizes, a maximized
                // window's too, so they are measured with the borders the window has now.
                let (width, height) = window_outer(hwnd, max, dpi);
                info.ptMaxTrackSize = POINT {
                    x: width.min(info.ptMaxTrackSize.x),
                    y: height.min(info.ptMaxTrackSize.y),
                };
                // A maximized window keeps a top border off the screen as well. Windows
                // proposes the maximised size of the primary monitor and adjusts it to this
                // window's monitor only while it is at least that large, so the guest's
                // smaller largest size is kept to this monitor's own work area here, or on a
                // monitor smaller than the primary one the window would maximise past its
                // screen's edge (maximized_outer).
                let guest = outer_size(max, dpi, true);
                let (width, height) = match window_work_area(hwnd) {
                    Some(work) => {
                        let (frame_width, frame_height) = outer_size((0, 0), dpi, true);
                        maximized_outer(
                            guest,
                            (
                                work.right - work.left + frame_width,
                                work.bottom - work.top + frame_height,
                            ),
                        )
                    }
                    None => (guest.0.min(info.ptMaxSize.x), guest.1.min(info.ptMaxSize.y)),
                };
                info.ptMaxSize = POINT {
                    x: width,
                    y: height,
                };
            }
            // Never smaller than Android gives this task, which its app may declare: the guest
            // would keep the task larger than the window, and no frame could show 1:1.
            let min = min_client_size(dpi, max, data.min_client.get());
            let (width, height) = window_outer(hwnd, min, dpi);
            info.ptMinTrackSize = POINT {
                x: width.max(info.ptMinTrackSize.x),
                y: height.max(info.ptMinTrackSize.y),
            };
            0
        }
        WM_WINDOWPOSCHANGING => {
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            let pos = &mut *(lparam as *mut WINDOWPOS);
            // A window fixed in size takes no size the user proposes (a snap, say); the sizes
            // it gives itself, for Android or for a change of mode, still apply.
            if let Some(data) = window_data(hwnd) {
                let user = !data.self_sizing.get() && !data.following.get();
                let normal = data.shown_mode.get() == WindowMode::Normal;
                if !data.resizable.get() && user && normal {
                    pos.flags |= SWP_NOSIZE;
                }
            }
            // DefWindowProc above has already kept the proposed size to WM_GETMINMAXINFO.
            if pos.flags & SWP_NOSIZE == 0 && IsIconic(hwnd) == 0 {
                if let Some(data) = window_data(hwnd) {
                    keep_to_frame(hwnd, data, pos);
                }
            }
            // A maximised window takes the most Android can draw (WM_GETMINMAXINFO above),
            // shown 1:1. On a monitor larger than that it is centred on the work area rather
            // than left in its top left corner. One that fills the work area has its client
            // area at the work area's corner, whatever position was proposed: the window
            // keeps its smaller frame's size, centred, until the guest draws the larger one,
            // and the position it takes that frame at is that centred one.
            if IsZoomed(hwnd) != 0 && pos.flags & (SWP_NOMOVE | SWP_NOSIZE) == 0 {
                let proposed = RECT {
                    left: pos.x,
                    top: pos.y,
                    right: pos.x + pos.cx,
                    bottom: pos.y + pos.cy,
                };
                if let Some(work) = work_area(&proposed) {
                    let (left, top) = maximized_border(hwnd);
                    pos.x = maximized_position(work.left, work.right - work.left, pos.cx, left);
                    pos.y = maximized_position(work.top, work.bottom - work.top, pos.cy, top);
                }
            }
            result
        }
        WM_WINDOWPOSCHANGED => {
            // Sends WM_SIZE, where the window takes a new size with its frame. A size it chose
            // and did not get no longer holds frames back.
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            if let Some(presenter) =
                window_data(hwnd).and_then(|data| data.presenter.borrow().clone())
            {
                presenter.release_claim();
            }
            result
        }
        WM_DPICHANGED => {
            // The scale goes first, so the task gets its new density before the size that
            // goes with it, which the resize below reports.
            let dpi = (wparam & 0xffff) as u32;
            let data = window_data(hwnd);
            if let Some(data) = data {
                (data.report)(FromHost::Dpi {
                    task_id: data.task_id,
                    dpi,
                });
                // The title bar's height follows the scale, and the task's inset with it.
                if titled(data) {
                    data.caption.report_height(hwnd, &*data.report);
                }
                // A splash still up keeps its icon sharp and its size on the new monitor.
                if let Some(presenter) = data.presenter.borrow().as_deref() {
                    presenter.splash_dpi_changed(dpi.max(96));
                }
                // The preview's media buttons are drawn again at the new scale.
                sync_thumb_bar(hwnd, data, ThumbBarStep::Restyled);
                // The app's icon the window manager sent is drawn again at the new scale.
                if data.icons.borrow().is_some() {
                    if let Some(icon) = super::live_icon::current() {
                        if let Some(icons) = super::live_icon::apply(hwnd, &icon.icon_png) {
                            data.icons.replace(Some(icons));
                        }
                    }
                }
            }
            // A per-monitor aware window is handed the rectangle that keeps its size in
            // points on the new monitor, and takes it at once (keep_to_frame), within what
            // Android can draw and the new monitor's work area. The guest's display can be
            // larger than any monitor, and a window larger than its monitor reaches onto the
            // next one, which can change its scale again.
            let suggested = &*(lparam as *const RECT);
            let zoomed = IsZoomed(hwnd) != 0;
            let mut limit = (i32::MAX, i32::MAX);
            if let Some(max) = data.and_then(|data| data.max_client.get()) {
                limit = outer_size(max, dpi.max(96), zoomed);
            }
            // A maximized window is kept to its monitor by Windows, and one that fills its
            // monitor covers more than the work area.
            if !zoomed && data.is_some_and(titled) {
                if let Some(work) = work_area(suggested) {
                    limit = (
                        limit.0.min(work.right - work.left),
                        limit.1.min(work.bottom - work.top),
                    );
                }
            }
            let (x, y, width, height) = rescaled_rect(
                (
                    suggested.left,
                    suggested.top,
                    suggested.right - suggested.left,
                    suggested.bottom - suggested.top,
                ),
                limit,
            );
            if let Some(data) = data {
                data.self_sizing.set(true);
                data.rescaling.set(true);
            }
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                x,
                y,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            if let Some(data) = data {
                data.rescaling.set(false);
                data.self_sizing.set(false);
                if titled(data) {
                    data.caption.paint(hwnd, data.presenter.borrow().as_deref());
                }
                // The icon's cell follows the scale.
                sync_back_button(hwnd, data);
            }
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            // The taskbar's theme or high contrast may have changed: the preview's media
            // buttons take their colour.
            if let Some(data) = window_data(hwnd) {
                sync_thumb_bar(hwnd, data, ThumbBarStep::Restyled);
            }
            // The caption's size, high contrast and the system colors may have changed.
            if let Some(data) = window_data(hwnd).filter(|data| titled(data)) {
                data.caption.report_height(hwnd, &*data.report);
                data.caption
                    .repaint(hwnd, data.presenter.borrow().as_deref());
                sync_back_button(hwnd, data);
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_ACTIVATE => {
            // The window menu takes the foreground while it is open; the window stays the
            // user's, and the app keeps its focus, as under Windows' own menu.
            let menu_took_it = (wparam & 0xffff) as u32 == WA_INACTIVE
                && window_data(hwnd).is_some_and(|data| data.caption.menu_open());
            if menu_took_it {
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
            if let Some(data) = window_data(hwnd) {
                if titled(data) {
                    data.caption.activated(
                        hwnd,
                        (wparam & 0xffff) as u32 != WA_INACTIVE,
                        data.presenter.borrow().as_deref(),
                    );
                }
                (data.report)(FromHost::Focus {
                    task_id: data.task_id,
                    focused: (wparam & 0xffff) as u32 != WA_INACTIVE,
                });
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        // Never painted white between frames: the visual covers the client area.
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut paint);
            EndPaint(hwnd, &paint);
            0
        }
        WM_CLOSE => {
            // The window manager asks Android to end the task and closes the window.
            if let Some(data) = window_data(hwnd) {
                (data.report)(FromHost::Closed {
                    task_id: data.task_id,
                });
            }
            0
        }
        WM_GETOBJECT => {
            let answered = window_data(hwnd).and_then(|data| {
                let mut tree = data.accessibility.try_borrow_mut().ok()?;
                tree.get_object(hwnd as isize, wparam, lparam)
            });
            match answered {
                Some(result) => result,
                None => DefWindowProcW(hwnd, message, wparam, lparam),
            }
        }
        WM_APP_ACCESSIBILITY => {
            // Taken out of the tree first: UI Automation may call back into the window.
            let target =
                window_data(hwnd).and_then(|data| data.accessibility.try_borrow().ok()?.events());
            if let Some(target) = target {
                target.raise(hwnd as isize);
            }
            0
        }
        WM_NCDESTROY => {
            drop_target::revoke(hwnd as isize);
            if let Some(data) = window_data(hwnd) {
                data.ime.detach();
                if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
                    tree.close(hwnd as isize);
                }
            }
            super::pointer::forget_contacts(hwnd);
            let data = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut WindowData;
            if !data.is_null() {
                drop(Box::from_raw(data));
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

/// Shows `hwnd`'s window menu for `opening` ([`window_menu::show`]), under the icon's cell
/// for a click on the icon or Alt+Space. Once it closes, the title bar and the app's focus
/// follow whichever window has the foreground then.
unsafe fn open_window_menu(hwnd: HWND, data: &WindowData, opening: window_menu::Opening) {
    let layout = data.caption.layout(hwnd);
    let cell = layout.icon_cell().unwrap_or((0, 0, 0, layout.height));
    data.caption.set_menu_open(true);
    window_menu::show(hwnd, opening, cell, data.resizable.get());
    data.caption.set_menu_open(false);
    let active = GetForegroundWindow() == hwnd;
    let presenter = data.presenter.borrow().clone();
    data.caption.activated(hwnd, active, presenter.as_deref());
    if !active {
        (data.report)(FromHost::Focus {
            task_id: data.task_id,
            focused: false,
        });
    }
}

/// The user chose `item` in `hwnd`'s system menu. The screenshot is taken here; the window
/// manager carries out the rest.
unsafe fn menu_chosen(hwnd: HWND, data: &WindowData, item: SystemMenuItem) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY};
    if item == SystemMenuItem::Screenshot {
        match data.presenter.borrow().clone() {
            Some(presenter) => super::screenshot::take(
                presenter,
                hwnd as isize,
                system_menu::display_text().screenshot_file,
                data.logger.clone(),
            ),
            None => note(
                data.logger.as_deref(),
                "the screenshot was not taken: the window shows no frames",
            ),
        }
        return;
    }
    if let Some(action) = system_menu::action(item) {
        // Each of these opens a window in another program (the app's new task, Android's
        // settings, DSA's settings). The user asked for it here, so that window may come to
        // the front rather than only flash on the taskbar.
        AllowSetForegroundWindow(ASFW_ANY);
        (data.report)(FromHost::MenuChosen {
            task_id: data.task_id,
            action,
        });
    }
}

/// A size is proposed for `hwnd` (`pos`, a step of a drag, a maximise, a move to another
/// monitor): the guest is asked for it, and the window takes the size of a frame instead, so
/// it is never shown at a size other than the frame in it. The edges the size change did not
/// move stay where they are.
unsafe fn keep_to_frame(hwnd: HWND, data: &WindowData, pos: &mut WINDOWPOS) {
    let dpi = GetDpiForWindow(hwnd).max(96);
    // The client area this outer size gives, as WM_NCCALCSIZE will lay it out within the same
    // move: a maximized window has a top border too.
    let (frame_width, frame_height) = window_outer(hwnd, (0, 0), dpi);
    let proposed = (
        (pos.cx - frame_width).max(0) as u32,
        (pos.cy - frame_height).max(0) as u32,
    );
    if proposed.0 == 0 || proposed.1 == 0 {
        return;
    }
    // A position change that keeps the window's outer size (a frame or style change, such as
    // the switch to maximized) proposes nothing new: only the client area it implies is kept
    // to the frame below, and the guest is not asked for it.
    let mut current: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut current);
    let same_outer =
        (pos.cx, pos.cy) == (current.right - current.left, current.bottom - current.top);
    // Not the size of a frame the window takes: the guest drew that one already.
    if data.reporting.get() && !data.following.get() && !same_outer {
        (data.report)(FromHost::Resized {
            task_id: data.task_id,
            width: proposed.0,
            height: proposed.1,
        });
    }
    let Some(presenter) = data.presenter.borrow().clone() else {
        return;
    };
    // The size the desktop suggests for a new monitor scale is taken at once. Kept at the old
    // frame's size, the window would be too large or too small for the new monitor by the
    // ratio of the scales, the desktop would find it on the monitor it came from, change its
    // scale back and suggest the size scaled the other way, and the window and its task would
    // flap between the two sizes.
    if data.rescaling.get() {
        presenter.scale_changed(proposed);
        return;
    }
    let Some(size) = presenter.size_for(proposed) else {
        return;
    };
    if size == proposed {
        return;
    }
    let (width, height) = window_outer(hwnd, size, dpi);
    if pos.flags & SWP_NOMOVE == 0 {
        let mut current: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut current);
        if pos.x != current.left {
            pos.x += pos.cx - width;
        }
        if pos.y != current.top {
            pos.y += pos.cy - height;
        }
    }
    pos.cx = width;
    pos.cy = height;
}

/// A frame of another size than the window's is kept for it: the window takes its size, and
/// the frame is shown with it (WM_SIZE). The edges the user is not dragging stay put.
unsafe fn take_kept_frame(hwnd: HWND, data: &WindowData) {
    if IsIconic(hwnd) != 0 {
        return;
    }
    let Some(presenter) = data.presenter.borrow().clone() else {
        return;
    };
    let Some(size) = presenter.kept_size() else {
        return;
    };
    let (width, height) = window_outer(hwnd, size, GetDpiForWindow(hwnd).max(96));
    let mut current: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut current);
    let (left_edge, top_edge) = data.drag_edges.get();
    let (mut x, mut y) = (
        if left_edge {
            current.right - width
        } else {
            current.left
        },
        if top_edge {
            current.bottom - height
        } else {
            current.top
        },
    );
    // A window that fills its monitor is centred on it when Android cannot draw that much.
    if !titled(data) {
        if let Some(monitor) = monitor_rect(hwnd) {
            (x, y) = centred(&monitor, (width as u32, height as u32));
        }
    } else if IsZoomed(hwnd) != 0 {
        // A maximised window goes where a maximised window of the frame's size belongs on
        // its own monitor, not where its last frame was: that one was smaller and centred,
        // so a frame the size of the work area would start at its corner and run off the
        // screen. WM_WINDOWPOSCHANGING places it the same way; this keeps the rectangle
        // it measures on the window's own monitor.
        if let Some(work) = window_work_area(hwnd) {
            let (left, top) = maximized_border(hwnd);
            x = maximized_position(work.left, work.right - work.left, width, left);
            y = maximized_position(work.top, work.bottom - work.top, height, top);
        }
    }
    data.following.set(true);
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        x,
        y,
        width,
        height,
        SWP_NOZORDER | SWP_NOACTIVATE,
    );
    data.following.set(false);
}

/// The outer size of a window whose client area is `client` pixels at `dpi`. The client
/// area reaches the window's top edge, under the title bar, so only the side and bottom
/// borders are added, and a top border as well when the window is `maximized`.
unsafe fn outer_size(client: (u32, u32), dpi: u32, maximized: bool) -> (i32, i32) {
    let (left, top, right, bottom) = task_frame(TASK_STYLE, TASK_EX_STYLE, dpi, maximized);
    (
        client.0 as i32 + left + right,
        client.1 as i32 + top + bottom,
    )
}

/// The outer size of `hwnd` with a client area of `client` pixels at `dpi`: the client area
/// itself while the window fills its monitor, which it does with no frame.
unsafe fn window_outer(hwnd: HWND, client: (u32, u32), dpi: u32) -> (i32, i32) {
    if window_data(hwnd).is_some_and(|data| !titled(data)) {
        return (client.0 as i32, client.1 as i32);
    }
    outer_size(client, dpi, IsZoomed(hwnd) != 0)
}

/// Where a window of `size` goes to be centred on `monitor`; at its top left when it is as
/// large.
fn centred(monitor: &RECT, size: (u32, u32)) -> (i32, i32) {
    let (width, height) = (size.0 as i32, size.1 as i32);
    (
        maximized_position(monitor.left, monitor.right - monitor.left, width, 0),
        maximized_position(monitor.top, monitor.bottom - monitor.top, height, 0),
    )
}

/// The borders (left, top) a maximised `hwnd` hangs off its work area's edges: those of its
/// frame at its scale, and none while it fills its monitor without a frame.
unsafe fn maximized_border(hwnd: HWND) -> (i32, i32) {
    if window_data(hwnd).is_some_and(|data| !titled(data)) {
        return (0, 0);
    }
    let dpi = GetDpiForWindow(hwnd).max(96);
    let (left, top, _, _) = task_frame(TASK_STYLE, TASK_EX_STYLE, dpi, true);
    (left, top)
}

/// Whether the window has its title bar: not while it fills its monitor.
fn titled(data: &WindowData) -> bool {
    data.shown_mode.get() != WindowMode::Fullscreen
}

/// Tells the window's UI Automation tree whether the title bar shows the back arrow now, and
/// the icon's cell it is in, so its button comes, goes and moves with the arrow.
unsafe fn sync_back_button(hwnd: HWND, data: &WindowData) {
    let cell = if titled(data) && data.caption.shows_back() {
        data.caption.layout(hwnd).icon_cell()
    } else {
        None
    };
    if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
        tree.set_back(hwnd as isize, cell);
    }
}

/// The whole of the monitor `hwnd` is on, when the monitor says.
unsafe fn monitor_rect(hwnd: HWND) -> Option<RECT> {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info.rcMonitor)
}

/// The work area of the monitor `hwnd` is on, when the monitor says.
unsafe fn window_work_area(hwnd: HWND) -> Option<RECT> {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info.rcWork)
}

/// Sizes and places `hwnd` for itself, not as the user: a fixed-size window takes it too.
unsafe fn place_self(hwnd: HWND, after: HWND, rect: (i32, i32, i32, i32), flags: u32) {
    let data = window_data(hwnd);
    if let Some(data) = data {
        data.self_sizing.set(true);
    }
    SetWindowPos(hwnd, after, rect.0, rect.1, rect.2, rect.3, flags);
    if let Some(data) = data {
        data.self_sizing.set(false);
    }
}

/// The messages a window's mode decides: F11, and the resizing a window fixed in size
/// refuses. `None` leaves the message to the window's other handlers.
unsafe fn mode_message(
    hwnd: HWND,
    data: &WindowData,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    match message {
        WM_KEYDOWN | WM_SYSKEYDOWN if wparam as u16 == VK_F11 => {
            // Held down, the key repeats; only the press counts.
            let repeat = (lparam >> 30) & 1 != 0;
            if !repeat {
                let mut mode = data.mode.get();
                let shown = mode.toggle_fullscreen();
                data.mode.set(mode);
                apply_mode(hwnd, data, shown);
                sync_back_button(hwnd, data);
            }
            Some(0)
        }
        WM_KEYUP | WM_SYSKEYUP if wparam as u16 == VK_F11 => Some(0),
        WM_SYSCOMMAND if !data.resizable.get() => {
            let command = (wparam & 0xfff0) as u32;
            (command == SC_MAXIMIZE || command == SC_SIZE).then_some(0)
        }
        WM_NCHITTEST if !data.resizable.get() => {
            let presenter = data.presenter.borrow().clone();
            let code = if data.shown_mode.get() == WindowMode::Fullscreen {
                DefWindowProcW(hwnd, message, wparam, lparam)
            } else {
                data.caption
                    .handle(hwnd, message, wparam, lparam, presenter.as_deref())
                    .unwrap_or_else(|| DefWindowProcW(hwnd, message, wparam, lparam))
            };
            // The edges and corners that would resize the window are its border.
            let code = if (HTLEFT as isize..=HTBOTTOMRIGHT as isize).contains(&code) {
                HTBORDER as LRESULT
            } else {
                code
            };
            Some(code)
        }
        _ => None,
    }
}

/// Shows `hwnd` in `mode`. The size each mode needs is proposed like any other, so the window
/// takes it together with a frame of that size.
unsafe fn apply_mode(hwnd: HWND, data: &WindowData, mode: WindowMode) {
    let previous = data.shown_mode.get();
    if previous == mode {
        return;
    }
    if previous == WindowMode::Normal {
        let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
        placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
        if GetWindowPlacement(hwnd, &mut placement) != 0 {
            data.restore.set(Some(placement));
        }
        if IsZoomed(hwnd) != 0 {
            ShowWindow(hwnd, SW_RESTORE);
        }
    }
    data.shown_mode.set(mode);
    let keep = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    match previous {
        WindowMode::Fullscreen => {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let style = (style & !WS_POPUP) | TASK_STYLE;
            SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
            let flags = keep | SWP_NOZORDER | SWP_FRAMECHANGED;
            place_self(hwnd, std::ptr::null_mut(), (0, 0, 0, 0), flags);
            // The title bar is back, and the app's top inset with it.
            data.caption.report_height(hwnd, &*data.report);
            data.caption
                .repaint(hwnd, data.presenter.borrow().as_deref());
        }
        WindowMode::PictureInPicture => {
            place_self(hwnd, HWND_NOTOPMOST, (0, 0, 0, 0), keep);
        }
        WindowMode::Normal => {}
    }
    match mode {
        WindowMode::Normal => {
            if let Some(placement) = data.restore.take() {
                data.self_sizing.set(true);
                SetWindowPlacement(hwnd, &placement);
                data.self_sizing.set(false);
            }
        }
        WindowMode::Fullscreen => {
            let Some(monitor) = monitor_rect(hwnd) else {
                return;
            };
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let style = (style & !TASK_STYLE) | WS_POPUP;
            SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
            // No title bar, so no top inset: the app draws to the monitor's top edge.
            (data.report)(FromHost::Caption {
                task_id: data.task_id,
                band: CaptionBand::default(),
            });
            if let Some(presenter) = data.presenter.borrow().as_deref() {
                presenter.hide_caption();
            }
            place_self(
                hwnd,
                HWND_TOP,
                (
                    monitor.left,
                    monitor.top,
                    monitor.right - monitor.left,
                    monitor.bottom - monitor.top,
                ),
                SWP_FRAMECHANGED | SWP_NOOWNERZORDER,
            );
        }
        WindowMode::PictureInPicture => {
            let Some(work) = window_work_area(hwnd) else {
                return;
            };
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            let task = (
                (client.right - client.left).max(1) as u32,
                (client.bottom - client.top).max(1) as u32,
            );
            let dpi = GetDpiForWindow(hwnd).max(96);
            let margin = (16 * dpi / 96) as i32;
            let area = (work.left, work.top, work.right, work.bottom);
            let (_, _, width, height) = picture_in_picture_place(task, area, margin);
            // The window's frame, not only its client area, keeps the margin from the corner.
            let (outer_width, outer_height) = outer_size((width, height), dpi, false);
            let x = (work.right - margin - outer_width).max(work.left);
            let y = (work.bottom - margin - outer_height).max(work.top);
            let rect = (x, y, outer_width, outer_height);
            place_self(hwnd, HWND_TOPMOST, rect, SWP_NOACTIVATE);
        }
    }
}

/// The work area of the monitor nearest `rect`, when the monitor says.
unsafe fn work_area(rect: &RECT) -> Option<RECT> {
    let monitor = MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (GetMonitorInfoW(monitor, &mut info) != 0).then_some(info.rcWork)
}

/// The largest client area a window can have on its monitor's work area.
unsafe fn usable_client_area(hwnd: HWND, dpi: u32) -> (u32, u32) {
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut info) == 0 {
        return (u32::MAX, u32::MAX);
    }
    let (frame_width, frame_height) = outer_size((0, 0), dpi, false);
    let work = info.rcWork;
    (
        (work.right - work.left - frame_width).max(1) as u32,
        (work.bottom - work.top - frame_height).max(1) as u32,
    )
}

/// Sizes `hwnd` so its client area is `size` pixels, or the largest of that shape that fits
/// its monitor. Returns the client area it got.
unsafe fn apply_size(hwnd: HWND, size: (u32, u32)) -> (u32, u32) {
    // The task's size is in the guest's pixels, which are the window's pixels.
    let dpi = GetDpiForWindow(hwnd).max(96);
    let client = fit_within(size, usable_client_area(hwnd, dpi));
    // A maximized window keeps the maximized size: resizing it would shrink it inside its
    // maximized frame, and the guest would be asked for a size nobody chose.
    if IsZoomed(hwnd) != 0 {
        return client;
    }
    // A window that fills its monitor has no frame, and stays centred on the monitor.
    if window_data(hwnd).is_some_and(|data| !titled(data)) {
        if let Some(monitor) = monitor_rect(hwnd) {
            let area = (
                (monitor.right - monitor.left).max(1) as u32,
                (monitor.bottom - monitor.top).max(1) as u32,
            );
            let client = fit_within(size, area);
            let (x, y) = centred(&monitor, client);
            let rect = (x, y, client.0 as i32, client.1 as i32);
            place_self(
                hwnd,
                std::ptr::null_mut(),
                rect,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            return client;
        }
    }
    let (outer_width, outer_height) = outer_size(client, dpi, false);
    place_self(
        hwnd,
        std::ptr::null_mut(),
        (0, 0, outer_width, outer_height),
        SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
    );
    client
}

impl Ui {
    fn log(&self, message: &str) {
        note(self.setup.logger.as_deref(), message);
    }

    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            // SAFETY: plain Win32 calls on this thread's own windows.
            unsafe {
                match command {
                    ToHost::Show {
                        task_id,
                        title,
                        width,
                        height,
                        focused,
                    } => self.open_window(task_id, &title, (width, height), focused),
                    ToHost::Retitle { task_id, title } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            SetWindowTextW(*hwnd, wide(&title).as_ptr());
                            // The title bar shows the name too.
                            if let Some(data) = window_data(*hwnd) {
                                let presenter = data.presenter.borrow().clone();
                                data.caption.set_title(*hwnd, &title, presenter.as_deref());
                            }
                        }
                    }
                    ToHost::Resize {
                        task_id,
                        width,
                        height,
                    } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                // The window's size; the task's is that at the window's scale.
                                let task = to_task_size((width, height), data.scale.get());
                                data.pointer.task_resized(task);
                                if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
                                    tree.task_resized(task);
                                }
                            }
                            apply_size(*hwnd, (width, height));
                        }
                    }
                    ToHost::Close { task_id } => self.close(task_id),
                    ToHost::TaskSize {
                        task_id,
                        width,
                        height,
                        max,
                        min,
                        scale,
                    } => {
                        if let Some(data) =
                            self.open.get(&task_id).and_then(|hwnd| window_data(*hwnd))
                        {
                            data.pointer.task_resized((width, height));
                            // A point in the window is a task pixel at the window's scale.
                            data.pointer.set_scale(scale);
                            // So is a node's place on the screen, and a screen point in a hit
                            // test.
                            if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
                                tree.task_resized((width, height));
                                tree.set_scale(scale);
                            }
                            data.max_client.set(max);
                            data.min_client.set(min);
                            if data.scale.replace(scale) != scale {
                                if let Some(presenter) = data.presenter.borrow().as_deref() {
                                    presenter.set_scale(scale);
                                }
                            }
                        }
                    }
                    ToHost::ImeCursor { task_id, cursor } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                data.ime.cursor_moved(cursor);
                            }
                        }
                    }
                    ToHost::ImeEditor { task_id, editor } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                data.ime.editor_changed(editor);
                            }
                        }
                    }
                    ToHost::Mode {
                        task_id,
                        mode,
                        resizable,
                    } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                data.resizable.set(resizable);
                                let mut state = data.mode.get();
                                let shown = state.app_asks(mode);
                                data.mode.set(state);
                                apply_mode(*hwnd, data, shown);
                                sync_back_button(*hwnd, data);
                            }
                        }
                    }
                    ToHost::CaptionTone { task_id, tone } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                let presenter = data.presenter.borrow().clone();
                                data.caption.set_tone(*hwnd, tone, presenter.as_deref());
                            }
                        }
                    }
                    ToHost::CaptionMode {
                        task_id,
                        app_drawn,
                        interactive,
                    } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                let presenter = data.presenter.borrow().clone();
                                data.caption.set_mode(
                                    *hwnd,
                                    app_drawn,
                                    interactive,
                                    presenter.as_deref(),
                                );
                            }
                        }
                    }
                    ToHost::CaptionBack { task_id, back } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                let presenter = data.presenter.borrow().clone();
                                data.caption.set_back(*hwnd, back, presenter.as_deref());
                                sync_back_button(*hwnd, data);
                            }
                        }
                    }
                    // The window host's `WindowTasks` takes it, before it reaches this thread;
                    // the window itself shows the same.
                    ToHost::ShownTask { .. } => {}
                    ToHost::AppMenu { new_window } => {
                        // Every window of the app has the same menu, and the next ones too.
                        self.new_window = new_window;
                        for hwnd in self.open.values() {
                            system_menu::apply(*hwnd, new_window);
                        }
                    }
                    ToHost::AppIcon {
                        icon_png,
                        caption_icon_png,
                    } => {
                        // Every window of the app shows the new icon at once, and the next ones
                        // too.
                        if super::live_icon::set(icon_png, caption_icon_png) {
                            for hwnd in self.open.values() {
                                if let Some(data) = window_data(*hwnd) {
                                    take_live_icon(*hwnd, data, true);
                                }
                            }
                        } else {
                            self.log("the app's new icon is not an image; its windows keep theirs");
                        }
                    }
                    ToHost::AppMedia { media } => {
                        // Every window's preview follows the app's media session, and the next
                        // windows' too.
                        if super::thumb_bar::set(media) {
                            for hwnd in self.open.values() {
                                if let Some(data) = window_data(*hwnd) {
                                    sync_thumb_bar(*hwnd, data, ThumbBarStep::Changed);
                                }
                            }
                        }
                    }
                }
            }
        }
        self.publish();
    }

    /// Logs the windows left showing a frame of an older size.
    fn tick(&self) {
        let presenters: Vec<Arc<WindowPresenter>> = self
            .setup
            .presenters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for presenter in presenters {
            presenter.tick();
        }
        // Each window's tree stops being watched once no client reads it.
        for hwnd in self.open.values() {
            // SAFETY: a window of this thread.
            if let Some(data) = unsafe { window_data(*hwnd) } {
                if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
                    tree.tick(*hwnd as isize);
                }
            }
        }
    }

    /// Lists the windows that are open.
    fn publish(&self) {
        let mut listing: Vec<OpenWindow> = self
            .open
            .iter()
            .map(|(task_id, hwnd)| OpenWindow {
                task_id: *task_id,
                hwnd: *hwnd as isize,
                // SAFETY: a window of this thread.
                ime_cursor: unsafe { window_data(*hwnd) }.and_then(|data| data.ime.cursor()),
                // SAFETY: as above.
                ime_editor: unsafe { window_data(*hwnd) }.and_then(|data| data.ime.editor()),
            })
            .collect();
        listing.sort_by_key(|window| window.task_id);
        *self.setup.listing.lock().unwrap_or_else(|e| e.into_inner()) = listing;
    }

    fn close(&mut self, task_id: i32) {
        let presenter = self
            .setup
            .presenters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&task_id);
        let hwnd = self.open.remove(&task_id);
        close_whole(&mut ClosingTask { presenter, hwnd });
    }

    unsafe fn open_window(&mut self, task_id: i32, title: &str, size: (u32, u32), focused: bool) {
        if self.open.contains_key(&task_id) {
            return;
        }
        let data = Box::into_raw(Box::new(WindowData {
            task_id,
            report: Arc::clone(&self.setup.report),
            presenter: RefCell::new(None),
            pointer: PointerWindow::new(
                task_id,
                size,
                self.setup.input.clone(),
                self.setup.drags.clone(),
            ),
            keys: KeyWindow::new(task_id, self.setup.input.clone()),
            ime: ImeWindow::new(task_id, self.setup.input.clone()),
            accessibility: RefCell::new(AccessibilityWindow::new(
                task_id,
                size,
                self.setup.input.clone(),
                self.setup.tasks.clone(),
                self.setup.accessibility.clone(),
                self.setup.logger.clone(),
            )),
            max_client: Cell::new(None),
            min_client: Cell::new(None),
            scale: Cell::new((1, 1)),
            reporting: Cell::new(false),
            minimized: Cell::new(false),
            caption: CaptionWindow::new(
                task_id,
                TASK_STYLE,
                TASK_EX_STYLE,
                ICON_ID,
                self.setup.input.clone(),
            ),
            drag_edges: Cell::new((false, false)),
            following: Cell::new(false),
            mode: Cell::new(ModeState::new()),
            shown_mode: Cell::new(WindowMode::Normal),
            resizable: Cell::new(true),
            restore: Cell::new(None),
            self_sizing: Cell::new(false),
            rescaling: Cell::new(false),
            logger: self.setup.logger.clone(),
            icons: RefCell::new(None),
            thumb_bar: super::thumb_bar::ThumbBar::new(),
        }));
        let class = wide(TASK_CLASS);
        let hwnd = CreateWindowExW(
            TASK_EX_STYLE,
            class.as_ptr(),
            wide(title).as_ptr(),
            TASK_STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            GetModuleHandleW(std::ptr::null()),
            data as *const std::ffi::c_void,
        );
        if hwnd.is_null() {
            // The window never took the data.
            drop(Box::from_raw(data));
            self.log(&format!(
                "could not open the window of task {task_id}: {}",
                std::io::Error::last_os_error()
            ));
            return;
        }
        super::pointer::deliver_contacts_at_once(hwnd);
        // The input method composes in the window's own document.
        if let Some(data) = window_data(hwnd) {
            if let Err(why) = data.ime.attach(hwnd) {
                self.log(&format!(
                    "task {task_id}'s window takes no input method: {why}"
                ));
            }
        }
        window_marks::mark_task(hwnd as isize, task_id);
        // The Android window menu, under Windows' own items.
        system_menu::apply(hwnd, self.new_window);
        // The app's icon, when it changed after the program was made.
        if let Some(data) = window_data(hwnd) {
            take_live_icon(hwnd, data, false);
        }
        // Files dropped from Explorer go to the task's app.
        if let Some(handler) = &self.setup.drops {
            if let Err(why) = drop_target::register(hwnd as isize, task_id, Arc::clone(handler)) {
                self.log(&why);
            }
        }
        let client = apply_size(hwnd, size);
        let presenter = match &self.setup.gpu {
            Some(gpu) => match WindowPresenter::new(
                task_id,
                Arc::clone(gpu),
                hwnd as isize,
                client,
                self.setup.logger.clone(),
            ) {
                Ok(presenter) => Some(presenter),
                Err(why) => {
                    self.log(&format!(
                        "task {task_id}'s window cannot show frames: {why}"
                    ));
                    None
                }
            },
            None => None,
        };
        if let Some(data) = window_data(hwnd) {
            *data.presenter.borrow_mut() = presenter.clone();
        }
        if let Some(presenter) = presenter {
            let window = hwnd as isize;
            presenter.on_frame_ready(Box::new(move || {
                // SAFETY: posting to a window is safe from any thread; one that has closed
                // drops the message.
                unsafe { PostMessageW(window as HWND, WM_APP_FRAME_READY, 0, 0) };
            }));
            presenter.on_band_colour(Box::new(move |sample| {
                let (wparam, lparam) = pack_band_sample(sample);
                // SAFETY: as above.
                unsafe { PostMessageW(window as HWND, WM_APP_BAND_COLOUR, wparam, lparam) };
            }));
            self.setup
                .presenters
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(task_id, presenter);
        }
        // From here on the window reports its size. First the scale of the monitor it opened
        // on, which the window manager may answer with the size that keeps the app the same
        // size to the eye there, then the size it has.
        if let Some(data) = window_data(hwnd) {
            data.reporting.set(true);
        }
        (self.setup.report)(FromHost::Dpi {
            task_id,
            dpi: GetDpiForWindow(hwnd).max(96),
        });
        // Then the title bar's height, which becomes the task's top inset.
        if let Some(data) = window_data(hwnd) {
            data.caption.extend_frame(hwnd);
            data.caption.report_height(hwnd, &*self.setup.report);
            data.caption
                .set_title(hwnd, title, data.presenter.borrow().as_deref());
            // Until the app's first frame the window shows the app's icon on a splash
            // background, never the desktop through an empty window. It is on the screen
            // before the window is shown.
            if let Some(presenter) = data.presenter.borrow().as_deref() {
                let dpi = GetDpiForWindow(hwnd).max(96);
                presenter.show_splash(super::splash::image(ICON_ID, dpi));
            }
        }
        (self.setup.report)(FromHost::Resized {
            task_id,
            width: client.0,
            height: client.1,
        });
        ShowWindow(hwnd, if focused { SW_SHOW } else { SW_SHOWNOACTIVATE });
        // The first window a program shows takes the show state its launcher asked for (a
        // shortcut set to open maximized or minimized), whatever is passed above. A task
        // window opens at its task's size, so it is shown normally again.
        if IsZoomed(hwnd) != 0 || IsIconic(hwnd) != 0 {
            ShowWindow(
                hwnd,
                if focused {
                    SW_SHOWNORMAL
                } else {
                    SW_SHOWNOACTIVATE
                },
            );
        }
        self.open.insert(task_id, hwnd);
    }
}

/// The steps of closing a task window, which [`close_whole`] takes in order.
trait ClosingWindow {
    /// No more frames are shown in the window; the one it shows stays in it.
    fn stop_frames(&mut self);
    /// The window leaves the screen. Windows plays its close animation on the window as it
    /// is now, with its content still in it.
    fn destroy(&mut self);
    /// What the window showed is let go: its swapchain, its surfaces and the newest frame.
    fn release_content(&mut self);
}

/// Closes a task window as one piece, as Windows closes its own: the window is destroyed
/// with its last frame still in it, and what it showed is let go only once it is gone.
/// Letting the content go first left an empty frame on the screen, and only that frame
/// played the close animation.
fn close_whole(window: &mut impl ClosingWindow) {
    window.stop_frames();
    window.destroy();
    window.release_content();
}

/// A task window being closed: its presenter and its window, either of which may be missing
/// (a window that could not show frames has no presenter).
struct ClosingTask {
    presenter: Option<Arc<WindowPresenter>>,
    hwnd: Option<HWND>,
}

impl ClosingWindow for ClosingTask {
    fn stop_frames(&mut self) {
        if let Some(presenter) = &self.presenter {
            presenter.close();
        }
    }

    fn destroy(&mut self) {
        if let Some(hwnd) = self.hwnd.take() {
            // SAFETY: a window of this thread.
            unsafe { DestroyWindow(hwnd) };
        }
    }

    fn release_content(&mut self) {
        if let Some(presenter) = self.presenter.take() {
            presenter.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A task window that records what is done to it, and what it showed when it left the
    /// screen.
    struct FakeWindow {
        steps: Vec<&'static str>,
        content_attached: bool,
        frames_flowing: bool,
        /// Whether the content was attached, and frames still came, when it was destroyed.
        destroyed: Option<(bool, bool)>,
    }

    impl ClosingWindow for FakeWindow {
        fn stop_frames(&mut self) {
            self.steps.push("stop_frames");
            self.frames_flowing = false;
        }

        fn destroy(&mut self) {
            self.steps.push("destroy");
            self.destroyed = Some((self.content_attached, self.frames_flowing));
        }

        fn release_content(&mut self) {
            self.steps.push("release_content");
            self.content_attached = false;
        }
    }

    #[test]
    fn fr3_content_stays_attached_until_the_window_is_gone() {
        let mut window = FakeWindow {
            steps: Vec::new(),
            content_attached: true,
            frames_flowing: true,
            destroyed: None,
        };
        close_whole(&mut window);
        // The window left the screen with its last frame still in it and no newer frame on
        // the way, so the close animation shows the whole window; its content went after.
        assert_eq!(window.destroyed, Some((true, false)));
        assert_eq!(
            window.steps,
            vec!["stop_frames", "destroy", "release_content"]
        );
        assert!(!window.content_attached);
    }

    #[test]
    fn fr22_a_band_sample_reaches_the_window_whole() {
        for sample in [
            BandSample::default(),
            BandSample {
                fill: Some(0xFFFF_FFFF),
                luminance: None,
            },
            BandSample {
                fill: None,
                luminance: Some(0),
            },
            BandSample {
                fill: Some(0xFF00_0000),
                luminance: Some(1000),
            },
        ] {
            let (wparam, lparam) = pack_band_sample(sample);
            assert_eq!(unpack_band_sample(wparam, lparam), sample);
        }
    }

    #[test]
    fn fr3_a_command_whose_wake_up_cannot_be_posted_does_not_end_the_windows() {
        // A message window that cannot be posted to stands for a UI thread whose queue is
        // full. The command still waits for the UI thread, and the program goes on, so the
        // app's windows stay open and Android does not end their tasks.
        let (commands, received) = std::sync::mpsc::channel();
        let handle = UiHandle {
            message_window: 1,
            commands,
            thread: None,
        };
        assert!(handle.send(ToHost::Close { task_id: 5 }));
        assert_eq!(received.try_recv().ok(), Some(ToHost::Close { task_id: 5 }));

        // Only a UI thread that has ended ends the program.
        drop(received);
        assert!(!handle.send(ToHost::Close { task_id: 5 }));
    }
}
