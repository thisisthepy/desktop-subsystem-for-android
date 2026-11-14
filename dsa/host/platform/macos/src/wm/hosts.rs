//! The window manager's end of the app window hosts' connections.
//!
//! The window hosts dial the window manager's socket (`window-manager.sock` in the user's DSA
//! folder, mode 0600). A thread takes each connection, runs the handshake and the `Attach`,
//! and reads what the window host says from then on: what the user did to its windows goes
//! to the manager, and each frame it showed to the presenter. A malformed message is logged
//! and dropped; it never ends the window manager.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::window_hosts::{
    accept_window_host, answer_attached, report_from_wire, HostId,
};
use dsa_host_unix::socket::{current_uid, UnixLocalListener};
use dsa_protocol::window_host::types::envelope::Body;
use dsa_protocol::window_host::{decode, encode_body};
use dsa_transport::local::LocalListener;
use dsa_transport::{Connection, FrameSender, TransportError};

use super::log;
use super::manager::ManagerEvent;
use super::present::Presenter;

/// The build the window manager names itself by to the window hosts.
pub const BUILD: &str = concat!("dsa-wm ", env!("CARGO_PKG_VERSION"));

/// How long a window host may take over its handshake.
const HANDSHAKE: Duration = Duration::from_secs(5);

/// The window manager's end of one window host's connection.
pub struct HostLink {
    id: HostId,
    /// The window host's process, which the frame surfaces are sent to.
    process_id: u32,
    sender: Mutex<Option<Box<dyn FrameSender>>>,
    closed: AtomicBool,
}

impl HostLink {
    pub fn id(&self) -> HostId {
        self.id
    }

    pub fn process_id(&self) -> u32 {
        self.process_id
    }

    /// Sends `body` to the window host. False when the connection is gone.
    pub fn send(&self, body: Body) -> bool {
        let frame = encode_body(body);
        let mut sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        match sender.as_mut() {
            Some(sender) => sender.send(&frame).is_ok(),
            None => false,
        }
    }

    /// Ends the connection; the window host then exits.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// The connected window hosts, by connection.
pub type Links = Arc<Mutex<HashMap<HostId, Arc<HostLink>>>>;

/// Listens on `socket` for window hosts until `stop` is set. An error when the socket cannot
/// be made.
pub fn listen(
    socket: &Path,
    links: Links,
    events: Sender<ManagerEvent>,
    presenter: Arc<Presenter>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    if let Some(folder) = socket.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("creating {}: {e}", folder.display()))?;
    }
    let mut listener = UnixLocalListener::bind(socket, 0o600)
        .map_err(|e| format!("listening on {}: {e}", socket.display()))?;
    thread::Builder::new()
        .name("dsa-wm-hosts".into())
        .spawn(move || {
            let own = format!("uid:{}", current_uid());
            let mut next = 1u64;
            while !stop.load(Ordering::Relaxed) {
                let (connection, peer) = match listener.accept(Duration::from_millis(200)) {
                    Ok(accepted) => accepted,
                    Err(TransportError::Timeout) => continue,
                    Err(error) => {
                        log(&format!(
                            "could not take an app window's connection: {error}"
                        ));
                        thread::sleep(Duration::from_millis(200));
                        continue;
                    }
                };
                // The socket is the user's alone; anyone else is refused all the same.
                if peer.user != own {
                    log(&format!(
                        "refused a window host of another user ({})",
                        peer.user
                    ));
                    continue;
                }
                let id = HostId(next);
                next += 1;
                let (links, events, presenter, stop) = (
                    Arc::clone(&links),
                    events.clone(),
                    Arc::clone(&presenter),
                    Arc::clone(&stop),
                );
                let _ = thread::Builder::new()
                    .name("dsa-wm-host".into())
                    .spawn(move || serve(connection, id, links, events, presenter, stop));
            }
        })
        .map_err(|e| format!("the window hosts cannot be served: {e}"))?;
    Ok(())
}

fn serve(
    mut connection: Box<dyn Connection>,
    id: HostId,
    links: Links,
    events: Sender<ManagerEvent>,
    presenter: Arc<Presenter>,
    stop: Arc<AtomicBool>,
) {
    let attach = match accept_window_host(&mut *connection, BUILD, HANDSHAKE) {
        Ok(attach) => attach,
        Err(why) => {
            log(&format!("refused an app window's connection: {why}"));
            return;
        }
    };
    // There is no GPU to name: a Mac's window hosts show the frames on the system's GPU.
    if let Err(why) = answer_attached(&mut *connection, None) {
        log(&why);
        return;
    }
    let Some(sender) = connection.try_clone_sender() else {
        log("an app window's connection cannot be shared");
        return;
    };
    let link = Arc::new(HostLink {
        id,
        process_id: attach.process_id,
        sender: Mutex::new(Some(sender)),
        closed: AtomicBool::new(false),
    });
    links
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, Arc::clone(&link));
    let attached = ManagerEvent::Attached {
        host: id,
        package: attach.package_name,
        launch: attach.launch,
        shortcut: attach.shortcut_id,
    };
    if events.send(attached).is_ok() {
        read(&mut *connection, &link, &events, &presenter, &stop);
    }
    link.close();
    connection.close();
    links.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    presenter.host_gone(id, link.process_id());
    let _ = events.send(ManagerEvent::Gone { host: id });
}

/// Reads the window host's messages until it goes or the window manager stops.
fn read(
    connection: &mut dyn Connection,
    link: &HostLink,
    events: &Sender<ManagerEvent>,
    presenter: &Presenter,
    stop: &AtomicBool,
) {
    let id = link.id();
    let mut malformed_logged = false;
    while !stop.load(Ordering::Relaxed) && !link.is_closed() {
        let frame = match connection.recv(Some(Duration::from_millis(200))) {
            Ok(frame) => frame,
            Err(TransportError::Timeout) => continue,
            Err(_) => return,
        };
        let Ok(envelope) = decode(&frame) else {
            if !malformed_logged {
                log("dropped a malformed message from an app window");
                malformed_logged = true;
            }
            continue;
        };
        let Some(body) = envelope.body else {
            continue;
        };
        if let Body::FramePresented(presented) = &body {
            presenter.presented(id, presented.task_id, presented.sequence, Instant::now());
        } else if let Some(report) = report_from_wire(&body) {
            if events
                .send(ManagerEvent::Report { host: id, report })
                .is_err()
            {
                return;
            }
        }
    }
}
