//! Frames that stay on the GPU: the renderer's dmabufs as the desktop's buffers
//! (linux-dmabuf), cropped to the frame in them (viewporter), with the title bar's band on a
//! subsurface above them.
//!
//! The renderer keeps each task's images at the largest size the task can have, and a frame
//! fills their top-left region, so a window crops the image to the frame and shows it one to
//! one. Nothing of the frame passes through the CPU to be shown. Where the desktop lacks one
//! of the three protocols, every frame is read back instead.
//!
//! Two things read a GPU frame back, in whatever layout the GPU keeps it
//! ([`crate::compositor::dmabuf_read`]): the title bar's band, which samples two rows at most
//! every [`BAND_PROBE_EVERY`] to take the app's top color when the app leaves the band empty,
//! and a screenshot, which reads every row under the band once.

use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::caption::BandSample;
use wayland_client::protocol::wl_buffer::WlBuffer;
use wayland_client::protocol::wl_shm::Format;
use wayland_client::protocol::wl_subcompositor::WlSubcompositor;
use wayland_client::protocol::wl_subsurface::WlSubsurface;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{delegate_noop, QueueHandle};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1::{self, ZwpLinuxBufferParamsV1},
    zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};

use super::shm::ShmBuffer;
use super::State;
use crate::compositor::dmabuf_read::{read_linear, DmabufReader, Imported, Rows};
use crate::compositor::frames::{
    app_rows, band_probe_rows, Frame, GpuFrame, GpuImage, DRM_FORMAT_MOD_LINEAR,
};
use crate::compositor::session::log;
use crate::compositor::title_bar::rows_band_sample;

/// How often a GPU frame's band is sampled, at most: the app's top color changes with its
/// screens, not with every frame, and each sample waits for the GPU.
pub const BAND_PROBE_EVERY: Duration = Duration::from_millis(200);

/// One of a display's GPU images, as a buffer of the desktop.
pub struct GpuBuffer {
    buffer: WlBuffer,
    size: (u32, u32),
    image: GpuImage,
    /// The image's dmabuf, kept to read the frame back.
    fd: OwnedFd,
    /// The image in the compositor's own GPU context, once a read needed it there.
    imported: Option<Imported>,
    /// Whether the band could not be read from this image, which is logged once.
    band_unread: bool,
}

impl Drop for GpuBuffer {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

/// The compositor's own GPU context for reading frames back, made the first time a frame in a
/// layout the CPU cannot read is read.
// One per compositor, so the reader stays inline rather than boxed.
#[allow(clippy::large_enum_variant)]
#[derive(Default)]
pub enum Reader {
    #[default]
    NotYet,
    Ready(DmabufReader),
    /// It could not be made; the reason was logged once.
    Failed(String),
}

impl Reader {
    fn get(&mut self) -> Result<&DmabufReader, String> {
        if matches!(self, Reader::NotYet) {
            *self = match DmabufReader::new() {
                Ok(reader) => Reader::Ready(reader),
                Err(why) => {
                    log(&format!(
                        "GPU frames in a tiled layout cannot be read back: {why}"
                    ));
                    Reader::Failed(why)
                }
            };
        }
        match self {
            Reader::Ready(reader) => Ok(reader),
            Reader::Failed(why) => Err(why.clone()),
            Reader::NotYet => Err("the GPU context is not made".into()),
        }
    }
}

/// Whether a GPU frame's band is due a new sample, last sampled at `last`.
pub fn band_probe_due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.saturating_duration_since(at) >= BAND_PROBE_EVERY)
}

impl State {
    /// Reads `rows` (from the top, inside the frame) of GPU image `slot` of `display_id`,
    /// `width` pixels from the left: by mapping it when it is in the linear layout, else on
    /// the GPU.
    pub(crate) fn read_gpu_rows(
        &mut self,
        display_id: u32,
        slot: u32,
        width: u32,
        rows: &[u32],
    ) -> Result<Rows, String> {
        let gpu = self
            .gpu_buffers
            .get_mut(&(display_id, slot))
            .ok_or_else(|| "the frame's GPU image is gone".to_string())?;
        let width = width.min(gpu.image.width);
        if rows.iter().any(|row| *row >= gpu.image.height) {
            return Err("the rows asked for are not in the frame".into());
        }
        let mut linear_failed = None;
        if gpu.image.modifier == DRM_FORMAT_MOD_LINEAR {
            match read_linear(&gpu.image, gpu.fd.as_fd(), width, rows) {
                Ok(read) => return Ok(read),
                Err(why) => linear_failed = Some(why),
            }
        }
        let reader = self
            .dmabuf_reader
            .get()
            .map_err(|why| match &linear_failed {
                Some(linear) => format!("{linear}; {why}"),
                None => why,
            })?;
        if gpu.imported.is_none() {
            gpu.imported = Some(reader.import(&gpu.image, gpu.fd.as_fd())?);
        }
        let imported = gpu.imported.as_ref().ok_or("the frame was not imported")?;
        reader.read(imported, width, rows)
    }

    /// The pixels of a GPU frame of `display_id` under the title bar's `band`, for a
    /// screenshot: a frame of the rows under the band.
    pub(crate) fn read_gpu_frame(
        &mut self,
        display_id: u32,
        frame: GpuFrame,
        band: u32,
    ) -> Result<Frame, String> {
        let rows: Vec<u32> = app_rows(band, frame.height).collect();
        if rows.is_empty() {
            return Err("the window shows nothing under its title bar".into());
        }
        let read = self.read_gpu_rows(display_id, frame.slot, frame.width, &rows)?;
        Ok(Frame {
            width: read.width,
            height: read.count,
            pixels: read.pixels,
        })
    }

    /// What a GPU frame shows under its band, from the two rows the band fill samples: the
    /// color the band takes when the app leaves it empty, and how light the band is. The
    /// default when the rows cannot be read (which is logged once for each image).
    fn gpu_band_sample(&mut self, display_id: u32, frame: GpuFrame, band: u32) -> BandSample {
        let Some([inside, below]) = band_probe_rows(band, frame.height) else {
            return BandSample::default();
        };
        match self.read_gpu_rows(display_id, frame.slot, frame.width, &[inside, below]) {
            Ok(read) => {
                let row = read.width as usize * 4;
                let (inside, below) = read.pixels.split_at(row.min(read.pixels.len()));
                rows_band_sample(inside, below, read.width)
            }
            Err(why) => {
                if let Some(gpu) = self.gpu_buffers.get_mut(&(display_id, frame.slot)) {
                    if !std::mem::replace(&mut gpu.band_unread, true) {
                        log(&format!(
                            "display {display_id}: the title bar cannot take the app's color: {why}"
                        ));
                    }
                }
                BandSample::default()
            }
        }
    }
}

/// The title bar's band over a GPU frame: a subsurface with a buffer of its own.
pub struct CaptionLayer {
    surface: WlSurface,
    subsurface: WlSubsurface,
    buffer: Option<ShmBuffer>,
    /// Where the layer stands in its window, in logical units.
    position: (i32, i32),
}

impl CaptionLayer {
    fn new(
        compositor: &wayland_client::protocol::wl_compositor::WlCompositor,
        subcompositor: &WlSubcompositor,
        parent: &WlSurface,
        task_id: i32,
        qh: &QueueHandle<State>,
    ) -> CaptionLayer {
        // The band's surface carries the task's ID, so a press on it reaches the window.
        let surface = compositor.create_surface(qh, task_id);
        let subsurface = subcompositor.get_subsurface(&surface, parent, qh, ());
        subsurface.set_sync();
        CaptionLayer {
            surface,
            subsurface,
            buffer: None,
            position: (0, 0),
        }
    }

    /// Takes the band away, as for a full screen window.
    pub fn hide(&mut self) {
        self.surface.attach(None, 0, 0);
        self.surface.commit();
        self.buffer = None;
    }

    pub fn destroy(self) {
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

impl State {
    /// Where `surface` sits in its task's window, in logical units: the title bar's layer
    /// where it was placed, the window's own surface at the origin.
    pub(crate) fn surface_offset(&self, task_id: i32, surface: &WlSurface) -> (f64, f64) {
        let Some(window) = self.windows.get(&task_id) else {
            return (0.0, 0.0);
        };
        match &window.caption_layer {
            Some(layer) if &layer.surface == surface => {
                (f64::from(layer.position.0), f64::from(layer.position.1))
            }
            _ => (0.0, 0.0),
        }
    }

    /// Whether the desktop can show the renderer's dmabufs, cropped, with a title bar above.
    pub fn takes_gpu_frames(&self) -> bool {
        self.dmabuf.is_some() && self.viewporter.is_some() && self.subcompositor.is_some()
    }

    /// The renderer exported a GPU image of a display: it becomes a buffer of the desktop.
    pub fn add_gpu_image(&mut self, image: GpuImage, fd: OwnedFd) {
        if let (Some(x11), Ok(copy)) = (&mut self.x11, fd.try_clone()) {
            x11.import(&image, copy);
        }
        let Some(dmabuf) = &self.dmabuf else {
            return;
        };
        let params = dmabuf.create_params(&self.qh, ());
        params.add(
            fd.as_fd(),
            0,
            image.offset,
            image.stride,
            (image.modifier >> 32) as u32,
            image.modifier as u32,
        );
        let buffer = params.create_immed(
            image.width as i32,
            image.height as i32,
            image.fourcc,
            zwp_linux_buffer_params_v1::Flags::empty(),
            &self.qh,
            Arc::new(AtomicBool::new(false)),
        );
        params.destroy();
        let replaced = self.gpu_buffers.insert(
            (image.display_id, image.slot),
            GpuBuffer {
                buffer,
                size: (image.width, image.height),
                image,
                fd,
                imported: None,
                band_unread: false,
            },
        );
        // The image it replaces leaves the compositor's GPU context too.
        if let (Some(imported), Reader::Ready(reader)) = (
            replaced.and_then(|mut old| old.imported.take()),
            &self.dmabuf_reader,
        ) {
            reader.release(imported);
        }
    }

    /// Shows a frame that is in one of the display's GPU images.
    pub fn present_gpu(&mut self, task_id: i32, display_id: u32, frame: GpuFrame) {
        let in_x11 = self
            .windows
            .get(&task_id)
            .is_some_and(|w| matches!(w.pip, Some(super::pip::Pip::X11)));
        if in_x11 {
            if let Some(x11) = &mut self.x11 {
                x11.show_gpu(task_id, display_id, frame);
            }
            if let Some(window) = self.windows.get_mut(&task_id) {
                window.last_gpu = Some((display_id, frame));
                window.model.frame_shown((frame.width, frame.height));
            }
            return;
        }
        // The band's fill and lightness, from two rows of the frame read back, now and then.
        let now = Instant::now();
        let probe = self.windows.get(&task_id).and_then(|window| {
            (window.model.has_caption()
                && window.pip.is_none()
                && band_probe_due(window.band_probed, now))
            // The band is the window's; the frame is read in its own pixels.
            .then(|| window.model.caption_rows_in_frame())
        });
        if let Some(band) = probe {
            let sample = self.gpu_band_sample(display_id, frame, band);
            if let Some(window) = self.windows.get_mut(&task_id) {
                window.gpu_band = sample;
                window.band_probed = Some(now);
            }
        }
        let Some(gpu) = self.gpu_buffers.get(&(display_id, frame.slot)) else {
            return;
        };
        let width = frame.width.min(gpu.size.0).max(1);
        let height = frame.height.min(gpu.size.1).max(1);
        let (Some(viewporter), Some(subcompositor)) = (&self.viewporter, &self.subcompositor)
        else {
            return;
        };
        let Some(window) = self.windows.get_mut(&task_id) else {
            return;
        };
        if !window.is_configured() {
            return;
        }
        window.last_gpu = Some((display_id, frame));
        // A task that keeps its density on a monitor of another scale fills the window at the
        // window's scale; the viewport scales it there.
        let in_window = window.model.frame_in_window((width, height));
        window.model.frame_shown(in_window);
        let (logical_width, logical_height) = window.model.to_logical(in_window);
        let in_pip = window.pip.is_some();
        let target = window.target().clone();
        let viewport = match window.pip.as_mut().and_then(|pip| pip.viewport()) {
            Some(viewport) => viewport,
            None => &mut window.viewport,
        };
        let viewport =
            viewport.get_or_insert_with(|| viewporter.get_viewport(&target, &self.qh, ()));
        viewport.set_source(0.0, 0.0, f64::from(width), f64::from(height));
        viewport.set_destination(logical_width, logical_height);

        // The title bar's band, on its own surface over the frame, filled with the app's top
        // color as sampled last when the app leaves the band empty, its glyphs in the tone
        // the band's lightness calls for.
        if window.model.has_caption() && !in_pip {
            let scale = window.model.scale.max(1) as i32;
            let parent = window.surface.clone();
            let sample = window.gpu_band;
            let (layout, pixels) = window.band_pixels(self.text.as_ref(), sample);
            let pixels = pixels.to_vec();
            let size = (layout.width.max(1), layout.height.max(1));
            let layer = window.caption_layer.get_or_insert_with(|| {
                CaptionLayer::new(&self.compositor, subcompositor, &parent, task_id, &self.qh)
            });
            let reuse = layer
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.size == size && !buffer.is_busy());
            if !reuse {
                layer.buffer =
                    ShmBuffer::with_format(&self.shm, &self.qh, size, Format::Argb8888).ok();
            }
            if let Some(buffer) = &mut layer.buffer {
                buffer.write(&pixels);
                buffer.set_busy();
                layer.position = (0, 0);
                layer.subsurface.set_position(0, 0);
                layer.surface.set_buffer_scale(scale);
                layer.surface.attach(Some(&buffer.buffer), 0, 0);
                layer
                    .surface
                    .damage_buffer(0, 0, size.0 as i32, size.1 as i32);
                layer.surface.commit();
            }
        } else if let Some(layer) = &mut window.caption_layer {
            layer.hide();
        }

        target.set_buffer_scale(1);
        target.attach(Some(&gpu.buffer), 0, 0);
        target.damage_buffer(0, 0, gpu.size.0 as i32, gpu.size.1 as i32);
        if !in_pip {
            window
                .xdg_surface()
                .set_window_geometry(0, 0, logical_width, logical_height);
        }
        target.commit();
    }
}

delegate_noop!(State: ignore ZwpLinuxDmabufV1);
delegate_noop!(State: ignore ZwpLinuxBufferParamsV1);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore WpViewport);
delegate_noop!(State: ignore WlSubcompositor);
delegate_noop!(State: ignore WlSubsurface);
