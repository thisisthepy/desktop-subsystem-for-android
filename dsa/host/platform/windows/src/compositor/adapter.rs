//! Integration adapter point for the native gfxstream host renderer backend.
//!
//! The gfxstream host renderer (`gfxstream_backend.dll`) remotes guest Vulkan and
//! OpenGL ES calls onto the host Vulkan driver. This adapter implements [`GraphicsRenderer`]
//! and serves as the integration point between accepted Hyper-V socket streams and the native
//! graphics backend.
//!
//! It also hands each display's frames to a [`DisplaySink`]: the renderer asks the sink for
//! a Direct3D 11 texture to copy a display's newest frame into, copies it on the GPU, and
//! tells the sink when it is there.
//!
//! Before the renderer starts, the adapter asks it for the GPU to run on (see
//! [`crate::render_gpu`]), and afterwards reports which GPU it runs on.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::compositor::{GraphicsRenderer, GraphicsStreamKind};
use dsa_transport::{Connection, FrameSender};

use crate::render_gpu::{GpuRequest, Luid, RendererGpu};
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

/// What the renderer reports about its GPU, as `dsa_gfx_gpu_info` lays it out.
#[repr(C)]
struct GpuInfo {
    name: [u8; 256],
    device_type: u32,
    luid: [u8; 8],
    luid_valid: i32,
    preference_matched: i32,
}

/// The backend's rules for choosing a GPU (`DSA_GFX_GPU_*`).
const GPU_LUID: u32 = 1;
const GPU_INTEGRATED: u32 = 2;
const GPU_DISCRETE: u32 = 3;
const GPU_NAME: u32 = 4;

type SelectGpuFn = unsafe extern "C" fn(u32, *const u8, *const c_char) -> c_int;
type GetGpuInfoFn = unsafe extern "C" fn(*mut GpuInfo) -> c_int;

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

/// Asks where to copy a display's new frame: returns a Direct3D 11 texture's shared NT
/// handle, or null to drop the frame.
pub type DisplayTargetFn = unsafe extern "C" fn(
    context: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
) -> *mut c_void;

/// Says that a display's frame is in the texture [`DisplayTargetFn`] named, filling its
/// top-left `width` x `height` pixels.
pub type DisplayFrameFn =
    unsafe extern "C" fn(context: *mut c_void, display_id: u32, width: u32, height: u32);

/// Where each display's frames go.
///
/// Both calls come from the renderer's presentation thread, one display at a time: first
/// [`DisplaySink::target`] for a frame, then, once the frame has been copied,
/// [`DisplaySink::frame`].
pub trait DisplaySink: Send + Sync {
    /// The shared NT handle of a Direct3D 11 texture of exactly `width` x `height` pixels in
    /// `DXGI_FORMAT_R8G8B8A8_UNORM` to copy the display's new frame into, or `None` to drop
    /// the frame. The size is the guest's buffer's, which stays at the largest size the task
    /// can have while its window is resized. The handle must stay open until a later call
    /// for the same display returns something else.
    fn target(&self, display_id: u32, width: u32, height: u32) -> Option<isize>;

    /// The frame is in the texture, filling its top-left `content` pixels, and the GPU has
    /// finished copying it. The texture is not written again for this display until this
    /// returns.
    fn frame(&self, display_id: u32, content: (u32, u32));
}

/// What can hand display frames to a [`DisplaySink`].
pub trait DisplayPresenter: Send + Sync {
    /// Starts handing every display's frames to `sink`, or stops with `None`.
    fn present_displays(&self, sink: Option<Arc<dyn DisplaySink>>) -> Result<(), String>;

    /// The LUID of the GPU the frames are on, so the sink's textures are made on the same one.
    fn adapter_luid(&self) -> Option<[u8; 8]>;
}

/// The sink registered with the native backend, kept alive while it is registered.
type SinkBox = Box<Arc<dyn DisplaySink>>;

unsafe extern "C" fn display_target_trampoline(
    context: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
) -> *mut c_void {
    if context.is_null() {
        return std::ptr::null_mut();
    }
    let sink = &*(context as *const Arc<dyn DisplaySink>);
    // A panic must not cross into the renderer; the frame is dropped instead.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sink.target(display_id, width, height)
    }))
    .ok()
    .flatten()
    .map_or(std::ptr::null_mut(), |handle| handle as *mut c_void)
}

unsafe extern "C" fn display_frame_trampoline(
    context: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
) {
    if context.is_null() {
        return;
    }
    let sink = &*(context as *const Arc<dyn DisplaySink>);
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        sink.frame(display_id, (width, height))
    }));
}

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

    /// Registers the per-display presentation callbacks, or stops presenting with `None`.
    ///
    /// # Safety
    /// `context` must stay valid until the callbacks are replaced or removed.
    unsafe fn set_display_callbacks(
        &self,
        callbacks: Option<(DisplayTargetFn, DisplayFrameFn)>,
        context: *mut c_void,
    ) -> Result<(), String> {
        let _ = (callbacks, context);
        Err("this graphics backend does not present displays".into())
    }

    /// The LUID of the GPU the renderer runs on.
    fn adapter_luid(&self) -> Option<[u8; 8]> {
        None
    }

    /// The GPU the renderer runs on, and whether it is the one asked for.
    fn gpu(&self) -> Option<RendererGpu> {
        None
    }
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
    /// Absent from a backend built before frames carried their content size: it still
    /// serves streams, and the windows stay empty.
    fn_set_display_callbacks: Option<
        unsafe extern "C" fn(Option<DisplayTargetFn>, Option<DisplayFrameFn>, *mut c_void) -> c_int,
    >,
    fn_get_adapter_luid: Option<unsafe extern "C" fn(*mut u8) -> c_int>,
    /// Absent from a backend built before the render GPU could be chosen.
    fn_get_gpu_info: Option<GetGpuInfoFn>,
}

// SAFETY: HMODULE is owned and accessed across threads safely through the loaded function pointers.
unsafe impl Send for NativeBackend {}
unsafe impl Sync for NativeBackend {}

impl NativeBackend {
    /// Attempts to load the native backend from the specified DLL path, letting the
    /// renderer rank the GPUs itself.
    pub fn load(path: &Path) -> Result<NativeBackend, String> {
        Self::load_on(path, &GpuRequest::Ranking).map(|(backend, _)| backend)
    }

    /// Loads the native backend from `path` and starts the renderer on the GPU `request`
    /// asks for. Also returns why the request could not be passed on, when it could not.
    pub fn load_on(
        path: &Path,
        request: &GpuRequest,
    ) -> Result<(NativeBackend, Option<String>), String> {
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
            let fn_set_display_callbacks =
                Self::get_symbol(handle, b"dsa_gfx_set_display_callbacks2\0").ok();
            let fn_get_adapter_luid = Self::get_symbol(handle, b"dsa_gfx_get_adapter_luid\0").ok();
            let fn_select_gpu: Option<SelectGpuFn> =
                Self::get_symbol(handle, b"dsa_gfx_select_gpu\0").ok();
            let fn_get_gpu_info: Option<GetGpuInfoFn> =
                Self::get_symbol(handle, b"dsa_gfx_get_gpu_info\0").ok();

            let note = select_gpu(fn_select_gpu, request);

            let ret = (fn_init)(1280, 720);
            if ret != 0 {
                FreeLibrary(handle);
                return Err(format!("dsa_gfx_init failed with code {ret}"));
            }

            Ok((
                NativeBackend {
                    handle,
                    fn_init,
                    fn_teardown,
                    fn_serve_stream,
                    fn_process_pipe,
                    fn_on_process_create,
                    fn_on_process_destroy,
                    fn_set_display_callbacks,
                    fn_get_adapter_luid,
                    fn_get_gpu_info,
                },
                note,
            ))
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

/// Passes `request` to the backend before it starts. Returns why it could not, if it could
/// not; the renderer then ranks the GPUs itself.
///
/// # Safety
/// `select` must be the backend's `dsa_gfx_select_gpu`, called before `dsa_gfx_init`.
unsafe fn select_gpu(select: Option<SelectGpuFn>, request: &GpuRequest) -> Option<String> {
    let (no_luid, no_name) = (std::ptr::null(), std::ptr::null());
    let name;
    let (rule, luid, name_ptr) = match request {
        GpuRequest::Ranking => return None,
        GpuRequest::Luid(Luid(bytes)) => (GPU_LUID, bytes.as_ptr(), no_name),
        GpuRequest::Integrated => (GPU_INTEGRATED, no_luid, no_name),
        GpuRequest::Discrete => (GPU_DISCRETE, no_luid, no_name),
        GpuRequest::Named(text) => match CString::new(text.as_str()) {
            Ok(text) => {
                name = text;
                (GPU_NAME, no_luid, name.as_ptr())
            }
            Err(_) => return Some(format!("the GPU name {text:?} holds a NUL character")),
        },
    };
    let Some(select) = select else {
        return Some(
            "this graphics backend cannot be asked for a GPU (no dsa_gfx_select_gpu); the \
             renderer ranks the GPUs itself"
                .into(),
        );
    };
    match select(rule, luid, name_ptr) {
        0 => None,
        code => Some(format!(
            "the graphics backend refused the GPU request (dsa_gfx_select_gpu returned \
             {code}); the renderer ranks the GPUs itself"
        )),
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

    unsafe fn set_display_callbacks(
        &self,
        callbacks: Option<(DisplayTargetFn, DisplayFrameFn)>,
        context: *mut c_void,
    ) -> Result<(), String> {
        let (target, frame) = match callbacks {
            Some((target, frame)) => (Some(target), Some(frame)),
            None => (None, None),
        };
        let Some(set_display_callbacks) = self.fn_set_display_callbacks else {
            return Err(
                "this graphics backend predates the current task window frames (no \
                 dsa_gfx_set_display_callbacks2); task windows stay empty"
                    .into(),
            );
        };
        match set_display_callbacks(target, frame, context) {
            0 => Ok(()),
            code => Err(format!(
                "the graphics backend cannot present displays (dsa_gfx_set_display_callbacks2 \
                 returned {code}); task windows stay empty"
            )),
        }
    }

    fn adapter_luid(&self) -> Option<[u8; 8]> {
        let get_adapter_luid = self.fn_get_adapter_luid?;
        let mut luid = [0u8; 8];
        // SAFETY: the backend writes at most 8 bytes into `luid`.
        (unsafe { get_adapter_luid(luid.as_mut_ptr()) } == 0).then_some(luid)
    }

    fn gpu(&self) -> Option<RendererGpu> {
        let get_gpu_info = self.fn_get_gpu_info?;
        let mut info = GpuInfo {
            name: [0; 256],
            device_type: 0,
            luid: [0; 8],
            luid_valid: 0,
            preference_matched: -1,
        };
        // SAFETY: `info` has the layout the backend fills.
        if unsafe { get_gpu_info(&mut info) } != 0 {
            return None;
        }
        Some(renderer_gpu(&info))
    }
}

fn renderer_gpu(info: &GpuInfo) -> RendererGpu {
    let len = info
        .name
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.name.len());
    RendererGpu {
        name: String::from_utf8_lossy(&info.name[..len]).into_owned(),
        luid: (info.luid_valid == 1).then_some(Luid(info.luid)),
        matched: match info.preference_matched {
            1 => Some(true),
            0 => Some(false),
            _ => None,
        },
    }
}

struct StreamContext<'a> {
    connection: Mutex<&'a mut dyn Connection>,
    /// Writes go here when the transport can share the connection, so a reply does not
    /// wait for the reader's `recv` to time out.
    sender: Option<Mutex<Box<dyn FrameSender>>>,
    pending: Mutex<Vec<u8>>,
}

impl<'a> StreamContext<'a> {
    fn new(connection: &'a mut dyn Connection) -> Self {
        let sender = connection.try_clone_sender().map(Mutex::new);
        StreamContext {
            connection: Mutex::new(connection),
            sender,
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
    let slice = std::slice::from_raw_parts(buf as *const u8, count);
    let sent = if let Some(sender) = &ctx.sender {
        match sender.lock() {
            Ok(mut sender) => sender.send(slice),
            Err(_) => return -1,
        }
    } else {
        match ctx.connection.lock() {
            Ok(mut conn) => conn.send(slice),
            Err(_) => return -1,
        }
    };
    match sent {
        Ok(()) => count as c_int,
        Err(_) => -1,
    }
}

/// Adapter connecting guest graphics streams to the native gfxstream host renderer.
pub struct GfxstreamAdapter {
    dll_path: Option<PathBuf>,
    backend: Option<Arc<dyn GfxBackend>>,
    /// The sink the backend presents to, boxed so its address stays put while registered.
    sink: Mutex<Option<SinkBox>>,
    /// Why the backend could not be loaded, or the GPU request not passed on.
    notes: Vec<String>,
}

impl GfxstreamAdapter {
    /// Creates a new adapter, searching for `gfxstream_backend.dll` at `custom_path`, beside
    /// the current executable, or in the current directory.
    /// The renderer ranks the GPUs itself.
    pub fn new(custom_path: Option<PathBuf>) -> GfxstreamAdapter {
        Self::on_gpu(custom_path, &GpuRequest::Ranking)
    }

    /// Like [`new`](Self::new), starting the renderer on the GPU `request` asks for.
    pub fn on_gpu(custom_path: Option<PathBuf>, request: &GpuRequest) -> GfxstreamAdapter {
        let dll_path = custom_path.or_else(Self::find_backend_dll);
        let mut notes = Vec::new();
        let backend = if let Some(path) = &dll_path {
            match NativeBackend::load_on(path, request) {
                Ok((b, note)) => {
                    notes.extend(note);
                    Some(Arc::new(b) as Arc<dyn GfxBackend>)
                }
                Err(err) => {
                    notes.push(format!("failed to load gfxstream backend library: {err}"));
                    None
                }
            }
        } else {
            notes.push(format!("{GFXSTREAM_BACKEND_DLL} was not found"));
            None
        };

        GfxstreamAdapter {
            dll_path,
            backend,
            sink: Mutex::new(None),
            notes,
        }
    }

    /// Creates an adapter configured with a custom or fake backend for testing.
    pub fn with_backend(backend: Arc<dyn GfxBackend>) -> GfxstreamAdapter {
        GfxstreamAdapter {
            dll_path: None,
            backend: Some(backend),
            sink: Mutex::new(None),
            notes: Vec::new(),
        }
    }

    /// Why the backend could not be loaded, or the GPU request not passed on, for the log.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// The GPU the renderer runs on, and whether it is the one asked for.
    pub fn renderer_gpu(&self) -> Option<RendererGpu> {
        self.backend.as_ref().and_then(|backend| backend.gpu())
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

impl DisplayPresenter for GfxstreamAdapter {
    fn present_displays(&self, sink: Option<Arc<dyn DisplaySink>>) -> Result<(), String> {
        let Some(backend) = &self.backend else {
            return Err("the graphics backend is not loaded; task windows stay empty".into());
        };
        let mut registered = self.sink.lock().unwrap_or_else(|e| e.into_inner());
        // The old sink stays alive until the backend has stopped calling it.
        // SAFETY: a null context with no callbacks is always valid.
        let stopped = unsafe { backend.set_display_callbacks(None, std::ptr::null_mut()) };
        *registered = None;
        let Some(sink) = sink else {
            return stopped;
        };
        let boxed: SinkBox = Box::new(sink);
        let context = &*boxed as *const Arc<dyn DisplaySink> as *mut c_void;
        // SAFETY: `context` points into `boxed`, which `self.sink` keeps until the callbacks
        // are replaced or removed.
        unsafe {
            backend.set_display_callbacks(
                Some((display_target_trampoline, display_frame_trampoline)),
                context,
            )?;
        }
        *registered = Some(boxed);
        Ok(())
    }

    fn adapter_luid(&self) -> Option<[u8; 8]> {
        self.backend
            .as_ref()
            .and_then(|backend| backend.adapter_luid())
    }
}

impl Drop for GfxstreamAdapter {
    fn drop(&mut self) {
        if let Some(backend) = &self.backend {
            // SAFETY: removing the callbacks is always valid.
            let _ = unsafe { backend.set_display_callbacks(None, std::ptr::null_mut()) };
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

    /// A backend that presents one frame for each display it is told to, as the native
    /// backend does when the guest sets a display's color buffer.
    #[derive(Default)]
    struct PresentingBackend {
        callbacks: Mutex<Option<(DisplayTargetFn, DisplayFrameFn, usize)>>,
    }

    impl PresentingBackend {
        fn post(&self, display_id: u32, width: u32, height: u32) -> Option<isize> {
            self.post_content(display_id, (width, height), (width, height))
        }

        /// A frame of `content` pixels in a ColorBuffer of `buffer` pixels.
        fn post_content(
            &self,
            display_id: u32,
            buffer: (u32, u32),
            content: (u32, u32),
        ) -> Option<isize> {
            let (target, frame, context) = (*self.callbacks.lock().unwrap())?;
            let context = context as *mut c_void;
            let handle = unsafe { target(context, display_id, buffer.0, buffer.1) };
            if handle.is_null() {
                return None;
            }
            unsafe { frame(context, display_id, content.0, content.1) };
            Some(handle as isize)
        }
    }

    impl GfxBackend for PresentingBackend {
        unsafe fn serve_stream(
            &self,
            _: StreamReadFn,
            _: StreamWriteFn,
            _: *mut c_void,
        ) -> Result<(), String> {
            Ok(())
        }
        unsafe fn process_pipe(
            &self,
            _: StreamReadFn,
            _: StreamWriteFn,
            _: *mut c_void,
        ) -> Result<(), String> {
            Ok(())
        }
        fn on_process_create(&self) -> u64 {
            1
        }
        fn on_process_destroy(&self, _: u64) {}
        unsafe fn set_display_callbacks(
            &self,
            callbacks: Option<(DisplayTargetFn, DisplayFrameFn)>,
            context: *mut c_void,
        ) -> Result<(), String> {
            *self.callbacks.lock().unwrap() =
                callbacks.map(|(target, frame)| (target, frame, context as usize));
            Ok(())
        }
        fn adapter_luid(&self) -> Option<[u8; 8]> {
            Some([1, 2, 3, 4, 5, 6, 7, 8])
        }
    }

    /// A sink that shows display 3 only, and records what it was shown.
    #[derive(Default)]
    struct RecordingSink {
        shown: Mutex<Vec<(u32, u32, u32)>>,
        framed: Mutex<Vec<(u32, (u32, u32))>>,
        panic_on: Option<u32>,
    }

    impl DisplaySink for RecordingSink {
        fn target(&self, display_id: u32, width: u32, height: u32) -> Option<isize> {
            if Some(display_id) == self.panic_on {
                panic!("a broken sink");
            }
            self.shown.lock().unwrap().push((display_id, width, height));
            (display_id == 3).then_some(0x1234)
        }
        fn frame(&self, display_id: u32, content: (u32, u32)) {
            self.framed.lock().unwrap().push((display_id, content));
        }
    }

    #[test]
    fn fr3_display_frames_reach_the_sink_and_unrouted_ones_are_dropped() {
        let backend = Arc::new(PresentingBackend::default());
        let adapter = GfxstreamAdapter::with_backend(backend.clone());
        let sink = Arc::new(RecordingSink::default());
        adapter
            .present_displays(Some(sink.clone() as Arc<dyn DisplaySink>))
            .unwrap();
        assert_eq!(
            DisplayPresenter::adapter_luid(&adapter),
            Some([1, 2, 3, 4, 5, 6, 7, 8])
        );

        assert_eq!(backend.post(3, 720, 1280), Some(0x1234));
        assert_eq!(backend.post(0, 1920, 1080), None);
        assert_eq!(
            *sink.shown.lock().unwrap(),
            vec![(3, 720, 1280), (0, 1920, 1080)]
        );
        assert_eq!(*sink.framed.lock().unwrap(), vec![(3, (720, 1280))]);

        adapter.present_displays(None).unwrap();
        assert_eq!(backend.post(3, 720, 1280), None);
    }

    #[test]
    fn fr4_a_frame_reaches_the_sink_with_its_content_size() {
        let backend = Arc::new(PresentingBackend::default());
        let adapter = GfxstreamAdapter::with_backend(backend.clone());
        let sink = Arc::new(RecordingSink::default());
        adapter
            .present_displays(Some(sink.clone() as Arc<dyn DisplaySink>))
            .unwrap();
        // The ColorBuffer stays at the display's size while the task is resized.
        assert_eq!(
            backend.post_content(3, (1920, 1080), (800, 600)),
            Some(0x1234)
        );
        assert_eq!(
            backend.post_content(3, (1920, 1080), (820, 610)),
            Some(0x1234)
        );
        assert_eq!(
            *sink.shown.lock().unwrap(),
            vec![(3, 1920, 1080), (3, 1920, 1080)]
        );
        assert_eq!(
            *sink.framed.lock().unwrap(),
            vec![(3, (800, 600)), (3, (820, 610))]
        );
    }

    #[test]
    fn nfr5_a_panicking_sink_drops_the_frame_instead_of_crossing_into_the_renderer() {
        let backend = Arc::new(PresentingBackend::default());
        let adapter = GfxstreamAdapter::with_backend(backend.clone());
        let sink = Arc::new(RecordingSink {
            panic_on: Some(3),
            ..RecordingSink::default()
        });
        adapter
            .present_displays(Some(sink.clone() as Arc<dyn DisplaySink>))
            .unwrap();
        assert_eq!(backend.post(3, 10, 10), None);
        assert!(sink.framed.lock().unwrap().is_empty());
    }

    #[test]
    fn fr3_without_a_backend_there_is_nothing_to_present() {
        let adapter = GfxstreamAdapter::new(Some(PathBuf::from("nonexistent_path_to_dll.dll")));
        let sink = Arc::new(RecordingSink::default());
        assert!(adapter
            .present_displays(Some(sink as Arc<dyn DisplaySink>))
            .is_err());
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

    /// What the fake `dsa_gfx_select_gpu` was last called with.
    type Selected = (u32, Option<[u8; 8]>, Option<String>);
    static SELECTED: Mutex<Option<Selected>> = Mutex::new(None);

    unsafe extern "C" fn fake_select_gpu(rule: u32, luid: *const u8, name: *const c_char) -> c_int {
        let luid = (!luid.is_null()).then(|| {
            let mut bytes = [0u8; 8];
            std::ptr::copy_nonoverlapping(luid, bytes.as_mut_ptr(), 8);
            bytes
        });
        let name = (!name.is_null()).then(|| CStr::from_ptr(name).to_string_lossy().into_owned());
        *SELECTED.lock().unwrap() = Some((rule, luid, name));
        0
    }

    #[test]
    fn pr3_the_render_gpu_request_reaches_the_backend() {
        let luid = Luid([1, 2, 3, 4, 5, 6, 7, 8]);
        let cases = [
            (GpuRequest::Luid(luid), (GPU_LUID, Some(luid.0), None)),
            (GpuRequest::Integrated, (GPU_INTEGRATED, None, None)),
            (GpuRequest::Discrete, (GPU_DISCRETE, None, None)),
            (
                GpuRequest::Named("RTX".into()),
                (GPU_NAME, None, Some("RTX".to_string())),
            ),
        ];
        for (request, expected) in cases {
            *SELECTED.lock().unwrap() = None;
            assert_eq!(unsafe { select_gpu(Some(fake_select_gpu), &request) }, None);
            assert_eq!(SELECTED.lock().unwrap().clone(), Some(expected));
        }

        // Nothing asked: the backend is not called.
        *SELECTED.lock().unwrap() = None;
        assert_eq!(
            unsafe { select_gpu(Some(fake_select_gpu), &GpuRequest::Ranking) },
            None
        );
        assert_eq!(*SELECTED.lock().unwrap(), None);

        // A backend that cannot be asked says so, for the log.
        let note = unsafe { select_gpu(None, &GpuRequest::Discrete) }.unwrap();
        assert!(note.contains("ranks the GPUs itself"));
    }

    #[test]
    fn pr3_the_renderer_reports_its_gpu_and_whether_it_was_asked_for() {
        let mut info = GpuInfo {
            name: [0; 256],
            device_type: 1,
            luid: [0x8a, 0x4e, 0x01, 0, 0, 0, 0, 0],
            luid_valid: 1,
            preference_matched: 1,
        };
        let name = b"Intel(R) Arc(TM) Graphics";
        info.name[..name.len()].copy_from_slice(name);
        let gpu = renderer_gpu(&info);
        assert_eq!(gpu.name, "Intel(R) Arc(TM) Graphics");
        assert_eq!(gpu.luid, Some(Luid::from_parts(0x0001_4e8a, 0)));
        assert_eq!(gpu.matched, Some(true));

        info.luid_valid = 0;
        info.preference_matched = 0;
        let gpu = renderer_gpu(&info);
        assert_eq!(gpu.luid, None);
        assert_eq!(gpu.matched, Some(false));

        info.preference_matched = -1;
        assert_eq!(renderer_gpu(&info).matched, None);
    }
}
