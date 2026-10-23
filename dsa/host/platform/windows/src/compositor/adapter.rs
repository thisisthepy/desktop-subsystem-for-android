//! Integration adapter point for the native gfxstream host renderer backend.
//!
//! The gfxstream host renderer (`gfxstream_backend.dll`) remotes guest Vulkan and
//! OpenGL ES calls onto the host Vulkan driver. This adapter implements [`GraphicsRenderer`]
//! and serves as the integration point between accepted Hyper-V socket streams and the native
//! graphics backend.

use std::ffi::{c_int, c_void, CStr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::compositor::{GraphicsRenderer, GraphicsStreamKind};
use dsa_transport::Connection;
use windows_sys::Win32::Foundation::HMODULE;
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

#[link(name = "kernel32")]
extern "system" {
    fn FreeLibrary(hLibModule: HMODULE) -> i32;
}

/// Default filename of the native gfxstream host renderer DLL.
pub const GFXSTREAM_BACKEND_DLL: &str = "gfxstream_backend.dll";

/// Function pointer type for reading from a stream callback.
pub type StreamReadFn =
    unsafe extern "C" fn(user_data: *mut c_void, buf: *mut c_void, count: usize) -> c_int;

/// Function pointer type for writing to a stream callback.
pub type StreamWriteFn =
    unsafe extern "C" fn(user_data: *mut c_void, buf: *const c_void, count: usize) -> c_int;

/// Function pointer type for framebuffer post notifications.
pub type PostCallbackFn = unsafe extern "C" fn(
    context: *mut c_void,
    display_id: u32,
    width: c_int,
    height: c_int,
    ydir: c_int,
    format: c_int,
    pixel_type: c_int,
    pixels: *mut u8,
);

/// Function pointer type for color buffer post notifications.
pub type ColorBufferPostFn = unsafe extern "C" fn(context: *mut c_void, color_buffer_id: u32);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Abstract backend interface to support real native and fake testing backends.
pub trait GfxBackend: Send + Sync {
    /// Serves an accepted command stream (port 5566).
    ///
    /// # Safety
    /// `user_data` must remain valid for the duration of the call.
    unsafe fn serve_stream(
        &self,
        read_cb: StreamReadFn,
        write_cb: StreamWriteFn,
        user_data: *mut c_void,
    ) -> Result<(), String>;

    /// Serves a process pipe connection (port 5567).
    ///
    /// # Safety
    /// `user_data` must remain valid for the duration of the call.
    unsafe fn process_pipe(
        &self,
        read_cb: StreamReadFn,
        write_cb: StreamWriteFn,
        user_data: *mut c_void,
    ) -> Result<(), String>;

    /// Generates a unique process identifier and notifies the backend.
    fn on_process_create(&self) -> u64;

    /// Cleans up graphics objects for the given process identifier.
    fn on_process_destroy(&self, puid: u64);
}

/// Native backend loaded from `gfxstream_backend.dll`.
pub struct NativeBackend {
    handle: HMODULE,
    #[allow(dead_code)]
    fn_init: unsafe extern "C" fn(u32, u32) -> c_int,
    fn_teardown: unsafe extern "C" fn(),
    fn_serve_stream: unsafe extern "C" fn(StreamReadFn, StreamWriteFn, *mut c_void) -> c_int,
    fn_process_pipe: unsafe extern "C" fn(StreamReadFn, StreamWriteFn, *mut c_void) -> c_int,
    fn_on_process_create: unsafe extern "C" fn() -> u64,
    fn_on_process_destroy: unsafe extern "C" fn(u64),
}

// SAFETY: HMODULE is owned and accessed across threads safely through the loaded function pointers.
unsafe impl Send for NativeBackend {}
unsafe impl Sync for NativeBackend {}

impl NativeBackend {
    /// Attempts to load the native backend from the specified DLL path.
    pub fn load(path: &Path) -> Result<NativeBackend, String> {
        let wide_path = wide(&path.to_string_lossy());
        // SAFETY: LoadLibraryW with null-terminated UTF-16 path.
        let handle = unsafe { LoadLibraryW(wide_path.as_ptr()) };
        if handle.is_null() {
            return Err(format!(
                "could not load graphics backend library {}: os error {}",
                path.display(),
                std::io::Error::last_os_error()
            ));
        }

        unsafe {
            let fn_init: unsafe extern "C" fn(u32, u32) -> c_int =
                Self::get_symbol(handle, b"dsa_gfx_init\0")?;
            let fn_teardown = Self::get_symbol(handle, b"dsa_gfx_teardown\0")?;
            let fn_serve_stream = Self::get_symbol(handle, b"dsa_gfx_serve_stream\0")?;
            let fn_process_pipe = Self::get_symbol(handle, b"dsa_gfx_process_pipe\0")?;
            let fn_on_process_create = Self::get_symbol(handle, b"dsa_gfx_on_process_create\0")?;
            let fn_on_process_destroy = Self::get_symbol(handle, b"dsa_gfx_on_process_destroy\0")?;

            let ret = (fn_init)(1280, 720);
            if ret != 0 {
                FreeLibrary(handle);
                return Err(format!("dsa_gfx_init failed with code {ret}"));
            }

            Ok(NativeBackend {
                handle,
                fn_init,
                fn_teardown,
                fn_serve_stream,
                fn_process_pipe,
                fn_on_process_create,
                fn_on_process_destroy,
            })
        }
    }

    unsafe fn get_symbol<T>(handle: HMODULE, symbol: &[u8]) -> Result<T, String> {
        let proc = GetProcAddress(handle, symbol.as_ptr());
        if proc.is_none() {
            let name = CStr::from_bytes_with_nul_unchecked(symbol).to_string_lossy();
            return Err(format!("missing exported symbol {name}"));
        }
        Ok(std::mem::transmute_copy(&proc))
    }
}

impl Drop for NativeBackend {
    fn drop(&mut self) {
        unsafe {
            (self.fn_teardown)();
            FreeLibrary(self.handle);
        }
    }
}

impl GfxBackend for NativeBackend {
    unsafe fn serve_stream(
        &self,
        read_cb: StreamReadFn,
        write_cb: StreamWriteFn,
        user_data: *mut c_void,
    ) -> Result<(), String> {
        let ret = (self.fn_serve_stream)(read_cb, write_cb, user_data);
        if ret == 0 {
            Ok(())
        } else {
            Err(format!("dsa_gfx_serve_stream returned code {ret}"))
        }
    }

    unsafe fn process_pipe(
        &self,
        read_cb: StreamReadFn,
        write_cb: StreamWriteFn,
        user_data: *mut c_void,
    ) -> Result<(), String> {
        let ret = (self.fn_process_pipe)(read_cb, write_cb, user_data);
        if ret == 0 {
            Ok(())
        } else {
            Err(format!("dsa_gfx_process_pipe returned code {ret}"))
        }
    }

    fn on_process_create(&self) -> u64 {
        unsafe { (self.fn_on_process_create)() }
    }

    fn on_process_destroy(&self, puid: u64) {
        unsafe { (self.fn_on_process_destroy)(puid) }
    }
}

struct StreamContext<'a> {
    connection: Mutex<&'a mut dyn Connection>,
    pending: Mutex<Vec<u8>>,
}

impl<'a> StreamContext<'a> {
    fn new(connection: &'a mut dyn Connection) -> Self {
        StreamContext {
            connection: Mutex::new(connection),
            pending: Mutex::new(Vec::new()),
        }
    }
}

unsafe extern "C" fn stream_read_trampoline(
    user_data: *mut c_void,
    buf: *mut c_void,
    count: usize,
) -> c_int {
    if user_data.is_null() || buf.is_null() || count == 0 {
        return 0;
    }
    let ctx = &*(user_data as *const StreamContext);
    let mut pending = match ctx.pending.lock() {
        Ok(guard) => guard,
        Err(_) => return -1,
    };

    if !pending.is_empty() {
        let n = pending.len().min(count);
        std::ptr::copy_nonoverlapping(pending.as_ptr(), buf as *mut u8, n);
        pending.drain(..n);
        return n as c_int;
    }

    let mut conn = match ctx.connection.lock() {
        Ok(guard) => guard,
        Err(_) => return -1,
    };

    match conn.recv(Some(Duration::from_millis(50))) {
        Ok(bytes) => {
            if bytes.is_empty() {
                return -1;
            }
            let n = bytes.len().min(count);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
            if bytes.len() > n {
                pending.extend_from_slice(&bytes[n..]);
            }
            n as c_int
        }
        Err(dsa_transport::TransportError::Timeout) => 0,
        Err(_) => -1,
    }
}

unsafe extern "C" fn stream_write_trampoline(
    user_data: *mut c_void,
    buf: *const c_void,
    count: usize,
) -> c_int {
    if user_data.is_null() {
        return -1;
    }
    if buf.is_null() || count == 0 {
        return 0;
    }
    let ctx = &*(user_data as *const StreamContext);
    let mut conn = match ctx.connection.lock() {
        Ok(guard) => guard,
        Err(_) => return -1,
    };
    let slice = std::slice::from_raw_parts(buf as *const u8, count);
    match conn.send(slice) {
        Ok(()) => count as c_int,
        Err(_) => -1,
    }
}

/// Adapter connecting guest graphics streams to the native gfxstream host renderer.
pub struct GfxstreamAdapter {
    dll_path: Option<PathBuf>,
    backend: Option<Arc<dyn GfxBackend>>,
}

impl GfxstreamAdapter {
    /// Creates a new adapter, searching for `gfxstream_backend.dll` at `custom_path`, beside
    /// the current executable, or in the current directory.
    pub fn new(custom_path: Option<PathBuf>) -> GfxstreamAdapter {
        let dll_path = custom_path.or_else(Self::find_backend_dll);
        let backend = if let Some(path) = &dll_path {
            match NativeBackend::load(path) {
                Ok(b) => Some(Arc::new(b) as Arc<dyn GfxBackend>),
                Err(err) => {
                    eprintln!("failed to load gfxstream backend library: {err}");
                    None
                }
            }
        } else {
            None
        };

        GfxstreamAdapter { dll_path, backend }
    }

    /// Creates an adapter configured with a custom or fake backend for testing.
    pub fn with_backend(backend: Arc<dyn GfxBackend>) -> GfxstreamAdapter {
        GfxstreamAdapter {
            dll_path: None,
            backend: Some(backend),
        }
    }

    /// Returns the resolved path to `gfxstream_backend.dll` if found.
    pub fn backend_path(&self) -> Option<&Path> {
        self.dll_path.as_deref()
    }

    /// Whether the graphics backend is available and ready to serve streams.
    pub fn is_available(&self) -> bool {
        self.backend.is_some()
    }

    fn find_backend_dll() -> Option<PathBuf> {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let candidate = dir.join(GFXSTREAM_BACKEND_DLL);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        let local_candidate = PathBuf::from(GFXSTREAM_BACKEND_DLL);
        if local_candidate.is_file() {
            Some(local_candidate)
        } else {
            None
        }
    }
}

impl GraphicsRenderer for GfxstreamAdapter {
    fn serve_stream(
        &self,
        kind: GraphicsStreamKind,
        mut connection: Box<dyn Connection>,
    ) -> Result<(), String> {
        if let Some(backend) = &self.backend {
            match kind {
                GraphicsStreamKind::ProcessPipe => {
                    let mut ctx = StreamContext::new(&mut *connection);
                    let user_data = &mut ctx as *mut StreamContext as *mut c_void;
                    // SAFETY: user_data points to a valid StreamContext on the current stack frame.
                    unsafe {
                        backend.process_pipe(
                            stream_read_trampoline,
                            stream_write_trampoline,
                            user_data,
                        )
                    }
                }
                GraphicsStreamKind::Command => {
                    let mut ctx = StreamContext::new(&mut *connection);
                    let user_data = &mut ctx as *mut StreamContext as *mut c_void;
                    // SAFETY: user_data points to a valid StreamContext on the current stack frame.
                    unsafe {
                        backend.serve_stream(
                            stream_read_trampoline,
                            stream_write_trampoline,
                            user_data,
                        )
                    }
                }
            }
        } else {
            // When the backend DLL has not been deployed, the stream is acknowledged
            // and maintained so that guest connections do not immediately crash.
            while let Ok(n) = connection.recv(None) {
                if n.is_empty() {
                    break;
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_transport::memory;
    use dsa_transport::{Connector, Listener};
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    struct TestFakeBackend {
        process_created: AtomicU64,
        process_destroyed: AtomicU64,
        serve_stream_calls: AtomicUsize,
    }

    impl TestFakeBackend {
        fn new() -> Self {
            TestFakeBackend {
                process_created: AtomicU64::new(0),
                process_destroyed: AtomicU64::new(0),
                serve_stream_calls: AtomicUsize::new(0),
            }
        }
    }

    impl GfxBackend for TestFakeBackend {
        unsafe fn serve_stream(
            &self,
            read_cb: StreamReadFn,
            write_cb: StreamWriteFn,
            user_data: *mut c_void,
        ) -> Result<(), String> {
            self.serve_stream_calls.fetch_add(1, Ordering::Relaxed);
            let mut buf = [0u8; 64];
            let n = read_cb(user_data, buf.as_mut_ptr() as *mut c_void, buf.len());
            if n > 0 {
                let resp = b"pong";
                write_cb(user_data, resp.as_ptr() as *const c_void, resp.len());
            }
            Ok(())
        }

        unsafe fn process_pipe(
            &self,
            read_cb: StreamReadFn,
            write_cb: StreamWriteFn,
            user_data: *mut c_void,
        ) -> Result<(), String> {
            let mut confirm = 0i32;
            let n = unsafe {
                read_cb(
                    user_data,
                    &mut confirm as *mut i32 as *mut c_void,
                    std::mem::size_of::<i32>(),
                )
            };
            if n != std::mem::size_of::<i32>() as c_int || confirm != 100 {
                return Err("invalid confirm code".into());
            }

            let puid = self.on_process_create();
            let sent = unsafe {
                write_cb(
                    user_data,
                    &puid as *const u64 as *const c_void,
                    std::mem::size_of::<u64>(),
                )
            };
            if sent != std::mem::size_of::<u64>() as c_int {
                self.on_process_destroy(puid);
                return Err("failed to send puid".into());
            }

            let mut dummy = [0u8; 8];
            loop {
                let n =
                    unsafe { read_cb(user_data, dummy.as_mut_ptr() as *mut c_void, dummy.len()) };
                if n < 0 {
                    break;
                }
            }

            self.on_process_destroy(puid);
            Ok(())
        }

        fn on_process_create(&self) -> u64 {
            self.process_created.fetch_add(1, Ordering::Relaxed);
            42
        }

        fn on_process_destroy(&self, _puid: u64) {
            self.process_destroyed.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn fr3_missing_backend_dll_logs_and_does_not_panic() {
        let adapter = GfxstreamAdapter::new(Some(PathBuf::from("nonexistent_path_to_dll.dll")));
        assert!(!adapter.is_available());

        let (mut server, mut client) = memory::pair();
        std::thread::spawn(move || {
            let mut conn = client.connect(Duration::from_secs(1)).expect("connect");
            let _ = conn.send(b"hello");
            conn.close();
        });

        let conn = server.accept(Duration::from_secs(1)).expect("accept");
        let result = adapter.serve_stream(GraphicsStreamKind::Command, conn);
        assert!(result.is_ok());
    }

    #[test]
    fn pr3_fake_backend_serves_process_pipe() {
        let fake = Arc::new(TestFakeBackend::new());
        let adapter = GfxstreamAdapter::with_backend(fake.clone());
        assert!(adapter.is_available());

        let (mut server, mut client) = memory::pair();
        let client_thread = std::thread::spawn(move || {
            let mut conn = client.connect(Duration::from_secs(1)).expect("connect");
            conn.send(&100i32.to_le_bytes()).expect("send confirm");
            let puid_bytes = conn.recv(Some(Duration::from_secs(1))).expect("recv puid");
            assert_eq!(puid_bytes.len(), 8);
            let puid = u64::from_le_bytes(puid_bytes.try_into().unwrap());
            assert_eq!(puid, 42);
            conn.close();
        });

        let conn = server.accept(Duration::from_secs(1)).expect("accept");
        let result = adapter.serve_stream(GraphicsStreamKind::ProcessPipe, conn);
        assert!(result.is_ok());

        client_thread.join().expect("client thread join");
        assert_eq!(fake.process_created.load(Ordering::Relaxed), 1);
        assert_eq!(fake.process_destroyed.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn pr3_fake_backend_serves_command_stream() {
        let fake = Arc::new(TestFakeBackend::new());
        let adapter = GfxstreamAdapter::with_backend(fake.clone());

        let (mut server, mut client) = memory::pair();
        let client_thread = std::thread::spawn(move || {
            let mut conn = client.connect(Duration::from_secs(1)).expect("connect");
            conn.send(b"ping").expect("send ping");
            let reply = conn.recv(Some(Duration::from_secs(1))).expect("recv reply");
            assert_eq!(reply, b"pong");
            conn.close();
        });

        let conn = server.accept(Duration::from_secs(1)).expect("accept");
        let result = adapter.serve_stream(GraphicsStreamKind::Command, conn);
        assert!(result.is_ok());

        client_thread.join().expect("client thread join");
        assert_eq!(fake.serve_stream_calls.load(Ordering::Relaxed), 1);
    }
}
