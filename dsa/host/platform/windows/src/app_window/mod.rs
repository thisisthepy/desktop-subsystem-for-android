//! The app window host: the program that owns an Android app's task windows.
//!
//! Each app runs its own copy of this program, named and drawn as the app (see
//! [`crate::app_exe`]), so Task Manager and the taskbar show the app as itself. It connects to
//! the window manager in the same session, says which app it shows, opens the windows the
//! window manager asks for ([`window`]), and shows the frames the window manager hands it:
//! for each task, a Direct3D 11 texture shared by an NT handle that the window manager has
//! placed in this process. Each frame is copied from that texture into the window's swapchain
//! on the GPU, and the window manager is told once the texture may be written again. It ends
//! when the window manager closes the connection, which it does when the app's last window
//! closes.

pub mod pointer;
pub mod window;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dsa_host_core::compositor::window_hosts::{
    connect_window_host, report_to_wire, to_host_from_wire, FromHost,
};
use dsa_protocol::window_host::types::{envelope::Body, Attach, FramePresented};
use dsa_protocol::window_host::{decode, encode_body};
use dsa_transport::{Connector, FrameSender, TransportError};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};

use self::window::{OpenWindow, Presenters, UiSetup};
use crate::compositor::log::{note, CompositorLogger};
use crate::compositor::present::{Gpu, OpenedTexture};
use crate::compositor::tasks::serve_input;

/// The build the window host names itself by to the window manager.
const BUILD: &str = "dsa-app-window";

/// The argument the window manager starts a window host with. Without it, the user started
/// it (from the Start menu, say), and the app is started in Android if it has no task yet.
pub const STARTED_BY_WINDOW_MANAGER: &str = "--started-by-window-manager";

/// The argument that names the app, for the installed window host, whose file name is not
/// a package's.
pub const PACKAGE_ARGUMENT: &str = "--package";

/// The argument that names the pipe to dial instead of this session's window manager's, for
/// tests.
pub const PIPE_ARGUMENT: &str = "--pipe";

/// The value that follows `flag` in `args`, if any.
pub fn argument(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|at| args.get(at + 1))
        .cloned()
}

/// Which app a window host shows: the package its file is named after, or the one
/// `--package` names. `None` when neither names a valid package.
pub fn package_of(program: &std::path::Path, args: &[String]) -> Option<String> {
    use dsa_host_core::compositor::window_hosts::is_valid_package;
    let named = argument(args, PACKAGE_ARGUMENT);
    let from_file = program
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string);
    named
        .or(from_file)
        .filter(|package| is_valid_package(package))
}

/// How long connecting and the handshake may take.
const CONNECT: Duration = Duration::from_secs(5);

/// What a window host is given to run.
pub struct HostSetup {
    /// The Android package of the app.
    pub package: String,
    /// Whether the user started it, rather than the window manager.
    pub launch: bool,
    /// Dials the window manager.
    pub connector: Box<dyn Connector>,
    /// Dials the service's local API, which the windows' pointer input goes to directly, so
    /// it never waits on the window manager.
    pub input: Box<dyn Connector>,
    /// The process the window manager places the frames' textures in: this one.
    pub process_id: u32,
    pub logger: Option<Arc<CompositorLogger>>,
    /// Where the windows that are open are listed, for tests.
    pub listing: Arc<Mutex<Vec<OpenWindow>>>,
}

/// Each task's texture, as the window manager last announced it.
type Textures = HashMap<i32, (u64, OpenedTexture)>;

/// Runs the window host until the window manager closes the connection. An error says why
/// it could not start.
pub fn run(mut setup: HostSetup) -> Result<(), String> {
    let log = setup.logger.clone();
    let mut connection = setup
        .connector
        .connect(CONNECT)
        .map_err(|e| format!("could not reach the DSA Window Manager: {e}"))?;
    let luid = connect_window_host(
        &mut *connection,
        BUILD,
        Attach {
            package_name: setup.package.clone(),
            process_id: setup.process_id,
            launch: setup.launch,
        },
        CONNECT,
    )?;
    let sender: Arc<Mutex<Box<dyn FrameSender>>> =
        Arc::new(Mutex::new(connection.try_clone_sender().ok_or(
            "the connection to the window manager cannot be shared",
        )?));
    let gpu = match Gpu::new(luid, true) {
        Ok(gpu) => {
            if luid.is_some() && !gpu.on_renderer_gpu {
                note(
                    log.as_deref(),
                    "the window manager's GPU was not found; the windows may stay empty",
                );
            }
            Some(gpu)
        }
        Err(why) => {
            note(
                log.as_deref(),
                &format!("the windows cannot show frames: {why}"),
            );
            None
        }
    };
    let report_sender = Arc::clone(&sender);
    let report: window::Report = Arc::new(move |report: FromHost| {
        let frame = encode_body(report_to_wire(&report));
        let _ = report_sender
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(&frame);
    });
    let (input_tx, input_rx) = std::sync::mpsc::channel();
    let input_connector = std::mem::replace(&mut setup.input, Box::new(NoConnector));
    let input_logger = log.clone();
    let input = std::thread::Builder::new()
        .name("dsa-app-window-input".into())
        .spawn(move || serve_input(input_connector, input_rx, input_logger))
        .map_err(|e| format!("could not start the pointer input: {e}"))?;
    let presenters: Presenters = Arc::default();
    let ui = window::start(UiSetup {
        report,
        input: input_tx,
        gpu: gpu.clone(),
        presenters: Arc::clone(&presenters),
        logger: log.clone(),
        listing: Arc::clone(&setup.listing),
    })?;

    let mut textures = Textures::new();
    loop {
        let frame = match connection.recv(None) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(TransportError::Closed) => break,
            Err(error) => {
                note(
                    log.as_deref(),
                    &format!("the connection to the window manager failed: {error}"),
                );
                break;
            }
        };
        let body = match decode(&frame) {
            Ok(envelope) => envelope.body,
            Err(error) => {
                note(
                    log.as_deref(),
                    &format!("dropped a message from the window manager: {error}"),
                );
                continue;
            }
        };
        let Some(body) = body else {
            continue;
        };
        if let Some(command) = to_host_from_wire(&body) {
            if let dsa_host_core::compositor::window_hosts::ToHost::Close { task_id } = &command {
                textures.remove(task_id);
            }
            if !ui.send(command) {
                break;
            }
            continue;
        }
        match body {
            Body::FrameTexture(texture) => {
                let handle = texture.handle as isize;
                let opened = match &gpu {
                    Some(gpu) => gpu
                        .open_shared(handle, (texture.width, texture.height))
                        .map_err(|why| note(log.as_deref(), &why))
                        .ok(),
                    None => None,
                };
                // The window manager placed the handle in this process for this alone; the
                // opened texture keeps what it needs.
                // SAFETY: a handle the window manager duplicated into this process.
                unsafe { CloseHandle(handle as HANDLE) };
                match opened {
                    Some(opened) => {
                        textures.insert(texture.task_id, (texture.texture_id, opened));
                    }
                    None => {
                        textures.remove(&texture.task_id);
                    }
                }
            }
            Body::Frame(frame) => {
                let presenter = presenters
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&frame.task_id)
                    .cloned();
                if let (Some(presenter), Some((id, texture))) =
                    (presenter, textures.get(&frame.task_id))
                {
                    if *id == frame.texture_id {
                        if let Err(why) = presenter.present(texture) {
                            note(log.as_deref(), &why);
                        }
                    }
                }
                // Answered even when nothing was shown, so the renderer never waits on it.
                let presented = encode_body(Body::FramePresented(FramePresented {
                    task_id: frame.task_id,
                    sequence: frame.sequence,
                }));
                let sent = sender
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send(&presented);
                if sent.is_err() {
                    break;
                }
            }
            _ => {}
        }
    }
    // The windows close first; the pointer input ends with them.
    ui.stop();
    let _ = input.join();
    Ok(())
}

/// Stands in for the input connector once it has been handed to its thread.
struct NoConnector;

impl Connector for NoConnector {
    fn connect(
        &mut self,
        _: Duration,
    ) -> Result<Box<dyn dsa_transport::Connection>, TransportError> {
        Err(TransportError::Closed)
    }
}
