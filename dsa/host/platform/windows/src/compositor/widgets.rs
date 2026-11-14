//! The widgets of the user's Android, as borderless windows on the user's desktop.
//!
//! The window manager watches the widgets placed on the user's desktop over the local API and
//! shows each one in a window of its own ([`sync`]). A widget window is a layered popup
//! window that belongs to the desktop: it is owned by the shell's desktop window, so it stays
//! when the user shows the desktop (Win+D), it is kept below every other window, and as a
//! tool window that never takes the focus it is neither in the taskbar nor in Alt+Tab
//! ([`window_styles`]).
//!
//! The window shows the widget's latest frame, a PNG image with an alpha channel, so a widget
//! with rounded corners or a see-through background looks as it does on a phone. Nothing is
//! drawn over the widget, not even while the pointer is over it, so the widget's own buttons
//! stay free to click in every corner. A right click opens the widget's menu at the pointer,
//! and the menu key or Shift+F10 opens it under the widget ([`super::widget_menu`]).
//!
//! A press, a move and a release inside the widget go to the widget's view in Android, so its
//! buttons and lists work and an app it opens shows as a window. A press that moves before
//! [`HOLD_TO_MOVE`] is the widget's own (a list scrolls), and a tap stays a tap ([`Gesture`]).
//!
//! A long press edits the widget as Android's home screen does: a finger or a pen held on it,
//! or the mouse's left button held, for [`HOLD_TO_MOVE`] without moving past the drag
//! threshold. The system's timer may come a little before that, so a hold checked too early
//! waits out the rest rather than being dropped ([`Gesture::held`]). Android's press is
//! cancelled then, so the long press never also clicks. The widget lifts: its image, at its
//! own size, rises a little over a soft shadow, with a border in the accent color around it
//! and a handle on each edge it may be resized along ([`edit_layout`], [`draw_lifted`]). While
//! it is resized, its size goes to Android when the drag pauses and when it ends
//! ([`ResizeReports`]), and its last image shows unscaled meanwhile. Dragging on
//! moves it; letting go without moving opens its menu under it (Widget settings, Resize,
//! Remove). While it is edited, dragging it moves it, a click on it opens the menu again, its
//! handles and edges resize it within what the widget allows ([`edit_hit_test`]), and Esc, a
//! click anywhere else, or the end of a move or a resize ends the edit. The window is also
//! resized by its edges when it is not edited ([`hit_test`]). When the user lets go, the service is told where the window is and how
//! large, in its monitor's pixels and density; the service remembers it and tells Android,
//! which draws the widget at that size and tells the widget's app its size in dp, so the app
//! can pick the layout that fits.
//!
//! The window manager starts at sign-in, and it starts Android then when it is not running
//! ([`starts_android_at_sign_in`]): the first app opened after the computer starts does not
//! wait for Android to boot, and the widgets the service remembers come back.
//!
//! The geometry, the gestures, the menu's choices and the watch are the platform-neutral ones
//! every host shares (`dsa_host_core::widget_windows`); what is here is Windows' own: the
//! layered windows, their place in the z-order and their Fluent drawing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::widget_windows;
pub use dsa_host_core::widget_windows::geometry::*;
pub use dsa_host_core::widget_windows::gesture::*;
pub use dsa_host_core::widget_windows::service::{
    coalesce, command_of, placed_report, reconfigure, starts_android_at_sign_in, WidgetWindows,
    WindowCommand, WindowReport,
};
use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm widgets";

/// The window style and extended style of a widget window: a popup with no frame, layered
/// so it can be partly see-through, a tool window so it is neither in the taskbar nor in
/// Alt+Tab, and one that never takes the focus, so clicking it leaves the user's window
/// active.
pub fn window_styles() -> (u32, u32) {
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_LAYERED: u32 = 0x0008_0000;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    (
        WS_POPUP,
        WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
    )
}

/// Where a widget window goes in the z-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZPlace {
    /// Right above the desktop: below the window this names, which is right above it.
    After(isize),
    /// It is right above the desktop already, or the desktop is the top window; the z-order
    /// stays.
    Keep,
    /// No desktop was found: the bottom of the z-order.
    Bottom,
}

/// The z-order a widget window `own` takes: right above the desktop's own window (`desktop`,
/// the one that holds the desktop icons), with `above_desktop` the window right above that
/// now. That is below every other window, but not the bottom of the z-order: once a
/// wallpaper slideshow or "show desktop" has run, the shell moves the icons into a window of
/// its own above `Progman` that paints the wallpaper, and a window at the bottom is hidden
/// under it.
pub fn z_place(desktop: Option<isize>, above_desktop: Option<isize>, own: isize) -> ZPlace {
    match (desktop, above_desktop) {
        (None, _) => ZPlace::Bottom,
        (Some(_), Some(above)) if above == own => ZPlace::Keep,
        (Some(_), Some(above)) => ZPlace::After(above),
        (Some(_), None) => ZPlace::Keep,
    }
}

/// How often the widget windows are checked to be right above the desktop. The shell
/// restacks its own windows without telling anyone: "show desktop", a wallpaper slideshow
/// and an Explorer restart raise the window that holds the desktop icons and paints the
/// wallpaper, and a widget window that only fixed its place when it moved itself stayed under
/// it, drawn but hidden behind the desktop.
pub const RESTACK_CHECK: Duration = Duration::from_millis(500);

/// One window below a widget window, as [`right_above_desktop`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Below {
    pub hwnd: isize,
    /// Whether it shows over part of the widget window: visible, not minimized and
    /// overlapping it.
    pub covers: bool,
}

/// Whether a widget window sits where it belongs, given the windows below it in z-order from
/// the nearest down (`below`): the desktop's own window (`desktop`) comes before any other
/// window that overlaps it. Other widget windows (`ours`) and windows that do not overlap it
/// may be in between. False when the desktop is not below it at all, which means the desktop
/// is drawn over it, and when an app's window is below it, which it would hide.
pub fn right_above_desktop(below: &[Below], desktop: isize, ours: &[isize]) -> bool {
    for window in below {
        if window.hwnd == desktop {
            return true;
        }
        if window.covers && !ours.contains(&window.hwnd) {
            return false;
        }
    }
    false
}

/// The colors a widget being edited is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditLook {
    /// The border and the handles' rims: the accent color, as WinUI draws a selection.
    pub accent: super::fluent::Argb,
    /// The inside of the handles.
    pub handle: super::fluent::Argb,
    /// The shadow under the lifted widget, at its darkest.
    pub shadow: super::fluent::Argb,
}

/// The colors of a widget being edited in `theme`: WinUI's accent fill (AccentDark1 on a light
/// theme, AccentLight2 on a dark one), the theme's surface for the handles, and the high
/// contrast colors while those are on.
pub fn edit_look(theme: &super::fluent::Theme) -> EditLook {
    if let Some(contrast) = theme.high_contrast {
        return EditLook {
            accent: contrast.highlight,
            handle: contrast.window,
            shadow: 0,
        };
    }
    EditLook {
        accent: if theme.dark {
            theme.accent.light2
        } else {
            theme.accent.dark1
        },
        handle: if theme.dark { 0xFF2D_2D2D } else { 0xFFFF_FFFF },
        shadow: if theme.dark { 0x5C00_0000 } else { 0x3800_0000 },
    }
}

/// The picture of a widget being edited, `width` by `height` premultiplied BGRA pixels on a
/// monitor of `dpi`: a soft shadow under the lifted widget, `lifted` (its image at the lifted
/// size, premultiplied BGRA) over it, a border in the accent color and the handles. Every
/// pixel of the widget stays just visible enough to be clicked; the room around it is clear
/// but for the shadow.
pub fn draw_edit(
    lifted: &[u8],
    width: usize,
    height: usize,
    dpi: u32,
    resize_mode: u32,
    look: &EditLook,
) -> Vec<u8> {
    let layout = edit_layout(width as i32, height as i32, dpi, resize_mode);
    let mut pixels = vec![0u8; width * height * 4];
    let mut canvas = super::fluent::Canvas {
        width,
        height,
        pixels: &mut pixels,
    };
    let (lx, ly, lw, lh) = layout.lifted;
    let radius = px_of_dip(EDIT_RADIUS_DIP, dpi) as f32;
    // The shadow: layers that spread from the widget into the room around it, each adding a
    // little, so it is darkest under the widget and fades out, a little below it as a raised
    // surface's is.
    if look.shadow >> 24 != 0 {
        const LAYERS: i32 = 6;
        let room = (edit_margin(dpi) - px_of_dip(LIFT_DIP, dpi)).max(1) as f32;
        let drop = px_of_dip(2, dpi) as f32;
        let alpha = ((look.shadow >> 24) as f32 / LAYERS as f32)
            .round()
            .max(1.0) as u32;
        let layer = (alpha.min(255) << 24) | (look.shadow & 0x00FF_FFFF);
        for step in 1..=LAYERS {
            let spread = room * step as f32 / LAYERS as f32;
            canvas.fill_rounded(
                lx as f32 - spread,
                ly as f32 - spread + drop,
                lw as f32 + 2.0 * spread,
                lh as f32 + 2.0 * spread,
                radius + spread,
                layer,
            );
        }
    }
    // The widget's image over it.
    let (lw_px, lh_px) = (lw.max(0) as usize, lh.max(0) as usize);
    for row in 0..lh_px {
        let y = ly + row as i32;
        if y < 0 || y as usize >= height {
            continue;
        }
        for column in 0..lw_px {
            let x = lx + column as i32;
            if x < 0 || x as usize >= width {
                continue;
            }
            let from = (row * lw_px + column) * 4;
            let Some(source) = lifted.get(from..from + 4) else {
                continue;
            };
            let to = (y as usize * width + x as usize) * 4;
            let keep = 255 - u32::from(source[3]);
            for (out, over) in canvas.pixels[to..to + 4].iter_mut().zip(source) {
                let under = u32::from(*out) * keep / 255;
                *out = (u32::from(*over) + under).min(255) as u8;
            }
            if canvas.pixels[to + 3] == 0 {
                canvas.pixels[to + 3] = 1;
            }
        }
    }
    let border = px_of_dip(EDIT_BORDER_DIP, dpi).max(1) as f32;
    let (x, y, w, h) = (lx as f32, ly as f32, lw as f32, lh as f32);
    canvas.stroke_rounded(x, y, w, h, radius, border, look.accent);
    let rim = (px_of_dip(3, dpi) as f32 / 2.0).max(1.0);
    for (_, handle) in &layout.handles {
        let (r, d) = (handle.radius, handle.radius * 2.0);
        let (left, top) = (handle.cx - r, handle.cy - r);
        canvas.fill_rounded(left, top, d, d, r, look.handle);
        canvas.stroke_rounded(left, top, d, d, r, rim, look.accent);
    }
    pixels
}

/// The picture of a widget lifted for editing in a window of `width` by `height` pixels on a
/// monitor of `dpi`: its `image` of `from` pixels (premultiplied BGRA, as Android drew it) at
/// its own size, raised a little over a soft shadow, with the accent border and the handles
/// around it ([`draw_edit`]). The image is never scaled to the lifted size.
pub fn draw_lifted(
    image: &[u8],
    from: (usize, usize),
    width: usize,
    height: usize,
    dpi: u32,
    resize_mode: u32,
    look: &EditLook,
) -> Vec<u8> {
    let layout = edit_layout(width as i32, height as i32, dpi, resize_mode);
    let (lx, ly, lw, lh) = layout.lifted;
    let (ix, iy, _, _) = layout.image;
    let room = (lw.max(1) as usize, lh.max(1) as usize);
    let at = ((ix - lx).max(0) as usize, (iy - ly).max(0) as usize);
    let lifted = unscaled_at(image, from, room, at);
    draw_edit(&lifted, width, height, dpi, resize_mode, look)
}

/// Where `WM_CONTEXTMENU` with `lparam` opens the menu: at the pointer for a right click,
/// under the widget from the keyboard, which gives -1, -1 for the position.
pub fn context_menu_at(lparam: isize) -> MenuAt {
    if lparam as u32 == u32::MAX {
        return MenuAt::Keyboard;
    }
    let lo = (lparam & 0xffff) as u16 as i16 as i32;
    let hi = ((lparam >> 16) & 0xffff) as u16 as i16 as i32;
    MenuAt::Point(lo, hi)
}

/// The anchor of a widget's menu opened `at`, under `shown` (the widget's rectangle on the
/// screen as drawn, lifted while it is edited), and whether it was opened from the keyboard,
/// which highlights its first item.
pub fn menu_anchor(at: MenuAt, shown: ScreenRect) -> (super::fluent_menu::Anchor, bool) {
    use super::fluent_menu::Anchor;
    match at {
        MenuAt::Point(x, y) => (Anchor::Point { x, y }, false),
        MenuAt::Widget => (Anchor::Below(shown), false),
        MenuAt::Keyboard => (Anchor::Below(shown), true),
    }
}

/// Shows the widgets of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends, with what it has to say in the window manager's log.
pub fn sync(
    connector: Box<dyn Connector>,
    windows: &mut dyn WidgetWindows,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let log: widget_windows::Log = Arc::new(move |line: &str| note(logger.as_deref(), line));
    widget_windows::sync(connector, windows, stop, BUILD, log);
}

/// The widget windows: Win32 layered windows on a thread of their own, which the watch
/// drives through a channel.
#[cfg(windows)]
pub struct WindowsWidgetWindows {
    commands: std::sync::mpsc::Sender<WindowCommand>,
    reports: std::sync::mpsc::Receiver<WindowReport>,
    controller: Arc<std::sync::atomic::AtomicIsize>,
    thread: Option<thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl WindowsWidgetWindows {
    /// Starts the windows' thread.
    pub fn open(logger: Option<Arc<CompositorLogger>>) -> Result<WindowsWidgetWindows, String> {
        let (commands, command_rx) = std::sync::mpsc::channel();
        let (report_tx, reports) = std::sync::mpsc::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let controller = Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let slot = Arc::clone(&controller);
        let thread = thread::Builder::new()
            .name("dsa-wm-widget-windows".into())
            .spawn(move || win::run(slot, command_rx, report_tx, ready_tx, logger))
            .map_err(|e| format!("could not start the widgets' thread: {e}"))?;
        match ready.recv() {
            Ok(Ok(())) => Ok(WindowsWidgetWindows {
                commands,
                reports,
                controller,
                thread: Some(thread),
            }),
            Ok(Err(why)) => {
                let _ = thread.join();
                Err(why)
            }
            Err(_) => {
                let _ = thread.join();
                Err("the widgets' thread ended before it was ready".into())
            }
        }
    }
}

#[cfg(windows)]
impl WidgetWindows for WindowsWidgetWindows {
    fn command(&mut self, command: WindowCommand) {
        if self.commands.send(command).is_ok() {
            win::wake(self.controller.load(Ordering::SeqCst));
        }
    }

    fn reports(&mut self) -> Vec<WindowReport> {
        self.reports.try_iter().collect()
    }
}

#[cfg(windows)]
impl Drop for WindowsWidgetWindows {
    fn drop(&mut self) {
        win::close(self.controller.load(Ordering::SeqCst));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use dsa_host_core::widgets::{Placement, Widget, WidgetFrame};
    use dsa_protocol::host_api::types::PointerAction;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
        MonitorFromWindow, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, MONITORINFO,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForWindow, GetSystemMetricsForDpi, SetThreadDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, ReleaseCapture, SetCapture, SetFocus, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON,
        VK_RETURN, VK_SPACE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW,
        FindWindowW, GetCursorPos, GetMessageW, GetWindow, GetWindowRect, KillTimer, LoadCursorW,
        PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetCursor,
        SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        TranslateMessage, UpdateLayeredWindow, GWLP_HWNDPARENT, GW_HWNDPREV, HTCLIENT, HWND_BOTTOM,
        HWND_MESSAGE, IDC_ARROW, IDC_SIZEALL, IDC_SIZENESW, IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE,
        MA_NOACTIVATE, MINMAXINFO, MSG, SM_CXDRAG, SM_CYDRAG, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, SW_SHOWNOACTIVATE, ULW_ALPHA, WINDOWPOS, WM_APP,
        WM_CAPTURECHANGED, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_GETMINMAXINFO,
        WM_GETOBJECT, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
        WM_MOUSEMOVE, WM_NCDESTROY, WM_NCHITTEST, WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE,
        WM_SIZE, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WM_TIMER, WM_WINDOWPOSCHANGING, WNDCLASSEXW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetMessageExtraInfo, IsWindow,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, GW_HWNDNEXT};

    use std::time::Instant;

    use super::{
        context_menu_at, draw_lifted, edit_layout, edit_look, edit_margin, frame_overdue, hit_test,
        hold_timer_ms, keep_clickable, max_window_size, menu_anchor, menu_closed_by,
        min_window_size, placed_report, placement_of, press_threshold, reachable_origin,
        right_above_desktop, to_frame, window_size, window_styles, z_place, Below, Cursor, Gesture,
        Hit, MenuAt, MenuClosed, Press, ResizePoll, ResizeReports, Step, WindowCommand,
        WindowReport, ZPlace, HOLD_TO_MOVE, NO_FRAME, NO_FRAME_CHECK, RESIZE_POLL, RESTACK_CHECK,
    };
    use crate::compositor::fluent::{read_theme, Theme};
    use crate::compositor::log::{note, CompositorLogger};
    use crate::compositor::widget_automation::{self, WidgetView, WM_WIDGET_FOCUS};
    use crate::compositor::widget_menu::{self, MenuChoice};
    use windows::Win32::UI::Accessibility::IRawElementProviderSimple;

    /// The message that tells the controller window commands are waiting.
    const WM_COMMANDS: u32 = WM_APP + 1;

    struct Window {
        widget: Widget,
        frame: Option<WidgetFrame>,
        /// The last image that had something in it, which the window keeps showing when
        /// Android sends one with nothing in it.
        shown: Option<WidgetFrame>,
        /// The widget's image at the size it is drawn at, so the window is drawn again (as an
        /// edit begins or ends, or the keyboard comes and goes) without decoding it again.
        base: Option<(i32, i32, Vec<u8>)>,
        /// Whether Android has a press of the widget.
        pressed: bool,
        gesture: Gesture,
        /// Whether the window has the keyboard.
        focused: bool,
        menu_open: bool,
        /// When the widget's size is passed on while it is resized.
        resizes: ResizeReports<super::ScreenRect>,
        /// Whether the resize has ended, with its last size perhaps still to pass on.
        resize_ended: bool,
        /// Since when the window waits for its first image; None once one came.
        waiting_since: Option<Instant>,
        /// Whether the wait was logged already.
        noted: bool,
        /// What UI Automation reads of the window, and the provider it was answered with.
        view: Arc<Mutex<WidgetView>>,
        provider: Option<IRawElementProviderSimple>,
        /// Some while the widget is being edited after a long press.
        editing: Option<Edit>,
    }

    /// A widget being edited: its window has `margin` pixels of room on each side for the
    /// shadow and the handles, and has the keyboard, which `previous` had before.
    #[derive(Debug, Clone, Copy)]
    struct Edit {
        margin: i32,
        previous: isize,
    }

    /// The window's timer that turns a held press into a move.
    const HOLD_TIMER: usize = 3;

    /// The window's timer that passes on the size of a widget being resized when the drag
    /// pauses.
    const RESIZE_TIMER: usize = 5;

    /// The controller's timer that checks for windows still without an image.
    const NO_FRAME_TIMER: usize = 1;

    /// The controller's timer that keeps the windows right above the desktop.
    const RESTACK_TIMER: usize = 2;

    /// What the last drawing of a window handed the system, so a test can tell a window that
    /// shows something from one that shows nothing.
    #[cfg(test)]
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Presented {
        pub hwnd: isize,
        /// Whether UpdateLayeredWindow took the picture.
        pub committed: bool,
        pub width: i32,
        pub height: i32,
        /// How many of its pixels are at least half opaque.
        pub opaque: usize,
        /// How many of its pixels are see-through: an alpha of at most 1, which keeps them
        /// clickable.
        pub clear: usize,
        /// Whether the window was shown already when it got the picture.
        pub visible: bool,
    }

    #[cfg(test)]
    pub(super) static PRESENTED: std::sync::Mutex<Vec<Presented>> =
        std::sync::Mutex::new(Vec::new());

    struct State {
        commands: Receiver<WindowCommand>,
        reports: Sender<WindowReport>,
        windows: HashMap<isize, Window>,
        by_id: HashMap<i32, isize>,
        class: Vec<u16>,
        taskbar_created: u32,
        logger: Option<Arc<CompositorLogger>>,
        decoder: Option<super::png::Decoder>,
        /// The theme a widget being edited is drawn in.
        theme: Theme,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
        STATE.with(|state| match state.try_borrow_mut() {
            Ok(mut state) => state.as_mut().map(f),
            Err(_) => None,
        })
    }

    pub(super) fn wake(controller: isize) {
        if controller != 0 {
            // SAFETY: posts to the controller window, which may have gone; then it fails.
            unsafe { PostMessageW(controller as HWND, WM_COMMANDS, 0, 0) };
        }
    }

    pub(super) fn close(controller: isize) {
        if controller != 0 {
            // SAFETY: as above.
            unsafe { PostMessageW(controller as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// The shell's desktop window, which widget windows belong to so they stay on the
    /// desktop when the user shows it.
    fn desktop_owner() -> HWND {
        let class = wide("Progman");
        // SAFETY: a NUL-terminated class name.
        unsafe { FindWindowW(class.as_ptr(), null()) }
    }

    fn lo(l: LPARAM) -> i32 {
        (l & 0xffff) as u16 as i16 as i32
    }

    fn hi(l: LPARAM) -> i32 {
        ((l >> 16) & 0xffff) as u16 as i16 as i32
    }

    fn rect_of(hwnd: HWND) -> RECT {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: a window of this thread and a rectangle to fill.
        unsafe { GetWindowRect(hwnd, &mut rect) };
        rect
    }

    fn dpi_of(hwnd: HWND) -> u32 {
        // SAFETY: a window of this thread.
        match unsafe { GetDpiForWindow(hwnd) } {
            0 => 96,
            dpi => dpi,
        }
    }

    /// The window's edit, while its widget is being edited.
    fn edit_of(hwnd: HWND) -> Option<Edit> {
        with_state(|s| s.windows.get(&(hwnd as isize)).and_then(|w| w.editing)).flatten()
    }

    /// The widget's own rectangle on the screen: its window's, less the room around it while
    /// it is being edited.
    fn widget_rect(hwnd: HWND) -> RECT {
        let rect = rect_of(hwnd);
        let margin = edit_of(hwnd).map_or(0, |edit| edit.margin);
        RECT {
            left: rect.left + margin,
            top: rect.top + margin,
            right: (rect.right - margin).max(rect.left + margin + 1),
            bottom: (rect.bottom - margin).max(rect.top + margin + 1),
        }
    }

    /// Lifts the widget for editing, after a long press: its window grows by the room for
    /// the shadow and the handles on each side, keeping the widget where it was, and takes
    /// the keyboard, so Esc ends the edit and a click on any other window does too (the
    /// window hears it lose the keyboard). The grown window moves the pointer that much
    /// further from the window's corner, which the press in progress is told.
    fn enter_edit(hwnd: HWND) {
        let key = hwnd as isize;
        let margin = edit_margin(dpi_of(hwnd));
        // SAFETY: a plain query.
        let previous = unsafe { GetForegroundWindow() } as isize;
        let entered = with_state(|s| {
            let w = s.windows.get_mut(&key)?;
            if w.editing.is_some() {
                return Some(false);
            }
            w.editing = Some(Edit { margin, previous });
            w.gesture.regrab((margin, margin));
            w.base = None;
            Some(true)
        })
        .flatten()
        .unwrap_or(false);
        if !entered {
            return;
        }
        // One widget is edited at a time.
        end_other_edits(hwnd);
        let rect = rect_of(hwnd);
        // SAFETY: a window of this thread; it keeps its place in the z-order. It takes the
        // foreground while the user's long press is in progress, which Windows allows.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left - margin,
                rect.top - margin,
                rect.right - rect.left + 2 * margin,
                rect.bottom - rect.top + 2 * margin,
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            SetForegroundWindow(hwnd);
            SetFocus(hwnd);
        }
        let grown = rect_of(hwnd);
        with_state(|s| {
            let id = s.windows.get(&key).map(|w| w.widget.widget_id);
            note(
                s.logger.as_deref(),
                &format!(
                    "widget {}: lifted for editing after a long press; its window is {}x{} pixels with {margin} pixels of room on each side",
                    id.unwrap_or_default(),
                    grown.right - grown.left,
                    grown.bottom - grown.top,
                ),
            );
        });
        present(hwnd);
    }

    /// Ends the widget's edit, if it is being edited: its window shrinks back around the
    /// widget, which stays where the edit left it. With `give_back`, the window that had the
    /// keyboard before the edit gets it back, as after Esc or a finished move; a click on
    /// another window has given it away already.
    fn leave_edit(hwnd: HWND, give_back: bool) {
        let key = hwnd as isize;
        let Some(edit) = with_state(|s| {
            let w = s.windows.get_mut(&key)?;
            let edit = w.editing.take()?;
            w.base = None;
            Some(edit)
        })
        .flatten() else {
            return;
        };
        let rect = rect_of(hwnd);
        let m = edit.margin;
        // SAFETY: a window of this thread; it keeps its place in the z-order.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left + m,
                rect.top + m,
                (rect.right - rect.left - 2 * m).max(1),
                (rect.bottom - rect.top - 2 * m).max(1),
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
            let previous = edit.previous as HWND;
            if give_back
                && GetForegroundWindow() == hwnd
                && !previous.is_null()
                && previous != hwnd
                && IsWindow(previous) != 0
            {
                SetForegroundWindow(previous);
            }
        }
        present(hwnd);
    }

    fn work_area(hwnd: HWND) -> Option<super::ScreenRect> {
        // SAFETY: a window of this thread, and a structure of the right size to fill.
        unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
            let w = info.rcWork;
            Some((w.left, w.top, w.right, w.bottom))
        }
    }

    /// Opens the controller window, says whether it did, and runs the windows' messages
    /// until it is closed.
    pub(super) fn run(
        slot: Arc<AtomicIsize>,
        commands: Receiver<WindowCommand>,
        reports: Sender<WindowReport>,
        ready: Sender<Result<(), String>>,
        logger: Option<Arc<CompositorLogger>>,
    ) {
        // Every window of this thread follows its monitor's scale.
        // SAFETY: changes this thread's DPI awareness only.
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let _com = crate::start_menu::ComApartment::enter();
        let decoder = match super::png::Decoder::new() {
            Ok(decoder) => Some(decoder),
            Err(why) => {
                note(logger.as_deref(), &format!("widgets show no image: {why}"));
                None
            }
        };
        let controller_class = wide(&format!("DsaWmWidgets_{}", std::process::id()));
        let class = wide(&format!("DsaWmWidget_{}", std::process::id()));
        // SAFETY: the module handle of this process.
        let instance = unsafe { GetModuleHandleW(null()) };
        for (name, proc_) in [
            (&controller_class, controller_proc as WndProc),
            (&class, widget_proc as WndProc),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                // SAFETY: a system cursor.
                hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: name.as_ptr(),
                hIconSm: null_mut(),
            };
            // SAFETY: the names outlive the classes, which live as long as the process.
            unsafe { RegisterClassExW(&wc) };
        }
        let taskbar = wide("TaskbarCreated");
        // SAFETY: a NUL-terminated message name.
        let taskbar_created = unsafe { RegisterWindowMessageW(taskbar.as_ptr()) };
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                commands,
                reports,
                windows: HashMap::new(),
                by_id: HashMap::new(),
                class,
                taskbar_created,
                logger,
                decoder,
                theme: read_theme(),
            })
        });
        // SAFETY: a message-only window, never shown.
        let controller = unsafe {
            CreateWindowExW(
                0,
                controller_class.as_ptr(),
                controller_class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                instance,
                null(),
            )
        };
        if controller.is_null() {
            let _ = ready.send(Err(format!(
                "could not open the widgets' window: {}",
                std::io::Error::last_os_error()
            )));
            return;
        }
        // SAFETY: a timer of the controller window, killed when it closes.
        unsafe {
            SetTimer(
                controller,
                NO_FRAME_TIMER,
                NO_FRAME_CHECK.as_millis() as u32,
                None,
            );
            SetTimer(
                controller,
                RESTACK_TIMER,
                RESTACK_CHECK.as_millis() as u32,
                None,
            );
        }
        slot.store(controller as isize, Ordering::SeqCst);
        let _ = ready.send(Ok(()));
        // SAFETY: the usual message loop of this thread's windows.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
                check_holds();
            }
        }
        slot.store(0, Ordering::SeqCst);
        STATE.with(|state| *state.borrow_mut() = None);
    }

    type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

    unsafe extern "system" fn controller_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COMMANDS => {
                apply_commands();
                0
            }
            WM_TIMER if wparam == NO_FRAME_TIMER => {
                note_missing_frames();
                0
            }
            WM_TIMER if wparam == RESTACK_TIMER => {
                keep_above_desktop();
                0
            }
            WM_CLOSE => {
                let windows = with_state(|s| s.windows.drain().collect::<Vec<_>>());
                for (hwnd, window) in windows.unwrap_or_default() {
                    widget_automation::release(hwnd, &window.view, window.provider);
                    // SAFETY: windows of this thread.
                    unsafe { DestroyWindow(hwnd as HWND) };
                }
                // SAFETY: the controller is this thread's, and so are its timers.
                unsafe {
                    KillTimer(hwnd, NO_FRAME_TIMER);
                    KillTimer(hwnd, RESTACK_TIMER);
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: the default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn apply_commands() {
        loop {
            let Some(Some(command)) = with_state(|s| s.commands.try_recv().ok()) else {
                return;
            };
            match command {
                WindowCommand::Place(widget) => place(widget),
                WindowCommand::Frame(frame) => {
                    let hwnd = with_state(|s| {
                        let hwnd = *s.by_id.get(&frame.widget_id)?;
                        let window = s.windows.get_mut(&hwnd)?;
                        if window.noted {
                            note(
                                s.logger.as_deref(),
                                &format!(
                                    "widget {}: its first image came, {}x{} pixels",
                                    frame.widget_id, frame.width, frame.height
                                ),
                            );
                        }
                        window.waiting_since = None;
                        window.noted = false;
                        window.frame = Some(frame);
                        window.base = None;
                        Some(hwnd)
                    })
                    .flatten();
                    if let Some(hwnd) = hwnd {
                        present(hwnd as HWND);
                    }
                }
                WindowCommand::Remove(widget_id) => {
                    let gone = with_state(|s| {
                        let hwnd = s.by_id.remove(&widget_id)?;
                        Some((hwnd, s.windows.remove(&hwnd)))
                    })
                    .flatten();
                    if let Some((hwnd, window)) = gone {
                        if let Some(window) = window {
                            widget_automation::release(hwnd, &window.view, window.provider);
                        }
                        // SAFETY: a window of this thread.
                        unsafe { DestroyWindow(hwnd as HWND) };
                    }
                }
            }
        }
    }

    /// Logs each widget window that has waited [`NO_FRAME`] for its first image from Android,
    /// once, so a widget that shows nothing is visible in the log.
    fn note_missing_frames() {
        let now = Instant::now();
        with_state(|s| {
            for (hwnd, window) in s.windows.iter_mut() {
                if !frame_overdue(window.waiting_since, window.noted, now) {
                    continue;
                }
                window.noted = true;
                let waited = window
                    .waiting_since
                    .map(|since| now.saturating_duration_since(since).as_secs())
                    .unwrap_or(NO_FRAME.as_secs());
                let rect = rect_of(*hwnd as HWND);
                note(
                    s.logger.as_deref(),
                    &format!(
                        "widget {} ({}) has had no image from Android for {waited} s; its window of {}x{} pixels shows only its placeholder. The service's and the agent's logs say why",
                        window.widget.widget_id,
                        window.widget.provider.provider,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                    ),
                );
            }
        });
    }

    /// The desktop's own window: the shell's window that holds the desktop icons, which a
    /// wallpaper slideshow or "show desktop" moves out of `Progman` into a `WorkerW`.
    pub(super) fn desktop_window() -> Option<HWND> {
        let progman = desktop_owner();
        let defview = wide("SHELLDLL_DefView");
        let worker = wide("WorkerW");
        // SAFETY: walks the top-level windows by class; the names outlive the calls.
        unsafe {
            if !progman.is_null()
                && !FindWindowExW(progman, null_mut(), defview.as_ptr(), null()).is_null()
            {
                return Some(progman);
            }
            let mut after: HWND = null_mut();
            loop {
                after = FindWindowExW(null_mut(), after, worker.as_ptr(), null());
                if after.is_null() {
                    break;
                }
                if !FindWindowExW(after, null_mut(), defview.as_ptr(), null()).is_null() {
                    return Some(after);
                }
            }
        }
        (!progman.is_null()).then_some(progman)
    }

    /// Puts every widget window that the shell left under the desktop, or above an app's
    /// window, right above the desktop again ([`right_above_desktop`]). The shell restacks
    /// its desktop window without moving the widget windows, so they hear nothing of it.
    fn keep_above_desktop() {
        let Some(desktop) = desktop_window() else {
            return;
        };
        let ours: Vec<isize> =
            with_state(|s| s.windows.keys().copied().collect()).unwrap_or_default();
        for &own in &ours {
            let rect = rect_of(own as HWND);
            let mut below = Vec::new();
            // SAFETY: walks the z-order down from a window of this thread; a window that goes
            // meanwhile ends the walk.
            unsafe {
                let mut next = GetWindow(own as HWND, GW_HWNDNEXT);
                while !next.is_null() && below.len() < 4096 {
                    let other = rect_of(next);
                    let overlaps = other.left < rect.right
                        && rect.left < other.right
                        && other.top < rect.bottom
                        && rect.top < other.bottom;
                    below.push(Below {
                        hwnd: next as isize,
                        covers: overlaps && IsWindowVisible(next) != 0 && IsIconic(next) == 0,
                    });
                    next = GetWindow(next, GW_HWNDNEXT);
                }
            }
            if !right_above_desktop(&below, desktop as isize, &ours) {
                // SAFETY: a window of this thread; WM_WINDOWPOSCHANGING puts it right above
                // the desktop.
                unsafe {
                    SetWindowPos(
                        own as HWND,
                        HWND_BOTTOM,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                    );
                }
            }
        }
    }

    /// Where `hwnd` goes in the z-order: right above the desktop ([`z_place`]).
    fn z_place_of(hwnd: HWND) -> ZPlace {
        let desktop = desktop_window();
        // SAFETY: reads the window right above the desktop's.
        let above = desktop.map(|d| unsafe { GetWindow(d, GW_HWNDPREV) });
        z_place(
            desktop.map(|d| d as isize),
            above.filter(|a| !a.is_null()).map(|a| a as isize),
            hwnd as isize,
        )
    }

    /// Opens the widget's window at its placement, or moves the open one there, and tells
    /// the service the window's size, which is how Android learns what size to draw at.
    fn place(widget: Widget) {
        let existing = with_state(|s| s.by_id.get(&widget.widget_id).copied()).flatten();
        let hwnd = match existing {
            Some(hwnd) => {
                // Placed anew (described again, as when Android restarts, or as Android does
                // when it draws the widget at a new size): an edit ends, and the window goes
                // where the placement says. While the user is moving or resizing it, the
                // window stays under the pointer: the placement is the one it reported a
                // moment ago, and moving it back made the widget jump.
                let dragging = with_state(|s| {
                    let window = s.windows.get_mut(&hwnd)?;
                    window.widget = widget.clone();
                    Some(window.gesture.dragging())
                })
                .flatten()
                .unwrap_or(false);
                if dragging {
                    return;
                }
                leave_edit(hwnd as HWND, false);
                hwnd as HWND
            }
            None => {
                let Some(class) = with_state(|s| s.class.clone()) else {
                    return;
                };
                let (style, ex_style) = window_styles();
                let (width, height) = window_size(&widget.placement, 96);
                // SAFETY: a popup window owned by the desktop; the names outlive the call.
                let hwnd = unsafe {
                    CreateWindowExW(
                        ex_style,
                        class.as_ptr(),
                        class.as_ptr(),
                        style,
                        widget.placement.x,
                        widget.placement.y,
                        width,
                        height,
                        desktop_owner(),
                        null_mut(),
                        GetModuleHandleW(null()),
                        null(),
                    )
                };
                if hwnd.is_null() {
                    with_state(|s| {
                        note(
                            s.logger.as_deref(),
                            &format!(
                                "the window of widget {} did not open: {}",
                                widget.widget_id,
                                std::io::Error::last_os_error()
                            ),
                        )
                    });
                    return;
                }
                // A finger's press reaches the widget at once, and a finger held on it moves
                // it as a held mouse button does, rather than becoming a right click.
                // SAFETY: the window was just made on this thread.
                unsafe { crate::app_window::pointer::deliver_contacts_at_once(hwnd) };
                with_state(|s| {
                    s.by_id.insert(widget.widget_id, hwnd as isize);
                    s.windows.insert(
                        hwnd as isize,
                        Window {
                            widget: widget.clone(),
                            frame: None,
                            shown: None,
                            base: None,
                            pressed: false,
                            gesture: Gesture::Idle,
                            focused: false,
                            menu_open: false,
                            resizes: ResizeReports::new(),
                            resize_ended: true,
                            waiting_since: Some(Instant::now()),
                            noted: false,
                            view: Arc::new(Mutex::new(WidgetView::default())),
                            provider: None,
                            editing: None,
                        },
                    );
                });
                hwnd
            }
        };
        let dpi = dpi_of(hwnd);
        let (width, height) = window_size(&widget.placement, dpi);
        let (x, y) = match work_area(hwnd) {
            Some(area) => reachable_origin(
                widget.placement.x,
                widget.placement.y,
                width,
                height,
                &[area],
            ),
            None => (widget.placement.x, widget.placement.y),
        };
        // SAFETY: a window of this thread; it goes right above the desktop, where
        // WM_WINDOWPOSCHANGING puts it.
        unsafe { SetWindowPos(hwnd, HWND_BOTTOM, x, y, width, height, SWP_NOACTIVATE) };
        // A layered window shows nothing at all until it is given a picture, so it gets its
        // placeholder before it is shown, never after.
        present(hwnd);
        // SAFETY: as above.
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        keep_above_desktop();
        report_placement(hwnd);
    }

    fn report_placement(hwnd: HWND) {
        let rect = widget_rect(hwnd);
        let dpi = dpi_of(hwnd);
        let widget = (rect.left, rect.top, rect.right, rect.bottom);
        with_state(|s| {
            let Some(window) = s.windows.get(&(hwnd as isize)) else {
                return;
            };
            let _ = s
                .reports
                .send(placed_report(window.widget.widget_id, widget, dpi));
        });
    }

    /// The widget's image at its own size, as Android drew it: its latest frame, or the last
    /// one that had something in it when Android sent one with nothing, with its width and
    /// height; None until the first one comes.
    fn frame_pixels(
        window: &mut Window,
        decoder: Option<&super::png::Decoder>,
        logger: Option<&CompositorLogger>,
    ) -> Option<(usize, usize, Vec<u8>)> {
        let decode = |frame: &WidgetFrame| {
            let decoder = decoder?;
            // Decoded at the size it was drawn at: never scaled to the window.
            let (w, h) = super::png::size(&frame.png).unwrap_or((frame.width, frame.height));
            let (w, h) = (w.max(1), h.max(1));
            decoder
                .decode(&frame.png, w, h)
                .map_err(|why| note(logger, &format!("a widget's image: {why}")))
                .ok()
                .map(|pixels| (w as usize, h as usize, pixels))
        };
        let mut decoded = window.frame.as_ref().and_then(decode);
        if decoded
            .as_ref()
            .is_some_and(|(_, _, pixels)| super::all_clear(pixels))
        {
            // A widget whose app stopped drawing keeps its last image; the placeholder is
            // only for a widget that has never shown one.
            note(
                logger,
                &format!(
                    "widget {}: Android sent an image with nothing in it; {}",
                    window.widget.widget_id,
                    if window.shown.is_some() {
                        "keeping the last one"
                    } else {
                        "showing the placeholder"
                    }
                ),
            );
            window.frame = window.shown.clone();
            decoded = window.frame.as_ref().and_then(decode);
        } else if decoded.is_some() {
            window.shown = window.frame.clone();
        }
        decoded
    }

    /// Draws the widget's image, and nothing over it. The image is Android's at its own size,
    /// never scaled: while the window is resized it shows the last one from its top left
    /// corner until Android sends one of the new size. Pixels the widget leaves clear stay
    /// just visible enough to be clicked, so the whole window takes the pointer.
    fn present(hwnd: HWND) {
        let rect = rect_of(hwnd);
        let (width, height) = (
            (rect.right - rect.left).max(1),
            (rect.bottom - rect.top).max(1),
        );
        let dpi = dpi_of(hwnd);
        let pixels = with_state(|s| {
            let State {
                windows,
                decoder,
                logger,
                theme,
                ..
            } = s;
            let window = windows.get_mut(&(hwnd as isize))?;
            // While it is edited the widget is lifted: its image at its own size, raised a
            // little over its shadow, inside the window's room for its border and handles.
            let resize_mode = window.widget.provider.resize_mode;
            let editing = window.editing.is_some();
            let widget = if editing {
                let (_, _, ww, wh) = edit_layout(width, height, dpi, resize_mode).widget;
                (ww.max(1) as usize, wh.max(1) as usize)
            } else {
                (width as usize, height as usize)
            };
            if window.base.is_none() {
                let (decoder, logger) = (decoder.as_ref(), logger.as_deref());
                window.base = frame_pixels(window, decoder, logger)
                    .map(|(w, h, pixels)| (w as i32, h as i32, pixels));
            }
            let card;
            let (image, from): (&[u8], (usize, usize)) = match &window.base {
                Some((w, h, image)) => (image, (*w as usize, *h as usize)),
                None => {
                    card = super::placeholder(widget.0, widget.1, dpi);
                    (&card, widget)
                }
            };
            let (w, h) = (width as usize, height as usize);
            let mut pixels = if editing {
                draw_lifted(image, from, w, h, dpi, resize_mode, &edit_look(theme))
            } else {
                super::unscaled(image, from, (w, h))
            };
            // Every pixel takes the pointer: around a widget being edited too, so the whole
            // reach of a handle grabs it and a press beside the widget ends the edit.
            keep_clickable(&mut pixels);
            Some(pixels)
        })
        .flatten();
        let Some(pixels) = pixels else {
            return;
        };
        let mut committed = false;
        let mut failure = None;
        // SAFETY: a DIB section of the window's size, filled from `pixels`, selected into a
        // memory DC for UpdateLayeredWindow and freed after.
        unsafe {
            let screen = GetDC(null_mut());
            let memory = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let mut bits: *mut core::ffi::c_void = null_mut();
            let bitmap = CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if !bitmap.is_null() && !bits.is_null() {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
                let old = SelectObject(memory, bitmap);
                let size = SIZE {
                    cx: width,
                    cy: height,
                };
                let source = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                committed = UpdateLayeredWindow(
                    hwnd,
                    screen,
                    null(),
                    &size,
                    memory,
                    &source,
                    0,
                    &blend,
                    ULW_ALPHA,
                ) != 0;
                if !committed {
                    failure = Some(std::io::Error::last_os_error());
                }
                SelectObject(memory, old);
                DeleteObject(bitmap);
            } else {
                failure = Some(std::io::Error::last_os_error());
            }
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
        }
        if let Some(why) = failure {
            // Without a picture a layered window is not drawn at all, so this is logged.
            with_state(|s| {
                let id = s.windows.get(&(hwnd as isize)).map(|w| w.widget.widget_id);
                note(
                    s.logger.as_deref(),
                    &format!(
                        "widget {}: its window of {width}x{height} pixels could not be drawn: {why}",
                        id.unwrap_or_default()
                    ),
                );
            });
        }
        record(hwnd, committed, width, height, &pixels);
        sync_view(hwnd);
    }

    /// Writes what UI Automation reads of the window now.
    fn sync_view(hwnd: HWND) {
        let text = widget_menu::display_text();
        with_state(|s| {
            let Some(window) = s.windows.get(&(hwnd as isize)) else {
                return;
            };
            let mut view = window.view.lock().unwrap_or_else(|e| e.into_inner());
            view.alive = true;
            view.hwnd = hwnd as isize;
            view.name = widget_menu::menu_name(&text, &window.widget.provider);
            view.focused = window.focused;
        });
    }

    /// Notes a step of a hold in the log: why a press held until its timer did not lift the
    /// widget, or that the timer came early and the rest is waited out.
    fn note_hold(hwnd: HWND, step: &Step) {
        let what = match step {
            Step::HoldRefused(why) => format!("a press was held but not lifted: {}", why.reason()),
            Step::HoldOn(left) => format!(
                "the long press timer came {} ms early; waiting out the rest",
                left.as_millis().max(1)
            ),
            _ => return,
        };
        with_state(|s| {
            let id = s.windows.get(&(hwnd as isize)).map(|w| w.widget.widget_id);
            note(
                s.logger.as_deref(),
                &format!("widget {}: {what}", id.unwrap_or_default()),
            );
        });
    }

    /// Checks the hold of every window whose press is due to lift its widget. The timer of
    /// a window is a low-priority message, which Windows makes only while no other message
    /// waits: while Android redraws a widget under the press (a pressed button's ripple, or
    /// album art), its images keep the queue busy and the timer came late or never, so a
    /// long press on a button of the widget did not lift it. The message loop checks after
    /// every message too.
    fn check_holds() {
        let now = Instant::now();
        let due: Vec<isize> = with_state(|s| {
            s.windows
                .iter()
                .filter(|(_, w)| match w.gesture {
                    Gesture::Widget { since, slid, .. } => {
                        !slid && now.saturating_duration_since(since) >= HOLD_TO_MOVE
                    }
                    _ => false,
                })
                .map(|(hwnd, _)| *hwnd)
                .collect()
        })
        .unwrap_or_default();
        for key in due {
            let hwnd = key as HWND;
            let mut cursor = POINT { x: 0, y: 0 };
            // SAFETY: the hold timer of a window of this thread, and a plain read of the
            // cursor.
            unsafe {
                KillTimer(hwnd, HOLD_TIMER);
                GetCursorPos(&mut cursor);
            }
            let rect = rect_of(hwnd);
            let (at, origin) = ((cursor.x, cursor.y), (rect.left, rect.top));
            let step = gesture(hwnd, |g| g.held(at, origin, now));
            do_step(hwnd, step, 0);
        }
    }

    /// How far the pointer moves before a press is a drag, at `dpi`.
    fn drag_threshold(dpi: u32) -> (i32, i32) {
        // SAFETY: plain reads of the system's metrics.
        let x = unsafe { GetSystemMetricsForDpi(SM_CXDRAG, dpi) };
        // SAFETY: as above.
        let y = unsafe { GetSystemMetricsForDpi(SM_CYDRAG, dpi) };
        (x.max(1), y.max(1))
    }

    /// How far the press being handled may move and still be a long press, at `dpi`: the
    /// drag threshold, widened for a finger or a pen.
    fn threshold_now(dpi: u32) -> (i32, i32) {
        // SAFETY: reads the extra information of the message being handled.
        let extra = unsafe { GetMessageExtraInfo() };
        let touch = crate::app_window::pointer::made_from_touch_or_pen(extra);
        press_threshold(drag_threshold(dpi), touch, dpi)
    }

    /// Runs a step of the window's gesture.
    fn gesture(hwnd: HWND, step: impl FnOnce(&mut Gesture) -> Step) -> Step {
        let key = hwnd as isize;
        let done = with_state(|s| s.windows.get_mut(&key).map(|w| step(&mut w.gesture)));
        done.flatten().unwrap_or(Step::Nothing)
    }

    fn set_pressed(hwnd: HWND, pressed: bool) {
        with_state(|s| {
            if let Some(w) = s.windows.get_mut(&(hwnd as isize)) {
                w.pressed = pressed;
            }
        });
    }

    /// Moves the window's top left corner to `x`, `y` on the screen.
    fn move_to(hwnd: HWND, (x, y): (i32, i32)) {
        // SAFETY: a window of this thread; it keeps its size and its place in the z-order.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
        }
    }

    /// Does what a step of the window's gesture asks.
    fn do_step(hwnd: HWND, step: Step, lparam: LPARAM) {
        match step {
            Step::Nothing => {}
            Step::Android(action) => {
                if action == PointerAction::Down {
                    set_pressed(hwnd, true);
                }
                pointer(hwnd, action, lparam);
                if matches!(action, PointerAction::Up | PointerAction::Cancel) {
                    set_pressed(hwnd, false);
                }
            }
            Step::HoldOn(left) => {
                note_hold(hwnd, &step);
                // SAFETY: a timer of this window, killed when the press ends.
                unsafe { SetTimer(hwnd, HOLD_TIMER, hold_timer_ms(left), None) };
            }
            Step::HoldRefused(_) => note_hold(hwnd, &step),
            Step::Lift => {
                // The long press is not the widget's: Android's press is cancelled, so it
                // never becomes a click, before the widget lifts.
                pointer(hwnd, PointerAction::Cancel, 0);
                set_pressed(hwnd, false);
                enter_edit(hwnd);
            }
            Step::EditMenu => open_menu(hwnd, MenuAt::Widget),
            Step::StartMove { to } => {
                // SAFETY: a system cursor, shown while the window moves.
                unsafe { SetCursor(LoadCursorW(null_mut(), IDC_SIZEALL)) };
                move_to(hwnd, to);
            }
            Step::MoveTo(to) => move_to(hwnd, to),
            Step::EndMove => {
                report_placement(hwnd);
                // A finished move ends the edit, as on a phone's home screen.
                leave_edit(hwnd, true);
            }
            Step::StartResize(edge) => {
                with_state(|s| {
                    if let Some(w) = s.windows.get_mut(&(hwnd as isize)) {
                        w.resizes.restart();
                        w.resize_ended = false;
                    }
                });
                set_cursor(edge.cursor());
                // SAFETY: a timer of this window, killed once the resize has passed on its
                // last size.
                unsafe { SetTimer(hwnd, RESIZE_TIMER, RESIZE_POLL.as_millis() as u32, None) };
            }
            Step::ResizeTo(to) => resize_to(hwnd, to),
            Step::EndResize => {
                // The last size is passed on now, or as soon as the last one passed on is
                // far enough behind, and the service keeps it.
                with_state(|s| {
                    if let Some(w) = s.windows.get_mut(&(hwnd as isize)) {
                        w.resize_ended = true;
                    }
                });
                poll_resize(hwnd);
                let rect = widget_rect(hwnd);
                with_state(|s| {
                    let id = s.windows.get(&(hwnd as isize)).map(|w| w.widget.widget_id);
                    note(
                        s.logger.as_deref(),
                        &format!(
                            "widget {}: resized to {}x{} pixels",
                            id.unwrap_or_default(),
                            rect.right - rect.left,
                            rect.bottom - rect.top
                        ),
                    );
                });
                // A finished resize ends the edit, as on a phone's home screen.
                leave_edit(hwnd, true);
            }
        }
    }

    #[cfg(test)]
    fn record(hwnd: HWND, committed: bool, width: i32, height: i32, pixels: &[u8]) {
        if let Ok(mut presented) = PRESENTED.lock() {
            presented.push(Presented {
                hwnd: hwnd as isize,
                committed,
                width,
                height,
                opaque: pixels.chunks_exact(4).filter(|p| p[3] >= 0x80).count(),
                clear: pixels.chunks_exact(4).filter(|p| p[3] <= 1).count(),
                // SAFETY: a window of this thread.
                visible: unsafe { IsWindowVisible(hwnd) } != 0,
            });
        }
    }

    #[cfg(not(test))]
    fn record(_: HWND, _: bool, _: i32, _: i32, _: &[u8]) {}

    unsafe extern "system" fn widget_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let key = hwnd as isize;
        match msg {
            // The whole window is client area: its edges and handles are resized by the
            // window's own drag (`Gesture::Resizing`), the same for a widget and one being
            // edited, which keeps the widget's image unscaled and tells Android the size as it
            // changes. The system's sizing loop stretched the old image to the new size.
            WM_NCHITTEST => HTCLIENT as LRESULT,
            WM_SETCURSOR if (lparam & 0xffff) as u32 == HTCLIENT => {
                let mut cursor = POINT { x: 0, y: 0 };
                // SAFETY: a plain read of the cursor's position.
                unsafe { GetCursorPos(&mut cursor) };
                let shape = hit_at(hwnd, cursor.x, cursor.y).cursor();
                set_cursor(shape);
                1
            }
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_WINDOWPOSCHANGING => {
                // Kept right above the desktop, below every other window, whatever raises it.
                let pos = lparam as *mut WINDOWPOS;
                // SAFETY: the system passes a WINDOWPOS it reads back.
                unsafe {
                    if !pos.is_null() && (*pos).flags & SWP_NOZORDER == 0 {
                        match z_place_of(hwnd) {
                            ZPlace::After(above) => (*pos).hwndInsertAfter = above as HWND,
                            ZPlace::Keep => (*pos).flags |= SWP_NOZORDER,
                            ZPlace::Bottom => (*pos).hwndInsertAfter = HWND_BOTTOM,
                        }
                    }
                }
                0
            }
            WM_GETMINMAXINFO => {
                // The widget's own limits, and the room around it while it is edited.
                let dpi = dpi_of(hwnd);
                let limits = with_state(|s| {
                    s.windows.get(&key).map(|w| {
                        let room = w.editing.map_or(0, |edit| 2 * edit.margin);
                        let (min_w, min_h) = min_window_size(&w.widget.provider, dpi);
                        let (max_w, max_h) = max_window_size(&w.widget.provider, dpi);
                        (min_w + room, min_h + room, max_w + room, max_h + room)
                    })
                })
                .flatten();
                if let Some((min_w, min_h, max_w, max_h)) = limits {
                    let info = lparam as *mut MINMAXINFO;
                    // SAFETY: the system passes a MINMAXINFO it reads back.
                    unsafe {
                        if !info.is_null() {
                            (*info).ptMinTrackSize = POINT { x: min_w, y: min_h };
                            (*info).ptMaxTrackSize = POINT {
                                x: max_w.max(min_w),
                                y: max_h.max(min_h),
                            };
                        }
                    }
                }
                0
            }
            WM_SIZE => {
                present(hwnd);
                0
            }
            WM_DPICHANGED => {
                let suggested = lparam as *const RECT;
                // An edit ends on a monitor of another scale: its room is in the old scale's
                // pixels, so the widget keeps the suggested place less the room at the new one.
                let new_dpi = ((wparam >> 16) & 0xffff) as u32;
                let room = with_state(|s| {
                    let w = s.windows.get_mut(&key)?;
                    w.base = None;
                    w.editing.take().map(|_| edit_margin(new_dpi.max(1)))
                })
                .flatten()
                .unwrap_or(0);
                // SAFETY: the system passes the rectangle it suggests for the new scale.
                unsafe {
                    if !suggested.is_null() {
                        let r = *suggested;
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            r.left + room,
                            r.top + room,
                            (r.right - r.left - 2 * room).max(1),
                            (r.bottom - r.top - 2 * room).max(1),
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                }
                report_placement(hwnd);
                present(hwnd);
                0
            }
            WM_MOUSEMOVE => {
                // Only a press in progress hears the pointer: hovering draws nothing.
                let rect = rect_of(hwnd);
                let (x, y) = (lo(lparam), hi(lparam));
                let (at, origin) = ((rect.left + x, rect.top + y), (rect.left, rect.top));
                let threshold = threshold_now(dpi_of(hwnd));
                let step = gesture(hwnd, |g| g.moved(at, origin, threshold));
                do_step(hwnd, step, lparam);
                0
            }
            WM_LBUTTONDOWN => {
                let rect = rect_of(hwnd);
                let (x, y) = (lo(lparam), hi(lparam));
                press(hwnd, (rect.left + x, rect.top + y), lparam);
                0
            }
            WM_LBUTTONUP => {
                // SAFETY: the hold timer of this window.
                unsafe { KillTimer(hwnd, HOLD_TIMER) };
                let step = gesture(hwnd, Gesture::up);
                // SAFETY: gives the mouse back; the gesture has ended, so losing it ends
                // nothing.
                unsafe { ReleaseCapture() };
                do_step(hwnd, step, lparam);
                0
            }
            WM_CAPTURECHANGED => {
                // SAFETY: the hold timer of this window.
                unsafe { KillTimer(hwnd, HOLD_TIMER) };
                let step = gesture(hwnd, Gesture::cancel);
                if step == Step::Android(PointerAction::Cancel) {
                    // A press still in progress lost the mouse to another window: it never
                    // becomes a long press, which the log says.
                    with_state(|s| {
                        let id = s.windows.get(&key).map(|w| w.widget.widget_id);
                        note(
                            s.logger.as_deref(),
                            &format!(
                                "widget {}: a press was held but not lifted: the window lost the mouse before the button went up",
                                id.unwrap_or_default()
                            ),
                        );
                    });
                }
                do_step(hwnd, step, 0);
                0
            }
            WM_TIMER if wparam == RESIZE_TIMER => {
                poll_resize(hwnd);
                0
            }
            WM_TIMER if wparam == HOLD_TIMER => {
                let mut cursor = POINT { x: 0, y: 0 };
                // SAFETY: the hold timer of this window, and a plain read of the cursor.
                unsafe {
                    KillTimer(hwnd, HOLD_TIMER);
                    GetCursorPos(&mut cursor);
                }
                let rect = rect_of(hwnd);
                let (at, origin) = ((cursor.x, cursor.y), (rect.left, rect.top));
                let now = Instant::now();
                let step = gesture(hwnd, |g| g.held(at, origin, now));
                do_step(hwnd, step, 0);
                0
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                let focused = msg == WM_SETFOCUS;
                let seen = with_state(|s| {
                    let w = s.windows.get_mut(&key)?;
                    w.focused = focused;
                    Some((Arc::clone(&w.view), w.menu_open))
                })
                .flatten();
                // The keyboard went to another window, as a click on it gives it: an edit
                // ends. Its own menu taking the keyboard ends nothing.
                if !focused && matches!(seen, Some((_, false))) {
                    leave_edit(hwnd, false);
                }
                sync_view(hwnd);
                if let (true, Some((view, _))) = (focused, seen) {
                    widget_automation::raise_focus(&view);
                }
                0
            }
            WM_KEYDOWN if wparam as u16 == VK_ESCAPE => {
                leave_edit(hwnd, true);
                0
            }
            WM_KEYDOWN if matches!(wparam as u16, VK_RETURN | VK_SPACE) => {
                open_menu(hwnd, MenuAt::Keyboard);
                0
            }
            WM_WIDGET_FOCUS => {
                // SAFETY: a window of this thread takes the keyboard, as UI Automation asked.
                unsafe {
                    SetForegroundWindow(hwnd);
                    SetFocus(hwnd);
                }
                0
            }
            WM_GETOBJECT => {
                let provider = with_state(|s| {
                    let w = s.windows.get_mut(&key)?;
                    let view = Arc::clone(&w.view);
                    let slot = &mut w.provider;
                    let provider = slot.get_or_insert_with(|| widget_automation::provider(&view));
                    Some(provider.clone())
                })
                .flatten();
                match provider.and_then(|p| widget_automation::answer(key, wparam, lparam, &p)) {
                    Some(result) => result,
                    // SAFETY: the default handling of every other object.
                    None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
                }
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
                // An edit's border and handles follow the theme and high contrast as they
                // change.
                with_state(|s| s.theme = read_theme());
                // SAFETY: the default handling of the message.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_CONTEXTMENU => {
                // A right click opens the menu at the pointer; the menu key or Shift+F10
                // opens it under the widget.
                open_menu(hwnd, context_menu_at(lparam));
                0
            }
            WM_CLOSE => 0,
            WM_NCDESTROY => {
                // SAFETY: the window is still alive until this message returns.
                unsafe {
                    crate::app_window::pointer::forget_contacts(hwnd);
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            m if m != 0 && with_state(|s| s.taskbar_created).unwrap_or(0) == m => {
                // The shell restarted: the window belongs to its new desktop.
                // SAFETY: a window of this thread.
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, desktop_owner() as isize);
                    SetWindowPos(
                        hwnd,
                        HWND_BOTTOM,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                    );
                }
                0
            }
            // SAFETY: the default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// The window's menu is open, or closed again: the keyboard going to the menu ends no
    /// edit while it is.
    fn set_menu_open(hwnd: HWND, open: bool) {
        with_state(|s| {
            if let Some(w) = s.windows.get_mut(&(hwnd as isize)) {
                w.menu_open = open;
            }
        });
    }

    /// What the point `x`, `y` on the screen hits in the window ([`hit_test`]).
    fn hit_at(hwnd: HWND, x: i32, y: i32) -> Hit {
        let rect = rect_of(hwnd);
        let (resize_mode, editing) = with_state(|s| {
            s.windows
                .get(&(hwnd as isize))
                .map(|w| (w.widget.provider.resize_mode, w.editing.is_some()))
        })
        .flatten()
        .unwrap_or((0, false));
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y) = (x - rect.left, y - rect.top);
        hit_test(editing, x, y, width, height, dpi_of(hwnd), resize_mode)
    }

    /// Shows `shape` as the cursor.
    fn set_cursor(shape: Cursor) {
        let id = match shape {
            Cursor::Arrow => IDC_ARROW,
            Cursor::SizeWestEast => IDC_SIZEWE,
            Cursor::SizeNorthSouth => IDC_SIZENS,
            Cursor::SizeNorthWestSouthEast => IDC_SIZENWSE,
            Cursor::SizeNorthEastSouthWest => IDC_SIZENESW,
        };
        // SAFETY: a system cursor.
        unsafe { SetCursor(LoadCursorW(null_mut(), id)) };
    }

    /// The smallest and largest the window may be now, in pixels: the widget's own limits,
    /// and the room around it while it is edited.
    fn size_limits(hwnd: HWND) -> Option<((i32, i32), (i32, i32))> {
        let dpi = dpi_of(hwnd);
        with_state(|s| {
            s.windows.get(&(hwnd as isize)).map(|w| {
                let room = w.editing.map_or(0, |edit| 2 * edit.margin);
                let (min_w, min_h) = min_window_size(&w.widget.provider, dpi);
                let (max_w, max_h) = max_window_size(&w.widget.provider, dpi);
                (
                    (min_w + room, min_h + room),
                    (max_w.max(min_w) + room, max_h.max(min_h) + room),
                )
            })
        })
        .flatten()
    }

    /// A press of the left button at `at` on the screen: an edge or a handle starts a resize,
    /// a press beside a widget being edited ends the edit, and any other goes to the widget
    /// ([`Gesture::press`]). `lparam` is the press's own message parameter, 0 when it came
    /// through the widget's menu.
    fn press(hwnd: HWND, at: (i32, i32), lparam: LPARAM) {
        // A press on any widget ends the edit of every other one, as a click outside a
        // widget being edited does; a press beside this one ends its own.
        end_other_edits(hwnd);
        let editing = edit_of(hwnd).is_some();
        let hit = hit_at(hwnd, at.0, at.1);
        if hit == Hit::Outside {
            leave_edit(hwnd, true);
            return;
        }
        let rect = rect_of(hwnd);
        let Some((min, max)) = size_limits(hwnd) else {
            return;
        };
        let press = Press {
            hit,
            editing,
            window: (rect.left, rect.top, rect.right, rect.bottom),
            min,
            max,
        };
        // SAFETY: the window takes the mouse until the button is released.
        unsafe { SetCapture(hwnd) };
        let now = Instant::now();
        let step = gesture(hwnd, |g| g.press(press, at, now));
        if matches!(step, Step::Android(_)) {
            let hold = HOLD_TO_MOVE.as_millis() as u32;
            // SAFETY: a timer of this window, killed when the press ends.
            unsafe { SetTimer(hwnd, HOLD_TIMER, hold, None) };
        }
        let lparam = if lparam == 0 {
            // From the screen to the window's own corner, as the press's message has it.
            let (x, y) = (at.0 - rect.left, at.1 - rect.top);
            ((y as u16 as LPARAM) << 16) | (x as u16 as LPARAM)
        } else {
            lparam
        };
        do_step(hwnd, step, lparam);
    }

    /// Resizes the window to `to` on the screen. The widget's size is passed on when the
    /// drag pauses and when it ends ([`ResizeReports`]); until Android's image of that size
    /// comes, the window shows the last one unscaled.
    fn resize_to(hwnd: HWND, to: super::ScreenRect) {
        let (l, t, r, b) = to;
        // SAFETY: a window of this thread; it keeps its place in the z-order.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                l,
                t,
                (r - l).max(1),
                (b - t).max(1),
                SWP_NOACTIVATE | SWP_NOZORDER,
            );
        }
        let rect = widget_rect(hwnd);
        let widget = (rect.left, rect.top, rect.right, rect.bottom);
        let now = Instant::now();
        with_state(|s| {
            if let Some(w) = s.windows.get_mut(&(hwnd as isize)) {
                w.resizes.moved(widget, now);
            }
        });
    }

    /// Passes the widget's size on if a resize has one due ([`ResizeReports::poll`]), and
    /// stops the window's resize timer once an ended resize has nothing more to pass on.
    fn poll_resize(hwnd: HWND) {
        let now = Instant::now();
        let polled = with_state(|s| {
            let w = s.windows.get_mut(&(hwnd as isize))?;
            Some((w.resizes.poll(now, w.resize_ended), w.resize_ended))
        })
        .flatten();
        let Some((poll, ended)) = polled else {
            return;
        };
        if let ResizePoll::Send(_) = poll {
            report_placement(hwnd);
        }
        if ended && poll != ResizePoll::Wait {
            // SAFETY: the resize timer of this window.
            unsafe { KillTimer(hwnd, RESIZE_TIMER) };
        }
    }

    /// Ends the edit of every widget but `hwnd`'s.
    fn end_other_edits(hwnd: HWND) {
        let key = hwnd as isize;
        let others: Vec<isize> = with_state(|s| {
            s.windows
                .iter()
                .filter(|(other, w)| **other != key && w.editing.is_some())
                .map(|(other, _)| *other)
                .collect()
        })
        .unwrap_or_default();
        for other in others {
            leave_edit(other as HWND, false);
        }
    }

    /// The lifted widget's rectangle on the screen while it is edited, else the widget's.
    fn shown_on_screen(hwnd: HWND) -> (i32, i32, i32, i32) {
        let rect = rect_of(hwnd);
        if edit_of(hwnd).is_none() {
            return (rect.left, rect.top, rect.right, rect.bottom);
        }
        let resize_mode = with_state(|s| {
            s.windows
                .get(&(hwnd as isize))
                .map(|w| w.widget.provider.resize_mode)
        })
        .flatten()
        .unwrap_or(0);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y, w, h) = edit_layout(width, height, dpi_of(hwnd), resize_mode).lifted;
        (
            rect.left + x,
            rect.top + y,
            rect.left + x + w,
            rect.top + y + h,
        )
    }

    /// Shows the window's menu (Widget settings when the widget can be set up again, Resize,
    /// Remove) where `at` says, and does what the user chose. Choosing anything ends an edit;
    /// closing the menu by pressing outside the widget, or on another window, ends it too.
    fn open_menu(hwnd: HWND, at: MenuAt) {
        let key = hwnd as isize;
        let Some((provider, placement, widget_id)) = with_state(|s| {
            s.windows.get(&key).map(|w| {
                (
                    w.widget.provider.clone(),
                    w.widget.placement,
                    w.widget.widget_id,
                )
            })
        })
        .flatten() else {
            return;
        };
        let rect = widget_rect(hwnd);
        let editing = edit_of(hwnd).is_some();
        // The menu of a widget being edited opens under it, as on a phone's home screen.
        let (anchor, keyboard) = menu_anchor(at, shown_on_screen(hwnd));
        let dpi = dpi_of(hwnd);
        // The size shown as current is the window's own, which a drag may have changed.
        let shown = placement_of(
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            dpi,
        );
        let placement = Placement {
            width_dp: shown.width_dp,
            height_dp: shown.height_dp,
            ..placement
        };
        set_menu_open(hwnd, true);
        let choice = widget_menu::show(key, anchor, keyboard, &provider, &placement);
        set_menu_open(hwnd, false);
        match choice {
            Some(MenuChoice::Resize {
                width_dp,
                height_dp,
            }) => {
                // The edit ends first, so the window is the widget's own size again.
                leave_edit(hwnd, true);
                let (width, height) = window_size(
                    &Placement {
                        width_dp,
                        height_dp,
                        ..placement
                    },
                    dpi,
                );
                // SAFETY: a window of this thread; it keeps its place and its z-order.
                unsafe {
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        0,
                        0,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
                    );
                }
                report_placement(hwnd);
            }
            Some(MenuChoice::Remove) => {
                leave_edit(hwnd, true);
                with_state(|s| {
                    let _ = s.reports.send(WindowReport::Remove { widget_id });
                });
            }
            Some(MenuChoice::Settings) => {
                // The setup opens as the app's window, which takes the keyboard.
                leave_edit(hwnd, false);
                with_state(|s| {
                    let _ = s.reports.send(WindowReport::Reconfigure { widget_id });
                });
            }
            None if editing => {
                // Closed by a press: the menu took it, so a press on a handle goes on as that
                // press, a resize; one beside the widget or on another window ends the edit;
                // closed with Esc or by a press on the widget, the edit goes on.
                let mut cursor = POINT { x: 0, y: 0 };
                // SAFETY: plain reads of the cursor, the buttons and the foreground window.
                let (left, right, foreground) = unsafe {
                    GetCursorPos(&mut cursor);
                    let down = |vk: u16| GetAsyncKeyState(i32::from(vk)) as u16 & 0x8000 != 0;
                    (down(VK_LBUTTON), down(VK_RBUTTON), GetForegroundWindow())
                };
                let hit = hit_at(hwnd, cursor.x, cursor.y);
                match menu_closed_by(hit, left, right, foreground == hwnd) {
                    MenuClosed::KeepEditing => {}
                    MenuClosed::EndEdit { give_back } => leave_edit(hwnd, give_back),
                    MenuClosed::GoOnPressing => press(hwnd, (cursor.x, cursor.y), 0),
                }
            }
            None => {}
        }
    }

    /// Reports pointer input inside the widget, in its frame's pixels. Moves count only while
    /// the button is down.
    fn pointer(hwnd: HWND, action: PointerAction, lparam: LPARAM) {
        let rect = rect_of(hwnd);
        let window = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y) = (lo(lparam), hi(lparam));
        with_state(|s| {
            let Some(w) = s.windows.get(&(hwnd as isize)) else {
                return;
            };
            if action == PointerAction::Move && !w.pressed {
                return;
            }
            let frame = w
                .frame
                .as_ref()
                .map(|f| (f.width, f.height))
                .unwrap_or((window.0.max(1) as u32, window.1.max(1) as u32));
            let (fx, fy) = to_frame(x, y, frame);
            let _ = s.reports.send(WindowReport::Pointer {
                widget_id: w.widget.widget_id,
                action,
                x: fx,
                y: fy,
            });
        });
    }
}

/// Decodes widget frames, and the icons of the apps that run in the background, with the
/// Windows Imaging Component.
#[cfg(windows)]
pub(crate) mod png {
    use windows::core::GUID;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA,
        IWICBitmapSource, IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone,
        WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

    pub(crate) struct Decoder {
        factory: IWICImagingFactory,
    }

    /// Runs `f` with COM entered on this thread, as the imaging component needs, and leaves
    /// it as it was. A thread already in COM, in either apartment, stays in it.
    pub(crate) fn with_com<T>(f: impl FnOnce() -> T) -> T {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        // SAFETY: enters COM for this thread; undone below only when this call entered it.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let result = f();
        if entered {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        result
    }

    /// The width and height a PNG says it has, from its header chunk.
    pub(crate) fn size(png: &[u8]) -> Option<(u32, u32)> {
        const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
        if png.len() < 24 || &png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
            return None;
        }
        let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
        (width > 0 && height > 0).then_some((width, height))
    }

    impl Decoder {
        pub(crate) fn new() -> Result<Decoder, String> {
            // SAFETY: COM is entered on this thread.
            let factory: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| format!("the imaging component is not there: {e}"))?;
            Ok(Decoder { factory })
        }

        /// The PNG image `png`, scaled to `width` by `height`, as premultiplied BGRA rows.
        pub(crate) fn decode(
            &self,
            png: &[u8],
            width: u32,
            height: u32,
        ) -> Result<Vec<u8>, String> {
            self.decode_as(png, width, height, &GUID_WICPixelFormat32bppPBGRA)
        }

        /// The same with straight alpha, as an icon's colour bitmap takes it.
        pub(crate) fn decode_straight(
            &self,
            png: &[u8],
            width: u32,
            height: u32,
        ) -> Result<Vec<u8>, String> {
            self.decode_as(png, width, height, &GUID_WICPixelFormat32bppBGRA)
        }

        fn decode_as(
            &self,
            png: &[u8],
            width: u32,
            height: u32,
            format: &GUID,
        ) -> Result<Vec<u8>, String> {
            let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
            // SAFETY: COM calls on interfaces this thread made, with buffers that outlive them.
            unsafe {
                let stream = self
                    .factory
                    .CreateStream()
                    .map_err(|e| fail("reading the image", e))?;
                stream
                    .InitializeFromMemory(png)
                    .map_err(|e| fail("reading the image", e))?;
                let decoder = self
                    .factory
                    .CreateDecoderFromStream(
                        &stream,
                        std::ptr::null(),
                        WICDecodeMetadataCacheOnDemand,
                    )
                    .map_err(|e| fail("the image is not a PNG", e))?;
                let frame = decoder
                    .GetFrame(0)
                    .map_err(|e| fail("the image has no frame", e))?;
                let converter = self
                    .factory
                    .CreateFormatConverter()
                    .map_err(|e| fail("converting the image", e))?;
                converter
                    .Initialize(
                        &frame,
                        format,
                        WICBitmapDitherTypeNone,
                        None::<&IWICPalette>,
                        0.0,
                        WICBitmapPaletteTypeCustom,
                    )
                    .map_err(|e| fail("converting the image", e))?;
                let scaler = self
                    .factory
                    .CreateBitmapScaler()
                    .map_err(|e| fail("scaling the image", e))?;
                scaler
                    .Initialize(&converter, width, height, WICBitmapInterpolationModeFant)
                    .map_err(|e| fail("scaling the image", e))?;
                let source: IWICBitmapSource = scaler.into();
                let stride = width * 4;
                let mut pixels = vec![0u8; (stride * height) as usize];
                source
                    .CopyPixels(std::ptr::null(), stride, &mut pixels)
                    .map_err(|e| fail("copying the image", e))?;
                Ok(pixels)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    use dsa_host_core::host_api::HostApiClient;
    use dsa_host_core::session::SessionState;
    use dsa_host_core::widgets::{self, Placement, Widget, WidgetEvent, WidgetFrame};
    use dsa_protocol::host_api::types::PointerAction;

    #[test]
    fn fr10_the_window_manager_starts_a_stopped_android_at_sign_in() {
        assert!(starts_android_at_sign_in(SessionState::Stopped));
        for running in [
            SessionState::Starting,
            SessionState::Active,
            SessionState::Dozing,
            SessionState::Frozen,
        ] {
            assert!(!starts_android_at_sign_in(running), "{running:?}");
        }
    }
    use dsa_host_core::widgets::WidgetProvider;

    fn clock() -> WidgetProvider {
        WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            package_name: "com.example.clock".into(),
            label: "Clock".into(),
            preview_png: Vec::new(),
            min_width_dp: 110,
            min_height_dp: 40,
            default_width_dp: 180,
            default_height_dp: 110,
            max_width_dp: 0,
            max_height_dp: 0,
            resize_mode: 3,
            needs_configuration: false,
            app_label: "Clock App".into(),
            reconfigurable: false,
        }
    }

    #[test]
    fn fr12_a_widget_window_without_an_image_shows_a_visible_placeholder() {
        let (width, height) = (600usize, 340usize);
        let pixels = placeholder(width, height, 192);
        assert_eq!(pixels.len(), width * height * 4);
        assert!(!all_clear(&pixels));
        let alpha = |x: usize, y: usize| pixels[(y * width + x) * 4 + 3];
        // The middle and every edge's midpoint are mostly opaque, so the window is never a
        // see-through hole on the desktop.
        for (x, y) in [
            (width / 2, height / 2),
            (0, height / 2),
            (width - 1, height / 2),
            (width / 2, 0),
            (width / 2, height - 1),
        ] {
            assert!(alpha(x, y) >= 0xc0, "alpha at {x},{y} is {}", alpha(x, y));
        }
        // The corners are rounded off.
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(width - 1, height - 1), 0);
        // Premultiplied: no colour exceeds its alpha.
        assert!(pixels
            .chunks_exact(4)
            .all(|p| p[..3].iter().all(|&c| c <= p[3])));
        // A tiny window still gets one.
        assert!(!all_clear(&placeholder(3, 2, 96)));
    }

    #[test]
    fn fr12_an_image_with_nothing_in_it_is_told_from_one_with_something_drawn() {
        assert!(all_clear(&[0, 0, 0, 0, 9, 9, 9, 0]));
        assert!(!all_clear(&[0, 0, 0, 0, 0, 0, 0, 1]));
    }

    #[test]
    fn fr12_a_widget_windows_size_follows_its_monitors_scale() {
        let placement = Placement {
            x: 10,
            y: 20,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        };
        // A dp is 0.819 of a device-independent pixel.
        assert_eq!(window_size(&placement, 96), (146, 89));
        assert_eq!(window_size(&placement, 144), (221, 135));
        assert_eq!(placement_of(10, 20, 221, 135, 144), placement);
        assert_eq!(android_density(96), 130);
        assert_eq!(android_density(144), 196);
        assert_eq!(android_density(120), 164);
        assert_eq!(dip_of_px(361, 240), 144);
        // 110 dp wide, and the smallest side a widget window has, 49 dp, which is 40
        // device-independent pixels.
        assert_eq!(min_window_size(&clock(), 192), (180, 80));
    }

    /// A 5 by 2 music widget placed at 369 by 170 dp was drawn with an empty band along its
    /// bottom on a monitor at 200%. Its window and the size Android draws it at must be the
    /// same pixels and the same dp at every scale, so the image fills the window as it is.
    #[test]
    fn fr12_a_widget_placed_at_369_by_170_dp_is_drawn_at_its_windows_size_at_200_and_125_percent() {
        let placement = Placement {
            x: 536,
            y: 258,
            width_dp: 369,
            height_dp: 170,
            resized: false,
        };
        for (dpi, density, pixels) in [(192, 262, (604, 278)), (120, 164, (378, 174))] {
            let (width, height) = window_size(&placement, dpi);
            assert_eq!((width, height), pixels, "the window at {dpi} dpi");
            assert_eq!(android_density(dpi), density);
            // The window reports this size, and Android is told the same dp it was placed at.
            assert_eq!(placement_of(536, 258, width, height, dpi), placement);
            let size = dsa_host_core::widgets::WidgetSize::of_pixels(
                width as u32,
                height as u32,
                android_density(dpi),
            );
            assert_eq!(
                (size.width, size.height, size.width_dp, size.height_dp),
                (width as u32, height as u32, 369, 170),
                "Android draws the widget at its window's size at {dpi} dpi"
            );
            // An image of that size covers the window pixel for pixel, not scaled.
            assert_eq!(
                to_frame(width - 1, height - 1, (size.width, size.height)),
                ((width - 1) as f32, (height - 1) as f32)
            );
        }
    }

    #[test]
    fn fr12_a_widget_window_is_kept_off_the_taskbar_and_alt_tab_and_never_takes_the_focus() {
        let (style, ex) = window_styles();
        assert_eq!(style, 0x8000_0000, "a popup with no frame");
        assert_ne!(ex & 0x80, 0, "a tool window: not in the taskbar or Alt+Tab");
        assert_ne!(
            ex & 0x0008_0000,
            0,
            "layered: see-through where the widget is"
        );
        assert_ne!(ex & 0x0800_0000, 0, "never takes the focus");
        assert_eq!(ex & 0x0000_0008, 0, "never topmost");
    }

    #[test]
    fn fr12_a_widget_window_is_resized_by_its_edges_and_has_no_band_along_its_top() {
        let (w, h) = (200, 120);
        assert_eq!(hit_test(false, 100, 60, w, h, 96, 3), Hit::Client);
        assert_eq!(
            hit_test(false, 100, 10, w, h, 96, 3),
            Hit::Client,
            "no band"
        );
        assert_eq!(hit_test(false, 2, 60, w, h, 96, 3), Hit::Left);
        assert_eq!(hit_test(false, 198, 60, w, h, 96, 3), Hit::Right);
        assert_eq!(hit_test(false, 100, 118, w, h, 96, 3), Hit::Bottom);
        assert_eq!(hit_test(false, 100, 2, w, h, 96, 3), Hit::Top);
        assert_eq!(hit_test(false, 198, 118, w, h, 96, 3), Hit::BottomRight);
        assert_eq!(hit_test(false, 1, 1, w, h, 96, 3), Hit::TopLeft);
        assert_eq!(hit_test(false, 198, 2, w, h, 96, 3), Hit::TopRight);
        // Only the edges the widget may be resized along resize it.
        assert_eq!(hit_test(false, 2, 60, w, h, 96, 2), Hit::Client);
        assert_eq!(hit_test(false, 100, 118, w, h, 96, 1), Hit::Client);
        assert_eq!(hit_test(false, 100, 2, w, h, 96, 0), Hit::Client);
        // The edges grow with the scale.
        assert_eq!(hit_test(false, 100, 30, w, h, 192, 3), Hit::Client);
        assert_eq!(hit_test(false, 10, 60, w, h, 192, 3), Hit::Left);
    }

    #[test]
    fn fr12_a_tap_and_a_drag_on_the_widget_stay_the_widgets() {
        let start = Instant::now();
        let (origin, threshold) = ((100, 100), (4, 4));
        let mut gesture = Gesture::Idle;
        assert_eq!(
            gesture.down(false, (150, 150), start),
            Step::Android(PointerAction::Down)
        );
        // A tap: the press and the release go to Android.
        assert_eq!(gesture.up(), Step::Android(PointerAction::Up));
        assert_eq!(gesture, Gesture::Idle);
        // A drag that starts at once (a list scrolling) is the widget's, however long it lasts.
        gesture.down(false, (150, 150), start);
        let moved = gesture.moved((150, 170), origin, threshold);
        assert_eq!(moved, Step::Android(PointerAction::Move));
        let later = start + HOLD_TO_MOVE * 2;
        assert_eq!(
            gesture.held((150, 170), origin, later),
            Step::HoldRefused(HoldRefused::Slid)
        );
        assert_eq!(gesture.up(), Step::Android(PointerAction::Up));
        // The window losing the mouse cancels Android's press.
        gesture.down(false, (150, 150), start);
        assert_eq!(gesture.cancel(), Step::Android(PointerAction::Cancel));
    }

    /// A long press edits the widget as Android's home screen does: the hold's timing and
    /// the drag threshold decide it, it never also clicks, letting go opens the menu and
    /// dragging moves the widget; the menu offers Widget settings only for a widget that can
    /// be set up again; and choosing it reaches Android for that widget through the service.
    #[test]
    fn fr12_long_pressing_a_widget_edits_it_like_a_home_screen() {
        let start = Instant::now();
        let (origin, threshold) = ((100, 100), (4, 4));
        let margin = edit_margin(96);

        // Held still for half a second, within the drag threshold: a long press. Android had
        // the press until then; the long press cancels it (Lift), so no click follows.
        let mut gesture = Gesture::Idle;
        assert_eq!(
            gesture.down(false, (150, 150), start),
            Step::Android(PointerAction::Down)
        );
        assert_eq!(
            gesture.moved((154, 153), origin, threshold),
            Step::Android(PointerAction::Move),
            "a wobble within the threshold is still a press"
        );
        let early = start + HOLD_TO_MOVE - Duration::from_millis(1);
        assert_eq!(
            gesture.held((154, 153), origin, early),
            Step::HoldOn(Duration::from_millis(1))
        );
        assert_eq!(
            gesture.held((154, 153), origin, start + HOLD_TO_MOVE),
            Step::Lift
        );
        // The window grows by its margin as the widget lifts.
        gesture.regrab((margin, margin));
        let lifted_origin = (origin.0 - margin, origin.1 - margin);
        assert_eq!(
            gesture.moved((156, 154), lifted_origin, threshold),
            Step::Nothing
        );
        // Let go where it was: the menu opens, and Android hears nothing more.
        assert_eq!(gesture.up(), Step::EditMenu);
        assert_eq!(gesture, Gesture::Idle);

        // A press that slides past the threshold before the half second is the widget's own,
        // and never lifts.
        let mut gesture = Gesture::Idle;
        gesture.down(false, (150, 150), start);
        gesture.moved((150, 155), origin, threshold);
        let later = start + HOLD_TO_MOVE * 2;
        assert_eq!(
            gesture.held((150, 155), origin, later),
            Step::HoldRefused(HoldRefused::Slid)
        );
        assert_eq!(gesture.up(), Step::Android(PointerAction::Up));

        // Lifted, then dragged past the threshold: the widget moves with the pointer, and
        // letting go ends the move (and the edit).
        let mut gesture = Gesture::Idle;
        gesture.down(false, (150, 150), start);
        assert_eq!(gesture.held((150, 150), origin, later), Step::Lift);
        gesture.regrab((margin, margin));
        assert_eq!(
            gesture.moved((160, 150), lifted_origin, threshold),
            Step::StartMove {
                to: (lifted_origin.0 + 10, lifted_origin.1)
            }
        );
        assert_eq!(
            gesture.moved((170, 175), lifted_origin, threshold),
            Step::MoveTo((lifted_origin.0 + 20, lifted_origin.1 + 25))
        );
        assert_eq!(gesture.up(), Step::EndMove);
        // The window losing the mouse while lifted sends Android nothing.
        gesture.down(false, (150, 150), start);
        gesture.held((150, 150), origin, later);
        assert_eq!(gesture.cancel(), Step::Nothing);

        // While it is edited, Android never has a press: a click opens the menu again, a
        // drag moves the widget at once, wherever it is pressed.
        let mut gesture = Gesture::Idle;
        assert_eq!(gesture.down(true, (150, 150), start), Step::Nothing);
        assert_eq!(gesture.up(), Step::EditMenu);
        gesture.down(true, (150, 150), start);
        assert_eq!(gesture, Gesture::Editing { down: (150, 150) });
        assert_eq!(
            gesture.moved((150, 140), origin, threshold),
            Step::StartMove { to: (100, 90) }
        );
        assert_eq!(gesture.up(), Step::EndMove);

        // A finger or a pen may wander a little more than a mouse.
        assert_eq!(press_threshold((4, 4), false, 96), (4, 4));
        assert_eq!(press_threshold((4, 4), true, 96), (8, 8));
        assert_eq!(press_threshold((4, 4), true, 192), (16, 16));
        assert_eq!(press_threshold((20, 20), true, 96), (20, 20));

        // The menu: Widget settings only for a widget whose app lets it be set up again.
        use crate::compositor::widget_menu::{self, MenuChoice, SETTINGS};
        let settable = WidgetProvider {
            provider: "com.example.weather/.WeatherWidget".into(),
            reconfigurable: true,
            needs_configuration: true,
            ..clock()
        };
        let text = widget_menu::menu_text(false);
        let placement = Placement {
            x: 0,
            y: 0,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        };
        let ids = |provider: &WidgetProvider| -> Vec<u32> {
            let sizes = widget_menu::size_choices(provider);
            widget_menu::entries(&text, &sizes, &placement, provider.reconfigurable)
                .iter()
                .filter_map(|entry| entry.item().map(|item| item.id))
                .collect()
        };
        assert_eq!(
            ids(&settable),
            vec![SETTINGS, widget_menu::RESIZE, widget_menu::REMOVE]
        );
        assert_eq!(
            ids(&clock()),
            vec![widget_menu::RESIZE, widget_menu::REMOVE]
        );
        let sizes = widget_menu::size_choices(&settable);
        assert_eq!(
            widget_menu::choice_of(SETTINGS, &sizes),
            Some(MenuChoice::Settings)
        );

        // Choosing it: the window's report goes through the window manager's watch to the
        // service, which opens the widget's setup in Android for the same widget.
        use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
        let wait = Duration::from_secs(10);
        let (listener, connector) = dsa_transport::local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let _service = RunningService::start(Box::new(listener), androids.clone());
        let mut client =
            HostApiClient::connect(&mut connector.clone(), "test", wait).expect("connecting");
        client.start().expect("starting Android");
        let android = androids
            .androids
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned()
            .expect("the caller's Android");
        android.widget_offer.lock().unwrap().push(settable.clone());
        let weather = client
            .add_widget(&settable.provider, wait)
            .expect("placing the widget");
        let windows = Scripted::default();
        windows
            .pending
            .lock()
            .unwrap()
            .push(WindowReport::Reconfigure {
                widget_id: weather.widget_id,
            });
        let stop = Arc::new(AtomicBool::new(false));
        let syncing = {
            let mut windows = windows.clone();
            let stop = Arc::clone(&stop);
            thread::spawn(move || sync(Box::new(connector), &mut windows, stop, None))
        };
        let deadline = Instant::now() + wait;
        while android.widget_reconfigures.lock().unwrap().is_empty() {
            assert!(
                Instant::now() < deadline,
                "the widget's settings never reached Android"
            );
            thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        syncing.join().unwrap();
        assert_eq!(
            *android.widget_reconfigures.lock().unwrap(),
            vec![weather.widget_id]
        );
        assert!(
            windows
                .commands
                .lock()
                .unwrap()
                .iter()
                .any(|c| matches!(c, WindowCommand::Place(w) if w.widget_id == weather.widget_id)),
            "the widget's window was opened"
        );
    }

    /// Widget windows that record what the watch asks of them and hand it the reports a
    /// test gives them.
    #[derive(Clone, Default)]
    struct Scripted {
        commands: Arc<std::sync::Mutex<Vec<WindowCommand>>>,
        pending: Arc<std::sync::Mutex<Vec<WindowReport>>>,
    }

    impl WidgetWindows for Scripted {
        fn command(&mut self, command: WindowCommand) {
            self.commands.lock().unwrap().push(command);
        }

        fn reports(&mut self) -> Vec<WindowReport> {
            std::mem::take(&mut *self.pending.lock().unwrap())
        }
    }

    #[test]
    fn fr12_a_widget_being_edited_lifts_with_a_border_and_a_handle_on_each_edge_it_resizes() {
        // A widget of 108 by 66 pixels at 100%, with 16 pixels of room on each side.
        let (width, height) = (140, 98);
        let layout = edit_layout(width, height, 96, 3);
        assert_eq!(layout.widget, (16, 16, 108, 66));
        assert_eq!(layout.lifted, (12, 12, 116, 74), "4 px larger on each side");
        let handles: Vec<(Hit, f32, f32, f32)> = layout
            .handles
            .iter()
            .map(|(hit, c)| (*hit, c.cx, c.cy, c.radius))
            .collect();
        assert_eq!(
            handles,
            vec![
                (Hit::Left, 12.0, 49.0, 6.0),
                (Hit::Right, 128.0, 49.0, 6.0),
                (Hit::Top, 70.0, 12.0, 6.0),
                (Hit::Bottom, 70.0, 86.0, 6.0),
            ]
        );
        // Only the edges the widget may be resized along have handles.
        let sideways = edit_layout(width, height, 96, widgets::RESIZE_HORIZONTAL);
        assert_eq!(
            sideways.handles.iter().map(|h| h.0).collect::<Vec<_>>(),
            vec![Hit::Left, Hit::Right]
        );
        assert!(edit_layout(width, height, 96, 0).handles.is_empty());
        // At 200% everything is twice as large.
        let large = edit_layout(2 * width, 2 * height, 192, 3);
        assert_eq!(large.widget, (32, 32, 216, 132));
        assert_eq!(large.lifted, (24, 24, 232, 148));

        // What a press hits: the handles and edges resize, inside moves, beside it is outside.
        let hit = |x, y, mode| edit_hit_test(x, y, width, height, 96, mode);
        assert_eq!(hit(70, 49, 3), Hit::Client);
        assert_eq!(hit(12, 49, 3), Hit::Left);
        assert_eq!(hit(4, 49, 3), Hit::Left, "a handle is grabbed near it");
        assert_eq!(hit(128, 49, 3), Hit::Right);
        assert_eq!(hit(70, 12, 3), Hit::Top);
        assert_eq!(hit(70, 86, 3), Hit::Bottom);
        assert_eq!(hit(13, 13, 3), Hit::TopLeft);
        assert_eq!(hit(126, 84, 3), Hit::BottomRight);
        assert_eq!(hit(1, 1, 3), Hit::Outside);
        assert_eq!(hit(30, 95, 3), Hit::Outside);
        // A widget that cannot be resized is only moved.
        assert_eq!(hit(12, 49, 0), Hit::Client);
        assert_eq!(hit(70, 12, 1), Hit::Client);
        assert_eq!(hit(4, 49, 0), Hit::Outside);
    }

    #[test]
    fn fr12_a_widget_being_edited_is_drawn_lifted_over_a_shadow_with_its_border_and_handles() {
        use crate::compositor::fluent::{premultiplied, Theme};
        let (width, height) = (140usize, 98usize);
        let (lw, lh) = (116usize, 74usize);
        let gray = [0x20u8, 0x20, 0x20, 0xff];
        let lifted = gray.repeat(lw * lh);
        for dark in [false, true] {
            let theme = Theme {
                dark,
                ..Theme::light()
            };
            let look = edit_look(&theme);
            let pixels = draw_edit(&lifted, width, height, 96, 3, &look);
            assert_eq!(pixels.len(), width * height * 4);
            let at = |x: usize, y: usize| {
                let i = (y * width + x) * 4;
                [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
            };
            // The widget's image, in the middle.
            assert_eq!(at(70, 49), gray);
            // Its border, in the accent color, along its edge.
            assert_eq!(at(40, 12), premultiplied(look.accent, 1.0));
            // A handle, in the theme's surface color, on the middle of each edge.
            assert_eq!(at(70, 12), premultiplied(look.handle, 1.0));
            assert_eq!(at(12, 49), premultiplied(look.handle, 1.0));
            // A soft shadow below it, darker nearer the widget, and clear corners.
            let near = at(40, 89);
            let far = at(40, 96);
            assert!(near[3] > far[3], "{near:?} {far:?}");
            assert!(near[3] > 0 && near[..3] == [0, 0, 0]);
            assert_eq!(at(0, 0), [0, 0, 0, 0]);
            // Premultiplied: no color exceeds its alpha.
            assert!(pixels
                .chunks_exact(4)
                .all(|p| p[..3].iter().all(|&c| c <= p[3])));
        }
        assert_eq!(edit_look(&Theme::light()).handle, 0xFFFF_FFFF);
        assert_ne!(
            edit_look(&Theme::light()).accent,
            edit_look(&Theme {
                dark: true,
                ..Theme::light()
            })
            .accent
        );
        // A see-through widget stays clickable all over while it is edited.
        let clear = vec![0u8; lw * lh * 4];
        let look = edit_look(&Theme::light());
        let pixels = draw_edit(&clear, width, height, 96, 0, &look);
        let alpha = |x: usize, y: usize| pixels[(y * width + x) * 4 + 3];
        assert!(alpha(70, 49) >= 1);
        assert!(alpha(20, 20) >= 1);
    }

    #[test]
    fn fr12_a_widget_window_is_resized_within_what_the_widget_allows() {
        assert_eq!(min_window_size(&clock(), 96), (89, 40));
        assert_eq!(max_window_size(&clock(), 96), (3328, 3328));
        let capped = WidgetProvider {
            max_width_dp: 200,
            max_height_dp: 120,
            ..clock()
        };
        assert_eq!(max_window_size(&capped, 96), (163, 98));
        let fixed = WidgetProvider {
            resize_mode: 0,
            ..clock()
        };
        assert_eq!(min_window_size(&fixed, 96), (146, 89));
        assert_eq!(max_window_size(&fixed, 96), (146, 89));
    }

    /// The 5 by 2 music widget that showed every one of these problems: resized only
    /// sideways, at least 369 dp wide.
    fn music() -> WidgetProvider {
        WidgetProvider {
            provider: "com.example.music/.Widget5x2".into(),
            package_name: "com.example.music".into(),
            label: "Music 5x2".into(),
            min_width_dp: 369,
            min_height_dp: 170,
            default_width_dp: 369,
            default_height_dp: 170,
            resize_mode: widgets::RESIZE_HORIZONTAL,
            ..clock()
        }
    }

    /// Hovering a widget draws nothing over it: no button sits in a corner over the widget's
    /// own buttons, and the window's picture is the widget's image, untouched but for the
    /// clear pixels that stay clickable. A right click opens its menu at the pointer, and
    /// the menu key or Shift+F10 under the widget.
    #[test]
    fn fr12_hovering_a_widget_draws_nothing_over_it_and_a_right_click_opens_its_menu() {
        use crate::compositor::fluent_menu::Anchor;
        let (width, height) = (200usize, 120usize);
        // A widget with a button of its own in its top right corner, and a clear hole.
        let mut image = vec![0x40u8; width * height * 4];
        for y in 4..28 {
            for x in 168..196 {
                let i = (y * width + x) * 4;
                image[i..i + 4].copy_from_slice(&[0xF0, 0x20, 0x20, 0xFF]);
            }
        }
        let hole = (60 * width + 100) * 4;
        image[hole..hole + 4].copy_from_slice(&[0, 0, 0, 0]);
        let mut shown = image.clone();
        keep_clickable(&mut shown);
        for (i, (got, was)) in shown.chunks_exact(4).zip(image.chunks_exact(4)).enumerate() {
            if was[3] == 0 {
                assert_eq!(got, [0, 0, 0, 1], "a clear pixel stays clickable");
            } else {
                assert_eq!(got, was, "pixel {i} of the widget was drawn over");
            }
        }
        // The top right corner is the widget's, for a press and a hover alike: only an edge
        // the widget is resized along is anything else.
        for mode in [0, widgets::RESIZE_HORIZONTAL, widgets::RESIZE_VERTICAL, 3] {
            assert_eq!(hit_test(false, 180, 20, 200, 120, 96, mode), Hit::Client);
            assert_eq!(
                hit_test(false, 180, 20, 200, 120, 96, mode).cursor(),
                Cursor::Arrow
            );
        }
        // A press there goes to Android, as anywhere inside the widget.
        let mut gesture = Gesture::Idle;
        let press = Press {
            hit: hit_test(false, 180, 20, 200, 120, 96, 3),
            editing: false,
            window: (100, 100, 300, 220),
            min: (89, 40),
            max: (3328, 3328),
        };
        assert_eq!(
            gesture.press(press, (280, 120), Instant::now()),
            Step::Android(PointerAction::Down)
        );

        // A right click: WM_CONTEXTMENU at the pointer's place on the screen.
        let lparam = ((240isize & 0xffff) << 16) | (1200isize & 0xffff);
        assert_eq!(context_menu_at(lparam), MenuAt::Point(1200, 240));
        let negative = ((-8i16 as u16 as isize) << 16) | (-1900i16 as u16 as isize);
        assert_eq!(context_menu_at(negative), MenuAt::Point(-1900, -8));
        let widget = (100, 100, 300, 220);
        assert_eq!(
            menu_anchor(MenuAt::Point(1200, 240), widget),
            (Anchor::Point { x: 1200, y: 240 }, false)
        );
        // From the keyboard the position is -1, -1: under the widget, its first item lit.
        assert_eq!(context_menu_at(-1), MenuAt::Keyboard);
        assert_eq!(context_menu_at(0xFFFF_FFFF), MenuAt::Keyboard);
        assert_eq!(
            menu_anchor(MenuAt::Keyboard, widget),
            (Anchor::Below(widget), true)
        );
        assert_eq!(
            menu_anchor(MenuAt::Widget, widget),
            (Anchor::Below(widget), false)
        );
    }

    /// The system's timer can come a tick before the half second has passed by the clock the
    /// press is timed with. Such a hold was dropped, so a long press often did nothing; now it
    /// waits out the rest and lifts.
    #[test]
    fn fr12_a_long_press_lifts_even_when_its_timer_comes_early() {
        let start = Instant::now();
        let origin = (100, 100);
        let mut gesture = Gesture::Idle;
        gesture.down(false, (150, 150), start);
        let early = start + HOLD_TO_MOVE - Duration::from_millis(12);
        let step = gesture.held((150, 150), origin, early);
        assert_eq!(step, Step::HoldOn(Duration::from_millis(12)));
        assert!(
            matches!(gesture, Gesture::Widget { slid: false, .. }),
            "still a long press: {gesture:?}"
        );
        // The timer is set again for the rest, never for less than it takes.
        assert_eq!(hold_timer_ms(Duration::from_millis(12)), 12);
        assert_eq!(hold_timer_ms(Duration::from_micros(11_200)), 12);
        assert_eq!(hold_timer_ms(Duration::from_micros(1)), 1);
        assert_eq!(hold_timer_ms(Duration::ZERO), 1);
        assert_eq!(
            gesture.held((150, 150), origin, start + HOLD_TO_MOVE),
            Step::Lift
        );
        // Checked again much later (a timer held up by Android's images), it lifts all the
        // same: nothing but a slide or the end of the press stops it.
        let mut gesture = Gesture::Idle;
        gesture.down(false, (150, 150), start);
        assert_eq!(
            gesture.held((150, 150), origin, start + HOLD_TO_MOVE * 3),
            Step::Lift
        );
        // A press that ended has nothing to lift, and a slide says why it does not.
        let mut gesture = Gesture::Idle;
        assert_eq!(gesture.held((150, 150), origin, start), Step::Nothing);
        assert!(!HoldRefused::Slid.reason().is_empty());
    }

    /// At 200% a hand resting on the mouse moves it a pixel or two. A threshold the system
    /// reports in unscaled pixels is still the 4 device-independent pixels it means.
    #[test]
    fn fr12_a_1_px_wobble_at_200_percent_is_still_a_long_press() {
        assert_eq!(press_threshold((4, 4), false, 192), (8, 8));
        assert_eq!(press_threshold((8, 8), false, 192), (8, 8));
        assert_eq!(press_threshold((4, 4), false, 96), (4, 4));
        let threshold = press_threshold((4, 4), false, 192);
        let start = Instant::now();
        let origin = (1000, 500);
        let mut gesture = Gesture::Idle;
        gesture.down(false, (1300, 640), start);
        for at in [(1301, 640), (1301, 641), (1300, 639), (1306, 646)] {
            assert_eq!(
                gesture.moved(at, origin, threshold),
                Step::Android(PointerAction::Move)
            );
        }
        assert!(matches!(gesture, Gesture::Widget { slid: false, .. }));
        assert_eq!(
            gesture.held((1306, 646), origin, start + HOLD_TO_MOVE),
            Step::Lift
        );
        // Past the threshold it is the widget's own drag.
        let mut gesture = Gesture::Idle;
        gesture.down(false, (1300, 640), start);
        gesture.moved((1309, 640), origin, threshold);
        assert_eq!(
            gesture.held((1309, 640), origin, start + HOLD_TO_MOVE),
            Step::HoldRefused(HoldRefused::Slid)
        );
    }

    /// An edge of a widget and a handle of one being edited resize it the same way: along
    /// the directions the widget allows only, within its smallest and largest size, the
    /// sides not dragged staying where they are. A direction it is not resized along shows
    /// no resize cursor.
    #[test]
    fn fr12_an_edge_and_a_handle_resize_a_widget_the_same_way_within_its_limits() {
        let dpi = 192;
        let provider = music();
        let (min, max) = (
            min_window_size(&provider, dpi),
            max_window_size(&provider, dpi),
        );
        let (width, height) = window_size(
            &Placement {
                x: 0,
                y: 0,
                width_dp: 369,
                height_dp: 170,
                resized: false,
            },
            dpi,
        );
        assert_eq!(min, (width, height), "369 by 170 dp is its smallest");
        assert_eq!(
            max.1, height,
            "never taller or shorter: resized sideways only"
        );
        let mode = provider.resize_mode;
        // Not edited: the left and right edges resize it; the top, the bottom and the
        // corners do not, and show no resize cursor.
        let right = hit_test(false, width - 2, height / 2, width, height, dpi, mode);
        assert_eq!(right, Hit::Right);
        assert_eq!(right.cursor(), Cursor::SizeWestEast);
        assert_eq!(
            hit_test(false, 2, height / 2, width, height, dpi, mode),
            Hit::Left
        );
        for (x, y) in [(width / 2, 1), (width / 2, height - 2)] {
            let hit = hit_test(false, x, y, width, height, dpi, mode);
            assert_eq!(hit, Hit::Client, "at {x},{y}");
            assert_eq!(hit.cursor(), Cursor::Arrow);
        }
        assert_eq!(
            hit_test(false, width - 2, 1, width, height, dpi, mode),
            Hit::Right,
            "a corner of a widget resized sideways only resizes it sideways"
        );

        let window = (500, 300, 500 + width, 300 + height);
        let resize =
            |hit: Hit, window: ScreenRect, min, max, down: (i32, i32), to: &[(i32, i32)]| {
                let mut gesture = Gesture::Idle;
                let press = Press {
                    hit,
                    editing: false,
                    window,
                    min,
                    max,
                };
                assert_eq!(
                    gesture.press(press, down, Instant::now()),
                    Step::StartResize(hit)
                );
                let origin = (window.0, window.1);
                let mut last = window;
                for &at in to {
                    match gesture.moved(at, origin, (8, 8)) {
                        Step::ResizeTo(rect) => last = rect,
                        other => panic!("a resize moved to {other:?}"),
                    }
                }
                assert_eq!(gesture.up(), Step::EndResize);
                last
            };
        // Wider by the right edge: the left side stays, the height too.
        let down = (window.2 - 2, window.1 + 100);
        let wider = resize(
            Hit::Right,
            window,
            min,
            max,
            down,
            &[(down.0 + 40, down.1 + 30), (down.0 + 120, down.1 - 50)],
        );
        assert_eq!(wider, (window.0, window.1, window.2 + 120, window.3));
        // Narrower than it may be: it stops at its smallest.
        let narrower = resize(
            Hit::Right,
            window,
            min,
            max,
            down,
            &[(down.0 - 300, down.1)],
        );
        assert_eq!(narrower, window);
        // By the left edge: the right side stays where it was.
        let down = (window.0 + 2, window.1 + 100);
        let left = resize(Hit::Left, window, min, max, down, &[(down.0 - 60, down.1)]);
        assert_eq!(left, (window.0 - 60, window.1, window.2, window.3));
        // A corner of a widget that may grow both ways is held to both limits.
        let limited = resize(
            Hit::BottomRight,
            window,
            (100, 100),
            (width + 10, height + 20),
            (window.2, window.3),
            &[(window.2 + 500, window.3 + 500)],
        );
        assert_eq!(limited, (window.0, window.1, window.2 + 10, window.3 + 20));

        // Edited: its handles and lifted edges are hit, and resize it the same way with its
        // room around it counted in.
        let margin = edit_margin(dpi);
        let (ew, eh) = (width + 2 * margin, height + 2 * margin);
        let layout = edit_layout(ew, eh, dpi, mode);
        assert_eq!(
            layout.handles.iter().map(|h| h.0).collect::<Vec<_>>(),
            vec![Hit::Left, Hit::Right],
            "no handle on the top or bottom of a widget resized sideways only"
        );
        let handle = layout.handles[1].1;
        let at = (handle.cx as i32, handle.cy as i32);
        let hit = hit_test(true, at.0, at.1, ew, eh, dpi, mode);
        assert_eq!(hit, Hit::Right);
        assert_eq!(hit.cursor(), Cursor::SizeWestEast);
        assert_eq!(
            hit_test(true, ew / 2, layout.lifted.1, ew, eh, dpi, mode),
            Hit::Client,
            "the top of a widget resized sideways only is no resize edge"
        );
        let edited = (
            500 - margin,
            300 - margin,
            500 - margin + ew,
            300 - margin + eh,
        );
        let room = 2 * margin;
        let down = (edited.0 + at.0, edited.1 + at.1);
        let grown = resize(
            hit,
            edited,
            (min.0 + room, min.1 + room),
            (max.0 + room, max.1 + room),
            down,
            &[(down.0 + 120, down.1)],
        );
        assert_eq!(grown, (edited.0, edited.1, edited.2 + 120, edited.3));
        // A press that closed the menu of the edited widget on that handle goes on as the
        // press, so the handle drags at once; one beside it ends the edit.
        assert_eq!(
            menu_closed_by(hit, true, false, true),
            MenuClosed::GoOnPressing
        );
        assert_eq!(
            menu_closed_by(Hit::Client, true, false, true),
            MenuClosed::KeepEditing,
            "a press on the widget only closes the menu"
        );
        assert_eq!(
            menu_closed_by(Hit::Outside, true, false, true),
            MenuClosed::EndEdit { give_back: true }
        );
        assert_eq!(
            menu_closed_by(Hit::Right, false, false, true),
            MenuClosed::KeepEditing,
            "closed with Esc"
        );
        assert_eq!(
            menu_closed_by(Hit::Right, true, false, false),
            MenuClosed::EndEdit { give_back: false },
            "another window took the press"
        );
        // A resize or a move in progress is not undone by a placement from the service.
        assert!(Gesture::Resizing {
            edge: Hit::Right,
            down,
            start: edited,
            min,
            max
        }
        .dragging());
        assert!(Gesture::Moving { grab: (1, 1) }.dragging());
        assert!(!Gesture::Idle.dragging());
    }

    /// A long-pressed widget lifts as a launcher shows a widget it picked up: its image rises
    /// a little over a soft shadow, inside its accent border and handles, and the image keeps
    /// its own pixels, never scaled to the lifted size.
    #[test]
    fn fr12_a_lifted_widget_rises_over_a_shadow_and_never_scales_its_frame() {
        use crate::compositor::fluent::Theme;
        let (width, height) = (140usize, 98usize);
        let layout = edit_layout(width as i32, height as i32, 96, 3);
        let (ix, iy, iw, ih) = layout.image;
        let (wx, wy, ww, wh) = layout.widget;
        assert_eq!((iw, ih), (ww, wh), "the image keeps the widget's own size");
        assert_eq!(ix, wx);
        assert_eq!(
            iy,
            wy - px_of_dip(RAISE_DIP, 96),
            "it rises by {RAISE_DIP} dp"
        );
        // At 200% it rises twice as far.
        let large = edit_layout(2 * width as i32, 2 * height as i32, 192, 3);
        assert_eq!(large.widget.1 - large.image.1, px_of_dip(RAISE_DIP, 192));

        // A frame of distinct, opaque pixels at the widget's size.
        let (fw, fh) = (iw as usize, ih as usize);
        let pixel = |x: usize, y: usize| [x as u8, y as u8, 0xC0, 0xFF];
        let frame: Vec<u8> = (0..fh)
            .flat_map(|y| (0..fw).flat_map(move |x| pixel(x, y)))
            .collect();
        let look = edit_look(&Theme::light());
        let drawn = draw_lifted(&frame, (fw, fh), width, height, 96, 3, &look);
        let at = |x: usize, y: usize| {
            let i = (y * width + x) * 4;
            [drawn[i], drawn[i + 1], drawn[i + 2], drawn[i + 3]]
        };
        // Every pixel of the frame, but the few its border and handles cover, is where it
        // was raised to, one to one.
        let (ix, iy) = (ix as usize, iy as usize);
        for y in 6..fh - 6 {
            for x in 6..fw - 6 {
                assert_eq!(at(ix + x, iy + y), pixel(x, y), "frame pixel {x},{y}");
            }
        }
        // Beside the image, inside the lifted rectangle, the frame does not reach: it was not
        // stretched to the lifted size.
        let beside = at(ix + fw + 1, iy + 10);
        assert_ne!(beside[2], 0xC0, "the frame was stretched: {beside:?}");
        // A soft shadow under it, darker nearer.
        let (lx, ly, lw, lh) = layout.lifted;
        let below = (ly + lh) as usize;
        // A quarter along, away from the handles in the middle of each edge.
        let mid = (lx + lw / 4) as usize;
        let near = at(mid, below + 3);
        let far = at(mid, height - 2);
        assert!(near[3] > 0 && near[..3] == [0, 0, 0], "no shadow: {near:?}");
        assert!(near[3] > far[3], "{near:?} {far:?}");
        // Its border in the accent color.
        assert_eq!(
            at(mid, ly as usize),
            crate::compositor::fluent::premultiplied(look.accent, 1.0)
        );
    }

    /// While a widget is resized its size is passed on when the drag pauses for
    /// [`RESIZE_PAUSE`] and when it ends, never more often than every [`RESIZE_REPORT`], and
    /// a size passed on already is not passed on again: each size Android is told makes it
    /// draw the widget's views anew.
    #[test]
    fn fr12_a_resize_passes_its_size_on_only_when_it_pauses_and_at_its_end() {
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut reports = ResizeReports::<u32>::new();
        assert_eq!(reports.poll(t0, false), ResizePoll::Idle);
        // A steady two-second drag, a step every 16 ms: nothing goes out while it moves.
        let mut sent = Vec::new();
        for step in 0..125u64 {
            let now = ms(step * 16);
            reports.moved(100 + step as u32, now);
            if let ResizePoll::Send(size) = reports.poll(now + Duration::from_millis(8), false) {
                sent.push(size);
            }
        }
        assert!(sent.is_empty(), "sent {sent:?} while the drag moved");
        // It pauses: the size it reached goes out once.
        let last = ms(124 * 16);
        assert_eq!(
            reports.poll(last + RESIZE_PAUSE - Duration::from_millis(1), false),
            ResizePoll::Wait
        );
        assert_eq!(
            reports.poll(last + RESIZE_PAUSE, false),
            ResizePoll::Send(224)
        );
        assert_eq!(
            reports.poll(last + RESIZE_PAUSE * 4, false),
            ResizePoll::Idle
        );
        let sent_at = last + RESIZE_PAUSE;
        // It moves on and pauses again soon after: the next size waits until 250 ms have
        // passed since the last one went out.
        reports.moved(230, sent_at + Duration::from_millis(10));
        let paused = sent_at + Duration::from_millis(10) + RESIZE_PAUSE;
        assert!(paused < sent_at + RESIZE_REPORT);
        assert_eq!(reports.poll(paused, false), ResizePoll::Wait);
        assert_eq!(
            reports.poll(sent_at + RESIZE_REPORT, false),
            ResizePoll::Send(230)
        );
        let sent_at = sent_at + RESIZE_REPORT;
        // The user lets go at once: the end does not wait for a pause, but it waits for the
        // 250 ms, then goes out, and nothing after it.
        reports.moved(240, sent_at + Duration::from_millis(5));
        assert_eq!(
            reports.poll(sent_at + Duration::from_millis(6), true),
            ResizePoll::Wait
        );
        assert_eq!(
            reports.poll(sent_at + RESIZE_REPORT, true),
            ResizePoll::Send(240)
        );
        assert_eq!(
            reports.poll(sent_at + RESIZE_REPORT * 2, true),
            ResizePoll::Idle
        );
        // A drag that comes back to the size passed on last passes nothing on.
        reports.restart();
        reports.moved(250, sent_at + RESIZE_REPORT * 3);
        reports.moved(240, sent_at + RESIZE_REPORT * 3 + Duration::from_millis(16));
        assert_eq!(
            reports.poll(sent_at + RESIZE_REPORT * 5, true),
            ResizePoll::Idle
        );
        // A press on an edge that never moves passes nothing on.
        let mut still = ResizeReports::<u32>::new();
        assert_eq!(still.poll(t0, true), ResizePoll::Idle);
    }

    /// A widget's image is never scaled to a window it was not drawn for: Android draws it at
    /// the size it is told, so an image of another size is one drawn before a resize, and it
    /// shows at its own size from the top left corner until the next one comes.
    #[test]
    fn fr12_a_frame_is_never_scaled_to_a_window_size_it_was_not_drawn_for() {
        let pixel = |x: usize, y: usize| [x as u8, y as u8, 0x10, 0xFF];
        let (fw, fh) = (6usize, 4usize);
        let frame: Vec<u8> = (0..fh)
            .flat_map(|y| (0..fw).flat_map(move |x| pixel(x, y)))
            .collect();
        // Its own size: as it is.
        assert_eq!(unscaled(&frame, (fw, fh), (fw, fh)), frame);
        // A wider and taller window: every pixel of the image where it was, the rest clear.
        let (tw, th) = (9usize, 5usize);
        let wide = unscaled(&frame, (fw, fh), (tw, th));
        let at = |pixels: &[u8], w: usize, x: usize, y: usize| {
            let i = (y * w + x) * 4;
            [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
        };
        for y in 0..th {
            for x in 0..tw {
                let expected = if x < fw && y < fh {
                    pixel(x, y)
                } else {
                    [0, 0, 0, 0]
                };
                assert_eq!(at(&wide, tw, x, y), expected, "at {x},{y}");
            }
        }
        // A narrower window: the image is cut at its edge, not squeezed.
        let narrow = unscaled(&frame, (fw, fh), (4, 4));
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(at(&narrow, 4, x, y), pixel(x, y));
            }
        }
        // Inside an edited widget's lifted rectangle, offset by the lift and still unscaled.
        let lifted = unscaled_at(&frame, (fw, fh), (10, 8), (2, 2));
        assert_eq!(at(&lifted, 10, 2, 2), pixel(0, 0));
        assert_eq!(at(&lifted, 10, 7, 5), pixel(5, 3));
        assert_eq!(at(&lifted, 10, 1, 1), [0, 0, 0, 0]);
        // And a press is the same pixel of the image it shows.
        assert_eq!(to_frame(5, 3, (fw as u32, fh as u32)), (5.0, 3.0));
    }

    /// An edge resize reaches Android as the widget's new size: the window reports its new
    /// pixels and dp at the monitor's density, and the service hands that size to Android,
    /// which draws the widget at it. At 200% for the 5 by 2 music widget.
    #[test]
    fn fr12_an_edge_resize_reaches_android_as_the_widgets_new_size() {
        use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
        use dsa_host_core::widgets::WidgetSize;
        let dpi = 192;
        let provider = music();
        let (width, height) = window_size(
            &Placement {
                x: 0,
                y: 0,
                width_dp: 369,
                height_dp: 170,
                resized: false,
            },
            dpi,
        );
        let window = (500, 300, 500 + width, 300 + height);
        let mut gesture = Gesture::Idle;
        let press = Press {
            hit: Hit::Right,
            editing: false,
            window,
            min: min_window_size(&provider, dpi),
            max: max_window_size(&provider, dpi),
        };
        let down = (window.2 - 2, window.1 + 50);
        gesture.press(press, down, Instant::now());
        let Step::ResizeTo(resized) = gesture.moved((down.0 + 160, down.1), (500, 300), (8, 8))
        else {
            panic!("the edge did not resize the window");
        };
        let report = placed_report(7, resized, dpi);
        let new_width = (resized.2 - resized.0) as u32;
        assert_eq!(new_width, width as u32 + 160);
        match &report {
            WindowReport::Placed {
                placement,
                width,
                height: h,
                density_dpi,
                ..
            } => {
                assert_eq!((*width, *h), (new_width, height as u32));
                assert_eq!(*density_dpi, android_density(dpi));
                assert_eq!((placement.x, placement.y), (500, 300));
                assert_eq!(placement.width_dp, dp_of_px(new_width as i32, dpi));
            }
            other => panic!("{other:?}"),
        }

        // Through the window manager's watch and the service, to Android.
        let wait = Duration::from_secs(10);
        let (listener, connector) = dsa_transport::local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let _service = RunningService::start(Box::new(listener), androids.clone());
        let mut client =
            HostApiClient::connect(&mut connector.clone(), "test", wait).expect("connecting");
        client.start().expect("starting Android");
        let android = androids
            .androids
            .lock()
            .unwrap()
            .values()
            .next()
            .cloned()
            .expect("the caller's Android");
        android.widget_offer.lock().unwrap().push(provider.clone());
        let placed = client
            .add_widget(&provider.provider, wait)
            .expect("placing the widget");
        let report = placed_report(placed.widget_id, resized, dpi);
        let windows = Scripted::default();
        windows.pending.lock().unwrap().push(report);
        let stop = Arc::new(AtomicBool::new(false));
        let syncing = {
            let mut windows = windows.clone();
            let stop = Arc::clone(&stop);
            thread::spawn(move || sync(Box::new(connector), &mut windows, stop, None))
        };
        let expected = WidgetSize::of_pixels(new_width, height as u32, android_density(dpi));
        let deadline = Instant::now() + wait;
        loop {
            let sizes = android.widget_sizes.lock().unwrap().clone();
            if sizes.contains(&(placed.widget_id, expected)) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "Android was never told the new size {expected:?}; it was told {sizes:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        stop.store(true, Ordering::Relaxed);
        syncing.join().unwrap();
        assert_eq!(expected.width_dp, dp_of_px(new_width as i32, dpi));
        assert_eq!(expected.height_dp, 170);
    }

    /// Pictures of the widget menu (its first level and its Resize submenu) and of a widget
    /// being edited with its menu under it, in the light and the dark theme at 100% and 150%,
    /// kept with the Test run when `DSA_WIDGET_MENU_PICTURES` names a folder.
    #[cfg(windows)]
    #[test]
    fn fr12_pictures_of_the_widget_menu() {
        use crate::compositor::fluent::text::{self, TEXT_FACES};
        use crate::compositor::fluent::{menu_palette, premultiplied, Theme};
        use crate::compositor::fluent_menu::{self, metrics, Look, MenuEntry};
        use crate::compositor::widget_menu;

        let folder = std::env::var_os("DSA_WIDGET_MENU_PICTURES").map(std::path::PathBuf::from);
        let desktop = [0xA5, 0x6E, 0x3A];
        let sizes = widget_menu::size_choices(&clock());
        let placement = Placement {
            x: 0,
            y: 0,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        };
        for dark in [false, true] {
            // Transparency off: the solid fill DWM's acrylic falls back to, as a picture can
            // show it.
            let theme = Theme {
                dark,
                transparency: false,
                ..Theme::light()
            };
            let palette = menu_palette(&theme);
            for dpi in [96, 144] {
                let words = widget_menu::menu_text(false);
                // The menu of a widget that can be set up again: every item it may have.
                let entries = widget_menu::entries(&words, &sizes, &placement, true);
                let submenu = entries
                    .iter()
                    .filter_map(MenuEntry::item)
                    .find(|item| item.id == widget_menu::RESIZE)
                    .map(|item| item.submenu.clone())
                    .unwrap_or_default();
                let draw = |entries: &[MenuEntry], focus: Option<usize>| {
                    let em = px_of_dip(metrics::TEXT_SIZE, dpi);
                    let widths: Vec<i32> = entries
                        .iter()
                        .map(|entry| match entry.item() {
                            Some(item) => text::measure(&item.label, &TEXT_FACES, em),
                            None => 0,
                        })
                        .collect();
                    let laid = fluent_menu::layout(entries, &widths, dpi);
                    let art = fluent_menu::row_art(entries, &laid);
                    let look = Look {
                        layout: &laid,
                        art: &art,
                        entries,
                        focus,
                        pressed: None,
                        cues: true,
                        palette: &palette,
                    };
                    let pixels = fluent_menu::render(&look);
                    (laid, pixels)
                };
                let (root, root_pixels) = draw(&entries, Some(0));
                let (sub, sub_pixels) = draw(&submenu, Some(2));
                // The labels are drawn: the first row's label box is not all background.
                let label = root.rows[0].text;
                let background = premultiplied(palette.background, 1.0);
                let inked = (label.y..label.y + label.h)
                    .flat_map(|y| (label.x..label.x + label.w).map(move |x| (x, y)))
                    .filter(|&(x, y)| {
                        let index = ((y * root.width + x) * 4) as usize;
                        root_pixels[index..index + 4] != background
                    })
                    .count();
                assert!(
                    inked > 20,
                    "the label {:?} has {inked} inked pixels",
                    words.settings
                );
                let mut picture = Picture::new(
                    (root.width + sub.width) as usize + 48,
                    root.height.max(sub.height) as usize + 32,
                    desktop,
                );
                let (root_w, root_h) = (root.width as usize, root.height as usize);
                picture.place(16, 16, root_w, root_h, &root_pixels);
                let (sub_w, sub_h) = (sub.width as usize, sub.height as usize);
                picture.place(root_w + 32, 16, sub_w, sub_h, &sub_pixels);
                let percent = dpi * 100 / 96;
                let name = if dark { "dark" } else { "light" };
                picture.save(folder.as_deref(), &format!("menu-{name}-{percent}.bmp"));

                let (width, height) = window_size(&placement, dpi);
                // The widget being edited after a long press: lifted over its shadow, with
                // its border and handles, and its menu under it.
                let margin = edit_margin(dpi);
                let (ew, eh) = (width + 2 * margin, height + 2 * margin);
                let lifted = edit_layout(ew, eh, dpi, clock().resize_mode).lifted;
                let (iw, ih) = (width as usize, height as usize);
                let image = placeholder(iw, ih, dpi);
                let (ew, eh) = (ew as usize, eh as usize);
                let edited = draw_lifted(&image, (iw, ih), ew, eh, dpi, 3, &edit_look(&theme));
                let mut picture = Picture::new(ew.max(root_w) + 32, eh + root_h + 40, desktop);
                picture.place(16, 16, ew, eh, &edited);
                let menu_x = (16 + lifted.0 as usize + lifted.2 as usize).saturating_sub(root_w);
                let menu_y = 16 + (lifted.1 + lifted.3) as usize + px_of_dip(4, dpi) as usize;
                picture.place(menu_x, menu_y, root_w, root_h, &root_pixels);
                picture.save(folder.as_deref(), &format!("edit-{name}-{percent}.bmp"));
            }
        }
    }

    /// An opaque picture of `width` by `height` pixels on a desktop color, saved as a BMP.
    #[cfg(windows)]
    struct Picture {
        width: usize,
        height: usize,
        /// Opaque BGRA rows, top first.
        pixels: Vec<u8>,
    }

    #[cfg(windows)]
    impl Picture {
        fn new(width: usize, height: usize, color: [u8; 3]) -> Picture {
            let pixels = [color[0], color[1], color[2], 0xff].repeat(width * height);
            Picture {
                width,
                height,
                pixels,
            }
        }

        /// Lays premultiplied BGRA `source` of `w` by `h` over the picture at `x`, `y`.
        fn place(&mut self, x: usize, y: usize, w: usize, h: usize, source: &[u8]) {
            for row in 0..h.min(self.height.saturating_sub(y)) {
                for column in 0..w.min(self.width.saturating_sub(x)) {
                    let from = (row * w + column) * 4;
                    let to = ((y + row) * self.width + x + column) * 4;
                    let keep = 255 - u32::from(source[from + 3]);
                    for channel in 0..3 {
                        let under = u32::from(self.pixels[to + channel]) * keep / 255;
                        let over = u32::from(source[from + channel]);
                        self.pixels[to + channel] = (over + under).min(255) as u8;
                    }
                }
            }
        }

        /// Writes the picture as a 32-bit BMP named `name` into `folder`, when there is one.
        fn save(&self, folder: Option<&std::path::Path>, name: &str) {
            let Some(folder) = folder else {
                return;
            };
            std::fs::create_dir_all(folder).unwrap();
            let mut out = Vec::with_capacity(54 + self.pixels.len());
            let size = (54 + self.pixels.len()) as u32;
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(self.width as i32).to_le_bytes());
            // Negative: the rows come top first.
            out.extend_from_slice(&(-(self.height as i32)).to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&[0; 24]);
            out.extend_from_slice(&self.pixels);
            std::fs::write(folder.join(name), out).unwrap();
        }
    }

    #[test]
    fn fr12_a_widget_window_off_every_monitor_comes_back_into_view() {
        let areas = [(0, 0, 1920, 1040)];
        assert_eq!(reachable_origin(100, 100, 200, 100, &areas), (100, 100));
        assert_eq!(reachable_origin(-150, 100, 200, 100, &areas), (-150, 100));
        assert_eq!(reachable_origin(3000, 100, 200, 100, &areas), (1720, 100));
        assert_eq!(reachable_origin(100, -500, 200, 100, &areas), (100, 0));
        assert_eq!(reachable_origin(5, 5, 10, 10, &[]), (5, 5));
    }

    #[test]
    fn fr12_a_widget_window_sits_right_above_the_desktop_not_under_the_wallpaper() {
        // The bottom of the z-order is under the shell's wallpaper window once it has one.
        assert_eq!(z_place(Some(10), Some(20), 30), ZPlace::After(20));
        assert_eq!(z_place(Some(10), Some(30), 30), ZPlace::Keep);
        assert_eq!(z_place(Some(10), None, 30), ZPlace::Keep);
        assert_eq!(z_place(None, None, 30), ZPlace::Bottom);
    }

    #[test]
    fn fr12_a_widget_window_without_an_image_is_logged_once_after_a_while() {
        let start = Instant::now();
        assert!(!frame_overdue(Some(start), false, start));
        assert!(!frame_overdue(
            Some(start),
            false,
            start + NO_FRAME - Duration::from_millis(1)
        ));
        assert!(frame_overdue(Some(start), false, start + NO_FRAME));
        assert!(!frame_overdue(Some(start), true, start + NO_FRAME * 3));
        assert!(!frame_overdue(None, false, start + NO_FRAME * 3));
    }

    #[test]
    fn fr12_a_point_in_the_window_is_the_same_pixel_of_the_widgets_unscaled_frame() {
        // The frame is shown unscaled from the top left corner, so a press is where it
        // shows, whatever size the window has grown or shrunk to meanwhile.
        assert_eq!(to_frame(50, 25, (200, 100)), (50.0, 25.0));
        assert_eq!(to_frame(500, -3, (200, 100)), (200.0, 0.0));
        assert_eq!(to_frame(7, 8, (200, 100)), (7.0, 8.0));
    }

    fn pointer(action: PointerAction, x: f32) -> WindowReport {
        WindowReport::Pointer {
            widget_id: 1,
            action,
            x,
            y: 0.0,
        }
    }

    #[test]
    fn fr12_moves_waiting_to_be_sent_give_way_to_the_newest() {
        let reports = vec![
            pointer(PointerAction::Down, 1.0),
            pointer(PointerAction::Move, 2.0),
            pointer(PointerAction::Move, 3.0),
            pointer(PointerAction::Move, 4.0),
            pointer(PointerAction::Up, 4.0),
        ];
        assert_eq!(
            coalesce(reports),
            vec![
                pointer(PointerAction::Down, 1.0),
                pointer(PointerAction::Move, 4.0),
                pointer(PointerAction::Up, 4.0),
            ]
        );
    }

    #[test]
    fn fr12_each_change_to_the_widgets_is_one_command_for_the_windows() {
        let widget = Widget {
            widget_id: 3,
            provider: clock(),
            placement: Placement::default(),
        };
        assert_eq!(
            command_of(WidgetEvent::Placed(widget.clone())),
            Some(WindowCommand::Place(widget))
        );
        assert_eq!(
            command_of(WidgetEvent::Removed { widget_id: 3 }),
            Some(WindowCommand::Remove(3))
        );
        assert_eq!(command_of(WidgetEvent::Listed(vec![3])), None);
    }

    #[test]
    fn fr12_a_widget_window_the_shell_put_under_the_desktop_is_put_back_above_it() {
        let (desktop, own, widget) = (10, 1, 2);
        let fits = |list: &[(isize, bool)], ours: &[isize]| {
            let below: Vec<Below> = list
                .iter()
                .map(|&(hwnd, covers)| Below { hwnd, covers })
                .collect();
            right_above_desktop(&below, desktop, ours)
        };
        // Right above the desktop, with a hidden window and another widget between.
        assert!(fits(
            &[(30, false), (widget, true), (desktop, true)],
            &[own, widget]
        ));
        // The shell raised its desktop window over it: the desktop is not below it.
        assert!(!fits(&[(40, false), (41, false)], &[own]));
        // An app's window is under it, which it would hide.
        assert!(!fits(&[(50, true), (desktop, true)], &[own]));
    }

    /// A placed widget window shows its placeholder from the moment it opens, through the real
    /// windows: it is given its picture before it is shown, the system takes the picture, the
    /// picture is mostly opaque, and the window is visible, on a monitor and right above the
    /// desktop.
    #[cfg(windows)]
    #[test]
    fn fr12_a_widget_window_shows_a_placeholder_then_a_see_through_image_it_keeps() {
        use windows_sys::Win32::Foundation::{HWND, RECT};
        use windows_sys::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE, GW_HWNDNEXT,
            WS_EX_LAYERED,
        };

        let mut windows = WindowsWidgetWindows::open(None).expect("opening the widget windows");
        let placement = Placement {
            x: 40,
            y: 40,
            width_dp: 180,
            height_dp: 110,
            resized: false,
        };
        windows.command(WindowCommand::Place(Widget {
            widget_id: 4242,
            provider: clock(),
            placement,
        }));
        let deadline = Instant::now() + Duration::from_secs(10);
        // Only this test opens widget windows, so the first drawing is its window's.
        let (first, hwnd) = loop {
            let found = win::PRESENTED.lock().unwrap().first().copied();
            if let Some(found) = found {
                break (found, found.hwnd as HWND);
            }
            assert!(
                Instant::now() < deadline,
                "the widget's window was never drawn"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // Shown right after its first drawing.
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            first.committed,
            "the system did not take the window's picture"
        );
        assert!(
            !first.visible,
            "the window was shown before it had a picture"
        );
        let pixels = (first.width * first.height) as usize;
        assert!(pixels > 0, "the window is {}x{}", first.width, first.height);
        assert!(
            first.opaque * 10 >= pixels * 9,
            "only {} of {pixels} pixels of the placeholder are opaque",
            first.opaque
        );
        // SAFETY: reads a window of the windows' thread.
        unsafe {
            assert!(IsWindowVisible(hwnd) != 0, "the window is hidden");
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert!(ex_style & WS_EX_LAYERED != 0);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetWindowRect(hwnd, &mut rect);
            assert_eq!(
                (rect.right - rect.left, rect.bottom - rect.top),
                (first.width, first.height)
            );
            assert!(
                !MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).is_null(),
                "the window at {},{} is on no monitor",
                rect.left,
                rect.top
            );
            // With a shell, the desktop is below the window, so the desktop is not drawn over it.
            if let Some(desktop) = win::desktop_window() {
                let mut next = GetWindow(hwnd, GW_HWNDNEXT);
                let mut steps = 0;
                while !next.is_null() && next != desktop && steps < 4096 {
                    next = GetWindow(next, GW_HWNDNEXT);
                    steps += 1;
                }
                assert_eq!(next, desktop, "the desktop is drawn over the window");
            }
        }

        // A widget's own image at the window's size, as Android draws it: its left half
        // drawn, its right half see-through, as a clock's text over the wallpaper. The system
        // is handed exactly that, alpha and all.
        let (fw, fh) = (first.width as u32, first.height as u32);
        let frame = |png: Vec<u8>, width: u32, height: u32| {
            WindowCommand::Frame(WidgetFrame {
                widget_id: 4242,
                width,
                height,
                png,
            })
        };
        let row: Vec<[u8; 4]> = (0..fw)
            .map(|x| {
                if x < fw / 2 {
                    [255u8, 255, 255, 255]
                } else {
                    [0u8, 0, 0, 0]
                }
            })
            .collect();
        let drawn: Vec<[u8; 4]> = (0..fh).flat_map(|_| row.iter().copied()).collect();
        windows.command(frame(rgba_png(fw, fh, &drawn), fw, fh));
        let image = next_presented(1, deadline);
        assert!(
            image.committed,
            "the system did not take the widget's image"
        );
        assert!(
            image.opaque * 10 >= pixels * 4 && image.opaque * 10 <= pixels * 6,
            "{} of {pixels} pixels are opaque; half the widget's image is see-through",
            image.opaque
        );
        assert!(
            image.clear * 10 >= pixels * 4,
            "only {} of {pixels} pixels are see-through; the widget's transparent half is drawn opaque",
            image.clear
        );

        // Its app stops drawing and the image comes back empty: the last one stays, not the
        // placeholder.
        windows.command(frame(rgba_png(8, 2, &[[0u8; 4]; 16]), 8, 2));
        let kept = next_presented(2, deadline);
        assert!(
            kept.opaque == image.opaque && kept.clear == image.clear,
            "after an empty image the window shows {} opaque and {} clear pixels, not its last image ({} and {})",
            kept.opaque,
            kept.clear,
            image.opaque,
            image.clear
        );

        // The pointer over the widget, its top right corner included, and leaving it again:
        // nothing is drawn over the widget, so the window is not drawn again at all.
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            PostMessageW, WM_CAPTURECHANGED, WM_KEYDOWN, WM_LBUTTONDOWN, WM_MOUSEMOVE,
        };
        let at = |x: i32, y: i32| ((y as u16 as isize) << 16) | (x as u16 as isize);
        let drawings = || win::PRESENTED.lock().unwrap().len();
        let before = drawings();
        // SAFETY: posts to the widget's window, which is alive until it is removed below.
        unsafe {
            for (x, y) in [(first.width / 2, first.height / 2), (first.width - 12, 12)] {
                PostMessageW(hwnd, WM_MOUSEMOVE, 0, at(x, y));
            }
            PostMessageW(hwnd, 0x02A3, 0, 0);
        }
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            drawings(),
            before,
            "hovering the widget drew over it: the window was drawn again"
        );

        // The left button held still on the widget for the half second: it lifts, its window
        // growing by the room for its border and handles, however the timer comes.
        // SAFETY: as above.
        let dpi = unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForWindow(hwnd) };
        let margin = edit_margin(if dpi == 0 { 96 } else { dpi });
        let lifted = (first.width + 2 * margin, first.height + 2 * margin);
        // SAFETY: as above.
        unsafe {
            PostMessageW(
                hwnd,
                WM_LBUTTONDOWN,
                1,
                at(first.width / 2, first.height / 2),
            )
        };
        let lift_deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let grown = win::PRESENTED
                .lock()
                .unwrap()
                .iter()
                .skip(before)
                .any(|p| (p.width, p.height) == lifted);
            if grown {
                break;
            }
            assert!(
                Instant::now() < lift_deadline,
                "holding the button for {} ms did not lift the widget into a window of {}x{}",
                HOLD_TO_MOVE.as_millis(),
                lifted.0,
                lifted.1
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        // Esc ends the edit; the press ends without opening the menu.
        // SAFETY: as above.
        unsafe {
            PostMessageW(hwnd, WM_KEYDOWN, 0x1B, 0);
            PostMessageW(hwnd, WM_CAPTURECHANGED, 0, 0);
        }
        std::thread::sleep(Duration::from_millis(200));
        windows.command(WindowCommand::Remove(4242));
    }

    /// The window's next drawing after the first `seen` ones.
    #[cfg(windows)]
    fn next_presented(seen: usize, deadline: Instant) -> win::Presented {
        let deadline = deadline.max(Instant::now() + Duration::from_secs(10));
        loop {
            if let Some(found) = win::PRESENTED.lock().unwrap().get(seen).copied() {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "the widget's window was not drawn again"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// An RGBA PNG of `width` by `height` straight-alpha `pixels`, stored without compression.
    #[cfg(windows)]
    fn rgba_png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &b in bytes {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            let sum = crc(&out[start..]);
            out.extend_from_slice(&sum.to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in pixels.chunks(width as usize) {
            raw.push(0);
            for p in row {
                raw.extend_from_slice(p);
            }
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        let len = raw.len() as u16;
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(&raw);
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}
