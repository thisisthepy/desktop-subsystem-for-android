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
//! others. F11 and the window menu's Full screen turn full screen on and off. A window that fills
//! its monitor shows its title bar while the pointer is at the monitor's top edge
//! ([`caption_revealed`]), so minimize and close stay in reach; its middle button leaves full
//! screen. The keyboard reaches it too: Alt pressed and released alone brings the title bar
//! out and keeps it out ([`AltTap`], [`keyboard_reveal`]), and Alt again, Esc or a press in the
//! app takes it away. Alt still reaches the app, and Alt with another key stays a shortcut.
//! Each change of mode proposes the new size like any
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

use dsa_host_core::compositor::caption::BandSample;
use dsa_host_core::compositor::task_windows::fit_within;
use dsa_host_core::compositor::window_hosts::{FromHost, ToHost};
use dsa_host_core::compositor::window_mode::{
    caption_revealed, picture_in_picture_place, ModeState, WindowMode,
};
use dsa_host_core::compositor::window_sizing::{
    frame_rect, is_edge_drag, kept_frame_action, maximized_outer, maximized_position,
    min_client_size, rescaled_rect, size_is_held, to_task_position, to_task_size,
    to_window_position, FrameRectRequest, KeptFrame, LoopKind, MoveLoop, SizeProposal,
};
use dsa_host_core::compositor::window_tasks::WindowTasks;
use dsa_host_core::ime::ImeCursor;
use dsa_host_core::input::InputEvent;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::ScreenToClient;
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromRect, MonitorFromWindow, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForWindow, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{VK_ESCAPE, VK_F11};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed, KillTimer, LoadCursorW, LoadIconW,
    PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos,
    SetWindowTextW, ShowWindow, TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA,
    HWND_MESSAGE, IDC_ARROW, MINMAXINFO, MSG, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOSIZE, SWP_NOZORDER, SW_SHOW, SW_SHOWNOACTIVATE, SW_SHOWNORMAL, WA_INACTIVE, WINDOWPOS,
    WINDOW_EX_STYLE, WINDOW_STYLE, WMSZ_BOTTOMLEFT, WMSZ_LEFT, WMSZ_TOP, WMSZ_TOPLEFT,
    WMSZ_TOPRIGHT, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND,
    WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_GETOBJECT, WM_MOVING, WM_NCCREATE, WM_NCDESTROY,
    WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WM_SIZING, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WM_TIMER,
    WM_WINDOWPOSCHANGED, WM_WINDOWPOSCHANGING, WNDCLASSEXW, WS_EX_APPWINDOW,
    WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowPlacement, SetWindowPlacement, GWL_STYLE, HTBORDER, HTBOTTOMRIGHT,
    HTLEFT, HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, SC_MAXIMIZE, SC_MOVE, SC_SIZE,
    SWP_FRAMECHANGED, SWP_NOOWNERZORDER, SW_RESTORE, WINDOWPLACEMENT, WM_COMMAND, WM_KEYDOWN,
    WM_KEYUP, WM_NCHITTEST, WM_SYSCOMMAND, WM_SYSKEYDOWN, WM_SYSKEYUP, WS_POPUP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, HTCAPTION, HTNOWHERE, SC_RESTORE, WM_MOUSEMOVE, WM_NCCALCSIZE, WM_NCMOUSELEAVE,
    WM_NCMOUSEMOVE, WS_SYSMENU,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_MBUTTONDOWN, WM_MOUSEWHEEL, WM_NCLBUTTONDOWN,
    WM_NCRBUTTONDOWN, WM_POINTERDOWN, WM_RBUTTONDOWN,
};

use crate::compositor::log::{
    input_trace, input_trace_on, trace_detailed, trace_move_ended, trace_moving, trace_place,
};
use crate::compositor::log::{note, CompositorLogger};
use crate::compositor::present::{self, SurfaceLink, WindowPresenter};
use crate::compositor::window_marks;
use std::fmt::Write as _;
use windows_sys::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowArranged;

use super::accessibility::{AccessibilityWindow, ApiConnect, CaptionCells, WM_APP_ACCESSIBILITY};
use super::caption::{task_frame, CaptionWindow};
use super::drag_source::{self, DragStarter, WM_APP_DRAG_OUT};
use super::drop_target::{self, DropHandler};
use super::ime::ImeWindow;
use super::keys::{other_modifiers_held, AltTap, KeyWindow};
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

/// A band sample as the parameters of [`WM_APP_BAND_COLOUR`]: the fill color in the low half
/// of `wparam` and the band's own color in its high half, and in `lparam` bit 0 set when
/// there is a fill, bit 1 set when the band's luminance was read, bit 2 set when its darkest
/// and lightest samples were, bit 3 set when the band is one color, the luminance itself
/// from bit 16, the darkest sample's from bit 32 and the lightest's from bit 48 (each at most
/// 1000).
fn pack_band_sample(sample: BandSample) -> (WPARAM, LPARAM) {
    let colors = u64::from(sample.fill.unwrap_or(0)) | u64::from(sample.band.unwrap_or(0)) << 32;
    let wparam = colors as WPARAM;
    let mut lparam = LPARAM::from(sample.fill.is_some());
    if sample.band.is_some() {
        lparam |= 8;
    }
    if let Some(luminance) = sample.luminance {
        lparam |= 2 | (luminance.min(1000) as LPARAM) << 16;
    }
    if let Some((darkest, lightest)) = sample.range {
        lparam |= 4 | (darkest.min(1000) as LPARAM) << 32 | (lightest.min(1000) as LPARAM) << 48;
    }
    (wparam, lparam)
}

/// The band sample [`pack_band_sample`] put in a message's parameters.
fn unpack_band_sample(wparam: WPARAM, lparam: LPARAM) -> BandSample {
    let field = |shift: u32| ((lparam >> shift) & 0xFFFF) as u16;
    BandSample {
        fill: (lparam & 1 != 0).then_some(wparam as u32),
        luminance: (lparam & 2 != 0).then_some(field(16)),
        range: (lparam & 4 != 0).then_some((field(32), field(48))),
        band: (lparam & 8 != 0).then_some((wparam as u64 >> 32) as u32),
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
    /// The window manager's GPU, which the windows' frames, caption and splash are drawn with
    /// (this process makes no Direct3D device).
    pub surfaces: Arc<SurfaceLink>,
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
    /// The app's package, which the place of its overlay windows is remembered under.
    pub package: String,
    /// The folder the places of apps' overlay windows are remembered in. `None` remembers
    /// none.
    pub overlay_places: Option<std::path::PathBuf>,
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
    /// Whether the move loop has moved the window or dragged an edge yet: only then is the
    /// user dragging it.
    move_loop: Cell<MoveLoop>,
    /// Whether the window is in its move or size loop, for the resize trace: plain moves
    /// there are logged a few a second (`win.move_loop`), not each.
    in_move_loop: Cell<bool>,
    /// How the running move or size loop started (`WM_SYSCOMMAND`).
    loop_kind: Cell<LoopKind>,
    /// The window size the trace last logged, so a drag logs a change of size.
    traced_size: Cell<(u32, u32)>,
    /// Whether the user is dragging one of the window's edges now.
    sizing: Cell<bool>,
    /// Whether the window holds the size the desktop gave it (maximised, snapped, full
    /// screen) and shows a frame of another size in its middle, rather than take the size of
    /// the frames Android draws for it ([`size_is_held`]).
    held: Cell<bool>,
    /// The cursor of the editor focused in the task, as the window manager last said, in the
    /// window's pixels at its scale; the input method gets it where the window shows it.
    ime_cursor: Cell<Option<ImeCursor>>,
    /// Whether the window is taking the size of a frame, which the guest is not asked for.
    following: Cell<bool>,
    /// What the app asked for and the user's F11 over it.
    mode: Cell<ModeState>,
    /// The mode the window is shown in now.
    shown_mode: Cell<WindowMode>,
    /// Whether a window that fills its monitor shows its title bar now: the pointer came to
    /// the top edge, or Alt brought it out.
    revealed: Cell<bool>,
    /// Whether the keyboard brought the title bar out (Alt alone), which keeps it out until
    /// Alt again, Esc or a press in the app, wherever the pointer goes.
    revealed_by_keys: Cell<bool>,
    /// Whether Alt is being pressed alone, which brings a full screen window's title bar out.
    alt_tap: Cell<AltTap>,
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
        // The input methods compose in the windows through the text services framework, which
        // runs on this thread, started when a window first needs it (`ime`).

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
            super::overlay::close_all(&ui.setup);
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
            // How the move or size loop that may follow starts: an edge drag is the size
            // command only, whatever WM_SIZING Windows sends inside a title bar move.
            let command = (wparam & 0xfff0) as u32;
            if let Some(kind) = LoopKind::started_by(command == SC_SIZE, command == SC_MOVE) {
                data.loop_kind.set(kind);
            }
            if let Some(item) = system_menu::chosen(wparam) {
                menu_chosen(hwnd, data, item);
                return 0;
            }
        }
        // A window that fills its monitor shows its title bar on Alt alone too, and hides it
        // on Esc or a press in the app. It sees every key first, F11 too, so Alt+F11 is never
        // taken for Alt alone.
        if let Some(result) = reveal_message(hwnd, data, message, wparam, lparam) {
            return result;
        }
        if let Some(result) = mode_message(hwnd, data, message, wparam, lparam) {
            return result;
        }
        let presenter = data.presenter.borrow().clone();
        // A window that fills its monitor shows its title bar while the pointer is at the top
        // edge, and takes it away once the pointer goes below it or out of the window.
        if !titled(data) {
            follow_reveal(hwnd, data, message, lparam, presenter.as_deref());
        }
        // The window menu: a click on the icon, a right click on the title bar, Alt+Space. A
        // window that fills its monitor keeps it, for Alt+Space and its revealed title bar.
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
        // The title bar first: what lies in it is the window's, never the app's. A window that
        // fills its monitor has one only while it is revealed, and lays out no frame for it.
        let revealed = !titled(data) && data.revealed.get() && message != WM_NCCALCSIZE;
        if titled(data) || revealed {
            if let Some(result) =
                data.caption
                    .handle(hwnd, message, wparam, lparam, presenter.as_deref())
            {
                // Its band neither drags nor resizes a window that fills its monitor.
                if revealed && message == WM_NCHITTEST {
                    return full_screen_hit(result);
                }
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
                        crate::compositor::trim::give_back_minimized();
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
                    // A window that holds its size shows its frame in its new middle.
                    if data.held.get() {
                        place_ime_cursor(hwnd, data);
                    }
                    // The buttons stay at the right end, and a maximized window's middle
                    // button restores it.
                    if titled(data) {
                        data.caption.paint(hwnd, data.presenter.borrow().as_deref());
                    }
                    // The buttons move with the right edge, the middle one is Restore while
                    // the window is maximized, and a narrow window has no icon's cell.
                    sync_caption_buttons(hwnd, data);
                }
            }
            0
        }
        WM_ENTERSIZEMOVE | WM_EXITSIZEMOVE => {
            if let Some(data) = window_data(hwnd) {
                if message == WM_ENTERSIZEMOVE {
                    data.drag_edges.set((false, false));
                }
                // WM_SIZING says when the drag is of an edge.
                data.sizing.set(false);
                if message == WM_EXITSIZEMOVE {
                    data.loop_kind.set(LoopKind::None);
                }
                // A click on the title bar enters the loop whether or not it moves the window,
                // as the first click of a double click does: the drag is reported only once
                // the window moves (WM_MOVING) or an edge is dragged (WM_SIZING).
                let mut move_loop = data.move_loop.get();
                let report = if message == WM_ENTERSIZEMOVE {
                    move_loop.entered();
                    None
                } else {
                    move_loop.exited()
                };
                data.move_loop.set(move_loop);
                let entered = message == WM_ENTERSIZEMOVE;
                data.in_move_loop.set(entered);
                if trace_detailed() {
                    if !entered {
                        trace_move_ended(data.logger.as_deref(), data.task_id);
                    }
                    let event = if entered { "enter" } else { "exit" };
                    let detail = format!("event={event} {}", placement_words(hwnd));
                    trace_window(data, "win.move_loop", window_rect(hwnd), &detail);
                }
                if let Some(dragging) = report {
                    (data.report)(FromHost::Dragging {
                        task_id: data.task_id,
                        dragging,
                    });
                }
                // The drop may have snapped the window: it keeps that rectangle (held), and the
                // guest is asked for its client size, instead of the window taking the size of
                // the frame it still shows.
                if !entered {
                    take_kept_frame(hwnd, data);
                }
            }
            0
        }
        WM_MOVING => {
            if let Some(data) = window_data(hwnd) {
                report_drag_started(data);
                if trace_detailed() {
                    let (pos, size) = rect_parts(&*(lparam as *const RECT));
                    trace_moving(data.logger.as_deref(), data.task_id, pos, size);
                }
            }
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_SIZING => {
            // Which edge the user drags, for when the window later takes the size of a frame.
            if let Some(data) = window_data(hwnd) {
                let edge = wparam as u32;
                report_drag_started(data);
                // Inside a title bar move loop Windows sends WM_SIZING when it changes the
                // size itself (the restore as the drag starts, a snap preview, the drop on a
                // snap zone): those sizes are taken as they are.
                if is_edge_drag(data.loop_kind.get(), true) {
                    data.sizing.set(true);
                    data.drag_edges.set((
                        matches!(edge, WMSZ_LEFT | WMSZ_TOPLEFT | WMSZ_BOTTOMLEFT),
                        matches!(edge, WMSZ_TOP | WMSZ_TOPLEFT | WMSZ_TOPRIGHT),
                    ));
                }
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
            // No largest size: a maximised, snapped or full screen window takes the size the
            // desktop gives it on its own monitor, and shows the largest frame Android draws in
            // its middle when the guest's display holds less (size_is_held, frame_placement).
            // Windows keeps every size it applies within the tracking sizes, a maximised
            // window's too, so a largest tracking size would keep it from filling its monitor.
            // A window the user drags larger than Android can draw keeps the size of the
            // largest frame Android draws instead (keep_to_frame).
            //
            // A maximised window fills its own monitor's work area and no more: left to
            // Windows, its client area, which reaches under the title bar, could be the whole
            // monitor, under the taskbar, and the guest would be asked for the monitor's size
            // (maximized_outer). WM_WINDOWPOSCHANGING keeps it to that size as well.
            if titled(data) {
                if let Some(outer) = maximized_window_outer(hwnd, None) {
                    info.ptMaxSize = POINT {
                        x: outer.0,
                        y: outer.1,
                    };
                    info.ptMaxTrackSize = POINT {
                        x: info.ptMaxTrackSize.x.max(outer.0),
                        y: info.ptMaxTrackSize.y.max(outer.1),
                    };
                }
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
            // What Windows asks for, and each step below that changes it, for the trace.
            let snap = |pos: &WINDOWPOS| (pos.x, pos.y, pos.cx, pos.cy, pos.flags);
            let asked = snap(&*(lparam as *const WINDOWPOS));
            let mut why: Vec<&str> = Vec::new();
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            let pos = &mut *(lparam as *mut WINDOWPOS);
            let mut step = |pos: &WINDOWPOS, last: &mut (i32, i32, i32, i32, u32), word| {
                if snap(pos) != *last {
                    why.push(word);
                    *last = snap(pos);
                }
            };
            let mut last = asked;
            step(pos, &mut last, "min_max");
            // A window fixed in size takes no size the user proposes (a snap, say); the sizes
            // it gives itself, for Android or for a change of mode, still apply.
            if let Some(data) = window_data(hwnd) {
                let user = !data.self_sizing.get() && !data.following.get();
                let normal = data.shown_mode.get() == WindowMode::Normal;
                if !data.resizable.get() && user && normal {
                    pos.flags |= SWP_NOSIZE;
                }
            }
            step(pos, &mut last, "fixed_size");
            // A maximised window's client area is its own monitor's work area, whatever size
            // was proposed, before the guest is asked for it below: never the whole monitor,
            // under the taskbar, and never a size measured on another monitor.
            if IsZoomed(hwnd) != 0
                && IsIconic(hwnd) == 0
                && pos.flags & SWP_NOSIZE == 0
                && window_data(hwnd).is_some_and(titled)
            {
                let proposed = RECT {
                    left: pos.x,
                    top: pos.y,
                    right: pos.x + pos.cx,
                    bottom: pos.y + pos.cy,
                };
                let on = (pos.flags & SWP_NOMOVE == 0).then_some(&proposed);
                if let Some((width, height)) = maximized_window_outer(hwnd, on) {
                    pos.cx = width;
                    pos.cy = height;
                }
            }
            step(pos, &mut last, "max_work_area");
            // DefWindowProc above has already kept the proposed size to WM_GETMINMAXINFO.
            if pos.flags & SWP_NOSIZE == 0 && IsIconic(hwnd) == 0 {
                if let Some(data) = window_data(hwnd) {
                    // A maximised, snapped or full screen window keeps the size proposed,
                    // whatever size the guest gives its task. A new monitor scale changes
                    // nothing about that.
                    if !data.rescaling.get() {
                        set_held(hwnd, data, holds_proposed_size(hwnd, data, pos));
                    }
                    keep_to_frame(hwnd, data, pos);
                }
            }
            step(pos, &mut last, "keep_to_frame");
            // A maximised window fills its work area, and has its client area at the work
            // area's corner, whatever position was proposed. One smaller than the work area
            // is centred on it rather than left in its top left corner.
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
            step(pos, &mut last, "max_position");
            if let Some(data) = window_data(hwnd).filter(|_| trace_detailed()) {
                trace_pos_changing(hwnd, data, asked, snap(pos), &why);
            }
            result
        }
        WM_WINDOWPOSCHANGED => {
            // Sends WM_SIZE, where the window takes a new size with its frame. A size it chose
            // and did not get no longer holds frames back.
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            if let Some(data) = window_data(hwnd).filter(|_| trace_detailed()) {
                let pos = &*(lparam as *const WINDOWPOS);
                let rect = window_rect(hwnd);
                let flags = pos.flags;
                if placement_worth_tracing(data, flags, rect.1) {
                    let detail = format!("{} {}", swp_words(flags), placement_words(hwnd));
                    trace_window(data, "win.pos_changed", rect, &detail);
                }
            }
            if let Some(presenter) =
                window_data(hwnd).and_then(|data| data.presenter.borrow().clone())
            {
                presenter.release_claim();
            }
            result
        }
        WM_DPICHANGED => {
            // The scale goes first, so the core shows the task at the new scale before the
            // resize below reports the window's new size. A task that follows its monitor
            // takes the monitor's density only when the move ends, at the size the window has
            // then, so the window keeps that size.
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
            // A window that holds the size the desktop gives it (maximised, snapped, full
            // screen) takes the suggested size whole, and shows the largest frame Android draws
            // in its middle.
            let held = zoomed || data.is_some_and(|data| data.held.get());
            if let Some(max) = data
                .and_then(|data| data.max_client.get())
                .filter(|_| !held)
            {
                limit = outer_size(max, dpi.max(96), zoomed);
            }
            // A maximized window is kept to its monitor by Windows, and one that fills its
            // monitor covers more than the work area.
            //
            // While the user drags the window, it keeps the size the desktop suggests up to the
            // whole monitor, as a native window does: cut to the work area, it would come out
            // shorter than the desktop's size to the eye (the taskbar's height), take another
            // shape under the user's hand, and come back a different size when dragged back.
            // Only a window larger than the monitor is cut, so it cannot reach the next one.
            let dragged = data.is_some_and(|data| data.in_move_loop.get());
            if !zoomed && data.is_some_and(titled) {
                if let Some(work) = area_near(suggested, !dragged) {
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
            if let Some(data) = data {
                let ((sx, sy), (sw, sh)) = rect_parts(suggested);
                trace_window(
                    data,
                    "win.dpi_changed",
                    window_rect(hwnd),
                    &format!(
                        "dpi={dpi} suggested={sx},{sy},{sw}x{sh} limit={}x{} held={held} \
                         dragged={dragged}",
                        limit.0.min(99_999),
                        limit.1.min(99_999)
                    ),
                );
            }
            set_pos_traced(
                hwnd,
                std::ptr::null_mut(),
                (x, y, width, height),
                SWP_NOZORDER | SWP_NOACTIVATE,
                data,
                "dpi_changed",
            );
            if let Some(data) = data {
                data.rescaling.set(false);
                data.self_sizing.set(false);
                if titled(data) {
                    data.caption.paint(hwnd, data.presenter.borrow().as_deref());
                }
                // The title bar's cells follow the scale.
                sync_caption_buttons(hwnd, data);
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
                sync_caption_buttons(hwnd, data);
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
            // A client that is no screen reader read the tree or acted on it: the window
            // reads the task's tree once for it.
            if let Some(data) = window_data(hwnd) {
                if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
                    tree.serve(hwnd as isize);
                }
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
    if item == SystemMenuItem::FullScreen {
        toggle_full_screen(hwnd, data);
        return;
    }
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
    // Only the user's live drag of an edge holds the window to the frame it shows. Every other
    // size is the desktop placing the window (the end of a move loop that snaps, a snap layout,
    // Win+arrow, a maximise or restore): its rectangle is taken as it is, never replaced by
    // the old frame's size or moved to fit it. The guest was asked for the size above, the last
    // frame is shown 1:1 until the frame of the new size arrives, and a frame of another
    // size does not resize an arranged window (take_kept_frame).
    if !data.sizing.get() {
        return;
    }
    let Some(size) = presenter.size_for(proposed) else {
        return;
    };
    if size == proposed {
        return;
    }
    let (width, height) = window_outer(hwnd, size, dpi);
    // The edges the user drags, as WM_SIZING reported them. Comparing the proposal with the
    // current rectangle would miss a left or top edge drag that does not move that edge yet.
    let movable = pos.flags & SWP_NOMOVE == 0;
    let (left_edge, top_edge) = data.drag_edges.get();
    let (left_moved, top_moved) = (movable && left_edge, movable && top_edge);
    let (x, y, width, height) = frame_rect(FrameRectRequest {
        requested: (pos.x, pos.y, pos.cx, pos.cy),
        frame_outer: Some((width, height)),
        edge_drag: true,
        moves: (left_moved, top_moved),
    });
    if pos.flags & SWP_NOMOVE == 0 {
        pos.x = x;
        pos.y = y;
    }
    pos.cx = width;
    pos.cy = height;
}

/// Whether `hwnd` keeps the size `pos` proposes for it whatever size the guest gives its task:
/// it is maximised, full screen, or snapped by the desktop to a part of its monitor (a size
/// other than its restored one that neither the user's drag of an edge nor the window itself
/// chose).
unsafe fn holds_proposed_size(hwnd: HWND, data: &WindowData, pos: &WINDOWPOS) -> bool {
    let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
    placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
    let off_normal_size = GetWindowPlacement(hwnd, &mut placement) != 0 && {
        let normal = placement.rcNormalPosition;
        (normal.right - normal.left, normal.bottom - normal.top) != (pos.cx, pos.cy)
    };
    size_is_held(SizeProposal {
        maximized: IsZoomed(hwnd) != 0,
        fills_monitor: !titled(data),
        user_sizing: data.sizing.get(),
        host_sizing: data.self_sizing.get() || data.following.get() || data.rescaling.get(),
        off_normal_size,
    })
}

/// The window holds the size the desktop gave it from now on (`held`), or takes the size of
/// its frames again: its frames, its pointer input, its UI Automation tree and its input
/// method's cursor all follow where the frame is in it.
unsafe fn set_held(hwnd: HWND, data: &WindowData, held: bool) {
    if data.held.replace(held) == held {
        return;
    }
    data.pointer.set_held(held);
    if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
        tree.set_held(held);
    }
    if let Some(presenter) = data.presenter.borrow().as_deref() {
        presenter.set_held(held);
    }
    place_ime_cursor(hwnd, data);
}

/// Gives the input method the editor's cursor where the window shows it. The window manager
/// sends it in the window's pixels at the window's scale; a window that holds its size shows
/// its frame in its middle, at the scale that fits it there.
unsafe fn place_ime_cursor(hwnd: HWND, data: &WindowData) {
    let Some(cursor) = data.ime_cursor.get() else {
        return;
    };
    let mut client: RECT = std::mem::zeroed();
    GetClientRect(hwnd, &mut client);
    let view = (
        (client.right - client.left).max(0) as u32,
        (client.bottom - client.top).max(0) as u32,
    );
    let placed = match data.pointer.placement(view) {
        Some(placement) => {
            let scale = data.scale.get();
            let at = |value: i32, origin: u32| -> i32 {
                let task = to_task_position(f64::from(value), scale);
                (f64::from(origin) + to_window_position(task, placement.scale)).round() as i32
            };
            ImeCursor {
                left: at(cursor.left, placement.origin.0),
                top: at(cursor.top, placement.origin.1),
                right: at(cursor.right, placement.origin.0),
                bottom: at(cursor.bottom, placement.origin.1),
            }
        }
        None => cursor,
    };
    data.ime.cursor_moved(placed);
}

/// A frame of another size than the window's is kept for it: the window takes its size, and
/// the frame is shown with it (WM_SIZE). The edges the user is not dragging stay put.
unsafe fn take_kept_frame(hwnd: HWND, data: &WindowData) {
    // A window that holds the size the desktop gave it shows every frame in it instead.
    if IsIconic(hwnd) != 0 || data.held.get() {
        return;
    }
    // The window the user moves by its title bar keeps the size Windows gives it until the
    // drop; the frame is taken once the loop ends (WM_EXITSIZEMOVE).
    if data.in_move_loop.get() && data.loop_kind.get() != LoopKind::Size {
        return;
    }
    let Some(presenter) = data.presenter.borrow().clone() else {
        return;
    };
    let Some(size) = presenter.kept_size() else {
        return;
    };
    // A window the desktop arranged (snapped) keeps its rectangle: the frame is shown in it and
    // the guest is asked for the window's size instead of the window taking the frame's.
    if kept_frame_action(false, IsWindowArranged(hwnd) != 0) == KeptFrame::HoldAndAsk {
        set_held(hwnd, data, true);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let (frame_width, frame_height) = window_outer(hwnd, (0, 0), dpi);
        let (_, (width, height)) = window_rect(hwnd);
        let client = (
            (width as i32 - frame_width).max(0) as u32,
            (height as i32 - frame_height).max(0) as u32,
        );
        if data.reporting.get() && client.0 > 0 && client.1 > 0 && client != size {
            (data.report)(FromHost::Resized {
                task_id: data.task_id,
                width: client.0,
                height: client.1,
            });
        }
        return;
    }
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
    set_pos_traced(
        hwnd,
        std::ptr::null_mut(),
        (x, y, width, height),
        SWP_NOZORDER | SWP_NOACTIVATE,
        Some(data),
        "frame_arrived",
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

/// The outer size `hwnd` takes maximised: its client area is the work area of the monitor
/// nearest `on` (where Windows proposes to put it), or of its own monitor without one, and its
/// borders hang off the work area's edges. `None` when the monitor does not say.
unsafe fn maximized_window_outer(hwnd: HWND, on: Option<&RECT>) -> Option<(i32, i32)> {
    let work = match on {
        Some(rect) => work_area(rect),
        None => window_work_area(hwnd),
    }?;
    let dpi = GetDpiForWindow(hwnd).max(96);
    Some(maximized_outer(
        (work.right - work.left, work.bottom - work.top),
        outer_size((0, 0), dpi, true),
    ))
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

/// Follows the pointer over a window that fills its monitor: the title bar comes out when the
/// pointer reaches the top edge, and goes once the pointer is below its band or out of the
/// window. While it shows, the band's points are the title bar's (`WM_NCHITTEST`), so the
/// window hears the pointer over it as non-client moves, and a move in the client area below
/// is the pointer leaving it.
///
/// # Safety
///
/// `hwnd` is a live task window of the calling thread, and `data` its data.
unsafe fn follow_reveal(
    hwnd: HWND,
    data: &WindowData,
    message: u32,
    lparam: LPARAM,
    presenter: Option<&WindowPresenter>,
) {
    // A title bar the keyboard brought out stays until the keyboard or a press takes it away.
    if data.revealed_by_keys.get() {
        return;
    }
    let pointer_y = match message {
        WM_MOUSEMOVE => Some(((lparam >> 16) & 0xffff) as i16 as i32),
        WM_NCMOUSEMOVE | WM_NCMOUSELEAVE => {
            let mut point: POINT = std::mem::zeroed();
            GetCursorPos(&mut point);
            let mut client: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut client);
            ScreenToClient(hwnd, &mut point);
            let inside = point.x >= client.left
                && point.x < client.right
                && point.y >= client.top
                && point.y < client.bottom;
            inside.then_some(point.y)
        }
        _ => return,
    };
    let dpi = GetDpiForWindow(hwnd).max(96);
    let band = data.caption.layout(hwnd).height;
    let now = caption_revealed(data.revealed.get(), pointer_y, band, dpi);
    show_revealed(hwnd, data, now, presenter);
}

/// Shows a full screen window's title bar (`revealed`) or takes it away, and tells the
/// window's UI Automation tree its buttons are on the screen or off it.
///
/// # Safety
///
/// `hwnd` is a live task window of the calling thread, and `data` its data.
unsafe fn show_revealed(
    hwnd: HWND,
    data: &WindowData,
    revealed: bool,
    presenter: Option<&WindowPresenter>,
) {
    if data.revealed.replace(revealed) == revealed {
        return;
    }
    if revealed {
        data.caption.paint(hwnd, presenter);
    } else {
        data.caption.hide(presenter);
    }
    sync_caption_buttons(hwnd, data);
}

/// What a message does to a full screen window's hidden title bar, from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevealInput {
    /// Alt was pressed and released alone.
    AltTapped,
    /// Esc was pressed.
    Escape,
    /// The pointer pressed the app, below the title bar.
    PressInApp,
}

/// The title bar of a full screen window after `input`, as (whether it shows, whether the
/// keyboard brought it out), from whether it shows now (`revealed`) and whether the keyboard
/// brought it out (`by_keys`); `None` when the input changes nothing. Alt alone brings it out
/// and keeps it out, as Alt shows a menu bar; Alt again, Esc or a press in the app takes away
/// what the keyboard brought out. The pointer's reveal follows the pointer instead
/// ([`caption_revealed`]), so Esc and presses leave it to the app.
fn keyboard_reveal(revealed: bool, by_keys: bool, input: RevealInput) -> Option<(bool, bool)> {
    let held = revealed && by_keys;
    match input {
        RevealInput::AltTapped if held => Some((false, false)),
        RevealInput::AltTapped => Some((true, true)),
        RevealInput::Escape | RevealInput::PressInApp if held => Some((false, false)),
        RevealInput::Escape | RevealInput::PressInApp => None,
    }
}

/// Follows the keys and presses that bring a full screen window's title bar out from the
/// keyboard or take it away. Alt goes on to the guest as it always does; only the Esc that
/// hides the title bar is taken (`Some`), as Esc closes a menu without reaching the app.
///
/// # Safety
///
/// `hwnd` is a live task window of the calling thread, and `data` its data.
unsafe fn reveal_message(
    hwnd: HWND,
    data: &WindowData,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    let mut tap = data.alt_tap.get();
    let input = match message {
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wparam as u16;
            let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
            let repeat = down && (lparam >> 30) & 1 != 0;
            if tap.key(vk, down, repeat, other_modifiers_held()) {
                Some(RevealInput::AltTapped)
            } else if down && vk == VK_ESCAPE {
                Some(RevealInput::Escape)
            } else {
                None
            }
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_POINTERDOWN => {
            tap.interrupt();
            Some(RevealInput::PressInApp)
        }
        // Alt with a click on the title bar, a turn of the wheel, or the focus going (Alt+Tab
        // takes it) is not Alt alone.
        WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN | WM_MOUSEWHEEL | WM_KILLFOCUS => {
            tap.interrupt();
            None
        }
        _ => None,
    };
    data.alt_tap.set(tap);
    let input = input?;
    if titled(data) {
        return None;
    }
    let (revealed, by_keys) =
        keyboard_reveal(data.revealed.get(), data.revealed_by_keys.get(), input)?;
    data.revealed_by_keys.set(by_keys);
    let presenter = data.presenter.borrow().clone();
    show_revealed(hwnd, data, revealed, presenter.as_deref());
    (input == RevealInput::Escape).then_some(0)
}

/// What a point of a full screen window's revealed band is: the band's buttons and its icon as
/// in any window, and nothing elsewhere, where an ordinary window would drag or resize.
fn full_screen_hit(code: LRESULT) -> LRESULT {
    let code = code as u32;
    if code == HTCAPTION || (HTLEFT..=HTBOTTOMRIGHT).contains(&code) {
        HTNOWHERE as LRESULT
    } else {
        code as LRESULT
    }
}

/// Turns full screen on or off over what the app asked for, as F11 and the window menu's Full
/// screen do, whatever the user's setting for the app's full screen.
///
/// # Safety
///
/// `hwnd` is a live task window of the calling thread, and `data` its data.
unsafe fn toggle_full_screen(hwnd: HWND, data: &WindowData) {
    let mut mode = data.mode.get();
    let shown = mode.toggle_fullscreen();
    data.mode.set(mode);
    apply_mode(hwnd, data, shown);
    sync_caption_buttons(hwnd, data);
}

/// Whether the window has its title bar for good: not while it fills its monitor, where it
/// shows one only while the pointer is at the top edge.
fn titled(data: &WindowData) -> bool {
    data.shown_mode.get() != WindowMode::Fullscreen
}

/// Tells the window's UI Automation tree where the title bar's buttons are now and what they
/// say: the icon's cell (the back arrow in it while the task can go back), minimize, maximize
/// (Restore while the window is maximized or fills its monitor) and close, off the screen
/// while a window that fills its monitor hides its title bar. Its buttons come, go, move and
/// are renamed with the title bar.
unsafe fn sync_caption_buttons(hwnd: HWND, data: &WindowData) {
    let full_screen = !titled(data);
    let cells = CaptionCells::of(
        &data.caption.layout(hwnd),
        data.caption.shows_back(),
        full_screen || IsZoomed(hwnd) != 0,
        // A window that fills its monitor can always leave it from its middle button.
        full_screen || data.resizable.get(),
        full_screen && !data.revealed.get(),
    );
    if let Ok(mut tree) = data.accessibility.try_borrow_mut() {
        tree.set_caption(hwnd as isize, cells);
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

/// The placement changes the window's maximised, minimised or restored state. Windows sets it
/// in WM_WINDOWPOSCHANGING and WM_WINDOWPOSCHANGED; windows-sys leaves it out.
const SWP_STATECHANGED: u32 = 0x8000;

/// `hwnd`'s window rectangle as position and size.
unsafe fn window_rect(hwnd: HWND) -> ((i32, i32), (u32, u32)) {
    let mut rect: RECT = std::mem::zeroed();
    GetWindowRect(hwnd, &mut rect);
    rect_parts(&rect)
}

fn rect_parts(rect: &RECT) -> ((i32, i32), (u32, u32)) {
    (
        (rect.left, rect.top),
        (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        ),
    )
}

/// The `SWP_` flags of `flags` a placement trace cares about, as `flags=nomove|nosize`.
fn swp_words(flags: u32) -> String {
    let words: Vec<&str> = [
        (SWP_NOMOVE, "nomove"),
        (SWP_NOSIZE, "nosize"),
        (SWP_FRAMECHANGED, "framechanged"),
        (SWP_STATECHANGED, "statechanged"),
    ]
    .iter()
    .filter(|(flag, _)| flags & flag != 0)
    .map(|(_, word)| *word)
    .collect();
    if words.is_empty() {
        "flags=-".to_string()
    } else {
        format!("flags={}", words.join("|"))
    }
}

/// Logs a resize trace placement line for `hwnd`'s task.
fn trace_window(
    data: &WindowData,
    stage: &'static str,
    (pos, size): ((i32, i32), (u32, u32)),
    detail: &str,
) {
    trace_place(
        data.logger.as_deref(),
        data.task_id,
        stage,
        pos,
        size,
        detail,
    );
}

/// Whether a placement with `flags` that leaves the window `size` belongs in the trace: not
/// a change of Z order or activation alone, and not a plain move during a drag, which
/// `win.move_loop` already samples.
unsafe fn placement_worth_tracing(data: &WindowData, flags: u32, size: (u32, u32)) -> bool {
    if flags & (SWP_FRAMECHANGED | SWP_STATECHANGED) != 0 {
        return true;
    }
    if flags & SWP_NOMOVE != 0 && flags & SWP_NOSIZE != 0 {
        return false;
    }
    let resized = flags & SWP_NOSIZE == 0 && data.traced_size.replace(size) != size;
    resized || !data.in_move_loop.get()
}

/// Logs `win.pos_changing`: the rectangle Windows `asked` for and, when DSA changed it,
/// the one it lets through (`gave`) and the steps that changed it.
unsafe fn trace_pos_changing(
    hwnd: HWND,
    data: &WindowData,
    asked: (i32, i32, i32, i32, u32),
    gave: (i32, i32, i32, i32, u32),
    why: &[&str],
) {
    let (x, y, w, h, flags) = asked;
    let size = (w.max(0) as u32, h.max(0) as u32);
    // Z order or activation alone, or a plain move during a drag, says nothing new.
    let plain = flags & (SWP_FRAMECHANGED | SWP_STATECHANGED) == 0;
    let idle = flags & SWP_NOMOVE != 0 && flags & SWP_NOSIZE != 0;
    let same_size = flags & SWP_NOSIZE != 0 || window_rect(hwnd).1 == size;
    if plain && asked == gave && (idle || (data.in_move_loop.get() && same_size)) {
        return;
    }
    let mut detail = swp_words(flags);
    if asked != gave {
        let (gx, gy, gw, gh, gflags) = gave;
        let _ = write!(
            detail,
            " gave={gx},{gy},{gw}x{gh} gave_{} why={}",
            swp_words(gflags),
            why.join("+")
        );
    }
    trace_window(data, "win.pos_changing", ((x, y), size), &detail);
}

/// Where `hwnd` is and in what state, for the `win.pos_changed` line: its visible frame,
/// its monitor's origin and scale, and whether it is maximised, arranged (snapped) or
/// restored.
unsafe fn placement_words(hwnd: HWND) -> String {
    let mut frame: RECT = std::mem::zeroed();
    let framed = DwmGetWindowAttribute(
        hwnd,
        DWMWA_EXTENDED_FRAME_BOUNDS as u32,
        &mut frame as *mut RECT as *mut _,
        std::mem::size_of::<RECT>() as u32,
    ) >= 0;
    let visible = if framed {
        let ((x, y), (w, h)) = rect_parts(&frame);
        format!("vis={x},{y},{w}x{h}")
    } else {
        "vis=-".to_string()
    };
    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    let screen = if GetMonitorInfoW(monitor, &mut info) != 0 {
        let ((x, y), (w, h)) = rect_parts(&info.rcMonitor);
        format!("mon={x},{y},{w}x{h}")
    } else {
        "mon=-".to_string()
    };
    let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
    placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
    let show = if GetWindowPlacement(hwnd, &mut placement) != 0 {
        placement.showCmd as i32
    } else {
        -1
    };
    format!(
        "{visible} {screen} dpi={} zoomed={} arranged={} show={show}",
        GetDpiForWindow(hwnd),
        IsZoomed(hwnd) != 0,
        IsWindowArranged(hwnd) != 0,
    )
}

/// `SetWindowPos` by DSA itself, with the window's rectangle before and after in the resize
/// trace (`wm.set_pos`) and `why` it was placed.
unsafe fn set_pos_traced(
    hwnd: HWND,
    after: HWND,
    rect: (i32, i32, i32, i32),
    flags: u32,
    data: Option<&WindowData>,
    why: &str,
) {
    let traced = data.filter(|_| trace_detailed());
    let before = window_rect(hwnd);
    SetWindowPos(hwnd, after, rect.0, rect.1, rect.2, rect.3, flags);
    // Every size DSA gives a window itself is logged with its reason, trace or no trace: a
    // window that changes size without the user is the first thing a report asks about.
    if let Some(data) = data {
        let after_size = window_rect(hwnd).1;
        if after_size != before.1 {
            note(
                data.logger.as_deref(),
                &format!(
                    "task {}: the window was resized from {}x{} to {}x{}: {}",
                    data.task_id,
                    before.1 .0,
                    before.1 .1,
                    after_size.0,
                    after_size.1,
                    size_change_reason(why)
                ),
            );
        }
    }
    if let Some(data) = traced {
        let ((bx, by), (bw, bh)) = before;
        let detail = format!(
            "why={why} asked={},{},{}x{} {} before={bx},{by},{bw}x{bh}",
            rect.0,
            rect.1,
            rect.2,
            rect.3,
            swp_words(flags)
        );
        trace_window(data, "wm.set_pos", window_rect(hwnd), &detail);
    }
}

/// Why DSA gave a window a size itself, in words, for the `why` [`set_pos_traced`] is called
/// with. The tag itself follows, so the line can be matched with the resize trace.
fn size_change_reason(why: &str) -> String {
    let words = match why {
        "dpi_changed" => {
            "the window moved onto a monitor of another scale and took the size Windows \
             suggests there, which keeps it the same size to the eye"
        }
        "frame_arrived" => "the app drew a frame of another size, and the window took its size",
        "guest_size" => "Android gave the task another size",
        "guest_size_fullscreen" => "Android gave the task another size in full screen",
        "mode_titled" => "the window got its title bar back",
        "mode_untopmost" => "the window left picture in picture",
        "mode_pip" => "the window went to picture in picture",
        _ => "DSA placed the window",
    };
    format!("{words} ({why})")
}

/// Sizes and places `hwnd` for itself, not as the user: a fixed-size window takes it too.
unsafe fn place_self(hwnd: HWND, after: HWND, rect: (i32, i32, i32, i32), flags: u32, why: &str) {
    let data = window_data(hwnd);
    if let Some(data) = data {
        data.self_sizing.set(true);
    }
    set_pos_traced(hwnd, after, rect, flags, data, why);
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
                toggle_full_screen(hwnd, data);
            }
            Some(0)
        }
        // The middle button of a full screen window's revealed title bar, and Restore or
        // Maximize from its menu, leave full screen.
        WM_SYSCOMMAND
            if data.shown_mode.get() == WindowMode::Fullscreen
                && matches!((wparam & 0xfff0) as u32, SC_MAXIMIZE | SC_RESTORE) =>
        {
            toggle_full_screen(hwnd, data);
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
    // A window that fills its monitor starts with its title bar hidden.
    data.revealed.set(false);
    data.revealed_by_keys.set(false);
    data.caption.set_full_screen(mode == WindowMode::Fullscreen);
    let keep = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    match previous {
        WindowMode::Fullscreen => {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let style = (style & !WS_POPUP) | TASK_STYLE;
            SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
            let flags = keep | SWP_NOZORDER | SWP_FRAMECHANGED;
            place_self(
                hwnd,
                std::ptr::null_mut(),
                (0, 0, 0, 0),
                flags,
                "mode_titled",
            );
            // The title bar is back, and the app's top inset with it.
            data.caption.report_height(hwnd, &*data.report);
            data.caption
                .repaint(hwnd, data.presenter.borrow().as_deref());
        }
        WindowMode::PictureInPicture => {
            place_self(hwnd, HWND_NOTOPMOST, (0, 0, 0, 0), keep, "mode_untopmost");
        }
        // A task's window is never an overlay's (`super::overlay`).
        WindowMode::Normal | WindowMode::Overlay => {}
    }
    match mode {
        WindowMode::Normal | WindowMode::Overlay => {
            if let Some(placement) = data.restore.take() {
                data.self_sizing.set(true);
                let before = window_rect(hwnd);
                SetWindowPlacement(hwnd, &placement);
                data.self_sizing.set(false);
                if trace_detailed() {
                    let ((bx, by), (bw, bh)) = before;
                    let ((nx, ny), (nw, nh)) = rect_parts(&placement.rcNormalPosition);
                    let detail = format!(
                        "why=mode_restore placement=1 normal={nx},{ny},{nw}x{nh} show={} \
                         before={bx},{by},{bw}x{bh}",
                        placement.showCmd
                    );
                    trace_window(data, "wm.set_pos", window_rect(hwnd), &detail);
                }
            }
        }
        WindowMode::Fullscreen => {
            let Some(monitor) = monitor_rect(hwnd) else {
                return;
            };
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            // It keeps its system menu, for Alt+Space and its revealed title bar's icon.
            let style = (style & !TASK_STYLE) | WS_POPUP | WS_SYSMENU;
            SetWindowLongPtrW(hwnd, GWL_STYLE, style as isize);
            // No title bar, so no top inset: the app draws to the monitor's top edge. The band
            // is forgotten with it, so the title bar's return reports it again.
            data.caption.report_none(&*data.report);
            data.caption.hide(data.presenter.borrow().as_deref());
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
                "mode_fullscreen",
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
            place_self(hwnd, HWND_TOPMOST, rect, SWP_NOACTIVATE, "mode_pip");
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

/// The work area (`work`) or the whole of the monitor nearest `rect`.
unsafe fn area_near(rect: &RECT, work: bool) -> Option<RECT> {
    let monitor = MonitorFromRect(rect, MONITOR_DEFAULTTONEAREST);
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (GetMonitorInfoW(monitor, &mut info) != 0).then_some(if work {
        info.rcWork
    } else {
        info.rcMonitor
    })
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
    // A maximized, snapped or full screen window keeps the size the desktop gave it: resizing
    // it would shrink it inside its place on the monitor, and the guest would be asked for a
    // size nobody chose. A size the guest gives its task is shown in it instead.
    if IsZoomed(hwnd) != 0 || window_data(hwnd).is_some_and(|data| data.held.get()) {
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
                "guest_size_fullscreen",
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
        "guest_size",
    );
    client
}

impl Ui {
    fn log(&self, message: &str) {
        note(self.setup.logger.as_deref(), message);
    }

    fn drain_commands(&mut self) {
        while let Ok(command) = self.commands.try_recv() {
            // An app overlay window is a window of another kind (`super::overlay`): no title
            // bar, no close button, above the others, placed where its app has it.
            if command.task_id().is_some_and(super::overlay::is_overlay) {
                let beside = self.open.values().next().copied();
                // SAFETY: on the windows' thread.
                unsafe { super::overlay::command(&self.setup, beside, command) };
                continue;
            }
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
                        let hwnd = self.open.get(&task_id).copied();
                        if let Some((hwnd, data)) =
                            hwnd.and_then(|hwnd| Some((hwnd, window_data(hwnd)?)))
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
                            if let Some(presenter) = data.presenter.borrow().as_deref() {
                                presenter.set_limit(max);
                            }
                            // A window that holds its size shows a frame of the new size
                            // elsewhere in it.
                            if data.held.get() {
                                place_ime_cursor(hwnd, data);
                            }
                        }
                    }
                    ToHost::ImeCursor { task_id, cursor } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                data.ime_cursor.set(Some(cursor));
                                place_ime_cursor(*hwnd, data);
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
                                sync_caption_buttons(*hwnd, data);
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
                        below,
                    } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                let presenter = data.presenter.borrow().clone();
                                data.caption.set_mode(
                                    *hwnd,
                                    app_drawn,
                                    interactive,
                                    below,
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
                                sync_caption_buttons(*hwnd, data);
                            }
                        }
                    }
                    // The window host's `WindowTasks` takes it, before it reaches this thread;
                    // the window itself shows the same.
                    ToHost::ShownTask { .. } => {}
                    // Only an overlay's window is placed by its app, and those are taken
                    // above.
                    ToHost::Overlay { .. } => {}
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
            ime: ImeWindow::new(task_id, self.setup.input.clone(), self.setup.logger.clone()),
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
            move_loop: Cell::new(MoveLoop::default()),
            in_move_loop: Cell::new(false),
            loop_kind: Cell::new(LoopKind::None),
            traced_size: Cell::new((0, 0)),
            sizing: Cell::new(false),
            held: Cell::new(false),
            ime_cursor: Cell::new(None),
            following: Cell::new(false),
            mode: Cell::new(ModeState::new()),
            shown_mode: Cell::new(WindowMode::Normal),
            revealed: Cell::new(false),
            revealed_by_keys: Cell::new(false),
            alt_tap: Cell::new(AltTap::default()),
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
        // The input method composes in the window's own document, made once the guest reports
        // an editor that takes text.
        if let Some(data) = window_data(hwnd) {
            data.ime.attach(hwnd);
        }
        window_marks::mark_task(hwnd as isize, task_id);
        // The Android window menu, under Windows' own items.
        system_menu::apply(hwnd, self.new_window);
        // The app's icon, when it changed after the program was made.
        if let Some(data) = window_data(hwnd) {
            take_live_icon(hwnd, data, false);
            // The tone the title bar's glyphs take, and why, once per change.
            let logger = self.setup.logger.clone();
            data.caption
                .set_log(Box::new(move |line| note(logger.as_deref(), line)));
        }
        // Files dropped from Explorer go to the task's app.
        if let Some(handler) = &self.setup.drops {
            if let Err(why) = drop_target::register(hwnd as isize, task_id, Arc::clone(handler)) {
                self.log(&why);
            }
        }
        let client = apply_size(hwnd, size);
        let presenter = match WindowPresenter::new(
            task_id,
            Arc::clone(&self.setup.surfaces),
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
        };
        if let Some(data) = window_data(hwnd) {
            *data.presenter.borrow_mut() = presenter.clone();
            if let Some(presenter) = presenter.as_deref() {
                presenter.set_held(data.held.get());
                presenter.set_limit(data.max_client.get());
            }
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

/// The move loop moved the window or dragged an edge: the first time, the user's drag starts.
fn report_drag_started(data: &WindowData) {
    let mut move_loop = data.move_loop.get();
    let started = move_loop.moved();
    data.move_loop.set(move_loop);
    if let Some(dragging) = started {
        (data.report)(FromHost::Dragging {
            task_id: data.task_id,
            dragging,
        });
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

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        VK_F4, VK_LMENU, VK_MENU, VK_RMENU, VK_SPACE, VK_TAB, VK_X,
    };

    /// Alt pressed and released with `between` pressed and released in the middle; whether
    /// the release of Alt was Alt alone.
    fn alt_around(tap: &mut AltTap, alt: u16, between: &[u16]) -> bool {
        assert!(!tap.key(alt, true, false, false));
        for key in between {
            assert!(!tap.key(*key, true, false, false));
            assert!(!tap.key(*key, false, false, false));
        }
        tap.key(alt, false, false, false)
    }

    #[test]
    fn fr18_alt_alone_reveals_the_full_screen_caption() {
        let mut tap = AltTap::default();
        for alt in [VK_MENU, VK_LMENU, VK_RMENU] {
            assert!(alt_around(&mut tap, alt, &[]), "alt {alt:#x}");
        }
        // Held down, Alt repeats; it is still Alt alone.
        assert!(!tap.key(VK_MENU, true, false, false));
        assert!(!tap.key(VK_MENU, true, true, false));
        assert!(!tap.key(VK_MENU, true, true, false));
        assert!(tap.key(VK_MENU, false, false, false));
        // The release brings a hidden title bar out, held by the keyboard.
        assert_eq!(
            keyboard_reveal(false, false, RevealInput::AltTapped),
            Some((true, true))
        );
        // Over a title bar the pointer brought out, the keyboard now holds it.
        assert_eq!(
            keyboard_reveal(true, false, RevealInput::AltTapped),
            Some((true, true))
        );
        // Alt alone again takes it away, as it closes a menu bar.
        assert_eq!(
            keyboard_reveal(true, true, RevealInput::AltTapped),
            Some((false, false))
        );
    }

    #[test]
    fn fr18_alt_with_another_key_does_not_reveal() {
        let mut tap = AltTap::default();
        // Alt+Space opens the window menu, Alt+F4 closes the window, Alt+Tab switches
        // windows, and Alt+X is the app's shortcut: none of them is Alt alone.
        for key in [VK_SPACE, VK_F4, VK_TAB, VK_X] {
            assert!(!alt_around(&mut tap, VK_MENU, &[key]), "key {key:#x}");
        }
        // Alt with Ctrl, Shift or a Windows key held is a shortcut too, whichever went first.
        assert!(!tap.key(VK_MENU, true, false, true));
        assert!(!tap.key(VK_MENU, false, false, true));
        assert!(!tap.key(VK_MENU, true, false, false));
        assert!(!tap.key(VK_MENU, false, false, true));
        // A press of the pointer, or the focus going, while Alt is down.
        assert!(!tap.key(VK_MENU, true, false, false));
        tap.interrupt();
        assert!(!tap.key(VK_MENU, false, false, false));
        // A key released while Alt is down (pressed before it) is not Alt alone either.
        assert!(!tap.key(VK_MENU, true, false, false));
        assert!(!tap.key(VK_X, false, false, false));
        assert!(!tap.key(VK_MENU, false, false, false));
        // A release of Alt with no press of it (pressed in another window) is nothing.
        assert!(!tap.key(VK_MENU, false, false, false));
        // And alone again, it still works.
        assert!(alt_around(&mut tap, VK_MENU, &[]));
    }

    #[test]
    fn fr18_escape_or_a_press_in_the_app_hides_the_revealed_caption() {
        for input in [RevealInput::Escape, RevealInput::PressInApp] {
            // What the keyboard brought out, Esc and a press take away.
            assert_eq!(keyboard_reveal(true, true, input), Some((false, false)));
            // A hidden title bar stays hidden, and Esc goes to the app.
            assert_eq!(keyboard_reveal(false, false, input), None);
            // The pointer's reveal follows the pointer; Esc and presses are the app's.
            assert_eq!(keyboard_reveal(true, false, input), None);
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
                range: None,
                band: None,
            },
            BandSample {
                fill: None,
                luminance: Some(0),
                range: Some((0, 0)),
                band: None,
            },
            BandSample {
                fill: Some(0xFF00_0000),
                luminance: Some(1000),
                range: Some((1000, 1000)),
                band: None,
            },
            // A light grey bar with a darker part: what turns an app's light glyphs dark.
            BandSample {
                fill: None,
                luminance: Some(745),
                range: Some((402, 913)),
                band: None,
            },
            // A bar of one color, which the band of an app below the title bar takes.
            BandSample {
                fill: None,
                luminance: Some(745),
                range: Some((745, 745)),
                band: Some(0xFFE0_E0E0),
            },
            BandSample {
                fill: Some(0xFF3F_51B5),
                luminance: Some(0),
                range: Some((0, 0)),
                band: Some(0xFF00_0000),
            },
        ] {
            let (wparam, lparam) = pack_band_sample(sample);
            assert_eq!(unpack_band_sample(wparam, lparam), sample);
        }
    }

    #[test]
    fn fr22_a_still_window_whose_only_frame_has_a_light_band_ends_with_dark_glyphs() {
        use dsa_host_core::compositor::caption::{frame_band_sample, CaptionTone};
        // MX Player's settings screen in R51's window: 1439 task pixels wide, a band of 58, a
        // light grey bar (#E0E0E0) over the first 105 rows, and an app that never asks for
        // dark status bar icons, which is read as a dark bar.
        let (width, height, band) = (1439u32, 1166u32, 58u32);
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            let color = if y < 105 {
                [0xE0, 0xE0, 0xE0, 0xFF]
            } else {
                [0xFF, 0xFF, 0xFF, 0xFF]
            };
            for _ in 0..width {
                rgba.extend_from_slice(&color);
            }
        }
        let (input, _guest) = std::sync::mpsc::channel();
        let caption = CaptionWindow::new(7, TASK_STYLE, TASK_EX_STYLE, ICON_ID, input);
        let lines = std::rc::Rc::new(RefCell::new(Vec::<String>::new()));
        let told = std::rc::Rc::clone(&lines);
        caption.set_log(Box::new(move |line| {
            told.borrow_mut().push(line.to_string())
        }));
        let hwnd: HWND = std::ptr::null_mut();
        // SAFETY: with no presenter the window draws nothing; the frame's mode is asked of a
        // null window, which Windows refuses and changes nothing.
        unsafe {
            caption.set_tone(hwnd, Some(CaptionTone::LightGlyphs), None);
        }
        assert_eq!(caption.glyph_tone(), CaptionTone::LightGlyphs);
        // The window is still: the presenter reads the band of its one frame, the window is
        // told it in a message, and that is all that ever comes.
        let sample = frame_band_sample(&rgba, width, height, band);
        let (wparam, lparam) = pack_band_sample(sample);
        // SAFETY: as above.
        unsafe {
            caption.set_sample(hwnd, unpack_band_sample(wparam, lparam), None);
        }
        assert_eq!(caption.glyph_tone(), CaptionTone::DarkGlyphs, "{sample:?}");
        // Said once each time the tone changes, with the rule and the numbers.
        let said = lines.borrow().clone();
        assert_eq!(said.len(), 2, "{said:?}");
        assert!(said[0].contains("take light glyphs"), "{said:?}");
        assert!(said[1].starts_with("task 7: "), "{said:?}");
        assert!(said[1].contains("take dark glyphs"), "{said:?}");
        assert!(
            said[1].contains("cannot be read anywhere on the band (darkest 745, lightest 745"),
            "{said:?}"
        );
        // Read again, the same band changes nothing and says nothing.
        // SAFETY: as above.
        unsafe {
            caption.set_sample(hwnd, sample, None);
            caption.set_tone(hwnd, Some(CaptionTone::LightGlyphs), None);
        }
        assert_eq!(caption.glyph_tone(), CaptionTone::DarkGlyphs);
        assert_eq!(lines.borrow().len(), 2);
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
