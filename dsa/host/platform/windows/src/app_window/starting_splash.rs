//! The window an app's window program shows while Android starts: the DSA logo, the one the
//! DSA programs on Windows carry as their icon, centred on the splash background of the user's
//! theme and breathing slowly, until the app's own window opens; then it fades out over it.
//!
//! The timing, the sizes and the pixels of each frame are `dsa_host_core::compositor::boot_splash`,
//! which every host shares. This draws them: one window of the app's name and icon (so the
//! taskbar shows the app), whose client area is a child window named with what the window
//! used to say ("Starting Android..."), so a screen reader still reads it although nothing
//! of it is written on the screen. A frame timer runs only while the logo moves and the
//! window can be seen: it stops when the window is minimized, and never starts when the user
//! turned animations off (the logo then stands still). The fade out goes through DWM's
//! window opacity, so no frame is drawn for it at all. Once the window is gone nothing of it
//! is left: its thread ends and its buffers are freed.

use std::cell::RefCell;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::time::Instant;

use dsa_host_core::compositor::boot_splash::{self, BootSplash};
use dsa_host_core::compositor::splash::Icon;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, SetDIBitsToDevice, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetClientRect, GetMessageW,
    GetWindowLongPtrW, IsIconic, KillTimer, LoadCursorW, LoadIconW, MoveWindow, PostQuitMessage,
    RegisterClassExW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, SystemParametersInfoW, TranslateMessage, CW_USEDEFAULT, GWL_EXSTYLE,
    IDC_APPSTARTING, LWA_ALPHA, MSG, SIZE_MINIMIZED, SPI_GETCLIENTAREAANIMATION, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOZORDER, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_PAINT, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_LAYERED, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU, WS_VISIBLE,
};

/// The class of the window shown while the app is being opened.
const STARTING_CLASS: &str = "DsaStartingApp";

/// The class of its client area, which draws the logo and carries the accessible name.
const SPLASH_CLASS: &str = "DsaStartingSplash";

/// The program's own icon: the app's, in an app's copy of the window host.
const ICON_ID: usize = 1;

/// The timer that draws the logo's next frame.
const FRAME_TIMER: usize = 1;

/// The timer that steps the fade out.
const FADE_TIMER: usize = 2;

/// The DSA logo, as the settings app and the installer carry it for their icon.
const LOGO_PNG: &[u8] = include_bytes!("../../settings/DsaSettings/Assets/logo.png");

/// What the window's thread keeps while it is open.
struct State {
    started: Instant,
    fade: Option<Instant>,
    reduce_motion: bool,
    dark: bool,
    logo: Option<(Vec<u8>, (u32, u32))>,
    splash: Option<BootSplash>,
    pixels: Vec<u8>,
    child: HWND,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Whether the user turned Windows' animations off (Settings > Accessibility > Visual
/// effects > Animation effects).
fn reduce_motion() -> bool {
    let mut on: i32 = 1;
    // SAFETY: a BOOL to fill, as SPI_GETCLIENTAREAANIMATION asks.
    let read = unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut on as *mut i32).cast(),
            0,
        )
    };
    read != 0 && on == 0
}

fn dark() -> bool {
    crate::compositor::system_settings::read().dark_mode
}

/// The logo, decoded to premultiplied BGRA at its own size.
fn decode_logo() -> Option<(Vec<u8>, (u32, u32))> {
    let (width, height) = crate::compositor::widgets::png::size(LOGO_PNG)?;
    let pixels = crate::compositor::widgets::png::with_com(|| {
        let decoder = crate::compositor::widgets::png::Decoder::new().ok()?;
        decoder.decode(LOGO_PNG, width, height).ok()
    })?;
    Some((pixels, (width, height)))
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn client_size(hwnd: HWND) -> (u32, u32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: a window of this thread and a rectangle to fill.
    unsafe { GetClientRect(hwnd, &mut rect) };
    (
        (rect.right - rect.left).max(0) as u32,
        (rect.bottom - rect.top).max(0) as u32,
    )
}

/// Draws the frames again for the window's size, DPI and theme.
fn rebuild(hwnd: HWND, state: &mut State) {
    let view = client_size(hwnd);
    // SAFETY: a window of this thread.
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    let logo = state.logo.as_ref().map(|(pixels, size)| Icon {
        size: *size,
        pixels,
    });
    state.splash = Some(BootSplash::new(view, state.dark, logo, dpi));
    if !state.child.is_null() {
        // SAFETY: the child of this window, on this thread.
        unsafe {
            MoveWindow(state.child, 0, 0, view.0 as i32, view.1 as i32, 1);
        }
    }
}

/// Starts or stops the frame timer as the logo and the window's visibility ask.
fn schedule(hwnd: HWND, state: &State) {
    // SAFETY: a window of this thread.
    let visible = unsafe { IsIconic(hwnd) } == 0;
    let moving = state.fade.is_none();
    match boot_splash::next_frame(state.reduce_motion, visible && moving) {
        // SAFETY: a timer of this thread's window; setting it again only resets it.
        Some(interval) => unsafe {
            SetTimer(hwnd, FRAME_TIMER, interval.as_millis() as u32, None);
        },
        // SAFETY: as above; killing a timer that is not set does nothing.
        None => unsafe {
            KillTimer(hwnd, FRAME_TIMER);
        },
    };
}

/// Runs the window, titled `title`, on the calling thread until it is closed; `hwnd` gets
/// the window once it is open.
pub fn run(title: &str, hwnd: &AtomicIsize) {
    let class = wide(STARTING_CLASS);
    let splash_class = wide(SPLASH_CLASS);
    let title = wide(title);
    let name = wide(super::launch::starting_text());
    // SAFETY: plain Win32 calls on this thread's own windows; the strings outlive them.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let icon = LoadIconW(module, ICON_ID as *const u16);
        let cursor = LoadCursorW(std::ptr::null_mut(), IDC_APPSTARTING);
        let frame_class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: icon,
            hCursor: cursor,
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: icon,
        };
        if RegisterClassExW(&frame_class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return;
        }
        let child_class = WNDCLASSEXW {
            lpfnWndProc: Some(splash_proc),
            lpszClassName: splash_class.as_ptr(),
            hIcon: std::ptr::null_mut(),
            hIconSm: std::ptr::null_mut(),
            ..frame_class
        };
        if RegisterClassExW(&child_class) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return;
        }
        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        // Sized for the system DPI first and corrected for the monitor it opens on below.
        let outer = outer_size(GetDpiForSystem(), style);
        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                started: Instant::now(),
                fade: None,
                reduce_motion: reduce_motion(),
                dark: dark(),
                logo: decode_logo(),
                splash: None,
                pixels: Vec::new(),
                child: std::ptr::null_mut(),
            })
        });
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            style,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            outer.0,
            outer.1,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        if window.is_null() {
            STATE.with(|cell| cell.borrow_mut().take());
            return;
        }
        let dpi = GetDpiForWindow(window).max(96);
        let outer = outer_size(dpi, style);
        SetWindowPos(
            window,
            std::ptr::null_mut(),
            0,
            0,
            outer.0,
            outer.1,
            SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOMOVE,
        );
        // The client area, named with what the window is waiting for: a screen reader reads
        // the name, and nothing of it is drawn.
        let (width, height) = client_size(window);
        let child = CreateWindowExW(
            0,
            splash_class.as_ptr(),
            name.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            0,
            0,
            width as i32,
            height as i32,
            window,
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        STATE.with(|cell| {
            if let Some(state) = cell.borrow_mut().as_mut() {
                state.child = child;
                rebuild(window, state);
                schedule(window, state);
            }
        });
        hwnd.store(window as isize, Ordering::SeqCst);
        ShowWindow(window, SW_SHOW);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    // Nothing of the window outlives it.
    STATE.with(|cell| cell.borrow_mut().take());
}

/// The outer size of a window of `style` whose client area is the splash's size at `dpi`.
fn outer_size(dpi: u32, style: u32) -> (i32, i32) {
    let (width, height) = boot_splash::window_size(dpi);
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: width as i32,
        bottom: height as i32,
    };
    // SAFETY: a rectangle to adjust.
    unsafe { AdjustWindowRectExForDpi(&mut rect, style, 0, 0, dpi.max(96)) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => 1,
        WM_SIZE => {
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    if wparam as u32 != SIZE_MINIMIZED {
                        rebuild(hwnd, state);
                    }
                    schedule(hwnd, state);
                }
            });
            0
        }
        WM_DPICHANGED => {
            // SAFETY: Windows passes the suggested window rectangle for the new DPI.
            let rect = unsafe { &*(lparam as *const RECT) };
            // SAFETY: this thread's window. WM_SIZE then draws the frames again.
            unsafe {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            0
        }
        WM_SETTINGCHANGE => {
            // The theme or the animation setting may have changed.
            STATE.with(|cell| {
                if let Some(state) = cell.borrow_mut().as_mut() {
                    state.reduce_motion = reduce_motion();
                    state.dark = dark();
                    rebuild(hwnd, state);
                    schedule(hwnd, state);
                }
            });
            invalidate(hwnd);
            0
        }
        WM_TIMER if wparam == FRAME_TIMER => {
            invalidate(hwnd);
            STATE.with(|cell| {
                if let Some(state) = cell.borrow().as_ref() {
                    schedule(hwnd, state);
                }
            });
            0
        }
        WM_TIMER if wparam == FADE_TIMER => {
            let left = STATE.with(|cell| {
                let state = cell.borrow();
                let state = state.as_ref()?;
                let since = state.fade?.elapsed();
                (!boot_splash::crossfade_done(since, state.reduce_motion))
                    .then(|| boot_splash::crossfade_opacity(since, state.reduce_motion))
            });
            match left {
                // SAFETY: this thread's window, layered since the fade began.
                Some(opacity) => unsafe {
                    SetLayeredWindowAttributes(hwnd, 0, (opacity * 255.0) as u8, LWA_ALPHA);
                },
                // SAFETY: this thread's window.
                None => unsafe {
                    KillTimer(hwnd, FADE_TIMER);
                    DestroyWindow(hwnd);
                },
            }
            0
        }
        WM_CLOSE => {
            // Fades out over the app's window, then goes. Asked again meanwhile, it only
            // goes on fading. With animations off it goes at once.
            let fade = STATE.with(|cell| match cell.borrow_mut().as_mut() {
                Some(state) if state.reduce_motion => Some(false),
                Some(state) if state.fade.is_some() => None,
                Some(state) => {
                    state.fade = Some(Instant::now());
                    Some(true)
                }
                None => Some(false),
            });
            match fade {
                // SAFETY: this thread's window; it becomes layered at full opacity, and the
                // fade timer steps DWM's opacity of it down.
                Some(true) => unsafe {
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style | WS_EX_LAYERED as isize);
                    SetLayeredWindowAttributes(hwnd, 0, 255, LWA_ALPHA);
                    KillTimer(hwnd, FRAME_TIMER);
                    SetTimer(
                        hwnd,
                        FADE_TIMER,
                        boot_splash::FRAME_INTERVAL.as_millis() as u32,
                        None,
                    );
                },
                // SAFETY: this thread's window.
                Some(false) => unsafe {
                    DestroyWindow(hwnd);
                },
                None => {}
            }
            0
        }
        WM_DESTROY => {
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
            0
        }
        // SAFETY: the default handling of every other message.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn invalidate(hwnd: HWND) {
    STATE.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            let target = if state.child.is_null() {
                hwnd
            } else {
                state.child
            };
            // SAFETY: a window of this thread.
            unsafe { InvalidateRect(target, std::ptr::null(), 0) };
        }
    });
}

unsafe extern "system" fn splash_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            // SAFETY: painting this window, between BeginPaint and EndPaint.
            unsafe {
                let mut paint: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut paint);
                STATE.with(|cell| {
                    let mut state = cell.borrow_mut();
                    let Some(state) = state.as_mut() else {
                        return;
                    };
                    let opacity =
                        boot_splash::logo_opacity(state.started.elapsed(), state.reduce_motion);
                    let Some(splash) = state.splash.as_ref() else {
                        return;
                    };
                    let (width, height) = splash.view();
                    if width == 0 || height == 0 {
                        return;
                    }
                    splash.frame_into(opacity, &mut state.pixels);
                    let mut info: BITMAPINFO = std::mem::zeroed();
                    info.bmiHeader = BITMAPINFOHEADER {
                        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                        biWidth: width as i32,
                        biHeight: -(height as i32),
                        biPlanes: 1,
                        biBitCount: 32,
                        biCompression: BI_RGB,
                        ..std::mem::zeroed()
                    };
                    SetDIBitsToDevice(
                        dc,
                        0,
                        0,
                        width,
                        height,
                        0,
                        0,
                        0,
                        height,
                        state.pixels.as_ptr().cast(),
                        &info,
                        DIB_RGB_COLORS,
                    );
                });
                EndPaint(hwnd, &paint);
            }
            0
        }
        // SAFETY: the default handling of every other message.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
