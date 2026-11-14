//! The window of an open dialog, drawn with [`super::render`].
//!
//! The window keeps a standard frame that `WM_NCCALCSIZE` takes away, so DWM still gives it
//! its shadow, its rounded corners and its Mica backdrop, and its whole client area is DWM's
//! frame, so the see-through pixels show the backdrop. It is centred on the work area of the
//! monitor under the pointer, above the other windows, and is dragged by anywhere that is not
//! a control. [`show`] runs the thread's messages until the dialog is answered, as a modal
//! dialog does. Closing it (Alt+F4, the taskbar) cancels it, as does its time running out
//! when it was given one. It follows a move to a monitor
//! of another scale and a change of the theme while it is open.

use std::cell::RefCell;
use std::ptr::{null, null_mut};
use std::sync::{Arc, Mutex};

use windows::Win32::UI::Accessibility::IRawElementProviderSimple;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{DwmExtendFrameIntoClientArea, DwmSetWindowAttribute};
use windows_sys::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
    VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetWindowRect, IsWindow, KillTimer, LoadCursorW, PostMessageW, PostQuitMessage,
    RegisterClassExW, SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TranslateMessage,
    HTCAPTION, HTCLIENT, HWND_TOPMOST, IDC_ARROW, MSG, SWP_NOZORDER, SW_SHOW, WM_APP, WM_CHAR,
    WM_CLOSE, WM_DPICHANGED, WM_ERASEBKGND, WM_GETOBJECT, WM_KEYDOWN, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEMOVE, WM_MOVE, WM_NCACTIVATE, WM_NCCALCSIZE, WM_NCHITTEST, WM_PAINT, WM_SETTINGCHANGE,
    WM_SYSCHAR, WM_SYSCOLORCHANGE, WM_SYSKEYDOWN, WM_THEMECHANGED, WM_TIMER, WNDCLASSEXW,
    WS_EX_APPWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_THICKFRAME,
};

use super::automation::{self, Action, View};
use super::{
    centred, frame_attributes, key_of, layout, metrics, palette, render, Answer, Control,
    DialogArt, DialogLayout, DialogPalette, DialogState, DialogText, Font, Key, Outcome,
};
use crate::compositor::fluent::text::{
    self, Align, ICON_FACES, REGULAR, SEMIBOLD, TEXT_FACES, TITLE_FACES,
};
use crate::compositor::fluent::{glyph, read_theme, Theme};
use crate::compositor::fluent_menu::{blit, work_area, Mask, Rect};
use crate::compositor::widgets::px_of_dip;

/// `WM_MOUSELEAVE`, which windows-sys names under another module.
const WM_MOUSELEAVE: u32 = 0x02A3;

/// The message a UI Automation client's request is posted to the dialog's window as.
const WM_DIALOG_ACTION: u32 = WM_APP + 0x41;

/// The timer that closes a dialog whose time ran out.
const TIMEOUT_TIMER: usize = 0x6474;

/// A dialog to show.
pub struct Request<'a> {
    pub text: &'a DialogText,
    /// How long the dialog waits for the user; past it, it closes as cancelled. `None` waits
    /// for as long as it takes.
    pub timeout: Option<std::time::Duration>,
}

/// The dialog while it is open.
struct Session {
    text: DialogText,
    state: DialogState,
    palette: DialogPalette,
    layout: DialogLayout,
    art: DialogArt,
    done: Option<Answer>,
    /// What the dialog's UI Automation providers read, and the provider its window answered
    /// `WM_GETOBJECT` with.
    view: Arc<Mutex<View>>,
    provider: Option<IRawElementProviderSimple>,
    /// The focused control and the check box's state UI Automation was last told of.
    announced: (Control, bool),
}

thread_local! {
    static SESSION: RefCell<Option<Session>> = const { RefCell::new(None) };
}

/// Runs `f` on the open dialog; None when none is open or it is in use further up the stack.
fn with_session<R>(f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSION.with(|session| match session.try_borrow_mut() {
        Ok(mut session) => session.as_mut().map(f),
        Err(_) => None,
    })
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Shows the dialog and returns how it was answered. Called on a thread with a message loop,
/// whose messages it runs while the dialog is open; no state of the caller may be borrowed
/// meanwhile. A dialog asked for while one is open on the thread is cancelled at once.
pub fn show(request: Request<'_>) -> Answer {
    let busy = SESSION.with(|session| match session.try_borrow() {
        Ok(session) => session.is_some(),
        Err(_) => true,
    });
    if busy {
        return Answer::Cancelled;
    }
    let class = register_class();
    let mut pointer = POINT { x: 0, y: 0 };
    // SAFETY: a plain read of the pointer's position.
    unsafe { GetCursorPos(&mut pointer) };
    let hwnd = create_window(&class, &request.text.title, (pointer.x, pointer.y));
    if hwnd.is_null() {
        return Answer::Cancelled;
    }
    let theme = read_theme();
    // SAFETY: a window of this thread.
    let dpi = match unsafe { GetDpiForWindow(hwnd) } {
        0 => 96,
        dpi => dpi,
    };
    let (laid, art) = lay_out(request.text, dpi);
    let (width, height) = (laid.width, laid.height);
    let state = DialogState::new(request.text.check.is_some());
    // A client's request is done on this thread, as a key would be.
    let window = hwnd as isize;
    let sink: automation::ActionSink = Arc::new(move |action: Action| {
        // SAFETY: posting to a window is safe from any thread; one that closed drops it.
        unsafe { PostMessageW(window as HWND, WM_DIALOG_ACTION, action.code(), 0) };
    });
    let mut view = View::open(window, sink);
    view.name = request.text.title.clone();
    SESSION.with(|session| {
        *session.borrow_mut() = Some(Session {
            text: request.text.clone(),
            state,
            palette: palette(&theme),
            layout: laid,
            art,
            done: None,
            view: Arc::new(Mutex::new(view)),
            provider: None,
            announced: (state.focus, state.checked),
        })
    });
    let area = work_area(hwnd).unwrap_or((0, 0, 1024, 768));
    let (x, y) = centred((width, height), area);
    // SAFETY: the dialog's own window, on this thread; it takes the foreground, as the user
    // just asked for it (the menu's Stop).
    unsafe {
        style(hwnd, &theme);
        SetWindowPos(hwnd, HWND_TOPMOST, x, y, width, height, 0);
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(hwnd);
        if let Some(timeout) = request.timeout {
            let ms = timeout.as_millis().clamp(1, u32::MAX as u128) as u32;
            SetTimer(hwnd, TIMEOUT_TIMER, ms, None);
        }
    }
    sync_view(hwnd);
    if let Some((view, focus)) = with_session(|s| (Arc::clone(&s.view), s.state.focus)) {
        automation::raise_opened(&view);
        automation::raise_focus(&view, focus);
    }
    run(hwnd);
    // UI Automation lets go of the dialog before its window goes.
    if let Some((view, provider)) = with_session(|s| (Arc::clone(&s.view), s.provider.take())) {
        view.lock().unwrap_or_else(|e| e.into_inner()).alive = false;
        automation::release(hwnd as isize, provider);
    }
    // SAFETY: as above.
    unsafe {
        if IsWindow(hwnd) != 0 {
            DestroyWindow(hwnd);
        }
    }
    let answer = with_session(|s| s.done).flatten();
    SESSION.with(|session| *session.borrow_mut() = None);
    answer.unwrap_or(Answer::Cancelled)
}

/// Runs this thread's messages until the dialog is answered or its window is gone.
fn run(hwnd: HWND) {
    loop {
        let done = with_session(|s| s.done.is_some()).unwrap_or(true);
        // SAFETY: a plain query of a window that may have gone.
        if done || unsafe { IsWindow(hwnd) } == 0 {
            return;
        }
        // SAFETY: the usual message loop; WM_QUIT is put back for the caller's loop.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            match GetMessageW(&mut msg, null_mut(), 0, 0) {
                0 => {
                    PostQuitMessage(msg.wParam as i32);
                    return;
                }
                -1 => return,
                _ => {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// `text` laid out at `dpi`, and its text rasterized for that layout.
fn lay_out(text: &DialogText, dpi: u32) -> (DialogLayout, DialogArt) {
    let px = |dip: u32| px_of_dip(dip, dpi);
    let (title_em, body_em) = (px(metrics::TITLE_SIZE), px(metrics::BODY_SIZE));
    let measure = |line: &str, font: Font| match font {
        Font::Title => text::measure_weighted(line, &TITLE_FACES, title_em, SEMIBOLD),
        Font::Body => text::measure(line, &TEXT_FACES, body_em),
    };
    let laid = layout(text, dpi, &measure);
    let draw = |line: &str, faces: &[&str], em: i32, weight: i32, at: Rect, align: Align| {
        let (width, height) = (at.w.max(0) as usize, at.h.max(0) as usize);
        text::mask_weighted(line, faces, em, weight, (width, height), align).map(|cover| Mask {
            width,
            height,
            cover,
        })
    };
    let body = |line: &str, at: Rect| draw(line, &TEXT_FACES, body_em, REGULAR, at, Align::Left);
    let button =
        |label: &str, at: Rect| draw(label, &TEXT_FACES, body_em, REGULAR, at, Align::Center);
    let art = DialogArt {
        title: draw(
            &text.title,
            &TITLE_FACES,
            title_em,
            SEMIBOLD,
            laid.title,
            Align::Left,
        ),
        lines: laid
            .lines
            .iter()
            .map(|(line, at)| body(line, *at))
            .collect(),
        check_label: match (&text.check, laid.check_label) {
            (Some(label), Some(at)) => body(label, at),
            _ => None,
        },
        check_mark: laid.check_box.and_then(|at| {
            draw(
                &glyph::CHECK.to_string(),
                &ICON_FACES,
                px(metrics::CHECK_GLYPH),
                REGULAR,
                at,
                Align::Center,
            )
        }),
        primary: button(&text.primary, laid.primary),
        secondary: button(&text.secondary, laid.secondary),
    };
    (laid, art)
}

/// Registers the dialog's window class once, and returns its name.
fn register_class() -> Vec<u16> {
    static REGISTERED: std::sync::Once = std::sync::Once::new();
    let class = wide(&format!("DsaFluentDialog_{}", std::process::id()));
    REGISTERED.call_once(|| {
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(dialog_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            // SAFETY: the module handle of this process.
            hInstance: unsafe { GetModuleHandleW(null()) },
            hIcon: null_mut(),
            // SAFETY: a system cursor.
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class.as_ptr(),
            hIconSm: null_mut(),
        };
        // SAFETY: the system copies the name; the class lives as long as the process.
        unsafe { RegisterClassExW(&wc) };
    });
    class
}

/// The window styles of the dialog: a popup with a sizing frame, which `WM_NCCALCSIZE` takes
/// away but which keeps DWM's shadow, above the other windows, with a taskbar button so it
/// can be found again behind other windows.
fn styles() -> (u32, u32) {
    (WS_POPUP | WS_THICKFRAME, WS_EX_TOPMOST | WS_EX_APPWINDOW)
}

/// Makes the (hidden) dialog window at `at` on the screen, so it takes the DPI of that point's
/// monitor. Null when it cannot be made.
fn create_window(class: &[u16], name: &str, at: (i32, i32)) -> HWND {
    let title = wide(name);
    let (style, ex_style) = styles();
    // SAFETY: a popup of this thread of a class registered by `register_class`; the names
    // outlive the call.
    unsafe {
        CreateWindowExW(
            ex_style,
            class.as_ptr(),
            title.as_ptr(),
            style,
            at.0,
            at.1,
            1,
            1,
            null_mut(),
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        )
    }
}

/// Gives the window its look ([`frame_attributes`]), with its whole client area DWM's frame.
/// Returns what DWM answered for each attribute.
unsafe fn style(hwnd: HWND, theme: &Theme) -> Vec<(u32, i32)> {
    let margins = MARGINS {
        cxLeftWidth: -1,
        cxRightWidth: -1,
        cyTopHeight: -1,
        cyBottomHeight: -1,
    };
    DwmExtendFrameIntoClientArea(hwnd, &margins);
    frame_attributes(theme)
        .into_iter()
        .map(|(attribute, value)| {
            let answer = DwmSetWindowAttribute(
                hwnd,
                attribute,
                &value as *const i32 as *const std::ffi::c_void,
                std::mem::size_of::<i32>() as u32,
            );
            (attribute, answer)
        })
        .collect()
}

/// Writes the dialog as it is now into its UI Automation view, and tells UI Automation when
/// the focus moved or the check box changed.
fn sync_view(hwnd: HWND) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: the dialog's own window, and a structure to fill.
    unsafe { GetWindowRect(hwnd, &mut rect) };
    let changed = with_session(|s| {
        let elements = automation::elements(&s.text, &s.layout, &s.state, (rect.left, rect.top));
        s.view.lock().unwrap_or_else(|e| e.into_inner()).elements = elements;
        let now = (s.state.focus, s.state.checked);
        let was = std::mem::replace(&mut s.announced, now);
        (Arc::clone(&s.view), was, now)
    });
    let Some((view, was, now)) = changed else {
        return;
    };
    if was.1 != now.1 {
        automation::raise_toggled(&view, now.1);
    }
    if was.0 != now.0 {
        automation::raise_focus(&view, now.0);
    }
}

/// Acts on an outcome: draws the dialog again, or ends it.
fn apply(hwnd: HWND, outcome: Outcome) {
    match outcome {
        Outcome::Nothing => {}
        Outcome::Changed => {
            sync_view(hwnd);
            // SAFETY: the dialog's own window.
            unsafe { InvalidateRect(hwnd, null(), 0) };
        }
        Outcome::Done(answer) => {
            with_session(|s| s.done = Some(answer));
        }
    }
}

/// The point of a mouse message in the window's pixels: its client area is the whole window.
fn client_point(lparam: LPARAM) -> (i32, i32) {
    (
        (lparam & 0xffff) as u16 as i16 as i32,
        ((lparam >> 16) & 0xffff) as u16 as i16 as i32,
    )
}

/// The control under a point of the window.
fn hit(at: (i32, i32)) -> Option<Control> {
    with_session(|s| s.layout.hit(at.0, at.1)).flatten()
}

/// Lays the dialog out again at `dpi`, after a move to a monitor of another scale.
fn rescale(hwnd: HWND, dpi: u32, suggested: &RECT) {
    let Some(text) = with_session(|s| s.text.clone()) else {
        return;
    };
    let (laid, art) = lay_out(&text, dpi);
    let (width, height) = (laid.width, laid.height);
    with_session(|s| {
        s.layout = laid;
        s.art = art;
    });
    // SAFETY: the dialog's own window.
    unsafe {
        SetWindowPos(
            hwnd,
            null_mut(),
            suggested.left,
            suggested.top,
            width,
            height,
            SWP_NOZORDER,
        );
        InvalidateRect(hwnd, null(), 0);
    }
}

/// Reads the theme again, after the user changed it while the dialog is open.
fn restyle(hwnd: HWND) {
    let theme = read_theme();
    with_session(|s| s.palette = palette(&theme));
    // SAFETY: the dialog's own window.
    unsafe {
        style(hwnd, &theme);
        InvalidateRect(hwnd, null(), 0);
    }
}

unsafe fn paint(hwnd: HWND) {
    let pixels = with_session(|s| {
        let pixels = render(&s.layout, &s.art, &s.state, &s.palette);
        (pixels, s.layout.width, s.layout.height)
    });
    let mut paint: PAINTSTRUCT = std::mem::zeroed();
    let dc = BeginPaint(hwnd, &mut paint);
    if let (false, Some((pixels, width, height))) = (dc.is_null(), pixels) {
        blit(dc, &pixels, width, height);
    }
    EndPaint(hwnd, &paint);
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // The client area is the whole window; DWM keeps the frame's shadow and corners.
        WM_NCCALCSIZE if wparam != 0 => 0,
        // Anywhere but a control drags the dialog, as a dialog's title bar does.
        WM_NCHITTEST => {
            let mut rect: RECT = std::mem::zeroed();
            GetWindowRect(hwnd, &mut rect);
            let (x, y) = client_point(lparam);
            match hit((x - rect.left, y - rect.top)) {
                Some(_) => HTCLIENT as LRESULT,
                None => HTCAPTION as LRESULT,
            }
        }
        // -1: nothing of a frame painted.
        WM_NCACTIVATE => DefWindowProcW(hwnd, msg, wparam, -1),
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            paint(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            TrackMouseEvent(&mut track);
            let over = hit(client_point(lparam));
            let outcome = with_session(|s| s.state.hover(over)).unwrap_or(Outcome::Nothing);
            apply(hwnd, outcome);
            0
        }
        WM_MOUSELEAVE => {
            let outcome = with_session(|s| s.state.hover(None)).unwrap_or(Outcome::Nothing);
            apply(hwnd, outcome);
            0
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            let on = hit(client_point(lparam));
            let outcome = with_session(|s| s.state.press(on)).unwrap_or(Outcome::Nothing);
            apply(hwnd, outcome);
            0
        }
        WM_LBUTTONUP => {
            ReleaseCapture();
            let on = hit(client_point(lparam));
            let outcome = with_session(|s| s.state.release(on)).unwrap_or(Outcome::Nothing);
            apply(hwnd, outcome);
            0
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let shift = GetKeyState(i32::from(VK_SHIFT)) < 0;
            let key = key_of(wparam as u16, shift);
            // With Alt only the access keys are the dialog's; Alt+F4 and the rest are
            // Windows'.
            let ours = match key {
                Some(Key::Char(_)) => true,
                Some(_) => msg == WM_KEYDOWN,
                None => false,
            };
            match key.filter(|_| ours) {
                Some(key) => {
                    let outcome = with_session(|s| {
                        let keys = s.text.keys;
                        s.state.key(key, &keys)
                    })
                    .unwrap_or(Outcome::Nothing);
                    apply(hwnd, outcome);
                    0
                }
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        // The characters the keys make are not wanted: no beep.
        WM_CHAR | WM_SYSCHAR => 0,
        WM_CLOSE => {
            apply(hwnd, Outcome::Done(Answer::Cancelled));
            0
        }
        WM_TIMER if wparam == TIMEOUT_TIMER => {
            KillTimer(hwnd, TIMEOUT_TIMER);
            apply(hwnd, Outcome::Done(Answer::Cancelled));
            0
        }
        WM_DIALOG_ACTION => {
            let outcome = Action::from_code(wparam).and_then(|action| {
                with_session(|s| match action {
                    Action::Activate(control) => s.state.activate(control),
                    Action::Focus(control) => s.state.focus_on(control),
                })
            });
            apply(hwnd, outcome.unwrap_or(Outcome::Nothing));
            0
        }
        WM_GETOBJECT => {
            let provider = with_session(|s| {
                s.provider
                    .get_or_insert_with(|| automation::dialog_provider(&s.view))
                    .clone()
            });
            match provider.and_then(|p| automation::answer(hwnd as isize, wparam, lparam, &p)) {
                Some(result) => result,
                None => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
        // The controls' places on the screen follow the window.
        WM_MOVE => {
            sync_view(hwnd);
            0
        }
        WM_DPICHANGED => {
            rescale(hwnd, (wparam & 0xffff) as u32, &*(lparam as *const RECT));
            0
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            restyle(hwnd);
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::fluent::{BACKDROP_BUILD, ROUNDED_BUILD};
    use crate::compositor::fluent_dialog::{
        DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_WINDOW_CORNER_PREFERENCE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetWindowLongPtrW, GWL_EXSTYLE, GWL_STYLE, WS_CAPTION,
    };

    /// A real dialog window on this Windows: a framed popup (DWM's shadow) with no classic
    /// caption, above the others, and DWM takes every attribute this build has.
    #[test]
    fn fr24_the_stop_dialog_window_is_a_windows_11_dialog() {
        let class = register_class();
        let hwnd = create_window(&class, "Stop Music?", (0, 0));
        assert!(!hwnd.is_null(), "the dialog's window was not made");
        // SAFETY: a window of this thread, destroyed below.
        unsafe {
            let window_style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert_eq!(window_style & WS_POPUP, WS_POPUP);
            assert_eq!(window_style & WS_THICKFRAME, WS_THICKFRAME);
            assert_eq!(window_style & WS_CAPTION, 0, "no classic title bar");
            assert_eq!(ex_style & WS_EX_TOPMOST, WS_EX_TOPMOST);
            let theme = read_theme();
            let answers = style(hwnd, &theme);
            assert_eq!(answers.len(), frame_attributes(&theme).len());
            for (attribute, answer) in &answers {
                let known = match *attribute {
                    DWMWA_WINDOW_CORNER_PREFERENCE => theme.build >= ROUNDED_BUILD,
                    DWMWA_SYSTEMBACKDROP_TYPE => theme.build >= BACKDROP_BUILD,
                    _ => true,
                };
                if known {
                    assert!(
                        *answer >= 0,
                        "DWM refused attribute {attribute} (0x{:08x})",
                        *answer as u32
                    );
                }
            }
            DestroyWindow(hwnd);
        }
        // The text is drawn: each box of the layout gets its mask.
        let text = DialogText {
            title: "Stop Music?".into(),
            body: "It stops everything it does in the background.".into(),
            check: Some("Do not ask again".into()),
            primary: "Stop".into(),
            secondary: "Cancel".into(),
            keys: [Some('D'), Some('S'), Some('C')],
        };
        let (laid, art) = lay_out(&text, 96);
        assert!(art.title.is_some() && art.primary.is_some() && art.secondary.is_some());
        assert!(art.check_label.is_some() && art.check_mark.is_some());
        assert_eq!(art.lines.len(), laid.lines.len());
    }
}
