//! The task windows, as native windows on this machine: one per guest task, each in its app's
//! own window host, opened and closed with its task, and a window close that ends its task.
//!
//! The service runs on the in-memory local API with a fake Android whose tasks the tests
//! change, as the guest's task messages would. The window hosts run in this process, on
//! threads, and reach the window manager over the in-memory transport; everything between
//! them is what the real programs say over their pipe, down to the frames' textures, whose
//! handles the window manager places in the window host's process (here, this one).
#![cfg(windows)]

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::FakeRenderer;
use dsa_host_core::host_api::UserAndroid;
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest};
use dsa_host_windows::app_window::window::OpenWindow;
use dsa_host_windows::app_window::{self, HostSetup};
use dsa_host_windows::compositor::adapter::{DisplayPresenter, DisplaySink};
use dsa_host_windows::compositor::hosts::HostLauncher;
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, Listener, TransportError};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextW, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    PostMessageW, SetWindowPos, SWP_NOMOVE, SWP_NOZORDER, WM_CLOSE,
};

const WAIT: Duration = Duration::from_secs(10);
const USER: &str = "test-user";

/// Connects to the in-memory local API as [`USER`].
struct AsUser(MemoryLocalConnector);

impl Connector for AsUser {
    fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.connect_as(Peer { user: USER.into() })
    }
}

/// A presenter that keeps the sink the window manager gives it, so a test can post frames.
#[derive(Default)]
struct CapturingPresenter {
    sink: Mutex<Option<Arc<dyn DisplaySink>>>,
}

impl DisplayPresenter for CapturingPresenter {
    fn present_displays(&self, sink: Option<Arc<dyn DisplaySink>>) -> Result<(), String> {
        *self.sink.lock().unwrap() = sink;
        Ok(())
    }
    fn adapter_luid(&self) -> Option<[u8; 8]> {
        None
    }
}

/// The window hosts this test started, by package: each one's open windows.
type Hosts = Arc<Mutex<BTreeMap<String, Arc<Mutex<Vec<OpenWindow>>>>>>;

/// Each window host this test was asked to start: its package and its app's details.
type Started = Arc<Mutex<Vec<(String, Option<AppInfo>)>>>;

/// Starts each app's window host on a thread of this process.
struct InProcessHosts {
    connector: MemoryConnector,
    hosts: Hosts,
    started: Started,
}

impl HostLauncher for InProcessHosts {
    fn start(&mut self, package: &str, app: Option<&AppInfo>) -> Result<(), String> {
        self.started
            .lock()
            .unwrap()
            .push((package.to_string(), app.cloned()));
        let listing: Arc<Mutex<Vec<OpenWindow>>> = Arc::default();
        self.hosts
            .lock()
            .unwrap()
            .insert(package.to_string(), Arc::clone(&listing));
        let setup = HostSetup {
            package: package.to_string(),
            launch: false,
            connector: Box::new(self.connector.clone()),
            process_id: std::process::id(),
            logger: None,
            listing,
        };
        thread::spawn(move || {
            if let Err(why) = app_window::run(setup) {
                eprintln!("window host: {why}");
            }
        });
        Ok(())
    }
}

struct Running {
    android: Arc<FakeAndroid>,
    hosts: Hosts,
    started: Started,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    service: Option<RunningService>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Running {
    /// Every open window of every window host, in task ID order.
    fn windows(&self) -> Vec<OpenWindow> {
        let mut all: Vec<OpenWindow> = self
            .hosts
            .lock()
            .unwrap()
            .values()
            .flat_map(|listing| listing.lock().unwrap().clone())
            .collect();
        all.sort_by_key(|window| window.task_id);
        all
    }
}

fn start(presenter: Option<Arc<CapturingPresenter>>) -> Running {
    let (api_listener, api_connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let service = RunningService::start(Box::new(api_listener), androids.clone());
    let config = CompositorConfig {
        status_poll_interval: Duration::from_millis(50),
        ..Default::default()
    };
    let unbound = || -> Result<Box<dyn Listener>, TransportError> {
        Err(TransportError::Unsupported("no VM in this test".into()))
    };
    let (host_listener, host_connector) = memory::pair();
    let hosts: Hosts = Arc::default();
    let started = Arc::default();
    let launcher = InProcessHosts {
        connector: host_connector,
        hosts: Arc::clone(&hosts),
        started: Arc::clone(&started),
    };
    let mut compositor = Compositor::with_custom_transport(
        config,
        Arc::new(FakeRenderer::new()),
        Box::new(move || Box::new(AsUser(api_connector.clone())) as Box<dyn Connector>),
        Box::new(move |_| unbound()),
        Box::new(move |_| unbound()),
    )
    .with_window_hosts(
        Box::new(move || Ok(Box::new(host_listener) as Box<dyn Listener>)),
        Box::new(launcher),
    );
    if let Some(presenter) = presenter {
        compositor = compositor.with_display_presenter(presenter);
    }
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::spawn(move || compositor.run(flag));
    let android = wait_for("the window manager to watch the tasks", || {
        androids.of(USER)
    });
    Running {
        android,
        hosts,
        started,
        stop,
        thread: Some(thread),
        service: Some(service),
    }
}

fn wait_for<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn task(task_id: i32, label: &str, width: i32, height: i32) -> Task {
    Task {
        task_id,
        bounds: Bounds {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        },
        package_name: format!("com.example.app{task_id}"),
        label: label.into(),
        focused: false,
    }
}

fn windows_of(running: &Running, count: usize) -> Vec<OpenWindow> {
    wait_for(&format!("{count} task windows"), || {
        let open = running.windows();
        (open.len() == count).then_some(open)
    })
}

fn title(hwnd: isize) -> String {
    let mut buffer = [0u16; 256];
    let length = unsafe { GetWindowTextW(hwnd as HWND, buffer.as_mut_ptr(), buffer.len() as i32) };
    String::from_utf16_lossy(&buffer[..length.max(0) as usize])
}

fn client_size(hwnd: isize) -> (i32, i32) {
    let mut rect = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetClientRect(hwnd as HWND, &mut rect) };
    (rect.right - rect.left, rect.bottom - rect.top)
}

fn window_thread(hwnd: isize) -> u32 {
    unsafe { GetWindowThreadProcessId(hwnd as HWND, std::ptr::null_mut()) }
}

#[test]
fn fr3_two_apps_open_two_native_windows_each_in_its_own_window_host() {
    let running = start(None);
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Clock", 400, 300)));
    running
        .android
        .publish(TaskEvent::Updated(task(2, "Maps", 500, 350)));

    let open = windows_of(&running, 2);
    assert_eq!(open[0].task_id, 1);
    assert_eq!(open[1].task_id, 2);
    assert_ne!(open[0].hwnd, open[1].hwnd);
    // Each app's window belongs to its own window host.
    let hosts = running.hosts.lock().unwrap().clone();
    assert_eq!(
        hosts.keys().cloned().collect::<Vec<_>>(),
        vec![
            "com.example.app1".to_string(),
            "com.example.app2".to_string()
        ]
    );
    assert_ne!(window_thread(open[0].hwnd), window_thread(open[1].hwnd));
    for (window, (label, size)) in open
        .iter()
        .zip([("Clock", (400, 300)), ("Maps", (500, 350))])
    {
        assert_eq!(title(window.hwnd), label);
        assert_ne!(unsafe { IsWindowVisible(window.hwnd as HWND) }, 0);
        assert_eq!(client_size(window.hwnd), size);
    }

    // A new label retitles the window in place.
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Alarm", 400, 300)));
    wait_for("the window to be retitled", || {
        (title(open[0].hwnd) == "Alarm").then_some(())
    });
}

#[test]
fn fr3_an_apps_window_host_is_made_with_the_apps_name_and_icon() {
    let running = start(None);
    let clock = AppInfo {
        package_name: "com.example.app1".into(),
        label: "Clock".into(),
        icon_png: vec![0x89, b'P', b'N', b'G'],
    };
    running.android.publish(TaskEvent::App(clock.clone()));
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Alarm", 400, 300)));
    windows_of(&running, 1);
    assert_eq!(
        *running.started.lock().unwrap(),
        vec![("com.example.app1".to_string(), Some(clock))]
    );
}

#[test]
fn fr3_two_tasks_of_one_app_share_its_window_host() {
    let running = start(None);
    let mut second = task(2, "Inbox 2", 400, 300);
    second.package_name = "com.example.app1".into();
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Inbox", 400, 300)));
    running.android.publish(TaskEvent::Updated(second));
    let open = windows_of(&running, 2);
    assert_eq!(running.started.lock().unwrap().len(), 1);
    assert_eq!(window_thread(open[0].hwnd), window_thread(open[1].hwnd));
}

#[test]
fn fr3_a_task_that_ends_in_the_guest_closes_its_window() {
    let running = start(None);
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Clock", 400, 300)));
    running
        .android
        .publish(TaskEvent::Updated(task(2, "Maps", 400, 300)));
    let open = windows_of(&running, 2);

    running.android.publish(TaskEvent::Removed { task_id: 1 });
    let left = windows_of(&running, 1);
    assert_eq!(left[0].task_id, 2);
    wait_for("the window to be destroyed", || {
        (unsafe { IsWindow(open[0].hwnd as HWND) } == 0).then_some(())
    });
}

#[test]
fn fr3_closing_a_task_window_asks_the_guest_to_end_the_task() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Clock", 400, 300)));
    let open = windows_of(&running, 1);

    unsafe { PostMessageW(open[0].hwnd as HWND, WM_CLOSE, 0, 0) };
    wait_for("the close to reach the guest", || {
        let requests = running.android.requests.lock().unwrap().clone();
        (requests == vec![TaskRequest::Close { task_id: 7 }]).then_some(())
    });
    windows_of(&running, 0);
    // The guest ends the task; nothing opens again.
    running.android.publish(TaskEvent::Removed { task_id: 7 });
    thread::sleep(Duration::from_millis(200));
    assert!(running.windows().is_empty());
}

#[test]
fn fr3_a_close_the_guest_cannot_take_opens_the_window_again() {
    // The fake Android is not running, so it refuses the request to end the task.
    let running = start(None);
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Clock", 400, 300)));
    let open = windows_of(&running, 1);
    unsafe { PostMessageW(open[0].hwnd as HWND, WM_CLOSE, 0, 0) };
    let reopened = wait_for("the window to open again", || {
        let now = running.windows();
        (now.len() == 1 && now[0].hwnd != open[0].hwnd).then_some(now)
    });
    assert_eq!(title(reopened[0].hwnd), "Clock");
}

#[test]
fn fr3_only_a_tasks_own_display_reaches_its_window_and_the_whole_screen_never_does() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    let Some(sink) = presenter.sink.lock().unwrap().clone() else {
        // The window manager could not create its Direct3D device here, so it showed no
        // frames and registered no sink. The window tests above still ran.
        eprintln!("skipping: no Direct3D 11 device on this machine");
        return;
    };
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Clock", 400, 300)));
    running.android.publish(TaskEvent::Frames {
        task_id: 1,
        display_id: 3,
    });
    // A task that has frames but that the agent has not described has no window.
    running.android.publish(TaskEvent::Frames {
        task_id: 9,
        display_id: 4,
    });
    running.android.publish(TaskEvent::Frames {
        task_id: 1,
        display_id: 0,
    });
    running.android.publish(TaskEvent::Frames {
        task_id: 1,
        display_id: 3,
    });
    let open = windows_of(&running, 1);

    // The window manager makes the texture, places its handle in the window host's process,
    // and the window host opens it there.
    let handle = wait_for("display 3 to reach task 1's window", || {
        sink.target(3, 400, 300)
    });
    assert_ne!(handle, 0);
    // The window host copies the frame into its window's swapchain, presents it and says
    // so; the frame call returns only then.
    let started = Instant::now();
    sink.frame(3);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the window host did not answer the frame: {:?}",
        started.elapsed()
    );
    assert_eq!(sink.target(0, 1920, 1080), None, "the whole screen");
    assert_eq!(sink.target(4, 400, 300), None, "a task without a window");
    assert_eq!(sink.target(5, 400, 300), None, "a display no task claimed");

    // A frame of another size gets a texture of that size; resizing the window meanwhile
    // stretches the last frame over it.
    let resized = sink
        .target(3, 640, 480)
        .expect("a texture for the new size");
    assert_ne!(resized, handle);
    sink.frame(3);
    unsafe {
        SetWindowPos(
            open[0].hwnd as HWND,
            std::ptr::null_mut(),
            0,
            0,
            900,
            700,
            SWP_NOMOVE | SWP_NOZORDER,
        )
    };

    // The task ends: its display reaches nothing.
    running.android.publish(TaskEvent::Removed { task_id: 1 });
    windows_of(&running, 0);
    wait_for("display 3 to reach nothing", || {
        sink.target(3, 640, 480).is_none().then_some(())
    });
}

#[test]
fn nfr5_a_service_that_goes_away_closes_every_task_window() {
    let mut running = start(None);
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Clock", 400, 300)));
    windows_of(&running, 1);

    // The service stops: no new connection is accepted, and the watch ends.
    drop(running.service.take());
    running.android.end_watches();
    windows_of(&running, 0);
}

#[test]
fn fr3_a_window_host_names_its_app_by_its_file_or_its_argument() {
    use std::path::Path;
    let none: Vec<String> = Vec::new();
    assert_eq!(
        app_window::package_of(
            Path::new(r"C:\Users\u\AppData\Local\DSA\apps\com.android.settings.exe"),
            &none
        ),
        Some("com.android.settings".to_string())
    );
    assert_eq!(
        app_window::package_of(Path::new(r"C:\Program Files\DSA\dsa-app-window.exe"), &none),
        None
    );
    let named = vec!["--package".to_string(), "org.example.notes".to_string()];
    assert_eq!(
        app_window::package_of(Path::new("dsa-app-window.exe"), &named),
        Some("org.example.notes".to_string())
    );
    let bad = vec!["--package".to_string(), r"..\evil".to_string()];
    assert_eq!(app_window::package_of(Path::new("x.y.exe"), &bad), None);
}
