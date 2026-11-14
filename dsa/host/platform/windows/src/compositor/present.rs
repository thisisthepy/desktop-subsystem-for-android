//! Direct3D 11 and DirectComposition: how a task's frames appear in its window.
//!
//! The window manager makes a [`SharedTexture`] for each task window and the renderer copies
//! the task's frame into it on the GPU. The texture is shared by an NT handle, which the
//! window manager places in the app window host's process; the window host opens it
//! ([`Gpu::open_shared`]) on the same GPU. Each task window there has a DirectComposition
//! visual holding a flip-model swapchain, and [`WindowPresenter::present`] copies the frame
//! into the swapchain and presents it. A frame is never scaled to its window: the window
//! takes the size of the frame it shows (`FrameFollow`). The one exception is a window that
//! holds the size the desktop gave it (maximised, snapped, full screen) while the guest keeps
//! its task smaller, because its display holds no more: the frame is shown in the window's
//! middle, scaled to fit for a task that keeps its density ([`WindowPresenter::set_held`]).
//! No frame passes through the CPU.
//!
//! The texture has the size of the guest's buffers, the largest a frame of the task can be,
//! and each frame fills its top-left region. The swapchain's buffers take the texture's
//! size once, and each frame sets only the swapchain's source size
//! (`IDXGISwapChain2::SetSourceSize`) to the region it fills, so a window resize never
//! resizes the swapchain's buffers. DirectComposition stretches the source region over the
//! whole of the buffers, so the visual scales from the buffers' size back to the frame's own
//! size (`frame_scale`).
//!
//! A frame of the window's size is presented at once. A frame of another size (the app has
//! drawn a size the user dragged to, or the size a drag ended at) is copied into the back
//! buffer but not presented, and the window is told ([`WindowPresenter::on_frame_ready`]). The
//! window takes that size ([`WindowPresenter::size_for`] in `WM_WINDOWPOSCHANGING`), and the
//! frame is presented and the visual committed in its `WM_SIZE`
//! ([`WindowPresenter::view_resized`]), so the new window size and the frame of that size
//! reach the screen together. Until then the window keeps the size of the frame it shows.
//! The back buffer holds the only copy of the kept frame, so between the window choosing its
//! size and taking it, a newer frame waits.
//!
//! Each presenter measures how long its window waited for a frame of its new size
//! (`ResizeLatency`) and logs a summary every 20 resizes and when the window closes, and a
//! warning when a window waits long for a frame of the size asked for.
//!
//! While the input trace is on, each presenter also finds out when its presents reached the
//! screen: it notes each present's count, reads the swapchain's frame statistics at the next
//! present and at each tick, and logs `app.shown` with the vertical blank on the host clock
//! for every present they show to be on the screen (see `PresentWatch`). After a traced press
//! ([`pressed`]) it compares a small square of each frame around the press point with what
//! was there when the press came, and logs `app.visible_change` for the first frame that
//! changed it enough to be seen (see `visible_change`).
//!
//! Whether the trace is on or not, each presenter logs when its window presented its first
//! frame and when that frame was shown, and says both on the window (`window_marks`), where
//! `dsa launch --wait-first-frame` reads them.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::caption::{band_sample, BandSample};
use dsa_host_core::compositor::frame_buffers::{
    buffers_after_frame, content_region, frame_scale, sample_points, swapchain_buffers,
    swapchain_change, uniform_colour, wanted_swapchain_buffers, HiddenBuffers, HiddenFrame,
    SwapchainChange, SAMPLE_GRID,
};
use dsa_host_core::compositor::frame_follow::{Arrival, FrameFollow};
use dsa_host_core::compositor::present_watch::{FrameStatistics, PresentWatch};
use dsa_host_core::compositor::resize_latency::ResizeLatency;
use dsa_host_core::compositor::splash;
use dsa_host_core::compositor::visible_change::{self, area_around, Area, PressWatch, Verdict};
use dsa_host_core::compositor::window_sizing::{
    frame_placement, to_task_length, window_size_of, FramePlacement, Scale,
};
use windows::core::{Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HMODULE, HWND, LUID, POINT};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionSurface, IDCompositionTarget,
    IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIAdapter, IDXGIAdapter3, IDXGIDevice, IDXGIDevice3, IDXGIFactory2,
    IDXGIFactory4, IDXGIResource1, IDXGISwapChain2, DXGI_CREATE_FACTORY_FLAGS,
    DXGI_ERROR_FRAME_STATISTICS_DISJOINT, DXGI_ERROR_WAS_STILL_DRAWING, DXGI_FRAME_STATISTICS,
    DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_PRESENT,
    DXGI_QUERY_VIDEO_MEMORY_INFO, DXGI_SCALING_STRETCH, DXGI_SHARED_RESOURCE_READ,
    DXGI_SHARED_RESOURCE_WRITE, DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG,
    DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Matrix3x2;

use super::device_recovery::{Recovering, Removable};
use super::log::{
    host_clock, input_trace, input_trace_on, note, qpc_frequency, trace_detailed, trace_stage,
    trace_summary, CompositorLogger,
};
use super::window_marks;
use dsa_host_core::resize_trace::millis;

/// How long a present waits for the GPU to finish reading the shared texture.
const COPY_WAIT: Duration = Duration::from_millis(500);

fn failed(what: &str, error: windows::core::Error) -> String {
    format!("{what}: {error}")
}

/// One set of devices: the Direct3D 11 device, and the DirectComposition device every task
/// window of a process shares when the process has windows. A GPU reset removes them, and
/// [`Gpu`] makes a new set.
pub struct Devices {
    device: ID3D11Device,
    context: Mutex<ID3D11DeviceContext>,
    factory: IDXGIFactory2,
    dcomp: Option<Mutex<IDCompositionDevice>>,
    /// Whether the device is on the GPU the renderer named; frames cannot cross GPUs.
    on_renderer_gpu: bool,
}

// SAFETY: the Direct3D 11 device and the DXGI factory are free-threaded. The immediate
// context and the DirectComposition device are used only under their locks.
unsafe impl Send for Devices {}
unsafe impl Sync for Devices {}

impl Removable for Devices {
    fn removed(&self) -> Option<i32> {
        // SAFETY: a plain COM call with no arguments, which the device answers at any time.
        unsafe { self.device.GetDeviceRemovedReason() }
            .err()
            .map(|error| error.code().0)
    }
}

/// The devices a process shows or shares frames with, made again after a GPU reset.
pub struct Gpu {
    devices: Recovering<Devices>,
}

impl Gpu {
    /// Creates the devices on the GPU `luid` names (the renderer's), else on the default
    /// GPU, else on the software rasteriser. `composition` adds the DirectComposition device
    /// that windows need; the window manager, which has none, leaves it out. A GPU reset
    /// makes them again the same way.
    pub fn new(luid: Option<[u8; 8]>, composition: bool) -> Result<Arc<Gpu>, String> {
        let devices = Recovering::new(Box::new(move || Devices::new(luid, composition)))?;
        Ok(Arc::new(Gpu { devices }))
    }

    /// Whether the device is on the GPU the renderer named; frames cannot cross GPUs.
    pub fn on_renderer_gpu(&self) -> bool {
        self.devices.current().0.on_renderer_gpu
    }

    /// How many times the devices were made: 1 until a GPU reset makes them again.
    pub fn generation(&self) -> u64 {
        self.devices.generation()
    }

    /// The current devices and their generation.
    pub fn devices(&self) -> (Arc<Devices>, u64) {
        self.devices.current()
    }

    /// Asks the device whether a GPU reset removed it, and makes new devices if it did.
    /// Returns why it was removed and the new generation when it made them; an error when no
    /// new ones could be made yet, which the next call tries again.
    pub fn recover(&self) -> Result<Option<(i32, u64)>, String> {
        self.devices.recover()
    }

    /// The devices of `generation` failed as removed devices do: new ones are made unless
    /// someone made them already. Returns the generation now current.
    pub fn replace(&self, generation: u64) -> Result<u64, String> {
        self.devices.replace(generation)
    }

    /// Asks the driver to free the memory it keeps for this device beyond the resources that
    /// are alive (its command buffers, caches and pools), as an app does on suspend. Called when
    /// no task window is shown; the driver makes what it needs again on the next copy.
    pub fn trim(&self) {
        self.devices.current().0.trim();
    }

    /// The GPU memory this whole process uses on this device's GPU, in KiB: its dedicated
    /// video memory (local) and the system memory the GPU maps for it (non-local). It counts
    /// every API in the process, so the renderer's Vulkan memory is in it too.
    pub fn video_memory_kb(&self) -> Option<(u64, u64)> {
        self.devices.current().0.video_memory_kb()
    }

    /// A texture the renderer can open by its shared handle and copy a frame into. Made on new
    /// devices when a GPU reset removed the current ones.
    pub fn shared_texture(&self, width: u32, height: u32) -> Result<SharedTexture, String> {
        let (mut texture, generation) = self
            .devices
            .with(|devices| devices.shared_texture(width, height))
            .map_err(|e| failed("creating a shared texture for a task window", e))?;
        texture.generation = generation;
        Ok(texture)
    }

    /// Opens the texture another process shared by `handle`, which is a handle of this
    /// process. The handle stays the caller's to close. Opened on new devices when a GPU
    /// reset removed the current ones.
    pub fn open_shared(&self, handle: isize, size: (u32, u32)) -> Result<OpenedTexture, String> {
        let (texture, generation) = self
            .devices
            .with(|devices| devices.open_shared(handle))
            .map_err(|e| failed("opening a task's shared frame texture", e))?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: a plain COM call writing one local structure.
        unsafe { texture.GetDesc(&mut desc) };
        if (desc.Width, desc.Height) != size {
            return Err(format!(
                "a task's frame texture is {}x{}, not the {}x{} it was announced as",
                desc.Width, desc.Height, size.0, size.1
            ));
        }
        Ok(OpenedTexture {
            texture,
            size,
            generation,
        })
    }
}

impl Devices {
    fn new(luid: Option<[u8; 8]>, composition: bool) -> Result<Devices, String> {
        // SAFETY: plain COM calls; every out pointer is a local.
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory2(DXGI_CREATE_FACTORY_FLAGS(0))
                .map_err(|e| failed("creating the DXGI factory", e))?;
            let adapter: Option<IDXGIAdapter> = luid.and_then(|luid| {
                let luid = LUID {
                    LowPart: u32::from_le_bytes([luid[0], luid[1], luid[2], luid[3]]),
                    HighPart: i32::from_le_bytes([luid[4], luid[5], luid[6], luid[7]]),
                };
                factory
                    .cast::<IDXGIFactory4>()
                    .ok()?
                    .EnumAdapterByLuid::<IDXGIAdapter>(luid)
                    .ok()
            });
            let on_renderer_gpu = adapter.is_some();
            let attempts: Vec<(Option<&IDXGIAdapter>, D3D_DRIVER_TYPE)> = match &adapter {
                Some(adapter) => vec![(Some(adapter), D3D_DRIVER_TYPE_UNKNOWN)],
                None => vec![
                    (None, D3D_DRIVER_TYPE_HARDWARE),
                    (None, D3D_DRIVER_TYPE_WARP),
                ],
            };
            let mut last_error = None;
            for (adapter, driver) in attempts {
                let mut device = None;
                let mut context = None;
                match D3D11CreateDevice(
                    adapter,
                    driver,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                ) {
                    Ok(()) => {
                        let (Some(device), Some(context)) = (device, context) else {
                            continue;
                        };
                        let dcomp = if composition {
                            let dxgi: IDXGIDevice = device
                                .cast()
                                .map_err(|e| failed("reaching the DXGI device", e))?;
                            let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)
                                .map_err(|e| failed("creating the DirectComposition device", e))?;
                            Some(Mutex::new(dcomp))
                        } else {
                            None
                        };
                        return Ok(Devices {
                            device,
                            context: Mutex::new(context),
                            factory,
                            dcomp,
                            on_renderer_gpu,
                        });
                    }
                    Err(error) => last_error = Some(error),
                }
            }
            Err(match last_error {
                Some(error) => failed("creating the Direct3D 11 device", error),
                None => "creating the Direct3D 11 device returned no device".into(),
            })
        }
    }

    fn trim(&self) {
        let context = self.context.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls under the context's lock. Every copy binds nothing, so
        // clearing the context's state loses nothing a later copy needs.
        unsafe {
            context.ClearState();
            context.Flush();
            if let Ok(dxgi) = self.device.cast::<IDXGIDevice3>() {
                dxgi.Trim();
            }
        }
    }

    fn video_memory_kb(&self) -> Option<(u64, u64)> {
        // SAFETY: plain COM calls; the out structures are locals.
        unsafe {
            let adapter: IDXGIAdapter3 = self
                .device
                .cast::<IDXGIDevice>()
                .ok()?
                .GetAdapter()
                .ok()?
                .cast()
                .ok()?;
            let mut local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            let mut non_local = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            adapter
                .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, &mut local)
                .ok()?;
            adapter
                .QueryVideoMemoryInfo(0, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, &mut non_local)
                .ok()?;
            Some((local.CurrentUsage / 1024, non_local.CurrentUsage / 1024))
        }
    }

    fn shared_texture(
        &self,
        width: u32,
        height: u32,
    ) -> Result<SharedTexture, windows::core::Error> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0)
                as u32,
        };
        // SAFETY: plain COM calls; every out pointer is a local.
        unsafe {
            let mut texture = None;
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
            let texture: ID3D11Texture2D =
                texture.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            let resource: IDXGIResource1 = texture.cast()?;
            let handle = resource.CreateSharedHandle(
                None,
                DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                PCWSTR::null(),
            )?;
            Ok(SharedTexture {
                _texture: texture,
                handle,
                size: (width, height),
                generation: 0,
            })
        }
    }

    fn open_shared(&self, handle: isize) -> Result<ID3D11Texture2D, windows::core::Error> {
        // SAFETY: plain COM calls; the handle is only read.
        unsafe {
            let device: ID3D11Device1 = self.device.cast()?;
            device.OpenSharedResource1(HANDLE(handle as *mut c_void))
        }
    }

    fn dcomp(&self) -> Result<&Mutex<IDCompositionDevice>, String> {
        self.dcomp
            .as_ref()
            .ok_or_else(|| "these devices were made without DirectComposition".to_string())
    }
}

/// A texture another process shares, opened in this one: where a task's frames arrive.
pub struct OpenedTexture {
    texture: ID3D11Texture2D,
    size: (u32, u32),
    /// The generation of the devices it was opened on: after a GPU reset it is no use.
    generation: u64,
}

// SAFETY: a Direct3D 11 resource may be used from any thread.
unsafe impl Send for OpenedTexture {}
unsafe impl Sync for OpenedTexture {}

impl OpenedTexture {
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The generation of the devices it was opened on ([`Gpu::generation`]).
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// A texture the renderer copies a task's frame into, and an app window host shows it from,
/// each opening it by its shared NT handle. The handle closes when the last reference goes.
pub struct SharedTexture {
    /// Held so the texture is this process's for as long as its handle is handed out.
    _texture: ID3D11Texture2D,
    handle: HANDLE,
    size: (u32, u32),
    /// The generation of the devices it was made on: after a GPU reset it is made again.
    generation: u64,
}

// SAFETY: a Direct3D 11 resource may be used from any thread; the handle is a kernel handle.
unsafe impl Send for SharedTexture {}
unsafe impl Sync for SharedTexture {}

impl SharedTexture {
    /// The NT handle the texture is shared by, in this process.
    pub fn handle(&self) -> isize {
        self.handle.0 as isize
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The generation of the devices it was made on ([`Gpu::generation`]).
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        // SAFETY: the handle was created for this texture and is closed once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// A texture a frame came in, the texture's size, and the region the frame fills.
type Newest = (ID3D11Texture2D, (u32, u32), (u32, u32));

/// What a visual is committed for: the buffers' size, the frame's and where the window shows
/// the frame.
type Committed = ((u32, u32), (u32, u32), FramePlacement);

struct State {
    /// The devices everything below was made on, and their generation. A GPU reset removes
    /// them; the window then makes all of it again on the new ones (`follow_gpu`).
    devices: Arc<Devices>,
    generation: u64,
    /// The window's DirectComposition target: `None` only while it is made again. Closing
    /// never takes the visual tree off it (`close`).
    target: Option<IDCompositionTarget>,
    /// The frame's visual, under the caption's.
    visual: IDCompositionVisual,
    /// The caption buttons' visual, above the frame's, and its surface and the surface's size.
    caption: IDCompositionVisual,
    caption_surface: Option<(IDCompositionSurface, (u32, u32))>,
    /// The splash's visual, under the frame's: what the window shows until its first frame.
    splash: IDCompositionVisual,
    /// The splash's surface and the surface's size, while it is shown.
    splash_surface: Option<(IDCompositionSurface, (u32, u32))>,
    /// What the splash is drawn from, while it is shown.
    splash_image: Option<SplashImage>,
    swapchain: Option<IDXGISwapChain2>,
    /// The size of the swapchain's buffers: the texture's, not a frame's.
    buffers: Option<(u32, u32)>,
    /// How many buffers the swapchain has, and how many it is to have: three while frames
    /// come, two once the window is still (`wanted_swapchain_buffers`).
    buffer_count: u32,
    wanted_buffers: u32,
    /// When the last frame came.
    last_frame_at: Option<Instant>,
    /// The size of the last frame shown: the swapchain's source size.
    frame: (u32, u32),
    /// The client size last proposed for the window, which the guest is asked for.
    asked: (u32, u32),
    /// What the visual was last committed for: the buffers' size, the frame's and where the
    /// window shows it.
    committed: Option<Committed>,
    /// How large the window shows each of the task's pixels (window pixels per task pixels):
    /// 1:1 unless the task keeps its density on a monitor of another scale. `follow` measures
    /// frames at this scale, so a frame matches the window when it fills it once scaled.
    scale: Scale,
    /// The size, in the task's pixels, of the frame kept in the back buffer for the window
    /// to take.
    kept_region: Option<(u32, u32)>,
    /// Which frame the window shows, which it keeps for the window to take, and its size.
    follow: FrameFollow,
    query: Option<ID3D11Query>,
    /// Where the resize trace reads a frame's sample pixels; made when the trace first looks.
    samples: Option<ID3D11Texture2D>,
    closed: bool,
    /// How long the window waits for a frame of its size after each resize.
    latency: ResizeLatency,
    /// The presents not yet known to be on the screen, while the input trace is on.
    presents: PresentWatch,
    /// The swapchain gives no frame statistics; said once in the log.
    no_statistics: bool,
    /// The present count of the window's first frame once it is presented; 0 when the
    /// swapchain was made again before that frame was shown, so any later present counts.
    first_frame: Option<u32>,
    /// Whether the first frame is known to have reached the screen.
    first_shown: bool,
    /// While the input trace is on: a copy of the frame last presented, on the GPU, and its
    /// size. What a press's area showed when the press came is read from it.
    last_frame: Option<(ID3D11Texture2D, (u32, u32))>,
    /// Where the area around a press is read back to the CPU.
    readback: Option<ID3D11Texture2D>,
    /// The press being watched for its first visible change, and when it came.
    watch: Option<(PressWatch, Duration)>,
    /// Whether the window is minimized, when its swapchain is let go.
    hidden: HiddenBuffers,
    /// The texture the newest frame came in, its size and the region the frame fills: what is
    /// shown again when a minimized window is restored.
    newest: Option<Newest>,
    /// The height of the title bar's band over the frame while it is shown; 0 otherwise.
    band: u32,
    /// Where pixels of the band and of the row under it are copied to be read: a row of
    /// each, `BAND_SAMPLES` wide.
    band_probe: Option<ID3D11Texture2D>,
    /// Whether a copy waits in `band_probe` to be read.
    band_pending: bool,
    /// When the band was last copied.
    band_probed: Option<Instant>,
    /// What the window was last told of the band: its fill and its lightness.
    band_sample: BandSample,
    /// Whether a screenshot waits for the next frame, and what it got once one came.
    capture_wanted: bool,
    captured: Option<Result<CapturedFrame, String>>,
}

/// A frame read back for a screenshot: `width` by `height` pixels as RGBA rows, top row
/// first, and the height of the title bar's band over the frame's top while the window shows
/// one (0 in a window that fills its monitor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub band: u32,
}

/// How long a screenshot of a window whose app is drawing waits for the app's next frame.
const CAPTURE_WAIT: Duration = Duration::from_millis(100);

/// A window whose last frame came longer ago than this has an app that is not drawing, so its
/// newest frame is whole in its texture and is read from there at once.
const CAPTURE_STILL_FOR: Duration = Duration::from_millis(50);

/// The `region` pixels at the top left of `texture`, read back to the CPU as RGBA rows. Waits
/// for the GPU.
///
/// # Safety
///
/// `texture` is a texture of `gpu`'s device; the caller holds the presenter's state lock.
unsafe fn read_frame(
    gpu: &Devices,
    texture: &ID3D11Texture2D,
    (width, height): (u32, u32),
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("the window has no frame to read".into());
    }
    // Made for this one read and let go after it, so a screenshot keeps no memory.
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    let mut staging = None;
    gpu.device
        .CreateTexture2D(&desc, None, Some(&mut staging))
        .map_err(|e| failed("creating the texture a screenshot is read into", e))?;
    let staging = staging.ok_or("creating the texture a screenshot is read into gave none")?;
    let context = gpu.context.lock().unwrap_or_else(|e| e.into_inner());
    let whole = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: width,
        bottom: height,
        back: 1,
    };
    context.CopySubresourceRegion(&staging, 0, 0, 0, 0, texture, 0, Some(&whole as *const _));
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    context
        .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        .map_err(|e| failed("reading a screenshot", e))?;
    let row = (width * 4) as usize;
    let mut pixels = Vec::with_capacity(row * height as usize);
    for y in 0..height as usize {
        let start = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
        pixels.extend_from_slice(std::slice::from_raw_parts(start, row));
    }
    context.Unmap(&staging, 0);
    Ok(pixels)
}

/// What a window's splash is drawn from: its background as `0xAARRGGBB`, the app's icon as
/// rows of premultiplied BGRA pixels and their size, and the monitor's dots per inch.
pub struct SplashImage {
    pub background: u32,
    pub icon: Option<(Vec<u8>, (u32, u32))>,
    pub dpi: u32,
}

/// A press a window has to watch: the point in guest pixels and when it came on the host clock.
type Press = ((f32, f32), Duration);

/// The client size a frame of `region` task pixels fills at the window's scale. A frame of the
/// task size the window's size, or the size last asked for, was turned into is that size
/// exactly, though rounding it back at the scale can land a pixel off.
fn frame_in_window(state: &State, region: (u32, u32)) -> (u32, u32) {
    window_size_of(region, state.scale, &[state.follow.view(), state.asked])
}

/// The presses each task's window has to watch, by task. Filled from the window's thread, taken
/// by the presenter.
fn presses() -> &'static Mutex<HashMap<i32, Press>> {
    static PRESSES: OnceLock<Mutex<HashMap<i32, Press>>> = OnceLock::new();
    PRESSES.get_or_init(Mutex::default)
}

/// An area of a frame as the input trace writes it: `left,top,width x height`.
fn describe_area(area: Area) -> String {
    format!(
        "{},{},{}x{}",
        area.left,
        area.top,
        area.width(),
        area.height()
    )
}

/// A traced press came at `point`, in guest pixels, in the window of task `task_id`: its next
/// frames are compared with what was on the screen until one visibly changes the area around
/// the point. A press that comes before the last one's change was found replaces it.
pub fn pressed(task_id: i32, point: (f32, f32)) {
    presses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(task_id, (point, host_clock()));
}

/// How many resizes go between two summaries of their waits in the log.
const SUMMARY_EVERY: usize = 20;

/// The longest a frame waits for the window to take the size of the frame kept before it.
/// Windows applies a size within the message that proposed it, so this is only reached when
/// the window never takes it.
const CLAIM_WAIT: Duration = Duration::from_millis(100);

/// A task window's DirectComposition target and its visuals: the frame's, the caption
/// buttons' above it and the splash's under it.
type Composition = (
    IDCompositionTarget,
    IDCompositionVisual,
    IDCompositionVisual,
    IDCompositionVisual,
);

/// Puts the visuals of a task window on the window `hwnd`, on `devices`. The window must have
/// no target on any other DirectComposition device.
fn compose_window(devices: &Devices, hwnd: isize) -> Result<Composition, String> {
    let dcomp = devices.dcomp()?.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: `hwnd` is a live window of this process; the rest are plain COM calls.
    unsafe {
        let target = dcomp
            .CreateTargetForHwnd(HWND(hwnd as *mut c_void), true)
            .map_err(|e| failed("attaching DirectComposition to a task window", e))?;
        let root = dcomp
            .CreateVisual()
            .map_err(|e| failed("creating a task window's visual", e))?;
        let visual = dcomp
            .CreateVisual()
            .map_err(|e| failed("creating a task window's visual", e))?;
        // The caption buttons are drawn over the app, whose content runs under them.
        let caption = dcomp
            .CreateVisual()
            .map_err(|e| failed("creating a task window's caption visual", e))?;
        root.AddVisual(&visual, false, None::<&IDCompositionVisual>)
            .map_err(|e| failed("adding a task window's frame visual", e))?;
        root.AddVisual(&caption, true, &visual)
            .map_err(|e| failed("adding a task window's caption visual", e))?;
        // Until the first frame the window shows a splash under where the frame goes.
        let splash = dcomp
            .CreateVisual()
            .map_err(|e| failed("creating a task window's splash visual", e))?;
        root.AddVisual(&splash, false, &visual)
            .map_err(|e| failed("adding a task window's splash visual", e))?;
        target
            .SetRoot(&root)
            .map_err(|e| failed("setting a task window's visual", e))?;
        dcomp
            .Commit()
            .map_err(|e| failed("committing a task window's visual", e))?;
        Ok((target, visual, caption, splash))
    }
}

/// Tells a window that a frame of another size is kept for it to take.
pub type FrameReady = Box<dyn Fn() + Send + Sync>;

/// Tells a window what the app's frame shows under its title bar: the color the band should
/// be filled with (0xAARRGGBB), `None` when the app draws the band itself, and how light the
/// band is, which the caption glyphs' tone follows.
pub type BandColour = Box<dyn Fn(BandSample) + Send + Sync>;

/// How many pixels are sampled across the band, and across the row under it.
const BAND_SAMPLES: u32 = 8;

/// How often the band is sampled at most, while frames come.
const BAND_PROBE_EVERY: Duration = Duration::from_millis(200);

/// Shows one task's frames in its window.
pub struct WindowPresenter {
    task_id: i32,
    /// The window, which says when its first frame was presented and shown.
    hwnd: isize,
    gpu: Arc<Gpu>,
    state: Mutex<State>,
    /// Wakes a frame waiting for the window to take the size of the frame kept before it.
    claim_released: Condvar,
    /// Tells the window a frame of another size is kept for it.
    frame_ready: Mutex<Option<FrameReady>>,
    /// Tells the window what to fill its title bar's band with.
    band_colour: Mutex<Option<BandColour>>,
    /// Wakes a screenshot waiting for the next frame.
    capture_done: Condvar,
    logger: Option<Arc<CompositorLogger>>,
}

// SAFETY: every COM object in the state is used under the state's lock, and the
// DirectComposition device under its own.
unsafe impl Send for WindowPresenter {}
unsafe impl Sync for WindowPresenter {}

impl WindowPresenter {
    /// Puts a visual on the window `hwnd` of task `task_id`, whose client area is `view`
    /// pixels. Called on the thread that owns the window.
    pub fn new(
        task_id: i32,
        gpu: Arc<Gpu>,
        hwnd: isize,
        view: (u32, u32),
        logger: Option<Arc<CompositorLogger>>,
    ) -> Result<Arc<Self>, String> {
        let (devices, generation) = gpu.devices();
        let (target, visual, caption, splash) = compose_window(&devices, hwnd)?;
        Ok(Arc::new(WindowPresenter {
            task_id,
            hwnd,
            gpu,
            state: Mutex::new(State {
                devices,
                generation,
                target: Some(target),
                visual,
                caption,
                caption_surface: None,
                splash,
                splash_surface: None,
                splash_image: None,
                swapchain: None,
                buffers: None,
                buffer_count: 0,
                wanted_buffers: swapchain_buffers(),
                last_frame_at: None,
                frame: (0, 0),
                asked: view,
                committed: None,
                scale: (1, 1),
                kept_region: None,
                follow: FrameFollow::new(view),
                query: None,
                samples: None,
                closed: false,
                latency: ResizeLatency::new(),
                presents: PresentWatch::new(),
                no_statistics: false,
                first_frame: None,
                first_shown: false,
                last_frame: None,
                readback: None,
                watch: None,
                hidden: HiddenBuffers::new(),
                newest: None,
                band: 0,
                band_probe: None,
                band_pending: false,
                band_probed: None,
                band_sample: BandSample::default(),
                capture_wanted: false,
                captured: None,
            }),
            claim_released: Condvar::new(),
            frame_ready: Mutex::new(None),
            band_colour: Mutex::new(None),
            capture_done: Condvar::new(),
            logger,
        }))
    }

    /// Has `ready` called, from the thread that shows frames, whenever a frame of another size
    /// than the window's is kept for the window to take.
    pub fn on_frame_ready(&self, ready: FrameReady) {
        *self.frame_ready.lock().unwrap_or_else(|e| e.into_inner()) = Some(ready);
    }

    /// Has `told` called, from the thread that shows frames, whenever what the app's frames
    /// show under the title bar changes: the color the band should be filled with (the app's
    /// top color while the app's frames leave the band empty or black, `None` once the app
    /// draws the band itself), and how light the band is.
    pub fn on_band_colour(&self, told: BandColour) {
        *self.band_colour.lock().unwrap_or_else(|e| e.into_inner()) = Some(told);
    }

    /// Samples the band and the row under it from the frame in `texture` (of `region`
    /// pixels), at most every [`BAND_PROBE_EVERY`]. The copy is read on a later frame, once
    /// the GPU has made it, so showing a frame never waits for it. Returns the band's new
    /// sample when it changed.
    unsafe fn probe_band(
        &self,
        state: &mut State,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
    ) -> Option<BandSample> {
        let devices = Arc::clone(&state.devices);
        // The band is the window's; the frame is sampled in its own pixels.
        let band = to_task_length(state.band, state.scale);
        if band == 0 || region.1 <= band + 2 || region.0 < BAND_SAMPLES {
            return None;
        }
        let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = None;
        if state.band_pending {
            let probe = state.band_probe.clone()?;
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            let flags = D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32;
            match context.Map(&probe, 0, D3D11_MAP_READ, flags, Some(&mut mapped)) {
                Ok(()) => {
                    let base = mapped.pData as *const u8;
                    let pitch = mapped.RowPitch as usize;
                    let row = |index: usize| -> Vec<u32> {
                        (0..BAND_SAMPLES as usize)
                            .map(|column| {
                                let at = base.add(index * pitch + column * 4);
                                let pixel = std::slice::from_raw_parts(at, 4);
                                // RGBA bytes, as 0xAARRGGBB.
                                u32::from_be_bytes([pixel[3], pixel[0], pixel[1], pixel[2]])
                            })
                            .collect()
                    };
                    let (inside, below) = (row(0), row(1));
                    context.Unmap(&probe, 0);
                    state.band_pending = false;
                    let sample = band_sample(&inside, &below);
                    if sample != state.band_sample {
                        state.band_sample = sample;
                        changed = Some(sample);
                    }
                }
                // The GPU has not made the copy yet: it is read on a later frame.
                Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => return None,
                Err(_) => state.band_pending = false,
            }
        }
        if state
            .band_probed
            .is_some_and(|at| at.elapsed() < BAND_PROBE_EVERY)
        {
            return changed;
        }
        if state.band_probe.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: BAND_SAMPLES,
                Height: 2,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let made = devices
                .device
                .CreateTexture2D(&desc, None, Some(&mut state.band_probe));
            if let Err(error) = made {
                let what = "creating the texture a title bar's band is sampled into";
                note(self.logger.as_deref(), &failed(what, error));
                // Not tried again on every frame.
                state.band_probed = Some(Instant::now());
                return changed;
            }
        }
        let Some(probe) = state.band_probe.clone() else {
            return changed;
        };
        // Across the band's middle row, and the row just under it, where the app's top begins.
        let rows = [band / 2, band + 2];
        for (index, y) in rows.into_iter().enumerate() {
            for column in 0..BAND_SAMPLES {
                let x = (2 * column + 1) * region.0 / (2 * BAND_SAMPLES);
                let pixel = D3D11_BOX {
                    left: x,
                    top: y,
                    front: 0,
                    right: x + 1,
                    bottom: y + 1,
                    back: 1,
                };
                context.CopySubresourceRegion(
                    &probe,
                    0,
                    column,
                    index as u32,
                    0,
                    texture,
                    0,
                    Some(&pixel as *const _),
                );
            }
        }
        state.band_pending = true;
        state.band_probed = Some(Instant::now());
        changed
    }

    /// The size of the frame kept for the window to take, if one is.
    pub fn kept_size(&self) -> Option<(u32, u32)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.follow.kept()
    }

    /// Something proposes a client area of `proposed` pixels for the window (a step of a drag,
    /// a maximise, a move to another monitor), and whoever calls this has asked the guest for
    /// it. Returns the client size the window takes instead: the kept frame's, or the size of
    /// the frame it shows, so the window never shows a frame of another size. `None` before
    /// any frame, when the window takes what is proposed.
    pub fn size_for(&self, proposed: (u32, u32)) -> Option<(u32, u32)> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return None;
        }
        let taken = state.follow.size_for(proposed);
        if taken.is_some_and(|taken| taken != proposed) && proposed != state.asked {
            state.asked = proposed;
            self.size_asked(&mut state, proposed);
        }
        taken
    }

    /// The window's monitor changed scale and the window takes a client area of `proposed`
    /// pixels there at once, the size the desktop suggests, and whoever calls this has asked
    /// the guest for it. Until a frame of it comes, the window shows its last frame 1:1, and
    /// the frames the app still draws at the old size are not shown and do not move the
    /// window back: that would put it on the other monitor again.
    pub fn scale_changed(&self, proposed: (u32, u32)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        state.follow.scale_changed(proposed);
        if proposed != state.asked {
            state.asked = proposed;
            self.size_asked(&mut state, proposed);
        }
        drop(state);
        // A frame that waited for a claim the change of scale let go goes on.
        self.claim_released.notify_all();
    }

    /// The window shows each of its task's pixels at `scale` (window pixels per task pixels)
    /// from now on: the task keeps its density on a monitor of another scale, and is shown the
    /// same size to the eye. The newest frame is shown again at once at the new scale, since a
    /// task that keeps its density and size may draw nothing new for the move.
    pub fn set_scale(&self, scale: Scale) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.scale == scale {
            return;
        }
        state.scale = scale;
        let Some((texture, size, region)) = state.newest.clone() else {
            return;
        };
        if state.hidden.frame() == HiddenFrame::Hold {
            return;
        }
        let shown = frame_in_window(&state, region);
        let keep = match state.follow.frame_arrived(shown) {
            Arrival::Show => false,
            Arrival::Keep => true,
            // The next frame is followed as any other.
            Arrival::Wait | Arrival::Stale => {
                self.commit_visual(&mut state);
                return;
            }
        };
        if keep {
            state.kept_region = Some(region);
        }
        // Presented with the visual's new scale, unless it is kept for the window to take.
        if let Err(why) = self.present_locked(&mut state, &texture, size, region, keep) {
            note(self.logger.as_deref(), &why);
            self.recover(&mut state);
            return;
        }
        drop(state);
        if keep {
            if let Some(ready) = self
                .frame_ready
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                ready();
            }
        }
    }

    /// The client area is now `view` pixels. The frame kept for that size is presented now,
    /// together with the visual's new scale. Returns once the compositor has taken it, so a
    /// live resize shows it with the new window size.
    pub fn view_resized(&self, view: (u32, u32)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        if state.first_frame.is_none() && state.splash_image.is_some() {
            if let Err(why) = self.draw_splash(&mut state, view) {
                note(self.logger.as_deref(), &why);
            }
        }
        let took_frame = state.follow.view_taken(view);
        if took_frame {
            let started = Instant::now();
            // The kept frame fills the window at its scale; the back buffer holds it at its own
            // size.
            let region = state.kept_region.take().unwrap_or(view);
            // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
            // under its lock.
            match unsafe { self.present_back_buffer(&mut state, region) } {
                Ok(new_size) => self.frame_shown(&mut state, view, new_size, None, started),
                Err(why) => {
                    note(self.logger.as_deref(), &why);
                    self.recover(&mut state);
                }
            }
        }
        let committed = self.commit_visual(&mut state);
        drop(state);
        self.claim_released.notify_all();
        if committed {
            self.wait_for_commit();
        }
        if took_frame {
            // The window's new size and its frame were handed over on this call stack, inside
            // the SetWindowPos that changed the size. Waiting here until the desktop has
            // composed them keeps the next step of the drag from queueing a newer size before
            // this one is on the screen, which could show an older frame in a newer rectangle.
            // SAFETY: a plain call with no arguments; it fails harmlessly when the desktop
            // does not compose.
            unsafe {
                let _ = windows_sys::Win32::Graphics::Dwm::DwmFlush();
            }
        }
    }

    /// The window holds the size the desktop gives it (`true`: it is maximised, snapped or
    /// full screen), or takes the size of its frames again. While it holds its size, it takes
    /// every size proposed at once and shows each frame in its middle, scaled to fit for a task
    /// that keeps its density, rather than shrink to a frame the guest keeps smaller.
    pub fn set_held(&self, held: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.follow.held() == held {
            return;
        }
        state.follow.set_held(held);
        // A frame kept for the window to take is shown now, in the window as it is.
        if let Some(region) = state.kept_region.take().filter(|_| held) {
            let started = Instant::now();
            // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
            // under its lock.
            match unsafe { self.present_back_buffer(&mut state, region) } {
                Ok(new_size) => {
                    let in_window = frame_in_window(&state, region);
                    self.frame_shown(&mut state, in_window, new_size, None, started);
                }
                Err(why) => {
                    note(self.logger.as_deref(), &why);
                    self.recover(&mut state);
                }
            }
        }
        self.commit_visual(&mut state);
        drop(state);
        // A frame that waited for a claim the hold let go goes on.
        self.claim_released.notify_all();
    }

    /// The window's position is settled. A size it chose and did not take (Windows did not
    /// apply it) no longer holds frames back.
    pub fn release_claim(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.follow.release();
        drop(state);
        self.claim_released.notify_all();
    }

    /// The guest is being asked for `view`: where the wait for a frame of that size starts.
    fn size_asked(&self, state: &mut State, view: (u32, u32)) {
        state.latency.view_changed(view, Instant::now());
        trace_stage(
            self.logger.as_deref(),
            self.task_id,
            "app.view",
            view,
            &format!("frame={}x{}", state.frame.0, state.frame.1),
        );
    }

    /// Waits until the compositor has taken the last commit. Holds no lock of the presenter's
    /// state, so the connection thread takes frames meanwhile.
    fn wait_for_commit(&self) {
        let (devices, _) = self.gpu.devices();
        let Ok(dcomp) = devices.dcomp() else {
            return;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: a plain COM call on the device, under its lock.
        unsafe {
            let _ = dcomp.WaitForCommitCompletion();
        }
    }

    /// Asks the device whether a GPU reset removed it, makes new devices if it did, and moves
    /// the window onto the current devices. Called after anything on the devices failed, and
    /// at every tick, so a window whose app draws nothing recovers too.
    fn recover(&self, state: &mut State) {
        match self.gpu.recover() {
            Ok(Some((reason, generation))) => note(
                self.logger.as_deref(),
                &format!(
                    "task {}: the GPU was reset (0x{:08x}); made new Direct3D devices \
                     (number {generation}), and the window stays",
                    self.task_id, reason as u32
                ),
            ),
            Ok(None) => {}
            Err(why) => note(
                self.logger.as_deref(),
                &format!(
                    "task {}: the GPU was reset, and new Direct3D devices cannot be made yet \
                     ({why}); tried again shortly",
                    self.task_id
                ),
            ),
        }
        self.follow_gpu(state);
    }

    /// Moves the window onto the current devices when a GPU reset replaced the ones its
    /// composition was made on: the target, visuals and surfaces are made again on the new
    /// ones, and the swapchain with the next frame. The window, its size and its task stay;
    /// the caption buttons and the splash come back when they are next drawn, and the frame
    /// with the next one the app draws.
    fn follow_gpu(&self, state: &mut State) {
        let (devices, generation) = self.gpu.devices();
        if generation == state.generation {
            return;
        }
        // A window takes one target at a time: the old one goes first.
        state.target = None;
        state.swapchain = None;
        state.buffers = None;
        state.buffer_count = 0;
        state.committed = None;
        state.frame = (0, 0);
        state.caption_surface = None;
        state.splash_surface = None;
        state.query = None;
        state.samples = None;
        state.last_frame = None;
        state.readback = None;
        state.watch = None;
        state.band_probe = None;
        state.band_pending = false;
        // Made on the old devices: nothing can be copied from it now. The next frame comes
        // in a texture opened on the new ones.
        state.newest = None;
        state.kept_region = None;
        state.follow.release();
        state.presents.forget();
        if !state.first_shown && state.first_frame.is_some() {
            state.first_frame = Some(0);
        }
        state.devices = Arc::clone(&devices);
        state.generation = generation;
        match compose_window(&devices, self.hwnd) {
            Ok((target, visual, caption, splash)) => {
                state.target = Some(target);
                state.visual = visual;
                state.caption = caption;
                state.splash = splash;
                note(
                    self.logger.as_deref(),
                    &format!(
                        "task {}: the window moved to the new Direct3D devices; it shows the \
                         app's next frame",
                        self.task_id
                    ),
                );
                if state.first_frame.is_none() && state.splash_image.is_some() {
                    let view = state.asked;
                    if let Err(why) = self.draw_splash(state, view) {
                        note(self.logger.as_deref(), &why);
                    }
                }
            }
            Err(why) => {
                // Tried again at the next tick, on whatever devices are current then.
                state.generation = 0;
                note(
                    self.logger.as_deref(),
                    &format!(
                        "task {}: the window could not move to the new Direct3D devices: {why}",
                        self.task_id
                    ),
                );
            }
        }
        // A frame waiting for the window to take a size would wait for nothing now.
        self.claim_released.notify_all();
    }

    /// Time has passed: a window that waits long for a frame of the size asked for is logged,
    /// once per size, and a window whose devices a GPU reset removed moves to new ones.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        self.recover(&mut state);
        self.trace_shown(&mut state);
        self.release_idle_buffer(&mut state);
        if let Some(((width, height), waited)) = state.latency.stuck(Instant::now()) {
            note(
                self.logger.as_deref(),
                &format!(
                    "a task window keeps its size: no {width}x{height} frame came in the {} ms \
                     since it was asked for that size",
                    waited.as_millis()
                ),
            );
        }
    }

    /// A shown window that has had no frame for a while gives its swapchain's third buffer
    /// back: the swapchain is resized to two buffers and the newest frame shown again from the
    /// shared texture, since resizing drops what the buffers held. The next frame makes the
    /// third buffer again. A window that keeps a frame for a size it is about to take is left
    /// alone, so the kept frame is not lost.
    fn release_idle_buffer(&self, state: &mut State) {
        if state.hidden.hidden() || state.swapchain.is_none() || state.follow.kept().is_some() {
            return;
        }
        let Some(quiet) = state.last_frame_at.map(|at| at.elapsed()) else {
            return;
        };
        let wanted = wanted_swapchain_buffers(quiet);
        if wanted >= state.buffer_count {
            return;
        }
        let Some((texture, size, region)) = state.newest.clone() else {
            return;
        };
        state.wanted_buffers = wanted;
        if let Err(why) = self.present_locked(state, &texture, size, region, false) {
            note(self.logger.as_deref(), &why);
            self.recover(state);
        }
    }

    /// Takes the caption buttons out of sight until the next [`show_caption`](Self::show_caption),
    /// as in a window that fills its monitor and has no title bar.
    pub fn hide_caption(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let devices = Arc::clone(&state.devices);
        if state.closed {
            return;
        }
        // No band, nothing to sample under it.
        state.band = 0;
        let Ok(dcomp) = devices.dcomp() else {
            return;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.caption.SetOffsetY2(-1.0e6);
            let _ = dcomp.Commit();
        }
    }

    /// Shows the caption buttons: `pixels`, rows of premultiplied BGRA `size` pixels
    /// large, with their left edge `left` client pixels from the window's. They stay until
    /// the next call, over every frame.
    pub fn show_caption(&self, left: i32, size: (u32, u32), pixels: &[u8]) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let devices = Arc::clone(&state.devices);
        if state.closed || size.0 == 0 || size.1 == 0 {
            return Ok(());
        }
        if pixels.len() < (size.0 * size.1 * 4) as usize {
            return Err("the caption image is smaller than its size".into());
        }
        state.band = size.1;
        let dcomp = devices.dcomp()?.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns; the update reads only
        // `pixels`, which holds the whole image.
        unsafe {
            if state.caption_surface.as_ref().map(|(_, at)| *at) != Some(size) {
                let surface = dcomp
                    .CreateSurface(
                        size.0,
                        size.1,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        DXGI_ALPHA_MODE_PREMULTIPLIED,
                    )
                    .map_err(|e| failed("creating a task window's caption surface", e))?;
                state
                    .caption
                    .SetContent(&surface)
                    .map_err(|e| failed("showing a task window's caption", e))?;
                state.caption_surface = Some((surface, size));
            }
            let Some((surface, _)) = state.caption_surface.as_ref() else {
                return Ok(());
            };
            let mut offset = POINT::default();
            let texture: ID3D11Texture2D = surface
                .BeginDraw(None, &mut offset)
                .map_err(|e| failed("drawing a task window's caption", e))?;
            let place = D3D11_BOX {
                left: offset.x as u32,
                top: offset.y as u32,
                front: 0,
                right: offset.x as u32 + size.0,
                bottom: offset.y as u32 + size.1,
                back: 1,
            };
            {
                let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
                context.UpdateSubresource(
                    &texture,
                    0,
                    Some(&place as *const D3D11_BOX),
                    pixels.as_ptr() as *const c_void,
                    size.0 * 4,
                    0,
                );
            }
            surface
                .EndDraw()
                .map_err(|e| failed("drawing a task window's caption", e))?;
            let _ = state.caption.SetOffsetX2(left as f32);
            let _ = state.caption.SetOffsetY2(0.0);
            dcomp
                .Commit()
                .map_err(|e| failed("committing a task window's caption", e))?;
        }
        Ok(())
    }

    /// The window was minimized. Its swapchain's buffers, and the trace's copies, are let go:
    /// a minimized window shows nothing, and they hold tens of MiB of the task's full size.
    /// Frames that arrive meanwhile stay in the shared texture.
    pub fn hide(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let devices = Arc::clone(&state.devices);
        if state.closed || !state.hidden.hide() {
            return;
        }
        if let Ok(dcomp) = devices.dcomp() {
            let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: plain COM calls on objects this presenter owns.
            unsafe {
                let _ = state.visual.SetContent(None::<&windows::core::IUnknown>);
                let _ = dcomp.Commit();
            }
        }
        state.swapchain = None;
        state.buffers = None;
        state.buffer_count = 0;
        state.committed = None;
        state.frame = (0, 0);
        state.presents.forget();
        state.last_frame = None;
        state.readback = None;
        state.samples = None;
        if !state.first_shown && state.first_frame.is_some() {
            // The first frame's present went with the swapchain: the next one shown stands in.
            state.first_frame = Some(0);
        }
        drop(state);
        // A frame waiting for the window to take a size would wait for nothing now.
        self.claim_released.notify_all();
    }

    /// The window was restored. The swapchain is made again and the newest frame shown at
    /// once from the shared texture, so the window does not come back empty while the app
    /// draws its next one.
    pub fn show(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || !state.hidden.show() {
            return;
        }
        let Some((texture, size, region)) = state.newest.clone() else {
            return;
        };
        if let Err(why) = self.present_locked(&mut state, &texture, size, region, false) {
            note(self.logger.as_deref(), &why);
            self.recover(&mut state);
        }
    }

    /// The window is closing: nothing more is shown in it. The frame it shows stays in it,
    /// so the close animation Windows plays when the window is destroyed shows the window
    /// whole, with its content, and not an empty frame. [`WindowPresenter::release`] lets
    /// the content go once the window is gone.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.closed {
            if let Some(summary) = state.latency.summary() {
                note(
                    self.logger.as_deref(),
                    &format!("a task window closed after {summary}"),
                );
            }
        }
        state.closed = true;
        drop(state);
        // A screenshot waiting for a frame gets none, and a frame waiting for the window to
        // take its size waits for nothing.
        self.capture_done.notify_all();
        self.claim_released.notify_all();
    }

    /// The window is gone: what it showed is let go. Nothing is committed to the window's
    /// visual tree, which went with the window; an empty tree committed now could reach the
    /// close animation, which still shows the window's last content.
    pub fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.newest = None;
        state.swapchain = None;
        state.buffers = None;
        state.buffer_count = 0;
        state.committed = None;
        state.splash_surface = None;
        state.splash_image = None;
        state.caption_surface = None;
        state.last_frame = None;
        state.readback = None;
        state.samples = None;
        state.band_probe = None;
        state.band_pending = false;
        state.presents.forget();
    }

    /// The frame the window shows, read back for a screenshot, or `None` before the app's
    /// first frame. A frame is whole in its texture only until the renderer writes the next
    /// one into it, so while the app draws, the screenshot takes the next frame as it is shown,
    /// waiting [`CAPTURE_WAIT`] at most; a still app's newest frame is read at once.
    pub fn capture(&self) -> Result<Option<CapturedFrame>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let devices = Arc::clone(&state.devices);
        if state.closed {
            return Ok(None);
        }
        let drawing = state
            .last_frame_at
            .is_some_and(|at| at.elapsed() < CAPTURE_STILL_FOR);
        if drawing {
            state.capture_wanted = true;
            state.captured = None;
            let deadline = Instant::now() + CAPTURE_WAIT;
            while state.captured.is_none() && !state.closed {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                state = self
                    .capture_done
                    .wait_timeout(state, left)
                    .unwrap_or_else(|e| e.into_inner())
                    .0;
            }
            state.capture_wanted = false;
            if let Some(captured) = state.captured.take() {
                return captured.map(Some);
            }
            if state.closed {
                return Ok(None);
            }
        }
        let Some((texture, _, region)) = state.newest.clone() else {
            return Ok(None);
        };
        let band = to_task_length(state.band, state.scale);
        // SAFETY: the texture is one of this presenter's device, read under the state lock.
        let rgba = unsafe { read_frame(&devices, &texture, region)? };
        Ok(Some(CapturedFrame {
            width: region.0,
            height: region.1,
            rgba,
            band,
        }))
    }

    /// Shows the frame in `texture`, which fills its top-left `content` pixels (0 when the
    /// frame fills the whole texture), and returns once it has been copied out, so the
    /// texture may be written again.
    pub fn present(&self, texture: &OpenedTexture, content: (u32, u32)) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Ok(());
        }
        self.follow_gpu(&mut state);
        if texture.generation != state.generation {
            // Opened on devices a GPU reset removed: the window manager sends a texture made
            // on new ones, and the frames after this one come in it.
            return Ok(());
        }
        let devices = Arc::clone(&state.devices);
        let region = content_region(content, texture.size);
        state.newest = Some((texture.texture.clone(), texture.size, region));
        // Frames in quick succession say the window is drawing: its swapchain takes all its
        // buffers again. A lone frame of a still window keeps the two it has.
        let now = Instant::now();
        let since_last = state
            .last_frame_at
            .map(|at| now.saturating_duration_since(at));
        state.wanted_buffers = buffers_after_frame(state.wanted_buffers, since_last);
        state.last_frame_at = Some(now);
        if state.hidden.frame() == HiddenFrame::Hold {
            // Nothing is shown in a minimized window; the frame waits in the texture.
            return Ok(());
        }
        let started = Instant::now();
        // A frame that arrives while the window is taking the size of the one kept before it
        // waits, so that frame is not lost from the back buffer.
        let deadline = started + CLAIM_WAIT;
        let arrival = loop {
            // The size the frame fills in the window, at the window's scale.
            let shown = frame_in_window(&state, region);
            match state.follow.frame_arrived(shown) {
                Arrival::Wait => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        state.follow.release();
                        continue;
                    }
                    state = self
                        .claim_released
                        .wait_timeout(state, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                    if state.closed {
                        return Ok(());
                    }
                }
                arrival => break arrival,
            }
        };
        if arrival == Arrival::Stale {
            // Drawn at the size the window had before its monitor changed scale: it is not
            // shown, the window keeps the size it took for the new scale, and the back buffer
            // keeps any frame kept for the window to take.
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_stale",
                region,
                &format!("view={}x{}", state.follow.view().0, state.follow.view().1),
            );
            return Ok(());
        }
        if texture.generation != state.generation {
            // A GPU reset moved the window to new devices while the frame waited.
            return Ok(());
        }
        let keep = arrival == Arrival::Keep;
        if keep {
            state.kept_region = Some(region);
        }
        let shown =
            match self.present_locked(&mut state, &texture.texture, texture.size, region, keep) {
                Ok(shown) => shown,
                Err(why) => {
                    // A swapchain resize, a present or a commit that fails because a GPU reset
                    // removed the devices moves the window to new ones; it stays open.
                    self.recover(&mut state);
                    return Err(why);
                }
            };
        self.trace_frame_colour(&mut state, &texture.texture, region);
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        let band_changed = unsafe { self.probe_band(&mut state, &texture.texture, region) };
        // A screenshot waiting for this frame reads it while it is whole in its texture.
        if state.capture_wanted {
            state.capture_wanted = false;
            // The band over the frame in the frame's own pixels.
            let band = to_task_length(state.band, state.scale);
            // SAFETY: as above.
            let read = unsafe { read_frame(&devices, &texture.texture, region) };
            state.captured = Some(read.map(|rgba| CapturedFrame {
                width: region.0,
                height: region.1,
                rgba,
                band,
            }));
            self.capture_done.notify_all();
        }
        if keep {
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_kept",
                region,
                &format!("view={}x{}", state.follow.view().0, state.follow.view().1),
            );
        } else {
            self.watch_press(&mut state, &texture.texture, region);
            let in_window = frame_in_window(&state, region);
            self.frame_shown(
                &mut state,
                in_window,
                shown.new_size,
                shown.resize_buffers,
                started,
            );
        }
        drop(state);
        if let Some(sample) = band_changed {
            if let Some(told) = self
                .band_colour
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                told(sample);
            }
        }
        if keep {
            // The window takes the frame's size, and shows it then.
            if let Some(ready) = self
                .frame_ready
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
            {
                ready();
            }
        }
        Ok(())
    }

    /// When the resize trace is on, says so if the frame in `texture` is all one colour. A
    /// frame nothing was drawn into reads this way here, before the swapchain has any part in
    /// it, so a black window with no such line was blackened after this point. One line per
    /// size at most.
    fn trace_frame_colour(&self, state: &mut State, texture: &ID3D11Texture2D, region: (u32, u32)) {
        if !trace_detailed() {
            return;
        }
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        match unsafe { self.frame_colour(state, texture, region) } {
            Ok(Some([red, green, blue, alpha])) => trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_one_colour",
                region,
                &format!("colour=#{red:02x}{green:02x}{blue:02x}{alpha:02x}"),
            ),
            Ok(None) => {}
            Err(why) => note(self.logger.as_deref(), &why),
        }
    }

    /// While the input trace is on, compares the frame just presented from `texture` (of
    /// `region` pixels) around the last traced press with what was there when the press came,
    /// and logs `app.visible_change` for the first frame that changed it visibly, or
    /// `app.visible_change_none` when no frame did. Reads a small square back from the GPU
    /// for each frame after a press, and keeps a GPU copy of the last frame meanwhile, which
    /// is why only the trace does this.
    fn watch_press(&self, state: &mut State, texture: &ID3D11Texture2D, region: (u32, u32)) {
        if !input_trace_on() {
            if state.last_frame.is_some() || state.watch.is_some() {
                state.last_frame = None;
                state.readback = None;
                state.watch = None;
            }
            return;
        }
        let press = presses()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.task_id);
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        if let Err(why) = unsafe { self.watch_press_locked(state, texture, region, press) } {
            state.watch = None;
            note(
                self.logger.as_deref(),
                &format!(
                    "task {}: the input trace cannot compare frames around a press: {why}",
                    self.task_id
                ),
            );
        }
    }

    unsafe fn watch_press_locked(
        &self,
        state: &mut State,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
        press: Option<((f32, f32), Duration)>,
    ) -> Result<(), String> {
        if let Some((point, at)) = press {
            state.watch = None;
            let last = state.last_frame.clone();
            match (area_around(point, region, visible_change::SIDE), last) {
                (Some(area), Some((last, size))) if size == region => {
                    let baseline = self.read_area(state, &last, area)?;
                    state.watch = Some((PressWatch::new(area, baseline), at));
                }
                (area, _) => input_trace(
                    "app.visible_change_none",
                    self.task_id,
                    if area.is_none() {
                        "reason=press-outside-frame"
                    } else {
                        "reason=no-frame-of-this-size-before-press"
                    },
                ),
            }
        }
        if let Some((mut watch, at)) = state.watch.take() {
            let area = watch.area();
            if region.0 >= area.right && region.1 >= area.bottom {
                let pixels = self.read_area(state, texture, area)?;
                let present = state
                    .swapchain
                    .as_ref()
                    .and_then(|swapchain| swapchain.GetLastPresentCount().ok())
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "-".into());
                let since = host_clock().saturating_sub(at);
                match watch.frame(&pixels) {
                    Verdict::Waiting => state.watch = Some((watch, at)),
                    Verdict::Visible { frames, fraction } => input_trace(
                        "app.visible_change",
                        self.task_id,
                        &format!(
                            "frames={frames} changed={fraction:.3} present={present} \
                             since_press={} area={}",
                            millis(since),
                            describe_area(area)
                        ),
                    ),
                    Verdict::GaveUp { frames, largest } => input_trace(
                        "app.visible_change_none",
                        self.task_id,
                        &format!(
                            "reason=no-visible-change frames={frames} largest={largest:.3} \
                             area={}",
                            describe_area(area)
                        ),
                    ),
                }
            } else {
                input_trace(
                    "app.visible_change_none",
                    self.task_id,
                    "reason=frame-smaller-than-area",
                );
            }
        }
        self.keep_last_frame(state, texture, region)
    }

    /// Copies the frame in `texture`, of `region` pixels, on the GPU, for the next press.
    unsafe fn keep_last_frame(
        &self,
        state: &mut State,
        texture: &ID3D11Texture2D,
        (width, height): (u32, u32),
    ) -> Result<(), String> {
        let devices = Arc::clone(&state.devices);
        if width == 0 || height == 0 {
            return Ok(());
        }
        if state
            .last_frame
            .as_ref()
            .is_none_or(|(_, size)| *size != (width, height))
        {
            state.last_frame = None;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: 0,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut made = None;
            devices
                .device
                .CreateTexture2D(&desc, None, Some(&mut made))
                .map_err(|e| failed("creating the copy of the last frame", e))?;
            if let Some(made) = made {
                state.last_frame = Some((made, (width, height)));
            }
        }
        let Some((last, _)) = &state.last_frame else {
            return Ok(());
        };
        let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
        let whole = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        context.CopySubresourceRegion(last, 0, 0, 0, 0, texture, 0, Some(&whole as *const _));
        Ok(())
    }

    /// The pixels of `area` of `source`, as tightly packed RGBA rows. Waits for the GPU.
    unsafe fn read_area(
        &self,
        state: &mut State,
        source: &ID3D11Texture2D,
        area: Area,
    ) -> Result<Vec<u8>, String> {
        let devices = Arc::clone(&state.devices);
        if state.readback.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: visible_change::SIDE,
                Height: visible_change::SIDE,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            devices
                .device
                .CreateTexture2D(&desc, None, Some(&mut state.readback))
                .map_err(|e| failed("creating the texture a press's area is read into", e))?;
        }
        let Some(readback) = &state.readback else {
            return Err("no texture to read a press's area into".into());
        };
        let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
        let from = D3D11_BOX {
            left: area.left,
            top: area.top,
            front: 0,
            right: area.right,
            bottom: area.bottom,
            back: 1,
        };
        context.CopySubresourceRegion(readback, 0, 0, 0, 0, source, 0, Some(&from as *const _));
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(readback, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| failed("reading a press's area", e))?;
        let row = (area.width() * 4) as usize;
        let mut pixels = Vec::with_capacity(area.bytes());
        for y in 0..area.height() as usize {
            let start = (mapped.pData as *const u8).add(y * mapped.RowPitch as usize);
            pixels.extend_from_slice(std::slice::from_raw_parts(start, row));
        }
        context.Unmap(readback, 0);
        Ok(pixels)
    }

    /// The colour all the sample pixels of the frame in `texture` have, if they have one. Waits
    /// for the GPU to copy a few pixels, which is why only the trace does this.
    unsafe fn frame_colour(
        &self,
        state: &mut State,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
    ) -> Result<Option<[u8; 4]>, String> {
        let devices = Arc::clone(&state.devices);
        if state.samples.is_none() {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: SAMPLE_GRID,
                Height: SAMPLE_GRID,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            devices
                .device
                .CreateTexture2D(&desc, None, Some(&mut state.samples))
                .map_err(|e| failed("creating the texture a frame is sampled into", e))?;
        }
        let Some(samples) = &state.samples else {
            return Ok(None);
        };
        let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
        for (index, (x, y)) in sample_points(region).into_iter().enumerate() {
            let index = index as u32;
            let pixel = D3D11_BOX {
                left: x,
                top: y,
                front: 0,
                right: x + 1,
                bottom: y + 1,
                back: 1,
            };
            context.CopySubresourceRegion(
                samples,
                0,
                index % SAMPLE_GRID,
                index / SAMPLE_GRID,
                0,
                texture,
                0,
                Some(&pixel as *const _),
            );
        }
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        context
            .Map(samples, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
            .map_err(|e| failed("reading the pixels sampled from a frame", e))?;
        let mut pixels = Vec::with_capacity((SAMPLE_GRID * SAMPLE_GRID) as usize);
        for row in 0..SAMPLE_GRID as usize {
            let start = (mapped.pData as *const u8).add(row * mapped.RowPitch as usize);
            for column in 0..SAMPLE_GRID as usize {
                let mut pixel = [0u8; 4];
                std::ptr::copy_nonoverlapping(start.add(column * 4), pixel.as_mut_ptr(), 4);
                pixels.push(pixel);
            }
        }
        context.Unmap(samples, 0);
        Ok(uniform_colour(&pixels))
    }

    /// A frame of `region` pixels reached the screen, `started` being when its showing began.
    fn frame_shown(
        &self,
        state: &mut State,
        region: (u32, u32),
        new_size: bool,
        resized: Option<Duration>,
        started: Instant,
    ) {
        let presented = Instant::now();
        let buffers = state.buffers.unwrap_or_default();
        if new_size {
            // Once per size: what showing a frame of a new size cost. `resize_buffers` is 0
            // unless the texture itself changed size.
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.present",
                region,
                &format!(
                    "buffers={}x{} resize_buffers={} present={}",
                    buffers.0,
                    buffers.1,
                    millis(resized.unwrap_or_default()),
                    millis(presented.saturating_duration_since(started))
                ),
            );
        }
        if let Some(waited) = state.latency.frame_shown(region, presented) {
            // One line per resize that ended in a frame of the window's size, a few a
            // second at most during a drag.
            trace_summary(
                self.logger.as_deref(),
                self.task_id,
                "app.summary",
                region,
                &format!(
                    "waited={} resize_buffers={} present={}",
                    millis(waited),
                    resized.map(millis).unwrap_or_else(|| "0.0".into()),
                    millis(presented.saturating_duration_since(started))
                ),
            );
            if state.latency.count() % SUMMARY_EVERY == 0 {
                if let Some(summary) = state.latency.summary() {
                    note(self.logger.as_deref(), &format!("task window: {summary}"));
                }
            }
        }
    }

    /// Scales the visual so the frame shows 1:1, and commits it, unless
    /// that is what is on the compositor already. Returns whether it committed. A present
    /// and a commit reach the compositor separately, so this comes right after the present
    /// that goes with it.
    fn commit_visual(&self, state: &mut State) -> bool {
        let devices = Arc::clone(&state.devices);
        let Some(buffers) = state.buffers else {
            // Nothing to show yet: the first frame commits it.
            return false;
        };
        // A window that holds the size the desktop gave it (maximised, snapped, full screen)
        // shows a frame of another size in its middle; any other window has its frame's size.
        let placement = frame_placement(
            state.frame,
            state.follow.view(),
            state.scale,
            state.follow.held(),
        );
        let wanted = (buffers, state.frame, placement);
        if state.committed == Some(wanted) {
            return false;
        }
        let (x, y) = frame_scale(buffers, state.frame);
        // A task that keeps its density on a monitor of another scale is shown at the window's
        // scale, the same size to the eye.
        let (numerator, denominator) = placement.scale;
        let ratio = numerator as f32 / denominator.max(1) as f32;
        let (x, y) = (x * ratio, y * ratio);
        let matrix = Matrix3x2 {
            M11: x,
            M12: 0.0,
            M21: 0.0,
            M22: y,
            M31: placement.origin.0 as f32,
            M32: placement.origin.1 as f32,
        };
        let Ok(dcomp) = devices.dcomp() else {
            return false;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.visual.SetTransform2(&matrix);
            if state.splash_surface.is_some() && state.first_frame.is_some() {
                // The first frame is presented, and this commit shows it: the splash under it
                // goes in the same commit, so nothing shows between the two.
                let _ = state.splash.SetContent(None::<&windows::core::IUnknown>);
            }
            let _ = dcomp.Commit();
        }
        if state.first_frame.is_some() {
            state.splash_surface = None;
            state.splash_image = None;
        }
        state.committed = Some(wanted);
        true
    }

    /// Shows `image` as the window's splash until its first frame: the window never shows the
    /// desktop through it while the app starts. Returns once the compositor has taken it, so
    /// the window can be shown with it.
    pub fn show_splash(&self, image: SplashImage) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.first_frame.is_some() {
            return;
        }
        state.splash_image = Some(image);
        let view = state.asked;
        let drawn = self.draw_splash(&mut state, view);
        drop(state);
        match drawn {
            Ok(()) => self.wait_for_commit(),
            Err(why) => note(self.logger.as_deref(), &why),
        }
    }

    /// The window is now on a monitor of `dpi`. A splash still shown keeps its icon 160 dp
    /// large there: it is drawn again from the icon's full image at the new size, with the
    /// client area the window takes for the new scale, when that comes.
    pub fn splash_dpi_changed(&self, dpi: u32) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.first_frame.is_some() {
            return;
        }
        let Some(image) = state.splash_image.as_mut() else {
            return;
        };
        if image.dpi == dpi {
            return;
        }
        image.dpi = dpi;
        let view = state.asked;
        if let Err(why) = self.draw_splash(&mut state, view) {
            note(self.logger.as_deref(), &why);
        }
    }

    /// Draws the splash at the size of a `view` pixel client area, and commits it.
    fn draw_splash(&self, state: &mut State, view: (u32, u32)) -> Result<(), String> {
        let devices = Arc::clone(&state.devices);
        let Some(image) = state.splash_image.as_ref() else {
            return Ok(());
        };
        if view.0 == 0 || view.1 == 0 {
            return Ok(());
        }
        let icon = image.icon.as_ref().map(|(pixels, size)| splash::Icon {
            size: *size,
            pixels,
        });
        let source = icon.map_or(0, |icon| icon.size.0.min(icon.size.1));
        let pixels = splash::compose(
            view,
            image.background,
            icon,
            splash::icon_edge(view, image.dpi, source),
        );
        let dcomp = devices.dcomp()?.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns; the update reads only
        // `pixels`, which holds the whole image.
        unsafe {
            if state.splash_surface.as_ref().map(|(_, at)| *at) != Some(view) {
                let surface = dcomp
                    .CreateSurface(
                        view.0,
                        view.1,
                        DXGI_FORMAT_B8G8R8A8_UNORM,
                        DXGI_ALPHA_MODE_PREMULTIPLIED,
                    )
                    .map_err(|e| failed("creating a task window's splash surface", e))?;
                state
                    .splash
                    .SetContent(&surface)
                    .map_err(|e| failed("showing a task window's splash", e))?;
                state.splash_surface = Some((surface, view));
            }
            let Some((surface, _)) = state.splash_surface.as_ref() else {
                return Ok(());
            };
            let mut offset = POINT::default();
            let texture: ID3D11Texture2D = surface
                .BeginDraw(None, &mut offset)
                .map_err(|e| failed("drawing a task window's splash", e))?;
            let place = D3D11_BOX {
                left: offset.x as u32,
                top: offset.y as u32,
                front: 0,
                right: offset.x as u32 + view.0,
                bottom: offset.y as u32 + view.1,
                back: 1,
            };
            {
                let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
                context.UpdateSubresource(
                    &texture,
                    0,
                    Some(&place as *const D3D11_BOX),
                    pixels.as_ptr() as *const c_void,
                    view.0 * 4,
                    0,
                );
            }
            surface
                .EndDraw()
                .map_err(|e| failed("drawing a task window's splash", e))?;
            dcomp
                .Commit()
                .map_err(|e| failed("committing a task window's splash", e))?;
        }
        Ok(())
    }

    /// Presents what is in the back buffer, of `size` pixels. Returns whether that size
    /// differs from the last one shown.
    unsafe fn present_back_buffer(
        &self,
        state: &mut State,
        (width, height): (u32, u32),
    ) -> Result<bool, String> {
        let devices = Arc::clone(&state.devices);
        let Some(swapchain) = &state.swapchain else {
            return Ok(false);
        };
        let new_size = state.frame != (width, height);
        if new_size {
            // Made or resized buffers show all of themselves until told otherwise.
            swapchain
                .SetSourceSize(width, height)
                .map_err(|e| failed("setting the size a task window shows", e))?;
        }
        let swapchain = swapchain.clone();
        // The statistics are read while the input trace is on, and until the first frame is
        // known to be on the screen, for the window's first frame time.
        let watching = input_trace_on() || !state.first_shown;
        if watching {
            // What the presents before this one did, before this one changes the statistics.
            self.trace_shown(state);
        }
        let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
        // Sync interval 0: the present is queued without waiting for a vertical blank.
        swapchain
            .Present(0, DXGI_PRESENT(0))
            .ok()
            .map_err(|e| failed("presenting a task window's frame", e))?;
        drop(context);
        let mut count = None;
        if watching && !state.no_statistics {
            if let Ok(last) = swapchain.GetLastPresentCount() {
                state.presents.presented(last);
                count = Some(last);
            }
        }
        state.frame = (width, height);
        if state.first_frame.is_none() {
            state.first_frame = Some(count.unwrap_or(0));
            self.first_frame_presented((width, height), count);
        }
        Ok(new_size)
    }

    /// The window presented its first frame, of `size` pixels: said in the log, in the input
    /// trace, and on the window for `dsa launch --wait-first-frame`.
    fn first_frame_presented(&self, (width, height): (u32, u32), count: Option<u32>) {
        let at = host_clock();
        window_marks::mark_time(self.hwnd, window_marks::FIRST_FRAME_PROPERTY, at);
        note(
            self.logger.as_deref(),
            &format!(
                "task {}: the window presented its first frame ({width}x{height}) at host={:.3}",
                self.task_id,
                at.as_secs_f64() * 1000.0
            ),
        );
        input_trace(
            "app.first_frame",
            self.task_id,
            &format!(
                "size={width}x{height} present={}",
                count.map(|c| c.to_string()).unwrap_or_else(|| "-".into())
            ),
        );
    }

    /// The window's first frame reached the screen at the vertical blank `vblank`.
    fn first_frame_shown(&self, present: u32, vblank: Duration, exact: bool) {
        window_marks::mark_time(self.hwnd, window_marks::FIRST_SHOWN_PROPERTY, vblank);
        note(
            self.logger.as_deref(),
            &format!(
                "task {}: the window's first frame was shown at host={:.3}",
                self.task_id,
                vblank.as_secs_f64() * 1000.0
            ),
        );
        input_trace(
            "app.first_shown",
            self.task_id,
            &format!(
                "present={present} vblank={:.3} exact={exact}",
                vblank.as_secs_f64() * 1000.0
            ),
        );
    }

    /// When the input trace is on, logs `app.shown` for each present the swapchain's frame
    /// statistics show to have reached the screen since the last look. A swapchain without
    /// statistics is said once, and not asked again.
    /// Until the window's first frame is known to be on the screen, it does the same with the
    /// trace off, to say when that frame was shown.
    fn trace_shown(&self, state: &mut State) {
        let tracing = input_trace_on();
        let done = !tracing && state.first_shown;
        if done || state.no_statistics || state.presents.pending() == 0 {
            return;
        }
        let Some(swapchain) = state.swapchain.clone() else {
            return;
        };
        let mut statistics = DXGI_FRAME_STATISTICS::default();
        // SAFETY: a plain COM call writing one local structure.
        match unsafe { swapchain.GetFrameStatistics(&mut statistics) } {
            Ok(()) => {}
            // The display path changed meanwhile; the next reading is good again.
            Err(error) if error.code() == DXGI_ERROR_FRAME_STATISTICS_DISJOINT => return,
            Err(error) => {
                state.no_statistics = true;
                state.presents.forget();
                note(
                    self.logger.as_deref(),
                    &format!(
                        "task {}: this window's swapchain gives no frame statistics ({error}), \
                         so the input trace has no app.shown lines for it",
                        self.task_id
                    ),
                );
                return;
            }
        }
        let shown = state.presents.statistics(
            FrameStatistics {
                present_count: statistics.PresentCount,
                sync_qpc: statistics.SyncQPCTime,
            },
            qpc_frequency(),
        );
        for present in shown {
            let shows_first = state
                .first_frame
                .is_some_and(|first| present.present >= first);
            if shows_first && !state.first_shown {
                state.first_shown = true;
                self.first_frame_shown(present.present, present.vblank, present.exact);
            }
            if !tracing {
                continue;
            }
            input_trace(
                "app.shown",
                self.task_id,
                &format!(
                    "present={} vblank={:.3} exact={}",
                    present.present,
                    present.vblank.as_secs_f64() * 1000.0,
                    present.exact
                ),
            );
        }
        if !tracing && state.first_shown {
            // Nothing more is asked of the statistics until the trace is turned on.
            state.presents.forget();
        }
    }

    fn present_locked(
        &self,
        state: &mut State,
        texture: &ID3D11Texture2D,
        texture_size: (u32, u32),
        (width, height): (u32, u32),
        stage: bool,
    ) -> Result<Shown, String> {
        let devices = Arc::clone(&state.devices);
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        unsafe {
            let started = Instant::now();
            // The buffers follow the texture, which keeps its size while the window is
            // resized; only the source size follows the frame.
            let mut change = swapchain_change(state.buffers, texture_size);
            if change == SwapchainChange::Keep
                && state.swapchain.is_some()
                && state.buffer_count != state.wanted_buffers
            {
                // The size fits, but a still window lets a buffer go or a drawing one takes
                // it back.
                change = SwapchainChange::Resize(texture_size);
            }
            match change {
                SwapchainChange::Create((buffer_width, buffer_height)) => {
                    let desc = DXGI_SWAP_CHAIN_DESC1 {
                        Width: buffer_width,
                        Height: buffer_height,
                        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                        Stereo: BOOL(0),
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: state.wanted_buffers,
                        // The only scaling a composition swapchain takes; the visual's
                        // transform does the scaling.
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        Flags: 0,
                    };
                    let swapchain = devices
                        .factory
                        .CreateSwapChainForComposition(&devices.device, &desc, None)
                        .map_err(|e| failed("creating a task window's swapchain", e))?;
                    let swapchain: IDXGISwapChain2 = swapchain
                        .cast()
                        .map_err(|e| failed("reaching a task window's IDXGISwapChain2", e))?;
                    {
                        let _dcomp = devices.dcomp()?.lock().unwrap_or_else(|e| e.into_inner());
                        state
                            .visual
                            .SetContent(&swapchain)
                            .map_err(|e| failed("showing a task window's swapchain", e))?;
                    }
                    state.swapchain = Some(swapchain);
                    state.buffer_count = state.wanted_buffers;
                    // The content is new: the first present commits it.
                    state.committed = None;
                }
                SwapchainChange::Resize((buffer_width, buffer_height)) => {
                    if let Some(swapchain) = &state.swapchain {
                        swapchain
                            .ResizeBuffers(
                                state.wanted_buffers,
                                buffer_width,
                                buffer_height,
                                DXGI_FORMAT_UNKNOWN,
                                DXGI_SWAP_CHAIN_FLAG(0),
                            )
                            .map_err(|e| failed("resizing a task window's swapchain", e))?;
                        state.buffer_count = state.wanted_buffers;
                    }
                }
                SwapchainChange::Keep => {}
            }
            let remade = change != SwapchainChange::Keep;
            if remade {
                // The old buffers' presents are not reported any more.
                state.presents.forget();
                if !state.first_shown && state.first_frame.is_some() {
                    // The first frame's present went with them: the next one shown stands in.
                    state.first_frame = Some(0);
                }
            }
            state.buffers = Some(texture_size);
            // How long making or resizing the swapchain took, when it happened.
            let resize_buffers = remade.then(|| started.elapsed());
            let mut new_size = remade || state.frame != (width, height);
            if remade {
                // Made or resized buffers show all of themselves until told otherwise, and
                // the frame last shown says nothing about them.
                state.frame = (0, 0);
            }
            let Some(swapchain) = &state.swapchain else {
                unreachable!("the swapchain was made above");
            };
            let back: ID3D11Texture2D = swapchain
                .GetBuffer(0)
                .map_err(|e| failed("reaching a task window's back buffer", e))?;
            {
                let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
                let region = D3D11_BOX {
                    left: 0,
                    top: 0,
                    front: 0,
                    right: width,
                    bottom: height,
                    back: 1,
                };
                context.CopySubresourceRegion(
                    &back,
                    0,
                    0,
                    0,
                    0,
                    texture,
                    0,
                    Some(&region as *const _),
                );
            }
            drop(back);
            if !stage {
                new_size = self.present_back_buffer(state, (width, height))? || new_size;
                // Right after the present, not after the wait below, so the two reach the
                // compositor as close together as they can. Only a new client area or new
                // buffers have anything to commit.
                self.commit_visual(state);
            }
            let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
            // The renderer writes the shared texture again once this returns, so the copy
            // out of it must be finished.
            if state.query.is_none() {
                let mut query = None;
                let desc = D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                };
                let _ = devices.device.CreateQuery(&desc, Some(&mut query));
                state.query = query;
            }
            if let Some(query) = &state.query {
                context.End(query);
                context.Flush();
                let started = Instant::now();
                loop {
                    let mut done = BOOL(0);
                    let _ = context.GetData(
                        query,
                        Some(&mut done as *mut BOOL as *mut c_void),
                        std::mem::size_of::<BOOL>() as u32,
                        0,
                    );
                    if done.as_bool() || started.elapsed() > COPY_WAIT {
                        break;
                    }
                    std::thread::yield_now();
                }
            }
            drop(context);
            Ok(Shown {
                new_size,
                resize_buffers,
            })
        }
    }
}

/// What showing one frame changed.
struct Shown {
    /// The frame's size differs from the last one's, or the swapchain was made or resized.
    new_size: bool,
    /// How long making or resizing the swapchain took, when it was.
    resize_buffers: Option<Duration>,
}
