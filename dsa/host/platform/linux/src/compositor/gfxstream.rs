//! gfxstream's host renderer (`libgfxstream_backend.so`), which runs the guest's Vulkan on
//! the host's GPU, and the graphics streams the guest opens to it.
//!
//! The renderer runs in a process of its own, `dsa-gfx` (see `crate::renderer`), which loads
//! this library. The guest opens two kinds of stream over vsock, as on Windows: the command
//! stream (port 5566), and a process pipe (port 5567) for each guest process that draws. The
//! renderer serves each on a thread of its own. Each task's finished frame is copied on the GPU
//! into one of the display's dmabufs, or read back where the driver or the desktop cannot take
//! dmabufs, and handed to a [`RendererFrames`]: in `dsa-gfx`, the connection to the window
//! manager; in the window manager, the windows ([`FrameSink`]).

use std::ffi::{c_char, c_int, c_void, CString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use dsa_host_core::memory::RendererMemory;
use libloading::Library;

use super::frames::{Frame, GpuFrame, GpuImage, LatestFrames};
use crate::compositor::wayland::Wake;

/// The command stream's port.
pub const COMMAND_PORT: u32 = 5566;
/// The process pipe's port.
pub const PROCESS_PIPE_PORT: u32 = 5567;

type ReadFn = unsafe extern "C" fn(*mut c_void, *mut c_void, usize) -> c_int;
type WriteFn = unsafe extern "C" fn(*mut c_void, *const c_void, usize) -> c_int;
type PixelsFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, *const u8);
type DmabufFn =
    unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32, c_int, u32, u32, u32, u64) -> c_int;
type DmabufFrameFn = unsafe extern "C" fn(*mut c_void, u32, u32, u32, u32);
type SetDmabufFn = unsafe extern "C" fn(
    Option<DmabufFn>,
    Option<DmabufFrameFn>,
    Option<PixelsFn>,
    *mut c_void,
) -> c_int;
type SelectGpuFn = unsafe extern "C" fn(u32, *const u8, *const c_char) -> c_int;

/// What the renderer counts of its memory, as `dsa_gfx_memory_stats` lays it out.
#[repr(C)]
#[derive(Default)]
struct MemoryStats {
    device_local_bytes: u64,
    host_visible_bytes: u64,
    imported_bytes: u64,
    largest_bytes: u64,
    device_local_count: u32,
    host_visible_count: u32,
    imported_count: u32,
    guest_processes: i32,
}

type GetMemoryStatsFn = unsafe extern "C" fn(*mut MemoryStats) -> c_int;

/// Where the renderer's frames go, from its presentation thread.
pub trait RendererFrames: Send + Sync {
    /// Whether frames may go on the GPU as dmabufs; when not, every frame is read back.
    fn takes_dmabufs(&self) -> bool;
    /// One GPU image of a display's ring, exported as the dmabuf `fd`. True when it is
    /// taken; false has the frames read back instead.
    fn dmabuf(&self, image: GpuImage, fd: OwnedFd) -> bool;
    /// A frame is in GPU image `frame.slot` of `display_id`.
    fn dmabuf_frame(&self, display_id: u32, frame: GpuFrame);
    /// A frame read back: `width` x `height` RGBA pixels, rows from the top.
    fn pixels(&self, display_id: u32, width: u32, height: u32, rgba: &[u8]);
}

/// The GPU images the renderer exports, each with the dmabuf that carries it.
pub type GpuImages = Arc<Mutex<Vec<(GpuImage, OwnedFd)>>>;

/// Where the frames go in the window manager: the newest frame of each display, the GPU
/// images, and what wakes the main loop.
pub struct FrameSink {
    pub frames: Arc<Mutex<LatestFrames>>,
    pub waker: Arc<dyn Wake>,
    /// Where the GPU images go, when the desktop takes dmabufs; `None` reads every frame
    /// back.
    pub images: Option<GpuImages>,
}

impl RendererFrames for FrameSink {
    fn takes_dmabufs(&self) -> bool {
        self.images.is_some()
    }

    fn dmabuf(&self, image: GpuImage, fd: OwnedFd) -> bool {
        let Some(images) = &self.images else {
            return false;
        };
        match images.lock() {
            Ok(mut images) => images.push((image, fd)),
            Err(_) => return false,
        }
        self.waker.wake();
        true
    }

    fn dmabuf_frame(&self, display_id: u32, frame: GpuFrame) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.put_gpu(display_id, frame);
        }
        self.waker.wake();
    }

    fn pixels(&self, display_id: u32, width: u32, height: u32, rgba: &[u8]) {
        if let Some(frame) = Frame::from_rgba(width, height, rgba) {
            if let Ok(mut frames) = self.frames.lock() {
                frames.put(display_id, frame);
            }
            self.waker.wake();
        }
    }
}

/// The context the backend calls back with: the frames' destination, boxed so its address
/// stays put for as long as the process runs.
type Context = Arc<dyn RendererFrames>;

/// The frames' destination the backend was given as its context.
///
/// # Safety
/// `context` must be the boxed destination `load` leaked, or null.
unsafe fn frames_of<'a>(context: *mut c_void) -> Option<&'a Context> {
    (!context.is_null()).then(|| &*(context as *const Context))
}

// The backend's C callback names every property of the image.
#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn on_dmabuf(
    context: *mut c_void,
    display_id: u32,
    slot: u32,
    width: u32,
    height: u32,
    fd: c_int,
    stride: u32,
    offset: u32,
    fourcc: u32,
    modifier: u64,
) -> c_int {
    if fd < 0 {
        return -1;
    }
    // SAFETY: the backend hands over the descriptor, ours from here on.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: `context` is the destination `load` leaked, which lives as long as the process.
    let Some(frames) = (unsafe { frames_of(context) }) else {
        return -1;
    };
    let image = GpuImage {
        display_id,
        slot,
        width,
        height,
        stride,
        offset,
        fourcc,
        modifier,
    };
    // A panic must not unwind into the renderer.
    let taken = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| frames.dmabuf(image, fd)));
    if taken.unwrap_or(false) {
        0
    } else {
        -1
    }
}

unsafe extern "C" fn on_dmabuf_frame(
    context: *mut c_void,
    display_id: u32,
    slot: u32,
    width: u32,
    height: u32,
) {
    // SAFETY: as in `on_dmabuf`.
    let Some(frames) = (unsafe { frames_of(context) }) else {
        return;
    };
    let frame = GpuFrame {
        slot,
        width,
        height,
    };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        frames.dmabuf_frame(display_id, frame)
    }));
}

unsafe extern "C" fn on_pixels(
    context: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
    rgba: *const u8,
) {
    if rgba.is_null() || width == 0 || height == 0 {
        return;
    }
    // SAFETY: as in `on_dmabuf`.
    let Some(frames) = (unsafe { frames_of(context) }) else {
        return;
    };
    let Some(len) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
    else {
        return;
    };
    // SAFETY: the backend hands `width * height` RGBA pixels, valid during the call.
    let rgba = unsafe { std::slice::from_raw_parts(rgba, len) };
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        frames.pixels(display_id, width, height, rgba)
    }));
}

/// How the frames leave the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePath {
    /// Copied on the GPU into dmabufs, read back only where an image cannot be exported.
    Gpu,
    /// Read back to memory.
    ReadBack,
}

/// The GPU the renderer is asked for, as `dsa_gfx_select_gpu` takes it: the backend's rule
/// (`DSA_GFX_GPU_*`: 2 integrated, 3 discrete, 4 by name) and part of a name for the name rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GpuChoice {
    pub rule: u32,
    pub name: String,
}

/// The loaded renderer.
pub struct Renderer {
    _library: Library,
    serve_stream: unsafe extern "C" fn(ReadFn, WriteFn, *mut c_void) -> c_int,
    process_pipe: unsafe extern "C" fn(ReadFn, WriteFn, *mut c_void) -> c_int,
    forget_display: Option<unsafe extern "C" fn(u32)>,
    memory_stats: Option<GetMemoryStatsFn>,
    /// Absent from a backend built before it survived a GPU reset.
    device_generation: Option<unsafe extern "C" fn() -> u64>,
    /// Absent from a backend built before it made a lost device again on a thread of its own.
    device_losses: Option<unsafe extern "C" fn() -> u64>,
    simulate_device_loss: Option<unsafe extern "C" fn() -> c_int>,
    path: FramePath,
    notes: Vec<String>,
}

/// Where the renderer is: `GFXSTREAM_BACKEND_PATH` when set, else beside this program, else
/// the library path.
pub fn backend_path() -> PathBuf {
    if let Some(path) = std::env::var_os("GFXSTREAM_BACKEND_PATH").filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
    {
        let beside = dir.join("libgfxstream_backend.so");
        if beside.exists() {
            return beside;
        }
    }
    PathBuf::from("libgfxstream_backend.so")
}

/// Asks the backend for `gpu` before it starts. Returns why it could not, if it could not.
///
/// # Safety
/// `select` must be the backend's `dsa_gfx_select_gpu`, called before `dsa_gfx_init`.
unsafe fn select_gpu(select: Option<SelectGpuFn>, gpu: &GpuChoice) -> Option<String> {
    let Some(select) = select else {
        return Some("this renderer cannot be asked for a GPU; it ranks the GPUs itself".into());
    };
    let name = match CString::new(gpu.name.as_str()) {
        Ok(name) => name,
        Err(_) => return Some(format!("the GPU name {:?} holds a NUL character", gpu.name)),
    };
    let name_ptr = if gpu.name.is_empty() {
        std::ptr::null()
    } else {
        name.as_ptr()
    };
    match select(gpu.rule, std::ptr::null(), name_ptr) {
        0 => None,
        code => Some(format!(
            "the renderer refused the render GPU setting (code {code}); it ranks the GPUs itself"
        )),
    }
}

impl Renderer {
    /// Loads the renderer at `path`, asks it for the GPU `gpu` names (none: its own ranking),
    /// starts it, and hands each task's frames to `frames`.
    pub fn load(
        path: &Path,
        gpu: Option<&GpuChoice>,
        frames: Arc<dyn RendererFrames>,
    ) -> Result<Renderer, String> {
        // SAFETY: the renderer's library initialises nothing when loaded.
        let library = unsafe { Library::new(path) }
            .map_err(|error| format!("cannot load {}: {error}", path.display()))?;
        let mut notes = Vec::new();
        // SAFETY: each symbol has the type the backend's header gives it.
        unsafe {
            let init: unsafe extern "C" fn(u32, u32) -> c_int = *library
                .get(b"dsa_gfx_init\0")
                .map_err(|error| format!("the renderer has no dsa_gfx_init: {error}"))?;
            let serve_stream = *library
                .get(b"dsa_gfx_serve_stream\0")
                .map_err(|error| format!("the renderer has no dsa_gfx_serve_stream: {error}"))?;
            let process_pipe = *library
                .get(b"dsa_gfx_process_pipe\0")
                .map_err(|error| format!("the renderer has no dsa_gfx_process_pipe: {error}"))?;
            let set_pixels: unsafe extern "C" fn(Option<PixelsFn>, *mut c_void) -> c_int = *library
                .get(b"dsa_gfx_set_display_pixels_callback\0")
                .map_err(|error| {
                    format!("the renderer cannot hand frames to this host: {error}")
                })?;
            let forget_display: Option<unsafe extern "C" fn(u32)> = library
                .get(b"dsa_gfx_forget_display\0")
                .ok()
                .map(|symbol| *symbol);
            let memory_stats: Option<GetMemoryStatsFn> = library
                .get(b"dsa_gfx_get_memory_stats\0")
                .ok()
                .map(|symbol| *symbol);
            let device_generation: Option<unsafe extern "C" fn() -> u64> = library
                .get(b"dsa_gfx_device_generation\0")
                .ok()
                .map(|symbol| *symbol);
            let device_losses: Option<unsafe extern "C" fn() -> u64> = library
                .get(b"dsa_gfx_device_losses\0")
                .ok()
                .map(|symbol| *symbol);
            let simulate_device_loss: Option<unsafe extern "C" fn() -> c_int> = library
                .get(b"dsa_gfx_simulate_device_loss\0")
                .ok()
                .map(|symbol| *symbol);
            if let Some(gpu) = gpu {
                let select: Option<SelectGpuFn> = library
                    .get(b"dsa_gfx_select_gpu\0")
                    .ok()
                    .map(|symbol| *symbol);
                notes.extend(select_gpu(select, gpu));
            }
            let code = init(1280, 720);
            if code != 0 {
                return Err(format!("the renderer did not start (code {code})"));
            }
            let set_dmabuf: Option<SetDmabufFn> = library
                .get(b"dsa_gfx_set_display_dmabuf_callbacks\0")
                .ok()
                .map(|symbol| *symbol);
            let gpu_frames = frames.takes_dmabufs();
            // The destination lives as long as the process: the backend may call it until the
            // process ends.
            let context = Box::into_raw(Box::new(frames)) as *mut c_void;
            let (code, path) = match set_dmabuf {
                Some(set_dmabuf) if gpu_frames => (
                    set_dmabuf(
                        Some(on_dmabuf),
                        Some(on_dmabuf_frame),
                        Some(on_pixels),
                        context,
                    ),
                    FramePath::Gpu,
                ),
                _ => (set_pixels(Some(on_pixels), context), FramePath::ReadBack),
            };
            if code != 0 {
                drop(Box::from_raw(context as *mut Context));
                return Err(format!("the renderer cannot present frames (code {code})"));
            }
            Ok(Renderer {
                _library: library,
                serve_stream,
                process_pipe,
                forget_display,
                memory_stats,
                device_generation,
                device_losses,
                simulate_device_loss,
                path,
                notes,
            })
        }
    }

    /// How the frames leave the renderer.
    pub fn frame_path(&self) -> FramePath {
        self.path
    }

    /// What the start said that the log should keep.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// No window shows `display_id` now: the renderer lets go of its images.
    pub fn forget_display(&self, display_id: u32) {
        if let Some(forget) = self.forget_display {
            // SAFETY: the backend takes any display ID; one it does not know is ignored.
            unsafe { forget(display_id) }
        }
    }

    /// How many times the renderer found its Vulkan device lost (a GPU reset): it counts up
    /// the moment it does, before the device is made again. 0 when it cannot say.
    pub fn device_losses(&self) -> u64 {
        // SAFETY: takes nothing.
        self.device_losses.map_or(0, |losses| unsafe { losses() })
    }

    /// How many times the renderer made its Vulkan device again after a loss. 0 when it
    /// cannot say.
    pub fn device_generation(&self) -> u64 {
        // SAFETY: takes nothing.
        self.device_generation
            .map_or(0, |generation| unsafe { generation() })
    }

    /// Makes every Vulkan device's queue and fence calls fail as a GPU reset does, so a test
    /// sees the renderer survive one on any driver. False when the renderer cannot.
    pub fn simulate_device_loss(&self) -> bool {
        // SAFETY: takes nothing.
        self.simulate_device_loss
            .is_some_and(|simulate| unsafe { simulate() } == 0)
    }

    /// What the renderer holds, as it counts it; `None` when it cannot say.
    pub fn memory(&self) -> Option<RendererMemory> {
        let stats_of = self.memory_stats?;
        let mut stats = MemoryStats::default();
        // SAFETY: `stats` has the layout the backend fills.
        if unsafe { stats_of(&mut stats) } != 0 {
            return None;
        }
        Some(RendererMemory {
            device_local_kb: stats.device_local_bytes / 1024,
            device_local_count: stats.device_local_count,
            host_visible_kb: stats.host_visible_bytes / 1024,
            host_visible_count: stats.host_visible_count,
            imported_kb: stats.imported_bytes / 1024,
            imported_count: stats.imported_count,
            largest_kb: stats.largest_bytes / 1024,
            guest_processes: stats.guest_processes,
        })
    }

    /// Serves one stream until the guest closes it.
    pub fn serve(&self, port: u32, stream: OwnedFd) {
        let fd = stream.as_raw_fd();
        let mut context = fd;
        let user = (&mut context as *mut RawFd).cast();
        // SAFETY: the callbacks read `user` as the stream's descriptor, which stays open
        // until this returns.
        unsafe {
            if port == PROCESS_PIPE_PORT {
                (self.process_pipe)(read_stream, write_stream, user);
            } else {
                (self.serve_stream)(read_stream, write_stream, user);
            }
        }
        drop(stream);
    }
}

// SAFETY: the renderer's entry points may be called from any thread.
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

/// The backend's read: some bytes, 0 when none came within a moment (it asks again), -1
/// when the stream ended.
unsafe extern "C" fn read_stream(user: *mut c_void, buf: *mut c_void, count: usize) -> c_int {
    if user.is_null() || buf.is_null() || count == 0 {
        return 0;
    }
    // SAFETY: `user` points at the descriptor `serve` keeps.
    let fd = unsafe { *(user as *const RawFd) };
    let mut poll = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let ready = unsafe { libc::poll(&mut poll, 1, 50) };
    if ready == 0 {
        return 0;
    }
    if ready < 0 {
        return if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
            0
        } else {
            -1
        };
    }
    let count = count.min(c_int::MAX as usize);
    // SAFETY: `buf` has room for `count` bytes.
    let read = unsafe { libc::read(fd, buf, count) };
    match read {
        n if n > 0 => n as c_int,
        _ => -1,
    }
}

/// The backend's write: all of `count` bytes, or -1.
unsafe extern "C" fn write_stream(user: *mut c_void, buf: *const c_void, count: usize) -> c_int {
    if user.is_null() {
        return -1;
    }
    if buf.is_null() || count == 0 {
        return 0;
    }
    // SAFETY: `user` points at the descriptor `serve` keeps.
    let fd = unsafe { *(user as *const RawFd) };
    let mut written = 0usize;
    while written < count {
        // SAFETY: writes from within `buf`'s `count` bytes.
        let n = unsafe { libc::write(fd, buf.cast::<u8>().add(written).cast(), count - written) };
        if n <= 0 {
            if n < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return -1;
        }
        written += n as usize;
    }
    count.min(c_int::MAX as usize) as c_int
}

/// How the guest's graphics streams reach this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    /// vsock, from the guest's virtio socket device.
    Vsock,
    /// Unix sockets named `gfx-<port>.sock` in this folder: a virtual machine monitor that
    /// proxies the guest's vsock to Unix sockets (cloud-hypervisor, Firecracker) puts them
    /// there.
    UnixDir(PathBuf),
}

impl Listen {
    /// `DSA_GRAPHICS_SOCKET_DIR` when set, else vsock.
    pub fn from_env() -> Listen {
        match std::env::var_os("DSA_GRAPHICS_SOCKET_DIR") {
            Some(dir) if !dir.is_empty() => Listen::UnixDir(PathBuf::from(dir)),
            _ => Listen::Vsock,
        }
    }
}

/// A listening socket.
pub struct Listener {
    fd: OwnedFd,
}

impl Listener {
    pub fn bind(listen: &Listen, port: u32) -> io::Result<Listener> {
        match listen {
            Listen::Vsock => {
                // SAFETY: a new socket, owned from here on.
                let fd = unsafe {
                    let raw =
                        libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
                    if raw < 0 {
                        return Err(io::Error::last_os_error());
                    }
                    OwnedFd::from_raw_fd(raw)
                };
                // SAFETY: a zeroed sockaddr_vm is valid; the fields are set below.
                let mut address: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
                address.svm_family = libc::AF_VSOCK as libc::sa_family_t;
                address.svm_port = port;
                address.svm_cid = libc::VMADDR_CID_ANY;
                // SAFETY: binds the socket to a whole sockaddr_vm.
                let bound = unsafe {
                    libc::bind(
                        fd.as_raw_fd(),
                        (&address as *const libc::sockaddr_vm).cast(),
                        std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
                    )
                };
                if bound != 0 {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: a bound socket.
                if unsafe { libc::listen(fd.as_raw_fd(), 16) } != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(Listener { fd })
            }
            Listen::UnixDir(dir) => {
                let path = dir.join(format!("gfx-{port}.sock"));
                let _ = std::fs::remove_file(&path);
                let listener = std::os::unix::net::UnixListener::bind(&path)?;
                Ok(Listener {
                    fd: OwnedFd::from(listener),
                })
            }
        }
    }

    /// Waits for the next stream.
    pub fn accept(&self) -> io::Result<OwnedFd> {
        loop {
            // SAFETY: accepts on a listening socket; the new descriptor is owned from here.
            let raw = unsafe {
                libc::accept4(
                    self.fd.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            if raw >= 0 {
                // SAFETY: a new descriptor no one else owns.
                return Ok(unsafe { OwnedFd::from_raw_fd(raw) });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

/// Accepts the guest's streams on `port` for as long as the process runs, serving each on a
/// thread of its own. `log` hears what went wrong.
pub fn serve_port(
    renderer: Arc<Renderer>,
    listen: Listen,
    port: u32,
    log: Arc<dyn Fn(&str) + Send + Sync>,
) {
    std::thread::spawn(move || {
        let listener = match Listener::bind(&listen, port) {
            Ok(listener) => listener,
            Err(error) => {
                log(&format!(
                    "graphics: cannot listen for the guest on port {port}: {error}"
                ));
                return;
            }
        };
        loop {
            match listener.accept() {
                Ok(stream) => {
                    let renderer = renderer.clone();
                    std::thread::spawn(move || renderer.serve(port, stream));
                }
                Err(error) => {
                    log(&format!("graphics: port {port} stopped: {error}"));
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
        }
    });
}
