//! Opening an app from its Start menu shortcut, a pinned taskbar button or Windows search.
//!
//! The app's window program then starts with no window manager behind it, and maybe no
//! Android either. Before it becomes the app's window host it makes sure both run: it asks the
//! service to start the user's Android and waits until it runs, and it starts the window
//! manager when its pipe is not there. While it waits it shows a small window under the app's
//! name and icon, so the taskbar shows the app at once. Then it connects to the window manager
//! as the app's window host, which asks Android for the app; if the app has a window host
//! already, that one gets the app and this program ends, so an app never has two.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, ERROR_SEM_TIMEOUT, HWND, LPARAM, LRESULT, RECT,
    WPARAM,
};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, DrawTextW, EndPaint, GetStockObject, SelectObject, COLOR_WINDOW,
    DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBRUSH, PAINTSTRUCT,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetClientRect, GetMessageW, LoadCursorW,
    LoadIconW, PostMessageW, PostQuitMessage, RegisterClassExW, ShowWindow, TranslateMessage,
    CW_USEDEFAULT, IDC_APPSTARTING, MSG, SW_SHOW, WM_CLOSE, WM_DESTROY, WM_PAINT, WNDCLASSEXW,
    WS_CAPTION, WS_MINIMIZEBOX, WS_OVERLAPPED, WS_SYSMENU,
};

/// How long Android may take to start before opening the app gives up.
pub const START_ANDROID: Duration = Duration::from_secs(180);

/// How long the window manager may take to open its pipe once started.
pub const START_WINDOW_MANAGER: Duration = Duration::from_secs(20);

/// How often the progress of either is looked at.
pub const POLL: Duration = Duration::from_millis(250);

/// The build the window program names itself by on the local API.
const BUILD: &str = "dsa-app-window";

/// The name of the value in `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Run` the installer
/// starts the window manager with at sign-in, which says where it is.
pub const RUN_VALUE: &str = "DsaWindowManager";

/// The window manager's file name, beside the installed window host.
pub const WINDOW_MANAGER_NAME: &str = "dsa-wm.exe";

/// Makes sure the caller's Android runs: asks the service at `api` to start it when it is
/// stopped and waits until it runs. `waiting` is called once when it has to wait, to show
/// that something is happening.
pub fn ensure_android(api: &mut dyn Connector, mut waiting: impl FnMut()) -> Result<(), String> {
    let mut client = HostApiClient::connect(api, BUILD, Duration::from_secs(5))
        .map_err(|e| format!("Android cannot be started: {e}"))?;
    let mut told = false;
    client
        .ensure_running(START_ANDROID, POLL, |_| {
            if !told {
                told = true;
                waiting();
            }
        })
        .map_err(|e| format!("Android did not start: {e}"))
}

/// Makes sure the window manager runs: when `running` says it does not, `start` starts it,
/// and this waits at most `timeout` for `running` to say it does, asking every `poll`.
pub fn ensure_window_manager(
    mut running: impl FnMut() -> bool,
    start: impl FnOnce() -> Result<(), String>,
    timeout: Duration,
    poll: Duration,
) -> Result<(), String> {
    if running() {
        return Ok(());
    }
    start()?;
    let deadline = Instant::now() + timeout;
    while !running() {
        if Instant::now() >= deadline {
            return Err(format!(
                "the DSA Window Manager did not start within {} s",
                timeout.as_secs()
            ));
        }
        thread::sleep(poll);
    }
    Ok(())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Whether the pipe `name` (without `\\.\pipe\`) is open for connections, without connecting.
pub fn pipe_exists(name: &str) -> bool {
    let path = wide(&format!(r"\\.\pipe\{name}"));
    // SAFETY: a NUL-terminated name. It returns at once when there is no such pipe.
    let free = unsafe { WaitNamedPipeW(path.as_ptr(), 1) } != 0;
    // A pipe whose every instance is busy is there all the same.
    free || unsafe { GetLastError() } == ERROR_SEM_TIMEOUT
}

/// Where the window manager is: the program the installer starts at sign-in, else the one
/// beside `beside` (the installed window host).
pub fn window_manager_path(beside: Option<&Path>) -> Option<PathBuf> {
    run_value()
        .map(|command| PathBuf::from(command.trim().trim_matches('"')))
        .filter(|path| path.is_file())
        .or_else(|| {
            beside
                .and_then(Path::parent)
                .map(|dir| dir.join(WINDOW_MANAGER_NAME))
                .filter(|path| path.is_file())
        })
}

/// The command the installer starts the window manager with at sign-in.
fn run_value() -> Option<String> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ};
    let key = wide(r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run");
    let value = wide(RUN_VALUE);
    let mut buffer = vec![0u16; 1024];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: NUL-terminated names and a buffer of the size given.
    let status = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut size,
        )
    };
    if status != 0 {
        return None;
    }
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
}

/// Runs `program` with `arguments` and lets it go.
pub fn run_program(program: &Path, arguments: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
    };
    let application = wide(&program.to_string_lossy());
    let mut command_line = wide(&format!("\"{}\" {arguments}", program.display()));
    // SAFETY: plain structures, zero is their documented initial state.
    let mut startup: STARTUPINFOW = unsafe { std::mem::zeroed() };
    startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
    let mut process: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: NUL-terminated strings; the command line buffer is writable, as the call needs.
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(format!(
            "could not run {}: {}",
            program.display(),
            io::Error::last_os_error()
        ));
    }
    // SAFETY: handles CreateProcessW returned, closed once.
    unsafe {
        CloseHandle(process.hProcess);
        CloseHandle(process.hThread);
    }
    Ok(())
}

/// Tells the user why the app could not be opened, in a message box titled `title`.
pub fn tell_user(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let title = wide(title);
    let text = wide(text);
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        )
    };
}

/// The class of the window shown while the app is being opened.
const STARTING_CLASS: &str = "DsaStartingApp";

/// The program's own icon: the app's, in an app's copy of the window host.
const ICON_ID: usize = 1;

/// What the waiting window says.
pub const STARTING_TEXT: &str = "Starting Android\u{2026}";

/// A small window under the app's name and icon, shown while the app is being opened, so the
/// taskbar shows the app at once. It closes when dropped.
pub struct StartingWindow {
    hwnd: Arc<AtomicIsize>,
    thread: Option<JoinHandle<()>>,
}

impl StartingWindow {
    /// Shows the window, titled `title`, on a thread of its own.
    pub fn show(title: &str) -> StartingWindow {
        let hwnd = Arc::new(AtomicIsize::new(0));
        let shared = Arc::clone(&hwnd);
        let title = title.to_string();
        let thread = thread::Builder::new()
            .name("dsa-app-window-starting".into())
            .spawn(move || starting_window(&title, &shared))
            .ok();
        StartingWindow { hwnd, thread }
    }

    /// The window, once it is open; 0 before.
    pub fn hwnd(&self) -> isize {
        self.hwnd.load(Ordering::SeqCst)
    }

    /// Closes the window and waits for its thread.
    pub fn close(mut self) {
        self.end();
    }

    fn end(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        // The window may still be opening; it is closed as soon as it is there.
        while !thread.is_finished() {
            let hwnd = self.hwnd.load(Ordering::SeqCst);
            if hwnd != 0 {
                // SAFETY: a window of this process; posting to one that is gone does nothing.
                unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = thread.join();
    }
}

impl Drop for StartingWindow {
    fn drop(&mut self) {
        self.end();
    }
}

fn starting_window(title: &str, hwnd: &AtomicIsize) {
    let class = wide(STARTING_CLASS);
    let title = wide(title);
    // SAFETY: plain Win32 calls on this thread's own window; the strings outlive them.
    unsafe {
        let module = GetModuleHandleW(std::ptr::null());
        let icon = LoadIconW(module, ICON_ID as *const u16);
        let class_info = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(starting_window_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: icon,
            hCursor: LoadCursorW(std::ptr::null_mut(), IDC_APPSTARTING),
            hbrBackground: (COLOR_WINDOW + 1) as usize as HBRUSH,
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            hIconSm: icon,
        };
        if RegisterClassExW(&class_info) == 0 && GetLastError() != ERROR_CLASS_ALREADY_EXISTS {
            return;
        }
        let window = CreateWindowExW(
            0,
            class.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            360,
            140,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            module,
            std::ptr::null(),
        );
        if window.is_null() {
            return;
        }
        hwnd.store(window as isize, Ordering::SeqCst);
        ShowWindow(window, SW_SHOW);
        let mut message: MSG = std::mem::zeroed();
        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn starting_window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            // SAFETY: painting this window, between BeginPaint and EndPaint.
            unsafe {
                let mut paint: PAINTSTRUCT = std::mem::zeroed();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                };
                GetClientRect(hwnd, &mut rect);
                SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
                let mut text: Vec<u16> = STARTING_TEXT.encode_utf16().collect();
                DrawTextW(
                    dc,
                    text.as_mut_ptr(),
                    text.len() as i32,
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
                EndPaint(hwnd, &paint);
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
