//! The task windows: one native top-level window per guest app task.
//!
//! All the windows live on one UI thread, which runs a message loop. Other threads reach it
//! through a [`UiHandle`]: the task watcher sends the guest's task events, and the request
//! thread says when a close did not reach the guest. A hidden message-only window receives
//! the wake-ups, so they arrive even while Windows runs its own loop for a live resize.
//!
//! The rules (which task has a window, which display goes to which window, how a window's
//! size and scale reach its task) are `TaskWindows`'s; this module only carries out the
//! changes it returns. A window procedure never reaches `TaskWindows` itself: it posts what
//! happened (a new client size, a new monitor scale, a drag starting or ending) to the
//! message window, and the UI thread acts on it there. Live resizes run inside Windows' own
//! sizing loop, which still delivers those posts.
//!
//! Each window hands its mouse, touch and pen messages to its `PointerWindow` (`pointer`),
//! which sends the task's pointer events on.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Instant;

use dsa_host_core::compositor::task_windows::{fit_within, TaskWindows, WindowChange};
use dsa_host_core::input::PointerEvent;
use dsa_host_core::tasks::{TaskEvent, TaskRequest};
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, GetMonitorInfoW, MonitorFromWindow, MONITORINFO,
    MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, SetProcessDpiAwarenessContext,
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage, RegisterClassExW,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, TranslateMessage,
    CREATESTRUCTW, CW_USEDEFAULT, GWLP_USERDATA, HWND_MESSAGE, IDC_ARROW, MINMAXINFO, MSG,
    SIZE_MINIMIZED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, SW_SHOWNOACTIVATE,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_DPICHANGED, WM_ENTERSIZEMOVE,
    WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_GETMINMAXINFO, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_SIZE,
    WM_TIMER, WNDCLASSEXW, WS_EX_APPWINDOW, WS_EX_NOREDIRECTIONBITMAP, WS_OVERLAPPEDWINDOW,
};

use super::frames::FrameRouter;
use super::log::{note, CompositorLogger};
use super::pointer::PointerWindow;
use super::present::{Gpu, WindowPresenter};

/// What other threads tell the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEvent {
    /// A new watch began; these are the guest's tasks now.
    Sync(Vec<TaskEvent>),
    /// A change to the guest's tasks.
    Task(TaskEvent),
    /// The service cannot be reached, so the guest's tasks are unknown.
    Lost,
    /// The request to end a task whose window the user closed did not reach the guest.
    CloseFailed(i32),
}

/// A task window that is open, as tests and logs see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenWindow {
    pub task_id: i32,
    /// The window's handle, as an integer.
    pub hwnd: isize,
}

const WM_APP_EVENTS: u32 = WM_APP + 1;
const WM_APP_USER_CLOSED: u32 = WM_APP + 2;
const WM_APP_QUIT: u32 = WM_APP + 3;
/// A task window's client area changed size (wparam: the task ID).
const WM_APP_RESIZED: u32 = WM_APP + 4;
/// A task window is on a monitor of another scale (wparam: the task ID, lparam: its DPI).
const WM_APP_DPI: u32 = WM_APP + 5;
/// The user started (lparam 1) or finished (0) dragging a task window (wparam: the task ID).
const WM_APP_DRAGGING: u32 = WM_APP + 6;

/// The message window's timer, which lets a window whose task did not take the size it
/// asked for take the task's size, and reports windows left stretched.
const TICK_TIMER: usize = 1;
const TICK_MILLIS: u32 = 200;

const TASK_CLASS: &str = "DsaTaskWindow";
const MESSAGE_CLASS: &str = "DsaCompositorMessages";
const TASK_STYLE: WINDOW_STYLE = WS_OVERLAPPEDWINDOW;
// Without a redirection surface, everything in the client area comes from the visual.
const TASK_EX_STYLE: WINDOW_EX_STYLE = WS_EX_NOREDIRECTIONBITMAP | WS_EX_APPWINDOW;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .filter(|unit| *unit != 0)
        .chain(std::iter::once(0))
        .collect()
}

/// How other threads reach the UI thread, and how to stop it.
pub struct UiHandle {
    message_window: isize,
    events: Sender<UiEvent>,
    thread: Option<JoinHandle<()>>,
}

/// A cloneable way to send [`UiEvent`]s to the UI thread.
#[derive(Clone)]
pub struct UiSender {
    message_window: isize,
    events: Sender<UiEvent>,
}

impl UiSender {
    /// Hands `event` to the UI thread. Returns false once the UI thread has ended.
    pub fn send(&self, event: UiEvent) -> bool {
        if self.events.send(event).is_err() {
            return false;
        }
        // SAFETY: posting to a window of another thread; a stale handle only fails.
        unsafe { PostMessageW(self.message_window as HWND, WM_APP_EVENTS, 0, 0) != 0 }
    }
}

impl UiHandle {
    pub fn sender(&self) -> UiSender {
        UiSender {
            message_window: self.message_window,
            events: self.events.clone(),
        }
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
    /// Where the requests to end a task go when the user closes its window.
    pub requests: Sender<TaskRequest>,
    /// Where the pointer input in every task window goes.
    pub input: Sender<PointerEvent>,
    /// The routes of each display's frames, which the UI thread keeps up to date.
    pub router: Arc<FrameRouter<WindowPresenter>>,
    /// The GPU devices, or `None` when frames cannot be shown (the windows still open).
    pub gpu: Option<Arc<Gpu>>,
    pub logger: Option<Arc<CompositorLogger>>,
    /// Where the UI thread lists the task windows that are open, in task ID order.
    pub listing: Arc<Mutex<Vec<OpenWindow>>>,
}

/// Starts the UI thread. Returns once its message window exists.
pub fn start(setup: UiSetup) -> Result<UiHandle, String> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let (events_tx, events_rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("dsa-compositor-ui".into())
        .spawn(move || run(setup, events_rx, ready_tx))
        .map_err(|e| format!("could not start the window thread: {e}"))?;
    match ready_rx.recv() {
        Ok(Ok(message_window)) => Ok(UiHandle {
            message_window,
            events: events_tx,
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

/// One open task window.
struct TaskWindow {
    hwnd: HWND,
    presenter: Option<Arc<WindowPresenter>>,
}

/// Kept in each task window's GWLP_USERDATA.
struct WindowData {
    task_id: i32,
    message_window: HWND,
    presenter: RefCell<Option<Arc<WindowPresenter>>>,
    /// The largest client area the guest can draw for the task, kept up to date by the UI
    /// thread, so the window procedure needs nothing else to answer WM_GETMINMAXINFO.
    max_client: Cell<Option<(u32, u32)>>,
    pointer: PointerWindow,
}

impl WindowData {
    /// Tells the UI thread about this window, outside the window procedure.
    unsafe fn post(&self, message: u32, lparam: LPARAM) {
        PostMessageW(
            self.message_window,
            message,
            self.task_id as u32 as usize,
            lparam,
        );
    }
}

/// The UI thread's state, reached from the message window's procedure.
struct Ui {
    windows: TaskWindows,
    open: HashMap<i32, TaskWindow>,
    events: Receiver<UiEvent>,
    setup: UiSetup,
    message_window: HWND,
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
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(procedure),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: GetModuleHandleW(std::ptr::null()),
            hIcon: std::ptr::null_mut(),
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: name.as_ptr(),
            hIconSm: std::ptr::null_mut(),
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

fn run(setup: UiSetup, events: Receiver<UiEvent>, ready: Sender<Result<isize, String>>) {
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
                "could not create the compositor's message window: {}",
                std::io::Error::last_os_error()
            )));
            return;
        }
        UI.with(|ui| {
            *ui.borrow_mut() = Some(Ui {
                windows: TaskWindows::new(),
                open: HashMap::new(),
                events,
                setup,
                message_window,
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
            let changes = ui.windows.reset();
            ui.carry_out(changes);
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
        WM_APP_EVENTS | WM_APP_USER_CLOSED | WM_APP_RESIZED | WM_APP_DPI | WM_APP_DRAGGING
        | WM_TIMER => {
            let handled = UI.with(|ui| match ui.try_borrow_mut() {
                Ok(mut ui) => {
                    if let Some(ui) = ui.as_mut() {
                        let task_id = wparam as u32 as i32;
                        match message {
                            WM_APP_EVENTS => ui.drain_events(),
                            WM_APP_USER_CLOSED => ui.user_closed(task_id),
                            WM_APP_RESIZED => ui.window_resized(task_id),
                            WM_APP_DPI => ui.window_dpi_changed(task_id, lparam as u32),
                            WM_APP_DRAGGING => ui.window_dragging(task_id, lparam != 0),
                            _ => ui.tick(),
                        }
                    }
                    true
                }
                Err(_) => false,
            });
            if !handled && message != WM_TIMER {
                // Reached from inside the UI's own work; try again once it is done.
                PostMessageW(hwnd, message, wparam, lparam);
            }
            0
        }
        WM_APP_QUIT => {
            PostQuitMessage(0);
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
    }
    match message {
        WM_NCCREATE => {
            let create = &*(lparam as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            DefWindowProcW(hwnd, message, wparam, lparam)
        }
        WM_SIZE => {
            if wparam as u32 != SIZE_MINIMIZED {
                let width = (lparam & 0xffff) as u32;
                let height = ((lparam >> 16) & 0xffff) as u32;
                if let Some(data) = window_data(hwnd) {
                    if let Some(presenter) = data.presenter.borrow().as_ref() {
                        // During a live resize this runs inside Windows' sizing loop; the
                        // last frame is stretched to the new size before it is shown, until
                        // the task draws a frame of that size.
                        presenter.view_resized((width, height));
                    }
                    // The task takes the new size.
                    data.post(WM_APP_RESIZED, 0);
                }
            }
            0
        }
        WM_ENTERSIZEMOVE | WM_EXITSIZEMOVE => {
            if let Some(data) = window_data(hwnd) {
                let active = message == WM_ENTERSIZEMOVE;
                data.post(WM_APP_DRAGGING, active as isize);
                if !active {
                    data.post(WM_APP_RESIZED, 0);
                }
            }
            0
        }
        WM_GETMINMAXINFO => {
            // Never larger than the guest can draw: a larger window could only show the
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
        WM_DPICHANGED => {
            // The task draws at the new monitor's density; the request goes before the
            // resize below, so the guest gets the density and then the size that goes with
            // it.
            let dpi = (wparam & 0xffff) as u32;
            let data = window_data(hwnd);
            if let Some(data) = data {
                data.post(WM_APP_DPI, dpi as isize);
            }
            // A per-monitor aware window is handed the rectangle that keeps its size in
            // points on the new monitor, and applies it itself, within what the guest can
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
        // Never painted white between frames: the visual covers the client area.
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut paint);
            EndPaint(hwnd, &paint);
            0
        }
        WM_CLOSE => {
            // The guest ends the task; the UI thread closes the window, outside this
            // procedure.
            if let Some(data) = window_data(hwnd) {
                PostMessageW(
                    data.message_window,
                    WM_APP_USER_CLOSED,
                    data.task_id as u32 as usize,
                    0,
                );
            }
            0
        }
        WM_NCDESTROY => {
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

impl Ui {
    fn log(&self, message: &str) {
        note(self.setup.logger.as_deref(), message);
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            let changes = match event {
                UiEvent::Sync(current) => self.windows.sync(&current),
                UiEvent::Task(event) => self.windows.apply(&event),
                UiEvent::Lost => self.windows.reset(),
                UiEvent::CloseFailed(task_id) => {
                    self.log(&format!(
                        "the request to end task {task_id} did not reach Android; its window \
                         opens again"
                    ));
                    self.windows.close_failed(task_id)
                }
            };
            self.carry_out(changes);
        }
        self.publish();
    }

    fn send(&self, requests: Vec<TaskRequest>) {
        for request in requests {
            if self.setup.requests.send(request).is_err() {
                self.log("the request thread has ended; a window's change does not reach Android");
                return;
            }
        }
    }

    /// A task window's client area changed: the task takes its size.
    fn window_resized(&mut self, task_id: i32) {
        let Some(window) = self.open.get(&task_id) else {
            return;
        };
        let mut rect: RECT = unsafe { std::mem::zeroed() };
        // SAFETY: a window of this thread; a destroyed one only fails.
        if unsafe { GetClientRect(window.hwnd, &mut rect) } == 0 {
            return;
        }
        let view = (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        );
        let requests = self.windows.window_resized(task_id, view, Instant::now());
        self.send(requests);
    }

    /// A task window is on a monitor of another scale: the task draws at its density.
    fn window_dpi_changed(&mut self, task_id: i32, dpi: u32) {
        let requests = self.windows.window_dpi_changed(task_id, dpi);
        self.send(requests);
    }

    fn window_dragging(&mut self, task_id: i32, active: bool) {
        self.windows
            .window_dragging(task_id, active, Instant::now());
    }

    /// Windows whose task did not take their size take the task's, and windows left
    /// stretched are logged.
    fn tick(&mut self) {
        let changes = self.windows.tick(Instant::now());
        if !changes.is_empty() {
            self.carry_out(changes);
        }
        for window in self.open.values() {
            if let Some(presenter) = &window.presenter {
                presenter.tick();
            }
        }
    }

    fn user_closed(&mut self, task_id: i32) {
        match self.windows.user_closed(task_id) {
            Some(request) => {
                if self.setup.requests.send(request).is_err() {
                    self.log("the request thread has ended; a closed window's task stays");
                }
            }
            None => self.log(&format!("closing the window of unknown task {task_id}")),
        }
        self.carry_out(vec![WindowChange::Close { task_id }]);
        self.publish();
    }

    /// Tells the frame router and the listing which windows are open, and each window how
    /// large its task is and how large the guest can draw it. The task's size changes with
    /// every resize the window asks for, without the window itself changing, and pointer
    /// input is measured in it.
    fn publish(&self) {
        for (task_id, window) in &self.open {
            // SAFETY: a window of this thread, whose data lives until WM_NCDESTROY.
            if let Some(data) = unsafe { window_data(window.hwnd) } {
                data.max_client.set(self.windows.max_client_size(*task_id));
                if let Some(size) = self.windows.task_size(*task_id) {
                    data.pointer.task_resized(size);
                }
            }
        }
        let routes: Vec<(u32, Arc<WindowPresenter>)> = self
            .windows
            .routes()
            .into_iter()
            .filter_map(|(display_id, task_id)| {
                let presenter = self.open.get(&task_id)?.presenter.clone()?;
                Some((display_id, presenter))
            })
            .collect();
        self.setup.router.set_routes(routes);
        let mut listing: Vec<OpenWindow> = self
            .open
            .iter()
            .map(|(task_id, window)| OpenWindow {
                task_id: *task_id,
                hwnd: window.hwnd as isize,
            })
            .collect();
        listing.sort_by_key(|window| window.task_id);
        *self.setup.listing.lock().unwrap_or_else(|e| e.into_inner()) = listing;
    }

    fn carry_out(&mut self, changes: Vec<WindowChange>) {
        for change in changes {
            // SAFETY: plain Win32 calls on this thread's own windows.
            unsafe {
                match change {
                    WindowChange::Open {
                        task_id,
                        title,
                        width,
                        height,
                        focused,
                    } => self.open_window(task_id, &title, (width, height), focused),
                    WindowChange::Retitle { task_id, title } => {
                        if let Some(window) = self.open.get(&task_id) {
                            SetWindowTextW(window.hwnd, wide(&title).as_ptr());
                        }
                    }
                    WindowChange::Resize {
                        task_id,
                        width,
                        height,
                    } => {
                        if let Some(window) = self.open.get(&task_id) {
                            if let Some(data) = window_data(window.hwnd) {
                                data.pointer.task_resized((width, height));
                            }
                            let dpi = GetDpiForWindow(window.hwnd).max(96);
                            let client =
                                fit_within((width, height), usable_client_area(window.hwnd, dpi));
                            let (outer_width, outer_height) = outer_size(client, dpi);
                            SetWindowPos(
                                window.hwnd,
                                std::ptr::null_mut(),
                                0,
                                0,
                                outer_width,
                                outer_height,
                                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                            );
                        }
                    }
                    WindowChange::Close { task_id } => {
                        if let Some(window) = self.open.remove(&task_id) {
                            if let Some(presenter) = &window.presenter {
                                presenter.close();
                            }
                            DestroyWindow(window.hwnd);
                        }
                    }
                }
            }
        }
    }

    unsafe fn open_window(&mut self, task_id: i32, title: &str, size: (u32, u32), focused: bool) {
        if self.open.contains_key(&task_id) {
            return;
        }
        let data = Box::new(WindowData {
            task_id,
            message_window: self.message_window,
            presenter: RefCell::new(None),
            max_client: Cell::new(self.windows.max_client_size(task_id)),
            pointer: PointerWindow::new(task_id, size, self.setup.input.clone()),
        });
        let data = Box::into_raw(data);
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
        // The task's size is in the guest's pixels, which are the window's pixels. On a
        // monitor of another scale the task draws at that monitor's density, at the size
        // that keeps it the same to the eye; the density goes to the guest before the size
        // the window reports once it has it.
        let dpi = GetDpiForWindow(hwnd).max(96);
        let size = match self.windows.window_opened(task_id, dpi) {
            Some((size, requests)) => {
                self.send(requests);
                size
            }
            None => size,
        };
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
        let presenter = match &self.setup.gpu {
            Some(gpu) => match WindowPresenter::new(
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
        ShowWindow(hwnd, if focused { SW_SHOW } else { SW_SHOWNOACTIVATE });
        self.open.insert(task_id, TaskWindow { hwnd, presenter });
    }
}
