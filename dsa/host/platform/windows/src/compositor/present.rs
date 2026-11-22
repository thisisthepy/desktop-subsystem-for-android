//! DirectComposition in an app window host: how a task's frames appear in its window.
//!
//! An app window host makes no Direct3D device and no DXGI factory. Its process has one
//! DirectComposition device made without a rendering device (`DCompositionCreateDevice2` with
//! none), which can build visual trees and show surfaces another process draws, and nothing
//! else. The window manager, which has each task's frame texture on its own Direct3D device,
//! owns each window's swapchains (the frame's, the caption buttons', the splash's and the
//! one-pixel fill's), each made for a DirectComposition surface handle it places in this
//! process (see `surfaces`). [`WindowPresenter`] shows each surface in the window's visual
//! tree (`CreateSurfaceFromHandle`), keeps the tree's transforms, and commits it; what is done
//! on the GPU it asks of the window manager over [`SurfaceLink`], and waits for the answer.
//! That saves each app process the driver memory of a device of its own.
//!
//! A frame is never scaled to its window: the window takes the size of the frame it shows
//! (`FrameFollow`). The one exception is a window that holds the size the desktop gave it
//! (maximised, snapped, full screen) while the guest keeps its task smaller: the frame is
//! shown in the window's middle, scaled to fit for a task that keeps its density
//! ([`WindowPresenter::set_held`]). No frame passes through the CPU.
//!
//! The frame swapchain's buffers have the texture's size, the largest a frame of the task can
//! be, and each frame sets only the swapchain's source size to the region it fills, so a window
//! resize never resizes the buffers. DirectComposition stretches the source region over the
//! whole of the buffers, so the visual scales from the buffers' size back to the frame's own
//! size (`frame_scale`).
//!
//! A frame of the window's size is presented at once. A frame of another size is copied into
//! the back buffer but not presented, and the window is told
//! ([`WindowPresenter::on_frame_ready`]). The window takes that size
//! ([`WindowPresenter::size_for`] in `WM_WINDOWPOSCHANGING`), and the frame is presented and the
//! visual committed in its `WM_SIZE` ([`WindowPresenter::view_resized`]), so the new window size
//! and the frame of that size reach the screen together.
//!
//! When the window manager's devices are made again (a GPU reset), every answer says so by a new
//! generation: the window draws its caption, fill and splash again, and the next frame comes in
//! a new swapchain whose surface replaces the old one. Until then the old surface keeps what it
//! last showed, so the window keeps its last frame. When this process's DirectComposition
//! device fails, the tree is made again and shows the same surfaces again from the handles the
//! window keeps.
//!
//! Each presenter measures how long its window waited for a frame of its new size
//! (`ResizeLatency`), and logs when its window presented its first frame and when that frame was
//! shown, and says both on the window (`window_marks`), where `dsa launch --wait-first-frame`
//! reads them. While the input trace is on, it logs `app.shown` for each present the frame
//! statistics show to be on the screen (see `PresentWatch`).

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::caption::BandSample;
use dsa_host_core::compositor::frame_buffers::{
    buffers_after_frame, frame_scale, swapchain_buffers, wanted_swapchain_buffers, HiddenBuffers,
    HiddenFrame,
};
use dsa_host_core::compositor::frame_follow::{Arrival, FrameFollow};
use dsa_host_core::compositor::present_watch::{FrameStatistics, PresentWatch};
use dsa_host_core::compositor::resize_latency::ResizeLatency;
use dsa_host_core::compositor::splash;
use dsa_host_core::compositor::surface_calls::{
    assemble_read, band_from_answer, PendingCalls, SURFACE_OP_CAPTION, SURFACE_OP_FILL,
    SURFACE_OP_PRESENT_FRAME, SURFACE_OP_PRESENT_KEPT, SURFACE_OP_READ_FRAME,
    SURFACE_OP_RELEASE_ALL, SURFACE_OP_RELEASE_FRAME, SURFACE_OP_RELEASE_SPLASH, SURFACE_OP_SPLASH,
    SURFACE_OP_TICK,
};
use dsa_host_core::compositor::window_sizing::{
    placement_ratios, shown_frame_placement, to_task_length, uncovered_fill, window_size_of,
    FramePlacement, Scale,
};
use dsa_protocol::window_host::encode_body;
use dsa_protocol::window_host::types::{envelope::Body, SurfaceAnswer, SurfaceCall};
use dsa_transport::FrameSender;
use windows::core::IUnknown;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::Graphics::DirectComposition::{
    DCompositionCreateDevice2, IDCompositionDesktopDevice, IDCompositionTarget,
    IDCompositionVisual, IDCompositionVisual2,
    DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR, DCOMPOSITION_BORDER_MODE_HARD,
};
use windows_numerics::Matrix3x2;

pub use super::gpu::{Devices, Gpu, SharedTexture};
use super::log::{
    host_clock, input_trace, input_trace_on, note, qpc_frequency, trace_stage, trace_summary,
    CompositorLogger,
};
use super::surfaces::STALE_FRAME;
use super::window_marks;
use dsa_host_core::resize_trace::millis;

fn failed(what: &str, error: windows::core::Error) -> String {
    format!("{what}: {error}")
}

/// How long a call to the window manager waits for its answer.
const CALL_WAIT: Duration = Duration::from_secs(2);

/// How long reading a frame back waits for every part of it.
const READ_WAIT: Duration = Duration::from_secs(5);

/// The connection a window host asks the window manager's GPU over: it sends each
/// [`SurfaceCall`] and waits for its answer, which the thread that reads the connection hands
/// in ([`SurfaceLink::answered`]).
pub struct SurfaceLink {
    sender: Arc<Mutex<Box<dyn FrameSender>>>,
    calls: PendingCalls,
}

impl SurfaceLink {
    pub fn new(sender: Arc<Mutex<Box<dyn FrameSender>>>) -> Arc<SurfaceLink> {
        Arc::new(SurfaceLink {
            sender,
            calls: PendingCalls::new(),
        })
    }

    /// Sends `call` and returns its answers. An error when the connection is gone or the
    /// window manager did not answer in time.
    fn call(&self, mut call: SurfaceCall, wait: Duration) -> Result<Vec<SurfaceAnswer>, String> {
        let id = self.calls.begin().map_err(|e| e.to_string())?;
        call.call_id = id;
        let frame = encode_body(Body::SurfaceCall(call));
        let sent = self
            .sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(&frame);
        if let Err(why) = sent {
            self.calls.abandon(id);
            return Err(format!("could not ask the window manager: {why}"));
        }
        self.calls.wait(id, wait).map_err(|e| e.to_string())
    }

    /// An answer came from the window manager.
    pub fn answered(&self, answer: SurfaceAnswer) {
        self.calls.deliver(answer);
    }

    /// The connection ended: every call waiting on it ends, and none starts until
    /// [`SurfaceLink::reopen`].
    pub fn close(&self) {
        self.calls.close();
    }

    /// A new window manager replaced the one that ended: calls go to it, over the sender this
    /// link was made with, which now reaches it.
    pub fn reopen(&self) {
        self.calls.reopen();
    }

    /// Whether the window manager can be asked: false from the end of its connection until a
    /// new window manager has replaced it.
    pub fn is_up(&self) -> bool {
        !self.calls.is_closed()
    }
}

/// This process's DirectComposition device, made without a rendering device, and how many
/// times it was made. Every task window of the process shares it.
struct Composition {
    device: Option<IDCompositionDesktopDevice>,
    generation: u64,
}

// SAFETY: the DirectComposition device is used only under the lock below.
unsafe impl Send for Composition {}

fn composition() -> &'static Mutex<Composition> {
    static COMPOSITION: OnceLock<Mutex<Composition>> = OnceLock::new();
    COMPOSITION.get_or_init(|| {
        Mutex::new(Composition {
            device: None,
            generation: 0,
        })
    })
}

/// The device and its generation, made when there is none.
fn composition_device() -> Result<(IDCompositionDesktopDevice, u64), String> {
    let mut composition = composition().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(device) = &composition.device {
        return Ok((device.clone(), composition.generation));
    }
    // SAFETY: no rendering device; the device can show surfaces other processes draw.
    let device: IDCompositionDesktopDevice =
        unsafe { DCompositionCreateDevice2(None::<&IUnknown>) }
            .map_err(|e| failed("creating the DirectComposition device", e))?;
    composition.generation += 1;
    composition.device = Some(device.clone());
    Ok((device, composition.generation))
}

/// The device of `generation` failed: the next window to ask makes a new one.
fn composition_failed(generation: u64) {
    let mut composition = composition().lock().unwrap_or_else(|e| e.into_inner());
    if composition.generation == generation {
        composition.device = None;
    }
}

/// A surface handle the window manager placed in this process, closed when dropped.
struct Handle(HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: a handle placed in this process for this window, closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A task window's DirectComposition target and its visuals: the frame's, the caption
/// buttons' above it, the splash's under it and the solid fill under all of them.
struct Tree {
    device: IDCompositionDesktopDevice,
    generation: u64,
    _target: IDCompositionTarget,
    visual: IDCompositionVisual2,
    caption: IDCompositionVisual2,
    splash: IDCompositionVisual2,
    fill: IDCompositionVisual2,
}

/// Puts the visuals of a task window on the window `hwnd`.
fn compose_window(hwnd: isize) -> Result<Tree, String> {
    let (device, generation) = composition_device()?;
    // SAFETY: `hwnd` is a live window of this process; the rest are plain COM calls.
    let made = unsafe {
        (|| -> Result<Tree, String> {
            let target = device
                .CreateTargetForHwnd(HWND(hwnd as *mut c_void), true)
                .map_err(|e| failed("attaching DirectComposition to a task window", e))?;
            let visual = || {
                device
                    .CreateVisual()
                    .map_err(|e| failed("creating a task window's visual", e))
            };
            let root = visual()?;
            let frame = visual()?;
            // The caption buttons are drawn over the app, whose content runs under them.
            let caption = visual()?;
            root.AddVisual(&frame, false, None::<&IDCompositionVisual>)
                .map_err(|e| failed("adding a task window's frame visual", e))?;
            root.AddVisual(&caption, true, &frame)
                .map_err(|e| failed("adding a task window's caption visual", e))?;
            // Until the first frame the window shows a splash under where the frame goes.
            let splash = visual()?;
            root.AddVisual(&splash, false, &frame)
                .map_err(|e| failed("adding a task window's splash visual", e))?;
            // Under everything, a solid fill in the app's colour: what shows wherever the
            // frame does not cover the window, so the desktop never shows through it.
            let fill = visual()?;
            root.AddVisual(&fill, false, &splash)
                .map_err(|e| failed("adding a task window's fill visual", e))?;
            // One pixel stretched to the window: nearest, with hard edges, so it stays solid.
            let _ = fill.SetBitmapInterpolationMode(
                DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
            );
            let _ = fill.SetBorderMode(DCOMPOSITION_BORDER_MODE_HARD);
            target
                .SetRoot(&root)
                .map_err(|e| failed("setting a task window's visual", e))?;
            device
                .Commit()
                .map_err(|e| failed("committing a task window's visual", e))?;
            Ok(Tree {
                device: device.clone(),
                generation,
                _target: target,
                visual: frame,
                caption,
                splash,
                fill,
            })
        })()
    };
    if made.is_err() {
        composition_failed(generation);
    }
    made
}

/// The surface the handle `handle` names, on `device`.
///
/// # Safety
///
/// `handle` is a live surface handle of this process.
unsafe fn surface_of(
    device: &IDCompositionDesktopDevice,
    handle: &Handle,
) -> Result<IUnknown, String> {
    device
        .CreateSurfaceFromHandle(handle.0)
        .map_err(|e| failed("opening a task window's surface", e))
}

/// What a visual is committed for: the buffers' size, the frame's, where the window shows the
/// frame, the window's size and the fill's colour.
type Committed = (
    (u32, u32),
    (u32, u32),
    FramePlacement,
    (u32, u32),
    Option<u32>,
);

/// The newest frame: the window manager's texture it came in and the region it fills.
type Newest = (u64, (u32, u32));

/// The caption buttons last drawn: their left edge, size and pixels, drawn again after a GPU
/// reset.
type CaptionImage = (i32, (u32, u32), Vec<u8>);

struct State {
    tree: Option<Tree>,
    /// The surfaces the window shows, as the window manager placed them in this process.
    frame_surface: Option<Handle>,
    caption_surface: Option<Handle>,
    splash_surface: Option<Handle>,
    fill_surface: Option<Handle>,
    /// The generation of the window manager's devices the answers last named; 0 before any.
    wm_generation: u64,
    caption_image: Option<CaptionImage>,
    caption_hidden: bool,
    /// Whether the splash's surface is shown, at which client size.
    splash_shown: Option<(u32, u32)>,
    splash_image: Option<SplashImage>,
    splash_background: Option<u32>,
    fill_colour: Option<u32>,
    /// The size of the frame swapchain's buffers: the texture's, not a frame's.
    buffers: Option<(u32, u32)>,
    buffer_count: u32,
    wanted_buffers: u32,
    last_frame_at: Option<Instant>,
    /// The size of the last frame shown: the swapchain's source size.
    frame: (u32, u32),
    asked: (u32, u32),
    committed: Option<Committed>,
    scale: Scale,
    kept_region: Option<(u32, u32)>,
    follow: FrameFollow,
    closed: bool,
    latency: ResizeLatency,
    presents: PresentWatch,
    no_statistics: bool,
    first_frame: Option<u32>,
    first_shown: bool,
    hidden: HiddenBuffers,
    newest: Option<Newest>,
    /// A new window manager replaced the one this window's surfaces came from: the caption and
    /// the splash are drawn again at the next tick, on the new window manager's device.
    redraw_due: bool,
    /// The height of the title bar's band over the frame while it is shown; 0 otherwise.
    band: u32,
    band_sample: BandSample,
    /// Whether frames came since the last tick, and whether the window manager still has a
    /// read of the band waiting: either makes the next tick ask.
    frames_since_tick: bool,
    band_pending: bool,
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

/// What a window's splash is drawn from: its background as `0xAARRGGBB`, the app's icon as
/// rows of premultiplied BGRA pixels and their size, and the monitor's dots per inch.
pub struct SplashImage {
    pub background: u32,
    pub icon: Option<(Vec<u8>, (u32, u32))>,
    pub dpi: u32,
}

/// A press a window has to watch: the point in guest pixels and when it came on the host clock.
type Press = ((f32, f32), Duration);

fn frame_in_window(state: &State, region: (u32, u32)) -> (u32, u32) {
    window_size_of(region, state.scale, &[state.follow.view(), state.asked])
}

fn presses() -> &'static Mutex<HashMap<i32, Press>> {
    static PRESSES: OnceLock<Mutex<HashMap<i32, Press>>> = OnceLock::new();
    PRESSES.get_or_init(Mutex::default)
}

/// A traced press came at `point`, in guest pixels, in the window of task `task_id`: the
/// window manager compares its next frames with what was on the screen until one visibly
/// changes the area around the point.
pub fn pressed(task_id: i32, point: (f32, f32)) {
    presses()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(task_id, (point, host_clock()));
}

/// How many resizes go between two summaries of their waits in the log.
const SUMMARY_EVERY: usize = 20;

/// The longest a frame waits for the window to take the size of the frame kept before it.
const CLAIM_WAIT: Duration = Duration::from_millis(100);

/// Tells a window that a frame of another size is kept for it to take.
pub type FrameReady = Box<dyn Fn() + Send + Sync>;

/// Tells a window what the app's frame shows under its title bar.
pub type BandColour = Box<dyn Fn(BandSample) + Send + Sync>;

/// What showing one frame changed.
struct Shown {
    new_size: bool,
    resize_buffers: Option<Duration>,
}

/// Shows one task's frames in its window.
pub struct WindowPresenter {
    task_id: i32,
    hwnd: isize,
    link: Arc<SurfaceLink>,
    state: Mutex<State>,
    claim_released: Condvar,
    frame_ready: Mutex<Option<FrameReady>>,
    band_colour: Mutex<Option<BandColour>>,
    capture_done: Condvar,
    logger: Option<Arc<CompositorLogger>>,
    see_through: bool,
}

// SAFETY: every COM object in the state is used under the state's lock.
unsafe impl Send for WindowPresenter {}
unsafe impl Sync for WindowPresenter {}

impl WindowPresenter {
    /// Puts a visual on the window `hwnd` of task `task_id`, whose client area is `view`
    /// pixels. Called on the thread that owns the window.
    pub fn new(
        task_id: i32,
        link: Arc<SurfaceLink>,
        hwnd: isize,
        view: (u32, u32),
        logger: Option<Arc<CompositorLogger>>,
    ) -> Result<Arc<Self>, String> {
        WindowPresenter::with_transparency(task_id, link, hwnd, view, logger, false)
    }

    /// As [`WindowPresenter::new`], for the window of an app overlay window: the frames keep
    /// their own transparency (premultiplied alpha).
    pub fn new_see_through(
        task_id: i32,
        link: Arc<SurfaceLink>,
        hwnd: isize,
        view: (u32, u32),
        logger: Option<Arc<CompositorLogger>>,
    ) -> Result<Arc<Self>, String> {
        WindowPresenter::with_transparency(task_id, link, hwnd, view, logger, true)
    }

    fn with_transparency(
        task_id: i32,
        link: Arc<SurfaceLink>,
        hwnd: isize,
        view: (u32, u32),
        logger: Option<Arc<CompositorLogger>>,
        see_through: bool,
    ) -> Result<Arc<Self>, String> {
        let tree = compose_window(hwnd)?;
        Ok(Arc::new(WindowPresenter {
            task_id,
            hwnd,
            link,
            state: Mutex::new(State {
                tree: Some(tree),
                frame_surface: None,
                caption_surface: None,
                splash_surface: None,
                fill_surface: None,
                wm_generation: 0,
                caption_image: None,
                caption_hidden: false,
                splash_shown: None,
                splash_image: None,
                splash_background: None,
                fill_colour: None,
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
                closed: false,
                latency: ResizeLatency::new(),
                presents: PresentWatch::new(),
                no_statistics: false,
                first_frame: None,
                first_shown: false,
                hidden: HiddenBuffers::new(),
                newest: None,
                redraw_due: false,
                band: 0,
                band_sample: BandSample::default(),
                frames_since_tick: false,
                band_pending: false,
                capture_wanted: false,
                captured: None,
            }),
            claim_released: Condvar::new(),
            frame_ready: Mutex::new(None),
            band_colour: Mutex::new(None),
            capture_done: Condvar::new(),
            logger,
            see_through,
        }))
    }

    pub fn on_frame_ready(&self, ready: FrameReady) {
        *self.frame_ready.lock().unwrap_or_else(|e| e.into_inner()) = Some(ready);
    }

    pub fn on_band_colour(&self, told: BandColour) {
        *self.band_colour.lock().unwrap_or_else(|e| e.into_inner()) = Some(told);
    }

    fn tell_band(&self, sample: Option<BandSample>) {
        let Some(sample) = sample else {
            return;
        };
        if let Some(told) = self
            .band_colour
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            told(sample);
        }
    }

    fn tell_frame_ready(&self) {
        if let Some(ready) = self
            .frame_ready
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            ready();
        }
    }

    /// Asks the window manager for `call` on this window, and takes in what every answer says
    /// besides its result: a new generation of the window manager's devices, new surfaces, the
    /// band, the statistics. Returns the answers, or why there are none.
    fn ask(
        &self,
        state: &mut State,
        mut call: SurfaceCall,
        wait: Duration,
    ) -> Result<Vec<SurfaceAnswer>, String> {
        call.task_id = self.task_id;
        call.see_through = self.see_through;
        let answers = self.link.call(call, wait)?;
        let Some(first) = answers.first().cloned() else {
            return Err("the window manager gave no answer".into());
        };
        if state.wm_generation != 0 && first.generation != state.wm_generation {
            self.wm_devices_replaced(state);
        }
        state.wm_generation = first.generation;
        // SAFETY: plain COM calls on this window's tree; the handles were placed in this
        // process for it.
        unsafe { self.take_surfaces(state, &first) };
        if first.buffer_width != 0 && first.buffer_height != 0 {
            state.buffers = Some((first.buffer_width, first.buffer_height));
        }
        state.band_pending = first.band_pending;
        if let Some(sample) = band_from_answer(&first) {
            state.band_sample = sample;
        }
        self.take_statistics(state, &first);
        if !first.error.is_empty() {
            return Err(first.error);
        }
        Ok(answers)
    }

    /// The window manager's devices were made again: everything it drew for this window is
    /// gone. The surfaces shown keep their last content until new ones replace them; the
    /// caption, the fill and the splash are drawn again, and the frame with the next one.
    fn wm_devices_replaced(&self, state: &mut State) {
        note(
            self.logger.as_deref(),
            &format!(
                "task {}: the window manager made new Direct3D devices; the window draws again \
                 and shows the app's next frame",
                self.task_id
            ),
        );
        state.buffers = None;
        state.buffer_count = 0;
        state.committed = None;
        state.frame = (0, 0);
        state.fill_colour = None;
        state.newest = None;
        state.kept_region = None;
        state.follow.release();
        state.presents.forget();
        if !state.first_shown && state.first_frame.is_some() {
            state.first_frame = Some(0);
        }
        self.claim_released.notify_all();
        // Drawn again after this answer is taken in, so their new surfaces come with them.
        state.splash_shown = None;
    }

    /// A new window manager replaced the one that ended (a crash or a restart). What the old
    /// one drew for this window is gone from it, but the visuals keep showing the surfaces they
    /// had, so the window keeps its last frame. Nothing is asked of the window manager here: this
    /// runs on the thread that reads its answers. The next frame brings new surfaces, and the
    /// caption and the splash are drawn again at the next tick.
    pub fn window_manager_replaced(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        self.wm_devices_replaced(&mut state);
        // The new window manager's generation is taken as it is, not as a second replacement.
        state.wm_generation = 0;
        state.redraw_due = true;
    }

    /// Shows the surfaces an answer placed in this process, in place of the ones before.
    unsafe fn take_surfaces(&self, state: &mut State, answer: &SurfaceAnswer) {
        let Some(tree) = &state.tree else {
            for value in [
                answer.frame_surface,
                answer.caption_surface,
                answer.splash_surface,
                answer.fill_surface,
            ] {
                if value != 0 {
                    drop(Handle(HANDLE(value as usize as *mut c_void)));
                }
            }
            return;
        };
        let slots: [(u64, &IDCompositionVisual2, &mut Option<Handle>); 4] = [
            (answer.frame_surface, &tree.visual, &mut state.frame_surface),
            (
                answer.caption_surface,
                &tree.caption,
                &mut state.caption_surface,
            ),
            (
                answer.splash_surface,
                &tree.splash,
                &mut state.splash_surface,
            ),
            (answer.fill_surface, &tree.fill, &mut state.fill_surface),
        ];
        for (value, visual, slot) in slots {
            if value == 0 {
                continue;
            }
            let handle = Handle(HANDLE(value as usize as *mut c_void));
            match surface_of(&tree.device, &handle) {
                Ok(surface) => {
                    let _ = visual.SetContent(&surface);
                }
                Err(why) => note(self.logger.as_deref(), &why),
            }
            *slot = Some(handle);
        }
        // The content is new: the next commit shows it.
        state.committed = None;
    }

    /// Takes in the frame statistics and the present count an answer carries.
    fn take_statistics(&self, state: &mut State, answer: &SurfaceAnswer) {
        if answer.no_statistics && !state.no_statistics {
            state.no_statistics = true;
            state.presents.forget();
            note(
                self.logger.as_deref(),
                &format!(
                    "task {}: this window's swapchain gives no frame statistics, so the input \
                     trace has no app.shown lines for it",
                    self.task_id
                ),
            );
        }
        if answer.presented && answer.has_present_count && !state.no_statistics {
            state.presents.presented(answer.present_count);
        }
        if !answer.has_statistics || state.no_statistics {
            return;
        }
        let tracing = input_trace_on();
        let shown = state.presents.statistics(
            FrameStatistics {
                present_count: answer.statistics_present_count,
                sync_qpc: answer.statistics_sync_qpc,
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
            if tracing {
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
        }
        if !tracing && state.first_shown {
            state.presents.forget();
        }
    }

    /// Whether the statistics are wanted: while the input trace is on, and until the first
    /// frame is known to be on the screen.
    fn watching(state: &State) -> bool {
        (input_trace_on() || !state.first_shown) && !state.no_statistics
    }

    /// Draws again, after the window manager's devices were made again, what this window
    /// drew: the caption buttons and the splash. The fill follows at the next commit.
    fn redraw(&self, state: &mut State) {
        if let Some((left, size, pixels)) = state.caption_image.clone() {
            if let Err(why) = self.draw_caption(state, left, size, &pixels) {
                note(self.logger.as_deref(), &why);
            }
        }
        if state.first_frame.is_none() && state.splash_image.is_some() {
            let view = state.asked;
            if let Err(why) = self.draw_splash(state, view) {
                note(self.logger.as_deref(), &why);
            }
        }
    }

    /// Commits the tree. A failure makes the process's DirectComposition device again and the
    /// tree on it, showing the same surfaces.
    fn commit(&self, state: &mut State) -> bool {
        let Some(tree) = &state.tree else {
            self.recompose(state);
            return false;
        };
        // SAFETY: a plain COM call on the device.
        match unsafe { tree.device.Commit() } {
            Ok(()) => true,
            Err(error) => {
                note(
                    self.logger.as_deref(),
                    &failed(
                        &format!("task {}: committing the window's visual", self.task_id),
                        error,
                    ),
                );
                composition_failed(tree.generation);
                self.recompose(state);
                false
            }
        }
    }

    /// Makes the window's tree again on the current DirectComposition device and shows the
    /// surfaces it showed.
    fn recompose(&self, state: &mut State) {
        state.tree = None;
        match compose_window(self.hwnd) {
            Ok(tree) => {
                // SAFETY: plain COM calls; the handles are this window's.
                unsafe {
                    let slots = [
                        (&state.frame_surface, &tree.visual),
                        (&state.caption_surface, &tree.caption),
                        (&state.splash_surface, &tree.splash),
                        (&state.fill_surface, &tree.fill),
                    ];
                    for (handle, visual) in slots {
                        if let Some(handle) = handle {
                            if let Ok(surface) = surface_of(&tree.device, handle) {
                                let _ = visual.SetContent(&surface);
                            }
                        }
                    }
                    if let Some((left, _, _)) = &state.caption_image {
                        let _ = tree.caption.SetOffsetX2(*left as f32);
                    }
                    if state.caption_hidden {
                        let _ = tree.caption.SetOffsetY2(-1.0e6);
                    }
                    let _ = tree.device.Commit();
                }
                state.tree = Some(tree);
                state.committed = None;
                note(
                    self.logger.as_deref(),
                    &format!(
                        "task {}: the window's visuals were made again on a new \
                         DirectComposition device",
                        self.task_id
                    ),
                );
            }
            Err(why) => note(
                self.logger.as_deref(),
                &format!(
                    "task {}: the window's visuals cannot be made again yet: {why}",
                    self.task_id
                ),
            ),
        }
    }

    pub fn kept_size(&self) -> Option<(u32, u32)> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.follow.kept()
    }

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
        self.claim_released.notify_all();
    }

    pub fn set_scale(&self, scale: Scale) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.scale == scale {
            return;
        }
        state.scale = scale;
        let Some((texture, region)) = state.newest else {
            return;
        };
        if state.hidden.frame() == HiddenFrame::Hold {
            return;
        }
        // The same frame measured at the new scale: shown if it fits the window, never a size
        // for the window to take.
        let shown = frame_in_window(&state, region);
        match state.follow.rescaled(shown) {
            Arrival::Show => {}
            Arrival::Stale => {
                let view = state.follow.view();
                trace_stage(
                    self.logger.as_deref(),
                    self.task_id,
                    "app.frame_stale",
                    region,
                    &format!(
                        "view={}x{} why=the scale changed under a frame drawn before it: not \
                         followed",
                        view.0, view.1
                    ),
                );
                self.commit_visual(&mut state);
                return;
            }
            Arrival::Keep | Arrival::Wait => {
                self.commit_visual(&mut state);
                return;
            }
        }
        if let Err(why) = self.present_locked(&mut state, texture, region, false, false) {
            note(self.logger.as_deref(), &why);
        }
    }

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
            let region = state.kept_region.take().unwrap_or(view);
            match self.present_back_buffer(&mut state, region) {
                Ok(new_size) => self.frame_shown(&mut state, view, new_size, None, started),
                Err(why) => note(self.logger.as_deref(), &why),
            }
        }
        let committed = self.commit_visual(&mut state);
        let device = state.tree.as_ref().map(|tree| tree.device.clone());
        drop(state);
        self.claim_released.notify_all();
        if committed {
            if let Some(device) = device {
                // SAFETY: a plain COM call on the device.
                unsafe {
                    let _ = device.WaitForCommitCompletion();
                }
            }
        }
        if took_frame {
            // Waiting until the desktop has composed the new size and its frame keeps the next
            // step of a drag from queueing a newer size before this one is on the screen.
            // SAFETY: a plain call with no arguments.
            unsafe {
                let _ = windows_sys::Win32::Graphics::Dwm::DwmFlush();
            }
        }
    }

    /// The largest client area the guest can draw for the task at the window's scale
    /// (`None`: not known), so a held window larger than it fills the strips it leaves.
    pub fn set_limit(&self, limit: Option<(u32, u32)>) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.follow.limit() == limit {
            return;
        }
        state.follow.set_limit(limit);
        // Committed again with the next frame or tick, with or without the fill.
        state.committed = None;
    }

    pub fn set_held(&self, held: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.follow.held() == held {
            return;
        }
        state.follow.set_held(held);
        if let Some(region) = state.kept_region.take().filter(|_| held) {
            let started = Instant::now();
            match self.present_back_buffer(&mut state, region) {
                Ok(new_size) => {
                    let in_window = frame_in_window(&state, region);
                    self.frame_shown(&mut state, in_window, new_size, None, started);
                }
                Err(why) => note(self.logger.as_deref(), &why),
            }
        }
        self.commit_visual(&mut state);
        drop(state);
        self.claim_released.notify_all();
    }

    pub fn release_claim(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.follow.release();
        drop(state);
        self.claim_released.notify_all();
    }

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

    /// Time has passed: a window that waits long for a frame of the size asked for is logged,
    /// the window manager reads what waits to be read (the band, the statistics), and a
    /// window whose swapchain has been still for a while gives a buffer back.
    pub fn tick(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        if state.tree.is_none() {
            self.recompose(&mut state);
        }
        if state.redraw_due && self.link.is_up() {
            state.redraw_due = false;
            self.redraw(&mut state);
            self.commit_visual(&mut state);
        }
        let watching = Self::watching(&state) && state.presents.pending() > 0;
        let owed = state.frames_since_tick || state.band_pending || watching;
        let mut band_changed = None;
        if owed {
            state.frames_since_tick = false;
            let before = state.band_sample;
            let generation = state.wm_generation;
            let call = SurfaceCall {
                op: SURFACE_OP_TICK,
                band: to_task_length(state.band, state.scale),
                statistics: watching,
                ..Default::default()
            };
            match self.ask(&mut state, call, CALL_WAIT) {
                Ok(_) => {}
                Err(why) => note(self.logger.as_deref(), &why),
            }
            if state.band_sample != before {
                band_changed = Some(state.band_sample);
                state.committed = None;
                self.commit_visual(&mut state);
            }
            if generation != 0 && generation != state.wm_generation {
                self.redraw(&mut state);
                self.commit_visual(&mut state);
            }
        }
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
        drop(state);
        self.tell_band(band_changed);
    }

    fn release_idle_buffer(&self, state: &mut State) {
        if state.hidden.hidden() || state.buffers.is_none() || state.follow.kept().is_some() {
            return;
        }
        let Some(quiet) = state.last_frame_at.map(|at| at.elapsed()) else {
            return;
        };
        let wanted = wanted_swapchain_buffers(quiet);
        if wanted >= state.buffer_count {
            return;
        }
        let Some((texture, region)) = state.newest else {
            return;
        };
        state.wanted_buffers = wanted;
        if let Err(why) = self.present_locked(state, texture, region, false, false) {
            note(self.logger.as_deref(), &why);
        }
    }

    pub fn hide_caption(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return;
        }
        state.band = 0;
        state.caption_hidden = true;
        if let Some(tree) = &state.tree {
            // SAFETY: a plain COM call on this window's visual.
            unsafe {
                let _ = tree.caption.SetOffsetY2(-1.0e6);
            }
        }
        self.commit(&mut state);
    }

    pub fn show_caption(&self, left: i32, size: (u32, u32), pixels: &[u8]) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || size.0 == 0 || size.1 == 0 {
            return Ok(());
        }
        if pixels.len() < (size.0 * size.1 * 4) as usize {
            return Err("the caption image is smaller than its size".into());
        }
        state.band = size.1;
        state.caption_hidden = false;
        state.caption_image = Some((left, size, pixels.to_vec()));
        let generation = state.wm_generation;
        let drawn = self.draw_caption(&mut state, left, size, pixels);
        if generation != 0 && generation != state.wm_generation {
            // Everything else this window drew went with the old devices.
            self.redraw(&mut state);
        }
        drawn
    }

    fn draw_caption(
        &self,
        state: &mut State,
        left: i32,
        size: (u32, u32),
        pixels: &[u8],
    ) -> Result<(), String> {
        let call = SurfaceCall {
            op: SURFACE_OP_CAPTION,
            width: size.0,
            height: size.1,
            pixels: pixels.to_vec(),
            ..Default::default()
        };
        self.ask(state, call, CALL_WAIT)?;
        if let Some(tree) = &state.tree {
            // SAFETY: plain COM calls on this window's visual.
            unsafe {
                let _ = tree.caption.SetOffsetX2(left as f32);
                let _ = tree.caption.SetOffsetY2(0.0);
            }
        }
        self.commit(state);
        Ok(())
    }

    /// The window was minimized. The window manager lets its frame swapchain go: a minimized
    /// window shows nothing, and its buffers hold tens of MiB of the task's full size.
    pub fn hide(&self) {
        // Without a window manager the surface cannot be made again when the window is shown, so
        // it keeps showing the last frame.
        if !self.link.is_up() {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || !state.hidden.hide() {
            return;
        }
        if let Some(tree) = &state.tree {
            // SAFETY: a plain COM call on this window's visual.
            unsafe {
                let _ = tree.visual.SetContent(None::<&IUnknown>);
            }
        }
        self.commit(&mut state);
        state.frame_surface = None;
        let call = SurfaceCall {
            op: SURFACE_OP_RELEASE_FRAME,
            ..Default::default()
        };
        if let Err(why) = self.ask(&mut state, call, CALL_WAIT) {
            note(self.logger.as_deref(), &why);
        }
        state.buffers = None;
        state.buffer_count = 0;
        state.committed = None;
        state.frame = (0, 0);
        state.presents.forget();
        if !state.first_shown && state.first_frame.is_some() {
            state.first_frame = Some(0);
        }
        drop(state);
        self.claim_released.notify_all();
    }

    /// The window was restored: the newest frame is shown at once from the texture.
    pub fn show(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || !state.hidden.show() {
            return;
        }
        let Some((texture, region)) = state.newest else {
            return;
        };
        if let Err(why) = self.present_locked(&mut state, texture, region, false, false) {
            note(self.logger.as_deref(), &why);
        }
    }

    /// The window is closing: nothing more is shown in it. Its content stays, so the close
    /// animation shows the window whole.
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
        self.capture_done.notify_all();
        self.claim_released.notify_all();
    }

    /// The window is gone: what it showed is let go, here and in the window manager.
    pub fn release(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        let call = SurfaceCall {
            op: SURFACE_OP_RELEASE_ALL,
            ..Default::default()
        };
        // Not waited on for long: a window manager that is gone has let everything go.
        let _ = self.ask(&mut state, call, CALL_WAIT);
        state.newest = None;
        state.buffers = None;
        state.committed = None;
        state.splash_image = None;
        state.caption_image = None;
        state.frame_surface = None;
        state.caption_surface = None;
        state.splash_surface = None;
        state.fill_surface = None;
        state.tree = None;
        state.presents.forget();
    }

    /// The frame the window shows, read back for a screenshot, or `None` before the app's
    /// first frame. While the app draws, the screenshot takes the next frame as it is shown,
    /// waiting [`CAPTURE_WAIT`] at most; a still app's newest frame is read at once.
    pub fn capture(&self) -> Result<Option<CapturedFrame>, String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
        if state.newest.is_none() {
            return Ok(None);
        }
        let band = to_task_length(state.band, state.scale);
        let call = SurfaceCall {
            op: SURFACE_OP_READ_FRAME,
            ..Default::default()
        };
        let answers = self.ask(&mut state, call, READ_WAIT)?;
        Ok(assemble_read(&answers)?.map(|frame| CapturedFrame {
            width: frame.width,
            height: frame.height,
            rgba: frame.rgba,
            band,
        }))
    }

    /// Shows the frame the window manager has in texture `texture_id`, which fills its
    /// top-left `content` pixels, and returns once the window manager has copied it out, so
    /// the texture may be written again.
    pub fn present(&self, texture_id: u64, content: (u32, u32)) -> Result<(), String> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Ok(());
        }
        // A frame that says no size fills the whole texture, whose size the buffers have.
        let region = if content.0 == 0 || content.1 == 0 {
            state.buffers.unwrap_or(state.asked)
        } else {
            content
        };
        state.newest = Some((texture_id, region));
        let now = Instant::now();
        let since_last = state
            .last_frame_at
            .map(|at| now.saturating_duration_since(at));
        state.wanted_buffers = buffers_after_frame(state.wanted_buffers, since_last);
        state.last_frame_at = Some(now);
        state.frames_since_tick = true;
        if state.hidden.frame() == HiddenFrame::Hold {
            return Ok(());
        }
        let started = Instant::now();
        let deadline = started + CLAIM_WAIT;
        let arrival = loop {
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
        let view = state.follow.view();
        if arrival == Arrival::Stale {
            let why = if state.follow.dragged_past(frame_in_window(&state, region)) {
                "a size the window was dragged past before its own, drawn late: not followed"
            } else {
                "drawn for the monitor scale the window had before"
            };
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_stale",
                region,
                &format!("view={}x{} why={why}", view.0, view.1),
            );
            return Ok(());
        }
        let keep = arrival == Arrival::Keep;
        if keep {
            // The window takes this frame's size: say so, since it is what resizes the window
            // on its own after the user let go.
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_kept",
                region,
                &format!(
                    "view={}x{} the window takes the frame's size",
                    view.0, view.1
                ),
            );
        }
        if keep {
            state.kept_region = Some(region);
        }
        let band_before = state.band_sample;
        let shown = match self.present_locked(&mut state, texture_id, region, keep, true) {
            Ok(shown) => shown,
            Err(why) if why == STALE_FRAME => return Ok(()),
            Err(why) => return Err(why),
        };
        let band_changed = (state.band_sample != band_before).then_some(state.band_sample);
        if keep {
            trace_stage(
                self.logger.as_deref(),
                self.task_id,
                "app.frame_kept",
                region,
                &format!("view={}x{}", state.follow.view().0, state.follow.view().1),
            );
        } else {
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
        self.tell_band(band_changed);
        if keep {
            self.tell_frame_ready();
        }
        Ok(())
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

    /// Scales the visual so the frame shows 1:1, fills the window under it with the app's
    /// colour, and commits, unless that is what is on the compositor already. Returns whether
    /// it committed.
    fn commit_visual(&self, state: &mut State) -> bool {
        let Some(buffers) = state.buffers else {
            return false;
        };
        let view = state.follow.view();
        let placement = shown_frame_placement(
            state.frame,
            view,
            state.scale,
            state.follow.held(),
            state.follow.stretched(),
        );
        // The window has no redirection surface (WS_EX_NOREDIRECTIONBITMAP) and paints no
        // background (WM_ERASEBKGND), so wherever the frame's visual does not reach, beside a
        // frame shown in a larger window, the desktop shows through unless the fill under the
        // frame covers it. Core decides from where the frame is (FrameFollow::strips_filled):
        // only when the placed frame does not cover a held window and cannot (the window is
        // beyond what the guest can draw) or no longer will (nothing is pending). While a
        // maximise, snap or restore waits for the app, nothing is drawn around the last frame.
        let colour = if state.follow.strips_filled(placement) {
            fill_colour(
                self.see_through,
                state.band_sample,
                state.splash_background,
                || super::system_settings::read().dark_mode,
            )
        } else {
            None
        };
        let wanted = (buffers, state.frame, placement, view, colour);
        if state.committed == Some(wanted) {
            return false;
        }
        // A fill that could not be drawn is not committed as shown: the next commit asks again
        // rather than leaving the strips beside the frame transparent.
        let mut fill_shown = true;
        if let Some(colour) = colour {
            if state.fill_colour != Some(colour) || state.fill_surface.is_none() {
                let call = SurfaceCall {
                    op: SURFACE_OP_FILL,
                    colour,
                    ..Default::default()
                };
                match self.ask(state, call, CALL_WAIT) {
                    Ok(_) => state.fill_colour = Some(colour),
                    Err(why) => {
                        note(self.logger.as_deref(), &why);
                        fill_shown = false;
                    }
                }
            }
            fill_shown &= state.fill_colour == Some(colour) && state.fill_surface.is_some();
        }
        let (x, y, dx, dy) =
            frame_transform(frame_scale(buffers, state.frame), state.frame, placement);
        let matrix = Matrix3x2 {
            M11: x,
            M12: 0.0,
            M21: 0.0,
            M22: y,
            M31: dx,
            M32: dy,
        };
        // No colour: the fill's one pixel is scaled to nothing, so it covers nothing.
        let fill_size = if colour.is_some() { view } else { (0, 0) };
        let fill = Matrix3x2 {
            M11: fill_size.0 as f32,
            M12: 0.0,
            M21: 0.0,
            M22: fill_size.1 as f32,
            M31: 0.0,
            M32: 0.0,
        };
        let drop_splash = state.splash_shown.is_some() && state.first_frame.is_some();
        if let Some(tree) = &state.tree {
            // SAFETY: plain COM calls on this window's visuals.
            unsafe {
                let _ = tree.fill.SetTransform2(&fill);
                let _ = tree.visual.SetTransform2(&matrix);
                if drop_splash {
                    // The first frame is presented, and this commit shows it: the splash
                    // under it goes in the same commit, so nothing shows between the two.
                    let _ = tree.splash.SetContent(None::<&IUnknown>);
                }
            }
        }
        let committed = self.commit(state);
        if drop_splash {
            state.splash_shown = None;
            state.splash_surface = None;
            let call = SurfaceCall {
                op: SURFACE_OP_RELEASE_SPLASH,
                ..Default::default()
            };
            let _ = self.ask(state, call, CALL_WAIT);
        }
        if state.first_frame.is_some() {
            state.splash_image = None;
        }
        if committed && fill_shown {
            state.committed = Some(wanted);
        }
        committed
    }

    /// Shows `image` as the window's splash until its first frame. Returns once the compositor
    /// has taken it, so the window can be shown with it.
    pub fn show_splash(&self, image: SplashImage) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.first_frame.is_some() {
            return;
        }
        state.splash_background = Some(image.background);
        state.splash_image = Some(image);
        let view = state.asked;
        let drawn = self.draw_splash(&mut state, view);
        let device = state.tree.as_ref().map(|tree| tree.device.clone());
        drop(state);
        match drawn {
            Ok(()) => {
                if let Some(device) = device {
                    // SAFETY: a plain COM call on the device.
                    unsafe {
                        let _ = device.WaitForCommitCompletion();
                    }
                }
            }
            Err(why) => note(self.logger.as_deref(), &why),
        }
    }

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

    /// Has the window manager draw the splash at the size of a `view` pixel client area, and
    /// commits it.
    fn draw_splash(&self, state: &mut State, view: (u32, u32)) -> Result<(), String> {
        let Some(image) = state.splash_image.as_ref() else {
            return Ok(());
        };
        if view.0 == 0 || view.1 == 0 {
            return Ok(());
        }
        let (pixels, (icon_width, icon_height)) = match &image.icon {
            Some((pixels, size)) => (pixels.clone(), *size),
            None => (Vec::new(), (0, 0)),
        };
        let call = SurfaceCall {
            op: SURFACE_OP_SPLASH,
            width: view.0,
            height: view.1,
            colour: image.background,
            pixels,
            icon_width,
            icon_height,
            dpi: image.dpi,
            ..Default::default()
        };
        self.ask(state, call, CALL_WAIT)?;
        state.splash_shown = Some(view);
        self.commit(state);
        Ok(())
    }

    /// Presents what is in the back buffer, of `size` pixels. Returns whether that size
    /// differs from the last one shown.
    fn present_back_buffer(&self, state: &mut State, size: (u32, u32)) -> Result<bool, String> {
        if state.buffers.is_none() {
            return Ok(false);
        }
        let new_size = state.frame != size;
        let call = SurfaceCall {
            op: SURFACE_OP_PRESENT_KEPT,
            width: size.0,
            height: size.1,
            statistics: Self::watching(state),
            ..Default::default()
        };
        let answers = self.ask(state, call, CALL_WAIT)?;
        let count = answers
            .first()
            .filter(|answer| answer.has_present_count)
            .map(|answer| answer.present_count);
        state.frame = size;
        self.first_presented(state, size, count);
        Ok(new_size)
    }

    fn first_presented(&self, state: &mut State, size: (u32, u32), count: Option<u32>) {
        if state.first_frame.is_none() {
            state.first_frame = Some(count.unwrap_or(0));
            self.first_frame_presented(size, count);
        }
    }

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

    /// Asks the window manager to copy the frame of `texture` filling `region` into the frame
    /// swapchain and, unless `stage`, to present it; then commits the visual that goes with
    /// it. `frame` says this is a new frame of the app's, which the band is sampled from and a
    /// waiting screenshot or press reads.
    fn present_locked(
        &self,
        state: &mut State,
        texture: u64,
        region: (u32, u32),
        stage: bool,
        frame: bool,
    ) -> Result<Shown, String> {
        let press = if frame && !stage {
            presses()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&self.task_id)
        } else {
            None
        };
        let capture = frame && state.capture_wanted;
        let call = SurfaceCall {
            op: SURFACE_OP_PRESENT_FRAME,
            texture_id: texture,
            width: region.0,
            height: region.1,
            keep: stage,
            buffer_count: state.wanted_buffers,
            band: to_task_length(state.band, state.scale),
            probe_band: frame,
            capture,
            press: press.is_some(),
            press_x: press.map_or(0.0, |((x, _), _)| x),
            press_y: press.map_or(0.0, |((_, y), _)| y),
            press_at_us: press.map_or(0, |(_, at)| at.as_micros() as u64),
            statistics: Self::watching(state),
            ..Default::default()
        };
        let generation = state.wm_generation;
        let answers = match self.ask(state, call, CALL_WAIT) {
            Ok(answers) => answers,
            Err(why) => {
                if generation != 0 && generation != state.wm_generation {
                    self.redraw(state);
                }
                return Err(why);
            }
        };
        if generation != 0 && generation != state.wm_generation {
            self.redraw(state);
        }
        let first = answers.first().cloned().unwrap_or_default();
        if capture {
            state.capture_wanted = false;
            let band = to_task_length(state.band, state.scale);
            state.captured = Some(assemble_read(&answers).and_then(|frame| {
                frame
                    .map(|frame| CapturedFrame {
                        width: frame.width,
                        height: frame.height,
                        rgba: frame.rgba,
                        band,
                    })
                    .ok_or_else(|| "the window manager had no frame to read".to_string())
            }));
            self.capture_done.notify_all();
        }
        state.buffer_count = state.wanted_buffers;
        let remade = first.remade;
        if remade {
            if !state.first_shown && state.first_frame.is_some() {
                state.first_frame = Some(0);
            }
            state.frame = (0, 0);
        }
        let mut new_size = remade || state.frame != region;
        if !stage {
            new_size = new_size || state.frame != region;
            state.frame = region;
            let count = first.has_present_count.then_some(first.present_count);
            self.first_presented(state, region, count);
            self.commit_visual(state);
        }
        Ok(Shown {
            new_size,
            resize_buffers: remade.then(|| Duration::from_micros(first.resize_us)),
        })
    }
}

/// The colour under a window's frame, from core ([`uncovered_fill`]): the app's colours read
/// from its frame, its splash background, else the theme's, `dark` asked only when needed.
fn fill_colour(
    see_through: bool,
    sample: BandSample,
    splash_background: Option<u32>,
    dark: impl FnOnce() -> bool,
) -> Option<u32> {
    uncovered_fill(
        see_through,
        sample.band,
        sample.fill,
        splash_background,
        || splash::background(dark()),
    )
}

/// The frame visual's scale and offset (x scale, y scale, x, y) that shows the frame where
/// core places it (`placement`), for a swapchain whose buffers the frame fills at
/// `buffer_scale`.
fn frame_transform(
    buffer_scale: (f32, f32),
    frame: (u32, u32),
    placement: FramePlacement,
) -> (f32, f32, f32, f32) {
    // Each axis on its own: a frame stretched over a window of another shape while the
    // window waits for the app scales its two axes differently.
    let (x, y) = placement_ratios(frame, placement);
    (
        buffer_scale.0 * x as f32,
        buffer_scale.1 * y as f32,
        placement.origin.0 as f32,
        placement.origin.1 as f32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::compositor::window_sizing::{frame_placement, vectors};

    #[test]
    fn nfr8_windows_places_each_frame_as_the_shared_vectors_say() {
        for case in vectors::PLACEMENTS {
            let placement = frame_placement(case.frame, case.view, case.scale, case.held);
            let (x, y, dx, dy) = frame_transform((1.0, 1.0), case.frame, placement);
            let expected = case.expected;
            assert_eq!(
                (dx, dy),
                (expected.origin.0 as f32, expected.origin.1 as f32)
            );
            assert!(
                (case.frame.0 as f32 * x - expected.size.0 as f32).abs() <= 1.0,
                "{case:?}"
            );
            assert!(
                (case.frame.1 as f32 * y - expected.size.1 as f32).abs() <= 1.0,
                "{case:?}"
            );
        }
    }

    #[test]
    fn nfr8_windows_fills_beside_the_frame_as_the_shared_vectors_say() {
        for case in vectors::fills() {
            let sample = BandSample {
                band: case.band,
                fill: case.fill,
                ..BandSample::default()
            };
            assert_eq!(
                fill_colour(case.see_through, sample, case.splash, || case.dark),
                case.expected,
                "{case:?}"
            );
        }
    }
}
