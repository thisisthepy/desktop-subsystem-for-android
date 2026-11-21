//! The window manager's side of the shared clipboard.
//!
//! The service runs in session 0 and cannot reach the user's clipboard, so the window manager,
//! which runs in the user's session for as long as their Android runs, carries it. It listens
//! for changes to the user's clipboard ([`ClipboardWindow`]) and passes new text and images to
//! the service, and puts what the user's Android copies on the user's clipboard ([`sync`],
//! which runs the loop every host shares). The service decides whether anything crosses, from
//! the user's clipboard setting.
//!
//! Images cross as PNG. The window reads the registered "PNG" format when a program put one
//! there, and otherwise turns `CF_DIBV5` or `CF_DIB` into PNG; it puts an image from Android
//! there as `CF_DIBV5`, from which Windows makes `CF_DIB` and `CF_BITMAP`, and as "PNG", which
//! keeps it exact in the programs that take one.
//!
//! What the window manager put on the clipboard itself is not passed back: the clipboard
//! window ignores changes it made, and [`sync`] does not send the clip it was just given.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread;

use dsa_host_core::clipboard::{self, Clip, HostClipboard};
use dsa_transport::Connector;

use super::log::{note, CompositorLogger};

/// The build the window manager names itself by on the local API.
const BUILD: &str = "dsa-wm clipboard";

/// Keeps the user's clipboard and their Android's in step until `stop` is set; see
/// [`clipboard::sync`].
pub fn sync(
    connector: Box<dyn Connector>,
    clipboard: &mut dyn HostClipboard,
    changes: Receiver<Clip>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    clipboard::sync(connector, BUILD, clipboard, changes, stop, &|line| {
        note(logger.as_deref(), line)
    });
}

/// A message-only window that listens for changes to the user's clipboard, reads its text and
/// images, and puts text and images on it.
pub struct ClipboardWindow {
    /// The window, as an integer so it crosses threads.
    hwnd: Arc<std::sync::atomic::AtomicIsize>,
    thread: Option<thread::JoinHandle<()>>,
}

/// `CF_UNICODETEXT`: UTF-16 text ending in a zero.
const CF_UNICODETEXT: u32 = 13;
/// `CF_DIB`: a device independent bitmap with a `BITMAPINFOHEADER`.
const CF_DIB: u32 = 8;
/// `CF_DIBV5`: a device independent bitmap with a `BITMAPV5HEADER`.
const CF_DIBV5: u32 = 17;

impl ClipboardWindow {
    /// Opens the window on a thread of its own. Each text or image the user copies afterwards
    /// goes to the returned receiver. An error says why the clipboard cannot be followed.
    pub fn open() -> Result<(ClipboardWindow, Receiver<Clip>), String> {
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
    fn read(&mut self) -> Option<Clip> {
        window::read_clip(self.hwnd.load(Ordering::SeqCst))
    }

    fn write(&mut self, clip: &Clip) -> Result<(), String> {
        let hwnd = self.hwnd.load(Ordering::SeqCst);
        match clip {
            Clip::Text(text) => window::write_text(hwnd, text),
            Clip::Image(png) => window::write_image(hwnd, png),
        }
    }
}

mod window {
    use std::cell::RefCell;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Duration;

    use dsa_host_core::clipboard::{self, Clip};
    use windows_sys::Win32::Foundation::{GlobalFree, HWND, LPARAM, LRESULT, WPARAM};
    use windows_sys::Win32::System::DataExchange::{
        AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
        GetClipboardOwner, IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW,
        RemoveClipboardFormatListener, SetClipboardData,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostMessageW, PostQuitMessage, RegisterClassExW, TranslateMessage, HWND_MESSAGE, MSG,
        WM_CLIPBOARDUPDATE, WM_CLOSE, WM_DESTROY, WNDCLASSEXW,
    };

    use super::{CF_DIB, CF_DIBV5, CF_UNICODETEXT};

    thread_local! {
        /// Where the window's thread sends what the user copies.
        static CHANGES: RefCell<Option<Sender<Clip>>> = const { RefCell::new(None) };
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The registered "PNG" clipboard format, which programs that copy images put a PNG
    /// under; 0 when it cannot be registered.
    fn png_format() -> u32 {
        let name = wide("PNG");
        // SAFETY: the name lives through the call.
        unsafe { RegisterClipboardFormatW(name.as_ptr()) }
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
                    if let Some(clip) = read_clip(hwnd as isize) {
                        CHANGES.with(|changes| {
                            if let Some(changes) = changes.borrow().as_ref() {
                                let _ = changes.send(clip);
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
        changes: Sender<Clip>,
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

    /// The bytes of `format` on the open clipboard, if it holds that format.
    ///
    /// # Safety
    ///
    /// The clipboard is open.
    unsafe fn data(format: u32) -> Option<Vec<u8>> {
        let handle = GetClipboardData(format);
        if handle.is_null() {
            return None;
        }
        let data = GlobalLock(handle) as *const u8;
        if data.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(data, GlobalSize(handle)).to_vec();
        GlobalUnlock(handle);
        Some(bytes)
    }

    /// Puts `bytes` on the open clipboard as `format`.
    ///
    /// # Safety
    ///
    /// The clipboard is open and was emptied by this process.
    unsafe fn set_data(format: u32, bytes: &[u8]) -> Result<(), String> {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if memory.is_null() {
            return Err("no memory for the clipboard".to_string());
        }
        let data = GlobalLock(memory) as *mut u8;
        if data.is_null() {
            GlobalFree(memory);
            return Err("could not fill the clipboard".to_string());
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        GlobalUnlock(memory);
        // The clipboard owns the memory once it takes it; it is freed here only if not.
        if SetClipboardData(format, memory).is_null() {
            let why = std::io::Error::last_os_error();
            GlobalFree(memory);
            return Err(format!("the clipboard did not take it: {why}"));
        }
        Ok(())
    }

    /// What the clipboard holds, if it holds text or an image and can be opened: text when
    /// there is text, otherwise the "PNG" format, otherwise a bitmap turned into PNG.
    pub(super) fn read_clip(hwnd: isize) -> Option<Clip> {
        if hwnd == 0 {
            return None;
        }
        let png = png_format();
        // SAFETY: asks only whether formats are there.
        let (has_text, has_png, has_dib) = unsafe {
            (
                IsClipboardFormatAvailable(CF_UNICODETEXT) != 0,
                png != 0 && IsClipboardFormatAvailable(png) != 0,
                IsClipboardFormatAvailable(CF_DIBV5) != 0
                    || IsClipboardFormatAvailable(CF_DIB) != 0,
            )
        };
        if !has_text && !has_png && !has_dib {
            return None;
        }
        if !open(hwnd) {
            return None;
        }
        // SAFETY: the clipboard is open; each handle stays the clipboard's, and is read only
        // between its lock and unlock, within its size.
        let (text, image) = unsafe {
            let text = if has_text {
                data(CF_UNICODETEXT).map(|bytes| {
                    let units: Vec<u16> = bytes
                        .chunks_exact(2)
                        .map(|unit| u16::from_le_bytes([unit[0], unit[1]]))
                        .collect();
                    let len = units
                        .iter()
                        .position(|&unit| unit == 0)
                        .unwrap_or(units.len());
                    String::from_utf16_lossy(&units[..len])
                })
            } else {
                None
            };
            let image = if text.is_some() {
                None
            } else if has_png {
                data(png).map(Ok)
            } else {
                data(CF_DIBV5)
                    .or_else(|| data(CF_DIB))
                    .map(|dib| clipboard::dib_to_png(&dib))
            };
            (text, image)
        };
        // SAFETY: closes what `open` opened.
        unsafe { CloseClipboard() };
        clipboard::choose(text, || match image? {
            Ok(png) => Some(png),
            Err(why) => {
                eprintln!("clipboard: a bitmap copied on the host cannot cross: {why}");
                None
            }
        })
    }

    /// Puts `text` on the clipboard, owned by `hwnd`.
    pub(super) fn write_text(hwnd: isize, text: &str) -> Result<(), String> {
        let units = wide(text);
        let bytes: Vec<u8> = units.iter().flat_map(|unit| unit.to_le_bytes()).collect();
        write(hwnd, &[(CF_UNICODETEXT, &bytes)])
    }

    /// Puts the image `png` on the clipboard, owned by `hwnd`: as `CF_DIBV5`, which every
    /// program pastes, and as "PNG".
    pub(super) fn write_image(hwnd: isize, png: &[u8]) -> Result<(), String> {
        let dib = clipboard::png_to_dib(png)?;
        let format = png_format();
        if format == 0 {
            return write(hwnd, &[(CF_DIBV5, &dib)]);
        }
        write(hwnd, &[(CF_DIBV5, &dib), (format, png)])
    }

    /// Empties the clipboard and puts each format on it, owned by `hwnd`. The first must go
    /// on; the others are extras.
    fn write(hwnd: isize, formats: &[(u32, &[u8])]) -> Result<(), String> {
        if hwnd == 0 {
            return Err("the clipboard's window is closed".into());
        }
        if !open(hwnd) {
            return Err("another program holds the clipboard".into());
        }
        // SAFETY: the clipboard is open and emptied here.
        let result = unsafe {
            EmptyClipboard();
            let mut result = Ok(());
            for (index, (format, bytes)) in formats.iter().enumerate() {
                let put = set_data(*format, bytes);
                if index == 0 {
                    result = put;
                }
            }
            result
        };
        // SAFETY: closes what `open` opened.
        unsafe { CloseClipboard() };
        result
    }
}
