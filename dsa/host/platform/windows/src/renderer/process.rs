//! `dsa-gfx.exe`: the graphics renderer process on Windows.
//!
//! It dials the window manager that started it (`--window-manager <pipe>`), loads
//! `gfxstream_backend.dll` on the GPU the window manager's `Start` asks for, and serves the
//! guest's graphics streams on the Hyper-V sockets of the VM the window manager names. Each
//! display's frame is copied on the GPU into the texture the window manager hands over (its
//! NT handle duplicated into this process), and the window manager is told once the copy is
//! done.
//!
//! The renderer lives only as long as its window manager: when the window manager's
//! connection ends, so does this process, at once, so a renderer can never be left holding
//! the VM's graphics ports for a window manager that is gone.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use dsa_host_core::compositor::{GraphicsRenderer, GraphicsStreamKind};
use dsa_host_core::renderer::{
    address_from, follow_device_state, open_renderer, HeldTargets, RendererProcessMemory, ServedVm,
    WindowManagerLink, WINDOW_MANAGER_ARGUMENT,
};
use dsa_protocol::renderer::types::{envelope::Body, FrameDone, FramePath, Started};
use dsa_protocol::renderer::{decode_body, send};
use dsa_transport::{Connector, Listener, TransportError};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

use super::{report_gpu, request_of};
use crate::compositor::adapter::{DisplayPresenter, DisplaySink, GfxstreamAdapter};
use crate::guid::Guid;
use crate::hvsocket::RawHvSocketListener;
use crate::pipe::SameUserPipeConnector;
use crate::vm_listener::{BindFn, VmBoundListener, VmIdSlot};

/// What the renderer calls itself in its handshake.
pub fn build() -> String {
    format!("dsa-gfx {}", crate::VERSION)
}

/// Ends this process at once with `code`, without running the libraries' shutdown: a driver
/// that hangs or crashes in its teardown must not keep a renderer whose window manager is
/// gone.
fn end_now(code: u32) -> ! {
    // SAFETY: ends this process.
    unsafe {
        TerminateProcess(GetCurrentProcess(), code);
    }
    std::process::exit(code as i32)
}

/// Where each display's frames go: asks the window manager for the texture, and tells it
/// once the frame is there.
pub struct RemoteSink {
    link: Arc<WindowManagerLink>,
    held: Mutex<HeldTargets<isize>>,
}

fn close(handle: isize) {
    // SAFETY: a handle the window manager duplicated into this process, closed once.
    unsafe { CloseHandle(handle as HANDLE) };
}

impl RemoteSink {
    pub fn new(link: Arc<WindowManagerLink>) -> RemoteSink {
        RemoteSink {
            link,
            // The window manager gives each display one texture, and a new one only when the
            // window outgrows it (`RemoteWindow::texture`), so the one it replaced is closed
            // here as soon as the new one is open rather than kept with nothing to use it.
            held: Mutex::new(HeldTargets::keeping(1)),
        }
    }

    /// No window shows `display_id` now: its textures' handles go.
    pub fn forget(&self, display_id: u32) {
        let released = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget(display_id);
        for handle in released {
            close(handle);
        }
    }
}

impl DisplaySink for RemoteSink {
    fn target(&self, display_id: u32, width: u32, height: u32) -> Option<isize> {
        let held = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .held(display_id);
        // No answer in time: this frame is dropped, and the texture held stays for the next.
        let target = self.link.target(display_id, width, height, true, held)?;
        let (write, release) = self.held.lock().unwrap_or_else(|e| e.into_inner()).take(
            display_id,
            &target,
            |target| (target.handle != 0).then_some(target.handle as usize as isize),
        );
        for old in release {
            close(old);
        }
        write
    }

    fn frame(&self, display_id: u32, content: (u32, u32)) {
        let current = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .current(display_id);
        if let Some((texture_id, _)) = current {
            self.link.send(Body::FrameDone(FrameDone {
                display_id,
                texture_id,
                width: content.0,
                height: content.1,
            }));
        }
    }
}

/// This process's working set and private commit, in KiB.
fn own_memory_kb() -> (Option<u64>, Option<u64>) {
    // SAFETY: plain counters, zero until filled.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    counters.cb = size;
    // SAFETY: our own process's pseudo handle, and counters of the size given; the extended
    // counters start with the plain ones.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            size,
        )
    };
    if ok == 0 {
        return (None, None);
    }
    (
        Some(counters.WorkingSetSize as u64 / 1024),
        Some(counters.PrivateUsage as u64 / 1024),
    )
}

/// What this process holds, with the renderer's own count.
fn memory(adapter: &GfxstreamAdapter) -> RendererProcessMemory {
    let (working_set_kb, private_kb) = own_memory_kb();
    RendererProcessMemory {
        process_id: std::process::id(),
        held: dsa_host_core::memory::HeldMemory {
            working_set_kb,
            private_kb,
            gpu_local_kb: None,
            gpu_non_local_kb: None,
        },
        counted: adapter.counted_memory(),
    }
}

/// No task window is shown: gives back the memory the renderer is not using. The guest's
/// surfaces stay, so a window that opens again shows at once; pages touched again come back
/// from the standby list.
fn give_back() {
    crate::compositor::committed::compact_heaps();
    // SAFETY: our own process's pseudo handle.
    unsafe {
        K32EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Accepts the guest's streams of `kind` on `port` of the VM `vm` names, and serves each
/// through `renderer` on a thread of its own, for as long as the process runs.
fn serve_port(
    renderer: Arc<GfxstreamAdapter>,
    vm: VmIdSlot,
    kind: GraphicsStreamKind,
    port: u32,
    link: Arc<WindowManagerLink>,
) {
    let bind: BindFn = Box::new(move |vm_id: Guid| {
        RawHvSocketListener::bind(vm_id, Guid::vsock_service(port))
            .map(|listener| Box::new(listener) as Box<dyn Listener>)
    });
    let mut listener = VmBoundListener::new(vm, bind);
    let _ = thread::Builder::new()
        .name(format!("dsa-gfx-{port}"))
        .spawn(move || {
            let mut reported = false;
            loop {
                match listener.accept(Duration::from_millis(200)) {
                    Ok(connection) => {
                        reported = false;
                        let renderer = Arc::clone(&renderer);
                        let link = Arc::clone(&link);
                        let _ = thread::Builder::new()
                            .name(format!("dsa-gfx-stream-{port}"))
                            .spawn(move || {
                                if let Err(why) = renderer.serve_stream(kind, connection) {
                                    link.note(&format!("a stream on port {port} ended: {why}"));
                                }
                            });
                    }
                    Err(TransportError::Timeout) => {}
                    Err(error) => {
                        if !reported {
                            link.note(&format!(
                                "the guest's graphics streams cannot be taken on port {port}: \
                                 {error}"
                            ));
                            reported = true;
                        }
                        thread::sleep(Duration::from_millis(500));
                    }
                }
            }
        });
}

/// Runs the renderer for the window manager `args` names. Never returns: the process ends
/// when its window manager does.
pub fn run(args: &[String]) -> ! {
    let Some(pipe) = address_from(args) else {
        eprintln!(
            "dsa-gfx is the graphics renderer of Desktop Subsystem for Android; the window \
             manager starts it ({WINDOW_MANAGER_ARGUMENT} <pipe>)"
        );
        end_now(2);
    };
    let mut connector = SameUserPipeConnector { name: pipe.clone() };
    let mut connection = match connector.connect(Duration::from_secs(10)) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("dsa-gfx: the window manager's pipe {pipe} cannot be opened: {error}");
            end_now(3);
        }
    };
    let start = match open_renderer(&mut *connection, &build()) {
        Ok(start) => start,
        Err(why) => {
            eprintln!("dsa-gfx: {why}");
            end_now(4);
        }
    };
    let Some(sender) = connection.try_clone_sender() else {
        end_now(5);
    };
    let link = WindowManagerLink::new(sender);

    // gfxstream's lines, and a panic in this process, go to the window manager's log, so the
    // reason a renderer ended is there even when the process then ends.
    let forward = Arc::clone(&link);
    crate::compositor::log::renderer_log_through(Box::new(move |severity: u8, message: &str| {
        forward.renderer_log(severity, message);
    }));
    let panics = Arc::clone(&link);
    std::panic::set_hook(Box::new(move |info| {
        let thread = thread::current();
        let location = info
            .location()
            .map(|at| format!(" at {}:{}", at.file(), at.line()))
            .unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic".to_string());
        panics.note(&format!(
            "panic in thread '{}'{location}: {message}",
            thread.name().unwrap_or("unnamed")
        ));
    }));
    // The renderer's threads carry every frame.
    if let Err(why) = crate::scheduling::raise_this_process() {
        link.note(&why);
    }

    let request = request_of(&start);
    // `GFXSTREAM_BACKEND_PATH` names another backend library, as the renderer's tests do;
    // otherwise it is the one installed beside this program.
    let backend = std::env::var_os("GFXSTREAM_BACKEND_PATH")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file());
    let adapter = Arc::new(GfxstreamAdapter::on_gpu(backend, &request));
    let mut started = Started {
        process_id: std::process::id(),
        notes: adapter.notes().to_vec(),
        ..Started::default()
    };
    report_gpu(&mut started, adapter.renderer_gpu().as_ref());
    let sink = Arc::new(RemoteSink::new(Arc::clone(&link)));
    let frame_path = if adapter.is_available() {
        match adapter.present_displays(Some(Arc::clone(&sink) as Arc<dyn DisplaySink>)) {
            Ok(()) => FramePath::Gpu,
            Err(why) => {
                started.notes.push(why);
                FramePath::Nowhere
            }
        }
    } else {
        FramePath::Nowhere
    };
    started.frame_path = frame_path as i32;
    started.adapter_luid = DisplayPresenter::adapter_luid(&*adapter)
        .map(|luid| luid.to_vec())
        .unwrap_or_default();
    if send(&mut *connection, Body::Started(started)).is_err() {
        end_now(6);
    }
    // A lost device reaches the window manager the moment the renderer finds it, not when the
    // window manager's own Direct3D device fails, which on the owner's PC was 13 s later.
    let watched = Arc::clone(&adapter);
    if let Err(why) = follow_device_state(
        Arc::clone(&link),
        Box::new(move || (watched.device_losses(), watched.device_generation())),
    ) {
        link.note(&format!(
            "the renderer's device state cannot be followed, so the window manager learns of \
             a GPU reset only from its own device: {why}"
        ));
    }

    let vm: VmIdSlot = Arc::default();
    let mut served = ServedVm::new();
    for (kind, port) in [
        (GraphicsStreamKind::Command, start.command_port),
        (GraphicsStreamKind::ProcessPipe, start.process_pipe_port),
    ] {
        serve_port(
            Arc::clone(&adapter),
            Arc::clone(&vm),
            kind,
            port,
            Arc::clone(&link),
        );
    }

    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(_) => break,
        };
        let Ok(body) = decode_body(&frame) else {
            link.note("the window manager sent something that is not a message; ending");
            break;
        };
        let Some(body) = link.take(body) else {
            continue;
        };
        match body {
            Body::GuestVm(guest) => {
                // The VM this renderer served is gone: a fresh renderer, which the window
                // manager starts at once, serves the next one without what this one kept.
                let told = (!guest.vm_id.is_empty()).then_some(guest.vm_id.as_str());
                if served.told(told) {
                    let (_, private_kb) = own_memory_kb();
                    link.note(&format!(
                        "the VM this renderer served is gone; it ends so that the window \
                         manager starts a fresh one for the next VM, without the {} MiB of \
                         private memory this one holds",
                        private_kb.map_or("?".to_string(), |kb| (kb / 1024).to_string())
                    ));
                    break;
                }
                let id = if guest.vm_id.is_empty() {
                    None
                } else {
                    match guest.vm_id.parse::<Guid>() {
                        Ok(id) => Some(id),
                        Err(_) => {
                            link.note(&format!("the VM ID {:?} is not a GUID", guest.vm_id));
                            None
                        }
                    }
                };
                *vm.lock().unwrap_or_else(|e| e.into_inner()) = id;
            }
            Body::ForgetDisplay(forget) => {
                DisplayPresenter::forget_display(&*adapter, forget.display_id);
                sink.forget(forget.display_id);
            }
            Body::MemoryRequest(asked) => {
                if asked.give_back {
                    give_back();
                }
                let report = memory(&adapter).to_report(asked.request);
                link.send(Body::MemoryReport(report));
            }
            Body::Stop(_) => break,
            _ => {}
        }
    }
    link.close();
    end_now(0)
}
