//! gfxstream's host renderer on macOS (`libgfxstream_backend.dylib`, on Vulkan through
//! MoltenVK), loaded at run time from the app bundle's `Frameworks` folder.
//!
//! The guest's graphics streams are served through it. Each task's frames reach the window
//! manager one of two ways:
//!
//! - On the GPU (`dsa_gfx_set_display_iosurface_callbacks`): the renderer asks the window
//!   manager for an `IOSurface` to put a display's frame in, imports it into Vulkan
//!   (`VK_EXT_metal_objects`), copies the frame into it, and says when it is there. Nothing
//!   passes through memory.
//! - Read back (`dsa_gfx_set_display_pixels_callback`): the frame is read to memory as RGBA
//!   and the window manager writes it into the surface. Used by a renderer built without the
//!   GPU path, or where the GPU path cannot start.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

type StreamRead = extern "C" fn(*mut c_void, *mut c_void, usize) -> c_int;
type StreamWrite = extern "C" fn(*mut c_void, *const c_void, usize) -> c_int;
type PixelsCallback = extern "C" fn(*mut c_void, u32, u32, u32, *const u8);
type TargetCallback = extern "C" fn(*mut c_void, u32, u32, u32) -> *mut c_void;
type CopiedCallback = extern "C" fn(*mut c_void, u32, u32, u32);
type Target = Option<TargetCallback>;
type Copied = Option<CopiedCallback>;
type Pixels = Option<PixelsCallback>;
type SetIosurface = extern "C" fn(Target, Copied, Pixels, *mut c_void) -> c_int;

/// Where the renderer's frames go.
pub trait FrameSink: Send + Sync {
    /// Display `display_id`'s newest frame, read back: `width` by `height` RGBA pixels, rows
    /// from the top without padding.
    fn pixels(&self, display_id: u32, width: u32, height: u32, rgba: &[u8]);
    /// The `IOSurfaceRef` (BGRA, at least `width` by `height`) to copy display `display_id`'s
    /// next frame into, or null to drop the frame. It must stay valid until the next call
    /// for the display returns another one.
    fn target(&self, display_id: u32, width: u32, height: u32) -> *mut c_void;
    /// The frame is in the surface [`FrameSink::target`] gave, filling its top-left `width`
    /// by `height` pixels.
    fn copied(&self, display_id: u32, width: u32, height: u32);
}

/// How the frames reach the window manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePath {
    /// Copied into the window manager's surfaces on the GPU.
    Gpu,
    /// Read back to memory.
    ReadBack,
}

struct Api {
    init: extern "C" fn(u32, u32) -> c_int,
    teardown: extern "C" fn(),
    serve_stream: extern "C" fn(StreamRead, StreamWrite, *mut c_void) -> c_int,
    process_pipe: extern "C" fn(StreamRead, StreamWrite, *mut c_void) -> c_int,
    set_pixels: extern "C" fn(Pixels, *mut c_void) -> c_int,
    set_iosurface: Option<SetIosurface>,
    forget_display: Option<extern "C" fn(u32)>,
    /// Absent from a renderer built before it survived a GPU reset.
    device_generation: Option<extern "C" fn() -> u64>,
    /// Absent from a renderer built before it made a lost device again on a thread of its own.
    device_losses: Option<extern "C" fn() -> u64>,
    simulate_device_loss: Option<extern "C" fn() -> c_int>,
}

/// The loaded renderer.
pub struct Renderer {
    api: Api,
    /// The sink, which the renderer holds as its context. Boxed so that address stays put.
    _sink: Box<Arc<dyn FrameSink>>,
    path: FramePath,
}

// The renderer's entry points are thread-safe; the library is never unloaded.
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

unsafe fn symbol<T: Copy>(library: *mut c_void, name: &str) -> Result<T, String> {
    optional(library, name)
        .ok_or_else(|| format!("the graphics renderer has no {name}; it is older than this host"))
}

unsafe fn optional<T: Copy>(library: *mut c_void, name: &str) -> Option<T> {
    let c_name = CString::new(name).ok()?;
    let found = libc::dlsym(library, c_name.as_ptr());
    if found.is_null() {
        return None;
    }
    Some(std::mem::transmute_copy::<*mut c_void, T>(&found))
}

extern "C" fn read_stream(user: *mut c_void, buffer: *mut c_void, count: usize) -> c_int {
    let stream = unsafe { &mut *(user as *mut &mut dyn ReadWrite) };
    let buffer = unsafe { std::slice::from_raw_parts_mut(buffer as *mut u8, count) };
    match stream.read(buffer) {
        Ok(n) => n.min(c_int::MAX as usize) as c_int,
        Err(_) => -1,
    }
}

extern "C" fn write_stream(user: *mut c_void, buffer: *const c_void, count: usize) -> c_int {
    let stream = unsafe { &mut *(user as *mut &mut dyn ReadWrite) };
    let buffer = unsafe { std::slice::from_raw_parts(buffer as *const u8, count) };
    match stream.write_all(buffer) {
        Ok(()) => count.min(c_int::MAX as usize) as c_int,
        Err(_) => -1,
    }
}

/// The sink the renderer was given as its context.
///
/// # Safety
///
/// `context` must be the boxed sink of a live [`Renderer`].
unsafe fn sink_of<'a>(context: *mut c_void) -> Option<&'a Arc<dyn FrameSink>> {
    (!context.is_null()).then(|| &*(context as *const Arc<dyn FrameSink>))
}

extern "C" fn on_pixels(
    context: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
    rgba: *const u8,
) {
    // A panic must not unwind into the renderer.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let Some(sink) = (unsafe { sink_of(context) }) else {
            return;
        };
        let Some(len) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
        else {
            return;
        };
        if rgba.is_null() || len == 0 {
            return;
        }
        let rgba = unsafe { std::slice::from_raw_parts(rgba, len) };
        sink.pixels(display_id, width, height, rgba);
    }));
}

extern "C" fn on_target(
    context: *mut c_void,
    display_id: u32,
    width: u32,
    height: u32,
) -> *mut c_void {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        match unsafe { sink_of(context) } {
            Some(sink) => sink.target(display_id, width, height),
            None => std::ptr::null_mut(),
        }
    }))
    .unwrap_or(std::ptr::null_mut())
}

extern "C" fn on_copied(context: *mut c_void, display_id: u32, width: u32, height: u32) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(sink) = unsafe { sink_of(context) } {
            sink.copied(display_id, width, height);
        }
    }));
}

/// A stream both ways, as a vsock connection is.
pub trait ReadWrite: Read + Write {}
impl<T: Read + Write> ReadWrite for T {}

impl Renderer {
    /// Loads the renderer at `path`, starts it for a guest display of `display` pixels, and
    /// sends its frames to `sink`, on the GPU when the renderer can.
    pub fn load(
        path: &Path,
        display: (u32, u32),
        sink: Arc<dyn FrameSink>,
    ) -> Result<Renderer, String> {
        let c_path = CString::new(path.as_os_str().to_string_lossy().as_bytes())
            .map_err(|_| "the renderer's path has a NUL in it".to_string())?;
        unsafe {
            let library = libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL);
            if library.is_null() {
                let why = libc::dlerror();
                let why = if why.is_null() {
                    String::from("unknown reason")
                } else {
                    CStr::from_ptr(why as *const c_char)
                        .to_string_lossy()
                        .into_owned()
                };
                return Err(format!(
                    "the graphics renderer at {} could not be loaded: {why}",
                    path.display()
                ));
            }
            let api = Api {
                init: symbol(library, "dsa_gfx_init")?,
                teardown: symbol(library, "dsa_gfx_teardown")?,
                serve_stream: symbol(library, "dsa_gfx_serve_stream")?,
                process_pipe: symbol(library, "dsa_gfx_process_pipe")?,
                set_pixels: symbol(library, "dsa_gfx_set_display_pixels_callback")?,
                set_iosurface: optional(library, "dsa_gfx_set_display_iosurface_callbacks"),
                forget_display: optional(library, "dsa_gfx_forget_display"),
                device_generation: optional(library, "dsa_gfx_device_generation"),
                device_losses: optional(library, "dsa_gfx_device_losses"),
                simulate_device_loss: optional(library, "dsa_gfx_simulate_device_loss"),
            };
            if (api.init)(display.0, display.1) != 0 {
                return Err(
                    "the graphics renderer did not start: this Mac's Vulkan (MoltenVK) may be \
                     missing"
                        .into(),
                );
            }
            let sink: Box<Arc<dyn FrameSink>> = Box::new(sink);
            let context = &*sink as *const Arc<dyn FrameSink> as *mut c_void;
            let gpu = api.set_iosurface.is_some_and(|set| {
                set(Some(on_target), Some(on_copied), Some(on_pixels), context) == 0
            });
            let path = if gpu {
                FramePath::Gpu
            } else if (api.set_pixels)(Some(on_pixels), context) == 0 {
                FramePath::ReadBack
            } else {
                (api.teardown)();
                return Err("the graphics renderer cannot hand frames over on this Mac".into());
            };
            Ok(Renderer {
                api,
                _sink: sink,
                path,
            })
        }
    }

    /// How the frames reach the window manager.
    pub fn frame_path(&self) -> FramePath {
        self.path
    }

    /// Display `display_id` is shown nowhere now: the renderer lets go of the surfaces it
    /// took for it, so their memory goes back.
    pub fn forget_display(&self, display_id: u32) {
        if let Some(forget) = self.api.forget_display {
            forget(display_id);
        }
    }

    /// How many times the renderer found its Vulkan device lost (a GPU reset, or MoltenVK's
    /// device lost with the GPU): it counts up the moment it does, before the device is made
    /// again. 0 when it cannot say.
    pub fn device_losses(&self) -> u64 {
        self.api.device_losses.map_or(0, |losses| losses())
    }

    /// How many times the renderer made its Vulkan device again after a loss. 0 when it
    /// cannot say.
    pub fn device_generation(&self) -> u64 {
        self.api
            .device_generation
            .map_or(0, |generation| generation())
    }

    /// Makes every Vulkan device's queue and fence calls fail as a GPU reset does, so a test
    /// sees the renderer survive one on any driver. False when the renderer cannot.
    pub fn simulate_device_loss(&self) -> bool {
        self.api
            .simulate_device_loss
            .is_some_and(|simulate| simulate() == 0)
    }

    /// Serves one render stream from the guest until it closes.
    pub fn serve_stream(&self, mut stream: &mut dyn ReadWrite) -> Result<(), String> {
        let user = &mut stream as *mut &mut dyn ReadWrite as *mut c_void;
        match (self.api.serve_stream)(read_stream, write_stream, user) {
            0 => Ok(()),
            code => Err(format!("the render stream ended with error {code}")),
        }
    }

    /// Serves one guest process's pipe until it closes.
    pub fn serve_process(&self, mut stream: &mut dyn ReadWrite) -> Result<(), String> {
        let user = &mut stream as *mut &mut dyn ReadWrite as *mut c_void;
        match (self.api.process_pipe)(read_stream, write_stream, user) {
            0 => Ok(()),
            code => Err(format!("the process pipe ended with error {code}")),
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        if let Some(set) = self.api.set_iosurface {
            set(None, None, None, std::ptr::null_mut());
        }
        (self.api.set_pixels)(None, std::ptr::null_mut());
        (self.api.teardown)();
    }
}
