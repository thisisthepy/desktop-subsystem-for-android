//! The window manager's Direct3D 11 devices, and the textures task frames arrive in.
//!
//! The window manager makes a [`SharedTexture`] for each task window and the renderer copies
//! the task's frame into it on the GPU. The window manager then copies each frame from there
//! into the window's swapchain (`surfaces`), so the app window programs make no Direct3D
//! device of their own. A GPU reset removes the devices, and [`Gpu`] makes new ones.

use std::sync::{Arc, Mutex};

use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, E_FAIL, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_UNKNOWN, D3D_DRIVER_TYPE_WARP,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D,
    D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory2, IDXGIAdapter, IDXGIAdapter3, IDXGIDevice, IDXGIDevice3, IDXGIFactory2,
    IDXGIFactory4, IDXGIResource1, DXGI_CREATE_FACTORY_FLAGS, DXGI_MEMORY_SEGMENT_GROUP_LOCAL,
    DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL, DXGI_QUERY_VIDEO_MEMORY_INFO, DXGI_SHARED_RESOURCE_READ,
    DXGI_SHARED_RESOURCE_WRITE,
};

use super::device_recovery::{Recovering, Removable};

fn failed(what: &str, error: windows::core::Error) -> String {
    format!("{what}: {error}")
}

/// One set of devices: the Direct3D 11 device and the DXGI factory. A GPU reset removes them,
/// and [`Gpu`] makes a new set.
pub struct Devices {
    pub(super) device: ID3D11Device,
    pub(super) context: Mutex<ID3D11DeviceContext>,
    pub(super) factory: IDXGIFactory2,
    /// Whether the device is on the GPU the renderer named; frames cannot cross GPUs.
    on_renderer_gpu: bool,
}

// SAFETY: the Direct3D 11 device and the DXGI factory are free-threaded. The immediate
// context is used only under its lock.
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
    /// GPU, else on the software rasteriser. A GPU reset makes them again the same way.
    pub fn new(luid: Option<[u8; 8]>) -> Result<Arc<Gpu>, String> {
        let devices = Recovering::new(Box::new(move || Devices::new(luid)))?;
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
}

impl Devices {
    fn new(luid: Option<[u8; 8]>) -> Result<Devices, String> {
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
                        // The swapchains' calls and the copies of every task window run on
                        // the threads of their window hosts. The runtime serialises each call
                        // of the device and its context; the lock around the context orders
                        // the sequences of calls that must not interleave.
                        if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
                            let _ = multithread.SetMultithreadProtected(true);
                        }
                        return Ok(Devices {
                            device,
                            context: Mutex::new(context),
                            factory,
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
}

/// A texture the renderer copies a task's frame into, opening it by its shared NT handle, and
/// the window manager copies the frame out of into the task window's swapchain. The handle closes when the last reference goes.
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

    /// The texture itself, on the window manager's devices.
    pub(super) fn texture(&self) -> &ID3D11Texture2D {
        &self._texture
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
