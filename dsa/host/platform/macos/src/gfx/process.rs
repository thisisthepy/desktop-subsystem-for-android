//! `dsa-gfx`: the graphics renderer process on macOS.
//!
//! It takes its end of the window manager's socket pair (`--window-manager <descriptor>`),
//! loads gfxstream on MoltenVK, and serves the guest's graphics streams on the relay sockets
//! `Start` names. For each frame it asks the window manager for the display's surface, which
//! arrives once by the window manager's Mach service (or is looked up by its global ID), and
//! has gfxstream copy the frame into it on the GPU. A frame gfxstream reads back is written
//! into the surface here. It ends when the window manager's connection closes.

use std::collections::HashMap;
use std::ffi::c_void;
use std::os::fd::RawFd;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use dsa_host_core::compositor::GraphicsStreamKind;
use dsa_host_core::memory::HeldMemory;
use dsa_host_core::renderer::{
    address_from, follow_device_state, open_renderer, RendererProcessMemory, WindowManagerLink,
    TEXTURES_PER_DISPLAY,
};
use dsa_host_unix::fd_frames::{stream_at, FdConnection};
use dsa_protocol::renderer::types::{
    envelope::Body, FrameDone, FramePath as Wire, Started, Target,
};
use dsa_transport::{Connection, TransportError};

use crate::compositor::renderer::{FramePath, FrameSink, Renderer};
use crate::compositor::surface::Surface;
use crate::compositor::surface_port::receive_surfaces;
use crate::launchd::WM_LABEL;
use crate::wm::run::{bundled_vulkan_driver, renderer_path};

/// How long a surface handed over by Mach port may take to arrive after its `Target`.
const SURFACE_WAIT: Duration = Duration::from_millis(500);

/// What the renderer calls itself in its handshake.
pub fn build() -> String {
    format!("dsa-gfx {}", crate::VERSION)
}

fn end_now(code: i32) -> ! {
    // SAFETY: ends this process without the libraries' teardown.
    unsafe { libc::_exit(code) }
}

/// The surfaces the window manager sent over Mach, by texture ID, until a frame takes them.
#[derive(Default)]
struct Arrived {
    surfaces: Mutex<HashMap<u64, Surface>>,
    came: Condvar,
}

impl Arrived {
    fn take(&self, texture_id: u64) -> Option<Surface> {
        let deadline = Instant::now() + SURFACE_WAIT;
        let mut surfaces = self.surfaces.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(surface) = surfaces.remove(&texture_id) {
                return Some(surface);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            surfaces = self
                .came
                .wait_timeout(surfaces, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// The surfaces one display's frames go into, newest last, and the one its last frame went
/// into.
#[derive(Default)]
struct Held {
    surfaces: Vec<(u64, Surface)>,
    last: Option<u64>,
}

/// Where each display's frames go: the window manager's surfaces.
struct RemoteSink {
    link: Arc<WindowManagerLink>,
    arrived: Arc<Arrived>,
    held: Mutex<HashMap<u32, Held>>,
}

impl RemoteSink {
    /// The surface for display `display_id`'s next frame, asked of the window manager.
    fn surface(&self, display_id: u32, width: u32, height: u32, exact: bool) -> *mut c_void {
        let held: Vec<u64> = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&display_id)
            .map(|held| held.surfaces.iter().map(|(id, _)| *id).collect())
            .unwrap_or_default();
        // No answer in time: this frame is dropped, and the surfaces held stay for the next.
        let Some(target) = self.link.target(display_id, width, height, exact, held) else {
            return std::ptr::null_mut();
        };
        self.take(display_id, &target)
    }

    fn take(&self, display_id: u32, target: &Target) -> *mut c_void {
        let mut all = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if target.drop {
            all.remove(&display_id);
            return std::ptr::null_mut();
        }
        let held = all.entry(display_id).or_default();
        if let Some((_, surface)) = held
            .surfaces
            .iter()
            .find(|(id, _)| *id == target.texture_id)
        {
            held.last = Some(target.texture_id);
            return surface.as_raw();
        }
        let opened = if target.handle != 0 {
            Surface::lookup(target.handle as u32).ok()
        } else {
            self.arrived.take(target.texture_id)
        };
        let Some(surface) = opened else {
            return std::ptr::null_mut();
        };
        let raw = surface.as_raw();
        held.surfaces.push((target.texture_id, surface));
        while held.surfaces.len() > TEXTURES_PER_DISPLAY {
            held.surfaces.remove(0);
        }
        held.last = Some(target.texture_id);
        raw
    }

    /// The display's last frame is in its surface: the window manager is told.
    fn done(&self, display_id: u32, width: u32, height: u32) {
        let texture_id = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&display_id)
            .and_then(|held| held.last);
        if let Some(texture_id) = texture_id {
            self.link.send(Body::FrameDone(FrameDone {
                display_id,
                texture_id,
                width,
                height,
            }));
        }
    }

    fn forget(&self, display_id: u32) {
        self.held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&display_id);
    }
}

impl FrameSink for RemoteSink {
    fn pixels(&self, display_id: u32, width: u32, height: u32, rgba: &[u8]) {
        let raw = self.surface(display_id, width, height, false);
        if raw.is_null() {
            return;
        }
        let written = {
            let all = self.held.lock().unwrap_or_else(|e| e.into_inner());
            all.get(&display_id).and_then(|held| {
                let last = held.last?;
                held.surfaces
                    .iter()
                    .find(|(id, _)| *id == last)
                    .map(|(_, surface)| surface.write_rgba(width, height, rgba))
            })
        };
        if let Some(Ok(())) = written {
            self.done(display_id, width, height);
        }
    }

    fn target(&self, display_id: u32, width: u32, height: u32) -> *mut c_void {
        self.surface(display_id, width, height, true)
    }

    fn copied(&self, display_id: u32, width: u32, height: u32) {
        self.done(display_id, width, height);
    }
}

fn memory() -> RendererProcessMemory {
    let pid = std::process::id();
    let own = crate::host_memory::process_memory_kb(pid as i32);
    RendererProcessMemory {
        process_id: pid,
        held: HeldMemory {
            working_set_kb: own.map(|(resident, _)| resident),
            private_kb: own.map(|(_, footprint)| footprint),
            gpu_local_kb: None,
            gpu_non_local_kb: None,
        },
        counted: None,
    }
}

/// Runs the renderer for the window manager `args` names. Never returns.
pub fn run(args: &[String]) -> ! {
    let Some(fd) = address_from(args).and_then(|address| address.parse::<RawFd>().ok()) else {
        eprintln!(
            "dsa-gfx is the graphics renderer of Desktop Subsystem for Android; the window \
             manager starts it"
        );
        end_now(2);
    };
    // SAFETY: ignoring a signal; a window manager gone mid-write ends this process by itself.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // SAFETY: the window manager started this process with its end of the pair at `fd`.
    let stream = unsafe { stream_at(fd) };
    let Ok(mut connection) = FdConnection::new(stream) else {
        end_now(3);
    };
    let start = match open_renderer(&mut connection, &build()) {
        Ok(start) => start,
        Err(why) => {
            eprintln!("dsa-gfx: {why}");
            end_now(4);
        }
    };
    let link = WindowManagerLink::new(Box::new(connection.sender()));
    let panics = Arc::clone(&link);
    std::panic::set_hook(Box::new(move |info| {
        panics.note(&format!("panic: {info}"));
    }));
    // The Vulkan loader finds MoltenVK through the bundle's driver description, unless the
    // environment names another.
    if std::env::var_os("VK_ICD_FILENAMES").is_none() {
        if let Some(driver) = bundled_vulkan_driver() {
            std::env::set_var("VK_ICD_FILENAMES", driver);
        }
    }

    let mut started = Started {
        process_id: std::process::id(),
        ..Started::default()
    };
    let arrived = Arc::new(Arrived::default());
    let filed = Arc::clone(&arrived);
    if let Err(why) = receive_surfaces(WM_LABEL, move |texture_id, surface| {
        filed
            .surfaces
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(texture_id, surface);
        filed.came.notify_all();
    }) {
        started.notes.push(format!(
            "{why}; the frame surfaces are found by their global IDs"
        ));
    }
    let sink = Arc::new(RemoteSink {
        link: Arc::clone(&link),
        arrived,
        held: Mutex::new(HashMap::new()),
    });
    // `GFXSTREAM_BACKEND_PATH` names another backend, as the renderer's tests do.
    let path = std::env::var_os("GFXSTREAM_BACKEND_PATH")
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(renderer_path);
    let display = (start.display_width.max(1), start.display_height.max(1));
    let renderer = match Renderer::load(&path, display, Arc::clone(&sink) as Arc<dyn FrameSink>) {
        Ok(renderer) => {
            started.frame_path = match renderer.frame_path() {
                FramePath::Gpu => Wire::Gpu,
                FramePath::ReadBack => Wire::ReadBack,
            } as i32;
            Some(Arc::new(renderer))
        }
        Err(why) => {
            started.notes.push(format!(
                "{why}; task windows open but show no frames until the renderer is installed"
            ));
            started.frame_path = Wire::Nowhere as i32;
            None
        }
    };
    if dsa_protocol::renderer::send(&mut connection, Body::Started(started)).is_err() {
        end_now(5);
    }
    // A lost device reaches the window manager the moment the renderer finds it, so it is
    // followed as a GPU reset (and the windows watched for frames that do not come back)
    // while the renderer makes its device again.
    if let Some(renderer) = &renderer {
        let watched = Arc::clone(renderer);
        if let Err(why) = follow_device_state(
            Arc::clone(&link),
            Box::new(move || (watched.device_losses(), watched.device_generation())),
        ) {
            link.note(&format!(
                "the renderer's device state cannot be followed, so the window manager does \
                 not learn of a GPU reset: {why}"
            ));
        }
    }
    let stop = Arc::new(AtomicBool::new(false));
    for (kind, socket) in [
        (GraphicsStreamKind::Command, &start.command_socket),
        (GraphicsStreamKind::ProcessPipe, &start.process_pipe_socket),
    ] {
        if socket.is_empty() {
            continue;
        }
        if let Err(why) = crate::wm::graphics::serve(
            PathBuf::from(socket),
            kind,
            renderer.clone(),
            Arc::clone(&stop),
        ) {
            link.note(&why);
        }
    }

    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(_) => break,
        };
        let Ok(body) = dsa_protocol::renderer::decode_body(&frame) else {
            break;
        };
        match link.take(body) {
            Some(Body::ForgetDisplay(forget)) => {
                if let Some(renderer) = &renderer {
                    renderer.forget_display(forget.display_id);
                }
                sink.forget(forget.display_id);
            }
            Some(Body::MemoryRequest(asked)) => {
                link.send(Body::MemoryReport(memory().to_report(asked.request)));
            }
            Some(Body::Stop(_)) => break,
            _ => {}
        }
    }
    link.close();
    end_now(0)
}
