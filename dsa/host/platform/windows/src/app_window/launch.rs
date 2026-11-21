//! Opening an app from its Start menu shortcut, a pinned taskbar button or Windows search.
//!
//! The app's window program then starts with no window manager behind it, and maybe no
//! Android either. Before it becomes the app's window host it makes sure both run: it starts the
//! window manager when its pipe is not there, because Android cannot finish booting without the
//! renderer in it, then asks the service to start the user's Android and waits until it is
//! ready. While it waits it shows a window under the app's name
//! and icon with the DSA logo breathing in it, so the taskbar shows the app at once. Then it connects to the window manager
//! as the app's window host, which asks Android for the app; if the app has a window host
//! already, that one gets the app and this program ends, so an app never has two.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use dsa_transport::Connector;
use windows_sys::Win32::Foundation::{GetLastError, ERROR_SEM_TIMEOUT, HWND};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};

/// How long Android may take to start before opening the app gives up.
pub const START_ANDROID: Duration = dsa_host_core::host_api::APP_START_ANDROID;

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
pub fn ensure_android(api: &mut dyn Connector, waiting: impl FnMut()) -> Result<(), String> {
    // The service sizes Android's display from this when it starts it. It can still start
    // without: the last record, or the image's size, serves.
    if let Err(why) = crate::monitors::record_current() {
        eprintln!("{BUILD}: {why}");
    }
    dsa_host_core::host_api::ensure_android_for_app(api, BUILD, START_ANDROID, POLL, waiting)
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

/// Makes sure what an app needs runs, in the order that works: Android cannot finish booting
/// until the window manager serves its renderer.
pub fn prepare_app(
    window_manager: impl FnOnce() -> Result<(), String>,
    android: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    window_manager()?;
    android()
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

/// Tells the user that something they asked for went well, in a message box titled `title`.
pub fn tell_user_done(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_OK};
    let title = wide(title);
    let text = wide(text);
    // SAFETY: NUL-terminated strings that outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        )
    };
}

/// What the waiting window says of itself to a screen reader, in the user's display language.
pub fn starting_text() -> &'static str {
    super::messages::starting(crate::ui_language::user_language())
}

/// A window under the app's name and icon, shown while the app is being opened, so the
/// taskbar shows the app at once. It shows the DSA logo breathing while Android starts
/// ([`super::starting_splash`]) and fades out when it is closed.
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
            .spawn(move || super::starting_splash::run(&title, &shared))
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
