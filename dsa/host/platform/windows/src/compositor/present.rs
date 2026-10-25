//! Direct3D 11 and DirectComposition: how a task's frames appear in its window.
//!
//! Each task window has a DirectComposition visual holding a flip-model swapchain. The
//! renderer copies the task's frame into a shared texture on the GPU, and
//! [`WindowPresenter::present`] copies that into the swapchain and presents it. The visual
//! is scaled to fill the window, so while the user resizes it, the last frame is stretched
//! over the new size until a frame of that size arrives. No frame passes through the CPU.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::task_windows::fill_scale;
use windows::core::{Interface, BOOL, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HMODULE, HWND, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Query, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_RESOURCE_MISC_SHARED,
    D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice, IDCompositionDevice, IDCompositionTarget, IDCompositionVisual,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE_IGNORE, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIAdapter, IDXGIDevice, IDXGIFactory2, IDXGIFactory4, IDXGIResource1,
    IDXGISwapChain1, DXGI_CREATE_FACTORY_FLAGS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE, DXGI_SWAP_CHAIN_DESC1,
    DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL, DXGI_USAGE_RENDER_TARGET_OUTPUT,
};
use windows_numerics::Matrix3x2;

use super::frames::FrameTarget;
use super::log::{note, CompositorLogger};

/// How long a present waits for the GPU to finish reading the shared texture.
const COPY_WAIT: Duration = Duration::from_millis(500);

fn failed(what: &str, error: windows::core::Error) -> String {
    format!("{what}: {error}")
}

/// The Direct3D 11 device and DirectComposition device every task window shares.
pub struct Gpu {
    device: ID3D11Device,
    context: Mutex<ID3D11DeviceContext>,
    factory: IDXGIFactory2,
    dcomp: Mutex<IDCompositionDevice>,
    /// Whether the device is on the GPU the renderer named; frames cannot cross GPUs.
    pub on_renderer_gpu: bool,
}

// SAFETY: the Direct3D 11 device and the DXGI factory are free-threaded. The immediate
// context and the DirectComposition device are used only under their locks.
unsafe impl Send for Gpu {}
unsafe impl Sync for Gpu {}

impl Gpu {
    /// Creates the devices on the GPU `luid` names (the renderer's), else on the default
    /// GPU, else on the software rasteriser.
    pub fn new(luid: Option<[u8; 8]>) -> Result<Arc<Gpu>, String> {
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
                        let dxgi: IDXGIDevice = device
                            .cast()
                            .map_err(|e| failed("reaching the DXGI device", e))?;
                        let dcomp: IDCompositionDevice = DCompositionCreateDevice(&dxgi)
                            .map_err(|e| failed("creating the DirectComposition device", e))?;
                        return Ok(Arc::new(Gpu {
                            device,
                            context: Mutex::new(context),
                            factory,
                            dcomp: Mutex::new(dcomp),
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
                texture,
                handle,
                size: (width, height),
            })
        }
    }
}

/// A texture the renderer copies a task's frame into, opened by its shared NT handle. The
/// handle closes when the last reference goes.
pub struct SharedTexture {
    texture: ID3D11Texture2D,
    handle: HANDLE,
    size: (u32, u32),
}

// SAFETY: a Direct3D 11 resource may be used from any thread; the handle is a kernel handle.
unsafe impl Send for SharedTexture {}
unsafe impl Sync for SharedTexture {}

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
    swapchain: Option<IDXGISwapChain1>,
    /// The size of the swapchain's buffers: the last frame's size.
    frame: (u32, u32),
    /// The size of the window's client area.
    view: (u32, u32),
    texture: Option<Arc<SharedTexture>>,
    query: Option<ID3D11Query>,
    closed: bool,
}

/// Shows one task's frames in its window.
pub struct WindowPresenter {
    gpu: Arc<Gpu>,
    state: Mutex<State>,
    logger: Option<Arc<CompositorLogger>>,
}

// SAFETY: every COM object in the state is used under the state's lock, and the
// DirectComposition device under its own.
unsafe impl Send for WindowPresenter {}
unsafe impl Sync for WindowPresenter {}

impl WindowPresenter {
    /// Puts a visual on the window `hwnd`, whose client area is `view` pixels. Called on the
    /// thread that owns the window.
    pub fn new(
        gpu: Arc<Gpu>,
        hwnd: isize,
        view: (u32, u32),
        logger: Option<Arc<CompositorLogger>>,
    ) -> Result<Arc<Self>, String> {
        let dcomp = gpu.dcomp.lock().unwrap_or_else(|e| e.into_inner());
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
            gpu,
            state: Mutex::new(State {
                target,
                visual,
                swapchain: None,
                frame: (0, 0),
                view,
                texture: None,
                query: None,
                closed: false,
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
        if state.swapchain.is_some() {
            self.scale(&state, true);
        }
    }

    /// The window is closing: nothing more is shown in it.
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        state.texture = None;
        let dcomp = self.gpu.dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.target.SetRoot(None::<&IDCompositionVisual>);
            let _ = dcomp.Commit();
        }
    }

    /// Scales the visual so the frame fills the client area, and commits it. `wait` waits
    /// until the compositor has taken the change, so a live resize shows it with the new
    /// window size.
    fn scale(&self, state: &State, wait: bool) {
        let (x, y) = fill_scale(state.frame, state.view);
        let matrix = Matrix3x2 {
            M11: x,
            M12: 0.0,
            M21: 0.0,
            M22: y,
            M31: 0.0,
            M32: 0.0,
        };
        let dcomp = self.gpu.dcomp.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: plain COM calls on objects this presenter owns.
        unsafe {
            let _ = state.visual.SetTransform2(&matrix);
            let _ = dcomp.Commit();
            if wait {
                let _ = dcomp.WaitForCommitCompletion();
            }
        }
    }

    fn present_locked(&self, state: &mut State, texture: &SharedTexture) -> Result<(), String> {
        let (width, height) = texture.size;
        // SAFETY: plain COM calls on objects this presenter owns, and the immediate context
        // under its lock.
        unsafe {
            let resized = match &state.swapchain {
                None => {
                    let desc = DXGI_SWAP_CHAIN_DESC1 {
                        Width: width,
                        Height: height,
                        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                        Stereo: BOOL(0),
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                        BufferCount: 2,
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
                    {
                        let _dcomp = self.gpu.dcomp.lock().unwrap_or_else(|e| e.into_inner());
                        state
                            .visual
                            .SetContent(&swapchain)
                            .map_err(|e| failed("showing a task window's swapchain", e))?;
                    }
                    state.swapchain = Some(swapchain);
                    true
                }
                Some(swapchain) if state.frame != (width, height) => {
                    swapchain
                        .ResizeBuffers(
                            0,
                            width,
                            height,
                            DXGI_FORMAT_UNKNOWN,
                            DXGI_SWAP_CHAIN_FLAG(0),
                        )
                        .map_err(|e| failed("resizing a task window's swapchain", e))?;
                    true
                }
                Some(_) => false,
            };
            state.frame = (width, height);
            let Some(swapchain) = &state.swapchain else {
                unreachable!("the swapchain was made above");
            };
            let back: ID3D11Texture2D = swapchain
                .GetBuffer(0)
                .map_err(|e| failed("reaching a task window's back buffer", e))?;
            let context = self.gpu.context.lock().unwrap_or_else(|e| e.into_inner());
            context.CopyResource(&back, &texture.texture);
            drop(back);
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
            if resized {
                self.scale(state, false);
            }
        }
        Ok(())
    }
}

impl FrameTarget for WindowPresenter {
    type Texture = SharedTexture;

    fn texture(&self, width: u32, height: u32) -> Option<Arc<SharedTexture>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return None;
        }
        if let Some(texture) = &state.texture {
            if texture.size == (width, height) {
                return Some(Arc::clone(texture));
            }
        }
        match self.gpu.shared_texture(width, height) {
            Ok(texture) => {
                let texture = Arc::new(texture);
                state.texture = Some(Arc::clone(&texture));
                Some(texture)
            }
            Err(why) => {
                note(self.logger.as_deref(), &why);
                state.texture = None;
                None
            }
        }
    }

    fn handle(texture: &SharedTexture) -> isize {
        texture.handle.0 as isize
    }

    fn present(&self, texture: &SharedTexture) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        if let Err(why) = self.present_locked(&mut state, texture) {
            note(self.logger.as_deref(), &why);
        }
    }
}
