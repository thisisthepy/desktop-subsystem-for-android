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
//! and its input method messages and typed characters to its `ImeWindow` ([`super::ime`]),
//! which sends the task's text on and places the input method's windows at the cursor the
//! guest reports.
//!
//! A window reports its client size, its monitor's scale (once when it opens, then on every
//! move to another monitor) and the start and end of a drag; the window manager turns them
//! into the task's size and density. It never grows past the size the window manager says
//! Android can draw, not even when maximised: on a monitor larger than that, a maximised
//! window takes that size, centred on the work area, so the task stays 1:1. It shows its
//! last frame stretched over a new size only until a frame of that size arrives. While the
//! user drags an edge it holds a new size back for up to 60 ms until that frame is in, so
//! the stretch is rarely seen at all.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use dsa_host_core::compositor::resize_sync::ResizeSync;
use dsa_host_core::compositor::task_windows::fit_within;
use dsa_host_core::compositor::window_hosts::{FromHost, ToHost};
use dsa_host_core::compositor::window_sizing::maximized_position;
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
    AdjustWindowRectExForDpi, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, IsZoomed, KillTimer, LoadCursorW, LoadIconW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateMessage, CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA, HWND_MESSAGE, IDC_ARROW,
    MINMAXINFO, MSG, SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SW_SHOW,
    SW_SHOWNOACTIVATE, WA_INACTIVE, WINDOWPOS, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_APP,
    WM_CLOSE, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO,
    WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE, WM_TIMER, WM_WINDOWPOSCHANGING, WNDCLASSEXW,
    WS_EX_APPWINDOW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};

use crate::compositor::log::{note, CompositorLogger};
use crate::compositor::present::{Gpu, WindowPresenter};

use super::ime::ImeWindow;
use super::keys::KeyWindow;
use super::pointer::PointerWindow;

/// A task window that is open, as tests and logs see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    pub task_id: i32,
    /// The window's handle, as an integer.
    pub hwnd: isize,
    /// Where the window places the input method's windows: the cursor of the task's focused
    /// editor, as the guest last reported it.
    pub ime_cursor: Option<dsa_host_core::ime::ImeCursor>,
}

/// Where a report about a window goes: to the window manager.
pub type Report = Arc<dyn Fn(FromHost) + Send + Sync>;

/// The windows' presenters by task, which the connection thread shows frames through.
pub type Presenters = Arc<Mutex<HashMap<i32, Arc<WindowPresenter>>>>;

const WM_APP_COMMANDS: u32 = WM_APP + 1;
const WM_APP_QUIT: u32 = WM_APP + 2;

/// The message window's timer, which reports windows left showing a stretched frame.
const TICK_TIMER: usize = 1;
const TICK_MILLIS: u32 = 200;

const TASK_CLASS: &str = "DsaAppWindow";
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
    pub fn send(&self, command: ToHost) -> bool {
        if self.commands.send(command).is_err() {
            return false;
        }
        // SAFETY: posting to a window of another thread; a stale handle only fails.
        unsafe { PostMessageW(self.message_window as HWND, WM_APP_COMMANDS, 0, 0) != 0 }
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
    /// The largest client area Android can draw for the task, as the window manager last
    /// said, for WM_GETMINMAXINFO.
    max_client: Cell<Option<(u32, u32)>>,
    /// Whether the window reports its size: not until it has been given the task's.
    reporting: Cell<bool>,
    /// Whether the window is minimized, so a restore is reported once.
    minimized: Cell<bool>,
    /// Whether a step of a drag waits for its frame.
    sync: Cell<ResizeSync>,
}

/// The UI thread's state, reached from the message window's procedure.
struct Ui {
    open: HashMap<i32, HWND>,
    commands: Receiver<ToHost>,
    setup: UiSetup,
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
            });
        });
        let _ = ready.send(Ok(message_window as isize));
        SetTimer(message_window, TICK_TIMER, TICK_MILLIS, None);

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

unsafe extern "system" fn task_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if let Some(data) = window_data(hwnd) {
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
                    let presenter = data.presenter.borrow().clone();
                    let report = |data: &WindowData| {
                        // Not while the window is being made, when its size is not yet the
                        // task's.
                        if data.reporting.get() {
                            (data.report)(FromHost::Resized {
                                task_id: data.task_id,
                                width,
                                height,
                            });
                        }
                    };
                    let view = (width, height);
                    match presenter {
                        // Dragged: the guest is told, then the size is held back from the
                        // screen until its frame is in, so Windows shows the new size
                        // together with content of that size. The last frame stays 1:1 at
                        // the top left meanwhile, and is stretched only if the frame does
                        // not come in time.
                        Some(presenter)
                            if data.sync.get().should_wait()
                                && data.reporting.get()
                                && presenter.has_frame() =>
                        {
                            let since = std::time::Instant::now();
                            if presenter.take_view(view) {
                                report(data);
                                let in_time = presenter.wait_for_frame(view, since);
                                presenter.fit_view();
                                let mut sync = data.sync.get();
                                sync.step_ended(in_time);
                                data.sync.set(sync);
                            }
                        }
                        Some(presenter) => {
                            // Inside Windows' sizing loop the last frame is stretched to the
                            // new size before it is shown.
                            presenter.view_resized(view);
                            report(data);
                        }
                        None => report(data),
                    }
                }
            }
            0
        }
        WM_ENTERSIZEMOVE | WM_EXITSIZEMOVE => {
            if let Some(data) = window_data(hwnd) {
                let mut sync = data.sync.get();
                if message == WM_ENTERSIZEMOVE {
                    sync.drag_started();
                } else {
                    sync.drag_ended();
                }
                data.sync.set(sync);
                (data.report)(FromHost::Dragging {
                    task_id: data.task_id,
                    dragging: message == WM_ENTERSIZEMOVE,
                });
            }
            0
        }
        WM_GETMINMAXINFO => {
            // Never larger than Android can draw: a larger window could only show the
            // task's frames stretched. Maximising takes this size too.
            if let Some(max) = window_data(hwnd).and_then(|data| data.max_client.get()) {
                let info = &mut *(lparam as *mut MINMAXINFO);
                let (width, height) = outer_size(max, GetDpiForWindow(hwnd).max(96));
                info.ptMaxTrackSize = POINT {
                    x: width.min(info.ptMaxTrackSize.x),
                    y: height.min(info.ptMaxTrackSize.y),
                };
                info.ptMaxSize = POINT {
                    x: width.min(info.ptMaxSize.x),
                    y: height.min(info.ptMaxSize.y),
                };
            }
            0
        }
        WM_WINDOWPOSCHANGING => {
            let result = DefWindowProcW(hwnd, message, wparam, lparam);
            // A maximised window takes the most Android can draw (WM_GETMINMAXINFO above),
            // shown 1:1. On a monitor larger than that it is centred on the work area rather
            // than left in its top left corner.
            let pos = &mut *(lparam as *mut WINDOWPOS);
            if IsZoomed(hwnd) != 0 && pos.flags & (SWP_NOMOVE | SWP_NOSIZE) == 0 {
                let proposed = RECT {
                    left: pos.x,
                    top: pos.y,
                    right: pos.x + pos.cx,
                    bottom: pos.y + pos.cy,
                };
                if let Some(work) = work_area(&proposed) {
                    pos.x = maximized_position(pos.x, work.left, work.right - work.left, pos.cx);
                    pos.y = maximized_position(pos.y, work.top, work.bottom - work.top, pos.cy);
                }
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
            }
            // A per-monitor aware window is handed the rectangle that keeps its size in
            // points on the new monitor, and applies it itself, within what Android can
            // draw.
            let suggested = &*(lparam as *const RECT);
            let (mut width, mut height) = (
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
            );
            if let Some(max) = data.and_then(|data| data.max_client.get()) {
                let (max_width, max_height) = outer_size(max, dpi.max(96));
                width = width.min(max_width);
                height = height.min(max_height);
            }
            SetWindowPos(
                hwnd,
                std::ptr::null_mut(),
                suggested.left,
                suggested.top,
                width,
                height,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            0
        }
        WM_ACTIVATE => {
            if let Some(data) = window_data(hwnd) {
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
        WM_NCDESTROY => {
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

/// The outer size of a window whose client area is `client` pixels at `dpi`.
unsafe fn outer_size(client: (u32, u32), dpi: u32) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: client.0 as i32,
        bottom: client.1 as i32,
    };
    AdjustWindowRectExForDpi(&mut rect, TASK_STYLE, 0, TASK_EX_STYLE, dpi);
    (rect.right - rect.left, rect.bottom - rect.top)
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
    let (frame_width, frame_height) = outer_size((0, 0), dpi);
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
    let (outer_width, outer_height) = outer_size(client, dpi);
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        0,
        0,
        outer_width,
        outer_height,
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
                        }
                    }
                    ToHost::Resize {
                        task_id,
                        width,
                        height,
                    } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                data.pointer.task_resized((width, height));
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
                    } => {
                        if let Some(data) =
                            self.open.get(&task_id).and_then(|hwnd| window_data(*hwnd))
                        {
                            data.pointer.task_resized((width, height));
                            data.max_client.set(max);
                        }
                    }
                    ToHost::ImeCursor { task_id, cursor } => {
                        if let Some(hwnd) = self.open.get(&task_id) {
                            if let Some(data) = window_data(*hwnd) {
                                data.ime.cursor_moved(*hwnd, cursor);
                            }
                        }
                    }
                }
            }
        }
        self.publish();
    }

    /// Logs the windows left showing a stretched frame.
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
        if let Some(presenter) = presenter {
            presenter.close();
        }
        if let Some(hwnd) = self.open.remove(&task_id) {
            // SAFETY: a window of this thread.
            unsafe { DestroyWindow(hwnd) };
        }
    }

    unsafe fn open_window(&mut self, task_id: i32, title: &str, size: (u32, u32), focused: bool) {
        if self.open.contains_key(&task_id) {
            return;
        }
        let data = Box::into_raw(Box::new(WindowData {
            task_id,
            report: Arc::clone(&self.setup.report),
            presenter: RefCell::new(None),
            pointer: PointerWindow::new(task_id, size, self.setup.input.clone()),
            keys: KeyWindow::new(task_id, self.setup.input.clone()),
            ime: ImeWindow::new(task_id, self.setup.input.clone()),
            max_client: Cell::new(None),
            reporting: Cell::new(false),
            minimized: Cell::new(false),
            sync: Cell::new(ResizeSync::new()),
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
        (self.setup.report)(FromHost::Resized {
            task_id,
            width: client.0,
            height: client.1,
        });
        ShowWindow(hwnd, if focused { SW_SHOW } else { SW_SHOWNOACTIVATE });
        self.open.insert(task_id, hwnd);
    }
}
