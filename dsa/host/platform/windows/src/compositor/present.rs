//! Direct3D 11 and DirectComposition: how a task's frames appear in its window.
//!
//! The window manager makes a [`SharedTexture`] for each task window and the renderer copies
//! the task's frame into it on the GPU. The texture is shared by an NT handle, which the
//! window manager places in the app window host's process; the window host opens it
//! ([`Gpu::open_shared`]) on the same GPU. Each task window there has a DirectComposition
//! visual holding a flip-model swapchain, and [`WindowPresenter::present`] copies the frame
//! into the swapchain and presents it. The visual is scaled to fill the window, so while the
//! user resizes it, the last frame is stretched over the new size until a frame of that size
//! arrives, which is then shown 1:1. No frame passes through the CPU.
//!
//! The texture has the size of the guest's buffers, the largest a frame of the task can be,
//! and each frame fills its top-left region. The swapchain's buffers take the texture's
//! size once, and each frame sets only the swapchain's source size
//! (`IDXGISwapChain2::SetSourceSize`) to the region it fills, so a window resize never
//! resizes the swapchain's buffers. DirectComposition stretches the source region over the
//! whole of the buffers, so the visual scales from the buffers' size to the client area,
//! whatever the size of the frame. A new frame size therefore changes nothing about the
//! visual; only a new client area does.
//!
//! While the user drags, a new size is held: the window asks the guest for it before it takes
//! it ([`WindowPresenter::hold_view`]), and a frame of that size that arrives meanwhile is
//! copied into the back buffer but not presented. When the window takes the size, the frame
//! is presented and the visual scaled in one go ([`WindowPresenter::view_resized`]), so
//! neither a slid picture nor an empty strip reaches the screen: until that moment the old
//! window shows the old frame. The back buffer holds the only copy of that frame, so a frame of
//! another size arriving meanwhile is left out.
//!
//! Each presenter measures how long its window waited for a frame of its new size
//! (`ResizeLatency`) and logs a summary every 20 resizes and when the window closes, and a
//! warning when a window is left showing a stretched frame.
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

use std::ffi::c_void;
use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::frame_buffers::{
    content_region, sample_points, swapchain_buffers, swapchain_change, uniform_colour,
    visual_scale, SwapchainChange, SAMPLE_GRID,
};
use dsa_host_core::compositor::present_watch::{FrameStatistics, PresentWatch};
use dsa_host_core::compositor::resize_latency::ResizeLatency;
use dsa_host_core::compositor::resize_sync::{self, frame_action, FrameAction, ResizeSync, Step};
use dsa_host_core::compositor::visible_change::{self, area_around, Area, PressWatch, Verdict};
use windows::core::{Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_READ, D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIAdapter, IDXGIAdapter3, IDXGIDevice, IDXGIDevice3, IDXGIFactory2,
    IDXGIFactory4, IDXGIResource1, IDXGISwapChain2, DXGI_CREATE_FACTORY_FLAGS,
    DXGI_ERROR_FRAME_STATISTICS_DISJOINT, DXGI_FRAME_STATISTICS, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_PRESENT, DXGI_QUERY_VIDEO_MEMORY_INFO,
    DXGI_SCALING_STRETCH, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Matrix3x2;

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

/// The Direct3D 11 device, and the DirectComposition device every task window of a process
/// shares when the process has windows.
pub struct Gpu {
    device: ID3D11Device,
    context: Mutex<ID3D11DeviceContext>,
    factory: IDXGIFactory2,
    dcomp: Option<Mutex<IDCompositionDevice>>,
    /// Whether the device is on the GPU the renderer named; frames cannot cross GPUs.
    pub on_renderer_gpu: bool,
}

// SAFETY: the Direct3D 11 device and the DXGI factory are free-threaded. The immediate
// context and the DirectComposition device are used only under their locks.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

impl Gpu {
    /// Creates the devices on the GPU `luid` names (the renderer's), else on the default
    /// GPU, else on the software rasteriser. `composition` adds the DirectComposition device
    /// that windows need; the window manager, which has none, leaves it out.
    pub fn new(luid: Option<[u8; 8]>, composition: bool) -> Result<Arc<Gpu>, String> {
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
                        return Ok(Arc::new(Gpu {
                            device,
                            context: Mutex::new(context),
                            factory,
                            dcomp,
                            on_renderer_gpu,
                        }));
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

    /// Asks the driver to free the memory it keeps for this device beyond the resources that
    /// are alive (its command buffers, caches and pools), as an app does on suspend. Called when
    /// no task window is shown; the driver makes what it needs again on the next copy.
    pub fn trim(&self) {
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

    /// The GPU memory this whole process uses on this device's GPU, in KiB: its dedicated
    /// video memory (local) and the system memory the GPU maps for it (non-local). It counts
    /// every API in the process, so the renderer's Vulkan memory is in it too.
    pub fn video_memory_kb(&self) -> Option<(u64, u64)> {
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

    /// A texture the renderer can open by its shared handle and copy a frame into.
    pub fn shared_texture(&self, width: u32, height: u32) -> Result<SharedTexture, String> {
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
                .CreateTexture2D(&desc, None, Some(&mut texture))
                .map_err(|e| failed("creating a shared texture for a task window", e))?;
            let texture: ID3D11Texture2D =
                texture.ok_or("creating a shared texture returned no texture")?;
            let resource: IDXGIResource1 = texture
                .cast()
                .map_err(|e| failed("reaching a shared texture's DXGI resource", e))?;
            let handle = resource
                .CreateSharedHandle(
                    None,
                    DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0,
                    PCWSTR::null(),
                )
                .map_err(|e| failed("sharing a task window's texture", e))?;
            Ok(SharedTexture {
                _texture: texture,
                handle,
                size: (width, height),
            })
        }
    }

    /// Opens the texture another process shared by `handle`, which is a handle of this
    /// process. The handle stays the caller's to close.
    pub fn open_shared(&self, handle: isize, size: (u32, u32)) -> Result<OpenedTexture, String> {
        // SAFETY: plain COM calls; the handle is only read.
        unsafe {
            let device: ID3D11Device1 = self
                .device
                .cast()
                .map_err(|e| failed("reaching the Direct3D 11.1 device", e))?;
            let texture: ID3D11Texture2D = device
                .OpenSharedResource1(HANDLE(handle as *mut c_void))
                .map_err(|e| failed("opening a task's shared frame texture", e))?;
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if (desc.Width, desc.Height) != size {
                return Err(format!(
                    "a task's frame texture is {}x{}, not the {}x{} it was announced as",
                    desc.Width, desc.Height, size.0, size.1
                ));
            }
            Ok(OpenedTexture { texture, size })
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
}

// SAFETY: a Direct3D 11 resource may be used from any thread.
unsafe impl Send for OpenedTexture {}
unsafe impl Sync for OpenedTexture {}

impl OpenedTexture {
    pub fn size(&self) -> (u32, u32) {
        self.size
    }
}

/// A texture the renderer copies a task's frame into, and an app window host shows it from,
/// each opening it by its shared NT handle. The handle closes when the last reference goes.
pub struct SharedTexture {
    /// Held so the texture is this process's for as long as its handle is handed out.
    _texture: ID3D11Texture2D,
    handle: HANDLE,
    size: (u32, u32),
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
}

impl Drop for SharedTexture {
    fn drop(&mut self) {
        // SAFETY: the handle was created for this texture and is closed once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

struct State {
    target: IDCompositionTarget,
    visual: IDCompositionVisual,
    swapchain: Option<IDXGISwapChain2>,
    /// The size of the swapchain's buffers: the texture's, not a frame's.
    buffers: Option<(u32, u32)>,
    /// The size of the last frame shown: the swapchain's source size.
    frame: (u32, u32),
    /// The size of the window's client area.
    view: (u32, u32),
    /// What the visual was last committed for: the buffers' size and the client area.
    committed: Option<((u32, u32), (u32, u32))>,
    /// The size the window is about to take, while it waits for its frame.
    hold: Option<Hold>,
    query: Option<ID3D11Query>,
    /// Where the resize trace reads a frame's sample pixels; made when the trace first looks.
    samples: Option<ID3D11Texture2D>,
    closed: bool,
    /// How long the window waits for a frame of its size after each resize.
    latency: ResizeLatency,
    /// Whether a step of a drag waits for its frame, and for how long.
    sync: ResizeSync,
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
}

/// The presses each task's window has to watch, by task: the point in guest pixels and when
/// the press came on the host clock. Filled from the window's thread, taken by the presenter.
fn presses() -> &'static Mutex<HashMap<i32, ((f32, f32), Duration)>> {
    static PRESSES: OnceLock<Mutex<HashMap<i32, ((f32, f32), Duration)>>> = OnceLock::new();
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

/// A size the window has asked the guest for and not yet taken.
struct Hold {
    view: (u32, u32),
    since: Instant,
    /// A frame of that size is in the back buffer, not yet presented.
    staged: bool,
}

/// How many resizes go between two summaries of their waits in the log.
const SUMMARY_EVERY: usize = 20;

/// A hold the window never followed with a new size (Windows did not apply the position it
/// proposed) is let go after this, so frames of that size are not kept from the screen.
const HOLD_FORGOTTEN: Duration = Duration::from_millis(500);

/// Shows one task's frames in its window.
pub struct WindowPresenter {
    task_id: i32,
    /// The window, which says when its first frame was presented and shown.
    hwnd: isize,
    gpu: Arc<Gpu>,
    state: Mutex<State>,
    /// The size of the latest frame that arrived, apart from the state so a window can wait
    /// for a frame while the frame is being shown.
    shown: Mutex<(u32, u32)>,
    arrived: Condvar,
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
        let dcomp = gpu.dcomp()?.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: `hwnd` is a live window of this thread; the rest are plain COM calls.
        let (target, visual) = unsafe {
            let target = dcomp
                .CreateTargetForHwnd(HWND(hwnd as *mut c_void), true)
                .map_err(|e| failed("attaching DirectComposition to a task window", e))?;
            let visual = dcomp
                .CreateVisual()
                .map_err(|e| failed("creating a task window's visual", e))?;
            target
                .SetRoot(&visual)
                .map_err(|e| failed("setting a task window's visual", e))?;
            dcomp
                .Commit()
                .map_err(|e| failed("committing a task window's visual", e))?;
            (target, visual)
        };
        drop(dcomp);
        Ok(Arc::new(WindowPresenter {
            task_id,
            hwnd,
            gpu,
            state: Mutex::new(State {
                target,
                visual,
                swapchain: None,
                buffers: None,
                frame: (0, 0),
                view,
                committed: None,
                hold: None,
                query: None,
                samples: None,
                closed: false,
                latency: ResizeLatency::new(),
                sync: ResizeSync::new(),
                presents: PresentWatch::new(),
                no_statistics: false,
                first_frame: None,
                first_shown: false,
                last_frame: None,
                readback: None,
                watch: None,
            }),
            shown: Mutex::new((0, 0)),
            arrived: Condvar::new(),
            logger,
        }))
    }

    /// The client area is now `view` pixels. A frame of a size the window was holding for is
    /// presented now, together with the visual's new scale; otherwise the last frame is
    /// stretched over the new size. Returns once the compositor has taken it, so a live
    /// resize shows it with the new window size.
    pub fn view_resized(&self, view: (u32, u32)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        let changed = state.view != view;
        state.view = view;
        let held = state.hold.as_ref().is_some_and(|hold| hold.view == view);
        if changed && !held {
            self.size_asked(&mut state, view);
        }
        self.show_hold(&mut state);
        let committed = self.commit_visual(&mut state);
        drop(state);
        if committed {
            self.wait_for_commit();
        }
    }

    /// The window is about to take a client area of `view` pixels, and whoever calls this
    /// asks the guest for that size and waits ([`Self::wait_for_hold`]). Frames of that size
    /// stay out of sight until the window has taken it. Returns false when this step of the
    /// drag does not wait: the window just takes the size.
    pub fn hold_view(&self, view: (u32, u32)) -> bool {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let arrived = *self.shown.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed
            || state.view == view
            || arrived == (0, 0)
            || arrived == view
            || !state.sync.should_wait(now)
        {
            return false;
        }
        self.size_asked(&mut state, view);
        state.hold = Some(Hold {
            view,
            since: now,
            staged: false,
        });
        true
    }

    /// Waits until the frame of the size last held for is in, for as long as the budget
    /// allows from `since`. Called on the window's thread, which holds no lock of the
    /// presenter while it waits, so the connection thread takes frames meanwhile.
    pub fn wait_for_hold(&self, view: (u32, u32), since: Instant) {
        let budget = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.sync.budget()
        };
        let in_time = {
            let mut shown = self.shown.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                match resize_sync::step(view, *shown, since.elapsed(), budget) {
                    Step::Show => break true,
                    Step::GiveUp => break false,
                    Step::Wait(left) => {
                        shown = self
                            .arrived
                            .wait_timeout(shown, left)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                    }
                }
            }
        };
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.sync.step_ended(in_time, Instant::now());
    }

    /// The window's position is settled: a hold it did not end by taking the size ends now,
    /// and a frame kept for it is shown as any late frame is.
    pub fn release_hold(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if !state.closed {
            self.show_hold(&mut state);
        }
    }

    /// The user took hold of the window's edge (`true`) or let go.
    pub fn dragging(&self, active: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if active {
            state.sync.drag_started();
        } else {
            state.sync.drag_ended();
        }
    }

    /// The guest is being asked for `view`: where the wait for a frame of that size starts.
    fn size_asked(&self, state: &mut State, view: (u32, u32)) {
        let now = Instant::now();
        state.latency.view_changed(view, now);
        state.sync.size_asked(view, now);
        trace_stage(
            self.logger.as_deref(),
            self.task_id,
            "app.view",
            view,
            &format!("frame={}x{}", state.frame.0, state.frame.1),
        );
    }

    /// Ends the hold, presenting the frame kept for it if one is.
    fn show_hold(&self, state: &mut State) {
        let Some(hold) = state.hold.take() else {
            return;
        };
        if !hold.staged {
            return;
        }
        let started = Instant::now();
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        let result = unsafe { self.present_back_buffer(state, hold.view) };
        match result {
            Ok(new_size) => self.frame_shown(state, hold.view, new_size, None, started),
            Err(why) => note(self.logger.as_deref(), &why),
        }
    }

    /// Waits until the compositor has taken the last commit. Holds no lock of the presenter's
    /// state, so the connection thread takes frames meanwhile.
    fn wait_for_commit(&self) {
        let Ok(dcomp) = self.gpu.dcomp() else {
            return;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: a plain COM call on the device, under its lock.
        unsafe {
            let _ = dcomp.WaitForCommitCompletion();
        }
    }

    /// Time has passed: a window left showing a stretched frame is logged, once per size.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        if state
            .hold
            .as_ref()
            .is_some_and(|hold| hold.since.elapsed() > HOLD_FORGOTTEN)
        {
            self.show_hold(&mut state);
        }
        self.trace_shown(&mut state);
        if let Some(((width, height), waited)) = state.latency.stuck(Instant::now()) {
            note(
                self.logger.as_deref(),
                &format!(
                    "a task window shows its last frame stretched: no {width}x{height} frame \
                     came in the {} ms since it took that size",
                    waited.as_millis()
                ),
            );
        }
    }

    /// The window is closing: nothing more is shown in it.
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
        let Ok(dcomp) = self.gpu.dcomp() else {
            return;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.target.SetRoot(None::<&IDCompositionVisual>);
            let _ = dcomp.Commit();
        }
    }

    /// Shows the frame in `texture`, which fills its top-left `content` pixels (0 when the
    /// frame fills the whole texture), and returns once it has been copied out, so the
    /// texture may be written again.
    pub fn present(&self, texture: &OpenedTexture, content: (u32, u32)) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Ok(());
        }
        let region = content_region(content, texture.size);
        let started = Instant::now();
        let hold = state.hold.as_ref().map(|hold| (hold.view, hold.staged));
        let action = frame_action(hold, region);
        if action == FrameAction::Drop {
            state.sync.frame_arrived(region, Instant::now());
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_left_out",
                region,
                &format!("held={:?}", hold.map(|(view, _)| view)),
            );
            return Ok(());
        }
        let stage = action == FrameAction::Stage;
        let shown =
            self.present_locked(&mut state, &texture.texture, texture.size, region, stage)?;
        self.trace_frame_colour(&mut state, &texture.texture, region);
        if !stage {
            self.watch_press(&mut state, &texture.texture, region);
        }
        state.sync.frame_arrived(region, Instant::now());
        if let Some(hold) = state.hold.as_mut() {
            hold.staged = stage;
        }
        if !stage {
            self.frame_shown(
                &mut state,
                region,
                shown.new_size,
                shown.resize_buffers,
                started,
            );
        }
        // After the frame is in, so a window that waits for this frame shows it as soon as it
        // wakes.
        *self.shown.lock().unwrap_or_else(|e| e.into_inner()) = region;
        self.arrived.notify_all();
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
            self.gpu
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
        let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
        let whole = D3D11_BOX {
            left: 0,
            top: 0,
            front: 0,
            right: width,
            bottom: height,
            back: 1,
        };
        context.CopySubresourceRegion(
            last,
            0,
            0,
            0,
            0,
            texture,
            0,
            Some(&whole as *const _),
        );
        Ok(())
    }

    /// The pixels of `area` of `source`, as tightly packed RGBA rows. Waits for the GPU.
    unsafe fn read_area(
        &self,
        state: &mut State,
        source: &ID3D11Texture2D,
        area: Area,
    ) -> Result<Vec<u8>, String> {
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
            self.gpu
                .device
                .CreateTexture2D(&desc, None, Some(&mut state.readback))
                .map_err(|e| failed("creating the texture a press's area is read into", e))?;
        }
        let Some(readback) = &state.readback else {
            return Err("no texture to read a press's area into".into());
        };
        let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
        let from = D3D11_BOX {
            left: area.left,
            top: area.top,
            front: 0,
            right: area.right,
            bottom: area.bottom,
            back: 1,
        };
        context.CopySubresourceRegion(
            readback,
            0,
            0,
            0,
            0,
            source,
            0,
            Some(&from as *const _),
        );
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
            self.gpu
                .device
                .CreateTexture2D(&desc, None, Some(&mut state.samples))
                .map_err(|e| failed("creating the texture a frame is sampled into", e))?;
        }
        let Some(samples) = &state.samples else {
            return Ok(None);
        };
        let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
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
                    "waited={} resize_buffers={} present={} budget={}",
                    millis(waited),
                    resized.map(millis).unwrap_or_else(|| "0.0".into()),
                    millis(presented.saturating_duration_since(started)),
                    millis(state.sync.budget())
                ),
            );
            if state.latency.count() % SUMMARY_EVERY == 0 {
                if let Some(summary) = state.latency.summary() {
                    note(self.logger.as_deref(), &format!("task window: {summary}"));
                }
            }
        }
    }

    /// Scales the visual so a frame of any size fills the client area, and commits it, unless
    /// that is what is on the compositor already. Returns whether it committed. A present
    /// and a commit reach the compositor separately, so this comes right after the present
    /// that goes with it.
    fn commit_visual(&self, state: &mut State) -> bool {
        let Some(buffers) = state.buffers else {
            // Nothing to show yet: the first frame commits it.
            return false;
        };
        let wanted = (buffers, state.view);
        if state.committed == Some(wanted) {
            return false;
        }
        let (x, y) = visual_scale(buffers, state.view);
        let matrix = Matrix3x2 {
            M11: x,
            M12: 0.0,
            M21: 0.0,
            M22: y,
            M31: 0.0,
            M32: 0.0,
        };
        let Ok(dcomp) = self.gpu.dcomp() else {
            return false;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.visual.SetTransform2(&matrix);
            let _ = dcomp.Commit();
        }
        state.committed = Some(wanted);
        true
    }

    /// Presents what is in the back buffer, of `size` pixels. Returns whether that size
    /// differs from the last one shown.
    unsafe fn present_back_buffer(
        &self,
        state: &mut State,
        (width, height): (u32, u32),
    ) -> Result<bool, String> {
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
        let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
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
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        unsafe {
            let started = Instant::now();
            // The buffers follow the texture, which keeps its size while the window is
            // resized; only the source size follows the frame.
            let change = swapchain_change(state.buffers, texture_size);
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
                        BufferCount: swapchain_buffers(),
                        // The only scaling a composition swapchain takes; the visual's
                        // transform does the scaling.
                        Scaling: DXGI_SCALING_STRETCH,
                        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
                        AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                        Flags: 0,
                    };
                    let swapchain = self
                        .gpu
                        .factory
                        .CreateSwapChainForComposition(&self.gpu.device, &desc, None)
                        .map_err(|e| failed("creating a task window's swapchain", e))?;
                    let swapchain: IDXGISwapChain2 = swapchain
                        .cast()
                        .map_err(|e| failed("reaching a task window's IDXGISwapChain2", e))?;
                    {
                        let _dcomp = self.gpu.dcomp()?.lock().unwrap_or_else(|e| e.into_inner());
                        state
                            .visual
                            .SetContent(&swapchain)
                            .map_err(|e| failed("showing a task window's swapchain", e))?;
                    }
                    state.swapchain = Some(swapchain);
                    // The content is new: the first present commits it.
                    state.committed = None;
                }
                SwapchainChange::Resize((buffer_width, buffer_height)) => {
                    if let Some(swapchain) = &state.swapchain {
                        swapchain
                            .ResizeBuffers(
                                0,
                                buffer_width,
                                buffer_height,
                                DXGI_FORMAT_UNKNOWN,
                                DXGI_SWAP_CHAIN_FLAG(0),
                            )
                            .map_err(|e| failed("resizing a task window's swapchain", e))?;
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
                let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
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
            let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
            // The renderer writes the shared texture again once this returns, so the copy
            // out of it must be finished.
            if state.query.is_none() {
                let mut query = None;
                let desc = D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                };
                let _ = self.gpu.device.CreateQuery(&desc, Some(&mut query));
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
