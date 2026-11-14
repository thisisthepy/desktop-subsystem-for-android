//! The compositor's side of the renderer process: what it starts `dsa-gfx` with, and where the
//! renderer's frames go.
//!
//! Each [`GpuImage`](dsa_protocol::renderer::types::GpuImage) brings its dmabuf with it, which
//! becomes a buffer of the desktop as before; each `GpuFrame` says which image a display's
//! frame is in. A frame read back comes in a memfd. They all reach the same [`FrameSink`] the
//! renderer used to fill in-process, so the windows draw them as they always have, and keep
//! their last frame while the renderer is away.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{Arc, Mutex};

use dsa_host_core::renderer::{
    DeviceChanges, Pending, RendererHandler, RendererLink, RendererProcessMemory, QUESTION_TIMEOUT,
};
use dsa_host_unix::renderer_launch::CurrentFds;
use dsa_protocol::renderer::types::{
    envelope::Body, FramePath, MemoryReport, MemoryRequest, Start, Started,
};

use super::start_for;
use crate::compositor::frames::{GpuFrame, GpuImage};
use crate::compositor::gfxstream::{FrameSink, RendererFrames};

/// The largest frame read back, in bytes: a 16384 x 16384 RGBA frame.
const MAX_PIXELS: u64 = 16384 * 16384 * 4;

/// The renderer process, as the compositor sees it.
pub struct RendererClient {
    sink: FrameSink,
    fds: CurrentFds,
    render_gpu: String,
    link: Mutex<Option<RendererLink>>,
    memory: Pending<MemoryReport>,
    /// Each DeviceState the renderer sent: a change is a GPU reset.
    device_changes: DeviceChanges,
    starts: Mutex<u64>,
    log: Box<dyn Fn(&str) + Send + Sync>,
}

/// Reads `width` x `height` RGBA pixels out of `memfd`.
fn read_pixels(memfd: &OwnedFd, width: u32, height: u32) -> Option<Vec<u8>> {
    let len = u64::from(width) * u64::from(height) * 4;
    if len == 0 || len > MAX_PIXELS {
        return None;
    }
    let mut pixels = vec![0u8; len as usize];
    // SAFETY: reads into a buffer of `len` bytes from a descriptor this process owns.
    let read = unsafe {
        libc::pread(
            memfd.as_raw_fd(),
            pixels.as_mut_ptr().cast(),
            pixels.len(),
            0,
        )
    };
    (read as u64 == len).then_some(pixels)
}

impl RendererClient {
    /// A client whose renderer runs on the GPU `render_gpu` names (the user's setting) and
    /// whose frames go to `sink`. `fds` is where the launcher puts each renderer's received
    /// descriptors.
    pub fn new(
        sink: FrameSink,
        fds: CurrentFds,
        render_gpu: String,
        log: Box<dyn Fn(&str) + Send + Sync>,
    ) -> Arc<RendererClient> {
        Arc::new(RendererClient {
            sink,
            fds,
            render_gpu,
            link: Mutex::new(None),
            memory: Pending::new(),
            device_changes: DeviceChanges::new(),
            starts: Mutex::new(0),
            log,
        })
    }

    /// A count that changes each time the renderer finds its GPU device lost or makes it
    /// again: the compositor takes any change as a GPU reset, the moment the renderer finds
    /// it.
    pub fn device_changes(&self) -> u64 {
        self.device_changes.count()
    }

    /// How many renderers have started.
    pub fn starts(&self) -> u64 {
        *self.starts.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The process ID of the renderer running now.
    pub fn renderer_process_id(&self) -> Option<u32> {
        self.link
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(RendererLink::process_id)
    }

    /// What the renderer running now holds; `None` when none runs or it does not answer.
    pub fn memory(&self) -> Option<RendererProcessMemory> {
        let link = self
            .link
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        let request = self.memory.begin();
        if !link.send(Body::MemoryRequest(MemoryRequest {
            request,
            give_back: false,
        })) {
            return None;
        }
        self.memory
            .wait(request, QUESTION_TIMEOUT)
            .map(|report| RendererProcessMemory::from_report(&report))
    }

    /// The next descriptor the renderer sent.
    fn take_fd(&self) -> Option<OwnedFd> {
        let fds = self.fds.lock().unwrap_or_else(|e| e.into_inner()).clone()?;
        let mut queue = fds.lock().unwrap_or_else(|e| e.into_inner());
        queue.pop_front()
    }
}

impl RendererHandler for RendererClient {
    fn start(&self) -> Start {
        start_for(&self.render_gpu, self.sink.takes_dmabufs())
    }

    fn connected(&self, started: &Started, link: &RendererLink) {
        for note in &started.notes {
            (self.log)(&format!("graphics: {note}"));
        }
        let path = match FramePath::try_from(started.frame_path) {
            Ok(FramePath::Gpu) => "frames go to the windows on the GPU",
            Ok(FramePath::ReadBack) => "frames are read back to memory",
            _ => "it shows no frames",
        };
        let mut starts = self.starts.lock().unwrap_or_else(|e| e.into_inner());
        (self.log)(&format!(
            "graphics: the renderer (dsa-gfx, pid {}) is {}; {path}",
            link.process_id(),
            if *starts > 0 {
                "running again"
            } else {
                "running"
            }
        ));
        *starts += 1;
        *self.link.lock().unwrap_or_else(|e| e.into_inner()) = Some(link.clone());
    }

    fn message(&self, body: Body, _link: &RendererLink) {
        match body {
            Body::GpuImage(image) => {
                let Some(fd) = self.take_fd() else {
                    (self.log)("graphics: a GPU image came without its dmabuf");
                    return;
                };
                self.sink.dmabuf(
                    GpuImage {
                        display_id: image.display_id,
                        slot: image.slot,
                        width: image.width,
                        height: image.height,
                        stride: image.stride,
                        offset: image.offset,
                        fourcc: image.fourcc,
                        modifier: image.modifier,
                    },
                    fd,
                );
            }
            Body::GpuFrame(frame) => self.sink.dmabuf_frame(
                frame.display_id,
                GpuFrame {
                    slot: frame.slot,
                    width: frame.width,
                    height: frame.height,
                },
            ),
            Body::PixelFrame(frame) => {
                let Some(memfd) = self.take_fd() else {
                    (self.log)("graphics: a frame came without its pixels");
                    return;
                };
                if let Some(pixels) = read_pixels(&memfd, frame.width, frame.height) {
                    self.sink
                        .pixels(frame.display_id, frame.width, frame.height, &pixels);
                }
            }
            Body::MemoryReport(report) => self.memory.complete(report.request, report),
            Body::DeviceState(state) => {
                // The main loop looks at this at its next tick and takes the change as a GPU
                // reset.
                let line = self.device_changes.told(&state);
                (self.log)(&format!("graphics: {line}"));
            }
            Body::RendererLog(line) => {
                let severity = char::from_u32(line.severity).unwrap_or('E');
                (self.log)(&format!("graphics renderer {severity}: {}", line.message));
            }
            Body::Note(line) => (self.log)(&format!("graphics: {}", line.text)),
            _ => {}
        }
    }

    fn disconnected(&self) {
        *self.link.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // The descriptors of the renderer that ended are of no more use; the windows keep the
        // buffers they already made.
        if let Some(fds) = self.fds.lock().unwrap_or_else(|e| e.into_inner()).take() {
            fds.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    fn log(&self, line: &str) {
        (self.log)(&format!("graphics: {line}"));
    }
}
