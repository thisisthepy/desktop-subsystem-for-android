//! `dsa-gfx`: the graphics renderer process on Linux.
//!
//! It takes its end of the compositor's socket pair (`--window-manager <descriptor>`), loads
//! gfxstream on the GPU the user's render GPU setting asks for, and serves the guest's
//! graphics streams. Each GPU image it exports crosses to the compositor as a dmabuf with a
//! [`GpuImage`] message, each frame in one as a [`GpuFrame`]; a frame read back crosses in a
//! memfd with a [`PixelFrame`]. It ends when the compositor's connection closes.

use std::io::Write;
use std::os::fd::{AsFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;

use dsa_host_core::memory::{HeldMemory, HostRole};
use dsa_host_core::renderer::{
    address_from, follow_device_state, open_renderer, RendererProcessMemory, WindowManagerLink,
};
use dsa_host_unix::fd_frames::{stream_at, FdConnection, FdSender};
use dsa_protocol::renderer::encode_body;
use dsa_protocol::renderer::types::{
    envelope::Body, FramePath as WireFramePath, GpuFrame as WireGpuFrame, GpuImage as WireGpuImage,
    PixelFrame, Started,
};
use dsa_transport::{Connection, TransportError};

use super::gpu_choice;
use crate::compositor::frames::{GpuFrame, GpuImage};
use crate::compositor::gfxstream::{self, FramePath, Listen, Renderer, RendererFrames};

/// What the renderer calls itself in its handshake.
pub fn build() -> String {
    format!("dsa-gfx {}", crate::VERSION)
}

/// Ends this process at once, without the libraries' teardown: a driver that hangs in it must
/// not keep a renderer whose compositor is gone.
fn end_now(code: i32) -> ! {
    // SAFETY: ends this process.
    unsafe { libc::_exit(code) }
}

/// The frames' way to the compositor.
struct RemoteFrames {
    link: Arc<WindowManagerLink>,
    sender: FdSender,
    on_gpu: bool,
}

/// A memfd holding `bytes`, to hand over with a message.
fn memfd_with(bytes: &[u8]) -> Option<OwnedFd> {
    // SAFETY: a new descriptor, owned from here on.
    let fd = unsafe { libc::memfd_create(c"dsa-frame".as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    // SAFETY: a descriptor nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let mut file = std::fs::File::from(fd);
    file.write_all(bytes).ok()?;
    Some(OwnedFd::from(file))
}

impl RendererFrames for RemoteFrames {
    fn takes_dmabufs(&self) -> bool {
        self.on_gpu
    }

    fn dmabuf(&self, image: GpuImage, fd: OwnedFd) -> bool {
        let message = encode_body(Body::GpuImage(WireGpuImage {
            display_id: image.display_id,
            slot: image.slot,
            width: image.width,
            height: image.height,
            stride: image.stride,
            offset: image.offset,
            fourcc: image.fourcc,
            modifier: image.modifier,
        }));
        // The compositor gets a descriptor of its own; this one closes when it goes.
        self.sender.send_with_fds(&message, &[fd.as_fd()]).is_ok()
    }

    fn dmabuf_frame(&self, display_id: u32, frame: GpuFrame) {
        self.link.send(Body::GpuFrame(WireGpuFrame {
            display_id,
            slot: frame.slot,
            width: frame.width,
            height: frame.height,
        }));
    }

    fn pixels(&self, display_id: u32, width: u32, height: u32, rgba: &[u8]) {
        let Some(memfd) = memfd_with(rgba) else {
            return;
        };
        let message = encode_body(Body::PixelFrame(PixelFrame {
            display_id,
            width,
            height,
        }));
        let _ = self.sender.send_with_fds(&message, &[memfd.as_fd()]);
    }
}

/// What this process holds, with gfxstream's own count.
fn memory(renderer: Option<&Renderer>) -> RendererProcessMemory {
    let own = crate::memory::process(std::process::id(), HostRole::Renderer);
    RendererProcessMemory {
        process_id: std::process::id(),
        held: HeldMemory {
            working_set_kb: own.as_ref().map(|p| p.working_set_kb),
            private_kb: own.as_ref().map(|p| p.private_kb),
            gpu_local_kb: None,
            gpu_non_local_kb: None,
        },
        counted: renderer.and_then(Renderer::memory),
    }
}

/// Runs the renderer for the compositor `args` names. Never returns: the process ends with
/// its compositor.
pub fn run(args: &[String]) -> ! {
    let Some(fd) = address_from(args).and_then(|address| address.parse::<RawFd>().ok()) else {
        eprintln!(
            "dsa-gfx is the graphics renderer of Desktop Subsystem for Android; the compositor \
             starts it"
        );
        end_now(2);
    };
    // A compositor gone mid-write must not end the renderer with a signal; it ends by itself.
    // SAFETY: ignoring a signal.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    // SAFETY: the compositor started this process with its end of the socket pair at `fd`.
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
    let sender = connection.sender();
    let link = WindowManagerLink::new(Box::new(sender.clone()));
    let panics = Arc::clone(&link);
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|at| format!(" at {}:{}", at.file(), at.line()))
            .unwrap_or_default();
        panics.note(&format!("panic{location}: {info}"));
    }));

    let frames: Arc<dyn RendererFrames> = Arc::new(RemoteFrames {
        link: Arc::clone(&link),
        sender: sender.clone(),
        on_gpu: start.frames_on_gpu,
    });
    let gpu = gpu_choice(&start);
    let mut started = Started {
        process_id: std::process::id(),
        ..Started::default()
    };
    let renderer = match Renderer::load(&gfxstream::backend_path(), gpu.as_ref(), frames) {
        Ok(renderer) => {
            started.notes.extend(renderer.notes().iter().cloned());
            started.frame_path = match renderer.frame_path() {
                FramePath::Gpu => WireFramePath::Gpu,
                FramePath::ReadBack => WireFramePath::ReadBack,
            } as i32;
            Some(Arc::new(renderer))
        }
        Err(why) => {
            started.notes.push(format!(
                "{why}; task windows open but show no frames until the renderer is installed"
            ));
            started.frame_path = WireFramePath::Nowhere as i32;
            None
        }
    };
    if dsa_protocol::renderer::send(&mut connection, Body::Started(started)).is_err() {
        end_now(5);
    }
    if let Some(renderer) = &renderer {
        // A lost device reaches the compositor the moment the renderer finds it, so it is
        // followed as a GPU reset (and the windows watched for frames that do not come back)
        // while the renderer makes its device again.
        let watched = Arc::clone(renderer);
        if let Err(why) = follow_device_state(
            Arc::clone(&link),
            Box::new(move || (watched.device_losses(), watched.device_generation())),
        ) {
            link.note(&format!(
                "the renderer's device state cannot be followed, so the compositor does not \
                 learn of a GPU reset: {why}"
            ));
        }
        let listen = Listen::from_env();
        let note = Arc::clone(&link);
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |line: &str| note.note(line));
        for port in [start.command_port, start.process_pipe_port] {
            gfxstream::serve_port(Arc::clone(renderer), listen.clone(), port, log.clone());
        }
    }

    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(_) => break,
        };
        let Ok(body) = dsa_protocol::renderer::decode_body(&frame) else {
            link.note("the compositor sent something that is not a message; ending");
            break;
        };
        match link.take(body) {
            Some(Body::ForgetDisplay(forget)) => {
                if let Some(renderer) = &renderer {
                    renderer.forget_display(forget.display_id);
                }
            }
            Some(Body::MemoryRequest(asked)) => {
                let report = memory(renderer.as_deref()).to_report(asked.request);
                link.send(Body::MemoryReport(report));
            }
            Some(Body::Stop(_)) => break,
            _ => {}
        }
    }
    link.close();
    end_now(0)
}
