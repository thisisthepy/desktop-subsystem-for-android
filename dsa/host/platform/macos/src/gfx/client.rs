//! The window manager's side of the renderer process on macOS: what it starts `dsa-gfx` with,
//! and handing it each display's surface.
//!
//! A frame's request is answered with the surface the [`Presenter`] gives for the display (the
//! one its window host shows). A surface the renderer does not hold yet goes to it once, by
//! the window manager's Mach service or by its global ID; each finished frame then reaches the
//! presenter as before, and the window host is told of it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use dsa_host_core::renderer::{
    DeviceChanges, Given, Pending, RendererHandler, RendererLink, RendererProcessMemory,
    TextureBook, QUESTION_TIMEOUT,
};
use dsa_protocol::renderer::types::{
    envelope::Body, ForgetDisplay, FramePath, MemoryReport, MemoryRequest, Start, Started, Target,
    TargetRequest,
};

use crate::compositor::renderer::FrameSink;
use crate::compositor::surface::Surface;
use crate::wm::log;
use crate::wm::present::Presenter;

/// How long the handover of a surface waits for the renderer's Mach port.
const PORT_WAIT: Duration = Duration::from_millis(500);

/// The renderer process, as the window manager sees it.
pub struct RendererClient {
    presenter: Arc<Presenter>,
    /// The relay sockets of the command stream and the process pipe.
    sockets: (PathBuf, PathBuf),
    display: (u32, u32),
    link: Mutex<Option<RendererLink>>,
    book: Mutex<TextureBook<usize>>,
    memory: Pending<MemoryReport>,
    /// Each DeviceState the renderer sent: a change is a GPU reset.
    device_changes: DeviceChanges,
    starts: Mutex<u64>,
}

impl RendererClient {
    /// A client whose renderer serves the streams on `sockets` for a guest display of
    /// `display` pixels, and whose frames go to `presenter`.
    pub fn new(
        presenter: Arc<Presenter>,
        sockets: (PathBuf, PathBuf),
        display: (u32, u32),
    ) -> Arc<RendererClient> {
        let client = Arc::new(RendererClient {
            presenter: Arc::clone(&presenter),
            sockets,
            display,
            link: Mutex::new(None),
            book: Mutex::new(TextureBook::new()),
            memory: Pending::new(),
            device_changes: DeviceChanges::new(),
            starts: Mutex::new(0),
        });
        let weak: Weak<RendererClient> = Arc::downgrade(&client);
        presenter.on_forget(Box::new(move |display_id| {
            if let Some(client) = weak.upgrade() {
                client.forget(display_id);
            }
        }));
        client
    }

    /// A count that changes each time the renderer finds its GPU device lost or makes it
    /// again: the window manager takes any change as a GPU reset, the moment the renderer
    /// finds it.
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

    fn forget(&self, display_id: u32) {
        self.book
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .withdraw(display_id);
        let link = self.link.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(link) = link {
            link.send(Body::ForgetDisplay(ForgetDisplay { display_id }));
        }
    }

    fn answer(&self, request: TargetRequest, link: &RendererLink) {
        let raw = self.presenter.target_for(
            request.display_id,
            request.width,
            request.height,
            request.exact,
        );
        let handle = (!raw.is_null()).then_some(raw as usize);
        let given = self.book.lock().unwrap_or_else(|e| e.into_inner()).give(
            request.display_id,
            handle,
            &request.held_textures,
        );
        let mut target = Target {
            request: request.request,
            drop: true,
            width: request.width,
            height: request.height,
            ..Target::default()
        };
        match given {
            Given::Drop => {}
            Given::Same(texture_id) => {
                target.drop = false;
                target.texture_id = texture_id;
            }
            Given::New(texture_id) => {
                target.drop = false;
                target.texture_id = texture_id;
                // By Mach port where the window manager has its service, else by global ID.
                // SAFETY: `raw` is the non-null IOSurfaceRef the presenter just gave for this
                // display, which stays valid until the display gets another surface or is no
                // longer shown, neither of which happens during this call.
                let sent = self.presenter.ports().is_some_and(|ports| unsafe {
                    ports.send_raw(link.process_id(), texture_id, raw, PORT_WAIT)
                });
                if !sent {
                    // SAFETY: as above.
                    target.handle = u64::from(unsafe { Surface::id_of(raw) });
                }
            }
        }
        link.send(Body::Target(target));
    }
}

impl RendererHandler for RendererClient {
    fn start(&self) -> Start {
        Start {
            display_width: self.display.0,
            display_height: self.display.1,
            command_socket: self.sockets.0.display().to_string(),
            process_pipe_socket: self.sockets.1.display().to_string(),
            frames_on_gpu: true,
            ..Start::default()
        }
    }

    fn connected(&self, started: &Started, link: &RendererLink) {
        for note in &started.notes {
            log(&format!("graphics: {note}"));
        }
        let path = match FramePath::try_from(started.frame_path) {
            Ok(FramePath::Gpu) => "frames go to the windows on the GPU",
            Ok(FramePath::ReadBack) => {
                "this renderer reads frames back to memory; they still reach the windows, at a \
                 higher cost"
            }
            _ => "it shows no frames",
        };
        self.book.lock().unwrap_or_else(|e| e.into_inner()).clear();
        let mut starts = self.starts.lock().unwrap_or_else(|e| e.into_inner());
        log(&format!(
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

    fn message(&self, body: Body, link: &RendererLink) {
        match body {
            Body::TargetRequest(request) => self.answer(request, link),
            Body::FrameDone(done) => {
                let current = self
                    .book
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .texture_of(done.display_id);
                if current == Some(done.texture_id) {
                    self.presenter
                        .copied(done.display_id, done.width, done.height);
                }
            }
            Body::MemoryReport(report) => self.memory.complete(report.request, report),
            Body::DeviceState(state) => {
                // The manager looks at this at its next tick and takes the change as a GPU
                // reset.
                let line = self.device_changes.told(&state);
                log(&format!("graphics: {line}"));
            }
            Body::RendererLog(line) => {
                let severity = char::from_u32(line.severity).unwrap_or('E');
                log(&format!("graphics renderer {severity}: {}", line.message));
            }
            Body::Note(line) => log(&format!("graphics: {}", line.text)),
            _ => {}
        }
    }

    fn disconnected(&self) {
        *self.link.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }

    fn log(&self, line: &str) {
        log(&format!("graphics: {line}"));
    }
}
