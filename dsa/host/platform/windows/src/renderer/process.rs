//! `dsa-gfx.exe`: the graphics renderer process on Windows.
//!
//! It dials the window manager that started it (`--window-manager <pipe>`), loads
//! `gfxstream_backend.dll` on the GPU the window manager's `Start` asks for, and serves the
//! guest's graphics streams on the Hyper-V sockets of the VM the window manager names. Each
//! display's frame is copied on the GPU into the texture the window manager hands over (its
//! NT handle duplicated into this process), and the window manager is told once the copy is
//! done.
//!
//! A window manager that stops the renderer, or says the VM it served is gone, ends it at once.
//! One that ends without a word (it crashed, or was ended from outside) leaves it waiting on
//! [`adoption_pipe`](super::adoption_pipe) for the next window manager, for
//! [`ADOPTION_WAIT`](dsa_host_core::renderer::ADOPTION_WAIT): the guest's streams and everything
//! its processes made here stay, so Android's apps keep drawing their text, images and icons
//! after the window manager restarts. When no window manager takes it over in that time, it
//! ends, so a renderer is never left holding the VM's graphics ports for nobody.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::{GraphicsRenderer, GraphicsStreamKind};
use dsa_host_core::renderer::{
    address_from, adopted_started, adoption_refusal, adoption_wait, follow_device_state,
    open_renderer, HeldTargets, RendererProcessMemory, ServedVm, WindowManagerLeft,
    WindowManagerLink, WINDOW_MANAGER_ARGUMENT,
};
use dsa_protocol::renderer::types::{envelope::Body, FrameDone, FramePath, Note, Start, Started};
use dsa_protocol::renderer::{decode_body, send};
use dsa_transport::local::LocalListener;
use dsa_transport::{Connection, Connector, Listener, TransportError};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, TerminateProcess};

use super::{adoption_pipe, report_gpu, report_vulkan_driver, request_of, vulkan_driver_of};
use crate::compositor::adapter::{DisplayPresenter, DisplaySink, GfxstreamAdapter};
use crate::guid::Guid;
use crate::hvsocket::RawHvSocketListener;
use crate::pipe::{current_user_only_sddl, NamedPipeListener, SameUserPipeConnector};
use crate::vm_listener::{BindFn, VmBoundListener, VmIdSlot};
use crate::vulkan_driver::{self, DriverOutcome};

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

    /// The window manager that gave the textures is gone: every display lets go of its texture,
    /// in `presenter` first and then here. The next window manager numbers its textures from 1
    /// again and makes new ones, and a texture kept under an old number, or an old handle value
    /// a new handle took, would have the frames written where nothing shows them.
    pub fn release_all(&self, presenter: &dyn DisplayPresenter) {
        let displays = self
            .held
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .displays();
        for display_id in displays {
            presenter.forget_display(display_id);
            self.forget(display_id);
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
/// Writes every live color buffer, as the renderer lists it, to the user's
/// `renderer-color-buffers.txt` beside the window manager's log, which the memory snapshot and
/// the diagnostics archive take. It is rewritten at each of the window manager's memory requests
/// (every few minutes and after a give-back), and its first line says when.
fn save_color_buffers(adapter: &GfxstreamAdapter) -> Result<(), String> {
    let Some(list) = adapter.color_buffer_list() else {
        return Ok(());
    };
    let root = crate::paths::UserRoot::current().ok_or("the user's folder is not known")?;
    let path = root.renderer_color_buffers();
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let text = format!(
        "# The graphics renderer's color buffers (pid {}), at {seconds} seconds after 1970 UTC.
{list}",
        std::process::id()
    );
    let temporary = path.with_extension("txt.new");
    std::fs::write(&temporary, text).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, &path).map_err(|e| e.to_string())
}

fn memory(adapter: &GfxstreamAdapter) -> RendererProcessMemory {
    let (working_set_kb, private_kb) = own_memory_kb();
    RendererProcessMemory::of_this_process(working_set_kb, private_kb, adapter.counted_memory())
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

    // Which Vulkan: a native driver, or Dozen from the Compatibility Pack where no GPU has
    // one. What the choice needs of the Vulkan loader goes into this process's environment
    // before gfxstream loads the loader, and nowhere else.
    let vulkan = vulkan_driver_of(&start);
    let found = vulkan_driver::look();
    let plan = vulkan_driver::plan(vulkan, &found, &request_of(&start));
    for (name, value) in &plan.env {
        std::env::set_var(name, value);
    }
    let request = plan.request.clone().unwrap_or_else(|| request_of(&start));
    // `GFXSTREAM_BACKEND_PATH` names another backend library, as the renderer's tests do;
    // otherwise it is the one installed beside this program.
    let backend = std::env::var_os("GFXSTREAM_BACKEND_PATH")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_file());
    let adapter = Arc::new(if plan.outcome == DriverOutcome::None {
        // Nothing to run on: gfxstream would only fail, or settle on a device that cannot
        // take the windows' textures.
        GfxstreamAdapter::without_backend(plan.reason.clone())
    } else {
        GfxstreamAdapter::on_gpu_with(backend, &request, &plan.features)
    });
    let mut started = Started {
        process_id: std::process::id(),
        notes: adapter.notes().to_vec(),
        ..Started::default()
    };
    let gpu = adapter.renderer_gpu();
    report_gpu(&mut started, gpu.as_ref());
    let (outcome, reason) = if adapter.is_available() {
        let outcome = vulkan_driver::outcome_for(
            &plan,
            &found,
            gpu.as_ref().map(|gpu| gpu.name.as_str()),
            gpu.as_ref().and_then(|gpu| gpu.luid),
        );
        let reason = if outcome == plan.outcome {
            plan.reason.clone()
        } else {
            format!(
                "{}; but gfxstream took {}, so it renders there",
                plan.reason,
                gpu.as_ref()
                    .map_or("another device", |gpu| gpu.name.as_str())
            )
        };
        (outcome, reason)
    } else if plan.outcome == DriverOutcome::None {
        (DriverOutcome::None, plan.reason.clone())
    } else {
        (
            DriverOutcome::None,
            format!(
                "{}; but gfxstream could not start on it ({}); {}",
                plan.reason,
                adapter.notes().join("; "),
                vulkan_driver::install_guidance()
            ),
        )
    };
    report_vulkan_driver(&mut started, outcome, &reason);
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
    // What a window manager that takes this renderer over is told it runs on.
    let first_started = started.clone();
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

    // Only the installed renderer, which its window manager names on the command line, waits
    // to be taken over: one a test runs inside its own program ends with the test's window
    // manager, as before.
    let adoptable = args.iter().any(|arg| arg == WINDOW_MANAGER_ARGUMENT);
    loop {
        let left = serve_window_manager(&mut *connection, &link, &adapter, &sink, &vm, &mut served);
        let Some(wait) = adoption_wait(left).filter(|_| adoptable) else {
            break;
        };
        // The window manager crashed or was ended. The guest's streams, and everything its
        // processes made here, stay; the next window manager takes this renderer over.
        link.lost();
        sink.release_all(&*adapter);
        connection.close();
        match wait_to_be_taken_over(wait, &start, &first_started) {
            Some(next) => {
                let Some(sender) = next.try_clone_sender() else {
                    break;
                };
                connection = next;
                link.attach(sender);
                served.adopted();
            }
            None => break,
        }
    }
    link.close();
    end_now(0)
}

/// Waits up to `wait` for the next window manager in this session to take this renderer over,
/// on [`adoption_pipe`]: its handshake, and its `Start`, which must ask for what `first` did.
/// Returns its connection once it was told what the renderer runs on (`first_started`), or
/// `None` when none came, or one asks for something else; the renderer then ends, and that
/// window manager starts a fresh one.
fn wait_to_be_taken_over(
    wait: Duration,
    first: &Start,
    first_started: &Started,
) -> Option<Box<dyn Connection>> {
    let sddl = current_user_only_sddl().ok()?;
    let mut listener = NamedPipeListener::bind(&adoption_pipe(), &sddl).ok()?;
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return None;
        }
        let mut connection = match listener.accept(left.min(Duration::from_millis(500))) {
            Ok((connection, _)) => connection,
            Err(TransportError::Timeout) => continue,
            Err(_) => return None,
        };
        let Ok(asked) = open_renderer(&mut *connection, &build()) else {
            continue;
        };
        if let Some(why) = adoption_refusal(first, &asked) {
            let _ = send(
                &mut *connection,
                Body::Note(Note {
                    text: format!(
                        "the renderer that kept running after the last window manager ended \
                         ends instead of serving this one ({why}); a fresh one starts"
                    ),
                }),
            );
            return None;
        }
        if send(
            &mut *connection,
            Body::Started(adopted_started(first_started)),
        )
        .is_ok()
        {
            return Some(connection);
        }
    }
}

/// Serves one window manager's messages on `connection` until it leaves, and says how.
fn serve_window_manager(
    connection: &mut dyn Connection,
    link: &Arc<WindowManagerLink>,
    adapter: &Arc<GfxstreamAdapter>,
    sink: &Arc<RemoteSink>,
    vm: &VmIdSlot,
    served: &mut ServedVm,
) -> WindowManagerLeft {
    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(_) => return WindowManagerLeft::Lost,
        };
        let Ok(body) = decode_body(&frame) else {
            link.note("the window manager sent something that is not a message; ending");
            return WindowManagerLeft::Stopped;
        };
        let Some(body) = link.take(body) else {
            continue;
        };
        match body {
            Body::GuestVm(guest) => {
                // The VM this renderer served is gone: a fresh renderer, which the window
                // manager starts at once, serves the next one without what this one kept.
                let told = (!guest.vm_id.is_empty()).then_some(guest.vm_id.as_str());
                if told.is_none() && served.awaits_vm() {
                    // A window manager that took this renderer over and has not looked at
                    // the VM yet: the guest's sockets stay bound to the VM served.
                    continue;
                }
                if served.told(told) {
                    let (_, private_kb) = own_memory_kb();
                    link.note(&format!(
                        "the VM this renderer served is gone; it ends so that the window \
                         manager starts a fresh one for the next VM, without the {} MiB of \
                         private memory this one holds",
                        private_kb.map_or("?".to_string(), |kb| (kb / 1024).to_string())
                    ));
                    return WindowManagerLeft::VmGone;
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
                DisplayPresenter::forget_display(&**adapter, forget.display_id);
                sink.forget(forget.display_id);
            }
            Body::MemoryRequest(asked) => {
                if asked.give_back {
                    give_back();
                } else {
                    // Windows are shown, so the working set stays, but what the heap keeps
                    // committed after a freed allocation goes: the window manager asks at its
                    // spare give-backs (a few seconds after a video's decoder closed, and every
                    // five minutes), and a video's uploads left their buffers' worth there.
                    crate::compositor::committed::compact_heaps();
                }
                let report = memory(adapter).to_report(asked.request);
                link.send(Body::MemoryReport(report));
                // Whose the GPU memory is, for the window manager's log beside the report.
                if let Some(owners) = adapter.counted_owners() {
                    link.note(&dsa_host_core::renderer::renderer_owners_line(&owners));
                }
                // Every color buffer, for the memory snapshot and the diagnostics archive.
                if let Err(why) = save_color_buffers(adapter) {
                    link.note(&format!("could not save the color buffer list: {why}"));
                }
            }
            Body::Stop(_) => return WindowManagerLeft::Stopped,
            _ => {}
        }
    }
}
