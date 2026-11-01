//! The window manager's side of the shared clipboard.
//!
//! The service runs in session 0 and cannot reach the user's clipboard, so the window manager,
//! which runs in the user's session for as long as their Android runs, carries it. It listens
//! for changes to the user's clipboard ([`ClipboardWindow`]) and passes new text to the service,
//! and puts the text the user's Android copies on the user's clipboard ([`sync`]). The service
//! decides whether anything crosses, from the user's clipboard setting.
//!
//! Text the window manager put on the clipboard itself is not passed back: the clipboard
//! window ignores changes it made, and [`sync`] does not send the text it was just given.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use dsa_host_core::clipboard;
use dsa_host_core::host_api::HostApiClient;
use dsa_transport::Connector;

use super::log::{note, CompositorLogger};

/// The build the window manager names itself by on the local API.
const BUILD: &str = "dsa-wm clipboard";

/// How long the window manager waits for the service to answer.
const ANSWER: Duration = Duration::from_secs(2);

/// How long it waits before trying the service again.
const RETRY: Duration = Duration::from_millis(500);

/// How long it waits for text from Android before it looks for text copied on the host.
const POLL: Duration = Duration::from_millis(50);

/// The user's clipboard, as [`sync`] uses it.
pub trait HostClipboard {
    /// The text on the clipboard now, if it holds text.
    fn text(&mut self) -> Option<String>;
    /// Puts `text` on the clipboard.
    fn set_text(&mut self, text: &str) -> Result<(), String>;
}

/// Keeps the user's clipboard and their Android's in step until `stop` is set.
///
/// `changes` carries each text the user copies on the host. Each new connection to the service
/// first passes the text the clipboard holds then, so the service has it for an Android that
/// starts later. A connection that breaks is made again; text copied meanwhile is not lost,
/// since the next connection passes the clipboard's text.
pub fn sync(
    mut connector: Box<dyn Connector>,
    clipboard: &mut dyn HostClipboard,
    changes: Receiver<String>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut last_error = String::new();
    // The text Android copied that the window manager put on the clipboard last.
    let mut from_android: Option<String> = None;
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_clipboard());
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!("the clipboard cannot reach Android: {error}"),
                    );
                    last_error = error;
                }
                // What was copied meanwhile is on the clipboard for the next connection.
                while changes.try_recv().is_ok() {}
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        let current = clipboard.text();
        if let Some(text) = current.filter(|text| from_android.as_ref() != Some(text)) {
            if offer(&text, logger.as_deref()) && stream.send(&text).is_err() {
                continue;
            }
        }
        'connected: while !stop.load(Ordering::Relaxed) {
            while let Ok(text) = changes.try_recv() {
                if from_android.as_ref() == Some(&text) || !offer(&text, logger.as_deref()) {
                    continue;
                }
                if stream.send(&text).is_err() {
                    break 'connected;
                }
            }
            match stream.next(POLL) {
                Ok(Some(text)) => {
                    if let Err(why) = clipboard.set_text(&text) {
                        note(
                            logger.as_deref(),
                            &format!("could not put text copied in Android on the clipboard: {why}"),
                        );
                    }
                    from_android = Some(text);
                }
                Ok(None) => {}
                Err(error) => {
                    note(
                        logger.as_deref(),
                        &format!("the clipboard's connection to the service ended: {error}"),
                    );
                    break;
                }
            }
        }
    }
}

/// Whether `text` may cross, saying so in the log when it may not.
fn offer(text: &str, logger: Option<&CompositorLogger>) -> bool {
    if clipboard::fits(text) {
        return true;
    }
    note(logger, &clipboard::too_long(text, "host"));
    false
}

/// A message-only window that listens for changes to the user's clipboard, reads its text, and
/// puts text on it.
pub struct ClipboardWindow {
    /// The window, as an integer so it crosses threads.
    hwnd: Arc<std::sync::atomic::AtomicIsize>,
    thread: Option<thread::JoinHandle<()>>,
}

/// `CF_UNICODETEXT`: UTF-16 text ending in a zero.
const CF_UNICODETEXT: u32 = 13;

impl ClipboardWindow {
    /// Opens the window on a thread of its own. Each text the user copies afterwards goes to
    /// the returned receiver. An error says why the clipboard cannot be followed.
    pub fn open() -> Result<(ClipboardWindow, Receiver<String>), String> {
        let (changes_tx, changes) = std::sync::mpsc::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let hwnd = Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let slot = Arc::clone(&hwnd);
        let thread = thread::Builder::new()
            .name("dsa-wm-clipboard-window".into())
            .spawn(move || window::run(slot, changes_tx, ready_tx))
            .map_err(|e| format!("could not start the clipboard's thread: {e}"))?;
        match ready.recv() {
            Ok(Ok(())) => Ok((
                ClipboardWindow {
                    hwnd,
                    thread: Some(thread),
                },
                changes,
            )),
            Ok(Err(why)) => {
                let _ = thread.join();
                Err(why)
            }
            Err(_) => {
                let _ = thread.join();
                Err("the clipboard's thread ended before its window opened".into())
            }
        }
    }

    /// The user's clipboard, through this window.
    pub fn clipboard(&self) -> WindowsClipboard {
        WindowsClipboard {
            hwnd: Arc::clone(&self.hwnd),
        }
    }
}

impl Drop for ClipboardWindow {
    fn drop(&mut self) {
        window::close(self.hwnd.load(Ordering::SeqCst));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The user's clipboard, read and written through a [`ClipboardWindow`].
pub struct WindowsClipboard {
    hwnd: Arc<std::sync::atomic::AtomicIsize>,
}

impl HostClipboard for WindowsClipboard {
    fn text(&mut self) -> Option<String> {
        window::read_text(self.hwnd.load(Ordering::SeqCst))
    }

    fn set_text(&mut self, text: &str) -> Result<(), String> {
        window::write_text(self.hwnd.load(Ordering::SeqCst), text)
    }
}

mod window {
    use std::cell::RefCell;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Duration;

    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::DataExchange::{
        AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
        GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard,
        RemoveClipboardFormatListener, SetClipboardData,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalFree, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostMessageW, PostQuitMessage, RegisterClassExW, TranslateMessage, HWND_MESSAGE, MSG,
        WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WNDCLASSEXW,
    };

    use super::CF_UNICODETEXT;

    thread_local! {
        /// Where the window's thread sends the text the user copies.
        static CHANGES: RefCell<Option<Sender<String>>> = const { RefCell::new(None) };
    }

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
            WM_CLIPBOARDUPDATE => {
                // SAFETY: asks who last put something on the clipboard.
                let owner = unsafe { GetClipboardOwner() };
                // What the window manager put there came from Android; it does not go back.
                if owner != hwnd {
                    if let Some(text) = read_text(hwnd as isize) {
                        CHANGES.with(|changes| {
                            if let Some(changes) = changes.borrow().as_ref() {
                                let _ = changes.send(text);
                            }
                        });
                    }
                }
                0
            }
            WM_CLOSE => {
                // SAFETY: the window belongs to this thread.
                unsafe { DestroyWindow(hwnd) };
                0
            }
            WM_DESTROY => {
                // SAFETY: the window belongs to this thread and is going away.
                unsafe {
                    RemoveClipboardFormatListener(hwnd);
                    PostQuitMessage(0);
                }
                0
            }
            // SAFETY: the default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// Opens the window, says whether it did, and runs its messages until it is closed.
    pub(super) fn run(
        slot: Arc<AtomicIsize>,
        changes: Sender<String>,
        ready: Sender<Result<(), String>>,
    ) {
        CHANGES.with(|slot| *slot.borrow_mut() = Some(changes));
        let class_name = wide(&format!("DsaWmClipboard_{}", std::process::id()));
        // SAFETY: the module handle of this process.
        let instance = unsafe { GetModuleHandleW(null()) };
        let class = WNDCLASSEXW {
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
        // SAFETY: registers the class of the message-only window; the names outlive it.
        unsafe { RegisterClassExW(&class) };
        // SAFETY: a message-only window, which is never shown and receives no broadcasts.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
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
        if hwnd.is_null() {
            let _ = ready.send(Err(format!(
                "could not open the clipboard's window: {}",
                std::io::Error::last_os_error()
            )));
            return;
        }
        // SAFETY: the window is this thread's.
        if unsafe { AddClipboardFormatListener(hwnd) } == 0 {
            let why = std::io::Error::last_os_error();
            // SAFETY: the window is this thread's.
            unsafe { DestroyWindow(hwnd) };
            let _ = ready.send(Err(format!(
                "could not listen for changes to the clipboard: {why}"
            )));
            return;
        }
        slot.store(hwnd as isize, Ordering::SeqCst);
        let _ = ready.send(Ok(()));
        // SAFETY: an all-zero MSG is valid to receive into.
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        loop {
            // SAFETY: the message loop of this thread's window.
            let result = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
            if result <= 0 {
                break;
            }
            // SAFETY: dispatches the message just received.
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        slot.store(0, Ordering::SeqCst);
    }

    /// Asks the window to close; its thread then ends.
    pub(super) fn close(hwnd: isize) {
        if hwnd != 0 {
            // SAFETY: posting to a window that may already be gone only fails.
            unsafe { PostMessageW(hwnd as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// Opens the clipboard for `hwnd`, waiting a little while another program holds it.
    fn open(hwnd: isize) -> bool {
        for _ in 0..20 {
            // SAFETY: opening the clipboard for a window of this process.
            if unsafe { OpenClipboard(hwnd as HWND) } != 0 {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    /// The text on the clipboard, if it holds text and can be opened.
    pub(super) fn read_text(hwnd: isize) -> Option<String> {
        if hwnd == 0 {
            return None;
        }
        // SAFETY: asks only whether a format is there.
        if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) } == 0 {
            return None;
        }
        if !open(hwnd) {
            return None;
        }
        // SAFETY: the clipboard is open; the handle stays the clipboard's, and is read only
        // between its lock and unlock, within its size.
        let text = unsafe {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() {
                None
            } else {
                let data = GlobalLock(handle) as *const u16;
                if data.is_null() {
                    None
                } else {
                    let units = GlobalSize(handle) / 2;
                    let all = std::slice::from_raw_parts(data, units);
                    let len = all.iter().position(|&unit| unit == 0).unwrap_or(units);
                    let text = String::from_utf16_lossy(&all[..len]);
                    GlobalUnlock(handle);
                    Some(text)
                }
            }
        };
        // SAFETY: closes what `open` opened.
        unsafe { CloseClipboard() };
        text
    }

    /// Puts `text` on the clipboard, owned by `hwnd`.
    pub(super) fn write_text(hwnd: isize, text: &str) -> Result<(), String> {
        if hwnd == 0 {
            return Err("the clipboard's window is closed".into());
        }
        let units = wide(text);
        if !open(hwnd) {
            return Err("another program holds the clipboard".into());
        }
        // SAFETY: the clipboard is open. The memory is filled within its size, and the
        // clipboard owns it once SetClipboardData takes it; it is freed here only if not.
        let result = unsafe {
            EmptyClipboard();
            let memory = GlobalAlloc(GMEM_MOVEABLE, units.len() * 2);
            if memory.is_null() {
                Err("no memory for the clipboard's text".to_string())
            } else {
                let data = GlobalLock(memory) as *mut u16;
                if data.is_null() {
                    GlobalFree(memory);
                    Err("could not fill the clipboard's text".to_string())
                } else {
                    std::ptr::copy_nonoverlapping(units.as_ptr(), data, units.len());
                    GlobalUnlock(memory);
                    if SetClipboardData(CF_UNICODETEXT, memory).is_null() {
                        let why = std::io::Error::last_os_error();
                        GlobalFree(memory);
                        Err(format!("the clipboard did not take the text: {why}"))
                    } else {
                        Ok(())
                    }
                }
            }
        };
        // SAFETY: closes what `open` opened.
        unsafe { CloseClipboard() };
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
    use dsa_transport::local;
    use std::sync::mpsc;
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(10);

    /// A clipboard in memory that records what was put on it.
    #[derive(Default)]
    struct FakeClipboard {
        text: Option<String>,
        set: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl HostClipboard for FakeClipboard {
        fn text(&mut self) -> Option<String> {
            self.text.clone()
        }
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            self.text = Some(text.to_string());
            self.set.lock().unwrap().push(text.to_string());
            Ok(())
        }
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn fr13_the_window_manager_carries_text_both_ways_without_echo() {
        let (listener, connector) = local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let _service = RunningService::start(Box::new(listener), androids.clone());
        let (changes_tx, changes) = mpsc::channel();
        let set = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut host = FakeClipboard {
            text: Some("on the clipboard at the start".into()),
            set: Arc::clone(&set),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let syncing = thread::spawn(move || {
            sync(Box::new(connector), &mut host, changes, flag, None);
        });

        // The clipboard's text at the start goes to the service.
        wait_until("the first text", || {
            androids
                .of("test")
                .is_some_and(|android| android.host_clipboard.lock().unwrap().len() == 1)
        });
        let android = androids.of("test").unwrap();

        // Text copied on the host goes to the service.
        changes_tx.send("copied on the host".into()).unwrap();
        wait_until("the copied text", || {
            android.host_clipboard.lock().unwrap().len() == 2
        });

        // Text Android copied goes on the clipboard, and is not sent back when the clipboard
        // says it changed.
        android.copy_in_android("copied in Android");
        wait_until("the clipboard to be set", || set.lock().unwrap().len() == 1);
        changes_tx.send("copied in Android".into()).unwrap();
        changes_tx.send("then the host again".into()).unwrap();
        wait_until("the next text", || {
            android.host_clipboard.lock().unwrap().len() == 3
        });
        assert_eq!(
            *android.host_clipboard.lock().unwrap(),
            vec![
                "on the clipboard at the start".to_string(),
                "copied on the host".to_string(),
                "then the host again".to_string(),
            ]
        );
        assert_eq!(*set.lock().unwrap(), vec!["copied in Android".to_string()]);

        stop.store(true, Ordering::SeqCst);
        syncing.join().unwrap();
    }

    #[test]
    fn fr13_text_longer_than_the_limit_is_not_passed_on() {
        let (listener, connector) = local::pair();
        let androids = Arc::new(FakeAndroids::default());
        let _service = RunningService::start(Box::new(listener), androids.clone());
        let (changes_tx, changes) = mpsc::channel();
        let mut host = FakeClipboard::default();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let syncing = thread::spawn(move || {
            sync(Box::new(connector), &mut host, changes, flag, None);
        });
        changes_tx
            .send("x".repeat(clipboard::MAX_CLIPBOARD_BYTES + 1))
            .unwrap();
        changes_tx.send("short".into()).unwrap();
        wait_until("the short text", || {
            androids
                .of("test")
                .is_some_and(|android| !android.host_clipboard.lock().unwrap().is_empty())
        });
        assert_eq!(
            *androids.of("test").unwrap().host_clipboard.lock().unwrap(),
            vec!["short".to_string()]
        );
        stop.store(true, Ordering::SeqCst);
        syncing.join().unwrap();
    }
}
