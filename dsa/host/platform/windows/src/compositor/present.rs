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
//! whole of the buffers, so the visual scales from the buffers' size to the client area.
//!
//! Each presenter measures how long its window waited for a frame of its new size
//! (`ResizeLatency`) and logs a summary every 20 resizes and when the window closes, and a
//! warning when a window is left showing a stretched frame.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::frame_buffers::{
    content_region, swapchain_change, visual_scale, SwapchainChange,
};
use dsa_host_core::compositor::resize_latency::ResizeLatency;
use dsa_host_core::compositor::task_windows::fill_scale;
use windows::core::{Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Device1, ID3D11DeviceContext, ID3D11Query,
    ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_BOX,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIFactory4, IDXGIResource1,
    IDXGISwapChain2, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Matrix3x2;

use super::log::{note, trace_stage, trace_summary, CompositorLogger};
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
    query: Option<ID3D11Query>,
    closed: bool,
    /// How long the window waits for a frame of its size after each resize.
    latency: ResizeLatency,
}

/// How many resizes go between two summaries of their waits in the log.
const SUMMARY_EVERY: usize = 20;

/// Shows one task's frames in its window.
pub struct WindowPresenter {
    task_id: i32,
    gpu: Arc<Gpu>,
    state: Mutex<State>,
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
            gpu,
            state: Mutex::new(State {
                target,
                visual,
                swapchain: None,
                buffers: None,
                frame: (0, 0),
                view,
                query: None,
                closed: false,
                latency: ResizeLatency::new(),
            }),
            logger,
        }))
    }

    /// The client area is now `view` pixels: the last frame is stretched over it.
    pub fn view_resized(&self, view: (u32, u32)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.view == view {
            return;
        }
        state.view = view;
        state.latency.view_changed(view, Instant::now());
        // Where a resize's wait starts: the client area took the size.
        trace_stage(
            self.logger.as_deref(),
            self.task_id,
            "app.view",
            view,
            &format!("frame={}x{}", state.frame.0, state.frame.1),
        );
        if state.swapchain.is_some() {
            self.scale(&state, true);
        }
    }

    /// Time has passed: a window left showing a stretched frame is logged, once per size.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
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
        let shown = self.present_locked(&mut state, &texture.texture, texture.size, region)?;
        let presented = Instant::now();
        if shown.new_size {
            // Once per size: what showing a frame of a new size cost. `resize_buffers` is 0
            // unless the texture itself changed size.
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.present",
                region,
                &format!(
                    "buffers={}x{} resize_buffers={} present={}",
                    texture.size.0,
                    texture.size.1,
                    millis(shown.resize_buffers.unwrap_or_default()),
                    millis(presented.saturating_duration_since(started))
                ),
            );
        }
        let resized = shown.resize_buffers;
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
        Ok(())
    }

    /// Scales the visual so the frame fills the client area, and commits it. `wait` waits
    /// until the compositor has taken the change, so a live resize shows it with the new
    /// window size.
    fn scale(&self, state: &State, wait: bool) {
        let (x, y) = match state.buffers {
            Some(buffers) => visual_scale(buffers, state.view),
            None => fill_scale(state.frame, state.view),
        };
        let matrix = Matrix3x2 {
            M11: x,
            M12: 0.0,
            M21: 0.0,
            M22: y,
            M31: 0.0,
            M32: 0.0,
        };
        let Ok(dcomp) = self.gpu.dcomp() else {
            return;
        };
        let dcomp = dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.visual.SetTransform2(&matrix);
            let _ = dcomp.Commit();
            if wait {
                let _ = dcomp.WaitForCommitCompletion();
            }
        }
    }

    fn present_locked(
        &self,
        state: &mut State,
        texture: &ID3D11Texture2D,
        texture_size: (u32, u32),
        (width, height): (u32, u32),
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
                        BufferCount: 2,
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
            state.buffers = Some(texture_size);
            // How long making or resizing the swapchain took, when it happened.
            let resize_buffers = remade.then(|| started.elapsed());
            let new_size = remade || state.frame != (width, height);
            let Some(swapchain) = &state.swapchain else {
                unreachable!("the swapchain was made above");
            };
            if new_size {
                // Made or resized buffers show all of themselves until told otherwise.
                swapchain
                    .SetSourceSize(width, height)
                    .map_err(|e| failed("setting the size a task window shows", e))?;
            }
            state.frame = (width, height);
            let back: ID3D11Texture2D = swapchain
                .GetBuffer(0)
                .map_err(|e| failed("reaching a task window's back buffer", e))?;
            let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
            let region = D3D11_BOX {
                left: 0,
                top: 0,
                front: 0,
                right: width,
                bottom: height,
                back: 1,
            };
            context.CopySubresourceRegion(&back, 0, 0, 0, 0, texture, 0, Some(&region as *const _));
            drop(back);
            // Sync interval 0: the present is queued without waiting for a vertical blank.
            swapchain
                .Present(0, DXGI_PRESENT(0))
                .ok()
                .map_err(|e| failed("presenting a task window's frame", e))?;
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
            if new_size {
                self.scale(state, false);
            }
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
