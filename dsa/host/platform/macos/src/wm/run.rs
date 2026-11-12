//! Running the window manager: the renderer, the graphics streams, the window hosts, the
//! service's task watch, and the manager loop that ties them together.
//!
//! Each connection to the service runs on a thread of its own and reconnects when the
//! service restarts. A broken stream, a dead guest, a window host that misbehaves or a
//! missing renderer is reported and waited out; none of it ends the window manager.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::app_windows::AppWindowSettings;
use dsa_host_core::compositor::frame_stall::{restart_in_background, Stall};
use dsa_host_core::compositor::window_hosts::{to_wire, HostId, ToHost};
use dsa_host_core::compositor::GraphicsStreamKind;
use dsa_host_core::guest_display::display_or_smallest;
use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::notifications::Notification;
use dsa_host_core::tasks::{AppInfo, PinShortcutRequest, TaskRequest};
use dsa_host_unix::socket::UnixLocalConnector;

use super::hosts::{self, Links, BUILD};
use super::launch::{installed_window_host, AppLauncher};
use super::log;
use super::manager::{Manager, ManagerEvent, Platform, Route};
use super::present::Presenter;
use crate::compositor::surface_port::SurfacePorts;
use crate::compositor::window_host::{support_folder, window_manager_socket};
use crate::gfx::client::RendererClient;
use crate::host_config::{user_root_in, MacHostConfig, UserFolders};
use crate::launchd::WM_LABEL;
use crate::service::relay_socket;
use crate::shell::app_stubs;
use dsa_host_core::renderer::{spawn_supervisor, RendererHandler};
use dsa_host_unix::renderer_launch::{CurrentFds, UnixLauncher};

/// How long to wait for the service before trying again.
const RETRY: Duration = Duration::from_secs(2);
/// How long one call to the service may take.
const TIMEOUT: Duration = Duration::from_secs(5);
/// How often the manager looks at its timers.
const TICK: Duration = Duration::from_millis(500);
/// How often the presenter looks for window hosts that are late.
const PRESENT_TICK: Duration = Duration::from_millis(100);
/// How often the user's window settings for each app are read again.
const APP_WINDOWS_EVERY: Duration = Duration::from_secs(30);

/// Where the window manager finds what it serves.
pub struct Places {
    /// The window hosts' socket.
    pub window_manager: PathBuf,
    /// The service's local API.
    pub host_api: PathBuf,
    /// The user's DSA folder, which holds the relay sockets.
    pub user: UserFolders,
    /// `~/Applications/DSA Apps`.
    pub apps: PathBuf,
    /// The renderer library.
    pub renderer: PathBuf,
}

impl Places {
    /// The places for the user whose home is `home`, as the agent's configuration names them.
    pub fn for_home(home: &Path) -> Places {
        let config = MacHostConfig::find(home).and_then(|path| match MacHostConfig::read(&path) {
            Ok(config) => Some(config),
            Err(why) => {
                log(&format!("{why}; using the default folders"));
                None
            }
        });
        let user = match &config {
            Some(config) => config.user(home),
            None => UserFolders {
                root: user_root_in(home),
            },
        };
        let host_api = match &config {
            Some(config) => config.socket(&user),
            None => user.socket(),
        };
        let support = support_folder().unwrap_or_else(|| user_root_in(home));
        Places {
            window_manager: window_manager_socket(&support),
            host_api,
            user,
            apps: app_stubs::default_folder(home),
            renderer: renderer_path(),
        }
    }
}

/// The renderer library: `DSA_RENDERER` when set, else `libgfxstream_backend.dylib` in the
/// `Frameworks` folder of the bundle this program is in.
pub fn renderer_path() -> PathBuf {
    if let Some(path) = std::env::var_os("DSA_RENDERER") {
        return PathBuf::from(path);
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let macos = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    macos.join("../Frameworks/libgfxstream_backend.dylib")
}

/// The Vulkan driver description (MoltenVK's) DSA.app carries beside the renderer, for the
/// Vulkan loader to find, when this program is in the bundle.
pub fn bundled_vulkan_driver() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let contents = exe.parent()?.parent()?;
    let driver = contents.join("Resources/vulkan/icd.d/MoltenVK_icd.json");
    driver.is_file().then_some(driver)
}

fn connect(host_api: &Path) -> Result<HostApiClient, String> {
    let mut connector = UnixLocalConnector {
        path: host_api.to_path_buf(),
    };
    HostApiClient::connect(&mut connector, BUILD, TIMEOUT).map_err(|e| e.to_string())
}

/// What the manager does outside itself, on a Mac.
struct MacPlatform {
    links: Links,
    presenter: Arc<Presenter>,
    requests: Sender<TaskRequest>,
    launcher: Arc<AppLauncher>,
    /// Held while the app bundles are being brought up to date.
    syncing: Arc<Mutex<()>>,
    /// Where the apps' requests to pin a shortcut go to be asked about.
    pins: Sender<PinShortcutRequest>,
    /// The service's local API, which restarts Android when its graphics did not recover.
    host_api: PathBuf,
    /// DSA's own news for the Notification Center.
    news: Sender<Notification>,
}

impl MacPlatform {
    fn link(&self, host: HostId) -> Option<Arc<hosts::HostLink>> {
        let links = self.links.lock().unwrap_or_else(|e| e.into_inner());
        links.get(&host).cloned()
    }
}

impl Platform for MacPlatform {
    fn start_host(&mut self, package: &str, app: Option<&AppInfo>) -> Result<(), String> {
        self.launcher.start(package, app)
    }

    fn send(&mut self, host: HostId, message: &ToHost) {
        if let Some(link) = self.link(host) {
            link.send(to_wire(message));
        }
    }

    fn disconnect(&mut self, host: HostId) {
        if let Some(link) = self.link(host) {
            link.close();
        }
    }

    fn request(&mut self, request: TaskRequest) {
        if self.requests.send(request).is_err() {
            log("the request thread has ended; a window's change does not reach Android");
        }
    }

    fn routes(&mut self, routes: &[Route]) {
        self.presenter.set_routes(routes);
    }

    fn installed(&mut self, apps: &[AppInfo]) {
        // Making the bundles converts every icon, which is not done on the manager's thread.
        let (launcher, syncing, apps) = (
            Arc::clone(&self.launcher),
            Arc::clone(&self.syncing),
            apps.to_vec(),
        );
        let _ = thread::Builder::new()
            .name("dsa-wm-app-bundles".into())
            .spawn(move || {
                let _held = syncing.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(why) = launcher.sync(&apps) {
                    log(&why);
                }
            });
    }

    fn pin_requested(&mut self, request: &PinShortcutRequest) {
        if self.pins.send(request.clone()).is_err() {
            // Nobody asks the user: the app hears no at once.
            self.request(TaskRequest::AnswerPinShortcut {
                request_id: request.request_id,
                accepted: false,
            });
        }
    }

    fn open_app_page(&mut self, package: &str) -> Result<(), String> {
        // Through the settings app's URL scheme, which reaches it whether it runs already
        // (in the menu bar) or has to start; arguments would reach only a new copy.
        let url = super::manager::app_page_url(package)
            .ok_or_else(|| format!("{package} is not a package name"))?;
        let mut child = std::process::Command::new("/usr/bin/open")
            .arg(&url)
            .spawn()
            .map_err(|e| format!("could not open {url}: {e}"))?;
        // Waited for apart from the manager, so it leaves no zombie and holds nothing up.
        let _ = thread::Builder::new()
            .name("dsa-wm-app-page".into())
            .spawn(move || {
                let _ = child.wait();
            });
        Ok(())
    }

    fn frames_shown(&mut self) -> u64 {
        self.presenter.frames_shown()
    }

    fn recover(&mut self, stall: &Stall, packages: Vec<String>) {
        let notice = stall
            .notice()
            .notification(crate::compositor::menu_bar::korean());
        if self.news.send(notice).is_err() {
            log("the notifications have ended; the user is not told that Android restarts");
        }
        if !stall.restart {
            return;
        }
        let connector = UnixLocalConnector {
            path: self.host_api.clone(),
        };
        let told: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|line: &str| log(line));
        if let Err(why) = restart_in_background(
            Box::new(connector),
            format!("{BUILD} restart after a GPU reset"),
            packages,
            told,
        ) {
            log(&format!("could not start restarting Android: {why}"));
        }
    }

    fn log(&mut self, line: &str) {
        log(line);
    }
}

/// Follows the guest's tasks, reconnecting whenever the watch ends.
fn watch_tasks(host_api: PathBuf, events: Sender<ManagerEvent>) {
    let _ = thread::Builder::new()
        .name("dsa-wm-tasks".into())
        .spawn(move || {
            let mut warned = false;
            loop {
                let watch = connect(&host_api)
                    .and_then(|client| client.watch_tasks().map_err(|e| e.to_string()));
                let mut stream = match watch {
                    Ok(stream) => stream,
                    Err(why) => {
                        if !warned {
                            log(&format!("waiting for the DSA service: {why}"));
                            warned = true;
                        }
                        thread::sleep(RETRY);
                        continue;
                    }
                };
                warned = false;
                if events
                    .send(ManagerEvent::Sync(stream.current().events()))
                    .is_err()
                {
                    return;
                }
                loop {
                    match stream.next(Duration::from_secs(1)) {
                        Ok(Some(event)) => {
                            if events.send(ManagerEvent::Task(Box::new(event))).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(why) => {
                            log(&format!("the task watch ended: {why}"));
                            break;
                        }
                    }
                }
                if events.send(ManagerEvent::Lost).is_err() {
                    return;
                }
                thread::sleep(RETRY);
            }
        });
}

/// The user's window settings for each app, as the service has them.
fn app_windows(host_api: &Path) -> Result<Vec<AppWindowSettings>, String> {
    let mut client = connect(host_api)?;
    client.app_window_settings().map_err(|e| e.to_string())
}

/// Reads the user's window settings for each app now and then.
fn watch_app_windows(host_api: PathBuf, events: Sender<ManagerEvent>, stop: Arc<AtomicBool>) {
    let _ = thread::Builder::new()
        .name("dsa-wm-app-windows".into())
        .spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                if let Ok(apps) = app_windows(&host_api) {
                    if events.send(ManagerEvent::AppWindows(apps)).is_err() {
                        return;
                    }
                }
                thread::sleep(APP_WINDOWS_EVERY);
            }
        });
}

/// Sends the manager's requests to the guest. A request to end a task that does not get
/// through opens the task's window again.
fn serve_requests(
    host_api: PathBuf,
    requests: Receiver<TaskRequest>,
    events: Sender<ManagerEvent>,
) {
    let _ = thread::Builder::new()
        .name("dsa-wm-requests".into())
        .spawn(move || {
            let mut client: Option<HostApiClient> = None;
            for request in requests {
                if client.is_none() {
                    client = connect(&host_api)
                        .map_err(|why| log(&format!("cannot reach the DSA service: {why}")))
                        .ok();
                }
                let result = match client.as_mut() {
                    Some(open) => open.request(&request).map_err(|e| e.to_string()),
                    None => Err("the DSA service is not reachable".to_string()),
                };
                if let Err(why) = result {
                    log(&format!(
                        "{} did not reach Android: {why}",
                        request.describe()
                    ));
                    client = None;
                    if let TaskRequest::Close { task_id } = request {
                        let _ = events.send(ManagerEvent::CloseFailed(task_id));
                    }
                }
            }
        });
}

/// Runs the window manager until `stop` is set.
pub fn run(places: Places, stop: Arc<AtomicBool>) -> Result<(), String> {
    // One window manager per user: a second one leaves the first in charge.
    if std::os::unix::net::UnixStream::connect(&places.window_manager).is_ok() {
        log("another window manager is running for this user; this one ends");
        return Ok(());
    }
    log(&format!("dsa-wm starting (version {})", crate::VERSION));
    // The renderer's Vulkan loader finds MoltenVK through the bundle's driver description,
    // unless the environment names another. Set before any other thread runs.
    if std::env::var_os("VK_ICD_FILENAMES").is_none() {
        if let Some(driver) = bundled_vulkan_driver() {
            std::env::set_var("VK_ICD_FILENAMES", driver);
        }
    }

    let ports = match SurfacePorts::check_in(WM_LABEL) {
        Ok(ports) => Some(ports),
        Err(why) => {
            log(&why);
            None
        }
    };
    let links: Links = Arc::default();
    let presenter = Arc::new(Presenter::new(Arc::clone(&links), ports));
    // The renderer runs in a process of its own, dsa-gfx, which this process starts and
    // starts again whenever it ends: a crash in the renderer or MoltenVK then ends only that
    // process, and the app windows keep their last frame. It serves the guest's streams on the
    // relay sockets.
    let sockets = (
        relay_socket(&places.user, GraphicsStreamKind::Command.port()),
        relay_socket(&places.user, GraphicsStreamKind::ProcessPipe.port()),
    );
    let client = RendererClient::new(Arc::clone(&presenter), sockets, display_or_smallest(None));
    let fds: CurrentFds = Arc::default();
    let mut launcher = UnixLauncher::installed(fds);
    if std::env::var_os("DSA_RENDERER").is_none() {
        launcher
            .env
            .push(("DSA_RENDERER".into(), places.renderer.display().to_string()));
    }
    if let Err(why) = spawn_supervisor(
        Box::new(launcher),
        Arc::clone(&client) as Arc<dyn RendererHandler>,
        BUILD.to_string(),
        Arc::clone(&stop),
    ) {
        log(&format!(
            "graphics: the renderer cannot be started ({why}); task windows open but show no \
             frames"
        ));
    }

    let (events, received) = channel();
    hosts::listen(
        &places.window_manager,
        Arc::clone(&links),
        events.clone(),
        Arc::clone(&presenter),
        Arc::clone(&stop),
    )?;
    let (requests, to_send) = channel();
    serve_requests(places.host_api.clone(), to_send, events.clone());
    watch_tasks(places.host_api.clone(), events.clone());
    watch_app_windows(places.host_api.clone(), events.clone(), Arc::clone(&stop));
    drop(events);
    let launcher = Arc::new(AppLauncher {
        folder: places.apps.clone(),
        window_host: std::env::current_exe()
            .ok()
            .and_then(|exe| installed_window_host(&exe)),
        desktop: std::env::var_os("HOME")
            .map(|home| crate::shell::app_stubs::default_desktop(Path::new(&home))),
    });
    let (pins, pin_requests) = channel();
    let (news, news_posted) = channel();
    super::notifications::show(
        places.host_api.clone(),
        places.user.root.join("notifications"),
        Arc::clone(&stop),
        pin_requests,
        news_posted,
        Arc::clone(&launcher),
    );
    let platform = MacPlatform {
        links: Arc::clone(&links),
        presenter: Arc::clone(&presenter),
        requests,
        launcher,
        syncing: Arc::default(),
        pins,
        host_api: places.host_api.clone(),
        news,
    };
    let mut manager = Manager::new(platform);
    let mut last_tick = Instant::now();
    // The renderer's word on its device: any change is a GPU reset, followed at once.
    let mut device_changes = client.device_changes();
    while !stop.load(Ordering::Relaxed) {
        let changes = client.device_changes();
        if changes != device_changes {
            device_changes = changes;
            manager.handle(ManagerEvent::GpuReset, Instant::now());
        }
        match received.recv_timeout(PRESENT_TICK) {
            Ok(event) => {
                // A failure in the rules must not end the window manager; the event is lost.
                let handled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    manager.handle(event, Instant::now())
                }));
                if handled.is_err() {
                    log("an event from Android or a window could not be handled; it is skipped");
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = Instant::now();
        presenter.tick(now);
        if now.saturating_duration_since(last_tick) >= TICK {
            last_tick = now;
            manager.tick(now);
        }
    }
    manager.finish();
    for link in links.lock().unwrap_or_else(|e| e.into_inner()).values() {
        link.close();
    }
    log("dsa-wm stopped");
    Ok(())
}
