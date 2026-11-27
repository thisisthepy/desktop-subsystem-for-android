//! The windows of an app's overlay windows: floating lyrics, a chat head, anything an app adds
//! over other apps in Android with the "display over other apps" permission.
//!
//! Each overlay is a window of its own on the windows' thread, beside the app's task windows,
//! and nothing like them: it has no border, no title bar and no close button (its app decides
//! when it goes), stays above every other window, is in neither the taskbar nor Alt+Tab, and
//! shows the desktop through whatever its app left clear, since its frames keep their own
//! transparency ([`WindowPresenter::new_see_through`]). The rules it follows name no window
//! system and live in [`dsa_host_core::compositor::overlay_windows`]; this file carries them
//! out.
//!
//! - **Where it is.** All of the app's overlays share one [`OverlayPlane`]. The window manager
//!   says where the app has each overlay on Android's display (`ToHost::Overlay`), and the
//!   plane says where that is on the desktop. A window is first shown once it knows.
//! - **Its size** is the overlay's at the scale of its monitor, as the window manager works it
//!   out from the DPI the window reports, and never more than the monitor's work area.
//! - **Input.** Every press, move and wheel turn inside the window is the app's, relative to
//!   the overlay. A click never takes the focus from the window the user works in, unless the
//!   overlay may take the focus in Android; then the click activates it, and keys and text go
//!   to the overlay. An overlay that takes no input lets the pointer through to what is under
//!   it.
//! - **Moving it.** DSA adds nothing to move an overlay by: it moves the way its app lets the
//!   user move it, by a drag inside the overlay. While a press lasts, each axis its app drags
//!   the overlay along with the pointer is followed by the window itself ([`OverlayDrag`]), so
//!   the drag carries the overlay onto any monitor, past where Android keeps it inside its
//!   display, and the app keeps hearing the pointer as the press found it. When the press
//!   ends the app's plane is put where the drag left the window and remembered for the app in
//!   the user's DSA folder (`apps\<package>.overlay.json`); the next time, the overlay's
//!   window opens on that monitor. The window tells the window manager when such a drag
//!   begins and ends (`FromHost::Dragging`), which asks Android for the overlay's density on
//!   the monitor the drag ended on, for an app set to System (forced) scaling.
//!
//! Such a window is an ordinary window of the user's desktop: Windows shows none of them on
//! the lock screen or the secure desktop, and it takes no input outside its own rectangle.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use dsa_host_core::compositor::overlay_windows::{
    click_activates, first_place, kept_on, remember, remembered, shown_size, Area, InputRemap,
    OverlayDrag, OverlayPlane, PUSH_SLOP_DIP,
};
use dsa_host_core::compositor::window_hosts::{FromHost, ToHost};
use dsa_host_core::compositor::window_sizing::{dip_to_px, to_window_size, Scale};
use dsa_host_core::tasks::OVERLAY_ID_BASE;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, ClientToScreen, EndPaint, GetMonitorInfoW, MonitorFromPoint, MonitorFromRect,
    MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::Pointer::{GetPointerInfo, GetPointerType, POINTER_INFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetMessageExtraInfo,
    GetWindowLongPtrW, GetWindowRect, LoadCursorW, RegisterClassExW, SetLayeredWindowAttributes,
    SetWindowLongPtrW, SetWindowPos, SetWindowTextW, ShowWindow, CREATESTRUCTW, GWLP_USERDATA,
    GWL_EXSTYLE, HTCLIENT, HWND_TOPMOST, IDC_ARROW, LWA_ALPHA, MA_ACTIVATE, MA_NOACTIVATE,
    POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SW_SHOWNOACTIVATE, WM_CAPTURECHANGED, WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND,
    WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCCREATE, WM_NCDESTROY,
    WM_NCHITTEST, WM_PAINT, WM_POINTERCAPTURECHANGED, WM_POINTERDOWN, WM_POINTERUP,
    WM_POINTERUPDATE, WM_SIZE, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_NOREDIRECTIONBITMAP, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

use crate::compositor::log::note;
use crate::compositor::present::WindowPresenter;

use super::ime::ImeWindow;
use super::keys::KeyWindow;
use super::pointer::{made_from_touch_or_pen, message_point, PointerWindow};
use super::window::{Report, UiSetup};

const OVERLAY_CLASS: &str = "DsaOverlayWindow";

/// Whether `task_id` names an app overlay window rather than a task.
pub(super) fn is_overlay(task_id: i32) -> bool {
    task_id >= OVERLAY_ID_BASE
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16()
        .filter(|unit| *unit != 0)
        .chain(std::iter::once(0))
        .collect()
}

/// Kept in each overlay window's GWLP_USERDATA.
struct OverlayData {
    task_id: i32,
    report: Report,
    presenter: RefCell<Option<Arc<WindowPresenter>>>,
    pointer: PointerWindow,
    keys: KeyWindow,
    ime: ImeWindow,
    /// The overlay's top left corner on Android's display; `None` until the window manager
    /// said, and the window is shown only then.
    guest: Cell<Option<(i32, i32)>>,
    /// The size of Android's display.
    display: Cell<(u32, u32)>,
    /// The overlay's size in Android's pixels, how large the window manager says the window
    /// shows each of them, and how large the window does show them (smaller when the overlay
    /// is larger than the monitor's work area).
    guest_size: Cell<(u32, u32)>,
    scale: Cell<Scale>,
    shown_scale: Cell<Scale>,
    focusable: Cell<bool>,
    touchable: Cell<bool>,
    /// The press on the overlay that lasts now, if one does.
    press: Cell<Option<Press>>,
}

/// A press on an overlay's window: the mouse's left button (`pointer_id` `None`) or one touch
/// or pen contact, and the drag it may become.
#[derive(Clone, Copy)]
struct Press {
    pointer_id: Option<u32>,
    drag: OverlayDrag,
}

/// The overlays of this window host's app.
#[derive(Default)]
struct App {
    plane: OverlayPlane,
    /// Whether what was remembered for the app has been read.
    read: bool,
    windows: HashMap<i32, isize>,
    package: String,
    places: Option<PathBuf>,
}

thread_local! {
    static APP: RefCell<App> = RefCell::new(App::default());
}

unsafe fn data_of<'a>(hwnd: HWND) -> Option<&'a OverlayData> {
    (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const OverlayData).as_ref()
}

unsafe fn register() -> Result<(), String> {
    let name = wide(OVERLAY_CLASS);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(overlay_proc),
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
            "could not register the overlay window class: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn area(rect: &RECT) -> Area {
    (rect.left, rect.top, rect.right, rect.bottom)
}

unsafe fn work_of(monitor: *mut core::ffi::c_void) -> Option<RECT> {
    let mut info: MONITORINFO = std::mem::zeroed();
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    (!monitor.is_null() && GetMonitorInfoW(monitor, &mut info) != 0).then_some(info.rcWork)
}

/// The work area of the monitor an app's first overlay goes to: the one of the app's window
/// `beside`, or the pointer's when the app has no window.
unsafe fn first_work_area(beside: Option<HWND>) -> Option<RECT> {
    if let Some(window) = beside {
        return work_of(MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST));
    }
    let mut pointer = POINT { x: 0, y: 0 };
    if GetCursorPos(&mut pointer) == 0 {
        return None;
    }
    work_of(MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST))
}

/// Where the window's client area begins on the desktop.
unsafe fn client_origin(hwnd: HWND) -> (i32, i32) {
    let mut point = POINT { x: 0, y: 0 };
    ClientToScreen(hwnd, &mut point);
    (point.x, point.y)
}

/// The styles of an overlay's window: never in the taskbar, always on top, drawn only by its
/// visual; not activated by a click unless the overlay may take the focus; and let through by
/// the pointer when the overlay takes no input.
fn ex_style(focusable: bool, touchable: bool) -> u32 {
    let mut style = WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOPMOST | WS_EX_TOOLWINDOW;
    if !click_activates(focusable) {
        style |= WS_EX_NOACTIVATE;
    }
    if !touchable {
        style |= WS_EX_LAYERED | WS_EX_TRANSPARENT;
    }
    style
}

/// Carries out what the window manager says about an overlay's window. `beside` is one of the
/// app's task windows, if it has any.
///
/// # Safety
///
/// Called on the windows' thread.
pub(super) unsafe fn command(setup: &UiSetup, beside: Option<HWND>, command: ToHost) {
    let window = |task_id: i32| -> Option<HWND> {
        APP.with(|app| app.borrow().windows.get(&task_id).map(|hwnd| *hwnd as HWND))
    };
    match command {
        ToHost::Show {
            task_id,
            title,
            width,
            height,
            ..
        } => open(setup, beside, task_id, &title, (width, height)),
        ToHost::Retitle { task_id, title } => {
            if let Some(hwnd) = window(task_id) {
                SetWindowTextW(hwnd, wide(&title).as_ptr());
            }
        }
        // The window's size is its overlay's at its scale; both come with TaskSize too.
        ToHost::Resize { task_id, .. } => {
            if let Some(hwnd) = window(task_id) {
                place(hwnd);
            }
        }
        ToHost::TaskSize {
            task_id,
            width,
            height,
            scale,
            ..
        } => {
            if let Some((hwnd, data)) = window(task_id).and_then(|h| Some((h, data_of(h)?))) {
                data.guest_size.set((width.max(1), height.max(1)));
                data.scale.set(scale);
                data.pointer.task_resized((width, height));
                place(hwnd);
            }
        }
        ToHost::Overlay {
            task_id,
            left,
            top,
            display,
            focusable,
            touchable,
        } => {
            if let Some((hwnd, data)) = window(task_id).and_then(|h| Some((h, data_of(h)?))) {
                let first = data.guest.replace(Some((left, top))).is_none();
                data.display.set(display);
                let flags = (
                    data.focusable.replace(focusable),
                    data.touchable.replace(touchable),
                );
                if first || flags != (focusable, touchable) {
                    restyle(hwnd, focusable, touchable);
                }
                place(hwnd);
                if first {
                    // Never active when it appears: only the user's click may activate it.
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
        }
        ToHost::Close { task_id } => close(setup, task_id),
        // An overlay's window has one mode, no title bar and no editor of its own to describe.
        _ => {}
    }
}

/// Closes every overlay window, as when the windows' thread ends.
pub(super) fn close_all(setup: &UiSetup) {
    let open: Vec<i32> = APP.with(|app| app.borrow().windows.keys().copied().collect());
    for task_id in open {
        close(setup, task_id);
    }
}

fn close(setup: &UiSetup, task_id: i32) {
    let presenter = setup
        .presenters
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&task_id);
    let hwnd = APP.with(|app| app.borrow_mut().windows.remove(&task_id));
    if let Some(presenter) = &presenter {
        presenter.close();
    }
    if let Some(hwnd) = hwnd {
        // SAFETY: a window of this thread.
        unsafe { DestroyWindow(hwnd as HWND) };
    }
    if let Some(presenter) = presenter {
        presenter.release();
    }
}

unsafe fn open(setup: &UiSetup, beside: Option<HWND>, task_id: i32, title: &str, size: (u32, u32)) {
    if APP.with(|app| app.borrow().windows.contains_key(&task_id)) {
        return;
    }
    if let Err(why) = register() {
        note(setup.logger.as_deref(), &why);
        return;
    }
    let kept = APP.with(|app| {
        let mut app = app.borrow_mut();
        if !app.read {
            // Where the user's drag left this app's overlays the last time.
            app.read = true;
            app.package = setup.package.clone();
            app.places = setup.overlay_places.clone();
            let kept = app
                .places
                .as_deref()
                .and_then(|dir| remembered(dir, &app.package));
            app.plane = OverlayPlane::new(kept);
        }
        app.plane.anchor()
    });
    let size = (size.0.max(1), size.1.max(1));
    let data = Box::into_raw(Box::new(OverlayData {
        task_id,
        report: Arc::clone(&setup.report),
        presenter: RefCell::new(None),
        pointer: PointerWindow::new(task_id, size, setup.input.clone(), None),
        keys: KeyWindow::new(task_id, setup.input.clone()),
        ime: ImeWindow::new(task_id, setup.input.clone(), setup.logger.clone()),
        guest: Cell::new(None),
        display: Cell::new((0, 0)),
        guest_size: Cell::new(size),
        scale: Cell::new((1, 1)),
        shown_scale: Cell::new((1, 1)),
        focusable: Cell::new(false),
        touchable: Cell::new(true),
        press: Cell::new(None),
    }));
    // On the monitor it will first be placed on, so the DPI it reports is that monitor's: where
    // the app's plane was left on the desktop, or else the monitor of the app's window.
    let (x, y) = match kept {
        Some(anchor) => anchor.host,
        None => first_work_area(beside).map_or((0, 0), |work| (work.left, work.top)),
    };
    let hwnd = CreateWindowExW(
        ex_style(false, true),
        wide(OVERLAY_CLASS).as_ptr(),
        wide(title).as_ptr(),
        WS_POPUP,
        x,
        y,
        size.0 as i32,
        size.1 as i32,
        std::ptr::null_mut(),
        std::ptr::null_mut(),
        GetModuleHandleW(std::ptr::null()),
        data as *const std::ffi::c_void,
    );
    if hwnd.is_null() {
        drop(Box::from_raw(data));
        note(
            setup.logger.as_deref(),
            &format!(
                "could not open the window of overlay {task_id}: {}",
                std::io::Error::last_os_error()
            ),
        );
        return;
    }
    super::pointer::deliver_contacts_at_once(hwnd);
    let Some(data) = data_of(hwnd) else {
        return;
    };
    // Its document is made once the guest reports an editor that takes text in it.
    data.ime.attach(hwnd);
    // Its frames are shown in it as they come, at its scale, see-through where the app drew
    // nothing; the window itself takes the size the window manager says.
    data.pointer.set_held(true);
    {
        match WindowPresenter::new_see_through(
            task_id,
            Arc::clone(&setup.surfaces),
            hwnd as isize,
            size,
            setup.logger.clone(),
        ) {
            Ok(presenter) => {
                presenter.set_held(true);
                *data.presenter.borrow_mut() = Some(Arc::clone(&presenter));
                setup
                    .presenters
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(task_id, presenter);
            }
            Err(why) => note(
                setup.logger.as_deref(),
                &format!("overlay {task_id}'s window cannot show frames: {why}"),
            ),
        }
    }
    APP.with(|app| app.borrow_mut().windows.insert(task_id, hwnd as isize));
    // The scale of its monitor, which the window manager answers with the overlay's size there.
    (setup.report)(FromHost::Dpi {
        task_id,
        dpi: GetDpiForWindow(hwnd).max(96),
    });
    note(
        setup.logger.as_deref(),
        &format!(
            "opened the window of app overlay window {task_id} ({}x{})",
            size.0, size.1
        ),
    );
}

/// Gives the window the styles of an overlay that may or may not take the focus and input.
unsafe fn restyle(hwnd: HWND, focusable: bool, touchable: bool) {
    let style = ex_style(focusable, touchable);
    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
    if style & WS_EX_LAYERED != 0 {
        // A layered window shows nothing until it is given its opacity.
        SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
    }
    SetWindowPos(
        hwnd,
        std::ptr::null_mut(),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
    );
}

/// Puts the window where its overlay is on the app's plane, at the overlay's size on its
/// monitor, or, on each axis a drag of the app has carried past the plane, under the pointer.
/// Nothing until the window manager has said where the overlay is.
unsafe fn place(hwnd: HWND) {
    let Some(data) = data_of(hwnd) else {
        return;
    };
    let Some(guest) = data.guest.get() else {
        return;
    };
    let Some(work) = work_of(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)) else {
        return;
    };
    let scale = data.scale.get();
    let guest_size = data.guest_size.get();
    let scaled = to_window_size(guest_size, scale);
    let size = shown_size(scaled, area(&work));
    // A window shown smaller than its overlay's size there shows the frames, and measures the
    // pointer, at the scale it has.
    let shown_scale = if size == scaled {
        scale
    } else {
        (size.0.max(1), guest_size.0.max(1))
    };
    data.shown_scale.set(shown_scale);
    data.pointer.set_scale(shown_scale);
    if let Some(presenter) = data.presenter.borrow().as_deref() {
        presenter.set_scale(shown_scale);
    }
    let display = data.display.get();
    let placed = APP.with(|app| {
        app.borrow_mut().plane.place(guest, scale, || {
            first_place(guest, guest_size, display, size, area(&work))
        })
    });
    let host = match data.press.get() {
        // While a press lasts the window is where the drag has it, on any monitor; it is
        // brought into reach when the press ends.
        Some(mut press) => {
            press.drag.app_moved(guest, placed, shown_scale);
            data.press.set(Some(press));
            press.drag.window_at(placed, shown_scale)
        }
        None => {
            // Kept in reach on the monitor that place is on.
            let host = within_reach(placed, size, work);
            if host != placed {
                APP.with(|app| app.borrow_mut().plane.moved_to(guest, host));
            }
            host
        }
    };
    SetWindowPos(
        hwnd,
        HWND_TOPMOST,
        host.0,
        host.1,
        size.0 as i32,
        size.1 as i32,
        SWP_NOACTIVATE,
    );
    remap_input(hwnd, data);
}

/// `at` moved as little as it takes to keep a window of `size` there on the work area of the
/// monitor it is most on; `work` when there is no monitor to ask.
unsafe fn within_reach(at: (i32, i32), size: (u32, u32), work: RECT) -> (i32, i32) {
    let rect = RECT {
        left: at.0,
        top: at.1,
        right: at.0 + size.0 as i32,
        bottom: at.1 + size.1 as i32,
    };
    let on = work_of(MonitorFromRect(&rect, MONITOR_DEFAULTTONEAREST)).unwrap_or(work);
    kept_on(at, size, area(&on))
}

/// Puts every overlay window of the app where the plane has it now.
unsafe fn place_all() {
    let open: Vec<isize> = APP.with(|app| app.borrow().windows.values().copied().collect());
    for hwnd in open {
        place(hwnd as HWND);
    }
}

/// Tells the window's pointer input how to give the app the pointer while a press lasts, for
/// the window as it is placed now.
unsafe fn remap_input(hwnd: HWND, data: &OverlayData) {
    let remap = match (data.press.get(), data.guest.get()) {
        (Some(press), Some(guest)) => Some(press.drag.input(
            client_origin(hwnd),
            guest,
            data.shown_scale.get(),
        )),
        _ => None,
    };
    data.pointer.set_remap(remap);
}

/// The contact a pointer message is about, and where it is on the desktop: the mouse's left
/// button (`None`) or a touch or pen contact. `None` for any other message, a mouse message
/// Windows made from a touch, or a pointer that is a mouse.
unsafe fn contact_of(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<(Option<u32>, (i32, i32))> {
    match message {
        WM_LBUTTONDOWN | WM_MOUSEMOVE => {
            if made_from_touch_or_pen(GetMessageExtraInfo()) {
                return None;
            }
            let (x, y) = message_point(lparam);
            let origin = client_origin(hwnd);
            Some((None, (origin.0 + x as i32, origin.1 + y as i32)))
        }
        WM_POINTERDOWN | WM_POINTERUPDATE => {
            let pointer_id = (wparam & 0xffff) as u32;
            let mut kind: POINTER_INPUT_TYPE = 0;
            if GetPointerType(pointer_id, &mut kind) == 0 || !matches!(kind, PT_TOUCH | PT_PEN) {
                return None;
            }
            let mut info: POINTER_INFO = std::mem::zeroed();
            if GetPointerInfo(pointer_id, &mut info) == 0 {
                return None;
            }
            let at = info.ptPixelLocation;
            Some((Some(pointer_id), (at.x, at.y)))
        }
        _ => None,
    }
}

/// A press begins on the window, at `at` on the desktop.
unsafe fn begin_press(hwnd: HWND, data: &OverlayData, pointer_id: Option<u32>, at: (i32, i32)) {
    // A second finger is the app's like the first, and moves nothing.
    if data.press.get().is_some() {
        return;
    }
    let Some(guest) = data.guest.get() else {
        return;
    };
    let drag = OverlayDrag::press(
        at,
        client_origin(hwnd),
        data.shown_scale.get(),
        guest,
        data.guest_size.get(),
        data.display.get(),
    );
    data.press.set(Some(Press { pointer_id, drag }));
    data.pointer.set_remap(Some(InputRemap::SAME));
}

/// The pointer of the press moved to `at`. Whether the window now follows it on some axis.
unsafe fn press_moved(
    hwnd: HWND,
    data: &OverlayData,
    pointer_id: Option<u32>,
    at: (i32, i32),
) -> bool {
    let Some(mut press) = data.press.get().filter(|p| p.pointer_id == pointer_id) else {
        return false;
    };
    let slop = dip_to_px(PUSH_SLOP_DIP, GetDpiForWindow(hwnd).max(96)) as i32;
    let carried = press.drag.carried();
    press
        .drag
        .pointer_moved(at, client_origin(hwnd), data.shown_scale.get(), slop);
    data.press.set(Some(press));
    if !carried && press.drag.carried() {
        // The window manager asks for the overlay's density only once the drag ends.
        (data.report)(FromHost::Dragging {
            task_id: data.task_id,
            dragging: true,
        });
    }
    press.drag.carried()
}

/// The press of `pointer_id` ended. When the window followed a drag, the app's plane is put
/// where the drag left it (brought into reach on the monitor it is most on), the app's other
/// overlays follow, and the place is remembered for the app.
unsafe fn end_press(hwnd: HWND, data: &OverlayData, pointer_id: Option<u32>) {
    let Some(press) = data.press.get().filter(|p| p.pointer_id == pointer_id) else {
        return;
    };
    data.press.set(None);
    data.pointer.set_remap(None);
    if !press.drag.carried() {
        // Where its app has it, kept in reach as when no press lasts.
        place(hwnd);
        return;
    }
    settle_drag(hwnd, data);
    // The drag is over, on the monitor it ended on: the window manager may ask Android to draw
    // the overlay at that monitor's density now.
    (data.report)(FromHost::Dragging {
        task_id: data.task_id,
        dragging: false,
    });
}

/// Puts the app's plane where a drag that ended left the window, brought into reach on the
/// monitor it is most on, and remembers it for the app.
unsafe fn settle_drag(hwnd: HWND, data: &OverlayData) {
    let Some(guest) = data.guest.get() else {
        return;
    };
    let mut window: RECT = std::mem::zeroed();
    if GetWindowRect(hwnd, &mut window) == 0 {
        return;
    }
    let size = (
        (window.right - window.left).max(1) as u32,
        (window.bottom - window.top).max(1) as u32,
    );
    let at = client_origin(hwnd);
    let host = within_reach(at, size, window);
    let kept = APP.with(|app| {
        let mut app = app.borrow_mut();
        let anchor = app.plane.moved_to(guest, host);
        app.places
            .as_deref()
            .map(|dir| remember(dir, &app.package, anchor))
    });
    if let Some(Err(why)) = kept {
        note(None, &why);
    }
    // On a monitor of another scale the window has heard its new size from the window manager.
    place_all();
}

unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let Some(data) = data_of(hwnd) else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    match message {
        // Only an overlay that may take the focus is activated, and only by a click.
        WM_MOUSEACTIVATE => {
            return if click_activates(data.focusable.get()) {
                MA_ACTIVATE as LRESULT
            } else {
                MA_NOACTIVATE as LRESULT
            };
        }
        // All of it is the app's: no border to resize by, no caption to drag by.
        WM_NCHITTEST => return HTCLIENT as LRESULT,
        // No close button, and Alt+F4 closes nothing: the app removes its overlay.
        WM_CLOSE => return 0,
        WM_ERASEBKGND => return 1,
        WM_PAINT => {
            let mut paint: PAINTSTRUCT = std::mem::zeroed();
            BeginPaint(hwnd, &mut paint);
            EndPaint(hwnd, &paint);
            return 0;
        }
        WM_SIZE => {
            let view = ((lparam & 0xffff) as u32, ((lparam >> 16) & 0xffff) as u32);
            if view.0 > 0 && view.1 > 0 {
                if let Some(presenter) = data.presenter.borrow().clone() {
                    presenter.view_resized(view);
                }
            }
            return 0;
        }
        WM_DPICHANGED => {
            // The window manager answers with the overlay's size on this monitor; the size
            // Windows suggests is not taken.
            (data.report)(FromHost::Dpi {
                task_id: data.task_id,
                dpi: ((wparam & 0xffff) as u32).max(96),
            });
            return 0;
        }
        WM_NCDESTROY => {
            data.ime.detach();
            super::pointer::forget_contacts(hwnd);
            let data = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut OverlayData;
            if !data.is_null() {
                drop(Box::from_raw(data));
            }
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
        _ => {}
    }
    // A press and its moves, before the app hears them, so it hears them as the press found
    // the window.
    let contact = contact_of(hwnd, message, wparam, lparam);
    let mut follows = false;
    if let Some((pointer_id, at)) = contact {
        match message {
            WM_LBUTTONDOWN | WM_POINTERDOWN => begin_press(hwnd, data, pointer_id, at),
            WM_MOUSEMOVE | WM_POINTERUPDATE => follows = press_moved(hwnd, data, pointer_id, at),
            _ => {}
        }
    }
    let mut result = data.pointer.handle(hwnd, message, wparam, lparam);
    if result.is_none() {
        result = data.keys.handle(hwnd, message, wparam, lparam);
    }
    if result.is_none() {
        result = data.ime.handle(hwnd, message, wparam, lparam);
    }
    // Then the window follows the drag, and a press that ended puts the plane where it left it.
    if follows {
        place(hwnd);
    }
    match message {
        WM_LBUTTONUP => end_press(hwnd, data, None),
        // The mouse was taken by another window, or the button came up elsewhere.
        WM_CAPTURECHANGED if lparam as HWND != hwnd => end_press(hwnd, data, None),
        WM_POINTERUP | WM_POINTERCAPTURECHANGED => {
            end_press(hwnd, data, Some((wparam & 0xffff) as u32))
        }
        _ => {}
    }
    match result {
        Some(result) => result,
        None => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr41_an_overlay_window_is_borderless_on_top_and_out_of_the_taskbar() {
        for (focusable, touchable) in [(false, true), (true, true), (false, false)] {
            let style = ex_style(focusable, touchable);
            assert_ne!(style & WS_EX_TOPMOST, 0);
            assert_ne!(style & WS_EX_TOOLWINDOW, 0);
            assert_ne!(style & WS_EX_NOREDIRECTIONBITMAP, 0);
        }
    }

    #[test]
    fn fr41_a_click_activates_only_an_overlay_that_may_take_the_focus() {
        assert_ne!(ex_style(false, true) & WS_EX_NOACTIVATE, 0);
        assert_eq!(ex_style(true, true) & WS_EX_NOACTIVATE, 0);
    }

    #[test]
    fn fr41_the_pointer_passes_through_an_overlay_that_takes_no_input() {
        let through = WS_EX_LAYERED | WS_EX_TRANSPARENT;
        assert_eq!(ex_style(false, false) & through, through);
        assert_eq!(ex_style(false, true) & through, 0);
    }

    #[test]
    fn fr41_overlay_ids_are_told_from_task_ids() {
        assert!(is_overlay(OVERLAY_ID_BASE));
        assert!(is_overlay(OVERLAY_ID_BASE + 7));
        assert!(!is_overlay(42));
        assert!(!is_overlay(1_000_042));
    }
}
