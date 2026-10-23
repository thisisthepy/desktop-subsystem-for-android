//! Sign-out and session termination listener.
//!
//! Windows broadcasts `WM_QUERYENDSESSION` and `WM_ENDSESSION` to top-level windows when
//! a user signs out or the system shuts down. A hidden top-level window listens for these
//! messages and triggers a clean shutdown.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, PostQuitMessage,
    RegisterClassExW, TranslateMessage, MSG, WM_CLOSE, WM_ENDSESSION, WM_QUERYENDSESSION,
    WNDCLASSEXW, WS_POPUP,
};

static GLOBAL_STOP: AtomicBool = AtomicBool::new(false);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION => {
            if wparam != 0 {
                GLOBAL_STOP.store(true, Ordering::SeqCst);
                PostQuitMessage(0);
            }
            0
        }
        WM_CLOSE => {
            GLOBAL_STOP.store(true, Ordering::SeqCst);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe extern "system" fn ctrl_handler(ctrl_type: u32) -> i32 {
    // 2: CTRL_CLOSE_EVENT, 5: CTRL_LOGOFF_EVENT, 6: CTRL_SHUTDOWN_EVENT
    if ctrl_type == 2 || ctrl_type == 5 || ctrl_type == 6 {
        GLOBAL_STOP.store(true, Ordering::SeqCst);
        1
    } else {
        0
    }
}

/// A listener that triggers `stop` when the user signs out.
pub struct SignOutListener {
    thread: Option<JoinHandle<()>>,
}

impl SignOutListener {
    /// Starts the sign-out listener thread, which sets `stop` when sign-out is detected.
    pub fn start(stop: Arc<AtomicBool>) -> SignOutListener {
        // SAFETY: registering console control handler for sign-out/shutdown events.
        unsafe { SetConsoleCtrlHandler(Some(ctrl_handler), 1) };

        let flag = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("dsa-signout-listener".into())
            .spawn(move || {
                let class_name = wide(&format!("DsaCompositorSignOut_{}", std::process::id()));
                // SAFETY: GetModuleHandleW with null retrieves the current module instance.
                let instance = unsafe { GetModuleHandleW(null()) };

                let wnd_class = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: 0,
                    lpfnWndProc: Some(window_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance,
                    hIcon: null_mut(),
                    hCursor: null_mut(),
                    hbrBackground: null_mut(),
                    lpszMenuName: null(),
                    lpszClassName: class_name.as_ptr(),
                    hIconSm: null_mut(),
                };

                // SAFETY: RegisterClassExW registers window class for hidden top-level window.
                unsafe { RegisterClassExW(&wnd_class) };

                // SAFETY: creating a hidden popup window to receive broadcast session messages.
                let hwnd = unsafe {
                    CreateWindowExW(
                        0,
                        class_name.as_ptr(),
                        class_name.as_ptr(),
                        WS_POPUP,
                        0,
                        0,
                        0,
                        0,
                        null_mut(),
                        null_mut(),
                        instance,
                        null(),
                    )
                };

                let mut msg: MSG = unsafe { std::mem::zeroed() };
                while !flag.load(Ordering::Relaxed) && !GLOBAL_STOP.load(Ordering::Relaxed) {
                    // SAFETY: message loop.
                    let result = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
                    if result <= 0 {
                        break;
                    }
                    unsafe {
                        TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
                flag.store(true, Ordering::SeqCst);

                if !hwnd.is_null() {
                    // SAFETY: destroy window before exit.
                    unsafe { DestroyWindow(hwnd) };
                }
            })
            .ok();

        SignOutListener { thread }
    }
}

impl Drop for SignOutListener {
    fn drop(&mut self) {
        GLOBAL_STOP.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
