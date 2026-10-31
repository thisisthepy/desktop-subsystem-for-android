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

use dsa_host_core::compositor::window_sizing::{density_for_dpi, min_client_size, rescale};
use dsa_host_core::compositor::FakeRenderer;
use dsa_host_core::host_api::{UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::ime::{ImeCursor, ImeEvent};
use dsa_host_core::input::{buttons, PointerAction, PointerEvent, PointerTool};
use dsa_host_core::keys::{keycode, meta, KeyAction, KeyEvent};
use dsa_host_core::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest};
use dsa_host_windows::app_window::window::OpenWindow;
use dsa_host_windows::app_window::{self, HostSetup};
use dsa_host_windows::compositor::adapter::{DisplayPresenter, DisplaySink};
use dsa_host_windows::compositor::hosts::HostLauncher;
use dsa_host_windows::compositor::tasks::serve_input;
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, Listener, TransportError};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::Ime::{GCS_COMPSTR, GCS_CURSORPOS};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_A, VK_BACK, VK_CONTROL, VK_MENU, VK_OEM_1, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, IsZoomed, PostMessageW, SendMessageW, SetWindowPos, ShowWindow, SWP_NOMOVE,
    SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CHAR, WM_CLOSE, WM_ENTERSIZEMOVE,
    WM_EXITSIZEMOVE, WM_IME_CHAR, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION,
    WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEWHEEL, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

const WAIT: Duration = Duration::from_secs(10);
const USER: &str = "test-user";

/// Connects to the in-memory local API as [`USER`].
struct AsUser(MemoryLocalConnector);

impl Connector for AsUser {
    fn connect(&mut self, _: Duration) -> Result<Box<dyn Connection>, TransportError> {
        self.0.connect_as(Peer {
            user: USER.into(),
            administrator: false,
        })
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
    /// The local API, which the windows' pointer input goes to.
    api: MemoryLocalConnector,
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
            input: Box::new(AsUser(self.api.clone())),
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
        api: api_connector.clone(),
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
        ..Task::default()
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
        // The window also asked for its monitor's density when it opened.
        let requests = running.android.requests.lock().unwrap().clone();
        let closes: Vec<_> = requests
            .into_iter()
            .filter(|request| matches!(request, TaskRequest::Close { .. }))
            .collect();
        (closes == vec![TaskRequest::Close { task_id: 7 }]).then_some(())
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
    sink.frame(3, (400, 300));
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the window host did not answer the frame: {:?}",
        started.elapsed()
    );
    assert_eq!(sink.target(0, 1920, 1080), None, "the whole screen");
    assert_eq!(sink.target(4, 400, 300), None, "a task without a window");
    assert_eq!(sink.target(5, 400, 300), None, "a display no task claimed");

    // A guest buffer of another size gets a texture of that size; resizing the window
    // meanwhile stretches the last frame over it.
    let resized = sink
        .target(3, 640, 480)
        .expect("a texture for the new size");
    assert_ne!(resized, handle);
    sink.frame(3, (640, 480));
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
fn fr4_frames_of_a_resizing_task_reuse_one_texture_and_each_is_shown() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    let Some(sink) = presenter.sink.lock().unwrap().clone() else {
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
    let open = windows_of(&running, 1);

    // The guest keeps the task's buffers at its display's size and draws each frame into
    // their top-left region, so every frame of the resize comes in the same texture.
    let handle = wait_for("display 3 to reach task 1's window", || {
        sink.target(3, 1280, 720)
    });
    for step in 0..12u32 {
        let size = (400 + 10 * step, 300 + 8 * step);
        resize_to(open[0].hwnd, size.0 as i32 + 16, size.1 as i32 + 39);
        assert_eq!(
            sink.target(3, 1280, 720),
            Some(handle),
            "a frame of {}x{} made a new texture",
            size.0,
            size.1
        );
        let started = Instant::now();
        sink.frame(3, size);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the window host did not answer the {}x{} frame: {:?}",
            size.0,
            size.1,
            started.elapsed()
        );
    }
    // A frame that says no size fills the texture, as one from an older guest does.
    sink.frame(3, (0, 0));

    running.android.publish(TaskEvent::Removed { task_id: 1 });
    windows_of(&running, 0);
}

/// One step of a drag, as the test saw it.
struct DragStep {
    /// How long the step held the window back.
    took: Duration,
    /// The window's client size when the fake guest sent the frame, if it did.
    client_when_sent: Option<(i32, i32)>,
    /// The window's client size before the step.
    before: (i32, i32),
}

/// Drags the window one step to `outer`. The fake guest answers `after` the step began, with a
/// frame of the client size that step comes to, or never when `after` is `None`.
fn drag_step(
    sink: &Arc<dyn DisplaySink>,
    hwnd: isize,
    outer: (i32, i32),
    after: Option<Duration>,
) -> DragStep {
    let before = client_size(hwnd);
    let (window, _) = window_and_work_area(hwnd);
    let client = (
        outer.0 - (window.right - window.left - before.0),
        outer.1 - (window.bottom - window.top - before.1),
    );
    let guest = {
        let sink = Arc::clone(sink);
        thread::spawn(move || {
            let after = after?;
            thread::sleep(after);
            let when_sent = client_size(hwnd);
            sink.frame(3, (client.0 as u32, client.1 as u32));
            Some(when_sent)
        })
    };
    let started = Instant::now();
    resize_to(hwnd, outer.0, outer.1);
    let took = started.elapsed();
    assert_eq!(client_size(hwnd), client, "the step did not take its size");
    DragStep {
        took,
        client_when_sent: guest.join().unwrap(),
        before,
    }
}

#[test]
fn fr4_a_dragged_window_keeps_a_new_size_back_until_the_frame_of_that_size_is_shown() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    let Some(sink) = presenter.sink.lock().unwrap().clone() else {
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
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;
    wait_for("display 3 to reach task 1's window", || {
        sink.target(3, 1280, 720)
    });
    // The window has a frame of its size on the screen before the drag.
    let (width, height) = client_size(hwnd);
    sink.frame(3, (width as u32, height as u32));
    unsafe { SendMessageW(hwnd as HWND, WM_ENTERSIZEMOVE, 0, 0) };

    // The guest answers 40 ms after the step began. The window still has its old size when
    // the frame comes, and takes the size only then: the step does not return before it, and
    // nothing on the screen disagrees in between.
    let step = drag_step(&sink, hwnd, (560, 450), Some(Duration::from_millis(40)));
    assert_eq!(
        step.client_when_sent,
        Some(step.before),
        "the window took its new size before the frame of that size was in"
    );
    assert!(
        step.took >= Duration::from_millis(35),
        "the resize returned after {:?}, before the guest's frame",
        step.took
    );
    assert!(
        step.took < Duration::from_millis(400),
        "the resize waited {:?} for a frame that came in 40 ms",
        step.took
    );

    // A guest that never answers holds the step back for the wait's budget and no longer.
    let took = drag_step(&sink, hwnd, (600, 480), None).took;
    assert!(
        took >= Duration::from_millis(45),
        "the resize returned after {took:?} without a frame, before the wait's budget"
    );
    assert!(
        took < Duration::from_millis(400),
        "the resize waited {took:?} for a frame that never came"
    );

    // After steps in a row without a frame the drag stops waiting: the next is not held back.
    drag_step(&sink, hwnd, (610, 490), None);
    drag_step(&sink, hwnd, (620, 500), None);
    let took = drag_step(&sink, hwnd, (630, 510), None).took;
    assert!(
        took < Duration::from_millis(40),
        "a drag whose guest never answers still waited {took:?} on its fourth step"
    );

    // The drag has not given up for good: later in the same drag, a step waits again, and a
    // guest that answers by then has every step after it wait too.
    thread::sleep(Duration::from_millis(600));
    let step = drag_step(&sink, hwnd, (640, 520), Some(Duration::from_millis(30)));
    assert_eq!(
        step.client_when_sent,
        Some(step.before),
        "a drag that gave up never waited again"
    );
    let step = drag_step(&sink, hwnd, (650, 530), Some(Duration::from_millis(30)));
    assert_eq!(
        step.client_when_sent,
        Some(step.before),
        "a guest that answered again was not waited for on the next step"
    );

    unsafe { SendMessageW(hwnd as HWND, WM_EXITSIZEMOVE, 0, 0) };
    running.android.publish(TaskEvent::Removed { task_id: 1 });
    windows_of(&running, 0);
}

#[test]
fn fr4_a_window_cannot_be_dragged_smaller_than_android_gives_a_task() {
    let running = start(None);
    running
        .android
        .publish(TaskEvent::Updated(task_at_monitor_density(1, 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;

    resize_to(hwnd, 10, 10);

    let expected = min_client_size(dpi_of_new_windows(), Some((1920, 1080)), None);
    assert_eq!(
        client_size(hwnd),
        (expected.0 as i32, expected.1 as i32),
        "the window went below the smallest size"
    );
}

#[test]
fn fr4_a_window_cannot_be_dragged_below_the_minimum_its_app_declares() {
    let running = start(None);
    let mut declared = task_at_monitor_density(1, 800, 600);
    declared.min_width = 700;
    declared.min_height = 500;
    running.android.publish(TaskEvent::Updated(declared));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;

    // The window stops at the minimum during the drag; nothing has to correct it afterwards.
    unsafe { SendMessageW(hwnd as HWND, WM_ENTERSIZEMOVE, 0, 0) };
    wait_for("the window to hear its task's minimum", || {
        resize_to(hwnd, 10, 10);
        (client_size(hwnd).0 >= 700).then_some(())
    });
    let expected = min_client_size(dpi_of_new_windows(), Some((1920, 1080)), Some((700, 500)));
    assert_eq!(client_size(hwnd), (expected.0 as i32, expected.1 as i32));
    unsafe { SendMessageW(hwnd as HWND, WM_EXITSIZEMOVE, 0, 0) };
}

fn resize_to(hwnd: isize, width: i32, height: i32) {
    unsafe {
        SetWindowPos(
            hwnd as HWND,
            std::ptr::null_mut(),
            0,
            0,
            width,
            height,
            SWP_NOMOVE | SWP_NOZORDER,
        )
    };
}

fn last_resize(running: &Running, task_id: i32) -> Option<(u32, u32)> {
    running
        .android
        .requests
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|request| match request {
            TaskRequest::Resize {
                task_id: id,
                width,
                height,
                ..
            } if *id == task_id => Some((*width, *height)),
            _ => None,
        })
}

fn resizes(running: &Running) -> usize {
    running
        .android
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|request| matches!(request, TaskRequest::Resize { .. }))
        .count()
}

/// A task that draws at the density of the window's monitor, so the window opens at its size.
fn task_at_monitor_density(task_id: i32, width: i32, height: i32) -> Task {
    let mut task = task(task_id, "Notes", width, height);
    task.density_dpi = density_for_dpi(dpi_of_new_windows());
    task.max_width = 1920;
    task.max_height = 1080;
    task
}

/// The DPI a new top-level window gets on this machine.
fn dpi_of_new_windows() -> u32 {
    unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() }.max(96)
}

#[test]
fn fr4_resizing_a_task_window_asks_the_guest_for_its_new_client_size() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task_at_monitor_density(1, 400, 300)));
    let open = windows_of(&running, 1);

    // The host's part of the wait: from the window's new size to the request reaching the
    // service. The guest's part (relayout, redraw, compose) needs the real machine.
    let mut waits = Vec::new();
    for step in 0..20 {
        let asked = Instant::now();
        resize_to(open[0].hwnd, 600 + step * 7, 500 + step * 3);
        let resized = asked.elapsed();
        let (width, height) = client_size(open[0].hwnd);
        let size = (width as u32, height as u32);
        let deadline = Instant::now() + WAIT;
        while last_resize(&running, 1) != Some(size) {
            assert!(
                Instant::now() < deadline,
                "the new size never reached the guest"
            );
            thread::yield_now();
        }
        waits.push((resized, asked.elapsed()));
    }
    waits.sort_by_key(|(_, total)| *total);
    let (resized, total) = waits[waits.len() / 2];
    eprintln!(
        "host side of a resize (median of 20): the window took the size in {} us, and the request reached the service {} us after the resize began (longest {} us)",
        resized.as_micros(),
        total.as_micros(),
        waits[waits.len() - 1].1.as_micros()
    );
}

#[test]
fn fr4_a_live_resize_ends_with_the_guest_asked_for_the_last_size() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task_at_monitor_density(1, 400, 300)));
    let open = windows_of(&running, 1);

    // Only the requests the drag makes count, not the ones the window made opening.
    thread::sleep(Duration::from_millis(200));
    let before = resizes(&running);
    let started = Instant::now();
    for step in 0..60 {
        resize_to(open[0].hwnd, 500 + step * 5, 420 + step * 3);
    }
    let (width, height) = client_size(open[0].hwnd);
    let size = (width as u32, height as u32);
    wait_for("the last size to reach the guest", || {
        (last_resize(&running, 1) == Some(size)).then_some(())
    });
    let sent = resizes(&running) - before;
    assert!(sent <= 60, "{sent} requests for 60 sizes");
    eprintln!(
        "a 60-step live resize sent {sent} size requests; the last arrived {} ms after the first step",
        started.elapsed().as_millis()
    );
}

/// The task's size requests since the first `skip` requests, with whether each was live.
fn resizes_since(running: &Running, skip: usize, task_id: i32) -> Vec<((u32, u32), bool)> {
    running
        .android
        .requests
        .lock()
        .unwrap()
        .iter()
        .skip(skip)
        .filter_map(|request| match request {
            TaskRequest::Resize {
                task_id: id,
                width,
                height,
                live,
            } if *id == task_id => Some(((*width, *height), *live)),
            _ => None,
        })
        .collect()
}

#[test]
fn fr4_a_dragged_window_resizes_its_task_live_and_reconfigures_it_when_let_go() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task_at_monitor_density(1, 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd as HWND;

    // Only the requests the drag makes count, not the ones the window made opening.
    thread::sleep(Duration::from_millis(200));
    let before = running.android.requests.lock().unwrap().len();
    // Windows' sizing loop starts as the user takes hold of the window's edge.
    unsafe { SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0) };
    resize_to(open[0].hwnd, 520, 430);
    wait_for("a live size to reach the guest", || {
        resizes_since(&running, before, 1)
            .iter()
            .any(|(_, live)| *live)
            .then_some(())
    });
    for step in 1..10 {
        resize_to(open[0].hwnd, 520 + step * 9, 430 + step * 4);
    }
    let (width, height) = client_size(open[0].hwnd);
    let size = (width as u32, height as u32);
    // The user lets go.
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    let asked = wait_for("the drag's end to reach the guest", || {
        let asked = resizes_since(&running, before, 1);
        (asked.last() == Some(&(size, false))).then_some(asked)
    });
    let (last, steps) = asked.split_last().unwrap();
    assert_eq!(*last, (size, false));
    assert!(
        steps.iter().all(|(_, live)| *live),
        "a size asked while the window was dragged was not live: {asked:?}"
    );

    // A drag that only moves the window asks nothing at its end. A resize outside a drag is
    // live too, and reconfigures the task once the task has the size.
    unsafe { SendMessageW(hwnd, WM_ENTERSIZEMOVE, 0, 0) };
    unsafe { SendMessageW(hwnd, WM_EXITSIZEMOVE, 0, 0) };
    let after_move = running.android.requests.lock().unwrap().len();
    resize_to(open[0].hwnd, 700, 500);
    let (width, height) = client_size(open[0].hwnd);
    let size = (width as u32, height as u32);
    wait_for("the resize to reach the guest", || {
        (resizes_since(&running, after_move, 1) == vec![(size, true)]).then_some(())
    });
    let taken = task_at_monitor_density(1, width, height);
    running.android.publish(TaskEvent::Updated(taken));
    let reconfigured = vec![(size, true), (size, false)];
    wait_for("the task to be reconfigured", || {
        (resizes_since(&running, after_move, 1) == reconfigured).then_some(())
    });
}

/// The window's outer rectangle and its monitor's work area.
fn window_and_work_area(hwnd: isize) -> (RECT, RECT) {
    let mut window = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    unsafe { GetWindowRect(hwnd as HWND, &mut window) };
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    unsafe {
        GetMonitorInfoW(
            MonitorFromWindow(hwnd as HWND, MONITOR_DEFAULTTONEAREST),
            &mut info,
        )
    };
    (window, info.rcWork)
}

#[test]
fn fr4_a_maximised_task_window_shows_its_task_1_to_1_without_a_new_texture_or_a_stray_size() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    running.android.start();
    // The guest draws at most 640x480: less than any monitor's work area, as 1920x1080 is
    // on a 4K monitor at 200%.
    let max = (640u32, 480u32);
    let mut clock = task_at_monitor_density(1, 400, 300);
    clock.max_width = max.0;
    clock.max_height = max.1;
    running.android.publish(TaskEvent::Updated(clock.clone()));
    running.android.publish(TaskEvent::Frames {
        task_id: 1,
        display_id: 3,
    });
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;
    let sink = presenter.sink.lock().unwrap().clone();
    let texture = sink.as_ref().map(|sink| {
        let handle = wait_for("display 3 to reach task 1's window", || {
            sink.target(3, 1280, 720)
        });
        sink.frame(3, (400, 300));
        handle
    });

    // Only the requests the maximise makes count, not the ones the window made opening.
    thread::sleep(Duration::from_millis(200));
    let before = running.android.requests.lock().unwrap().len();
    unsafe { ShowWindow(hwnd as HWND, SW_MAXIMIZE) };
    assert_ne!(unsafe { IsZoomed(hwnd as HWND) }, 0);
    let (width, height) = client_size(hwnd);
    assert_eq!(
        (width as u32, height as u32),
        max,
        "a maximised window takes the most the guest can draw, and no more"
    );
    // Centred on the work area, not left in its corner.
    let (window, work) = window_and_work_area(hwnd);
    let off_centre = |start: i32, end: i32, work_start: i32, work_end: i32| {
        ((start - work_start) - (work_end - end)).abs()
    };
    assert!(
        off_centre(window.left, window.right, work.left, work.right) <= 1
            && off_centre(window.top, window.bottom, work.top, work.bottom) <= 1,
        "the maximised window at {}x{}..{}x{} is not centred on the work area {}x{}..{}x{}",
        window.left,
        window.top,
        window.right,
        window.bottom,
        work.left,
        work.top,
        work.right,
        work.bottom
    );

    // The task is given the size live, so its app lays itself out again instead of being
    // recreated at a size it has not drawn at.
    wait_for("the maximised size to reach the guest", || {
        (resizes_since(&running, before, 1) == vec![(max, true)]).then_some(())
    });
    // The guest's frames keep coming in the same texture, the first ones still of the old
    // size, then of the new one.
    if let (Some(sink), Some(texture)) = (&sink, texture) {
        assert_eq!(sink.target(3, 1280, 720), Some(texture), "a new texture");
        sink.frame(3, (400, 300));
        sink.frame(3, max);
    }
    // Once the task has the size, it is reconfigured once, at that size.
    let mut maximised = clock.clone();
    maximised.bounds.right = maximised.bounds.left + max.0 as i32;
    maximised.bounds.bottom = maximised.bounds.top + max.1 as i32;
    running.android.publish(TaskEvent::Updated(maximised));
    let reconfigured = vec![(max, true), (max, false)];
    wait_for("the task to be reconfigured", || {
        (resizes_since(&running, before, 1) == reconfigured).then_some(())
    });
    if let (Some(sink), Some(texture)) = (&sink, texture) {
        assert_eq!(sink.target(3, 1280, 720), Some(texture), "a new texture");
        sink.frame(3, max);
    }
    // The window keeps its size: nothing larger than the guest draws, nothing of size zero.
    thread::sleep(Duration::from_millis(300));
    assert_eq!(client_size(hwnd), (max.0 as i32, max.1 as i32));
    let asked = resizes_since(&running, 0, 1);
    let within = |((width, height), _): &((u32, u32), bool)| {
        (1..=max.0).contains(width) && (1..=max.1).contains(height)
    };
    assert!(
        asked.iter().all(within),
        "a size outside 1x1..{}x{} was asked for: {asked:?}",
        max.0,
        max.1
    );

    // Restoring it gives the task its old size back the same way.
    let restored_from = running.android.requests.lock().unwrap().len();
    unsafe { ShowWindow(hwnd as HWND, SW_RESTORE) };
    assert_eq!(unsafe { IsZoomed(hwnd as HWND) }, 0);
    let (width, height) = client_size(hwnd);
    let restored = (width as u32, height as u32);
    assert_ne!(restored, max);
    wait_for("the restored size to reach the guest", || {
        (resizes_since(&running, restored_from, 1) == vec![(restored, true)]).then_some(())
    });

    running.android.publish(TaskEvent::Removed { task_id: 1 });
    windows_of(&running, 0);
}

#[test]
fn fr4_a_task_window_cannot_grow_past_what_the_guest_can_draw() {
    let running = start(None);
    running.android.start();
    let mut small = task_at_monitor_density(1, 400, 300);
    small.max_width = 500;
    small.max_height = 420;
    running.android.publish(TaskEvent::Updated(small));
    let open = windows_of(&running, 1);

    resize_to(open[0].hwnd, 1400, 1000);
    let (width, height) = client_size(open[0].hwnd);
    assert!(
        width <= 500 && height <= 420,
        "the client area grew to {width}x{height}"
    );
}

#[test]
fn fr4_a_task_window_opens_at_its_monitors_scale() {
    let running = start(None);
    running.android.start();
    let mut notes = task(1, "Notes", 400, 300);
    notes.density_dpi = 160;
    notes.max_width = 1920;
    notes.max_height = 1080;
    running.android.publish(TaskEvent::Updated(notes));
    let open = windows_of(&running, 1);
    let dpi = unsafe { GetDpiForWindow(open[0].hwnd as HWND) }.max(96);
    let wanted = density_for_dpi(dpi);

    let (width, height) = rescale((400, 300), 160, wanted);
    assert_eq!(client_size(open[0].hwnd), (width as i32, height as i32));
    if wanted != 160 {
        wait_for("the density to reach the guest", || {
            let requests = running.android.requests.lock().unwrap().clone();
            (requests.first()
                == Some(&TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: wanted,
                }))
            .then_some(())
        });
        wait_for("the size that goes with it", || {
            (last_resize(&running, 1) == Some((width, height))).then_some(())
        });
    }
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
    // Each app's program is in a folder named after its package, and named after the app.
    assert_eq!(
        app_window::package_of(
            Path::new(r"C:\Users\u\AppData\Local\DSA\apps\com.android.deskclock\Clock.exe"),
            &none
        ),
        Some("com.android.deskclock".to_string())
    );
    assert_eq!(
        app_window::package_of(
            Path::new(r"C:\Users\u\AppData\Local\DSA\apps\com.example.notes\My.Notes.exe"),
            &none
        ),
        Some("com.example.notes".to_string())
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

/// The button flags a button message carries in its `wparam` (`MK_LBUTTON`, `MK_RBUTTON`).
const MK_LBUTTON: u32 = 0x0001;
const MK_RBUTTON: u32 = 0x0002;

fn lparam(x: i32, y: i32) -> isize {
    ((y as i16 as u16 as usize) << 16 | x as i16 as u16 as usize) as isize
}

/// Waits until the fake Android has been given `count` pointer events other than hover
/// events, and returns those. The tests post only button and wheel messages; a hover can
/// still arrive when the machine's real cursor happens to rest over the test window.
fn pointers(running: &Running, count: usize) -> Vec<PointerEvent> {
    wait_for(&format!("{count} pointer events in Android"), || {
        let given: Vec<PointerEvent> = running
            .android
            .pointers
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                !matches!(
                    event.action,
                    PointerAction::HoverMove | PointerAction::HoverExit
                )
            })
            .copied()
            .collect();
        (given.len() >= count).then_some(given)
    })
}

#[test]
fn fr5_a_right_click_in_a_task_window_reaches_its_task_as_a_secondary_click() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Clock", 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd as HWND;
    let (width, height) = client_size(open[0].hwnd);
    let (x, y) = (width / 2, height / 4);
    unsafe {
        PostMessageW(hwnd, WM_RBUTTONDOWN, MK_RBUTTON as usize, lparam(x, y));
        PostMessageW(hwnd, WM_RBUTTONUP, 0, lparam(x, y));
    }
    let given = pointers(&running, 2);
    let summary: Vec<(i32, PointerAction, PointerTool, u32)> = given
        .iter()
        .map(|event| (event.task_id, event.action, event.tool, event.buttons))
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                7,
                PointerAction::Down,
                PointerTool::Mouse,
                buttons::SECONDARY
            ),
            (7, PointerAction::Up, PointerTool::Mouse, 0),
        ]
    );
    let expected_x = x as f32 * 400.0 / width as f32;
    let expected_y = y as f32 * 300.0 / height as f32;
    assert!((given[0].x - expected_x).abs() <= 2.0, "{:?}", given[0]);
    assert!((given[0].y - expected_y).abs() <= 2.0, "{:?}", given[0]);
}

#[test]
fn fr5_the_four_corners_of_a_task_window_land_on_the_corners_of_its_task() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Clock", 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd as HWND;
    let (width, height) = client_size(open[0].hwnd);
    let corners = [
        ((0, 0), (0.0, 0.0)),
        ((width - 1, 0), (399.0, 0.0)),
        ((0, height - 1), (0.0, 299.0)),
        ((width - 1, height - 1), (399.0, 299.0)),
    ];
    for ((x, y), _) in corners {
        unsafe {
            PostMessageW(hwnd, WM_LBUTTONDOWN, MK_LBUTTON as usize, lparam(x, y));
            PostMessageW(hwnd, WM_LBUTTONUP, 0, lparam(x, y));
        }
    }
    let given = pointers(&running, 8);
    for (index, (client, guest)) in corners.iter().enumerate() {
        let down = given[index * 2];
        assert_eq!(down.action, PointerAction::Down);
        assert!(
            (down.x - guest.0).abs() <= 2.0 && (down.y - guest.1).abs() <= 2.0,
            "client point {client:?} of a {width}x{height} window landed at ({}, {}), \
             expected {guest:?}",
            down.x,
            down.y
        );
    }
}

#[test]
fn fr5_the_wheel_in_a_task_window_scrolls_its_task() {
    let running = start(None);
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Clock", 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd as HWND;
    let mut point = POINT { x: 10, y: 20 };
    unsafe { ClientToScreen(hwnd, &mut point) };
    let down_one_notch = ((-120i16) as u16 as usize) << 16;
    unsafe {
        PostMessageW(
            hwnd,
            WM_MOUSEWHEEL,
            down_one_notch,
            lparam(point.x, point.y),
        )
    };
    let scroll = pointers(&running, 1)[0];
    assert_eq!(scroll.action, PointerAction::Scroll);
    assert_eq!((scroll.scroll_x, scroll.scroll_y), (0.0, -1.0));
    let (width, height) = client_size(open[0].hwnd);
    assert!((scroll.x - 10.0 * 400.0 / width as f32).abs() <= 2.0);
    assert!((scroll.y - 20.0 * 300.0 / height as f32).abs() <= 2.0);
}

/// Reaches the in-memory local API only once `up` is set, as a service that is starting.
struct Starting {
    up: Arc<AtomicBool>,
    inner: AsUser,
}

impl Connector for Starting {
    fn connect(&mut self, timeout: Duration) -> Result<Box<dyn Connection>, TransportError> {
        if !self.up.load(Ordering::SeqCst) {
            return Err(TransportError::Closed);
        }
        self.inner.connect(timeout)
    }
}

#[test]
fn fr5_input_while_the_service_is_away_is_dropped_and_input_after_it_returns_arrives() {
    let (api_listener, api_connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(Box::new(api_listener), androids.clone());
    let up = Arc::new(AtomicBool::new(false));
    let (events, received) = std::sync::mpsc::channel();
    let connector = Starting {
        up: Arc::clone(&up),
        inner: AsUser(api_connector),
    };
    let thread = thread::spawn(move || serve_input(Box::new(connector), received, None));
    let event =
        |x: f32| PointerEvent::new(7, PointerAction::HoverMove, PointerTool::Mouse, (x, 1.0));
    events.send(event(1.0).into()).unwrap();
    thread::sleep(Duration::from_millis(100));
    up.store(true, Ordering::SeqCst);
    androids.android(USER).unwrap().start();
    let android = androids.of(USER).unwrap();
    // The thread tries the service again after a short wait; input from then on arrives.
    let delivered = wait_for("input to arrive once the service is back", || {
        events.send(event(2.0).into()).unwrap();
        thread::sleep(Duration::from_millis(50));
        let given = android.pointers.lock().unwrap().clone();
        (!given.is_empty()).then_some(given)
    });
    assert!(
        delivered.iter().all(|given| given.x == 2.0),
        "{delivered:?}"
    );
    drop(events);
    thread.join().unwrap();
}

/// The `lparam` of a key message: a repeat count of one, the scan code, the extended-key
/// flag, and for a release the previous-state and transition bits.
fn key_lparam(scan: u32, extended: bool, down: bool) -> isize {
    let mut bits = 1usize | ((scan as usize) << 16);
    if extended {
        bits |= 1 << 24;
    }
    if !down {
        bits |= (1 << 30) | (1 << 31);
    }
    bits as isize
}

/// Sends a key message to the window and returns once the window has handled it.
fn send_key(hwnd: HWND, message: u32, vk: u16, scan: u32, extended: bool) {
    let down = matches!(message, WM_KEYDOWN | WM_SYSKEYDOWN);
    unsafe { SendMessageW(hwnd, message, vk as usize, key_lparam(scan, extended, down)) };
}

/// Waits until the fake Android has been given `count` key events, and returns them with
/// the lock bits cleared, since those follow the locks of the machine the test runs on.
fn keys(running: &Running, count: usize) -> Vec<KeyEvent> {
    wait_for(&format!("{count} key events in Android"), || {
        let given: Vec<KeyEvent> = running
            .android
            .keys
            .lock()
            .unwrap()
            .iter()
            .map(|event| KeyEvent {
                meta_state: event.meta_state & !meta::LOCKS,
                ..*event
            })
            .collect();
        (given.len() >= count).then_some(given)
    })
}

fn one_task_window(running: &Running) -> HWND {
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Notes", 400, 300)));
    windows_of(running, 1)[0].hwnd as HWND
}

#[test]
fn fr16_keys_in_a_task_window_reach_its_task_with_their_codes_and_modifiers() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    // Ctrl+A, then a held Backspace that repeats, then the keypad's Enter.
    send_key(hwnd, WM_KEYDOWN, VK_CONTROL, 0x1d, false);
    send_key(hwnd, WM_KEYDOWN, VK_A, 0x1e, false);
    send_key(hwnd, WM_KEYUP, VK_A, 0x1e, false);
    send_key(hwnd, WM_KEYUP, VK_CONTROL, 0x1d, false);
    send_key(hwnd, WM_KEYDOWN, VK_BACK, 0x0e, false);
    send_key(hwnd, WM_KEYDOWN, VK_BACK, 0x0e, false);
    send_key(hwnd, WM_KEYUP, VK_BACK, 0x0e, false);
    send_key(hwnd, WM_KEYDOWN, VK_RETURN, 0x1c, true);
    send_key(hwnd, WM_KEYUP, VK_RETURN, 0x1c, true);
    let ctrl = meta::CTRL_ON | meta::CTRL_LEFT_ON;
    let summary: Vec<(i32, KeyAction, u32, u32, u32)> = keys(&running, 9)
        .iter()
        .map(|event| {
            (
                event.task_id,
                event.action,
                event.key_code,
                event.repeat_count,
                event.meta_state,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (7, KeyAction::Down, keycode::CTRL_LEFT, 0, ctrl),
            (7, KeyAction::Down, keycode::A, 0, ctrl),
            (7, KeyAction::Up, keycode::A, 0, ctrl),
            (7, KeyAction::Up, keycode::CTRL_LEFT, 0, 0),
            (7, KeyAction::Down, keycode::DEL, 0, 0),
            (7, KeyAction::Down, keycode::DEL, 1, 0),
            (7, KeyAction::Up, keycode::DEL, 0, 0),
            (7, KeyAction::Down, keycode::NUMPAD_ENTER, 0, 0),
            (7, KeyAction::Up, keycode::NUMPAD_ENTER, 0, 0),
        ]
    );
}

#[test]
fn fr16_right_modifiers_and_alt_keys_reach_the_task() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    send_key(hwnd, WM_KEYDOWN, VK_SHIFT, 0x36, false);
    send_key(hwnd, WM_KEYDOWN, VK_OEM_1, 0x27, false);
    send_key(hwnd, WM_KEYUP, VK_OEM_1, 0x27, false);
    send_key(hwnd, WM_KEYUP, VK_SHIFT, 0x36, false);
    // Alt comes as system key messages.
    send_key(hwnd, WM_SYSKEYDOWN, VK_MENU, 0x38, false);
    send_key(hwnd, WM_SYSKEYDOWN, VK_TAB, 0x0f, false);
    send_key(hwnd, WM_SYSKEYUP, VK_TAB, 0x0f, false);
    send_key(hwnd, WM_KEYUP, VK_MENU, 0x38, false);
    let shift = meta::SHIFT_ON | meta::SHIFT_RIGHT_ON;
    let alt = meta::ALT_ON | meta::ALT_LEFT_ON;
    let summary: Vec<(KeyAction, u32, u32)> = keys(&running, 8)
        .iter()
        .map(|event| (event.action, event.key_code, event.meta_state))
        .collect();
    assert_eq!(
        summary,
        vec![
            (KeyAction::Down, keycode::SHIFT_RIGHT, shift),
            (KeyAction::Down, keycode::SEMICOLON, shift),
            (KeyAction::Up, keycode::SEMICOLON, shift),
            (KeyAction::Up, keycode::SHIFT_RIGHT, 0),
            (KeyAction::Down, keycode::ALT_LEFT, alt),
            (KeyAction::Down, keycode::TAB, alt),
            (KeyAction::Up, keycode::TAB, alt),
            (KeyAction::Up, keycode::ALT_LEFT, 0),
        ]
    );
}

#[test]
fn fr16_losing_the_focus_releases_every_key_still_held() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    send_key(hwnd, WM_KEYDOWN, VK_SHIFT, 0x2a, false);
    send_key(hwnd, WM_KEYDOWN, VK_RIGHT, 0x4d, true);
    unsafe { SendMessageW(hwnd, WM_KILLFOCUS, 0, 0) };
    let given = keys(&running, 4);
    let released: Vec<(KeyAction, u32, bool)> = given[2..]
        .iter()
        .map(|event| (event.action, event.key_code, event.canceled))
        .collect();
    assert_eq!(
        released,
        vec![
            (KeyAction::Up, keycode::DPAD_RIGHT, true),
            (KeyAction::Up, keycode::SHIFT_LEFT, true),
        ]
    );
    // The real releases, which arrive wherever the focus went, are not sent again.
    send_key(hwnd, WM_KEYUP, VK_RIGHT, 0x4d, true);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(running.android.keys.lock().unwrap().len(), 4);
}

/// Waits until the fake Android has been given `count` pieces of text input, and returns
/// them.
fn text(running: &Running, count: usize) -> Vec<ImeEvent> {
    wait_for(&format!("{count} pieces of text in Android"), || {
        let given = running.android.text.lock().unwrap().clone();
        (given.len() >= count).then_some(given)
    })
}

fn commit(text: &str) -> ImeEvent {
    ImeEvent::Commit {
        task_id: 7,
        text: text.into(),
    }
}

#[test]
fn fr6_a_key_that_types_a_letter_reaches_the_task_as_text_and_never_as_a_key() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    // Through the window's own message loop, which turns the key into its character as it
    // does for a real key; then Enter, which types no text and goes as a key.
    unsafe {
        PostMessageW(
            hwnd,
            WM_KEYDOWN,
            VK_A as usize,
            key_lparam(0x1e, false, true),
        );
        PostMessageW(
            hwnd,
            WM_KEYUP,
            VK_A as usize,
            key_lparam(0x1e, false, false),
        );
        PostMessageW(
            hwnd,
            WM_KEYDOWN,
            VK_RETURN as usize,
            key_lparam(0x1c, false, true),
        );
        PostMessageW(
            hwnd,
            WM_KEYUP,
            VK_RETURN as usize,
            key_lparam(0x1c, false, false),
        );
    }
    let codes: Vec<(KeyAction, u32)> = keys(&running, 2)
        .iter()
        .map(|event| (event.action, event.key_code))
        .collect();
    assert_eq!(
        codes,
        vec![
            (KeyAction::Down, keycode::ENTER),
            (KeyAction::Up, keycode::ENTER)
        ]
    );
    // The letter, once, as the layout typed it; Enter's carriage return is not text.
    assert_eq!(text(&running, 1), vec![commit("a")]);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(running.android.text.lock().unwrap().len(), 1);
    assert_eq!(running.android.keys.lock().unwrap().len(), 2);
}

#[test]
fn fr6_characters_of_any_layout_reach_the_task_as_text() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    unsafe {
        // A German layout's sharp s, then U+1F600 in its two halves, then a carriage return,
        // whose key went as a key event.
        for unit in [0xdf_usize, 0xd83d, 0xde00, 0x0d, 0x6b] {
            SendMessageW(hwnd, WM_CHAR, unit, 1);
        }
    }
    assert_eq!(
        text(&running, 3),
        vec![commit("\u{df}"), commit("\u{1f600}"), commit("k")]
    );
    assert!(running.android.keys.lock().unwrap().is_empty());
}

#[test]
fn fr6_input_method_messages_without_a_composition_send_nothing() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    unsafe {
        // The window opens no composition window of its own: the guest's editor shows it.
        assert_eq!(SendMessageW(hwnd, WM_IME_STARTCOMPOSITION, 0, 0), 0);
        // No input method composes here, so there is nothing to read.
        SendMessageW(
            hwnd,
            WM_IME_COMPOSITION,
            0,
            (GCS_COMPSTR | GCS_CURSORPOS) as isize,
        );
        SendMessageW(hwnd, WM_IME_ENDCOMPOSITION, 0, 0);
        // A commit already carried an input method's characters.
        SendMessageW(hwnd, WM_IME_CHAR, 0xd55c, 1);
        SendMessageW(hwnd, WM_CHAR, usize::from(b'z'), 1);
    }
    assert_eq!(text(&running, 1), vec![commit("z")]);
}

#[test]
fn fr6_the_window_places_the_input_method_at_the_cursor_the_guest_reports() {
    let running = start(None);
    one_task_window(&running);
    let cursor = ImeCursor {
        left: 120,
        top: 64,
        right: 122,
        bottom: 96,
    };
    running
        .android
        .publish(TaskEvent::ImeCursor { task_id: 7, cursor });
    wait_for("the window to place the input method at the cursor", || {
        (running.windows().first()?.ime_cursor == Some(cursor)).then_some(())
    });
}

/// Each word the guest was given on whether any task window is shown, in order.
fn shown_words(running: &Running) -> Vec<bool> {
    running.android.shown.lock().unwrap().clone()
}

fn wait_for_shown(running: &Running, shown: bool) {
    wait_for(
        &format!("the guest to hear whether a window is shown ({shown})"),
        || (shown_words(running).last() == Some(&shown)).then_some(()),
    );
}

#[test]
fn fr10_minimizing_and_closing_every_task_window_tells_the_guest_none_is_shown() {
    let running = start(None);
    running.android.start();
    // No window yet.
    wait_for_shown(&running, false);
    running
        .android
        .publish(TaskEvent::Updated(task(7, "Clock", 400, 300)));
    running
        .android
        .publish(TaskEvent::Updated(task(8, "Maps", 400, 300)));
    let open = windows_of(&running, 2);
    wait_for_shown(&running, true);

    // One window minimized leaves the other shown.
    unsafe { ShowWindow(open[0].hwnd as HWND, SW_MINIMIZE) };
    thread::sleep(Duration::from_millis(300));
    assert_eq!(shown_words(&running).last(), Some(&true));
    unsafe { ShowWindow(open[1].hwnd as HWND, SW_MINIMIZE) };
    wait_for_shown(&running, false);

    // Restoring a window shows it again, which wakes the guest.
    unsafe { ShowWindow(open[1].hwnd as HWND, SW_RESTORE) };
    wait_for_shown(&running, true);

    // Closing the shown window leaves only a minimized one.
    unsafe { PostMessageW(open[1].hwnd as HWND, WM_CLOSE, 0, 0) };
    wait_for_shown(&running, false);
    unsafe { ShowWindow(open[0].hwnd as HWND, SW_RESTORE) };
    wait_for_shown(&running, true);
    unsafe { PostMessageW(open[0].hwnd as HWND, WM_CLOSE, 0, 0) };
    wait_for_shown(&running, false);
}
