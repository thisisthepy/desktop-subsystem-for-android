//! The window manager's side of each task window's surfaces: the GPU work an app window host
//! asks for.
//!
//! An app window host makes no Direct3D device and no DXGI factory: a device costs each app
//! process tens of MiB of driver memory, and the window manager already has one with every
//! task's frame texture on it. So the window manager owns each task window's swapchains, one
//! for the frame, one for the caption buttons, one for the splash and a one-pixel one for the
//! fill under the frame, each made for a DirectComposition surface handle
//! (`DCompositionCreateSurfaceHandle`, `IDXGIFactoryMedia::CreateSwapChainForCompositionSurfaceHandle`).
//! Each handle is placed in the window host's process, which shows the surface in the window's
//! visual tree (`IDCompositionDevice::CreateSurfaceFromHandle`) and keeps the tree, the
//! transforms and the commits to itself. The window host sends a `SurfaceCall` for each thing to
//! do on the GPU and gets a `SurfaceAnswer` back ([`WindowSurfaces::call`]).
//!
//! A swapchain made again (the first frame, a restored window, a GPU reset) comes with a new
//! surface handle in the answer; the window host shows the new surface from then on. Until
//! then the old surface keeps what it last showed, so the window keeps its last frame.
//!
//! What reads a frame's pixels runs here too, since only this process has the frame on a
//! device: the band under the title bar the caption takes its colour from, the frame read back
//! for a screenshot, and, while the trace is on, the frame's colour and the first visible
//! change after a press.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::caption::{
    band_points, band_reading_owed, band_sample, describe_band_sample, BandSample, BAND_SAMPLES,
};
use dsa_host_core::compositor::frame_buffers::{
    copy_extent, image_bytes, sample_points, swapchain_change, uniform_colour, upload_problem,
    SwapchainChange, SAMPLE_GRID,
};
use dsa_host_core::compositor::splash;
use dsa_host_core::compositor::surface_calls::{
    band_to_answer, known_op, read_parts, READ_PART_BYTES, SURFACE_OP_CAPTION, SURFACE_OP_FILL,
    SURFACE_OP_PRESENT_FRAME, SURFACE_OP_PRESENT_KEPT, SURFACE_OP_READ_FRAME,
    SURFACE_OP_RELEASE_ALL, SURFACE_OP_RELEASE_FRAME, SURFACE_OP_RELEASE_SPLASH, SURFACE_OP_SPLASH,
    SURFACE_OP_TICK,
};
use dsa_host_core::compositor::visible_change::{self, area_around, Area, PressWatch, Verdict};
use dsa_host_core::resize_trace::millis;
use dsa_protocol::window_host::types::{SurfaceAnswer, SurfaceCall};
use windows::core::{Interface, BOOL};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Query, ID3D11Texture2D, D3D11_BOX, D3D11_CPU_ACCESS_READ, D3D11_MAPPED_SUBRESOURCE,
    D3D11_MAP_FLAG_DO_NOT_WAIT, D3D11_MAP_READ, D3D11_QUERY_DESC, D3D11_QUERY_EVENT,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateSurfaceHandle, COMPOSITIONOBJECT_READ, COMPOSITIONOBJECT_WRITE,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_ALPHA_MODE, DXGI_ALPHA_MODE_IGNORE, DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT,
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_UNKNOWN, DXGI_SAMPLE_DESC,
};
use windows::Win32::Graphics::Dxgi::{
    IDXGIFactoryMedia, IDXGISwapChain2, DXGI_ERROR_FRAME_STATISTICS_DISJOINT,
    DXGI_ERROR_WAS_STILL_DRAWING, DXGI_FRAME_STATISTICS, DXGI_PRESENT, DXGI_SCALING_STRETCH,
    DXGI_SWAP_CHAIN_DESC1, DXGI_SWAP_CHAIN_FLAG, DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
    DXGI_USAGE_RENDER_TARGET_OUTPUT,
};

use super::log::{
    host_clock, input_trace, input_trace_on, note, trace_detailed, trace_stage, CompositorLogger,
};
use super::present::{Devices, Gpu, SharedTexture};

/// What a call answers when the frame it names is not in the texture the window manager has
/// for the task now: the renderer made a new texture meanwhile (it restarted, or a GPU reset).
/// The window host shows nothing for that frame and does not log it.
pub const STALE_FRAME: &str = "the frame is in a texture that has been replaced";

/// What the window manager's blank watch shares with the window hosts' threads: the tasks
/// whose frames are looked at, and whether the newest frame of each was one colour.
struct BlankFrames {
    watched: std::collections::BTreeSet<i32>,
    blank: std::collections::BTreeMap<i32, bool>,
}

static BLANK_FRAMES: std::sync::Mutex<BlankFrames> = std::sync::Mutex::new(BlankFrames {
    watched: std::collections::BTreeSet::new(),
    blank: std::collections::BTreeMap::new(),
});

/// Sets the tasks whose frames are looked at as they are shown (the others cost nothing).
pub fn watch_blank_frames(tasks: &[i32]) {
    let mut frames = BLANK_FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    frames.watched = tasks.iter().copied().collect();
    let watched = frames.watched.clone();
    frames.blank.retain(|task_id, _| watched.contains(task_id));
}

/// Whether the newest frame shown of each watched task was one colour, for the tasks that
/// have had one.
pub fn blank_frames() -> Vec<(i32, bool)> {
    let frames = BLANK_FRAMES.lock().unwrap_or_else(|e| e.into_inner());
    frames
        .blank
        .iter()
        .map(|(task, blank)| (*task, *blank))
        .collect()
}

fn blank_frame_watched(task_id: i32) -> bool {
    BLANK_FRAMES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .watched
        .contains(&task_id)
}

fn note_blank_frame(task_id: i32, blank: bool) {
    BLANK_FRAMES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .blank
        .insert(task_id, blank);
}

/// How long a present waits for the GPU to finish reading the shared texture.
const COPY_WAIT: Duration = Duration::from_millis(500);

/// How often the band is sampled at most, while frames come.
const BAND_PROBE_EVERY: Duration = Duration::from_millis(200);

/// A copy of the band still unread after this long is said in the log, once.
const BAND_COPY_SLOW: Duration = Duration::from_secs(2);

fn failed(what: &str, error: windows::core::Error) -> String {
    format!("{what}: {error}")
}

/// A DirectComposition surface handle of this process, closed when dropped.
struct SurfaceHandle(HANDLE);

impl Drop for SurfaceHandle {
    fn drop(&mut self) {
        // SAFETY: a handle this process created, closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A swapchain made for a DirectComposition surface handle, and the handle.
struct Chain {
    swapchain: IDXGISwapChain2,
    /// Dropped after the swapchain, so the surface outlives what draws into it.
    _surface: SurfaceHandle,
    size: (u32, u32),
    count: u32,
}

/// Places a handle of this process in the window host's process and returns its value there.
pub type Place<'a> = &'a dyn Fn(isize) -> Option<u64>;

/// Makes a swapchain of `count` buffers of `size` for a new DirectComposition surface handle,
/// and returns it with the handle.
///
/// # Safety
///
/// Plain COM calls on `devices`, which the caller keeps alive.
unsafe fn make_chain(
    devices: &Devices,
    size: (u32, u32),
    count: u32,
    format: DXGI_FORMAT,
    alpha: DXGI_ALPHA_MODE,
) -> Result<Chain, String> {
    let surface = DCompositionCreateSurfaceHandle(
        (COMPOSITIONOBJECT_READ | COMPOSITIONOBJECT_WRITE) as u32,
        None,
    )
    .map_err(|e| failed("creating a task window's surface handle", e))?;
    let surface = SurfaceHandle(surface);
    let media: IDXGIFactoryMedia = devices
        .factory
        .cast()
        .map_err(|e| failed("reaching IDXGIFactoryMedia", e))?;
    let desc = DXGI_SWAP_CHAIN_DESC1 {
        Width: size.0.max(1),
        Height: size.1.max(1),
        Format: format,
        Stereo: BOOL(0),
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
        BufferCount: count.max(2),
        // The only scaling a composition swapchain takes; the window host's visual does the
        // scaling.
        Scaling: DXGI_SCALING_STRETCH,
        SwapEffect: DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
        AlphaMode: alpha,
        Flags: 0,
    };
    let swapchain = media
        .CreateSwapChainForCompositionSurfaceHandle(&devices.device, Some(surface.0), &desc, None)
        .map_err(|e| failed("creating a task window's swapchain", e))?;
    let swapchain: IDXGISwapChain2 = swapchain
        .cast()
        .map_err(|e| failed("reaching a task window's IDXGISwapChain2", e))?;
    Ok(Chain {
        swapchain,
        _surface: surface,
        size,
        count: count.max(2),
    })
}

/// Writes `pixels` (rows of 4-byte pixels, `size` large) into `chain`'s back buffer and
/// presents it.
///
/// # Safety
///
/// `chain` was made on `devices`; `pixels` holds the whole image.
unsafe fn upload(
    devices: &Devices,
    chain: &Chain,
    size: (u32, u32),
    pixels: &[u8],
) -> Result<(), String> {
    // A wrong size or too few bytes would read past what the driver was given.
    if let Some(why) = upload_problem(chain.size, size, pixels.len()) {
        return Err(why);
    }
    let place = D3D11_BOX {
        left: 0,
        top: 0,
        front: 0,
        right: size.0,
        bottom: size.1,
        back: 1,
    };
    // The swapchain's calls use the immediate context as the copies do, so every one of them
    // is made under its lock, never beside another thread's copy or trim.
    let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
    let back: ID3D11Texture2D = chain
        .swapchain
        .GetBuffer(0)
        .map_err(|e| failed("reaching a task window's back buffer", e))?;
    context.UpdateSubresource(
        &back,
        0,
        Some(&place as *const D3D11_BOX),
        pixels.as_ptr() as *const c_void,
        size.0 * 4,
        0,
    );
    drop(back);
    chain
        .swapchain
        .Present(0, DXGI_PRESENT(0))
        .ok()
        .map_err(|e| failed("presenting a task window's surface", e))
}

/// One task window's swapchains and what reads its frames, on the window manager's devices.
pub struct WindowSurfaces {
    task_id: i32,
    gpu: Arc<Gpu>,
    devices: Arc<Devices>,
    generation: u64,
    frame: Option<Chain>,
    /// The source size the frame swapchain was last presented with.
    shown: (u32, u32),
    caption: Option<Chain>,
    splash: Option<Chain>,
    fill: Option<Chain>,
    query: Option<ID3D11Query>,
    /// The texture the newest frame came in and the region it fills.
    newest: Option<(Arc<SharedTexture>, (u32, u32))>,
    last_frame_at: Option<Instant>,
    /// The height of the title bar's band over the frame, in the frame's pixels.
    band: u32,
    band_probe: Option<ID3D11Texture2D>,
    band_pending: bool,
    band_probed: Option<Instant>,
    band_sample: BandSample,
    band_waits_noted: bool,
    /// While the trace is on: where a frame's sample pixels are read, a copy of the frame last
    /// presented, where the area around a press is read, and the press being watched.
    samples: Option<ID3D11Texture2D>,
    last_frame: Option<(ID3D11Texture2D, (u32, u32))>,
    readback: Option<ID3D11Texture2D>,
    watch: Option<(PressWatch, Duration)>,
    no_statistics: bool,
    logger: Option<Arc<CompositorLogger>>,
}

// SAFETY: every COM object here is used only by the thread that holds the window's entry in
// its link's map, under that map's lock; the immediate context is used under its own lock.
unsafe impl Send for WindowSurfaces {}

impl WindowSurfaces {
    pub fn new(
        task_id: i32,
        gpu: Arc<Gpu>,
        logger: Option<Arc<CompositorLogger>>,
    ) -> WindowSurfaces {
        let (devices, generation) = gpu.devices();
        WindowSurfaces {
            task_id,
            gpu,
            devices,
            generation,
            frame: None,
            shown: (0, 0),
            caption: None,
            splash: None,
            fill: None,
            query: None,
            newest: None,
            last_frame_at: None,
            band: 0,
            band_probe: None,
            band_pending: false,
            band_probed: None,
            band_sample: BandSample::default(),
            band_waits_noted: false,
            samples: None,
            last_frame: None,
            readback: None,
            watch: None,
            no_statistics: false,
            logger,
        }
    }

    /// Lets every swapchain and copy go, as when the devices they were made on are gone.
    fn drop_all(&mut self) {
        self.frame = None;
        self.shown = (0, 0);
        self.caption = None;
        self.splash = None;
        self.fill = None;
        self.query = None;
        self.newest = None;
        self.band_probe = None;
        self.band_pending = false;
        self.samples = None;
        self.last_frame = None;
        self.readback = None;
        self.watch = None;
    }

    /// Moves onto new devices when a GPU reset replaced the ones everything was made on.
    fn follow_gpu(&mut self) {
        match self.gpu.recover() {
            Ok(Some((reason, generation))) => note(
                self.logger.as_deref(),
                &format!(
                    "task {}: the GPU was reset (0x{:08x}); made new Direct3D devices (number \
                     {generation}), and the window's surfaces are made again",
                    self.task_id, reason as u32
                ),
            ),
            Ok(None) => {}
            Err(why) => note(
                self.logger.as_deref(),
                &format!(
                    "task {}: new Direct3D devices cannot be made yet ({why})",
                    self.task_id
                ),
            ),
        }
        let (devices, generation) = self.gpu.devices();
        if generation != self.generation {
            self.drop_all();
            self.devices = devices;
            self.generation = generation;
        }
    }

    /// Carries out `call` with `texture`, the task's frame texture now and its ID, and returns
    /// the answers: one, or several for a frame read back. Never panics on what the window host
    /// sends: an operation it does not know, or one that fails, is answered with an error.
    pub fn call(
        &mut self,
        call: &SurfaceCall,
        texture: Option<(Arc<SharedTexture>, u64)>,
        place: Place<'_>,
    ) -> Vec<SurfaceAnswer> {
        self.follow_gpu();
        let mut answer = SurfaceAnswer {
            task_id: call.task_id,
            call_id: call.call_id,
            generation: self.generation,
            ..Default::default()
        };
        let mut parts = Vec::new();
        let result = if !known_op(call.op) {
            Err(format!("unknown surface operation {}", call.op))
        } else {
            // SAFETY: plain COM calls on objects this window owns, made on its devices, and
            // the immediate context under its lock.
            unsafe { self.carry_out(call, texture, place, &mut answer, &mut parts) }
        };
        if let Err(why) = result {
            answer.error = why;
            if answer.error != STALE_FRAME {
                // A failure because a GPU reset removed the devices moves onto new ones at
                // the next call; the window host draws again when it sees the generation.
                let _ = self.gpu.recover();
            }
        }
        if let Some(frame) = &self.frame {
            answer.buffer_width = frame.size.0;
            answer.buffer_height = frame.size.1;
        }
        answer.band_pending = self.band_pending;
        if parts.is_empty() {
            return vec![answer];
        }
        // The parts of a frame read back carry the rest of the answer in the first.
        let last = parts.len() - 1;
        parts
            .into_iter()
            .enumerate()
            .map(|(index, part)| {
                let mut each = if index == 0 {
                    answer.clone()
                } else {
                    SurfaceAnswer {
                        task_id: answer.task_id,
                        call_id: answer.call_id,
                        generation: answer.generation,
                        has_frame: answer.has_frame,
                        frame_width: answer.frame_width,
                        frame_height: answer.frame_height,
                        ..Default::default()
                    }
                };
                each.rgba = part;
                each.more = index < last;
                each
            })
            .collect()
    }

    unsafe fn carry_out(
        &mut self,
        call: &SurfaceCall,
        texture: Option<(Arc<SharedTexture>, u64)>,
        place: Place<'_>,
        answer: &mut SurfaceAnswer,
        parts: &mut Vec<Vec<u8>>,
    ) -> Result<(), String> {
        match call.op {
            SURFACE_OP_PRESENT_FRAME => {
                let Some((texture, id)) = texture else {
                    return Err(STALE_FRAME.into());
                };
                if id != call.texture_id || texture.generation() != self.generation {
                    return Err(STALE_FRAME.into());
                }
                self.present_frame(call, &texture, place, answer, parts)
            }
            SURFACE_OP_PRESENT_KEPT => {
                self.present_back_buffer((call.width, call.height), answer)?;
                self.statistics(call, answer);
                Ok(())
            }
            SURFACE_OP_CAPTION => {
                let size = (call.width, call.height);
                if size.0 == 0 || size.1 == 0 {
                    return Ok(());
                }
                if (call.pixels.len() as u64) < image_bytes(size) {
                    return Err("the caption image is smaller than its size".into());
                }
                let made = self.ensure_image(Image::Caption, size, place, answer)?;
                let chain = self.caption.as_ref().ok_or("no caption swapchain")?;
                upload(&self.devices, chain, size, &call.pixels)?;
                let _ = made;
                Ok(())
            }
            SURFACE_OP_SPLASH => {
                let view = (call.width, call.height);
                if view.0 == 0 || view.1 == 0 {
                    return Ok(());
                }
                let icon_size = (call.icon_width, call.icon_height);
                let icon_bytes = image_bytes(icon_size);
                let icon = (!call.pixels.is_empty() && call.pixels.len() as u64 >= icon_bytes)
                    .then(|| splash::Icon {
                        size: icon_size,
                        pixels: &call.pixels,
                    });
                let source = icon.map_or(0, |icon| icon.size.0.min(icon.size.1));
                let pixels = splash::compose(
                    view,
                    call.colour,
                    icon,
                    splash::icon_edge(view, call.dpi.max(96), source),
                );
                self.ensure_image(Image::Splash, view, place, answer)?;
                let chain = self.splash.as_ref().ok_or("no splash swapchain")?;
                upload(&self.devices, chain, view, &pixels)
            }
            SURFACE_OP_FILL => {
                self.ensure_image(Image::Fill, (1, 1), place, answer)?;
                let colour = call.colour;
                // Opaque, so premultiplied BGRA is the colour's own bytes.
                let pixel = [
                    colour as u8,
                    (colour >> 8) as u8,
                    (colour >> 16) as u8,
                    0xFF,
                ];
                let chain = self.fill.as_ref().ok_or("no fill swapchain")?;
                upload(&self.devices, chain, (1, 1), &pixel)
            }
            SURFACE_OP_RELEASE_FRAME => {
                self.frame = None;
                self.shown = (0, 0);
                self.last_frame = None;
                self.readback = None;
                self.samples = None;
                // The driver keeps what the swapchain held until asked to give it back.
                self.gpu.trim();
                Ok(())
            }
            SURFACE_OP_RELEASE_SPLASH => {
                self.splash = None;
                Ok(())
            }
            SURFACE_OP_RELEASE_ALL => {
                self.drop_all();
                self.gpu.trim();
                Ok(())
            }
            SURFACE_OP_READ_FRAME => {
                let Some((texture, region)) = self.newest.clone() else {
                    return Ok(());
                };
                if texture.generation() != self.generation {
                    return Ok(());
                }
                let rgba = self.read_frame(texture.texture(), region)?;
                answer.has_frame = true;
                answer.frame_width = region.0;
                answer.frame_height = region.1;
                for range in read_parts(region.0, region.1, READ_PART_BYTES) {
                    parts.push(rgba[range].to_vec());
                }
                Ok(())
            }
            SURFACE_OP_TICK => {
                self.band = call.band;
                if let Some(sample) = self.settle_band() {
                    band_to_answer(sample, answer);
                }
                self.statistics(call, answer);
                Ok(())
            }
            _ => Err(format!("unknown surface operation {}", call.op)),
        }
    }

    /// Makes the swapchain of `image` at `size`, or resizes it to that. A new one is placed in
    /// the window host's process and named in `answer`. Returns whether it was made.
    unsafe fn ensure_image(
        &mut self,
        image: Image,
        size: (u32, u32),
        place: Place<'_>,
        answer: &mut SurfaceAnswer,
    ) -> Result<bool, String> {
        let devices = Arc::clone(&self.devices);
        let slot = match image {
            Image::Caption => &mut self.caption,
            Image::Splash => &mut self.splash,
            Image::Fill => &mut self.fill,
        };
        if let Some(chain) = slot.as_mut() {
            if chain.size == size {
                return Ok(false);
            }
            let _context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
            chain
                .swapchain
                .ResizeBuffers(
                    chain.count,
                    size.0,
                    size.1,
                    DXGI_FORMAT_UNKNOWN,
                    DXGI_SWAP_CHAIN_FLAG(0),
                )
                .map_err(|e| failed("resizing a task window's surface", e))?;
            chain.size = size;
            return Ok(false);
        }
        let chain = make_chain(
            &devices,
            size,
            2,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_ALPHA_MODE_PREMULTIPLIED,
        )?;
        let placed = place(chain._surface.0 .0 as isize)
            .ok_or("could not place a task window's surface handle in its window's program")?;
        match image {
            Image::Caption => answer.caption_surface = placed,
            Image::Splash => answer.splash_surface = placed,
            Image::Fill => answer.fill_surface = placed,
        }
        *slot = Some(chain);
        Ok(true)
    }

    unsafe fn present_frame(
        &mut self,
        call: &SurfaceCall,
        texture: &Arc<SharedTexture>,
        place: Place<'_>,
        answer: &mut SurfaceAnswer,
        parts: &mut Vec<Vec<u8>>,
    ) -> Result<(), String> {
        let devices = Arc::clone(&self.devices);
        let texture_size = texture.size();
        // A frame that says no size fills the whole texture; one that says more than the
        // texture holds is cut to it.
        let region = copy_extent((call.width, call.height), texture_size, texture_size)
            .ok_or("the frame has no pixels")?;
        self.newest = Some((Arc::clone(texture), region));
        self.last_frame_at = Some(Instant::now());
        self.band = call.band;
        let started = Instant::now();
        let wanted = call.buffer_count.max(2);
        let mut change = swapchain_change(self.frame.as_ref().map(|f| f.size), texture_size);
        if change == SwapchainChange::Keep && self.frame.as_ref().is_some_and(|f| f.count != wanted)
        {
            change = SwapchainChange::Resize(texture_size);
        }
        match change {
            SwapchainChange::Create(size) => {
                let chain = make_chain(
                    &devices,
                    size,
                    wanted,
                    DXGI_FORMAT_R8G8B8A8_UNORM,
                    // Android's buffers are premultiplied, as the desktop's compositor takes
                    // them.
                    if call.see_through {
                        DXGI_ALPHA_MODE_PREMULTIPLIED
                    } else {
                        DXGI_ALPHA_MODE_IGNORE
                    },
                )?;
                answer.frame_surface = place(chain._surface.0 .0 as isize).ok_or(
                    "could not place a task window's surface handle in its window's program",
                )?;
                self.frame = Some(chain);
            }
            SwapchainChange::Resize(size) => {
                if let Some(chain) = self.frame.as_mut() {
                    // ResizeBuffers flushes the immediate context: it runs under the lock, with
                    // no copy, trim or present of another window in progress, and with the
                    // context holding nothing of the old buffers.
                    let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
                    context.ClearState();
                    context.Flush();
                    chain
                        .swapchain
                        .ResizeBuffers(
                            wanted,
                            size.0,
                            size.1,
                            DXGI_FORMAT_UNKNOWN,
                            DXGI_SWAP_CHAIN_FLAG(0),
                        )
                        .map_err(|e| failed("resizing a task window's swapchain", e))?;
                    chain.size = size;
                    chain.count = wanted;
                }
            }
            SwapchainChange::Keep => {}
        }
        let remade = change != SwapchainChange::Keep;
        if remade {
            answer.remade = true;
            answer.resize_us = started.elapsed().as_micros() as u64;
            self.shown = (0, 0);
        }
        let chain = self.frame.as_ref().ok_or("no frame swapchain")?;
        {
            let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
            let back: ID3D11Texture2D = chain
                .swapchain
                .GetBuffer(0)
                .map_err(|e| failed("reaching a task window's back buffer", e))?;
            // The back buffer's own size, not the one this window believes it has: a copy
            // past either resource ends the process inside the driver.
            let mut back_desc = D3D11_TEXTURE2D_DESC::default();
            back.GetDesc(&mut back_desc);
            let (width, height) =
                copy_extent(region, texture_size, (back_desc.Width, back_desc.Height))
                    .ok_or("the window's back buffer is empty")?;
            let from = D3D11_BOX {
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
                texture.texture(),
                0,
                Some(&from as *const _),
            );
            drop(back);
        }
        if !call.keep {
            self.present_back_buffer(region, answer)?;
        }
        self.wait_for_copy();
        self.trace_frame_colour(texture.texture(), region);
        if blank_frame_watched(self.task_id) && !call.keep {
            // A failed read says nothing about the frame: the last answer stands.
            if let Ok(colour) = self.frame_colour(texture.texture(), region) {
                note_blank_frame(self.task_id, colour.is_some());
            }
        }
        if call.probe_band {
            if let Some(sample) = self.probe_band(texture.texture(), region, true) {
                band_to_answer(sample, answer);
            }
        }
        if call.capture {
            let rgba = self.read_frame(texture.texture(), region)?;
            answer.has_frame = true;
            answer.frame_width = region.0;
            answer.frame_height = region.1;
            for range in read_parts(region.0, region.1, READ_PART_BYTES) {
                parts.push(rgba[range].to_vec());
            }
        }
        if !call.keep {
            let press = call.press.then(|| {
                (
                    (call.press_x, call.press_y),
                    Duration::from_micros(call.press_at_us),
                )
            });
            self.watch_press(texture.texture(), region, press);
        }
        self.statistics(call, answer);
        Ok(())
    }

    /// Presents what is in the frame swapchain's back buffer, whose top-left `size` pixels hold
    /// the frame.
    unsafe fn present_back_buffer(
        &mut self,
        size: (u32, u32),
        answer: &mut SurfaceAnswer,
    ) -> Result<(), String> {
        let Some(chain) = &self.frame else {
            return Ok(());
        };
        let size = (
            size.0.min(chain.size.0).max(1),
            size.1.min(chain.size.1).max(1),
        );
        {
            let _context = self
                .devices
                .context
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.shown != size {
                chain
                    .swapchain
                    .SetSourceSize(size.0, size.1)
                    .map_err(|e| failed("setting the size a task window shows", e))?;
            }
            // Sync interval 0: the present is queued without waiting for a vertical blank.
            chain
                .swapchain
                .Present(0, DXGI_PRESENT(0))
                .ok()
                .map_err(|e| failed("presenting a task window's frame", e))?;
        }
        self.shown = size;
        answer.presented = true;
        if let Ok(count) = chain.swapchain.GetLastPresentCount() {
            answer.has_present_count = true;
            answer.present_count = count;
        }
        Ok(())
    }

    /// Waits until the GPU has copied out of the shared texture: the renderer writes it again
    /// once the window host answers this frame.
    unsafe fn wait_for_copy(&mut self) {
        let context = self
            .devices
            .context
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.query.is_none() {
            let mut query = None;
            let desc = D3D11_QUERY_DESC {
                Query: D3D11_QUERY_EVENT,
                MiscFlags: 0,
            };
            let _ = self.devices.device.CreateQuery(&desc, Some(&mut query));
            self.query = query;
        }
        let Some(query) = &self.query else {
            context.Flush();
            return;
        };
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

    /// The frame swapchain's statistics, into `answer`, when the call wants them.
    unsafe fn statistics(&mut self, call: &SurfaceCall, answer: &mut SurfaceAnswer) {
        if !call.statistics {
            return;
        }
        if self.no_statistics {
            answer.no_statistics = true;
            return;
        }
        let Some(chain) = &self.frame else {
            return;
        };
        let mut statistics = DXGI_FRAME_STATISTICS::default();
        match chain.swapchain.GetFrameStatistics(&mut statistics) {
            Ok(()) => {
                answer.has_statistics = true;
                answer.statistics_present_count = statistics.PresentCount;
                answer.statistics_sync_qpc = statistics.SyncQPCTime;
            }
            // The display path changed meanwhile; the next reading is good again.
            Err(error) if error.code() == DXGI_ERROR_FRAME_STATISTICS_DISJOINT => {}
            Err(_) => {
                self.no_statistics = true;
                answer.no_statistics = true;
            }
        }
    }

    /// Reads the band of the newest frame once frames stop coming, so the last frame of a
    /// burst is read too. Returns the band's new sample when it changed.
    unsafe fn settle_band(&mut self) -> Option<BandSample> {
        if !band_reading_owed(self.band_pending, self.last_frame_at, self.band_probed) {
            return None;
        }
        let copy = band_reading_owed(false, self.last_frame_at, self.band_probed);
        let (texture, region) = self.newest.clone()?;
        if texture.generation() != self.generation {
            return None;
        }
        self.probe_band(texture.texture(), region, copy)
    }

    /// Samples the band and the row under it from the frame in `texture` (of `region` pixels),
    /// at most every [`BAND_PROBE_EVERY`]. The copy is read on a later frame or tick, once the
    /// GPU has made it, so showing a frame never waits for it. With `copy` false it only reads
    /// the copy that waits. Returns the band's new sample when it changed.
    unsafe fn probe_band(
        &mut self,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
        copy: bool,
    ) -> Option<BandSample> {
        let devices = Arc::clone(&self.devices);
        let band = self.band;
        let points = band_points(region, band)?;
        let context = devices.context.lock().unwrap_or_else(|e| e.into_inner());
        let mut changed = None;
        if self.band_pending {
            let probe = self.band_probe.clone()?;
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
                    self.band_pending = false;
                    self.band_waits_noted = false;
                    let sample = band_sample(&inside, &below);
                    if sample != self.band_sample {
                        self.band_sample = sample;
                        changed = Some(sample);
                        note(
                            self.logger.as_deref(),
                            &format!(
                                "task {}: the title bar's band ({band} rows of a {}x{} frame) \
                                 reads {}",
                                self.task_id,
                                region.0,
                                region.1,
                                describe_band_sample(sample)
                            ),
                        );
                    }
                }
                Err(error) if error.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                    let waited = self.band_probed.map(|at| at.elapsed());
                    if !self.band_waits_noted
                        && waited.is_some_and(|waited| waited >= BAND_COPY_SLOW)
                    {
                        self.band_waits_noted = true;
                        note(
                            self.logger.as_deref(),
                            &format!(
                                "task {}: the copy of the title bar's band has waited {} ms \
                                 to be read; the glyphs keep their tone until it is",
                                self.task_id,
                                waited.unwrap_or_default().as_millis()
                            ),
                        );
                        context.Flush();
                    }
                    return None;
                }
                Err(error) => {
                    self.band_pending = false;
                    // Copied again at the next frame or tick: a still window whose one copy
                    // failed would otherwise never have its band read.
                    self.band_probed = None;
                    if !self.band_waits_noted {
                        self.band_waits_noted = true;
                        let what = format!(
                            "task {}: reading the copy of the title bar's band",
                            self.task_id
                        );
                        note(self.logger.as_deref(), &failed(&what, error));
                    }
                    return None;
                }
            }
        }
        if !copy
            || self
                .band_probed
                .is_some_and(|at| at.elapsed() < BAND_PROBE_EVERY)
        {
            return changed;
        }
        if self.band_probe.is_none() {
            let made = devices.device.CreateTexture2D(
                &staging_desc(BAND_SAMPLES, 2),
                None,
                Some(&mut self.band_probe),
            );
            if let Err(error) = made {
                let what = "creating the texture a title bar's band is sampled into";
                note(self.logger.as_deref(), &failed(what, error));
                self.band_probed = Some(Instant::now());
                return changed;
            }
        }
        let Some(probe) = self.band_probe.clone() else {
            return changed;
        };
        // Across the band's middle row, and the row just under it, where the app's top begins.
        let rows = [points.inside, points.below];
        for (index, y) in rows.into_iter().enumerate() {
            for (column, &x) in points.columns.iter().enumerate() {
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
                    column as u32,
                    index as u32,
                    0,
                    texture,
                    0,
                    Some(&pixel as *const _),
                );
            }
        }
        // Submitted now, so the copy of a still window's last frame does not sit unsubmitted.
        context.Flush();
        self.band_pending = true;
        self.band_probed = Some(Instant::now());
        changed
    }

    /// The `region` pixels at the top left of `texture`, read back as RGBA rows. Waits for the
    /// GPU. The staging texture is made for this one read and let go after it.
    unsafe fn read_frame(
        &self,
        texture: &ID3D11Texture2D,
        (width, height): (u32, u32),
    ) -> Result<Vec<u8>, String> {
        if width == 0 || height == 0 {
            return Err("the window has no frame to read".into());
        }
        let mut staging = None;
        self.devices
            .device
            .CreateTexture2D(&staging_desc(width, height), None, Some(&mut staging))
            .map_err(|e| failed("creating the texture a screenshot is read into", e))?;
        let staging = staging.ok_or("creating the texture a screenshot is read into gave none")?;
        let context = self
            .devices
            .context
            .lock()
            .unwrap_or_else(|e| e.into_inner());
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

    /// When the resize trace is on, says so if the frame in `texture` is all one colour.
    unsafe fn trace_frame_colour(&mut self, texture: &ID3D11Texture2D, region: (u32, u32)) {
        if !trace_detailed() {
            self.samples = None;
            return;
        }
        match self.frame_colour(texture, region) {
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

    unsafe fn frame_colour(
        &mut self,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
    ) -> Result<Option<[u8; 4]>, String> {
        let devices = Arc::clone(&self.devices);
        if self.samples.is_none() {
            devices
                .device
                .CreateTexture2D(
                    &staging_desc(SAMPLE_GRID, SAMPLE_GRID),
                    None,
                    Some(&mut self.samples),
                )
                .map_err(|e| failed("creating the texture a frame is sampled into", e))?;
        }
        let Some(samples) = &self.samples else {
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

    /// While the input trace is on, compares the frame just presented around the last traced
    /// press with what was there when the press came, and logs `app.visible_change` for the
    /// first frame that changed it visibly, or `app.visible_change_none` when none did.
    unsafe fn watch_press(
        &mut self,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
        press: Option<((f32, f32), Duration)>,
    ) {
        if !input_trace_on() {
            if self.last_frame.is_some() || self.watch.is_some() {
                self.last_frame = None;
                self.readback = None;
                self.watch = None;
            }
            return;
        }
        if let Err(why) = self.watch_press_locked(texture, region, press) {
            self.watch = None;
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
        &mut self,
        texture: &ID3D11Texture2D,
        region: (u32, u32),
        press: Option<((f32, f32), Duration)>,
    ) -> Result<(), String> {
        if let Some((point, at)) = press {
            self.watch = None;
            let last = self.last_frame.clone();
            match (area_around(point, region, visible_change::SIDE), last) {
                (Some(area), Some((last, size))) if size == region => {
                    let baseline = self.read_area(&last, area)?;
                    self.watch = Some((PressWatch::new(area, baseline), at));
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
        if let Some((mut watch, at)) = self.watch.take() {
            let area = watch.area();
            if region.0 >= area.right && region.1 >= area.bottom {
                let pixels = self.read_area(texture, area)?;
                let present = self
                    .frame
                    .as_ref()
                    .and_then(|chain| chain.swapchain.GetLastPresentCount().ok())
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "-".into());
                let since = host_clock().saturating_sub(at);
                match watch.frame(&pixels) {
                    Verdict::Waiting => self.watch = Some((watch, at)),
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
        self.keep_last_frame(texture, region)
    }

    /// Copies the frame in `texture`, of `region` pixels, on the GPU, for the next press.
    unsafe fn keep_last_frame(
        &mut self,
        texture: &ID3D11Texture2D,
        (width, height): (u32, u32),
    ) -> Result<(), String> {
        let devices = Arc::clone(&self.devices);
        if width == 0 || height == 0 {
            return Ok(());
        }
        if self
            .last_frame
            .as_ref()
            .is_none_or(|(_, size)| *size != (width, height))
        {
            self.last_frame = None;
            let mut desc = staging_desc(width, height);
            desc.Usage = D3D11_USAGE_DEFAULT;
            desc.CPUAccessFlags = 0;
            let mut made = None;
            devices
                .device
                .CreateTexture2D(&desc, None, Some(&mut made))
                .map_err(|e| failed("creating the copy of the last frame", e))?;
            if let Some(made) = made {
                self.last_frame = Some((made, (width, height)));
            }
        }
        let Some((last, _)) = &self.last_frame else {
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
        &mut self,
        source: &ID3D11Texture2D,
        area: Area,
    ) -> Result<Vec<u8>, String> {
        let devices = Arc::clone(&self.devices);
        if self.readback.is_none() {
            devices
                .device
                .CreateTexture2D(
                    &staging_desc(visible_change::SIDE, visible_change::SIDE),
                    None,
                    Some(&mut self.readback),
                )
                .map_err(|e| failed("creating the texture a press's area is read into", e))?;
        }
        let Some(readback) = &self.readback else {
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
}

/// Which of a window's images a swapchain holds.
#[derive(Clone, Copy)]
enum Image {
    Caption,
    Splash,
    Fill,
}

/// A staging texture's description: `width` x `height` RGBA pixels the CPU reads.
fn staging_desc(width: u32, height: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
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
    }
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

/// Every window's surfaces on one window host's connection, by task.
pub type Surfaces = HashMap<i32, WindowSurfaces>;
